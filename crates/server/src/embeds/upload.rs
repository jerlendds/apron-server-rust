pub(super) fn safe_media_type(requested: Option<&str>, bytes: &[u8]) -> String {
    if let Some(image) = avatar_media_type(bytes) {
        return image.to_owned();
    }
    match requested {
        Some("text/plain") => "text/plain; charset=utf-8".to_owned(),
        _ => "application/octet-stream".to_owned(),
    }
}

pub(crate) fn avatar_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}
