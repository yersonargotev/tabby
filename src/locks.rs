//! Persistent Manually Locked Tab state.
//!
//! Locks are plugin-owned state, not user-editable configuration. The v1 store is
//! keyed by Herdr's `tab_id`, but those IDs can be reused after tab or workspace
//! churn. A label that exactly matches Herdr's reported tab number marks a fresh
//! lifecycle and discards stale state for that ID. Otherwise locks remain until an
//! explicit unlock operation removes them.

use crate::herdr_client::{HerdrApi, HerdrError};
use crate::labeler::LabelCandidate;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManualLockDecision {
    AutoManaged,
    Lock { label: String },
}

pub fn is_default_tab_label(current_label: &str, tab_number: Option<u64>) -> bool {
    tab_number
        .map(|number| number.to_string())
        .is_some_and(|number| number == current_label)
}

pub fn detect_manual_lock(
    current_label: &str,
    last_plugin_label: Option<&str>,
    stable_label_candidate: Option<&LabelCandidate>,
) -> ManualLockDecision {
    if last_plugin_label.is_none() {
        return ManualLockDecision::AutoManaged;
    }

    if last_plugin_label == Some(current_label) {
        return ManualLockDecision::AutoManaged;
    }

    if stable_label_candidate.is_some_and(|candidate| candidate.label() == current_label) {
        return ManualLockDecision::AutoManaged;
    }

    ManualLockDecision::Lock {
        label: current_label.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualLock {
    tab_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
}

impl ManualLock {
    pub fn new(tab_id: impl Into<String>, label: Option<String>) -> Self {
        Self {
            tab_id: tab_id.into(),
            label,
        }
    }

    pub fn tab_id(&self) -> &str {
        &self.tab_id
    }

    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockStore {
    version: u8,
    locks: BTreeMap<String, ManualLock>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    last_plugin_labels: BTreeMap<String, String>,
}

impl Default for LockStore {
    fn default() -> Self {
        Self {
            version: 1,
            locks: BTreeMap::new(),
            last_plugin_labels: BTreeMap::new(),
        }
    }
}

impl LockStore {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, LockStoreError> {
        match fs::read_to_string(path.as_ref()) {
            Ok(contents) => {
                let store: Self = serde_json::from_str(&contents)?;
                if store.version == 1 {
                    Ok(store)
                } else {
                    Err(LockStoreError::UnsupportedVersion(store.version))
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), LockStoreError> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }

        let contents = serde_json::to_string_pretty(self)?;
        let temp_path = temp_path_for(path);
        fs::write(&temp_path, contents)?;
        fs::rename(&temp_path, path)?;
        Ok(())
    }

    pub fn lock_tab(&mut self, tab_id: impl Into<String>, label: Option<String>) {
        let tab_id = tab_id.into();
        let lock = ManualLock::new(tab_id.clone(), label);
        self.locks.insert(tab_id, lock);
    }

    pub fn is_locked(&self, tab_id: &str) -> bool {
        self.locks.contains_key(tab_id)
    }

    pub fn last_plugin_label(&self, tab_id: &str) -> Option<&str> {
        self.last_plugin_labels.get(tab_id).map(String::as_str)
    }

    pub fn record_plugin_label(
        &mut self,
        tab_id: impl Into<String>,
        label: impl Into<String>,
    ) -> bool {
        let tab_id = tab_id.into();
        let label = label.into();
        if self.last_plugin_labels.get(&tab_id) == Some(&label) {
            return false;
        }
        self.last_plugin_labels.insert(tab_id, label);
        true
    }

    pub fn discard_tab_state_for_default_label(
        &mut self,
        tab_id: &str,
        current_label: &str,
        tab_number: Option<u64>,
    ) -> bool {
        if !is_default_tab_label(current_label, tab_number) {
            return false;
        }

        self.discard_tab_state(tab_id)
    }

    pub(crate) fn discard_tab_state(&mut self, tab_id: &str) -> bool {
        let removed_lock = self.locks.remove(tab_id).is_some();
        let removed_baseline = self.last_plugin_labels.remove(tab_id).is_some();
        removed_lock || removed_baseline
    }

    pub fn unlock_tab(&mut self, tab_id: &str) -> bool {
        self.locks.remove(tab_id).is_some()
    }

    pub fn unlock_all(&mut self) {
        self.locks.clear();
    }

    pub fn locks(&self) -> impl Iterator<Item = &ManualLock> {
        self.locks.values()
    }

    pub fn len(&self) -> usize {
        self.locks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.locks.is_empty()
    }
}

pub fn lock_tab_at_path(
    path: impl AsRef<Path>,
    tab_id: &str,
    label: Option<String>,
) -> Result<(), LockStoreError> {
    mutate_store_at_path(path, |store| store.lock_tab(tab_id.to_string(), label))
}

pub fn unlock_tab_at_path(path: impl AsRef<Path>, tab_id: &str) -> Result<bool, LockStoreError> {
    mutate_store_at_path(path, |store| store.unlock_tab(tab_id))
}

pub fn unlock_all_at_path(path: impl AsRef<Path>) -> Result<(), LockStoreError> {
    mutate_store_at_path(path, LockStore::unlock_all)
}

fn mutate_store_at_path<R>(
    path: impl AsRef<Path>,
    mutate: impl FnOnce(&mut LockStore) -> R,
) -> Result<R, LockStoreError> {
    let path = path.as_ref();
    let mut store = LockStore::load(path)?;
    let result = mutate(&mut store);
    store.save(path)?;
    Ok(result)
}

pub fn unlock_focused_tab_at_path<C>(
    path: impl AsRef<Path>,
    herdr: &mut C,
) -> Result<UnlockFocusedOutcome, UnlockFocusedError>
where
    C: HerdrApi,
{
    let focused_tab_id = herdr
        .list_tabs()?
        .into_iter()
        .find(|tab| tab.focused)
        .map(|tab| tab.tab_id);

    let Some(tab_id) = focused_tab_id else {
        return Ok(UnlockFocusedOutcome::NoFocusedTab);
    };

    if unlock_tab_at_path(path, &tab_id)? {
        Ok(UnlockFocusedOutcome::Unlocked { tab_id })
    } else {
        Ok(UnlockFocusedOutcome::NotLocked { tab_id })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnlockFocusedOutcome {
    NoFocusedTab,
    Unlocked { tab_id: String },
    NotLocked { tab_id: String },
}

#[derive(Debug)]
pub enum LockStoreError {
    Io(io::Error),
    Json(serde_json::Error),
    UnsupportedVersion(u8),
}

impl fmt::Display for LockStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "lock store I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "lock store JSON parsing failed: {error}"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported lock store version `{version}`")
            }
        }
    }
}

impl std::error::Error for LockStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::UnsupportedVersion(_) => None,
        }
    }
}

