use std::path::PathBuf;
use axum::extract::Extension;
mod auth;
pub(crate) mod state;
pub(crate) mod model;
pub(crate) mod identity;
pub(crate) mod index;
pub(crate) mod html;
pub(crate) mod handlers;
#[cfg(test)]
mod api_tests;

use axum::{middleware, routing::{get, post}, Router};
use handlers::{account::*, archive::*};
use state::{seed_tenant, Root};
use std::collections::HashMap;
use std::fs;
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() {
    // Bare fmt::init() + env-filter feature = silent when RUST_LOG is unset; default to info.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let profiles_dir = PathBuf::from(
        std::env::var("PROFILES_DIR").unwrap_or_else(|_| "/profiles".to_string()),
    );
    let data_root = PathBuf::from(
        std::env::var("DATA_DIR").unwrap_or_else(|_| "/data".to_string()),
    );
    let backup_root = PathBuf::from(
        std::env::var("BACKUP_DIR").unwrap_or_else(|_| "/backup".to_string()),
    );
    let archive_root = PathBuf::from(
        std::env::var("ARCHIVE_DIR").unwrap_or_else(|_| "/archive".to_string()),
    );

    // Fail closed: no roster, no startup. An archive that boots anonymous
    // is an archive with no doors, and silent-empty would look like data loss.
    let profiles = auth::load_profiles(&profiles_dir);
    if profiles.is_empty() {
        panic!(
            "No valid profiles found under {:?}. Create profiles/<slug>/profile.json \
             (see README) — refusing to start without at least one authenticated tenant.",
            profiles_dir
        );
    }
    let key = Arc::new(auth::session_key(&archive_root.join("session.key")));

    let mut tenants = HashMap::new();
    for p in profiles {
        let _ = fs::create_dir_all(data_root.join(&p.slug));
        let t = seed_tenant(&profiles_dir, &p.slug, &p.name, &data_root, &archive_root, &backup_root);
        info!("tenant '{}': archive {:?}", t.name, t.archive_dir);
        // Auto-index on boot so browsing works immediately, per tenant.
        let tt = t.clone();
        tokio::spawn(async move { let _ = scan_handler(Extension(tt)).await; });
        tenants.insert(p.slug.clone(), t);
    }
    let root = Root { tenants: Arc::new(tenants), profiles_dir, key };

    let app = build_app(root);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    info!("Server running on port 3000");
    axum::serve(listener, app).await.unwrap();
}

/// The full HTTP surface: public auth routes plus the cookie-protected
/// archive API. Split out of main() so tests can drive it in-process.
pub(crate) fn build_app(root: Root) -> Router {
    let protected = Router::new()
        .route("/api/whoami", get(whoami))
        .route("/api/profile", get(profile_info))
        .route("/api/profile/password", post(change_password))
        .route("/api/profile/name", post(change_name))
        .route("/api/profile/addresses", post(change_addresses))
        .route("/api/profile/portrait", post(upload_portrait).delete(delete_portrait))
        .route("/api/scan", post(scan_handler))
        .route("/api/merge", post(merge_handler))
        .route("/api/filters", get(filters_handler))
        .route("/api/messages", get(list_messages))
        .route("/api/search", get(search_handler))
        .route("/api/email/:id", get(get_email_content))
        .route("/api/email/:id/meta", get(email_meta))
        .route("/api/thread", get(thread_detail))
        .route("/api/attachment/:id/:part", get(get_attachment_data))
        .route("/api/status", get(status_handler))
        .route("/api/backup", post(backup_handler))
        .layer(middleware::from_fn_with_state(root.clone(), require_tenant));

    Router::new()
        .route("/api/profiles", get(list_profiles))
        .route("/api/login", post(login_handler))
        .route("/api/logout", post(logout_handler))
        .route("/profiles/:slug/portrait", get(portrait_handler))
        .merge(protected)
        // No CORS: the SvelteKit server proxies /api same-origin, so the API
        // has no legitimate cross-origin browser caller — refusing CORS means
        // other websites cannot poke this appliance with a user's cookie.
        .with_state(root)
}
