//! Reads the Claude Code session store (`~/.claude/projects/<encoded-cwd>/<session>.jsonl`),
//! which the Claude desktop app, the CLI and the IDE extensions all share.
//! The JSONL format is internal to Claude Code, so everything here is best-effort:
//! we only pick out cwd, title and timestamps, and skip whatever we don't recognise.

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet, VecDeque},
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        LazyLock,
        atomic::{AtomicUsize, Ordering as AtomicOrdering},
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher, event::ModifyKind};
use parking_lot::Mutex;
use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::core::{Res, emit, home};

type Obj = Map<String, Value>;

/* ---------------- types (mirror src/shared/types.ts) ---------------- */

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    /// folder the session ran in — `claude --resume` must run from here
    pub cwd: String,
    /// repo the session belongs to (worktree sessions roll up to their repo)
    pub project_cwd: String,
    pub created: i64,
    pub updated: i64,
    pub cwd_exists: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectGroup {
    pub key: String,
    pub cwd: String,
    pub name: String,
    pub updated: i64,
    pub sessions: Vec<SessionInfo>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    pub used: u64,
    pub model: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTask {
    pub id: String,
    pub command: String,
    pub description: String,
    /// running, completed, failed, killed… (as Claude Code reports it)
    pub status: String,
    pub started_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_file: Option<String>,
    /// the end of its output
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatFile {
    pub path: String,
    /// created, edited or read (a file Claude read and then changed counts as changed)
    pub action: String,
    /// successful Write / Edit calls on it
    pub edits: u32,
    /// the last time Claude touched it
    pub at: i64,
    pub exists: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    id: String,
    role: &'static str,
    text: String,
    at: i64,
}

#[derive(Serialize)]
pub struct ChatMessageUpdate {
    revision: u64,
    rows: Option<Vec<ChatMessage>>,
}

/* ---------------- commands ---------------- */

#[tauri::command]
pub async fn sessions_list() -> Res<Vec<ProjectGroup>> {
    watch_sessions();
    Ok(tokio::task::spawn_blocking(list_sessions).await?)
}

#[tauri::command]
pub async fn sessions_images(session_id: String, since: Option<u64>) -> Res<SessionImages> {
    Ok(tokio::task::spawn_blocking(move || session_images(&session_id, since)).await?)
}

#[tauri::command]
pub async fn sessions_context(session_id: String) -> Res<Option<Context>> {
    Ok(tokio::task::spawn_blocking(move || session_context(&session_id)).await?)
}

#[tauri::command]
pub async fn sessions_tasks(session_id: String) -> Res<Vec<ChatTask>> {
    Ok(tokio::task::spawn_blocking(move || session_tasks(&session_id)).await?)
}

#[tauri::command]
pub async fn sessions_files(session_id: String) -> Res<Vec<ChatFile>> {
    Ok(tokio::task::spawn_blocking(move || session_files(&session_id)).await?)
}

#[tauri::command]
pub async fn sessions_messages(session_id: String, since: Option<u64>) -> Res<ChatMessageUpdate> {
    watch_sessions();
    Ok(tokio::task::spawn_blocking(move || {
        scan(&MESSAGE_SCANS, &session_id, MessageScan::feed, |s| ChatMessageUpdate {
            revision: s.revision,
            rows: (since != Some(s.revision)).then(|| s.messages.iter().cloned().collect()),
        })
        .unwrap_or_else(|| ChatMessageUpdate {
            revision: 0,
            rows: (since != Some(0)).then(Vec::new),
        })
    })
    .await?)
}

/* ---------------- session list ---------------- */

pub fn projects_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".claude"))
        .join("projects")
}

struct Cached {
    mtime: SystemTime,
    size: u64,
    info: Option<SessionInfo>,
}

static CACHE: LazyLock<Mutex<HashMap<PathBuf, Cached>>> = LazyLock::new(Default::default);

fn ms(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    }
}

/// `length` bytes from `start` (fewer at the end of the file), decoded lossily like Buffer#toString.
fn read_slice(file: &Path, start: u64, length: u64) -> io::Result<String> {
    let mut f = File::open(file)?;
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity(length.min(64 * 1024 * 1024) as usize);
    f.take(length).read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn parse_lines(text: &str, drop_first: bool) -> Vec<Obj> {
    text.split('\n')
        .skip(usize::from(drop_first)) // probably cut mid-line
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str::<Obj>(l).ok()) // partial lines fail and are skipped
        .collect()
}

fn str_of<'a>(o: &'a Obj, key: &str) -> Option<&'a str> {
    o.get(key).and_then(Value::as_str)
}

fn is_type(o: &Obj, ty: &str) -> bool {
    str_of(o, "type") == Some(ty)
}

/// JS truthiness of an optional JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// JS `String(v)`, near enough.
fn js_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        Value::Bool(_) | Value::Number(_) => v.to_string(),
        Value::Array(a) => a.iter().map(|x| if x.is_null() { String::new() } else { js_string(x) }).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// `a ?? b ?? ''` over object keys, then `String()`.
fn js_string_coalesce(o: Option<&Obj>, keys: &[&str]) -> String {
    o.and_then(|o| keys.iter().find_map(|k| o.get(*k).filter(|v| !v.is_null())))
        .map(js_string)
        .unwrap_or_default()
}

