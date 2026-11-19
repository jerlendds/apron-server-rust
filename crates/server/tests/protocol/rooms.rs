use super::support::*;
use serde_json::{Value, json};

#[test]
#[ignore = "requires APRON_TEST_WS with rooms"]
fn listing_filters_threads_members_and_join_leave_updates() {
    let mut owner = Peer::open();
    owner.cap("rooms");
    let user = owner.guest()["you"]["user_id"].clone();
    let title = unique("parent");
    let room = owner.room(json!({"title":title,"description":"kept"}));
    let thread = owner.room(json!({"parent_room_id":room,"title":"Thread"}));
    let mut other = Peer::open();
    let other_user = other.guest()["you"]["user_id"].clone();
    let list = other.call("room_list", json!({"filter":"not_joined"})).ok();
    array(&list["not_joined"]);
    assert!(list.get("joined").is_none());
    assert!(!has_room(&list, "not_joined", &thread));
    other.join(&room);
    let update = owner.take("room_update", |p| {
        items(&p["memberships"]).iter().any(|r| {
            r["room_id"] == room
                && items(&r["members"])
                    .iter()
                    .any(|m| m["user"]["user_id"] == other_user && m["joined"] == true)
        })
    });
    assert!(!has_room(&update, "joined", &room));
    let listing = other
        .call(
            "room_list",
            json!({"filter":"not_joined","parent_room_id":room}),
        )
        .ok();
    assert!(has_room(&listing, "not_joined", &thread));

    let out = other
        .call("room_list", json!({"filter":"joined","members":true}))
        .ok();
    let record = array(&out["joined"])
        .iter()
        .find(|r| r["room_id"] == room)
        .unwrap();
    for member in [&user, &other_user] {
        assert!(
            array(&record["members"])
                .iter()
                .any(|entry| &entry["user_id"] == member)
        );
    }
    if owner.has_cap("history") {
        log_id(&record["log_id"]);
        log_id(&record["latest_log_id"]);
        log_id(&record["history_log_id"]);
    }
    let changed = owner.call("room_set", json!({"room_id":room,"title":"Renamed"}));
    let record = changed.before("room_update", |p| has_room(p, "updated", &room));
    let record = array(&record["updated"])
        .iter()
        .find(|r| r["room_id"] == room)
        .unwrap();
    assert_eq!(record["description"], "kept");
    assert_eq!(changed.ok()["room_id"], room);

    let leave = other.call("room_leave", json!({"room_id":room}));
    let left = leave.before("room_update", |p| has_room(p, "left", &room));
    assert!(array(&left["memberships"]).iter().any(|r| {
        r["room_id"] == room
            && items(&r["members"])
                .iter()
                .any(|m| m["user"]["user_id"] == other_user && m["joined"] == false)
    }));
    assert_eq!(leave.ok(), json!({}));
    let listing = other.call("room_list", json!({"filter":"joined"})).ok();
    assert!(!has_room(&listing, "joined", &room));
    assert!(!has_room(&listing, "joined", &thread));
    assert!(
        array(&listing["joined"])
            .iter()
            .all(|r| r.get("members").is_none())
    );
    assert!(listing.get("users").is_none());
}

#[test]
#[ignore = "requires APRON_TEST_WS with rooms, private rooms and history"]
fn private_rooms_and_threads_do_not_leak_and_missing_ids_are_indistinguishable() {
    let mut owner = Peer::open();
    owner.cap("rooms");
    owner.cap("history");
    owner.guest();
    let room = owner.room(json!({"title":unique("private"),"private":true}));
    let thread = owner.room(json!({"title":"Inherited private thread","parent_room_id":room}));
    let notice = owner.take("room_update", |p| has_room(p, "joined", &thread));
    assert!(
        array(&notice["joined"])
            .iter()
            .any(|r| r["room_id"] == thread && r["private"] == true)
    );

    let mut other = Peer::open();
    other.guest();
    let message = owner.post(json!({"room_id":room,"body":{"text":"members only"}}));
    let list = other.call("room_list", json!({})).ok();
    for target in [&room, &thread] {
        assert!(!has_room(&list, "joined", target) && !has_room(&list, "not_joined", target));
    }
    let missing = unique("missing");
    for target in [&room, &thread, &missing] {
        other
            .call("room_join", json!({"room_id":target}))
            .error(INVALID);
        other
            .call("history", json!({"room_id":target}))
            .error(INVALID);
        other
            .call(
                "message",
                json!({"room_id":target,"body":{"text":"invalid"}}),
            )
            .error(INVALID);
        other
            .call(
                "room_set",
                json!({"parent_room_id":target,"title":"invalid"}),
            )
            .error(INVALID);
    }
    other
        .call(
            "message",
            json!({"body":{"text":"cannot quote a hidden message"},
        "reply_to":{"message_id":message["message_id"]}}),
        )
        .error(INVALID);
    other.fence();
    assert!(
        !other
            .inbox
            .iter()
            .any(|f| f["params"]["message_id"] == message["message_id"])
    );
}

