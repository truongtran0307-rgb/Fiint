//! Personal Zalo account (the protocol is a Rust port of zca-js).
//!
//! Login is a QR scan; the session (imei + cookies + user agent) is kept encrypted so the next
//! launch logs in by itself. Zalo has no "list my conversations with history" call for 1-to-1
//! chats, so we keep our own cache: everything the listener receives (live messages and the recent
//! history Zalo replays on connect) is stored here and read by Claude's `zalo_*` tools and by the
//! task list (`zalo_tasks`).
//!
//! On disk: `zalo/cache.json` holds the conversations, names and avatars; each conversation's
//! messages live in their own `zalo/msgs/<id>.json`, written only when that conversation changed.

mod api;
mod cookies;
mod listen;
mod proto;
mod tools;
mod types;

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use indexmap::{IndexMap, IndexSet};
use parking_lot::{Mutex, MutexGuard};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{sync::mpsc, task::AbortHandle};

pub use tools::{INSTRUCTIONS, call_tool, tools};
pub use types::*;

use crate::core::{self, Res, emit};
use api::{Api, Credentials, QrEvent, ThreadType};
use listen::{Action, Event, Listener, Msg};
use proto::{jnum, jstr, now_ms, truthy};

/// messages kept per conversation (the tools read ≤ 500, the task list ≤ 400)
const MAX_PER_THREAD: usize = 1000;
/// pages of history asked from the socket after connecting, per kind (people / groups)
const OLD_PAGES: u32 = 15;
const CACHE_VERSION: u32 = 2;

fn dir() -> PathBuf {
    core::user_data().join("zalo")
}
fn cred_file() -> PathBuf {
    dir().join("session.bin")
}

/* ---------------- state ---------------- */

#[derive(Default)]
struct Store {
    loaded: bool,
    threads: IndexMap<String, ZaloThread>,
    /// each conversation's messages, oldest first
    messages: IndexMap<String, Vec<ZaloMessage>>,
    /// userId / groupId → display name (friends, groups, senders seen)
    names: IndexMap<String, String>,
    /// userId / groupId → avatar url
    avatars: IndexMap<String, String>,
    state: ZaloState,
    retries: u32,
    replay_pages: [u32; 2],
    /// profiles asked for (never asked twice)
    profile_queue: IndexSet<String>,
    profile_timer: bool,
    /// conversations whose messages must be written; `meta_dirty`: cache.json must be
    dirty: IndexSet<String>,
    meta_dirty: bool,
}

static STORE: LazyLock<Mutex<Store>> = LazyLock::new(Default::default);

/// The store, with the cache loaded on first use.
fn store() -> MutexGuard<'static, Store> {
    let mut s = STORE.lock();
    if !s.loaded {
        s.loaded = true;
        load_cache(&dir(), &mut s);
    }
    s
}

struct Session {
    id: u64,
    api: Arc<Api>,
    listener: Arc<Listener>,
}

static SESSION: Mutex<Option<Arc<Session>>> = Mutex::new(None);
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
/// bumped by every login attempt / logout: a login that finishes after a newer one started is dropped
static GEN: AtomicU64 = AtomicU64::new(0);
static RETRY: Mutex<Option<AbortHandle>> = Mutex::new(None);
static QR_ABORT: Mutex<Option<AbortHandle>> = Mutex::new(None);

fn session() -> Option<Arc<Session>> {
    SESSION.lock().clone()
}

/// The logged-in API, online or not.
fn api_any() -> Option<Arc<Api>> {
    session().map(|s| s.api.clone())
}

fn need() -> anyhow::Result<Arc<Api>> {
    let a = api_any();
    match a {
        Some(a) if store().state.status == "online" => Ok(a),
        _ => anyhow::bail!("Zalo chưa đăng nhập. Mở tab Zalo trong VIBE CODE và quét QR (hoặc bấm Kết nối lại)."),
    }
}

pub fn state() -> ZaloState {
    store().state.clone()
}

fn set_state(f: impl FnOnce(&mut ZaloState)) {
    let st = {
        let mut s = store();
        f(&mut s.state);
        s.state.clone()
    };
    emit("zalo:state", (st,));
}

fn spawn<F: Future<Output = ()> + Send + 'static>(f: F) {
    tauri::async_runtime::spawn(f);
}

/* ---------------- cache on disk ---------------- */

/// cache.json: everything but the messages.
#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct Meta {
    version: u32,
    threads: Vec<ZaloThread>,
    names: IndexMap<String, String>,
    avatars: IndexMap<String, String>,
    /// version 1 kept every conversation's messages in here too
    #[serde(skip_serializing_if = "Option::is_none")]
    messages: Option<IndexMap<String, Vec<ZaloMessage>>>,
}

/// A conversation's file name: Zalo ids are digits (groups may carry a letter); anything else is
/// hex-encoded so it can never leave the folder.
fn thread_file(dir: &Path, id: &str) -> PathBuf {
    let safe = !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    // "~" never occurs in a kept-as-is id, so an encoded name can't collide with one
    let name = if safe { id.to_owned() } else { format!("~{}", id.bytes().map(|b| format!("{b:02x}")).collect::<String>()) };
    dir.join("msgs").join(format!("{name}.json"))
}

/// The newest `MAX_PER_THREAD`, oldest first.
fn keep_newest(list: &mut Vec<ZaloMessage>) {
    if list.len() > MAX_PER_THREAD {
        list.drain(..list.len() - MAX_PER_THREAD);
    }
}

