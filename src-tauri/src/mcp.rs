//! The app's tools for Claude, as MCP servers over HTTP on localhost (like Claude desktop's).
//!
//! Every `claude` chat is launched with `--mcp-config <file>` naming three servers:
//!   vibe-browser  http://127.0.0.1:<port>/mcp/<chatId>   — that chat's own Web tab
//!   vibe-mssql    http://127.0.0.1:<port>/db/<chatId>    — SQL Server connections
//!   vibe-zalo     http://127.0.0.1:<port>/zalo/<chatId>  — the personal Zalo account
//! Streamable HTTP transport with plain JSON replies (no SSE), guarded by a per-launch token.

use std::{
    future::Future,
    sync::{
        OnceLock,
        atomic::{AtomicU16, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::Bytes,
    extract::Path,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use rand::RngExt;
use serde_json::{Value, json};

use crate::{browser, core::user_data};

const BROWSER: &str = "vibe-browser";
const DB: &str = "vibe-mssql";
const ZALO: &str = "vibe-zalo";

static PORT: AtomicU16 = AtomicU16::new(0);

fn token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| {
        let bytes: [u8; 24] = rand::rng().random();
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    })
}

/// Starts the server on a free localhost port; returns once chats can be given a config.
pub async fn start() -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    PORT.store(listener.local_addr()?.port(), Ordering::SeqCst);
    let router = Router::new().route("/{kind}/{chat}", post(on_post).get(not_post).delete(not_post));
    tauri::async_runtime::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(())
}

/// `--mcp-config` file for one chat (rewritten each launch: the port and token change).
pub fn config_for(chat: &str) -> Option<String> {
    let port = PORT.load(Ordering::SeqCst);
    if port == 0 {
        return None;
    }
    let dir = user_data().join("mcp");
    std::fs::create_dir_all(&dir).ok()?;
    let file = dir.join(format!("{}.json", chat.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect::<String>()));
    let chat_enc = urlencode(chat);
    let server = |kind: &str| json!({ "type": "http", "url": format!("http://127.0.0.1:{port}/{kind}/{chat_enc}"), "headers": { "Authorization": format!("Bearer {}", token()) } });
    // this build has no Database tab: only the chat's Web tab and Zalo
    let config = json!({ "mcpServers": { BROWSER: server("mcp"), ZALO: server("zalo") } });
    std::fs::write(&file, serde_json::to_vec_pretty(&config).ok()?).ok()?;
    Some(file.to_string_lossy().into_owned())
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

async fn not_post() -> Response {
    // no server-initiated stream (GET) and no session to end (DELETE)
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST")]).into_response()
}

async fn on_post(Path((kind, chat)): Path<(String, String)>, headers: HeaderMap, body: Bytes) -> Response {
    if !matches!(kind.as_str(), "mcp" | "db" | "zalo") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let auth = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok());
    if auth != Some(format!("Bearer {}", token()).as_str()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(msg) = serde_json::from_slice::<Value>(&body) else {
        return json_reply(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "Parse error" } }));
    };
    // notifications (no id) get no reply
    let Some(id) = msg.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };
    let method = msg["method"].as_str().unwrap_or_default();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let reply = match handle(&kind, &chat, method, &params).await {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    };
    json_reply(reply)
}

fn json_reply(v: Value) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], v.to_string()).into_response()
}

async fn handle(kind: &str, chat: &str, method: &str, params: &Value) -> Result<Value, (i32, String)> {
    let (name, instructions, tools) = match kind {
        "db" => (DB, crate::mssql::INSTRUCTIONS, crate::mssql::tools()),
        "zalo" => (ZALO, crate::zalo::INSTRUCTIONS, crate::zalo::tools()),
        _ => (BROWSER, BROWSER_INSTRUCTIONS, browser_tools()),
    };
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": name, "version": env!("CARGO_PKG_VERSION") },
            "instructions": instructions,
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools })),
        "tools/call" => {
            let tool = params["name"].as_str().unwrap_or_default().to_owned();
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let result = match kind {
                "db" => text_result(crate::mssql::call_tool(&tool, args)).await,
                "zalo" => text_result(crate::zalo::call_tool(&tool, args)).await,
                _ => call_browser(chat, &tool, &args).await,
            };
            Ok(match result {
                Ok(content) => json!({ "content": content }),
                Err(e) => json!({ "content": [{ "type": "text", "text": format!("{e:#}") }], "isError": true }),
            })
        }
        _ => Err((-32601, format!("Method not found: {method}"))),
    }
}

async fn text_result(f: impl Future<Output = anyhow::Result<String>>) -> anyhow::Result<Value> {
    Ok(json!([{ "type": "text", "text": f.await? }]))
}

