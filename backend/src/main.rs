mod auth;

use axum::{
    extract::{Extension, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use mboxshell::model::attachment::AttachmentMeta;
use mboxshell::model::mail::MailEntry;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::collections::HashMap;
use std::io::{BufReader, BufWriter, Read, Write};
use std::time::SystemTime;
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

/// One person's archive: everything handlers used to share globally is now
/// scoped to a tenant. Handlers receive it via Extension after cookie auth.
#[derive(Clone)]
struct Tenant {
    slug: String,
    name: String,
    /// fingerprint of the current secret; sessions carry it, so a password
    /// change revokes all older cookies. Reread at boot, updated live on change.
    epoch: Arc<RwLock<String>>,
    inner: Arc<RwLock<AppStateInner>>,
    data_dir: PathBuf,
    archive_dir: PathBuf,
    backup_dir: PathBuf,
    busy: Arc<AtomicBool>,
    backup: Arc<Mutex<BackupProgress>>,
}

/// Global state: the tenant map plus everything the auth layer needs.
#[derive(Clone)]
struct Root {
    tenants: Arc<HashMap<String, Tenant>>,
    profiles_dir: PathBuf,
    key: Arc<Vec<u8>>,
}

#[derive(Serialize, Clone, Default)]
struct BackupProgress {
    running: bool,
    phase: String,
    done: u64,
    total: u64,
}

#[derive(Default)]
struct AppStateInner {
    mbox_path: Option<PathBuf>,
    entries: Vec<MailEntry>,
    order: Vec<usize>,
    facets: Facets,
    threads: Vec<Thread>,
    thread_order: Vec<usize>,
    tmap: HashMap<String, usize>,
}

#[derive(Clone)]
struct Thread {
    tid: String,
    msgs: Vec<usize>,
    subject: String,
    from: String,
    date: DateTime<Utc>,
    attachments: bool,
}

#[derive(Serialize)]
struct ThreadOut {
    tid: String,
    subject: String,
    from: String,
    date: String,
    count: usize,
    matched: usize,
    attachments: bool,
}

#[derive(Serialize, Clone)]
struct Facets {
    labels: Vec<(String, u64)>,
    min_ts: i64,
    max_ts: i64,
    total: u64,
}

impl Default for Facets {
    fn default() -> Self {
        Facets { labels: Vec::new(), min_ts: 0, max_ts: 0, total: 0 }
    }
}

#[derive(Deserialize)]
struct FilterParams {
    q: Option<String>,
    cat: Option<String>,
    status: Option<String>,
    other: Option<String>,
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
    attachments: bool,
}

#[derive(Serialize)]
struct ScanResponse {
    message: String,
    status: String,
}

/// Clears the shared busy flag on scope exit, however the task returns.
struct BusyGuard(Arc<AtomicBool>);
impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Ensures the backup progress UI never gets stuck on a stale state.
struct BackupGuard(Arc<Mutex<BackupProgress>>);
impl Drop for BackupGuard {
    fn drop(&mut self) {
        let mut p = self.0.lock().unwrap();
        *p = BackupProgress::default();
    }
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
        attachments: entry.has_attachments,
    }
}

fn thread_out(t: &Thread, matched: usize) -> ThreadOut {
    ThreadOut {
        tid: t.tid.clone(),
        subject: t.subject.clone(),
        from: t.from.clone(),
        date: t.date.to_rfc3339(),
        count: t.msgs.len(),
        matched,
        attachments: t.attachments,
    }
}

fn clean_subject(s: &str) -> String {
    let re = match Regex::new(r"(?i)^(re|fw|fwd)(\[\d+\])?:\s*") {
        Ok(r) => r,
        Err(_) => return s.trim().to_string(),
    };
    let mut cur = s.trim().to_string();
    while let Some(m) = re.find(&cur) {
        let next = cur[m.end()..].trim().to_string();
        if next.is_empty() {
            break;
        }
        cur = next;
    }
    cur
}

fn tid_of(entries: &[MailEntry], idx: usize) -> String {
    match entries[idx].thread_id.as_deref() {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => format!("solo-{}", idx),
    }
}

/// Base font for email documents: most emails never declare a global font and
/// would otherwise render in browser-default serif. Setting it on html/body
/// only changes the inheritable default — elements with their own font-family
/// (designed newsletters etc.) are untouched.
const BASE_STYLE: &str = "<style>html,body{font-family:ui-sans-serif,system-ui,-apple-system,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;}</style>";

const FOLD_STYLE: &str = r#"<style>
details.quotes{font-family:ui-sans-serif,-apple-system,'Segoe UI',Roboto,sans-serif;font-size:13px;}
details.quotes>summary{color:#8a8a8a;cursor:pointer;padding:0.75rem 0 0.35rem;outline:none;list-style:none;}
details.quotes>summary::-webkit-details-marker{display:none;}
details.quotes[open]>summary{padding-top:0.9rem;color:#666;}
</style>"#;

/// Fold the quoted history of a reply into a collapsed native <details>.
/// Gmail nests blockquotes (each reply wraps the previous whole thread), so we
/// cut at the OUTERMOST quote marker. Both `<blockquote` and `gmail_quote` are
/// needed as markers: forwarded/mobile copies of Gmail mail keep the blockquote
/// but lose the class. To avoid misfolding newsletters that use blockquotes as
/// design elements, a history header ("wrote:" in any locale) must sit right at
/// the marker boundary, not merely somewhere later in the body.
fn fold_quotes(html: &str) -> String {
    let lower = html.to_lowercase();
    if lower.len() < 400 {
        return html.to_string();
    }
    let heads = ["wrote:", "escribi", "schreef", "ecrit", "schrieb", "scrive"];
    let mut cut: Option<usize> = None;
    for pat in ["<blockquote", "class=\"gmail_quote\""] {
        let mut from = 0;
        while let Some(rel) = lower[from..].find(pat) {
            let raw_i = from + rel;
            from = raw_i + 1;
            if !html.is_char_boundary(raw_i) {
                continue;
            }
            // Anchor to the enclosing tag start — but only when the match is
            // *inside* a tag (class="..."). If the pattern already begins at
            // '<', walking back overshoots past a closing tag and the fold
            // gets closed early by that tag, leaving an empty <details>.
            let i = if html.as_bytes()[raw_i] == b'<' {
                raw_i
            } else {
                match html[..raw_i].rfind('<') {
                    Some(t) => t,
                    None => continue,
                }
            };
            if i <= 100 || i + 400 >= lower.len() {
                continue;
            }
            let win = &lower[i.saturating_sub(600)..(i + 600).min(lower.len())];
            if heads.iter().any(|h| win.contains(h)) {
                cut = Some(cut.map_or(i, |c| c.min(i)));
                break;
            }
        }
    }
    match cut {
        Some(i) => {
            // Never cut mid-tag; the fold must start at an element boundary…
            if !html[..i].trim_end().ends_with('>') {
                return html.to_string();
            }
            // …and must not swallow an unclosed table (breaks rendering).
            let tail_l = &lower[i..];
            if tail_l.matches("<table").count() > tail_l.matches("</table").count() {
                return html.to_string();
            }
            format!(
                "{}\n<details class=\"quotes\"><summary>Show quoted text</summary>\n{}\n</details>",
                &html[..i],
                &html[i..]
            )
        }
        None => html.to_string(),
    }
}

fn strip_scripts(html: &str) -> String {
    match Regex::new(r"(?is)<script[^>]*>.*?(</script>|$)|</?script[^>]*>") {
        Ok(re) => re.replace_all(html, "").into_owned(),
        Err(_) => html.to_string(),
    }
}

fn build_threads(entries: &[MailEntry]) -> (Vec<Thread>, Vec<usize>, HashMap<String, usize>) {
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, _) in entries.iter().enumerate() {
        groups.entry(tid_of(entries, i)).or_default().push(i);
    }
    let mut threads: Vec<Thread> = Vec::with_capacity(groups.len());
    for (tid, mut msgs) in groups {
        msgs.sort_by_key(|&i| entries[i].date);
        let latest = *msgs.last().unwrap();
        let first = msgs[0];
        let att = msgs.iter().any(|&i| entries[i].has_attachments);
        let subject = msgs
            .iter()
            .map(|&i| entries[i].subject.clone())
            .find(|s| !s.trim().is_empty())
            .map(|s| clean_subject(&s))
            .unwrap_or_default();
        let e0 = &entries[first];
        threads.push(Thread {
            tid,
            msgs,
            subject,
            from: format!("{} <{}>", e0.from.display_name, e0.from.address),
            date: entries[latest].date,
            attachments: att,
        });
    }
    let mut order: Vec<usize> = (0..threads.len()).collect();
    order.sort_by(|&a, &b| threads[b].date.cmp(&threads[a].date));
    let tmap = threads.iter().enumerate().map(|(i, t)| (t.tid.clone(), i)).collect();
    (threads, order, tmap)
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

fn csv_labels(s: &Option<String>) -> Vec<String> {
    s.as_deref()
        .map(|v| {
            v.split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Default)]
struct Sel {
    cat: Option<String>,
    status: Option<String>,
    other: Vec<String>,
}

fn parse_sel(p: &FilterParams) -> Sel {
    Sel {
        cat: p.cat.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string),
        status: p.status.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string),
        other: csv_labels(&p.other),
    }
}

fn has_lbl(e: &MailEntry, l: &str) -> bool {
    e.labels.iter().any(|x| x.eq_ignore_ascii_case(l))
}

const STATUS_LABELS: [&str; 10] = ["Inbox", "Important", "Unread", "Opened", "Archived", "Starred", "Sent", "Drafts", "Spam", "Trash"];
/// Real Gmail inbox tabs. "Category Bills/Purchases/Travel/…" are auto-detected
/// labels, not tabs: they live in the `other` group.
const CATEGORY_TABS: [&str; 6] = [
    "Category Personal",
    "Category Social",
    "Category Updates",
    "Category Forums",
    "Category Reservations",
    "Category Promotions",
];

/// 0=top, 1=category, 2=status, 3=other. Frontend mirrors these constants.
fn group_of(l: &str) -> u8 {
    let l = l.to_lowercase();
    if CATEGORY_TABS.iter().any(|c| c.to_lowercase() == l) {
        0
    } else if STATUS_LABELS.iter().any(|s| s.to_lowercase() == l) {
        1
    } else {
        2
    }
}

/// Whether the entry satisfies the selection, optionally skipping one group
/// (for per-group facet counts). 0=category, 1=status, 2=other; `other` values OR.
fn sel_ok(e: &MailEntry, s: &Sel, skip: u8) -> bool {
    if skip != 0 {
        if let Some(l) = &s.cat {
            if !has_lbl(e, l) {
                return false;
            }
        }
    }
    if skip != 1 {
        if let Some(l) = &s.status {
            if !has_lbl(e, l) {
                return false;
            }
        }
    }
    if skip != 2 && !s.other.is_empty() && !s.other.iter().any(|l| has_lbl(e, l)) {
        return false;
    }
    true
}

fn date_att_ok(e: &MailEntry, after: &Option<DateTime<Utc>>, before: &Option<DateTime<Utc>>, att: bool) -> bool {
    if let Some(a) = after {
        if e.date < *a {
            return false;
        }
    }
    if let Some(b) = before {
        if e.date >= *b {
            return false;
        }
    }
    if att && !e.has_attachments {
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

    let s = parse_sel(p);
    let qterm = |l: &String| format!("label:\"{}\"", l.replace('"', ""));
    if let Some(l) = &s.cat {
        parts.push(qterm(l));
    }
    if let Some(l) = &s.status {
        parts.push(qterm(l));
    }
    // Values in `other` are OR; groups combine by AND (juxtaposition).
    if !s.other.is_empty() {
        parts.push(s.other.iter().map(qterm).collect::<Vec<_>>().join(" OR "));
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
        total: entries.len() as u64,
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

/// A tenant with the boot-time epoch seeded from its on-disk profile, so a
/// restart doesn't invalidate sessions signed under the current secret.
/// Shared by boot and integration tests.
fn seed_tenant(
    profiles_dir: &std::path::Path,
    slug: &str,
    name: &str,
    data_root: &std::path::Path,
    archive_root: &std::path::Path,
    backup_root: &std::path::Path,
) -> Tenant {
    let epoch = auth::read_profile(profiles_dir, slug)
        .map(|sp| auth::secret_fingerprint(&sp.secret))
        .unwrap_or_default();
    Tenant {
        slug: slug.to_string(),
        name: name.to_string(),
        epoch: Arc::new(RwLock::new(epoch)),
        inner: Arc::new(RwLock::new(AppStateInner::default())),
        data_dir: data_root.join(slug),
        archive_dir: archive_root.join(slug),
        backup_dir: backup_root.join(slug),
        busy: Arc::new(AtomicBool::new(false)),
        backup: Arc::new(Mutex::new(BackupProgress::default())),
    }
}

/// The full HTTP surface: public auth routes plus the cookie-protected
/// archive API. Split out of main() so tests can drive it in-process.
fn build_app(root: Root) -> Router {
    let protected = Router::new()
        .route("/api/whoami", get(whoami))
        .route("/api/profile", get(profile_info))
        .route("/api/profile/password", post(change_password))
        .route("/api/profile/name", post(change_name))
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
        .layer(CorsLayer::permissive())
        .with_state(root)
}

async fn merge_handler(Extension(state): Extension<Tenant>) -> Json<ScanResponse> {
    let data_dir = state.data_dir.clone();
    let archive_dir = state.archive_dir.clone();
    state.busy.store(true, Ordering::SeqCst);

    tokio::task::spawn_blocking(move || {
        let _busy = BusyGuard(state.busy.clone());
        let mut mbox_files: Vec<(PathBuf, SystemTime)> = Vec::new();
        for entry in WalkDir::new(&data_dir).into_iter().filter_map(|e| e.ok()) {
            if entry.path().extension().map_or(false, |ext| ext == "mbox") {
                let mt = entry
                    .metadata()
                    .map(|m| m.modified().unwrap_or(SystemTime::UNIX_EPOCH))
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                mbox_files.push((entry.path().to_path_buf(), mt));
            }
        }
        // The existing archive (kept in its own folder) is always a source.
        let archived = archive_dir.join("merged.mbox");
        if archived.exists() {
            let mt = archived
                .metadata()
                .map(|m| m.modified().unwrap_or(SystemTime::UNIX_EPOCH))
                .unwrap_or(SystemTime::UNIX_EPOCH);
            mbox_files.push((archived, mt));
        }
        // merged.mbox itself is a valid source: the archive accumulates
        // in place, so raw exports can be deleted once merged. The output
        // is written via .tmp + rename, so the input is never clobbered.
        if mbox_files.is_empty() {
            return;
        }

        // Oldest-first output; the newest export owns each Message-ID. This
        // dedups across Takeout generations, where byte-identical hashing
        // fails because exported headers (X-Gmail-Labels) drift over time.
        mbox_files.sort_by_key(|(_, mt)| *mt);
        let mut indexes: Vec<Vec<MailEntry>> = Vec::new();
        for (p, _) in &mbox_files {
            match builder::build_index(p, false, None) {
                Ok(e) => indexes.push(e),
                Err(err) => {
                    error!("Merge aborted, indexing {:?}: {}", p, err);
                    return;
                }
            }
        }
        let mut owner: HashMap<String, usize> = HashMap::new();
        for (fidx, entries) in indexes.iter().enumerate().rev() {
            for e in entries {
                if !e.message_id.is_empty() {
                    owner.entry(e.message_id.clone()).or_insert(fidx);
                }
            }
        }

        let output = archive_dir.join("merged.mbox");
        if fs::create_dir_all(&archive_dir).is_err() {
            error!("Merge aborted: cannot create archive dir {:?}", archive_dir);
            return;
        }
        let tmp = archive_dir.join("merged.mbox.tmp");
        info!("Merging {} files into {:?}", mbox_files.len(), output);
        let mut out = match fs::File::create(&tmp) {
            Ok(f) => BufWriter::with_capacity(1 << 20, f),
            Err(e) => {
                error!("Merge aborted, creating {:?}: {}", tmp, e);
                return;
            }
        };
        let mut kept = 0u64;
        let mut dups = 0u64;
        let mut prev_ended_nl = true;
        for (fidx, (path, _)) in mbox_files.iter().enumerate() {
            let mut store = match MboxStore::open(path) {
                Ok(s) => s,
                Err(e) => {
                    error!("Merge aborted, opening {:?}: {}", path, e);
                    drop(out);
                    let _ = fs::remove_file(&tmp);
                    return;
                }
            };
            let entries = &indexes[fidx];
            for (i, entry) in entries.iter().enumerate() {
                if i > 0 && i % 50_000 == 0 {
                    info!("Merging {:?}: {}/{}", path.file_name().unwrap_or_default(), i, entries.len());
                }
                if !entry.message_id.is_empty()
                    && owner.get(&entry.message_id).map_or(false, |o| *o != fidx)
                {
                    dups += 1;
                    continue;
                }
                let raw = match store.get_raw_message(entry) {
                    Ok(r) => r,
                    Err(e) => {
                        error!("Merge aborted, reading message: {}", e);
                        drop(out);
                        let _ = fs::remove_file(&tmp);
                        return;
                    }
                };
                if kept > 0 && !prev_ended_nl {
                    // Ensure the next `From ` line starts fresh after a blank line.
                    let _ = out.write_all(b"\n");
                }
                let _ = out.write_all(&raw);
                prev_ended_nl = raw.last() == Some(&b'\n');
                kept += 1;
            }
        }
        let finish = |mut out: BufWriter<fs::File>| -> std::io::Result<()> {
            out.flush()?;
            let mut file = out
                .into_inner()
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
            file.flush()?;
            fs::rename(&tmp, &output)
        };
        match finish(out) {
            Ok(()) => info!("Merge complete: kept {} messages, skipped {} older duplicates", kept, dups),
            Err(e) => {
                error!("Merge aborted on write: {}", e);
                let _ = fs::remove_file(&tmp);
            }
        }
    });

    Json(ScanResponse {
        message: "Merge process started".to_string(),
        status: "processing".to_string(),
    })
}

async fn scan_handler(Extension(state): Extension<Tenant>) -> Json<ScanResponse> {
    let inner = state.inner.clone();
    let data_dir = state.data_dir.clone();
    let archive_dir = state.archive_dir.clone();
    state.busy.store(true, Ordering::SeqCst);

    tokio::task::spawn_blocking(move || {
        let _busy = BusyGuard(state.busy.clone());
        // The archive lives apart from the upload folder so it can't be
        // cleaned up by accident: data/ is for drops, archive/ is the keeper.
        let mut merged_path = archive_dir.join("merged.mbox");
        if !merged_path.exists() {
            let old_layout = data_dir.join("merged.mbox");
            if old_layout.exists() {
                merged_path = old_layout;
            }
        }
        let target_path = if merged_path.exists() {
            merged_path
        } else {
            // No merged archive: index the newest mbox available and flag it
            // loudly if several exist, since only one gets indexed.
            let mut candidates: Vec<(PathBuf, SystemTime)> = Vec::new();
            for entry in WalkDir::new(&data_dir).into_iter().filter_map(|e| e.ok()) {
                 if entry.path().extension().map_or(false, |ext| ext == "mbox") {
                     let mt = entry
                         .metadata()
                         .map(|m| m.modified().unwrap_or(SystemTime::UNIX_EPOCH))
                         .unwrap_or(SystemTime::UNIX_EPOCH);
                     candidates.push((entry.path().to_path_buf(), mt));
                 }
            }
            if candidates.is_empty() {
                info!("nothing to index for '{}' yet — drop .mbox files into {:?}", state.slug, data_dir);
                return;
            }
            if candidates.len() > 1 {
                warn!(
                    "{} mbox files present but no merged.mbox — indexing only the newest; run Merge to combine them",
                    candidates.len()
                );
            }
            candidates.sort_by_key(|b| std::cmp::Reverse(b.1));
            candidates.remove(0).0
        };

        info!("Indexing: {:?}", target_path);
        match builder::build_index(&target_path, false, None) {
            Ok(entries) => {
                let facets = compute_facets(&entries);
                let (threads, thread_order, tmap) = build_threads(&entries);
                let mut order: Vec<usize> = (0..entries.len()).collect();
                order.sort_by(|&a, &b| entries[b].date.cmp(&entries[a].date));
                info!("Threading: {} conversations", threads.len());
                let mut lock = inner.write().unwrap();
                lock.mbox_path = Some(target_path);
                lock.facets = facets;
                lock.order = order;
                lock.threads = threads;
                lock.thread_order = thread_order;
                lock.tmap = tmap;
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

async fn filters_handler(
    Extension(state): Extension<Tenant>,
    Query(params): Query<FilterParams>,
) -> Json<Facets> {
    let inner = state.inner.clone();
    let fallback = state.inner.read().unwrap().facets.clone();
    let facets = tokio::task::spawn_blocking(move || {
        let lock = inner.read().unwrap();
        let base = &lock.facets;
        let s = parse_sel(&params);
        let after = parse_bound(&params.after);
        let before = parse_bound(&params.before);
        let att = params.attachment.as_deref() == Some("1");
        let any = s.cat.is_some()
            || s.status.is_some()
            || !s.other.is_empty()
            || after.is_some()
            || before.is_some()
            || att;
        if !any {
            return base.clone();
        }
        // Per-group facet counts: each group's labels are counted under all
        // *other* active filters, so a selection never zeroes its own group.
        let mut counts: HashMap<String, u64> = HashMap::new();
        for e in &lock.entries {
            if !date_att_ok(e, &after, &before, att) {
                continue;
            }
            let a = sel_ok(e, &s, 0);
            let b = sel_ok(e, &s, 1);
            let c = sel_ok(e, &s, 2);
            for l in &e.labels {
                // A label is counted under every filter except its own group's.
                let inc = match group_of(l) {
                    0 => a,
                    1 => b,
                    _ => c,
                };
                if inc {
                    *counts.entry(l.clone()).or_insert(0) += 1;
                }
            }
        }
        let mut labels: Vec<(String, u64)> = counts.into_iter().collect();
        labels.sort_unstable_by(|x, y| y.1.cmp(&x.1).then_with(|| x.0.cmp(&y.0)));
        labels.truncate(200);
        Facets {
            labels,
            min_ts: base.min_ts,
            max_ts: base.max_ts,
            total: base.total,
        }
    })
    .await
    .unwrap_or(fallback);

    Json(facets)
}

async fn list_messages(
    Extension(state): Extension<Tenant>,
    Query(params): Query<FilterParams>,
) -> Json<Vec<ThreadOut>> {
    let inner = state.inner.clone();
    let items = tokio::task::spawn_blocking(move || {
        let lock = inner.read().unwrap();
        let offset = params.offset.unwrap_or(0);
        let limit = params.limit.unwrap_or(100).clamp(1, 500);
        let s = parse_sel(&params);
        let after = parse_bound(&params.after);
        let before = parse_bound(&params.before);
        let want_att = params.attachment.as_deref() == Some("1");
        let filtered = s.cat.is_some()
            || s.status.is_some()
            || !s.other.is_empty()
            || after.is_some()
            || before.is_some()
            || want_att;

        if !filtered {
            return match lock.thread_order.get(offset..) {
                Some(slice) => slice
                    .iter()
                    .take(limit)
                    .map(|&ti| thread_out(&lock.threads[ti], 0))
                    .collect(),
                None => Vec::new(),
            };
        }

        let mut out = Vec::new();
        let mut skipped = 0usize;
        for &ti in &lock.thread_order {
            let t = &lock.threads[ti];
            let pass = (!want_att || t.attachments)
                && t.msgs
                    .iter()
                    .any(|&i| sel_ok(&lock.entries[i], &s, 3) && date_att_ok(&lock.entries[i], &after, &before, false));
            if !pass {
                continue;
            }
            if skipped < offset {
                skipped += 1;
                continue;
            }
            if out.len() >= limit {
                break;
            }
            out.push(thread_out(t, 0));
        }
        out
    })
    .await
    .unwrap_or_else(|_| Vec::new());

    Json(items)
}

#[derive(Serialize)]
struct ThreadDetail {
    tid: String,
    subject: String,
    messages: Vec<EmailMetadata>,
}

#[derive(Deserialize)]
struct TidParam {
    tid: String,
}

async fn thread_detail(Extension(state): Extension<Tenant>, Query(q): Query<TidParam>) -> impl IntoResponse {
    let lock = state.inner.read().unwrap();
    match lock.tmap.get(&q.tid) {
        Some(&ti) => {
            let t = &lock.threads[ti];
            Json(ThreadDetail {
                tid: q.tid.clone(),
                subject: t.subject.clone(),
                messages: t.msgs.iter().map(|&i| metadata_for(&lock, i)).collect(),
            })
            .into_response()
        }
        None => (StatusCode::NOT_FOUND, "Thread not found").into_response(),
    }
}

async fn search_handler(
    Extension(state): Extension<Tenant>,
    Query(params): Query<FilterParams>,
) -> Json<Vec<ThreadOut>> {
    let inner = state.inner.clone();
    let offset = params.offset.unwrap_or(0);
    let limit = params.limit.unwrap_or(50).clamp(1, 500);
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
            Ok((_, results)) => {
                // Fold matching messages into their threads; a thread shows if
                // any message matches, with `matched` counting the hits.
                let mut counts: HashMap<String, usize> = HashMap::new();
                let mut tids: Vec<usize> = Vec::new();
                for &idx in &results {
                    let tid = tid_of(&lock.entries, idx);
                    let c = counts.entry(tid.clone()).or_insert(0);
                    *c += 1;
                    if *c == 1 {
                        if let Some(&ti) = lock.tmap.get(&tid) {
                            tids.push(ti);
                        }
                    }
                }
                tids.sort_by(|&a, &b| lock.threads[b].date.cmp(&lock.threads[a].date));
                tids.iter()
                    .skip(offset)
                    .take(limit)
                    .map(|&ti| {
                        let t = &lock.threads[ti];
                        thread_out(t, counts.get(&t.tid).copied().unwrap_or(0))
                    })
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
    Extension(state): Extension<Tenant>,
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
                // Untrusted email HTML: strip scripts (the frame adds back
                // only its own height-reporter), fold quoted history safely.
                let html = strip_scripts(&fold_quotes(&html));
                // Folded quotes live inside the email document; ship a style
                // so the summary matches the app chrome instead of serif.
                let html = if html.contains("<details class=\"quotes\"") {
                    format!("{FOLD_STYLE}{html}")
                } else {
                    html
                };
                let html = format!("{BASE_STYLE}{html}");
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

async fn email_meta(Extension(state): Extension<Tenant>, Path(id): Path<String>) -> impl IntoResponse {
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
    Extension(state): Extension<Tenant>,
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

#[derive(Serialize)]
struct StatusOut {
    busy: bool,
    source: Option<String>,
    total: u64,
    oldest_ts: i64,
    latest_ts: i64,
    backup: BackupProgress,
}

async fn status_handler(Extension(state): Extension<Tenant>) -> Json<StatusOut> {
    let lock = state.inner.read().unwrap();
    Json(StatusOut {
        busy: state.busy.load(Ordering::SeqCst),
        source: lock
            .mbox_path
            .as_ref()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string())),
        total: lock.facets.total,
        oldest_ts: lock.facets.min_ts,
        latest_ts: lock.facets.max_ts,
        backup: state.backup.lock().unwrap().clone(),
    })
}

#[derive(Serialize)]
struct BackupOut {
    ok: bool,
    message: String,
    target: Option<String>,
    bytes: Option<u64>,
    secs: Option<u64>,
}

/// Safe copy of the current archive, gzip-compressed: written to a temp file
/// with progress reporting, verified by full decompression, then atomically
/// renamed — a partial or corrupt backup never appears as a completed one.
async fn backup_handler(Extension(state): Extension<Tenant>) -> Json<BackupOut> {
    let fail = |m: String| {
        Json(BackupOut {
            ok: false,
            message: m,
            target: None,
            bytes: None,
            secs: None,
        })
    };

    if state.busy.load(Ordering::SeqCst) {
        return fail("A merge or scan is running — wait for it to finish, then back up.".to_string());
    }
    if state.backup.lock().unwrap().running {
        return fail("A backup is already running.".to_string());
    }
    let src = state.inner.read().unwrap().mbox_path.clone();
    let Some(src) = src else {
        return fail("No archive is loaded — open the inbox (or press Re-scan) first.".to_string());
    };
    let backup_dir = state.backup_dir.clone();
    let progress = state.backup.clone();

    let result = tokio::task::spawn_blocking(move || -> Result<(String, u64, u64), String> {
        let _guard = BackupGuard(progress.clone());
        let set = |phase: &str, done: u64, total: u64| {
            let mut p = progress.lock().unwrap();
            p.running = true;
            p.phase = phase.to_string();
            p.done = done;
            p.total = total;
        };

        fs::create_dir_all(&backup_dir).map_err(|e| format!("cannot create backup dir {:?}: {}", backup_dir, e))?;
        let name = format!("mbox-archive-{}.mbox.gz", Utc::now().format("%Y%m%d-%H%M%S"));
        let target = backup_dir.join(&name);
        let tmp = backup_dir.join(format!("{}.tmp", name));
        let size = fs::metadata(&src).map_err(|e| format!("source unreadable: {}", e))?.len();
        info!("Safe copy (gzip): {:?} ({} bytes) -> {:?}", src, size, target);
        let start = std::time::Instant::now();

        // Compression pass. The encoder is closed by scoping (Drop flushes);
        // any residual error is caught by the verification pass below.
        let mut rd = BufReader::with_capacity(1 << 20, fs::File::open(&src).map_err(|e| e.to_string())?);
        let mut buf = vec![0u8; 4 << 20];
        {
            let mut enc = GzEncoder::new(
                BufWriter::with_capacity(1 << 20, fs::File::create(&tmp).map_err(|e| e.to_string())?),
                Compression::new(6),
            );
            let mut done = 0u64;
            loop {
                let n = rd.read(&mut buf).map_err(|e| format!("read failed: {}", e))?;
                if n == 0 {
                    break;
                }
                enc.write_all(&buf[..n]).map_err(|e| format!("write failed: {}", e))?;
                done += n as u64;
                set("compressing", done, size);
            }
            enc.try_finish().map_err(|e| format!("gzip finish failed: {}", e))?;
        }
        let to_sync = fs::File::open(&tmp).map_err(|e| e.to_string())?;
        to_sync.sync_all().map_err(|e| e.to_string())?;

        // Verification pass: the backup is only real if it decompresses
        // back to the exact byte count of the source.
        set("verifying", 0, size);
        let mut dec = GzDecoder::new(BufReader::with_capacity(1 << 20, fs::File::open(&tmp).map_err(|e| e.to_string())?));
        let mut count = 0u64;
        loop {
            let n = match dec.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            count += n as u64;
            set("verifying", count, size);
        }
        if count != size {
            let _ = fs::remove_file(&tmp);
            return Err(format!(
                "verification failed: backup decompresses to {} of {} bytes — discarded",
                count, size
            ));
        }

        fs::rename(&tmp, &target).map_err(|e| format!("finalizing failed: {}", e))?;
        let gz = fs::metadata(&target).map_err(|e| e.to_string())?.len();
        Ok((
            target.to_string_lossy().to_string(),
            gz,
            start.elapsed().as_secs(),
        ))
    })
    .await;

    match result {
        Ok(Ok((target, bytes, secs))) => Json(BackupOut {
            ok: true,
            message: "Backup complete (gzip, verified).".to_string(),
            target: Some(target),
            bytes: Some(bytes),
            secs: Some(secs),
        }),
        Ok(Err(msg)) => fail(msg),
        Err(_) => fail("Backup task was aborted.".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Multi-tenancy: authentication endpoints and the per-request tenant layer.
// ---------------------------------------------------------------------------

/// Rejects requests without a valid session cookie; otherwise injects the
/// matching Tenant for handlers to extract.
async fn require_tenant(
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

#[derive(Serialize)]
struct ProfileOut {
    slug: String,
    name: String,
    has_portrait: bool,
}

/// Chooser data — intentionally the only roster view available pre-login.
/// Read fresh off disk so renames show up without a restart.
async fn list_profiles(State(root): State<Root>) -> Json<Vec<ProfileOut>> {
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

#[derive(Deserialize)]
struct LoginIn {
    slug: String,
    password: String,
}

async fn login_handler(
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
        match auth::write_profile(&root.profiles_dir, &body.slug, &stored.name, &upgraded) {
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

async fn logout_handler() -> Response {
    (
        [(header::SET_COOKIE, auth::expired_cookie(auth::COOKIE_NAME))],
        Json(serde_json::json!({ "ok": true })),
    )
        .into_response()
}

/// Display name, read fresh from disk so a rename applies without a restart.
/// Falls back to the boot-time name if the profile can't be read.
fn display_name(root: &Root, t: &Tenant) -> String {
    auth::read_profile(&root.profiles_dir, &t.slug)
        .map(|sp| sp.name)
        .unwrap_or_else(|| t.name.clone())
}

async fn whoami(State(root): State<Root>, Extension(t): Extension<Tenant>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "slug": t.slug, "name": display_name(&root, &t) }))
}

static SLUG_OK: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
fn slug_re() -> &'static Regex {
    SLUG_OK.get_or_init(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$").unwrap())
}

/// Serves a tenant's portrait pre-login (the chooser needs it). Slug is
/// strictly validated; only fixed portrait filenames are ever looked up.
async fn portrait_handler(
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

#[derive(Deserialize)]
struct PasswordIn {
    current: String,
    new: String,
}

async fn change_password(
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
    auth::write_profile(&root.profiles_dir, &t.slug, &stored.name, &new_hash)
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

#[derive(Deserialize)]
struct NameIn {
    name: String,
}

/// Display-name edit. The stored secret is rewritten verbatim, so neither the
/// session epoch nor anyone's cookie is touched — a rename logs nobody out.
async fn change_name(
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
    auth::write_profile(&root.profiles_dir, &t.slug, name, &stored.secret).map_err(|e| {
        bad(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("could not save: {}", e),
        )
    })?;
    info!("display name set to '{}' for '{}'", name, t.slug);
    Ok(Json(serde_json::json!({ "ok": true, "name": name })))
}

#[derive(Deserialize)]
struct PortraitIn {
    /// data URL or bare base64 of the image bytes
    data: String,
}

async fn upload_portrait(
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

async fn delete_portrait(
    State(root): State<Root>,
    Extension(t): Extension<Tenant>,
) -> Json<serde_json::Value> {
    auth::delete_portrait(&root.profiles_dir, &t.slug);
    Json(serde_json::json!({ "ok": true }))
}

async fn profile_info(
    State(root): State<Root>,
    Extension(t): Extension<Tenant>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "slug": t.slug,
        "name": display_name(&root, &t),
        "has_portrait": auth::find_portrait(&root.profiles_dir, &t.slug).is_some(),
    }))
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use serde_json::json;
    use tower::ServiceExt;

    struct Env {
        base: PathBuf,
        profiles: PathBuf,
        data: PathBuf,
        archive: PathBuf,
        backup: PathBuf,
        key: Arc<Vec<u8>>,
    }

    impl Env {
        fn new(tag: &str) -> Env {
            let base = std::env::temp_dir().join(format!(
                "mbox_it_{}_{}_{}",
                tag,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .subsec_nanos()
            ));
            let env = Env {
                profiles: base.join("profiles"),
                data: base.join("data"),
                archive: base.join("archive"),
                backup: base.join("backup"),
                base,
                key: Arc::new((0..32u8).collect()),
            };
            fs::create_dir_all(&env.profiles).unwrap();
            env
        }
        fn add(&self, slug: &str, name: &str, password: &str) {
            fs::create_dir_all(self.profiles.join(slug)).unwrap();
            auth::write_profile(&self.profiles, slug, name, &auth::sha256_hex(password)).unwrap();
        }
        /// Mirrors the boot path exactly: roster off disk, epoch seeded from
        /// the stored secret. Two calls = process restart against the same disk.
        fn root(&self) -> Root {
            let mut tenants = HashMap::new();
            for p in auth::load_profiles(&self.profiles) {
                tenants.insert(
                    p.slug.clone(),
                    seed_tenant(&self.profiles, &p.slug, &p.name, &self.data, &self.archive, &self.backup),
                );
            }
            Root {
                tenants: Arc::new(tenants),
                profiles_dir: self.profiles.clone(),
                key: self.key.clone(),
            }
        }
        fn app(&self) -> Router {
            build_app(self.root())
        }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn req(method: &str, uri: &str, body: Body, cookie: Option<&str>, json: bool) -> Request<Body> {
        let mut rb = Request::builder().method(method).uri(uri);
        if json {
            rb = rb.header(header::CONTENT_TYPE, "application/json");
        }
        if let Some(c) = cookie {
            rb = rb.header(header::COOKIE, c);
        }
        rb.body(body).unwrap()
    }

    async fn send(app: &Router, r: Request<Body>) -> Response {
        app.clone().oneshot(r).await.unwrap()
    }

    async fn login(app: &Router, slug: &str, password: &str) -> Response {
        send(
            app,
            req(
                "POST",
                "/api/login",
                Body::from(json!({ "slug": slug, "password": password }).to_string()),
                None,
                true,
            ),
        )
        .await
    }

    fn cookie_of(r: &Response) -> String {
        r.headers()
            .get(header::SET_COOKIE)
            .expect("response carries no Set-Cookie")
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    }

    async fn whoami_status(app: &Router, cookie: &str) -> StatusCode {
        send(app, req("GET", "/api/whoami", Body::empty(), Some(cookie), false))
            .await
            .status()
    }

    async fn json_of(r: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn anonymous_and_bad_credentials_are_rejected() {
        let env = Env::new("reject");
        env.add("daniel", "Daniel", "daniel");
        let app = env.app();
        assert_eq!(whoami_status(&app, "mbx_s=nope").await, StatusCode::UNAUTHORIZED);
        assert_eq!(login(&app, "daniel", "hunter2").await.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(login(&app, "ghost", "daniel").await.status(), StatusCode::UNAUTHORIZED);
        let body = json_of(login(&app, "daniel", "hunter2").await).await;
        assert_eq!(body["error"], "wrong password");
    }

    #[tokio::test]
    async fn login_upgrades_legacy_secret_and_both_sessions_work() {
        let env = Env::new("upgrade");
        env.add("daniel", "Daniel", "daniel");
        let app = env.app();
        let r = login(&app, "daniel", "daniel").await;
        assert_eq!(r.status(), StatusCode::OK);
        let body = json_of(send(&app, req("GET", "/api/whoami", Body::empty(), Some(&cookie_of(&r)), false)).await).await;
        assert_eq!(body["slug"], "daniel");
        assert!(
            auth::read_profile(&env.profiles, "daniel").unwrap().secret.starts_with("$2"),
            "legacy secret should have been upgraded to bcrypt"
        );
        // and the bcrypt path must issue working sessions too
        let r2 = login(&app, "daniel", "daniel").await;
        assert_eq!(r2.status(), StatusCode::OK);
        assert_eq!(whoami_status(&app, &cookie_of(&r2)).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn sessions_survive_a_restart() {
        // Regression guard for boot-time epoch seeding: a fresh Root built
        // from the same disk must keep accepting cookies signed under the
        // current secret.
        let env = Env::new("restart");
        env.add("daniel", "Daniel", "daniel");
        let cookie = cookie_of(&login(&env.app(), "daniel", "daniel").await);
        let rebooted = env.app();
        assert_eq!(whoami_status(&rebooted, &cookie).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn password_change_revokes_every_older_session() {
        let env = Env::new("revoke");
        env.add("daniel", "Daniel", "daniel");
        let app = env.app();
        let a = cookie_of(&login(&app, "daniel", "daniel").await);
        let b = cookie_of(&login(&app, "daniel", "daniel").await);
        let changed = send(
            &app,
            req(
                "POST",
                "/api/profile/password",
                Body::from(json!({ "current": "daniel", "new": "secretive" }).to_string()),
                Some(&a),
                true,
            ),
        )
        .await;
        assert_eq!(changed.status(), StatusCode::OK);
        let a2 = cookie_of(&changed);
        assert_eq!(whoami_status(&app, &a2).await, StatusCode::OK, "changed device keeps working");
        assert_eq!(whoami_status(&app, &a).await, StatusCode::UNAUTHORIZED, "old cookie revoked");
        assert_eq!(whoami_status(&app, &b).await, StatusCode::UNAUTHORIZED, "other device revoked");
        assert_eq!(login(&app, "daniel", "daniel").await.status(), StatusCode::UNAUTHORIZED, "old password dead");
        let fresh = login(&app, "daniel", "secretive").await;
        assert_eq!(fresh.status(), StatusCode::OK, "new password logs in");
        assert_eq!(whoami_status(&app, &cookie_of(&fresh)).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn too_short_new_password_is_400_and_changes_nothing() {
        let env = Env::new("shortpw");
        env.add("daniel", "Daniel", "daniel");
        let app = env.app();
        let a = cookie_of(&login(&app, "daniel", "daniel").await);
        let before = auth::read_profile(&env.profiles, "daniel").unwrap().secret;
        let r = send(
            &app,
            req(
                "POST",
                "/api/profile/password",
                Body::from(json!({ "current": "daniel", "new": "abc" }).to_string()),
                Some(&a),
                true,
            ),
        )
        .await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        assert_eq!(auth::read_profile(&env.profiles, "daniel").unwrap().secret, before);
        assert_eq!(whoami_status(&app, &a).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn tampered_cookie_is_rejected() {
        let env = Env::new("tamper");
        env.add("daniel", "Daniel", "daniel");
        let app = env.app();
        let cookie = cookie_of(&login(&app, "daniel", "daniel").await);
        let (_, val) = cookie.split_once('=').unwrap();
        let (ph, sig) = val.split_once('.').unwrap();
        let mut chars: Vec<char> = ph.chars().collect();
        chars[0] = if chars[0] == '0' { '1' } else { '0' };
        let bad = format!("mbx_s={}.{}", chars.iter().collect::<String>(), sig);
        assert_eq!(whoami_status(&app, &bad).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn logout_expires_the_session_cookie() {
        let env = Env::new("logout");
        env.add("daniel", "Daniel", "daniel");
        let app = env.app();
        let r = send(&app, req("POST", "/api/logout", Body::empty(), None, false)).await;
        assert_eq!(r.status(), StatusCode::OK);
        let sc = r.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap();
        assert!(sc.contains("Max-Age=0"));
    }

    async fn get_json(app: &Router, uri: &str, cookie: Option<&str>) -> (StatusCode, serde_json::Value) {
        let r = send(app, req("GET", uri, Body::empty(), cookie, false)).await;
        let status = r.status();
        (status, json_of(r).await)
    }

    #[tokio::test]
    async fn rename_updates_all_views_without_touching_sessions() {
        let env = Env::new("rename");
        env.add("daniel", "Daniel", "daniel");
        let app = env.app();
        let r = login(&app, "daniel", "daniel").await;
        let cookie = cookie_of(&r);
        let secret_before = auth::read_profile(&env.profiles, "daniel").unwrap().secret;

        let changed = send(
            &app,
            req(
                "POST",
                "/api/profile/name",
                Body::from(json!({ "name": "  Dan  " }).to_string()),
                Some(&cookie),
                true,
            ),
        )
        .await;
        assert_eq!(changed.status(), StatusCode::OK);

        let (s, body) = get_json(&app, "/api/whoami", Some(&cookie)).await;
        assert_eq!(s, StatusCode::OK, "renamed device keeps its session");
        assert_eq!(body["name"], "Dan", "whoami shows trimmed new name");
        let (_, prof) = get_json(&app, "/api/profile", Some(&cookie)).await;
        assert_eq!(prof["name"], "Dan");
        let (s, roster) = get_json(&app, "/api/profiles", None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(roster[0]["name"], "Dan", "chooser reads new name pre-login");

        assert_eq!(
            auth::read_profile(&env.profiles, "daniel").unwrap().secret,
            secret_before,
            "rename must not alter the secret"
        );
        assert_eq!(login(&app, "daniel", "daniel").await.status(), StatusCode::OK, "old password still works");
    }

    #[tokio::test]
    async fn rename_rejects_blank_and_overlong_names() {
        let env = Env::new("badname");
        env.add("daniel", "Daniel", "daniel");
        let app = env.app();
        let cookie = cookie_of(&login(&app, "daniel", "daniel").await);
        for name in ["", "   ", &"x".repeat(81)] {
            let r = send(
                &app,
                req(
                    "POST",
                    "/api/profile/name",
                    Body::from(json!({ "name": name }).to_string()),
                    Some(&cookie),
                    true,
                ),
            )
            .await;
            assert_eq!(r.status(), StatusCode::BAD_REQUEST, "accepted name {:?}", name);
        }
        assert_eq!(auth::read_profile(&env.profiles, "daniel").unwrap().name, "Daniel");
    }
}
