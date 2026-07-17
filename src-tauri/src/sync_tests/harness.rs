//! Test harness: a stub cloud plus simulated machines.
//!
//! A "machine" is a temp directory acting as `$HOME` (holding `~/.codex`,
//! `~/.claude`, and — via Tauri's path resolver — the baseline, config, and
//! backup stores) plus a mock Tauri app handle. Machines act sequentially:
//! every operation first points the process-global `$HOME` at its own home,
//! which is why every test must hold [`lock_env`] for its whole body.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use tempfile::TempDir;

use super::stub_s3::StubS3;
use crate::{
    do_pull_link, do_push_link, history_object_key, new_commit_id, new_upload_id, now_secs,
    sha256_bytes, CloudCacheSlot, CloudManifest, CommitRecord, CommitSummary, HeadFile,
    LocalProfile, ManifestEntry, ProfileLink, StorageConfig, SyncConfig, SyncLink, SyncResult,
    CLOUD_SCHEMA_VERSION,
};

pub const BUCKET: &str = "test-bucket";

/// The two harness kinds, as (logical root, starter local-profile id). Fresh
/// configs use these fixed ids; tests may remove either profile later.
const KINDS: [(&str, &str); 2] = [(".codex", "codex"), (".claude", "claude")];

fn kind_profile_id(root: &str) -> &'static str {
    KINDS
        .iter()
        .find(|(kind, _)| *kind == root)
        .map(|(_, id)| *id)
        .unwrap_or_else(|| panic!("unknown root '{}'", root))
}

/// Distinct per TestCloud instance so two clouds in one test are two
/// storages with independent baselines and caches.
static STORAGE_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Serializes tests: `$HOME` is process-global state.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub async fn lock_env() -> tokio::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().await
}

fn keep_dirs() -> bool {
    std::env::var("KEEP_SYNC_TEST_DIRS").is_ok()
}

fn new_tempdir(prefix: &str) -> TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("create temp dir")
}

fn next_storage_id() -> String {
    format!("st{}", STORAGE_COUNTER.fetch_add(1, Ordering::SeqCst))
}

/// A test cloud with either transport in front of the same on-disk layout:
/// the stub S3 server over HTTP, or the app's local-folder mode reading and
/// writing the directory itself.
pub struct TestCloud {
    tmp: Option<TempDir>,
    stub: Option<StubS3>,
    /// This cloud's `StorageConfig.id` in every machine's config.
    pub storage_id: String,
}

impl TestCloud {
    /// S3 backend (stub server); the default most scenarios were written on.
    pub async fn start() -> TestCloud {
        let tmp = new_tempdir("sync-cloud-");
        fs::create_dir_all(tmp.path().join(BUCKET)).unwrap();
        let stub = StubS3::start(tmp.path().to_path_buf()).await;
        TestCloud {
            tmp: Some(tmp),
            stub: Some(stub),
            storage_id: next_storage_id(),
        }
    }

    /// Local-folder backend: no server at all — the sync engine's Store
    /// operates on the bucket directory directly.
    pub async fn start_local() -> TestCloud {
        let tmp = new_tempdir("sync-cloud-");
        fs::create_dir_all(tmp.path().join(BUCKET)).unwrap();
        TestCloud {
            tmp: Some(tmp),
            stub: None,
            storage_id: next_storage_id(),
        }
    }

    pub fn is_local(&self) -> bool {
        self.stub.is_none()
    }

    /// The stub server, on the S3 backend only.
    pub fn stub(&self) -> &StubS3 {
        self.stub.as_ref().expect("stub S3 backend")
    }

    pub fn bucket_dir(&self) -> PathBuf {
        self.tmp.as_ref().unwrap().path().join(BUCKET)
    }

    /// This cloud as one storage entry in a v2 config.
    pub fn storage_config(&self) -> StorageConfig {
        match &self.stub {
            Some(stub) => StorageConfig {
                id: self.storage_id.clone(),
                name: format!("cloud {}", self.storage_id),
                kind: "s3".to_string(),
                bucket: BUCKET.to_string(),
                access_key_id: "test-access-key".to_string(),
                secret_access_key: "test-secret".to_string(),
                s3_endpoint: stub.endpoint.clone(),
                region: "auto".to_string(),
                ..Default::default()
            },
            None => StorageConfig {
                id: self.storage_id.clone(),
                name: format!("cloud {}", self.storage_id),
                kind: "local".to_string(),
                local_dir: self.bucket_dir().to_string_lossy().into_owned(),
                ..Default::default()
            },
        }
    }

