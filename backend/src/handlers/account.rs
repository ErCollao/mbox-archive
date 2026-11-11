use crate::identity::address_counts;
use axum::middleware;
use axum::http::HeaderValue;
use chrono::Utc;
use regex::Regex;
use tracing::{info, warn};
use crate::model::*;
use crate::auth;
use crate::state::{Root, Tenant};
use axum::{extract::{Extension, Path, State}, http::{header, StatusCode}, response::{IntoResponse, Json, Response}};
use std::fs;

/// Rejects requests without a valid session cookie; otherwise injects the
/// matching Tenant for handlers to extract.
pub(crate) async fn require_tenant(
    State(root): State<Root>,
    mut req: axum::extract::Request,
    next: middleware::Next,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    let cookie = req
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| auth::extract_cookie(h, auth::COOKIE_NAME));
    let tenant = cookie
        .as_deref()
        .and_then(|c| auth::verify_session(&root.key, c, Utc::now().timestamp()))
        .and_then(|(slug, _exp, fp)| {
            // A cookie outlives the secret it was issued under only by accident:
            // after a password change the fingerprint mismatches and it dies.
            root.tenants.get(&slug).cloned().filter(|t| {
                t.epoch.read().map(|e| *e == fp).unwrap_or(false)
            })
        });
    match tenant {
        Some(t) => {
            req.extensions_mut().insert(t);
            Ok(next.run(req).await)
        }
        None => Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "unauthorized" })),
        )),
    }
}

/// Chooser data — intentionally the only roster view available pre-login.
/// Read fresh off disk so renames show up without a restart.
pub(crate) async fn list_profiles(State(root): State<Root>) -> Json<Vec<ProfileOut>> {
    Json(
        auth::load_profiles(&root.profiles_dir)
            .into_iter()
            .map(|p| ProfileOut {
                slug: p.slug.clone(),
                name: p.name,
                has_portrait: auth::find_portrait(&root.profiles_dir, &p.slug).is_some(),
            })
            .collect(),
    )
}

pub(crate) async fn login_handler(
    State(root): State<Root>,
    Json(body): Json<LoginIn>,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    let auth_fail = || (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({ "error": "wrong password" })),
    );
    // Read the roster fresh on every login: password changes apply instantly,
    // no restart. Unknown slug and wrong password are indistinguishable.
    let stored = root
        .tenants
        .get(&body.slug)
        .and_then(|_| auth::read_profile(&root.profiles_dir, &body.slug))
        .ok_or_else(auth_fail)?;
    if !auth::verify_secret(&stored.secret, &body.password) {
        return Err(auth_fail());
    }
    // Transparent migration: saltless sha256 -> bcrypt, using the plaintext
    // we already have verified. Worst case it logs and keeps the old hash.
    let final_secret = if auth::is_legacy(&stored.secret) {
        let upgraded = auth::hash_password(&body.password);
        match auth::write_profile_full(&root.profiles_dir, &body.slug, &stored.name, &upgraded, &stored.addresses) {
            Ok(()) => {
                info!("upgraded '{}' secret to bcrypt", body.slug);
                upgraded
            }
            Err(e) => {
                warn!("secret upgrade failed for {}: {}", body.slug, e);
                stored.secret.clone()
            }
        }
    } else {
        stored.secret.clone()
    };
    let fp = auth::secret_fingerprint(&final_secret);
    if let Some(t) = root.tenants.get(&body.slug) {
        if let Ok(mut e) = t.epoch.write() {
            *e = fp.clone();
        }
    }
    let expiry = Utc::now().timestamp() + auth::SESSION_YEARS * 365 * 24 * 3600;
    let val = auth::sign_session(&root.key, &body.slug, expiry, &fp);
    info!("login: {} unlocked their archive", body.slug);
    Ok((
        [(header::SET_COOKIE, auth::set_cookie(auth::COOKIE_NAME, &val, expiry))],
        Json(serde_json::json!({ "ok": true, "slug": body.slug, "name": stored.name })),
    )
        .into_response())
}

pub(crate) async fn logout_handler() -> Response {
    (
        [(header::SET_COOKIE, auth::expired_cookie(auth::COOKIE_NAME))],
        Json(serde_json::json!({ "ok": true })),
    )
        .into_response()
}

