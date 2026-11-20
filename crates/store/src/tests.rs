//! Unit tests for the invariants that the protocol relies on (PROTOCOL.md §1.2,
//! §2.1, §4.3.1): log ID assignment, the per-room index, and durable commits.
use crate::{Record, Retry, State, Store, epoch_millis};
use serde_json::json;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

const MAX_LOG_ID: u64 = (1 << 53) - 1;

/// A directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let suffix = format!(
            "{}-{}-{}",
            std::process::id(),
            epoch_millis(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(format!("apron-store-{label}-{suffix}"));
        std::fs::create_dir(&path).expect("create temp dir");
        Self(path)
    }

    fn db(&self) -> String {
        self.0
            .join("apron.db")
            .to_str()
            .expect("utf-8 path")
            .to_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn path_exists(path: &str) -> bool {
    Path::new(path).exists()
}

/// Appends one record the way the server does: assign an ID, then log it.
fn append(state: &mut State, kind: &str, rooms: &[&str]) -> u64 {
    let id = state.next_log_id().expect("log ID");
    let rooms = rooms.iter().map(|room| room.to_string()).collect();
    state.log(id, kind, rooms, json!({"log_id": id.to_string()}));
    id
}

fn ids(state: &State, room: &str, start: u64, end: u64) -> Vec<u64> {
    state
        .room_records(room, start, end)
        .map(|(id, _)| id)
        .collect()
}

// --- log_id assignment (§2.1) ---------------------------------------------

#[test]
fn log_ids_strictly_increase_within_one_millisecond() {
    let mut state = State::default();
    let before = epoch_millis();
    let issued: Vec<u64> = (0..10_000).map(|_| state.next_log_id().unwrap()).collect();
    assert!(issued.windows(2).all(|pair| pair[0] < pair[1]));
    // 10k IDs in far less than 10k ms means many shared a millisecond and
    // were bumped to last + 1.
    assert!(issued.windows(2).any(|pair| pair[1] == pair[0] + 1));
    assert!(issued[0] >= before, "the first ID is the commit time");
    assert_eq!(state.watermark, *issued.last().unwrap());
}

#[test]
fn log_id_follows_the_watermark_when_the_clock_is_behind() {
    let mut state = State {
        watermark: epoch_millis() + 3_600_000,
        ..State::default()
    };
    let previous = state.watermark;
    assert_eq!(state.next_log_id(), Some(previous + 1));
    assert_eq!(state.next_log_id(), Some(previous + 2));
}

#[test]
fn log_ids_stay_below_2_pow_53() {
    let mut state = State {
        watermark: MAX_LOG_ID - 1,
        ..State::default()
    };
    assert_eq!(state.next_log_id(), Some(MAX_LOG_ID));
    assert_eq!(state.next_log_id(), None, "2^53 is out of range");
    assert_eq!(state.watermark, MAX_LOG_ID, "a refused ID does not advance");

    let mut state = State {
        watermark: u64::MAX,
        ..State::default()
    };
    assert_eq!(state.next_log_id(), None, "no overflow at u64::MAX");
}

#[test]
fn concurrent_appends_never_duplicate_or_reorder() {
    // The server serializes appends behind one lock; the log must stay a
    // strictly increasing sequence in commit order however threads interleave.
    let state = Arc::new(Mutex::new(State::default()));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let state = Arc::clone(&state);
            thread::spawn(move || {
                for _ in 0..500 {
                    append(&mut state.lock().unwrap(), "message", &["general"]);
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let state = state.lock().unwrap();
    let logged: Vec<u64> = state.records.keys().copied().collect();
    assert_eq!(logged.len(), 4_000, "no ID was issued twice");
    assert_eq!(ids(&state, "general", 0, u64::MAX), logged);
    assert_eq!(state.watermark, *logged.last().unwrap());
}

// --- the per-room log (§4.3.1) --------------------------------------------

#[test]
fn room_records_are_inclusive_ascending_and_scoped_to_the_room() {
    let mut state = State::default();
    let a1 = append(&mut state, "message", &["a"]);
    let b1 = append(&mut state, "message", &["b"]);
    let a2 = append(&mut state, "reactions", &["a"]);
    let a3 = append(&mut state, "memberships", &["a"]);

    assert_eq!(ids(&state, "a", 0, u64::MAX), [a1, a2, a3]);
    assert_eq!(ids(&state, "b", 0, u64::MAX), [b1]);
    assert_eq!(ids(&state, "a", a1, a2), [a1, a2], "both bounds inclusive");
    assert_eq!(ids(&state, "a", a2, a2), [a2], "a single-ID window");
    assert_eq!(ids(&state, "a", b1, b1), [] as [u64; 0]);
    let reversed: Vec<u64> = state
        .room_records("a", 0, u64::MAX)
        .rev()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(reversed, [a3, a2, a1]);
}

#[test]
fn a_moved_message_is_in_the_log_of_both_rooms() {
    let mut state = State::default();
    let created = append(&mut state, "message", &["source"]);
    let moved = append(&mut state, "message", &["source", "destination"]);
    let after = append(&mut state, "reactions", &["destination"]);

    assert_eq!(ids(&state, "source", 0, u64::MAX), [created, moved]);
    assert_eq!(ids(&state, "destination", 0, u64::MAX), [moved, after]);
}

#[test]
fn room_bounds_report_the_first_record_and_latest_position() {
    let mut state = State::default();
    let first = append(&mut state, "rooms", &["a"]);
    append(&mut state, "message", &["b"]);
    let last = append(&mut state, "message", &["a"]);
    append(&mut state, "message", &["b"]);

    assert_eq!(state.room_bounds("a"), (Some(first), last));
    assert_eq!(
        state.room_bounds("unknown"),
        (None, state.watermark),
        "an empty room has no history and the server's watermark"
    );
}

#[test]
fn rebuilding_the_room_index_matches_live_logging() {
    let mut state = State::default();
    append(&mut state, "message", &["a"]);
    append(&mut state, "message", &["a", "b"]);
    append(&mut state, "memberships", &["b"]);
    let live = (ids(&state, "a", 0, u64::MAX), ids(&state, "b", 0, u64::MAX));
    state.room_log.clear();
    state.rebuild_room_index();
    assert_eq!(
        (ids(&state, "a", 0, u64::MAX), ids(&state, "b", 0, u64::MAX)),
        live
    );
}

// --- durable commits ------------------------------------------------------

#[tokio::test]
async fn memory_store_round_trips_the_committed_state() {
    let mut store = Store::memory();
    assert_eq!(store.load().await.unwrap().watermark, 0);
    let mut state = State::default();
    let id = append(&mut state, "message", &["general"]);
    store.commit(&state).await.unwrap();
    let loaded = store.load().await.unwrap();
    assert_eq!(loaded.watermark, id);
    assert_eq!(ids(&loaded, "general", 0, u64::MAX), [id]);
}

#[tokio::test]
async fn fresh_database_is_migrated_and_loads_empty() {
    let dir = TempDir::new("fresh");
    let mut store = Store::open(&dir.db()).await.unwrap();
    assert!(path_exists(&dir.db()));
    let state = store.load().await.unwrap();
    assert_eq!(state.watermark, 0);
    assert!(state.records.is_empty());
    // Both tables exist, so the first commit succeeds.
    store.commit(&state).await.unwrap();
}

#[tokio::test]
async fn migrations_are_idempotent() {
    let dir = TempDir::new("migrate");
    let db = turso::Builder::new_local(&dir.db()).build().await.unwrap();
    let connection = db.connect().unwrap();
    crate::migrations::apply(&connection).await.unwrap();
    connection
        .execute("INSERT INTO apron_log VALUES (1, 'kept')", ())
        .await
        .unwrap();
    crate::migrations::apply(&connection).await.unwrap();
    let mut rows = connection
        .query("SELECT payload FROM apron_log", ())
        .await
        .unwrap();
    let row = rows.next().await.unwrap().expect("row survives");
    assert_eq!(row.get::<String>(0).unwrap(), "kept");
    assert!(rows.next().await.unwrap().is_none());
}

#[tokio::test]
async fn reopen_restores_records_watermark_index_and_retries() {
    let dir = TempDir::new("reopen");
    let mut state = State::default();
    let created = append(&mut state, "message", &["source"]);
    let moved = append(&mut state, "message", &["source", "destination"]);
    state.retries.entry("ada".into()).or_default().insert(
        "c1".into(),
        Retry {
            method: "message".into(),
            params: json!("fingerprint"),
            result: json!({"message_id": created.to_string()}),
            committed_at: 42,
        },
    );
    {
        let mut store = Store::open(&dir.db()).await.unwrap();
        store.load().await.unwrap();
        store.commit(&state).await.unwrap();
    }

    let mut store = Store::open(&dir.db()).await.unwrap();
    let mut loaded = store.load().await.unwrap();
    assert_eq!(loaded.watermark, moved);
    assert_eq!(
        loaded.records.keys().copied().collect::<Vec<_>>(),
        [created, moved]
    );
    assert_eq!(ids(&loaded, "source", 0, u64::MAX), [created, moved]);
    assert_eq!(ids(&loaded, "destination", 0, u64::MAX), [moved]);
    let retry = &loaded.retries["ada"]["c1"];
    assert_eq!(retry.method, "message");
    assert_eq!(retry.result["message_id"], created.to_string());
    assert!(loaded.next_log_id().unwrap() > moved);
}

#[tokio::test]
async fn reopen_continues_after_a_watermark_ahead_of_the_clock() {
    let dir = TempDir::new("clock");
    let ahead = epoch_millis() + 3_600_000;
    {
        let mut store = Store::open(&dir.db()).await.unwrap();
        let mut state = store.load().await.unwrap();
        state.watermark = ahead;
        append(&mut state, "message", &["general"]);
        store.commit(&state).await.unwrap();
    }
    let mut store = Store::open(&dir.db()).await.unwrap();
    let mut state = store.load().await.unwrap();
    assert_eq!(state.next_log_id(), Some(ahead + 2));
}

#[tokio::test]
async fn successive_commits_append_only_new_records() {
    let dir = TempDir::new("append");
    let mut store = Store::open(&dir.db()).await.unwrap();
    let mut state = store.load().await.unwrap();
    let mut expected = BTreeSet::new();
    for _ in 0..3 {
        // A re-insert of an already committed ID would hit the primary key.
        expected.insert(append(&mut state, "message", &["general"]));
        expected.insert(append(&mut state, "reactions", &["general"]));
        store.commit(&state).await.unwrap();
    }
    // Commits that log nothing still persist the projections.
    state.next_guest = 7;
    store.commit(&state).await.unwrap();

    drop(store);
    let mut store = Store::open(&dir.db()).await.unwrap();
    let loaded = store.load().await.unwrap();
    assert_eq!(
        loaded.records.keys().copied().collect::<BTreeSet<_>>(),
        expected
    );
    assert_eq!(loaded.next_guest, 7);
    let kinds: Vec<&str> = loaded
        .records
        .values()
        .map(|r: &Record| r.kind.as_str())
        .collect();
    assert_eq!(kinds, ["message", "reactions"].repeat(3));
}