    /// Every profile in the bucket as (id, head). Scans two levels:
    /// top-level profiles and sync-link namespaces like `001/.codex`.
    fn all_profiles(&self) -> Vec<(String, HeadFile)> {
        let mut out = Vec::new();
        if let Ok(entries) = fs::read_dir(self.bucket_dir()) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('_') || !entry.path().is_dir() {
                    continue;
                }
                if let Some(head) = self.head_of(&name) {
                    out.push((name, head));
                } else if let Ok(children) = fs::read_dir(entry.path()) {
                    for child in children.flatten() {
                        if !child.path().is_dir() {
                            continue;
                        }
                        let child_name =
                            format!("{}/{}", name, child.file_name().to_string_lossy());
                        if let Some(head) = self.head_of(&child_name) {
                            out.push((child_name, head));
                        }
                    }
                }
            }
        }
        out
    }

    /// One profile id per root (last wins — use `profiles_for_root` when a
    /// root may hold several).
    pub fn profiles_by_root(&self) -> HashMap<String, String> {
        self.all_profiles()
            .into_iter()
            .map(|(id, head)| (head.root, id))
            .collect()
    }

    /// ALL profile ids holding `root`, sorted.
    pub fn profiles_for_root(&self, root: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .all_profiles()
            .into_iter()
            .filter(|(_, head)| head.root == root)
            .map(|(id, _)| id)
            .collect();
        out.sort();
        out
    }

    pub fn profile_for_root(&self, root: &str) -> String {
        self.profiles_by_root()
            .remove(root)
            .unwrap_or_else(|| panic!("no cloud profile for {}", root))
    }

    pub fn head_of(&self, profile_id: &str) -> Option<HeadFile> {
        let data = fs::read(self.bucket_dir().join(profile_id).join("_head.json")).ok()?;
        serde_json::from_slice(&data).ok()
    }

    pub fn head(&self, root: &str) -> HeadFile {
        self.head_of(&self.profile_for_root(root)).expect("head")
    }

    /// The manifest the head currently references, for the root's (single)
    /// profile. With several profiles per root, use `manifest_of`.
    pub fn manifest(&self, root: &str) -> CloudManifest {
        self.manifest_of(&self.profile_for_root(root))
    }

    pub fn manifest_of(&self, profile_id: &str) -> CloudManifest {
        let head = self.head_of(profile_id).expect("head");
        let data = fs::read(self.bucket_dir().join(profile_id).join(&head.manifest_key))
            .expect("manifest object");
        serde_json::from_slice(&data).expect("parse manifest")
    }

    pub fn manifest_file_sha(&self, root: &str, rel: &str) -> Option<String> {
        self.manifest(root)
            .files
            .get(rel)
            .map(|entry| entry.sha256.clone())
    }

    pub fn commit_records(&self, root: &str) -> Vec<CommitRecord> {
        let dir = self
            .bucket_dir()
            .join(self.profile_for_root(root))
            .join("_commits");
        let mut commits: Vec<CommitRecord> = Vec::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                if let Ok(data) = fs::read(entry.path()) {
                    if let Ok(commit) = serde_json::from_slice(&data) {
                        commits.push(commit);
                    }
                }
            }
        }
        commits.sort_by_key(|commit| commit.generation);
        commits
    }

    /// The published commit chain, newest first, walked from the head via
    /// `previous_commit_key` — excludes orphaned commits from lost races.
    pub fn commit_chain(&self, root: &str) -> Vec<CommitRecord> {
        let profile_dir = self.bucket_dir().join(self.profile_for_root(root));
        let head = self.head(root);
        let mut chain = Vec::new();
        let mut next = Some(head.commit_key);
        while let Some(key) = next {
            let commit: CommitRecord =
                serde_json::from_slice(&fs::read(profile_dir.join(&key)).expect("commit object"))
                    .expect("parse commit");
            next = commit.previous_commit_key.clone();
            chain.push(commit);
        }
        chain
    }

    /// Upload-batch metadata files, as (upload_id, status) pairs.
    pub fn upload_batches(&self, root: &str) -> Vec<(String, String)> {
        let dir = self
            .bucket_dir()
            .join(self.profile_for_root(root))
            .join("_uploads");
        let mut batches = Vec::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let id = entry.file_name().to_string_lossy().into_owned();
                if let Ok(data) = fs::read(entry.path().join("_upload.json")) {
                    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&data) {
                        let status = value["status"].as_str().unwrap_or("").to_string();
                        batches.push((id, status));
                    }
                }
            }
        }
        batches.sort();
        batches
    }
}

