use axum::{
    extract::{Path, Query, State},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path as StdPath, PathBuf},
    sync::{Arc, RwLock},
};
use mboxshell::model::mail::MailEntry;
use mboxshell::index::builder;
use mboxshell::search;
use mboxshell::store::reader::MboxStore;
use tower_http::cors::CorsLayer;
use walkdir::WalkDir;
use tracing::{info, error};

#[derive(Clone)]
struct AppState {
    inner: Arc<RwLock<AppStateInner>>,
    data_dir: PathBuf,
}

struct AppStateInner {
    mbox_path: Option<PathBuf>,
    entries: Vec<MailEntry>,
}

#[derive(Serialize, Deserialize)]
struct SearchQuery {
    q: String,
}

#[derive(Serialize)]
struct EmailMetadata {
    id: String,
    subject: String,
    from: String,
    date: String,
    snippet: String,
}

#[derive(Serialize)]
struct ScanResponse {
    message: String,
    status: String,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let data_dir = PathBuf::from("/data");
    fs::create_dir_all(&data_dir).unwrap();

    let state = AppState {
        inner: Arc::new(RwLock::new(AppStateInner {
            mbox_path: None,
            entries: Vec::new(),
        })),
        data_dir,
    };

    let app = Router::new()
        .route("/api/scan", post(scan_handler))
        .route("/api/merge", post(merge_handler))
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
                let mut lock = inner.write().unwrap();
                lock.mbox_path = Some(target_path);
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

async fn search_handler(
    State(state): State<AppState>,
    Query(params): Query<SearchQuery>,
) -> Json<Vec<EmailMetadata>> {
    let lock = state.inner.read().unwrap();
    let mbox_path = match &lock.mbox_path {
        Some(p) => p,
        None => return Json(vec![]),
    };
    
    if params.q.trim().is_empty() {
        return Json(vec![]);
    }

    match search::execute(mbox_path, &lock.entries, &params.q, None) {
        Ok((_, results)) => {
            let mut items = Vec::new();
            for &idx in results.iter().take(50) {
                let entry = &lock.entries[idx];
                items.push(EmailMetadata {
                    id: idx.to_string(),
                    subject: entry.subject.clone(),
                    from: format!("{} <{}>", entry.from.display_name, entry.from.address),
                    date: entry.date.to_rfc3339(),
                    snippet: "".to_string(),
                });
            }
            Json(items)
        }
        Err(_) => Json(vec![]),
    }
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
                axum::response::Html(html.clone()).into_response()
            } else if let Some(text) = &body.text {
                text.clone().into_response()
            } else {
                "No content".to_string().into_response()
            }
        }
        Err(e) => format!("Error reading email: {}", e).into_response(),
    }
}