fn user_text(rec: &Obj) -> Option<String> {
    if !is_type(rec, "user") {
        return None;
    }
    let content = rec.get("message")?.get("content")?;
    // skip injected command / system wrappers
    let real = |t: &&str| !t.is_empty() && !t.starts_with('<') && !t.starts_with("Caveat:");
    let text = match content {
        Value::String(s) => Some(s.trim()).filter(real)?,
        // the prompt may follow injected blocks (<system-reminder>, IDE context…) in the same message
        Value::Array(items) => items
            .iter()
            .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|c| c.get("text").and_then(Value::as_str).map(str::trim))
            .find(real)?,
        _ => return None,
    };
    Some(text.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn find_title(recs: &[Obj]) -> Option<String> {
    recs.iter()
        .rev()
        .filter(|r| is_type(r, "custom-title"))
        .find_map(|r| str_of(r, "customTitle").map(str::trim).filter(|t| !t.is_empty()))
        .or_else(|| recs.iter().rev().filter(|r| is_type(r, "summary")).find_map(|r| str_of(r, "summary")))
        .map(str::to_owned)
}

fn read_session(file: &Path, dir_name: &str) -> io::Result<Option<SessionInfo>> {
    let st = fs::metadata(file)?;
    let mtime = st.modified()?;
    let size = st.len();
    if let Some(hit) = CACHE.lock().get(file)
        && hit.mtime == mtime
        && hit.size == size
    {
        return Ok(hit.info.clone());
    }

    const HEAD: u64 = 96 * 1024;
    const TAIL: u64 = 256 * 1024;
    let head = parse_lines(&read_slice(file, 0, HEAD.min(size))?, false);
    let tail = if size > HEAD { parse_lines(&read_slice(file, size.saturating_sub(TAIL), TAIL.min(size))?, true) } else { Vec::new() };

    let mut cwd: Option<String> = None;
    let mut first_prompt: Option<String> = None;
    let mut created: Option<i64> = None;
    for r in &head {
        if cwd.is_none()
            && let Some(c) = str_of(r, "cwd").filter(|c| !c.is_empty())
        {
            cwd = Some(c.to_owned());
        }
        if created.is_none()
            && let Some(ts) = str_of(r, "timestamp")
        {
            created = parse_date(ts).filter(|&t| t != 0);
        }
        if first_prompt.is_none() {
            first_prompt = user_text(r);
        }
        if cwd.is_some() && first_prompt.is_some() && created.is_some() {
            break;
        }
    }

    let head_title = find_title(&head);
    let mut info = None;
    if first_prompt.is_some() || head_title.as_deref().is_some_and(|t| !t.is_empty()) {
        let title = find_title(&tail)
            .or(head_title)
            .or_else(|| first_prompt.as_deref().map(|p| p.chars().take(90).collect()))
            .unwrap_or_default();
        let cwd = cwd.unwrap_or_else(|| decode_dir_name(dir_name));
        // "updated" = the last real message, not the file's mtime: just opening/resuming a chat
        // appends bookkeeping records (titles, snapshots…) that would bump it to "now"
        let last_msg = (if tail.is_empty() { &head } else { &tail })
            .iter()
            .rev()
            .filter(|r| is_type(r, "user") || is_type(r, "assistant"))
            .find_map(|r| str_of(r, "timestamp").and_then(parse_date).filter(|&t| t != 0));
        info = Some(SessionInfo {
            id: file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            title,
            project_cwd: project_root(&cwd),
            created: created.unwrap_or_else(|| st.created().map(ms).unwrap_or_else(|_| ms(mtime))),
            updated: last_msg.unwrap_or_else(|| ms(mtime)),
            cwd_exists: Path::new(&cwd).exists(),
            cwd,
        });
    }
    CACHE.lock().insert(file.to_path_buf(), Cached { mtime, size, info: info.clone() });
    Ok(info)
}

/// Lossy fallback when no record carries a cwd: "E--softek-core2" → "E:\softek\core2".
fn decode_dir_name(d: &str) -> String {
    let b = d.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && &b[1..3] == b"--" {
        format!("{}:\\{}", &d[..1], d[3..].replace('-', "\\"))
    } else {
        d.to_owned()
    }
}

/// Worktree sessions belong to the repo they were cut from.
fn project_root(cwd: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(.*?)[\\/]\.claude[\\/]worktrees[\\/]").unwrap());
    RE.captures(cwd).and_then(|c| c.get(1)).map_or_else(|| cwd.to_owned(), |m| m.as_str().to_owned())
}

fn is_sep(c: char) -> bool {
    c == '/' || (cfg!(windows) && c == '\\')
}

/// Path components the way Node's `path` sees them (drive prefix dropped on Windows).
fn path_parts(p: &str) -> Vec<&str> {
    let mut p = p;
    if cfg!(windows) && p.len() >= 2 && p.as_bytes()[1] == b':' && p.as_bytes()[0].is_ascii_alphabetic() {
        p = &p[2..];
    }
    p.split(is_sep).filter(|s| !s.is_empty()).collect()
}

/// `path.basename(p)`
fn basename(p: &str) -> &str {
    path_parts(p).last().copied().unwrap_or("")
}

/// `path.basename(path.dirname(p))`
fn parent_name(p: &str) -> &str {
    let parts = path_parts(p);
    if parts.len() >= 2 { parts[parts.len() - 2] } else { "" }
}

/// `a.localeCompare(b, undefined, { sensitivity: 'base', numeric: true })`, approximately:
/// case-insensitive, digit runs compared by value.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                fn take(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
                    let mut s = String::new();
                    while let Some(&c) = it.peek()
                        && c.is_ascii_digit()
                    {
                        s.push(c);
                        it.next();
                    }
                    s
                }
                let (n, m) = (take(&mut x), take(&mut y));
                let (n, m) = (n.trim_start_matches('0'), m.trim_start_matches('0'));
                let o = n.len().cmp(&m.len()).then_with(|| n.cmp(m));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                if c != d {
                    return c.cmp(&d);
                }
                x.next();
                y.next();
            }
        }
    }
}

