use std::{
    net::{Ipv4Addr, SocketAddrV4, UdpSocket},
    path::Path as FilePath,
    sync::LazyLock,
};

use anyhow::{Context, Result, bail};
use axum::{
    Form, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use tauri_plugin_dialog::DialogExt;
use tokio::{
    net::TcpListener,
    sync::{Mutex, oneshot},
};
use uuid::Uuid;

use crate::core::{self, Res};

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    id: String,
    title: String,
    date: String,
    venue: String,
    #[serde(default)]
    completed_at: Option<i64>,
    guests: Vec<Guest>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Guest {
    name: String,
    email: String,
    submitted_at: i64,
}

#[derive(Deserialize)]
pub struct EventDraft {
    title: String,
    date: String,
    venue: String,
}

#[derive(Deserialize)]
pub struct Registration {
    name: String,
    email: String,
}

struct Preview {
    event_id: String,
    stop: oneshot::Sender<()>,
}

#[derive(Clone)]
struct PreviewState {
    event_id: String,
    key: String,
}

static EVENTS: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
static PREVIEW: LazyLock<Mutex<Option<Preview>>> = LazyLock::new(|| Mutex::new(None));

async fn load() -> Result<Vec<Event>> {
    load_file(&core::user_data().join("events.json")).await
}

async fn load_file(path: &FilePath) -> Result<Vec<Event>> {
    match tokio::fs::read(path).await {
        Ok(data) => Ok(serde_json::from_slice(&data).context("Không đọc được dữ liệu sự kiện")?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

async fn save(events: &[Event]) -> Result<()> {
    save_file(&core::user_data().join("events.json"), events).await
}

async fn save_file(path: &FilePath, events: &[Event]) -> Result<()> {
    core::write_atomic(path, serde_json::to_vec(events)?).await?;
    Ok(())
}

fn field(value: &str, max: usize) -> Result<String> {
    let value = value.trim();
    if value.chars().count() > max || value.chars().any(char::is_control) {
        bail!("Nội dung vượt giới hạn hoặc chứa ký tự không hợp lệ.");
    }
    Ok(value.to_owned())
}

#[tauri::command]
pub async fn event_list() -> Res<Vec<Event>> {
    let _guard = EVENTS.lock().await;
    Ok(load().await?)
}

fn validated_draft(draft: EventDraft) -> Result<EventDraft> {
    let title = field(&draft.title, 120)?;
    if title.is_empty() {
        bail!("Nhập tên sự kiện.");
    }
    if !draft.date.is_empty() {
        chrono::NaiveDate::parse_from_str(&draft.date, "%Y-%m-%d")
            .context("Ngày tổ chức không hợp lệ")?;
    }
    Ok(EventDraft {
        title,
        date: draft.date,
        venue: field(&draft.venue, 180)?,
    })
}

#[tauri::command]
pub async fn event_create(draft: EventDraft) -> Res<Event> {
    let draft = validated_draft(draft)?;
    let event = Event {
        id: Uuid::new_v4().to_string(),
        title: draft.title,
        date: draft.date,
        venue: draft.venue,
        completed_at: None,
        guests: Vec::new(),
    };
    let _guard = EVENTS.lock().await;
    let mut events = load().await?;
    events.insert(0, event.clone());
    save(&events).await?;
    Ok(event)
}

#[tauri::command]
pub async fn event_update(id: String, draft: EventDraft) -> Res<Event> {
    let draft = validated_draft(draft)?;
    let _guard = EVENTS.lock().await;
    let mut events = load().await?;
    let event = events
        .iter_mut()
        .find(|event| event.id == id)
        .context("Không tìm thấy sự kiện.")?;
    event.title = draft.title;
    event.date = draft.date;
    event.venue = draft.venue;
    let updated = event.clone();
    save(&events).await?;
    core::emit("events:changed", ());
    Ok(updated)
}

#[tauri::command]
pub async fn event_set_completed(id: String, completed: bool) -> Res<Event> {
    let _guard = EVENTS.lock().await;
    let mut events = load().await?;
    let event = events
        .iter_mut()
        .find(|event| event.id == id)
        .context("Không tìm thấy sự kiện.")?;
    event.completed_at = if completed {
        event.completed_at.or_else(|| Some(core::now_ms() as i64))
    } else {
        None
    };
    let updated = event.clone();
    save(&events).await?;
    if completed {
        stop_preview_for(&id).await;
    }
    core::emit("events:changed", ());
    Ok(updated)
}

#[tauri::command]
pub async fn event_delete(id: String) -> Res<()> {
    let _guard = EVENTS.lock().await;
    let mut events = load().await?;
    let index = events
        .iter()
        .position(|event| event.id == id)
        .context("Không tìm thấy sự kiện.")?;
    events.remove(index);
    save(&events).await?;
    stop_preview_for(&id).await;
    core::emit("events:changed", ());
    Ok(())
}

async fn stop_preview_for(id: &str) {
    let mut active = PREVIEW.lock().await;
    if active
        .as_ref()
        .is_some_and(|preview| preview.event_id == id)
    {
        let _ = active.take().unwrap().stop.send(());
    }
}

async fn register(event_id: &str, input: Registration) -> Result<Event> {
    let updated = register_in(&core::user_data().join("events.json"), event_id, input).await?;
    core::emit("events:changed", ());
    Ok(updated)
}

async fn register_in(path: &FilePath, event_id: &str, input: Registration) -> Result<Event> {
    let name = field(&input.name, 100)?;
    let email = field(&input.email, 160)?.to_lowercase();
    let valid_email = email.split_once('@').is_some_and(|(user, host)| {
        !user.is_empty()
            && host
                .split_once('.')
                .is_some_and(|(a, b)| !a.is_empty() && !b.is_empty())
    }) && !email.contains(char::is_whitespace)
        && email.matches('@').count() == 1;
    if name.is_empty() || !valid_email {
        bail!("Nhập họ tên và địa chỉ email hợp lệ.");
    }
    let _guard = EVENTS.lock().await;
    let mut events = load_file(path).await?;
    let event = events
        .iter_mut()
        .find(|event| event.id == event_id)
        .context("Không tìm thấy sự kiện.")?;
    if event.completed_at.is_some() {
        bail!("Sự kiện đã hoàn thành và đã đóng đăng ký.");
    }
    if event.guests.iter().any(|guest| guest.email == email) {
        bail!("Email này đã đăng ký thử.");
    }
    if event.guests.len() >= 1000 {
        bail!("Danh sách thử đã đạt 1.000 lượt đăng ký.");
    }
    event.guests.push(Guest {
        name,
        email,
        submitted_at: core::now_ms() as i64,
    });
    let updated = event.clone();
    save_file(path, &events).await?;
    Ok(updated)
}

#[tauri::command]
pub async fn event_register(id: String, registration: Registration) -> Res<Event> {
    Ok(register(&id, registration).await?)
}

pub(crate) fn local_ip() -> Result<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    // Select the outgoing interface without sending a datagram.
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 80))?;
    match socket.local_addr()?.ip() {
        std::net::IpAddr::V4(ip) if ip.is_private() => Ok(ip),
        _ => bail!("Không tìm thấy địa chỉ mạng nội bộ. Hãy kiểm tra Wi-Fi."),
    }
}

#[tauri::command]
pub async fn event_preview_start(id: String) -> Res<String> {
    let _guard = EVENTS.lock().await;
    let events = load().await?;
    let event = events
        .iter()
        .find(|event| event.id == id)
        .context("Không tìm thấy sự kiện.")?;
    if event.completed_at.is_some() {
        return Err(core::err("Mở lại sự kiện để bật đăng ký thử."));
    }
    let mut active = PREVIEW.lock().await;
    let ip = local_ip()?;
    let listener = TcpListener::bind(SocketAddrV4::new(ip, 0))
        .await
        .context("Không mở được trang đăng ký trong mạng nội bộ")?;
    let key = Uuid::new_v4().simple().to_string();
    let url = format!(
        "http://{}/r/{key}",
        listener
            .local_addr()
            .context("Không đọc được địa chỉ đăng ký")?
    );
    let state = PreviewState {
        event_id: id.clone(),
        key,
    };
    let router = Router::new()
        .route("/r/{key}", get(show_form).post(submit_form))
        .layer(DefaultBodyLimit::max(4096))
        .with_state(state);
    let (stop, cancel) = oneshot::channel();
    if let Some(previous) = active.replace(Preview { event_id: id, stop }) {
        let _ = previous.stop.send(());
    }
    tauri::async_runtime::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = cancel.await;
            })
            .await;
    });
    Ok(url)
}