impl Drop for TestCloud {
    fn drop(&mut self) {
        if keep_dirs() {
            if let Some(tmp) = self.tmp.take() {
                eprintln!("[keep] cloud root: {}", tmp.keep().display());
            }
        }
    }
}

/// Publish a commit directly against the bucket directory, bypassing the
/// sync engine — simulates a concurrent machine's push landing. Callable
/// from a stub-server hook to lose a head CAS deterministically.
pub fn publish_external_commit(
    bucket_dir: &Path,
    profile_id: &str,
    files: &[(&str, &[u8])],
    actor: &str,
) -> u64 {
    let profile_dir = bucket_dir.join(profile_id);
    let head: HeadFile =
        serde_json::from_slice(&fs::read(profile_dir.join("_head.json")).expect("head"))
            .expect("parse head");
    let manifest: CloudManifest =
        serde_json::from_slice(&fs::read(profile_dir.join(&head.manifest_key)).expect("manifest"))
            .expect("parse manifest");

    let upload_id = new_upload_id().unwrap();
    let mut new_files = manifest.files.clone();
    for (rel, data) in files {
        let object_key = format!("_uploads/{}/files/{}", upload_id, rel);
        let path = profile_dir.join(&object_key);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, data).unwrap();
        new_files.insert(
            rel.to_string(),
            ManifestEntry {
                sha256: sha256_bytes(data),
                size: data.len() as u64,
                object_key,
                // Harness-seeded entries simulate an older build's manifest
                // (no captured mtime); real-machine pushes cover the field.
                source_mtime: 0,
            },
        );
    }

    let generation = head.generation + 1;
    let commit_id = new_commit_id().unwrap();
    let manifest_key = history_object_key("_manifests", generation, &commit_id);
    let commit_key = history_object_key("_commits", generation, &commit_id);
    let new_manifest = CloudManifest {
        schema_version: CLOUD_SCHEMA_VERSION,
        generation,
        commit_id: commit_id.clone(),
        updated_at: now_secs(),
        files: new_files,
        resolved_conflicts: manifest.resolved_conflicts,
    };
    let manifest_bytes = serde_json::to_vec(&new_manifest).unwrap();
    let manifest_sha256 = sha256_bytes(&manifest_bytes);
    let commit = CommitRecord {
        schema_version: CLOUD_SCHEMA_VERSION,
        commit_id: commit_id.clone(),
        generation,
        created_at: now_secs(),
        actor_name: actor.to_string(),
        machine_name: "external".to_string(),
        upload_id,
        message: format!("external push of {} file(s)", files.len()),
        manifest_key: manifest_key.clone(),
        manifest_sha256: manifest_sha256.clone(),
        previous_commit_key: Some(head.commit_key.clone()),
        previous_manifest_sha256: Some(head.manifest_sha256.clone()),
        summary: CommitSummary {
            added: files.len() as u64,
            ..Default::default()
        },
    };
    let new_head = HeadFile {
        schema_version: CLOUD_SCHEMA_VERSION,
        profile_id: profile_id.to_string(),
        root: head.root.clone(),
        state: "active".to_string(),
        generation,
        commit_id,
        manifest_key: manifest_key.clone(),
        commit_key: commit_key.clone(),
        manifest_sha256,
        updated_at: now_secs(),
    };

    fs::create_dir_all(profile_dir.join("_manifests")).unwrap();
    fs::create_dir_all(profile_dir.join("_commits")).unwrap();
    fs::write(profile_dir.join(&manifest_key), &manifest_bytes).unwrap();
    fs::write(
        profile_dir.join(&commit_key),
        serde_json::to_vec(&commit).unwrap(),
    )
    .unwrap();
    fs::write(
        profile_dir.join("_head.json"),
        serde_json::to_vec(&new_head).unwrap(),
    )
    .unwrap();
    generation
}

/// Copy a tree with fresh mtimes (read + write, never `fs::copy`) so
/// relocation tests exercise the baseline's sha path, not the stat fast
/// path.
fn copy_tree_fresh_mtimes(from: &Path, to: &Path) {
    for entry in walkdir::WalkDir::new(from)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let rel = entry.path().strip_prefix(from).unwrap();
        let dest = to.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&dest).unwrap();
        } else {
            fs::create_dir_all(dest.parent().unwrap()).unwrap();
            fs::write(&dest, fs::read(entry.path()).unwrap()).unwrap();
        }
    }
}

