use std::{convert::Infallible, sync::Arc};

use axum::body::{Body, Bytes};
use futures_util::stream::unfold;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub(crate) struct Live {
    sender: watch::Sender<Arc<Vec<u8>>>,
    pub cancelled: CancellationToken,
}

impl Live {
    pub fn new() -> Self {
        let (sender, _) = watch::channel(Arc::new(Vec::new()));
        Self {
            sender,
            cancelled: CancellationToken::new(),
        }
    }

    pub fn push(&self, chunk: &[u8]) {
        self.sender
            .send_modify(|bytes| Arc::make_mut(bytes).extend_from_slice(chunk));
    }

    pub fn body(&self) -> Body {
        Body::from_stream(unfold(
            (self.sender.subscribe(), 0),
            |(mut reader, mut offset)| async move {
                loop {
                    let chunk = {
                        let bytes = reader.borrow_and_update();
                        let chunk = Bytes::copy_from_slice(&bytes[offset..]);
                        offset = bytes.len();
                        chunk
                    };
                    if !chunk.is_empty() {
                        return Some((Ok::<_, Infallible>(chunk), (reader, offset)));
                    }
                    reader.changed().await.ok()?;
                }
            },
        ))
    }
}

pub(super) struct AppendBuffer {
    text: String,
    incomplete: Vec<u8>,
    budget: usize,
    pub full: bool,
}

impl AppendBuffer {
    pub fn new(budget: usize) -> Self {
        Self {
            text: String::new(),
            incomplete: Vec::new(),
            budget,
            full: budget == 0,
        }
    }

    /// Return the raw prefix readers can apply to get the same text as the final snapshot.
    pub fn accept(&mut self, input: &[u8], eof: bool) -> Vec<u8> {
        let mut data = std::mem::take(&mut self.incomplete);
        data.extend_from_slice(input);
        let mut consumed = 0;
        while !self.full && consumed < data.len() {
            let rest = &data[consumed..];
            let (valid, invalid_length) = match std::str::from_utf8(rest) {
                Ok(valid) => (valid, None),
                Err(error) => {
                    let valid = std::str::from_utf8(&rest[..error.valid_up_to()])
                        .expect("valid UTF-8 prefix");
                    (valid, error.error_len())
                }
            };
            for character in valid.chars() {
                if !self.apply(character) {
                    break;
                }
                consumed += character.len_utf8();
                if self.full {
                    break;
                }
            }
            if self.full || consumed == data.len() {
                break;
            }
            if let Some(length) = invalid_length.or_else(|| eof.then_some(data.len() - consumed)) {
                if self.apply(char::REPLACEMENT_CHARACTER) {
                    consumed += length;
                }
            } else {
                self.incomplete.extend_from_slice(&data[consumed..]);
                break;
            }
        }
        data.truncate(consumed);
        data
    }

    fn apply(&mut self, character: char) -> bool {
        match character {
            '\u{08}' => {
                self.text.pop();
            }
            '\u{0c}' => self.text.clear(),
            character if self.text.len() + character.len_utf8() <= self.budget => {
                self.text.push(character);
            }
            _ => {
                self.full = true;
                return false;
            }
        }
        self.full = self.text.len() == self.budget;
        true
    }
}

pub(super) fn append_text(base: &str, raw: &[u8], budget: usize) -> String {
    let mut append = AppendBuffer::new(budget);
    append.accept(raw, true);
    let mut text = base.to_owned();
    text.push_str(&append.text);
    text
}

