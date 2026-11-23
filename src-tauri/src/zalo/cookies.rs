//! The session's cookies, kept the way zca-js keeps them in a tough-cookie `CookieJar`: the saved
//! session (`Credentials.cookie`) is tough-cookie's JSON (`jar.toJSON().cookies`), so sessions saved
//! by the Electron build load here and sessions saved here load there.

use chrono::{DateTime, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use serde_json::{Map, Value, json};

#[derive(Clone, Debug, PartialEq)]
pub struct Cookie {
    pub key: String,
    pub value: String,
    /// lower-case, no leading dot
    pub domain: String,
    pub path: String,
    /// `None`: a session cookie (tough-cookie's "Infinity")
    pub expires: Option<DateTime<Utc>>,
    /// seconds; wins over `expires` (relative to `creation`)
    pub max_age: Option<f64>,
    pub secure: bool,
    pub http_only: bool,
    pub host_only: bool,
    pub path_is_default: bool,
    pub creation: DateTime<Utc>,
    pub last_accessed: Option<DateTime<Utc>>,
    pub same_site: Option<String>,
}

impl Cookie {
    fn expired(&self, now: DateTime<Utc>) -> bool {
        if let Some(m) = self.max_age {
            if m <= 0.0 {
                return true;
            }
            if m.is_finite() {
                return self.creation + chrono::Duration::milliseconds((m * 1000.0) as i64) <= now;
            }
            return false;
        }
        self.expires.is_some_and(|e| e <= now)
    }

    fn domain_matches(&self, host: &str) -> bool {
        if self.host_only {
            host == self.domain
        } else {
            host == self.domain || host.strip_suffix(self.domain.as_str()).is_some_and(|p| p.ends_with('.'))
        }
    }

    fn path_matches(&self, path: &str) -> bool {
        let path = if path.is_empty() { "/" } else { path };
        path == self.path
            || path.starts_with(&self.path) && (self.path.ends_with('/') || path[self.path.len()..].starts_with('/'))
    }

    /// tough-cookie's `Cookie.fromJSON` (also takes `name` for `key`, as zca-js does).
    pub fn from_json(v: &Value) -> Option<Self> {
        let o = v.as_object()?;
        let s = |k: &str| o.get(k).and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        });
        let key = s("key").filter(|k| !k.is_empty()).or_else(|| s("name"))?;
        let date = |k: &str| o.get(k).and_then(parse_json_date);
        let max_age = match o.get("maxAge") {
            Some(Value::Number(n)) => n.as_f64(),
            Some(Value::String(s)) if s == "Infinity" => Some(f64::INFINITY),
            Some(Value::String(s)) if s == "-Infinity" => Some(f64::NEG_INFINITY),
            _ => None,
        };
        let domain = s("domain").map(|d| d.trim_start_matches('.').to_ascii_lowercase()).unwrap_or_default();
        Some(Self {
            key,
            value: s("value").unwrap_or_default(),
            host_only: o.get("hostOnly").and_then(Value::as_bool).unwrap_or(domain.is_empty()),
            domain,
            path: s("path").filter(|p| p.starts_with('/')).unwrap_or_else(|| "/".into()),
            expires: date("expires"),
            max_age,
            secure: o.get("secure").and_then(Value::as_bool).unwrap_or(false),
            http_only: o.get("httpOnly").and_then(Value::as_bool).unwrap_or(false),
            path_is_default: o.get("pathIsDefault").and_then(Value::as_bool).unwrap_or(false),
            creation: date("creation").unwrap_or_else(Utc::now),
            last_accessed: date("lastAccessed"),
            same_site: s("sameSite"),
        })
    }

    /// tough-cookie's `Cookie.toJSON` (defaults left out).
    pub fn to_json(&self) -> Value {
        let iso = |d: &DateTime<Utc>| d.to_rfc3339_opts(SecondsFormat::Millis, true);
        let mut m = Map::new();
        m.insert("key".into(), json!(self.key));
        m.insert("value".into(), json!(self.value));
        if let Some(e) = &self.expires {
            m.insert("expires".into(), json!(iso(e)));
        }
        if let Some(a) = self.max_age {
            m.insert(
                "maxAge".into(),
                if a.is_infinite() {
                    json!(if a > 0.0 { "Infinity" } else { "-Infinity" })
                } else if a.fract() == 0.0 {
                    json!(a as i64)
                } else {
                    json!(a)
                },
            );
        }
        m.insert("domain".into(), json!(self.domain));
        m.insert("path".into(), json!(self.path));
        if self.secure {
            m.insert("secure".into(), json!(true));
        }
        if self.http_only {
            m.insert("httpOnly".into(), json!(true));
        }
        m.insert("hostOnly".into(), json!(self.host_only));
        if self.path_is_default {
            m.insert("pathIsDefault".into(), json!(true));
        }
        m.insert("creation".into(), json!(iso(&self.creation)));
        if let Some(l) = &self.last_accessed {
            m.insert("lastAccessed".into(), json!(iso(l)));
        }
        if let Some(s) = &self.same_site {
            m.insert("sameSite".into(), json!(s));
        }
        Value::Object(m)
    }
}