#[test]
#[ignore = "requires APRON_TEST_WS allowing posts to visible rooms without joining"]
fn posting_without_joining_delivers_to_members() {
    let mut member = Peer::open();
    member.cap("rooms");
    member.guest();
    let room = member.room(json!({"title":unique("post-without-joining")}));
    let mut poster = Peer::open();
    let user = poster.guest()["you"]["user_id"].clone();
    let reply = poster
        .call(
            "message",
            json!({"room_id":room,"body":{"text":"from outside"}}),
        )
        .ok();
    let snapshot = member.take("message", |p| p["message_id"] == reply["message_id"]);
    assert_eq!(snapshot["from"]["user_id"], user);
    let joined = poster.call("room_list", json!({"filter":"joined"})).ok();
    if !has_room(&joined, "joined", &room) {
        assert!(
            !poster
                .inbox
                .iter()
                .any(|f| f["params"]["message_id"] == reply["message_id"])
        );
    }
}

#[test]
#[ignore = "requires APRON_TEST_WS with history, rooms, edit and reactions"]
fn history_paginates_mixed_records_with_inclusive_bounds() {
    let mut peer = Peer::open();
    for cap in ["rooms", "history", "edit", "reactions"] {
        peer.cap(cap);
    }
    peer.guest();
    let room = peer.room(json!({"title":unique("history")}));
    let message = peer.post(json!({"room_id":room,"body":{"text":"first"}}));
    let id = &message["message_id"];
    let first = log_id(id);
    let reaction = peer.call("reactions", json!({"message_id":id,"emojis":["👍"]}));
    let middle = log_id(&reaction.before("reactions", |p| p["message_id"] == *id)["log_id"]);
    reaction.ok();
    let edit = peer.call(
        "message",
        json!({"room_id":room,"message_id":id,"body":{"text":"latest"}}),
    );
    let last = log_id(&edit.before("message", |p| p["message_id"] == *id)["log_id"]);
    edit.ok();
    assert!(first < middle && middle < last);

    let mut after = first;
    for (expected, kind) in [
        (first, "messages"),
        (middle, "reactions"),
        (last, "messages"),
    ] {
        let page = history(
            &mut peer,
            json!({"room_id":room,"after":after.to_string(),
            "before":last.to_string(),"limit":1}),
        );
        assert_eq!(log_id(&page["first_log_id"]), expected);
        assert_eq!(log_id(&page["last_log_id"]), expected);
        assert_eq!(page["more"], expected != last);
        assert_eq!(log_id(&array(&page[kind])[0]["log_id"]), expected);
        assert!(log_id(&page["latest_log_id"]) >= last);
        assert!(log_id(&page["history_log_id"]) <= first);
        after = expected + 1;
    }
    let backwards = history(
        &mut peer,
        json!({"room_id":room,"before":last.to_string(),"limit":2}),
    );
    assert_eq!(log_id(&backwards["first_log_id"]), middle);
    assert_eq!(log_id(&backwards["last_log_id"]), last);
    assert_eq!(backwards["more"], true);

    let full = history(
        &mut peer,
        json!({"room_id":room,"after":first.to_string(),"before":last.to_string()}),
    );
    assert_eq!(log_id(&full["first_log_id"]), first);
    assert_eq!(log_id(&full["last_log_id"]), last);
    assert_eq!(
        array(&full["messages"]).last().unwrap()["body"]["text"],
        "latest"
    );
    let empty = history(
        &mut peer,
        json!({"room_id":room,"after":(last+1).to_string()}),
    );
    assert_eq!(empty["more"], false);
    assert!(empty.get("first_log_id").is_none() && empty.get("last_log_id").is_none());
    for kind in ["rooms", "messages", "reactions", "memberships"] {
        assert!(items(&empty[kind]).is_empty());
    }
}

pub fn history(peer: &mut Peer, params: Value) -> Value {
    let page = peer.call("history", params).ok();
    assert!(page["more"].is_boolean());
    assert!(page.get("history_log_id").is_some());
    log_id(&page["latest_log_id"]);
    for key in ["rooms", "messages", "reactions", "memberships"] {
        assert!(
            items(&page[key])
                .windows(2)
                .all(|pair| log_id(&pair[0]["log_id"]) < log_id(&pair[1]["log_id"]))
        );
    }
    page
}

