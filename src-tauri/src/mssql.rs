//! SQL Server for the Database tab and Claude's `mssql_*` tools.
//!
//! Queries run in a long-lived PowerShell process (`mssql_bridge.ps1`): Windows ships SqlClient
//! with .NET Framework, so no driver is needed and SQL logins, Windows auth and LocalDB all work.
//! One JSON request per stdin line → one JSON reply per stdout line.
//! Saved connections live in `<userData>/db-connections.json`, connection strings sealed.

use std::{
    collections::HashMap,
    fmt::{self, Write as _},
    path::Path,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use parking_lot::Mutex;
use regex::Regex;
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStderr, ChildStdout},
    sync::oneshot,
};

use crate::{
    core::{Res, command, user_data},
    secure,
};

const BRIDGE_PS1: &str = include_str!("mssql_bridge.ps1");

fn re(s: &str) -> Regex {
    Regex::new(s).expect("valid regex")
}

/* ======================= the bridge process (PowerShell + SqlClient) ======================= */

type Reply = Result<Value, String>;

#[derive(Default)]
struct Pending {
    /// set once the process is gone: why it stopped
    dead: Option<String>,
    waiting: HashMap<u64, oneshot::Sender<Reply>>,
}

struct Bridge {
    child: Mutex<Option<Child>>,
    stdin: tokio::sync::Mutex<ChildStdin>,
    pending: Mutex<Pending>,
}

impl Bridge {
    fn alive(&self) -> bool {
        self.pending.lock().dead.is_none()
    }

    fn kill(&self) {
        if let Some(c) = self.child.lock().as_mut() {
            let _ = c.start_kill();
        }
    }
}

/// The running bridge, if any.
static CURRENT: Mutex<Option<Arc<Bridge>>> = Mutex::new(None);
/// One start at a time (held until the new process says it is ready).
static STARTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static SEQ: AtomicU64 = AtomicU64::new(0);

/// Keep stdin plain ASCII: the bridge reads it with the console code page.
fn ascii(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if (c as u32) < 0x7f {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for u in c.encode_utf16(&mut units) {
                let _ = write!(out, "\\u{u:04x}");
            }
        }
    }
    out
}

async fn start() -> Result<Arc<Bridge>> {
    let _starting = STARTING.lock().await;
    if let Some(b) = CURRENT.lock().clone()
        && b.alive()
    {
        return Ok(b);
    }
    let dir = user_data().join("mssql");
    std::fs::create_dir_all(&dir)?;
    let script = dir.join("bridge.ps1");
    std::fs::write(&script, BRIDGE_PS1)?;
    let mut child = command("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("could not start PowerShell")?;
    let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
        bail!("PowerShell has no stdio");
    };
    let bridge = Arc::new(Bridge { child: Mutex::new(Some(child)), stdin: tokio::sync::Mutex::new(stdin), pending: Mutex::default() });
    *CURRENT.lock() = Some(bridge.clone());
    let (ready_tx, ready_rx) = oneshot::channel();
    tauri::async_runtime::spawn(read_loop(bridge.clone(), stdout, stderr, ready_tx));
    match ready_rx.await {
        Ok(Ok(())) => Ok(bridge),
        Ok(Err(e)) => Err(anyhow!(e)),
        Err(_) => bail!("Query stopped."),
    }
}

/// Routes replies to their callers; when the process ends, fails everything still waiting.
async fn read_loop(bridge: Arc<Bridge>, stdout: ChildStdout, mut stderr: ChildStderr, ready_tx: oneshot::Sender<Result<(), String>>) {
    let mut ready = Some(ready_tx);
    let err_buf = Arc::new(Mutex::new(String::new()));
    let err_task = tauri::async_runtime::spawn({
        let err_buf = err_buf.clone();
        async move {
            let mut chunk = [0u8; 4096];
            while let Ok(n) = stderr.read(&mut chunk).await
                && n > 0
            {
                let mut b = err_buf.lock();
                b.push_str(&String::from_utf8_lossy(&chunk[..n]));
                let len = b.chars().count();
                if len > 4000 {
                    *b = b.chars().skip(len - 4000).collect();
                }
            }
        }
    });

    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(line) else { continue };
        if truthy(msg.get("ready")) {
            if let Some(tx) = ready.take() {
                let _ = tx.send(Ok(()));
            }
            continue;
        }
        if let Some(id) = msg.get("id").and_then(Value::as_u64)
            && let Some(tx) = bridge.pending.lock().waiting.remove(&id)
        {
            let _ = tx.send(Ok(msg));
        }
    }

    // the process is gone (or going): give stderr a moment to arrive, then fail what waits
    let _ = tokio::time::timeout(Duration::from_millis(500), err_task).await;
    let stderr = err_buf.lock().trim().to_owned();
    let err = match stderr.lines().next() {
        Some(first) if !stderr.is_empty() => format!("SQL runner stopped: {}", first.trim()),
        _ => "Query stopped.".to_owned(),
    };
    {
        let mut p = bridge.pending.lock();
        p.dead = Some(err.clone());
        for (_, tx) in p.waiting.drain() {
            let _ = tx.send(Err(err.clone()));
        }
    }
    if let Some(tx) = ready.take() {
        let _ = tx.send(Err(err));
    }
    {
        let mut cur = CURRENT.lock();
        if cur.as_ref().is_some_and(|b| Arc::ptr_eq(b, &bridge)) {
            *cur = None;
        }
    }
    let child = bridge.child.lock().take();
    if let Some(mut c) = child {
        let _ = c.start_kill();
        let _ = c.wait().await;
    }
}

