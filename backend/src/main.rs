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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
use walkdir::WalkDir;
use tracing::{debug, info, error, warn};

/// One person's archive: everything handlers used to share globally is now
/// scoped to a tenant. Handlers receive it via Extension after cookie auth.
#[derive(Clone)]
struct Tenant {
    slug: String,
    name: String,
    /// fingerprint of the current secret; sessions carry it, so a password
    /// change revokes all older cookies. Reread at boot, updated live on change.
    epoch: Arc<RwLock<String>>,
    /// declared identities (accounts & aliases); live so a profile edit
    /// applies without restart, like the display name
    addresses: Arc<RwLock<Vec<String>>>,
    inner: Arc<RwLock<AppStateInner>>,
    data_dir: PathBuf,
    archive_dir: PathBuf,
    backup_dir: PathBuf,
    busy: Arc<AtomicBool>,
    search: Arc<SearchProgress>,
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
    /// declared addresses involved in this thread; empty (omitted) unless the
    /// tenant has 2+ addresses configured
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    accts: Vec<String>,
}

#[derive(Serialize, Clone)]
struct Facets {
    labels: Vec<(String, u64)>,
    #[serde(default)]
    addresses: Vec<(String, u64)>,
    min_ts: i64,
    max_ts: i64,
    total: u64,
}