/* ---------------- browser tools ---------------- */

const BROWSER_INSTRUCTIONS: &str = "These tools drive the Web tab of this chat in VIBE CODE — the browser the user is looking at. Use them when the user refers to 'the browser', 'the web page', 'this page/form/screen', 'tab Web', or something visible on the page they have open \
(prefer them over Chrome or other browser tools: this is the page the user means). \
The page is usually already open, often mid-way through a form: start with browser_snapshot (or browser_screenshot) to see it as it is. \
Do not navigate to it again or reload it just to look at it — that would wipe what they typed. \
They are also how you check your own work: after you change the code of a web page or web app, open it here and verify it before saying it works \
(start its dev server first if it needs one, in the background). Use browser_navigate, with reload: true when the page is already open and does not hot-reload \
(if its fields hold input the user typed, ask before reloading), then browser_snapshot and try the changed feature. \
Results list console errors, uncaught exceptions and failed requests of the page: fix those too.";

fn browser_tools() -> Value {
    let target = json!({
        "ref": { "type": "string", "description": "Element number from the last browser_snapshot, e.g. \"12\"" },
        "selector": { "type": "string", "description": "CSS selector, used when no ref is given" }
    });
    let mut type_props = target.clone();
    type_props["text"] = json!({ "type": "string" });
    type_props["submit"] = json!({ "type": "boolean" });
    json!([
        {
            "name": "browser_navigate",
            "description": "Open a URL in this chat's Web tab (the built-in browser the user sees). Waits for the page to load. Do NOT use it to look at the page the user already has open — that throws away what they typed; use browser_snapshot instead. Opening the URL that is already loaded is a no-op unless `reload` is true.",
            "inputSchema": { "type": "object", "properties": { "url": { "type": "string" }, "reload": { "type": "boolean", "description": "Reload even if this URL is already open, e.g. to see code you just changed. Loses unsaved form input: if the user typed into the page, ask first." } }, "required": ["url"] }
        },
        {
            "name": "browser_snapshot",
            "description": "Start here. Reads the page that is open in the Web tab right now, exactly as the user left it (no reload): URL, title, visible text, a numbered list of interactive elements with their current values, and the page's console errors and failed requests. Use the numbers as `ref` for browser_click / browser_type. Take a new snapshot after the page changes.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        { "name": "browser_click", "description": "Click an element (by `ref` from browser_snapshot, or by CSS `selector`). Waits for a page it opens to load; reports the new URL and any errors the click caused.", "inputSchema": { "type": "object", "properties": target } },
        {
            "name": "browser_type",
            "description": "Set the text of an input, textarea, contenteditable or select (option value or label). `submit` presses Enter afterwards.",
            "inputSchema": { "type": "object", "properties": type_props, "required": ["text"] }
        },
        {
            "name": "browser_press_key",
            "description": "Press a key in the page, e.g. \"Enter\", \"Escape\", \"Tab\", \"Down\", \"PageDown\", \"a\".",
            "inputSchema": { "type": "object", "properties": { "key": { "type": "string" } }, "required": ["key"] }
        },
        { "name": "browser_screenshot", "description": "Screenshot of the Web tab as the user sees it.", "inputSchema": { "type": "object", "properties": {} } },
        {
            "name": "browser_eval",
            "description": "Run a JavaScript expression in the page and return its (JSON) result. Promises are awaited.",
            "inputSchema": { "type": "object", "properties": { "expression": { "type": "string" } }, "required": ["expression"] }
        }
    ])
}

/// Numbers every visible interactive element (data-vibe-ref) and returns them with the page text.
const SNAPSHOT_JS: &str = r#"(() => {
  const SEL = 'a[href],button,input,select,textarea,summary,[role=button],[role=link],[role=checkbox],[role=radio],[role=tab],[role=menuitem],[role=option],[role=switch],[contenteditable=""],[contenteditable=true],[onclick]'
  document.querySelectorAll('[data-vibe-ref]').forEach((e) => e.removeAttribute('data-vibe-ref'))
  const elements = []
  let n = 0
  for (const el of document.querySelectorAll(SEL)) {
    const r = el.getBoundingClientRect()
    if (r.width === 0 && r.height === 0) continue
    const cs = getComputedStyle(el)
    if (cs.visibility === 'hidden' || cs.display === 'none') continue
    el.setAttribute('data-vibe-ref', String(++n))
    const tag = el.tagName.toLowerCase()
    const kind = el.getAttribute('role') || (tag === 'input' ? 'input[' + (el.type || 'text') + ']' : tag)
    const label = (el.getAttribute('aria-label') || el.innerText || el.value || el.placeholder || el.title || el.name || '')
      .replace(/\s+/g, ' ').trim().slice(0, 80)
    let line = '[' + n + '] ' + kind + ' "' + label + '"'
    if (tag === 'a') line += ' -> ' + (el.getAttribute('href') || '').slice(0, 120)
    if (tag === 'input' || tag === 'textarea') {
      if (el.type !== 'checkbox' && el.type !== 'radio' && el.type !== 'password' && el.value) line += ' value="' + String(el.value).slice(0, 120) + '"'
    } else if (tag === 'select' && el.selectedIndex >= 0) line += ' selected="' + (el.options[el.selectedIndex].text || '').trim().slice(0, 80) + '"'
    if (el.disabled) line += ' (disabled)'
    if (el.checked) line += ' (checked)'
    elements.push(line)
    if (n >= 400) break
  }
  const text = document.body ? document.body.innerText.replace(/\n{3,}/g, '\n\n').slice(0, 20000) : ''
  return { title: document.title, url: location.href, elements, text }
})()"#;

