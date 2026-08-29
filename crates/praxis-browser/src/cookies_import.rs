//! Import cookies from the user's regular browser — port of
//! `core/cookies_import.py`. Reads the Chromium cookie SQLite DB and
//! decrypts v10/v11 values with platform-native key material, then injects
//! them via CDP `Storage.setCookies`.
//!
//! Decryption is pure Rust (`aes`, `cbc`, `pbkdf2`); the only platform call
//! is fetching the key (macOS `security`, Linux `secret-tool`). v20 (Chrome
//! 127+ App-Bound Encryption) is unsupported and skipped, matching the
//! reference. Windows is unsupported in this build — its DPAPI key unwrap
//! needs an FFI call the crate's `#![forbid(unsafe_code)]` disallows.

use std::path::{Path, PathBuf};

use aes::cipher::{BlockDecryptMut, KeyIvInit};
use chromiumoxide_cdp::cdp::browser_protocol::network::CookieParam;
use chromiumoxide_cdp::cdp::browser_protocol::storage::SetCookiesParams;
use hmac::Hmac;
use serde_json::{json, Value};
use sha1::Sha1;

use crate::connection::Tab;
use crate::{Error, Result};

#[cfg_attr(target_os = "windows", allow(dead_code))]
type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

/// A decrypted cookie, ready for CDP injection.
#[derive(Debug, Clone)]
pub struct ExtractedCookie {
    /// Cookie name.
    pub name: String,
    /// Decrypted value.
    pub value: String,
    /// `host_key`.
    pub domain: String,
    /// Path.
    pub path: String,
    /// `is_secure`.
    pub secure: bool,
    /// `is_httponly`.
    pub http_only: bool,
    /// Unix expiry (seconds), or `None` for a session cookie.
    pub expires: Option<f64>,
}

const PROFILES: [&str; 4] = ["Default", "Profile 1", "Profile 2", "Profile 3"];

fn browser_roots(browser: &str) -> Vec<PathBuf> {
    let home = crate::config::home_dir();
    let j = |p: &str| home.join(p);
    #[cfg(target_os = "macos")]
    let table: &[(&str, &str)] = &[
        ("chrome", "Library/Application Support/Google/Chrome"),
        ("chromium", "Library/Application Support/Chromium"),
        (
            "brave",
            "Library/Application Support/BraveSoftware/Brave-Browser",
        ),
        ("edge", "Library/Application Support/Microsoft Edge"),
    ];
    #[cfg(target_os = "windows")]
    let table: &[(&str, &str)] = &[
        ("chrome", "AppData/Local/Google/Chrome/User Data"),
        ("chromium", "AppData/Local/Chromium/User Data"),
        (
            "brave",
            "AppData/Local/BraveSoftware/Brave-Browser/User Data",
        ),
        ("edge", "AppData/Local/Microsoft/Edge/User Data"),
    ];
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let table: &[(&str, &str)] = &[
        ("chrome", ".config/google-chrome"),
        ("chromium", ".config/chromium"),
        ("brave", ".config/BraveSoftware/Brave-Browser"),
        ("edge", ".config/microsoft-edge"),
    ];
    table
        .iter()
        .filter(|(b, _)| *b == browser)
        .map(|(_, p)| j(p))
        .collect()
}

fn find_cookie_db(browser: &str, user_data_dir: Option<&str>) -> Result<PathBuf> {
    let bases: Vec<PathBuf> = match user_data_dir {
        Some(d) => vec![PathBuf::from(d)],
        None => browser_roots(browser),
    };
    for base in &bases {
        for profile in PROFILES {
            for sub in ["Network/Cookies", "Cookies"] {
                let db = base.join(profile).join(sub);
                if db.exists() {
                    return Ok(db);
                }
            }
        }
    }
    Err(Error::Invalid(format!(
        "Could not find {browser} cookie database. Searched: {bases:?}"
    )))
}