/// Content equality for `rels` across machines, with names in the failure.
pub fn assert_converged(machines: &[&Machine], rels: &[&str]) {
    for rel in rels {
        let contents: Vec<(&str, String)> = machines
            .iter()
            .map(|machine| (machine.name, machine.read(rel)))
            .collect();
        for pair in contents.windows(2) {
            assert_eq!(
                pair[0].1, pair[1].1,
                "'{}' diverges between {} and {}",
                rel, pair[0].0, pair[1].0
            );
        }
    }
}

/// A custom local profile beyond the two defaults: its config row, the
/// resolved mount dir (container semantics applied), and an optional pinned
/// cloud prefix applied to every storage it links to.
struct ExtraProfile {
    profile: LocalProfile,
    dir: PathBuf,
    pin: Option<String>,
}

pub struct Machine {
    pub name: &'static str,
    home: Option<TempDir>,
    /// Sync-link local side: per-root mount overrides, becoming each
    /// default profile's `path`.
    mounts: RefCell<HashMap<&'static str, PathBuf>>,
    /// Sync-link cloud side: pinned cloud prefixes per root, applied to the
    /// (kind profile, storage) link whenever a cloud's config materializes.
    pins: RefCell<HashMap<&'static str, String>>,
    /// Custom local profiles (multi-storage matrix rows beyond the two
    /// defaults), synced via `push_profile`/`pull_profile`.
    extras: RefCell<Vec<ExtraProfile>>,
    app: tauri::App<tauri::test::MockRuntime>,
}

impl Machine {
    pub fn new(name: &'static str) -> Machine {
        let home = new_tempdir(&format!("sync-home-{}-", name));
        fs::create_dir_all(home.path().join(".codex")).unwrap();
        fs::create_dir_all(home.path().join(".claude")).unwrap();
        let app = tauri::test::mock_builder()
            .manage(CloudCacheSlot::default())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("build mock app");
        Machine {
            name,
            home: Some(home),
            mounts: RefCell::new(HashMap::new()),
            pins: RefCell::new(HashMap::new()),
            extras: RefCell::new(Vec::new()),
            app,
        }
    }

    /// Register a custom local profile (a matrix row beyond the two
    /// defaults) mounted at `dir` (container semantics), optionally pinned
    /// to a cloud prefix on every storage it links to.
    pub fn add_profile(&self, id: &str, root: &str, dir: PathBuf, pin: Option<&str>) {
        let effective = Self::effective_mount(root, dir.clone());
        fs::create_dir_all(&effective).unwrap();
        self.extras.borrow_mut().push(ExtraProfile {
            profile: LocalProfile {
                id: id.to_string(),
                root: root.to_string(),
                path: dir.to_string_lossy().into_owned(),
                ..Default::default()
            },
            dir: effective,
            pin: pin.map(String::from),
        });
    }

    /// Builder: mount a root at a custom directory (created if missing).
    /// Mirrors `Roots::for_profile` container semantics: a dir not named
    /// after the root hosts it as a subdirectory.
    pub fn mount(self, root: &'static str, dir: PathBuf) -> Machine {
        let dir = Self::effective_mount(root, dir);
        fs::create_dir_all(&dir).unwrap();
        self.mounts.borrow_mut().insert(root, dir);
        self
    }

    fn effective_mount(root: &str, dir: PathBuf) -> PathBuf {
        if dir.file_name().and_then(|n| n.to_str()) == Some(root) {
            dir
        } else {
            dir.join(root)
        }
    }

    /// A machine keeping `.codex` at a custom mount (`<home>/scratch/.codex`
    /// — outside the default `~/.codex`), like a user's `/scratch/.codex`.
    pub fn with_codex_root(name: &'static str) -> Machine {
        let machine = Machine::new(name);
        let custom = machine.home().join("scratch").join(".codex");
        machine.mount(".codex", custom)
    }

    pub fn mount_dir(&self, root: &str) -> Option<PathBuf> {
        self.mounts.borrow().get(root).cloned()
    }

    /// Move the mount for `root` to `new_dir`. With `move_files`, the tree
    /// is copied over with fresh mtimes (forcing sha re-verification) and
    /// the old location removed; without it, the old tree stays behind and
    /// the new mount starts empty.
    pub fn relocate(&mut self, root: &'static str, new_dir: PathBuf, move_files: bool) {
        let new_dir = Self::effective_mount(root, new_dir);
        let old = self.path(root);
        if move_files {
            copy_tree_fresh_mtimes(&old, &new_dir);
            fs::remove_dir_all(&old).unwrap();
        } else {
            fs::create_dir_all(&new_dir).unwrap();
        }
        self.mounts.borrow_mut().insert(root, new_dir);
    }