#[test]
#[ignore = "requires APRON_TEST_WS with history, edit, reactions and room membership"]
fn replay_keeps_latest_message_and_empty_reaction_and_membership_states() {
    let mut owner = Peer::open();
    for cap in ["rooms", "history", "edit", "reactions"] {
        owner.cap(cap);
    }
    owner.guest();
    let room = owner.room(json!({"title":unique("replay")}));
    let mut member = Peer::open();
    let member_id = member.guest()["you"]["user_id"].clone();
    member.join(&room);
    let initial = owner.post(json!({"room_id":room,"body":{"text":"original"}}));
    let message = &initial["message_id"];
    let middle = owner.call("reactions", json!({"message_id":message,"emojis":["👍"]}));
    let start = log_id(&middle.before("reactions", |p| p["message_id"] == *message)["log_id"]);
    middle.ok();
    owner
        .call("reactions", json!({"message_id":message,"emojis":[]}))
        .ok();
    member.call("room_leave", json!({"room_id":room})).ok();
    let delete = owner.call(
        "message",
        json!({"room_id":room,"message_id":message,"deleted":true}),
    );
    let end = log_id(&delete.before("message", |p| p["message_id"] == *message)["log_id"]);
    delete.ok();

    let page = history(
        &mut owner,
        json!({"room_id":room,"after":start.to_string(),"before":end.to_string()}),
    );
    assert_eq!(log_id(&page["first_log_id"]), start);
    assert_eq!(log_id(&page["last_log_id"]), end);
    assert_eq!(page["messages"][0]["deleted"], true);
    assert_eq!(
        array(&page["reactions"]).last().unwrap()["reactions"][0]["emojis"],
        json!([])
    );
    let leave = array(&page["memberships"]).last().unwrap();
    assert!(
        array(&leave["members"])
            .iter()
            .any(|m| m["user"]["user_id"] == member_id && m["joined"] == false)
    );
}

#[test]
#[ignore = "requires APRON_TEST_WS with rooms and history"]
fn thread_updates_reach_parent_members_and_thread_parent_is_immutable() {
    let mut creator = Peer::open();
    creator.cap("rooms");
    creator.guest();
    let root = creator.room(json!({"title":unique("root")}));
    let alternative = creator.room(json!({"title":unique("alternative")}));
    let mut parent_member = Peer::open();
    parent_member.guest();
    parent_member.join(&root);
    let thread = creator.room(json!({"title":unique("thread"),"parent_room_id":root}));
    let notice = parent_member.take("room_update", |p| has_room(p, "updated", &thread));
    assert!(!has_room(&notice, "joined", &thread));
    assert!(
        array(&notice["updated"])
            .iter()
            .any(|r| r["room_id"] == thread && r["parent_room_id"] == root)
    );

    let change = creator.call(
        "room_set",
        json!({"room_id":thread,"parent_room_id":alternative}),
    );
    if change.frame.get("error").is_some() {
        change.error(INVALID);
    } else {
        assert_eq!(change.ok()["room_id"], thread);
    }
    let children = parent_member
        .call(
            "room_list",
            json!({"parent_room_id":root,"filter":"not_joined"}),
        )
        .ok();
    assert!(has_room(&children, "not_joined", &thread));

    let message = creator.post(json!({"room_id":thread,"body":{"text":"thread only"}}));
    parent_member.fence();
    assert!(
        !parent_member
            .inbox
            .iter()
            .any(|f| f["params"]["message_id"] == message["message_id"]
                || has_room(&f["params"], "updated", &thread))
    );
}

#[test]
#[ignore = "requires APRON_TEST_WS with rooms, edit and a private-room capable profile"]
fn a_message_cannot_move_to_a_room_hidden_from_its_author() {
    let mut writer = Peer::open();
    writer.cap("edit");
    writer.cap("rooms");
    writer.guest();
    let original_room = writer.room(json!({"title":unique("visible-source")}));
    let message = writer.post(json!({"room_id":original_room,"body":{"text":"original"}}));
    let mut other = Peer::open();
    other.guest();
    let private = other.room(json!({"title":unique("hidden-destination"),"private":true}));
    writer
        .call(
            "message",
            json!({"message_id":message["message_id"],"room_id":private,
        "body":{"text":"would move"}}),
        )
        .error(INVALID);
    other.fence();
    assert!(
        !other
            .inbox
            .iter()
            .any(|f| f["params"]["message_id"] == message["message_id"])
    );
}
