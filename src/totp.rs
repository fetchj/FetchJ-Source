use hmac::{Hmac, Mac};
use sha1::Sha1;
use std::path::{Path, PathBuf};

const FILE: &str = "FetchJ_totp.bin";
const STEP: u64 = 30;
const DIGITS: u32 = 6;

fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE)
}

pub fn base32(s: &str) -> Option<Vec<u8>> {
    let mut bits: u64 = 0;
    let mut n = 0u32;
    let mut out = Vec::new();
    for c in s.chars() {
        if c.is_whitespace() || c == '-' || c == '=' {
            continue;
        }
        let v = match c.to_ascii_uppercase() {
            ch @ 'A'..='Z' => ch as u64 - 'A' as u64,
            ch @ '2'..='7' => ch as u64 - '2' as u64 + 26,
            _ => return None,
        };
        bits = (bits << 5) | v;
        n += 5;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
            bits &= (1u64 << n) - 1;
        }
    }
    (!out.is_empty()).then_some(out)
}

pub fn parse(input: &str) -> Option<Vec<u8>> {
    let t = input.trim();
    if t.to_ascii_lowercase().starts_with("otpauth://") {
        let q = t.split_once('?')?.1;
        let sec = q.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            k.eq_ignore_ascii_case("secret").then(|| v.replace("%3D", "=").replace("%20", ""))
        })?;
        return base32(&sec);
    }
    base32(t)
}

pub fn code_at(key: &[u8], t: u64) -> String {
    let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(key).expect("HMAC takes every key length");
    mac.update(&(t / STEP).to_be_bytes());
    let h = mac.finalize().into_bytes();
    let o = (h[h.len() - 1] & 0x0f) as usize;
    let bin = ((h[o] as u32 & 0x7f) << 24) | ((h[o + 1] as u32) << 16) | ((h[o + 2] as u32) << 8) | h[o + 3] as u32;
    format!("{:0width$}", bin % 10u32.pow(DIGITS), width = DIGITS as usize)
}

pub fn secs_valid(t: u64) -> u64 {
    STEP - t % STEP
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn supported() -> bool {
    cfg!(windows)
}

pub fn stored(state_dir: &Path) -> bool {
    path(state_dir).is_file()
}

pub fn save(state_dir: &Path, input: &str) -> Result<(), String> {
    let key = parse(input).ok_or_else(|| "format".to_string())?;
    if key.len() < 10 {
        return Err("format".into());
    }
    let enc = protect(&key).ok_or_else(|| "os".to_string())?;
    let _ = std::fs::create_dir_all(state_dir);
    std::fs::write(path(state_dir), enc).map_err(|_| "io".to_string())
}

pub fn clear(state_dir: &Path) {
    let _ = std::fs::remove_file(path(state_dir));
}

pub fn current(state_dir: &Path) -> Option<(String, u64)> {
    let enc = std::fs::read(path(state_dir)).ok()?;
    let key = unprotect(&enc)?;
    let t = now();
    Some((code_at(&key, t), secs_valid(t)))
}

#[cfg(windows)]
fn dpapi(data: &[u8], encrypt: bool) -> Option<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        if encrypt {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        }
    };
    if ok == 0 || out.pbData.is_null() {
        return None;
    }
    let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
    unsafe {
        LocalFree(out.pbData as _);
    }
    Some(v)
}

#[cfg(windows)]
pub(crate) fn protect(key: &[u8]) -> Option<Vec<u8>> {
    dpapi(key, true)
}
#[cfg(windows)]
pub(crate) fn unprotect(enc: &[u8]) -> Option<Vec<u8>> {
    dpapi(enc, false)
}
#[cfg(not(windows))]
pub(crate) fn protect(_key: &[u8]) -> Option<Vec<u8>> {
    None
}
#[cfg(not(windows))]
pub(crate) fn unprotect(_enc: &[u8]) -> Option<Vec<u8>> {
    None
}