    pub fn home(&self) -> &Path {
        self.home.as_ref().unwrap().path()
    }

    pub fn handle(&self) -> &crate::AppHandle {
        self.app.handle()
    }

    /// Point the process-global `$HOME` at this machine. Called by every
    /// operation; machines therefore act strictly one at a time.
    pub fn activate(&self) {
        std::env::set_var("HOME", self.home());
    }

    /// Logical rel → this machine's physical path (mirrors `Roots::abs`,
    /// including the app-record remap: logical `.{root}/agent-sync/**`
    /// lives under `~/.agent-sync/{codex,claude}/` — the default profile
    /// ids — never inside the root).
    pub fn path(&self, rel: &str) -> PathBuf {
        for (root, slug) in KINDS {
            let dir = self.home().join(".agent-sync").join(slug);
            if rel == format!("{}/agent-sync", root) {
                return dir;
            }
            if let Some(rest) = rel.strip_prefix(&format!("{}/agent-sync/", root)) {
                return dir.join(rest);
            }
        }
        for (root, dir) in self.mounts.borrow().iter() {
            if rel == *root {
                return dir.clone();
            }
            if let Some(rest) = rel.strip_prefix(&format!("{}/", root)) {
                return dir.join(rest);
            }
        }
        self.home().join(rel)
    }

    /// Logical rel → physical path for a custom profile (mirrors
    /// `Roots::abs` for that profile, including its per-profile
    /// `~/.agent-sync/{profile id}` remap). Default profile ids delegate to
    /// [`Machine::path`].
    pub fn profile_path(&self, profile_id: &str, rel: &str) -> PathBuf {
        if KINDS.iter().any(|(_, id)| *id == profile_id) {
            return self.path(rel);
        }
        let extras = self.extras.borrow();
        let extra = extras
            .iter()
            .find(|e| e.profile.id == profile_id)
            .unwrap_or_else(|| panic!("unknown profile '{}'", profile_id));
        let root = extra.profile.root.as_str();
        let remap = self.home().join(".agent-sync").join(profile_id);
        if rel == format!("{}/agent-sync", root) {
            return remap;
        }
        if let Some(rest) = rel.strip_prefix(&format!("{}/agent-sync/", root)) {
            return remap.join(rest);
        }
        if rel == root {
            return extra.dir.clone();
        }
        if let Some(rest) = rel.strip_prefix(&format!("{}/", root)) {
            return extra.dir.join(rest);
        }
        self.home().join(rel)
    }

    pub fn seed(&self, rel: &str, content: &str) {
        let path = self.path(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    pub fn seed_profile(&self, profile_id: &str, rel: &str, content: &str) {
        let path = self.profile_path(profile_id, rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    pub fn delete(&self, rel: &str) {
        fs::remove_file(self.path(rel)).unwrap();
    }

    pub fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.path(rel))
            .unwrap_or_else(|error| panic!("{}: read {}: {}", self.name, rel, error))
    }

    pub fn read_profile(&self, profile_id: &str, rel: &str) -> String {
        fs::read_to_string(self.profile_path(profile_id, rel)).unwrap_or_else(|error| {
            panic!("{}: read {} of '{}': {}", self.name, rel, profile_id, error)
        })
    }

    /// Files under a logical directory, as logical rels with `/` separators.
    pub fn list(&self, rel_dir: &str) -> Vec<String> {
        Self::list_under(&self.path(rel_dir), rel_dir)
    }

    pub fn list_profile(&self, profile_id: &str, rel_dir: &str) -> Vec<String> {
        Self::list_under(&self.profile_path(profile_id, rel_dir), rel_dir)
    }

    fn list_under(base: &Path, rel_dir: &str) -> Vec<String> {
        let mut out = Vec::new();
        for entry in walkdir::WalkDir::new(base)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                let rest = entry
                    .path()
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(format!("{}/{}", rel_dir, rest));
            }
        }
        out.sort();
        out
    }

    /// Upsert this cloud's storage, every local profile row (defaults with
    /// their mount paths, plus custom ones), the (profile, storage) link,
    /// and any pinned prefix into the machine's saved config — the harness
    /// equivalent of the settings matrix. Persists directly (no save-diff
    /// cleanups), like a config already at rest.
    fn ensure_link_config(&self, cloud: &TestCloud, root: &str) {
        self.ensure_profile_link(cloud, kind_profile_id(root));
    }

