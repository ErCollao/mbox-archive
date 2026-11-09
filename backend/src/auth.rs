// Multi-tenancy foundation: profile roster (folder-based), password hashing,
// and signed session cookies. Everything here fails closed: a missing or
// unreadable profile means that tenant simply does not exist.

use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};
use tracing::warn;

pub const COOKIE_NAME: &str = "mbx_s";
pub const SESSION_YEARS: i64 = 1;

#[derive(Deserialize)]
pub struct StoredProfile {
    pub name: String,
    /// bcrypt hash ("$2...") or legacy lowercase hex sha256
    pub secret: String,
}

#[derive(Clone)]
pub struct Profile {
    pub slug: String,
    pub name: String,
}

/// One folder per person: profiles/<slug>/profile.json. A folder without a
/// valid profile.json is skipped loudly, never fatal for the others.
pub fn load_profiles(dir: &std::path::Path) -> Vec<Profile> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) => {
            warn!("profiles dir {:?} unreadable: {}", dir, err);
            return out;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let slug = entry.file_name().to_string_lossy().to_string();
        if slug.starts_with('.') || slug.starts_with('_') {
            continue;
        }
        let json_path = path.join("profile.json");
        match fs::read_to_string(&json_path)
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str::<StoredProfile>(&s).map_err(|e| e.to_string()))
        {
            Ok(pf) => out.push(Profile {
                slug,
                name: pf.name,
            }),
            Err(err) => warn!("skipping profile {:?}: {}", json_path, err),
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn sha256_hex(text: &str) -> String {
    hex::encode(<Sha256 as Digest>::digest(text))
}

/// Fixed-order comparison so the check doesn't short-circuit on the first byte.
pub fn secret_matches(expected_hex: &str, password: &str) -> bool {
    let got = sha256_hex(password);
    if got.len() != expected_hex.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in got.bytes().zip(expected_hex.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// 32-byte session key, persisted hex at `path`, generated on first boot.
pub fn session_key(path: &std::path::Path) -> Vec<u8> {
    if let Ok(s) = fs::read_to_string(path) {
        if let Ok(k) = hex::decode(s.trim()) {
            if k.len() == 32 {
                return k;
            }
        }
    }
    let mut key = vec![0u8; 32];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| {
            use std::io::Read;
            f.read_exact(&mut key)
        })
        .expect("cannot read /dev/urandom for session key");
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, hex::encode(&key));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    key
}

/// Cookie value: hex("slug:expiry_epoch") + "." + hex(HMAC-SHA256).
pub fn sign_session(key: &[u8], slug: &str, expiry_epoch: i64) -> String {
    let payload = format!("{}:{}", slug, expiry_epoch);
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(payload.as_bytes());
    format!("{}.{}", hex::encode(payload), hex::encode(mac.finalize().into_bytes()))
}

/// Returns (slug, expiry_epoch) if the signature and expiry check out.
pub fn verify_session(key: &[u8], value: &str, now_epoch: i64) -> Option<(String, i64)> {
    let (payload_hex, sig_hex) = value.split_once('.')?;
    let payload = hex::decode(payload_hex).ok()?;
    let mut expect = Hmac::<Sha256>::new_from_slice(key).ok()?;
    expect.update(&payload);
    expect
        .verify_slice(&hex::decode(sig_hex).ok()?)
        .ok()?;
    let text = String::from_utf8(payload).ok()?;
    let (slug, exp) = text.rsplit_once(':')?;
    let exp = exp.parse::<i64>().ok()?;
    if exp <= now_epoch || slug.is_empty() {
        return None;
    }
    Some((slug.to_string(), exp))
}

fn max_age_remaining(expiry_epoch: i64) -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    (expiry_epoch - now).max(0)
}

fn rfc1123(seconds_left: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(seconds_left))
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string()
}

pub fn set_cookie(name: &str, value: &str, expiry_epoch: i64) -> String {
    let left = max_age_remaining(expiry_epoch);
    format!(
        "{}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}; Expires={}",
        name,
        value,
        left,
        rfc1123(left)
    )
}

pub fn expired_cookie(name: &str) -> String {
    format!("{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT", name)
}

pub fn extract_cookie(header: &str, name: &str) -> Option<String> {
    header
        .split(';')
        .filter_map(|p| p.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

pub fn find_portrait(profiles_dir: &std::path::Path, slug: &str) -> Option<(PathBuf, &'static str)> {
    for (file, mime) in [
        ("portrait.jpg", "image/jpeg"),
        ("portrait.jpeg", "image/jpeg"),
        ("portrait.png", "image/png"),
        ("portrait.webp", "image/webp"),
    ] {
        let p = profiles_dir.join(slug).join(file);
        if p.exists() {
            return Some((p, mime));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Live profile access: read/rewrite profile.json, bcrypt secrets, portraits.
// The password endpoints own these files; login re-reads disk so changes
// apply immediately without a restart.
// ---------------------------------------------------------------------------

pub fn read_profile(dir: &std::path::Path, slug: &str) -> Option<StoredProfile> {
    let raw = fs::read_to_string(dir.join(slug).join("profile.json")).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Legacy = plain sha256 hex; anything starting with `$2` is bcrypt.
pub fn is_legacy(secret: &str) -> bool {
    !secret.starts_with("$2")
}

pub fn verify_secret(stored: &str, password: &str) -> bool {
    if is_legacy(stored) {
        secret_matches(stored, password)
    } else {
        bcrypt::verify(password, stored).unwrap_or(false)
    }
}

pub fn hash_password(password: &str) -> String {
    // cost 10 ≈ a few hundred ms — painful for GPU crackers, invisible to humans.
    bcrypt::hash(password, 10).expect("bcrypt hashing failed")
}

pub fn write_profile(dir: &std::path::Path, slug: &str, name: &str, secret: &str) -> std::io::Result<()> {
    let target = dir.join(slug).join("profile.json");
    let tmp = dir.join(slug).join("profile.json.tmp");
    let body = format!(
        "{{\n  \"name\": {},\n  \"secret\": \"{}\"\n}}\n",
        serde_json::to_string(name).unwrap(),
        secret
    );
    fs::write(&tmp, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    fs::rename(&tmp, &target)
}

pub fn save_portrait(dir: &std::path::Path, slug: &str, bytes: &[u8], ext: &str) -> std::io::Result<()> {
    delete_portrait(dir, slug);
    fs::write(dir.join(slug).join(format!("portrait.{}", ext)), bytes)
}

pub fn delete_portrait(dir: &std::path::Path, slug: &str) {
    for file in ["portrait.jpg", "portrait.jpeg", "portrait.png", "portrait.webp"] {
        let _ = fs::remove_file(dir.join(slug).join(file));
    }
}
