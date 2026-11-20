//! §4.3 `history` and §4.4 moves: previous-record links, the room-less log,
//! and the reactions record that follows a moved message.
use super::rooms::history;
use super::support::*;
use serde_json::{Value, json};

/// Saves `params` as an edit and returns the snapshot delivered to the
/// caller.
fn save(peer: &mut Peer, params: Value) -> Value {
    let id = params["message_id"].clone();
    let exchange = peer.call("message", params);
    let snapshot = exchange
        .before("message", |p| p["message_id"] == id)
        .clone();
    assert_eq!(exchange.ok()["message_id"], id);
    snapshot
}

/// The one record at `position` in `room`, as `history` returns it when both
/// bounds are that position. Asserts that the page holds nothing else.
fn exactly(peer: &mut Peer, room: &str, position: &Value, kind: &str) -> Value {
    let page = history(
        peer,
        json!({"room_id":room,"after":position,"before":position}),
    );
    assert_eq!(page["first_log_id"], *position);
    assert_eq!(page["last_log_id"], *position);
    assert_eq!(page["more"], false);
    for other in ["rooms", "messages", "reactions", "memberships"] {
        let expected = usize::from(other == kind);
        assert_eq!(items(&page[other]).len(), expected, "{other} in {page}");
    }
    page[kind][0].clone()
}

fn react(peer: &mut Peer, message: &Value, emojis: Value) {
    peer.call("reactions", json!({"message_id":message,"emojis":emojis}))
        .ok();
}

// --- previous records (§4.3.1) ---------------------------------------------

#[test]
#[ignore = "requires APRON_TEST_WS with rooms, history and edit"]
fn each_edit_points_to_the_previous_snapshot_that_history_returns() {
    let mut peer = Peer::open();
    for cap in ["rooms", "history", "edit"] {
        peer.cap(cap);
    }
    peer.guest();
    let room = peer.room(json!({"title":unique("prev-log-id")}));
    let first = peer.post(json!({"room_id":room,"body":{"text":"one"}}));
    let message = first["message_id"].clone();
    assert!(
        first.get("prev_log_id").is_none(),
        "a new message has no previous record: {first}"
    );

    let mut previous = first;
    for text in ["two", "three"] {
        let snapshot = save(
            &mut peer,
            json!({"room_id":room,"message_id":message,"body":{"text":text}}),
        );
        // Room records and snapshots MAY carry `prev_log_id`; this server does.
        assert_eq!(snapshot["prev_log_id"], previous["log_id"], "{snapshot}");
        assert!(
            snapshot.get("prev_room_id").is_none(),
            "an edit in place is not a move: {snapshot}"
        );
        let earlier = exactly(&mut peer, &room, &snapshot["prev_log_id"], "messages");
        assert_eq!(earlier["log_id"], previous["log_id"]);
        assert_eq!(earlier["body"], previous["body"]);
        previous = snapshot;
    }
}

#[test]
#[ignore = "requires APRON_TEST_WS with rooms and history"]
fn a_room_set_points_to_the_previous_room_record() {
    let mut peer = Peer::open();
    peer.cap("history");
    peer.guest();
    let title = unique("room-prev-before");
    let room = peer.room(json!({"title":title}));
    let listed = peer.call("room_list", json!({"room_id":room})).ok();
    let created = items(&listed["joined"])
        .iter()
        .find(|r| r["room_id"] == room)
        .unwrap()["log_id"]
        .clone();
    log_id(&created);

    let exchange = peer.call(
        "room_set",
        json!({"room_id":room,"title":unique("room-prev-after")}),
    );
    let update = exchange
        .before("room_update", |p| has_room(p, "updated", &room))
        .clone();
    exchange.ok();
    let record = items(&update["updated"])
        .iter()
        .find(|r| r["room_id"] == room)
        .unwrap();
    assert_eq!(record["prev_log_id"], created, "{record}");

    let earlier = exactly(&mut peer, &room, &created, "rooms");
    assert_eq!(earlier["title"], title.as_str());
    assert_eq!(earlier["log_id"], created);
}

// --- moves (§4.4) -------------------------------------------------------------