/// JS expression for the element a call points at: { ref } from the last snapshot, or { selector }.
fn find_js(args: &Value) -> anyhow::Result<String> {
    if let Some(r) = args.get("ref").filter(|v| !v.is_null()) {
        let r = r.as_str().map(str::to_owned).unwrap_or_else(|| r.to_string());
        return Ok(format!("document.querySelector('[data-vibe-ref=\"' + {} + '\"]')", Value::String(r)));
    }
    if let Some(s) = args.get("selector").and_then(Value::as_str) {
        return Ok(format!("document.querySelector({})", Value::String(s.into())));
    }
    anyhow::bail!("Pass `ref` (from browser_snapshot) or `selector`.")
}

fn click_js(find: &str) -> String {
    format!(
        r#"(() => {{
  const el = {find}
  if (!el) return 'Element not found. Take a new browser_snapshot: refs change when the page changes.'
  el.scrollIntoView({{ block: 'center', inline: 'center' }})
  if (el.focus) el.focus()
  const o = {{ bubbles: true, cancelable: true, view: window }}
  el.dispatchEvent(new PointerEvent('pointerdown', o)); el.dispatchEvent(new MouseEvent('mousedown', o))
  el.dispatchEvent(new PointerEvent('pointerup', o)); el.dispatchEvent(new MouseEvent('mouseup', o))
  el.click()
  return 'ok'
}})()"#
    )
}

/// Replaces the field's value in a way React/Vue inputs notice (native setter + input/change).
fn type_js(find: &str, text: &str) -> String {
    let text = Value::String(text.into());
    format!(
        r#"(() => {{
  const el = {find}
  if (!el) return 'Element not found. Take a new browser_snapshot: refs change when the page changes.'
  el.scrollIntoView({{ block: 'center' }})
  el.focus()
  const text = {text}
  if (el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement) {{
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, text)
    el.dispatchEvent(new Event('input', {{ bubbles: true }}))
    el.dispatchEvent(new Event('change', {{ bubbles: true }}))
  }} else if (el instanceof HTMLSelectElement) {{
    const opt = [...el.options].find((o) => o.value === text || o.text.trim() === text)
    if (!opt) return 'No option "' + text + '" in this select.'
    el.value = opt.value
    el.dispatchEvent(new Event('input', {{ bubbles: true }}))
    el.dispatchEvent(new Event('change', {{ bubbles: true }}))
  }} else if (el.isContentEditable) {{
    document.execCommand('selectAll')
    document.execCommand('insertText', false, text)
  }} else return 'That element is not a text field.'
  return 'ok'
}})()"#
    )
}

fn text(t: impl Into<String>) -> Value {
    json!([{ "type": "text", "text": t.into() }])
}

fn same_page(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.split('#').next().unwrap_or_default().trim_end_matches('/').to_owned();
    norm(a) == norm(b)
}

const PAGE_STATE_JS: &str = "({ url: location.href, title: document.title, ready: document.readyState })";

/// Waits (up to `max`) for the document to finish loading; returns its { url, title, ready }.
async fn settle(w: &tauri::Webview, max: Duration) -> Value {
    let start = Instant::now();
    let mut last = Value::Null;
    while start.elapsed() < max {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if let Ok(v) = browser::eval(w, PAGE_STATE_JS).await {
            let done = v["ready"] == "complete" && v["url"].as_str().is_some_and(|u| u != "about:blank");
            last = v;
            if done {
                break;
            }
        }
    }
    last
}

