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
    // Policy here: posting does not join the poster. The poster gets
    // neither a `room_update` `joined` nor the broadcast; only the result.
    let joined = poster.call("room_list", json!({"filter":"joined"})).ok();
    assert!(!has_room(&joined, "joined", &room), "the poster was joined");
    poster.fence();
    assert!(
        !poster
            .inbox
            .iter()
            .any(|f| f["params"]["message_id"] == reply["message_id"]
                || (event(f, "room_update") && has_room(&f["params"], "joined", &room))),
        "{:?}",
        poster.inbox
    );
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

    // `parent_room_id` is fixed at creation; changing it is invalid_params.
    creator
        .call(
            "room_set",
            json!({"room_id":thread,"parent_room_id":alternative}),
        )
        .error(INVALID);
    let listed = creator.call("room_list", json!({"room_id":thread})).ok();
    let record = items(&listed["joined"])
        .iter()
        .find(|r| r["room_id"] == thread)
        .unwrap();
    assert_eq!(record["parent_room_id"], root.as_str(), "{record}");
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

// --- helpers --------------------------------------------------------------

fn listing(peer: &mut Peer, params: Value) -> Value {
    peer.call("room_list", params).ok()
}

fn ids(list: &Value, key: &str) -> Vec<String> {
    items(&list[key])
        .iter()
        .map(|r| string(&r["room_id"]).to_owned())
        .collect()
}

fn record<'a>(list: &'a Value, key: &str, room: &str) -> &'a Value {
    items(&list[key])
        .iter()
        .find(|r| r["room_id"] == room)
        .unwrap_or_else(|| panic!("{room} not in {key}: {list}"))
}

fn membership_of(update: &Value, room: &str, user: &Value, joined: bool) -> bool {
    items(&update["memberships"]).iter().any(|r| {
        r["room_id"] == room
            && items(&r["members"])
                .iter()
                .any(|m| m["user"]["user_id"] == *user && m["joined"] == joined)
    })
}

fn quiet(peer: &mut Peer) {
    peer.fence();
    peer.inbox.clear();
}

// --- room_list filters (§4.2.1) -------------------------------------------

#[test]
#[ignore = "requires APRON_TEST_WS with rooms and history"]
fn latest_log_id_filter_is_applied_or_ignored_consistently() {
    let mut peer = Peer::open();
    peer.cap("rooms");
    peer.cap("history");
    peer.guest();
    let quiet_room = peer.room(json!({"title":unique("quiet")}));
    let active = peer.room(json!({"title":unique("active")}));
    let leaving = peer.room(json!({"title":unique("leaving")}));
    let before = listing(&mut peer, json!({"filter":"joined"}));
    let checkpoint = items(&before["joined"])
        .iter()
        .map(|r| log_id(&r["latest_log_id"]))
        .max()
        .unwrap();

    peer.post(json!({"room_id":active,"body":{"text":"after the checkpoint"}}));
    peer.call("room_leave", json!({"room_id":leaving})).ok();
    let since = listing(
        &mut peer,
        json!({"filter":"joined","latest_log_id":checkpoint.to_string()}),
    );
    if since.get("left").is_some() {
        // Applied: only rooms that changed, and the rooms left since then.
        let joined = ids(&since, "joined");
        assert!(joined.contains(&active), "{since}");
        assert!(!joined.contains(&quiet_room), "{since}");
        for room in items(&since["joined"]) {
            assert!(log_id(&room["latest_log_id"]) > checkpoint, "{room}");
        }
        assert!(ids(&since, "left").contains(&leaving), "{since}");
        let none = listing(
            &mut peer,
            json!({"filter":"joined","latest_log_id":u64::pow(2, 52).to_string()}),
        );
        assert_eq!(none["left"], json!([]), "left is present even when empty");
    } else {
        // Ignored: a full listing, exactly as without the filter.
        let full = listing(&mut peer, json!({"filter":"joined"}));
        assert_eq!(ids(&since, "joined"), ids(&full, "joined"));
        assert!(ids(&since, "joined").contains(&quiet_room));
        assert!(!ids(&since, "joined").contains(&leaving));
    }
}