#[tauri::command]
pub async fn event_preview_stop() {
    if let Some(preview) = PREVIEW.lock().await.take() {
        let _ = preview.stop.send(());
    }
}

fn escaped(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn page(status: StatusCode, title: &str, content: &str) -> Response {
    let html = include_str!("event_preview.html")
        .split_once("{{title}}")
        .expect("preview title placeholder");
    let (middle, tail) = html
        .1
        .split_once("{{content}}")
        .expect("preview content placeholder");
    let html = [html.0, &escaped(title), middle, content, tail].concat();
    (status, [
        (header::CACHE_CONTROL, "no-store"),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::CONTENT_SECURITY_POLICY, "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"),
    ], Html(html)).into_response()
}

const FORM: &str = r#"<form method="post"><label>Họ tên<input name="name" autocomplete="name" maxlength="100" required></label><label>Email<input name="email" type="email" autocomplete="email" maxlength="160" required></label><button>Gửi đăng ký thử</button></form>"#;

async fn show_form(State(state): State<PreviewState>, Path(key): Path<String>) -> Response {
    if key != state.key {
        return StatusCode::NOT_FOUND.into_response();
    }
    let _guard = EVENTS.lock().await;
    match load().await {
        Ok(events) => match events.iter().find(|event| event.id == state.event_id) {
            Some(event) if event.completed_at.is_none() => {
                let details = [event.date.as_str(), event.venue.as_str()]
                    .into_iter()
                    .filter(|v| !v.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · ");
                page(
                    StatusCode::OK,
                    &event.title,
                    &format!(
                        "<p>{}</p><p>Dùng tên và email giả để thử.</p>{FORM}",
                        escaped(&details)
                    ),
                )
            }
            _ => StatusCode::NOT_FOUND.into_response(),
        },
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn submit_form(
    State(state): State<PreviewState>,
    Path(key): Path<String>,
    Form(input): Form<Registration>,
) -> Response {
    if key != state.key {
        return StatusCode::NOT_FOUND.into_response();
    }
    match register(&state.event_id, input).await {
        Ok(_) => page(
            StatusCode::OK,
            "Đã nhận đăng ký thử",
            "<p>Bạn có thể đóng trang này và xem danh sách khách trong VIBE CODE.</p>",
        ),
        Err(e) => {
            let message = e.to_string();
            let public = if e.downcast_ref::<std::io::Error>().is_some() || e.source().is_some() {
                "Không lưu được dữ liệu trên máy tổ chức. Hãy báo người tổ chức."
            } else {
                &message
            };
            page(
                StatusCode::BAD_REQUEST,
                "Chưa ghi được đăng ký",
                &format!("<p>{}</p>{FORM}", escaped(public)),
            )
        }
    }
}

fn csv_cell(value: &str) -> String {
    let neutralize = value.starts_with(['=', '+', '-', '@', '\t', '\r', '\n']);
    format!(
        "\"{}{}\"",
        if neutralize { "'" } else { "" },
        value.replace('"', "\"\"")
    )
}

fn guest_csv(guests: &[Guest]) -> String {
    let mut csv = String::from("\u{feff}Họ tên,Email,Thời điểm đăng ký (UTC)\r\n");
    for guest in guests {
        let time = chrono::DateTime::from_timestamp_millis(guest.submitted_at)
            .map(|time| time.to_rfc3339())
            .unwrap_or_default();
        csv.push_str(
            &[
                csv_cell(&guest.name),
                csv_cell(&guest.email),
                csv_cell(&time),
            ]
            .join(","),
        );
        csv.push_str("\r\n");
    }
    csv
}

#[tauri::command]
pub async fn event_export(id: String) -> Res<Option<String>> {
    let csv = {
        let _guard = EVENTS.lock().await;
        let events = load().await?;
        let event = events
            .iter()
            .find(|event| event.id == id)
            .context("Không tìm thấy sự kiện.")?;
        guest_csv(&event.guests)
    };
    let picked = tokio::task::spawn_blocking(|| {
        core::app()
            .dialog()
            .file()
            .set_file_name("khach-dang-ky-thu.csv")
            .add_filter("CSV", &["csv"])
            .blocking_save_file()
    })
    .await?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    let path = picked.into_path()?;
    tokio::fs::write(&path, csv).await?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_preserves_vietnamese_and_neutralizes_formula_cells() {
        let csv = guest_csv(&[Guest {
            name: "=SUM(1,2)".into(),
            email: "khach@example.com".into(),
            submitted_at: 0,
        }]);
        assert!(csv.starts_with("\u{feff}Họ tên,Email,"));
        assert!(csv.contains("\"'=SUM(1,2)\",\"khach@example.com\""));
        assert_eq!(csv_cell("An \"Nguyễn\""), "\"An \"\"Nguyễn\"\"\"");
    }

    #[tokio::test]
    async fn preview_escapes_event_names_and_rejects_wrong_links() {
        let state = PreviewState {
            event_id: "fixture".into(),
            key: "valid".into(),
        };
        let denied = show_form(State(state), Path("wrong".into())).await;
        assert_eq!(denied.status(), StatusCode::NOT_FOUND);
        let response = page(StatusCode::OK, "<script>{{content}}</script>", FORM);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap();
        let html = std::str::from_utf8(&body).unwrap();
        assert!(html.contains("&lt;script&gt;{{content}}&lt;/script&gt;"));
        assert!(html.contains(FORM));
        assert!(!html.contains("<script>"));
    }

    #[tokio::test]
    async fn registration_is_persisted_once_and_exportable() {
        let path = std::env::temp_dir().join(format!("vibe-event-{}.json", Uuid::new_v4()));
        let event = Event {
            id: "test-event".into(),
            title: "Hội thảo thử".into(),
            date: "2026-11-22".into(),
            venue: String::new(),
            completed_at: None,
            guests: Vec::new(),
        };
        save_file(&path, &[event]).await.unwrap();
        let registration = || Registration {
            name: "Nguyễn An".into(),
            email: "An@example.com".into(),
        };
        let (first, second) = tokio::join!(
            register_in(&path, "test-event", registration()),
            register_in(&path, "test-event", registration()),
        );
        assert_ne!(first.is_ok(), second.is_ok());
        let saved = load_file(&path).await.unwrap();
        tokio::fs::remove_file(path).await.unwrap();
        assert_eq!(saved[0].guests.len(), 1);
        assert!(guest_csv(&saved[0].guests).contains("\"Nguyễn An\",\"an@example.com\""));
    }
}
