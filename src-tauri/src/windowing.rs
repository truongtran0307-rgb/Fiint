//! The main window and how it lives on screen:
//!  - "window"   — an ordinary desktop window
//!  - "dropdown" — Guake-style: a global hotkey slides it down from the top edge of the
//!    screen under the mouse, above every other app, and back up again.
//!
//! Plus the tray icon, the taskbar badge and start-at-login.

use std::{
    sync::{
        LazyLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{
    App, PhysicalPosition, PhysicalRect, PhysicalSize, WebviewUrl, WebviewWindowBuilder, Window, WindowEvent,
    image::Image,
    menu::{CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu},
    tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent},
    window::Color,
};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use crate::{
    core::{MAIN, Res, app, emit, main_window},
    settings,
};

/// Launched by the OS at sign-in: start tucked away in the tray.
pub const HIDDEN_ARG: &str = "--hidden";
/// F12 is reserved by Windows for the debugger, so fall back through a few Guake-ish keys.
const HOTKEY_FALLBACKS: [&str; 3] = ["Alt+`", "Ctrl+Alt+`", "Ctrl+Alt+Space"];

struct State {
    mode: String,
    normal: Option<PhysicalRect<i32, u32>>,
    tray: Option<TrayIcon>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State { mode: "window".into(), normal: None, tray: None }));
static ANIMATING: AtomicBool = AtomicBool::new(false);
static HOTKEY_OK: AtomicBool = AtomicBool::new(false);
static QUITTING: AtomicBool = AtomicBool::new(false);

fn mode() -> String {
    STATE.lock().mode.clone()
}

fn dropdown() -> bool {
    mode() == "dropdown"
}

/// Whole-window opacity. In see-through mode the page fades its own backgrounds instead.
fn win_opacity() -> f64 {
    let s = settings::get();
    if s.see_through { 1.0 } else { s.drop_opacity.clamp(0.2, 1.0) }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowState {
    mode: String,
    maximized: bool,
    hotkey: String,
    hotkey_ok: bool,
}

fn window_state() -> WindowState {
    WindowState {
        mode: mode(),
        maximized: main_window().and_then(|w| w.is_maximized().ok()).unwrap_or(false),
        hotkey: settings::get().hotkey,
        hotkey_ok: HOTKEY_OK.load(Ordering::Relaxed),
    }
}

fn push_state() {
    emit("win:state", (window_state(),));
}

/* ---------------- geometry ---------------- */

/// Work area of the screen under the mouse.
fn target_work_area(w: &Window) -> Option<PhysicalRect<i32, u32>> {
    let m = w
        .cursor_position()
        .ok()
        .and_then(|p| w.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| w.current_monitor().ok().flatten())
        .or_else(|| w.primary_monitor().ok().flatten())?;
    Some(*m.work_area())
}

fn drop_bounds(w: &Window) -> Option<PhysicalRect<i32, u32>> {
    let wa = target_work_area(w)?;
    let h = (wa.size.height as f64 * settings::get().drop_height.clamp(0.25, 1.0)).round() as u32;
    Some(PhysicalRect { position: wa.position, size: PhysicalSize::new(wa.size.width, h) })
}

fn set_bounds(w: &Window, r: PhysicalRect<i32, u32>) {
    let _ = w.set_size(r.size);
    let _ = w.set_position(r.position);
}

fn bounds(w: &Window) -> Option<PhysicalRect<i32, u32>> {
    Some(PhysicalRect { position: w.outer_position().ok()?, size: w.outer_size().ok()? })
}

/* ---------------- opacity (Win32 layered window) ---------------- */

#[cfg(windows)]
fn set_opacity(w: &Window, op: f64) {
    use windows::Win32::{
        Foundation::{COLORREF, HWND},
        UI::WindowsAndMessaging::{GWL_EXSTYLE, GetWindowLongPtrW, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongPtrW, WS_EX_LAYERED},
    };
    let Ok(h) = w.hwnd() else { return };
    let hwnd = HWND(h.0);
    // SAFETY: hwnd is our live top-level window; these only change its extended style / alpha.
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if op >= 0.999 {
            if ex & WS_EX_LAYERED.0 as isize != 0 {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex & !(WS_EX_LAYERED.0 as isize));
            }
            return;
        }
        if ex & WS_EX_LAYERED.0 as isize == 0 {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_LAYERED.0 as isize);
        }
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), (op.clamp(0.0, 1.0) * 255.0).round() as u8, LWA_ALPHA);
    }
}

