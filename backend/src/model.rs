use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

#[derive(Serialize, Clone, Default)]
pub(crate) struct BackupProgress {
    pub(crate) running: bool,
    pub(crate) phase: String,
    pub(crate) done: u64,
    pub(crate) total: u64,
}

#[derive(Clone)]
pub(crate) struct Thread {
    pub(crate) tid: String,
    pub(crate) msgs: Vec<usize>,
    pub(crate) subject: String,
    pub(crate) from: String,
    pub(crate) date: DateTime<Utc>,
    pub(crate) attachments: bool,
}

#[derive(Serialize)]
pub(crate) struct ThreadOut {
    pub(crate) tid: String,
    pub(crate) subject: String,
    pub(crate) from: String,
    pub(crate) date: String,
    pub(crate) count: usize,
    pub(crate) matched: usize,
    pub(crate) attachments: bool,
    /// declared addresses involved in this thread; empty (omitted) unless the
    /// tenant has 2+ addresses configured
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) accts: Vec<String>,
}

#[derive(Serialize, Clone, Default)]
pub(crate) struct Facets {
    pub(crate) labels: Vec<(String, u64)>,
    #[serde(default)]
    pub(crate) addresses: Vec<(String, u64)>,
    pub(crate) min_ts: i64,
    pub(crate) max_ts: i64,
    pub(crate) total: u64,
}

#[derive(Deserialize)]
pub(crate) struct FilterParams {
    pub(crate) q: Option<String>,
    pub(crate) cat: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) other: Option<String>,
    pub(crate) after: Option<String>,
    pub(crate) before: Option<String>,
    pub(crate) attachment: Option<String>,
    pub(crate) acct: Option<String>,
    pub(crate) body: Option<String>,
    pub(crate) offset: Option<usize>,
    pub(crate) limit: Option<usize>,
}

#[derive(Serialize)]
pub(crate) struct EmailMetadata {
    pub(crate) id: String,
    pub(crate) subject: String,
    pub(crate) from: String,
    pub(crate) date: String,
    pub(crate) snippet: String,
    pub(crate) attachments: bool,
}

#[derive(Serialize)]
pub(crate) struct ScanResponse {
    pub(crate) message: String,
    pub(crate) status: String,
}

#[derive(Serialize)]
pub(crate) struct AttachmentInfo {
    pub(crate) filename: String,
    pub(crate) content_type: String,
    pub(crate) size: u64,
    pub(crate) is_inline: bool,
    pub(crate) part_index: usize,
}

#[derive(Serialize)]
pub(crate) struct EmailMetaOut {
    pub(crate) id: String,
    pub(crate) subject: String,
    pub(crate) from: String,
    pub(crate) to: Vec<String>,
    pub(crate) date: String,
    pub(crate) labels: Vec<String>,
    pub(crate) attachments: Vec<AttachmentInfo>,
    pub(crate) inline_images: usize,
}

#[derive(Serialize)]
pub(crate) struct ThreadDetail {
    pub(crate) tid: String,
    pub(crate) subject: String,
    pub(crate) messages: Vec<EmailMetadata>,
}

#[derive(Deserialize)]
pub(crate) struct TidParam {
    pub(crate) tid: String,
}

#[derive(Serialize)]
pub(crate) struct StatusOut {
    pub(crate) busy: bool,
    pub(crate) source: Option<String>,
    pub(crate) total: u64,
    pub(crate) oldest_ts: i64,
    pub(crate) latest_ts: i64,
    pub(crate) search: SearchOut,
    pub(crate) backup: BackupProgress,
}

#[derive(Serialize)]
pub(crate) struct SearchOut {
    pub(crate) running: bool,
    pub(crate) processed: u64,
    pub(crate) total: u64,
}

#[derive(Serialize)]
pub(crate) struct BackupOut {
    pub(crate) ok: bool,
    pub(crate) message: String,
    pub(crate) target: Option<String>,
    pub(crate) bytes: Option<u64>,
    pub(crate) secs: Option<u64>,
}

#[derive(Serialize)]
pub(crate) struct ProfileOut {
    pub(crate) slug: String,
    pub(crate) name: String,
    pub(crate) has_portrait: bool,
}

#[derive(Deserialize)]
pub(crate) struct LoginIn {
    pub(crate) slug: String,
    pub(crate) password: String,
}

#[derive(Deserialize)]
pub(crate) struct PasswordIn {
    pub(crate) current: String,
    pub(crate) new: String,
}

#[derive(Deserialize)]
pub(crate) struct NameIn {
    pub(crate) name: String,
}

#[derive(Deserialize)]
pub(crate) struct AddressesIn {
    pub(crate) addresses: Vec<String>,
}

#[derive(Deserialize)]
pub(crate) struct PortraitIn {
    /// data URL or bare base64 of the image bytes
    pub(crate) data: String,
}
