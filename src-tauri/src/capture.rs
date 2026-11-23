//! Screen capture: grabs the monitor under the mouse (the title-bar button hides the app first, the
//! hotkey grabs the screen as it is, app included), and shows it in a frameless
//! full-screen window where you pick a region and mark it up (boxes, arrows, pen, text, numbered
//! steps, mosaic…). The result goes to the clipboard and, by default, into the active chat.
//!
//! Like the Zalo toast, the editor page is a single HTML file served from its own `capture://`
//! protocol, so it loads instantly and can `invoke` the commands below.

use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};

use parking_lot::Mutex;
use tauri::{
    Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder,
    ipc::{InvokeBody, Request, Response},
};
use tauri_plugin_dialog::DialogExt;

use crate::core::{Res, app, emit, err, main_window, now_ms};

const LABEL: &str = "capture";
/// Global shortcut that starts a capture.
pub const HOTKEY: &str = "Ctrl+Alt+Z";
const PAGE: &str = include_str!("capture.html");

/// The grabbed screen: width, height, RGBA pixels.
static FRAME: Mutex<Option<(u32, u32, Vec<u8>)>> = Mutex::new(None);
/// The app window was on screen before the capture, so it comes back after.
static WAS_VISIBLE: AtomicBool = AtomicBool::new(false);

fn register_protocol() -> bool {
    static DONE: OnceLock<bool> = OnceLock::new();
    *DONE.get_or_init(|| {
        let plugin = tauri::plugin::Builder::<tauri::Wry>::new("vibe-capture")
            .register_uri_scheme_protocol("capture", |_ctx, _req| {
                tauri::http::Response::builder()
                    .header("Content-Type", "text/html; charset=utf-8")
                    .body(PAGE.as_bytes().to_vec())
                    .expect("static response")
            })
            .build();
        app().plugin(plugin).is_ok()
    })
}

/// Grab the screen and open the editor over it. `hide`: take the app off the screen first (the
/// title-bar button); the hotkey keeps it, so what is on screen, VIBE CODE included, is grabbed.
#[tauri::command]
pub async fn capture_start(hide: bool) -> Res<()> {
    if app().get_webview_window(LABEL).is_some() || !register_protocol() {
        return Ok(());
    }
    let main = main_window();
    let visible = main.as_ref().is_some_and(|w| w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false));
    WAS_VISIBLE.store(visible, Ordering::SeqCst);
    if let Some(w) = &main
        && visible
        && hide
    {
        let _ = w.hide();
        // let the window actually leave the screen before it is grabbed
        tokio::time::sleep(std::time::Duration::from_millis(220)).await;
    }

    // the monitor under the mouse
    let cursor = app().cursor_position().unwrap_or_default();
    let monitors = app().available_monitors().unwrap_or_default();
    let monitor = monitors
        .iter()
        .find(|m| {
            let (p, s) = (m.position(), m.size());
            cursor.x >= p.x as f64 && cursor.y >= p.y as f64 && cursor.x < (p.x + s.width as i32) as f64 && cursor.y < (p.y + s.height as i32) as f64
        })
        .or(monitors.first())
        .cloned()
        .ok_or_else(|| err("no monitor"))?;
    let (pos, size) = (*monitor.position(), *monitor.size());

    let grabbed = tokio::task::spawn_blocking(move || grab(pos.x, pos.y, size.width, size.height)).await?;
    let Some(pixels) = grabbed else {
        restore(false);
        return Err(err("could not capture the screen"));
    };
    *FRAME.lock() = Some((size.width, size.height, pixels));

    let url = "capture://localhost/".parse().map_err(|_| err("bad url"))?;
    let built = WebviewWindowBuilder::new(app(), LABEL, WebviewUrl::CustomProtocol(url))
        .title("VIBE CODE capture")
        .visible(false)
        .decorations(false)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .skip_taskbar(true)
        .always_on_top(true)
        .shadow(false)
        .build();
    match built {
        Ok(w) => {
            let _ = w.set_position(PhysicalPosition::new(pos.x, pos.y));
            let _ = w.set_size(PhysicalSize::new(size.width, size.height));
            Ok(())
        }
        Err(e) => {
            FRAME.lock().take();
            restore(false);
            Err(err(e.to_string()))
        }
    }
}

/// The page asks for the grabbed screen: 8 bytes (width, height as u32 LE) then RGBA pixels.
#[tauri::command]
pub fn capture_frame() -> Response {
    let guard = FRAME.lock();
    let Some((w, h, px)) = guard.as_ref() else { return Response::new(Vec::new()) };
    let mut out = Vec::with_capacity(8 + px.len());
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(px);
    Response::new(out)
}

