//! Stand-in for Electron's `safeStorage` (no JS counterpart file; src/main/store.js calls it).
//! On Windows Electron stores strings in Chromium's os_crypt v10 format, and so does this:
//!
//!   secrets.bin = "v10" + 12-byte nonce + AES-256-GCM ciphertext + 16-byte tag
//!   key         = `Local State` -> os_crypt.encrypted_key (base64) -> strip the "DPAPI" prefix -> CryptUnprotectData
//!
//! so a secrets.bin written by the Electron app opens here, and the other way round. Data without the v10
//! prefix is a bare DPAPI blob (what very old Chromium versions wrote), which is still readable.
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use serde_json::{json, Value};

const PREFIX: &[u8] = b"v10";
const DPAPI_TAG: &[u8] = b"DPAPI";

pub struct SafeStorage {
    local_state: PathBuf,
    key: Mutex<Option<[u8; 32]>>,
}

impl SafeStorage {
    /// `dir` is the data dir; `Local State` lives directly in it, as in Electron's userData.
    pub fn new(dir: &Path) -> Self {
        SafeStorage { local_state: dir.join("Local State"), key: Mutex::new(None) }
    }

    /// Reads the key from `Local State`, creating (and DPAPI-wrapping) a fresh one when the file has none,
    /// which is what Chromium does on first run.
    fn key(&self) -> Result<[u8; 32], String> {
        let mut guard = self.key.lock().unwrap();
        if let Some(k) = *guard {
            return Ok(k);
        }
        let state: Value = std::fs::read(&self.local_state).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(|| json!({}));
        let b64 = state.pointer("/os_crypt/encrypted_key").and_then(Value::as_str);
        let key = match b64 {
            Some(b64) => {
                let raw = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| format!("os_crypt.encrypted_key is not base64: {e}"))?;
                let blob = raw.strip_prefix(DPAPI_TAG).ok_or("os_crypt.encrypted_key has no DPAPI prefix")?;
                let k = dpapi::unprotect(blob)?;
                <[u8; 32]>::try_from(k.as_slice()).map_err(|_| "os_crypt key is not 32 bytes".to_string())?
            }
            None => {
                use rand::RngCore;
                let mut k = [0u8; 32];
                rand::thread_rng().fill_bytes(&mut k);
                let wrapped = dpapi::protect(&k)?;
                let mut state = state;
                if !state.is_object() {
                    state = json!({});
                }
                state["os_crypt"]["encrypted_key"] = Value::String(base64::engine::general_purpose::STANDARD.encode([DPAPI_TAG, &wrapped].concat()));
                if let Some(parent) = self.local_state.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::write(&self.local_state, serde_json::to_vec(&state).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
                k
            }
        };
        *guard = Some(key);
        Ok(key)
    }

    pub fn is_encryption_available(&self) -> bool {
        cfg!(windows) && self.key().is_ok()
    }

    pub fn decrypt_string(&self, buf: &[u8]) -> Result<String, String> {
        let plain = match buf.strip_prefix(PREFIX) {
            Some(rest) => {
                if rest.len() < 12 + 16 {
                    return Err("encrypted data is too short".into());
                }
                let (nonce, ct) = rest.split_at(12);
                let cipher = Aes256Gcm::new_from_slice(&self.key()?).map_err(|e| e.to_string())?;
                cipher.decrypt(Nonce::from_slice(nonce), ct).map_err(|_| "could not decrypt (wrong key or damaged data)".to_string())?
            }
            None => dpapi::unprotect(buf)?,
        };
        String::from_utf8(plain).map_err(|e| e.to_string())
    }

    pub fn encrypt_string(&self, s: &str) -> Result<Vec<u8>, String> {
        use rand::RngCore;
        let mut nonce = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let cipher = Aes256Gcm::new_from_slice(&self.key()?).map_err(|e| e.to_string())?;
        let ct = cipher.encrypt(Nonce::from_slice(&nonce), s.as_bytes()).map_err(|_| "encryption failed".to_string())?;
        Ok([PREFIX, &nonce[..], &ct[..]].concat())
    }
}

#[cfg(windows)]
mod dpapi {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB};

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 }
    }

    unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(out.pbData as *mut _));
        v
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
        unsafe {
            let mut out = CRYPT_INTEGER_BLOB::default();
            CryptUnprotectData(&blob(data), None, None, None, None, 0, &mut out).map_err(|e| format!("CryptUnprotectData failed: {e}"))?;
            Ok(take(out))
        }
    }

    pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
        unsafe {
            let mut out = CRYPT_INTEGER_BLOB::default();
            CryptProtectData(&blob(data), None, None, None, None, 0, &mut out).map_err(|e| format!("CryptProtectData failed: {e}"))?;
            Ok(take(out))
        }
    }
}

#[cfg(not(windows))]
mod dpapi {
    pub fn unprotect(_: &[u8]) -> Result<Vec<u8>, String> {
        Err("secure storage needs Windows (DPAPI)".into())
    }
    pub fn protect(_: &[u8]) -> Result<Vec<u8>, String> {
        Err("secure storage needs Windows (DPAPI)".into())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn round_trips_in_the_os_crypt_v10_layout() {
        let dir = std::env::temp_dir().join(format!("agu-safe-{}", crate::util::uuid_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = SafeStorage::new(&dir);
        let enc = s.encrypt_string(r#"{"a":1}"#).unwrap();
        assert_eq!(&enc[..3], b"v10");
        assert_eq!(enc.len(), 3 + 12 + 7 + 16);
        // A second instance reads the key back from `Local State`.
        let s2 = SafeStorage::new(&dir);
        assert_eq!(s2.decrypt_string(&enc).unwrap(), r#"{"a":1}"#);
        std::fs::remove_dir_all(&dir).ok();
    }
}