pub(super) fn terminal_output(raw: &[u8], limit: usize) -> String {
    if raw.len() <= limit {
        return String::from_utf8_lossy(raw).into_owned();
    }
    let tail = &raw[raw.len() - limit..];
    let first = tail
        .iter()
        .position(|&byte| byte < 0x20)
        .or_else(|| tail.iter().position(|&byte| !(0x80..0xc0).contains(&byte)))
        .unwrap_or(tail.len());
    String::from_utf8_lossy(&tail[first..]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{AppendBuffer, append_text};

    #[test]
    fn live_append_and_snapshot_agree_when_a_split_code_point_fills_the_budget() {
        let mut append = AppendBuffer::new(3);
        let mut raw = append.accept(b"old", false);
        assert!(append.full);
        raw.extend(append.accept(b"\x0creplacement", true));
        assert_eq!(raw, b"old");

        let mut append = AppendBuffer::new(3);
        let mut raw = append.accept(b"ab\x0cX\xc3", false);
        assert_eq!(raw, b"ab\x0cX");
        raw.extend(append.accept(b"\xa9ignored", false));
        assert!(append.full);
        assert_eq!(raw, "ab\x0cXé".as_bytes());
        assert_eq!(append_text("prefix:", &raw, 3), "prefix:Xé");
    }
}

#[cfg(test)]
mod spec_tests {
    //! §4.8.5–4.8.6: terminal trimming, append edits and budget, and decoding.
    use super::{AppendBuffer, append_text, terminal_output};

    /// The spec's `keep_last`, transcribed from the Python in §4.8.6.
    fn keep_last(output: &[u8], limit: usize) -> &[u8] {
        if output.len() <= limit {
            return output;
        }
        let tail = &output[output.len() - limit..];
        if let Some(i) = tail.iter().position(|&b| b < 0x20) {
            return &tail[i..];
        }
        let skip = tail
            .iter()
            .take_while(|b| (0x80..0xc0).contains(*b))
            .count();
        &tail[skip..]
    }

    /// Deterministic xorshift, so failures reproduce without a rand crate.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    // --- terminal trimming (§4.8.6) ---

    #[test]
    fn terminal_output_under_the_limit_is_unchanged() {
        let output = "\x1b[32mok\x1b[0m  42 tests\r\n";
        assert_eq!(terminal_output(output.as_bytes(), output.len()), output);
        assert_eq!(terminal_output(output.as_bytes(), 1000), output);
        assert_eq!(terminal_output(b"", 0), "");
    }

    #[test]
    fn terminal_output_starts_at_the_first_control_byte_in_the_tail() {
        // Tail of 8 is "ne2\nline"; it starts at the line feed.
        assert_eq!(terminal_output(b"line1\nline2\nline3", 8), "\nline3");
        // The control byte may be an escape, mid-line.
        assert_eq!(terminal_output(b"aaaa\x1b[31mred", 9), "\x1b[31mred");
        // A control byte at the very start of the tail keeps the whole tail.
        assert_eq!(terminal_output(b"xx\rabc", 4), "\rabc");
    }

    #[test]
    fn terminal_output_without_controls_skips_leading_continuation_bytes() {
        // "é" is C3 A9; a 4-byte tail of "aéxyz" begins at A9.
        assert_eq!(terminal_output("aéxyz".as_bytes(), 4), "xyz");
        // "😀" is F0 9F 98 80; cut after F0 9F leaves two continuation bytes.
        assert_eq!(terminal_output("a😀bc".as_bytes(), 4), "bc");
        // A tail of only continuation bytes keeps nothing.
        assert_eq!(terminal_output(&[0x41, 0x80, 0x80, 0x80], 3), "");
    }

    #[test]
    fn terminal_output_never_splits_a_character_and_matches_keep_last() {
        let alphabet = ["a", "Z", " ", "é", "€", "😀", "\n", "\r", "\x1b[0m", "\t"];
        let mut rng = Rng(0x5eed);
        for _ in 0..2_000 {
            let text: String = (0..rng.below(40))
                .map(|_| alphabet[rng.below(alphabet.len())])
                .collect();
            let limit = rng.below(text.len() + 2);
            let kept = terminal_output(text.as_bytes(), limit);
            let expected = String::from_utf8_lossy(keep_last(text.as_bytes(), limit));
            assert_eq!(kept, expected, "{text:?} limit {limit}");
            assert!(
                !kept.contains('\u{fffd}'),
                "split a character: {text:?} limit {limit}"
            );
            assert!(text.ends_with(&kept) && kept.len() <= limit);
        }
    }

    // --- append edits (§4.8.5) ---

    #[test]
    fn backspace_removes_one_code_point_not_one_byte() {
        assert_eq!(append_text("", "aé\x08".as_bytes(), 100), "a");
        assert_eq!(append_text("", "a😀\x08b".as_bytes(), 100), "ab");
        assert_eq!(append_text("", "ab\x08\x08\x08c".as_bytes(), 100), "c");
    }

    #[test]
    fn form_feed_clears_everything_before_it() {
        assert_eq!(append_text("", b"draft one\x0cfinal", 100), "final");
        assert_eq!(append_text("", b"a\x0cb\x0cc", 100), "c");
        assert_eq!(append_text("", b"abc\x0c", 100), "");
    }

    #[test]
    fn edits_never_remove_body_text() {
        assert_eq!(append_text("Plan:\n", b"\x08\x08\x08", 100), "Plan:\n");
        assert_eq!(append_text("Plan:\n", b"x\x0c", 100), "Plan:\n");
        assert_eq!(
            append_text("Plan:\n", b"\x0c1.\x08\x082.", 100),
            "Plan:\n2."
        );
    }

    #[test]
    fn backspace_at_the_start_of_the_stream_is_a_no_op() {
        assert_eq!(append_text("", b"\x08\x08hi", 100), "hi");
        assert_eq!(append_text("", b"\x0c\x08hi", 100), "hi");
    }

    #[test]
    fn edits_are_the_same_however_the_stream_is_split() {
        let raw = "a😀\x08é€\x0cxé\x08y\u{fffd}z".as_bytes();
        let whole = append_text("", raw, 100);
        for split in 0..=raw.len() {
            let mut buffer = AppendBuffer::new(100);
            let mut kept = buffer.accept(&raw[..split], false);
            kept.extend(buffer.accept(&raw[split..], true));
            assert_eq!(append_text("", &kept, 100), whole, "split at {split}");
        }
        assert_eq!(whole, "xy\u{fffd}z");
    }

    // --- append budget (§4.8.5) ---

    #[test]
    fn the_kept_text_ends_on_a_whole_character_within_the_budget() {
        // "é" is two bytes; a budget of 4 holds "aé", not half of the next "é".
        let mut buffer = AppendBuffer::new(4);
        let kept = buffer.accept("aééé".as_bytes(), false);
        assert!(buffer.full, "the text ends when the budget is full");
        assert_eq!(kept, "aé".as_bytes());
        assert_eq!(append_text("base", "aééé".as_bytes(), 4), "baseaé");

        let mut rng = Rng(42);
        let alphabet = ["a", "é", "€", "😀", "\x08", "\x0c"];
        for _ in 0..1_000 {
            let raw: String = (0..rng.below(30))
                .map(|_| alphabet[rng.below(alphabet.len())])
                .collect();
            let budget = rng.below(20);
            let appended = append_text("", raw.as_bytes(), budget);
            assert!(
                appended.len() <= budget,
                "{raw:?} budget {budget}: {appended:?}"
            );
        }
    }

    #[test]
    fn a_zero_budget_keeps_nothing() {
        let mut buffer = AppendBuffer::new(0);
        assert!(buffer.full);
        assert!(buffer.accept(b"anything", true).is_empty());
    }

    // --- decoding: WHATWG UTF-8 decode without BOM ---

    #[test]
    fn invalid_bytes_become_replacement_characters_per_maximal_subpart() {
        let cases: &[(&[u8], &str)] = &[
            (b"a\xffb", "a\u{fffd}b"),
            // A truncated 3-byte sequence is one U+FFFD.
            (b"\xe2\x82z", "\u{fffd}z"),
            // F0 80 is never valid, so each byte is its own U+FFFD.
            (b"\xf0\x80\x80", "\u{fffd}\u{fffd}\u{fffd}"),
            // Surrogates encoded in UTF-8 are invalid.
            (b"\xed\xa0\x80", "\u{fffd}\u{fffd}\u{fffd}"),
            // A truncated sequence at the end of the stream is one U+FFFD.
            (b"ok\xf0\x9f\x98", "ok\u{fffd}"),
        ];
        for (raw, expected) in cases {
            assert_eq!(append_text("", raw, 100), *expected, "append {raw:?}");
            assert_eq!(terminal_output(raw, 100), *expected, "terminal {raw:?}");
        }
    }

    #[test]
    fn a_leading_bom_is_kept() {
        assert_eq!(append_text("", b"\xef\xbb\xbfhi", 100), "\u{feff}hi");
        assert_eq!(terminal_output(b"\xef\xbb\xbfhi", 100), "\u{feff}hi");
    }
}