    fn ensure_profile_link(&self, cloud: &TestCloud, profile_id: &str) {
        self.activate();
        let mut saved = crate::load_sync_config(self.handle())
            .unwrap_or_else(|_| crate::default_sync_config());
        let storage = cloud.storage_config();
        if let Some(existing) = saved.storages.iter_mut().find(|s| s.id == storage.id) {
            let probe = existing.supports_conditional_writes;
            let opt_ins = existing.included_default_exclusions.clone();
            *existing = storage;
            existing.supports_conditional_writes = probe;
            existing.included_default_exclusions = opt_ins;
        } else {
            saved.storages.push(storage);
        }
        let mounts = self.mounts.borrow();
        for (kind, id) in KINDS {
            let path = mounts
                .get(kind)
                .map(|dir| dir.to_string_lossy().into_owned())
                .unwrap_or_default();
            if let Some(profile) = saved.local_profiles.iter_mut().find(|p| p.id == id) {
                profile.path = path;
            }
        }
        drop(mounts);
        for extra in self.extras.borrow().iter() {
            match saved
                .local_profiles
                .iter_mut()
                .find(|p| p.id == extra.profile.id)
            {
                Some(row) => *row = extra.profile.clone(),
                None => saved.local_profiles.push(extra.profile.clone()),
            }
        }
        // This profile's root kind and pin: defaults pin per root via
        // `pins`, custom profiles carry their own.
        let (root, pin) = match KINDS.iter().find(|(_, id)| *id == profile_id) {
            Some((kind, _)) => ((*kind).to_string(), self.pins.borrow().get(kind).cloned()),
            None => {
                let extras = self.extras.borrow();
                let extra = extras
                    .iter()
                    .find(|e| e.profile.id == profile_id)
                    .unwrap_or_else(|| panic!("unknown profile '{}'", profile_id));
                (extra.profile.root.clone(), extra.pin.clone())
            }
        };
        if !saved
            .links
            .iter()
            .any(|l| l.profile == profile_id && l.storage == cloud.storage_id)
        {
            saved.links.push(SyncLink {
                profile: profile_id.to_string(),
                storage: cloud.storage_id.clone(),
                cloud: ProfileLink::default(),
            });
        }
        if let Some(prefix) = pin {
            let link = saved
                .links
                .iter_mut()
                .find(|l| l.profile == profile_id && l.storage == cloud.storage_id)
                .expect("just ensured");
            if link.cloud.profile_id != prefix {
                link.cloud = ProfileLink {
                    root,
                    profile_id: prefix,
                    pinned: true,
                    ..Default::default()
                };
            } else {
                link.cloud.pinned = true;
            }
        }
        crate::persist_sync_config(self.handle(), &saved).unwrap();
    }