async fn bridge(mut req: Value) -> Result<Value> {
    let b = start().await?;
    let id = SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    req["id"] = id.into();
    let (tx, rx) = oneshot::channel();
    {
        let mut p = b.pending.lock();
        if let Some(e) = &p.dead {
            bail!("{e}");
        }
        p.waiting.insert(id, tx);
    }
    let line = ascii(&serde_json::to_string(&req)?) + "\n";
    let wrote = {
        let mut stdin = b.stdin.lock().await;
        match stdin.write_all(line.as_bytes()).await {
            Ok(()) => stdin.flush().await,
            Err(e) => Err(e),
        }
    };
    let reply = match wrote {
        Ok(()) => rx.await,
        // the process is probably exiting: its own error says more, if it comes soon
        Err(e) => match tokio::time::timeout(Duration::from_secs(2), rx).await {
            Ok(r) => r,
            Err(_) => {
                b.pending.lock().waiting.remove(&id);
                return Err(anyhow!(e).context("SQL runner is not responding"));
            }
        },
    };
    match reply {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(anyhow!(e)),
        Err(_) => bail!("Query stopped."),
    }
}

/// Stop whatever is running (the runner restarts on the next query). Also called on app exit.
pub fn cancel_all() {
    let b = CURRENT.lock().take();
    if let Some(b) = b {
        b.kill();
    }
}

/* ======================= shapes (src/shared/types.ts) ======================= */

/// A saved connection as the UI sees it — no password.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DbConnection {
    pub id: String,
    pub name: String,
    pub server: String,
    pub database: String,
    /// "sql" | "windows"
    pub auth: String,
    pub user: String,
    pub claude_read_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

#[derive(Serialize)]
pub struct DbConnectionWithConn {
    #[serde(flatten)]
    c: DbConnection,
    conn: String,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DbConnectionInput {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub conn: String,
    #[serde(default)]
    pub claude_read_only: bool,
    #[serde(default)]
    pub color: Option<String>,
}

/// A connection string found in the project or in SSMS, ready to add.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DbFound {
    pub name: String,
    pub conn: String,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_password: Option<bool>,
}