#[cfg(not(windows))]
fn set_opacity(_w: &Window, _op: f64) {}

/* ---------------- slide animation ---------------- */

fn animate(w: Window, from_y: i32, to: PhysicalRect<i32, u32>, from_op: f64, to_op: f64, ms: u64, done: impl FnOnce(&Window) + Send + 'static) {
    ANIMATING.store(true, Ordering::SeqCst);
    std::thread::spawn(move || {
        let start = Instant::now();
        loop {
            let t = (start.elapsed().as_secs_f64() * 1000.0 / ms as f64).min(1.0);
            let e = 1.0 - (1.0 - t).powi(3); // ease-out cubic
            let y = (from_y as f64 + (to.position.y - from_y) as f64 * e).round() as i32;
            let _ = w.set_position(PhysicalPosition::new(to.position.x, y));
            set_opacity(&w, from_op + (to_op - from_op) * e);
            if t >= 1.0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(7));
        }
        ANIMATING.store(false, Ordering::SeqCst);
        done(&w);
    });
}

fn slide_down(w: &Window) {
    let Some(b) = drop_bounds(w) else { return };
    let from = b.position.y - b.size.height as i32;
    set_opacity(w, 0.0);
    set_bounds(w, PhysicalRect { position: PhysicalPosition::new(b.position.x, from), size: b.size });
    let _ = w.set_always_on_top(true);
    let _ = w.show();
    animate(w.clone(), from, b, 0.0, win_opacity(), 170, |w| {
        let _ = w.set_focus();
        emit("win:shown", [(); 0]);
    });
}

fn slide_up(w: &Window) {
    let Some(b) = bounds(w) else { return };
    let to = PhysicalRect { position: PhysicalPosition::new(b.position.x, b.position.y - b.size.height as i32), size: b.size };
    animate(w.clone(), b.position.y, to, win_opacity(), 0.0, 130, move |w| {
        let _ = w.hide();
        set_bounds(w, b);
    });
}

/* ---------------- show / hide ---------------- */

pub fn toggle() {
    let Some(w) = main_window() else { return };
    if ANIMATING.load(Ordering::SeqCst) {
        return;
    }
    let visible = w.is_visible().unwrap_or(false);
    let focused = w.is_focused().unwrap_or(false);
    if !dropdown() {
        if visible && focused && !w.is_minimized().unwrap_or(false) {
            let _ = w.minimize();
        } else {
            show();
        }
        return;
    }
    if visible {
        if focused { slide_up(&w) } else { let _ = w.set_focus(); }
    } else {
        slide_down(&w);
    }
}

pub fn show() {
    let Some(w) = main_window() else { return };
    if dropdown() {
        if !w.is_visible().unwrap_or(false) && !ANIMATING.load(Ordering::SeqCst) {
            slide_down(&w);
        } else {
            let _ = w.set_focus();
        }
        return;
    }
    let _ = w.unminimize();
    let _ = w.show();
    let _ = w.set_focus();
}

fn hide_dropdown() {
    if let Some(w) = main_window()
        && dropdown()
        && w.is_visible().unwrap_or(false)
        && !ANIMATING.load(Ordering::SeqCst)
    {
        slide_up(&w);
    }
}

fn apply_dropdown(w: &Window, show: bool) {
    let _ = w.set_skip_taskbar(true);
    let _ = w.set_maximizable(false);
    let _ = w.set_minimizable(false);
    let _ = w.set_always_on_top(true);
    if show {
        slide_down(w);
    } else {
        if let Some(b) = drop_bounds(w) {
            set_bounds(w, b);
        }
        let _ = w.hide();
    }
}

fn apply_window(w: &Window) {
    let _ = w.set_always_on_top(false);
    let _ = w.set_skip_taskbar(false);
    let _ = w.set_maximizable(true);
    let _ = w.set_minimizable(true);
    set_opacity(w, win_opacity());
    let normal = STATE.lock().normal;
    match normal {
        Some(b) => set_bounds(w, b),
        None => {
            if let Some(wa) = target_work_area(w) {
                let sw = 1480.min(wa.size.width.saturating_sub(80));
                let sh = 920.min(wa.size.height.saturating_sub(60));
                set_bounds(w, PhysicalRect {
                    position: PhysicalPosition::new(wa.position.x + ((wa.size.width - sw) / 2) as i32, wa.position.y + ((wa.size.height - sh) / 2) as i32),
                    size: PhysicalSize::new(sw, sh),
                });
            }
        }
    }
    let _ = w.show();
    let _ = w.set_focus();
}

