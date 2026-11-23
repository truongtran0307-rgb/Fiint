//! The Zalo store without a network: parsing, ingesting, the work feed, reading, recalls and the
//! cache on disk.

use super::*;
use listen::{group_message, user_message};
use serde_json::json;

const ME: &str = "me";

fn store_as_me() -> Store {
    let mut s = Store::default();
    s.state.me = Some(ZaloSelf { id: ME.into(), name: "Tôi".into(), avatar: String::new() });
    s
}

/// A message from `from` to me (or from me when `from` is ME) in a personal chat with `peer`.
fn dm(peer: &str, from: &str, id: &str, ts: i64, text: &str) -> Msg {
    let (uid_from, id_to) = if from == ME { ("0", peer) } else { (from, "0") };
    user_message(ME, json!({ "msgId": id, "uidFrom": uid_from, "idTo": id_to, "ts": ts.to_string(), "msgType": "webchat", "content": text, "dName": format!("Name {from}") }))
}

/// A group message; `extra` is merged into the socket data (mentions, quote…).
fn gm(group: &str, from: &str, id: &str, ts: i64, text: &str, extra: Value) -> Msg {
    let mut d = json!({ "msgId": id, "uidFrom": if from == ME { "0" } else { from }, "idTo": group, "ts": ts, "msgType": "webchat", "content": text, "dName": format!("Name {from}") });
    if let (Some(o), Some(e)) = (d.as_object_mut(), extra.as_object()) {
        o.extend(e.clone());
    }
    group_message(ME, d)
}

fn ids(list: &[ZaloMessage]) -> Vec<&str> {
    list.iter().map(|m| m.id.as_str()).collect()
}

fn msg(id: &str, ts: i64) -> ZaloMessage {
    ZaloMessage { id: id.into(), ts, ..Default::default() }
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("vibe-zalo-test-{tag}-{}-{}", std::process::id(), now_ms()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// What the save task would do, synchronously.
fn save(dir: &Path, s: &mut Store) -> Vec<PathBuf> {
    take_writes(dir, s)
        .into_iter()
        .map(|(p, b)| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b).unwrap();
            p
        })
        .collect()
}

/* ---------------- parsing ---------------- */

#[test]
fn attachments_parse_like_zalo_ts() {
    let photo = parse_attachment("chat.photo", &json!({ "href": "https://h", "thumb": "https://t", "params": "{\"hd\":\"https://hd\",\"width\":1920,\"height\":\"1080\"}" })).unwrap();
    assert_eq!(photo.kind, "photo");
    assert_eq!(photo.url.as_deref(), Some("https://hd"));
    assert_eq!((photo.width, photo.height), (Some(1920.0), Some(1080.0)));
    assert_eq!(serde_json::to_value(&photo).unwrap(), json!({ "kind": "photo", "url": "https://hd", "thumb": "https://t", "width": 1920, "height": 1080 }));
    let file = parse_attachment("share.file", &json!({ "title": "a.PDF", "href": "https://f", "params": { "fileSize": "2048", "fileExt": "PDF" } })).unwrap();
    assert_eq!((file.kind.as_str(), file.size, file.ext.as_deref()), ("file", Some(2048.0), Some("pdf")));
    let voice = parse_attachment("chat.voice", &json!({ "href": "https://v", "params": "{\"duration\":2500}" })).unwrap();
    assert_eq!(voice.duration, Some(2.5));
    let card = parse_attachment("chat.recommended", &json!({ "action": "recommened.user", "title": "Bob", "params": "{\"phone\":\"0909\"}" })).unwrap();
    assert_eq!((card.kind.as_str(), card.desc.as_deref()), ("card", Some("0909")));
    assert!(parse_attachment("webchat", &json!("hello")).is_none());
    assert_eq!(content_text(Some(&file), &json!({})), "[file] a.PDF https://f");
    assert_eq!(content_text(None, &json!({})), "[tin nhắn]");
}

