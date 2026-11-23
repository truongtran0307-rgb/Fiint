//! The built-in browser: every Web tab is a native child WebView of the main window, laid over
//! the spot the page reserves for it.
//!
//! Why this is light: a tab that is not on screen is made invisible to the engine (so its timers,
//! animations and video throttle), asked to use little memory, and after a while frozen outright
//! (WebView2 `TrySuspend`) — the Electron build kept every page of every chat running at full
//! speed. A frozen page wakes up as soon as it is shown again, with its form input intact.
//!
//! Claude's browser tools drive a tab through the DevTools protocol (Runtime.evaluate,
//! Page.captureScreenshot, Input.dispatchKeyEvent).

use std::{
    collections::HashMap,
    sync::LazyLock,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{Value, json};
use tauri::{LogicalPosition, LogicalSize, Manager, Webview, WebviewUrl, webview::WebviewBuilder};
use tokio::sync::oneshot;

use crate::core::{Res, app, emit, err};

/// Hidden this long → frozen.
const SUSPEND_AFTER: Duration = Duration::from_secs(90);

struct Tab {
    chat: String,
    webview: Webview,
    visible: bool,
    hidden_since: Option<Instant>,
    suspended: bool,
    /// console / errors are being recorded (turned on the first time Claude's tools use the tab)
    logging: bool,
}

static TABS: LazyLock<Mutex<HashMap<String, Tab>>> = LazyLock::new(Default::default);
/// label → console errors / warnings, uncaught exceptions and failed requests of the page now loaded
static LOGS: LazyLock<Mutex<HashMap<String, Vec<LogLine>>>> = LazyLock::new(Default::default);
type LogLine = (Instant, String);
/// chatId → label of the tab that chat's browser tools drive
static ACTIVE: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Default::default);
static WAITERS: LazyLock<Mutex<HashMap<String, Vec<oneshot::Sender<String>>>>> = LazyLock::new(Default::default);

fn webview(label: &str) -> Option<Webview> {
    TABS.lock().get(label).map(|t| t.webview.clone())
}

#[derive(Clone, Serialize)]
pub struct WebNav {
    url: String,
    title: String,
    back: bool,
    fwd: bool,
    loading: bool,
}

fn emit_nav(label: &str, nav: WebNav) {
    emit("web:nav", (label, nav));
}

/* ---------------- commands ---------------- */

/// Tabs whose page is still being created, and those of them closed meanwhile: a tab closed before
/// `web_open` finished would otherwise leave its page (a whole WebView2 renderer) alive for good.
static OPENING: LazyLock<Mutex<(std::collections::HashSet<String>, std::collections::HashSet<String>)>> = LazyLock::new(Default::default);

#[tauri::command]
pub async fn web_open(label: String, chat_id: String, url: String, user_agent: Option<String>) -> Res<()> {
    if TABS.lock().contains_key(&label) || !OPENING.lock().0.insert(label.clone()) {
        return Ok(());
    }
    let res = open_tab(label.clone(), chat_id, url, user_agent).await;
    let closed = {
        let mut o = OPENING.lock();
        o.0.remove(&label);
        o.1.remove(&label)
    };
    if closed {
        web_close(label);
    }
    res
}

async fn open_tab(label: String, chat_id: String, url: String, user_agent: Option<String>) -> Res<()> {
    let window = app().get_window(crate::core::MAIN).ok_or_else(|| err("no main window"))?;
    let parsed = url.parse().map_err(|_| err(format!("bad URL: {url}")))?;
    let popup_label = label.clone();
    #[allow(unused_mut)]
    let mut b = WebviewBuilder::new(&label, WebviewUrl::External(parsed))
        // popups open in the same tab
        .on_new_window(move |url, _features| {
            if matches!(url.scheme(), "http" | "https")
                && let Some(w) = webview(&popup_label)
            {
                let _ = w.navigate(url);
            }
            tauri::webview::NewWindowResponse::Deny
        })
        .zoom_hotkeys_enabled(true);
    if let Some(ua) = user_agent.filter(|u| !u.is_empty()) {
        b = b.user_agent(&ua);
    }
    #[cfg(not(windows))]
    {
        let l1 = label.clone();
        let l2 = label.clone();
        b = b
            .on_page_load(move |w, p| {
                let loading = matches!(p.event(), tauri::webview::PageLoadEvent::Started);
                emit_nav(&l1, WebNav { url: w.url().map(|u| u.to_string()).unwrap_or_default(), title: String::new(), back: true, fwd: true, loading });
            })
            .on_document_title_changed(move |w, title| {
                emit_nav(&l2, WebNav { url: w.url().map(|u| u.to_string()).unwrap_or_default(), title, back: true, fwd: true, loading: false });
            });
    }
    // parked off-screen until the page tells us where it goes
    let wv = window.add_child(b, LogicalPosition::new(-10_000.0, -10_000.0), LogicalSize::new(800.0, 600.0))?;
    let _ = wv.hide();
    #[cfg(windows)]
    win::hook(&wv, label.clone());
    TABS.lock().insert(label, Tab { chat: chat_id, webview: wv, visible: false, hidden_since: Some(Instant::now()), suspended: false, logging: false });
    start_sweeper();
    Ok(())
}

