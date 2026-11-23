//! Secrets at rest (saved SQL passwords, the Zalo session).
//!
//! Same format Electron's `safeStorage` wrote, so everything the old build saved still opens:
//! `"v10" + 12-byte nonce + AES-256-GCM(ciphertext + tag)`, with the AES key kept in
//! `<userData>/Local State` → `os_crypt.encrypted_key` = base64("DPAPI" + DPAPI-protected key).
//! On Windows only the signed-in user can unwrap that key.

use std::sync::OnceLock;

use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};

use crate::core::user_data;

const PREFIX: &[u8] = b"v10";

fn key() -> Result<&'static [u8; 32]> {
    static KEY: OnceLock<[u8; 32]> = OnceLock::new();
    if let Some(k) = KEY.get() {
        return Ok(k);
    }
    let k = load_or_create_key()?;
    Ok(KEY.get_or_init(|| k))
}

fn load_or_create_key() -> Result<[u8; 32]> {
    let file = user_data().join("Local State");
    let mut state: Value = std::fs::read_to_string(&file).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| json!({}));
    if let Some(enc) = state.pointer("/os_crypt/encrypted_key").and_then(Value::as_str) {
        let raw = B64.decode(enc).context("os_crypt key is not base64")?;
        let wrapped = raw.strip_prefix(b"DPAPI").ok_or_else(|| anyhow!("unknown os_crypt key format"))?;
        let key = unwrap_key(wrapped)?;
        return key.try_into().map_err(|_| anyhow!("os_crypt key has the wrong length"));
    }
    // first run on this machine: make a key and keep it the way Chromium does
    let mut key = [0u8; 32];
    rand::fill(&mut key);
    let mut wrapped = b"DPAPI".to_vec();
    wrapped.extend(wrap_key(&key)?);
    if !state.is_object() {
        state = json!({});
    }
    state["os_crypt"] = json!({ "encrypted_key": B64.encode(wrapped) });
    std::fs::write(&file, serde_json::to_vec(&state)?)?;
    Ok(key)
}

/// Encrypts `plain` (the old `safeStorage.encryptString`).
pub fn seal(plain: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(key()?.into());
    let mut nonce = [0u8; 12];
    rand::fill(&mut nonce);
    let mut out = PREFIX.to_vec();
    out.extend_from_slice(&nonce);
    out.extend(cipher.encrypt(&Nonce::from(nonce), plain).map_err(|_| anyhow!("encryption failed"))?);
    Ok(out)
}

/// Decrypts what `seal` (or the Electron build) wrote.
pub fn open(sealed: &[u8]) -> Result<Vec<u8>> {
    let Some(rest) = sealed.strip_prefix(PREFIX) else {
        // very old Chromium wrote bare DPAPI blobs
        return unwrap_key(sealed);
    };
    if rest.len() < 12 + 16 {
        bail!("sealed data is too short");
    }
    let (nonce, body) = rest.split_at(12);
    let cipher = Aes256Gcm::new(key()?.into());
    let nonce: [u8; 12] = nonce.try_into()?;
    cipher.decrypt(&Nonce::from(nonce), body).map_err(|_| anyhow!("could not decrypt (was it saved by another Windows user?)"))
}

pub fn seal_str(s: &str) -> Result<String> {
    Ok(B64.encode(seal(s.as_bytes())?))
}

pub fn open_str(b64: &str) -> Result<String> {
    Ok(String::from_utf8(open(&B64.decode(b64)?)?)?)
}

/* ---------------- key wrapping: DPAPI on Windows, a private file elsewhere ---------------- */

#[cfg(windows)]
fn wrap_key(data: &[u8]) -> Result<Vec<u8>> {
    dpapi(data, true)
}

#[cfg(windows)]
fn unwrap_key(data: &[u8]) -> Result<Vec<u8>> {
    dpapi(data, false)
}

#[cfg(windows)]
fn dpapi(data: &[u8], protect: bool) -> Result<Vec<u8>> {
    use windows::Win32::{
        Foundation::{HLOCAL, LocalFree},
        Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData},
    };
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: input points at `data` for the duration of the call; output is freed below.
    unsafe {
        if protect {
            CryptProtectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output)?;
        } else {
            CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output)?;
        }
        let out = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(out)
    }
}

/// macOS / Linux: the key is stored as-is in a file only this user can read.
#[cfg(not(windows))]
fn wrap_key(data: &[u8]) -> Result<Vec<u8>> {
    let file = user_data().join("Local State.key");
    std::fs::write(&file, data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(b"file".to_vec())
}

#[cfg(not(windows))]
fn unwrap_key(_data: &[u8]) -> Result<Vec<u8>> {
    Ok(std::fs::read(user_data().join("Local State.key"))?)
}
