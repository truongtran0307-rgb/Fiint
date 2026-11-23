//! "Việc của tôi": Claude reads what was sent to me on Zalo (personal chats, @mentions, replies to
//! me) and keeps a checklist — not answered = todo, answered but not finished = doing, reported
//! done = done. Port of src/main/zaloTasks.ts.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::LazyLock,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use indexmap::IndexMap;
use parking_lot::{Mutex, MutexGuard};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{io::AsyncWriteExt, task::AbortHandle};

use crate::{
    core::{self, Res, emit, main_window},
    toast::{self, ToastCard, ToastTag},
    zalo::{self, WorkThread, fmt_time},
};

const WINDOW_DAYS: i64 = 7;
const KEEP_DONE_DAYS: i64 = 14;
const DAY: i64 = 86_400_000;
/// after a message to me: if nothing more arrives for this long, send the batch to Claude
const REALTIME_DELAY: Duration = Duration::from_secs(30);
const MODEL: &str = "haiku";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ZaloTask {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub thread_id: String,
    pub thread_name: String,
    pub group: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    pub ts: i64,
    /// todo | doing | done
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual: Option<bool>,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZaloIncoming {
    pub thread_id: String,
    pub msg_id: String,
    pub thread_name: String,
    pub group: bool,
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    pub text: String,
    pub ts: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZaloTasksState {
    pub tasks: Vec<ZaloTask>,
    pub running: bool,
    pub analyzed_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub incoming: Vec<ZaloIncoming>,
    pub fresh: Vec<String>,
    pub changed: Vec<String>,
    pub unseen: IndexMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_at: Option<i64>,
}

#[derive(Default)]
struct Tasks {
    loaded: bool,
    tasks: Vec<ZaloTask>,
    analyzed_at: i64,
    running: bool,
    error: Option<String>,
    again: bool,
    auto: Option<AbortHandle>,
    /// when the waiting batch goes to Claude
    due_at: i64,
    /// messages to me waiting to become tasks (shown as progress)
    incoming: Vec<ZaloIncoming>,
    /// tasks created by the last analysis
    fresh: Vec<String>,
    /// tasks whose status the last analysis changed
    changed: Vec<String>,
    /// tasks created / changed by Claude that I haven't looked at yet: "new" | "updated"
    unseen: IndexMap<String, String>,
    /// the Zalo tab is on screen in a focused window: changes count as seen right away, no pop-up
    viewing: bool,
    /// source messages of tasks I deleted: the analysis never turns them into tasks again
    dismissed: Vec<String>,
}

static T: LazyLock<Mutex<Tasks>> = LazyLock::new(Default::default);

fn dir() -> PathBuf {
    core::user_data().join("zalo")
}
fn file() -> PathBuf {
    dir().join("tasks.json")
}

fn tasks() -> MutexGuard<'static, Tasks> {
    let mut t = T.lock();
    if !t.loaded {
        t.loaded = true;
        if let Ok(d) = std::fs::read_to_string(file()).map_err(anyhow::Error::from).and_then(|s| Ok(serde_json::from_str::<Value>(&s)?)) {
            t.tasks = d["tasks"].as_array().into_iter().flatten().filter_map(|x| serde_json::from_value(x.clone()).ok()).collect();
            t.analyzed_at = d["analyzedAt"].as_i64().unwrap_or(0);
            t.unseen = d["unseen"].as_object().into_iter().flatten().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned()))).collect();
            t.dismissed = d["dismissed"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_owned)).collect();
        }
    }
    t
}

async fn save() {
    let data = {
        let t = tasks();
        serde_json::json!({ "tasks": t.tasks, "analyzedAt": t.analyzed_at, "unseen": t.unseen, "dismissed": t.dismissed }).to_string()
    };
    let _ = core::write_atomic(file(), data).await;
}

fn uid_of(t: &ZaloTask) -> Option<String> {
    t.from_id.clone().or_else(|| (!t.group).then(|| t.thread_id.clone()))
}