impl From<io::Error> for LockStoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for LockStoreError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Debug)]
pub enum UnlockFocusedError {
    Herdr(HerdrError),
    LockStore(LockStoreError),
}

impl fmt::Display for UnlockFocusedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Herdr(error) => write!(formatter, "failed to find focused Herdr tab: {error}"),
            Self::LockStore(error) => write!(formatter, "failed to unlock focused tab: {error}"),
        }
    }
}

impl std::error::Error for UnlockFocusedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Herdr(error) => Some(error),
            Self::LockStore(error) => Some(error),
        }
    }
}

impl From<HerdrError> for UnlockFocusedError {
    fn from(error: HerdrError) -> Self {
        Self::Herdr(error)
    }
}

impl From<LockStoreError> for UnlockFocusedError {
    fn from(error: LockStoreError) -> Self {
        Self::LockStore(error)
    }
}

fn temp_path_for(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("locks.json");
    path.with_file_name(format!(".{file_name}.tmp"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr_client::{PaneInfo, PaneProcessInfo, RenameTabResult, TabInfo};
    use crate::labeler::LabelCandidate;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEMP_DIR_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn detects_manual_lock_when_label_differs_from_plugin_and_candidate() {
        let decision = detect_manual_lock(
            "my custom label",
            Some("nvim"),
            Some(&LabelCandidate::significant_command("codex")),
        );

        assert_eq!(
            decision,
            ManualLockDecision::Lock {
                label: "my custom label".to_string()
            }
        );
    }

    #[test]
    fn does_not_lock_when_current_label_matches_last_applied_or_seen_label() {
        let decision = detect_manual_lock(
            "nvim",
            Some("nvim"),
            Some(&LabelCandidate::significant_command("codex")),
        );

        assert_eq!(decision, ManualLockDecision::AutoManaged);
    }

    #[test]
    fn does_not_lock_when_current_label_matches_stable_label_candidate() {
        let decision = detect_manual_lock(
            "codex",
            Some("nvim"),
            Some(&LabelCandidate::significant_command("codex")),
        );

        assert_eq!(decision, ManualLockDecision::AutoManaged);
    }

    #[test]
    fn does_not_lock_without_a_prior_plugin_baseline() {
        let decision = detect_manual_lock(
            "preexisting custom label",
            None,
            Some(&LabelCandidate::working_directory_basename("tabby")),
        );

        assert_eq!(decision, ManualLockDecision::AutoManaged);
    }

    #[test]
    fn default_numeric_label_discards_state_for_reused_tab_id() {
        let mut store = LockStore::default();
        store.record_plugin_label("w2:t1", "nvim");
        store.lock_tab("w2:t1", Some("custom".to_string()));

        let changed = store.discard_tab_state_for_default_label("w2:t1", "1", Some(1));

        assert!(changed);
        assert!(!store.is_locked("w2:t1"));
        assert_eq!(store.last_plugin_label("w2:t1"), None);
    }

    #[test]
    fn non_default_numeric_label_preserves_manual_state() {
        let mut store = LockStore::default();
        store.record_plugin_label("w2:t2", "nvim");
        store.lock_tab("w2:t2", Some("1".to_string()));

        let changed = store.discard_tab_state_for_default_label("w2:t2", "1", Some(2));

        assert!(!changed);
        assert!(store.is_locked("w2:t2"));
        assert_eq!(store.last_plugin_label("w2:t2"), Some("nvim"));
    }

    #[test]
    fn lock_survives_store_reload() {
        let temp_dir = TestTempDir::new();
        let path = temp_dir.path().join("state").join("locks.json");

        let mut store = LockStore::default();
        store.lock_tab("w1:t1", Some("custom".to_string()));
        store.save(&path).expect("save lock store");

        let reloaded = LockStore::load(&path).expect("reload lock store");

        assert!(reloaded.is_locked("w1:t1"));
        assert_eq!(reloaded.len(), 1);
        assert_eq!(
            reloaded.locks().next().and_then(ManualLock::label),
            Some("custom")
        );
    }

    #[test]
    fn unlock_tab_removes_only_that_lock() {
        let mut store = LockStore::default();
        store.lock_tab("w1:t1", Some("custom one".to_string()));
        store.lock_tab("w1:t2", Some("custom two".to_string()));

        assert!(store.unlock_tab("w1:t1"));

        assert!(!store.is_locked("w1:t1"));
        assert!(store.is_locked("w1:t2"));
    }

    #[test]
    fn unlock_focused_tab_removes_only_focused_lock_from_path() {
        let temp_dir = TestTempDir::new();
        let path = temp_dir.path().join("locks.json");
        let mut store = LockStore::default();
        store.lock_tab("w1:t1", Some("editor".to_string()));
        store.lock_tab("w1:t2", Some("server".to_string()));
        store.save(&path).expect("save lock store");
        let mut herdr = FakeHerdr {
            tabs: vec![tab("w1:t1", false), tab("w1:t2", true)],
        };

        let outcome = unlock_focused_tab_at_path(&path, &mut herdr).expect("unlock focused tab");
        let reloaded = LockStore::load(&path).expect("reload lock store");

        assert_eq!(
            outcome,
            UnlockFocusedOutcome::Unlocked {
                tab_id: "w1:t2".to_string()
            }
        );
        assert!(reloaded.is_locked("w1:t1"));
        assert!(!reloaded.is_locked("w1:t2"));
    }

    #[test]
    fn unlock_tab_at_path_removes_only_that_lock() {
        let temp_dir = TestTempDir::new();
        let path = temp_dir.path().join("locks.json");
        lock_tab_at_path(&path, "w1:t1", Some("editor".to_string())).expect("lock tab one");
        lock_tab_at_path(&path, "w1:t2", Some("server".to_string())).expect("lock tab two");

        assert!(unlock_tab_at_path(&path, "w1:t1").expect("unlock tab"));
        let reloaded = LockStore::load(&path).expect("reload lock store");

        assert!(!reloaded.is_locked("w1:t1"));
        assert!(reloaded.is_locked("w1:t2"));
    }

    #[test]
    fn unlock_focused_reports_when_focused_tab_was_not_locked() {
        let temp_dir = TestTempDir::new();
        let path = temp_dir.path().join("locks.json");
        lock_tab_at_path(&path, "w1:t1", Some("editor".to_string())).expect("lock tab one");
        let mut herdr = FakeHerdr {
            tabs: vec![tab("w1:t2", true)],
        };

        let outcome = unlock_focused_tab_at_path(&path, &mut herdr).expect("unlock focused tab");
        let reloaded = LockStore::load(&path).expect("reload lock store");

        assert_eq!(
            outcome,
            UnlockFocusedOutcome::NotLocked {
                tab_id: "w1:t2".to_string()
            }
        );
        assert!(reloaded.is_locked("w1:t1"));
    }

    #[test]
    fn unlock_focused_reports_when_no_tab_is_focused() {
        let temp_dir = TestTempDir::new();
        let path = temp_dir.path().join("locks.json");
        lock_tab_at_path(&path, "w1:t1", Some("editor".to_string())).expect("lock tab one");
        let mut herdr = FakeHerdr {
            tabs: vec![tab("w1:t1", false)],
        };

        let outcome = unlock_focused_tab_at_path(&path, &mut herdr).expect("unlock focused tab");
        let reloaded = LockStore::load(&path).expect("reload lock store");

        assert_eq!(outcome, UnlockFocusedOutcome::NoFocusedTab);
        assert!(reloaded.is_locked("w1:t1"));
    }

    #[test]
    fn unlock_all_clears_all_locks() {
        let temp_dir = TestTempDir::new();
        let path = temp_dir.path().join("locks.json");
        lock_tab_at_path(&path, "w1:t1", Some("editor".to_string())).expect("lock tab one");
        lock_tab_at_path(&path, "w1:t2", Some("server".to_string())).expect("lock tab two");

        unlock_all_at_path(&path).expect("unlock all");
        let reloaded = LockStore::load(&path).expect("reload lock store");

        assert!(reloaded.is_empty());
    }

    #[test]
    fn missing_store_loads_empty_and_saves_only_to_injected_temp_path() {
        let temp_dir = TestTempDir::new();
        let path = temp_dir.path().join("nested").join("locks.json");

        let store = LockStore::load(&path).expect("missing store loads empty");
        assert!(store.is_empty());

        unlock_all_at_path(&path).expect("save empty store to injected path");
        assert!(path.exists());
        assert!(path.starts_with(temp_dir.path()));
    }

    #[test]
    fn stale_lock_is_retained_until_explicit_unlock() {
        let temp_dir = TestTempDir::new();
        let path = temp_dir.path().join("locks.json");
        lock_tab_at_path(&path, "stale-tab-id", Some("old custom".to_string()))
            .expect("lock stale tab id");
        let mut herdr = FakeHerdr {
            tabs: vec![tab("current-tab-id", true)],
        };

        let outcome = unlock_focused_tab_at_path(&path, &mut herdr).expect("unlock focused tab");
        let reloaded = LockStore::load(&path).expect("reload lock store");

        assert_eq!(
            outcome,
            UnlockFocusedOutcome::NotLocked {
                tab_id: "current-tab-id".to_string()
            }
        );
        assert!(reloaded.is_locked("stale-tab-id"));
        assert!(!reloaded.is_locked("current-tab-id"));
    }

    struct FakeHerdr {
        tabs: Vec<TabInfo>,
    }

    impl HerdrApi for FakeHerdr {
        fn list_tabs(&mut self) -> Result<Vec<TabInfo>, HerdrError> {
            Ok(self.tabs.clone())
        }

        fn list_panes(&mut self) -> Result<Vec<PaneInfo>, HerdrError> {
            unreachable!("unlock-focused only needs tab.list")
        }

        fn pane_process_info(&mut self, _pane_id: &str) -> Result<PaneProcessInfo, HerdrError> {
            unreachable!("unlock-focused only needs tab.list")
        }

        fn rename_tab(
            &mut self,
            _tab_id: &str,
            _label: &str,
        ) -> Result<RenameTabResult, HerdrError> {
            unreachable!("unlock-focused only needs tab.list")
        }
    }

    fn tab(tab_id: &str, focused: bool) -> TabInfo {
        TabInfo {
            tab_id: tab_id.to_string(),
            workspace_id: "w1".to_string(),
            number: None,
            label: "label".to_string(),
            focused,
            pane_count: None,
            agent_status: None,
        }
    }

    struct TestTempDir {
        path: PathBuf,
    }

    impl TestTempDir {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time after unix epoch")
                .as_nanos();
            let id = NEXT_TEMP_DIR_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "tabby-locks-test-{}-{unique}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
