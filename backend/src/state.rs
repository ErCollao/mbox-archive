use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use mboxshell::model::mail::MailEntry;
use crate::model::*;
use crate::auth;

/// One person's archive: everything handlers used to share globally is now
/// scoped to a tenant. Handlers receive it via Extension after cookie auth.
#[derive(Clone)]
pub(crate) struct Tenant {
    pub(crate) slug: String,
    pub(crate) name: String,
    /// fingerprint of the current secret; sessions carry it, so a password
    /// change revokes all older cookies. Reread at boot, updated live on change.
    pub(crate) epoch: Arc<RwLock<String>>,
    /// declared identities (accounts & aliases); live so a profile edit
    /// applies without restart, like the display name
    pub(crate) addresses: Arc<RwLock<Vec<String>>>,
    pub(crate) inner: Arc<RwLock<AppStateInner>>,
    pub(crate) data_dir: PathBuf,
    pub(crate) archive_dir: PathBuf,
    pub(crate) backup_dir: PathBuf,
    pub(crate) busy: Arc<AtomicBool>,
    pub(crate) search: Arc<SearchProgress>,
    pub(crate) backup: Arc<Mutex<BackupProgress>>,
}

/// Global state: the tenant map plus everything the auth layer needs.
#[derive(Clone)]
pub(crate) struct Root {
    pub(crate) tenants: Arc<HashMap<String, Tenant>>,
    pub(crate) profiles_dir: PathBuf,
    pub(crate) key: Arc<Vec<u8>>,
}

#[derive(Default)]
pub(crate) struct AppStateInner {
    pub(crate) mbox_path: Option<PathBuf>,
    pub(crate) entries: Vec<MailEntry>,
    pub(crate) order: Vec<usize>,
    pub(crate) facets: Facets,
    pub(crate) threads: Vec<Thread>,
    pub(crate) thread_order: Vec<usize>,
    pub(crate) tmap: HashMap<String, usize>,
}

/// Clears the shared busy flag on scope exit, however the task returns.
pub(crate) struct BusyGuard(pub(crate) Arc<AtomicBool>);

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Ensures the backup progress UI never gets stuck on a stale state.
pub(crate) struct BackupGuard(pub(crate) Arc<Mutex<BackupProgress>>);

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
pub(crate) struct SearchProgress {
    pub(crate) next_id: AtomicU64,
    pub(crate) slots: Mutex<HashMap<u64, (usize, usize)>>,
}

pub(crate) struct SearchGuard {
    pub(crate) sp: Arc<SearchProgress>,
    pub(crate) id: u64,
}

impl SearchGuard {
    pub(crate) fn new(sp: Arc<SearchProgress>) -> Self {
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

/// A tenant with the boot-time epoch seeded from its on-disk profile, so a
/// restart doesn't invalidate sessions signed under the current secret.
/// Shared by boot and integration tests.
pub(crate) fn seed_tenant(
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

#[cfg(test)]
mod guard_tests {
    use super::*;
    

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