/// Where the page sits (CSS px of the main window) and its zoom; `visible` false parks it.
#[tauri::command]
pub fn web_place(label: String, x: f64, y: f64, w: f64, h: f64, zoom: f64, visible: bool) {
    let mut tabs = TABS.lock();
    let Some(t) = tabs.get_mut(&label) else { return };
    if visible {
        let _ = t.webview.set_position(LogicalPosition::new(x, y));
        let _ = t.webview.set_size(LogicalSize::new(w.max(1.0), h.max(1.0)));
        let _ = t.webview.set_zoom(zoom);
    }
    if visible == t.visible {
        return;
    }
    t.visible = visible;
    if visible {
        t.hidden_since = None;
        t.suspended = false;
        let _ = t.webview.show();
        #[cfg(windows)]
        win::set_awake(&t.webview, true);
    } else {
        t.hidden_since = Some(Instant::now());
        let _ = t.webview.hide();
        #[cfg(windows)]
        win::set_awake(&t.webview, false);
    }
}

#[tauri::command]
pub fn web_set_active(chat_id: String, label: String) {
    ACTIVE.lock().insert(chat_id.clone(), label.clone());
    for tx in WAITERS.lock().remove(&chat_id).unwrap_or_default() {
        let _ = tx.send(label.clone());
    }
}

#[tauri::command]
pub async fn web_navigate(label: String, url: String) -> Res<()> {
    let w = webview(&label).ok_or_else(|| err("tab is gone"))?;
    w.navigate(url.parse().map_err(|_| err(format!("bad URL: {url}")))?)?;
    Ok(())
}

#[tauri::command]
pub fn web_history(label: String, step: i32) {
    if let Some(w) = webview(&label) {
        #[cfg(windows)]
        win::history(&w, step);
        #[cfg(not(windows))]
        let _ = w.eval(if step < 0 { "history.back()" } else { "history.forward()" });
    }
}

#[tauri::command]
pub async fn web_reload(label: String, hard: bool) -> Res<()> {
    let w = webview(&label).ok_or_else(|| err("tab is gone"))?;
    if hard {
        cdp(&w, "Page.reload", json!({ "ignoreCache": true })).await?;
    } else {
        w.reload()?;
    }
    Ok(())
}

#[tauri::command]
pub fn web_stop(label: String) {
    if let Some(w) = webview(&label) {
        #[cfg(windows)]
        win::stop(&w);
        #[cfg(not(windows))]
        let _ = w.eval("window.stop()");
    }
}

/// Phone / tablet emulation: a different browser identity, then a reload so the site serves its
/// mobile layout. `null` → the engine's own.
#[tauri::command]
pub async fn web_set_user_agent(label: String, user_agent: Option<String>) -> Res<()> {
    let w = webview(&label).ok_or_else(|| err("tab is gone"))?;
    cdp(&w, "Emulation.setUserAgentOverride", json!({ "userAgent": user_agent.unwrap_or_default() })).await?;
    w.reload()?;
    Ok(())
}

#[tauri::command]
pub fn web_close(label: String) {
    let tab = TABS.lock().remove(&label);
    if tab.is_none() {
        let mut o = OPENING.lock();
        if o.0.contains(&label) {
            o.1.insert(label);
        }
        return;
    }
    if let Some(t) = tab {
        ACTIVE.lock().retain(|_, l| *l != label);
        LOGS.lock().remove(&label);
        let _ = t.webview.close();
    }
}

/* ---------------- sleeping tabs ---------------- */

