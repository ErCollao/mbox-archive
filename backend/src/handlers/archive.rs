use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::SystemTime;
use chrono::Utc;
use mboxshell::model::mail::MailEntry;
use tracing::{debug, info, warn, error};
use crate::model::*;
use crate::html::*;
use crate::identity::*;
use crate::index::*;
use crate::state::*;
use crate::state::Tenant;
use std::fs;
use axum::{extract::{Extension, Path, Query}, http::{header, HeaderMap, HeaderValue, StatusCode}, response::{IntoResponse, Json}};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use mboxshell::index::builder;
use mboxshell::search;
use mboxshell::store::reader::MboxStore;
use std::io::{BufReader, BufWriter, Read, Write};
use walkdir::WalkDir;

pub(crate) fn metadata_for(inner: &AppStateInner, idx: usize) -> EmailMetadata {
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

pub(crate) async fn merge_handler(Extension(state): Extension<Tenant>) -> Json<ScanResponse> {
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

pub(crate) async fn scan_handler(Extension(state): Extension<Tenant>) -> Json<ScanResponse> {
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

pub(crate) async fn filters_handler(
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

pub(crate) async fn list_messages(
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

pub(crate) async fn thread_detail(Extension(state): Extension<Tenant>, Query(q): Query<TidParam>) -> impl IntoResponse {
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

pub(crate) async fn search_handler(
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

pub(crate) async fn get_email_content(
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

pub(crate) async fn email_meta(Extension(state): Extension<Tenant>, Path(id): Path<String>) -> impl IntoResponse {
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

pub(crate) async fn get_attachment_data(
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

pub(crate) async fn status_handler(Extension(state): Extension<Tenant>) -> Json<StatusOut> {
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

/// Safe copy of the current archive, gzip-compressed: written to a temp file
/// with progress reporting, verified by full decompression, then atomically
/// renamed — a partial or corrupt backup never appears as a completed one.
pub(crate) async fn backup_handler(Extension(state): Extension<Tenant>) -> Json<BackupOut> {
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