#[test]
#[ignore = "requires APRON_TEST_WS with rooms, history, edit and reactions"]
fn a_moved_message_points_to_its_source_and_brings_only_nonempty_reactions() {
    let mut author = Peer::open();
    for cap in ["rooms", "history", "edit", "reactions"] {
        author.cap(cap);
    }
    author.guest();
    let source = author.room(json!({"title":unique("move-source")}));
    let destination = author.room(json!({"title":unique("move-destination")}));
    let mut fan = Peer::open();
    let fan_id = fan.guest()["you"]["user_id"].clone();
    let mut fickle = Peer::open();
    let fickle_id = fickle.guest()["you"]["user_id"].clone();
    let mut observer = Peer::open();
    observer.guest();
    for peer in [&mut fan, &mut fickle] {
        peer.join(&source);
        peer.join(&destination);
    }
    observer.join(&destination);

    let posted = author.post(json!({"room_id":source,"body":{"text":"wandering"}}));
    let message = posted["message_id"].clone();
    react(&mut fan, &message, json!(["👍"]));
    react(&mut fickle, &message, json!(["🎉"]));
    react(&mut fickle, &message, json!([]));
    observer.fence();
    observer.inbox.clear();

    let moved = save(
        &mut author,
        json!({"room_id":destination,"message_id":message,"body":{"text":"wandering"}}),
    );
    assert_eq!(moved["room_id"], destination.as_str());
    assert_eq!(moved["prev_room_id"], source.as_str(), "{moved}");
    assert_eq!(moved["prev_log_id"], posted["log_id"], "{moved}");

    // The previous snapshot stays in the source room, and only there.
    let earlier = exactly(&mut author, &source, &moved["prev_log_id"], "messages");
    assert_eq!(earlier["room_id"], source.as_str());
    let elsewhere = history(
        &mut author,
        json!({"room_id":destination,"after":moved["prev_log_id"],"before":moved["prev_log_id"]}),
    );
    assert!(items(&elsewhere["messages"]).is_empty(), "{elsewhere}");

    // The move snapshot is in both rooms' logs (§4.3.1).
    for room in [&source, &destination] {
        exactly(&mut author, room, &moved["log_id"], "messages");
    }

    // One reactions record follows in the destination, with every non-empty
    // set and no empty one.
    let after_move = json!((log_id(&moved["log_id"]) + 1).to_string());
    let page = history(
        &mut author,
        json!({"room_id":destination,"after":after_move}),
    );
    let records = items(&page["reactions"])
        .iter()
        .filter(|r| r["message_id"] == message)
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 1, "{page}");
    let record = records[0];
    assert_eq!(record["room_id"], destination.as_str());
    let sets = array(&record["reactions"]);
    assert_eq!(sets.len(), 1, "{record}");
    assert_eq!(sets[0]["from"]["user_id"], fan_id);
    assert_eq!(sets[0]["emojis"], json!(["👍"]));
    assert!(sets.iter().all(|s| s["from"]["user_id"] != fickle_id));

    // It is logged in the destination only.
    let source_after = history(&mut author, json!({"room_id":source,"after":after_move}));
    assert!(
        items(&source_after["reactions"])
            .iter()
            .all(|r| r["message_id"] != message),
        "{source_after}"
    );

    // A destination member receives the snapshot and the same record.
    let snapshot = observer.take("message", |p| p["message_id"] == message);
    assert_eq!(snapshot["log_id"], moved["log_id"]);
    let live = observer.take("reactions", |p| p["message_id"] == message);
    assert_eq!(live, *record);
}

// --- the room-less log (§4.3) -------------------------------------------------

#[test]
#[ignore = "requires APRON_TEST_WS with rooms, history and private rooms"]
fn history_without_room_id_merges_received_rooms_only() {
    let mut reader = Peer::open();
    reader.cap("rooms");
    reader.cap("history");
    reader.guest();
    let mut other = Peer::open();
    other.guest();
    let first = reader.room(json!({"title":unique("merged-a")}));
    let second = reader.room(json!({"title":unique("merged-b")}));
    let unjoined = other.room(json!({"title":unique("merged-public")}));
    let hidden = other.room(json!({"title":unique("merged-private"),"private":true}));
    for room in [&first, &second] {
        other.join(room);
    }

    let posts = [
        (
            other.post(json!({"room_id":first,"body":{"text":"a1"}})),
            true,
        ),
        (
            other.post(json!({"room_id":unjoined,"body":{"text":"c"}})),
            false,
        ),
        (
            other.post(json!({"room_id":second,"body":{"text":"b"}})),
            true,
        ),
        (
            other.post(json!({"room_id":hidden,"body":{"text":"d"}})),
            false,
        ),
        (
            other.post(json!({"room_id":first,"body":{"text":"a2"}})),
            true,
        ),
    ];
    let start = posts[0].0["log_id"].clone();
    let end = posts[4].0["log_id"].clone();
    let page = history(&mut reader, json!({"after":start,"before":end}));
    assert_eq!(page["first_log_id"], start);
    assert_eq!(page["last_log_id"], end);
    assert_eq!(page["more"], false);

    let received = items(&page["messages"])
        .iter()
        .map(|m| m["message_id"].clone())
        .collect::<Vec<_>>();
    let expected = posts
        .iter()
        .filter(|(_, received)| *received)
        .map(|(m, _)| m["message_id"].clone())
        .collect::<Vec<_>>();
    assert_eq!(received, expected, "one ascending log of joined rooms only");

    // Nothing in the page comes from a room the reader doesn't receive.
    let joined = reader.call("room_list", json!({"filter":"joined"})).ok();
    for kind in ["rooms", "messages", "reactions", "memberships"] {
        for record in items(&page[kind]) {
            let room = string(&record["room_id"]);
            assert!(has_room(&joined, "joined", room), "{kind} from {room}");
        }
    }
    assert!(
        !items(&joined["joined"])
            .iter()
            .any(|r| r["room_id"] == unjoined || r["room_id"] == hidden)
    );
}