async fn set_mode(m: String) {
    let Some(w) = main_window() else { return };
    if m == mode() {
        return;
    }
    if m == "dropdown" {
        if w.is_maximized().unwrap_or(false) {
            let _ = w.unmaximize();
        }
        {
            let mut st = STATE.lock();
            st.normal = bounds(&w);
            st.mode = m.clone();
        }
        apply_dropdown(&w, true);
    } else {
        STATE.lock().mode = m.clone();
        apply_window(&w);
    }
    settings::set(|s| s.mode = m).await;
    push_state();
    build_tray_menu();
}

/// Re-apply drop-down height / opacity after the settings changed (in-app panel or tray).
pub fn apply_drop_prefs() {
    let Some(w) = main_window() else { return };
    if dropdown() && w.is_visible().unwrap_or(false) && !ANIMATING.load(Ordering::SeqCst) {
        if let Some(b) = drop_bounds(&w) {
            set_bounds(&w, b);
        }
        set_opacity(&w, win_opacity());
    }
    // the opacity setting applies to the normal window too
    if !dropdown() {
        set_opacity(&w, win_opacity());
    }
    build_tray_menu();
    push_state();
}

/* ---------------- hotkey ---------------- */

fn register_hotkey() {
    let gs = app().global_shortcut();
    let _ = gs.unregister_all();
    // screen capture, from any app
    let _ = gs.on_shortcut(crate::capture::HOTKEY, |_app, _shortcut, ev| {
        if ev.state == ShortcutState::Pressed {
            tauri::async_runtime::spawn(async {
                let _ = crate::capture::capture_start(false).await;
            });
        }
    });
    let wanted = settings::get().hotkey;
    HOTKEY_OK.store(false, Ordering::Relaxed);
    let candidates = std::iter::once(wanted.as_str()).chain(HOTKEY_FALLBACKS.iter().copied().filter(|k| *k != wanted));
    for key in candidates {
        let ok = gs
            .on_shortcut(key, |_app, _shortcut, ev| {
                if ev.state == ShortcutState::Pressed {
                    toggle();
                }
            })
            .is_ok();
        if ok {
            HOTKEY_OK.store(true, Ordering::Relaxed);
            if key != wanted {
                let key = key.to_owned();
                tauri::async_runtime::spawn(async move { settings::set(|s| s.hotkey = key).await });
            }
            return;
        }
    }
}

/* ---------------- tray ---------------- */

const HEIGHTS: [f64; 6] = [0.4, 0.5, 0.6, 0.75, 0.9, 1.0];
const OPACITIES: [f64; 6] = [1.0, 0.9, 0.8, 0.6, 0.4, 0.25];

fn open_at_login() -> bool {
    settings::get().open_at_login.unwrap_or(true)
}

/// Register / unregister the sign-in entry. Installed app only: in dev it would point at a debug exe.
fn apply_login_item() {
    if cfg!(debug_assertions) {
        return;
    }
    let al = app().autolaunch();
    let _ = if open_at_login() { al.enable() } else { al.disable() };
}

fn build_tray_menu() {
    let Some(tray) = STATE.lock().tray.clone() else { return };
    let s = settings::get();
    let hk = HOTKEY_OK.load(Ordering::Relaxed);
    let _ = tray.set_tooltip(Some(format!("VIBE CODE — {} to show/hide", if hk { s.hotkey.as_str() } else { "no hotkey" })));
    if let Ok(menu) = tray_menu(&s, hk) {
        let _ = tray.set_menu(Some(menu));
    }
}

fn menu_refs(v: &[CheckMenuItem<tauri::Wry>]) -> Vec<&dyn IsMenuItem<tauri::Wry>> {
    v.iter().map(|i| i as &dyn IsMenuItem<tauri::Wry>).collect()
}