fn parse_json_date(v: &Value) -> Option<DateTime<Utc>> {
    match v {
        Value::String(s) if s == "Infinity" => None,
        Value::String(s) => DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc)).or_else(|| parse_http_date(s)),
        Value::Number(n) => n.as_i64().and_then(|ms| Utc.timestamp_millis_opt(ms).single()),
        _ => None,
    }
}

/// `Expires=` dates: RFC 1123 / 850 / the Netscape "Wed, 18-Mar-2026 10:00:00 GMT" form.
fn parse_http_date(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(d) = DateTime::parse_from_rfc2822(s) {
        return Some(d.with_timezone(&Utc));
    }
    let spaced = s.replace('-', " ");
    if let Ok(d) = DateTime::parse_from_rfc2822(&spaced) {
        return Some(d.with_timezone(&Utc));
    }
    let body = spaced.split_once(',').map(|(_, b)| b.trim()).unwrap_or(&spaced);
    let body = body.trim_end_matches("GMT").trim_end_matches("UTC").trim();
    ["%d %b %Y %H:%M:%S", "%d %b %y %H:%M:%S", "%b %d %H:%M:%S %Y"]
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(body, f).ok())
        .map(|n| n.and_utc())
}

/// One `Set-Cookie` header, before it is placed in the jar.
#[derive(Debug, Default)]
pub struct SetCookie {
    pub key: String,
    pub value: String,
    pub domain: Option<String>,
    pub path: Option<String>,
    pub expires: Option<DateTime<Utc>>,
    pub max_age: Option<f64>,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: Option<String>,
}

/// tough-cookie's `Cookie.parse` (strict: a cookie needs `name=value`).
pub fn parse_set_cookie(header: &str) -> Option<SetCookie> {
    let mut parts = header.split(';');
    let (k, v) = parts.next()?.split_once('=')?;
    let mut c = SetCookie { key: k.trim().to_owned(), value: v.trim().trim_matches('"').to_owned(), ..Default::default() };
    if c.key.is_empty() {
        return None;
    }
    for attr in parts {
        let (name, val) = attr.split_once('=').map(|(a, b)| (a.trim(), b.trim())).unwrap_or((attr.trim(), ""));
        match name.to_ascii_lowercase().as_str() {
            "expires" => c.expires = parse_http_date(val).or(c.expires),
            "max-age" => {
                if let Ok(n) = val.parse::<i64>() {
                    c.max_age = Some(n as f64);
                }
            }
            "domain" => {
                let d = val.trim_start_matches('.').to_ascii_lowercase();
                c.domain = (!d.is_empty()).then_some(d);
            }
            "path" => c.path = val.starts_with('/').then(|| val.to_owned()),
            "secure" => c.secure = true,
            "httponly" => c.http_only = true,
            "samesite" => c.same_site = Some(val.to_ascii_lowercase()),
            _ => {}
        }
    }
    Some(c)
}

#[derive(Default, Debug)]
pub struct CookieJar {
    cookies: Vec<Cookie>,
}

impl CookieJar {
    /// zca-js `parseCookies`: an array of tough-cookie JSON cookies, or `{cookies: [...]}`.
    pub fn from_credentials(v: &Value) -> Self {
        let list = v.as_array().or_else(|| v.get("cookies").and_then(Value::as_array));
        let mut jar = Self::default();
        for c in list.into_iter().flatten().filter_map(Cookie::from_json) {
            let mut c = c;
            if c.domain.is_empty() {
                // tough-cookie: set against https://chat.zalo.me, so host-only there
                c.domain = "chat.zalo.me".into();
                c.host_only = true;
            }
            jar.put(c);
        }
        jar
    }