impl Default for Facets {
    fn default() -> Self {
        Facets { labels: Vec::new(), addresses: Vec::new(), min_ts: 0, max_ts: 0, total: 0 }
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
    acct: Option<String>,
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

/// Per-request slots for in-flight searches, so "Full" body scans over
/// hundreds of thousands of messages stop looking like a hung app — and so
/// two overlapping searches never show each other's numbers. Slots are keyed
/// by a request id and removed when the guard drops, whatever the exit path
/// (completion, error, panic): the counters cannot leak.
#[derive(Default)]
struct SearchProgress {
    next_id: AtomicU64,
    slots: Mutex<HashMap<u64, (usize, usize)>>,
}

struct SearchGuard {
    sp: Arc<SearchProgress>,
    id: u64,
}
impl SearchGuard {
    fn new(sp: Arc<SearchProgress>) -> Self {
        let id = sp.next_id.fetch_add(1, Ordering::SeqCst);
        sp.slots.lock().unwrap().insert(id, (0, 0));
        Self { sp, id }
    }
}
impl Drop for SearchGuard {
    fn drop(&mut self) {
        self.sp.slots.lock().unwrap().remove(&self.id);
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

/// Identity tagging: which *declared* addresses involve this message.
/// Tags are derived at read time from index fields that already exist
/// (from/to/cc) — nothing is stored, so editing the list relabels the
/// whole archive instantly and merges/rescans are unaffected.

/// `hajni+work` -> `hajni` (plus-tags are alias decorations, not new bases)
fn addr_base(local: &str) -> &str {
    match local.find('+') {
        Some(i) => &local[..i],
        None => local,
    }
}

/// Match a raw header address against the declared list.
/// Exact declaration wins over its plus-base (so a separately declared
/// `hajni+work@…` keeps its own tag); `+variants` collapse into the base.
fn addr_tag(raw: &str, declared: &[String]) -> Option<String> {
    let lower = raw.trim().to_lowercase();
    if lower.is_empty() {
        return None;
    }
    if let Some(d) = declared.iter().find(|d| d.as_str() == lower) {
        return Some(d.clone());
    }
    let (local, domain) = lower.split_once('@')?;
    let base = format!("{}@{}", addr_base(local), domain);
    declared.iter().find(|d| d.as_str() == base).cloned()
}

/// Declared addresses this message involves, in declaration order.
/// Off entirely below 2 addresses: one identity tells you nothing.
fn entry_tags(e: &MailEntry, declared: &[String]) -> Vec<String> {
    if declared.len() < 2 {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut consider = |addr: &str| {
        if let Some(t) = addr_tag(addr, declared) {
            if !out.contains(&t) {
                out.push(t);
            }
        }
    };
    consider(&e.from.address);
    for a in &e.to {
        consider(&a.address);
    }
    for a in &e.cc {
        consider(&a.address);
    }
    out
}

fn address_counts(entries: &[MailEntry], declared: &[String]) -> Vec<(String, u64)> {
    if declared.len() < 2 {
        return Vec::new();
    }
    let mut counts: HashMap<String, u64> = HashMap::new();
    for e in entries {
        for t in entry_tags(e, declared) {
            *counts.entry(t).or_insert(0) += 1;
        }
    }
    // declaration order, then by count — stable UI
    let mut out: Vec<(String, u64)> = declared
        .iter()
        .map(|d| (d.clone(), counts.get(d).copied().unwrap_or(0)))
        .filter(|(_, c)| *c > 0)
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out
}

/// Gmail treats `user+tag@domain` as `user@domain`. The archive index stores
/// literal addresses, so `to:hajni@gmail.com` as a raw term would miss the
/// digest that arrived at `hajni+news@gmail.com`. Strategy: peel full-email
/// `to:`/`from:` terms out of the index query, run the index on the rest,
/// then match the peeled terms here with fold-aware equality.
/// Only simple queries (no OR/parens) are rewritten; anything fancier keeps
/// literal semantics rather than risk changing its meaning.
fn fold_norm(addr: &str) -> String {
    let lower = addr.trim().trim_matches(|c| c == '<' || c == '>').to_lowercase();
    match lower.split_once('@') {
        Some((local, domain)) => format!("{}@{}", addr_base(local), domain),
        None => lower,
    }
}

/// The message's own address pool for a term field: `from` sees only the
/// sender; `to` means "arrived via this identity", so To and CC both count.
fn pool_for<'a>(e: &'a MailEntry, field: &str) -> Vec<&'a str> {
    let mut v = Vec::new();
    if field == "from" {
        v.push(e.from.address.as_str());
    } else {
        for a in &e.to {
            v.push(a.address.as_str());
        }
        for a in &e.cc {
            v.push(a.address.as_str());
        }
    }
    v
}

struct IdentityTerms {
    terms: Vec<(String, String)>, // (field, folded address)
    residual: String,             // query to hand to the index
}

fn peel_identity_terms(raw_q: &str) -> IdentityTerms {
    let lower = raw_q.to_lowercase();
    if !raw_q.contains(':') || lower.contains(" or ") || raw_q.contains('(') {
        return IdentityTerms { terms: Vec::new(), residual: raw_q.to_string() };
    }
    let re = Regex::new(r#"(?i)\b(to|from):<?"?([A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,})"?>?"#).unwrap();
    let terms: Vec<(String, String)> = re
        .captures_iter(raw_q)
        .map(|c| (c[1].to_lowercase(), fold_norm(&c[2])))
        .collect();
    if terms.is_empty() {
        return IdentityTerms { terms: Vec::new(), residual: raw_q.to_string() };
    }
    let residual = re.replace_all(raw_q, "").trim().to_string();
    IdentityTerms { terms, residual }
}

/// Threads whose message pool satisfies every peeled term (AND across
/// fields, OR within a field — e.g. `to:a@x to:b@x` is a group mail to both).
fn thread_matches_identities(t: &Thread, entries: &[MailEntry], terms: &[(String, String)]) -> bool {
    terms.iter().all(|(field, folded)| {
        t.msgs.iter().any(|&i| pool_for(&entries[i], field).iter().any(|a| fold_norm(a) == *folded))
    })
}

fn thread_out(t: &Thread, matched: usize, entries: &[MailEntry], declared: &[String]) -> ThreadOut {
    // union of message tags, in declaration order (stable row badges)
    let mut accts: Vec<String> = Vec::new();
    if declared.len() >= 2 {
        for &i in &t.msgs {
            for tag in entry_tags(&entries[i], declared) {
                if !accts.contains(&tag) {
                    accts.push(tag);
                }
            }
        }
        accts.sort_unstable_by(|a, b| {
            declared.iter().position(|d| d == a).cmp(&declared.iter().position(|d| d == b))
        });
    }
    ThreadOut {
        tid: t.tid.clone(),
        subject: t.subject.clone(),
        from: t.from.clone(),
        date: t.date.to_rfc3339(),
        count: t.msgs.len(),
        matched,
        attachments: t.attachments,
        accts,
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
    // Loop to a fixed point: a single pass lets split markers reassemble into
    // a fresh <script> ("<sc<script></script>ript>"). The sandboxed iframe
    // would neutralize it anyway, but this layer must not merely look busy.
    let re = match Regex::new(r"(?is)<script[^>]*>.*?(</script>|$)|</?script[^>]*>") {
        Ok(r) => r,
        Err(_) => return html.to_string(),
    };
    let mut cur = html.to_string();
    loop {
        let next = re.replace_all(&cur, "").into_owned();
        if next == cur {
            return cur;
        }
        cur = next;
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
    addrs: Vec<String>,
}

fn parse_sel(p: &FilterParams) -> Sel {
    Sel {
        cat: p.cat.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string),
        status: p.status.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string),
        other: csv_labels(&p.other),
        addrs: p
            .acct
            .as_deref()
            .map(|v| v.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default(),
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
/// `skip` names the group whose own selection is ignored (per-group facet
/// counting); pass 9 from real filtering paths, which honor every group.
fn sel_ok(e: &MailEntry, s: &Sel, skip: u8, declared: &[String]) -> bool {
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
    if skip != 3 && !s.addrs.is_empty() {
        let tags = entry_tags(e, declared);
        if !s.addrs.iter().any(|a| tags.iter().any(|t| t.eq_ignore_ascii_case(a))) {
            return false;
        }
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
        addresses: Vec::new(), // filled by the scan handler from the declared list
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
    let (epoch, addresses) = auth::read_profile(profiles_dir, slug)
        .map(|sp| (auth::secret_fingerprint(&sp.secret), sp.addresses))
        .unwrap_or_default();
    Tenant {
        slug: slug.to_string(),
        name: name.to_string(),
        epoch: Arc::new(RwLock::new(epoch)),
        addresses: Arc::new(RwLock::new(addresses.into_iter().map(|a| a.to_lowercase()).collect())),
        inner: Arc::new(RwLock::new(AppStateInner::default())),
        data_dir: data_root.join(slug),
        archive_dir: archive_root.join(slug),
        backup_dir: backup_root.join(slug),
        busy: Arc::new(AtomicBool::new(false)),
        search: Arc::new(SearchProgress::default()),
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
                let declared = state.addresses.read().unwrap().clone();
                let mut facets = compute_facets(&entries);
                facets.addresses = address_counts(&entries, &declared);
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
    let declared = state.addresses.read().unwrap().clone();
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
            || !s.addrs.is_empty()
            || after.is_some()
            || before.is_some()
            || att;
        if !any {
            return base.clone();
        }
        // Per-group facet counts: each group's labels are counted under all
        // *other* active filters, so a selection never zeroes its own group.
        let mut counts: HashMap<String, u64> = HashMap::new();
        let mut addr_counts: HashMap<String, u64> = HashMap::new();
        for e in &lock.entries {
            if !date_att_ok(e, &after, &before, att) {
                continue;
            }
            let a = sel_ok(e, &s, 0, &declared);
            let b = sel_ok(e, &s, 1, &declared);
            let c = sel_ok(e, &s, 2, &declared);
            let d = sel_ok(e, &s, 3, &declared);
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
            if d {
                for t in entry_tags(e, &declared) {
                    *addr_counts.entry(t).or_insert(0) += 1;
                }
            }
        }
        let mut labels: Vec<(String, u64)> = counts.into_iter().collect();
        labels.sort_unstable_by(|x, y| y.1.cmp(&x.1).then_with(|| x.0.cmp(&y.0)));
        labels.truncate(200);
        let addresses: Vec<(String, u64)> = declared
            .iter()
            .filter_map(|d| addr_counts.get(d).map(|c| (d.clone(), *c)).filter(|(_, c)| *c > 0))
            .collect();
        Facets {
            labels,
            addresses,
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
    let declared = state.addresses.read().unwrap().clone();
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
            || !s.addrs.is_empty()
            || after.is_some()
            || before.is_some()
            || want_att;

        if !filtered {
            return match lock.thread_order.get(offset..) {
                Some(slice) => slice
                    .iter()
                    .take(limit)
                    .map(|&ti| thread_out(&lock.threads[ti], 0, &lock.entries, &declared))
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
                    .any(|&i| sel_ok(&lock.entries[i], &s, 9, &declared) && date_att_ok(&lock.entries[i], &after, &before, false));
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
            out.push(thread_out(t, 0, &lock.entries, &declared));
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
    Query(mut params): Query<FilterParams>,
) -> Json<Vec<ThreadOut>> {
    let inner = state.inner.clone();
    let declared = state.addresses.read().unwrap().clone();
    let offset = params.offset.unwrap_or(0);
    let limit = params.limit.unwrap_or(50).clamp(1, 500);
    let raw_q = params.q.clone().unwrap_or_default();
    let peeled = peel_identity_terms(raw_q.trim());
    if !peeled.terms.is_empty() {
        // the index only ever sees the remainder; the terms are enforced
        // with fold-aware matching afterwards, where we own the semantics
        params.q = if peeled.residual.is_empty() {
            None
        } else {
            Some(peeled.residual.clone())
        };
    }
    let q = compose_query(&params);
    if q.trim().is_empty() && peeled.terms.is_empty() {
        return Json(Vec::new());
    }
    let terms = peeled.terms;

    let sp = state.search.clone();

    // The body-scan path can run for minutes on large archives: keep it off
    // the async runtime, log its milestones, and expose its counters.
    let items = tokio::task::spawn_blocking(move || {
        let _guard = SearchGuard::new(sp.clone());
        let my_id = _guard.id;
        let lock = inner.read().unwrap();
        let s = parse_sel(&params);

        let mut tids: Vec<usize>;
        let mut counts: HashMap<String, usize> = HashMap::new();

        let acct_ok = |ti: usize| -> bool {
            s.addrs.is_empty()
                || lock.threads[ti].msgs.iter().any(|&i| {
                    let tags = entry_tags(&lock.entries[i], &declared);
                    s.addrs.iter().any(|a| tags.iter().any(|tg| tg.eq_ignore_ascii_case(a)))
                })
        };

        if q.trim().is_empty() {
            // identity-only query ("to:hajni@gmail.com"): every thread whose
            // messages satisfy all terms; thread_order is already newest-first
            tids = lock.thread_order.iter().copied()
                .filter(|&ti| thread_matches_identities(&lock.threads[ti], &lock.entries, &terms) && acct_ok(ti))
                .collect();
            for &ti in &tids {
                let t = &lock.threads[ti];
                let matched = t.msgs.iter().filter(|&&i| {
                    let e = &lock.entries[i];
                    terms.iter().any(|(f, folded)| pool_for(e, f).iter().any(|a| fold_norm(a) == *folded))
                }).count();
                counts.insert(t.tid.clone(), matched);
            }
        } else {
            let mbox_path = match &lock.mbox_path {
                Some(p) => p.clone(),
                None => return Vec::new(),
            };
            let progress = |processed: usize, total: usize| {
                if let Some(slot) = sp.slots.lock().unwrap().get_mut(&my_id) {
                    slot.0 = processed;
                    slot.1 = total;
                }
                if processed > 0 && processed % 5000 == 0 {
                    // the user-visible truth lives in /api/status.search now;
                    // this is only for server-side diagnosis
                    debug!("Body search progress: {}/{} messages", processed, total);
                }
                true
            };
            match search::execute(&mbox_path, &lock.entries, &q, Some(&progress)) {
                Ok((_, results)) => {
                    // Fold matching messages into their threads; a thread shows if
                    // any message matches, with `matched` counting the hits.
                    tids = Vec::new();
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
                    // identity terms and the account filter narrow results
                    // here, after the query — the archive cannot speak of them
                    tids.retain(|&ti| thread_matches_identities(&lock.threads[ti], &lock.entries, &terms) && acct_ok(ti));
                }
                Err(e) => {
                    warn!("Search failed: {}", e);
                    return Vec::new();
                }
            }
        }

        tids.iter()
            .skip(offset)
            .take(limit)
            .map(|&ti| {
                let t = &lock.threads[ti];
                thread_out(t, counts.get(&t.tid).copied().unwrap_or(0), &lock.entries, &declared)
            })
            .collect()
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

    let (mbox_path, entry) = {
        let lock = state.inner.read().unwrap();
        match (&lock.mbox_path, lock.entries.get(idx)) {
            (Some(p), Some(e)) => (p.clone(), e.clone()),
            (None, _) => return "No MBOX loaded".to_string().into_response(),
            (_, None) => return "Email not found".to_string().into_response(),
        }
    };

    // MIME parsing plus regex surgery on untrusted HTML: real work that
    // belongs off the async runtime, like every other heavy handler here
    let rendered = tokio::task::spawn_blocking(move || -> Option<(bool, Vec<u8>)> {
        let mut store = MboxStore::open(&mbox_path).ok()?;
        let body = store.get_message(&entry).ok()?;
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
            Some((true, html.into_bytes()))
        } else if let Some(text) = &body.text {
            Some((false, text.clone().into_bytes()))
        } else {
            Some((false, b"No content".to_vec()))
        }
    })
    .await
    .ok()
    .flatten();

    match rendered {
        Some((is_html, bytes)) => {
            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox"));
            headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
            if is_html {
                (headers, axum::response::Html(bytes)).into_response()
            } else {
                (headers, String::from_utf8_lossy(&bytes).into_owned()).into_response()
            }
        }
        None => "Could not read email".to_string().into_response(),
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

#[cfg(test)]
mod html_tests {
    use super::strip_scripts;

    #[test]
    fn split_and_nested_script_tags_cannot_reassemble() {
        let out = strip_scripts("<sc<script></script>ript>alert(1)</scr</script>ipt><p>hi</p>");
        assert!(!out.to_lowercase().contains("script"), "{out}");
        assert!(out.contains("<p>hi</p>"));
    }

    #[test]
    fn benign_html_survives() {
        let src = "<table><tr><td>ok</td></tr></table>";
        assert_eq!(strip_scripts(src), src);
    }
}

#[cfg(test)]
mod address_tests {
    use super::*;

    fn declared() -> Vec<String> {
        vec!["hajni@gmail.com".into(), "hajni+work@gmail.com".into(), "hajni@outlook.com".into()]
    }

    fn mail(from: &str, to: &[&str], cc: &[&str]) -> MailEntry {
        let addr = |s: &str| mboxshell::model::address::EmailAddress {
            display_name: String::new(),
            address: s.to_string(),
        };
        MailEntry {
            offset: 0,
            length: 0,
            date: Utc::now(),
            from: addr(from),
            to: to.iter().map(|a| addr(a)).collect(),
            cc: cc.iter().map(|a| addr(a)).collect(),
            subject: String::new(),
            message_id: String::new(),
            in_reply_to: None,
            references: Vec::new(),
            has_attachments: false,
            content_type: String::new(),
            text_size: 0,
            labels: Vec::new(),
            thread_id: None,
            sequence: 0,
        }
    }

    #[test]
    fn tag_exact_specific_and_plus_collapsing() {
        let d = declared();
        // exact wins over plus-base when declared explicitly
        assert_eq!(addr_tag("hajni+work@gmail.com", &d).as_deref(), Some("hajni+work@gmail.com"));
        // undeclared plus variant collapses to the base tag
        assert_eq!(addr_tag("Hajni+News@Gmail.com", &d).as_deref(), Some("hajni@gmail.com"));
        // case-insensitive exact
        assert_eq!(addr_tag("HAJNI@OUTLOOK.COM", &d).as_deref(), Some("hajni@outlook.com"));
        // strangers
        assert_eq!(addr_tag("someone@else.org", &d), None);
        assert_eq!(addr_tag("", &d), None);
    }

    #[test]
    fn entry_tags_gate_below_two_and_dedupe() {
        let one = vec!["hajni@gmail.com".to_string()];
        let e = mail("x@y.z", &["hajni@gmail.com"], &[]);
        assert!(entry_tags(&e, &one).is_empty(), "single identity tells you nothing");
        let d = declared();
        let e2 = mail("hajni+work@gmail.com", &["hajni@outlook.com", "friend@a.nl"], &["hajni+misc@gmail.com"]);
        assert_eq!(entry_tags(&e2, &d), vec![
            "hajni+work@gmail.com".to_string(),
            "hajni@outlook.com".to_string(),
            "hajni@gmail.com".to_string(),
        ], "declaration order, deduped");
    }

    #[test]
    fn address_counts_zeroes_absent_and_orders_by_count() {
        let d = declared();
        let entries = vec![
            mail("a@b.c", &["hajni@gmail.com", "hajni+work@gmail.com"], &[]),
            mail("a@b.c", &["hajni@gmail.com"], &[]),
            mail("a@b.c", &["hajni@gmail.com"], &[]),
        ];
        assert_eq!(address_counts(&entries, &d), vec![
            ("hajni@gmail.com".to_string(), 3u64),
            ("hajni+work@gmail.com".to_string(), 1),
        ], "counted tags only, count desc, ties in declaration order");
        assert!(address_counts(&entries, &["solo@x.y".to_string()]).is_empty());
    }

    #[test]
    fn fold_norm_strips_decoration_and_plus() {
        assert_eq!(fold_norm("<HAJNI+News@Gmail.com>"), "hajni@gmail.com");
        assert_eq!(fold_norm("plain@x.io"), "plain@x.io");
    }

    #[test]
    fn peel_only_touches_simple_full_email_terms() {
        let p = peel_identity_terms("to:hajni@gmail.com subject:digest");
        assert_eq!(p.terms, vec![("to".to_string(), "hajni@gmail.com".to_string())]);
        assert_eq!(p.residual, "subject:digest");

        let one_field = peel_identity_terms("to:daniel@collado-ruiz.es");
        assert_eq!(one_field.terms.len(), 1);
        assert!(one_field.residual.is_empty(), "identity-only query: terms carry the meaning");

        let by_name = peel_identity_terms("from:dave subject:x");
        assert!(by_name.terms.is_empty() && by_name.residual == "from:dave subject:x", "non-email term stays raw");
        let complex = peel_identity_terms("to:a@x.io OR to:b@y.io");
        assert!(complex.terms.is_empty(), "OR-queries keep literal semantics");
    }

    #[test]
    fn search_guard_slots_are_isolated_and_raii() {
        let sp = std::sync::Arc::new(SearchProgress::default());
        assert!(sp.slots.lock().unwrap().is_empty());
        let g1 = SearchGuard::new(sp.clone());
        let g2 = SearchGuard::new(sp.clone());
        let (id1, id2) = (g1.id, g2.id);
        assert_ne!(id1, id2, "each request owns its slot");
        {
            let mut s = sp.slots.lock().unwrap();
            assert_eq!(s.len(), 2);
            s.get_mut(&id1).unwrap().0 = 77;
            s.get_mut(&id2).unwrap().0 = 5;
            assert_eq!(s[&id1].0, 77, "counters never bleed across requests");
        }
        drop(g1);
        assert_eq!(sp.slots.lock().unwrap().len(), 1);
        assert!(!sp.slots.lock().unwrap().contains_key(&id1), "drop reclaims the slot");
        assert!(sp.slots.lock().unwrap().contains_key(&id2));
        drop(g2);
        assert!(sp.slots.lock().unwrap().is_empty());
    }
}

#[derive(Serialize)]
struct StatusOut {
    busy: bool,
    source: Option<String>,
    total: u64,
    oldest_ts: i64,
    latest_ts: i64,
    search: SearchOut,
    backup: BackupProgress,
}

#[derive(Serialize)]
struct SearchOut {
    running: bool,
    processed: u64,
    total: u64,
}

async fn status_handler(Extension(state): Extension<Tenant>) -> Json<StatusOut> {
    let lock = state.inner.read().unwrap();
    // newest slot = the search the user started last, which is the one worth
    // showing when several are somehow in flight
    let (search_running, s_processed, s_total) = {
        let slots = state.search.slots.lock().unwrap();
        match slots.iter().max_by_key(|(id, _)| *id) {
            Some((_, (p, t))) => (true, *p as u64, *t as u64),
            None => (false, 0, 0),
        }
    };
    Json(StatusOut {
        busy: state.busy.load(Ordering::SeqCst),
        source: lock
            .mbox_path
            .as_ref()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string())),
        total: lock.facets.total,
        oldest_ts: lock.facets.min_ts,
        latest_ts: lock.facets.max_ts,
        search: SearchOut {
            running: search_running,
            processed: s_processed,
            total: s_total,
        },
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
    auth::write_profile_full(&root.profiles_dir, &t.slug, name, &stored.secret, &stored.addresses).map_err(|e| {
        bad(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("could not save: {}", e),
        )
    })?;
    info!("display name set to '{}' for '{}'", name, t.slug);
    Ok(Json(serde_json::json!({ "ok": true, "name": name })))
}

#[derive(Deserialize)]
struct AddressesIn {
    addresses: Vec<String>,
}

/// Declared identities (accounts & aliases). Saved to the profile but also
/// applied live: tags and counts derive from in-memory entries, so changing
/// the list relabels the entire archive without a rescan.
async fn change_addresses(
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
        "addresses": t.addresses.read().unwrap().clone(),
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

    async fn body_str(r: Response) -> String {
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 22).await.unwrap();
        String::from_utf8_lossy(&bytes).to_string()
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

    // ---- archive API: fixtures + helpers -------------------------------

    impl Env {
        fn mail(&self, slug: &str, file: &str, content: &str) {
            let dir = self.data.join(slug);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(file), content).unwrap();
        }
        fn add_with_addresses(&self, slug: &str, name: &str, password: &str, addresses: &[String]) {
            self.add(slug, name, password);
            let sp = auth::read_profile(&self.profiles, slug).unwrap();
            auth::write_profile_full(&self.profiles, slug, name, &sp.secret, addresses).unwrap();
        }
    }

    /// A two-message thread (reply chains to q1) plus, in a second file, a
    /// newer solo message and an edited copy of q1 for merge dedup tests.
    /// mboxshell threads solely on Gmail's `X-GM-THRID`, which real Takeout
    /// exports carry on every message — so the fixture does too.
    const THREAD_MBOX: &str = r#"From dave@example.com Mon Jan  5 10:00:00 2026
From: Dave Liang <dave@example.com>
To: me@example.com
Subject: Quarterly report
Date: Mon, 5 Jan 2026 10:00:00 +0000
Message-ID: <q1@example.com>
X-GM-THRID: 18ab3c4d5e6f7081

Q1 numbers are in. See the quarterly report summary for details.

From me@example.com Tue Jan  6 09:00:00 2026
From: Me <me@example.com>
To: dave@example.com
Subject: Re: Quarterly report
Date: Tue, 6 Jan 2026 09:00:00 +0000
Message-ID: <q2@example.com>
In-Reply-To: <q1@example.com>
References: <q1@example.com>
X-GM-THRID: 18ab3c4d5e6f7081

Thanks, reviewing it now.
"#;

    const LUNCH_MBOX: &str = r#"From dave@example.com Thu Jan  8 12:00:00 2026
From: Dave Liang <dave@example.com>
To: me+inbox@example.com
Subject: Lunch plans
Date: Thu, 8 Jan 2026 12:00:00 +0000
Message-ID: <l1@example.com>

Pizza or sushi, decide quickly.
"#;

    const UPDATE_MBOX: &str = r#"From dave@example.com Fri Jan  9 12:00:00 2026
From: Dave Liang <dave@example.com>
To: me@example.com
Subject: Quarterly report final
Date: Fri, 9 Jan 2026 12:00:00 +0000
Message-ID: <q1@example.com>
X-GM-THRID: 18ab3c4d5e6f7081

Same message id, corrected body text here.
"#;

    async fn scan(app: &Router, cookie: &str) {
        let r = send(app, req("POST", "/api/scan", Body::empty(), Some(cookie), false)).await;
        assert_eq!(r.status(), StatusCode::OK);
    }

    async fn post(app: &Router, cookie: &str, uri: &str) -> StatusCode {
        send(app, req("POST", uri, Body::empty(), Some(cookie), false)).await.status()
    }

    async fn wait_indexed(app: &Router, cookie: &str, total: u64) -> serde_json::Value {
        for _ in 0..200 {
            let (_, st) = get_json(app, "/api/status", Some(cookie)).await;
            if !st["busy"].as_bool().unwrap() && st["total"].as_u64() == Some(total) {
                return st;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("index never reached total={total}");
    }

    async fn threads(app: &Router, cookie: &str, uri: &str) -> serde_json::Value {
        let (s, v) = get_json(app, uri, Some(cookie)).await;
        assert_eq!(s, StatusCode::OK, "{uri}");
        v
    }

    /// Thread ids are raw Message-IDs (`<a1@x>`), illegal bare in a URI.
    fn enc(s: &str) -> String {
        s.replace('%', "%25").replace('<', "%3C").replace('>', "%3E").replace(' ', "%20")
    }

    #[tokio::test]
    async fn scan_indexes_threads_email_and_facets() {
        let env = Env::new("scanapi");
        env.add("daniel", "Daniel", "daniel");
        // one file: with several raw mboxes and no archive, scan indexes only the newest
        env.mail("daniel", "full.mbox", &format!("{THREAD_MBOX}\n{LUNCH_MBOX}"));
        let app = env.app();
        let cookie = cookie_of(&login(&app, "daniel", "daniel").await);

        scan(&app, &cookie).await;
        let st = wait_indexed(&app, &cookie, 3).await;
        assert_eq!(st["busy"], false);

        let list = threads(&app, &cookie, "/api/messages").await;
        assert_eq!(list.as_array().unwrap().len(), 2, "two threads: reply folded into root");
        assert_eq!(list[0]["subject"], "Lunch plans", "newest thread first");
        let q = &list[1];
        assert_eq!(q["subject"], "Quarterly report");
        assert_eq!(q["count"], 2);
        assert_eq!(q["from"], "Dave Liang <dave@example.com>");

        let (_, fac) = get_json(&app, "/api/filters", Some(&cookie)).await;
        assert_eq!(fac["total"], 3);
        assert!(fac["min_ts"].as_i64().unwrap() < fac["max_ts"].as_i64().unwrap());

        let tid = q["tid"].as_str().unwrap();
        let (_, det) = get_json(&app, &format!("/api/thread?tid={}", enc(tid)), Some(&cookie)).await;
        let msgs = det["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(
            msgs[0]["date"].as_str().unwrap() < msgs[1]["date"].as_str().unwrap(),
            "thread messages ascending"
        );

        let id = msgs[0]["id"].as_str().unwrap();
        let r = send(&app, req("GET", &format!("/api/email/{id}"), Body::empty(), Some(&cookie), false)).await;
        assert_eq!(r.status(), StatusCode::OK);
        assert!(body_str(r).await.contains("quarterly report summary"));
        let (_, meta) = get_json(&app, &format!("/api/email/{id}/meta"), Some(&cookie)).await;
        assert_eq!(meta["subject"], "Quarterly report");
        assert_eq!(meta["attachments"].as_array().unwrap().len(), 0);

        let r = send(&app, req("GET", "/api/thread?tid=nope", Body::empty(), Some(&cookie), false)).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn search_matches_subject_but_bodies_only_when_requested() {
        let env = Env::new("searchapi");
        env.add("daniel", "Daniel", "daniel");
        env.mail("daniel", "full.mbox", &format!("{THREAD_MBOX}\n{LUNCH_MBOX}"));
        let app = env.app();
        let cookie = cookie_of(&login(&app, "daniel", "daniel").await);
        scan(&app, &cookie).await;
        wait_indexed(&app, &cookie, 3).await;

        let r = threads(&app, &cookie, "/api/search?q=quarterly").await;
        assert_eq!(r.as_array().unwrap().len(), 1, "subject hit folds to one thread");
        assert_eq!(r[0]["subject"], "Quarterly report");
        assert!(r[0]["matched"].as_u64().unwrap() >= 1);

        let r = threads(&app, &cookie, "/api/search?q=from:dave").await;
        assert_eq!(r.as_array().unwrap().len(), 2, "both threads are from dave (one message each)");

        // "summary" exists only in the body: absent by default, found with body=1
        let r = threads(&app, &cookie, "/api/search?q=summary").await;
        assert_eq!(r.as_array().unwrap().len(), 0);
        let r = threads(&app, &cookie, "/api/search?q=summary&body=1").await;
        assert_eq!(r.as_array().unwrap().len(), 1);

        let r = threads(&app, &cookie, "/api/search?q=zzq-nonexistent").await;
        assert_eq!(r.as_array().unwrap().len(), 0);
        let r = threads(&app, &cookie, "/api/search?q=").await;
        assert_eq!(r.as_array().unwrap().len(), 0, "empty query never scans");
    }

    #[tokio::test]
    async fn merge_dedups_by_message_id_and_rescan_reads_the_archive() {
        let env = Env::new("mergeapi");
        env.add("daniel", "Daniel", "daniel");
        env.mail("daniel", "01.mbox", THREAD_MBOX);
        env.mail("daniel", "02.mbox", LUNCH_MBOX);
        env.mail("daniel", "03.mbox", UPDATE_MBOX);
        let app = env.app();
        let cookie = cookie_of(&login(&app, "daniel", "daniel").await);

        assert_eq!(post(&app, &cookie, "/api/merge").await, StatusCode::OK);
        for _ in 0..200 {
            if !env.archive.join("daniel").join("merged.mbox").exists() {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            } else {
                break;
            }
        }
        let merged = fs::read_to_string(env.archive.join("daniel").join("merged.mbox")).expect("merged.mbox");
        assert_eq!(merged.matches("Message-ID: <q1@example.com>").count(), 1, "duplicate id collapsed");
        assert!(merged.contains("<l1@example.com>") && merged.contains("<q2@example.com>"));

        scan(&app, &cookie).await;
        let st = wait_indexed(&app, &cookie, 3).await;
        assert_eq!(st["source"], "merged.mbox", "scan prefers the archive over raw exports");
    }

    #[tokio::test]
    async fn tenants_only_ever_see_their_own_archive() {
        let env = Env::new("scopeapi");
        env.add("daniel", "Daniel", "daniel");
        env.add("hajni", "Hajni", "hajni");
        env.mail("daniel", "export.mbox", THREAD_MBOX);
        env.mail("hajni", "export.mbox", LUNCH_MBOX);
        let app = env.app();
        let d = cookie_of(&login(&app, "daniel", "daniel").await);
        let h = cookie_of(&login(&app, "hajni", "hajni").await);
        scan(&app, &d).await;
        scan(&app, &h).await;
        wait_indexed(&app, &d, 2).await;
        wait_indexed(&app, &h, 1).await;

        let list = threads(&app, &h, "/api/messages").await;
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0]["subject"], "Lunch plans");
        let r = threads(&app, &h, "/api/search?q=quarterly").await;
        assert_eq!(r.as_array().unwrap().len(), 0, "another tenant's mail never leaks into search");
    }

    #[tokio::test]
    async fn addresses_tag_filter_and_edit_live() {
        let env = Env::new("addrs");
        env.add_with_addresses("daniel", "Daniel", "daniel", &[
            "me@example.com".into(),
            "dave@example.com".into(),
        ]);
        env.mail("daniel", "full.mbox", &format!("{THREAD_MBOX}\n{LUNCH_MBOX}"));
        let app = env.app();
        let cookie = cookie_of(&login(&app, "daniel", "daniel").await);
        scan(&app, &cookie).await;
        wait_indexed(&app, &cookie, 3).await;

        let list = threads(&app, &cookie, "/api/messages").await;
        let accts = list[0]["accts"].as_array().unwrap();
        assert_eq!(accts.len(), 2, "every fixture thread involves both declared identities");
        assert_eq!(accts[0], "me@example.com", "declaration order");
        let (_, fac) = get_json(&app, "/api/filters", Some(&cookie)).await;
        let counts = fac["addresses"].as_array().unwrap();
        assert_eq!(counts.len(), 2);
        assert_eq!(counts[0][1], 3, "me@example.com in all three messages");

        let both = threads(&app, &cookie, "/api/messages?acct=dave@example.com").await;
        assert_eq!(both.as_array().unwrap().len(), 2);
        let none = threads(&app, &cookie, "/api/messages?acct=zz@zz.zz").await;
        assert_eq!(none.as_array().unwrap().len(), 0);
        let s = threads(&app, &cookie, "/api/search?q=quarterly&acct=me@example.com").await;
        assert_eq!(s.as_array().unwrap().len(), 1);
        let s2 = threads(&app, &cookie, "/api/search?q=quarterly&acct=zz@zz.zz").await;
        assert_eq!(s2.as_array().unwrap().len(), 0);

        // live edit: lowercased, deduped, applied without rescan, epoch intact
        let r = send(
            &app,
            req(
                "POST",
                "/api/profile/addresses",
                Body::from(
                    json!({ "addresses": ["  Hajni@Gmail.com ", "hajni+work@gmail.com", "hajni@gmail.com"] })
                        .to_string(),
                ),
                Some(&cookie),
                true,
            ),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        let body = json_of(r).await;
        assert_eq!(
            body["addresses"],
            json!(["hajni@gmail.com", "hajni+work@gmail.com"])
        );
        let (st, _) = get_json(&app, "/api/whoami", Some(&cookie)).await;
        assert_eq!(st, StatusCode::OK, "address edit must not revoke sessions");
        let (_, prof) = get_json(&app, "/api/profile", Some(&cookie)).await;
        assert_eq!(prof["addresses"].as_array().unwrap().len(), 2);

        let list2 = threads(&app, &cookie, "/api/messages").await;
        assert!(list2[0].get("accts").is_none(), "no involved identity -> tag omitted, no rescan");
        let (_, fac2) = get_json(&app, "/api/filters", Some(&cookie)).await;
        assert_eq!(fac2["addresses"].as_array().unwrap().len(), 0);

        let bad = send(
            &app,
            req(
                "POST",
                "/api/profile/addresses",
                Body::from(json!({ "addresses": ["nope"] }).to_string()),
                Some(&cookie),
                true,
            ),
        )
        .await;
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
        let after = fs::read_to_string(env.profiles.join("daniel").join("profile.json")).unwrap();
        assert!(after.contains("hajni+work@gmail.com"), "rejected edit changed nothing");
    }

    #[tokio::test]
    async fn single_declared_address_stays_invisible() {
        let env = Env::new("oneaddr");
        env.add_with_addresses("daniel", "Daniel", "daniel", &["me@example.com".into()]);
        env.mail("daniel", "export.mbox", THREAD_MBOX);
        let app = env.app();
        let cookie = cookie_of(&login(&app, "daniel", "daniel").await);
        scan(&app, &cookie).await;
        wait_indexed(&app, &cookie, 2).await;
        let list = threads(&app, &cookie, "/api/messages").await;
        assert!(list[0].get("accts").is_none(), "one identity is not a distinction");
        let (_, fac) = get_json(&app, "/api/filters", Some(&cookie)).await;
        assert_eq!(fac["addresses"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn identity_search_folds_aliases_and_status_reports_search() {
        let env = Env::new("idsearch");
        env.add("daniel", "Daniel", "daniel");
        env.mail("daniel", "full.mbox", &format!("{THREAD_MBOX}\n{LUNCH_MBOX}"));
        let app = env.app();
        let cookie = cookie_of(&login(&app, "daniel", "daniel").await);
        scan(&app, &cookie).await;
        let st = wait_indexed(&app, &cookie, 3).await;
        assert_eq!(st["search"]["running"], false, "idle status exposes the search block");
        assert_eq!(st["search"]["processed"], 0);

        // to:me@example.com must find the lunch thread whose To literally says
        // me+inbox@example.com — the whole point of fold-aware identity terms
        let t = threads(&app, &cookie, "/api/search?q=to:me@example.com").await;
        let subjects: Vec<&str> = t.as_array().unwrap().iter().map(|x| x["subject"].as_str().unwrap()).collect();
        assert!(subjects.contains(&"Lunch plans"), "plus-alias recipient found: {subjects:?}");
        assert!(subjects.contains(&"Quarterly report"));

        // from: narrows to the sender pool only
        let t = threads(&app, &cookie, "/api/search?q=to:dave@example.com").await;
        assert_eq!(t.as_array().unwrap().len(), 1, "only the reply went to dave");
        assert_eq!(t[0]["subject"], "Quarterly report");

        // term + residual query: both constraints apply
        let t = threads(&app, &cookie, "/api/search?q=to:me@example.com%20subject:lunch").await;
        assert_eq!(t.as_array().unwrap().len(), 1);
        assert_eq!(t[0]["subject"], "Lunch plans");

        // unknown identity finds nothing
        let t = threads(&app, &cookie, "/api/search?q=to:nobody@nowhere.io").await;
        assert_eq!(t.as_array().unwrap().len(), 0);
    }
}
