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
/// The expected hash is lowercased: disk input is not trusted to keep our
/// convention (the boot loader does, ad-hoc file edits may not).
pub fn secret_matches(expected_hex: &str, password: &str) -> bool {
    let expected = expected_hex.to_lowercase();
    let got = sha256_hex(password);
    if got.len() != expected.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in got.bytes().zip(expected.bytes()) {
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

/// Short fingerprint of a stored secret. Cookies carry it so that changing
/// (or upgrading) the password instantly invalidates every older session.
pub fn secret_fingerprint(secret: &str) -> String {
    hex::encode(<Sha256 as Digest>::digest(secret))[..16].to_string()
}

/// Cookie value: hex("slug:expiry_epoch:fingerprint") + "." + hex(HMAC-SHA256).
pub fn sign_session(key: &[u8], slug: &str, expiry_epoch: i64, fingerprint: &str) -> String {
    let payload = format!("{}:{}:{}", slug, expiry_epoch, fingerprint);
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(payload.as_bytes());
    format!("{}.{}", hex::encode(payload), hex::encode(mac.finalize().into_bytes()))
}

/// Returns (slug, expiry_epoch, fingerprint) if the signature and expiry check out.
pub fn verify_session(key: &[u8], value: &str, now_epoch: i64) -> Option<(String, i64, String)> {
    let (payload_hex, sig_hex) = value.split_once('.')?;
    let payload = hex::decode(payload_hex).ok()?;
    let mut expect = Hmac::<Sha256>::new_from_slice(key).ok()?;
    expect.update(&payload);
    expect
        .verify_slice(&hex::decode(sig_hex).ok()?)
        .ok()?;
    let text = String::from_utf8(payload).ok()?;
    let (slug, rest) = text.split_once(':')?;
    let (exp, fp) = rest.rsplit_once(':')?;
    let exp = exp.parse::<i64>().ok()?;
    if exp <= now_epoch || slug.is_empty() {
        return None;
    }
    Some((slug.to_string(), exp, fp.to_string()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn key() -> Vec<u8> {
        (0..32u8).collect()
    }
    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }
    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "mboxauth_{}_{}_{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn sign_verify_roundtrip() {
        let k = key();
        let exp = now() + 3600;
        let c = sign_session(&k, "daniel", exp, "deadbeef");
        assert_eq!(verify_session(&k, &c, now()), Some(("daniel".to_string(), exp, "deadbeef".to_string())));
    }

    #[test]
    fn slug_with_weird_chars_survives() {
        // slugs are folder names; ':' is the field separator, everything else must survive
        let k = key();
        let exp = now() + 3600;
        let c = sign_session(&k, "a-h_1", exp, "ff00");
        let (slug, _, fp) = verify_session(&k, &c, now()).unwrap();
        assert_eq!((slug.as_str(), fp.as_str()), ("a-h_1", "ff00"));
    }

    #[test]
    fn tampered_payload_rejected() {
        let k = key();
        let c = sign_session(&k, "daniel", now() + 3600, "aa");
        // swap a hex char of the payload part
        let (ph, sig) = c.split_once('.').unwrap();
        let mut b: Vec<char> = ph.chars().collect();
        b[0] = if b[0] == '0' { '1' } else { '0' };
        let c2 = format!("{}.{}", b.iter().collect::<String>(), sig);
        assert_eq!(verify_session(&k, &c2, now()), None);
    }

    #[test]
    fn tampered_signature_rejected() {
        let k = key();
        let c = sign_session(&k, "daniel", now() + 3600, "aa");
        let (ph, sig) = c.split_once('.').unwrap();
        let mut b: Vec<char> = sig.chars().collect();
        b[5] = if b[5] == 'f' { 'e' } else { 'f' };
        let c2 = format!("{}.{}", ph, b.iter().collect::<String>());
        assert_eq!(verify_session(&k, &c2, now()), None);
    }

    #[test]
    fn wrong_key_rejected() {
        let c = sign_session(&key(), "daniel", now() + 3600, "aa");
        let mut other = key();
        other[0] ^= 1;
        assert_eq!(verify_session(&other, &c, now()), None);
    }

    #[test]
    fn expired_rejected() {
        let k = key();
        let c = sign_session(&k, "daniel", now() - 10, "aa");
        assert_eq!(verify_session(&k, &c, now()), None);
    }

    #[test]
    fn garbage_inputs_rejected() {
        let k = key();
        for bad in ["", "x", "a.b", "..", "zz.zz", "61.b", "61.", ".61", &format!("{}.{}", "zz", "zz")] {
            assert_eq!(verify_session(&k, bad, now()), None, "accepted: {:?}", bad);
        }
    }

    #[test]
    fn extract_cookie_variants() {
        assert_eq!(extract_cookie("mbx_s=abc", "mbx_s").as_deref(), Some("abc"));
        assert_eq!(extract_cookie("other=1; mbx_s=abc ; x=2", "mbx_s").as_deref(), Some("abc"));
        assert_eq!(extract_cookie("other=1; x=2", "mbx_s"), None);
    }

    #[test]
    fn legacy_secret_matching() {
        let h = sha256_hex("daniel");
        assert!(h.len() == 64);
        assert!(secret_matches(&h, "daniel"));
        assert!(!secret_matches(&h, "Daniel"));
        assert!(secret_matches(&h.to_uppercase(), "daniel")); // hex is case-insensitive
    }

    #[test]
    fn is_legacy_detection() {
        assert!(is_legacy("bd3dae5fb91f88a4f0978222dfd58f59a124257cb081486387cbae9df11fb879"));
        assert!(!is_legacy("$2b$10$abcdefghijklmnopqrstuv"));
    }

    #[test]
    fn bcrypt_verify_path() {
        // cost 4 keeps the test suite quick; verify_secret is cost-agnostic
        let h = bcrypt::hash("s3cret", 4).unwrap();
        assert!(!is_legacy(&h));
        assert!(verify_secret(&h, "s3cret"));
        assert!(!verify_secret(&h, "s3cr3t"));
    }

    #[test]
    fn fingerprint_stable_and_distinct() {
        let a = secret_fingerprint("one");
        assert_eq!(a, secret_fingerprint("one"));
        assert_ne!(a, secret_fingerprint("two"));
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn profile_write_read_roundtrip() {
        let d = tmpdir("prof");
        fs::create_dir_all(d.join("hajni")).unwrap();
        write_profile(&d, "hajni", "Hajni", "hash123").unwrap();
        let sp = read_profile(&d, "hajni").unwrap();
        assert_eq!(sp.name, "Hajni");
        assert_eq!(sp.secret, "hash123");
        assert!(!d.join("hajni").join("profile.json.tmp").exists());
        assert!(read_profile(&d, "ghost").is_none());
        // unicode names survive the round-trip
        write_profile(&d, "hajni", "Hajni Хайникова", "x").unwrap();
        assert_eq!(read_profile(&d, "hajni").unwrap().name, "Hajni Хайникова");
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn portrait_lifecycle() {
        let d = tmpdir("port");
        fs::create_dir_all(d.join("daniel")).unwrap();
        assert!(find_portrait(&d, "daniel").is_none());
        save_portrait(&d, "daniel", b"\xff\xd8\xff\xfffake", "jpg").unwrap();
        let (p, mime) = find_portrait(&d, "daniel").unwrap();
        assert_eq!(mime, "image/jpeg");
        // extension switch must not leave a stale sibling behind
        save_portrait(&d, "daniel", b"\x89PNG fake", "png").unwrap();
        assert!(!d.join("daniel").join("portrait.jpg").exists());
        assert!(d.join("daniel").join("portrait.png").exists());
        delete_portrait(&d, "daniel");
        assert!(find_portrait(&d, "daniel").is_none());
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn cookie_format_basics() {
        let exp = now() + 3600;
        let s = set_cookie("mbx_s", "payload.sig", exp);
        assert!(s.starts_with("mbx_s=payload.sig; Path=/; HttpOnly; SameSite=Lax"));
        assert!(s.contains("Max-Age="));
        assert!(s.contains("GMT"));
        assert!(expired_cookie("mbx_s").contains("Max-Age=0"));
    }

    #[test]
    fn session_key_roundtrip() {
        let d = tmpdir("key");
        let p: &Path = &d.join("session.key");
        let k1 = session_key(p);
        assert_eq!(k1.len(), 32);
        let k2 = session_key(p);
        assert_eq!(k1, k2, "key must persist across calls");
        // corrupt file regenerates rather than crashing
        fs::write(p, "not-hex").unwrap();
        assert_eq!(session_key(p).len(), 32);
        fs::remove_dir_all(&d).ok();
    }
}
