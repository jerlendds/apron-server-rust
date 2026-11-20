use crate::{Record, State};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl State {
    pub fn next_log_id(&mut self) -> Option<u64> {
        let id = epoch_millis().max(self.watermark.checked_add(1)?);
        if id >= (1 << 53) {
            return None;
        }
        self.watermark = id;
        Some(id)
    }

    pub fn log(&mut self, id: u64, kind: &str, rooms: Vec<String>, value: serde_json::Value) {
        for room in &rooms {
            self.room_log.entry(room.clone()).or_default().insert(id);
        }
        self.records.insert(
            id,
            Record {
                kind: kind.into(),
                rooms,
                value,
            },
        );
    }
}