fn tray_menu(s: &settings::AppSettings, hk: bool) -> tauri::Result<Menu<tauri::Wry>> {
    let a = app();
    let drop = dropdown();
    let heights: Vec<CheckMenuItem<_>> = HEIGHTS
        .iter()
        .map(|h| CheckMenuItem::with_id(a, format!("h:{h}"), format!("{}%", (h * 100.0).round()), true, (s.drop_height - h).abs() < 0.01, None::<&str>))
        .collect::<Result<_, _>>()?;
    let opacities: Vec<CheckMenuItem<_>> = OPACITIES
        .iter()
        .map(|o| CheckMenuItem::with_id(a, format!("o:{o}"), format!("{}%", (o * 100.0).round()), true, (s.drop_opacity - o).abs() < 0.005, None::<&str>))
        .collect::<Result<_, _>>()?;
    let packaged = !cfg!(debug_assertions);
    Menu::with_items(a, &[
        &MenuItem::with_id(a, "toggle", "Show / Hide", true, hk.then_some(s.hotkey.as_str()))?,
        &PredefinedMenuItem::separator(a)?,
        &CheckMenuItem::with_id(a, "mode:dropdown", "Drop-down (Guake)", true, drop, None::<&str>)?,
        &CheckMenuItem::with_id(a, "mode:window", "Normal window", true, !drop, None::<&str>)?,
        &PredefinedMenuItem::separator(a)?,
        &Submenu::with_items(a, "Drop-down height", true, &menu_refs(&heights))?,
        &Submenu::with_items(a, "Window opacity", true, &menu_refs(&opacities))?,
        &CheckMenuItem::with_id(a, "blur", "Hide when focus is lost", true, s.hide_on_blur, None::<&str>)?,
        &PredefinedMenuItem::separator(a)?,
        &CheckMenuItem::with_id(
            a,
            "login",
            if packaged { "Start with Windows" } else { "Start with Windows (installed app only)" },
            packaged,
            packaged && open_at_login(),
            None::<&str>,
        )?,
        &PredefinedMenuItem::separator(a)?,
        &MenuItem::with_id(a, "quit", "Quit VIBE CODE", true, None::<&str>)?,
    ])
}

fn on_menu(ev: MenuEvent) {
    let id = ev.id().0.clone();
    tauri::async_runtime::spawn(async move {
        match id.as_str() {
            "toggle" => toggle(),
            "mode:dropdown" => set_mode("dropdown".into()).await,
            "mode:window" => set_mode("window".into()).await,
            "blur" => {
                settings::set(|s| s.hide_on_blur = !s.hide_on_blur).await;
            }
            "login" => {
                let on = !open_at_login();
                settings::set(|s| s.open_at_login = Some(on)).await;
                apply_login_item();
            }
            "quit" => quit(),
            _ => {
                if let Some(h) = id.strip_prefix("h:").and_then(|v| v.parse::<f64>().ok()) {
                    settings::set(|s| s.drop_height = h).await;
                    apply_drop_prefs();
                } else if let Some(o) = id.strip_prefix("o:").and_then(|v| v.parse::<f64>().ok()) {
                    settings::set(|s| s.drop_opacity = o).await;
                    apply_drop_prefs();
                }
            }
        }
        build_tray_menu();
        push_state();
    });
}

pub fn quit() {
    QUITTING.store(true, Ordering::SeqCst);
    crate::pty::kill_all();
    app().exit(0);
}

/* ---------------- setup ---------------- */

pub fn init(app: &mut App) -> anyhow::Result<()> {
    let s = settings::get();
    let start_hidden = std::env::args().any(|a| a == HIDDEN_ARG);
    STATE.lock().mode = if s.mode == "dropdown" { "dropdown".into() } else { "window".into() };

    let bg = if s.see_through {
        Color(0, 0, 0, 0)
    } else if s.theme == "light" {
        Color(0xec, 0xec, 0xee, 0xff)
    } else {
        Color(0x2a, 0x2a, 0x2c, 0xff)
    };
    #[allow(unused_mut)]
    let mut b = WebviewWindowBuilder::new(app, MAIN, WebviewUrl::default())
        .title("VIBE CODE")
        .inner_size(1480.0, 920.0)
        .min_inner_size(760.0, 280.0)
        .center()
        .visible(false)
        .transparent(s.see_through)
        .background_color(bg)
        // the page draws its own title bar and window buttons (drop-down mode hides them)
        .decorations(false)
        .shadow(true)
        .zoom_hotkeys_enabled(false);
    #[cfg(target_os = "macos")]
    {
        b = b
            .decorations(true)
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true)
            .traffic_light_position(tauri::LogicalPosition::new(12.0, 12.0));
    }
    let w = b.build()?;
    if let Ok(icon) = Image::from_path(crate::core::resource("icon.png")) {
        let _ = w.set_icon(icon);
    }

    let tray_icon = Image::from_path(crate::core::resource("tray.png")).or_else(|_| Image::from_bytes(include_bytes!("../../resources/tray.png")))?;
    let tray = TrayIconBuilder::with_id("main")
        .icon(tray_icon)
        .show_menu_on_left_click(false)
        .on_menu_event(|_app, ev| on_menu(ev))
        .on_tray_icon_event(|_tray, ev| match ev {
            TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } => toggle(),
            TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } => show(),
            _ => {}
        })
        .build(app)?;
    STATE.lock().tray = Some(tray);

    apply_login_item();
    register_hotkey();
    build_tray_menu();

    if dropdown() {
        apply_dropdown(&w.as_ref().window(), !start_hidden);
    } else {
        set_opacity(&w.as_ref().window(), win_opacity());
        if !start_hidden {
            let _ = w.show();
        }
    }
    Ok(())
}