fn load_cache(dir: &Path, s: &mut Store) {
    let Some(meta) = std::fs::read(dir.join("cache.json")).ok().and_then(|b| serde_json::from_slice::<Meta>(&b).ok()) else { return };
    for t in meta.threads {
        s.threads.insert(t.id.clone(), t);
    }
    s.names = meta.names;
    s.avatars = meta.avatars;
    match meta.messages {
        // the old single-file cache: split it up on the next save
        Some(all) => {
            for (id, mut list) in all {
                list.sort_by_key(|m| m.ts);
                keep_newest(&mut list);
                s.dirty.insert(id.clone());
                s.messages.insert(id, list);
            }
            s.meta_dirty = true;
        }
        None => {
            let ids: Vec<String> = s.threads.keys().cloned().collect();
            for id in ids {
                let Ok(b) = std::fs::read(thread_file(dir, &id)) else { continue };
                if let Ok(mut list) = serde_json::from_slice::<Vec<ZaloMessage>>(&b) {
                    keep_newest(&mut list);
                    s.messages.insert(id, list);
                }
            }
        }
    }
}

/// What the next save writes: (file, bytes) pairs, encoded under the lock (only what changed).
fn take_writes(dir: &Path, s: &mut Store) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    for id in std::mem::take(&mut s.dirty) {
        let list = s.messages.get(&id).map(Vec::as_slice).unwrap_or_default();
        if let Ok(b) = serde_json::to_vec(list) {
            out.push((thread_file(dir, &id), b));
        }
    }
    if std::mem::take(&mut s.meta_dirty) {
        let meta = Meta {
            version: CACHE_VERSION,
            threads: s.threads.values().cloned().collect(),
            names: s.names.clone(),
            avatars: s.avatars.clone(),
            messages: None,
        };
        if let Ok(b) = serde_json::to_vec(&meta) {
            out.push((dir.join("cache.json"), b));
        }
    }
    out
}

#[derive(Default)]
struct Timers {
    save_pending: bool,
    ui_pending: bool,
}
static TIMERS: Mutex<Timers> = Mutex::new(Timers { save_pending: false, ui_pending: false });

/// Write what changed a little later (many messages arriving together make one save).
fn schedule_save() {
    let mut t = TIMERS.lock();
    if t.save_pending {
        return;
    }
    t.save_pending = true;
    spawn(async {
        tokio::time::sleep(Duration::from_secs(3)).await;
        TIMERS.lock().save_pending = false;
        let writes = {
            let mut s = store();
            take_writes(&dir(), &mut s)
        };
        for (path, data) in writes {
            let _ = core::write_atomic(path, data).await;
        }
    });
}

/// Names / avatars changed: the task list re-reads them (once for a burst).
fn notify_ui() {
    let mut t = TIMERS.lock();
    if t.ui_pending {
        return;
    }
    t.ui_pending = true;
    spawn(async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        TIMERS.lock().ui_pending = false;
        emit("zalo:changed", (Vec::<String>::new(),));
    });
}

/* ---------------- parsing Zalo messages ---------------- */

pub fn attach_label(kind: &str) -> &'static str {
    match kind {
        "photo" => "Ảnh",
        "video" => "Video",
        "voice" => "Tin nhắn thoại",
        "sticker" => "Sticker",
        "gif" => "GIF",
        "file" => "File",
        "link" => "Link",
        "location" => "Vị trí",
        "card" => "Danh thiếp",
        _ => "Tin nhắn",
    }
}