#[test]
fn more_attachment_kinds() {
    let video = parse_attachment("chat.video.msg", &json!({ "href": "https://v", "params": { "video_width": 720, "video_height": 1280, "duration": 61000 } })).unwrap();
    assert_eq!((video.kind.as_str(), video.width, video.height, video.duration), ("video", Some(720.0), Some(1280.0), Some(61.0)));
    assert_eq!(parse_attachment("chat.sticker", &json!({ "id": 42, "catId": 7 })).unwrap().kind, "sticker");
    // a sticker without an id is nothing we can show
    assert!(parse_attachment("chat.sticker", &json!({ "catId": 7 })).is_none());
    assert_eq!(parse_attachment("chat.location.new", &json!({ "title": "Nhà" })).unwrap().kind, "location");
    assert_eq!(parse_attachment("chat.link", &json!({ "href": "https://x", "action": "recommened.link" })).unwrap().kind, "link");
    assert_eq!(parse_attachment("chat.gif", &json!({ "href": "https://g", "params": { "width": 10, "height": 20 } })).unwrap().width, Some(10.0));
    assert_eq!(parse_attachment("chat.doodle", &json!({ "href": "https://d" })).unwrap().kind, "photo");
    assert_eq!(parse_attachment("chat.todo", &json!({ "title": "x" })).unwrap().kind, "other");
    // bad params: ignored, not a failure
    let p = parse_attachment("chat.photo", &json!({ "href": "https://h", "params": "{not json" })).unwrap();
    assert_eq!((p.url.as_deref(), p.width), (Some("https://h"), None));
}

#[test]
fn previews() {
    let mut m = ZaloMessage { text: "hi".into(), ..Default::default() };
    assert_eq!(preview_of(&m), "hi");
    m.att = Some(ZaloAttachment { kind: "file".into(), title: Some("a.pdf".into()), ..Default::default() });
    assert_eq!(preview_of(&m), "📎 a.pdf");
    m.att = Some(ZaloAttachment { kind: "photo".into(), ..Default::default() });
    assert_eq!(preview_of(&m), "🖼 Ảnh");
    m.att = Some(ZaloAttachment { kind: "link".into(), url: Some("https://x".into()), ..Default::default() });
    assert_eq!(preview_of(&m), "🔗 https://x");
    m.recalled = Some(true);
    assert_eq!(preview_of(&m), "Tin nhắn đã thu hồi");
    assert_eq!(attach_label("voice"), "Tin nhắn thoại");
    assert_eq!(attach_label("???"), "Tin nhắn");
}

#[test]
fn message_from_socket() {
    let s = Store::default();
    let m = group_message(
        ME,
        json!({
            "msgId": "7300000000000000001", "cliMsgId": "1731", "uidFrom": "55", "idTo": "g1", "dName": "Lan",
            "ts": "1731999999999", "msgType": "webchat", "content": "chào", "ttl": 0, "st": 3, "at": 0, "cmd": 521,
            "quote": { "fromD": "Tôi", "msg": "hỏi", "globalMsgId": 99, "ownerId": 1 },
            "mentions": [{ "uid": "me", "pos": 0, "len": 3 }]
        }),
    );
    let z = to_message(&s, &m);
    let v = serde_json::to_value(&z).unwrap();
    assert_eq!(v["id"], "7300000000000000001");
    assert_eq!(v["threadId"], "g1");
    assert_eq!(v["fromName"], "Lan");
    assert_eq!(v["ts"], 1731999999999i64);
    assert_eq!(v["text"], "chào");
    assert_eq!(v["quote"], "Tôi: hỏi");
    assert_eq!(v["reply"], json!({ "id": "99", "from": "Tôi", "text": "hỏi" }));
    assert_eq!(v["mentions"], json!([{ "uid": "me", "pos": 0, "len": 3 }]));
    // nothing only the old chat tab needed
    assert!(v.get("raw").is_none() && v.get("status").is_none() && v.get("cliMsgId").is_none() && v.get("att").is_none());
    let back: ZaloMessage = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(serde_json::to_value(&back).unwrap(), v);
}

