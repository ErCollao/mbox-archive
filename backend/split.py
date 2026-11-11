#!/usr/bin/env python3
"""Split main.rs into domain modules by top-level item index."""
import re, sys

SRC = 'src/main.rs'
lines = open(SRC).read().split('\n')

# --- locate top-level items -------------------------------------------------
starts = []
for i, l in enumerate(lines):
    if re.match(r'^(use |extern |mod |pub |fn |async fn |struct |enum |impl |const |static |trait |macro_rules|#\[|#\})', l):
        starts.append(i)
starts = sorted(set(starts))

items = []  # (start, end, name)
for j, s in enumerate(starts):
    e = (starts[j+1] if j+1 < len(starts) else len(lines))
    txt = lines[s]
    m = re.match(r'(?:#\[[^\]]*\]|(?:pub(?:\(crate\))? )?(?:async )?(?:fn|struct|enum|impl(?:<[^>]*>)?|trait|const|static|mod|use)\s+([A-Za-z_0-9:<>]+)?)?', txt)
    name = ''
    mm = re.search(r'\b(?:fn|struct|enum|trait|const|static|mod)\s+([A-Za-z_0-9]+)', txt)
    if mm: name = mm.group(1)
    if txt.startswith('#['): name = ''  # attribute: attach marker
    items.append([s, e, name])

# attach doc comments that trail the previous chunk back to the next item
def move_comment_runs(items):
    for j in range(1, len(items)):
        prev, cur = items[j-1], items[j]
        k = cur[0] - 1
        # walk back over blank/comment lines belonging to this item
        seen_comment = False
        p = k
        while p > prev[0] and (p >= prev[1] - 1):
            break
        # scan the tail of the previous chunk: last run of comment(+blank) lines
        tail = prev[1] - 1
        b = tail
        while b > prev[0] and (lines[b].strip() == '' or lines[b].lstrip().startswith('//')):
            b -= 1
        run_start = b + 1
        if run_start < tail and any(lines[x].lstrip().startswith('//') for x in range(run_start, min(tail+1, len(lines)))):
            # only move if run touches cur[0] boundary via previous chunk end
            # previous chunk ends at next start (cur[0]) — run is at very tail
            prev[1] = run_start
            cur[0] = run_start
move_comment_runs(items)

# --- name → module map ------------------------------------------------------
MAP = {
 'state':   ['Tenant','Root','BackupProgress','AppStateInner','BusyGuard','SearchProgress','SearchGuard','seed_tenant','Drop'],
 'model':   ['Thread','ThreadOut','Facets','FilterParams','EmailMetadata','ScanResponse','AttachmentInfo','EmailMetaOut','ThreadDetail','TidParam','StatusOut','SearchOut','BackupOut','ProfileOut','LoginIn','PasswordIn','NameIn','AddressesIn','PortraitIn','Default'],
 'identity':['addr_base','addr_tag','entry_tags','address_counts','fold_norm','pool_for','IdentityTerms','peel_identity_terms','thread_matches_identities','thread_out','address_tests'],
 'index':   ['Sel','parse_sel','has_lbl','STATUS_LABELS','CATEGORY_TABS','group_of','sel_ok','date_att_ok','compose_query','compute_facets','build_threads','tid_of','clean_subject','parse_bound','csv_labels'],
 'html':    ['BASE_STYLE','FOLD_STYLE','fold_quotes','strip_scripts','rewrite_cid','percent_encode','html_tests'],
 'archive': ['merge_handler','scan_handler','filters_handler','list_messages','thread_detail','search_handler','get_email_content','email_meta','get_attachment_data','status_handler','backup_handler','metadata_for'],
 'account': ['require_tenant','list_profiles','login_handler','logout_handler','whoami','SLUG_OK','slug_re','portrait_handler','change_password','change_name','change_addresses','upload_portrait','delete_portrait','profile_info','display_name'],
 'tests':   ['integration_tests'],
 'main':    ['main','build_app'],
}
name2mod = {}
for mod, names in MAP.items():
    for n in names: name2mod[n] = mod