/// Main window events (wired in lib.rs).
pub fn on_event(window: &Window, event: &WindowEvent) {
    match event {
        // The close button only tucks the window away: chats keep running and the tray icon
        // brings it back. Quitting for real is "Quit VIBE CODE" in the tray menu.
        WindowEvent::CloseRequested { api, .. } if !QUITTING.load(Ordering::SeqCst) => {
            api.prevent_close();
            if dropdown() {
                hide_dropdown();
            } else {
                let _ = window.hide();
            }
        }
        WindowEvent::Resized(_) => {
            if !ANIMATING.load(Ordering::SeqCst) {
                if dropdown() {
                    // dragging the bottom edge in drop-down mode changes the remembered height
                    if let Some(w) = main_window()
                        && w.is_visible().unwrap_or(false)
                        && let (Some(wa), Ok(size)) = (target_work_area(&w), w.outer_size())
                    {
                        let ratio = ((size.height as f64 / wa.size.height as f64).clamp(0.25, 1.0) * 100.0).round() / 100.0;
                        if (ratio - settings::get().drop_height).abs() >= 0.01 {
                            tauri::async_runtime::spawn(async move {
                                settings::set(|s| s.drop_height = ratio).await;
                                build_tray_menu();
                            });
                        }
                    }
                } else {
                    push_state();
                }
            }
        }
        WindowEvent::Focused(false)
            if dropdown() && settings::get().hide_on_blur && !ANIMATING.load(Ordering::SeqCst) => {
                // let a click on the tray icon (which also blurs) toggle instead of double-hiding
                tauri::async_runtime::spawn(async {
                    tokio::time::sleep(Duration::from_millis(120)).await;
                    if let Some(w) = main_window()
                        && !w.is_focused().unwrap_or(true)
                        && w.is_visible().unwrap_or(false)
                        && !ANIMATING.load(Ordering::SeqCst)
                    {
                        slide_up(&w);
                    }
                });
            }
        _ => {}
    }
}

/* ---------------- commands ---------------- */

#[tauri::command]
pub fn win_state() -> WindowState {
    window_state()
}

#[tauri::command]
pub fn win_minimize() {
    if let Some(w) = main_window() {
        let _ = w.minimize();
    }
}

#[tauri::command]
pub fn win_toggle_maximize() {
    if let Some(w) = main_window() {
        let _ = if w.is_maximized().unwrap_or(false) { w.unmaximize() } else { w.maximize() };
        push_state();
    }
}

#[tauri::command]
pub fn win_close() {
    if let Some(w) = main_window() {
        let _ = w.close();
    }
}

#[tauri::command]
pub fn win_hide() {
    hide_dropdown();
}

#[tauri::command]
pub async fn win_set_mode(mode: String) -> Res<()> {
    set_mode(mode).await;
    Ok(())
}

/// Number on the taskbar icon: a PNG drawn by the page (data: URL), null clears it.
#[tauri::command]
pub fn win_badge(png: Option<String>, label: String) {
    let _ = label;
    let Some(w) = main_window() else { return };
    let icon = png.and_then(|p| p.split_once(',').map(|(_, b)| b.to_owned())).and_then(|b| B64.decode(b).ok()).and_then(|bytes| Image::from_bytes(&bytes).ok());
    #[cfg(windows)]
    let _ = w.set_overlay_icon(icon);
    #[cfg(not(windows))]
    let _ = (w, icon);
}