fn parse_json_obj(v: Option<&Value>) -> Value {
    match v {
        Some(o @ Value::Object(_)) => o.clone(),
        Some(Value::String(s)) if s.trim().starts_with('{') => serde_json::from_str::<Value>(s).ok().filter(Value::is_object).unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

fn vstr(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

fn vnum(v: Option<&Value>) -> Option<f64> {
    let n = jnum(v);
    (n.is_finite() && n > 0.0).then_some(n)
}

/// What an attachment message carries, in one shape.
fn parse_attachment(msg_type: &str, content: &Value) -> Option<ZaloAttachment> {
    if !content.is_object() {
        return None;
    }
    let c = content;
    let p = parse_json_obj(c.get("params"));
    let base = ZaloAttachment { url: vstr(c.get("href")), thumb: vstr(c.get("thumb")), title: vstr(c.get("title")), desc: vstr(c.get("description")), ..Default::default() };
    let t = msg_type.strip_prefix("chat.").or_else(|| msg_type.strip_prefix("share.")).unwrap_or(msg_type);
    let k = |kind: &str, a: ZaloAttachment| Some(ZaloAttachment { kind: kind.into(), ..a });
    let dur = || vnum(p.get("duration")).map(|d| d / 1000.0);
    if t == "photo" || t == "doodle" {
        let url = vstr(p.get("hd")).or_else(|| vstr(p.get("rawUrl"))).or(base.url.clone());
        return k("photo", ZaloAttachment { url, width: vnum(p.get("width")), height: vnum(p.get("height")), ..base });
    }
    if t.starts_with("video") {
        return k("video", ZaloAttachment { width: vnum(p.get("video_width")), height: vnum(p.get("video_height")), duration: dur(), ..base });
    }
    if t == "voice" {
        return k("voice", ZaloAttachment { duration: dur(), ..base });
    }
    if t == "gif" {
        return k("gif", ZaloAttachment { width: vnum(p.get("width")), height: vnum(p.get("height")), ..base });
    }
    if t == "sticker" {
        return vnum(c.get("id")).map(|_| ZaloAttachment { kind: "sticker".into(), ..Default::default() });
    }
    if t == "file" {
        return k("file", ZaloAttachment { size: vnum(p.get("fileSize")), ext: vstr(p.get("fileExt")).map(|e| e.to_lowercase()), ..base });
    }
    if t.starts_with("location") {
        return k("location", base);
    }
    if t == "recommended" || t == "link" {
        let action = vstr(c.get("action")).unwrap_or_default();
        if action.contains("user") {
            let desc = vstr(p.get("phone")).or(base.desc.clone());
            return k("card", ZaloAttachment { desc, ..base });
        }
        return k("link", base);
    }
    k("other", base)
}

/// Readable text for any Zalo message: plain text as is, attachments as "[kind] title url".
fn content_text(att: Option<&ZaloAttachment>, content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_owned();
    }
    let Some(a) = att else { return "[tin nhắn]".into() };
    let mut parts = vec![format!("[{}]", attach_label(&a.kind).to_lowercase())];
    for v in [&a.title, &a.desc, &a.url].into_iter().flatten() {
        if !parts.contains(v) {
            parts.push(v.clone());
        }
    }
    parts.join(" ")
}

/// One line for the conversation list.
pub fn preview_of(m: &ZaloMessage) -> String {
    if m.recalled == Some(true) {
        return "Tin nhắn đã thu hồi".into();
    }
    let Some(a) = &m.att else { return m.text.clone() };
    let title = |d: &str| a.title.clone().unwrap_or_else(|| d.into());
    match a.kind.as_str() {
        "file" => format!("📎 {}", title("File")),
        "photo" => format!("🖼 {}", title("Ảnh")),
        "video" => format!("🎬 {}", title("Video")),
        "voice" => "🎤 Tin nhắn thoại".into(),
        "sticker" => "Sticker".into(),
        "gif" => "GIF".into(),
        "link" => format!("🔗 {}", a.title.clone().or(a.url.clone()).unwrap_or_else(|| "Link".into())),
        "location" => format!("📍 {}", title("Vị trí")),
        "card" => format!("👤 {}", title("Danh thiếp")),
        _ => a.title.clone().unwrap_or_else(|| m.text.clone()),
    }
}

fn first_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn refresh_thread_last(s: &mut Store, thread_id: &str) {
    let Some(last) = s.messages.get(thread_id).and_then(|a| a.last()) else { return };
    let who = if last.group && !last.mine { format!("{}: ", last.from_name) } else { String::new() };
    let (ts, line) = (last.ts, who + &first_chars(&preview_of(last), 200));
    if let Some(t) = s.threads.get_mut(thread_id) {
        t.last_ts = ts;
        t.last = line;
    }
}

fn to_message(s: &Store, m: &Msg) -> ZaloMessage {
    let d = &m.data;
    let group = m.kind == ThreadType::Group;
    let content = d.get("content").cloned().unwrap_or(Value::Null);
    let msg_type = jstr(d.get("msgType"));
    let att = parse_attachment(&msg_type, &content);
    let uid_from = jstr(d.get("uidFrom"));
    let from_id = if m.is_self { s.state.me.as_ref().map(|me| me.id.clone()).unwrap_or_else(|| uid_from.clone()) } else { uid_from.clone() };
    let q = d.get("quote").filter(|q| truthy(Some(q)));
    let mentions: Option<Vec<MentionRef>> = d.get("mentions").and_then(Value::as_array).map(|list| {
        list.iter().map(|x| MentionRef { uid: jstr(x.get("uid")), pos: jnum(x.get("pos")) as i64, len: jnum(x.get("len")) as i64 }).collect()
    });
    let d_name = d.get("dName").and_then(Value::as_str).filter(|n| !n.is_empty());
    let ts = jnum(d.get("ts"));
    let attach_text = |q: &Value| if truthy(q.get("attach")) { "[tệp đính kèm]".to_owned() } else { String::new() };
    let qmsg = |q: &Value| q.get("msg").filter(|v| truthy(Some(v))).map(|v| jstr(Some(v)));
    ZaloMessage {
        // no id at all: "" (jstr would give "undefined", and every such message would share it)
        id: d.get("msgId").filter(|v| !v.is_null()).map(|v| jstr(Some(v))).unwrap_or_default(),
        thread_id: m.thread_id.clone(),
        group,
        from_id,
        from_name: if m.is_self {
            "Tôi".into()
        } else {
            d_name.map(str::to_owned).or_else(|| s.names.get(&uid_from).filter(|n| !n.is_empty()).cloned()).unwrap_or_else(|| uid_from.clone())
        },
        mine: m.is_self,
        ts: if ts.is_finite() && ts != 0.0 { ts as i64 } else { now_ms() },
        kind: msg_type,
        text: content_text(att.as_ref(), &content),
        att,
        mentions: mentions.filter(|m| !m.is_empty()),
        quote: q.filter(|q| truthy(q.get("msg")) || truthy(q.get("attach"))).map(|q| {
            format!("{}: {}", jstr(q.get("fromD")), qmsg(q).unwrap_or_else(|| "[tệp đính kèm]".into()))
        }),
        reply: q.map(|q| Reply {
            id: jstr(q.get("globalMsgId")),
            from: q.get("fromD").map(|v| jstr(Some(v))).unwrap_or_default(),
            text: qmsg(q).unwrap_or_else(|| attach_text(q)),
        }),
        recalled: None,
    }
}

/// Put `msg` in its place (the list stays sorted by time). `None`: already there.
///
/// Messages arrive almost always newest-last, so this is a binary search plus a look at the few
/// messages with the same timestamp — not a scan of the whole conversation per message.
fn insert_sorted(list: &mut Vec<ZaloMessage>, msg: ZaloMessage) -> Option<usize> {
    let from = list.partition_point(|x| x.ts < msg.ts);
    let to = from + list[from..].partition_point(|x| x.ts == msg.ts);
    if list[from..to].iter().any(|x| x.id == msg.id) {
        return None;
    }
    list.insert(to, msg);
    Some(to)
}

/// What [`ingest_into`] leaves for afterwards (outside the lock).
#[derive(Default, Debug, PartialEq)]
struct Ingested {
    /// conversations that got new messages
    touched: IndexSet<String>,
    /// group members whose avatar we don't have yet
    profiles: IndexSet<String>,
}

/// Store messages in `s`: sorted, without duplicates, capped per conversation; conversations
/// are created as they first show up and named from what the messages tell.
fn ingest_into(s: &mut Store, list: &[Msg]) -> Ingested {
    let mut out = Ingested::default();
    for m in list {
        let msg = to_message(s, m);
        if msg.id.is_empty() {
            continue;
        }
        let d_name = m.data.get("dName").and_then(Value::as_str).filter(|n| !n.is_empty());
        if !m.is_self
            && let Some(n) = d_name
            && s.names.get(&msg.from_id).is_none_or(|old| old != n)
        {
            s.names.insert(msg.from_id.clone(), n.to_owned());
            s.meta_dirty = true;
        }
        let (group, from_id, mine) = (msg.group, msg.from_id.clone(), msg.mine);
        let arr = s.messages.entry(m.thread_id.clone()).or_default();
        let Some(pos) = insert_sorted(arr, msg) else { continue };
        let newest = pos + 1 == arr.len();
        keep_newest(arr);
        if !s.threads.contains_key(&m.thread_id) {
            let t = ZaloThread {
                id: m.thread_id.clone(),
                group,
                name: s.names.get(&m.thread_id).cloned().unwrap_or_default(),
                avatar: s.avatars.get(&m.thread_id).cloned(),
                ..Default::default()
            };
            s.threads.insert(m.thread_id.clone(), t);
        }
        if let Some(t) = s.threads.get_mut(&m.thread_id)
            && t.name.is_empty()
            && !group
            && !mine
            && let Some(n) = d_name
        {
            t.name = n.to_owned();
        }
        if newest {
            refresh_thread_last(s, &m.thread_id);
        }
        if group && !mine && !s.avatars.contains_key(&from_id) {
            out.profiles.insert(from_id);
        }
        s.dirty.insert(m.thread_id.clone());
        s.meta_dirty = true;
        out.touched.insert(m.thread_id.clone());
    }
    out
}

/// Store messages. `live`: just arrived (the task list looks at them right away).
fn ingest(list: Vec<Msg>, live: bool) {
    if list.is_empty() {
        return;
    }
    let got = ingest_into(&mut store(), &list);
    for uid in got.profiles {
        want_profile(uid);
    }
    if got.touched.is_empty() {
        return;
    }
    schedule_save();
    if live {
        crate::zalo_tasks::on_ingest(got.touched.into_iter().collect());
    }
}

/// Conversations with something addressed to me since `since`: every personal chat with an
/// incoming message, and groups where someone @mentioned me or replied to me — with my own later
/// messages, so it's clear what I answered. `only`: just these conversations.
pub fn work_feed(since: i64, max_threads: usize, max_msgs: usize, only: Option<&[String]>) -> Vec<WorkThread> {
    work_feed_in(&store(), since, max_threads, max_msgs, only)
}

fn work_feed_in(s: &Store, since: i64, max_threads: usize, max_msgs: usize, only: Option<&[String]>) -> Vec<WorkThread> {
    let me = s.state.me.as_ref().map(|m| m.id.as_str());
    let mut out = Vec::new();
    let threads: Box<dyn Iterator<Item = &ZaloThread>> = match only {
        Some(ids) => Box::new(ids.iter().filter_map(|id| s.threads.get(id))),
        None => Box::new(s.threads.values()),
    };
    for t in threads {
        let Some(arr) = s.messages.get(&t.id).filter(|a| !a.is_empty()) else { continue };
        if arr[arr.len() - 1].ts < since {
            continue;
        }
        // only the recent part is looked at (the list is sorted by time)
        let recent_from = arr.partition_point(|m| m.ts < since);
        let mine: IndexSet<&str> = arr.iter().filter(|m| m.mine).map(|m| m.id.as_str()).collect();
        let recent: Vec<&ZaloMessage> = arr[recent_from..].iter().filter(|m| m.recalled != Some(true)).collect();
        let to_me = |m: &ZaloMessage| {
            !m.mine
                && (!t.group
                    || m.mentions.as_ref().is_some_and(|l| l.iter().any(|x| Some(x.uid.as_str()) == me))
                    || m.reply.as_ref().is_some_and(|r| mine.contains(r.id.as_str())))
        };
        let Some(first) = recent.iter().position(|m| to_me(m)) else { continue };
        let pick: Vec<&&ZaloMessage> = recent[first..].iter().filter(|m| !t.group || m.mine || to_me(m)).collect();
        let msgs = pick[pick.len().saturating_sub(max_msgs)..]
            .iter()
            .map(|m| {
                let reply = m.reply.as_ref().map(|r| format!("[trả lời {}: \"{}\"] ", r.from, first_chars(&r.text, 80))).unwrap_or_default();
                let body = if m.text.is_empty() { preview_of(m) } else { m.text.clone() };
                WorkMsg {
                    id: m.id.clone(),
                    ts: m.ts,
                    from: if m.mine { "Tôi".into() } else { m.from_name.clone() },
                    from_id: m.from_id.clone(),
                    mine: m.mine,
                    to_me: to_me(m),
                    text: first_chars(&(reply + &body), 600),
                }
            })
            .collect();
        out.push(WorkThread { thread_id: t.id.clone(), name: if t.name.is_empty() { t.id.clone() } else { t.name.clone() }, group: t.group, msgs });
    }
    out.sort_by_key(|t| std::cmp::Reverse(t.msgs.last().map(|m| m.ts).unwrap_or(0)));
    out.truncate(max_threads);
    out
}

pub fn self_id() -> Option<String> {
    store().state.me.as_ref().map(|m| m.id.clone())
}

/// A user's avatar url, if known (queues a profile fetch otherwise).
pub fn avatar_of(user_id: &str) -> Option<String> {
    let a = store().avatars.get(user_id).cloned();
    if a.is_none() {
        want_profile(user_id.to_owned());
    }
    a
}

/* ---------------- profiles, fetched lazily ---------------- */

fn want_profile(uid: String) {
    if uid.is_empty() || api_any().is_none() {
        return;
    }
    let mut s = store();
    if !s.profile_queue.insert(uid) || s.profile_timer {
        return;
    }
    s.profile_timer = true;
    spawn(async {
        tokio::time::sleep(Duration::from_millis(800)).await;
        flush_profiles().await;
    });
}

async fn flush_profiles() {
    let ids: Vec<String> = {
        let mut s = store();
        s.profile_timer = false;
        s.profile_queue.iter().filter(|id| !s.avatars.contains_key(*id)).take(100).cloned().collect()
    };
    let Some(a) = api_any() else { return };
    if ids.is_empty() {
        return;
    }
    let Ok(r) = a.get_user_info(&ids).await else { return };
    if apply_profiles(&mut store(), &r) {
        schedule_save();
        notify_ui();
    }
}

/// `getUserInfo`'s answer into names / avatars. Whether anything changed.
fn apply_profiles(s: &mut Store, r: &Value) -> bool {
    let mut any = false;
    for (k, p) in r["changed_profiles"].as_object().into_iter().flatten() {
        let id = k.strip_suffix("_0").unwrap_or(k).to_owned();
        let avatar = vstr(p.get("avatar"));
        let name = vstr(p.get("displayName")).or_else(|| vstr(p.get("zaloName")));
        if let Some(av) = &avatar {
            s.avatars.insert(id.clone(), av.clone());
            any = true;
        }
        if let Some(n) = &name {
            s.names.insert(id.clone(), n.clone());
            any = true;
        }
        if let Some(t) = s.threads.get_mut(&id) {
            if avatar.is_some() {
                t.avatar = avatar;
            }
            if t.name.is_empty() {
                t.name = name.unwrap_or_default();
            }
        }
    }
    s.meta_dirty |= any;
    any
}

/* ---------------- recalls ---------------- */

fn recall_locally(m: &mut ZaloMessage) {
    m.recalled = Some(true);
    m.text = "Tin nhắn đã được thu hồi".into();
    m.att = None;
    m.reply = None;
}

/// A message was recalled. Whether it was one we have.
fn apply_undo_in(s: &mut Store, u: &Action) -> bool {
    let id = jstr(u.data["content"].get("globalMsgId"));
    let Some(m) = s.messages.get_mut(&u.thread_id).and_then(|a| a.iter_mut().rev().find(|m| m.id == id)) else { return false };
    recall_locally(m);
    refresh_thread_last(s, &u.thread_id);
    s.dirty.insert(u.thread_id.clone());
    s.meta_dirty = true;
    true
}

/* ---------------- session ---------------- */

async fn save_creds(c: Credentials) {
    let Ok(raw) = serde_json::to_vec(&c) else { return };
    let body = match crate::secure::seal(&raw) {
        Ok(enc) => [b"E1".as_slice(), &enc].concat(),
        Err(_) => [b"P1".as_slice(), &raw].concat(),
    };
    let _ = core::write_atomic(cred_file(), body).await;
}

fn read_creds() -> Option<Credentials> {
    let b = std::fs::read(cred_file()).ok()?;
    let (tag, rest) = b.split_at_checked(2)?;
    let raw = if tag == b"E1" { crate::secure::open(rest).ok()? } else { rest.to_vec() };
    serde_json::from_slice(&raw).ok()
}

fn after_login(a: Arc<Api>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let listener = match Listener::new(a.clone(), tx) {
        Ok(l) => l,
        Err(e) => return set_state(|s| {
            s.status = "error";
            s.error = Some(format!("{e:#}"));
        }),
    };
    let id = NEXT_SESSION.fetch_add(1, Ordering::SeqCst);
    *SESSION.lock() = Some(Arc::new(Session { id, api: a.clone(), listener: listener.clone() }));
    store().retries = 0;
    let uid = a.uid.clone();
    set_state(|s| {
        s.status = "online";
        s.qr = None;
        s.error = None;
        let prev = s.me.take().unwrap_or_default();
        s.me = Some(ZaloSelf { id: uid.clone(), name: prev.name, avatar: prev.avatar });
    });
    let acc = a.clone();
    spawn(async move {
        if let Ok(r) = acc.fetch_account_info().await {
            let p = &r["profile"];
            let name = vstr(p.get("displayName")).or_else(|| vstr(p.get("zaloName"))).unwrap_or_default();
            let avatar = p.get("avatar").and_then(Value::as_str).unwrap_or_default().to_owned();
            set_state(|s| s.me = Some(ZaloSelf { id: uid, name, avatar }));
        }
    });
    let n = a.clone();
    spawn(async move { load_names(&n).await });
    spawn(run_events(id, Arc::downgrade(&listener), rx));
    if let Err(e) = listener.start(true) {
        set_state(|s| {
            s.status = "error";
            s.error = Some(format!("{e:#}"));
        });
    }
}

fn is_current(id: u64) -> bool {
    session().is_some_and(|s| s.id == id)
}

/// Live messages that arrive together, as one batch: one lock, one save, one task-list look.
fn next_batch(first: Event, rx: &mut mpsc::UnboundedReceiver<Event>) -> (Vec<Msg>, Option<Event>) {
    let Event::Message(m) = first else { return (Vec::new(), Some(first)) };
    let mut batch = vec![m];
    while batch.len() < 500 {
        match rx.try_recv() {
            Ok(Event::Message(m)) => batch.push(m),
            Ok(other) => return (batch, Some(other)),
            Err(_) => break,
        }
    }
    (batch, None)
}

async fn run_events(id: u64, listener: Weak<Listener>, mut rx: mpsc::UnboundedReceiver<Event>) {
    while let Some(first) = rx.recv().await {
        let (live, rest) = next_batch(first, &mut rx);
        ingest(live, true);
        let Some(ev) = rest else { continue };
        match ev {
            Event::CipherKey => {
                if is_current(id)
                    && let Some(l) = listener.upgrade()
                {
                    request_replay(&l);
                }
            }
            Event::Message(m) => ingest(vec![m], true),
            Event::OldMessages(list, kind) => {
                let oldest = list.iter().min_by(|x, y| jnum(x.data.get("ts")).total_cmp(&jnum(y.data.get("ts")))).map(|m| jstr(m.data.get("msgId")));
                let n = list.len();
                ingest(list, false);
                if n > 0 {
                    let i = if kind == ThreadType::User { 0 } else { 1 };
                    let more = {
                        let mut s = store();
                        s.replay_pages[i] += 1;
                        s.replay_pages[i] < OLD_PAGES
                    };
                    if more && let Some(l) = listener.upgrade() {
                        l.request_old_messages(kind, oldest);
                    }
                }
            }
            Event::Undo(u) => {
                if apply_undo_in(&mut store(), &u) {
                    schedule_save();
                }
            }
            Event::Closed(code) => {
                on_closed(id, code);
                break;
            }
            Event::Error(e) => {
                if cfg!(debug_assertions) {
                    eprintln!("zalo listener: {e}");
                }
            }
        }
    }
}

fn on_closed(id: u64, code: u16) {
    if !is_current(id) {
        return;
    }
    // 3000: the account opened Zalo Web somewhere else — only one web session may listen
    if code == 3000 {
        return set_state(|s| {
            s.status = "error";
            s.error = Some("Zalo Web đang mở ở nơi khác nên phiên này bị ngắt. Bấm Kết nối lại.".into());
        });
    }
    let retries = {
        let mut s = store();
        (s.retries < 5).then(|| {
            s.retries += 1;
            s.retries
        })
    };
    let Some(retries) = retries else {
        return set_state(|s| {
            s.status = "error";
            s.error = Some(format!("Mất kết nối Zalo ({code})."));
        });
    };
    set_state(|s| {
        s.status = "connecting";
        s.error = None;
    });
    let h = tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(2000 * retries as u64)).await;
        RETRY.lock().take();
        reconnect(true).await;
    });
    if let Some(old) = RETRY.lock().replace(h.inner().abort_handle()) {
        old.abort();
    }
}