/// Display name, read fresh from disk so a rename applies without a restart.
/// Falls back to the boot-time name if the profile can't be read.
pub(crate) fn display_name(root: &Root, t: &Tenant) -> String {
    auth::read_profile(&root.profiles_dir, &t.slug)
        .map(|sp| sp.name)
        .unwrap_or_else(|| t.name.clone())
}

pub(crate) async fn whoami(State(root): State<Root>, Extension(t): Extension<Tenant>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "slug": t.slug, "name": display_name(&root, &t) }))
}

pub(crate) static SLUG_OK: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();

pub(crate) fn slug_re() -> &'static Regex {
    SLUG_OK.get_or_init(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$").unwrap())
}

/// Serves a tenant's portrait pre-login (the chooser needs it). Slug is
/// strictly validated; only fixed portrait filenames are ever looked up.
pub(crate) async fn portrait_handler(
    State(root): State<Root>,
    Path(slug): Path<String>,
) -> Response {
    if !slug_re().is_match(&slug) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    match auth::find_portrait(&root.profiles_dir, &slug)
        .and_then(|(path, mime)| fs::read(&path).ok().map(|b| (b, mime)))
    {
        Some((bytes, mime)) => (
            [
                (header::CONTENT_TYPE, HeaderValue::from_static(mime)),
                (header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=300")),
            ],
            bytes,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "no portrait").into_response(),
    }
}

// ---------------------------------------------------------------------------
// Profile settings: password change and portrait management.
// ---------------------------------------------------------------------------

pub(crate) async fn change_password(
    State(root): State<Root>,
    Extension(t): Extension<Tenant>,
    Json(b): Json<PasswordIn>,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    let server_err = |e: String| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e })),
        )
    };
    if b.new.chars().count() < 6 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "new password needs at least 6 characters" })),
        ));
    }
    let stored = auth::read_profile(&root.profiles_dir, &t.slug).ok_or((
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": "profile file is missing" })),
    ))?;
    if !auth::verify_secret(&stored.secret, &b.current) {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "current password is wrong" })),
        ));
    }
    let new_hash = auth::hash_password(&b.new);
    auth::write_profile_full(&root.profiles_dir, &t.slug, &stored.name, &new_hash, &stored.addresses)
        .map_err(|e| server_err(format!("could not save: {}", e)))?;
    // Revokes every session signed under the old secret...
    let fp = auth::secret_fingerprint(&new_hash);
    if let Ok(mut e) = t.epoch.write() {
        *e = fp.clone();
    }
    // ...except this device, which silently gets a fresh cookie.
    let expiry = Utc::now().timestamp() + auth::SESSION_YEARS * 365 * 24 * 3600;
    let val = auth::sign_session(&root.key, &t.slug, expiry, &fp);
    info!("password changed for '{}' (older sessions revoked)", t.slug);
    Ok((
        [(header::SET_COOKIE, auth::set_cookie(auth::COOKIE_NAME, &val, expiry))],
        Json(serde_json::json!({ "ok": true })),
    )
        .into_response())
}

/// Display-name edit. The stored secret is rewritten verbatim, so neither the
/// session epoch nor anyone's cookie is touched — a rename logs nobody out.
pub(crate) async fn change_name(
    State(root): State<Root>,
    Extension(t): Extension<Tenant>,
    Json(b): Json<NameIn>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let bad = |code: StatusCode, msg: &str| {
        (code, Json(serde_json::json!({ "error": msg })))
    };
    let name = b.name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(bad(StatusCode::BAD_REQUEST, "name needs 1–80 characters"));
    }
    let stored = auth::read_profile(&root.profiles_dir, &t.slug).ok_or_else(|| {
        bad(StatusCode::NOT_FOUND, "profile file is missing")
    })?;
    auth::write_profile_full(&root.profiles_dir, &t.slug, name, &stored.secret, &stored.addresses).map_err(|e| {
        bad(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("could not save: {}", e),
        )
    })?;
    info!("display name set to '{}' for '{}'", name, t.slug);
    Ok(Json(serde_json::json!({ "ok": true, "name": name })))
}