pub fn get_tasks() -> ZaloTasksState {
    let (list, s) = {
        let t = tasks();
        let s = ZaloTasksState {
            tasks: vec![],
            running: t.running,
            analyzed_at: t.analyzed_at,
            error: t.error.clone(),
            incoming: t.incoming.clone(),
            fresh: t.fresh.clone(),
            changed: t.changed.clone(),
            unseen: t.unseen.clone(),
            due_at: t.auto.is_some().then_some(t.due_at),
        };
        (t.tasks.clone(), s)
    };
    // avatars outside the lock (zalo has its own)
    let tasks = list.into_iter().map(|mut x| {
        x.avatar = uid_of(&x).and_then(|u| zalo::avatar_of(&u));
        x
    });
    ZaloTasksState { tasks: tasks.collect(), ..s }
}

fn broadcast() {
    emit("zalo:tasks", (get_tasks(),));
}

pub fn init() {
    drop(tasks());
}

/// Realtime: a new message to me (or my reply in a chat with open work) → analyze within seconds.
pub fn on_ingest(ids: Vec<String>) {
    let now = core::now_ms() as i64;
    // only the conversations that just got messages (not a scan of every conversation per message)
    let feed: Vec<WorkThread> = zalo::work_feed(now - WINDOW_DAYS * DAY, 40, 40, Some(&ids));
    let mut relevant = false;
    let mut add = Vec::new();
    {
        let t = tasks();
        let since = t.analyzed_at.max(now - 10 * 60_000);
        for th in &feed {
            for m in &th.msgs {
                if m.ts <= since {
                    continue;
                }
                if m.to_me {
                    relevant = true;
                    if !t.incoming.iter().any(|x| x.msg_id == m.id) && !add.iter().any(|(x, _): &(ZaloIncoming, String)| x.msg_id == m.id) {
                        let inc = ZaloIncoming {
                            thread_id: th.thread_id.clone(),
                            msg_id: m.id.clone(),
                            thread_name: th.name.clone(),
                            group: th.group,
                            from: m.from.clone(),
                            avatar: None,
                            text: m.text.chars().take(160).collect(),
                            ts: m.ts,
                        };
                        add.push((inc, m.from_id.clone()));
                    }
                } else if m.mine && t.tasks.iter().any(|x| x.thread_id == th.thread_id && x.status != "done") {
                    relevant = true;
                }
            }
        }
    }
    let add: Vec<ZaloIncoming> = add.into_iter().map(|(mut i, uid)| {
        i.avatar = zalo::avatar_of(&uid);
        i
    }).collect();
    if !relevant {
        return;
    }
    {
        let mut t = tasks();
        t.incoming.extend(add);
        // each new message restarts the wait: the batch goes once the chat is quiet for REALTIME_DELAY
        if let Some(h) = t.auto.take() {
            h.abort();
        }
        t.due_at = now + REALTIME_DELAY.as_millis() as i64;
        let h = tauri::async_runtime::spawn(async {
            tokio::time::sleep(REALTIME_DELAY).await;
            tasks().auto = None;
            analyze().await;
        });
        t.auto = Some(h.inner().abort_handle());
    }
    broadcast();
}

fn status_tag(status: &str) -> ToastTag {
    let (text, color) = match status {
        "doing" => ("Đang xử lý", "#d9922e"),
        "done" => ("Hoàn thành", "#2f9e63"),
        _ => ("Cần làm", "#e5484d"),
    };
    ToastTag { text: text.into(), color: color.into() }
}

/// Badge + Zalo-style pop-ups for tasks Claude just created or changed.
fn announce(created: &[ZaloTask], updated: &[ZaloTask]) {
    let w = main_window();
    let focused = w.as_ref().is_some_and(|w| w.is_focused().unwrap_or(false));
    let visible = w.as_ref().is_some_and(|w| w.is_visible().unwrap_or(false));
    {
        let mut t = tasks();
        if t.viewing && visible && focused {
            return;
        }
        for x in created {
            t.unseen.insert(x.id.clone(), "new".into());
        }
        for x in updated {
            if t.unseen.get(&x.id).map(String::as_str) != Some("new") {
                t.unseen.insert(x.id.clone(), "updated".into());
            }
        }
    }
    let card = |x: &ZaloTask, new: bool| ToastCard {
        id: x.id.clone(),
        name: x.from.clone(),
        avatar: uid_of(x).and_then(|u| zalo::avatar_of(&u)),
        title: x.from.clone(),
        tag: Some(if new { ToastTag { text: "Việc mới".into(), color: "#3b82f6".into() } } else { status_tag(&x.status) }),
        body: x.title.clone(),
        meta: x.group.then(|| x.thread_name.clone()),
    };
    let cards: Vec<ToastCard> = created.iter().map(|x| card(x, true)).chain(updated.iter().map(|x| card(x, false))).collect();
    if cards.is_empty() {
        return;
    }
    toast::show(cards);
    if let Some(w) = w
        && !focused
    {
        let _ = w.request_user_attention(Some(tauri::UserAttentionType::Informational));
    }
}