#[test]
#[ignore = "requires APRON_TEST_WS with rooms"]
fn room_id_lists_one_room_by_membership_and_overrides_parent() {
    let mut owner = Peer::open();
    owner.cap("rooms");
    owner.guest();
    let room = owner.room(json!({"title":unique("target")}));
    let elsewhere = owner.room(json!({"title":unique("elsewhere")}));
    let mut other = Peer::open();
    other.guest();

    let outside = listing(&mut other, json!({"room_id":room}));
    assert_eq!(ids(&outside, "not_joined"), vec![room.clone()]);
    assert_eq!(ids(&outside, "joined"), [] as [String; 0]);
    let overridden = listing(
        &mut other,
        json!({"room_id":room,"parent_room_id":elsewhere}),
    );
    assert_eq!(
        ids(&overridden, "not_joined"),
        vec![room.clone()],
        "room_id wins"
    );

    other.join(&room);
    let inside = listing(&mut other, json!({"room_id":room}));
    assert_eq!(ids(&inside, "joined"), vec![room.clone()]);
    assert_eq!(ids(&inside, "not_joined"), [] as [String; 0]);
    let joined_only = listing(&mut other, json!({"room_id":room,"filter":"not_joined"}));
    assert_eq!(ids(&joined_only, "not_joined"), [] as [String; 0]);
    assert!(joined_only.get("joined").is_none());
}

#[test]
#[ignore = "requires APRON_TEST_WS with rooms"]
fn user_id_with_members_lists_only_that_user_or_is_ignored() {
    let mut owner = Peer::open();
    owner.cap("rooms");
    let owner_id = owner.guest()["you"]["user_id"].clone();
    let room = owner.room(json!({"title":unique("members")}));
    let mut member = Peer::open();
    let member_id = member.guest()["you"]["user_id"].clone();
    member.join(&room);

    let list = listing(
        &mut owner,
        json!({"room_id":room,"members":true,"user_id":member_id}),
    );
    let members: Vec<&Value> = array(&record(&list, "joined", &room)["members"])
        .iter()
        .map(|m| &m["user_id"])
        .collect();
    let users: Vec<&Value> = array(&list["users"])
        .iter()
        .map(|u| &u["user_id"])
        .collect();
    if members.len() == 1 {
        assert_eq!(members, [&member_id]);
        assert_eq!(users, [&member_id], "users follows the filter");
    } else {
        assert!(members.contains(&&owner_id) && members.contains(&&member_id));
    }
    for user in array(&list["users"]) {
        opaque(&user["user_id"]);
        assert!(
            user.get("name").is_some(),
            "users carries complete objects: {user}"
        );
    }
}

#[test]
#[ignore = "requires APRON_TEST_WS with rooms"]
fn the_default_filter_returns_both_arrays_even_when_empty() {
    let mut peer = Peer::open();
    peer.cap("rooms");
    peer.guest();
    let room = peer.room(json!({"title":unique("only")}));
    for params in [
        json!({"room_id":room}),
        json!({"room_id":room,"filter":"all"}),
    ] {
        let list = listing(&mut peer, params);
        assert_eq!(ids(&list, "joined"), vec![room.clone()]);
        assert_eq!(
            list["not_joined"],
            json!([]),
            "a requested array is present: {list}"
        );
    }
    let all = listing(&mut peer, json!({}));
    array(&all["joined"]);
    array(&all["not_joined"]);
}

#[test]
#[ignore = "requires APRON_TEST_WS with rooms"]
fn joined_lists_threads_at_every_depth_and_not_joined_only_top_level() {
    let mut owner = Peer::open();
    owner.cap("rooms");
    owner.guest();
    let root = owner.room(json!({"title":unique("root")}));
    let thread = owner.room(json!({"title":"thread","parent_room_id":root}));
    let nested = owner.room(json!({"title":"nested","parent_room_id":thread}));

    let mine = ids(&listing(&mut owner, json!({"filter":"joined"})), "joined");
    for room in [&root, &thread, &nested] {
        assert!(mine.contains(room), "{room} missing from joined");
    }
    let mut other = Peer::open();
    other.guest();
    let theirs = ids(
        &listing(&mut other, json!({"filter":"not_joined"})),
        "not_joined",
    );
    assert!(theirs.contains(&root));
    assert!(!theirs.contains(&thread) && !theirs.contains(&nested));
    let children = listing(
        &mut other,
        json!({"parent_room_id":thread,"filter":"not_joined"}),
    );
    assert_eq!(ids(&children, "not_joined"), [nested]);
}