fn list_sessions() -> Vec<ProjectGroup> {
    let root = projects_dir();
    let Ok(rd) = fs::read_dir(&root) else { return Vec::new() };
    let dirs: Vec<String> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();

    let mut jobs: Vec<(PathBuf, String)> = Vec::new();
    for d in dirs {
        let Ok(rd) = fs::read_dir(root.join(&d)) else { continue };
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().ends_with(".jsonl") {
                jobs.push((e.path(), d.clone()));
            }
        }
    }

    // files that are gone drop out of the cache
    {
        let live: HashSet<&PathBuf> = jobs.iter().map(|(p, _)| p).collect();
        CACHE.lock().retain(|p, _| live.contains(p));
    }

    // Unchanged files are a cache hit (one stat); a cold start reads them on a few threads.
    let next = AtomicUsize::new(0);
    let found = Mutex::new(Vec::<SessionInfo>::new());
    let workers = thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8).min(jobs.len().max(1));
    thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                while let Some((file, dir)) = jobs.get(next.fetch_add(1, AtomicOrdering::Relaxed)) {
                    if let Ok(Some(info)) = read_session(file, dir) {
                        found.lock().push(info);
                    }
                }
            });
        }
    });
    let mut all = found.into_inner();
    // newest first, so a group takes its folder spelling from its latest session
    all.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));

    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, ProjectGroup> = HashMap::new();
    for s in all {
        let key = s.project_cwd.to_lowercase().trim_end_matches(['\\', '/']).to_owned();
        let g = groups.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            let name = match basename(&s.project_cwd) {
                "" => s.project_cwd.clone(),
                n => n.to_owned(),
            };
            ProjectGroup { key, cwd: s.project_cwd.clone(), name, updated: 0, sessions: Vec::new() }
        });
        g.updated = g.updated.max(s.updated);
        g.sessions.push(s);
    }
    let mut list: Vec<ProjectGroup> = order.into_iter().filter_map(|k| groups.remove(&k)).collect();
    for g in &mut list {
        g.sessions.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
    }

    // Two folders with the same name ("monk" in two places) → add the parent folder to tell them apart.
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, g) in list.iter().enumerate() {
        by_name.entry(g.name.to_lowercase()).or_default().push(i);
    }
    for same in by_name.values().filter(|v| v.len() >= 2) {
        for &i in same {
            let g = &mut list[i];
            let parent = parent_name(&g.cwd).to_owned();
            if !parent.is_empty() {
                g.name = format!("{} · {parent}", g.name);
            }
        }
    }
    list.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    list
}

/* ---------------- watcher ---------------- */

static WATCHER: Mutex<Option<RecommendedWatcher>> = Mutex::new(None);

/// Whether a change under the store should refresh the list: project folders coming and going,
/// and `.jsonl` files directly inside them (chokidar `depth: 1` with non-jsonl files ignored).
fn relevant(root: &Path, ev: &notify::Event) -> bool {
    if matches!(ev.kind, EventKind::Access(_)) {
        return false;
    }
    let structural = matches!(ev.kind, EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_)));
    ev.paths.iter().any(|p| {
        let Ok(rel) = p.strip_prefix(root) else { return false };
        if !(1..=2).contains(&rel.components().count()) {
            return false;
        }
        if p.extension().is_some_and(|e| e == "jsonl") {
            return true;
        }
        // folders: only adds / removes, not "something inside changed"
        structural && (p.is_dir() || !p.exists())
    })
}

