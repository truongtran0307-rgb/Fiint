//! The Zalo Web HTTP API, ported from the parts of zca-js 2.3.0 VIBE CODE uses: cookie login
//! (`zalo.js` + `apis/login.js`), QR login (`apis/loginQR.js`), `utils.request` (cookies,
//! redirects) and the endpoints in `apis/*.js` — each method names the file it comes from.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use parking_lot::Mutex;
use reqwest::{
    Method, Url,
    header::{LOCATION, SET_COOKIE},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{
    cookies::CookieJar,
    listen::{Msg, group_message},
    proto::{
        API_TYPE, API_VERSION, ParamsEncryptor, decode_aes, decrypt_resp, encode_aes, encode_aes_utf8_key, jnum, jstr, make_url,
        now_ms, sign_key, truthy, zalo_uuid,
    },
};

/// An error Zalo answered with (`ZaloApiError`): its message, and `error_code` when it gave one.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ZaloError {
    pub message: String,
    pub code: Option<i64>,
}

fn zerr(message: impl Into<String>) -> anyhow::Error {
    ZaloError { message: message.into(), code: None }.into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadType {
    User,
    Group,
}

/// What zca-js keeps of a session (`Credentials`): enough to log in again without a QR scan.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Credentials {
    pub imei: String,
    /// tough-cookie JSON: an array of cookies, or `{cookies: [...]}`
    pub cookie: Value,
    pub user_agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/* ---------------- HTTP: zca-js `request()` ---------------- */

/// `URLSearchParams` serialization (application/x-www-form-urlencoded).
fn form_encode(pairs: &[(String, String)]) -> String {
    let mut u = Url::parse("http://x/").expect("static URL");
    u.query_pairs_mut().extend_pairs(pairs);
    u.query().unwrap_or_default().to_owned()
}

pub enum Body {
    Empty,
    Form(Vec<(String, String)>),
}

pub struct Resp {
    pub status: reqwest::StatusCode,
    pub body: Vec<u8>,
}

impl Resp {
    pub fn json(&self) -> Result<Value> {
        Ok(serde_json::from_slice(&self.body)?)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

pub struct Http {
    client: reqwest::Client,
    pub jar: Mutex<CookieJar>,
    pub user_agent: String,
}

fn headers(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

impl Http {
    pub fn new(user_agent: &str, jar: CookieJar) -> Result<Self> {
        // redirects are followed by hand, so every hop's Set-Cookie lands in the jar
        let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build()?;
        Ok(Self { client, jar: Mutex::new(jar), user_agent: user_agent.to_owned() })
    }

    /// `getDefaultHeaders`. (No Accept-Encoding: responses come back uncompressed.)
    fn default_headers(&self, url: &Url) -> Vec<(String, String)> {
        let origin = url.origin().ascii_serialization();
        let mut h = headers(&[
            ("Accept", "application/json, text/plain, */*"),
            ("Accept-Language", "en-US,en;q=0.9"),
            ("content-type", "application/x-www-form-urlencoded"),
            ("Origin", "https://chat.zalo.me"),
            ("Referer", "https://chat.zalo.me/"),
        ]);
        h.push(("User-Agent".into(), self.user_agent.clone()));
        let cookie = self.jar.lock().cookie_string(&origin);
        if !cookie.is_empty() {
            h.push(("Cookie".into(), cookie));
        }
        h
    }

    /// One request with the session's headers + cookies, storing the cookies it gets back and
    /// following redirects (as GETs, with fresh default headers, as zca-js does).
    pub async fn request(&self, url: &str, method: Method, extra: Vec<(String, String)>, body: Body) -> Result<Resp> {
        let mut url = Url::parse(url).with_context(|| format!("bad URL {url}"))?;
        let mut method = method;
        let mut extra = extra;
        let mut body = Some(body);
        for _ in 0..10 {
            let mut h = self.default_headers(&url);
            for (k, v) in extra.drain(..) {
                match h.iter_mut().find(|(hk, _)| hk.eq_ignore_ascii_case(&k)) {
                    Some(slot) => slot.1 = v,
                    None => h.push((k, v)),
                }
            }
            let mut req = self.client.request(method.clone(), url.clone());
            for (k, v) in h {
                req = req.header(k, v);
            }
            req = match body.take() {
                Some(Body::Form(f)) => req.body(form_encode(&f)),
                _ => req,
            };
            let resp = req.send().await?;
            {
                let mut jar = self.jar.lock();
                for v in resp.headers().get_all(SET_COOKIE) {
                    if let Ok(s) = v.to_str() {
                        jar.set_from_header(s, &url);
                    }
                }
            }
            if let Some(loc) = resp.headers().get(LOCATION).and_then(|v| v.to_str().ok()) {
                url = url.join(loc)?;
                method = Method::GET;
                continue;
            }
            let status = resp.status();
            let body = resp.bytes().await?.to_vec();
            return Ok(Resp { status, body });
        }
        bail!("too many redirects")
    }
}

/// `error_code != 0`, loosely (JS `!=`).
fn code_ok(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Number(n)) => n.as_f64() == Some(0.0),
        Some(Value::String(_)) => jnum(v) == 0.0,
        Some(Value::Bool(b)) => !b,
        _ => false,
    }
}

fn api_error(j: &Value) -> anyhow::Error {
    ZaloError {
        message: j.get("error_message").and_then(Value::as_str).unwrap_or_default().to_owned(),
        code: j.get("error_code").and_then(|c| c.as_i64().or_else(|| c.as_str().and_then(|s| s.parse().ok()))),
    }
    .into()
}

/* ---------------- login with saved cookies ---------------- */

/// A logged-in session: what zca-js keeps in its context + `API`.
pub struct Api {
    pub http: Http,
    pub imei: String,
    pub language: String,
    pub uid: String,
    secret_key: String,
    pub settings: Value,
    extra_ver: Value,
    service_map: Value,
    pub ws_urls: Vec<String>,
}

/// `Zalo.login(credentials)`.
pub async fn login(creds: &Credentials) -> Result<Arc<Api>> {
    if creds.imei.is_empty() || !truthy(Some(&creds.cookie)) || creds.user_agent.is_empty() {
        return Err(zerr("Missing required params"));
    }
    let http = Http::new(&creds.user_agent, CookieJar::from_credentials(&creds.cookie))?;
    let language = creds.language.clone().filter(|l| !l.is_empty()).unwrap_or_else(|| "vi".into());
    let login_data = get_login_info(&http, &creds.imei, &language).await?;
    let server = get_server_info(&http, &creds.imei).await?;
    let info = login_data.as_ref().and_then(|d| d.get("data")).filter(|d| truthy(Some(d))).cloned();
    let Some(info) = info else { return Err(zerr("Đăng nhập thất bại")) };
    let secret_key = info.get("zpw_enk").and_then(Value::as_str).unwrap_or_default().to_owned();
    if secret_key.is_empty() {
        return Err(zerr("Khởi tạo ngữ cảnh thất bại."));
    }
    let settings = server.get("setttings").filter(|v| truthy(Some(v))).or_else(|| server.get("settings")).cloned().unwrap_or(Value::Null);
    Ok(Arc::new(Api {
        uid: jstr(info.get("uid")),
        imei: creds.imei.clone(),
        language,
        secret_key,
        settings,
        extra_ver: server.get("extra_ver").cloned().unwrap_or(Value::Null),
        ws_urls: info.get("zpw_ws").and_then(Value::as_array).map(|a| a.iter().filter_map(|u| u.as_str().map(str::to_owned)).collect()).unwrap_or_default(),
        service_map: info.get("zpw_service_map_v3").cloned().unwrap_or(Value::Null),
        http,
    }))
}

/// `login()` in apis/login.js: the encrypted `getLoginInfo` call. `None` when it didn't decrypt to JSON.
async fn get_login_info(http: &Http, imei: &str, language: &str) -> Result<Option<Value>> {
    let data = json!({ "computer_name": "Web", "imei": imei, "language": language, "ts": now_ms() }).to_string();
    let enc = ParamsEncryptor::new(API_TYPE, imei, now_ms() as u64)?;
    let encoded = encode_aes_utf8_key(&enc.encrypt_key, &data, false, false)?;
    let mut params: Vec<(&str, String)> = vec![
        ("zcid", enc.zcid.clone()),
        ("zcid_ext", enc.zcid_ext.clone()),
        ("enc_ver", ParamsEncryptor::ENC_VER.into()),
        ("params", encoded),
        ("type", API_TYPE.to_string()),
        ("client_version", API_VERSION.to_string()),
    ];
    let signkey = sign_key("getlogininfo", &params);
    params.push(("signkey", signkey));
    params.push(("nretry", "0".into()));
    let url = make_url("https://wpa.chat.zalo.me/api/login/getLoginInfo", &params, true)?;
    let r = http.request(&url, Method::GET, vec![], Body::Empty).await?;
    if !r.status.is_success() {
        bail!("Failed to fetch login info: {}", r.status.canonical_reason().unwrap_or_default());
    }
    let j = r.json()?;
    let Some(data) = j.get("data").and_then(Value::as_str) else { return Ok(None) };
    Ok(decrypt_resp(&enc.encrypt_key, data).ok().filter(|v| !v.is_string() && !v.is_null()))
}

async fn get_server_info(http: &Http, imei: &str) -> Result<Value> {
    let base = [("imei", imei.to_owned()), ("type", API_TYPE.to_string()), ("client_version", API_VERSION.to_string()), ("computer_name", "Web".into())];
    let mut params = base.to_vec();
    params.push(("signkey", sign_key("getserverinfo", &base)));
    let url = make_url("https://wpa.chat.zalo.me/api/login/getServerInfo", &params, false)?;
    let r = http.request(&url, Method::GET, vec![], Body::Empty).await?;
    if !r.status.is_success() {
        bail!("Failed to fetch server info: {}", r.status.canonical_reason().unwrap_or_default());
    }
    let j = r.json()?;
    match j.get("data") {
        Some(d) if !d.is_null() => Ok(d.clone()),
        _ => bail!("Failed to fetch server info: {}", jstr(j.get("error_message"))),
    }
}

/* ---------------- QR login ---------------- */

/// The user agent zca-js logs in with when none is given.
pub const QR_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:133.0) Gecko/20100101 Firefox/133.0";

pub enum QrEvent {
    /// base64 PNG (no data: prefix)
    Generated { image: String },
    Expired,
    Scanned { display_name: String },
    Declined,
    /// the session to save, before the cookie login that follows
    GotLoginInfo(Credentials),
}

/// The error a QR login ends with when it expired, was declined or cancelled.
pub const QR_ABORTED: &str = "Operation aborted";

const SEC_CH_UA: &str = r#""Chromium";v="130", "Google Chrome";v="130", "Not?A_Brand";v="99""#;

fn id_headers(referer: &str, document: bool) -> Vec<(String, String)> {
    let mut h = if document {
        headers(&[
            ("accept", "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7"),
            ("priority", "u=0, i"),
            ("sec-fetch-dest", "document"),
            ("sec-fetch-mode", "navigate"),
            ("upgrade-insecure-requests", "1"),
        ])
    } else {
        headers(&[
            ("accept", "*/*"),
            ("content-type", "application/x-www-form-urlencoded"),
            ("priority", "u=1, i"),
            ("sec-fetch-dest", "empty"),
            ("sec-fetch-mode", "cors"),
        ])
    };
    h.extend(headers(&[
        ("accept-language", "vi-VN,vi;q=0.9,fr-FR;q=0.8,fr;q=0.7,en-US;q=0.6,en;q=0.5"),
        ("sec-ch-ua", SEC_CH_UA),
        ("sec-ch-ua-mobile", "?0"),
        ("sec-ch-ua-platform", "\"Windows\""),
        ("Referer", referer),
        ("Referrer-Policy", "strict-origin-when-cross-origin"),
    ]));
    h
}

const REF_PC: &str = "https://id.zalo.me/account?continue=https%3A%2F%2Fzalo.me%2Fpc";
const REF_CHAT: &str = "https://id.zalo.me/account?continue=https%3A%2F%2Fchat.zalo.me%2F";

fn form(pairs: &[(&str, &str)]) -> Body {
    Body::Form(pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect())
}

async fn id_post(http: &Http, url: &str, referer: &str, pairs: &[(&str, &str)]) -> Result<Value> {
    let mut h = id_headers(referer, false);
    h.push(("sec-fetch-site".into(), "same-origin".into()));
    http.request(url, Method::POST, h, form(pairs)).await?.json()
}

/// `apiloginQR` + the cookie login after it. `on` hears each step; expiry (100 s) and a decline
/// on the phone end the login with [`QR_ABORTED`] right after their event.
pub async fn login_qr(on: impl Fn(QrEvent) + Send + Sync) -> Result<Arc<Api>> {
    let http = Http::new(QR_USER_AGENT, CookieJar::default())?;

    // loadLoginPage: the login script's version
    let mut h = id_headers("https://chat.zalo.me/", true);
    h.push(("cache-control".into(), "max-age=0".into()));
    h.push(("sec-fetch-site".into(), "same-site".into()));
    h.push(("sec-fetch-user".into(), "?1".into()));
    let html = http.request("https://id.zalo.me/account?continue=https%3A%2F%2Fchat.zalo.me%2F", Method::GET, h, Body::Empty).await?.text();
    let version = regex::Regex::new(r"https://stc-zlogin\.zdn\.vn/main-([\d.]+)\.js")?
        .captures(&html)
        .map(|c| c[1].to_owned())
        .ok_or_else(|| zerr("Cannot get API login version"))?;

    let _ = id_post(&http, "https://id.zalo.me/account/logininfo", REF_PC, &[("continue", "https://zalo.me/pc"), ("v", &version)]).await;
    let _ = id_post(&http, "https://id.zalo.me/account/verify-client", REF_PC, &[("type", "device"), ("continue", "https://zalo.me/pc"), ("v", &version)]).await;
    let generated = id_post(&http, "https://id.zalo.me/account/authen/qr/generate", REF_PC, &[("continue", "https://zalo.me/pc"), ("v", &version)]).await.ok();
    let Some(data) = generated.as_ref().and_then(|g| g.get("data")).filter(|d| truthy(Some(d))) else {
        return Err(zerr(format!("Unable to generate QRCode\nResponse: {}", generated.map(|g| g.to_string()).unwrap_or_else(|| "undefined".into()))));
    };
    let code = jstr(data.get("code"));
    let image = data.get("image").and_then(Value::as_str).unwrap_or_default();
    on(QrEvent::Generated { image: image.strip_prefix("data:image/png;base64,").unwrap_or(image).to_owned() });

    let waiting = async {
        let scan = long_poll(&http, "https://id.zalo.me/account/authen/qr/waiting-scan", &[("code", &code), ("continue", "https://chat.zalo.me/"), ("v", &version)])
            .await
            .ok()
            .filter(|s| truthy(s.get("data")))
            .ok_or_else(|| zerr("Cannot get scan result"))?;
        on(QrEvent::Scanned { display_name: scan["data"].get("display_name").and_then(Value::as_str).unwrap_or_default().to_owned() });
        long_poll(
            &http,
            "https://id.zalo.me/account/authen/qr/waiting-confirm",
            &[("code", &code), ("gToken", ""), ("gAction", "CONFIRM_QR"), ("continue", "https://chat.zalo.me/"), ("v", &version)],
        )
        .await
        .map_err(|_| zerr("Cannot get confirm result"))
    };
    let confirm = tokio::select! {
        r = waiting => r?,
        _ = tokio::time::sleep(Duration::from_secs(100)) => {
            on(QrEvent::Expired);
            return Err(zerr(QR_ABORTED));
        }
    };
    match confirm.get("error_code").and_then(Value::as_i64) {
        Some(-13) => {
            on(QrEvent::Declined);
            return Err(zerr(QR_ABORTED));
        }
        _ if !code_ok(confirm.get("error_code")) => {
            return Err(zerr(format!("An error has occurred.\nResponse: {}", serde_json::to_string_pretty(&confirm).unwrap_or_default())));
        }
        _ => {}
    }

    // checkSession: its redirects set the chat cookies
    let mut h = id_headers(REF_CHAT, true);
    h.push(("sec-fetch-site".into(), "same-origin".into()));
    http.request("https://id.zalo.me/account/checksession?continue=https%3A%2F%2Fchat.zalo.me%2Findex.html", Method::GET, h, Body::Empty)
        .await
        .map_err(|_| zerr("Cannot get session, login failed"))?;
    let mut h = id_headers("https://chat.zalo.me/", false);
    h.retain(|(k, _)| k != "content-type");
    h.push(("sec-fetch-site".into(), "same-site".into()));
    let user = http.request("https://jr.chat.zalo.me/jr/userinfo", Method::GET, h, Body::Empty).await.and_then(|r| r.json()).ok();
    let Some(user) = user.as_ref().and_then(|u| u.get("data")).filter(|d| truthy(Some(d))) else {
        return Err(zerr("Can't get account info"));
    };
    if !truthy(user.get("logged")) {
        return Err(zerr("Can't login"));
    }

    let creds = Credentials { imei: zalo_uuid(QR_USER_AGENT), cookie: http.jar.lock().to_json(), user_agent: QR_USER_AGENT.into(), language: None };
    on(QrEvent::GotLoginInfo(creds.clone()));
    login(&Credentials { language: Some("vi".into()), ..creds }).await
}

/// `waitingScan` / `waitingConfirm`: Zalo answers error 8 while nothing happened yet.
async fn long_poll(http: &Http, url: &str, pairs: &[(&str, &str)]) -> Result<Value> {
    loop {
        let r = id_post(http, url, REF_CHAT, pairs).await?;
        if r.get("error_code").and_then(Value::as_i64) != Some(8) {
            return Ok(r);
        }
    }
}

/* ---------------- the API ---------------- */

fn obj(pairs: Vec<(&str, Value)>) -> Value {
    Value::Object(pairs.into_iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k.to_owned(), v)).collect::<Map<_, _>>())
}