#[derive(Serialize, Debug)]
pub struct DbObject {
    pub schema: String,
    pub name: String,
    /// U table, V view, P procedure, FN / IF / TF function
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Serialize, Debug)]
pub struct DbColumn {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub nullable: bool,
    pub identity: bool,
    pub pk: bool,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DbColumnHead {
    #[serde(default)]
    pub name: String,
    #[serde(rename = "type", default)]
    pub kind: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DbResultSet {
    #[serde(default)]
    pub columns: Vec<DbColumnHead>,
    #[serde(default)]
    pub rows: Vec<Vec<Value>>,
    #[serde(default)]
    pub truncated: bool,
}

fn minus_one() -> i64 {
    -1
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DbQueryResult {
    #[serde(default)]
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub results: Vec<DbResultSet>,
    #[serde(default)]
    pub messages: Vec<String>,
    /// rows inserted / updated / deleted; -1 for none
    #[serde(default = "minus_one")]
    pub affected: i64,
    #[serde(default)]
    pub ms: u64,
}

#[derive(Serialize, Debug)]
pub struct TestResult {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct BuildConnFields {
    pub server: String,
    #[serde(default)]
    pub database: Option<String>,
    #[serde(default)]
    pub windows_auth: bool,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub trust_cert: Option<bool>,
    #[serde(default)]
    pub encrypt: Option<bool>,
}

/* ======================= saved connections ======================= */

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct Stored {
    id: String,
    name: String,
    /// connection string, sealed (base64) when `enc`
    conn: String,
    #[serde(default)]
    enc: bool,
    #[serde(default)]
    claude_read_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    color: Option<String>,
}

/// Held around every read-modify-write of the store file.
static STORE: Mutex<()> = Mutex::new(());

fn store_file() -> std::path::PathBuf {
    user_data().join("db-connections.json")
}

fn load() -> Vec<Stored> {
    std::fs::read_to_string(store_file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn persist(list: &[Stored]) -> Result<()> {
    let file = store_file();
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(list)?)?;
    std::fs::rename(&tmp, &file)?;
    Ok(())
}

fn seal(conn: &str) -> (String, bool) {
    match secure::seal_str(conn) {
        Ok(s) => (s, true),
        Err(_) => (conn.to_owned(), false),
    }
}

fn unseal(s: &Stored) -> Result<String> {
    if s.enc { secure::open_str(&s.conn) } else { Ok(s.conn.clone()) }
}

/// Connection string → key/value pairs (keys lower-cased), honouring "quoted" and 'quoted' values.
pub fn parse_conn(conn: &str) -> HashMap<String, String> {
    let c: Vec<char> = conn.chars().collect();
    let find = |ch: char, from: usize| c.iter().skip(from).position(|&x| x == ch).map(|p| p + from);
    let mut out = HashMap::new();
    let mut i = 0;
    while i < c.len() {
        let Some(eq) = find('=', i) else { break };
        let key = c[i..eq].iter().collect::<String>().trim().to_lowercase();
        let mut j = eq + 1;
        while c.get(j) == Some(&' ') {
            j += 1;
        }
        let mut val = String::new();
        if let Some(&q) = c.get(j)
            && (q == '"' || q == '\'')
        {
            j += 1;
            while j < c.len() {
                if c[j] == q {
                    if c.get(j + 1) == Some(&q) {
                        val.push(q);
                        j += 2;
                        continue;
                    }
                    j += 1;
                    break;
                }
                val.push(c[j]);
                j += 1;
            }
            i = find(';', j).map_or(c.len(), |s| s + 1);
        } else {
            let semi = find(';', j);
            val = c[j.min(c.len())..semi.unwrap_or(c.len())].iter().collect::<String>().trim().to_owned();
            i = semi.map_or(c.len(), |s| s + 1);
        }
        if !key.is_empty() {
            out.insert(key, val);
        }
    }
    out
}

fn pick(kv: &HashMap<String, String>, keys: &[&str]) -> String {
    keys.iter().find_map(|k| kv.get(*k)).cloned().unwrap_or_default()
}

/// A value safe inside a connection string.
fn quote_val(v: &str) -> String {
    static NEEDS: LazyLock<Regex> = LazyLock::new(|| re(r#"[;'"=]|^\s|\s$"#));
    if NEEDS.is_match(v) { format!("\"{}\"", v.replace('"', "\"\"")) } else { v.to_owned() }
}

pub fn build_conn(f: &BuildConnFields) -> String {
    let mut parts = vec![format!("Server={}", quote_val(&f.server))];
    if let Some(db) = f.database.as_deref()
        && !db.is_empty()
    {
        parts.push(format!("Database={}", quote_val(db)));
    }
    if f.windows_auth {
        parts.push("Integrated Security=True".into());
    } else {
        parts.push(format!("User ID={}", quote_val(f.user.as_deref().unwrap_or(""))));
        parts.push(format!("Password={}", quote_val(f.password.as_deref().unwrap_or(""))));
    }
    if f.encrypt == Some(true) {
        parts.push("Encrypt=True".into());
    }
    if f.trust_cert != Some(false) {
        parts.push("TrustServerCertificate=True".into());
    }
    parts.join(";")
}

fn describe(s: &Stored) -> DbConnection {
    // unreadable (sealed on another account) → just the name
    let kv = unseal(s).map(|c| parse_conn(&c)).unwrap_or_default();
    static INTEGRATED: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(true|yes|sspi)$"));
    let integrated = INTEGRATED.is_match(&pick(&kv, &["integrated security", "trusted_connection"]));
    DbConnection {
        id: s.id.clone(),
        name: s.name.clone(),
        server: pick(&kv, &["server", "data source", "address", "addr", "network address"]),
        database: pick(&kv, &["database", "initial catalog"]),
        auth: if integrated { "windows" } else { "sql" }.into(),
        user: if integrated { String::new() } else { pick(&kv, &["user id", "uid", "user"]) },
        claude_read_only: s.claude_read_only,
        color: s.color.clone(),
    }
}

pub fn list_connections() -> Vec<DbConnection> {
    load().iter().map(describe).collect()
}

fn get_connection(id: &str) -> Result<Option<DbConnectionWithConn>> {
    let Some(s) = load().into_iter().find(|c| c.id == id) else { return Ok(None) };
    Ok(Some(DbConnectionWithConn { c: describe(&s), conn: unseal(&s)? }))
}

fn save_connection(input: DbConnectionInput) -> Result<Vec<DbConnection>> {
    let _g = STORE.lock();
    let mut list = load();
    let i = input.id.as_deref().and_then(|id| list.iter().position(|c| c.id == id));
    let (conn, enc) = seal(&input.conn);
    let name = input.name.trim();
    let item = Stored {
        id: match i {
            Some(i) => list[i].id.clone(),
            None => uuid::Uuid::new_v4().simple().to_string()[..12].to_owned(),
        },
        name: if name.is_empty() { "SQL Server".into() } else { name.to_owned() },
        conn,
        enc,
        claude_read_only: input.claude_read_only,
        color: input.color,
    };
    match i {
        Some(i) => list[i] = item,
        None => list.push(item),
    }
    persist(&list)?;
    Ok(list.iter().map(describe).collect())
}

fn save_connections(inputs: Vec<DbConnectionInput>) -> Result<Vec<DbConnection>> {
    let mut out = list_connections();
    for c in inputs {
        out = save_connection(c)?;
    }
    Ok(out)
}

fn remove_connection(id: &str) -> Result<Vec<DbConnection>> {
    let _g = STORE.lock();
    let list: Vec<Stored> = load().into_iter().filter(|c| c.id != id).collect();
    persist(&list)?;
    Ok(list.iter().map(describe).collect())
}

/// By id or by name (case-insensitive) — Claude refers to connections by name.
fn find_stored(reference: &str) -> Result<Stored> {
    let list = load();
    let wanted = reference.trim().to_lowercase();
    let found = list.iter().find(|c| c.id == reference).or_else(|| list.iter().find(|c| c.name.to_lowercase() == wanted));
    match found {
        Some(s) => Ok(s.clone()),
        None => {
            let names = list.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ");
            bail!("No saved connection \"{reference}\". Saved: {}", if names.is_empty() { "(none)" } else { &names })
        }
    }
}

/* ======================= queries ======================= */

struct Batch {
    text: String,
    /// editor line the batch starts on (1-based)
    line: usize,
}

/// T-SQL batches split on GO lines (GO isn't SQL — SSMS / sqlcmd split on it, so do we).
fn batches(sql: &str) -> Vec<Batch> {
    static GO: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^\s*go\s*(--.*)?$"));
    let mut out = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut start_line = 1;
    for (i, l) in sql.split('\n').enumerate() {
        let l = l.strip_suffix('\r').unwrap_or(l);
        if GO.is_match(l) {
            let text = cur.join("\n");
            if !text.trim().is_empty() {
                out.push(Batch { text, line: start_line });
            }
            cur.clear();
            start_line = i + 2;
        } else {
            cur.push(l);
        }
    }
    let text = cur.join("\n");
    if !text.trim().is_empty() {
        out.push(Batch { text, line: start_line });
    }
    out
}

fn bracket(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

#[derive(Default)]
struct RunOpts<'a> {
    database: Option<&'a str>,
    max_rows: Option<u32>,
    timeout: Option<u32>,
    read_only: bool,
}

async fn run_on(conn: &str, sql: &str, o: RunOpts<'_>) -> Result<DbQueryResult> {
    static LINE: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^Line (\d+):"));
    let t0 = Instant::now();
    let database = o.database.filter(|d| !d.is_empty());
    let mut result = DbQueryResult { ok: true, error: None, results: vec![], messages: vec![], affected: -1, ms: 0 };
    for b in batches(sql) {
        // USE on the same line, so the server's line numbers still match the editor's
        let text = match database {
            Some(d) => format!("USE {}; {}", bracket(d), b.text),
            None => b.text,
        };
        let reply = bridge(json!({
            "conn": conn,
            "sql": text,
            "maxRows": o.max_rows.unwrap_or(1000),
            "timeout": o.timeout.unwrap_or(120),
            "readOnly": o.read_only,
        }))
        .await?;
        let r: DbQueryResult = serde_json::from_value(reply).context("unexpected reply from the SQL runner")?;
        result.results.extend(r.results);
        // (our own USE says "Changed database context to …" — not news to the user)
        result.messages.extend(r.messages.into_iter().filter(|m| !(database.is_some() && m.starts_with("Changed database context to "))));
        if r.affected >= 0 {
            result.affected = result.affected.max(0) + r.affected;
        }
        if !r.ok {
            result.ok = false;
            // server line numbers are per batch: shift them to the editor's
            let err = r.error.unwrap_or_else(|| "Query failed".into());
            let shifted = LINE.replace_all(&err, |c: &regex::Captures| {
                let n: usize = c[1].parse().unwrap_or(0);
                format!("Line {}:", n + b.line - 1)
            });
            result.error = Some(shifted.into_owned());
            break;
        }
    }
    result.ms = t0.elapsed().as_millis() as u64;
    Ok(result)
}

async fn query(id: &str, sql: &str, database: Option<&str>, max_rows: Option<u32>) -> Result<DbQueryResult> {
    let conn = unseal(&find_stored(id)?)?;
    run_on(&conn, sql, RunOpts { database, max_rows, ..Default::default() }).await
}

/// `String(v)` as JavaScript would write a cell.
fn js_str(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// JavaScript truthiness of a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// `Number(v)` as JavaScript would convert it.
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => match s.trim() {
            "" => 0.0,
            t => t.parse().unwrap_or(f64::NAN),
        },
        Some(_) => f64::NAN,
    }
}

fn first_rows(r: &DbQueryResult) -> &[Vec<Value>] {
    r.results.first().map(|s| s.rows.as_slice()).unwrap_or_default()
}

fn ok_or_err(r: DbQueryResult) -> Result<DbQueryResult> {
    if r.ok { Ok(r) } else { Err(anyhow!(r.error.unwrap_or_else(|| "Query failed".into()))) }
}

async fn test_connection(conn: &str) -> Result<TestResult> {
    let r = run_on(
        conn,
        "SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(64)) + ' ' + CAST(SERVERPROPERTY('Edition') AS nvarchar(128)), DB_NAME()",
        RunOpts { timeout: Some(20), ..Default::default() },
    )
    .await?;
    if !r.ok {
        return Ok(TestResult { ok: false, error: r.error, version: None });
    }
    let row = first_rows(&r).first().map(Vec::as_slice).unwrap_or_default();
    Ok(TestResult { ok: true, error: None, version: Some(format!("SQL Server {} · database {}", js_str(row.first()), js_str(row.get(1)))) })
}

async fn list_databases(id: &str) -> Result<Vec<String>> {
    let r = ok_or_err(
        query(id, "SELECT name FROM sys.databases WHERE HAS_DBACCESS(name) = 1 ORDER BY CASE WHEN database_id <= 4 THEN 1 ELSE 0 END, name", None, Some(5000)).await?,
    )?;
    Ok(first_rows(&r).iter().map(|x| js_str(x.first())).collect())
}

async fn list_objects(id: &str, database: &str) -> Result<Vec<DbObject>> {
    let r = ok_or_err(
        query(
            id,
            "SELECT s.name, o.name, o.type FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id
     WHERE o.type IN ('U','V','P','FN','IF','TF') AND o.is_ms_shipped = 0 ORDER BY o.type, s.name, o.name",
            Some(database),
            Some(20000),
        )
        .await?,
    )?;
    Ok(first_rows(&r)
        .iter()
        .map(|x| DbObject { schema: js_str(x.first()), name: js_str(x.get(1)), kind: js_str(x.get(2)).trim().to_owned() })
        .collect())
}

fn columns_sql(schema: &str, table: &str) -> String {
    format!(
        "
SELECT c.name, TYPE_NAME(c.user_type_id)
  + CASE WHEN TYPE_NAME(c.user_type_id) IN ('varchar','char','varbinary','binary') THEN '(' + CASE WHEN c.max_length = -1 THEN 'max' ELSE CAST(c.max_length AS varchar) END + ')'
         WHEN TYPE_NAME(c.user_type_id) IN ('nvarchar','nchar') THEN '(' + CASE WHEN c.max_length = -1 THEN 'max' ELSE CAST(c.max_length / 2 AS varchar) END + ')'
         WHEN TYPE_NAME(c.user_type_id) IN ('decimal','numeric') THEN '(' + CAST(c.precision AS varchar) + ',' + CAST(c.scale AS varchar) + ')'
         ELSE '' END,
  c.is_nullable, c.is_identity,
  CASE WHEN EXISTS (SELECT 1 FROM sys.index_columns ic JOIN sys.indexes i ON i.object_id = ic.object_id AND i.index_id = ic.index_id
                    WHERE i.is_primary_key = 1 AND ic.object_id = c.object_id AND ic.column_id = c.column_id) THEN 1 ELSE 0 END,
  (SELECT OBJECT_SCHEMA_NAME(fk.referenced_object_id) + '.' + OBJECT_NAME(fk.referenced_object_id) + '.' + COL_NAME(fk.referenced_object_id, fk.referenced_column_id)
     FROM sys.foreign_key_columns fk WHERE fk.parent_object_id = c.object_id AND fk.parent_column_id = c.column_id)
FROM sys.columns c WHERE c.object_id = OBJECT_ID(N'{}.{}')
ORDER BY c.column_id",
        bracket(schema).replace('\'', "''"),
        bracket(table).replace('\'', "''")
    )
}

async fn list_columns(id: &str, database: &str, schema: &str, table: &str) -> Result<Vec<DbColumn>> {
    let r = ok_or_err(query(id, &columns_sql(schema, table), Some(database), Some(5000)).await?)?;
    Ok(first_rows(&r)
        .iter()
        .map(|x| DbColumn {
            name: js_str(x.first()),
            kind: js_str(x.get(1)),
            nullable: truthy(x.get(2)),
            identity: truthy(x.get(3)),
            pk: truthy(x.get(4)),
            reference: truthy(x.get(5)).then(|| js_str(x.get(5))),
        })
        .collect())
}

/* ======================= finding existing connections ======================= */

fn looks_sql(s: &str) -> bool {
    static SERVER: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)(^|;)\s*(server|data source|address|addr)\s*="));
    static DB: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)(initial catalog|database|integrated security|trusted_connection|user id|uid)\s*="));
    static OTHER: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)(^|;)\s*(host|port)\s*="));
    SERVER.is_match(s) && DB.is_match(s) && !OTHER.is_match(s)
}

/// Drops `/* … */` and `// …` comments, except `//` right after `:`, `"` or a word character
/// (URLs and the like) — the old regex's lookbehind, which `regex` has no syntax for.
fn strip_json_comments(text: &str) -> String {
    static COMMENT: LazyLock<Regex> = LazyLock::new(|| re(r"(?mR)/\*[\s\S]*?\*/|//.*$"));
    let mut out = String::with_capacity(text.len());
    let (mut last, mut pos) = (0, 0);
    while let Some(m) = COMMENT.find_at(text, pos) {
        if m.as_str().starts_with("//")
            && text[..m.start()].chars().next_back().is_some_and(|c| c == ':' || c == '"' || c == '_' || c.is_ascii_alphanumeric())
        {
            pos = m.start() + 1;
            continue;
        }
        out.push_str(&text[last..m.start()]);
        last = m.end();
        pos = m.end();
    }
    out.push_str(&text[last..]);
    out
}

/// Every string in a JSON document with the key (or array index) it sits under, in document order.
struct JsonStrings<'a> {
    key: String,
    out: &'a mut Vec<(String, String)>,
}

impl<'de> DeserializeSeed<'de> for JsonStrings<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for JsonStrings<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON value")
    }
    fn visit_str<E>(self, v: &str) -> Result<(), E> {
        self.out.push((self.key, v.to_owned()));
        Ok(())
    }
    fn visit_bool<E>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let mut i = 0usize;
        while seq.next_element_seed(JsonStrings { key: i.to_string(), out: &mut *self.out })?.is_some() {
            i += 1;
        }
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(k) = map.next_key::<String>()? {
            map.next_value_seed(JsonStrings { key: k, out: &mut *self.out })?;
        }
        Ok(())
    }
}