/// Push a "sessions changed" ping whenever Claude (any client) writes to the store.
fn watch_sessions() {
    let mut slot = WATCHER.lock();
    if slot.is_some() {
        return;
    }
    let root = projects_dir();
    // the session ids whose transcript changed ("" for a project folder coming or going, "*" when
    // events were lost and any chat may have changed)
    let (tx, rx) = mpsc::channel::<String>();
    let filter_root = root.clone();
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        // a watcher error or overflow (many chats writing at once) loses events: every chat re-reads
        let Ok(ev) = res else {
            let _ = tx.send("*".into());
            return;
        };
        if ev.need_rescan() {
            let _ = tx.send("*".into());
        } else if relevant(&filter_root, &ev) {
            for p in &ev.paths {
                let id = if p.extension().is_some_and(|e| e == "jsonl") { p.file_stem().map(|s| s.to_string_lossy().into_owned()) } else { None };
                let _ = tx.send(id.unwrap_or_default());
            }
        }
    });
    let Ok(mut watcher) = watcher else { return };
    // the store may not exist yet: leave the slot empty so the next list call tries again
    if watcher.watch(&root, RecursiveMode::Recursive).is_err() {
        return;
    }
    // Coalesce filesystem bursts without holding back a visible chat's new reply.
    thread::Builder::new()
        .name("sessions-watch".into())
        .spawn(move || {
            while let Ok(first) = rx.recv() {
                let mut ids: HashSet<String> = HashSet::from([first]);
                let deadline = Instant::now() + Duration::from_millis(320);
                loop {
                    let left = deadline.saturating_duration_since(Instant::now());
                    match rx.recv_timeout(left.min(Duration::from_millis(110))) {
                        Ok(id) => {
                            ids.insert(id);
                        }
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                // listeners for one chat (its images, tasks, context) skip pings for other chats ("*": none skip)
                ids.remove("");
                emit("sessions:changed", (ids.into_iter().collect::<Vec<_>>(),));
            }
        })
        .ok();
    *slot = Some(watcher);
}

/* ---------------- one session's file ---------------- */

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn find_session_file(session_id: &str) -> Option<PathBuf> {
    let root = projects_dir();
    let name = format!("{session_id}.jsonl");
    fs::read_dir(&root).ok()?.flatten().map(|e| root.join(e.file_name()).join(&name)).find(|f| f.exists())
}

/* ---------------- reading a transcript as it grows ---------------- */

/// A transcript only ever grows. Images and background tasks used to re-read (and re-parse) the
/// whole file - often tens of MB - every time any chat wrote a line; now each remembers how far
/// it got and parses only the new lines.
struct Scan<S> {
    file: PathBuf,
    offset: u64,
    last_use: u64,
    state: S,
}

type ScanCache<S> = LazyLock<Mutex<HashMap<String, Scan<S>>>>;

/// open chats' scans kept; the least recently used goes first
const SCAN_KEEP: usize = 8;
static SCAN_CLOCK: AtomicUsize = AtomicUsize::new(0);

/// Feed the lines added since the last call to `feed` (a rewritten / shorter file starts over),
/// then hand the state to `done`, all under the cache's lock.
fn scan<S: Default, R>(cache: &ScanCache<S>, session_id: &str, feed: impl Fn(&mut S, &str), done: impl FnOnce(&S) -> R) -> Option<R> {
    if !valid_id(session_id) {
        return None;
    }
    let mut map = cache.lock();
    let stale = map.get(session_id).is_none_or(|s| !s.file.exists());
    if stale {
        let file = find_session_file(session_id)?;
        map.insert(session_id.to_owned(), Scan { file, offset: 0, last_use: 0, state: S::default() });
    }
    let sc = map.get_mut(session_id)?;
    sc.last_use = SCAN_CLOCK.fetch_add(1, AtomicOrdering::Relaxed) as u64;
    let len = fs::metadata(&sc.file).ok()?.len();
    if len < sc.offset {
        sc.offset = 0;
        sc.state = S::default();
    }
    if len > sc.offset {
        let mut f = File::open(&sc.file).ok()?;
        f.seek(SeekFrom::Start(sc.offset)).ok()?;
        let mut buf = Vec::with_capacity((len - sc.offset).min(64 * 1024 * 1024) as usize);
        f.take(len - sc.offset).read_to_end(&mut buf).ok()?;
        // only whole lines: the one Claude is still writing waits for the next call
        if let Some(end) = buf.iter().rposition(|b| *b == b'\n') {
            sc.offset += end as u64 + 1;
            feed(&mut sc.state, &String::from_utf8_lossy(&buf[..end]));
        }
    }
    let out = done(&sc.state);
    if map.len() > SCAN_KEEP
        && let Some(old) = map.iter().min_by_key(|(_, s)| s.last_use).map(|(k, _)| k.clone())
    {
        map.remove(&old);
    }
    Some(out)
}

/* ---------------- user-visible conversation ---------------- */

const CHAT_MESSAGE_LIMIT: usize = 120;
const CHAT_TEXT_LIMIT: usize = 24_000;

#[derive(Default)]
struct MessageScan {
    messages: VecDeque<ChatMessage>,
    sequence: u64,
    revision: u64,
}

static MESSAGE_SCANS: ScanCache<MessageScan> = LazyLock::new(|| Mutex::new(HashMap::new()));
static MESSAGE_REVISION: AtomicUsize = AtomicUsize::new(1);

impl MessageScan {
    fn feed(&mut self, text: &str) {
        let mut changed = false;
        for line in text.lines() {
            let Ok(row) = serde_json::from_str::<Obj>(line) else { continue };
            let role = match str_of(&row, "type") {
                Some("user") => "user",
                Some("assistant") => "assistant",
                _ => continue,
            };
            if truthy(row.get("isSidechain")) || truthy(row.get("isMeta")) {
                continue;
            }
            let Some(content) = row.get("message").and_then(|message| message.get("content")) else { continue };
            let blocks: Vec<&str> = match content {
                Value::String(text) => vec![text],
                Value::Array(blocks) => blocks.iter()
                    .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|block| block.get("text").and_then(Value::as_str))
                    .collect(),
                _ => continue,
            };
            let mut message = String::new();
            for block in blocks {
                let block = block.trim();
                if block.is_empty() || (role == "user" && hidden_user_block(block)) {
                    continue;
                }
                if !message.is_empty() {
                    message.push_str("\n\n");
                }
                message.push_str(block);
                if message.len() >= CHAT_TEXT_LIMIT {
                    break;
                }
            }
            if message.is_empty() {
                continue;
            }
            if message.len() > CHAT_TEXT_LIMIT {
                let mut end = CHAT_TEXT_LIMIT;
                while !message.is_char_boundary(end) {
                    end -= 1;
                }
                message.truncate(end);
                message.push_str("\n…");
            }
            self.sequence += 1;
            let id = str_of(&row, "uuid").map(str::to_owned).unwrap_or_else(|| format!("row-{}", self.sequence));
            let at = str_of(&row, "timestamp").and_then(parse_date).unwrap_or_default();
            let item = ChatMessage { id, role, text: message, at };
            if let Some(existing) = self.messages.iter_mut().find(|existing| existing.id == item.id) {
                if existing.role != item.role || existing.text != item.text || existing.at != item.at {
                    *existing = item;
                    changed = true;
                }
            } else {
                self.messages.push_back(item);
                changed = true;
                if self.messages.len() > CHAT_MESSAGE_LIMIT {
                    self.messages.pop_front();
                }
            }
        }
        if changed {
            self.revision = MESSAGE_REVISION.fetch_add(1, AtomicOrdering::Relaxed) as u64;
        }
    }
}

