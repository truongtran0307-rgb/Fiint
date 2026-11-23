//! Terminals: one pseudo-console per chat / shell, streamed to the page through its own Channel.

use std::{
    collections::HashMap,
    io::{Read, Write},
    path::PathBuf,
    sync::{
        LazyLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use parking_lot::Mutex;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tokio::sync::mpsc;

use crate::core::{Res, clean_env, which};

/* ---------------- coding agents ---------------- */

/// (id, command on PATH) — src/shared/agents.ts
const AGENTS: [(&str, &str); 7] = [
    ("claude", "claude"),
    ("codex", "codex"),
    ("grok", "grok"),
    ("gemini", "gemini"),
    ("qwen", "qwen"),
    ("copilot", "copilot"),
    ("opencode", "opencode"),
];

fn agent_cmd(id: Option<&str>) -> &'static str {
    let id = id.unwrap_or("claude");
    AGENTS.iter().find(|(a, _)| *a == id).map_or("claude", |(_, c)| c)
}

fn default_shell() -> String {
    static SHELL: LazyLock<String> = LazyLock::new(|| {
        if cfg!(windows) {
            which("pwsh.exe").map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "powershell.exe".into())
        } else {
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())
        }
    });
    SHELL.clone()
}

/// The `claude` executable, also where npm / the native installer put it when PATH misses it.
pub async fn claude_path() -> Option<PathBuf> {
    tokio::task::spawn_blocking(|| {
        which("claude").or_else(|| {
            if !cfg!(windows) {
                return None;
            }
            let home = crate::core::home();
            let appdata = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default();
            [home.join(".local").join("bin").join("claude.exe"), appdata.join("npm").join("claude.cmd")].into_iter().find(|p| p.exists())
        })
    })
    .await
    .ok()
    .flatten()
}

#[derive(Serialize)]
pub struct ClaudeInfo {
    found: bool,
    path: Option<String>,
    shell: String,
}

#[tauri::command]
pub async fn claude_detect() -> Res<ClaudeInfo> {
    let path = claude_path().await;
    Ok(ClaudeInfo { found: path.is_some(), path: path.map(|p| p.to_string_lossy().into_owned()), shell: default_shell() })
}

#[derive(Serialize)]
pub struct AgentInfo {
    id: &'static str,
    found: bool,
}

#[tauri::command]
pub async fn agents_detect() -> Res<Vec<AgentInfo>> {
    Ok(tokio::task::spawn_blocking(|| AGENTS.iter().map(|(id, cmd)| AgentInfo { id, found: which(cmd).is_some() }).collect()).await?)
}