fn request_replay(l: &Listener) {
    store().replay_pages = [0, 0];
    l.request_old_messages(ThreadType::User, None);
    l.request_old_messages(ThreadType::Group, None);
}

fn message_count() -> i64 {
    store().messages.values().map(Vec::len).sum::<usize>() as i64
}

/// "Đồng bộ tất cả": names, the socket replay again, and the latest page of every group.
/// Returns how many new messages came in.
pub async fn sync_all() -> anyhow::Result<i64> {
    let a = need()?;
    let before = message_count();
    load_names(&a).await;
    if let Some(s) = session() {
        request_replay(&s.listener);
    }
    let groups: Vec<String> = store().threads.values().filter(|t| t.group).map(|t| t.id.clone()).collect();
    for chunk in groups.chunks(4) {
        let pages = futures_util::future::join_all(chunk.iter().map(|id| a.get_cloud_message(id, None, false))).await;
        for (_, msgs) in pages.into_iter().flatten() {
            ingest(msgs, false);
        }
    }
    Ok(message_count() - before)
}

/// Friend and group names / avatars, so conversations show who they are.
async fn load_names(a: &Api) {
    if let Ok(list) = a.get_all_friends().await {
        apply_friends(&mut store(), &list);
        schedule_save();
        notify_ui();
    }
    let Ok(groups) = a.get_all_groups().await else { return };
    let ids: Vec<String> = groups["gridVerMap"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
    for chunk in ids.chunks(50) {
        let Ok(info) = a.get_group_info(chunk).await else { break };
        apply_groups(&mut store(), &info);
    }
    schedule_save();
    notify_ui();
}

fn apply_friends(s: &mut Store, list: &[Value]) {
    for f in list {
        let id = jstr(f.get("userId"));
        if id.is_empty() {
            continue;
        }
        let name = vstr(f.get("displayName")).or_else(|| vstr(f.get("zaloName"))).unwrap_or_default();
        s.names.insert(id.clone(), name);
        if let Some(av) = vstr(f.get("avatar")) {
            s.avatars.insert(id, av);
        }
    }
    let Store { threads, names, avatars, .. } = s;
    for t in threads.values_mut().filter(|t| !t.group) {
        if let Some(n) = names.get(&t.id).filter(|n| !n.is_empty()) {
            t.name = n.clone();
        }
        if let Some(av) = avatars.get(&t.id) {
            t.avatar = Some(av.clone());
        }
    }
    s.meta_dirty = true;
}

/// Group names / avatars. Groups we have no messages of aren't added as conversations (an
/// account can be in hundreds); their names are still known for when a message comes.
fn apply_groups(s: &mut Store, info: &Value) {
    for (gid, g) in info["gridInfoMap"].as_object().into_iter().flatten() {
        let name = jstr(g.get("name"));
        let avt = vstr(g.get("avt"));
        s.names.insert(gid.clone(), name.clone());
        if let Some(av) = &avt {
            s.avatars.insert(gid.clone(), av.clone());
        }
        if let Some(t) = s.threads.get_mut(gid) {
            t.group = true;
            t.name = name;
            t.avatar = avt;
        }
    }
    s.meta_dirty = true;
}

/// Start: log in with the saved session, if any.
pub fn init() {
    drop(store());
    if read_creds().is_some() {
        spawn(async {
            reconnect(false).await;
        });
    }
}

pub async fn reconnect(auto: bool) -> ZaloState {
    if let Some(h) = RETRY.lock().take() {
        h.abort();
    }
    if !auto {
        store().retries = 0;
    }
    let Some(creds) = read_creds() else {
        set_state(|s| s.status = "off");
        return state();
    };
    let generation = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    stop_listener();
    set_state(|s| {
        s.status = "connecting";
        s.error = None;
    });
    let r = api::login(&creds).await;
    if GEN.load(Ordering::SeqCst) == generation {
        match r {
            Ok(a) => after_login(a),
            Err(e) => set_state(|s| {
                s.status = "error";
                s.error = Some(format!("Không đăng nhập lại được: {e}. Hãy đăng nhập lại bằng QR."));
            }),
        }
    }
    state()
}

fn on_qr_event(generation: u64, ev: QrEvent) {
    if GEN.load(Ordering::SeqCst) != generation {
        return;
    }
    match ev {
        QrEvent::Generated { image } => set_state(|s| {
            s.status = "qr";
            s.qr = Some(format!("data:image/png;base64,{image}"));
            s.scanned = None;
        }),
        QrEvent::Expired => set_state(|s| {
            s.status = "qr";
            s.qr = None;
            s.error = Some("Mã QR đã hết hạn.".into());
        }),
        QrEvent::Scanned { display_name } => set_state(|s| s.scanned = Some(display_name)),
        QrEvent::Declined => set_state(|s| {
            s.status = "off";
            s.qr = None;
            s.error = Some("Đã từ chối đăng nhập trên điện thoại.".into());
        }),
        QrEvent::GotLoginInfo(c) => spawn(save_creds(c)),
    }
}

pub async fn login_qr() {
    let generation = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    stop_listener();
    set_state(|s| {
        s.status = "qr";
        s.qr = None;
        s.error = None;
    });
    let task = tauri::async_runtime::spawn(api::login_qr(move |ev| on_qr_event(generation, ev)));
    if let Some(old) = QR_ABORT.lock().replace(task.inner().abort_handle()) {
        old.abort();
    }
    let r = task.await;
    if GEN.load(Ordering::SeqCst) != generation {
        return;
    }
    QR_ABORT.lock().take();
    let err = match r {
        Ok(Ok(a)) => return after_login(a),
        Ok(Err(e)) => e.to_string(),
        Err(_) => api::QR_ABORTED.to_owned(),
    };
    if state().status == "qr" {
        set_state(|s| {
            s.status = if s.error.is_some() { "qr" } else { "off" };
            s.qr = None;
            if s.error.is_none() {
                s.error = Some(err);
            }
        });
    }
}

pub fn cancel_login() {
    if let Some(h) = QR_ABORT.lock().take() {
        h.abort();
        GEN.fetch_add(1, Ordering::SeqCst);
    }
    set_state(|s| {
        s.status = "off";
        s.qr = None;
        s.scanned = None;
        s.error = None;
    });
}

fn stop_listener() {
    if let Some(s) = SESSION.lock().take() {
        s.listener.stop();
    }
}

/// Forget the session and everything cached.
pub async fn logout() {
    if let Some(h) = RETRY.lock().take() {
        h.abort();
    }
    GEN.fetch_add(1, Ordering::SeqCst);
    stop_listener();
    {
        let mut s = store();
        s.threads.clear();
        s.messages.clear();
        s.names.clear();
        s.avatars.clear();
        s.profile_queue.clear();
        s.dirty.clear();
        s.meta_dirty = false;
    }
    let _ = tokio::fs::remove_dir_all(dir()).await;
    set_state(|s| {
        s.status = "off";
        s.me = None;
        s.qr = None;
        s.error = None;
    });
    emit("zalo:changed", (Vec::<String>::new(),));
}

/* ---------------- reading (Claude's tools, the task list) ---------------- */

fn type_of(thread_id: &str, group: Option<bool>) -> ThreadType {
    let g = group.unwrap_or_else(|| store().threads.get(thread_id).is_some_and(|t| t.group));
    if g { ThreadType::Group } else { ThreadType::User }
}

pub fn list_threads(query: &str, limit: usize) -> Vec<ZaloThread> {
    list_threads_in(&store(), query, limit)
}

fn list_threads_in(s: &Store, query: &str, limit: usize) -> Vec<ZaloThread> {
    let q = query.trim().to_lowercase();
    let mut list: Vec<&ZaloThread> = s.threads.values().filter(|t| q.is_empty() || t.name.to_lowercase().contains(&q) || t.id.contains(&q)).collect();
    list.sort_by_key(|t| std::cmp::Reverse(t.last_ts));
    list.into_iter().take(limit).cloned().collect()
}

pub fn get_messages(thread_id: &str, limit: usize, before: Option<i64>) -> Vec<ZaloMessage> {
    get_messages_in(&store(), thread_id, limit, before)
}

fn get_messages_in(s: &Store, thread_id: &str, limit: usize, before: Option<i64>) -> Vec<ZaloMessage> {
    let all = s.messages.get(thread_id).map(Vec::as_slice).unwrap_or_default();
    // sorted by time: everything before `before` is a prefix
    let end = before.filter(|b| *b != 0).map_or(all.len(), |b| all.partition_point(|m| m.ts < b));
    all[end.saturating_sub(limit)..end].to_vec()
}

/// Older messages of a group from Zalo's cloud (1-to-1 chats only have what the socket replayed).
pub async fn load_older(thread_id: &str, pages: usize) -> anyhow::Result<i64> {
    let a = need()?;
    let count = || store().messages.get(thread_id).map(Vec::len).unwrap_or(0) as i64;
    if !store().threads.get(thread_id).is_some_and(|t| t.group) {
        return Ok(0);
    }
    let before = count();
    let mut cursor = store().messages.get(thread_id).and_then(|a| a.first()).map(|m| m.id.clone());
    let mut is_old = false;
    for _ in 0..pages {
        let (r, msgs) = a.get_cloud_message(thread_id, cursor.as_deref(), is_old).await?;
        let empty = msgs.is_empty();
        ingest(msgs, false);
        if !truthy(r.get("hasMore")) || (empty && truthy(r.get("isFiltered"))) {
            break;
        }
        cursor = r.get("lastMsgId").map(|v| jstr(Some(v)));
        is_old = truthy(r.get("isOld"));
    }
    Ok(count() - before)
}

pub fn search_messages(query: &str, thread_id: Option<&str>, limit: usize) -> Vec<ZaloMessage> {
    search_messages_in(&store(), query, thread_id, limit)
}

fn search_messages_in(s: &Store, query: &str, thread_id: Option<&str>, limit: usize) -> Vec<ZaloMessage> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let lists: Vec<&Vec<ZaloMessage>> = match thread_id {
        Some(t) => s.messages.get(t).into_iter().collect(),
        None => s.messages.values().collect(),
    };
    let mut hits: Vec<&ZaloMessage> = lists.into_iter().flatten().filter(|m| m.text.to_lowercase().contains(&q) || m.from_name.to_lowercase().contains(&q)).collect();
    hits.sort_by_key(|m| std::cmp::Reverse(m.ts));
    hits.into_iter().take(limit).cloned().collect()
}