#[test]
#[ignore = "requires APRON_TEST_WS with rooms and history"]
fn rooms_are_listed_most_recently_active_first() {
    let mut peer = Peer::open();
    peer.cap("rooms");
    peer.cap("history");
    peer.guest();
    let a = peer.room(json!({"title":unique("a")}));
    let b = peer.room(json!({"title":unique("b")}));
    let c = peer.room(json!({"title":unique("c")}));
    peer.post(json!({"room_id":a,"body":{"text":"now the newest"}}));

    let list = listing(&mut peer, json!({"filter":"joined"}));
    let order = ids(&list, "joined");
    let at = |room: &String| order.iter().position(|r| r == room).unwrap();
    assert!(at(&a) < at(&c) && at(&c) < at(&b), "{order:?}");
    let latest: Vec<u64> = items(&list["joined"])
        .iter()
        .map(|r| log_id(&r["latest_log_id"]))
        .collect();
    assert!(
        latest.windows(2).all(|pair| pair[0] >= pair[1]),
        "{latest:?}"
    );
}

// --- membership (§4.2.2) --------------------------------------------------

#[test]
#[ignore = "requires APRON_TEST_WS with rooms, private rooms and history"]
fn adding_a_user_to_a_private_room_makes_it_visible_and_notifies_both() {
    let mut owner = Peer::open();
    owner.cap("rooms");
    owner.cap("history");
    owner.guest();
    let room = owner.room(json!({"title":unique("invite-only"),"private":true}));
    let mut guest = Peer::open();
    let guest_id = guest.guest()["you"]["user_id"].clone();
    assert!(!ids(&listing(&mut guest, json!({})), "not_joined").contains(&room));

    let add = owner.call("room_join", json!({"room_id":room,"user_id":guest_id}));
    if add.frame["error"]["code"] == UNSUPPORTED {
        return;
    }
    let notice = add.before("room_update", |p| membership_of(p, &room, &guest_id, true));
    let logged = items(&notice["memberships"])
        .iter()
        .find(|r| r["room_id"] == room)
        .unwrap()["log_id"]
        .clone();
    assert_eq!(add.ok(), json!({}));

    let welcome = guest.take("room_update", |p| has_room(p, "joined", &room));
    let joined = record(&welcome, "joined", &room);
    assert_eq!(joined["private"], true);
    assert!(
        array(&joined["members"])
            .iter()
            .any(|m| m["user_id"] == guest_id)
    );
    assert!(membership_of(&welcome, &room, &guest_id, true));

    let list = listing(&mut guest, json!({"room_id":room}));
    let seen = record(&list, "joined", &room);
    assert_eq!(
        seen["latest_log_id"], logged,
        "the membership advanced latest_log_id"
    );
    guest.post(json!({"room_id":room,"body":{"text":"thanks for the invite"}}));

    let leave = guest.call("room_leave", json!({"room_id":room}));
    let own = leave.before("room_update", |p| has_room(p, "left", &room));
    assert!(
        membership_of(own, &room, &guest_id, false),
        "a leaver gets its own record"
    );
    leave.ok();
    owner.take("room_update", |p| membership_of(p, &room, &guest_id, false));
}

#[test]
#[ignore = "requires APRON_TEST_WS with rooms and history"]
fn joining_a_room_twice_logs_one_membership() {
    let mut owner = Peer::open();
    owner.cap("rooms");
    owner.cap("history");
    owner.guest();
    let room = owner.room(json!({"title":unique("twice")}));
    let mut member = Peer::open();
    let member_id = member.guest()["you"]["user_id"].clone();
    member.join(&room);
    owner.take("room_update", |p| membership_of(p, &room, &member_id, true));
    let latest = record(
        &listing(&mut member, json!({"room_id":room})),
        "joined",
        &room,
    )["latest_log_id"]
        .clone();
    quiet(&mut owner);
    quiet(&mut member);

    let again = member.call("room_join", json!({"room_id":room}));
    assert!(
        !again.before.iter().any(|f| event(f, "room_update")),
        "a repeated join changes nothing: {:?}",
        again.before
    );
    assert_eq!(again.ok(), json!({}));
    owner.fence();
    assert!(
        !owner.inbox.iter().any(|f| event(f, "room_update")),
        "{:?}",
        owner.inbox
    );
    let after = record(
        &listing(&mut member, json!({"room_id":room})),
        "joined",
        &room,
    )["latest_log_id"]
        .clone();
    assert_eq!(after, latest, "nothing was logged");
}

