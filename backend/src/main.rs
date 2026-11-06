use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use mboxshell::model::attachment::AttachmentMeta;
use mboxshell::model::mail::MailEntry;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, RwLock},
};
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
    facets: Facets,
}

#[derive(Serialize, Clone)]
struct Facets {
    labels: Vec<(String, u64)>,
    min_ts: i64,
    max_ts: i64,
}

impl Default for Facets {
    fn default() -> Self {
        Facets { labels: Vec::new(), min_ts: 0, max_ts: 0 }
    }
}

#[derive(Deserialize)]
struct FilterParams {
    q: Option<String>,
    label: Option<String>,
    after: Option<String>,
    before: Option<String>,
    attachment: Option<String>,
    body: Option<String>,
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

#[derive(Serialize)]
struct ScanResponse {
    message: String,
    status: String,
}

#[derive(Serialize)]
struct AttachmentInfo {
    filename: String,
    content_type: String,
    size: u64,
    is_inline: bool,
    part_index: usize,
}

#[derive(Serialize)]
struct EmailMetaOut {
    id: String,
    subject: String,
    from: String,
    to: Vec<String>,
    date: String,
    labels: Vec<String>,
    attachments: Vec<AttachmentInfo>,
    inline_images: usize,
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

fn parse_bound(s: &Option<String>) -> Option<DateTime<Utc>> {
    let trimmed = s.as_deref().unwrap_or("").trim();
    if trimmed.is_empty() {
        return None;
    }
    let date = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d").ok()?;
    date.and_hms_opt(0, 0, 0)
        .map(|n| Utc.from_utc_datetime(&n))
}

fn label_selection(csv: &Option<String>) -> Vec<String> {
    csv.as_deref()
        .map(|s| {
            s.split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn entry_in_filters(
    entry: &MailEntry,
    labels: &[String],
    after: &Option<DateTime<Utc>>,
    before: &Option<DateTime<Utc>>,
    want_attachment: bool,
) -> bool {
    if !labels.is_empty()
        && !labels.iter().any(|sel| {
            entry.labels.iter().any(|l| l.eq_ignore_ascii_case(sel))
        })
    {
        return false;
    }
    if let Some(a) = after {
        if entry.date < *a {
            return false;
        }
    }
    // `before:` excludes the given day, matching mboxshell semantics.
    if let Some(b) = before {
        if entry.date >= *b {
            return false;
        }
    }
    if want_attachment && !entry.has_attachments {
        return false;
    }
    true
}

/// Build an mboxshell query from structured search params. Plain words match
/// headers only (instant); bodies require the explicit `body` flag. Quoted or
/// field-syntax queries pass through untouched.
fn compose_query(p: &FilterParams) -> String {
    let mut parts: Vec<String> = Vec::new();
    let raw = p.q.clone().unwrap_or_default();
    let raw = raw.trim();
    let want_body = p.body.as_deref() == Some("1");

    if !raw.is_empty() {
        if raw.contains(':') || raw.contains('"') {
            parts.push(raw.to_string());
        } else {
            let toks: Vec<String> = raw
                .split_whitespace()
                .map(|t| {
                    if want_body {
                        format!("body:{t}")
                    } else {
                        format!("subject:{t} OR from:{t} OR to:{t}")
                    }
                })
                .collect();
            if !toks.is_empty() {
                parts.push(toks.join(" "));
            }
        }
    }

    let labels = label_selection(&p.label);
    if !labels.is_empty() {
        parts.push(
            labels
                .iter()
                .map(|l| format!("label:\"{}\"", l.replace('"', "")))
                .collect::<Vec<_>>()
                .join(" OR "),
        );
    }
    if p.attachment.as_deref() == Some("1") {
        parts.push("has:attachment".to_string());
    }
    if parse_bound(&p.after).is_some() {
        parts.push(format!("after:{}", p.after.as_deref().unwrap().trim()));
    }
    if parse_bound(&p.before).is_some() {
        parts.push(format!("before:{}", p.before.as_deref().unwrap().trim()));
    }

    parts.join(" ")
}

fn compute_facets(entries: &[MailEntry]) -> Facets {
    let floor = NaiveDate::from_ymd_opt(2000, 1, 1)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|n| Utc.from_utc_datetime(&n))
        .unwrap();
    let now = Utc::now();
    let mut counts: HashMap<String, u64> = HashMap::new();
    let mut min: Option<DateTime<Utc>> = None;
    let mut max: Option<DateTime<Utc>> = None;
    for e in entries {
        for l in &e.labels {
            *counts.entry(l.clone()).or_insert(0) += 1;
        }
        if e.date >= floor && e.date <= now {
            min = Some(min.map_or(e.date, |m| m.min(e.date)));
            max = Some(max.map_or(e.date, |m| m.max(e.date)));
        }
    }
    let mut labels: Vec<(String, u64)> = counts.into_iter().collect();
    labels.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    labels.truncate(200);
    Facets {
        labels,
        min_ts: min.map(|d| d.timestamp_millis()).unwrap_or(0),
        max_ts: max.map(|d| d.timestamp_millis()).unwrap_or(0),
    }
}

fn rewrite_cid(html: &str, atts: &[AttachmentMeta], mail_id: &str) -> String {
    if !html.contains("cid:") {
        return html.to_string();
    }
    let strip = |s: &str| s.trim_matches(|c| c == '<' || c == '>').to_lowercase();
    let map: HashMap<String, String> = atts
        .iter()
        .filter(|a| a.is_inline && a.content_id.is_some())
        .map(|a| {
            (
                strip(a.content_id.as_deref().unwrap()),
                format!("/api/attachment/{}/{}", mail_id, a.part_index),
            )
        })
        .collect();
    let re = match Regex::new(r#"(?i)cid:([^"'\s>]+)"#) {
        Ok(r) => r,
        Err(_) => return html.to_string(),
    };
    re.replace_all(html, |caps: &regex::Captures| {
        let key = strip(&caps[1]);
        map.get(&key).cloned().unwrap_or_else(|| caps[0].to_string())
    })
    .into_owned()
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
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
            facets: Facets::default(),
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
        .route("/api/filters", get(filters_handler))
        .route("/api/messages", get(list_messages))
        .route("/api/search", get(search_handler))
        .route("/api/email/:id", get(get_email_content))
        .route("/api/email/:id/meta", get(email_meta))
        .route("/api/attachment/:id/:part", get(get_attachment_data))
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
                let facets = compute_facets(&entries);
                let mut order: Vec<usize> = (0..entries.len()).collect();
                order.sort_by(|&a, &b| entries[b].date.cmp(&entries[a].date));
                let mut lock = inner.write().unwrap();
                lock.mbox_path = Some(target_path);
                lock.facets = facets;
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

async fn filters_handler(State(state): State<AppState>) -> Json<Facets> {
    let facets = state.inner.read().unwrap().facets.clone();
    Json(facets)
}

async fn list_messages(
    State(state): State<AppState>,
    Query(params): Query<FilterParams>,
) -> Json<Vec<EmailMetadata>> {
    let inner = state.inner.clone();
    let items = tokio::task::spawn_blocking(move || {
        let lock = inner.read().unwrap();
        let offset = params.offset.unwrap_or(0);
        let limit = params.limit.unwrap_or(100).clamp(1, 500);
        let labels = label_selection(&params.label);
        let after = parse_bound(&params.after);
        let before = parse_bound(&params.before);
        let want_att = params.attachment.as_deref() == Some("1");
        let filtered = !labels.is_empty()
            || after.is_some()
            || before.is_some()
            || want_att;

        if !filtered {
            return match lock.order.get(offset..) {
                Some(slice) => slice
                    .iter()
                    .take(limit)
                    .map(|&idx| metadata_for(&lock, idx))
                    .collect(),
                None => Vec::new(),
            };
        }

        let mut out = Vec::new();
        let mut skipped = 0usize;
        for &idx in &lock.order {
            if !entry_in_filters(&lock.entries[idx], &labels, &after, &before, want_att) {
                continue;
            }
            if skipped < offset {
                skipped += 1;
                continue;
            }
            if out.len() >= limit {
                break;
            }
            out.push(metadata_for(&lock, idx));
        }
        out
    })
    .await
    .unwrap_or_else(|_| Vec::new());

    Json(items)
}

async fn search_handler(
    State(state): State<AppState>,
    Query(params): Query<FilterParams>,
) -> Json<Vec<EmailMetadata>> {
    let inner = state.inner.clone();
    let q = compose_query(&params);
    if q.trim().is_empty() {
        return Json(Vec::new());
    }

    // The body-scan path can run for minutes on large archives: keep it off
    // the async runtime and make its progress visible in the logs.
    let items = tokio::task::spawn_blocking(move || {
        let lock = inner.read().unwrap();
        let mbox_path = match &lock.mbox_path {
            Some(p) => p.clone(),
            None => return Vec::new(),
        };
        let progress = |processed: usize, total: usize| {
            if processed > 0 && processed % 5000 == 0 {
                info!("Body search progress: {}/{} messages", processed, total);
            }
            true
        };
        match search::execute(&mbox_path, &lock.entries, &q, Some(&progress)) {
            Ok((_, mut results)) => {
                results.sort_unstable_by(|&a, &b| lock.entries[b].date.cmp(&lock.entries[a].date));
                results
                    .iter()
                    .take(50)
                    .map(|&idx| metadata_for(&lock, idx))
                    .collect()
            }
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
                let html = rewrite_cid(html, &body.attachments, &id);
                // Untrusted email HTML: isolate it from this origin
                let mut headers = HeaderMap::new();
                headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox"));
                headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
                (headers, axum::response::Html(html)).into_response()
            } else if let Some(text) = &body.text {
                text.clone().into_response()
            } else {
                "No content".to_string().into_response()
            }
        }
        Err(e) => format!("Error reading email: {}", e).into_response(),
    }
}

async fn email_meta(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let inner = state.inner.clone();
    let out = tokio::task::spawn_blocking(move || -> Option<EmailMetaOut> {
        let idx = id.parse::<usize>().ok()?;
        let lock = inner.read().unwrap();
        let mbox_path = lock.mbox_path.clone()?;
        let entry = lock.entries.get(idx)?;
        let mut store = MboxStore::open(&mbox_path).ok()?;
        let body = store.get_message(entry).ok()?;
        Some(EmailMetaOut {
            id: idx.to_string(),
            subject: entry.subject.clone(),
            from: format!("{} <{}>", entry.from.display_name, entry.from.address),
            to: entry
                .to
                .iter()
                .map(|a| format!("{} <{}>", a.display_name, a.address))
                .collect(),
            date: entry.date.to_rfc3339(),
            labels: entry.labels.clone(),
            attachments: body
                .attachments
                .iter()
                .map(|a| AttachmentInfo {
                    filename: a.filename.clone(),
                    content_type: a.content_type.clone(),
                    size: a.size,
                    is_inline: a.is_inline,
                    part_index: a.part_index,
                })
                .collect(),
            inline_images: body
                .attachments
                .iter()
                .filter(|a| a.is_inline && a.content_type.starts_with("image/"))
                .count(),
        })
    })
    .await
    .ok()
    .flatten();

    match out {
        Some(m) => Json(m).into_response(),
        None => (StatusCode::NOT_FOUND, "Email not found").into_response(),
    }
}

async fn get_attachment_data(
    State(state): State<AppState>,
    Path((id, part)): Path<(String, usize)>,
) -> impl IntoResponse {
    let inner = state.inner.clone();
    let res = tokio::task::spawn_blocking(move || -> Option<(Vec<u8>, String, String)> {
        let idx = id.parse::<usize>().ok()?;
        let lock = inner.read().unwrap();
        let mbox_path = lock.mbox_path.clone()?;
        let entry = lock.entries.get(idx)?;
        let mut store = MboxStore::open(&mbox_path).ok()?;
        let body = store.get_message(entry).ok()?;
        let meta = body.attachments.iter().find(|a| a.part_index == part)?;
        let bytes = store.get_attachment(entry, meta).ok()?;
        Some((bytes, meta.content_type.clone(), meta.filename.clone()))
    })
    .await
    .unwrap_or(None);

    match res {
        Some((bytes, content_type, filename)) => {
            // Never render active or markup content inline, only download it.
            let ct = content_type.to_lowercase();
            let ct = if ct.contains("html")
                || ct.contains("xml")
                || ct.contains("svg")
                || ct.contains("script")
            {
                "application/octet-stream".to_string()
            } else {
                content_type
            };
            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(&ct).unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")));
            headers.insert(
                header::CONTENT_DISPOSITION,
                HeaderValue::from_str(&format!("attachment; filename*=UTF-8''{}", percent_encode(&filename)))
                    .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
            );
            headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private"));
            (headers, bytes).into_response()
        }
        None => (StatusCode::NOT_FOUND, "Attachment not found").into_response(),
    }
}
