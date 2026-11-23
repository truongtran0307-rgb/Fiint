//! Zalo Web's websocket: live messages, the recent history Zalo replays on request and recalls.
//! Port of zca-js `apis/listen.js` (only the events VIBE CODE uses) plus the message models (`models/Message.js`, `Reaction.js`, …).
//!
//! Only one web session per account may listen: a listener that was stopped never reconnects.

use std::{collections::HashMap, sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::{sync::mpsc, task::AbortHandle};
use tokio_tungstenite::tungstenite::{
    Message,
    client::IntoClientRequest,
    http::HeaderValue,
    protocol::{CloseFrame, frame::coding::CloseCode},
};

use super::{
    api::{Api, ThreadType},
    proto::{decode_event_data, encode_frame, frame_header, is_zero_id, jstr, make_url, now_ms},
};

/// A message, as zca-js `UserMessage` / `GroupMessage` leave it: "0" ids replaced by mine.
#[derive(Clone, Debug)]
pub struct Msg {
    pub kind: ThreadType,
    pub thread_id: String,
    pub is_self: bool,
    pub data: Value,
}

fn fix_quote_owner(data: &mut Value) {
    if let Some(q) = data.get_mut("quote").filter(|q| q.is_object())
        && let Some(owner) = q.get("ownerId").cloned()
    {
        q["ownerId"] = json!(jstr(Some(&owner)));
    }
}

pub fn user_message(uid: &str, mut data: Value) -> Msg {
    let is_self = is_zero_id(data.get("uidFrom"));
    let thread_id = jstr(if is_self { data.get("idTo") } else { data.get("uidFrom") });
    if is_zero_id(data.get("idTo")) {
        data["idTo"] = json!(uid);
    }
    if is_self {
        data["uidFrom"] = json!(uid);
    }
    fix_quote_owner(&mut data);
    Msg { kind: ThreadType::User, thread_id, is_self, data }
}

pub fn group_message(uid: &str, mut data: Value) -> Msg {
    let is_self = is_zero_id(data.get("uidFrom"));
    let thread_id = jstr(data.get("idTo"));
    if is_self {
        data["uidFrom"] = json!(uid);
    }
    fix_quote_owner(&mut data);
    Msg { kind: ThreadType::Group, thread_id, is_self, data }
}

/// `Undo`: same id rules as messages, the thread is the group or the other person.
#[derive(Clone, Debug)]
pub struct Action {
    pub thread_id: String,
    pub data: Value,
}

fn action(uid: &str, mut data: Value, is_group: bool) -> Action {
    let is_self = is_zero_id(data.get("uidFrom"));
    let thread_id = jstr(if is_group || is_self { data.get("idTo") } else { data.get("uidFrom") });
    if is_zero_id(data.get("idTo")) {
        data["idTo"] = json!(uid);
    }
    if is_self {
        data["uidFrom"] = json!(uid);
    }
    Action { thread_id, data }
}

#[derive(Debug)]
pub enum Event {
    /// the socket has its key: history can be requested now
    CipherKey,
    Message(Msg),
    OldMessages(Vec<Msg>, ThreadType),
    /// a message recalled ("thu hồi")
    Undo(Action),
    /// the socket closed for good (no retry): its close code
    Closed(u16),
    Error(String),
}

struct Retry {
    count: u32,
    max: u32,
    times: Vec<u64>,
}

enum Out {
    Frame(Vec<u8>),
    Close(u16),
}

#[derive(Default)]
struct State {
    ws_url: String,
    rotate_count: usize,
    retries: HashMap<String, Retry>,
    out: Option<mpsc::UnboundedSender<Out>>,
    req_id: u64,
    cipher_key: Option<String>,
    ping: Option<AbortHandle>,
    /// set by `stop`: never reconnect after that
    stopped: bool,
    /// the code we closed with, when we closed
    closing: Option<u16>,
}

pub struct Listener {
    api: Arc<Api>,
    urls: Vec<String>,
    events: mpsc::UnboundedSender<Event>,
    st: Mutex<State>,
}

impl Listener {
    pub fn new(api: Arc<Api>, events: mpsc::UnboundedSender<Event>) -> anyhow::Result<Arc<Self>> {
        let urls = api.ws_urls.clone();
        let first = urls.first().ok_or_else(|| anyhow::anyhow!("Zalo did not give a websocket address"))?;
        let mut st = State { ws_url: make_url(first, &[("t", now_ms().to_string())], true)?, ..Default::default() };
        if let Some(retries) = api.settings["features"]["socket"]["retries"].as_object() {
            for (code, r) in retries {
                let times = match &r["times"] {
                    Value::Number(n) => vec![n.as_u64().unwrap_or(0)],
                    Value::Array(a) => a.iter().filter_map(Value::as_u64).collect(),
                    _ => vec![],
                };
                st.retries.insert(code.clone(), Retry { count: 0, max: r["max"].as_u64().unwrap_or(0) as u32, times });
            }
        }
        Ok(Arc::new(Self { api, urls, events, st: Mutex::new(st) }))
    }

    fn socket_setting(&self, key: &str) -> &Value {
        &self.api.settings["features"]["socket"][key]
    }

    fn emit(&self, e: Event) {
        let _ = self.events.send(e);
    }

    /// Connect (`retry_on_close`: reconnect the way Zalo's settings say on the codes they list).
    pub fn start(self: &Arc<Self>, retry_on_close: bool) -> anyhow::Result<()> {
        let mut st = self.st.lock();
        if st.out.is_some() {
            anyhow::bail!("Already started");
        }
        if st.stopped {
            anyhow::bail!("Listener was stopped");
        }
        let (tx, rx) = mpsc::unbounded_channel();
        st.out = Some(tx);
        st.closing = None;
        let url = st.ws_url.clone();
        drop(st);
        let me = self.clone();
        tauri::async_runtime::spawn(async move { me.run(url, rx, retry_on_close).await });
        Ok(())
    }

    /// Close for good.
    pub fn stop(&self) {
        let mut st = self.st.lock();
        st.stopped = true;
        if let Some(out) = st.out.take() {
            st.closing = Some(1000);
            let _ = out.send(Out::Close(1000));
        }
        Self::reset(&mut st);
    }

    fn reset(st: &mut State) {
        st.out = None;
        st.cipher_key = None;
        if let Some(p) = st.ping.take() {
            p.abort();
        }
    }

    async fn run(self: Arc<Self>, url: String, rx: mpsc::UnboundedReceiver<Out>, retry_on_close: bool) {
        let code = self.pump(url, rx).await;
        let stopped = {
            let mut st = self.st.lock();
            Self::reset(&mut st);
            st.stopped
        };
        let retry = if retry_on_close && !stopped { self.can_retry(code) } else { None };
        match retry {
            // zca-js treats a 0 ms retry time as "don't retry"
            Some(ms) if ms > 0 => {
                if self.should_rotate(code) {
                    self.rotate();
                }
                tokio::time::sleep(Duration::from_millis(ms)).await;
                if self.st.lock().stopped || self.start(true).is_err() {
                    self.emit(Event::Closed(code));
                }
            }
            _ => self.emit(Event::Closed(code)),
        }
    }

    /// One connection, until it closes: its close code.
    async fn pump(self: &Arc<Self>, url: String, mut rx: mpsc::UnboundedReceiver<Out>) -> u16 {
        let mut req = match url.as_str().into_client_request() {
            Ok(r) => r,
            Err(e) => {
                self.emit(Event::Error(e.to_string()));
                return 1006;
            }
        };
        let cookie = self.api.http.jar.lock().cookie_string("https://chat.zalo.me");
        let h = req.headers_mut();
        for (k, v) in [
            ("accept-language", "en-US,en;q=0.9".to_owned()),
            ("cache-control", "no-cache".into()),
            ("pragma", "no-cache".into()),
            ("origin", "https://chat.zalo.me".into()),
            ("user-agent", self.api.http.user_agent.clone()),
            ("cookie", cookie),
        ] {
            if let Ok(v) = HeaderValue::from_str(&v) {
                h.insert(k, v);
            }
        }
        let ws = match tokio_tungstenite::connect_async(req).await {
            Ok((ws, _)) => ws,
            Err(e) => {
                self.emit(Event::Error(e.to_string()));
                return 1006;
            }
        };
        let (mut sink, mut stream) = ws.split();
        let writer = tauri::async_runtime::spawn(async move {
            while let Some(o) = rx.recv().await {
                let r = match o {
                    Out::Frame(b) => sink.send(Message::Binary(b.into())).await,
                    Out::Close(code) => sink.send(Message::Close(Some(CloseFrame { code: CloseCode::from(code), reason: "".into() }))).await,
                };
                if r.is_err() {
                    break;
                }
            }
        });
        let mut code = None;
        while let Some(item) = stream.next().await {
            match item {
                Ok(Message::Binary(b)) => self.on_frame(&b),
                Ok(Message::Close(f)) => code = Some(f.map(|f| u16::from(f.code)).unwrap_or(1005)),
                Ok(_) => {}
                Err(e) => {
                    self.emit(Event::Error(e.to_string()));
                    break;
                }
            }
        }
        writer.abort();
        code.or(self.st.lock().closing).unwrap_or(1006)
    }

    /// `canRetry`: the wait before reconnecting, if this code may retry.
    fn can_retry(&self, code: u16) -> Option<u64> {
        let listed = self.socket_setting("close_and_retry_codes").as_array().is_some_and(|a| a.iter().any(|c| c.as_u64() == Some(code as u64)));
        if !listed {
            return None;
        }
        let mut st = self.st.lock();
        let r = st.retries.get_mut(&code.to_string())?;
        if r.count >= r.max {
            return None;
        }
        r.count += 1;
        let i = r.count as usize - 1;
        r.times.get(i).or(r.times.last()).copied()
    }

    fn should_rotate(&self, code: u16) -> bool {
        let listed = self.socket_setting("rotate_error_codes").as_array().is_some_and(|a| a.iter().any(|c| c.as_u64() == Some(code as u64)));
        listed && self.st.lock().rotate_count < self.urls.len().saturating_sub(1)
    }

    fn rotate(&self) {
        let mut st = self.st.lock();
        st.rotate_count += 1;
        if let Some(u) = self.urls.get(st.rotate_count)
            && let Ok(url) = make_url(u, &[("t", now_ms().to_string())], true)
        {
            st.ws_url = url;
        }
    }

    /// `sendWs`
    fn send_ws(&self, version: u8, cmd: u16, sub_cmd: u8, mut data: Value, require_id: bool) {
        let mut st = self.st.lock();
        let Some(out) = st.out.clone() else { return };
        if require_id {
            data["req_id"] = json!(format!("req_{}", st.req_id));
            st.req_id += 1;
        }
        let _ = out.send(Out::Frame(encode_frame(version, cmd, sub_cmd, data.to_string().as_bytes())));
    }

    /// Ask Zalo to replay a page of recent messages (older than `last_id` when given).
    pub fn request_old_messages(&self, kind: ThreadType, last_id: Option<String>) {
        let cmd = if kind == ThreadType::User { 510 } else { 511 };
        self.send_ws(1, cmd, 1, json!({ "first": true, "lastId": last_id, "preIds": [] }), true);
    }

    fn decode(&self, parsed: &Value) -> Result<Value, String> {
        let key = self.st.lock().cipher_key.clone();
        decode_event_data(parsed, key.as_deref()).map(|v| v.get("data").cloned().unwrap_or(Value::Null)).map_err(|e| e.to_string())
    }

    fn on_frame(self: &Arc<Self>, frame: &[u8]) {
        let Some((version, cmd, sub)) = frame_header(frame) else {
            self.emit(Event::Error("Invalid header".into()));
            return;
        };
        let text = String::from_utf8_lossy(&frame[4..]);
        if text.is_empty() {
            return;
        }
        let parsed: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => return self.emit(Event::Error(e.to_string())),
        };
        if let Err(e) = self.dispatch(version, cmd, sub, &parsed) {
            self.emit(Event::Error(e));
        }
    }

    fn dispatch(self: &Arc<Self>, version: u8, cmd: u16, sub: u8, parsed: &Value) -> Result<(), String> {
        let uid = self.api.uid.clone();
        let list = |v: &Value, key: &str| v.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
        let is_undo = |m: &Value| m.get("content").is_some_and(|c| c.is_object() && c.get("deleteMsg").is_some());

        if version == 1 && cmd == 1 && sub == 1 && parsed.get("key").is_some() {
            let key = jstr(parsed.get("key"));
            let interval = self.socket_setting("ping_interval").as_u64().filter(|&n| n > 0).unwrap_or(60_000);
            let me = Arc::downgrade(self);
            let ping = tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(interval)).await;
                    let Some(me) = me.upgrade() else { break };
                    me.send_ws(1, 2, 1, json!({ "eventId": now_ms() }), false);
                }
            });
            let mut st = self.st.lock();
            st.cipher_key = Some(key);
            if let Some(old) = st.ping.replace(ping.inner().abort_handle()) {
                old.abort();
            }
            drop(st);
            self.emit(Event::CipherKey);
        }
        if version == 1 && (cmd == 501 || cmd == 551) && sub == 0 {
            for m in list(&self.decode(parsed)?, "msgs") {
                if is_undo(&m) {
                    self.emit(Event::Undo(action(&uid, m, false)));
                } else {
                    self.emit(Event::Message(user_message(&uid, m)));
                }
            }
        }
        if version == 1 && cmd == 521 && sub == 0 {
            for m in list(&self.decode(parsed)?, "groupMsgs") {
                if is_undo(&m) {
                    self.emit(Event::Undo(action(&uid, m, true)));
                } else {
                    self.emit(Event::Message(group_message(&uid, m)));
                }
            }
        }
        if cmd == 510 && sub == 1 {
            let msgs = list(&self.decode(parsed)?, "msgs").into_iter().map(|m| user_message(&uid, m)).collect();
            self.emit(Event::OldMessages(msgs, ThreadType::User));
        }
        if cmd == 511 && sub == 1 {
            let msgs = list(&self.decode(parsed)?, "groupMsgs").into_iter().map(|m| group_message(&uid, m)).collect();
            self.emit(Event::OldMessages(msgs, ThreadType::Group));
        }
        if version == 1 && cmd == 3000 && sub == 0 {
            // another Zalo Web session took over
            let mut st = self.st.lock();
            st.closing = Some(3000);
            if let Some(out) = &st.out {
                let _ = out.send(Out::Close(3000));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_models_like_zca() {
        let me = "999";
        let mine = user_message(me, json!({ "msgId": "1", "uidFrom": "0", "idTo": "123", "quote": { "ownerId": 55 } }));
        assert!(mine.is_self);
        assert_eq!(mine.thread_id, "123");
        assert_eq!(mine.data["uidFrom"], "999");
        assert_eq!(mine.data["quote"]["ownerId"], "55");
        let theirs = user_message(me, json!({ "msgId": "2", "uidFrom": "123", "idTo": "0" }));
        assert!(!theirs.is_self);
        assert_eq!(theirs.thread_id, "123");
        assert_eq!(theirs.data["idTo"], "999");
        let g = group_message(me, json!({ "msgId": "3", "uidFrom": "0", "idTo": "g1" }));
        assert!(g.is_self && g.thread_id == "g1" && g.data["uidFrom"] == "999");
        let r = action(me, json!({ "uidFrom": "123", "idTo": "0" }), false);
        assert_eq!(r.thread_id, "123");
        let r = action(me, json!({ "uidFrom": "123", "idTo": "g7" }), true);
        assert_eq!(r.thread_id, "g7");
    }
}