fn start_sweeper() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        tauri::async_runtime::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_secs(15)).await;
                let due: Vec<Webview> = {
                    let mut tabs = TABS.lock();
                    tabs.values_mut()
                        .filter(|t| !t.visible && !t.suspended && t.hidden_since.is_some_and(|h| h.elapsed() >= SUSPEND_AFTER))
                        .map(|t| {
                            t.suspended = true;
                            t.webview.clone()
                        })
                        .collect()
                };
                for w in due {
                    #[cfg(windows)]
                    win::suspend(&w);
                    #[cfg(not(windows))]
                    let _ = w;
                }
            }
        });
    });
}

/// Web tab: drop cached files (HTTP cache, Cache Storage, service workers) but keep cookies and
/// localStorage, so logins survive.
pub async fn clear_cache() -> Res<()> {
    let w = app().get_webview(crate::core::MAIN).ok_or_else(|| err("no main window"))?;
    #[cfg(windows)]
    win::clear_cache(&w).await?;
    #[cfg(not(windows))]
    let _ = w;
    Ok(())
}

/* ---------------- for Claude's tools (mcp.rs) ---------------- */

/// The tab a chat's tools drive. Asks the page to open (and show) the chat's Web tab, waiting up
/// to 15 s for it.
pub async fn view_for(chat: &str) -> Result<Webview> {
    emit("browser:open", (chat,));
    let label = ACTIVE.lock().get(chat).cloned();
    if let Some(l) = label
        && let Some(w) = webview(&l)
    {
        return Ok(w);
    }
    let (tx, rx) = oneshot::channel();
    WAITERS.lock().entry(chat.to_owned()).or_default().push(tx);
    let label = tokio::time::timeout(Duration::from_secs(15), rx)
        .await
        .map_err(|_| anyhow!("This chat's Web tab did not open within 15s. Ask the user to open this chat (or its Web tab) and try again."))??;
    webview(&label).context("the Web tab closed")
}

/// Claude is about to drive the tab: unfreeze it if it was frozen, keep it from freezing again
/// for a while, and start recording its console errors (the first time).
pub async fn wake(w: &Webview) {
    let (resume, log) = {
        let mut tabs = TABS.lock();
        let Some(t) = tabs.get_mut(w.label()) else { return };
        if !t.visible {
            t.hidden_since = Some(Instant::now());
        }
        (std::mem::take(&mut t.suspended), !std::mem::replace(&mut t.logging, true))
    };
    #[cfg(windows)]
    if resume {
        win::resume(w);
    }
    #[cfg(not(windows))]
    let _ = resume;
    // Only for tabs Claude drives: some sites treat a page with Runtime enabled as a bot.
    if log && (cdp(w, "Runtime.enable", json!({})).await.is_err() || cdp(w, "Log.enable", json!({})).await.is_err())
        && let Some(t) = TABS.lock().get_mut(w.label())
    {
        t.logging = false;
    }
}

/// Console errors / warnings, uncaught exceptions and failed requests of the page now loaded
/// (only those after `since`, if given).
pub fn errors(w: &Webview, since: Option<Instant>) -> Vec<String> {
    LOGS.lock().get(w.label()).map(|v| v.iter().filter(|(t, _)| since.is_none_or(|s| *t >= s)).map(|(_, m)| m.clone()).collect()).unwrap_or_default()
}

/// A DevTools event of a tab whose logging is on → its error list.
#[cfg_attr(not(windows), allow(dead_code))]
fn record(label: &str, event: &str, params: &str) {
    let p: Value = serde_json::from_str(params).unwrap_or_default();
    let str_of = |v: &Value| v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string());
    let line = match event {
        // a new document: the old page's errors no longer apply
        "Runtime.executionContextsCleared" => {
            LOGS.lock().remove(label);
            return;
        }
        "Runtime.consoleAPICalled" => {
            let kind = p["type"].as_str().unwrap_or_default();
            if !matches!(kind, "error" | "warning" | "assert") {
                return;
            }
            let args = p["args"].as_array().map(|a| a.iter().map(|x| x.get("value").map(str_of).or_else(|| x["description"].as_str().map(str::to_owned)).unwrap_or_default()).collect::<Vec<_>>().join(" "));
            let f = &p["stackTrace"]["callFrames"][0];
            let at = f["url"].as_str().filter(|u| !u.is_empty()).map(|u| format!(" ({u}:{})", f["lineNumber"].as_u64().unwrap_or(0) + 1)).unwrap_or_default();
            format!("console.{kind}: {}{at}", args.unwrap_or_default())
        }
        "Runtime.exceptionThrown" => {
            let d = &p["exceptionDetails"];
            format!("uncaught: {}", d["exception"]["description"].as_str().or(d["text"].as_str()).unwrap_or("exception"))
        }
        "Log.entryAdded" => {
            let e = &p["entry"];
            if e["level"] != "error" {
                return;
            }
            let url = e["url"].as_str().filter(|u| !u.is_empty()).map(|u| format!(" ({u})")).unwrap_or_default();
            format!("{}: {}{url}", e["source"].as_str().unwrap_or("log"), e["text"].as_str().unwrap_or_default())
        }
        _ => return,
    };
    let line: String = line.chars().take(600).collect();
    let mut logs = LOGS.lock();
    let v = logs.entry(label.to_owned()).or_default();
    v.push((Instant::now(), line));
    if v.len() > 50 {
        v.remove(0);
    }
}