/* ---------------- sending ---------------- */

/// A text to a person or group; the new message's id.
pub async fn send_text(thread_id: &str, text: &str, group: Option<bool>) -> anyhow::Result<String> {
    let a = need()?;
    let r = a.send_text(text, thread_id, type_of(thread_id, group)).await?;
    Ok(r.get("msgId").map(|v| jstr(Some(v))).unwrap_or_default())
}

/// "HH:MM:SS dd/mm/yyyy" in local time (`toLocaleString('vi-VN', {hour12: false})`).
pub fn fmt_time(ts: i64) -> String {
    use chrono::TimeZone;
    chrono::Local.timestamp_millis_opt(ts).single().map(|d| d.format("%H:%M:%S %d/%m/%Y").to_string()).unwrap_or_default()
}

/* ---------------- commands (the Zalo tab) ---------------- */

#[tauri::command]
pub async fn zalo_state() -> ZaloState {
    state()
}

#[tauri::command]
pub async fn zalo_login() {
    login_qr().await
}

#[tauri::command]
pub async fn zalo_cancel() {
    cancel_login()
}

#[tauri::command]
pub async fn zalo_reconnect() -> ZaloState {
    reconnect(false).await
}

#[tauri::command]
pub async fn zalo_logout() {
    logout().await
}

#[tauri::command]
pub async fn zalo_sync_all() -> Res<i64> {
    Ok(sync_all().await?)
}

#[cfg(test)]
mod tests;