impl Api {
    /// The first URL of a service in `zpw_service_map_v3`.
    fn svc(&self, name: &str) -> Result<String> {
        let v = &self.service_map[name];
        v.as_array()
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .or_else(|| v.as_str())
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("Zalo did not give a `{name}` service"))
    }

    fn url(&self, base: &str, params: &[(&str, String)]) -> Result<String> {
        make_url(base, params, true)
    }

    fn enc(&self, params: &Value) -> Result<String> {
        encode_aes(&self.secret_key, &params.to_string()).map_err(|_| zerr("Failed to encrypt params"))
    }

    /// `resolveResponse` / `handleZaloResponse`: unwrap (and decrypt) `data`.
    pub fn resolve(&self, r: &Resp, encrypted: bool) -> Result<Value> {
        if !r.status.is_success() {
            return Err(zerr(format!("Request failed with status code {}", r.status.as_u16())));
        }
        let j: Value = serde_json::from_slice(&r.body).map_err(|_| zerr("Failed to parse response data"))?;
        if !code_ok(j.get("error_code")) {
            return Err(api_error(&j));
        }
        let decoded: Value = if encrypted {
            let data = j.get("data").and_then(Value::as_str).ok_or_else(|| zerr("Failed to parse response data"))?;
            decode_aes(&self.secret_key, data).ok().and_then(|s| serde_json::from_str(&s).ok()).ok_or_else(|| zerr("Failed to parse response data"))?
        } else {
            j
        };
        if !code_ok(decoded.get("error_code")) {
            return Err(api_error(&decoded));
        }
        Ok(decoded.get("data").cloned().unwrap_or(Value::Null))
    }

    async fn get(&self, url: &str) -> Result<Value> {
        let r = self.http.request(url, Method::GET, vec![], Body::Empty).await?;
        self.resolve(&r, true)
    }

    /// GET with `params` (encrypted) added to the URL.
    async fn get_with(&self, base: &str, params: &Value) -> Result<Value> {
        let url = self.url(base, &[("params", self.enc(params)?)])?;
        self.get(&url).await
    }

    /// POST `params=<encrypted>` as a form.
    async fn post_with(&self, url: &str, params: &Value) -> Result<Value> {
        let body = Body::Form(vec![("params".into(), self.enc(params)?)]);
        let r = self.http.request(url, Method::POST, vec![], body).await?;
        self.resolve(&r, true)
    }

    /* ----- profile, friends, groups ----- */

    /// fetchAccountInfo.js
    pub async fn fetch_account_info(&self) -> Result<Value> {
        self.get(&self.url(&format!("{}/api/social/profile/me-v2", self.svc("profile")?), &[])?).await
    }

    /// getAllFriends.js
    pub async fn get_all_friends(&self) -> Result<Vec<Value>> {
        let base = self.url(&format!("{}/api/social/friend/getfriends", self.svc("profile")?), &[])?;
        let params = json!({ "incInvalid": 1, "page": 1, "count": 20000, "avatar_size": 120, "actiontime": 0, "imei": self.imei });
        Ok(self.get_with(&base, &params).await?.as_array().cloned().unwrap_or_default())
    }

    /// getAllGroups.js
    pub async fn get_all_groups(&self) -> Result<Value> {
        self.get(&self.url(&format!("{}/api/group/getlg/v4", self.svc("group_poll")?), &[])?).await
    }

    /// getGroupInfo.js
    pub async fn get_group_info(&self, ids: &[String]) -> Result<Value> {
        let url = self.url(&format!("{}/api/group/getmg-v2", self.svc("group")?), &[])?;
        let map = format!("{{{}}}", ids.iter().map(|id| format!("{}:0", Value::String(id.clone()))).collect::<Vec<_>>().join(","));
        self.post_with(&url, &json!({ "gridVerMap": map })).await
    }

    /// getUserInfo.js
    pub async fn get_user_info(&self, ids: &[String]) -> Result<Value> {
        if ids.is_empty() {
            return Err(zerr("Missing user id"));
        }
        let url = self.url(&format!("{}/api/social/friend/getprofiles/v2", self.svc("profile")?), &[])?;
        let ids: Vec<String> = ids.iter().map(|id| if id.contains('_') { id.clone() } else { format!("{id}_0") }).collect();
        let params = json!({
            "phonebook_version": self.extra_ver.get("phonebook").cloned().unwrap_or(Value::Null),
            "friend_pversion_map": ids,
            "avatar_size": 120,
            "language": self.language,
            "show_online_status": 1,
            "imei": self.imei,
        });
        self.post_with(&url, &params).await
    }

    /// findUser.js (the phone goes international for Vietnamese accounts).
    pub async fn find_user(&self, phone: &str) -> Result<Value> {
        if phone.is_empty() {
            return Err(zerr("Missing phoneNumber"));
        }
        let phone = match phone.strip_prefix('0') {
            Some(rest) if self.language == "vi" => format!("84{rest}"),
            _ => phone.to_owned(),
        };
        let params = self.enc(&json!({ "phone": phone, "avatar_size": 240, "language": self.language, "imei": self.imei, "reqSrc": 40 }))?;
        let base = self.url(&format!("{}/api/friend/profile/get", self.svc("friend")?), &[])?;
        // zca-js puts `params` on the URL twice
        let once = make_url(&base, &[("params", params.clone())], false)?;
        self.get(&self.url(&once, &[("params", params)])?).await
    }

    /// getCloudMessage.js: one page (≤ 50) of a group's history. Returns the page and its messages.
    pub async fn get_cloud_message(&self, group_id: &str, last_msg_id: Option<&str>, is_old: bool) -> Result<(Value, Vec<Msg>)> {
        if group_id.is_empty() {
            return Err(zerr("Missing groupId"));
        }
        let group_id = group_id.strip_prefix('g').unwrap_or(group_id);
        let svc = self.svc("group_cloud_message")?;
        let base = self.url(&format!("{svc}/api/cm/{}", if is_old { "getoldv2" } else { "getrecentv2" }), &[])?;
        let last = last_msg_id.filter(|s| !s.is_empty());
        let params = json!({
            "groupId": group_id,
            "globalMsgId": last.unwrap_or("0"),
            "count": 50,
            "msgIds": [],
            "imei": self.imei,
            "src": if last.is_some() { 3 } else { 1 },
        });
        let url = self.url(&base, &[("params", self.enc(&params)?), ("nretry", "0".into())])?;
        let mut data = self.get(&url).await?;
        if let Some(s) = data.as_str() {
            data = serde_json::from_str(s)?;
        }
        let msgs = data.get("groupMsgs").and_then(Value::as_array).cloned().unwrap_or_default().into_iter().map(|m| group_message(&self.uid, m)).collect();
        Ok((data, msgs))
    }

    /* ----- sending: sendMessage.js, text only ----- */

    /// `sendMessage` for a plain text (`handleMessage` without quote or mentions): what the MCP
    /// tool `zalo_send_message` needs. Returns Zalo's `data` (it carries the new `msgId`).
    pub async fn send_text(&self, msg: &str, thread_id: &str, kind: ThreadType) -> Result<Value> {
        if thread_id.is_empty() {
            return Err(zerr("Missing threadId"));
        }
        if msg.is_empty() {
            return Err(zerr("Missing message content"));
        }
        let group = kind == ThreadType::Group;
        let base = if group {
            self.url(&format!("{}/api/group", self.svc("group")?), &[("nretry", "0".into())])?
        } else {
            self.url(&format!("{}/api/message", self.svc("chat")?), &[("nretry", "0".into())])?
        };
        let mut url = Url::parse(&base)?;
        url.set_path(&format!("{}/{}", url.path(), text_endpoint(group)));
        self.post_with(url.as_str(), &text_params(msg, thread_id, group, &self.imei, now_ms())).await
    }
}