    pub fn to_json(&self) -> Value {
        Value::Array(self.cookies.iter().map(Cookie::to_json).collect())
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    fn put(&mut self, mut c: Cookie) {
        if let Some(old) = self.cookies.iter_mut().find(|o| o.domain == c.domain && o.path == c.path && o.key == c.key) {
            c.creation = old.creation;
            *old = c;
        } else {
            self.cookies.push(c);
        }
    }

    /// A `Set-Cookie` from a response to `request_url`. Like zca-js `request()`, a cookie naming
    /// any domain is stored for that domain; one naming none is host-only for the responding host.
    pub fn set_from_header(&mut self, header: &str, request_url: &reqwest::Url) {
        let Some(sc) = parse_set_cookie(header) else { return };
        let host = request_url.host_str().unwrap_or_default().to_ascii_lowercase();
        let (domain, host_only) = match sc.domain {
            Some(d) => (d, false),
            None => (host, true),
        };
        let now = Utc::now();
        let c = Cookie {
            key: sc.key,
            value: sc.value,
            domain,
            path_is_default: sc.path.is_none(),
            path: sc.path.unwrap_or_else(|| "/".into()),
            expires: sc.expires,
            max_age: sc.max_age,
            secure: sc.secure,
            http_only: sc.http_only,
            host_only,
            creation: now,
            last_accessed: Some(now),
            same_site: sc.same_site,
        };
        if c.expired(now) {
            self.cookies.retain(|o| !(o.domain == c.domain && o.path == c.path && o.key == c.key));
        } else {
            self.put(c);
        }
    }

    /// tough-cookie's `getCookieString(url)`: matching, unexpired cookies, longest path first.
    pub fn cookie_string(&self, url: &str) -> String {
        let Ok(url) = reqwest::Url::parse(url) else { return String::new() };
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        let secure = matches!(url.scheme(), "https" | "wss");
        let now = Utc::now();
        let mut list: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| !c.expired(now) && c.domain_matches(&host) && c.path_matches(url.path()) && (secure || !c.secure))
            .collect();
        list.sort_by(|a, b| b.path.len().cmp(&a.path.len()).then(a.creation.cmp(&b.creation)));
        list.iter().map(|c| format!("{}={}", c.key, c.value)).collect::<Vec<_>>().join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> reqwest::Url {
        reqwest::Url::parse(s).unwrap()
    }

    #[test]
    fn set_and_send_like_tough_cookie() {
        let mut jar = CookieJar::default();
        jar.set_from_header("a=1; Domain=.zalo.me; Path=/; Expires=Wed, 18 Mar 2099 10:00:00 GMT; Secure; HttpOnly; SameSite=None", &url("https://id.zalo.me/account"));
        jar.set_from_header("c=3; Domain=chat.zalo.me; Max-Age=3600", &url("https://id.zalo.me/"));
        jar.set_from_header("h=4; Path=/", &url("https://id.zalo.me/x"));
        jar.set_from_header("old=5; Domain=zalo.me; Expires=Wed, 18-Mar-2001 10:00:00 GMT", &url("https://id.zalo.me/"));
        assert_eq!(jar.cookie_string("https://chat.zalo.me"), "a=1; c=3");
        assert_eq!(jar.cookie_string("https://wpa.chat.zalo.me/api"), "a=1; c=3");
        assert_eq!(jar.cookie_string("https://id.zalo.me/"), "a=1; h=4");
        assert_eq!(jar.cookie_string("http://chat.zalo.me"), "c=3");
        assert_eq!(jar.cookie_string("https://notzalo.me"), "");
        // replacing keeps one copy
        jar.set_from_header("a=2; Domain=zalo.me; Path=/", &url("https://chat.zalo.me/"));
        assert_eq!(jar.cookie_string("https://zalo.me/"), "a=2");
        // an expired Set-Cookie deletes
        jar.set_from_header("c=; Domain=chat.zalo.me; Max-Age=0", &url("https://chat.zalo.me/"));
        assert_eq!(jar.cookie_string("https://chat.zalo.me"), "a=2");
    }

    #[test]
    fn roundtrips_tough_cookie_json() {
        // what zca-js saved (tough-cookie 5 `toJSON().cookies`), including its "null" host cookies
        let saved = serde_json::json!([
            {"key":"a","value":"1","expires":"2099-03-18T10:00:00.000Z","domain":"zalo.me","path":"/","secure":true,"httpOnly":true,"hostOnly":false,"creation":"2026-11-19T07:52:45.650Z","lastAccessed":"2026-11-19T07:52:45.650Z","sameSite":"none"},
            {"key":"b","value":"2","domain":"null","path":"/","hostOnly":true,"creation":"2026-11-19T07:52:45.657Z","lastAccessed":"2026-11-19T07:52:45.657Z"},
            {"key":"c","value":"3","maxAge":3600,"domain":"chat.zalo.me","path":"/","hostOnly":false,"pathIsDefault":true,"creation":"2099-01-01T00:00:00.000Z","lastAccessed":"2026-11-19T07:52:45.657Z"},
            {"name":"zpw_sek","value":"s e k","domain":".chat.zalo.me","path":"/"}
        ]);
        let jar = CookieJar::from_credentials(&saved);
        assert_eq!(jar.len(), 4);
        // tough-cookie order: longer path first, then older creation first (c was "created" in 2099)
        assert_eq!(jar.cookie_string("https://chat.zalo.me"), "a=1; zpw_sek=s e k; c=3");
        let back = jar.to_json();
        assert_eq!(back[0], saved[0]);
        assert_eq!(back[1], saved[1]);
        assert_eq!(back[2], saved[2]);
        assert_eq!(back[3]["key"], "zpw_sek");
        assert_eq!(back[3]["domain"], "chat.zalo.me");
        // the {cookies: [...]} form too
        assert_eq!(CookieJar::from_credentials(&serde_json::json!({ "cookies": saved })).len(), 4);
    }
}
