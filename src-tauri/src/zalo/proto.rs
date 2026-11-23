//! Zalo Web's wire format, ported from zca-js 2.3.0 (`dist/utils.js`, `apis/login.js`,
//! `apis/listen.js`): the AES-CBC used for request params, the login-time param encryptor and
//! its signature, websocket frames and how their (encrypted, deflated) event data decodes.
//!
//! Everything here is pure: unit tests compare it with values produced by zca-js itself.

use std::io::Read;

use aes_gcm::{
    AesGcm,
    aead::{Aead, Payload},
    aes::{
        Aes128, Aes192, Aes256, Block,
        cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit, consts::U16},
    },
};
use anyhow::{Context, Result, anyhow, bail};
use base64::{
    Engine,
    alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use serde_json::Value;

/// zca-js `createContext` defaults (`apiType`, `apiVersion`).
pub const API_TYPE: u32 = 30;
pub const API_VERSION: u32 = 685;

/// Standard alphabet, padded on output, lenient on input (like crypto-js' Base64.parse).
pub const B64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent).with_decode_allow_trailing_bits(true),
);

/* ---------------- hashing ---------------- */

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn md5_hex(data: impl AsRef<[u8]>) -> String {
    use md5::{Digest, Md5};
    hex(&Md5::digest(data.as_ref()))
}

/// `getSignKey`: md5("zsecure" + type + the values of `params`, ordered by key).
pub fn sign_key(kind: &str, params: &[(&str, String)]) -> String {
    let mut sorted: Vec<&(&str, String)> = params.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let mut s = format!("zsecure{kind}");
    for (_, v) in sorted {
        s.push_str(v);
    }
    md5_hex(s)
}

/// zca-js `generateZaloUUID`: a v4 uuid + "-" + md5(user agent).
pub fn zalo_uuid(user_agent: &str) -> String {
    format!("{}-{}", uuid::Uuid::new_v4(), md5_hex(user_agent))
}

/* ---------------- AES-CBC, zero IV, PKCS#7 (crypto-js) ---------------- */

enum AesKey {
    A128(Aes128),
    A192(Aes192),
    A256(Aes256),
}

impl AesKey {
    fn new(key: &[u8]) -> Result<Self> {
        Ok(match key.len() {
            16 => Self::A128(Aes128::new_from_slice(key).map_err(|_| anyhow!("bad AES key"))?),
            24 => Self::A192(Aes192::new_from_slice(key).map_err(|_| anyhow!("bad AES key"))?),
            32 => Self::A256(Aes256::new_from_slice(key).map_err(|_| anyhow!("bad AES key"))?),
            n => bail!("AES key must be 16, 24 or 32 bytes (got {n})"),
        })
    }

    fn encrypt(&self, b: &mut Block) {
        match self {
            Self::A128(c) => c.encrypt_block(b),
            Self::A192(c) => c.encrypt_block(b),
            Self::A256(c) => c.encrypt_block(b),
        }
    }

    fn decrypt(&self, b: &mut Block) {
        match self {
            Self::A128(c) => c.decrypt_block(b),
            Self::A192(c) => c.decrypt_block(b),
            Self::A256(c) => c.decrypt_block(b),
        }
    }
}

pub fn cbc_encrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let cipher = AesKey::new(key)?;
    let pad = 16 - data.len() % 16;
    let mut buf = data.to_vec();
    buf.extend(std::iter::repeat_n(pad as u8, pad));
    let mut prev = [0u8; 16];
    for chunk in buf.as_chunks_mut::<16>().0 {
        let mut block = Block::default();
        for (i, b) in block.iter_mut().enumerate() {
            *b = chunk[i] ^ prev[i];
        }
        cipher.encrypt(&mut block);
        chunk.copy_from_slice(&block);
        prev.copy_from_slice(&block);
    }
    Ok(buf)
}

pub fn cbc_decrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let cipher = AesKey::new(key)?;
    if data.is_empty() || !data.len().is_multiple_of(16) {
        bail!("ciphertext is not a whole number of blocks");
    }
    let mut out = Vec::with_capacity(data.len());
    let mut prev = [0u8; 16];
    for chunk in data.as_chunks::<16>().0 {
        let mut block = Block::default();
        block.copy_from_slice(chunk);
        cipher.decrypt(&mut block);
        for i in 0..16 {
            out.push(block[i] ^ prev[i]);
        }
        prev.copy_from_slice(chunk);
    }
    // crypto-js strips as many bytes as the last one says, without checking them
    let pad = *out.last().unwrap_or(&0) as usize;
    if pad == 0 || pad > 16 {
        bail!("bad padding");
    }
    out.truncate(out.len() - pad);
    Ok(out)
}