fn json_strings(text: &str) -> serde_json::Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut de = serde_json::Deserializer::from_str(text);
    JsonStrings { key: "connection".into(), out: &mut out }.deserialize(&mut de)?;
    de.end()?;
    Ok(out)
}

/// SQL Server connection strings in a project: appsettings*.json, *.config, .env files.
fn scan_project_sync(folder: &Path) -> Vec<DbFound> {
    let mut out = Vec::new();
    if !folder.as_os_str().is_empty() && folder.exists() {
        walk(folder, folder, 0, &mut out);
    }
    out
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<DbFound>) {
    const SKIP: [&str; 10] = ["node_modules", ".git", "bin", "obj", "dist", "out", ".vs", ".idea", "packages", "wwwroot"];
    static JSON_NAME: LazyLock<Regex> = LazyLock::new(|| re(r"^(appsettings.*|connectionstrings|secrets|launchsettings)\.json$"));
    static ENV_NAME: LazyLock<Regex> = LazyLock::new(|| re(r"^\.env(\..*)?$"));
    static JSON_PAIR: LazyLock<Regex> = LazyLock::new(|| re(r#""([^"]+)"\s*:\s*"((?:[^"\\]|\\.)*)""#));
    static UNESCAPE: LazyLock<Regex> = LazyLock::new(|| re(r"\\(.)"));
    static TRAILING_COMMA: LazyLock<Regex> = LazyLock::new(|| re(r",(\s*[}\]])"));
    static ADD: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)<add\b[^>]*>"));
    static XML_NAME: LazyLock<Regex> = LazyLock::new(|| re(r#"(?i)name\s*=\s*"([^"]*)""#));
    static XML_CS: LazyLock<Regex> = LazyLock::new(|| re(r#"(?i)connectionString\s*=\s*"([^"]*)""#));
    static ENV_LINE: LazyLock<Regex> = LazyLock::new(|| re(r"(?mR)^\s*([A-Za-z0-9_.]+)\s*=\s*(.+)$"));

    if depth > 5 || out.len() > 200 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        let n = name.to_lowercase();
        if ft.is_dir() {
            if !SKIP.contains(&n.as_str()) && !name.starts_with('.') {
                walk(root, &p, depth + 1, out);
            }
            continue;
        }
        let json = JSON_NAME.is_match(&n);
        let xml = n.ends_with(".config") && !n.ends_with("packages.config");
        let env = ENV_NAME.is_match(&n);
        if !json && !xml && !env {
            continue;
        }
        let Ok(bytes) = std::fs::read(&p) else { continue };
        let text = String::from_utf8_lossy(&bytes);
        let rel = p
            .strip_prefix(root)
            .ok()
            .map(|r| r.display().to_string())
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| name.clone());
        let proj = p.parent().and_then(Path::file_name).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let mut add = |key: &str, conn: String| {
            if looks_sql(&conn) && !out.iter().any(|f| f.conn == conn) {
                out.push(DbFound { name: format!("{proj} · {key}"), conn, file: rel.clone(), needs_password: None });
            }
        };
        if json {
            let body = text.strip_prefix('\u{feff}').unwrap_or(&text);
            let cleaned = TRAILING_COMMA.replace_all(&strip_json_comments(body), "$1").into_owned();
            match json_strings(&cleaned) {
                Ok(pairs) => {
                    for (k, v) in pairs {
                        add(&k, v);
                    }
                }
                Err(_) => {
                    for m in JSON_PAIR.captures_iter(&text) {
                        add(&m[1], UNESCAPE.replace_all(&m[2], "$1").into_owned());
                    }
                }
            }
        } else if xml {
            for m in ADD.find_iter(&text) {
                let tag = m.as_str();
                let name = XML_NAME.captures(tag).map_or("connection".to_owned(), |c| c[1].to_owned());
                if let Some(cs) = XML_CS.captures(tag) {
                    let cs = cs[1].replace("&quot;", "\"").replace("&apos;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&");
                    add(&name, cs);
                }
            }
        } else {
            for m in ENV_LINE.captures_iter(&text) {
                let v = m[2].trim();
                // drop one pair of matching quotes around the value
                let unquoted = match v.chars().next() {
                    Some(q @ ('"' | '\'')) if v.len() >= 2 && v.ends_with(q) => &v[1..v.len() - 1],
                    _ => v,
                };
                add(&m[1], unquoted.to_owned());
            }
        }
    }
}

/// Connections saved in SQL Server Management Studio (passwords from Windows Credential Manager).
async fn scan_ssms() -> Result<Vec<DbFound>> {
    let r = bridge(json!({ "op": "ssms" })).await?;
    let mut out = Vec::new();
    for c in r.get("connections").and_then(Value::as_array).into_iter().flatten() {
        let auth = match c.get("auth") {
            None | Some(Value::Null) => String::new(),
            v => js_str(v),
        };
        // 0 = Windows, 1 = SQL login; Entra ID kinds need a newer driver than Windows ships
        if auth != "0" && auth != "1" {
            continue;
        }
        let server = js_str(c.get("server"));
        let user = match c.get("user") {
            None | Some(Value::Null) => String::new(),
            v => js_str(v),
        };
        let password = c.get("password").and_then(Value::as_str).filter(|p| !p.is_empty());
        let database = match c.get("database") {
            None | Some(Value::Null) => String::new(),
            v => js_str(v),
        };
        out.push(DbFound {
            name: if auth == "0" { server.clone() } else { format!("{server} ({user})") },
            conn: build_conn(&BuildConnFields {
                server: server.clone(),
                database: Some(database),
                windows_auth: auth == "0",
                user: Some(user.clone()),
                password: Some(password.unwrap_or_default().to_owned()),
                trust_cert: Some(c.get("trustCert") != Some(&Value::Bool(false))),
                encrypt: Some(c.get("encrypt") == Some(&Value::Bool(true))),
            }),
            file: match c.get("source") {
                None | Some(Value::Null) => "SSMS".into(),
                v => js_str(v),
            },
            needs_password: Some(auth == "1" && password.is_none()),
        });
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

/* ======================= commands ======================= */

#[tauri::command]
pub async fn db_list() -> Res<Vec<DbConnection>> {
    Ok(list_connections())
}

#[tauri::command]
pub async fn db_get(id: String) -> Res<Option<DbConnectionWithConn>> {
    Ok(get_connection(&id)?)
}

#[tauri::command]
pub async fn db_save(c: DbConnectionInput) -> Res<Vec<DbConnection>> {
    Ok(save_connection(c)?)
}

#[tauri::command]
pub async fn db_save_many(list: Vec<DbConnectionInput>) -> Res<Vec<DbConnection>> {
    Ok(save_connections(list)?)
}

#[tauri::command]
pub async fn db_remove(id: String) -> Res<Vec<DbConnection>> {
    Ok(remove_connection(&id)?)
}

#[tauri::command]
pub async fn db_test(conn: String) -> Res<TestResult> {
    Ok(test_connection(&conn).await?)
}

#[tauri::command]
pub async fn db_build_conn(f: BuildConnFields) -> Res<String> {
    Ok(build_conn(&f))
}

#[tauri::command]
pub async fn db_parse_conn(conn: String) -> Res<HashMap<String, String>> {
    Ok(parse_conn(&conn))
}

#[tauri::command]
pub async fn db_query(id: String, sql: String, database: Option<String>, max_rows: Option<u32>) -> Res<DbQueryResult> {
    Ok(query(&id, &sql, database.as_deref(), max_rows).await?)
}

#[tauri::command]
pub async fn db_cancel() -> Res<()> {
    cancel_all();
    Ok(())
}

#[tauri::command]
pub async fn db_databases(id: String) -> Res<Vec<String>> {
    Ok(list_databases(&id).await?)
}

#[tauri::command]
pub async fn db_objects(id: String, database: String) -> Res<Vec<DbObject>> {
    Ok(list_objects(&id, &database).await?)
}

#[tauri::command]
pub async fn db_columns(id: String, database: String, schema: String, table: String) -> Res<Vec<DbColumn>> {
    Ok(list_columns(&id, &database, &schema, &table).await?)
}

#[tauri::command]
pub async fn db_scan_project(folder: String) -> Res<Vec<DbFound>> {
    Ok(tokio::task::spawn_blocking(move || scan_project_sync(Path::new(&folder))).await?)
}

#[tauri::command]
pub async fn db_scan_ssms() -> Res<Vec<DbFound>> {
    Ok(scan_ssms().await?)
}

/* ======================= for Claude (MCP tools) ======================= */

const CELL_MAX: usize = 300;

/// The first `n` characters of `s`, if it is longer than that.
fn cut(s: &str, n: usize) -> Option<&str> {
    s.char_indices().nth(n).map(|(i, _)| &s[..i])
}

fn as_table(r: &DbResultSet) -> String {
    let esc = |v: Option<&Value>| -> String {
        let s = match v {
            None | Some(Value::Null) => return "NULL".into(),
            Some(Value::String(s)) => s.clone(),
            Some(v) => v.to_string(),
        };
        let s = match cut(&s, CELL_MAX) {
            Some(head) => format!("{head}…"),
            None => s,
        };
        s.replace('|', "\\|").replace("\r\n", "⏎").replace('\n', "⏎")
    };
    let head = format!(
        "| {} |",
        r.columns
            .iter()
            .map(|c| esc(Some(&Value::String(if c.name.is_empty() { "(no name)".into() } else { c.name.clone() }))))
            .collect::<Vec<_>>()
            .join(" | ")
    );
    let sep = format!("|{}|", vec!["---"; r.columns.len()].join("|"));
    let mut lines = vec![head, sep];
    lines.extend(r.rows.iter().map(|row| format!("| {} |", row.iter().map(|v| esc(Some(v))).collect::<Vec<_>>().join(" | "))));
    let n = r.rows.len();
    format!(
        "{}\n({n} row{}{})",
        lines.join("\n"),
        if n == 1 { "" } else { "s" },
        if r.truncated { ", more rows not shown — narrow the query or raise max_rows" } else { "" }
    )
}

pub const INSTRUCTIONS: &str = "Query the SQL Server databases the user connected in VIBE CODE's Database tab. Start with mssql_connections, explore with mssql_schema, then mssql_query. Prefer small, targeted SELECTs (TOP n).";

/// The `mssql_*` tools, as MCP `tools/list` entries.
pub fn tools() -> Value {
    json!([
        {
            "name": "mssql_connections",
            "description": "List the SQL Server connections the user saved in VIBE CODE (name, server, default database, auth, whether you may only read).",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "mssql_schema",
            "description": "Explore a database. Without `table`: list databases (if no `database`), or tables/views/procedures in `database`. With `table` (\"schema.name\" or \"name\"): its columns with types, nullability, primary key and foreign keys.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "Connection name from mssql_connections" },
                    "database": { "type": "string" },
                    "table": { "type": "string" }
                },
                "required": ["connection"]
            }
        },
        {
            "name": "mssql_query",
            "description": "Run T-SQL on a saved SQL Server connection and get the rows back as a table. Use T-SQL syntax (TOP, not LIMIT). GO separates batches. Connections marked read-only run inside a transaction that is always rolled back.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "connection": { "type": "string", "description": "Connection name from mssql_connections" },
                    "sql": { "type": "string" },
                    "database": { "type": "string", "description": "Database to run in (default: the connection's own)" },
                    "max_rows": { "type": "number", "description": "Rows to return per result set (default 100, max 2000)" }
                },
                "required": ["connection", "sql"]
            }
        }
    ])
}

/// Runs one of the tools above; returns the text Claude sees.
pub async fn call_tool(name: &str, args: Value) -> Result<String> {
    if name == "mssql_connections" {
        let list = list_connections();
        if list.is_empty() {
            return Ok("No connections saved yet. Ask the user to add one in the Database tab of VIBE CODE.".into());
        }
        return Ok(list
            .iter()
            .map(|c| {
                format!(
                    "- {}: {}{} ({}){}",
                    c.name,
                    c.server,
                    if c.database.is_empty() { String::new() } else { format!(" / {}", c.database) },
                    if c.auth == "windows" { "Windows auth".to_owned() } else { format!("login {}", c.user) },
                    if c.claude_read_only { " [read-only]" } else { "" }
                )
            })
            .collect::<Vec<_>>()
            .join("\n"));
    }
    let connection = match args.get("connection") {
        None | Some(Value::Null) => String::new(),
        v => js_str(v),
    };
    let s = find_stored(&connection)?;
    let conn = unseal(&s)?;
    let database = truthy(args.get("database")).then(|| js_str(args.get("database")));
    let database = database.as_deref();
    match name {
        "mssql_schema" => {
            if truthy(args.get("table")) {
                let raw = js_str(args.get("table")).replace(['[', ']'], "");
                let mut parts: Vec<&str> = raw.split('.').collect();
                let table = parts.pop().unwrap_or_default().to_owned();
                let schema = parts.pop().filter(|s| !s.is_empty()).unwrap_or("dbo").to_owned();
                let r = ok_or_err(run_on(&conn, &columns_sql(&schema, &table), RunOpts { database, max_rows: Some(2000), ..Default::default() }).await?)?;
                let rows = first_rows(&r);
                if rows.is_empty() {
                    return Ok(format!("No table or view {schema}.{table}{}.", database.map(|d| format!(" in {d}")).unwrap_or_default()));
                }
                let cols = rows
                    .iter()
                    .map(|x| {
                        format!(
                            "- {} {}{}{}{}{}",
                            js_str(x.first()),
                            js_str(x.get(1)),
                            if truthy(x.get(2)) { " NULL" } else { " NOT NULL" },
                            if truthy(x.get(4)) { " PK" } else { "" },
                            if truthy(x.get(3)) { " IDENTITY" } else { "" },
                            if truthy(x.get(5)) { format!(" → {}", js_str(x.get(5))) } else { String::new() }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                return Ok(format!("{schema}.{table}:\n{cols}"));
            }
            let Some(database) = database else {
                let r = ok_or_err(
                    run_on(&conn, "SELECT name FROM sys.databases WHERE HAS_DBACCESS(name) = 1 ORDER BY name", RunOpts { max_rows: Some(5000), ..Default::default() })
                        .await?,
                )?;
                return Ok(format!("Databases:\n{}", first_rows(&r).iter().map(|x| format!("- {}", js_str(x.first()))).collect::<Vec<_>>().join("\n")));
            };
            let r = ok_or_err(
                run_on(
                    &conn,
                    "SELECT o.type_desc, s.name + '.' + o.name FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id
       WHERE o.type IN ('U','V','P','FN','IF','TF') AND o.is_ms_shipped = 0 ORDER BY o.type, s.name, o.name",
                    RunOpts { database: Some(database), max_rows: Some(20000), ..Default::default() },
                )
                .await?,
            )?;
            let mut groups: Vec<(String, Vec<String>)> = Vec::new();
            for row in first_rows(&r) {
                let (t, n) = (js_str(row.first()), js_str(row.get(1)));
                match groups.iter_mut().find(|(k, _)| *k == t) {
                    Some((_, ns)) => ns.push(n),
                    None => groups.push((t, vec![n])),
                }
            }
            if groups.is_empty() {
                return Ok("No user objects.".into());
            }
            Ok(groups.iter().map(|(t, ns)| format!("{t} ({}):\n{}", ns.len(), ns.join(", "))).collect::<Vec<_>>().join("\n\n"))
        }
        "mssql_query" => {
            static WRITES_TX: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\b(commit|save\s+tran(saction)?)\b"));
            let sql = match args.get("sql") {
                None | Some(Value::Null) => String::new(),
                v => js_str(v),
            };
            if s.claude_read_only && WRITES_TX.is_match(&sql) {
                bail!("This connection is read-only for Claude: COMMIT / SAVE TRANSACTION are not allowed.");
            }
            let n = js_number(args.get("max_rows"));
            let max = if n.is_nan() || n == 0.0 { 100.0 } else { n }.clamp(1.0, 2000.0) as u32;
            let r = run_on(&conn, &sql, RunOpts { database, max_rows: Some(max), read_only: s.claude_read_only, ..Default::default() }).await?;
            let mut parts: Vec<String> = r.results.iter().map(as_table).collect();
            if !r.messages.is_empty() {
                parts.push(format!("Messages:\n{}", r.messages.join("\n")));
            }
            if r.affected >= 0 {
                parts.push(format!("{} row(s) affected{}.", r.affected, if s.claude_read_only { " (rolled back: read-only connection)" } else { "" }));
            }
            if !r.ok {
                parts.push(format!("Error: {}", r.error.as_deref().unwrap_or("undefined")));
            }
            let text = parts.join("\n\n");
            let text = if text.is_empty() { "Done (no results).".to_owned() } else { text };
            Ok(match cut(&text, 80000) {
                Some(head) => format!("{head}\n… (output cut — ask for fewer rows or columns)"),
                None => text,
            })
        }
        _ => bail!("Unknown tool {name}"),
    }
}

// the MCP server awaits `call_tool` from its own tasks
const _: fn() = || {
    fn send<T: Send>(_: T) {}
    send(call_tool("", Value::Null));
};