#[test]
fn my_message_and_names() {
    let mut s = store_as_me();
    s.names.insert("55".into(), "Lan (saved)".into());
    let mine = to_message(&s, &dm("55", ME, "1", 10, "ok"));
    assert!(mine.mine);
    assert_eq!((mine.from_id.as_str(), mine.from_name.as_str(), mine.thread_id.as_str()), (ME, "Tôi", "55"));
    // no dName: the saved name, then the id
    let m = user_message(ME, json!({ "msgId": "2", "uidFrom": "55", "idTo": "0", "ts": 11, "content": "x" }));
    assert_eq!(to_message(&s, &m).from_name, "Lan (saved)");
    let m = user_message(ME, json!({ "msgId": "3", "uidFrom": "77", "idTo": "0", "ts": 11, "content": "x" }));
    assert_eq!(to_message(&s, &m).from_name, "77");
}

#[test]
fn missing_timestamp_is_now() {
    let s = Store::default();
    let m = user_message(ME, json!({ "msgId": "1", "uidFrom": "5", "idTo": "0", "content": "x" }));
    let before = now_ms();
    assert!(to_message(&s, &m).ts >= before);
}

/* ---------------- ordering & ingest ---------------- */

#[test]
fn insert_keeps_time_order_and_skips_duplicates() {
    let mut l = Vec::new();
    assert_eq!(insert_sorted(&mut l, msg("a", 10)), Some(0));
    assert_eq!(insert_sorted(&mut l, msg("c", 30)), Some(1));
    // late arrival goes in its place
    assert_eq!(insert_sorted(&mut l, msg("b", 20)), Some(1));
    // the same message again
    assert_eq!(insert_sorted(&mut l, msg("b", 20)), None);
    // same time, another message: kept, after the first
    assert_eq!(insert_sorted(&mut l, msg("b2", 20)), Some(2));
    // older than everything
    assert_eq!(insert_sorted(&mut l, msg("z", 1)), Some(0));
    assert_eq!(ids(&l), ["z", "a", "b", "b2", "c"]);
}

#[test]
fn keep_newest_caps_from_the_front() {
    let mut l: Vec<ZaloMessage> = (0..MAX_PER_THREAD as i64 + 5).map(|i| msg(&i.to_string(), i)).collect();
    keep_newest(&mut l);
    assert_eq!(l.len(), MAX_PER_THREAD);
    assert_eq!(l[0].id, "5");
    let mut short = vec![msg("a", 1)];
    keep_newest(&mut short);
    assert_eq!(short.len(), 1);
}

#[test]
fn ingest_creates_a_named_conversation() {
    let mut s = store_as_me();
    let got = ingest_into(&mut s, &[dm("55", "55", "1", 100, "chào anh")]);
    assert_eq!(got.touched.iter().collect::<Vec<_>>(), ["55"]);
    let t = &s.threads["55"];
    assert_eq!((t.name.as_str(), t.group, t.last_ts, t.last.as_str()), ("Name 55", false, 100, "chào anh"));
    assert_eq!(s.names["55"], "Name 55");
    assert!(s.dirty.contains("55") && s.meta_dirty);
}

#[test]
fn ingest_twice_changes_nothing() {
    let mut s = store_as_me();
    let batch = [dm("55", "55", "1", 100, "a"), dm("55", ME, "2", 200, "b")];
    ingest_into(&mut s, &batch);
    s.dirty.clear();
    let again = ingest_into(&mut s, &batch);
    assert_eq!(again, Ingested::default());
    assert_eq!(s.messages["55"].len(), 2);
    assert!(s.dirty.is_empty());
}

#[test]
fn replayed_history_does_not_replace_the_last_line() {
    let mut s = store_as_me();
    ingest_into(&mut s, &[dm("55", "55", "new", 500, "mới nhất")]);
    ingest_into(&mut s, &[dm("55", "55", "old", 100, "cũ")]);
    assert_eq!(ids(&s.messages["55"]), ["old", "new"]);
    assert_eq!((s.threads["55"].last.as_str(), s.threads["55"].last_ts), ("mới nhất", 500));
}

