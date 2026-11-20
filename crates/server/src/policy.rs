use store::State;

pub(crate) const DEFAULT_ROOM: &str = "general";

pub(crate) fn joined(state: &State, user: &str, room: &str) -> bool {
    state
        .members
        .get(room)
        .is_some_and(|members| members.contains(user))
}

pub(crate) fn visible(state: &State, user: &str, room: &str) -> bool {
    let mut current = Some(room);
    while let Some(id) = current {
        let Some(record) = state.rooms.get(id) else {
            return false;
        };
        if record["private"] == true && !joined(state, user, id) {
            return false;
        }
        current = record["parent_room_id"].as_str();
    }
    true
}

pub(crate) fn shares_room(state: &State, first: &str, second: &str) -> bool {
    state.members.iter().any(|(room, members)| {
        members.contains(first)
            && members.contains(second)
            && visible(state, first, room)
            && visible(state, second, room)
    })
}

pub(crate) fn user_visible(state: &State, actor: &str, target: &str) -> bool {
    actor == target
        || state.members.iter().any(|(room, members)| {
            members.contains(target) && visible(state, actor, room) && visible(state, target, room)
        })
}
