//! What every module shares: the app handle, events to the window, the error type commands
//! return, the data folder, and child processes that never flash a console window.

use std::{
    collections::HashMap,
    ffi::OsStr,
    fmt::Display,
    path::PathBuf,
    sync::OnceLock,
};

use serde::{Serialize, Serializer};
use tauri::{AppHandle, Emitter, Manager, Window};

static APP: OnceLock<AppHandle> = OnceLock::new();

/// Called once from `setup`, before anything else touches the handle.
pub fn init(app: &AppHandle) {
    let _ = APP.set(app.clone());
}

pub fn app() -> &'static AppHandle {
    APP.get().expect("core::init not called")
}

pub const MAIN: &str = "main";

pub fn main_window() -> Option<Window> {
    app().get_window(MAIN)
}

/// Event to the main window. `args` is the listener's argument list: pass a tuple, e.g.
/// `emit("fs:changed", (paths,))` — serde writes tuples as JSON arrays.
pub fn emit<A: Serialize + Clone>(event: &str, args: A) {
    if let Some(w) = main_window() {
        let _ = w.emit_to(MAIN, event, args);
    }
}

/* ---------------- errors ---------------- */

/// Error a command hands back to the page: it arrives as the rejected promise's message.
#[derive(Debug)]
pub struct Error(pub String);

impl<E: Into<anyhow::Error>> From<E> for Error {
    fn from(e: E) -> Self {
        Self(format!("{:#}", e.into()))
    }
}

impl Serialize for Error {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub fn err(msg: impl Into<String>) -> Error {
    Error(msg.into())
}

pub type Res<T> = Result<T, Error>;

/* ---------------- folders ---------------- */

/// The app's data folder. Kept at "%APPDATA%/TINJS Studio" (the Electron build's folder) so
/// settings, recent folders, saved connections and the Zalo session carry over.
/// `STUDIO_USER_DATA` gives a dev copy its own profile.
pub fn user_data() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::var_os("STUDIO_USER_DATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs::config_dir().unwrap_or_else(std::env::temp_dir).join("TINJS Studio"));
        let _ = std::fs::create_dir_all(&dir);
        dir
    })
    .clone()
}

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_default()
}

/// A bundled file from `resources/` (icons, the PowerShell bridge…).
pub fn resource(name: &str) -> PathBuf {
    app()
        .path()
        .resource_dir()
        .map(|d| d.join("resources").join(name))
        .unwrap_or_else(|_| PathBuf::from("resources").join(name))
}

/// Writes through a temp file + rename, so a crash never leaves half a JSON file behind.
pub async fn write_atomic(path: impl Into<PathBuf>, data: impl AsRef<[u8]>) -> std::io::Result<()> {
    let path = path.into();
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    tokio::fs::write(&tmp, data).await?;
    tokio::fs::rename(&tmp, &path).await
}

/* ---------------- child processes ---------------- */

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `tokio::process::Command` that never pops up a console window on Windows.
pub fn command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    #[allow(unused_mut)]
    let mut c = tokio::process::Command::new(program);
    #[cfg(windows)]
    c.creation_flags(CREATE_NO_WINDOW);
    c.kill_on_drop(true);
    c
}

/// Same, blocking (`std::process`).
pub fn command_sync(program: impl AsRef<OsStr>) -> std::process::Command {
    #[allow(unused_mut)]
    let mut c = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// The environment chats start with. When the app itself was launched from inside a Claude Code
/// session, that session's variables (CLAUDECODE, CLAUDE_CODE_*, MCP_*, the host's API URL) would
/// make every chat a child session that stops saving transcripts, so they are dropped.
pub fn clean_env() -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    if env.contains_key("CLAUDECODE") || env.contains_key("CLAUDE_CODE_SESSION_ID") {
        env.retain(|k, _| !(k == "CLAUDECODE" || k.starts_with("CLAUDE_") || k.starts_with("MCP_") || k == "ANTHROPIC_BASE_URL"));
    }
    env
}

/// First match of `cmd` on PATH (PATHEXT aware on Windows).
pub fn which(cmd: &str) -> Option<PathBuf> {
    which::which(cmd).ok()
}

/// macOS apps opened from Finder get launchd's bare PATH: take the login shell's PATH, plus the
/// usual install folders, so `claude`, `git`, `node`… are found.
#[cfg(target_os = "macos")]
pub fn fix_mac_path() {
    let mut parts: Vec<String> = Vec::new();
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    if let Ok(out) = std::process::Command::new(&shell).args(["-ilc", "printf \"__PATH__%s__PATH__\" \"$PATH\""]).output() {
        let s = String::from_utf8_lossy(&out.stdout);
        if let Some(m) = s.split("__PATH__").nth(1) {
            parts.extend(m.split(':').map(str::to_owned));
        }
    }
    let home = home().display().to_string();
    parts.extend(std::env::var("PATH").unwrap_or_default().split(':').map(str::to_owned));
    for p in ["/opt/homebrew/bin", "/usr/local/bin"] {
        parts.push(p.into());
    }
    for p in [".local/bin", ".claude/local", ".npm-global/bin", ".bun/bin"] {
        parts.push(format!("{home}/{p}"));
    }
    let mut seen = std::collections::HashSet::new();
    parts.retain(|p| !p.is_empty() && seen.insert(p.clone()));
    // SAFETY: called once at startup, before any other thread reads the environment.
    unsafe { std::env::set_var("PATH", parts.join(":")) };
}

#[cfg(not(target_os = "macos"))]
pub fn fix_mac_path() {}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