#[test]
fn group_last_line_names_the_sender_and_wants_avatars() {
    let mut s = store_as_me();
    s.names.insert("g1".into(), "Team".into());
    s.avatars.insert("77".into(), "https://a77".into());
    let got = ingest_into(&mut s, &[gm("g1", "55", "1", 10, "hello", json!({})), gm("g1", "77", "2", 20, "yo", json!({})), gm("g1", ME, "3", 30, "ok", json!({}))]);
    let t = &s.threads["g1"];
    assert_eq!((t.name.as_str(), t.group, t.last.as_str()), ("Team", true, "ok"));
    // 55 has no avatar yet; 77 has one; my own never asked
    assert_eq!(got.profiles.iter().collect::<Vec<_>>(), ["55"]);
    ingest_into(&mut s, &[gm("g1", "55", "4", 40, "bye", json!({}))]);
    assert_eq!(s.threads["g1"].last, "Name 55: bye");
}

#[test]
fn ingest_caps_each_conversation() {
    let mut s = store_as_me();
    let batch: Vec<Msg> = (0..MAX_PER_THREAD as i64 + 20).map(|i| dm("55", "55", &format!("m{i}"), i + 1, "x")).collect();
    ingest_into(&mut s, &batch);
    let l = &s.messages["55"];
    assert_eq!(l.len(), MAX_PER_THREAD);
    assert_eq!(l.last().unwrap().id, format!("m{}", MAX_PER_THREAD + 19));
    assert_eq!(l[0].id, "m20");
}

#[test]
fn ingest_skips_messages_without_id_and_learns_names() {
    let mut s = store_as_me();
    let no_id = user_message(ME, json!({ "uidFrom": "5", "idTo": "0", "ts": 1, "content": "?" }));
    assert!(ingest_into(&mut s, &[no_id]).touched.is_empty());
    assert!(s.messages.get("5").is_none_or(Vec::is_empty));
    // a sender's new display name replaces the old one
    s.names.insert("55".into(), "Old".into());
    ingest_into(&mut s, &[dm("55", "55", "1", 1, "x")]);
    assert_eq!(s.names["55"], "Name 55");
}

#[test]
fn many_conversations_in_one_batch() {
    let mut s = store_as_me();
    let batch: Vec<Msg> = (0..50).map(|i| dm(&format!("u{i}"), &format!("u{i}"), &format!("id{i}"), i, "x")).collect();
    let got = ingest_into(&mut s, &batch);
    assert_eq!(got.touched.len(), 50);
    assert_eq!(s.threads.len(), 50);
    assert_eq!(s.dirty.len(), 50);
}

/* ---------------- the work feed ---------------- */

#[test]
fn personal_chats_with_incoming_messages_count() {
    let mut s = store_as_me();
    ingest_into(&mut s, &[dm("55", "55", "1", 100, "giúp mình"), dm("55", ME, "2", 110, "ok")]);
    let f = work_feed_in(&s, 0, 10, 10, None);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].name, "Name 55");
    let m = &f[0].msgs;
    assert_eq!(m.iter().map(|x| (x.id.as_str(), x.to_me, x.mine)).collect::<Vec<_>>(), [("1", true, false), ("2", false, true)]);
    assert_eq!(m[1].from, "Tôi");
}

#[test]
fn chats_with_only_my_messages_do_not() {
    let mut s = store_as_me();
    ingest_into(&mut s, &[dm("55", ME, "1", 100, "hi")]);
    assert!(work_feed_in(&s, 0, 10, 10, None).is_empty());
}