/// `encodeAES(secretKey, data)`: the session's request params (key = base64 `zpw_enk`).
pub fn encode_aes(secret_key: &str, data: &str) -> Result<String> {
    let key = B64.decode(secret_key.trim()).context("secret key is not base64")?;
    Ok(B64.encode(cbc_encrypt(&key, data.as_bytes())?))
}

/// `decodeAES(secretKey, data)`: an API response's `data`.
pub fn decode_aes(secret_key: &str, data: &str) -> Result<String> {
    let key = B64.decode(secret_key.trim()).context("secret key is not base64")?;
    let raw = B64.decode(decode_uri_component(data)?.trim()).context("response is not base64")?;
    Ok(String::from_utf8(cbc_decrypt(&key, &raw)?)?)
}

/// `ParamsEncryptor.encodeAES`: key = the UTF-8 bytes of `key`, output hex or base64.
pub fn encode_aes_utf8_key(key: &str, message: &str, as_hex: bool, uppercase: bool) -> Result<String> {
    let ct = cbc_encrypt(key.as_bytes(), message.as_bytes())?;
    let s = if as_hex { hex(&ct) } else { B64.encode(ct) };
    Ok(if uppercase { s.to_uppercase() } else { s })
}

/// `decryptResp(key, data)`: the login response, parsed when it is JSON.
pub fn decrypt_resp(key: &str, data: &str) -> Result<Value> {
    let raw = B64.decode(decode_uri_component(data)?.trim()).context("login response is not base64")?;
    let text = String::from_utf8(cbc_decrypt(key.as_bytes(), &raw)?)?;
    Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

/* ---------------- the login param encryptor ---------------- */

/// `ParamsEncryptor`: the per-login key Zalo derives from (type, imei, launch time) + a random salt.
pub struct ParamsEncryptor {
    pub zcid: String,
    pub zcid_ext: String,
    pub encrypt_key: String,
}

impl ParamsEncryptor {
    pub const ENC_VER: &str = "v2";

    pub fn new(kind: u32, imei: &str, first_launch_time: u64) -> Result<Self> {
        Self::with_ext(kind, imei, first_launch_time, random_hex())
    }

    pub fn with_ext(kind: u32, imei: &str, first_launch_time: u64, zcid_ext: String) -> Result<Self> {
        let zcid = encode_aes_utf8_key("3FC4F0D2AB50057BCE0D90D9187A22B1", &format!("{kind},{imei},{first_launch_time}"), true, true)?;
        let n = md5_hex(&zcid_ext).to_uppercase();
        let (even_n, _) = split_even_odd(&n);
        let (even_z, odd_z) = split_even_odd(&zcid);
        let mut key: String = even_n.iter().take(8).collect();
        key.extend(even_z.iter().take(12));
        key.extend(odd_z.iter().rev().take(12));
        Ok(Self { zcid, zcid_ext, encrypt_key: key })
    }
}

fn split_even_odd(s: &str) -> (Vec<char>, Vec<char>) {
    let mut even = Vec::new();
    let mut odd = Vec::new();
    for (i, c) in s.chars().enumerate() {
        if i % 2 == 0 { even.push(c) } else { odd.push(c) }
    }
    (even, odd)
}

/// `ParamsEncryptor.randomString()`: 6–12 hex digits.
fn random_hex() -> String {
    let u = uuid::Uuid::new_v4();
    let len = 6 + (u.as_bytes()[0] as usize % 7);
    u.simple().to_string().chars().take(len).collect()
}

/* ---------------- websocket ---------------- */

/// A frame Zalo's socket takes: version, cmd (u16 LE), sub-command, then the JSON body.
pub fn encode_frame(version: u8, cmd: u16, sub_cmd: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + body.len());
    out.push(version);
    out.extend_from_slice(&cmd.to_le_bytes());
    out.push(sub_cmd);
    out.extend_from_slice(body);
    out
}

/// (version, cmd, sub-command) of a received frame.
pub fn frame_header(frame: &[u8]) -> Option<(u8, u16, u8)> {
    (frame.len() >= 4).then(|| (frame[0], u16::from_le_bytes([frame[1], frame[2]]), frame[3]))
}