fn hidden_user_block(text: &str) -> bool {
    ["<system-reminder", "<local-command", "<command-name", "<command-message", "<command-args", "<user-prompt-submit-hook", "<task-notification", "<ide_", "Caveat: The messages below"]
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

/* ---------------- images in a session ---------------- */

const MAX_IMAGES: usize = 40;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

fn image_mime(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return None,
    })
}

enum Found {
    Data(String),
    Path(String),
}

#[derive(Default)]
struct ImageWalk {
    /// the newest pictures only (inline ones are big)
    found: Vec<Found>,
    seen: HashSet<String>,
    /// pictures found so far: the list's version
    total: u64,
}

impl ImageWalk {
    fn add(&mut self, key: String, item: Found) {
        if self.seen.insert(key) {
            self.found.push(item);
            self.total += 1;
            if self.found.len() > MAX_IMAGES * 2 {
                self.found.drain(..self.found.len() - MAX_IMAGES);
            }
        }
    }

    fn feed(&mut self, text: &str) {
        static EXT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.(png|jpe?g|gif|webp)").unwrap());
        for line in text.split('\n') {
            if !line.contains("image") && !EXT_RE.is_match(line) {
                continue;
            }
            // partial line while Claude is writing
            let Ok(row) = serde_json::from_str::<Value>(line) else { continue };
            let from_user = row.get("type").and_then(Value::as_str) == Some("user");
            match row.get("message").and_then(|m| m.get("content")) {
                Some(Value::String(s)) => self.walk(&serde_json::json!([{ "type": "text", "text": s }]), from_user),
                Some(c) => self.walk(c, from_user),
                None => {}
            }
        }
    }