/// Whether the tab is on screen (a hidden one cannot be captured).
pub fn is_visible(w: &Webview) -> bool {
    TABS.lock().get(w.label()).is_some_and(|t| t.visible)
}

pub fn chat_of(w: &Webview) -> Option<String> {
    TABS.lock().get(w.label()).map(|t| t.chat.clone())
}

/// One DevTools protocol call.
pub async fn cdp(w: &Webview, method: &str, params: Value) -> Result<Value> {
    #[cfg(windows)]
    {
        win::cdp(w, method, params).await
    }
    #[cfg(not(windows))]
    {
        let _ = (w, method, params);
        bail!("The browser tools need Windows (WebView2) for now.")
    }
}

/// Evaluates a JS expression in the page (promises awaited) and returns its JSON value.
pub async fn eval(w: &Webview, expression: &str) -> Result<Value> {
    let r = cdp(w, "Runtime.evaluate", json!({ "expression": expression, "awaitPromise": true, "returnByValue": true, "userGesture": true })).await?;
    if let Some(ex) = r.get("exceptionDetails") {
        let msg = ex.pointer("/exception/description").or_else(|| ex.get("text")).and_then(Value::as_str).unwrap_or("JavaScript error");
        bail!("{msg}");
    }
    Ok(r.pointer("/result/value").cloned().unwrap_or(Value::Null))
}

/// Presses a key the way a keyboard would (keyDown [+ char] + keyUp).
pub async fn press_key(w: &Webview, key: &str) -> Result<()> {
    let (key_name, code, vk, text) = key_info(key);
    let mut down = json!({ "type": if text.is_some() { "keyDown" } else { "rawKeyDown" }, "key": key_name, "code": code, "windowsVirtualKeyCode": vk });
    if let Some(t) = &text {
        down["text"] = json!(t);
    }
    cdp(w, "Input.dispatchKeyEvent", down).await?;
    cdp(w, "Input.dispatchKeyEvent", json!({ "type": "keyUp", "key": key_name, "code": code, "windowsVirtualKeyCode": vk })).await?;
    Ok(())
}

fn key_info(key: &str) -> (String, String, u32, Option<String>) {
    let k = key.to_lowercase();
    let named = |name: &str, code: &str, vk: u32, text: Option<&str>| (name.to_owned(), code.to_owned(), vk, text.map(str::to_owned));
    match k.as_str() {
        "enter" | "return" => named("Enter", "Enter", 13, Some("\r")),
        "tab" => named("Tab", "Tab", 9, None),
        "escape" | "esc" => named("Escape", "Escape", 27, None),
        "backspace" => named("Backspace", "Backspace", 8, None),
        "delete" | "del" => named("Delete", "Delete", 46, None),
        "space" | " " => named(" ", "Space", 32, Some(" ")),
        "up" | "arrowup" => named("ArrowUp", "ArrowUp", 38, None),
        "down" | "arrowdown" => named("ArrowDown", "ArrowDown", 40, None),
        "left" | "arrowleft" => named("ArrowLeft", "ArrowLeft", 37, None),
        "right" | "arrowright" => named("ArrowRight", "ArrowRight", 39, None),
        "pageup" => named("PageUp", "PageUp", 33, None),
        "pagedown" => named("PageDown", "PageDown", 34, None),
        "home" => named("Home", "Home", 36, None),
        "end" => named("End", "End", 35, None),
        _ => {
            let mut chars = key.chars();
            if let (Some(c), None) = (chars.next(), chars.next()) {
                let up = c.to_ascii_uppercase();
                let (code, vk) = if up.is_ascii_alphabetic() {
                    (format!("Key{up}"), up as u32)
                } else if c.is_ascii_digit() {
                    (format!("Digit{c}"), c as u32)
                } else {
                    (String::new(), 0)
                };
                (c.to_string(), code, vk, Some(c.to_string()))
            } else if let Some(n) = k.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()).filter(|n| (1..=12).contains(n)) {
                (format!("F{n}"), format!("F{n}"), 111 + n, None)
            } else {
                (key.to_owned(), key.to_owned(), 0, None)
            }
        }
    }
}