/// `handleMessage`'s last path segment for a plain text.
fn text_endpoint(group: bool) -> &'static str {
    if group { "sendmsg" } else { "sms" }
}

/// `handleMessage`'s params for a plain text (no quote, no mentions).
fn text_params(msg: &str, thread_id: &str, group: bool, imei: &str, client_id: i64) -> Value {
    let some_if = |cond: bool, v: Value| if cond { v } else { Value::Null };
    obj(vec![
        ("message", json!(msg)),
        ("clientId", json!(client_id)),
        ("imei", some_if(!group, json!(imei))),
        ("ttl", json!(0)),
        ("visibility", some_if(group, json!(0))),
        ("toid", some_if(!group, json!(thread_id))),
        ("grid", some_if(group, json!(thread_id))),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loose_error_codes() {
        assert!(code_ok(Some(&json!(0))) && code_ok(Some(&json!("0"))) && code_ok(Some(&json!(""))));
        assert!(!code_ok(None) && !code_ok(Some(&json!(-1))) && !code_ok(Some(&Value::Null)));
    }

    #[test]
    fn text_to_a_person() {
        let p = text_params("chào", "123", false, "imei-1", 42);
        assert_eq!(p, json!({ "message": "chào", "clientId": 42, "imei": "imei-1", "ttl": 0, "toid": "123" }));
        assert_eq!(text_endpoint(false), "sms");
    }

    #[test]
    fn text_to_a_group() {
        let p = text_params("hi", "g9", true, "imei-1", 7);
        // groups: no imei, `grid` instead of `toid`, visibility 0
        assert_eq!(p, json!({ "message": "hi", "clientId": 7, "ttl": 0, "visibility": 0, "grid": "g9" }));
        assert_eq!(text_endpoint(true), "sendmsg");
    }

    #[test]
    fn object_drops_nulls() {
        assert_eq!(obj(vec![("a", json!(1)), ("b", Value::Null), ("c", json!(""))]), json!({ "a": 1, "c": "" }));
    }
}