/// Strip Chromium's 32-byte SHA-256 domain hash prefix when present.
#[cfg_attr(target_os = "windows", allow(dead_code))]
fn strip_sha256_prefix(mut plain: Vec<u8>) -> Vec<u8> {
    if plain.len() > 32 && !plain[..32].is_ascii() {
        plain.drain(..32);
    }
    plain
}

#[cfg_attr(target_os = "windows", allow(dead_code))]
fn pbkdf2_key(password: &[u8], iterations: u32) -> [u8; 16] {
    let mut key = [0u8; 16];
    pbkdf2::pbkdf2::<Hmac<Sha1>>(password, b"saltysalt", iterations, &mut key)
        .expect("16 is a valid HMAC-SHA1 output length");
    key
}

#[cfg_attr(target_os = "windows", allow(dead_code))]
fn decrypt_aes_cbc(key: &[u8; 16], ciphertext: &[u8]) -> Option<String> {
    let iv = [b' '; 16];
    let pt = Aes128CbcDec::new(key.into(), &iv.into())
        .decrypt_padded_vec_mut::<aes::cipher::block_padding::Pkcs7>(ciphertext)
        .ok()?;
    Some(String::from_utf8_lossy(&strip_sha256_prefix(pt)).into_owned())
}

#[cfg(target_os = "macos")]
fn platform_key(browser: &str, _db: &Path) -> Result<Vec<u8>> {
    let service = match browser {
        "chrome" => "Chrome Safe Storage",
        "chromium" => "Chromium Safe Storage",
        "brave" => "Brave Safe Storage",
        "edge" => "Microsoft Edge Safe Storage",
        _ => {
            return Err(Error::Invalid(format!(
                "Unknown browser for macOS Keychain: {browser}"
            )))
        }
    };
    let out = std::process::Command::new("/usr/bin/security")
        .args(["-q", "find-generic-password", "-w", "-s", service])
        .output()
        .map_err(|e| Error::Invalid(format!("running /usr/bin/security: {e}")))?;
    if !out.status.success() {
        return Err(Error::Invalid(format!(
            "Failed to get '{service}' password from Keychain (you may need to click 'Allow' in the Keychain dialog)"
        )));
    }
    let password = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(pbkdf2_key(password.as_bytes(), 1003).to_vec())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_key(browser: &str, _db: &Path) -> Result<Vec<u8>> {
    // libsecret via `secret-tool`, else the Chromium fallback "peanuts".
    let password = std::process::Command::new("secret-tool")
        .args(["lookup", "application", browser])
        .output()
        .ok()
        .filter(|o| o.status.success() && !o.stdout.is_empty())
        .map_or_else(
            || "peanuts".to_string(),
            |o| String::from_utf8_lossy(&o.stdout).into_owned(),
        );
    // Linux uses 1 PBKDF2 iteration; the key is derived at decrypt time.
    Ok(password.into_bytes())
}

#[cfg(target_os = "windows")]
fn platform_key(_browser: &str, _db: &Path) -> Result<Vec<u8>> {
    // The DPAPI key unwrap (CryptUnprotectData) needs an FFI call, which the
    // crate's `#![forbid(unsafe_code)]` disallows. macOS and Linux key
    // material comes from safe subprocess calls; Windows would need a
    // `windows`/`winapi` FFI, so it is unsupported in this build.
    Err(Error::Invalid(
        "cookies_import is not supported on Windows in this build (DPAPI needs an FFI call, and the crate forbids unsafe code); run it on macOS or Linux, or use browser_start --profile to reuse a logged-in profile".to_string(),
    ))
}

/// Decrypt one `encrypted_value` blob (returns `None` on any failure, e.g.
/// a v20 App-Bound cookie).
fn decrypt_value(encrypted: &[u8], key: &[u8]) -> Option<String> {
    if encrypted.len() < 3 {
        return None;
    }
    let prefix = &encrypted[..3];
    #[cfg(target_os = "macos")]
    {
        let k: [u8; 16] = key.try_into().ok()?;
        (prefix == b"v10")
            .then(|| decrypt_aes_cbc(&k, &encrypted[3..]))
            .flatten()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if prefix == b"v10" || prefix == b"v11" {
            let k = pbkdf2_key(key, 1);
            decrypt_aes_cbc(&k, &encrypted[3..])
        } else {
            None
        }
    }
    #[cfg(target_os = "windows")]
    {
        let _ = (prefix, key);
        None
    }
}

/// Column indices of the Chromium `cookies` table, parsed from its
/// `CREATE TABLE` text so schema-version differences don't shift us onto the
/// wrong column.
struct CookieCols {
    host_key: Option<usize>,
    name: Option<usize>,
    value: Option<usize>,
    encrypted_value: Option<usize>,
    path: Option<usize>,
    is_secure: Option<usize>,
    is_httponly: Option<usize>,
    expires_utc: Option<usize>,
}

fn cookie_column_indexes(db: &crate::sqlite::Database) -> Result<CookieCols> {
    let sql = db.table_create_sql("cookies")?;
    let cols = crate::sqlite::parse_create_columns(&sql);
    let find = |name: &str| cols.iter().position(|c| c == name);
    Ok(CookieCols {
        host_key: find("host_key"),
        name: find("name"),
        value: find("value"),
        encrypted_value: find("encrypted_value"),
        path: find("path"),
        is_secure: find("is_secure"),
        is_httponly: find("is_httponly"),
        expires_utc: find("expires_utc"),
    })
}

/// Read + decrypt cookies for `domain` from `browser`'s local DB.
pub fn cookies_extract(
    domain: &str,
    browser: &str,
    user_data_dir: Option<&str>,
) -> Result<Vec<ExtractedCookie>> {
    let db_path = find_cookie_db(browser, user_data_dir)?;
    let tmp = std::env::temp_dir().join(format!("adb_cookies_{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let tmp_db = tmp.join("Cookies");
    let _guard = TempDirGuard(tmp.clone());
    std::fs::copy(&db_path, &tmp_db)?;
    for sidecar in ["Cookies-wal", "Cookies-shm"] {
        let src = db_path.with_file_name(sidecar);
        if src.exists() {
            let _ = std::fs::copy(&src, tmp.join(sidecar));
        }
    }

    // Read the `cookies` table with the pure-Rust reader (no C sqlite, so the
    // crate still cross-compiles). Column order in every Chromium schema
    // (v10+): the fields we need are addressed by name below.
    let db = crate::sqlite::Database::open(&tmp_db)?;
    let root = db.table_root("cookies")?;
    // Resolve the column layout from sqlite_master's CREATE TABLE text so we
    // read the right indices regardless of Chromium schema version.
    let idx = cookie_column_indexes(&db)?;

    let mut key: Option<Vec<u8>> = None;
    let mut out = Vec::new();
    let mut v20_skipped = 0u32;
    let mut walk_err: Option<Error> = None;
    db.walk_table(root, &mut |cols| {
        let get_text = |i: Option<usize>| {
            i.and_then(|i| cols.get(i))
                .map(crate::sqlite::SqlValue::as_text)
                .unwrap_or_default()
        };
        let get_int = |i: Option<usize>| {
            i.and_then(|i| cols.get(i))
                .map_or(0, crate::sqlite::SqlValue::as_i64)
        };
        let host_key = get_text(idx.host_key);
        if !host_key.contains(domain) {
            return true;
        }
        let name = get_text(idx.name);
        let plaintext = get_text(idx.value);
        let encrypted = idx
            .encrypted_value
            .and_then(|i| cols.get(i))
            .map(crate::sqlite::SqlValue::as_bytes)
            .unwrap_or_default();
        let path = get_text(idx.path);
        let is_secure = get_int(idx.is_secure);
        let is_httponly = get_int(idx.is_httponly);
        let expires_utc = get_int(idx.expires_utc);

        let value = if !plaintext.is_empty() {
            Some(plaintext)
        } else if !encrypted.is_empty() {
            if key.is_none() {
                match platform_key(browser, &db_path) {
                    Ok(k) => key = Some(k),
                    Err(e) => {
                        walk_err = Some(e);
                        return false;
                    }
                }
            }
            let v = decrypt_value(&encrypted, key.as_deref().unwrap_or(&[]));
            if v.is_none() && encrypted.starts_with(b"v20") {
                v20_skipped += 1;
            }
            v
        } else {
            None
        };
        let Some(value) = value else { return true };

        let expires = if expires_utc > 0 {
            Some((expires_utc as f64) / 1_000_000.0 - 11_644_473_600.0)
        } else {
            None
        };
        out.push(ExtractedCookie {
            name,
            value,
            domain: host_key,
            path,
            secure: is_secure != 0,
            http_only: is_httponly != 0,
            expires: expires.filter(|e| *e > 0.0),
        });
        true
    })?;
    if let Some(e) = walk_err {
        return Err(e);
    }
    if v20_skipped > 0 {
        eprintln!(
            "warning: {v20_skipped} cookie(s) for {domain:?} are v20 (Chrome 127+ App-Bound Encryption) and were skipped (only v10/v11 are supported)"
        );
    }
    Ok(out)
}

/// Extract cookies for `domain` and inject them into the automation session.
/// `{imported, domain, browser, cookies}` or `{imported: 0, …, error}`.
pub async fn cookies_import(
    tab: &Tab,
    domain: &str,
    browser: &str,
    user_data_dir: Option<&str>,
) -> Result<Value> {
    let cookies = cookies_extract(domain, browser, user_data_dir)?;
    if cookies.is_empty() {
        return Ok(json!({
            "imported": 0,
            "domain": domain,
            "browser": browser,
            "error": format!("No cookies found for {domain} in {browser}"),
        }));
    }
    let params: Vec<CookieParam> = cookies
        .iter()
        .map(|c| {
            let mut v = json!({
                "name": c.name,
                "value": c.value,
                "domain": c.domain,
                "path": c.path,
                "secure": c.secure,
                "httpOnly": c.http_only,
            });
            if let Some(e) = c.expires {
                v["expires"] = json!(e);
            }
            serde_json::from_value(v)
        })
        .collect::<std::result::Result<_, _>>()
        .map_err(Error::Json)?;
    tab.browser_connection()
        .send(SetCookiesParams::new(params))
        .await?;
    let listed: Vec<Value> = cookies
        .iter()
        .map(|c| json!({"name": c.name, "domain": c.domain}))
        .collect();
    Ok(json!({
        "imported": cookies.len(),
        "domain": domain,
        "browser": browser,
        "cookies": listed,
    }))
}

struct TempDirGuard(PathBuf);
impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pbkdf2_matches_chromium_macos_params() {
        // Known vector: PBKDF2-HMAC-SHA1("peanuts","saltysalt",1,16).
        let k = pbkdf2_key(b"peanuts", 1);
        assert_eq!(
            k,
            [
                0xfd, 0x62, 0x1f, 0xe5, 0xa2, 0xb4, 0x02, 0x53, 0x9d, 0xfa, 0x14, 0x7c, 0xa9, 0x27,
                0x27, 0x78
            ]
        );
    }

    #[test]
    fn cbc_roundtrip_and_prefix_strip() {
        use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut};
        let key = pbkdf2_key(b"peanuts", 1);
        let iv = [b' '; 16];
        let mut plain = vec![0u8; 32]; // non-ascii SHA256 prefix
        plain[0] = 0xff;
        plain.extend_from_slice(b"secretvalue");
        let ct = cbc::Encryptor::<aes::Aes128>::new(&key.into(), &iv.into())
            .encrypt_padded_vec_mut::<Pkcs7>(&plain);
        assert_eq!(decrypt_aes_cbc(&key, &ct).unwrap(), "secretvalue");
    }
}