/// Declared identities (accounts & aliases). Saved to the profile but also
/// applied live: tags and counts derive from in-memory entries, so changing
/// the list relabels the entire archive without a rescan.
pub(crate) async fn change_addresses(
    State(root): State<Root>,
    Extension(t): Extension<Tenant>,
    Json(b): Json<AddressesIn>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let bad = |code: StatusCode, msg: String| (code, Json(serde_json::json!({ "error": msg })));
    let mut cleaned: Vec<String> = Vec::new();
    for raw in b.addresses {
        let s = raw.trim().to_lowercase();
        if s.is_empty() || s.contains(' ') || !s.contains('@') || s.len() > 254 {
            return Err(bad(StatusCode::BAD_REQUEST, format!("'{}' is not an email address", raw.trim())));
        }
        if !cleaned.contains(&s) {
            cleaned.push(s);
        }
    }
    if cleaned.len() > 30 {
        return Err(bad(StatusCode::BAD_REQUEST, "at most 30 addresses".to_string()));
    }
    let stored = auth::read_profile(&root.profiles_dir, &t.slug)
        .ok_or_else(|| bad(StatusCode::NOT_FOUND, "profile file is missing".to_string()))?;
    auth::write_profile_full(&root.profiles_dir, &t.slug, &stored.name, &stored.secret, &cleaned)
        .map_err(|e| bad(StatusCode::INTERNAL_SERVER_ERROR, format!("could not save: {}", e)))?;
    *t.addresses.write().unwrap() = cleaned.clone();
    let counts = {
        let lock = t.inner.read().unwrap();
        address_counts(&lock.entries, &cleaned)
    };
    t.inner.write().unwrap().facets.addresses = counts;
    info!("{} addresses set for '{}'", cleaned.len(), t.slug);
    Ok(Json(serde_json::json!({ "ok": true, "addresses": cleaned })))
}

pub(crate) async fn upload_portrait(
    State(root): State<Root>,
    Extension(t): Extension<Tenant>,
    Json(b): Json<PortraitIn>,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    use base64::Engine;
    let bad = |code: StatusCode, msg: &str| {
        Err((
            code,
            Json(serde_json::json!({ "error": msg })),
        ))
    };
    // Base64 standard alphabet never contains ',', so splitting at the last
    // comma strips a data-URL prefix and leaves bare input untouched.
    let payload = b.data.rsplit(',').next().unwrap_or("");
    let bytes = match base64::engine::general_purpose::STANDARD.decode(payload.trim()) {
        Ok(v) => v,
        Err(_) => return bad(StatusCode::BAD_REQUEST, "not a valid base64 payload"),
    };
    if bytes.len() > 12_000_000 {
        return bad(StatusCode::PAYLOAD_TOO_LARGE, "image is larger than 12 MB");
    }
    // Trust magic bytes over whatever the browser claimed.
    let ext = match (bytes.first(), bytes.get(1), bytes.get(2), bytes.get(3)) {
        (Some(0xFF), Some(0xD8), Some(0xFF), _) => "jpg",
        (Some(0x89), Some(0x50), Some(0x4E), Some(0x47)) => "png",
        _ if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" => "webp",
        _ => return bad(StatusCode::BAD_REQUEST, "only JPEG, PNG, or WebP images"),
    };
    if let Err(e) = auth::save_portrait(&root.profiles_dir, &t.slug, &bytes, ext) {
        return bad(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("could not save: {}", e),
        );
    }
    info!("portrait updated for '{}' ({})", t.slug, ext);
    Ok(Json(serde_json::json!({ "ok": true, "kind": ext })).into_response())
}

pub(crate) async fn delete_portrait(
    State(root): State<Root>,
    Extension(t): Extension<Tenant>,
) -> Json<serde_json::Value> {
    auth::delete_portrait(&root.profiles_dir, &t.slug);
    Json(serde_json::json!({ "ok": true }))
}

pub(crate) async fn profile_info(
    State(root): State<Root>,
    Extension(t): Extension<Tenant>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "slug": t.slug,
        "name": display_name(&root, &t),
        "has_portrait": auth::find_portrait(&root.profiles_dir, &t.slug).is_some(),
        "addresses": t.addresses.read().unwrap().clone(),
    }))
}