/// The Zalo tab is (not) on screen; while it is, everything counts as seen.
pub fn set_viewing(on: bool) {
    tasks().viewing = on;
    if on {
        mark_seen();
    }
}

pub fn mark_seen() {
    {
        let mut t = tasks();
        if t.unseen.is_empty() {
            return;
        }
        t.unseen.clear();
    }
    tauri::async_runtime::spawn(save());
    broadcast();
}

pub fn set_status(id: &str, status: &str) -> ZaloTasksState {
    {
        let mut t = tasks();
        if let Some(x) = t.tasks.iter_mut().find(|x| x.id == id) {
            x.status = status.to_owned();
            x.manual = Some(true);
            x.updated_at = core::now_ms() as i64;
        }
        t.unseen.shift_remove(id);
    }
    tauri::async_runtime::spawn(save());
    broadcast();
    get_tasks()
}

pub fn remove_task(id: &str) -> ZaloTasksState {
    {
        let mut t = tasks();
        if let Some(mid) = t.tasks.iter().find(|x| x.id == id).and_then(|x| x.msg_id.clone()) {
            t.dismissed.push(mid);
            // bounded: message ids older than the analysis window can't come back anyway
            let n = t.dismissed.len();
            if n > 500 {
                t.dismissed.drain(..n - 500);
            }
        }
        t.tasks.retain(|x| x.id != id);
        t.unseen.shift_remove(id);
    }
    tauri::async_runtime::spawn(save());
    broadcast();
    get_tasks()
}

/* ------------------------- context for a new chat ------------------------- */

const MAX_DOWNLOAD: u64 = 25 * 1024 * 1024;

fn safe_name(s: &str) -> String {
    static BAD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"[<>:"/\\|?*\x00-\x1f]+"#).unwrap());
    static WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
    let s = BAD.replace_all(s, "_");
    let s: String = WS.replace_all(&s, " ").trim().chars().take(80).collect();
    if s.is_empty() { "file".into() } else { s }
}