/* ---------------- WebView2 ---------------- */

#[cfg(windows)]
mod win {
    use super::*;
    use webview2_com::{
        CallDevToolsProtocolMethodCompletedHandler, ClearBrowsingDataCompletedHandler, DevToolsProtocolEventReceivedEventHandler, DocumentTitleChangedEventHandler, HistoryChangedEventHandler,
        Microsoft::Web::WebView2::Win32::*, NavigationCompletedEventHandler, NavigationStartingEventHandler, TrySuspendCompletedHandler,
    };
    use windows::core::{BOOL, HSTRING, Interface, PWSTR};

    fn take_pwstr(p: PWSTR) -> String {
        if p.is_null() {
            return String::new();
        }
        // SAFETY: WebView2 hands back a CoTaskMem string we now own.
        let s = unsafe { p.to_string() }.unwrap_or_default();
        unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(p.as_ptr() as *const _)) };
        s
    }

    fn nav_state(core: &ICoreWebView2, loading: bool) -> WebNav {
        let mut url = PWSTR::null();
        let mut title = PWSTR::null();
        let (mut back, mut fwd) = (BOOL(0), BOOL(0));
        // SAFETY: out-params of a live ICoreWebView2 on its own (UI) thread.
        unsafe {
            let _ = core.Source(&mut url);
            let _ = core.DocumentTitle(&mut title);
            let _ = core.CanGoBack(&mut back);
            let _ = core.CanGoForward(&mut fwd);
        }
        WebNav { url: take_pwstr(url), title: take_pwstr(title), back: back.as_bool(), fwd: fwd.as_bool(), loading }
    }

    /// Navigation / title / history events of a new tab → "web:nav" for its address bar.
    pub fn hook(w: &Webview, label: String) {
        let _ = w.with_webview(move |pw| {
            // SAFETY: runs on the UI thread that owns the controller.
            unsafe {
                let Ok(core) = pw.controller().CoreWebView2() else { return };
                // Google refuses to sign in inside an "embedded" browser and tells it by `window.chrome.webview`.
                // Tabs show outside sites and talk to us only through the DevTools protocol, so they don't need it:
                // without it (from the next page on) Google sign-in works like in Chrome.
                if let Ok(s) = core.Settings() {
                    let _ = s.SetIsWebMessageEnabled(false);
                    let _ = s.SetAreHostObjectsAllowed(false);
                }
                let mut token = 0i64;
                let l = label.clone();
                let _ = core.add_NavigationStarting(
                    &NavigationStartingEventHandler::create(Box::new(move |s, _| {
                        if let Some(s) = s {
                            emit_nav(&l, nav_state(&s, true));
                        }
                        Ok(())
                    })),
                    &mut token,
                );
                let l = label.clone();
                let _ = core.add_NavigationCompleted(
                    &NavigationCompletedEventHandler::create(Box::new(move |s, _| {
                        if let Some(s) = s {
                            emit_nav(&l, nav_state(&s, false));
                        }
                        Ok(())
                    })),
                    &mut token,
                );
                let l = label.clone();
                let _ = core.add_HistoryChanged(
                    &HistoryChangedEventHandler::create(Box::new(move |s, _| {
                        if let Some(s) = s {
                            emit_nav(&l, nav_state(&s, false));
                        }
                        Ok(())
                    })),
                    &mut token,
                );
                // console / errors for Claude: delivered once wake() turns the domains on
                for event in ["Runtime.consoleAPICalled", "Runtime.exceptionThrown", "Log.entryAdded", "Runtime.executionContextsCleared"] {
                    let Ok(rx) = core.GetDevToolsProtocolEventReceiver(&HSTRING::from(event)) else { continue };
                    let l = label.clone();
                    let _ = rx.add_DevToolsProtocolEventReceived(
                        &DevToolsProtocolEventReceivedEventHandler::create(Box::new(move |_, args| {
                            if let Some(args) = args {
                                let mut p = PWSTR::null();
                                let _ = args.ParameterObjectAsJson(&mut p);
                                record(&l, event, &take_pwstr(p));
                            }
                            Ok(())
                        })),
                        &mut token,
                    );
                }
                let l = label;
                let _ = core.add_DocumentTitleChanged(
                    &DocumentTitleChangedEventHandler::create(Box::new(move |s, _| {
                        if let Some(s) = s {
                            emit_nav(&l, nav_state(&s, false));
                        }
                        Ok(())
                    })),
                    &mut token,
                );
            }
        });
    }

    /// Shown: full speed. Hidden: invisible to the engine (throttled) and asked to use little memory.
    pub fn set_awake(w: &Webview, awake: bool) {
        let _ = w.with_webview(move |pw| unsafe {
            let ctl = pw.controller();
            let _ = ctl.SetIsVisible(awake);
            if let Ok(core) = ctl.CoreWebView2() {
                if awake && let Ok(c3) = core.cast::<ICoreWebView2_3>() {
                    let _ = c3.Resume();
                }
                if let Ok(c19) = core.cast::<ICoreWebView2_19>() {
                    let _ = c19.SetMemoryUsageTargetLevel(if awake { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL } else { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW });
                }
            }
        });
    }

    /// Freezes a hidden tab: no script runs, its memory can be paged out. Showing it resumes it.
    pub fn suspend(w: &Webview) {
        let _ = w.with_webview(|pw| unsafe {
            if let Ok(core) = pw.controller().CoreWebView2()
                && let Ok(c3) = core.cast::<ICoreWebView2_3>()
            {
                let _ = c3.TrySuspend(&TrySuspendCompletedHandler::create(Box::new(|_, _| Ok(()))));
            }
        });
    }

    pub fn resume(w: &Webview) {
        let _ = w.with_webview(|pw| unsafe {
            if let Ok(core) = pw.controller().CoreWebView2()
                && let Ok(c3) = core.cast::<ICoreWebView2_3>()
            {
                let _ = c3.Resume();
            }
        });
    }

    pub fn history(w: &Webview, step: i32) {
        let _ = w.with_webview(move |pw| unsafe {
            if let Ok(core) = pw.controller().CoreWebView2() {
                let _ = if step < 0 { core.GoBack() } else { core.GoForward() };
            }
        });
    }

    pub fn stop(w: &Webview) {
        let _ = w.with_webview(|pw| unsafe {
            if let Ok(core) = pw.controller().CoreWebView2() {
                let _ = core.Stop();
            }
        });
    }

    pub async fn cdp(w: &Webview, method: &str, params: Value) -> Result<Value> {
        let (tx, rx) = oneshot::channel::<Result<String>>();
        let (method, params) = (HSTRING::from(method), HSTRING::from(params.to_string()));
        w.with_webview(move |pw| unsafe {
            let Ok(core) = pw.controller().CoreWebView2() else {
                let _ = tx.send(Err(anyhow!("the page is not ready")));
                return;
            };
            // on failure the handler (and with it `tx`) is dropped: the await below reports it
            let handler = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |hr, json| {
                let _ = tx.send(hr.map(|_| json).map_err(|e| anyhow!("{}", e.message())));
                Ok(())
            }));
            let _ = core.CallDevToolsProtocolMethod(&method, &params, &handler);
        })?;
        let json = tokio::time::timeout(Duration::from_secs(60), rx).await.map_err(|_| anyhow!("the page did not answer within 60s"))?.map_err(|_| anyhow!("the page is not ready"))??;
        Ok(serde_json::from_str(&json).unwrap_or(Value::Null))
    }

    pub async fn clear_cache(w: &Webview) -> Result<()> {
        let (tx, rx) = oneshot::channel::<()>();
        w.with_webview(move |pw| unsafe {
            let done = move || {
                let _ = tx.send(());
            };
            let profile = pw.controller().CoreWebView2().and_then(|c| c.cast::<ICoreWebView2_13>()).and_then(|c| c.Profile()).and_then(|p| p.cast::<ICoreWebView2Profile2>());
            match profile {
                Ok(p) => {
                    let kinds = COREWEBVIEW2_BROWSING_DATA_KINDS(
                        COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE.0 | COREWEBVIEW2_BROWSING_DATA_KINDS_CACHE_STORAGE.0 | COREWEBVIEW2_BROWSING_DATA_KINDS_SERVICE_WORKERS.0,
                    );
                    let _ = p.ClearBrowsingData(
                        kinds,
                        &ClearBrowsingDataCompletedHandler::create(Box::new(move |_| {
                            done();
                            Ok(())
                        })),
                    );
                }
                Err(_) => done(),
            }
        })?;
        let _ = tokio::time::timeout(Duration::from_secs(30), rx).await;
        Ok(())
    }
}