    /// Rels touching a root, preserving order: the root itself or paths
    /// under it (including the remapped `agent-sync` prefix, which is
    /// logically inside the root).
    fn rels_for_root<'a>(rels: &[&'a str], root: &str) -> Vec<&'a str> {
        rels.iter()
            .filter(|rel| **rel == root || rel.starts_with(&format!("{}/", root)))
            .copied()
            .collect()
    }

    fn merge_results(combined: &mut Option<SyncResult>, next: SyncResult) {
        match combined {
            None => *combined = Some(next),
            Some(result) => {
                result.files_synced += next.files_synced;
                result.message = format!("{} · {}", result.message, next.message);
                result.success = result.success && next.success;
                result.timestamp = next.timestamp;
                if next.setup_state.is_some() {
                    result.setup_state = next.setup_state;
                }
            }
        }
    }

    pub async fn push(&self, cloud: &TestCloud, rels: &[&str]) -> Result<SyncResult, String> {
        self.activate();
        let mut combined = None;
        for (root, _) in KINDS {
            let root_rels = Self::rels_for_root(rels, root);
            if root_rels.is_empty() {
                continue;
            }
            self.ensure_link_config(cloud, root);
            let files: Vec<String> = root_rels
                .iter()
                .map(|rel| self.path(rel).to_string_lossy().into_owned())
                .collect();
            let result = do_push_link(
                self.handle(),
                Arc::new(AtomicBool::new(false)),
                &cloud.storage_id,
                kind_profile_id(root),
                &files,
            )
            .await?;
            Self::merge_results(&mut combined, result);
        }
        combined.ok_or_else(|| "Nothing selected to push".to_string())
    }

    pub async fn push_all(&self, cloud: &TestCloud) -> Result<SyncResult, String> {
        self.push(cloud, &[".codex", ".claude"]).await
    }

    pub async fn pull(&self, cloud: &TestCloud) -> Result<SyncResult, String> {
        self.activate();
        let mut combined = None;
        for (root, _) in KINDS {
            self.ensure_link_config(cloud, root);
            let result = do_pull_link(self.handle(), &cloud.storage_id, kind_profile_id(root))
                .await?;
            Self::merge_results(&mut combined, result);
        }
        combined.ok_or_else(|| "nothing pulled".to_string())
    }

    /// Pull exactly one root's link (the per-cell operation).
    pub async fn pull_root(&self, cloud: &TestCloud, root: &str) -> Result<SyncResult, String> {
        self.activate();
        self.ensure_link_config(cloud, root);
        do_pull_link(self.handle(), &cloud.storage_id, kind_profile_id(root)).await
    }

    /// Push one profile's link — works for custom profiles, where the
    /// kind-based `push` cannot reach.
    pub async fn push_profile(
        &self,
        cloud: &TestCloud,
        profile_id: &str,
        rels: &[&str],
    ) -> Result<SyncResult, String> {
        self.activate();
        self.ensure_profile_link(cloud, profile_id);
        let files: Vec<String> = rels
            .iter()
            .map(|rel| {
                self.profile_path(profile_id, rel)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        do_push_link(
            self.handle(),
            Arc::new(AtomicBool::new(false)),
            &cloud.storage_id,
            profile_id,
            &files,
        )
        .await
    }

    /// Pull one profile's link — the custom-profile mirror of `pull_root`.
    pub async fn pull_profile(
        &self,
        cloud: &TestCloud,
        profile_id: &str,
    ) -> Result<SyncResult, String> {
        self.activate();
        self.ensure_profile_link(cloud, profile_id);
        do_pull_link(self.handle(), &cloud.storage_id, profile_id).await
    }

    /// The machine's persisted sync config (probe results, resolved links).
    pub fn saved_config(&self) -> SyncConfig {
        self.activate();
        crate::load_sync_config(self.handle())
            .unwrap_or_else(|_| crate::default_sync_config())
    }

    /// The saved cloud side of this machine's link for (cloud, root); None
    /// when the link is absent or never resolved.
    pub fn saved_link(&self, cloud: &TestCloud, root: &str) -> Option<ProfileLink> {
        let profile_id = kind_profile_id(root);
        self.saved_config()
            .links
            .iter()
            .find(|l| l.storage == cloud.storage_id && l.profile == profile_id)
            .map(|l| l.cloud.clone())
            .filter(|c| !c.profile_id.is_empty())
    }

    /// Resolved links on this cloud's storage, as (default profile id, cloud).
    pub fn saved_links(&self, cloud: &TestCloud) -> Vec<(String, ProfileLink)> {
        self.saved_config()
            .links
            .iter()
            .filter(|l| l.storage == cloud.storage_id && !l.cloud.profile_id.is_empty())
            .map(|l| (l.profile.clone(), l.cloud.clone()))
            .collect()
    }

    /// The saved conditional-write probe result for this cloud's storage.
    pub fn saved_probe(&self, cloud: &TestCloud) -> Option<bool> {
        self.saved_config()
            .storages
            .iter()
            .find(|s| s.id == cloud.storage_id)
            .and_then(|s| s.supports_conditional_writes)
    }

    /// Materialize this cloud's storage + both kind links in the saved
    /// config so commands that load config internally can be exercised.
    pub fn persist_cloud_config(&self, cloud: &TestCloud) {
        for (root, _) in KINDS {
            self.ensure_link_config(cloud, root);
        }
    }

    /// Set this machine's sync link for a root: local side (mount) and
    /// cloud side (pinned prefix). Empty strings revert each side to its
    /// default. Network-free, mirroring the old command's contract; the
    /// pin lands in every already-known storage and any future one.
    pub async fn set_sync_link(
        &self,
        root: &str,
        local_dir: &str,
        cloud_prefix: &str,
    ) -> Result<(), String> {
        self.activate();
        let root_key = KINDS
            .iter()
            .find(|(kind, _)| *kind == root)
            .map(|(kind, _)| *kind)
            .ok_or_else(|| format!("unknown root '{}'", root))?;
        if !cloud_prefix.is_empty() {
            crate::validate_profile_id(cloud_prefix)?;
        }
        // Local side: a trial Roots build validates the mount.
        if local_dir.is_empty() {
            self.mounts.borrow_mut().remove(root_key);
        } else {
            let probe = LocalProfile {
                id: kind_profile_id(root).to_string(),
                root: root.to_string(),
                path: local_dir.to_string(),
                ..Default::default()
            };
            let roots = crate::Roots::for_profile(&probe)?;
            self.mounts.borrow_mut().insert(root_key, roots.dir.clone());
        }
        // Cloud side, mirrored into the saved config's links for this kind.
        let profile_id = kind_profile_id(root);
        let mut saved = crate::load_sync_config(self.handle())
            .unwrap_or_else(|_| crate::default_sync_config());
        if cloud_prefix.is_empty() {
            self.pins.borrow_mut().remove(root_key);
            // Back to auto: drop pinned links so discovery relinks; a plain
            // discovered link is already "auto" and stays.
            for link in saved.links.iter_mut().filter(|l| l.profile == profile_id) {
                if link.cloud.pinned {
                    link.cloud = ProfileLink::default();
                }
            }
        } else {
            self.pins
                .borrow_mut()
                .insert(root_key, cloud_prefix.to_string());
            for link in saved.links.iter_mut().filter(|l| l.profile == profile_id) {
                if link.cloud.profile_id != cloud_prefix {
                    link.cloud = ProfileLink {
                        root: root.to_string(),
                        profile_id: cloud_prefix.to_string(),
                        pinned: true,
                        ..Default::default()
                    };
                } else {
                    link.cloud.pinned = true;
                }
            }
        }
        // Mount paths ride along for saved-config readers.
        let mounts = self.mounts.borrow();
        for (kind, id) in KINDS {
            let path = mounts
                .get(kind)
                .map(|dir| dir.to_string_lossy().into_owned())
                .unwrap_or_default();
            if let Some(profile) = saved.local_profiles.iter_mut().find(|p| p.id == id) {
                profile.path = path;
            }
        }
        drop(mounts);
        crate::persist_sync_config(self.handle(), &saved).map_err(|e| e.to_string())
    }

    /// Pin ONE link's cloud side to an exact profile id — what the UI
    /// picker persists. Unlike [`Machine::pin_cloud_prefix`]/`pins`, this
    /// targets a single (profile, storage) cell and never re-applies.
    pub fn pick_cloud_profile(
        &self,
        cloud: &TestCloud,
        local_id: &str,
        profile_id: &str,
        label: &str,
    ) {
        self.ensure_profile_link(cloud, local_id);
        let mut saved = crate::load_sync_config(self.handle())
            .unwrap_or_else(|_| crate::default_sync_config());
        let root = saved
            .local_profiles
            .iter()
            .find(|p| p.id == local_id)
            .map(|p| p.root.clone())
            .unwrap_or_else(|| panic!("unknown profile '{}'", local_id));
        let link = saved
            .links
            .iter_mut()
            .find(|l| l.profile == local_id && l.storage == cloud.storage_id)
            .expect("link just ensured");
        link.cloud = ProfileLink {
            root,
            profile_id: profile_id.to_string(),
            profile_label: label.to_string(),
            pinned: true,
            ..Default::default()
        };
        crate::persist_sync_config(self.handle(), &saved).unwrap();
    }

    /// Pin a cloud prefix for a root — the sync-link cloud side, applied to
    /// every storage this machine knows now or later.
    pub fn pin_cloud_prefix(&self, root: &str, prefix: &str) {
        self.activate();
        let root_key = KINDS
            .iter()
            .find(|(kind, _)| *kind == root)
            .map(|(kind, _)| *kind)
            .expect("known root");
        self.pins
            .borrow_mut()
            .insert(root_key, prefix.to_string());
        let profile_id = kind_profile_id(root);
        let mut saved = crate::load_sync_config(self.handle())
            .unwrap_or_else(|_| crate::default_sync_config());
        for link in saved.links.iter_mut().filter(|l| l.profile == profile_id) {
            link.cloud = ProfileLink {
                root: root.to_string(),
                profile_id: prefix.to_string(),
                pinned: true,
                ..Default::default()
            };
        }
        crate::persist_sync_config(self.handle(), &saved).unwrap();
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        if keep_dirs() {
            if let Some(home) = self.home.take() {
                eprintln!("[keep] {} home: {}", self.name, home.keep().display());
            }
        }
    }
}