#[test]
fn groups_need_a_mention_or_a_reply_to_me() {
    let mut s = store_as_me();
    ingest_into(&mut s, &[gm("g1", "55", "1", 10, "chuyện phiếm", json!({}))]);
    assert!(work_feed_in(&s, 0, 10, 10, None).is_empty());

    ingest_into(&mut s, &[gm("g1", "55", "2", 20, "@me làm cái này", json!({ "mentions": [{ "uid": ME, "pos": 0, "len": 3 }] }))]);
    let f = work_feed_in(&s, 0, 10, 10, None);
    assert_eq!(f.len(), 1);
    // the small talk before it is left out
    assert_eq!(f[0].msgs.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["2"]);

    // someone else mentioned: not for me
    let mut s2 = store_as_me();
    ingest_into(&mut s2, &[gm("g2", "55", "1", 10, "@bob", json!({ "mentions": [{ "uid": "bob", "pos": 0, "len": 4 }] }))]);
    assert!(work_feed_in(&s2, 0, 10, 10, None).is_empty());
}

#[test]
fn a_reply_to_my_group_message_counts() {
    let mut s = store_as_me();
    ingest_into(
        &mut s,
        &[
            gm("g1", ME, "100", 10, "ai làm giúp?", json!({})),
            gm("g1", "55", "101", 20, "chuyện khác", json!({})),
            gm("g1", "66", "102", 30, "để em", json!({ "quote": { "fromD": "Tôi", "msg": "ai làm giúp?", "globalMsgId": "100" } })),
            gm("g1", ME, "103", 40, "cảm ơn", json!({})),
        ],
    );
    let f = work_feed_in(&s, 0, 10, 10, None);
    assert_eq!(f.len(), 1);
    // from the reply on: the reply and my answer, not the other member's chatter
    assert_eq!(f[0].msgs.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["102", "103"]);
    assert!(f[0].msgs[0].text.starts_with("[trả lời Tôi: \"ai làm giúp?\"] "));
}

