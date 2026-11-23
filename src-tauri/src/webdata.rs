//! Bookmarks and history for the Web tab, shared by every tab of every chat.
//! Imported once from all Chrome profiles; visits in the Web tab are added from then on.
//! Kept in `<userData>/web-data.json`.

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        LazyLock,
        atomic::{AtomicU64, Ordering as AtomicOrdering},
    },
    time::Duration,
};

use parking_lot::Mutex;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::{Res, home, now_ms, user_data, write_atomic};

const MAX_HISTORY: usize = 20000;

/// Web tab bookmark: a link, or a folder of them.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct WebBookmark {
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<WebBookmark>>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct WebData {
    /// when bookmarks and history were imported from Chrome (0 = not yet)
    pub imported_at: u64,
    pub bookmarks: Vec<WebBookmark>,
    /// Chrome is installed, so there is something to import
    pub chrome_found: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct WebSuggestion {
    pub url: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bookmark: Option<bool>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct HistoryEntry {
    url: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    visits: i64,
    /// last visit, ms
    #[serde(default)]
    last: i64,
}

#[derive(Default)]
struct State {
    loaded: bool,
    imported_at: u64,
    bookmarks: Vec<WebBookmark>,
    history: HashMap<String, HistoryEntry>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(Mutex::default);
/// Bumped by every `save`; a pending write only goes ahead if no newer one replaced it.
static SAVE_GEN: AtomicU64 = AtomicU64::new(0);

fn file() -> PathBuf {
    user_data().join("web-data.json")
}

fn chrome_root() -> PathBuf {
    if cfg!(target_os = "macos") {
        home().join("Library").join("Application Support").join("Google").join("Chrome")
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(dirs::data_local_dir)
            .unwrap_or_default()
            .join("Google")
            .join("Chrome")
            .join("User Data")
    } else {
        home().join(".config").join("google-chrome")
    }
}

fn is_http(url: &str) -> bool {
    url.get(..5).is_some_and(|s| s.eq_ignore_ascii_case("http:")) || url.get(..6).is_some_and(|s| s.eq_ignore_ascii_case("https:"))
}

fn load(st: &mut State) {
    if st.loaded {
        return;
    }
    st.loaded = true;
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Saved {
        #[serde(default)]
        imported_at: u64,
        #[serde(default)]
        bookmarks: Vec<WebBookmark>,
        #[serde(default)]
        history: Vec<HistoryEntry>,
    }
    let Some(d) = std::fs::read_to_string(file()).ok().and_then(|s| serde_json::from_str::<Saved>(&s).ok()) else { return };
    st.imported_at = d.imported_at;
    st.bookmarks = d.bookmarks;
    st.history.extend(d.history.into_iter().map(|h| (h.url.clone(), h)));
}

/// Writes a second after the last change (visits come in bursts).
fn save() {
    let generation = SAVE_GEN.fetch_add(1, AtomicOrdering::Relaxed) + 1;
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if SAVE_GEN.load(AtomicOrdering::Relaxed) != generation {
            return;
        }
        let json = {
            let st = STATE.lock();
            let mut list: Vec<&HistoryEntry> = st.history.values().collect();
            list.sort_by_key(|h| std::cmp::Reverse(h.last));
            list.truncate(MAX_HISTORY);
            serde_json::json!({ "importedAt": st.imported_at, "bookmarks": st.bookmarks, "history": list }).to_string()
        };
        let _ = write_atomic(file(), json).await;
    });
}

fn get_web_data() -> WebData {
    let mut st = STATE.lock();
    load(&mut st);
    WebData { imported_at: st.imported_at, bookmarks: st.bookmarks.clone(), chrome_found: chrome_root().join("Local State").exists() }
}

/* ---------------- Chrome import ---------------- */

fn to_bookmark(n: &Value) -> Option<WebBookmark> {
    let title = n.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
    match n.get("type").and_then(Value::as_str) {
        Some("url") => {
            let url = n.get("url").and_then(Value::as_str).filter(|u| !u.is_empty())?;
            Some(WebBookmark { title, url: Some(url.to_owned()), children: None })
        }
        Some("folder") => {
            let children = n.get("children").and_then(Value::as_array).into_iter().flatten().filter_map(to_bookmark).collect();
            Some(WebBookmark { title, url: None, children: Some(children) })
        }
        _ => None,
    }
}

/// "Profile 2" before "Profile 10".
fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn chunks(s: &str) -> Vec<(bool, String)> {
        let mut out: Vec<(bool, String)> = Vec::new();
        for c in s.chars() {
            let digit = c.is_ascii_digit();
            match out.last_mut() {
                Some((d, chunk)) if *d == digit => chunk.push(c),
                _ => out.push((digit, c.to_string())),
            }
        }
        out
    }
    let (ca, cb) = (chunks(a), chunks(b));
    for (x, y) in ca.iter().zip(&cb) {
        let ord = match (x, y) {
            ((true, x), (true, y)) => {
                let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                x.len().cmp(&y.len()).then_with(|| x.cmp(y))
            }
            ((_, x), (_, y)) => x.to_lowercase().cmp(&y.to_lowercase()).then_with(|| x.cmp(y)),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    ca.len().cmp(&cb.len())
}

/// Chrome profile folders, Default first.
fn profiles() -> Vec<String> {
    let Some(ls) = std::fs::read_to_string(chrome_root().join("Local State")).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) else {
        return vec!["Default".into()];
    };
    let mut names: Vec<String> = ls.pointer("/profile/info_cache").and_then(Value::as_object).map(|m| m.keys().cloned().collect()).unwrap_or_default();
    names.sort_by(|a, b| match (a.as_str(), b.as_str()) {
        ("Default", "Default") => Ordering::Equal,
        ("Default", _) => Ordering::Less,
        (_, "Default") => Ordering::Greater,
        _ => natural_cmp(a, b),
    });
    names
}

/// Bar items of every profile (deduplicated by URL); "other" and "mobile" bookmarks go in one folder at the end.
fn import_bookmarks(root: &Path, dirs: &[String]) -> Vec<WebBookmark> {
    let mut bar = Vec::new();
    let mut other = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |to: &mut Vec<WebBookmark>, nodes: Option<&Value>| {
        for n in nodes.and_then(Value::as_array).into_iter().flatten() {
            let url = n.get("url").and_then(Value::as_str).filter(|u| !u.is_empty());
            if n.get("type").and_then(Value::as_str) == Some("url") && url.is_none_or(|u| seen.contains(u)) {
                continue;
            }
            if let Some(u) = url {
                seen.insert(u.to_owned());
            }
            if let Some(b) = to_bookmark(n)
                && (b.url.is_some() || b.children.as_ref().is_some_and(|c| !c.is_empty()))
            {
                to.push(b);
            }
        }
    };
    for d in dirs {
        // signed-in profiles keep them in AccountBookmarks, local ones in Bookmarks
        for name in ["AccountBookmarks", "Bookmarks"] {
            let Some(file) = std::fs::read_to_string(root.join(d).join(name)).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) else {
                continue;
            };
            let Some(roots) = file.get("roots") else { continue };
            add(&mut bar, roots.pointer("/bookmark_bar/children"));
            add(&mut other, roots.pointer("/other/children"));
            add(&mut other, roots.pointer("/synced/children"));
        }
    }
    if !other.is_empty() {
        bar.push(WebBookmark { title: "Other bookmarks".into(), url: None, children: Some(other) });
    }
    bar
}