/// The page has drawn the screen: show it (no flash of an empty window).
#[tauri::command]
pub fn capture_ready() {
    if let Some(w) = app().get_webview_window(LABEL) {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Esc / ✕: close without keeping anything.
#[tauri::command]
pub fn capture_cancel() {
    close();
    restore(false);
}

/// The finished picture (PNG body). `x-action`: "chat" (clipboard + active chat), "copy"
/// (clipboard only) or "save" (a file you pick).
#[tauri::command]
pub async fn capture_done(request: Request<'_>) -> Res<()> {
    let InvokeBody::Raw(png) = request.body() else { return Err(err("expected PNG bytes")) };
    let action = request.headers().get("x-action").and_then(|v| v.to_str().ok()).unwrap_or("chat").to_owned();
    let png = png.clone();
    close();
    if action == "save" {
        let name = format!("Screenshot {}.png", chrono::Local::now().format("%Y-%m-%d %H%M%S"));
        let picked = app().dialog().file().set_file_name(name).add_filter("PNG image", &["png"]).blocking_save_file();
        if let Some(path) = picked.and_then(|p| p.into_path().ok()) {
            tokio::fs::write(path, &png).await?;
        }
        restore(false);
        return Ok(());
    }
    let _ = tokio::task::spawn_blocking({
        let png = png.clone();
        move || copy_png(&png)
    })
    .await;
    if action == "chat" {
        let dir = std::env::temp_dir().join("tinjs-paste");
        tokio::fs::create_dir_all(&dir).await?;
        let path = dir.join(format!("capture-{}.png", now_ms()));
        tokio::fs::write(&path, &png).await?;
        restore(true);
        emit("capture:done", (path.to_string_lossy().to_string(),));
    } else {
        restore(false);
    }
    Ok(())
}

fn close() {
    FRAME.lock().take();
    if let Some(w) = app().get_webview_window(LABEL) {
        let _ = w.destroy();
    }
}

/// Bring the app back if it was up before the capture (or `force` it up).
fn restore(force: bool) {
    if force || WAS_VISIBLE.load(Ordering::SeqCst) {
        crate::windowing::show();
    }
}

/// PNG → CF_DIB on the clipboard, so it pastes into any app.
fn copy_png(png: &[u8]) -> Option<()> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png)).read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let channels = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        _ => return None,
    };
    // top-down BGRA
    let mut bgra = Vec::with_capacity((info.width * info.height * 4) as usize);
    for px in buf[..info.buffer_size()].chunks_exact(channels) {
        bgra.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    set_clipboard_dib(info.width, info.height, &bgra)
}

#[cfg(windows)]
fn grab(x: i32, y: i32, w: u32, h: u32) -> Option<Vec<u8>> {
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC,
        SRCCOPY, SelectObject,
    };
    // SAFETY: plain GDI calls on handles created here and released on every path.
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w as i32, h as i32);
        let old = SelectObject(mem, bmp.into());
        let ok = BitBlt(mem, 0, 0, w as i32, h as i32, Some(screen), x, y, SRCCOPY | CAPTUREBLT).is_ok();
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32), // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut px = vec![0u8; (w * h * 4) as usize];
        let lines = if ok { GetDIBits(mem, bmp, 0, h, Some(px.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS) } else { 0 };
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        if lines == 0 {
            return None;
        }
        for p in px.chunks_exact_mut(4) {
            p.swap(0, 2); // BGRA → RGBA
            p[3] = 255;
        }
        Some(px)
    }
}

#[cfg(windows)]
fn set_clipboard_dib(w: u32, h: u32, bgra: &[u8]) -> Option<()> {
    use windows::Win32::{
        Foundation::{GlobalFree, HANDLE, HGLOBAL},
        Graphics::Gdi::{BI_RGB, BITMAPINFOHEADER},
        System::{
            DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
        },
    };
    const CF_DIB: u32 = 8;
    let header = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: w as i32,
        biHeight: h as i32, // CF_DIB readers expect bottom-up
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: w * h * 4,
        ..Default::default()
    };
    let hsize = size_of::<BITMAPINFOHEADER>();
    let row = (w * 4) as usize;
    // SAFETY: the global block is sized for header + pixels; the clipboard owns it once set,
    // and is closed on every path after opening.
    unsafe {
        let mem: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, hsize + bgra.len()).ok()?;
        let dst = GlobalLock(mem).cast::<u8>();
        if dst.is_null() {
            let _ = GlobalFree(Some(mem));
            return None;
        }
        std::ptr::copy_nonoverlapping((&raw const header).cast::<u8>(), dst, hsize);
        for (i, line) in bgra.chunks_exact(row).rev().enumerate() {
            std::ptr::copy_nonoverlapping(line.as_ptr(), dst.add(hsize + i * row), row);
        }
        let _ = GlobalUnlock(mem);
        if OpenClipboard(None).is_err() {
            let _ = GlobalFree(Some(mem));
            return None;
        }
        let _ = EmptyClipboard();
        let set = SetClipboardData(CF_DIB, Some(HANDLE(mem.0)));
        let _ = CloseClipboard();
        if set.is_err() {
            let _ = GlobalFree(Some(mem));
            return None;
        }
    }
    Some(())
}

#[cfg(not(windows))]
fn grab(_x: i32, _y: i32, _w: u32, _h: u32) -> Option<Vec<u8>> {
    None
}

#[cfg(not(windows))]
fn set_clipboard_dib(_w: u32, _h: u32, _bgra: &[u8]) -> Option<()> {
    None
}