#[test]
fn since_recalled_and_limits() {
    let mut s = store_as_me();
    ingest_into(&mut s, &[dm("55", "55", "old", 10, "cũ"), dm("55", "55", "a", 100, "1"), dm("55", "55", "b", 200, "2"), dm("55", "55", "c", 300, "3")]);
    // older than `since`: out
    let f = work_feed_in(&s, 50, 10, 10, None);
    assert_eq!(f[0].msgs.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
    // max_msgs keeps the newest
    let f = work_feed_in(&s, 50, 10, 2, None);
    assert_eq!(f[0].msgs.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["b", "c"]);
    // recalled ones are gone
    let undo = Action { thread_id: "55".into(), data: json!({ "content": { "globalMsgId": "b" } }) };
    assert!(apply_undo_in(&mut s, &undo));
    let f = work_feed_in(&s, 50, 10, 10, None);
    assert_eq!(f[0].msgs.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["a", "c"]);
    // nothing new since: the conversation drops out
    assert!(work_feed_in(&s, 1000, 10, 10, None).is_empty());
}

#[test]
fn feed_is_newest_first_capped_and_filterable() {
    let mut s = store_as_me();
    ingest_into(&mut s, &[dm("a", "a", "1", 100, "x"), dm("b", "b", "2", 300, "x"), dm("c", "c", "3", 200, "x")]);
    let names = |f: Vec<WorkThread>| f.into_iter().map(|t| t.thread_id).collect::<Vec<_>>();
    assert_eq!(names(work_feed_in(&s, 0, 10, 10, None)), ["b", "c", "a"]);
    assert_eq!(names(work_feed_in(&s, 0, 2, 10, None)), ["b", "c"]);
    let only = ["a".to_owned(), "missing".to_owned()];
    assert_eq!(names(work_feed_in(&s, 0, 10, 10, Some(&only))), ["a"]);
}

#[test]
fn long_texts_are_cut() {
    let mut s = store_as_me();
    let long = "x".repeat(2000);
    ingest_into(&mut s, &[dm("55", "55", "1", 1, &long)]);
    assert_eq!(work_feed_in(&s, 0, 1, 1, None)[0].msgs[0].text.chars().count(), 600);
}

/* ---------------- recalls ---------------- */

#[test]
fn recall_clears_the_message_and_the_last_line() {
    let mut s = store_as_me();
    let photo = user_message(ME, json!({ "msgId": "9", "uidFrom": "55", "idTo": "0", "ts": 50, "msgType": "chat.photo", "content": { "href": "https://p" } }));
    ingest_into(&mut s, &[dm("55", "55", "1", 10, "a"), photo]);
    assert_eq!(s.threads["55"].last, "🖼 Ảnh");
    s.dirty.clear();
    assert!(apply_undo_in(&mut s, &Action { thread_id: "55".into(), data: json!({ "content": { "globalMsgId": "9" } }) }));
    let m = s.messages["55"].last().unwrap();
    assert_eq!((m.recalled, m.att.is_none(), m.text.as_str()), (Some(true), true, "Tin nhắn đã được thu hồi"));
    assert_eq!(s.threads["55"].last, "Tin nhắn đã thu hồi");
    assert!(s.dirty.contains("55"));
    // unknown message / conversation: nothing happens
    assert!(!apply_undo_in(&mut s, &Action { thread_id: "55".into(), data: json!({ "content": { "globalMsgId": "nope" } }) }));
    assert!(!apply_undo_in(&mut s, &Action { thread_id: "zz".into(), data: json!({ "content": { "globalMsgId": "9" } }) }));
}

/* ---------------- reading ---------------- */

#[test]
fn reading_pages_back_through_history() {
    let mut s = store_as_me();
    let batch: Vec<Msg> = (1..=10).map(|i| dm("55", "55", &format!("m{i}"), i * 10, "x")).collect();
    ingest_into(&mut s, &batch);
    assert_eq!(ids(&get_messages_in(&s, "55", 3, None)), ["m8", "m9", "m10"]);
    // before 50: the three before it
    assert_eq!(ids(&get_messages_in(&s, "55", 3, Some(50))), ["m2", "m3", "m4"]);
    assert_eq!(ids(&get_messages_in(&s, "55", 100, Some(25))), ["m1", "m2"]);
    // 0 means "no limit in time"
    assert_eq!(get_messages_in(&s, "55", 100, Some(0)).len(), 10);
    assert!(get_messages_in(&s, "55", 5, Some(5)).is_empty());
    assert!(get_messages_in(&s, "nobody", 5, None).is_empty());
}

#[test]
fn search_finds_text_and_senders() {
    let mut s = store_as_me();
    ingest_into(&mut s, &[dm("55", "55", "1", 10, "Báo cáo tuần"), dm("66", "66", "2", 20, "báo cáo tháng"), dm("66", "66", "3", 30, "khác")]);
    assert_eq!(ids(&search_messages_in(&s, "BÁO CÁO", None, 10)), ["2", "1"]);
    assert_eq!(ids(&search_messages_in(&s, "báo cáo", Some("55"), 10)), ["1"]);
    assert_eq!(ids(&search_messages_in(&s, "báo cáo", None, 1)), ["2"]);
    assert_eq!(ids(&search_messages_in(&s, "name 66", None, 10)), ["3", "2"]);
    assert!(search_messages_in(&s, "   ", None, 10).is_empty());
    assert!(search_messages_in(&s, "báo", Some("missing"), 10).is_empty());
}

#[test]
fn conversation_list_filters_and_sorts() {
    let mut s = store_as_me();
    ingest_into(&mut s, &[dm("11", "11", "1", 10, "x"), dm("22", "22", "2", 30, "x"), dm("33", "33", "3", 20, "x")]);
    let ids = |l: Vec<ZaloThread>| l.into_iter().map(|t| t.id).collect::<Vec<_>>();
    assert_eq!(ids(list_threads_in(&s, "", 10)), ["22", "33", "11"]);
    assert_eq!(ids(list_threads_in(&s, "", 1)), ["22"]);
    assert_eq!(ids(list_threads_in(&s, "NAME 33", 10)), ["33"]);
    assert_eq!(ids(list_threads_in(&s, "1", 10)), ["11"]);
    assert!(list_threads_in(&s, "nobody", 10).is_empty());
}

/* ---------------- names & avatars ---------------- */

#[test]
fn profiles_fill_names_and_avatars() {
    let mut s = store_as_me();
    ingest_into(&mut s, &[dm("55", "55", "1", 1, "x")]);
    s.threads.get_mut("55").unwrap().name.clear();
    let r = json!({ "changed_profiles": { "55_0": { "avatar": "https://a", "displayName": "", "zaloName": "Lan Z" }, "66": { "avatar": "" } } });
    assert!(apply_profiles(&mut s, &r));
    assert_eq!((s.avatars["55"].as_str(), s.names["55"].as_str()), ("https://a", "Lan Z"));
    assert_eq!((s.threads["55"].avatar.as_deref(), s.threads["55"].name.as_str()), (Some("https://a"), "Lan Z"));
    // empty values are not stored
    assert!(!s.avatars.contains_key("66"));
    assert!(!apply_profiles(&mut s, &json!({})));
}

#[test]
fn friends_name_personal_chats() {
    let mut s = store_as_me();
    ingest_into(&mut s, &[dm("55", "55", "1", 1, "x")]);
    apply_friends(&mut s, &[json!({ "userId": "55", "displayName": "Lan", "avatar": "https://l" }), json!({ "userId": "", "displayName": "nobody" })]);
    assert_eq!((s.threads["55"].name.as_str(), s.threads["55"].avatar.as_deref()), ("Lan", Some("https://l")));
    assert!(!s.names.contains_key(""));
}

#[test]
fn groups_are_named_but_not_all_added() {
    let mut s = store_as_me();
    ingest_into(&mut s, &[gm("g1", "55", "1", 1, "x", json!({}))]);
    apply_groups(&mut s, &json!({ "gridInfoMap": { "g1": { "name": "Team", "avt": "https://t" }, "g2": { "name": "Silent group" } } }));
    assert_eq!((s.threads["g1"].name.as_str(), s.threads["g1"].avatar.as_deref()), ("Team", Some("https://t")));
    // a group with no messages: its name is known, it isn't a conversation (yet)
    assert_eq!(s.names["g2"], "Silent group");
    assert!(!s.threads.contains_key("g2"));
    ingest_into(&mut s, &[gm("g2", "55", "2", 2, "x", json!({}))]);
    assert_eq!(s.threads["g2"].name, "Silent group");
}

/* ---------------- the cache on disk ---------------- */

#[test]
fn file_names_stay_inside_the_folder() {
    let d = Path::new("base");
    assert_eq!(thread_file(d, "123456"), Path::new("base").join("msgs").join("123456.json"));
    assert_eq!(thread_file(d, "g-1_a"), Path::new("base").join("msgs").join("g-1_a.json"));
    let evil = thread_file(d, "../../x");
    assert_eq!(evil.parent().unwrap(), Path::new("base").join("msgs"));
    assert!(!evil.to_string_lossy().contains(".."));
    assert_ne!(thread_file(d, ""), thread_file(d, "x"));
}

#[test]
fn cache_round_trip_writes_only_what_changed() {
    let dir = temp_dir("roundtrip");
    let mut s = store_as_me();
    ingest_into(&mut s, &[dm("55", "55", "1", 10, "a"), gm("g1", "66", "2", 20, "b", json!({}))]);
    s.avatars.insert("55".into(), "https://a".into());
    let written = save(&dir, &mut s);
    assert_eq!(written.len(), 3); // two conversations + cache.json
    // nothing changed: nothing to write
    assert!(take_writes(&dir, &mut s).is_empty());
    // one new message: only that conversation (and the list)
    ingest_into(&mut s, &[dm("55", "55", "3", 30, "c")]);
    let written = save(&dir, &mut s);
    assert_eq!(written.len(), 2);
    assert!(written.contains(&thread_file(&dir, "55")) && written.contains(&dir.join("cache.json")));

    let mut back = Store::default();
    load_cache(&dir, &mut back);
    assert_eq!(ids(&back.messages["55"]), ["1", "3"]);
    assert_eq!(ids(&back.messages["g1"]), ["2"]);
    assert_eq!(back.threads["55"].last, "c");
    assert_eq!(back.avatars["55"], "https://a");
    assert!(back.dirty.is_empty() && !back.meta_dirty);
    let meta: Value = serde_json::from_slice(&std::fs::read(dir.join("cache.json")).unwrap()).unwrap();
    assert_eq!(meta["version"], CACHE_VERSION);
    assert!(meta.get("messages").is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn old_single_file_cache_is_split() {
    let dir = temp_dir("migrate");
    // version 1: messages inside cache.json, with fields this version no longer keeps
    let old = json!({
        "threads": [{ "id": "55", "group": false, "name": "Lan", "lastTs": 20, "last": "b", "unread": 3, "pinned": true }],
        "messages": { "55": [
            { "id": "2", "threadId": "55", "ts": 20, "text": "b", "cliMsgId": "x", "status": "seen", "raw": { "content": "b" } },
            { "id": "1", "threadId": "55", "ts": 10, "text": "a", "reactions": { "/-heart": ["66"] } }
        ] },
        "names": { "55": "Lan" },
        "avatars": {},
        "friends": ["55"],
        "stickers": { "1": "https://s" },
        "syncedAt": 123
    });
    std::fs::write(dir.join("cache.json"), old.to_string()).unwrap();
    let mut s = Store::default();
    load_cache(&dir, &mut s);
    // sorted on the way in
    assert_eq!(ids(&s.messages["55"]), ["1", "2"]);
    assert_eq!(s.threads["55"].name, "Lan");
    assert!(s.dirty.contains("55") && s.meta_dirty);
    save(&dir, &mut s);
    let mut back = Store::default();
    load_cache(&dir, &mut back);
    assert_eq!(ids(&back.messages["55"]), ["1", "2"]);
    assert!(std::fs::metadata(thread_file(&dir, "55")).is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn broken_or_missing_cache_starts_empty() {
    let dir = temp_dir("broken");
    let mut s = Store::default();
    load_cache(&dir, &mut s);
    assert!(s.threads.is_empty());
    std::fs::write(dir.join("cache.json"), "{ not json").unwrap();
    load_cache(&dir, &mut s);
    assert!(s.threads.is_empty());
    // a conversation file that is damaged: that conversation has no messages, the rest loads
    std::fs::write(dir.join("cache.json"), json!({ "version": 2, "threads": [{ "id": "a" }, { "id": "b" }] }).to_string()).unwrap();
    std::fs::create_dir_all(dir.join("msgs")).unwrap();
    std::fs::write(thread_file(&dir, "a"), "[{").unwrap();
    std::fs::write(thread_file(&dir, "b"), json!([{ "id": "1", "ts": 1 }]).to_string()).unwrap();
    load_cache(&dir, &mut s);
    assert_eq!(s.threads.len(), 2);
    assert!(!s.messages.contains_key("a"));
    assert_eq!(s.messages["b"].len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/* ---------------- live events ---------------- */

#[test]
fn live_messages_are_batched() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    for i in 0..3 {
        tx.send(Event::Message(dm("55", "55", &i.to_string(), i, "x"))).unwrap();
    }
    tx.send(Event::CipherKey).unwrap();
    tx.send(Event::Message(dm("55", "55", "9", 9, "x"))).unwrap();
    let first = rx.try_recv().unwrap();
    let (batch, rest) = next_batch(first, &mut rx);
    assert_eq!(batch.len(), 3);
    assert!(matches!(rest, Some(Event::CipherKey)));
    // the message after the other event starts the next batch
    let (batch, rest) = next_batch(rx.try_recv().unwrap(), &mut rx);
    assert_eq!((batch.len(), rest.is_none()), (1, true));
    // a non-message event alone
    let (batch, rest) = next_batch(Event::Closed(1000), &mut rx);
    assert!(batch.is_empty() && matches!(rest, Some(Event::Closed(1000))));
}
