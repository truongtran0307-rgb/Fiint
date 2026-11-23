//! Zalo-style pop-up notifications (port of src/main/zaloToast.ts): a small frameless,
//! transparent, always-on-top window in the bottom-right corner that stacks cards (avatar, sender,
//! text), slides them in and hides them after a few seconds (paused on hover). Not the Windows
//! toast — it looks like the app and never steals focus.
//!
//! The page is served from a `toast://` protocol (registered by a tiny runtime plugin), so it is a
//! local origin and can `invoke` like the app. It reports its stack height through `toast_close`
//! after every change; at 0 the window is destroyed, so it costs nothing while idle.

use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};

use parking_lot::Mutex;
use serde::Serialize;
use tauri::{Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder, webview::PageLoadEvent};

use crate::core::{self, emit};

const LABEL: &str = "toast";
const WIDTH: f64 = 300.0;
const MARGIN: f64 = 8.0;

#[derive(Clone, Debug, Serialize)]
pub struct ToastTag {
    pub text: String,
    pub color: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ToastCard {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// initials fallback
    pub name: String,
    pub title: String,
    pub body: String,
    /// small line under the body, e.g. group name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<String>,
    /// chip before the body: "Việc mới", "Đang xử lý", "Hoàn thành"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<ToastTag>,
}

/// cards waiting for the page to load
static PENDING: Mutex<Vec<ToastCard>> = Mutex::new(Vec::new());
static READY: AtomicBool = AtomicBool::new(false);

fn register_protocol() -> bool {
    static DONE: OnceLock<bool> = OnceLock::new();
    *DONE.get_or_init(|| {
        let plugin = tauri::plugin::Builder::<tauri::Wry>::new("vibe-toast")
            .register_uri_scheme_protocol("toast", |_ctx, _req| {
                tauri::http::Response::builder()
                    .header("Content-Type", "text/html; charset=utf-8")
                    .body(PAGE.as_bytes().to_vec())
                    .expect("static response")
            })
            .build();
        core::app().plugin(plugin).is_ok()
    })
}

fn push_js(c: &ToastCard) -> String {
    format!("push({})", serde_json::to_string(c).unwrap_or_else(|_| "{}".into()))
}

fn flush(w: &WebviewWindow) {
    for c in std::mem::take(&mut *PENDING.lock()) {
        let _ = w.eval(push_js(&c));
    }
}

/// Show these cards (the window is made on demand).
pub fn show(cards: Vec<ToastCard>) {
    if cards.is_empty() {
        return;
    }
    let app = core::app();
    if let Some(w) = app.get_webview_window(LABEL)
        && READY.load(Ordering::SeqCst)
    {
        for c in &cards {
            let _ = w.eval(push_js(c));
        }
        return;
    }
    PENDING.lock().extend(cards);
    if app.get_webview_window(LABEL).is_some() || !register_protocol() {
        return;
    }
    let Ok(url) = "toast://localhost/".parse() else { return };
    let built = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::CustomProtocol(url))
        .title("VIBE CODE")
        .inner_size(WIDTH, 10.0)
        .visible(false)
        .decorations(false)
        .transparent(true)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .skip_taskbar(true)
        .always_on_top(true)
        .visible_on_all_workspaces(true)
        .focused(false)
        .focusable(false)
        .shadow(false)
        .on_page_load(|w, p| {
            if p.event() == PageLoadEvent::Finished {
                READY.store(true, Ordering::SeqCst);
                flush(&w);
            }
        })
        .build();
    if built.is_err() {
        PENDING.lock().clear();
    }
}

/// Bottom-right of the primary screen's work area, `h` CSS pixels tall.
fn place(w: &WebviewWindow, h: f64) {
    let Ok(Some(m)) = w.primary_monitor() else { return };
    let scale = m.scale_factor();
    let area = m.work_area();
    let width = (WIDTH * scale).round() as i32;
    let height = ((h * scale).round() as i32).min(area.size.height as i32 - (MARGIN * 2.0 * scale) as i32).max(1);
    let margin = (MARGIN * scale).round() as i32;
    let _ = w.set_size(tauri::PhysicalSize::new(width as u32, height as u32));
    let _ = w.set_position(tauri::PhysicalPosition::new(area.position.x + area.size.width as i32 - width - margin, area.position.y + area.size.height as i32 - height - margin));
}

/// A card was clicked: bring the app up on the task list.
#[tauri::command]
pub fn toast_click(id: Option<String>) {
    let _ = id;
    crate::windowing::show();
    emit("zalo:showTasks", Vec::<()>::new());
}

/// The page's stack changed to `height` CSS px; 0 = the last card is gone, so the window goes.
#[tauri::command]
pub fn toast_close(height: Option<f64>) {
    let Some(w) = core::app().get_webview_window(LABEL) else { return };
    match height.filter(|h| *h > 0.0) {
        Some(h) => {
            place(&w, h);
            if !w.is_visible().unwrap_or(false) {
                let _ = w.show();
            }
        }
        None if PENDING.lock().is_empty() => {
            READY.store(false, Ordering::SeqCst);
            let _ = w.destroy();
        }
        None => {}
    }
}

const PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><style>
:root { color-scheme: dark; }
html, body { margin: 0; background: transparent; overflow: hidden; font-family: 'Segoe UI', Inter, system-ui, sans-serif; user-select: none; }
#stack { display: flex; flex-direction: column; justify-content: flex-end; gap: 6px; padding: 4px; }
.card { position: relative; display: flex; align-items: center; gap: 9px; padding: 8px 24px 8px 9px; border-radius: 8px; cursor: pointer;
  background: #1f2a44; color: #e8edf7; border: 1px solid rgba(255,255,255,.08);
  box-shadow: 0 4px 14px rgba(0,0,0,.4); animation: in .25s cubic-bezier(.2,.9,.3,1.2); transition: transform .22s, opacity .22s; overflow: hidden; }
.card:hover { background: #24304d; }
.card.out { opacity: 0; transform: translateX(110%); }
@keyframes in { from { opacity: 0; transform: translateX(110%); } to { opacity: 1; transform: none; } }
.av { flex: none; width: 36px; height: 36px; border-radius: 50%; background: #3b6fe0; color: #fff;
  display: grid; place-items: center; font-weight: 700; font-size: 14px; overflow: hidden; }
.av img { width: 100%; height: 100%; object-fit: cover; }
.main { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 1px; }
.name { font-size: 12.5px; font-weight: 700; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.name span { font-weight: 400; color: #8b96ad; }
.body { font-size: 12px; line-height: 1.35; color: #c9d1e2; display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
.tag { font-weight: 600; }
.x { position: absolute; top: 4px; right: 4px; width: 18px; height: 18px; border: 0; border-radius: 5px; background: transparent;
  color: #8b96ad; font-size: 11px; line-height: 18px; cursor: pointer; padding: 0; opacity: 0; }
.card:hover .x { opacity: 1; }
.x:hover { background: rgba(255,255,255,.1); color: #fff; }
.bar { position: absolute; left: 0; right: 0; bottom: 0; height: 2px; background: #3b82f6; transform-origin: left;
  animation: bar var(--life) linear forwards; opacity: .6; }
.card:hover .bar, body:hover .bar { animation-play-state: paused; }
@keyframes bar { to { transform: scaleX(0); } }
</style></head><body><div id="stack"></div><script>
const LIFE = 7000, MAX = 3
const stack = document.getElementById('stack')
const invoke = (cmd, args) => { try { return window.__TAURI_INTERNALS__.invoke(cmd, args) } catch (e) {} }
let hovering = false
document.body.addEventListener('mouseenter', () => { hovering = true })
document.body.addEventListener('mouseleave', () => { hovering = false })
function report() { requestAnimationFrame(() => invoke('toast_close', { height: stack.children.length ? stack.offsetHeight : 0 })) }
function esc(s) { const d = document.createElement('div'); d.textContent = s || ''; return d.innerHTML }
function remove(el) {
  if (el.dataset.gone) return
  el.dataset.gone = '1'
  el.classList.add('out')
  setTimeout(() => { el.remove(); report() }, 250)
}
function push(c) {
  const el = document.createElement('div')
  el.className = 'card'
  el.style.setProperty('--life', LIFE + 'ms')
  const init = esc((c.name || '?').trim().split(/\s+/).slice(-1)[0].slice(0, 1).toUpperCase())
  el.innerHTML =
    '<div class="av">' + (c.avatar ? '<img src="' + esc(c.avatar) + '" onerror="this.parentNode.textContent=\'' + init + '\'">' : init) + '</div>' +
    '<div class="main"><div class="name">' + esc(c.title) + (c.meta ? ' <span>· ' + esc(c.meta) + '</span>' : '') + '</div>' +
    '<div class="body">' + (c.tag ? '<span class="tag" style="color:' + esc(c.tag.color) + '">' + esc(c.tag.text) + ':</span> ' : '') + esc(c.body) + '</div></div>' +
    '<button class="x" title="Đóng">✕</button><i class="bar"></i>'
  el.addEventListener('click', (e) => {
    if (e.target.closest('.x')) { remove(el); return }
    invoke('toast_click', { id: c.id })
    remove(el)
  })
  // auto-hide: counts only while the pointer is away
  let left = LIFE, last = Date.now()
  const tick = setInterval(() => {
    const now = Date.now()
    if (!hovering) left -= now - last
    last = now
    if (left <= 0) { clearInterval(tick); remove(el) }
  }, 200)
  stack.appendChild(el)
  while (stack.children.length > MAX) stack.firstElementChild.remove()
  report()
}
</script></body></html>"#;
