use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, RwLock},
};
use mboxshell::model::mail::MailEntry;
use mboxshell::index::builder;
use mboxshell::search;
use mboxshell::store::reader::MboxStore;
use tower_http::cors::CorsLayer;
use walkdir::WalkDir;
use tracing::{info, error, warn};

#[derive(Clone)]
struct AppState {
    inner: Arc<RwLock<AppStateInner>>,
    data_dir: PathBuf,
}

struct AppStateInner {
    mbox_path: Option<PathBuf>,
    entries: Vec<MailEntry>,
    order: Vec<usize>,
}

#[derive(Serialize, Deserialize)]
struct SearchQuery {
    q: String,
}

#[derive(Deserialize)]
struct ListQuery {
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Serialize)]
struct EmailMetadata {
    id: String,
    subject: String,
    from: String,
    date: String,
    snippet: String,
}

fn metadata_for(inner: &AppStateInner, idx: usize) -> EmailMetadata {
    let entry = &inner.entries[idx];
    EmailMetadata {
        id: idx.to_string(),
        subject: entry.subject.clone(),
        from: format!("{} <{}>", entry.from.display_name, entry.from.address),
        date: entry.date.to_rfc3339(),
        snippet: String::new(),
    }
}

#[derive(Serialize)]
struct ScanResponse {
    message: String,
    status: String,
}

#[tokio::main]
async fn main() {
    // Bare fmt::init() + env-filter feature = silent when RUST_LOG is unset; default to info.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let data_dir = PathBuf::from("/data");
    fs::create_dir_all(&data_dir).unwrap();

    let state = AppState {
        inner: Arc::new(RwLock::new(AppStateInner {
            mbox_path: None,
            entries: Vec::new(),
            order: Vec::new(),
        })),
        data_dir,
    };

    // Auto-load the persistent index on boot so browsing works immediately.
    {
        let s = state.clone();
        tokio::spawn(async move { scan_handler(State(s)).await; });
    }

    let app = Router::new()
        .route("/api/scan", post(scan_handler))
        .route("/api/merge", post(merge_handler))
        .route("/api/messages", get(list_messages))
        .route("/api/search", get(search_handler))
        .route("/api/email/:id", get(get_email_content))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    info!("Server running on port 3000");
    axum::serve(listener, app).await.unwrap();
}

async fn merge_handler(State(state): State<AppState>) -> Json<ScanResponse> {
    let data_dir = state.data_dir.clone();
    
    tokio::task::spawn_blocking(move || {
        let mut mbox_files = Vec::new();
        for entry in WalkDir::new(&data_dir).into_iter().filter_map(|e| e.ok()) {
            if entry.path().extension().map_or(false, |ext| ext == "mbox") {
                if entry.file_name() != "merged.mbox" {
                    mbox_files.push(entry.path().to_path_buf());
                }
            }
        }

        if mbox_files.is_empty() {
             return;
        }

        let output = data_dir.join("merged.mbox");
        info!("Merging {} files into {:?}", mbox_files.len(), output);
        
        let res = mboxshell::export::mbox::merge_mbox_files(
            &mbox_files,
            &output,
            true, // dedup
            false, // add_source_header
            &|curr, total, name| {
                info!("Merging file {}/{}: {}", curr, total, name);
            }
        );

        match res {
            Ok(stats) => info!("Merge complete: {:?}", stats),
            Err(e) => error!("Merge failed: {}", e),
        }
    });

    Json(ScanResponse {
        message: "Merge process started".to_string(),
        status: "processing".to_string(),
    })
}

async fn scan_handler(State(state): State<AppState>) -> Json<ScanResponse> {
    let inner = state.inner.clone();
    let data_dir = state.data_dir.clone();

    tokio::task::spawn_blocking(move || {
        let merged_path = data_dir.join("merged.mbox");
        let target_path = if merged_path.exists() {
            merged_path
        } else {
            // Fallback to the first mbox file found if merged doesn't exist
            let mut first = None;
            for entry in WalkDir::new(&data_dir).into_iter().filter_map(|e| e.ok()) {
                 if entry.path().extension().map_or(false, |ext| ext == "mbox") {
                     first = Some(entry.path().to_path_buf());
                     break;
                 }
            }
            match first {
                Some(p) => p,
                None => {
                    error!("No mbox files found to scan");
                    return;
                }
            }
        };

        info!("Indexing: {:?}", target_path);
        match builder::build_index(&target_path, false, None) {
            Ok(entries) => {
                let mut order: Vec<usize> = (0..entries.len()).collect();
                order.sort_by(|&a, &b| entries[b].date.cmp(&entries[a].date));
                let mut lock = inner.write().unwrap();
                lock.mbox_path = Some(target_path);
                lock.order = order;
                lock.entries = entries;
                info!("Indexing complete. Total messages: {}", lock.entries.len());
            }
            Err(e) => error!("Indexing failed: {}", e),
        }
    });

    Json(ScanResponse {
        message: "Scanning started".to_string(),
        status: "processing".to_string(),
    })
}