    fn walk(&mut self, blocks: &Value, from_user: bool) {
        static PATH_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#"(?i)[A-Za-z]:[\\/][^"'\n\r<>|*?]+?\.(?:png|jpe?g|gif|webp)(?-u:\b)"#).unwrap());
        let Some(blocks) = blocks.as_array() else { return };
        for b in blocks {
            let Some(b) = b.as_object() else { continue };
            match str_of(b, "type") {
                Some("image") => {
                    let Some(src) = b.get("source").and_then(Value::as_object) else { continue };
                    if str_of(src, "type") == Some("base64")
                        && let Some(data) = str_of(src, "data").filter(|d| !d.is_empty())
                    {
                        let key = format!("{}{}", data.chars().take(200).collect::<String>(), data.len());
                        let mime = str_of(src, "media_type").unwrap_or("image/png");
                        self.add(key, Found::Data(format!("data:{mime};base64,{data}")));
                    }
                }
                Some("tool_result") => {
                    if let Some(c) = b.get("content") {
                        self.walk(c, false);
                    }
                }
                Some("text") if from_user => {
                    if let Some(text) = str_of(b, "text") {
                        for m in PATH_RE.find_iter(text) {
                            self.add(m.as_str().to_lowercase(), Found::Path(m.as_str().to_owned()));
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

static IMAGE_SCANS: ScanCache<ImageWalk> = LazyLock::new(Default::default);

#[derive(Serialize)]
pub struct SessionImages {
    /// changes whenever a picture is added
    version: u64,
    /// `None` when the caller already has this version (nothing re-encoded or sent)
    images: Option<Vec<String>>,
}

/// Every image in a chat, oldest first: pictures pasted into the conversation, screenshots tools
/// returned, and image files named in the user's messages. Returned as data: URLs.
fn session_images(session_id: &str, since: Option<u64>) -> SessionImages {
    let picked = scan(&IMAGE_SCANS, session_id, ImageWalk::feed, |w| {
        if since == Some(w.total) {
            return (w.total, None);
        }
        let skip = w.found.len().saturating_sub(MAX_IMAGES);
        let items: Vec<Found> = w
            .found
            .iter()
            .skip(skip)
            .map(|f| match f {
                Found::Data(d) => Found::Data(d.clone()),
                Found::Path(p) => Found::Path(p.clone()),
            })
            .collect();
        (w.total, Some(items))
    });
    let Some((version, items)) = picked else { return SessionImages { version: 0, images: Some(Vec::new()) } };
    // files are read and encoded outside the lock
    let images = items.map(|items| {
        let mut out = Vec::new();
        for item in items {
            match item {
                Found::Data(d) => out.push(d),
                Found::Path(p) => {
                    let Some(mime) = p.rsplit('.').next().and_then(image_mime) else { continue };
                    // the file may be gone (e.g. temp paste cleaned up)
                    let Ok(st) = fs::metadata(&p) else { continue };
                    if !st.is_file() || st.len() > MAX_FILE_BYTES {
                        continue;
                    }
                    if let Ok(bytes) = fs::read(&p) {
                        out.push(format!("data:{mime};base64,{}", B64.encode(bytes)));
                    }
                }
            }
        }
        out
    });
    SessionImages { version, images }
}

/* ---------------- context window ---------------- */

/// How full a chat's context window is: the token count of the last main-thread API call
/// (input + cache read + cache write + output), from the tail of the session file.
fn session_context(session_id: &str) -> Option<Context> {
    if !valid_id(session_id) {
        return None;
    }
    let file = find_session_file(session_id)?;
    let size = fs::metadata(&file).ok()?.len();
    // widen the window until a usage line turns up (big tool results can push it far back)
    for span in [256 * 1024, 4 * 1024 * 1024, size] {
        let start = size.saturating_sub(span);
        let rows = parse_lines(&read_slice(&file, start, size - start).ok()?, start > 0);
        for r in rows.iter().rev() {
            if !is_type(r, "assistant") || truthy(r.get("isSidechain")) || truthy(r.get("isApiErrorMessage")) {
                continue;
            }
            let Some(msg) = r.get("message").and_then(Value::as_object) else { continue };
            let model = str_of(msg, "model");
            if model == Some("<synthetic>") {
                continue;
            }
            let Some(u) = msg.get("usage").and_then(Value::as_object) else { continue };
            let n = |k: &str| u.get(k).and_then(Value::as_f64).unwrap_or(0.0);
            let used = n("input_tokens") + n("cache_read_input_tokens") + n("cache_creation_input_tokens") + n("output_tokens");
            if used > 0.0 {
                return Some(Context { used: used as u64, model: model.unwrap_or_default().to_owned() });
            }
        }
        if start == 0 {
            break;
        }
    }
    None
}

/* ---------------- background tasks ---------------- */

struct Call {
    command: String,
    description: String,
}

#[derive(Default)]
struct TaskScan {
    calls: HashMap<String, Call>,
    /// insertion-ordered map: a re-set task keeps its first position, like a JS Map
    tasks: Vec<ChatTask>,
    task_index: HashMap<String, usize>,
    ended: HashMap<String, (String, i64)>,
}

impl TaskScan {
    fn feed(&mut self, text: &str) {
        static OUTPUT_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"Output is being written to: ([^\n\r\u{2028}\u{2029}]+?\.output)").unwrap());
        static NOTE_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"(?s)<task-id>([^<]+)</task-id>.*?<status>([^<]+)</status>").unwrap());
        let TaskScan { calls, tasks, task_index, ended } = self;
        for line in text.split('\n') {
            if !line.contains("run_in_background") && !line.contains("backgroundTaskId") && !line.contains("<task-notification>") {
                continue;
            }
            let Ok(row) = serde_json::from_str::<Obj>(line) else { continue };
            let at = match row.get("timestamp") {
                None | Some(Value::Null) => None,
                Some(v) => parse_date(&js_string(v)),
            }
            .unwrap_or(0);
            let content = row.get("message").and_then(|m| m.get("content")).and_then(Value::as_array);

            if is_type(&row, "assistant")
                && let Some(content) = content
            {
                for b in content {
                    let Some(b) = b.as_object() else { continue };
                    let input = b.get("input").and_then(Value::as_object);
                    if is_type(b, "tool_use")
                        && let Some(id) = str_of(b, "id").filter(|id| !id.is_empty())
                        && truthy(input.and_then(|i| i.get("run_in_background")))
                    {
                        calls.insert(
                            id.to_owned(),
                            Call {
                                command: js_string_coalesce(input, &["command", "prompt"]),
                                description: js_string_coalesce(input, &["description"]),
                            },
                        );
                    }
                }
            }

            let task_id = row.get("toolUseResult").and_then(|r| r.get("backgroundTaskId")).and_then(Value::as_str).filter(|s| !s.is_empty());
            if let Some(task_id) = task_id
                && let Some(content) = content
            {
                let r = content.iter().filter_map(Value::as_object).find(|b| is_type(b, "tool_result"));
                let msg = match r.and_then(|r| r.get("content")) {
                    Some(Value::String(s)) => s.clone(),
                    Some(v) if !v.is_null() => v.to_string(),
                    _ => "\"\"".to_owned(),
                };
                let call = calls.get(r.and_then(|r| str_of(r, "tool_use_id")).unwrap_or(""));
                let task = ChatTask {
                    id: task_id.to_owned(),
                    command: call.map(|c| c.command.clone()).unwrap_or_default(),
                    description: call.map(|c| c.description.clone()).unwrap_or_default(),
                    status: "running".into(),
                    started_at: at,
                    ended_at: None,
                    output_file: OUTPUT_RE.captures(&msg).map(|c| c[1].to_owned()),
                    output: None,
                };
                match task_index.get(task_id) {
                    Some(&i) => tasks[i] = task,
                    None => {
                        task_index.insert(task_id.to_owned(), tasks.len());
                        tasks.push(task);
                    }
                }
            }

            // the notification shows up several times (queued, delivered): the first one is when it ended
            let note = str_of(&row, "content").unwrap_or(line);
            for c in NOTE_RE.captures_iter(note) {
                ended.entry(c[1].to_owned()).or_insert_with(|| (c[2].to_owned(), at));
            }
        }

    }
}

static TASK_SCANS: ScanCache<TaskScan> = LazyLock::new(Default::default);

/// Background tasks Claude started in a chat (Bash/PowerShell with run_in_background), with their
/// status from the task notifications and the last part of their output file.
fn session_tasks(session_id: &str) -> Vec<ChatTask> {
    static ANSI_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*m").unwrap());
    let Some((mut tasks, ended)) = scan(&TASK_SCANS, session_id, TaskScan::feed, |s| (s.tasks.clone(), s.ended.clone())) else {
        return Vec::new();
    };
    tasks.reverse();
    for t in &mut tasks {
        if let Some((status, at)) = ended.get(&t.id) {
            t.status = status.clone();
            t.ended_at = Some(*at);
        }
        let Some(out_file) = &t.output_file else { continue };
        // the output file may have been cleaned up
        if let Ok(st) = fs::metadata(out_file)
            && let Ok(tail) = read_slice(Path::new(out_file), st.len().saturating_sub(6000), 6000)
        {
            t.output = Some(ANSI_RE.replace_all(&tail, "").into_owned());
        }
    }
    tasks
}

/* ---------------- files Claude touched ---------------- */

#[derive(Clone, Copy)]
enum Touch {
    Write,
    Edit,
    Read,
}

#[derive(Default)]
struct FileScan {
    /// tool calls waiting for their result (a failed Edit changed nothing)
    pending: HashMap<String, (String, Touch)>,
    /// in order of first touch; `index` is keyed by the lowercased path
    files: Vec<ChatFile>,
    index: HashMap<String, usize>,
}

impl FileScan {
    fn feed(&mut self, text: &str) {
        for line in text.split('\n') {
            let call = line.contains("\"file_path\"") || line.contains("\"notebook_path\"");
            let result = !self.pending.is_empty() && line.contains("tool_result") && self.pending.keys().any(|id| line.contains(id.as_str()));
            if !call && !result {
                continue;
            }
            let Ok(row) = serde_json::from_str::<Obj>(line) else { continue };
            // only the conversation's own rows: others (api-request-blob…) repeat its tool calls
            let (assistant, user) = (is_type(&row, "assistant"), is_type(&row, "user"));
            if !assistant && !user {
                continue;
            }
            let Some(content) = row.get("message").and_then(|m| m.get("content")).and_then(Value::as_array) else { continue };
            let at = row.get("timestamp").and_then(|v| parse_date(&js_string(v))).unwrap_or(0);
            // a Write over an existing file is an edit; over nothing, a new file
            let created = row.get("toolUseResult").and_then(|r| r.get("type")).and_then(Value::as_str) == Some("create");
            for b in content.iter().filter_map(Value::as_object) {
                if assistant && is_type(b, "tool_use") {
                    let touch = match str_of(b, "name") {
                        Some("Write") => Touch::Write,
                        Some("Edit" | "MultiEdit" | "NotebookEdit") => Touch::Edit,
                        Some("Read") => Touch::Read,
                        _ => continue,
                    };
                    let input = b.get("input").and_then(Value::as_object);
                    let path = input.and_then(|i| str_of(i, "file_path").or_else(|| str_of(i, "notebook_path"))).unwrap_or("");
                    if let Some(id) = str_of(b, "id")
                        && !path.is_empty()
                    {
                        self.pending.insert(id.to_owned(), (path.to_owned(), touch));
                    }
                } else if user
                    && is_type(b, "tool_result")
                    && let Some((path, touch)) = str_of(b, "tool_use_id").and_then(|id| self.pending.remove(id))
                    && !truthy(b.get("is_error"))
                {
                    self.touch(path, touch, created, at);
                }
            }
        }
    }

    fn touch(&mut self, path: String, touch: Touch, created: bool, at: i64) {
        let key = path.replace('/', "\\").to_lowercase();
        let files = &mut self.files;
        let i = *self.index.entry(key).or_insert_with(|| {
            files.push(ChatFile { path, action: "read".into(), edits: 0, at, exists: true });
            files.len() - 1
        });
        let f = &mut self.files[i];
        f.at = f.at.max(at);
        if matches!(touch, Touch::Read) {
            return;
        }
        f.edits += 1;
        if matches!(touch, Touch::Write) && created {
            f.action = "created".into();
        } else if f.action == "read" {
            f.action = "edited".into();
        }
    }
}

static FILE_SCANS: ScanCache<FileScan> = LazyLock::new(Default::default);

/// Files Claude created, edited or read in a chat (its Write / Edit / Read tools), most recent first.
fn session_files(session_id: &str) -> Vec<ChatFile> {
    let Some(mut files) = scan(&FILE_SCANS, session_id, FileScan::feed, |s| s.files.clone()) else {
        return Vec::new();
    };
    files.sort_by(|a, b| b.at.cmp(&a.at));
    for f in &mut files {
        f.exists = Path::new(&f.path).is_file();
    }
    files
}

/* ---------------- dates ---------------- */

/// `Date.parse` for the ISO 8601 timestamps Claude Code writes ("2025-01-02T03:04:05.678Z").
/// Times without an offset are read as UTC. `None` where JS would give NaN.
fn parse_date(s: &str) -> Option<i64> {
    let b = s.trim().as_bytes();
    let num = |from: usize, len: usize| -> Option<i64> {
        let part = b.get(from..from + len)?;
        part.iter().all(u8::is_ascii_digit).then(|| part.iter().fold(0i64, |n, d| n * 10 + i64::from(d - b'0')))
    };
    let (y, mo, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    if b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    if b.len() == 10 {
        return Some(days * 86_400_000);
    }
    if !matches!(b.get(10), Some(b'T' | b't' | b' ')) || b.get(13) != Some(&b':') {
        return None;
    }
    let (h, mi) = (num(11, 2)?, num(14, 2)?);
    let mut i = 16;
    let mut sec = 0;
    let mut millis = 0;
    if b.get(i) == Some(&b':') {
        sec = num(i + 1, 2)?;
        i += 3;
        if b.get(i) == Some(&b'.') {
            i += 1;
            let start = i;
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
            if i == start {
                return None;
            }
            let frac = &b[start..i.min(start + 3)];
            millis = frac.iter().fold(0i64, |n, d| n * 10 + i64::from(d - b'0')) * 10i64.pow(3 - frac.len() as u32);
        }
    }
    if h > 24 || mi > 59 || sec > 59 {
        return None;
    }
    let offset_min = match b.get(i) {
        None => 0,
        Some(b'Z' | b'z') if i + 1 == b.len() => 0,
        Some(&sign @ (b'+' | b'-')) => {
            let oh = num(i + 1, 2)?;
            let om = match b.get(i + 3) {
                Some(b':') => num(i + 4, 2).filter(|_| i + 6 == b.len())?,
                Some(_) => num(i + 3, 2).filter(|_| i + 5 == b.len())?,
                None => 0,
            };
            let off = oh * 60 + om;
            if sign == b'+' { off } else { -off }
        }
        _ => return None,
    };
    Some((days * 86_400 + h * 3600 + mi * 60 + sec - offset_min * 60) * 1000 + millis)
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_revision_only_changes_for_visible_conversation() {
        let mut transcript = MessageScan::default();
        transcript.feed(r#"{"type":"assistant","uuid":"hidden","message":{"content":[{"type":"tool_use","name":"Read"}]}}"#);
        assert_eq!(transcript.revision, 0);
        let row = r#"{"type":"assistant","uuid":"one","message":{"content":[{"type":"text","text":"Một"}]}}"#;
        transcript.feed(row);
        let revision = transcript.revision;
        assert!(revision > 0);
        transcript.feed(row);
        assert_eq!(transcript.revision, revision);
        transcript.feed(r#"{"type":"assistant","uuid":"one","message":{"content":[{"type":"text","text":"Một và hai"}]}}"#);
        assert_ne!(transcript.revision, revision);
        assert_eq!(transcript.messages.len(), 1);
        assert_eq!(transcript.messages[0].text, "Một và hai");
    }

    #[test]
    fn dates() {
        assert_eq!(parse_date("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(parse_date("2025-01-02T03:04:05.678Z"), Some(1_735_787_045_678));
        assert_eq!(parse_date("2025-01-02T05:04:05.678+02:00"), Some(1_735_787_045_678));
        assert_eq!(parse_date("nope"), None);
    }

    #[test]
    fn paths() {
        assert_eq!(decode_dir_name("E--softek-core2"), "E:\\softek\\core2");
        assert_eq!(project_root("E:\\x\\repo\\.claude\\worktrees\\a"), "E:\\x\\repo");
        assert_eq!(natural_cmp("a10", "A9"), Ordering::Greater);
    }
}

/// Delete a conversation from Claude Code's store: its transcript (and its subagent folder) go to the Recycle Bin.
#[tauri::command]
pub async fn sessions_delete(session_id: String) -> Res<()> {
    if session_id.is_empty() || !session_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(crate::core::err("bad session id"));
    }
    let mut found = false;
    for e in fs::read_dir(projects_dir())?.flatten() {
        let file = e.path().join(format!("{session_id}.jsonl"));
        if file.is_file() {
            crate::fsx::trash(&file).await?;
            found = true;
        }
        let dir = e.path().join(&session_id);
        if dir.is_dir() {
            crate::fsx::trash(&dir).await?;
        }
    }
    if !found {
        return Err(crate::core::err("this chat has no saved history"));
    }
    Ok(())
}