/* ---------------- processes ---------------- */

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnOptions {
    id: String,
    cwd: String,
    /// "claude" | "shell"
    kind: String,
    agent: Option<String>,
    claude_args: Option<Vec<String>>,
    cols: u16,
    rows: u16,
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PtyMsg {
    Data { data: String },
    Exit { code: i32 },
}

struct Session {
    generation: u64,
    /// keystrokes go to a writer thread: a ConPTY input pipe whose process isn't reading blocks,
    /// and that must never stall the command (Tauri runs sync commands on the UI thread)
    input: std::sync::mpsc::Sender<Vec<u8>>,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

static SESSIONS: LazyLock<Mutex<HashMap<String, Session>>> = LazyLock::new(Default::default);
/// Terminals on screen right now. Their output goes out every 5 ms; the others' is gathered for
/// 50 ms — ten busy chats in the background then wake the page ~200×/s instead of ~2000×/s.
static VISIBLE: LazyLock<Mutex<std::collections::HashSet<String>>> = LazyLock::new(Default::default);
static GENERATION: AtomicU64 = AtomicU64::new(1);

/// PowerShell-safe argument.
fn quote_arg(a: &str) -> String {
    if !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || "_-./:=".contains(c)) {
        a.to_owned()
    } else {
        format!("'{}'", a.replace('\'', "''"))
    }
}

enum Chunk {
    Data(Vec<u8>),
    Exit(i32),
}

#[tauri::command]
pub async fn pty_spawn(opts: SpawnOptions, out: Channel<PtyMsg>) -> Res<()> {
    pty_kill(opts.id.clone());
    let shell = default_shell();
    let windows = cfg!(windows);

    let args: Vec<String> = if opts.kind == "claude" {
        let cmd = agent_cmd(opts.agent.as_deref());
        let mut parts = vec![cmd.to_owned()];
        parts.extend(opts.claude_args.unwrap_or_default());
        // the chat's own Web tab, database and Zalo as MCP tools (Claude only); last, because
        // --mcp-config takes several values
        if cmd == "claude"
            && let Some(cfg) = crate::mcp::config_for(&opts.id)
        {
            parts.extend(["--mcp-config".to_owned(), cfg]);
        }
        let line = parts.iter().map(|p| quote_arg(p)).collect::<Vec<_>>().join(" ");
        // inside an interactive shell: when the agent exits the user lands in a shell in the
        // same folder instead of a dead pane
        if windows { vec!["-NoLogo".into(), "-NoExit".into(), "-Command".into(), line] } else { vec!["-lc".into(), format!("{line}; exec {shell}")] }
    } else if windows {
        vec!["-NoLogo".into()]
    } else {
        vec!["-l".into()]
    };

    let mut cmd = CommandBuilder::new(&shell);
    cmd.args(&args);
    // Claude Code keys folder trust by the exact path (`E:/x` ≠ `E:\x`): hand it the native form
    cmd.cwd(if windows { opts.cwd.replace('/', "\\") } else { opts.cwd.clone() });
    cmd.env_clear();
    for (k, v) in clean_env() {
        cmd.env(k, v);
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("FORCE_COLOR", "1");

    let size = PtySize { rows: opts.rows.max(2), cols: opts.cols.max(2), pixel_width: 0, pixel_height: 0 };
    let pair = native_pty_system().openpty(size)?;
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let killer = child.clone_killer();

    // in order, off the caller's thread; ends when the session (its sender) is dropped or a write fails
    let (input, keys) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::Builder::new().name(format!("pty-write-{}", opts.id)).spawn(move || {
        while let Ok(mut data) = keys.recv() {
            // a paste arrives as many pieces: write what has queued up in one go
            while let Ok(more) = keys.try_recv() {
                data.extend(more);
            }
            if writer.write_all(&data).and_then(|_| writer.flush()).is_err() {
                break;
            }
        }
    })?;

    let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
    SESSIONS.lock().insert(opts.id.clone(), Session { generation, input, master: pair.master, killer });

    let (tx, mut rx) = mpsc::unbounded_channel::<Chunk>();

    // blocking reads on their own thread
    let tx_read = tx.clone();
    std::thread::Builder::new().name(format!("pty-read-{}", opts.id)).spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx_read.send(Chunk::Data(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
            }
        }
    })?;

    // exit: give the last output a moment to arrive (ConPTY keeps the pipe open until closed)
    std::thread::Builder::new().name(format!("pty-wait-{}", opts.id)).spawn(move || {
        let code = child.wait().map(|s| s.exit_code() as i32).unwrap_or(-1);
        std::thread::sleep(Duration::from_millis(40));
        let _ = tx.send(Chunk::Exit(code));
    })?;

    // Claude Code redraws in many tiny chunks; one message per chunk floods the page and makes the
    // chat stutter. But a fixed gathering delay lands on every keystroke's echo (and a 5 ms sleep is
    // really ~16 ms on Windows' default timer), which made typing lag. So: output after a quiet
    // moment goes out at once; a running stream is sent at most once a frame (hidden: every 50 ms).
    let id = opts.id;
    tauri::async_runtime::spawn(async move {
        let mut carry: Vec<u8> = Vec::new();
        let mut last_send = std::time::Instant::now() - Duration::from_secs(1);
        let send = |carry: &mut Vec<u8>, out: &Channel<PtyMsg>| {
            if carry.is_empty() {
                return;
            }
            // keep a UTF-8 sequence that was cut in half for the next round
            let cut = match std::str::from_utf8(carry) {
                Ok(_) => carry.len(),
                Err(e) if e.error_len().is_none() => e.valid_up_to(),
                Err(_) => carry.len(),
            };
            let rest = carry.split_off(cut);
            let data = String::from_utf8_lossy(carry).into_owned();
            *carry = rest;
            if !data.is_empty() {
                let _ = out.send(PtyMsg::Data { data });
            }
        };
        while let Some(first) = rx.recv().await {
            let mut exit = None;
            match first {
                Chunk::Data(d) => carry.extend(d),
                Chunk::Exit(c) => exit = Some(c),
            }
            // whatever is already queued goes in the same piece
            while exit.is_none() {
                match rx.try_recv() {
                    Ok(Chunk::Data(d)) => carry.extend(d),
                    Ok(Chunk::Exit(c)) => exit = Some(c),
                    Err(_) => break,
                }
            }
            let gap = Duration::from_millis(if VISIBLE.lock().contains(&id) { 16 } else { 50 });
            let wait = gap.saturating_sub(last_send.elapsed());
            if exit.is_none() && !wait.is_zero() {
                let deadline = tokio::time::sleep(wait);
                tokio::pin!(deadline);
                while carry.len() < 256 * 1024 {
                    tokio::select! {
                        _ = &mut deadline => break,
                        m = rx.recv() => match m {
                            Some(Chunk::Data(d)) => carry.extend(d),
                            Some(Chunk::Exit(c)) => { exit = Some(c); break }
                            None => break,
                        }
                    }
                }
            }
            send(&mut carry, &out);
            last_send = std::time::Instant::now();
            if let Some(code) = exit {
                // a process we killed or replaced (restart) must not report "exited": the id
                // already belongs to a new, live process
                let mine = {
                    let mut s = SESSIONS.lock();
                    if s.get(&id).is_some_and(|x| x.generation == generation) { s.remove(&id) } else { None }
                };
                if let Some(sess) = mine {
                    VISIBLE.lock().remove(&id);
                    let _ = out.send(PtyMsg::Exit { code });
                    // closing a ConPTY can stall: not on an async worker
                    std::thread::spawn(move || drop(sess));
                }
                break;
            }
        }
    });
    Ok(())
}