async fn list_messages(
    State(state): State<AppState>,
    Query(params): Query<ListQuery>,
) -> Json<Vec<EmailMetadata>> {
    let inner = state.inner.clone();
    let items = tokio::task::spawn_blocking(move || {
        let lock = inner.read().unwrap();
        let offset = params.offset.unwrap_or(0);
        let limit = params.limit.unwrap_or(100).clamp(1, 500);
        match lock.order.get(offset..) {
            Some(slice) => slice.iter().take(limit).map(|&idx| metadata_for(&lock, idx)).collect(),
            None => Vec::new(),
        }
    })
    .await
    .unwrap_or_else(|_| Vec::new());

    Json(items)
}

/// Plain words (no field syntax) search headers only, keeping search instant
/// on multi-GB archives. Body content requires explicit `body:` terms.
fn default_metadata_query(q: &str) -> String {
    if q.contains(':') || q.contains('"') {
        return q.to_string();
    }
    q.split_whitespace()
        .map(|t| format!("subject:{t} OR from:{t} OR to:{t}"))
        .collect::<Vec<_>>()
        .join(" ")
}

async fn search_handler(
    State(state): State<AppState>,
    Query(params): Query<SearchQuery>,
) -> Json<Vec<EmailMetadata>> {
    let inner = state.inner.clone();
    let q = params.q;
    // The body-scan path can run for minutes on large archives: keep it off
    // the async runtime and make its progress visible in the logs.
    let items = tokio::task::spawn_blocking(move || {
        let lock = inner.read().unwrap();
        let mbox_path = match &lock.mbox_path {
            Some(p) => p.clone(),
            None => return Vec::new(),
        };
        if q.trim().is_empty() {
            return Vec::new();
        }
        let q = default_metadata_query(&q);
        let progress = |processed: usize, total: usize| {
            if processed > 0 && processed % 5000 == 0 {
                info!("Body search progress: {}/{} messages", processed, total);
            }
            true
        };
        match search::execute(&mbox_path, &lock.entries, &q, Some(&progress)) {
            Ok((_, results)) => results
                .iter()
                .take(50)
                .map(|&idx| metadata_for(&lock, idx))
                .collect(),
            Err(e) => {
                warn!("Search failed: {}", e);
                Vec::new()
            }
        }
    })
    .await
    .unwrap_or_else(|_| Vec::new());

    Json(items)
}

async fn get_email_content(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let idx: usize = match id.parse() {
        Ok(i) => i,
        Err(_) => return "Invalid ID".to_string().into_response(),
    };

    let lock = state.inner.read().unwrap();
    let mbox_path = match &lock.mbox_path {
        Some(p) => p,
        None => return "No MBOX loaded".to_string().into_response(),
    };

    let entry = match lock.entries.get(idx) {
        Some(e) => e,
        None => return "Email not found".to_string().into_response(),
    };

    let mut store = match MboxStore::open(mbox_path) {
        Ok(s) => s,
        Err(_) => return "Could not open MBOX".to_string().into_response(),
    };

    match store.get_message(entry) {
        Ok(body) => {
            // Prefer HTML if available, otherwise Text
            if let Some(html) = &body.html {
                // Untrusted email HTML: isolate it from this origin
                let mut headers = HeaderMap::new();
                headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox"));
                headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
                return (headers, axum::response::Html(html.clone())).into_response();
            } else if let Some(text) = &body.text {
                text.clone().into_response()
            } else {
                "No content".to_string().into_response()
            }
        }
        Err(e) => format!("Error reading email: {}", e).into_response(),
    }
}