/// The page's errors (since `since`, if given) as a block to append to a tool result.
fn errors_note(w: &tauri::Webview, since: Option<Instant>) -> String {
    let e = browser::errors(w, since);
    if e.is_empty() { String::new() } else { format!("\n\nConsole errors / failed requests ({}):\n- {}", e.len(), e.join("\n- ")) }
}

/// After a click / key press: let a page it opens load, then say where we are and what broke.
async fn after_action(w: &tauri::Webview, result: String, before: &str, since: Instant) -> Value {
    if result != "ok" {
        return text(result);
    }
    let now = settle(w, Duration::from_secs(15)).await;
    let url = now["url"].as_str().unwrap_or_default();
    let moved = if !url.is_empty() && url != before { format!(" — now at {url} \"{}\"", now["title"].as_str().unwrap_or_default()) } else { String::new() };
    text(format!("ok{moved}{}", errors_note(w, Some(since))))
}

async fn page_url(w: &tauri::Webview) -> String {
    browser::eval(w, "location.href").await.ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}

async fn call_browser(chat: &str, name: &str, args: &Value) -> anyhow::Result<Value> {
    let w = browser::view_for(chat).await?;
    debug_assert!(browser::chat_of(&w).is_some());
    browser::wake(&w).await;
    let since = Instant::now();
    match name {
        "browser_navigate" => {
            let url = args["url"].as_str().unwrap_or_default();
            if !(url.starts_with("http://") || url.starts_with("https://") || url.starts_with("file:")) {
                anyhow::bail!("Only http(s) and file URLs.");
            }
            let here = browser::eval(&w, "({ url: location.href, title: document.title })").await.unwrap_or(Value::Null);
            let (cur, title) = (here["url"].as_str().unwrap_or_default(), here["title"].as_str().unwrap_or_default());
            if !args["reload"].as_bool().unwrap_or(false) && same_page(cur, url) {
                return Ok(text(format!("Already open: {cur} — \"{title}\". Not reloaded, so the user's form input is kept. Use browser_snapshot to read it.")));
            }
            w.navigate(url.parse()?)?;
            let last = settle(&w, Duration::from_secs(30)).await;
            Ok(text(format!("Loaded {} — \"{}\"{}", last["url"].as_str().unwrap_or(url), last["title"].as_str().unwrap_or_default(), errors_note(&w, None))))
        }
        "browser_snapshot" => {
            let s = browser::eval(&w, SNAPSHOT_JS).await?;
            let elements = s["elements"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n")).unwrap_or_default();
            Ok(text(format!(
                "URL: {}\nTitle: {}\n\nInteractive elements:\n{}\n\nPage text:\n{}{}",
                s["url"].as_str().unwrap_or_default(),
                s["title"].as_str().unwrap_or_default(),
                if elements.is_empty() { "(none)" } else { &elements },
                s["text"].as_str().unwrap_or_default(),
                errors_note(&w, None)
            )))
        }
        "browser_click" => {
            let before = page_url(&w).await;
            let r = value_text(browser::eval(&w, &click_js(&find_js(args)?)).await?);
            Ok(after_action(&w, r, &before, since).await)
        }
        "browser_type" => {
            let r = value_text(browser::eval(&w, &type_js(&find_js(args)?, args["text"].as_str().unwrap_or_default())).await?);
            if r != "ok" || !args["submit"].as_bool().unwrap_or(false) {
                return Ok(text(r));
            }
            let before = page_url(&w).await;
            browser::press_key(&w, "Enter").await?;
            Ok(after_action(&w, r, &before, since).await)
        }
        "browser_press_key" => {
            let before = page_url(&w).await;
            browser::press_key(&w, args["key"].as_str().unwrap_or_default()).await?;
            Ok(after_action(&w, "ok".into(), &before, since).await)
        }
        "browser_screenshot" => {
            if !browser::is_visible(&w) {
                return Ok(text("The Web tab is hidden (another chat or the Source code tab is in front), so there is nothing to capture."));
            }
            let r = browser::cdp(&w, "Page.captureScreenshot", json!({ "format": "png" })).await?;
            let data = r["data"].as_str().unwrap_or_default();
            if data.is_empty() || B64.decode(data).is_err() {
                return Ok(text("Could not capture the Web tab."));
            }
            Ok(json!([{ "type": "image", "data": data, "mimeType": "image/png" }]))
        }
        "browser_eval" => {
            let v = browser::eval(&w, args["expression"].as_str().unwrap_or_default()).await?;
            Ok(text(if v.is_null() { "undefined".to_owned() } else { serde_json::to_string_pretty(&v)? }))
        }
        _ => anyhow::bail!("Unknown tool {name}"),
    }
}

fn value_text(v: Value) -> String {
    match v {
        Value::String(s) => s,
        other => other.to_string(),
    }
}