fn ext_of(url: &str, fallback: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.(png|jpe?g|gif|webp|bmp)(?:$|[?#])").unwrap());
    RE.captures(url).map(|c| c[1].to_lowercase()).unwrap_or_else(|| fallback.into())
}

async fn download(url: &str, path: &Path) -> bool {
    async fn go(url: &str, path: &Path) -> Result<()> {
        let mut r = reqwest::get(url).await?.error_for_status()?;
        if r.content_length().is_some_and(|n| n > MAX_DOWNLOAD) {
            bail!("too big");
        }
        let mut buf = Vec::new();
        while let Some(chunk) = r.chunk().await? {
            buf.extend_from_slice(&chunk);
            if buf.len() as u64 > MAX_DOWNLOAD {
                bail!("too big");
            }
        }
        tokio::fs::write(path, buf).await?;
        Ok(())
    }
    go(url, path).await.is_ok()
}

fn collapse_ws(s: &str) -> String {
    static WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
    WS.replace_all(s, " ").into_owned()
}

#[derive(Serialize)]
pub struct TaskContext {
    file: String,
    images: Vec<String>,
    files: Vec<String>,
}

/// Everything a coding chat needs about a task: the conversation around it written to a Markdown
/// file, with photos and files sent in that chat downloaded next to it.
pub async fn task_context(id: &str) -> Result<TaskContext> {
    let t = tasks().tasks.iter().find(|x| x.id == id).cloned().ok_or_else(|| anyhow!("Không tìm thấy việc này"))?;
    let all: Vec<zalo::ZaloMessage> = zalo::get_messages(&t.thread_id, 400, None).into_iter().filter(|m| m.recalled != Some(true)).collect();
    let mut start = t.msg_id.as_ref().and_then(|mid| all.iter().position(|m| &m.id == mid));
    if start.is_none() {
        start = all.iter().position(|m| m.ts >= t.ts);
    }
    let start = start.unwrap_or(all.len().saturating_sub(30));
    // a little of what came before (the question often builds on it), then everything since, capped
    let msgs: Vec<&zalo::ZaloMessage> = all[start.saturating_sub(15)..].iter().take(120).collect();
    let out = dir().join("context").join(&t.id);
    let _ = tokio::fs::remove_dir_all(&out).await;
    tokio::fs::create_dir_all(&out).await?;
    let (mut images, mut files) = (Vec::new(), Vec::new());
    let mut n = 0;
    let mut lines = Vec::new();
    for m in msgs {
        let who = if m.mine { "Tôi" } else { &m.from_name };
        let reply = m.reply.as_ref().map(|r| format!(" _(trả lời {}: \"{}\")_", r.from, collapse_ws(&r.text).chars().take(120).collect::<String>())).unwrap_or_default();
        let mut body = m.text.replace("\r\n", "\n").replace('\n', "\n  ");
        if let Some(a) = &m.att {
            if (a.kind == "photo" || a.kind == "gif") && (a.url.is_some() || a.thumb.is_some()) {
                let url = a.url.clone().or(a.thumb.clone()).unwrap_or_default();
                n += 1;
                let path = out.join(format!("img-{n:02}.{}", ext_of(&url, "jpg")));
                if download(&url, &path).await {
                    let p = path.display().to_string();
                    body = format!("[ảnh] {p}") + &a.desc.as_ref().map(|d| format!(" — {d}")).unwrap_or_default();
                    images.push(p);
                }
            } else if a.kind == "file"
                && let Some(url) = &a.url
                && a.size.unwrap_or(0.0) <= MAX_DOWNLOAD as f64
            {
                let name = match &a.title {
                    Some(t) => safe_name(t),
                    None => {
                        n += 1;
                        safe_name(&format!("file-{n}{}", a.ext.as_ref().map(|e| format!(".{e}")).unwrap_or_default()))
                    }
                };
                let path = out.join(name);
                if download(url, &path).await {
                    let p = path.display().to_string();
                    body = format!("[tệp] {p}");
                    files.push(p);
                }
            }
        }
        let req = if t.msg_id.as_deref() == Some(m.id.as_str()) { " ⟵ **yêu cầu**" } else { "" };
        lines.push(format!("- **{who}** ({}){reply}{req}: {body}", fmt_time(m.ts)));
    }
    let list = |title: &str, paths: &[String]| {
        if paths.is_empty() { String::new() } else { format!("- {title}:\n{}\n", paths.iter().map(|p| format!("  - {p}")).collect::<Vec<_>>().join("\n")) }
    };
    let md = format!(
        "# Việc từ Zalo: {title}\n\n**Hướng dẫn cho bạn (Claude):** Đây là một việc tôi nhận qua Zalo. Hãy đọc kỹ đoạn chat bên dưới{img}{fil}, rồi tìm hiểu code trong project hiện tại liên quan tới yêu cầu. Sau đó tóm tắt lại yêu cầu bằng tiếng Việt, nêu nguyên nhân / vị trí code liên quan và đề xuất cách xử lý. CHƯA sửa code cho tới khi tôi xác nhận.\n\n- Người yêu cầu: {from}{grp}\n- Thời gian: {time}\n- Tóm tắt: {title}\n{detail}{li}{lf}\n## Đoạn chat {head}\n\n{body}\n",
        title = t.title,
        img = if images.is_empty() { "" } else { " và XEM TẤT CẢ ảnh đính kèm (dùng Read với đường dẫn ảnh)" },
        fil = if files.is_empty() { "" } else { ", mở các tệp đính kèm" },
        from = t.from,
        grp = if t.group { format!(" (nhóm \"{}\")", t.thread_name) } else { String::new() },
        time = fmt_time(t.ts),
        detail = t.detail.as_ref().map(|d| format!("- Ngữ cảnh: {d}\n")).unwrap_or_default(),
        li = list("Ảnh đính kèm", &images),
        lf = list("Tệp đính kèm", &files),
        head = if t.group { format!("nhóm \"{}\"", t.thread_name) } else { format!("với {}", t.thread_name) },
        body = lines.join("\n"),
    );
    let file = out.join("context.md");
    tokio::fs::write(&file, md).await?;
    Ok(TaskContext { file: file.display().to_string(), images, files })
}

/* ------------------------------ Claude ------------------------------ */

async fn run_claude(prompt: &str) -> Result<String> {
    let mut path = crate::pty::claude_path().await.ok_or_else(|| anyhow!("Không tìm thấy Claude Code trên máy"))?;
    // npm's extensionless sh shim: Windows needs the .cmd next to it
    if cfg!(windows) && path.extension().is_none() {
        let cmd = PathBuf::from(format!("{}.cmd", path.display()));
        if cmd.exists() {
            path = cmd;
        }
    }
    let _ = tokio::fs::create_dir_all(dir()).await;
    // no saved session (keeps the chat history clean), no MCP servers — it only needs to read the prompt
    let mut child = core::command(&path)
        .args(["-p", "--output-format", "json", "--model", MODEL, "--no-session-persistence", "--strict-mcp-config"])
        .current_dir(dir())
        .env_clear()
        .envs(core::clean_env())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("không chạy được claude")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(prompt.as_bytes()).await?;
        drop(stdin);
    }
    let out = tokio::time::timeout(Duration::from_secs(300), child.wait_with_output()).await.map_err(|_| anyhow!("Claude phản hồi quá lâu"))??;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    match serde_json::from_str::<Value>(&stdout) {
        Ok(j) => {
            if zalo_truthy(j.get("is_error")) {
                let msg = j.get("result").filter(|r| zalo_truthy(Some(r))).map(value_text).unwrap_or_else(|| "Claude báo lỗi".into());
                bail!("{msg}");
            }
            Ok(j.get("result").map(value_text).unwrap_or_default())
        }
        Err(_) => {
            let code = out.status.code().map(|c| c.to_string()).unwrap_or_else(|| "null".into());
            let text = if !stderr.is_empty() { stderr.into_owned() } else if !stdout.is_empty() { stdout.into_owned() } else { format!("claude thoát với mã {code}") };
            bail!("{}", text.trim().chars().take(300).collect::<String>())
        }
    }
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        v => v.to_string(),
    }
}

