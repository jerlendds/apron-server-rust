use crate::{Record, State};

impl State {
    pub(crate) fn rebuild_room_index(&mut self) {
        self.room_log.clear();
        for (&id, record) in &self.records {
            for room in &record.rooms {
                self.room_log.entry(room.clone()).or_default().insert(id);
            }
        }
    }

    pub fn room_bounds(&self, room: &str) -> (Option<u64>, u64) {
        match self.room_log.get(room) {
            Some(ids) => (
                ids.first().copied(),
                ids.last().copied().unwrap_or(self.watermark),
            ),
            None => (None, self.watermark),
        }
    }

    pub fn room_records(
        &self,
        room: &str,
        start: u64,
        end: u64,
    ) -> impl DoubleEndedIterator<Item = (u64, &Record)> + '_ {
        self.room_log
            .get(room)
            .into_iter()
            .flat_map(move |entries| entries.range(start..=end))
            .filter_map(|&id| self.records.get(&id).map(|record| (id, record)))
    }
}
