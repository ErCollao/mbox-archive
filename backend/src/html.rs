use std::collections::HashMap;
use regex::Regex;
use mboxshell::model::attachment::AttachmentMeta;

/// Base font for email documents: most emails never declare a global font and
/// would otherwise render in browser-default serif. Setting it on html/body
/// only changes the inheritable default — elements with their own font-family
/// (designed newsletters etc.) are untouched.
pub(crate) const BASE_STYLE: &str = "<style>html,body{font-family:ui-sans-serif,system-ui,-apple-system,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;}</style>";

pub(crate) const FOLD_STYLE: &str = r#"<style>
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
pub(crate) fn fold_quotes(html: &str) -> String {
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

pub(crate) fn strip_scripts(html: &str) -> String {
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

pub(crate) fn rewrite_cid(html: &str, atts: &[AttachmentMeta], mail_id: &str) -> String {
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

pub(crate) fn percent_encode(s: &str) -> String {
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
