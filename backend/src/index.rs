use std::collections::HashMap;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use regex::Regex;
use mboxshell::model::mail::MailEntry;
use crate::model::*;
use crate::identity::entry_tags;

pub(crate) fn clean_subject(s: &str) -> String {
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

pub(crate) fn tid_of(entries: &[MailEntry], idx: usize) -> String {
    match entries[idx].thread_id.as_deref() {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => format!("solo-{}", idx),
    }
}

pub(crate) fn build_threads(entries: &[MailEntry]) -> (Vec<Thread>, Vec<usize>, HashMap<String, usize>) {
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

pub(crate) fn parse_bound(s: &Option<String>) -> Option<DateTime<Utc>> {
    let trimmed = s.as_deref().unwrap_or("").trim();
    if trimmed.is_empty() {
        return None;
    }
    let date = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d").ok()?;
    date.and_hms_opt(0, 0, 0)
        .map(|n| Utc.from_utc_datetime(&n))
}

pub(crate) fn csv_labels(s: &Option<String>) -> Vec<String> {
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
pub(crate) struct Sel {
    pub(crate) cat: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) other: Vec<String>,
    pub(crate) addrs: Vec<String>,
}

pub(crate) fn parse_sel(p: &FilterParams) -> Sel {
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

pub(crate) fn has_lbl(e: &MailEntry, l: &str) -> bool {
    e.labels.iter().any(|x| x.eq_ignore_ascii_case(l))
}

pub(crate) const STATUS_LABELS: [&str; 10] = ["Inbox", "Important", "Unread", "Opened", "Archived", "Starred", "Sent", "Drafts", "Spam", "Trash"];

/// Real Gmail inbox tabs. "Category Bills/Purchases/Travel/…" are auto-detected
/// labels, not tabs: they live in the `other` group.
pub(crate) const CATEGORY_TABS: [&str; 6] = [
    "Category Personal",
    "Category Social",
    "Category Updates",
    "Category Forums",
    "Category Reservations",
    "Category Promotions",
];

/// 0=top, 1=category, 2=status, 3=other. Frontend mirrors these constants.
pub(crate) fn group_of(l: &str) -> u8 {
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
pub(crate) fn sel_ok(e: &MailEntry, s: &Sel, skip: u8, declared: &[String]) -> bool {
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

pub(crate) fn date_att_ok(e: &MailEntry, after: &Option<DateTime<Utc>>, before: &Option<DateTime<Utc>>, att: bool) -> bool {
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
pub(crate) fn compose_query(p: &FilterParams) -> String {
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

pub(crate) fn compute_facets(entries: &[MailEntry]) -> Facets {
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