buckets = {m: [] for m in MAP}
use_prelude = []
pending_attr = None
for s, e, name in items:
    chunk = '\n'.join(lines[s:e])
    first = lines[s]
    if first.startswith('use ') or first.startswith('extern '):
        use_prelude.append(chunk.strip()); continue
    if first.startswith('mod '):
        continue  # module decls rewritten wholesale
    tgt = name2mod.get(name)
    if tgt is None:
        if first.startswith('#['):
            pending_attr = (chunk, s, e)
            continue
        if name == '' and 'fn main' in chunk:
            tgt = 'main'
        elif re.search(r'impl\s+Default\s+for\s+Facets', chunk):
            tgt = 'model'
        elif re.search(r'impl\s+Drop\s+for\s+(BusyGuard|BackupGuard|SearchGuard)', chunk):
            tgt = 'state'
        elif re.search(r'impl\s+SearchGuard', chunk):
            tgt = 'state'
        else:
            print('UNMAPPED:', first[:70]); sys.exit(1)
    if pending_attr and name2mod.get(name) == pending_attr[3] if False else False:
        pass
    buckets[tgt].append(chunk)
    if pending_attr:
        # insert preceding attributes right before this item
        buckets[tgt][-2 if False else -1] = pending_attr[0] + '\n' + chunk
        pending_attr = None

# attributes attach to the FOLLOWING item: rebuild buckets with lookahead
buckets = {m: [] for m in MAP}
i = 0
flat = items
lookup = {}
def target_of(s, e, name, first):
    if first.startswith('use ') or first.startswith('extern ') or first.startswith('mod '): return None
    if name in name2mod: return name2mod[name]
    if 'fn main' in first: return 'main'
    if re.search(r'impl\s+Default\s+for\s+Facets', '\n'.join(lines[s:e])): return 'model'
    if re.search(r'impl\s+Drop\s+for\s+(BusyGuard|BackupGuard|SearchGuard)', '\n'.join(lines[s:e])): return 'state'
    if re.search(r'impl\s+SearchGuard', first): return 'state'
    return '???'
for s, e, name in flat:
    first = lines[s]
    if first.startswith('use ') or first.startswith('extern '): continue
    if first.startswith('mod '): continue
    if first.startswith('#['):  # attribute: prepend to next non-attribute chunk by same ownership
        tgt = None
        # look ahead for the target of the next item
        nxt = [t for t in flat if t[0] == e]
        nt = target_of(nxt[0][0], nxt[0][1], nxt[0][2], lines[nxt[0][0]]) if nxt and nxt[0][0] < len(lines) else None
        if nt is None or nt == '???':
            nt = None
        if nt:
            buckets[nt].append('\n'.join(lines[s:e]))
            continue
        # main() attribute belongs to main via next item lookup above
        buckets['main'].append('\n'.join(lines[s:e]))
        continue
    t = target_of(s, e, name, first)
    if t == '???':
        print('UNMAPPED at line', s+1, ':', first[:80]); sys.exit(1)
    buckets[t].append('\n'.join(lines[s:e]))

# --- visibility rewriting ---------------------------------------------------
HEADERS = {
'state': """use crate::auth;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
""",
'model': """use chrono::{DateTime, Utc};
use mboxshell::model::mail::MailEntry;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
""",
'identity': """use crate::model::{Thread, ThreadOut};
use mboxshell::model::mail::MailEntry;
use regex::Regex;
""",
'index': """use crate::identity::entry_tags;
use crate::model::{Facets, FilterParams, Thread};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use mboxshell::model::mail::MailEntry;
use regex::Regex;
use std::collections::HashMap;
""",
'html': """use mboxshell::model::attachment::AttachmentMeta;
use regex::Regex;
use std::collections::HashMap;
""",
'archive': """use crate::html::{fold_quotes, percent_encode, rewrite_cid, strip_scripts, BASE_STYLE, FOLD_STYLE};
use crate::identity::{address_counts, entry_tags, peel_identity_terms, thread_matches_identities, thread_out};
use crate::index::{build_threads, compute_facets, compose_query, date_att_ok, parse_bound, parse_sel, sel_ok, tid_of};
use crate::model::*;
use crate::state::{SearchGuard, Tenant};
use axum::{
    extract::{Extension, Path, Query},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Json, Response},
};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use mboxshell::index::builder;
use mboxshell::search;
use mboxshell::store::reader::MboxStore;
use std::collections::HashMap;
use std::io::{BufReader, BufWriter, Read, Write};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::SystemTime;
use std::fs;
use std::path::PathBuf;
use tracing::{debug, error, info, warn};
use walkdir::WalkDir;
""",
'account': """use crate::auth;
use crate::model::*;
use crate::state::{Root, Tenant};
use axum::{
    extract::{Extension, Path, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Json, Response},
};
use regex::Regex;
use std::fs;
use std::sync::OnceLock;
use tracing::info;
""",
'tests': """use crate::auth;
use crate::build_app;
use crate::html::*;
use crate::identity::*;
use crate::index::*;
use crate::model::*;
use crate::state::*;
use crate::handlers::{account::*, archive::*};
use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::Router;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::fs;
use tower::ServiceExt;
""",
}