fn zalo_truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Existing<'a> {
    id: &'a str,
    thread_id: &'a str,
    title: &'a str,
    status: &'a str,
}

fn build_prompt(feed: &[WorkThread], open: &[ZaloTask]) -> String {
    let convo = feed
        .iter()
        .map(|t| {
            let lines: Vec<String> = t.msgs.iter().map(|m| format!("  [{}] {} {}{}: {}", m.id, fmt_time(m.ts), m.from, if m.to_me { " →TÔI" } else { "" }, collapse_ws(&m.text))).collect();
            format!("### threadId={} · {} \"{}\"\n{}", t.thread_id, if t.group { "Nhóm" } else { "Cá nhân" }, t.name, lines.join("\n"))
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let existing: Vec<Existing> = open.iter().map(|t| Existing { id: &t.id, thread_id: &t.thread_id, title: &t.title, status: &t.status }).collect();
    format!(
        r#"Bạn là trợ lý quản lý công việc. Dưới đây là tin nhắn Zalo gửi tới TÔI (chat cá nhân, nhóm @nhắc tên tôi hoặc trả lời tin của tôi) trong {WINDOW_DAYS} ngày qua, kèm các tin "Tôi" đã gửi sau đó. Dòng có "→TÔI" là tin gửi trực tiếp cho tôi.

Nhiệm vụ: lập danh sách CÔNG VIỆC của tôi — những yêu cầu, câu hỏi cần tôi trả lời, việc được giao/nhờ tôi làm. Bỏ qua chào hỏi, tán gẫu, quảng cáo, tin thông báo tự động, tin không cần tôi làm gì.

Trạng thái mỗi việc:
- "todo": tôi chưa trả lời / chưa phản hồi gì.
- "doing": tôi đã phản hồi nhưng chưa xong (vd "ok", "để em xem", "đang làm", hẹn sẽ gửi).
- "done": tôi đã báo xong / đã gửi / đã trả lời đầy đủ câu hỏi, hoặc người yêu cầu nói đã ổn / huỷ.

Việc đã có (giữ nguyên "id" nếu vẫn là việc đó, cập nhật status/title nếu cần; KHÔNG tạo trùng; việc "done" là đã xong hẳn — KHÔNG mở lại, KHÔNG tạo lại việc đó dưới id/tiêu đề khác):
{existing}

Tin nhắn:
{convo}

Chỉ trả về MỘT object JSON, không giải thích, dạng:
{{"tasks":[{{"id":"<id việc đã có hoặc null>","threadId":"...","msgId":"<id tin yêu cầu>","from":"<người giao>","title":"<việc cần làm, ngắn gọn, tiếng Việt>","detail":"<1 câu tóm tắt ngữ cảnh / tôi đã trả lời gì>","status":"todo|doing|done"}}]}}
Trả về đầy đủ mọi việc (cả việc đã có còn liên quan)."#,
        existing = serde_json::to_string(&existing).unwrap_or_else(|_| "[]".into()),
    )
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawTask {
    id: Option<String>,
    thread_id: Option<String>,
    msg_id: Option<String>,
    from: Option<String>,
    title: Option<String>,
    detail: Option<String>,
    status: Option<String>,
}

fn parse(text: &str) -> Result<Vec<RawTask>> {
    let (Some(a), Some(b)) = (text.find('{'), text.rfind('}')) else { bail!("Claude không trả về JSON") };
    if b <= a {
        bail!("Claude không trả về JSON");
    }
    let j: Value = serde_json::from_str(&text[a..=b])?;
    Ok(j["tasks"].as_array().into_iter().flatten().filter_map(|t| serde_json::from_value(t.clone()).ok()).collect())
}

fn trimmed(s: &Option<String>) -> Option<String> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

/// Read the last few days of messages to me and update the checklist.
pub async fn analyze() -> ZaloTasksState {
    loop {
        {
            let mut t = tasks();
            if t.running {
                t.again = true;
                drop(t);
                return get_tasks();
            }
        }
        if zalo::state().status != "online" && zalo::self_id().is_none() {
            tasks().error = Some("Zalo chưa đăng nhập".into());
            broadcast();
            return get_tasks();
        }
        // messages that arrive during this run stay "incoming" for the next one
        let handling: Vec<String> = {
            let mut t = tasks();
            t.running = true;
            t.error = None;
            t.incoming.iter().map(|m| m.msg_id.clone()).collect()
        };
        broadcast();
        let r = run_once().await;
        {
            let mut t = tasks();
            if let Err(e) = r {
                t.error = Some(format!("{e:#}"));
            }
            t.running = false;
            t.incoming.retain(|m| !handling.contains(&m.msg_id));
        }
        broadcast();
        let again = std::mem::take(&mut tasks().again);
        if !again {
            return get_tasks();
        }
    }
}

async fn run_once() -> Result<()> {
    let now = core::now_ms() as i64;
    let feed = zalo::work_feed(now - WINDOW_DAYS * DAY, 40, 40, None);
    let mut created: Vec<ZaloTask> = Vec::new();
    let mut updated_ids: Vec<String> = Vec::new();
    // what the previous run already read: tasks "found" in those messages now are almost always
    // duplicates of work that was done or deleted — never re-announce them
    let prev = tasks().analyzed_at;
    if !feed.is_empty() {
        let open: Vec<ZaloTask> = tasks().tasks.iter().filter(|t| feed.iter().any(|f| f.thread_id == t.thread_id)).cloned().collect();
        let raw = parse(&run_claude(&build_prompt(&feed, &open)).await?)?;
        let mut t = tasks();
        for r in raw {
            let Some(th) = r.thread_id.as_ref().and_then(|id| feed.iter().find(|f| &f.thread_id == id)) else { continue };
            let Some(title) = trimmed(&r.title) else { continue };
            let status = match r.status.as_deref() {
                Some(s @ ("done" | "doing")) => s.to_owned(),
                _ => "todo".to_owned(),
            };
            let src = th.msgs.iter().find(|m| Some(&m.id) == r.msg_id.as_ref());
            let cur = r
                .id
                .as_ref()
                .and_then(|id| t.tasks.iter().position(|x| &x.id == id))
                .or_else(|| r.msg_id.as_ref().and_then(|mid| t.tasks.iter().position(|x| x.thread_id == th.thread_id && x.msg_id.as_ref() == Some(mid))));
            if let Some(i) = cur {
                let x = &mut t.tasks[i];
                x.title = title;
                x.detail = trimmed(&r.detail).or(x.detail.take());
                x.thread_name = th.name.clone();
                if x.from_id.is_none()
                    && let Some(src) = src.filter(|s| !s.mine)
                {
                    x.from_id = Some(src.from_id.clone());
                }
                // a status set by hand stays, unless Claude now sees it finished;
                // finished work (by hand or by Claude) is never reopened by the analysis
                if x.status != "done" && (x.manual != Some(true) || status == "done") {
                    if x.status != status {
                        x.updated_at = now;
                        if !updated_ids.contains(&x.id) {
                            updated_ids.push(x.id.clone());
                        }
                    }
                    if status == "done" {
                        x.manual = Some(false);
                    }
                    x.status = status;
                }
            } else {
                // nothing new to do, or a message whose task I deleted
                if status == "done" || r.msg_id.as_ref().is_some_and(|m| t.dismissed.contains(m)) {
                    continue;
                }
                // from a message an earlier run already read, in a chat that has tasks: a re-worded duplicate
                let old = src.is_none_or(|s| s.ts <= prev);
                if old && prev > 0 && t.tasks.iter().any(|x| x.thread_id == th.thread_id) {
                    continue;
                }
                let task = ZaloTask {
                    id: uuid::Uuid::new_v4().to_string(),
                    title,
                    detail: trimmed(&r.detail),
                    thread_id: th.thread_id.clone(),
                    thread_name: th.name.clone(),
                    group: th.group,
                    msg_id: src.map(|s| s.id.clone()),
                    from: trimmed(&r.from).or_else(|| src.map(|s| s.from.clone())).unwrap_or_else(|| th.name.clone()),
                    from_id: match src {
                        Some(s) if !s.mine => Some(s.from_id.clone()),
                        _ => (!th.group).then(|| th.thread_id.clone()),
                    },
                    avatar: None,
                    ts: src.map(|s| s.ts).or_else(|| th.msgs.last().map(|m| m.ts)).unwrap_or(now),
                    status,
                    manual: None,
                    updated_at: now,
                };
                t.tasks.push(task.clone());
                created.push(task);
            }
        }
    }
    let updated: Vec<ZaloTask> = {
        let mut t = tasks();
        // forget finished work after a while
        let keep = now - KEEP_DONE_DAYS * DAY;
        t.tasks.retain(|x| x.status != "done" || x.updated_at > keep);
        let ids: Vec<String> = t.tasks.iter().map(|x| x.id.clone()).collect();
        t.unseen.retain(|k, _| ids.contains(k));
        t.tasks.sort_by_key(|x| std::cmp::Reverse(x.ts));
        t.analyzed_at = now;
        t.fresh = created.iter().map(|x| x.id.clone()).collect();
        t.changed = updated_ids.clone();
        updated_ids.iter().filter_map(|id| t.tasks.iter().find(|x| &x.id == id).cloned()).collect()
    };
    let new_open: Vec<ZaloTask> = created.into_iter().filter(|x| x.status != "done").collect();
    announce(&new_open, &updated);
    save().await;
    Ok(())
}

/* ------------------------------ commands ------------------------------ */

#[tauri::command]
pub async fn zalo_tasks() -> ZaloTasksState {
    get_tasks()
}

#[tauri::command]
pub async fn zalo_tasks_analyze() -> ZaloTasksState {
    analyze().await
}

#[tauri::command]
pub async fn zalo_task_status(id: String, status: String) -> ZaloTasksState {
    set_status(&id, &status)
}

#[tauri::command]
pub async fn zalo_task_context(id: String) -> Res<TaskContext> {
    Ok(task_context(&id).await?)
}

#[tauri::command]
pub async fn zalo_task_remove(id: String) -> ZaloTasksState {
    remove_task(&id)
}

#[tauri::command]
pub async fn zalo_tasks_viewing(viewing: bool) {
    set_viewing(viewing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(safe_name("a<b>:c\"d/e\\f|g?h*  x.pdf"), "a_b_c_d_e_f_g_h_ x.pdf");
        assert_eq!(safe_name("   "), "file");
        assert_eq!(ext_of("https://x/y.JPEG?a=1", "jpg"), "jpeg");
        assert_eq!(ext_of("https://x/y", "jpg"), "jpg");
        let r = parse("ok {\"tasks\":[{\"id\":null,\"threadId\":\"1\",\"title\":\" a \",\"status\":\"doing\"}]} done").unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(trimmed(&r[0].title).as_deref(), Some("a"));
        assert!(parse("no json").is_err());
        let e = vec![Existing { id: "1", thread_id: "2", title: "t", status: "todo" }];
        assert_eq!(serde_json::to_string(&e).unwrap(), r#"[{"id":"1","threadId":"2","title":"t","status":"todo"}]"#);
    }
}
