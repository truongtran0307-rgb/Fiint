//! "Bảng việc trên iPhone": Zalo chỉ đăng nhập được ở một nơi, nên máy Mac giữ phiên Zalo và
//! phát bảng "Việc của tôi" qua Wi-Fi. iPhone mở link (quét QR), xem việc và đổi trạng thái;
//! mọi thay đổi đi qua cùng kho việc của app, nên bảng trên Mac và trên iPhone luôn khớp nhau.
//!
//! Link có một khoá ngẫu nhiên và chỉ mở trên địa chỉ mạng nội bộ. Khoá và cổng được lưu lại
//! để link (và biểu tượng "Thêm vào MH chính" trên iPhone) vẫn dùng được sau khi mở lại app.

use std::{
    net::SocketAddrV4,
    path::PathBuf,
    sync::LazyLock,
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tokio::{
    net::TcpListener,
    sync::{Mutex, oneshot},
};
use uuid::Uuid;

use crate::{
    core::{self, Res},
    events::local_ip,
    zalo_tasks,
};

/// cổng thử trước; nếu bận thì lấy cổng bất kỳ rồi nhớ lại cổng đó
const PREFERRED_PORT: u16 = 47_821;

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
struct Config {
    key: String,
    port: u16,
    enabled: bool,
}

struct Running {
    url: String,
    stop: oneshot::Sender<()>,
}

#[derive(Clone)]
struct Key(String);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PhoneInfo {
    /// link đang mở, hoặc None khi đã tắt
    url: Option<String>,
    /// mã QR (SVG) của link
    qr: Option<String>,
    /// lỗi lần tự bật gần nhất (vd. chưa có Wi-Fi)
    error: Option<String>,
}

static SERVER: LazyLock<Mutex<Option<Running>>> = LazyLock::new(|| Mutex::new(None));
static LAST_ERROR: LazyLock<parking_lot::Mutex<Option<String>>> = LazyLock::new(Default::default);

fn file() -> PathBuf {
    core::user_data().join("zalo").join("phone.json")
}

async fn load() -> Config {
    let mut cfg: Config = tokio::fs::read(file())
        .await
        .ok()
        .and_then(|d| serde_json::from_slice(&d).ok())
        .unwrap_or_default();
    if cfg.key.len() < 16 {
        cfg.key = new_key();
    }
    cfg
}

async fn save(cfg: &Config) -> Result<()> {
    core::write_atomic(file(), serde_json::to_vec(cfg)?).await?;
    Ok(())
}

fn new_key() -> String {
    Uuid::new_v4().simple().to_string()[..20].to_owned()
}

fn qr_svg(url: &str) -> Option<String> {
    let code = qrcode::QrCode::new(url.as_bytes()).ok()?;
    Some(
        code.render::<qrcode::render::svg::Color>()
            .min_dimensions(200, 200)
            .quiet_zone(true)
            .dark_color(qrcode::render::svg::Color("#000000"))
            .light_color(qrcode::render::svg::Color("#ffffff"))
            .build(),
    )
}

fn info(url: Option<String>) -> PhoneInfo {
    PhoneInfo {
        qr: url.as_deref().and_then(qr_svg),
        url,
        error: LAST_ERROR.lock().clone(),
    }
}

async fn start(cfg: &mut Config) -> Result<String> {
    let mut active = SERVER.lock().await;
    if let Some(prev) = active.take() {
        let _ = prev.stop.send(());
    }
    let ip = local_ip()?;
    let listener = match TcpListener::bind(SocketAddrV4::new(ip, if cfg.port == 0 { PREFERRED_PORT } else { cfg.port })).await {
        Ok(l) => l,
        Err(_) => TcpListener::bind(SocketAddrV4::new(ip, 0))
            .await
            .context("Không mở được bảng việc trong mạng nội bộ")?,
    };
    let addr = listener.local_addr().context("Không đọc được địa chỉ bảng việc")?;
    cfg.port = addr.port();
    let url = format!("http://{addr}/z/{}", cfg.key);
    let (stop, cancel) = oneshot::channel();
    let router = router(cfg.key.clone());
    tauri::async_runtime::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = cancel.await;
            })
            .await;
    });
    *active = Some(Running { url: url.clone(), stop });
    *LAST_ERROR.lock() = None;
    Ok(url)
}