/// `decodeEventData`: `{data, encrypt}` → the event JSON. encrypt 0: plain JSON; 1: base64 of
/// deflated JSON; 2: AES-GCM(cipher key) of deflated JSON; 3: AES-GCM of plain JSON.
pub fn decode_event_data(parsed: &Value, cipher_key: Option<&str>) -> Result<Value> {
    let Some(raw) = parsed.get("data").and_then(Value::as_str) else {
        bail!("Invalid data, expected string");
    };
    let Some(kind) = parsed.get("encrypt").and_then(Value::as_i64) else {
        bail!("Invalid encrypt type, expected number");
    };
    if !(0..=3).contains(&kind) {
        bail!("Invalid encrypt type, expected 0-3 but got {kind}");
    }
    if kind == 0 {
        return Ok(serde_json::from_str(raw)?);
    }
    let decoded = if kind == 1 { B64.decode(raw.trim())? } else { B64.decode(decode_uri_component(raw)?.trim())? };
    let decrypted = if kind == 1 {
        decoded
    } else {
        let Some(key) = cipher_key.filter(|_| decoded.len() >= 48) else {
            bail!("Invalid data length or missing cipher key");
        };
        aes_gcm_16(&B64.decode(key)?, &decoded[..16], &decoded[16..32], &decoded[32..])?
    };
    let plain = if kind == 3 { decrypted } else { inflate(&decrypted)? };
    Ok(serde_json::from_slice(&plain)?)
}

/// AES-GCM with the 16-byte IV Zalo uses (WebCrypto infers AES-128/256 from the key).
fn aes_gcm_16(key: &[u8], iv: &[u8], aad: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let nonce = aes_gcm::Nonce::<U16>::try_from(iv).map_err(|_| anyhow!("bad IV"))?;
    let payload = Payload { msg: data, aad };
    let out = match key.len() {
        16 => AesGcm::<Aes128, U16>::new_from_slice(key).map_err(|_| anyhow!("bad key"))?.decrypt(&nonce, payload),
        32 => AesGcm::<Aes256, U16>::new_from_slice(key).map_err(|_| anyhow!("bad key"))?.decrypt(&nonce, payload),
        n => bail!("cipher key must be 16 or 32 bytes (got {n})"),
    };
    out.map_err(|_| anyhow!("could not decrypt event data"))
}

/// `pako.inflate`: zlib, or gzip when the header says so.
pub fn inflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    if data.starts_with(&[0x1f, 0x8b]) {
        flate2::read::GzDecoder::new(data).read_to_end(&mut out)?;
    } else {
        flate2::read::ZlibDecoder::new(data).read_to_end(&mut out)?;
    }
    Ok(out)
}

/* ---------------- URLs ---------------- */

/// `makeURL`: append `params`, then `zpw_ver` / `zpw_type` unless present.
pub fn make_url(base: &str, params: &[(&str, String)], api_version: bool) -> Result<String> {
    let mut url = reqwest::Url::parse(base).with_context(|| format!("bad URL {base}"))?;
    {
        let has = |url: &reqwest::Url, k: &str| url.query_pairs().any(|(key, _)| key == k);
        let need_ver = api_version && !has(&url, "zpw_ver");
        let need_type = api_version && !has(&url, "zpw_type");
        let mut q = url.query_pairs_mut();
        for (k, v) in params {
            q.append_pair(k, v);
        }
        if need_ver {
            q.append_pair("zpw_ver", &API_VERSION.to_string());
        }
        if need_type {
            q.append_pair("zpw_type", &API_TYPE.to_string());
        }
    }
    Ok(url.to_string())
}

/// JS `decodeURIComponent` (throws on a malformed escape).
pub fn decode_uri_component(s: &str) -> Result<String> {
    if !s.contains('%') {
        return Ok(s.to_owned());
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
            let Some(v) = h else { bail!("URI malformed") };
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| anyhow!("URI malformed"))
}

/* ---------------- JS value semantics ---------------- */

/// JS `String(v)` for a JSON value (`None` = undefined).
pub fn jstr(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(other) => other.to_string(),
    }
}

/// JS truthiness.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// JS `Number(v)` (NaN for what doesn't convert).
pub fn jnum(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                i64::from_str_radix(h, 16).map(|n| n as f64).unwrap_or(f64::NAN)
            } else if matches!(t, "Infinity" | "+Infinity") {
                f64::INFINITY
            } else if t == "-Infinity" {
                f64::NEG_INFINITY
            } else if t.chars().any(|c| c.is_ascii_alphabetic() && !matches!(c, 'e' | 'E')) {
                f64::NAN // Rust would take "inf" / "nan"; JS doesn't
            } else {
                t.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
        Some(_) => f64::NAN,
    }
}

/// `v == "0"` (loose): the string "0" or the number 0.
pub fn is_zero_id(v: Option<&Value>) -> bool {
    match v {
        Some(Value::String(s)) => s == "0",
        Some(Value::Number(n)) => n.as_f64() == Some(0.0),
        _ => false,
    }
}