/// false when there is no live process for this id (it died without us hearing about it).
#[tauri::command]
pub fn pty_write(id: String, data: String) -> bool {
    let mut s = SESSIONS.lock();
    let Some(sess) = s.get(&id) else { return false };
    if sess.input.send(data.into_bytes()).is_ok() {
        return true;
    }
    // the writer thread ended: the process is gone
    let dead = s.remove(&id);
    drop(s);
    VISIBLE.lock().remove(&id);
    std::thread::spawn(move || drop(dead));
    false
}

#[tauri::command]
pub async fn pty_resize(id: String, cols: u16, rows: u16) {
    // ResizePseudoConsole waits on conhost: keep it off the UI thread
    let _ = tokio::task::spawn_blocking(move || {
        if let Some(s) = SESSIONS.lock().get(&id) {
            let _ = s.master.resize(PtySize { rows: rows.max(2), cols: cols.max(2), pixel_width: 0, pixel_height: 0 });
        }
    })
    .await;
}

#[tauri::command]
pub fn pty_visible(id: String, visible: bool) {
    let mut v = VISIBLE.lock();
    if visible {
        v.insert(id);
    } else {
        v.remove(&id);
    }
}

#[tauri::command]
pub fn pty_kill(id: String) {
    VISIBLE.lock().remove(&id);
    let sess = SESSIONS.lock().remove(&id);
    if let Some(mut s) = sess {
        // closing a ConPTY can stall: never on the caller's thread
        std::thread::spawn(move || {
            let _ = s.killer.kill();
            drop(s);
        });
    }
}

pub fn kill_all() {
    let all: Vec<Session> = SESSIONS.lock().drain().map(|(_, s)| s).collect();
    for mut s in all {
        let _ = s.killer.kill();
    }
}
