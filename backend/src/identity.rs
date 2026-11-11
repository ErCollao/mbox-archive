use std::collections::HashMap;
use regex::Regex;
use mboxshell::model::mail::MailEntry;
use crate::model::{Thread, ThreadOut};

/// `hajni+work` -> `hajni` (plus-tags are alias decorations, not new bases)
pub(crate) fn addr_base(local: &str) -> &str {
    match local.find('+') {
        Some(i) => &local[..i],
        None => local,
    }
}

/// Match a raw header address against the declared list.
/// Exact declaration wins over its plus-base (so a separately declared
/// `hajni+work@…` keeps its own tag); `+variants` collapse into the base.
pub(crate) fn addr_tag(raw: &str, declared: &[String]) -> Option<String> {
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
pub(crate) fn entry_tags(e: &MailEntry, declared: &[String]) -> Vec<String> {
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

pub(crate) fn address_counts(entries: &[MailEntry], declared: &[String]) -> Vec<(String, u64)> {
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
    out.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    out
}

/// Gmail treats `user+tag@domain` as `user@domain`. The archive index stores
/// literal addresses, so `to:hajni@gmail.com` as a raw term would miss the
/// digest that arrived at `hajni+news@gmail.com`. Strategy: peel full-email
/// `to:`/`from:` terms out of the index query, run the index on the rest,
/// then match the peeled terms here with fold-aware equality.
/// Only simple queries (no OR/parens) are rewritten; anything fancier keeps
/// literal semantics rather than risk changing its meaning.
pub(crate) fn fold_norm(addr: &str) -> String {
    let lower = addr.trim().trim_matches(|c| c == '<' || c == '>').to_lowercase();
    match lower.split_once('@') {
        Some((local, domain)) => format!("{}@{}", addr_base(local), domain),
        None => lower,
    }
}

/// The message's own address pool for a term field: `from` sees only the
/// sender; `to` means "arrived via this identity", so To and CC both count.
pub(crate) fn pool_for<'a>(e: &'a MailEntry, field: &str) -> Vec<&'a str> {
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

pub(crate) struct IdentityTerms {
    pub(crate) terms: Vec<(String, String)>, // (field, folded address)
    pub(crate) residual: String,             // query to hand to the index
}

pub(crate) fn peel_identity_terms(raw_q: &str) -> IdentityTerms {
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
pub(crate) fn thread_matches_identities(t: &Thread, entries: &[MailEntry], terms: &[(String, String)]) -> bool {
    terms.iter().all(|(field, folded)| {
        t.msgs.iter().any(|&i| pool_for(&entries[i], field).iter().any(|a| fold_norm(a) == *folded))
    })
}

pub(crate) fn thread_out(t: &Thread, matched: usize, entries: &[MailEntry], declared: &[String]) -> ThreadOut {
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

#[cfg(test)]
mod address_tests {
    use super::*;
    use chrono::Utc;

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
}