/// Milliseconds since the epoch, as JS `Date.now()`.
pub fn now_ms() -> i64 {
    crate::core::now_ms() as i64
}

#[cfg(test)]
mod tests {
    //! Expected values were produced by zca-js 2.3.0's own helpers under node (crypto-js, pako,
    //! WebCrypto) — see the vector script in the task notes.
    use super::*;

    const PLAIN: &str = r#"{"imei":"abc-123","text":"Xin chào 👋 \"q\"","n":1}"#;

    #[test]
    fn encode_aes_matches_zca() {
        assert_eq!(
            encode_aes("AAECAwQFBgcICQoLDA0ODw==", PLAIN).unwrap(),
            "IEYbby6K8plUNeGKatxhpYgkaZ49oRlGVvR70j/TrLaaNYf/dwMjB5GIdJlxI2VRWAF8Yfh7+zx1y92KaHcmmw=="
        );
        assert_eq!(
            encode_aes("ABEiM0RVZneImaq7zN3u/wARIjNEVWZ3iJmqu8zd7v8=", PLAIN).unwrap(),
            "zJ97ujC7o++hZBZ+MTbuMWCTOxqZgx6vfkcKQ7s1jMfEEBfi5IxQLUinYneELg/0WCgIlJuf0oBEJqSrftmZpQ=="
        );
    }

    #[test]
    fn decode_aes_roundtrip_and_uri_encoded() {
        let enc = "IEYbby6K8plUNeGKatxhpYgkaZ49oRlGVvR70j/TrLaaNYf/dwMjB5GIdJlxI2VRWAF8Yfh7+zx1y92KaHcmmw==";
        assert_eq!(decode_aes("AAECAwQFBgcICQoLDA0ODw==", enc).unwrap(), PLAIN);
        let uri = enc.replace('+', "%2B").replace('/', "%2F").replace('=', "%3D");
        assert_eq!(decode_aes("AAECAwQFBgcICQoLDA0ODw==", &uri).unwrap(), PLAIN);
    }

    #[test]
    fn sign_keys_match_zca() {
        let p = [
            ("zcid", "ABC".to_string()),
            ("zcid_ext", "f00d".into()),
            ("enc_ver", "v2".into()),
            ("params", "xyz==".into()),
            ("type", "30".into()),
            ("client_version", "685".into()),
        ];
        assert_eq!(sign_key("getlogininfo", &p), "311171f7166804f88e231dea50c65903");
        let s = [("imei", "imei-1".to_string()), ("type", "30".into()), ("client_version", "685".into()), ("computer_name", "Web".into())];
        assert_eq!(sign_key("getserverinfo", &s), "e056a872bbfd6d4998380170e8df066b");
    }

    #[test]
    fn params_encryptor_matches_zca() {
        let pe = ParamsEncryptor::with_ext(30, "imei-1", 1700000000000, "a1b2c3d4e5".into()).unwrap();
        assert_eq!(pe.zcid, "0E1E394F287EC558340011224B0BAE698D6F8454F6A9D7AFA8C0F07059D228A5");
        assert_eq!(pe.encrypt_key, "C3A3B143013427C5301258290008F796");
        let enc = encode_aes_utf8_key(&pe.encrypt_key, PLAIN, false, false).unwrap();
        assert_eq!(enc, "3BFow1GREB3yn8ZysM0p/wXOqx7QJIOOHSqrcWI8KzBTvhZdTEVfWHD2MaRyRbEzKlIGFK+sKuFrrAlx5NTffA==");
        let back = decrypt_resp(&pe.encrypt_key, &enc.replace('+', "%2B").replace('/', "%2F")).unwrap();
        assert_eq!(back["text"], "Xin chào 👋 \"q\"");
        assert_eq!(back["n"], 1);
        // a fresh encryptor makes a 6–12 digit salt and a 32-char key
        let r = ParamsEncryptor::new(30, "imei-1", 1).unwrap();
        assert!((6..=12).contains(&r.zcid_ext.len()) && r.zcid_ext.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(r.encrypt_key.len(), 32);
    }

    #[test]
    fn md5_and_uuid() {
        let ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:133.0) Gecko/20100101 Firefox/133.0";
        assert_eq!(md5_hex(ua), "a69b52f9d7f760edf3fd052bcda2542f");
        assert!(zalo_uuid(ua).ends_with("-a69b52f9d7f760edf3fd052bcda2542f"));
    }