def visib(text):
    out = []
    for l in text.split('\n'):
        if re.match(r'^(async fn|fn|pub fn|pub async fn|struct|pub struct|enum|pub enum|const|pub const|static|pub static|trait|pub trait)\b', l):
            core = l
            if core.startswith('pub '): pass
            elif core.startswith('pub('): pass
            else: core = 'pub(crate) ' + core
            out.append(core); continue
        if re.match(r'^\s{4}(mut\s+)?[a-z_][a-z_0-9]*\s*:', l):  # struct field
            out.append(re.sub(r'^(\s+)([a-z_])', r'\1pub(crate) \2', l)); continue
        if l.strip() == 'fn new(' :
            out.append(l.replace('fn new(', 'pub(crate) fn new(')); continue
        out.append(l)
    # tuple guards: BusyGuard(Arc<...>) field private → pub(crate)
    text2 = '\n'.join(out)
    text2 = re.sub(r'(pub\(crate\) struct (?:BusyGuard|BackupGuard)\()Arc', r'\1pub(crate) Arc', text2)
    text2 = re.sub(r'(pub\(crate\) struct SearchGuard \{\n\s+)sp:', r'\1pub(crate) sp:', text2)
    return text2

import os
os.makedirs('src/handlers', exist_ok=True)

for mod in ['state','model','identity','index','html']:
    open(f'src/{mod}.rs','w').write(HEADERS[mod] + '\n' + visib('\n\n'.join(buckets[mod])) + '\n')
open('src/handlers/archive.rs','w').write(HEADERS['archive'] + '\n' + visib('\n\n'.join(buckets['archive'])) + '\n')
open('src/handlers/account.rs','w').write(HEADERS['account'] + '\n' + visib('\n\n'.join(buckets['account'])) + '\n')
open('src/handlers/mod.rs','w').write('pub(crate) mod account;\npub(crate) mod archive;\n')

tests_body = '\n\n'.join(buckets['tests'])
tests_body = tests_body.replace('mod integration_tests {', '', 1)
# drop the trailing closing brace of the old module and the use super::* line
tests_body = re.sub(r'use super::\*;', 'use crate::axum_reexports_GLOB_PLACEHOLDER;', tests_body) if 'use super::*;' in tests_body else tests_body
# strip the last lone "}" that closed the mod
tb = tests_body.rstrip()
assert tb.endswith('}'), tb[-80:]
tests_body = tb[:-1]
# strip the internal "use" lines that HEADERS already provides
for ul in ['use axum::body::Body;','use axum::http::{header, StatusCode};','use serde_json::json;','use tower::ServiceExt;','use super::*;']:
    tests_body = tests_body.replace(ul, '')
open('src/api_tests.rs','w').write(HEADERS['tests'] + '\n' + tests_body.strip() + '\n')

# --- new main.rs --------------------------------------------------------------
main_mod = 'mod auth;\npub(crate) mod state;\npub(crate) mod model;\npub(crate) mod identity;\npub(crate) mod index;\npub(crate) mod html;\npub(crate) mod handlers;\n#[cfg(test)]\nmod api_tests;\n'
keep_use = """use axum::{
    middleware,
    routing::{get, post},
    Router,
};
use state::{seed_tenant, Root};
use std::fs;
use std::sync::{Arc, RwLock};
use handlers::{account::*, archive::*};
use tracing::info;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
"""
main_chunks = visib('\n\n'.join(buckets['main']))
open('src/main.rs','w').write(main_mod + '\n' + keep_use + '\n' + main_chunks + '\n')
print('split written:', {k: len(v) for k, v in buckets.items()})
