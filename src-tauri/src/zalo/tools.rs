//! Claude's tools for the user's Zalo account (MCP server "vibe-zalo").

use serde_json::{Value, json};

use super::{ZaloMessage, fmt_time, get_messages, list_threads, load_older, need, search_messages, state, store};

pub const INSTRUCTIONS: &str = "Read messages of the user's personal Zalo account connected in VIBE CODE's Zalo tab. Start with zalo_list_chats to find a thread id, then zalo_read_messages. Message history is what VIBE CODE has synced since login plus recent history Zalo replays; groups can load older messages with zalo_read_messages {older: true}.";

pub fn tools() -> Value {
    json!([
        {
            "name": "zalo_status",
            "description": "Whether Zalo is logged in, and as whom.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "zalo_list_chats",
            "description": "Recent Zalo conversations (people and groups), newest first: thread id, name, type, time and last message. `query` filters by name.",
            "inputSchema": { "type": "object", "properties": { "query": { "type": "string" }, "limit": { "type": "number", "description": "default 30" } } }
        },
        {
            "name": "zalo_read_messages",
            "description": "Messages of one conversation, oldest first. `before` (ms timestamp) pages back through the synced history. For groups, `older: true` first fetches older messages from Zalo.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "threadId": { "type": "string" },
                    "limit": { "type": "number", "description": "default 50, max 500" },
                    "before": { "type": "number" },
                    "older": { "type": "boolean" }
                },
                "required": ["threadId"]
            }
        },
        {
            "name": "zalo_search_messages",
            "description": "Search synced Zalo messages by text or sender name (all conversations, or one `threadId`). Newest first.",
            "inputSchema": { "type": "object", "properties": { "query": { "type": "string" }, "threadId": { "type": "string" }, "limit": { "type": "number" } }, "required": ["query"] }
        },
        {
            "name": "zalo_contacts",
            "description": "Zalo friends and groups with their ids. `query` filters by name.",
            "inputSchema": { "type": "object", "properties": { "query": { "type": "string" } } }
        },
        {
            "name": "zalo_find_user",
            "description": "Look up a Zalo user by phone number (returns their id and name).",
            "inputSchema": { "type": "object", "properties": { "phone": { "type": "string" } }, "required": ["phone"] }
        }
    ])
}

fn fmt(ts: i64) -> String {
    if ts == 0 { "-".into() } else { fmt_time(ts) }
}

fn fmt_msg(m: &ZaloMessage) -> String {
    let who = if m.mine { "Tôi" } else { &m.from_name };
    let from = if m.group { format!(" ({})", m.from_id) } else { String::new() };
    let quote = m.quote.as_ref().map(|q| format!("↪ «{}» ", q.chars().take(80).collect::<String>())).unwrap_or_default();
    format!("[{}] {who}{from}: {quote}{}", fmt(m.ts), m.text)
}

/// JS `String(v ?? '')`.
fn arg_str(args: &Value, k: &str) -> String {
    match args.get(k) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    }
}

/// `Math.min(max, Math.max(1, Number(v) || d))`
fn n(v: Option<&Value>, d: f64, max: f64) -> usize {
    let x = super::proto::jnum(v);
    let x = if x.is_nan() || x == 0.0 { d } else { x };
    x.max(1.0).min(max) as usize
}

pub async fn call_tool(name: &str, args: Value) -> anyhow::Result<String> {
    match name {
        "zalo_status" => {
            let st = state();
            Ok(if st.status == "online" {
                let me = st.me.unwrap_or_default();
                format!("Logged in as {} (id {}). {} conversations cached.", me.name, me.id, store().threads.len())
            } else {
                let err = st.error.map(|e| format!(": {e}")).unwrap_or_default();
                format!("Not logged in ({}{err}). Ask the user to open the Zalo tab in VIBE CODE and scan the QR code.", st.status)
            })
        }
        "zalo_list_chats" => {
            let list = list_threads(&arg_str(&args, "query"), n(args.get("limit"), 30.0, 500.0));
            if list.is_empty() {
                return Ok("No conversations synced yet.".into());
            }
            Ok(list
                .iter()
                .map(|t| format!("{} {}  {}  · {}  · {}", if t.group { "group" } else { "user " }, t.id, if t.name.is_empty() { "(no name)" } else { &t.name }, fmt(t.last_ts), t.last))
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "zalo_read_messages" => {
            let id = arg_str(&args, "threadId");
            let mut note = String::new();
            if super::proto::truthy(args.get("older")) {
                note = format!("Fetched {} older messages.\n", load_older(&id, 3).await?);
            }
            let before = args.get("before").and_then(Value::as_f64).map(|b| b as i64);
            let list = get_messages(&id, n(args.get("limit"), 50.0, 500.0), before);
            let head = {
                let s = store();
                let t = s.threads.get(&id);
                let name = t.map(|t| t.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| id.clone());
                format!("{} {name}\n", if t.is_some_and(|t| t.group) { "Group" } else { "Chat with" })
            };
            if list.is_empty() {
                return Ok(format!("{head}{note}No messages synced for this conversation."));
            }
            let body = list.iter().map(fmt_msg).collect::<Vec<_>>().join("\n");
            Ok(format!("{head}{note}{body}\n(oldest ts here: {} — pass as `before` for earlier messages)", list[0].ts))
        }
        "zalo_search_messages" => {
            let thread = Some(arg_str(&args, "threadId")).filter(|t| !t.is_empty());
            let list = search_messages(&arg_str(&args, "query"), thread.as_deref(), n(args.get("limit"), 50.0, 500.0));
            if list.is_empty() {
                return Ok("No matches.".into());
            }
            let s = store();
            Ok(list
                .iter()
                .map(|m| {
                    let name = s.threads.get(&m.thread_id).map(|t| t.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| m.thread_id.clone());
                    format!("{name} ({}) {}", m.thread_id, fmt_msg(m))
                })
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "zalo_contacts" => {
            let q = arg_str(&args, "query").to_lowercase();
            let s = store();
            let rows: Vec<String> = s
                .names
                .iter()
                .filter(|(_, v)| q.is_empty() || v.to_lowercase().contains(&q))
                .take(500)
                .map(|(id, v)| format!("{} {id}  {v}", if s.threads.get(id).is_some_and(|t| t.group) { "group" } else { "user " }))
                .collect();
            Ok(if rows.is_empty() { "No contacts match.".into() } else { rows.join("\n") })
        }
        "zalo_find_user" => {
            let u = need()?.find_user(&arg_str(&args, "phone")).await?;
            let uid = u.get("uid").filter(|v| super::proto::truthy(Some(v)));
            Ok(match uid {
                Some(uid) => {
                    let name = u.get("display_name").and_then(Value::as_str).filter(|n| !n.is_empty()).or_else(|| u.get("zalo_name").and_then(Value::as_str)).unwrap_or_default();
                    format!("id {}  {name}", super::proto::jstr(Some(uid)))
                }
                None => "Not found.".into(),
            })
        }
        _ => anyhow::bail!("Unknown tool {name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn send_tool_is_neither_listed_nor_callable() {
        let removed = "zalo_send_message";
        assert!(!tools().as_array().unwrap().iter().any(|tool| tool["name"] == removed));
        let error = call_tool(removed, json!({})).await.unwrap_err();
        assert_eq!(error.to_string(), format!("Unknown tool {removed}"));
    }
}