    #[test]
    fn make_url_matches_zca() {
        assert_eq!(
            make_url("https://tt-chat-wpa.chat.zalo.me/api/message/sms?nretry=0", &[("params", "a+b/c==".into()), ("x", "1".into())], true).unwrap(),
            "https://tt-chat-wpa.chat.zalo.me/api/message/sms?nretry=0&params=a%2Bb%2Fc%3D%3D&x=1&zpw_ver=685&zpw_type=30"
        );
        assert_eq!(
            make_url("https://files.zalo.me/api/message/photo_original/send?", &[("nretry", "0".into())], true).unwrap(),
            "https://files.zalo.me/api/message/photo_original/send?nretry=0&zpw_ver=685&zpw_type=30"
        );
        let once = make_url("https://a.zalo.me/x", &[], true).unwrap();
        assert_eq!(make_url(&once, &[("p", "1".into())], true).unwrap(), "https://a.zalo.me/x?zpw_ver=685&zpw_type=30&p=1");
    }

    #[test]
    fn frames_match_zca() {
        let body = br#"{"first":true,"lastId":null,"preIds":[],"req_id":"req_0"}"#;
        let f = encode_frame(1, 510, 1, body);
        assert_eq!(
            hex(&f),
            "01fe01017b226669727374223a747275652c226c6173744964223a6e756c6c2c22707265496473223a5b5d2c227265715f6964223a227265715f30227d"
        );
        assert_eq!(frame_header(&f), Some((1, 510, 1)));
        assert_eq!(frame_header(&[1, 2]), None);
    }

    #[test]
    fn event_data_all_encryptions() {
        let key = "eW3IKbdwwN1/3bOziTGtGg==";
        let ev = r#"{"data":{"msgs":[{"msgId":"7300000000000000001","uidFrom":"0","idTo":"123","content":"hi"}]},"error_code":0}"#;
        let want: Value = serde_json::from_str(ev).unwrap();
        let e1 = "eJyrVkpJLElUsqpWyi1OL1ayigYzPFOUrJTMjQ3QgKGSjlJpZopbUX4uUN4AyMtMCckHMg2NjIGc5Py8ktS8EiA/I1OpNrZWRym1qCi/KD45PyVVycqgFgBsuR8C";
        let e2 = "hX690Znr76w1qPB%2Fn4zpsOwrajmqSfrBxfjPNkupHINPn2EUOss89F2ev2idwy9ywtZIGMndRpW1nmybKFgy3tuxRsqtrDypleC7DKmfxIXGkU2teYqPjwswt9ryKun1HONA2ym7dp2AcH%2BQXT4dHViYqTaTmJPwfic%2BMOqoWDcf3EG1JsY71TFzZS%2BO";
        let e3 = "eLPOda5L18GDkb2oEfcAD/TUewpeHoHJYPJjEBQ8rtoPg7GOhXM9agmP3oVXMfsGJFMmRzDvVRqg7F/E+/2ui2NOm1QF0lt/9KZbZlpU3DLPymugZAG/LXrZoa6BWWN1PsK/SJELH1ZcIbqp7DxzoaKFjNkdVHsnYX9eK9TVohGHzbHpskURKcxZnCJOxd0mMoNodI5ts5XAwNQf";
        for (data, enc) in [(ev, 0), (e1, 1), (e2, 2), (e3, 3)] {
            let got = decode_event_data(&serde_json::json!({ "data": data, "encrypt": enc }), Some(key)).unwrap();
            assert_eq!(got, want, "encrypt={enc}");
        }
        // the 19-digit id survives exactly (zca-js uses json-bigint here)
        assert_eq!(want["data"]["msgs"][0]["msgId"], "7300000000000000001");
        assert!(decode_event_data(&serde_json::json!({ "data": e2, "encrypt": 2 }), None).is_err());
        assert!(decode_event_data(&serde_json::json!({ "data": e2, "encrypt": 4 }), Some(key)).is_err());
    }

    #[test]
    fn js_values() {
        use serde_json::json;
        assert_eq!(jstr(Some(&json!(12))), "12");
        assert_eq!(jstr(None), "undefined");
        assert!(!truthy(Some(&json!(""))) && truthy(Some(&json!("x"))) && !truthy(Some(&json!(0))));
        assert_eq!(jnum(Some(&json!(" 42 "))), 42.0);
        assert_eq!(jnum(Some(&json!(""))), 0.0);
        assert!(jnum(Some(&json!("abc"))).is_nan());
        assert!(jnum(None).is_nan());
        assert!(is_zero_id(Some(&json!("0"))) && is_zero_id(Some(&json!(0))) && !is_zero_id(Some(&json!("10"))));
        assert_eq!(decode_uri_component("a%20b%C3%A0").unwrap(), "a bà");
        assert!(decode_uri_component("%E0%A4%A").is_err());
    }
}