// --- creating and editing (§4.2.4) ----------------------------------------

#[test]
#[ignore = "requires APRON_TEST_WS with rooms"]
fn a_room_created_without_private_is_public() {
    let mut owner = Peer::open();
    owner.cap("rooms");
    owner.guest();
    let exchange = owner.call("room_set", json!({"title":unique("public")}));
    let room = string(&exchange.frame["result"]["room_id"]).to_owned();
    let update = exchange.before("room_update", |p| has_room(p, "joined", &room));
    assert_ne!(record(update, "joined", &room)["private"], true);
    exchange.ok();
    let mut other = Peer::open();
    other.guest();
    let list = listing(&mut other, json!({"filter":"not_joined"}));
    assert_ne!(record(&list, "not_joined", &room)["private"], true);
}

#[test]
#[ignore = "requires APRON_TEST_WS with rooms and private rooms"]
fn a_private_thread_of_a_public_room_stays_hidden_from_parent_members() {
    let mut owner = Peer::open();
    owner.cap("rooms");
    owner.guest();
    let root = owner.room(json!({"title":unique("public-root")}));
    let mut parent_member = Peer::open();
    parent_member.guest();
    parent_member.join(&root);
    quiet(&mut parent_member);

    let thread = owner.room(json!({"title":"secret","parent_room_id":root,"private":true}));
    let created = listing(&mut owner, json!({"room_id":thread}));
    assert_eq!(record(&created, "joined", &thread)["private"], true);
    owner
        .call("room_set", json!({"room_id":thread,"title":"still secret"}))
        .ok();
    owner.post(json!({"room_id":thread,"body":{"text":"members only"}}));

    parent_member.fence();
    assert!(
        !parent_member
            .inbox
            .iter()
            .any(|f| f.to_string().contains(&thread)),
        "a private thread leaked: {:?}",
        parent_member.inbox
    );
    let children = listing(&mut parent_member, json!({"parent_room_id":root}));
    assert!(!ids(&children, "not_joined").contains(&thread));
    parent_member
        .call("room_list", json!({"room_id":thread}))
        .error(INVALID);
}

#[test]
#[ignore = "requires APRON_TEST_WS with rooms, private rooms and ext"]
fn id_fields_are_fixed_and_omitted_client_fields_are_kept() {
    let mut owner = Peer::open();
    owner.cap("rooms");
    owner.cap("ext");
    owner.guest();
    let root = owner.room(json!({"title":unique("root")}));
    let other_root = owner.room(json!({"title":unique("other-root")}));
    let room = owner.room(
        json!({"title":unique("kept"),"parent_room_id":root,"private":true,
        "description":"why","ext":{"agent":{"mode":"plan"}}}),
    );

    let rename = owner.call("room_set", json!({"room_id":room,"title":"renamed"}));
    let update = rename.before("room_update", |p| has_room(p, "updated", &room));
    let updated = record(update, "updated", &room);
    assert_eq!(updated["title"], "renamed");
    assert_eq!(updated["private"], true, "private is kept when omitted");
    assert_eq!(updated["description"], "why");
    assert_eq!(
        updated["ext"],
        json!({"agent":{"mode":"plan"}}),
        "ext is kept when omitted"
    );
    assert_eq!(updated["parent_room_id"], root);
    rename.ok();

    for change in [
        json!({"room_id":room,"parent_room_id":other_root}),
        json!({"room_id":room,"private":false}),
    ] {
        let exchange = owner.call("room_set", change.clone());
        if exchange.frame.get("error").is_some() {
            exchange.error(INVALID);
        } else {
            exchange.ok();
        }
        let now = listing(&mut owner, json!({"room_id":room}));
        let current = record(&now, "joined", &room);
        assert_eq!(current["parent_room_id"], root, "{change}");
        assert_eq!(current["private"], true, "{change}");
        assert_eq!(
            current["title"], "renamed",
            "a rejected change is not applied"
        );
    }
}