fn router(key: String) -> Router {
    Router::new()
        .route("/z/{key}", get(show_page))
        .route("/z/{key}/api", get(api_state))
        .route("/z/{key}/api/status", post(api_status))
        .route("/z/{key}/api/analyze", post(api_analyze))
        .route("/z/{key}/icon.png", get(icon))
        .layer(DefaultBodyLimit::max(4096))
        .with_state(Key(key))
}

/// Mở lại bảng khi app khởi động nếu lần trước đang bật.
pub fn init() {
    tauri::async_runtime::spawn(async {
        let mut cfg = load().await;
        if !cfg.enabled {
            return;
        }
        match start(&mut cfg).await {
            Ok(_) => {
                let _ = save(&cfg).await;
            }
            Err(e) => *LAST_ERROR.lock() = Some(format!("{e:#}")),
        }
    });
}

/* ------------------------------ commands ------------------------------ */

#[tauri::command]
pub async fn zalo_phone_info() -> PhoneInfo {
    let url = SERVER.lock().await.as_ref().map(|r| r.url.clone());
    info(url)
}

#[tauri::command]
pub async fn zalo_phone_start() -> Res<PhoneInfo> {
    let mut cfg = load().await;
    let url = start(&mut cfg).await?;
    cfg.enabled = true;
    save(&cfg).await?;
    Ok(info(Some(url)))
}

#[tauri::command]
pub async fn zalo_phone_stop() -> Res<PhoneInfo> {
    if let Some(r) = SERVER.lock().await.take() {
        let _ = r.stop.send(());
    }
    let mut cfg = load().await;
    cfg.enabled = false;
    save(&cfg).await?;
    *LAST_ERROR.lock() = None;
    Ok(info(None))
}

/// Đổi khoá: link cũ (kể cả biểu tượng trên màn hình chính iPhone) thôi hoạt động.
#[tauri::command]
pub async fn zalo_phone_new_link() -> Res<PhoneInfo> {
    let mut cfg = load().await;
    cfg.key = new_key();
    let running = SERVER.lock().await.is_some();
    let url = if running { Some(start(&mut cfg).await?) } else { None };
    save(&cfg).await?;
    Ok(info(url))
}

/* ------------------------------ web ------------------------------ */

fn denied() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

fn no_store<T: IntoResponse>(body: T) -> Response {
    ([(header::CACHE_CONTROL, "no-store"), (header::REFERRER_POLICY, "no-referrer")], body).into_response()
}

async fn show_page(State(Key(k)): State<Key>, Path(key): Path<String>) -> Response {
    if key != k {
        return denied();
    }
    (
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::REFERRER_POLICY, "no-referrer"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src 'self' https: data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
            ),
        ],
        Html(include_str!("zalo_phone.html")),
    )
        .into_response()
}

async fn icon(State(Key(k)): State<Key>, Path(key): Path<String>) -> Response {
    if key != k {
        return denied();
    }
    ([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "max-age=86400")], include_bytes!("../icons/128x128@2x.png").as_slice()).into_response()
}

async fn api_state(State(Key(k)): State<Key>, Path(key): Path<String>) -> Response {
    if key != k {
        return denied();
    }
    no_store(Json(zalo_tasks::get_tasks()))
}

#[derive(Deserialize)]
struct StatusBody {
    id: String,
    status: String,
}

async fn api_status(State(Key(k)): State<Key>, Path(key): Path<String>, Json(body): Json<StatusBody>) -> Response {
    if key != k {
        return denied();
    }
    if !matches!(body.status.as_str(), "todo" | "doing" | "done") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    no_store(Json(zalo_tasks::set_status(&body.id, &body.status)))
}

async fn api_analyze(State(Key(k)): State<Key>, Path(key): Path<String>) -> Response {
    if key != k {
        return denied();
    }
    tauri::async_runtime::spawn(zalo_tasks::analyze());
    no_store(Json(serde_json::json!({ "ok": true })))
}
