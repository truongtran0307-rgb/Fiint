//! What the Zalo cache holds (cache.json, msgs/*.json) and what Claude's tools and the task list read.
//! Fields older versions wrote and no longer kept are ignored when a cache is read.

use serde::{Deserialize, Serialize, Serializer};

/// A JS number that is usually whole: written without ".0" when it is.
pub fn ser_num<S: Serializer>(v: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => s.serialize_i64(*f as i64),
        Some(f) => s.serialize_f64(*f),
        None => s.serialize_none(),
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZaloState {
    /// off | qr | connecting | online | error
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scanned: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(rename = "self", skip_serializing_if = "Option::is_none")]
    pub me: Option<ZaloSelf>,
}

impl Default for ZaloState {
    fn default() -> Self {
        Self { status: "off", qr: None, scanned: None, error: None, me: None }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ZaloSelf {
    pub id: String,
    pub name: String,
    pub avatar: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ZaloThread {
    pub id: String,
    pub group: bool,
    pub name: String,
    pub last_ts: i64,
    pub last: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ZaloAttachment {
    /// photo | video | voice | file | sticker | gif | link | location | card | other
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumb: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_num")]
    pub width: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_num")]
    pub height: Option<f64>,
    /// bytes
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_num")]
    pub size: Option<f64>,
    /// seconds
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_num")]
    pub duration: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Reply {
    pub id: String,
    pub from: String,
    pub text: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MentionRef {
    pub uid: String,
    pub pos: i64,
    pub len: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ZaloMessage {
    pub id: String,
    pub thread_id: String,
    pub group: bool,
    pub from_id: String,
    pub from_name: String,
    #[serde(rename = "self")]
    pub mine: bool,
    pub ts: i64,
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply: Option<Reply>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub att: Option<ZaloAttachment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mentions: Option<Vec<MentionRef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recalled: Option<bool>,
}

/// A message in [`super::work_feed`].
#[derive(Clone, Debug)]
pub struct WorkMsg {
    pub id: String,
    pub ts: i64,
    pub from: String,
    pub from_id: String,
    pub mine: bool,
    /// sent to me: a personal chat, an @mention of me, or a reply to my message
    pub to_me: bool,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct WorkThread {
    pub thread_id: String,
    pub name: String,
    pub group: bool,
    pub msgs: Vec<WorkMsg>,
}