/// History of one profile, read from a copy (Chrome keeps the file open).
fn read_history(src: &Path, tmp: &Path) -> rusqlite::Result<Vec<HistoryEntry>> {
    use rusqlite::{Connection, OpenFlags};
    if std::fs::copy(src, tmp).is_err() {
        return Ok(Vec::new());
    }
    let db = Connection::open_with_flags(tmp, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    // Chrome time is microseconds since 1601: turned into ms since 1970 in SQL
    let mut stmt = db.prepare(&format!(
        "SELECT url, title, visit_count AS visits, last_visit_time / 1000 - 11644473600000 AS last FROM urls WHERE hidden = 0 ORDER BY last_visit_time DESC LIMIT {MAX_HISTORY}"
    ))?;
    let rows = stmt.query_map([], |r| {
        Ok(HistoryEntry {
            url: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
            title: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            visits: r.get::<_, Option<i64>>(2)?.unwrap_or_default(),
            last: r.get::<_, Option<i64>>(3)?.unwrap_or_default(),
        })
    })?;
    rows.collect()
}

fn import_history(root: &Path, dirs: &[String]) -> Vec<Vec<HistoryEntry>> {
    let tmp = std::env::temp_dir().join(format!("vibe-chrome-history-{}.db", now_ms()));
    let mut out = Vec::new();
    for d in dirs {
        // a profile without history, or a copy that failed, just adds nothing
        if let Ok(rows) = read_history(&root.join(d).join("History"), &tmp) {
            out.push(rows);
        }
        let _ = std::fs::remove_file(&tmp);
    }
    out
}

async fn import_chrome() -> anyhow::Result<WebData> {
    load(&mut STATE.lock());
    let (bookmarks, profiles_rows) = tokio::task::spawn_blocking(|| {
        let root = chrome_root();
        let dirs = profiles();
        (import_bookmarks(&root, &dirs), import_history(&root, &dirs))
    })
    .await?;
    {
        let mut st = STATE.lock();
        st.bookmarks = bookmarks;
        for rows in profiles_rows {
            for r in rows {
                if !is_http(&r.url) {
                    continue;
                }
                match st.history.get_mut(&r.url) {
                    Some(h) => {
                        h.visits += r.visits;
                        if r.last > h.last {
                            h.last = r.last;
                            if !r.title.is_empty() {
                                h.title = r.title;
                            }
                        }
                    }
                    None => {
                        st.history.insert(r.url.clone(), r);
                    }
                }
            }
        }
        st.imported_at = now_ms();
    }
    tokio::fs::create_dir_all(user_data()).await?;
    save();
    Ok(get_web_data())
}

/* ---------------- visits and suggestions ---------------- */

fn record_visit(url: &str, title: &str) {
    if !is_http(url) {
        return;
    }
    {
        let mut st = STATE.lock();
        load(&mut st);
        let now = now_ms() as i64;
        match st.history.get_mut(url) {
            Some(h) => {
                h.visits += 1;
                h.last = now;
                if !title.is_empty() {
                    h.title = title.to_owned();
                }
            }
            None => {
                st.history.insert(url.to_owned(), HistoryEntry { url: url.to_owned(), title: title.to_owned(), visits: 1, last: now });
            }
        }
    }
    save();
}

fn set_visit_title(url: &str, title: &str) {
    let changed = {
        let mut st = STATE.lock();
        match st.history.get_mut(url) {
            Some(h) if !title.is_empty() && h.title != title => {
                h.title = title.to_owned();
                true
            }
            _ => false,
        }
    };
    if changed {
        save();
    }
}

struct Found {
    url: String,
    title: String,
    bookmark: Option<bool>,
    visits: i64,
    last: i64,
}

/// Address-bar suggestions: bookmarks and history whose URL or title contains every typed word.
fn suggest(query: &str, limit: usize) -> Vec<WebSuggestion> {
    static SCHEME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^https?://(www\.)?").expect("valid regex"));
    let mut st = STATE.lock();
    load(&mut st);
    let q = query.to_lowercase();
    let words: Vec<&str> = q.split_whitespace().collect();
    let Some(&first) = words.first() else { return Vec::new() };
    let matches = |url: &str, title: &str| {
        let s = format!("{url} {title}").to_lowercase();
        words.iter().all(|w| s.contains(w))
    };
    // every match, bookmarked or visited, ranked together (first-seen order breaks ties)
    let mut found: Vec<Found> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    fn walk(list: &[WebBookmark], matches: &dyn Fn(&str, &str) -> bool, found: &mut Vec<Found>, index: &mut HashMap<String, usize>) {
        for b in list {
            if let Some(children) = &b.children {
                walk(children, matches, found, index);
            } else if let Some(url) = b.url.as_deref().filter(|u| !u.is_empty())
                && matches(url, &b.title)
            {
                let f = Found { url: url.to_owned(), title: b.title.clone(), bookmark: Some(true), visits: 0, last: 0 };
                match index.get(url) {
                    Some(&i) => found[i] = f,
                    None => {
                        index.insert(url.to_owned(), found.len());
                        found.push(f);
                    }
                }
            }
        }
    }
    walk(&st.bookmarks, &matches, &mut found, &mut index);
    for h in st.history.values() {
        if !matches(&h.url, &h.title) {
            continue;
        }
        match index.get(&h.url) {
            Some(&i) => {
                found[i].visits = h.visits;
                found[i].last = h.last;
            }
            None => {
                index.insert(h.url.clone(), found.len());
                found.push(Found { url: h.url.clone(), title: h.title.clone(), bookmark: None, visits: h.visits, last: h.last });
            }
        }
    }
    drop(st);
    let now = now_ms() as f64;
    // frequent, recent, bookmarked and short (site roots) first; a host typed from its start ranks highest
    let score = |x: &Found| -> f64 {
        let bare = SCHEME.replace(&x.url, "").to_lowercase();
        let age = if x.last != 0 { (now - x.last as f64) / (30.0 * 864e5) } else { 3.0 };
        (1.0 + x.visits as f64).ln() - age.min(6.0) + if x.bookmark == Some(true) { 2.0 } else { 0.0 } + if bare.starts_with(first) { 5.0 } else { 0.0 }
            - bare.encode_utf16().count() as f64 / 60.0
    };
    let mut scored: Vec<(f64, Found)> = found.into_iter().map(|f| (score(&f), f)).collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal));
    scored.into_iter().take(limit).map(|(_, f)| WebSuggestion { url: f.url, title: f.title, bookmark: f.bookmark }).collect()
}

/* ---------------- commands ---------------- */

#[tauri::command]
pub async fn web_data() -> Res<WebData> {
    Ok(get_web_data())
}

#[tauri::command]
pub async fn web_import_chrome() -> Res<WebData> {
    Ok(import_chrome().await?)
}

#[tauri::command]
pub async fn web_suggest(query: String) -> Res<Vec<WebSuggestion>> {
    Ok(suggest(&query, 8))
}

#[tauri::command]
pub async fn web_visit(url: String, title: Option<String>) -> Res<()> {
    record_visit(&url, title.as_deref().unwrap_or(""));
    Ok(())
}

#[tauri::command]
pub async fn web_title(url: String, title: String) -> Res<()> {
    set_visit_title(&url, &title);
    Ok(())
}
