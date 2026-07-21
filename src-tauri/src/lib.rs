use aws_sdk_s3::config::{Credentials, Region};
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::{Client as S3Client, Config as S3Config};
use aws_smithy_http_client::{tls, Builder as HttpClientBuilder};
use rusqlite::backup::Backup;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};
use tauri::Emitter;
use tauri::Manager;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout};
use walkdir::WalkDir;

/// Concrete Tauri runtime at every AppHandle call site: Wry in production,
/// the mock runtime under `cfg(test)` so integration tests can drive the
/// real sync flows headlessly.
#[cfg(not(test))]
type TauriRuntime = tauri::Wry;
#[cfg(test)]
type TauriRuntime = tauri::test::MockRuntime;
type AppHandle = tauri::AppHandle<TauriRuntime>;

mod activity_log;
mod codex_config;
mod codex_plugins;
mod codex_sidebar;
mod project_paths;
mod project_sync_v3;
mod readiness;

#[cfg(test)]
mod sync_tests;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileEntry {
    name: String,
    path: String,
    is_dir: bool,
    size: u64,
    modified: u64,
    children: Option<Vec<FileEntry>>,
    included: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ConfigSource {
    id: String,
    label: String,
    path: String,
    kind: String, // "local" | "cloud"
    entries: Vec<FileEntry>,
}

/// Config schema v2 (PLAN_MULTI_STORAGE.md): N named storages × N local
/// profiles, links as matrix edges. Clean break — `load_sync_config`
/// accepts `schema: 2` only; anything else is treated as unconfigured.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SyncConfig {
    #[serde(default)]
    schema: u32,
    #[serde(default)]
    storages: Vec<StorageConfig>,
    #[serde(default)]
    local_profiles: Vec<LocalProfile>,
    #[serde(default)]
    links: Vec<SyncLink>,
}

pub const CONFIG_SCHEMA_VERSION: u32 = 2;

/// One named sync destination — everything the v1 flat config said about
/// "the" remote, now one of several. A storage is a self-contained universe
/// of cloud profiles; links are purely client-side wiring.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct StorageConfig {
    /// Stable id (`[a-z0-9-]`), referenced by links and by baseline/cache
    /// keys — never re-used across storage entries.
    id: String,
    /// Display name ("Personal", "Team", "Backup / NAS").
    #[serde(default)]
    name: String,
    /// Backend selector: "s3" = S3-compatible bucket, "local" = local
    /// folder (same profile layout, CAS via a lock file). Unknown values
    /// keep failing safe.
    kind: String,
    // S3 / R2
    #[serde(default)]
    bucket: String,
    #[serde(default)]
    access_key_id: String,
    #[serde(default)]
    secret_access_key: String,
    #[serde(default)]
    account_id: String,
    #[serde(default)]
    s3_endpoint: String,
    #[serde(default)]
    region: String,
    /// Local-folder mode: the directory that plays the bucket's role.
    #[serde(default)]
    local_dir: String,
    /// Per-storage opt-ins. Deliberately per-storage, not global: opting a
    /// sensitive optional file into a Personal bucket must not leak it into
    /// a Team bucket. The Never tier stays hard-denied regardless.
    #[serde(default)]
    included_default_exclusions: Vec<String>,
    /// Probed once per storage: whether it honors conditional writes.
    /// false runs the head publish in single-writer (last-writer-wins) mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    supports_conditional_writes: Option<bool>,
}

/// One local agent root this machine syncs. Fresh configs start with
/// `~/.codex` and `~/.claude`; users may remove them or add more at custom
/// paths. Display name is `name` when set, else derived from the path.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct LocalProfile {
    /// Stable id (`[a-z0-9-]`); also the profile's `~/.agent-sync/{id}`
    /// app-record directory, so two same-kind profiles never share locks.
    id: String,
    /// Which agent root this profile holds: ".codex" | ".claude".
    root: String,
    /// "" = `~/{root}`; else a mount with container semantics (a folder not
    /// named after the root hosts it as a subdirectory).
    #[serde(default)]
    path: String,
    /// Optional user-chosen display name; "" = derive from the path. Local
    /// only — the shared name is the cloud profile's (`_tag.json.label`).
    #[serde(default)]
    name: String,
}

/// A matrix edge: this local profile syncs with this storage. Sync state
/// (baseline, cloud cache) is keyed by `(storage, cloud profile)` — see
/// `baseline_path` — so same-named cloud profiles in different storages
/// never cross-talk.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SyncLink {
    /// `LocalProfile.id`.
    profile: String,
    /// `StorageConfig.id`.
    storage: String,
    /// Cloud side: resolved profile prefix + identity, or empty until the
    /// first push/pull discovers or creates one.
    #[serde(default)]
    cloud: ProfileLink,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ProfileLink {
    /// Which agent root this profile holds: ".codex" | ".claude".
    #[serde(default)]
    root: String,
    #[serde(default)]
    profile_id: String,
    #[serde(default)]
    profile_label: String,
    #[serde(default)]
    actor_name: String,
    #[serde(default)]
    machine_name: String,
    /// Sync-link cloud side: the user chose this prefix explicitly. A pinned
    /// prefix that is missing from the store is created at that exact name,
    /// never rediscovered.
    #[serde(default)]
    pinned: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SyncResult {
    success: bool,
    files_synced: usize,
    message: String,
    timestamp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    setup_state: Option<codex_plugins::CodexRepairState>,
}

/// Local baseline record for one path. `sha256` is the logical content applied
/// locally. Most cloud objects contain those exact bytes; an older raw Codex
/// config can project to different logical bytes, in which case
/// `cloud_object_sha256` remembers the raw manifest hash until the next push
/// republishes the portable projection.
#[derive(Serialize, Deserialize, Clone, Debug)]
struct FileRecord {
    sha256: String,
    size: u64,
    mtime: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cloud_object_sha256: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct SyncManifest {
    files: HashMap<String, FileRecord>,
    #[serde(default)]
    last_push: u64,
}

// ── Profile cloud layout (see DESIGN2.md) ───────────────────────────────────
//
// The single authoritative object per profile is `{profile_id}/_head.json`,
// compare-and-swapped on every publish. Manifests, commits, and upload
// batches are immutable and written before the head flips, so a racing or
// crashed push leaves only unpublished orphans — never a published
// generation without its history, and never a half-visible file set.

const CLOUD_SCHEMA_VERSION: u32 = 1;
const HEAD_OBJECT: &str = "_head.json";
const TAG_OBJECT: &str = "_tag.json";

#[derive(Serialize, Deserialize, Clone, Debug)]
struct HeadFile {
    schema_version: u32,
    profile_id: String,
    /// Agent root this profile holds; the auto-link discriminator.
    #[serde(default)]
    root: String,
    state: String,
    generation: u64,
    commit_id: String,
    manifest_key: String,
    commit_key: String,
    manifest_sha256: String,
    updated_at: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct ManifestEntry {
    sha256: String,
    size: u64,
    /// Profile-relative content key: `_uploads/{upload_id}/objects/{sha256}`.
    object_key: String,
    /// Source file's modification time (epoch seconds) at upload scan time,
    /// restored on pull so tools that index by mtime (Codex's thread rebuild)
    /// see real recency. 0 = unknown (entry from an older build): skip restore
    /// (PLAN_CODEX_THREAD_REBUILD_AND_SIDEBAR.md Part A).
    #[serde(default)]
    source_mtime: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct CloudManifest {
    schema_version: u32,
    generation: u64,
    commit_id: String,
    updated_at: u64,
    files: BTreeMap<String, ManifestEntry>,
    /// Durable, narrow deletion intent for explicitly reviewed conflict
    /// siblings. The value is the logical SHA the user resolved. This keeps
    /// resolution effective even when a replica loses its local baseline;
    /// ordinary deletions remain union-restored.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    resolved_conflicts: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct CommitSummary {
    added: u64,
    modified: u64,
    deleted: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct CommitRecord {
    schema_version: u32,
    commit_id: String,
    generation: u64,
    created_at: u64,
    actor_name: String,
    machine_name: String,
    upload_id: String,
    message: String,
    manifest_key: String,
    manifest_sha256: String,
    #[serde(default)]
    previous_commit_key: Option<String>,
    #[serde(default)]
    previous_manifest_sha256: Option<String>,
    summary: CommitSummary,
}

#[derive(Serialize, Clone, Debug)]
pub struct ProfileInfo {
    profile_id: String,
    root: String,
    label: String,
    files: u64,
    generation: u64,
    updated_at: u64,
    last_actor_name: String,
    last_machine_name: String,
}

/// In-memory cache of the last fetched cloud head + manifest, so
/// `get_file_statuses` never hits the network. Push and pull update it;
/// `refresh_cloud_state` refetches on demand.
#[derive(Clone)]
struct CloudStateCache {
    /// `StorageConfig.id` — part of the cache key: same-named profiles in
    /// two storages are unrelated (PLAN_MULTI_STORAGE.md 2b).
    storage_id: String,
    profile_id: String,
    generation: u64,
    commit_id: String,
    fetched_at: u64,
    files: HashMap<String, String>, // validated rel path -> sha256
}

#[derive(Default)]
struct CloudCacheSlot(std::sync::Mutex<HashMap<String, CloudStateCache>>);

/// `(storage, cloud profile)` — the only valid key for per-link sync state.
fn link_state_key(storage_id: &str, profile_id: &str) -> String {
    format!("{}__{}", storage_id, profile_id)
}

#[derive(Serialize, Clone, Debug)]
pub struct CloudState {
    storage: String,
    profile: String,
    root: String,
    profile_label: String,
    generation: u64,
    commit_id: String,
    fetched_at: u64,
    files: u64,
}

fn cache_from_manifest(
    head: &HeadFile,
    manifest_files: &BTreeMap<String, ManifestEntry>,
    storage_id: &str,
    profile_id: &str,
) -> CloudStateCache {
    let mut files = HashMap::new();
    for (path, entry) in manifest_files {
        if let Ok(rel) = validate_cloud_key(path) {
            if !path_or_conflict_shadow_is_never_synced(&rel) {
                files.insert(rel, entry.sha256.clone());
            }
        }
    }
    CloudStateCache {
        storage_id: storage_id.to_string(),
        profile_id: profile_id.to_string(),
        generation: head.generation,
        commit_id: head.commit_id.clone(),
        fetched_at: now_secs(),
        files,
    }
}

fn store_cloud_cache(app: &AppHandle, mut cache: CloudStateCache) {
    cache
        .files
        .retain(|rel, _| !path_or_conflict_shadow_is_never_synced(rel));
    if let Some(slot) = app.try_state::<CloudCacheSlot>() {
        if let Ok(mut guard) = slot.0.lock() {
            guard.insert(link_state_key(&cache.storage_id, &cache.profile_id), cache);
        }
    }
}

fn load_cloud_cache(
    app: &AppHandle,
    storage_id: &str,
    profile_id: &str,
) -> Option<CloudStateCache> {
    let slot = app.try_state::<CloudCacheSlot>()?;
    let mut guard = slot.0.lock().ok()?;
    let cache = guard.get_mut(&link_state_key(storage_id, profile_id))?;
    cache
        .files
        .retain(|rel, _| !path_or_conflict_shadow_is_never_synced(rel));
    Some(cache.clone())
}

fn drop_cloud_cache(app: &AppHandle, storage_id: &str, profile_id: &str) {
    if let Some(slot) = app.try_state::<CloudCacheSlot>() {
        if let Ok(mut guard) = slot.0.lock() {
            guard.remove(&link_state_key(storage_id, profile_id));
        }
    }
}

fn root_display_label(root: &str) -> &'static str {
    match root {
        ".codex" => "Codex",
        ".claude" => "Claude",
        _ => "Agent Config",
    }
}

fn process_is_running(name: &str) -> bool {
    std::process::Command::new("pgrep")
        .args(["-x", name])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Test-only override: mapping/apply tests must not depend on whether the
/// developer's machine happens to run the desktop app.
#[cfg(test)]
pub(crate) static TEST_CODEX_DESKTOP_RUNNING: AtomicBool = AtomicBool::new(false);

/// The desktop bundle has shipped under both names. The current macOS app
/// uses `ChatGPT` even though its bundle identifier is still com.openai.codex.
fn codex_desktop_is_running() -> bool {
    #[cfg(test)]
    {
        TEST_CODEX_DESKTOP_RUNNING.load(Ordering::SeqCst)
    }
    #[cfg(not(test))]
    {
        ["ChatGPT", "Codex"].into_iter().any(process_is_running)
    }
}

#[cfg(test)]
pub(crate) static TEST_CLAUDE_CLI_RUNNING: AtomicBool = AtomicBool::new(false);

/// Conservative guard for Claude alias mutation: any running `claude` CLI
/// refuses the operation — per-profile process attribution is not reliable
/// (PLAN_CLAUDE_PROJECT_PATH_REMAP.md §6).
fn claude_cli_is_running() -> bool {
    #[cfg(test)]
    {
        TEST_CLAUDE_CLI_RUNNING.load(Ordering::SeqCst)
    }
    #[cfg(not(test))]
    {
        process_is_running("claude")
    }
}

/// Best-effort advisory: syncing while an agent is running can lose
/// in-flight state (open files keep old inodes; live files change under
/// the scan). The warning never blocks the sync.
fn warn_if_agents_running(app: &AppHandle) {
    for name in ["codex", "claude"] {
        if process_is_running(name) {
            emit_log(
                app,
                "info",
                &format!(
                    "⚠ {} appears to be running — its live files may change during sync",
                    name
                ),
            );
        }
    }
}

fn random_hex(bytes: usize) -> Result<String, String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|error| format!("random bytes: {}", error))?;
    Ok(buf.iter().map(|b| format!("{:02x}", b)).collect())
}

fn new_profile_id() -> Result<String, String> {
    random_hex(16) // 128 bits; hex never starts with the reserved '_'
}

fn new_commit_id() -> Result<String, String> {
    random_hex(8) // 16 lowercase hex chars, as the key grammar requires
}

fn new_upload_id() -> Result<String, String> {
    random_hex(13)
}

fn history_object_key(dir: &str, generation: u64, commit_id: &str) -> String {
    format!("{}/{:012}-{}.json", dir, generation, commit_id)
}

fn profile_key(profile_id: &str, rest: &str) -> String {
    format!("{}/{}", profile_id, rest)
}

fn default_actor_name() -> String {
    std::env::var("USER")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn default_machine_name() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

const R2_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Test-only override (milliseconds; 0 = disabled) so integration tests can
/// exercise the ambiguous-publish path without a two-minute stall.
#[cfg(test)]
static TEST_REQUEST_TIMEOUT_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn r2_request_timeout() -> Duration {
    #[cfg(test)]
    {
        let ms = TEST_REQUEST_TIMEOUT_MS.load(Ordering::Relaxed);
        if ms > 0 {
            return Duration::from_millis(ms);
        }
    }
    R2_REQUEST_TIMEOUT
}
const SYNC_CONCURRENCY: usize = 80;

// Default-sync allowlist (see AGENT_SYNC_FILE_SETS.md): the Required and
// Optional tiers. A path outside these lists does not sync unless the user
// opts it in per remote (`included_default_exclusions`), and the Never tier
// below is hard-denied even for opt-ins.
const DEFAULT_SYNC_DIRS: &[&str] = &[
    // Codex — required for conversation restore
    ".codex/sessions",
    ".codex/archived_sessions",
    // Codex — behavior and configuration. Global `skills/` is owned by
    // schema-3 project sync (PLAN_V3_GLOBAL_PLUGINS_AND_SKILLS.md): custom
    // skills sync as reviewed directory snapshots there, so legacy profile
    // sync no longer copies them file-by-file.
    ".codex/memories",
    ".codex/rules",
    ".codex/prompts",
    ".codex/agents",
    // Claude — required for conversation restore
    ".claude/projects",
    // Claude — conversation-adjacent
    ".claude/file-history",
    ".claude/todos",
    // Claude — behavior and configuration. `skills/` is schema-3-owned; see
    // the Codex note above.
    ".claude/agents",
    ".claude/commands",
];
const DEFAULT_SYNC_FILES: &[&str] = &[
    ".codex/session_index.jsonl",
    ".codex/history.jsonl",
    ".codex/AGENTS.md",
    ".codex/hooks.json",
    ".codex/config.toml",
    // App-generated portable plugin intent, refreshed before every `.codex`
    // push (see codex_plugins.rs / PLAN_ENVIRONMENT_RECONCILER.md).
    codex_plugins::LOCK_REL,
    // App-generated portable sidebar state (codex_sidebar.rs /
    // PLAN_CODEX_THREAD_REBUILD_AND_SIDEBAR.md Part B).
    codex_sidebar::LOCK_REL,
    ".claude/history.jsonl",
    ".claude/CLAUDE.md",
    ".claude/keybindings.json",
    ".claude/settings.json",
    ".claude/plugins/config.json",
    // App-generated portable plugin intent (PLAN_CLAUDE_PLUGIN_LOCK.md).
    codex_plugins::CLAUDE_LOCK_REL,
];

// Never-sync tier (see AGENT_SYNC_FILE_SETS.md): hard-denied even when a
// default exclusion is opted back in — credentials, machine identity, live
// process metadata, reinstallable clones, and VCS/OS junk anywhere in a tree.
const NEVER_SYNC_FILE_PREFIXES: &[&str] = &[
    ".codex/auth.json",
    ".codex/installation_id",
    // Mixes machine/account identity into UI state; the portable subset
    // travels via the sidebar lock instead (codex_sidebar.rs).
    ".codex/.codex-global-state.json",
    ".claude/.credentials.json",
    ".claude/settings.local.json",
];
const NEVER_SYNC_DIRS: &[&str] = &[
    ".codex/.tmp",
    ".codex/plugins/cache",
    ".claude/sessions",
    ".claude/plugins/repos",
    ".claude/plugins/marketplaces",
];
const NEVER_SYNC_COMPONENTS: &[&str] = &[".git", ".DS_Store"];

fn path_is_never_synced(path: &str) -> bool {
    let path = normalized_relative_path(path);
    // The desktop targets commonly use case-insensitive filesystems. Match
    // fixed safety-boundary names case-insensitively so an opt-in or remote
    // key with different casing cannot address the same credential/cache
    // directory under a second logical spelling.
    let folded = path.to_ascii_lowercase();
    NEVER_SYNC_FILE_PREFIXES
        .iter()
        .any(|prefix| folded.starts_with(&prefix.to_ascii_lowercase()))
        || NEVER_SYNC_DIRS
            .iter()
            .any(|root| path_matches_root(&folded, &root.to_ascii_lowercase()))
        || path.split('/').any(|component| {
            NEVER_SYNC_COMPONENTS
                .iter()
                .any(|blocked| component.eq_ignore_ascii_case(blocked))
        })
}

/// Conflict siblings inherit the policy of the file or directory they shadow,
/// including the hard Never tier. Repeat until stable so a crafted path with
/// more than one valid marker cannot bypass the boundary.
fn path_or_conflict_shadow_is_never_synced(path: &str) -> bool {
    let mut candidate = normalized_relative_path(path);
    if candidate
        .split('/')
        .rev()
        .skip(1)
        .any(|component| strip_conflict_marker(component) != component)
    {
        // Conflict markers are generated for files only. A marker-bearing
        // directory is a lookalike namespace and is denied outright.
        return true;
    }
    loop {
        if path_is_never_synced(&candidate) {
            return true;
        }
        let shadowed = strip_conflict_marker(&candidate);
        if shadowed == candidate {
            return false;
        }
        candidate = shadowed;
    }
}

fn normalized_relative_path(path: &str) -> String {
    path.replace('\\', "/")
        .trim_matches('/')
        .trim_end_matches('/')
        .to_string()
}

fn path_matches_root(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn path_matches_exclusion(path: &str, exclusion: &str) -> bool {
    exclusion.strip_suffix('*').map_or_else(
        || path_matches_root(path, exclusion),
        |prefix| path.starts_with(prefix),
    )
}

fn is_sqlite_database(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("sqlite"))
}

fn is_sqlite_sidecar(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            [".sqlite-wal", ".sqlite-shm", ".sqlite-journal"]
                .iter()
                .any(|suffix| name.ends_with(suffix))
        })
}

/// `name.sync-conflict-<hash8>.ext` siblings inherit the eligibility of the
/// file they shadow, so conflict copies of allowlisted files propagate.
fn strip_conflict_marker(path: &str) -> String {
    const MARKER: &str = ".sync-conflict-";
    // The engine only generates conflict *files*. A lookalike marker in a
    // directory name must not make that directory inherit an allowlisted
    // sibling's policy or expose all descendants to upload.
    let basename_start = path.rfind('/').map_or(0, |slash| slash + 1);
    if let Some(relative_pos) = path[basename_start..].rfind(MARKER) {
        // `conflict_copy_rel` always preserves a non-empty filename stem.
        // Without this check, `.sync-conflict-deadbeefAGENTS.md` would strip
        // to the allowlisted `AGENTS.md` even though it is not a conflict copy.
        if relative_pos == 0 {
            return path.to_string();
        }
        let pos = basename_start + relative_pos;
        let tag_start = pos + MARKER.len();
        let tag_end = tag_start + 8;
        // Work in bytes until the tag has been proven ASCII. A path with
        // seven hex bytes followed by a multibyte UTF-8 character must stay
        // an ordinary filename, not panic while slicing through that codepoint.
        if path
            .as_bytes()
            .get(tag_start..tag_end)
            .is_some_and(|tag| tag.iter().all(u8::is_ascii_hexdigit))
        {
            if let (Some(prefix), Some(suffix)) = (path.get(..pos), path.get(tag_end..)) {
                // Generated names either end after the hash or retain the
                // original extension beginning with '.'. Reject arbitrary
                // trailing text so lookalikes cannot inherit an allowlist.
                if suffix.is_empty() || suffix.starts_with('.') {
                    return format!("{}{}", prefix, suffix);
                }
            }
        }
    }
    path.to_string()
}

fn is_conflict_copy_rel(path: &str) -> bool {
    let path = normalized_relative_path(path);
    let Some(name) = Path::new(&path).file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    const MARKER: &str = ".sync-conflict-";
    let Some(pos) = name.rfind(MARKER) else {
        return false;
    };
    if pos == 0 {
        return false;
    }
    let tag_start = pos + MARKER.len();
    let tag_end = tag_start + 8;
    let Some(tag) = name.as_bytes().get(tag_start..tag_end) else {
        return false;
    };
    if !tag.iter().all(u8::is_ascii_hexdigit) {
        return false;
    }
    name.get(tag_end..)
        .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('.'))
}

fn relative_path_is_included(path: &str, opt_ins: &[String]) -> bool {
    let path = normalized_relative_path(path);
    if is_sqlite_sidecar(Path::new(&path)) {
        return false;
    }
    if path_or_conflict_shadow_is_never_synced(&path) {
        return false;
    }
    let mut base = path.clone();
    loop {
        let stripped = strip_conflict_marker(&base);
        if stripped == base {
            break;
        }
        base = stripped;
    }
    if DEFAULT_SYNC_FILES.contains(&base.as_str())
        || DEFAULT_SYNC_DIRS
            .iter()
            .any(|root| path_matches_root(&base, root))
    {
        return true;
    }
    opt_ins
        .iter()
        .map(|value| normalized_relative_path(value))
        .any(|value| path_matches_exclusion(&base, &value))
}

/// A directory that is not itself included may still contain included paths
/// (`.claude/plugins` holds only `plugins/config.json`); walks must descend
/// into such ancestors instead of pruning them.
fn dir_may_contain_included(rel: &str, opt_ins: &[String]) -> bool {
    let rel = normalized_relative_path(rel);
    if rel.is_empty() {
        return true;
    }
    if path_or_conflict_shadow_is_never_synced(&rel) {
        return false;
    }
    if relative_path_is_included(&rel, opt_ins) {
        return true;
    }
    let prefix = format!("{}/", rel);
    DEFAULT_SYNC_FILES.iter().any(|p| p.starts_with(&prefix))
        || DEFAULT_SYNC_DIRS.iter().any(|p| p.starts_with(&prefix))
        || opt_ins
            .iter()
            .map(|value| normalized_relative_path(value))
            .any(|value| value.starts_with(&prefix))
}

fn path_is_included(path: &Path, roots: &Roots, opt_ins: &[String]) -> bool {
    roots
        .rel(path)
        .is_some_and(|rel| relative_path_is_included(&rel, opt_ins))
}

fn dir_path_may_contain_included(path: &Path, roots: &Roots, opt_ins: &[String]) -> bool {
    roots
        .rel(path)
        .is_some_and(|rel| dir_may_contain_included(&rel, opt_ins))
}

#[derive(Clone)]
struct UploadControl {
    paused: Arc<AtomicBool>,
}

impl Default for UploadControl {
    fn default() -> Self {
        Self {
            paused: Arc::new(AtomicBool::new(false)),
        }
    }
}

async fn wait_if_paused(app: &AppHandle, paused: &Arc<AtomicBool>) {
    let mut logged = false;
    while paused.load(Ordering::SeqCst) {
        if !logged {
            emit_log(app, "info", "⏸  upload paused");
            logged = true;
        }
        sleep(Duration::from_millis(250)).await;
    }
    if logged {
        emit_log(app, "info", "▶  upload resumed");
    }
}

/// One local profile's mount: logical root (".codex" / ".claude") → this
/// profile's physical directory. Cloud manifests, baselines, and the
/// allowlist always speak logical paths; only this mount decides where they
/// live on this machine's disk. Default (empty path) is `~/{root}`.
///
/// One prefix is remapped out of the root: logical `.{root}/agent-sync/**`
/// (the app's own generated records, e.g. plugin locks) lives physically at
/// `~/.agent-sync/{profile id}/**`, never inside the agent root — the root
/// holds only what the agent itself produces (PLAN_GLOBAL_AGENT_SYNC_DIR.md),
/// and keying by profile id keeps two same-kind profiles from sharing locks
/// (PLAN_MULTI_STORAGE.md §4). The default profiles' ids are "codex" and
/// "claude", reproducing the familiar layout. A stale in-root `agent-sync/`
/// file maps to no logical path at all (`rel` → None), so it can never
/// collide with the remapped location's manifest entry.
#[derive(Clone)]
struct Roots {
    home: PathBuf,
    /// This profile's root kind: ".codex" | ".claude".
    root: String,
    /// Resolved mount directory (container semantics).
    dir: PathBuf,
    /// `~/.agent-sync/{profile id}` — app-owned records for this profile.
    remap: PathBuf,
}

fn expand_home_relative_path(path: &str, home: &Path) -> PathBuf {
    if path == "~" {
        home.to_path_buf()
    } else if let Some(relative) = path.strip_prefix("~/") {
        home.join(relative)
    } else {
        PathBuf::from(path)
    }
}

/// Resolve the nearest existing ancestor, then append any not-yet-created
/// suffix. This catches aliases through symlinked containers even when the
/// configured mount itself is a fresh path.
fn prospective_canonical_path(path: &Path) -> Result<PathBuf, String> {
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(format!(
            "configured path '{}' contains '.' or '..'",
            path.display()
        ));
    }
    let mut cursor = path.to_path_buf();
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(&cursor) {
            Ok(_) => {
                let mut resolved = fs::canonicalize(&cursor)
                    .map_err(|error| format!("resolve '{}': {}", cursor.display(), error))?;
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = cursor.file_name().ok_or_else(|| {
                    format!("cannot resolve prospective path '{}'", path.display())
                })?;
                missing.push(name.to_os_string());
                cursor = cursor
                    .parent()
                    .ok_or_else(|| format!("'{}' has no existing ancestor", path.display()))?
                    .to_path_buf();
            }
            Err(error) => {
                return Err(format!("inspect '{}': {}", cursor.display(), error));
            }
        }
    }
}

impl Roots {
    fn for_profile(profile: &LocalProfile) -> Result<Roots, String> {
        let home = dirs::home_dir().ok_or("Cannot find home directory")?;
        Roots::for_profile_with_home(profile, home)
    }

    fn for_profile_with_home(profile: &LocalProfile, home: PathBuf) -> Result<Roots, String> {
        if !ALLOWED_SYNC_ROOTS.contains(&profile.root.as_str()) {
            return Err(format!("unknown root '{}'", profile.root));
        }
        if profile.id.is_empty()
            || !profile
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(format!(
                "profile id must be non-empty [a-z0-9-]: '{}'",
                profile.id
            ));
        }
        let dir = if profile.path.is_empty() {
            home.join(&profile.root)
        } else {
            // The settings UI presents home-relative paths with `~`, and
            // users commonly type the same form. Expand only the current
            // user's home; `~other` remains unsupported and fails below.
            let path = expand_home_relative_path(&profile.path, &home);
            if !path.is_absolute() {
                return Err(format!(
                    "{} folder must be an absolute path or start with ~/: '{}'",
                    root_display_label(&profile.root),
                    profile.path
                ));
            }
            // Container semantics: a folder not itself named after the root
            // hosts the root as a subdirectory — `~/backup` mounts `.claude`
            // at `~/backup/.claude`, so one container can hold both roots
            // and the folder's contents stay self-describing. A folder
            // literally named `.claude`/`.codex` is used as-is (flat).
            if path.file_name().and_then(|n| n.to_str()) == Some(profile.root.as_str()) {
                path
            } else {
                path.join(&profile.root)
            }
        };
        // The app-owned global dir must never be (or contain, or sit inside)
        // an agent root: its contents are remapped logical paths plus the
        // unsyncable machine.json, and a root mounted there would re-enter
        // the namespace the remap exists to keep separate.
        let agent_sync = home.join(".agent-sync");
        let canonical_dir = prospective_canonical_path(&dir)?;
        let canonical_agent_sync = prospective_canonical_path(&agent_sync)?;
        if paths_overlap(&dir, &agent_sync) || paths_overlap(&canonical_dir, &canonical_agent_sync)
        {
            return Err(format!(
                "{} folder must not overlap the app directory '{}'",
                root_display_label(&profile.root),
                agent_sync.display()
            ));
        }
        let remap = agent_sync.join(&profile.id);
        Ok(Roots {
            home,
            root: profile.root.clone(),
            dir,
            remap,
        })
    }

    /// `~/.agent-sync` — physical home of all app-generated records
    /// (per-profile remap dirs plus the top-level machine.json and
    /// local-state.json).
    fn agent_sync(&self) -> PathBuf {
        self.home.join(".agent-sync")
    }

    /// Logical rel → physical path. Unknown first components fall back to
    /// home-relative, preserving pre-Roots behavior for paths that slip
    /// past the allowlist.
    fn abs(&self, rel: &str) -> PathBuf {
        let rel = normalized_relative_path(rel);
        let (root, rest) = match rel.split_once('/') {
            Some((root, rest)) => (root, Some(rest)),
            None => (rel.as_str(), None),
        };
        if root != self.root {
            return self.home.join(&rel);
        }
        // App records live outside the agent root (see struct docs).
        if let Some(rest) = rest {
            if rest == "agent-sync" {
                return self.remap.clone();
            }
            if let Some(tail) = rest.strip_prefix("agent-sync/") {
                return self.remap.join(tail);
            }
        }
        match rest {
            Some(rest) => self.dir.join(rest),
            None => self.dir.clone(),
        }
    }

    /// Physical path → logical rel; None when outside this profile's mount.
    fn rel(&self, path: &Path) -> Option<String> {
        if let Ok(rest) = path.strip_prefix(&self.remap) {
            let rest = rest.to_string_lossy().replace('\\', "/");
            return Some(if rest.is_empty() {
                format!("{}/agent-sync", self.root)
            } else {
                format!("{}/agent-sync/{}", self.root, rest)
            });
        }
        if let Ok(rest) = path.strip_prefix(&self.dir) {
            let rest = rest.to_string_lossy().replace('\\', "/");
            // Legacy in-root agent-sync: the remapped location owns the
            // logical path; a stale copy here is invisible to the engine.
            if rest == "agent-sync" || rest.starts_with("agent-sync/") {
                return None;
            }
            return Some(if rest.is_empty() {
                self.root.clone()
            } else {
                format!("{}/{}", self.root, rest)
            });
        }
        None
    }
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

#[cfg(test)]
impl Roots {
    /// Default ".codex" mount under an arbitrary home (tests).
    fn for_home(home: &Path) -> Roots {
        Roots {
            home: home.to_path_buf(),
            root: ".codex".to_string(),
            dir: home.join(".codex"),
            remap: home.join(".agent-sync").join("codex"),
        }
    }
}

/// `~/.agent-sync/machine.json` — machine-local registry mapping each
/// local profile to its physical directory and per-storage cloud links.
/// Regenerated on config save, push, and pull; `sync_config.json` stays the
/// single configuration authority and this file is never read back. It
/// holds absolute paths and sits outside the remapped subtrees, so
/// `Roots::rel` maps it to no logical path and it can never enter a
/// manifest.
fn write_machine_registry(roots: &Roots, config: &SyncConfig) {
    let entries: serde_json::Map<String, serde_json::Value> = config
        .local_profiles
        .iter()
        .filter_map(|profile| {
            let mount = Roots::for_profile_with_home(profile, roots.home.clone()).ok()?;
            let links: Vec<serde_json::Value> = config
                .links
                .iter()
                .filter(|link| link.profile == profile.id)
                .map(|link| {
                    serde_json::json!({
                        "storage": link.storage,
                        "profile_id": link.cloud.profile_id,
                    })
                })
                .collect();
            Some((
                profile.id.clone(),
                serde_json::json!({
                    "root": profile.root,
                    "local_path": mount.dir.to_string_lossy(),
                    "links": links,
                }),
            ))
        })
        .collect();
    let value = serde_json::json!({ "schema": 2, "profiles": entries });
    let dir = roots.agent_sync();
    // Best-effort: a failed registry write never blocks a sync.
    let _ = (|| -> Result<(), String> {
        let destination = dir.join("machine.json");
        ensure_app_owned_path_has_no_symlinks(
            roots.agent_sync(),
            &destination,
            "machine registry",
        )?;
        fs::create_dir_all(&dir)
            .map_err(|error| format!("create '{}': {}", dir.display(), error))?;
        ensure_app_owned_path_has_no_symlinks(
            roots.agent_sync(),
            &destination,
            "machine registry",
        )?;
        let mut tmp = tempfile::NamedTempFile::new_in(&dir)
            .map_err(|error| format!("create machine registry temp file: {}", error))?;
        tmp.as_file_mut()
            .write_all(format!("{:#}\n", value).as_bytes())
            .map_err(|error| format!("write machine registry: {}", error))?;
        tmp.persist(&destination)
            .map_err(|error| format!("publish machine registry: {}", error.error))?;
        Ok(())
    })();
}

fn is_lock_conflict_sibling_name(lock_file: &str, candidate: &str) -> bool {
    let (stem, extension) = match lock_file.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, Some(extension)),
        _ => (lock_file, None),
    };
    let prefix = format!("{}.sync-conflict-", stem);
    let Some(tag) = candidate.strip_prefix(&prefix) else {
        return false;
    };
    let tag = match extension {
        Some(extension) => {
            let suffix = format!(".{}", extension);
            let Some(tag) = tag.strip_suffix(&suffix) else {
                return false;
            };
            tag
        }
        None => tag,
    };
    tag.len() == 8 && tag.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Best-effort removal of the legacy in-root `agent-sync/` directory. The
/// engine never reads it (`Roots::rel` maps it to None), so this is cleanup
/// only. Deletes just the app-generated lock and its conflict-copy
/// siblings; unknown files survive and keep the directory alive — the app
/// never deletes data it did not generate.
fn remove_stale_in_root_agent_sync(root_dir: &Path, lock_rel: &str) {
    let Some(lock_file) = lock_rel.rsplit('/').next() else {
        return;
    };
    let dir = root_dir.join("agent-sync");
    if !fs::symlink_metadata(&dir).is_ok_and(|metadata| metadata.file_type().is_dir()) {
        return;
    }
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == lock_file || is_lock_conflict_sibling_name(lock_file, name) {
            let _ = fs::remove_file(entry.path());
        }
    }
    let _ = fs::remove_dir(&dir); // fails (kept) unless empty
}

fn collect_upload_files(
    files: &[String],
    roots: &Roots,
    opt_ins: &[String],
) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for path_str in files {
        let path = PathBuf::from(path_str);
        // `Path::is_file` follows a final symlink. A file selected directly
        // in the tree must obey the same no-follow rule as WalkDir below or
        // an allowlisted link could upload bytes from outside the sync root.
        let is_regular_file =
            fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_file());
        if is_regular_file {
            if let Some(rel) = roots.rel(&path) {
                if relative_path_is_included(&rel, opt_ins)
                    && checked_physical_sync_path(roots, &rel).is_ok()
                {
                    out.push((path.clone(), rel));
                }
            }
        } else if path.is_dir() {
            // A configured root itself may intentionally be a symlink, but a
            // selected directory below it may not redirect the walk.
            if roots.rel(&path).is_some_and(|rel| {
                rel != roots.root && checked_physical_sync_path(roots, &rel).is_err()
            }) {
                continue;
            }
            for entry in WalkDir::new(&path)
                .follow_links(false)
                .into_iter()
                .filter_entry(|entry| {
                    if entry.file_type().is_dir() {
                        dir_path_may_contain_included(entry.path(), roots, opt_ins)
                    } else {
                        path_is_included(entry.path(), roots, opt_ins)
                    }
                })
                .filter_map(|e| e.ok())
            {
                if !entry.file_type().is_file() {
                    continue;
                }
                if let Some(rel) = roots.rel(entry.path()) {
                    if relative_path_is_included(&rel, opt_ins)
                        && checked_physical_sync_path(roots, &rel).is_ok()
                    {
                        out.push((entry.path().to_path_buf(), rel));
                    }
                }
            }
        }
    }
    out
}

/// Conflict siblings for a remapped lock live beside the canonical lock in
/// `~/.agent-sync/{codex,claude}`. They are not descendants of the selected
/// agent root, so a later push must carry them explicitly or a conflict made
/// during Pull would remain local-only. Match only names emitted by
/// `conflict_copy_rel` and never follow symlinks.
fn lock_conflict_siblings(lock_path: &Path) -> Vec<PathBuf> {
    let Some(parent) = lock_path.parent() else {
        return Vec::new();
    };
    let Some(file_name) = lock_path.file_name().and_then(|name| name.to_str()) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut siblings = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                return None;
            }
            let name = entry.file_name();
            let name = name.to_str()?;
            is_lock_conflict_sibling_name(file_name, name).then(|| entry.path())
        })
        .collect::<Vec<_>>();
    siblings.sort();
    siblings
}

fn sha256_hex(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest.iter().map(|b| format!("{:02x}", b)).collect()
}

fn sha256_bytes(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    digest.iter().map(|b| format!("{:02x}", b)).collect()
}

fn sqlite_backup_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let source = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("open SQLite '{}': {}", path.display(), error))?;
    source
        .busy_timeout(Duration::from_secs(5))
        .map_err(|error| format!("configure SQLite '{}': {}", path.display(), error))?;

    let temp_dir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let snapshot_path = temp_dir.path().join("snapshot.sqlite");
    let mut destination = Connection::open(&snapshot_path)
        .map_err(|error| format!("create SQLite snapshot: {}", error))?;
    {
        let backup = Backup::new(&source, &mut destination)
            .map_err(|error| format!("start SQLite backup '{}': {}", path.display(), error))?;
        backup
            .run_to_completion(128, Duration::from_millis(10), None)
            .map_err(|error| format!("backup SQLite '{}': {}", path.display(), error))?;
    }
    drop(destination);
    fs::read(&snapshot_path)
        .map_err(|error| format!("read SQLite snapshot '{}': {}", path.display(), error))
}

fn read_upload_data(path: &Path) -> Result<Vec<u8>, String> {
    if is_sqlite_database(path) {
        sqlite_backup_bytes(path)
    } else {
        fs::read(path).map_err(|error| format!("read '{}': {}", path.display(), error))
    }
}

/// Read the logical bytes represented by a synced path. Most paths are their
/// physical file bytes; Codex config artifacts are projected to the portable
/// view so machine-local marketplace and managed MCP state never participates
/// in status, baseline, manifest, or upload comparisons.
fn read_sync_bytes(rel: &str, path: &Path) -> Result<Vec<u8>, String> {
    let physical = read_upload_data(path)?;
    if codex_config::is_config_artifact(rel) {
        codex_config::project_portable_bytes(&physical)
            .map_err(|error| format!("project '{}': {}", path.display(), error))
    } else {
        Ok(physical)
    }
}

fn file_mtime_secs(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn file_record(path: &Path, data: &[u8]) -> FileRecord {
    let mtime = file_mtime_secs(path);
    FileRecord {
        sha256: sha256_bytes(data),
        size: data.len() as u64,
        mtime,
        cloud_object_sha256: None,
    }
}

fn record_cloud_object_sha(mut record: FileRecord, cloud_object_sha256: &str) -> FileRecord {
    if record.sha256 != cloud_object_sha256 {
        record.cloud_object_sha256 = Some(cloud_object_sha256.to_string());
    }
    record
}

fn recorded_cloud_sha(record: &FileRecord) -> &str {
    record
        .cloud_object_sha256
        .as_deref()
        .unwrap_or(&record.sha256)
}

fn cloud_projection_needs_republish(record: &FileRecord) -> bool {
    record.cloud_object_sha256.is_some()
}

fn push_needs_projection_republish(mode: SyncMode, record: &FileRecord) -> bool {
    mode == SyncMode::Push && cloud_projection_needs_republish(record)
}

/// Baselines are scoped per link: relinking to a different profile must
/// not inherit the previous profile's applied-state records.
fn baseline_path(
    app: &AppHandle,
    local_id: &str,
    storage_id: &str,
    profile_id: &str,
) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("baselines");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // Keyed per (local profile, storage, cloud profile): the baseline is
    // per-replica state, so two local roots syncing one cloud profile keep
    // independent baselines — like two machines (PLAN_MULTI_STORAGE.md 2b).
    // Sync-link prefixes contain '/' ("001/.codex"); flatten to one file.
    Ok(dir.join(format!(
        "{}__{}.json",
        local_id,
        link_state_key(storage_id, profile_id).replace('/', "__")
    )))
}

fn load_baseline(
    app: &AppHandle,
    local_id: &str,
    storage_id: &str,
    profile_id: &str,
) -> Result<SyncManifest, String> {
    let p = baseline_path(app, local_id, storage_id, profile_id)?;
    if !p.exists() {
        return Ok(SyncManifest::default());
    }
    let raw = fs::read_to_string(&p).map_err(|e| e.to_string())?;
    let mut manifest: SyncManifest = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let original_len = manifest.files.len();
    purge_never_synced_baseline(&mut manifest);
    if manifest.files.len() != original_len {
        let sanitized = serde_json::to_string(&manifest).map_err(|e| e.to_string())?;
        fs::write(&p, sanitized).map_err(|e| e.to_string())?;
    }
    Ok(manifest)
}

fn purge_never_synced_baseline(manifest: &mut SyncManifest) {
    manifest
        .files
        .retain(|rel, _| !path_or_conflict_shadow_is_never_synced(rel));
}

fn save_baseline(
    app: &AppHandle,
    local_id: &str,
    storage_id: &str,
    profile_id: &str,
    manifest: &SyncManifest,
) -> Result<(), String> {
    let p = baseline_path(app, local_id, storage_id, profile_id)?;
    let mut sanitized = manifest.clone();
    purge_never_synced_baseline(&mut sanitized);
    let raw = serde_json::to_string(&sanitized).map_err(|e| e.to_string())?;
    fs::write(&p, raw).map_err(|e| e.to_string())
}

fn file_status_at_path(path: &Path, rel: &str, manifest: &SyncManifest) -> &'static str {
    if codex_config::is_config_artifact(rel) {
        return match (manifest.files.get(rel), read_sync_bytes(rel, path)) {
            (None, Ok(_)) => "new",
            (Some(record), Ok(data)) if record.sha256 == sha256_bytes(&data) => "synced",
            _ => "modified",
        };
    }
    if is_sqlite_database(path) {
        return match (manifest.files.get(rel), sqlite_backup_bytes(path)) {
            (None, _) => "new",
            (Some(record), Ok(data)) if record.sha256 == sha256_bytes(&data) => "synced",
            _ => "modified",
        };
    }

    let Ok(meta) = fs::metadata(path) else {
        return "new";
    };
    let size = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match manifest.files.get(rel) {
        None => "new",
        Some(record) => {
            if record.size == size && record.mtime == mtime {
                "synced"
            } else {
                match read_sync_bytes(rel, path) {
                    Ok(data) if sha256_bytes(&data) == record.sha256 => "synced",
                    _ => "modified",
                }
            }
        }
    }
}

fn s3_secret_from_config(secret: &str) -> String {
    if secret.starts_with("cfat_") {
        sha256_hex(secret)
    } else {
        secret.to_string()
    }
}

fn build_tree(path: &Path, roots: &Roots, opt_ins: &[String], depth: u8) -> Option<FileEntry> {
    let meta = fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() {
        return None;
    }
    let name = path.file_name()?.to_string_lossy().to_string();

    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let is_dir = meta.is_dir();
    let included = if is_dir {
        // Ancestors of allowlisted paths stay visible and walkable even when
        // the directory itself is not wholly included.
        dir_path_may_contain_included(path, roots, opt_ins)
    } else {
        path_is_included(path, roots, opt_ins)
    };

    if is_dir {
        let children = if depth < 4 && included {
            let mut kids: Vec<FileEntry> = fs::read_dir(path)
                .ok()?
                .filter_map(|e| e.ok())
                .filter_map(|e| build_tree(&e.path(), roots, opt_ins, depth + 1))
                .collect();
            kids.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
            Some(kids)
        } else {
            None // excluded or too deep — don't walk
        };
        Some(FileEntry {
            name,
            path: path.to_string_lossy().to_string(),
            is_dir: true,
            size: 0,
            modified,
            children,
            included,
        })
    } else {
        Some(FileEntry {
            name,
            path: path.to_string_lossy().to_string(),
            is_dir: false,
            size: meta.len(),
            modified,
            children: None,
            included,
        })
    }
}

fn read_source(roots: &Roots, profile: &LocalProfile, opt_ins: &[String]) -> Option<ConfigSource> {
    let dir = &roots.dir;
    // A mount that doesn't exist yet (fresh custom folder before the first
    // pull) still shows up — with no entries — so the root is visible and
    // pullable instead of silently vanishing from the sidebar.
    let mut entries: Vec<FileEntry> = if dir.exists() {
        fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok())
            .filter_map(|e| build_tree(&e.path(), roots, opt_ins, 0))
            .collect()
    } else {
        Vec::new()
    };
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
    Some(ConfigSource {
        id: profile.id.clone(),
        label: local_profile_label(roots, profile),
        path: dir.to_string_lossy().to_string(),
        kind: "local".to_string(),
        entries,
    })
}

/// Display name for a local profile: the user-chosen name when set, else
/// the familiar `~/.codex` for defaults or the real path for custom mounts
/// — the sidebar stays honest about what syncs.
fn local_profile_label(roots: &Roots, profile: &LocalProfile) -> String {
    if !profile.name.trim().is_empty() {
        profile.name.trim().to_string()
    } else if roots.dir == roots.home.join(&profile.root) {
        format!("~/{}", profile.root)
    } else {
        roots.dir.to_string_lossy().to_string()
    }
}

/// The opt-ins a profile's file tree shows: the union across its linked
/// storages — a file that syncs anywhere shows as included. Push itself
/// filters strictly per storage.
fn profile_opt_in_union(config: &SyncConfig, profile_id: &str) -> Vec<String> {
    let mut out: Vec<String> = config
        .links
        .iter()
        .filter(|link| link.profile == profile_id)
        .filter_map(|link| config.storages.iter().find(|s| s.id == link.storage))
        .flat_map(|storage| storage.included_default_exclusions.iter().cloned())
        .collect();
    out.sort();
    out.dedup();
    out
}

#[tauri::command]
async fn list_config_dirs(app: AppHandle) -> Result<Vec<ConfigSource>, String> {
    let config = load_sync_config(&app).unwrap_or_default();
    let sources: Vec<ConfigSource> = config
        .local_profiles
        .iter()
        .filter_map(|profile| {
            let roots = Roots::for_profile(profile).ok()?;
            read_source(&roots, profile, &profile_opt_in_union(&config, &profile.id))
        })
        .collect();

    Ok(sources)
}

// ── File editing ─────────────────────────────────────────────────────────────
//
// The editor writes only existing regular files under the two config roots,
// through a temp-file + rename, guarded by an optimistic lock on the sha256
// the file had when it was opened — a running agent rewriting the file
// mid-edit surfaces as a conflict instead of a silent clobber.

const MAX_EDITABLE_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Serialize, Debug)]
pub struct FileDocument {
    content: String,
    sha256: String,
    editable: bool,
    /// Why the file is read-only, when it is.
    reason: Option<String>,
}

/// A path the UI may read: an existing regular file (never a symlink) under
/// a configured root whose parent resolves inside that root.
fn validate_readable_path(roots: &Roots, path: &str) -> Result<PathBuf, String> {
    let requested = PathBuf::from(path);
    let rel = roots
        .rel(&requested)
        .ok_or_else(|| format!("'{}' is outside the config roots", path))?;
    let rel = validate_cloud_key(&rel)?;
    let meta = fs::symlink_metadata(&requested).map_err(|e| format!("stat '{}': {}", rel, e))?;
    if !meta.is_file() {
        return Err(format!("'{}' is not a regular file", rel));
    }
    // Symlinked ancestors could point the write outside the config root.
    let root = &roots.dir;
    let canonical_root = root
        .canonicalize()
        .map_err(|e| format!("resolve '{}': {}", rel, e))?;
    let canonical_parent = requested
        .parent()
        .ok_or_else(|| format!("'{}' has no parent directory", rel))?
        .canonicalize()
        .map_err(|e| format!("resolve '{}': {}", rel, e))?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err(format!("'{}' resolves outside its config root", rel));
    }
    Ok(requested)
}

/// A readable path the editor may also replace; SQLite remains read-only.
fn validate_editable_path(roots: &Roots, path: &str) -> Result<PathBuf, String> {
    let requested = validate_readable_path(roots, path)?;
    if is_sqlite_database(&requested) || is_sqlite_sidecar(&requested) {
        return Err("SQLite databases cannot be edited as text".to_string());
    }
    Ok(requested)
}

fn read_text_file(roots: &Roots, path: &str) -> Result<FileDocument, String> {
    let readable = validate_readable_path(roots, path)?;
    let data = fs::read(readable).map_err(|e| format!("Cannot read file: {}", e))?;
    let sha256 = sha256_bytes(&data);
    let too_big = data.len() as u64 > MAX_EDITABLE_BYTES;
    let content = String::from_utf8(data)
        .map_err(|_| "Cannot read file: not valid UTF-8 text".to_string())?;
    let (editable, reason) = if too_big {
        (false, Some("file is larger than 5 MB".to_string()))
    } else {
        match validate_editable_path(roots, path) {
            Ok(_) => (true, None),
            Err(error) => (false, Some(error)),
        }
    };
    Ok(FileDocument {
        content,
        sha256,
        editable,
        reason,
    })
}

const CHANGED_ON_DISK: &str =
    "changed on disk since it was opened — reload it, or save again to overwrite";

/// Returns the saved content's sha256 (the editor's next lock token).
fn write_text_file(
    roots: &Roots,
    path: &str,
    content: &str,
    expected_sha256: &str,
) -> Result<String, String> {
    let target = validate_editable_path(roots, path)?;
    if content.len() as u64 > MAX_EDITABLE_BYTES {
        return Err("file is larger than 5 MB".to_string());
    }
    let current = fs::read(&target).map_err(|e| format!("read '{}': {}", path, e))?;
    if sha256_bytes(&current) != expected_sha256 {
        return Err(CHANGED_ON_DISK.to_string());
    }
    let revalidated = validate_editable_path(roots, path)?;
    if revalidated != target {
        return Err(format!("'{}' changed physical mapping while saving", path));
    }
    let parent = target
        .parent()
        .ok_or_else(|| format!("'{}' has no parent directory", path))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| format!("create temporary file for '{}': {}", path, error))?;
    tmp.as_file_mut()
        .write_all(content.as_bytes())
        .map_err(|error| format!("write '{}': {}", path, error))?;
    tmp.persist(&target)
        .map_err(|error| format!("rename into '{}': {}", path, error.error))?;
    Ok(sha256_bytes(content.as_bytes()))
}

/// The mount owning a physical path: the first profile whose `rel` maps it.
/// Mount overlap is rejected at save time, so "first" is unambiguous.
fn roots_for_path(config: &SyncConfig, path: &str) -> Result<Roots, String> {
    let requested = PathBuf::from(path);
    config
        .local_profiles
        .iter()
        .filter_map(|profile| Roots::for_profile(profile).ok())
        .find(|roots| roots.rel(&requested).is_some())
        .ok_or_else(|| format!("'{}' is outside the config roots", path))
}

#[tauri::command]
async fn read_file_content(app: AppHandle, path: String) -> Result<FileDocument, String> {
    let config = load_sync_config(&app).unwrap_or_default();
    let roots = roots_for_path(&config, &path)?;
    read_text_file(&roots, &path)
}

#[tauri::command]
async fn write_file_content(
    app: AppHandle,
    path: String,
    content: String,
    expected_sha256: String,
) -> Result<String, String> {
    let config = load_sync_config(&app).unwrap_or_default();
    let roots = roots_for_path(&config, &path)?;
    write_text_file(&roots, &path, &content, &expected_sha256)
}

fn config_file_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("sync_config.json"))
}

/// A fresh config starts with the two common local profiles. Once saved,
/// either starter may be removed like any other profile.
fn default_sync_config() -> SyncConfig {
    let mut config = SyncConfig {
        schema: CONFIG_SCHEMA_VERSION,
        ..Default::default()
    };
    add_starter_profiles(&mut config);
    config
}

/// Populate only a brand-new config with the common `~/.codex` / `~/.claude`
/// rows. Their ids double as their `~/.agent-sync/{codex,claude}` record dirs.
fn add_starter_profiles(config: &mut SyncConfig) {
    for (id, root) in [("codex", ".codex"), ("claude", ".claude")] {
        if !config.local_profiles.iter().any(|p| p.id == id) {
            config.local_profiles.push(LocalProfile {
                id: id.to_string(),
                root: root.to_string(),
                path: String::new(),
                name: String::new(),
            });
        }
    }
}

fn load_sync_config(app: &AppHandle) -> Result<SyncConfig, String> {
    let p = config_file_path(app)?;
    if !p.exists() {
        return Ok(default_sync_config());
    }
    let raw = fs::read_to_string(&p).map_err(|e| e.to_string())?;
    // Clean break (PLAN_MULTI_STORAGE.md §3.1): only schema 2 parses;
    // anything else — including any pre-v2 file — is unconfigured.
    match serde_json::from_str::<SyncConfig>(&raw) {
        Ok(config) if config.schema == CONFIG_SCHEMA_VERSION => Ok(config),
        _ => Ok(default_sync_config()),
    }
}

#[tauri::command]
async fn get_sync_config(app: AppHandle) -> Result<SyncConfig, String> {
    load_sync_config(&app)
}

fn persist_sync_config(app: &AppHandle, config: &SyncConfig) -> Result<(), String> {
    let p = config_file_path(app)?;
    let raw = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    fs::write(&p, raw).map_err(|e| e.to_string())
}

/// Structural validation for a v2 config: unique well-formed ids, valid
/// mounts (pairwise non-overlapping, none inside `~/.agent-sync` or a
/// local-kind storage directory), links referencing existing rows, and one
/// link per matrix cell. Several links MAY target one cloud profile —
/// baselines are per link, so each behaves like an independent machine.
fn validate_sync_config(config: &SyncConfig) -> Result<(), String> {
    let id_ok = |id: &str| {
        !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    let mut storage_ids = HashSet::new();
    for storage in &config.storages {
        if !id_ok(&storage.id) {
            return Err(format!("storage id must be [a-z0-9-]: '{}'", storage.id));
        }
        if !storage_ids.insert(storage.id.clone()) {
            return Err(format!("duplicate storage id '{}'", storage.id));
        }
        if storage.kind != "s3" && storage.kind != "local" {
            return Err(format!(
                "storage '{}' has unsupported kind '{}'",
                storage_display_name(storage),
                storage.kind
            ));
        }
    }
    let mut profile_ids = HashSet::new();
    let mut mounts: Vec<(String, PathBuf, PathBuf)> = Vec::new();
    for profile in &config.local_profiles {
        if !profile_ids.insert(profile.id.clone()) {
            return Err(format!("duplicate profile id '{}'", profile.id));
        }
        let roots = Roots::for_profile(profile)?;
        let canonical = prospective_canonical_path(&roots.dir)?;
        for (other_id, other_dir, other_canonical) in &mounts {
            if paths_overlap(&roots.dir, other_dir) || paths_overlap(&canonical, other_canonical) {
                return Err(format!(
                    "profile folders must not overlap: '{}' vs '{}' ({})",
                    roots.dir.display(),
                    other_dir.display(),
                    other_id
                ));
            }
        }
        mounts.push((profile.id.clone(), roots.dir, canonical));
    }
    for storage in &config.storages {
        if storage.kind == "local" && !storage.local_dir.is_empty() {
            let store_dir = PathBuf::from(&storage.local_dir);
            for (profile_id, dir, _) in &mounts {
                if paths_overlap(&store_dir, dir) {
                    return Err(format!(
                        "storage '{}' folder overlaps profile '{}' mount '{}' — it would sync into itself",
                        storage_display_name(storage),
                        profile_id,
                        dir.display()
                    ));
                }
            }
        }
    }
    let mut cells = HashSet::new();
    for link in &config.links {
        if !profile_ids.contains(&link.profile) {
            return Err(format!(
                "link references unknown profile '{}'",
                link.profile
            ));
        }
        if !storage_ids.contains(&link.storage) {
            return Err(format!(
                "link references unknown storage '{}'",
                link.storage
            ));
        }
        if !cells.insert((link.profile.clone(), link.storage.clone())) {
            return Err(format!(
                "duplicate link for profile '{}' and storage '{}'",
                link.profile, link.storage
            ));
        }
        if !link.cloud.profile_id.is_empty() {
            validate_profile_id(&link.cloud.profile_id)?;
        }
    }
    Ok(())
}

fn storage_display_name(storage: &StorageConfig) -> String {
    if storage.name.is_empty() {
        storage.id.clone()
    } else {
        storage.name.clone()
    }
}

#[tauri::command]
async fn save_sync_config(app: AppHandle, mut config: SyncConfig) -> Result<(), String> {
    config.schema = CONFIG_SCHEMA_VERSION;
    validate_sync_config(&config)?;
    let saved = load_sync_config(&app).unwrap_or_else(|_| default_sync_config());

    // Per-storage carry-over and cleanups (PLAN_MULTI_STORAGE.md §3.2). The
    // settings UI rebuilds the config object field-by-field; don't let a
    // save silently drop probed capabilities or resolved cloud links — but
    // when a storage's destination identity changes, its per-storage state
    // must not carry over.
    let mut stale_links: Vec<(String, String, String)> = Vec::new();
    for storage in &mut config.storages {
        let Some(prev) = saved.storages.iter().find(|s| s.id == storage.id) else {
            continue;
        };
        if storage_identity(storage) == storage_identity(prev) {
            if storage.supports_conditional_writes.is_none() {
                storage.supports_conditional_writes = prev.supports_conditional_writes;
            }
        } else {
            storage.supports_conditional_writes = None;
            for link in config.links.iter_mut().filter(|l| l.storage == storage.id) {
                // A pinned prefix is user intent and survives; everything
                // resolved (ids, labels) belonged to the old destination.
                link.cloud = if link.cloud.pinned {
                    ProfileLink {
                        root: link.cloud.root.clone(),
                        profile_id: link.cloud.profile_id.clone(),
                        pinned: true,
                        ..Default::default()
                    }
                } else {
                    ProfileLink::default()
                };
            }
            for prev_link in saved.links.iter().filter(|l| l.storage == storage.id) {
                stale_links.push((
                    prev_link.profile.clone(),
                    prev_link.storage.clone(),
                    prev_link.cloud.profile_id.clone(),
                ));
            }
        }
    }
    // Carry resolved cloud state for links the UI round-tripped without it.
    for link in &mut config.links {
        if link.cloud.profile_id.is_empty() && !link.cloud.pinned {
            let identity_unchanged = matches!(
                (
                    config.storages.iter().find(|s| s.id == link.storage),
                    saved.storages.iter().find(|s| s.id == link.storage),
                ),
                (Some(new), Some(old)) if storage_identity(new) == storage_identity(old)
            );
            if identity_unchanged {
                if let Some(prev) = saved
                    .links
                    .iter()
                    .find(|l| l.profile == link.profile && l.storage == link.storage)
                {
                    link.cloud = prev.cloud.clone();
                }
            }
        }
    }
    // Removed links (including links of removed storages/profiles) lose
    // their baseline and cache; so does a surviving cell re-picked to a
    // different cloud profile — its old baseline would otherwise be
    // inherited stale if the user ever picks the old profile again. The
    // cloud profile itself stays recoverable either way.
    for prev_link in &saved.links {
        let current = config
            .links
            .iter()
            .find(|l| l.profile == prev_link.profile && l.storage == prev_link.storage);
        let stale = match current {
            None => true,
            Some(link) => {
                !prev_link.cloud.profile_id.is_empty()
                    && link.cloud.profile_id != prev_link.cloud.profile_id
            }
        };
        if stale {
            stale_links.push((
                prev_link.profile.clone(),
                prev_link.storage.clone(),
                prev_link.cloud.profile_id.clone(),
            ));
        }
    }
    for (local_id, storage_id, cloud_profile_id) in stale_links {
        delete_link_state(&app, &local_id, &storage_id, &cloud_profile_id);
    }

    persist_sync_config(&app, &config)?;
    if let Some(home) = dirs::home_dir() {
        // Any valid mount works for the registry write; it only needs home.
        let anchor = Roots {
            home: home.clone(),
            root: ".codex".to_string(),
            dir: home.join(".codex"),
            remap: home.join(".agent-sync").join("codex"),
        };
        write_machine_registry(&anchor, &config);
    }
    Ok(())
}

/// Identity of one storage destination. Per-storage state (resolved cloud
/// links, the conditional-write probe result, baselines, caches) is only
/// valid for the destination it was created against.
fn storage_identity(storage: &StorageConfig) -> String {
    if storage.kind == "local" {
        format!("local:{}", storage.local_dir)
    } else {
        let endpoint = if !storage.s3_endpoint.is_empty() {
            &storage.s3_endpoint
        } else {
            &storage.account_id
        };
        format!("s3:{}/{}", endpoint, storage.bucket)
    }
}

/// Forget one link's local sync state: baseline file and cloud cache. Never
/// touches the cloud. An empty cloud profile id means the link never
/// resolved — nothing to forget.
fn delete_link_state(app: &AppHandle, local_id: &str, storage_id: &str, cloud_profile_id: &str) {
    if cloud_profile_id.is_empty() {
        return;
    }
    if let Ok(path) = baseline_path(app, local_id, storage_id, cloud_profile_id) {
        let _ = fs::remove_file(path);
    }
    drop_cloud_cache(app, storage_id, cloud_profile_id);
}

// ── S3 helpers ────────────────────────────────────────────────────────────────

fn make_s3_client(config: &StorageConfig) -> Result<S3Client, String> {
    if config.bucket.is_empty() {
        return Err("Bucket is not configured".to_string());
    }
    let access_key = if !config.access_key_id.is_empty() {
        &config.access_key_id
    } else {
        return Err("R2 Access Key ID is not configured".to_string());
    };
    if config.secret_access_key.is_empty() {
        return Err("R2 Secret Access Key is not configured".to_string());
    }
    let secret_access_key = s3_secret_from_config(&config.secret_access_key);
    let endpoint = if !config.s3_endpoint.is_empty() {
        config.s3_endpoint.clone()
    } else if !config.account_id.is_empty() {
        format!("https://{}.r2.cloudflarestorage.com", config.account_id)
    } else {
        return Err("Endpoint is not configured".to_string());
    };

    let creds = Credentials::new(access_key, secret_access_key, None, None, "agent-sync");

    let region = if config.region.is_empty() {
        "auto".to_string()
    } else {
        config.region.clone()
    };

    let s3_config = S3Config::builder()
        .credentials_provider(creds)
        .region(Region::new(region))
        .endpoint_url(&endpoint)
        .force_path_style(true)
        .behavior_version_latest()
        .http_client(
            HttpClientBuilder::new()
                .tls_provider(tls::Provider::Rustls(
                    tls::rustls_provider::CryptoMode::AwsLc,
                ))
                .build_https(),
        )
        .build();

    let client = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        S3Client::from_conf(s3_config)
    }))
    .map_err(|e| {
        if let Some(msg) = e.downcast_ref::<&str>() {
            format!("R2 client creation panicked: {}", msg)
        } else if let Some(msg) = e.downcast_ref::<String>() {
            format!("R2 client creation panicked: {}", msg)
        } else {
            "R2 client creation panicked".to_string()
        }
    })?;
    Ok(client)
}

// ── Union reconciliation ─────────────────────────────────────────────────────
//
// Push and pull share one reconcile pass. For every path, the local scan (L),
// the cloud listing (C), and the local baseline manifest (B — the state this
// machine last pushed or pulled, with the cloud ETag it saw) pick an action:
//
//   local changed only   → upload on push, keep on pull ("local ahead")
//   cloud changed only   → apply the cloud version locally (backed up first)
//   both changed         → fetch and merge: deterministic JSONL drivers for
//                          history/session_index, deterministic conflict-copy
//                          siblings for everything else (local wins the path)
//   deletions            → never propagated: the union restores the file
//
// Push is therefore "download the conflict, resolve locally as a union, then
// publish"; pull applies the same union locally without publishing.

fn normalize_etag(etag: &str) -> String {
    etag.trim_matches('"').to_string()
}

const ALLOWED_SYNC_ROOTS: &[&str] = &[".codex", ".claude"];

const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Cloud manifest paths are untrusted input: reject anything that could
/// escape the allowed roots before it is ever joined onto $HOME.
fn validate_cloud_key(key: &str) -> Result<String, String> {
    if key.contains('\\') || key.contains(':') || key.chars().any(|c| c.is_control()) {
        return Err(format!("unsafe cloud key '{}'", key.escape_debug()));
    }
    if key.starts_with('/') {
        return Err(format!("absolute cloud key '{}'", key));
    }
    let rel = normalized_relative_path(key);
    if rel.is_empty()
        || rel
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(format!("unsafe cloud key '{}'", key));
    }
    for component in rel.split('/') {
        let stem = component.split('.').next().unwrap_or("");
        if WINDOWS_RESERVED_NAMES
            .iter()
            .any(|name| stem.eq_ignore_ascii_case(name))
        {
            return Err(format!("reserved device name in cloud key '{}'", key));
        }
    }
    if !ALLOWED_SYNC_ROOTS
        .iter()
        .any(|root| rel.strip_prefix(root).is_some_and(|s| s.starts_with('/')))
    {
        return Err(format!(
            "cloud key '{}' outside ~/.codex and ~/.claude",
            key
        ));
    }
    Ok(rel)
}

fn ensure_app_owned_path_has_no_symlinks(
    app_root: PathBuf,
    destination: &Path,
    label: &str,
) -> Result<(), String> {
    let below_root = destination.strip_prefix(&app_root).map_err(|_| {
        format!(
            "app path '{}' escapes '{}'",
            destination.display(),
            app_root.display()
        )
    })?;
    let mut current = app_root;
    for component in std::iter::once(None).chain(below_root.components().map(Some)) {
        if let Some(component) = component {
            current.push(component.as_os_str());
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "'{}' traverses symlink '{}' — skipped",
                    label,
                    current.display()
                ))
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "inspect path for '{}': {}: {}",
                    label,
                    current.display(),
                    error
                ))
            }
        }
    }
    Ok(())
}

/// Resolve a logical sync path to its physical destination and reject every
/// existing symlink below the trusted physical root. The root itself may be a
/// configured symlink; only cloud-controlled descendants are forbidden from
/// redirecting reads, writes, or deletes outside that selected location.
fn checked_physical_sync_path(roots: &Roots, rel: &str) -> Result<PathBuf, String> {
    let rel = validate_cloud_key(rel)?;
    let (logical_root, tail) = rel
        .split_once('/')
        .ok_or_else(|| format!("cloud key '{}' has no path below its root", rel))?;
    if logical_root != roots.root {
        return Err(format!(
            "cloud key '{}' is outside this profile's root '{}'",
            rel, roots.root
        ));
    }
    let (physical_root, app_owned_root) = if tail == "agent-sync" || tail.starts_with("agent-sync/")
    {
        (roots.remap.clone(), true)
    } else {
        (roots.dir.clone(), false)
    };
    let destination = roots.abs(&rel);
    // Configured agent roots may intentionally be symlinks, but the remapped
    // app-owned namespace is not user-selected.
    if app_owned_root {
        ensure_app_owned_path_has_no_symlinks(roots.agent_sync(), &destination, &rel)?;
        return Ok(destination);
    }
    let below_root = destination.strip_prefix(&physical_root).map_err(|_| {
        format!(
            "physical path '{}' escapes its configured root '{}'",
            destination.display(),
            physical_root.display()
        )
    })?;
    let mut current = physical_root;
    for component in below_root.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "'{}' traverses symlink '{}' — skipped",
                    rel,
                    current.display()
                ))
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "inspect path for '{}': {}: {}",
                    rel,
                    current.display(),
                    error
                ))
            }
        }
    }
    Ok(destination)
}

fn is_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Content keys in a manifest are equally untrusted: they must match
/// `_uploads/{upload_id}/files/{path}` — the readable snapshot layout, where
/// `{path}` passes the same safe-relative-path rules as manifest paths. The
/// legacy content-addressed `_uploads/{upload_id}/objects/{sha256}` form is
/// still accepted on read.
fn validate_object_key(key: &str) -> Result<(), String> {
    let fail = || format!("unsafe object key '{}'", key.escape_debug());
    let rest = key.strip_prefix("_uploads/").ok_or_else(fail)?;
    let (upload_id, rest) = rest.split_once('/').ok_or_else(fail)?;
    if upload_id.is_empty()
        || upload_id.len() > 64
        || !upload_id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(fail());
    }
    if let Some(path) = rest.strip_prefix("files/") {
        validate_cloud_key(path).map(|_| ()).map_err(|_| fail())
    } else if let Some(sha) = rest.strip_prefix("objects/") {
        if is_lower_hex(sha, 64) {
            Ok(())
        } else {
            Err(fail())
        }
    } else {
        Err(fail())
    }
}

/// Head-referenced keys must match `{dir}/{generation:012}-{commit_id}.json`.
fn validate_history_key(key: &str, dir: &str) -> bool {
    let Some(rest) = key.strip_prefix(dir).and_then(|r| r.strip_prefix('/')) else {
        return false;
    };
    if rest.contains('/') {
        return false;
    }
    let Some(name) = rest.strip_suffix(".json") else {
        return false;
    };
    let Some((generation, commit_id)) = name.split_once('-') else {
        return false;
    };
    generation.len() == 12
        && generation.bytes().all(|b| b.is_ascii_digit())
        && is_lower_hex(commit_id, 16)
}

/// Profile prefixes: auto-generated hex ids and user-chosen sync-link names
/// like "001" or "001/.codex" — one or two `/`-separated segments of
/// `[a-z0-9._-]`. Segments never start with `_` (reserved) and are never
/// `.`/`..`; a leading `.` is otherwise allowed so a second segment can
/// literally be ".codex".
fn validate_profile_id(profile_id: &str) -> Result<(), String> {
    let invalid = || format!("invalid profile id '{}'", profile_id.escape_debug());
    if profile_id.is_empty() || profile_id.len() > 128 {
        return Err(invalid());
    }
    let segments: Vec<&str> = profile_id.split('/').collect();
    if segments.len() > 2 {
        return Err(invalid());
    }
    for segment in segments {
        if segment.is_empty()
            || segment == "."
            || segment == ".."
            || segment.starts_with('_')
            || !segment
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        {
            return Err(invalid());
        }
    }
    Ok(())
}

fn rel_in_scope(rel: &str, scope: Option<&[String]>) -> bool {
    match scope {
        None => true,
        Some(roots) => roots.iter().any(|root| path_matches_root(rel, root)),
    }
}

/// Deterministic sibling name for the losing side of an unmergeable conflict.
/// The name derives from the content hash, so every machine resolving the
/// same pair produces the same file — the resolution is idempotent fleet-wide.
fn conflict_copy_rel(rel: &str, content_sha256: &str) -> String {
    let tag = &content_sha256[..content_sha256.len().min(8)];
    let (dir, name) = match rel.rsplit_once('/') {
        Some((dir, name)) => (Some(dir), name),
        None => (None, rel),
    };
    let renamed = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => {
            format!("{}.sync-conflict-{}.{}", stem, tag, ext)
        }
        _ => format!("{}.sync-conflict-{}", name, tag),
    };
    match dir {
        Some(dir) => format!("{}/{}", dir, renamed),
        None => renamed,
    }
}

// ── Deterministic JSONL merge drivers ────────────────────────────────────────

fn json_line_timestamp(line: &str) -> u64 {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|value| value.get("ts").or_else(|| value.get("timestamp")).cloned())
        .and_then(|ts| ts.as_u64().or_else(|| ts.as_f64().map(|f| f as u64)))
        .unwrap_or(0)
}

/// Two-way union of append-only prompt-history files: dedupe exact lines,
/// order by embedded timestamp (`ts` for codex, `timestamp` for claude), then
/// by line bytes. Byte-deterministic regardless of which side is local, so
/// independent merges on two machines converge.
fn merge_history_jsonl(local: &str, cloud: &str) -> String {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut records: Vec<(u64, &str)> = Vec::new();
    for line in local.lines().chain(cloud.lines()) {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() || !seen.insert(line) {
            continue;
        }
        records.push((json_line_timestamp(line), line));
    }
    records.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    let mut out = records
        .iter()
        .map(|(_, line)| *line)
        .collect::<Vec<_>>()
        .join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Debug)]
enum IndexTimestamp {
    Missing,
    Number(u64),
    Text(String),
}

fn session_index_updated_at(record: &serde_json::Value) -> IndexTimestamp {
    match record.get("updated_at") {
        Some(value) => {
            if let Some(n) = value.as_u64() {
                IndexTimestamp::Number(n)
            } else if let Some(f) = value.as_f64() {
                IndexTimestamp::Number(f as u64)
            } else if let Some(s) = value.as_str() {
                IndexTimestamp::Text(s.to_string())
            } else {
                IndexTimestamp::Missing
            }
        }
        None => IndexTimestamp::Missing,
    }
}

const SESSION_INDEX_CAP: usize = 100;

/// Keyed union of the codex resume-picker index: key by `id`, keep the later
/// `updated_at` (ties break to the lexically greater line), sort ascending,
/// and keep only the newest SESSION_INDEX_CAP records — codex prunes this
/// file itself, and an unbounded union would resurrect pruned entries forever.
fn merge_session_index_jsonl(local: &str, cloud: &str) -> String {
    struct Rec {
        updated_at: IndexTimestamp,
        id: String,
        line: String,
    }
    let mut keyed: HashMap<String, Rec> = HashMap::new();
    let mut unkeyed: Vec<Rec> = Vec::new();
    let mut seen_raw: HashSet<String> = HashSet::new();
    for line in local.lines().chain(cloud.lines()) {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let parsed = serde_json::from_str::<serde_json::Value>(line).ok();
        let id = parsed
            .as_ref()
            .and_then(|v| v.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let updated_at = parsed
            .as_ref()
            .map(session_index_updated_at)
            .unwrap_or(IndexTimestamp::Missing);
        let rec = Rec {
            updated_at,
            id: id.clone(),
            line: line.to_string(),
        };
        if id.is_empty() {
            if seen_raw.insert(rec.line.clone()) {
                unkeyed.push(rec);
            }
            continue;
        }
        let replace = match keyed.get(&id) {
            None => true,
            Some(existing) => match rec.updated_at.cmp(&existing.updated_at) {
                std::cmp::Ordering::Greater => true,
                std::cmp::Ordering::Less => false,
                std::cmp::Ordering::Equal => rec.line > existing.line,
            },
        };
        if replace {
            keyed.insert(id, rec);
        }
    }
    let mut records: Vec<Rec> = keyed.into_values().chain(unkeyed).collect();
    records.sort_by(|a, b| {
        a.updated_at
            .cmp(&b.updated_at)
            .then_with(|| a.id.cmp(&b.id))
            .then_with(|| a.line.cmp(&b.line))
    });
    if records.len() > SESSION_INDEX_CAP {
        let excess = records.len() - SESSION_INDEX_CAP;
        records.drain(..excess);
    }
    let mut out = records
        .iter()
        .map(|r| r.line.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

fn merge_history_driver(local: &str, cloud: &str) -> Option<String> {
    Some(merge_history_jsonl(local, cloud))
}

fn merge_session_index_driver(local: &str, cloud: &str) -> Option<String> {
    Some(merge_session_index_jsonl(local, cloud))
}

fn merge_sidebar_driver(local: &str, cloud: &str) -> Option<String> {
    Some(codex_sidebar::merge_sidebar_lock(local, cloud))
}

fn merge_driver(rel: &str) -> Option<fn(&str, &str) -> Option<String>> {
    match rel {
        ".codex/history.jsonl" | ".claude/history.jsonl" => Some(merge_history_driver),
        ".codex/session_index.jsonl" => Some(merge_session_index_driver),
        codex_plugins::LOCK_REL => Some(codex_plugins::merge_codex_plugin_lock),
        codex_plugins::CLAUDE_LOCK_REL => Some(codex_plugins::merge_claude_plugin_lock),
        codex_sidebar::LOCK_REL => Some(merge_sidebar_driver),
        _ => None,
    }
}

fn is_active_plugin_lock(rel: &str) -> bool {
    matches!(
        rel,
        codex_plugins::LOCK_REL | codex_plugins::CLAUDE_LOCK_REL
    )
}

fn parse_active_plugin_lock(
    rel: &str,
    raw: &[u8],
) -> Result<codex_plugins::CodexPluginLock, String> {
    match rel {
        codex_plugins::CLAUDE_LOCK_REL => codex_plugins::parse_claude_lock_bytes(raw),
        codex_plugins::LOCK_REL => codex_plugins::parse_lock_bytes(raw),
        _ => Err(format!("'{}' is not an active plugin lock", rel)),
    }
}

// ── Cloud access ─────────────────────────────────────────────────────────────

fn sdk_status<E>(err: &SdkError<E>) -> Option<u16> {
    match err {
        SdkError::ServiceError(ctx) => Some(ctx.raw().status().as_u16()),
        SdkError::ResponseError(ctx) => Some(ctx.raw().status().as_u16()),
        _ => None,
    }
}

enum PutCondition {
    Unconditional,
    /// Compare-and-swap against the ETag observed when the object was read.
    IfMatch(String),
    /// Put-if-absent (`If-None-Match: *`) — the profile-creation CAS.
    IfAbsent,
}

enum PutOutcome {
    Written,
    /// The store rejected the precondition: someone else published first.
    PreconditionFailed,
    /// Timed out with the write possibly applied — resolve by re-reading the
    /// object and comparing commit ids.
    Ambiguous,
}

/// Storage backend for the profile cloud layout: an S3-compatible bucket or
/// (with local-folder mode) a plain directory. Same key space, same ETag/CAS
/// semantics either way, so the sync algorithm above this never branches.
#[derive(Clone)]
enum Store {
    S3 { client: S3Client, bucket: String },
    Local { root: PathBuf },
}

/// `mount`: the profile the store is about to sync — a local-folder store
/// must not live inside that mount (or contain it): the sync would ingest
/// its own store artifacts. Cross-profile overlap is checked at save time;
/// this re-checks the active pair at the narrow waist, loudly.
fn make_store(config: &StorageConfig, mount: Option<&Roots>) -> Result<Store, String> {
    if config.kind == "local" {
        if config.local_dir.is_empty() {
            return Err("Local sync folder is not configured".to_string());
        }
        let root = PathBuf::from(&config.local_dir);
        if !root.is_absolute() {
            return Err(format!(
                "Local sync folder must be an absolute path: '{}'",
                config.local_dir
            ));
        }
        if let Some(roots) = mount {
            let canonical_store = prospective_canonical_path(&root)?;
            let canonical_dir = prospective_canonical_path(&roots.dir)?;
            if paths_overlap(&root, &roots.dir) || paths_overlap(&canonical_store, &canonical_dir) {
                return Err(format!(
                    "Local sync folder '{}' overlaps the config root '{}'",
                    root.display(),
                    roots.dir.display()
                ));
            }
        }
        fs::create_dir_all(&root)
            .map_err(|e| format!("create local sync folder '{}': {}", root.display(), e))?;
        return Ok(Store::Local { root });
    }
    Ok(Store::S3 {
        client: make_s3_client(config)?,
        bucket: config.bucket.clone(),
    })
}

/// Test-only interposition point, called with the key before a local
/// conditional put evaluates its precondition — mirrors the stub S3 server's
/// RunBefore hook so head-CAS races are testable on both backends.
#[cfg(test)]
static LOCAL_CAS_HOOK: std::sync::Mutex<Option<Box<dyn FnMut(&str) + Send>>> =
    std::sync::Mutex::new(None);

/// Write via temp file + rename: readers and a crash see either the old or
/// the new content, never a torn file. Stray `.tmp-*` files from a crash are
/// unreferenced orphans, like unpublished upload batches.
fn local_write_atomic(root: &Path, key: &str, data: &[u8]) -> Result<(), String> {
    let path = root.join(key);
    let parent = path
        .parent()
        .ok_or_else(|| format!("invalid key '{}'", key))?;
    fs::create_dir_all(parent).map_err(|e| format!("create '{}': {}", parent.display(), e))?;
    let tmp = parent.join(format!(".tmp-{}", random_hex(8)?));
    fs::write(&tmp, data).map_err(|e| format!("write '{}': {}", key, e))?;
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("publish '{}': {}", key, e)
    })
}

fn local_read_optional(root: &Path, key: &str) -> Result<Option<Vec<u8>>, String> {
    match fs::read(root.join(key)) {
        Ok(data) => Ok(Some(data)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read '{}': {}", key, e)),
    }
}

/// Check-then-write made atomic by an exclusive lock on `<root>/.lock`.
/// Sound across processes on one machine and on lock-honoring network
/// filesystems; folder-sync services (Dropbox etc.) don't propagate locks,
/// where concurrent multi-machine publishes degrade to the service's own
/// conflict handling — losing a generation pointer at worst, never bytes.
fn local_put_conditional(
    root: &Path,
    key: &str,
    data: &[u8],
    condition: &PutCondition,
) -> Result<PutOutcome, String> {
    #[cfg(test)]
    if !matches!(condition, PutCondition::Unconditional) {
        if let Ok(mut hook) = LOCAL_CAS_HOOK.lock() {
            if let Some(callback) = hook.as_mut() {
                callback(key);
            }
        }
    }
    fs::create_dir_all(root).map_err(|e| format!("create '{}': {}", root.display(), e))?;
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(".lock"))
        .map_err(|e| format!("open lock file: {}", e))?;
    lock_file.lock().map_err(|e| format!("lock store: {}", e))?;
    let current = local_read_optional(root, key)?;
    let precondition_holds = match condition {
        PutCondition::Unconditional => true,
        PutCondition::IfMatch(etag) => current
            .as_ref()
            .is_some_and(|bytes| sha256_bytes(bytes) == *etag),
        PutCondition::IfAbsent => current.is_none(),
    };
    if !precondition_holds {
        return Ok(PutOutcome::PreconditionFailed); // lock released on drop
    }
    local_write_atomic(root, key, data)?;
    Ok(PutOutcome::Written)
}

impl Store {
    /// Unconditional PUT; returns the store's ETag when it reports one.
    async fn put(&self, key: &str, data: Vec<u8>) -> Result<Option<String>, String> {
        match self {
            Store::S3 { client, bucket } => {
                let body = ByteStream::from(bytes::Bytes::from(data));
                let request = client
                    .put_object()
                    .bucket(bucket)
                    .key(key)
                    .body(body)
                    .send();
                let resp = timeout(r2_request_timeout(), request)
                    .await
                    .map_err(|_| {
                        format!(
                            "S3 put '{}' timed out after {}s",
                            key,
                            r2_request_timeout().as_secs()
                        )
                    })?
                    .map_err(|e| format!("S3 put '{}': {}", key, e))?;
                Ok(resp.e_tag().map(normalize_etag))
            }
            Store::Local { root } => {
                let etag = sha256_bytes(&data);
                local_write_atomic(root, key, &data)?;
                Ok(Some(etag))
            }
        }
    }

    async fn put_conditional(
        &self,
        key: &str,
        data: Vec<u8>,
        condition: &PutCondition,
    ) -> Result<PutOutcome, String> {
        match self {
            Store::S3 { client, bucket } => {
                let mut request = client
                    .put_object()
                    .bucket(bucket)
                    .key(key)
                    .body(ByteStream::from(bytes::Bytes::from(data)));
                match condition {
                    PutCondition::Unconditional => {}
                    PutCondition::IfMatch(etag) => {
                        request = request.if_match(format!("\"{}\"", etag))
                    }
                    PutCondition::IfAbsent => request = request.if_none_match("*"),
                }
                match timeout(r2_request_timeout(), request.send()).await {
                    Err(_) => Ok(PutOutcome::Ambiguous),
                    Ok(Ok(_)) => Ok(PutOutcome::Written),
                    Ok(Err(err)) => match sdk_status(&err) {
                        // 412 is the standard precondition failure; AWS S3
                        // also returns 409 (ConditionalRequestConflict) for
                        // concurrent conditional writes on the same key.
                        Some(412) | Some(409) => Ok(PutOutcome::PreconditionFailed),
                        _ => Err(format!("S3 put '{}': {}", key, err)),
                    },
                }
            }
            Store::Local { root } => local_put_conditional(root, key, &data, condition),
        }
    }

    /// GET that treats a missing key as data, not an error.
    async fn get_optional(&self, key: &str) -> Result<Option<(Vec<u8>, Option<String>)>, String> {
        match self {
            Store::S3 { client, bucket } => {
                let request = client.get_object().bucket(bucket).key(key).send();
                match timeout(r2_request_timeout(), request).await {
                    Err(_) => Err(format!("get '{}' timed out", key)),
                    Ok(Ok(resp)) => {
                        let etag = resp.e_tag().map(normalize_etag);
                        let data = resp
                            .body
                            .collect()
                            .await
                            .map(|b| b.into_bytes().to_vec())
                            .map_err(|e| format!("body '{}': {}", key, e))?;
                        Ok(Some((data, etag)))
                    }
                    Ok(Err(err)) => {
                        if let SdkError::ServiceError(ctx) = &err {
                            if ctx.err().is_no_such_key() {
                                return Ok(None);
                            }
                        }
                        if sdk_status(&err) == Some(404) {
                            return Ok(None);
                        }
                        Err(format!("S3 get '{}': {}", key, err))
                    }
                }
            }
            Store::Local { root } => Ok(local_read_optional(root, key)?.map(|data| {
                let etag = sha256_bytes(&data);
                (data, Some(etag))
            })),
        }
    }

    async fn get(&self, key: &str) -> Result<(Vec<u8>, Option<String>), String> {
        match self.get_optional(key).await? {
            Some(found) => Ok(found),
            None => Err(format!("get '{}': no such key", key)),
        }
    }

    /// Top-level prefixes of the store, `_`-reserved names skipped.
    async fn list_top_prefixes(&self) -> Result<Vec<String>, String> {
        match self {
            Store::S3 { client, bucket } => {
                let mut prefixes = Vec::new();
                let mut continuation: Option<String> = None;
                loop {
                    let mut request = client.list_objects_v2().bucket(bucket).delimiter("/");
                    if let Some(token) = &continuation {
                        request = request.continuation_token(token);
                    }
                    let resp = timeout(r2_request_timeout(), request.send())
                        .await
                        .map_err(|_| {
                            format!(
                                "S3 list timed out after {}s",
                                r2_request_timeout().as_secs()
                            )
                        })?
                        .map_err(|e| format!("S3 list: {}", e))?;
                    for prefix in resp.common_prefixes() {
                        if let Some(name) = prefix.prefix().and_then(|p| p.strip_suffix('/')) {
                            if !name.starts_with('_') && !name.is_empty() {
                                prefixes.push(name.to_string());
                            }
                        }
                    }
                    continuation = resp.next_continuation_token().map(str::to_string);
                    if continuation.is_none() {
                        break;
                    }
                }
                Ok(prefixes)
            }
            Store::Local { root } => {
                let mut prefixes = Vec::new();
                match fs::read_dir(root) {
                    Ok(entries) => {
                        for entry in entries.flatten() {
                            if !entry.path().is_dir() {
                                continue;
                            }
                            let name = entry.file_name().to_string_lossy().into_owned();
                            // `.`-names are store internals (.lock, .tmp-*).
                            if !name.starts_with('_') && !name.starts_with('.') {
                                prefixes.push(name);
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(format!("list '{}': {}", root.display(), e)),
                }
                Ok(prefixes)
            }
        }
    }

    /// Immediate child prefixes of a top-level prefix ("001" →
    /// ["001/.codex"]), `_`-reserved names skipped. Sync-link namespaces put
    /// profiles one level below the bucket root.
    async fn list_child_prefixes(&self, parent: &str) -> Result<Vec<String>, String> {
        match self {
            Store::S3 { client, bucket } => {
                let mut prefixes = Vec::new();
                let mut continuation: Option<String> = None;
                let want = format!("{}/", parent);
                loop {
                    let mut request = client
                        .list_objects_v2()
                        .bucket(bucket)
                        .prefix(&want)
                        .delimiter("/");
                    if let Some(token) = &continuation {
                        request = request.continuation_token(token);
                    }
                    let resp = timeout(r2_request_timeout(), request.send())
                        .await
                        .map_err(|_| {
                            format!(
                                "S3 list timed out after {}s",
                                r2_request_timeout().as_secs()
                            )
                        })?
                        .map_err(|e| format!("S3 list: {}", e))?;
                    for prefix in resp.common_prefixes() {
                        if let Some(name) = prefix.prefix().and_then(|p| p.strip_suffix('/')) {
                            if let Some(child) = name.strip_prefix(&want) {
                                if !child.is_empty() && !child.starts_with('_') {
                                    prefixes.push(name.to_string());
                                }
                            }
                        }
                    }
                    continuation = resp.next_continuation_token().map(str::to_string);
                    if continuation.is_none() {
                        break;
                    }
                }
                Ok(prefixes)
            }
            Store::Local { root } => {
                let mut prefixes = Vec::new();
                match fs::read_dir(root.join(parent)) {
                    Ok(entries) => {
                        for entry in entries.flatten() {
                            if !entry.path().is_dir() {
                                continue;
                            }
                            let name = entry.file_name().to_string_lossy().into_owned();
                            if !name.starts_with('_') {
                                prefixes.push(format!("{}/{}", parent, name));
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(format!("list '{}': {}", parent, e)),
                }
                Ok(prefixes)
            }
        }
    }

    async fn delete(&self, key: &str) -> Result<(), String> {
        match self {
            Store::S3 { client, bucket } => client
                .delete_object()
                .bucket(bucket)
                .key(key)
                .send()
                .await
                .map(|_| ())
                .map_err(|e| format!("S3 delete '{}': {}", key, e)),
            Store::Local { root } => match fs::remove_file(root.join(key)) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("delete '{}': {}", key, e)),
            },
        }
    }
}

// ── Profile head, discovery, creation ────────────────────────────────────────

async fn fetch_head(
    store: &Store,
    profile_id: &str,
) -> Result<Option<(HeadFile, Option<String>)>, String> {
    let key = profile_key(profile_id, HEAD_OBJECT);
    match store.get_optional(&key).await? {
        None => Ok(None),
        Some((data, etag)) => {
            let head: HeadFile =
                serde_json::from_slice(&data).map_err(|e| format!("parse '{}': {}", key, e))?;
            if !validate_history_key(&head.manifest_key, "_manifests")
                || !validate_history_key(&head.commit_key, "_commits")
            {
                return Err(format!(
                    "profile '{}': head references invalid keys",
                    profile_id
                ));
            }
            Ok(Some((head, etag)))
        }
    }
}

/// Fetch the manifest the head references and verify its bytes against
/// `head.manifest_sha256` — any divergence is detected profile corruption.
async fn fetch_cloud_manifest(
    store: &Store,
    profile_id: &str,
    head: &HeadFile,
) -> Result<CloudManifest, String> {
    let key = profile_key(profile_id, &head.manifest_key);
    let (data, _) = store.get(&key).await?;
    if sha256_bytes(&data) != head.manifest_sha256 {
        return Err(format!(
            "profile corruption: manifest '{}' does not match head.manifest_sha256",
            key
        ));
    }
    serde_json::from_slice(&data).map_err(|e| format!("parse manifest '{}': {}", key, e))
}

/// Prefixes with a readable `_head.json` are profiles; `_tag.json` is only a
/// display cache. One bad profile must not block discovery. Sync-link
/// namespaces ("001/.codex") put profiles one level down, so top-level
/// prefixes without a head get their children probed too (depth ≤ 2).
async fn discover_profiles(store: &Store) -> Result<Vec<ProfileInfo>, String> {
    let mut profiles = Vec::new();
    let mut candidates = store.list_top_prefixes().await?;
    let mut index = 0;
    while index < candidates.len() {
        let prefix = candidates[index].clone();
        index += 1;
        let head = match fetch_head(store, &prefix).await {
            Ok(Some((head, _))) => head,
            Ok(None) | Err(_) => {
                // Not a profile (or unreadable); recurse one level from top.
                if !prefix.contains('/') {
                    if let Ok(children) = store.list_child_prefixes(&prefix).await {
                        candidates.extend(children);
                    }
                }
                continue;
            }
        };
        let tag = store
            .get_optional(&profile_key(&prefix, TAG_OBJECT))
            .await
            .ok()
            .flatten()
            .and_then(|(data, _)| serde_json::from_slice::<serde_json::Value>(&data).ok())
            .unwrap_or(serde_json::Value::Null);
        profiles.push(ProfileInfo {
            profile_id: prefix,
            root: head.root.clone(),
            label: tag["label"]
                .as_str()
                .unwrap_or("(unlabeled profile)")
                .to_string(),
            files: tag["files"].as_u64().unwrap_or(0),
            generation: head.generation,
            updated_at: head.updated_at,
            last_actor_name: tag["last_commit"]["actor_name"]
                .as_str()
                .unwrap_or("")
                .to_string(),
            last_machine_name: tag["last_commit"]["machine_name"]
                .as_str()
                .unwrap_or("")
                .to_string(),
        });
    }
    Ok(profiles)
}

/// Best-effort display cache; failures are logged and ignored. Label
/// precedence (PLAN_PROFILE_NAMES.md): `rename_to` (the pusher's custom
/// profile name) renames the profile for everyone; otherwise an existing
/// tag's non-empty label wins over the caller's cached copy, so a rename
/// from another machine survives this machine's pushes. Returns the label
/// actually written so callers can adopt it.
#[allow(clippy::too_many_arguments)]
async fn write_tag_best_effort(
    app: &AppHandle,
    store: &Store,
    profile_id: &str,
    root: &str,
    label: &str,
    rename_to: Option<&str>,
    commit: &CommitRecord,
    files: u64,
) -> String {
    let label = match rename_to {
        Some(name) => name.to_string(),
        None => store
            .get_optional(&profile_key(profile_id, TAG_OBJECT))
            .await
            .ok()
            .flatten()
            .and_then(|(data, _)| serde_json::from_slice::<serde_json::Value>(&data).ok())
            .and_then(|tag| tag["label"].as_str().map(str::to_string))
            .filter(|existing| !existing.is_empty())
            .unwrap_or_else(|| label.to_string()),
    };
    let tag = serde_json::json!({
        "schema_version": CLOUD_SCHEMA_VERSION,
        "label": label,
        "root": root,
        "updated_at": commit.created_at,
        "generation": commit.generation,
        "files": files,
        "last_commit": {
            "commit_id": commit.commit_id,
            "generation": commit.generation,
            "created_at": commit.created_at,
            "actor_name": commit.actor_name,
            "machine_name": commit.machine_name,
            "message": commit.message,
        },
    });
    let key = profile_key(profile_id, TAG_OBJECT);
    if let Err(error) = store.put(&key, tag.to_string().into_bytes()).await {
        emit_log(
            app,
            "info",
            &format!("tag write failed (non-fatal): {}", error),
        );
    }
    label
}

/// Land a pending rename without publishing: rewrite only the tag's label
/// when it differs. The tag is display data outside the CAS surface, so a
/// no-change push may rewrite it. Best-effort like every tag write.
async fn rename_tag_best_effort(
    app: &AppHandle,
    store: &Store,
    storage_id: &str,
    profile_id: &str,
    head: &HeadFile,
    name: &str,
) {
    let key = profile_key(profile_id, TAG_OBJECT);
    let mut tag = store
        .get_optional(&key)
        .await
        .ok()
        .flatten()
        .and_then(|(data, _)| serde_json::from_slice::<serde_json::Value>(&data).ok())
        .unwrap_or_else(|| {
            // Tag lost or never written: rebuild the display cache from head.
            serde_json::json!({
                "schema_version": CLOUD_SCHEMA_VERSION,
                "root": head.root,
                "updated_at": head.updated_at,
                "generation": head.generation,
            })
        });
    if tag["label"].as_str() != Some(name) {
        tag["label"] = name.into();
        if let Err(error) = store.put(&key, tag.to_string().into_bytes()).await {
            emit_log(
                app,
                "info",
                &format!("tag write failed (non-fatal): {}", error),
            );
            return;
        }
        emit_log(app, "ok", &format!("Renamed cloud profile to '{}'", name));
    }
    adopt_profile_label(app, storage_id, profile_id, name);
}

/// Refresh every saved link caching this cloud profile's label — rename
/// healing (PLAN_PROFILE_NAMES.md). Best-effort like the tag itself.
fn adopt_profile_label(app: &AppHandle, storage_id: &str, cloud_profile_id: &str, label: &str) {
    if label.is_empty() {
        return;
    }
    let Ok(mut saved) = load_sync_config(app) else {
        return;
    };
    let mut changed = false;
    for link in saved.links.iter_mut() {
        if link.storage == storage_id
            && link.cloud.profile_id == cloud_profile_id
            && link.cloud.profile_label != label
        {
            link.cloud.profile_label = label.to_string();
            changed = true;
        }
    }
    if changed {
        let _ = persist_sync_config(app, &saved);
    }
}

/// Probe whether the store honors conditional writes. The negative case must
/// be tested explicitly: some stores accept the headers and silently ignore
/// them, so only a real precondition failure proves support.
async fn ensure_conditional_capability(
    app: &AppHandle,
    store: &Store,
    storage: &StorageConfig,
) -> Result<bool, String> {
    if let Store::Local { .. } = store {
        // The lock-file CAS is always available, and skipping the probe keeps
        // the persisted flag scoped to the S3 storage — a stale `false` from
        // a probed bucket must not downgrade local mode to single-writer.
        return Ok(true);
    }
    if let Some(value) = storage.supports_conditional_writes {
        return Ok(value);
    }
    let mut saved = load_sync_config(app).unwrap_or_else(|_| default_sync_config());
    if let Some(value) = saved
        .storages
        .iter()
        .find(|s| s.id == storage.id)
        .and_then(|s| s.supports_conditional_writes)
    {
        return Ok(value);
    }
    emit_log(app, "info", "Probing remote for conditional-write support…");
    let key = format!("_probe/{}", random_hex(8)?);
    store.put(&key, b"probe".to_vec()).await?;
    let outcome = store
        .put_conditional(
            &key,
            b"probe-stale".to_vec(),
            &PutCondition::IfMatch("0123456789abcdef0123456789abcdef".to_string()),
        )
        .await?;
    let _ = store.delete(&key).await;
    let supported = match outcome {
        PutOutcome::PreconditionFailed => true,
        PutOutcome::Written => false,
        PutOutcome::Ambiguous => {
            return Err("conditional-write probe was inconclusive — try again".to_string())
        }
    };
    emit_log(
        app,
        if supported { "ok" } else { "info" },
        if supported {
            "Remote honors conditional writes — multi-writer publishes enabled"
        } else {
            "⚠ Remote ignores conditional writes — single-writer mode (racing pushes are last-writer-wins)"
        },
    );
    if let Some(entry) = saved.storages.iter_mut().find(|s| s.id == storage.id) {
        entry.supports_conditional_writes = Some(supported);
        persist_sync_config(app, &saved)?;
    }
    Ok(supported)
}

/// Create a profile in the bucket: immutable generation-0 manifest and
/// commit first, then publish `_head.json` with put-if-absent as the
/// creation CAS. A crash before the head write leaves an invisible,
/// harmless prefix. `explicit_id` pins a user-chosen prefix (sync links):
/// one attempt at that exact name, occupied fails loudly.
#[allow(clippy::too_many_arguments)]
async fn create_profile_cloud(
    app: &AppHandle,
    store: &Store,
    root: &str,
    label: &str,
    actor_name: &str,
    machine_name: &str,
    conditional: bool,
    explicit_id: Option<&str>,
) -> Result<ProfileLink, String> {
    let attempts = if explicit_id.is_some() { 1 } else { 3 };
    for _ in 0..attempts {
        let profile_id = match explicit_id {
            Some(id) => {
                validate_profile_id(id)?;
                id.to_string()
            }
            None => new_profile_id()?,
        };
        let commit_id = new_commit_id()?;
        let manifest = CloudManifest {
            schema_version: CLOUD_SCHEMA_VERSION,
            generation: 0,
            commit_id: commit_id.clone(),
            updated_at: now_secs(),
            files: BTreeMap::new(),
            resolved_conflicts: BTreeMap::new(),
        };
        let manifest_bytes = serde_json::to_vec(&manifest).map_err(|e| e.to_string())?;
        let manifest_sha256 = sha256_bytes(&manifest_bytes);
        let manifest_key = history_object_key("_manifests", 0, &commit_id);
        let commit_key = history_object_key("_commits", 0, &commit_id);
        let commit = CommitRecord {
            schema_version: CLOUD_SCHEMA_VERSION,
            commit_id: commit_id.clone(),
            generation: 0,
            created_at: now_secs(),
            actor_name: actor_name.to_string(),
            machine_name: machine_name.to_string(),
            upload_id: String::new(),
            message: "Create profile".to_string(),
            manifest_key: manifest_key.clone(),
            manifest_sha256: manifest_sha256.clone(),
            previous_commit_key: None,
            previous_manifest_sha256: None,
            summary: CommitSummary::default(),
        };
        store
            .put(&profile_key(&profile_id, &manifest_key), manifest_bytes)
            .await?;
        store
            .put(
                &profile_key(&profile_id, &commit_key),
                serde_json::to_vec(&commit).map_err(|e| e.to_string())?,
            )
            .await?;
        let head = HeadFile {
            schema_version: CLOUD_SCHEMA_VERSION,
            profile_id: profile_id.clone(),
            root: root.to_string(),
            state: "active".to_string(),
            generation: 0,
            commit_id: commit_id.clone(),
            manifest_key,
            commit_key,
            manifest_sha256,
            updated_at: now_secs(),
        };
        let head_bytes = serde_json::to_vec(&head).map_err(|e| e.to_string())?;
        let condition = if conditional {
            PutCondition::IfAbsent
        } else {
            // Single-writer mode degrades creation to check-then-write; the
            // random id makes a collision effectively impossible.
            if fetch_head(store, &profile_id).await?.is_some() {
                continue;
            }
            PutCondition::Unconditional
        };
        let outcome = store
            .put_conditional(
                &profile_key(&profile_id, HEAD_OBJECT),
                head_bytes,
                &condition,
            )
            .await?;
        let written = match outcome {
            PutOutcome::Written => true,
            PutOutcome::PreconditionFailed => false, // retried partial creation — regenerate
            PutOutcome::Ambiguous => matches!(
                fetch_head(store, &profile_id).await?,
                Some((current, _)) if current.commit_id == commit_id
            ),
        };
        if !written {
            continue;
        }
        write_tag_best_effort(app, store, &profile_id, root, label, None, &commit, 0).await;
        emit_log(
            app,
            "ok",
            &format!(
                "Created cloud profile '{}' for {} ({})",
                label, root, profile_id
            ),
        );
        return Ok(ProfileLink {
            root: root.to_string(),
            profile_id,
            profile_label: label.to_string(),
            actor_name: actor_name.to_string(),
            machine_name: machine_name.to_string(),
            pinned: explicit_id.is_some(),
        });
    }
    Err(match explicit_id {
        Some(id) => format!(
            "cloud path '{}' already exists with different contents — link it instead",
            id
        ),
        None => "could not create a cloud profile (kept colliding)".to_string(),
    })
}

/// Persist a link's resolved cloud side into the saved config.
fn set_link_cloud(saved: &mut SyncConfig, storage_id: &str, profile_id: &str, cloud: &ProfileLink) {
    if let Some(link) = saved
        .links
        .iter_mut()
        .find(|l| l.profile == profile_id && l.storage == storage_id)
    {
        link.cloud = cloud.clone();
    } else {
        saved.links.push(SyncLink {
            profile: profile_id.to_string(),
            storage: storage_id.to_string(),
            cloud: cloud.clone(),
        });
    }
}

/// A label not colliding with any already in the storage: "Claude",
/// "Claude 2", … — so auto-created same-root profiles stay tellable apart.
fn unique_profile_label(base: &str, existing: &HashSet<String>) -> String {
    if !existing.contains(base) {
        return base.to_string();
    }
    let mut n = 2;
    loop {
        let candidate = format!("{} {}", base, n);
        if !existing.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Resolve one link's cloud profile: use the resolved id when its head still
/// exists, recreate a pinned prefix at its exact name, otherwise auto-link
/// by `head.root` — one candidate links itself, none creates one, several
/// require an explicit choice. Several links (local roots) may share one
/// cloud profile: baselines are per link, so each acts as its own machine.
async fn resolve_profile_for_link(
    app: &AppHandle,
    store: &Store,
    storage: &StorageConfig,
    local: &LocalProfile,
) -> Result<ProfileLink, String> {
    let root = local.root.as_str();
    let saved = load_sync_config(app).unwrap_or_else(|_| default_sync_config());
    let linked = saved
        .links
        .iter()
        .find(|l| l.profile == local.id && l.storage == storage.id)
        .map(|l| l.cloud.clone())
        .filter(|cloud| !cloud.profile_id.is_empty());
    if let Some(link) = linked {
        if let Some((head, _)) = fetch_head(store, &link.profile_id).await? {
            // The prefix must hold this root's namespace, whoever linked it.
            if !head.root.is_empty() && head.root != root {
                return Err(format!(
                    "profile '{}' holds {} — cannot sync it as {}",
                    link.profile_id, head.root, root
                ));
            }
            return Ok(link);
        }
        if link.pinned {
            // Sync-link cloud side: the user named this prefix; create it at
            // that exact name rather than rediscovering by root.
            emit_log(
                app,
                "info",
                &format!(
                    "{}: creating cloud profile at pinned path '{}'",
                    root, link.profile_id
                ),
            );
            let conditional = ensure_conditional_capability(app, store, storage).await?;
            let label = if link.profile_label.is_empty() {
                root_display_label(root).to_string()
            } else {
                link.profile_label.clone()
            };
            let actor = if link.actor_name.is_empty() {
                default_actor_name()
            } else {
                link.actor_name.clone()
            };
            let machine = if link.machine_name.is_empty() {
                default_machine_name()
            } else {
                link.machine_name.clone()
            };
            let created = create_profile_cloud(
                app,
                store,
                root,
                &label,
                &actor,
                &machine,
                conditional,
                Some(&link.profile_id),
            )
            .await?;
            let mut saved = load_sync_config(app).unwrap_or_else(|_| default_sync_config());
            set_link_cloud(&mut saved, &storage.id, &local.id, &created);
            persist_sync_config(app, &saved)?;
            return Ok(created);
        }
        // An unpinned link is only valid for the destination it was created
        // against. After a storage identity change (or an externally deleted
        // profile) fall through to discovery instead of failing every push
        // with "no _head.json"; the persist below replaces the stale link.
        emit_log(
            app,
            "info",
            &format!(
                "{}: linked profile '{}' does not exist in this destination — relinking",
                root, link.profile_id
            ),
        );
    }
    let discovered = discover_profiles(store).await?;
    let labels: HashSet<String> = discovered.iter().map(|p| p.label.clone()).collect();
    let matching: Vec<ProfileInfo> = discovered.into_iter().filter(|p| p.root == root).collect();
    let link = match matching.len() {
        0 => {
            emit_log(
                app,
                "info",
                &format!(
                    "No cloud profile for {} in this storage — creating one",
                    root
                ),
            );
            let conditional = ensure_conditional_capability(app, store, storage).await?;
            create_profile_cloud(
                app,
                store,
                root,
                &unique_profile_label(root_display_label(root), &labels),
                &default_actor_name(),
                &default_machine_name(),
                conditional,
                None,
            )
            .await?
        }
        1 => {
            let info = &matching[0];
            emit_log(
                app,
                "ok",
                &format!(
                    "Linked existing {} profile '{}' ({})",
                    root, info.label, info.profile_id
                ),
            );
            ProfileLink {
                root: root.to_string(),
                profile_id: info.profile_id.clone(),
                profile_label: info.label.clone(),
                actor_name: default_actor_name(),
                machine_name: default_machine_name(),
                pinned: false,
            }
        }
        n => {
            let ids: Vec<String> = matching
                .iter()
                .map(|p| format!("{} ({})", p.label, p.profile_id))
                .collect();
            return Err(format!(
                "{} cloud profiles for {} exist in storage '{}' — pin one explicitly: {}",
                n,
                root,
                storage_display_name(storage),
                ids.join(", ")
            ));
        }
    };
    // Reload before persisting: the capability probe may have just saved.
    let mut saved = load_sync_config(app).unwrap_or_else(|_| default_sync_config());
    set_link_cloud(&mut saved, &storage.id, &local.id, &link);
    persist_sync_config(app, &saved)?;
    Ok(link)
}

// ── Local apply and backups ──────────────────────────────────────────────────

const BACKUP_RUNS_KEPT: usize = 10;

fn backups_root(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("backups");
    if fs::symlink_metadata(&dir).is_ok_and(|metadata| !metadata.file_type().is_dir()) {
        return Err(format!(
            "backup root '{}' is not a real directory",
            dir.display()
        ));
    }
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn create_backup_run(root: &Path) -> Result<PathBuf, String> {
    let prefix = format!("{:020}-", now_secs());
    tempfile::Builder::new()
        .prefix(&prefix)
        .tempdir_in(root)
        .map(|run| run.keep())
        .map_err(|error| format!("create backup run in '{}': {}", root.display(), error))
}

fn prune_backup_runs(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut runs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect();
    runs.sort();
    while runs.len() > BACKUP_RUNS_KEPT {
        let _ = fs::remove_dir_all(runs.remove(0));
    }
}

fn sidecar_path(db: &Path, suffix: &str) -> PathBuf {
    let mut name = db
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    name.push_str(suffix);
    db.with_file_name(name)
}

fn backup_copy_no_follow(
    backup_run: &Path,
    source: &Path,
    destination_rel: &str,
) -> Result<(), String> {
    let destination_rel = validate_cloud_key(destination_rel)?;
    let destination = backup_run.join(&destination_rel);
    let below_run = destination
        .strip_prefix(backup_run)
        .map_err(|_| format!("backup path '{}' escapes its run", destination.display()))?;
    let mut current = backup_run.to_path_buf();
    for component in std::iter::once(None).chain(below_run.components().map(Some)) {
        if let Some(component) = component {
            current.push(component.as_os_str());
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "backup '{}' traverses symlink '{}'",
                    destination_rel,
                    current.display()
                ))
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("inspect backup path: {}", error)),
        }
    }
    let source_metadata = fs::symlink_metadata(source)
        .map_err(|error| format!("inspect backup source '{}': {}", source.display(), error))?;
    if !source_metadata.file_type().is_file() {
        return Err(format!(
            "backup source '{}' is not a regular file",
            source.display()
        ));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| format!("backup '{}' has no parent", destination_rel))?;
    fs::create_dir_all(parent).map_err(|error| format!("create backup parent: {}", error))?;
    // Recheck after directory creation before opening either endpoint.
    let mut current = backup_run.to_path_buf();
    for component in parent
        .strip_prefix(backup_run)
        .unwrap_or(Path::new(""))
        .components()
    {
        current.push(component.as_os_str());
        if fs::symlink_metadata(&current).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(format!(
                "backup '{}' traverses symlink '{}'",
                destination_rel,
                current.display()
            ));
        }
    }
    let mut input = fs::File::open(source)
        .map_err(|error| format!("read backup source '{}': {}", source.display(), error))?;
    let mut output = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&destination)
        .map_err(|error| format!("create backup '{}': {}", destination_rel, error))?;
    std::io::copy(&mut input, &mut output)
        .map_err(|error| format!("backup '{}': {}", destination_rel, error))?;
    Ok(())
}

fn backup_local_file(backup_run: &Path, roots: &Roots, rel: &str) -> Result<(), String> {
    if path_or_conflict_shadow_is_never_synced(rel) {
        return Err(format!("'{}' is in the hard Never tier", rel));
    }
    let source = checked_physical_sync_path(roots, rel)?;
    if !source.exists() {
        return Ok(());
    }
    backup_copy_no_follow(backup_run, &source, rel)?;
    if is_sqlite_database(&source) {
        for suffix in ["-wal", "-shm", "-journal"] {
            let sidecar = sidecar_path(&source, suffix);
            if sidecar.exists() {
                backup_copy_no_follow(backup_run, &sidecar, &format!("{}{}", rel, suffix))?;
            }
        }
    }
    Ok(())
}

/// `source_mtime > 0` restores the manifest's captured modification time
/// after the rename (merge outputs pass 0 — they are genuinely new content;
/// SQLite snapshots are exempt below). Best-effort: a failed restore only
/// costs recency cosmetics, and the returned record stats the file *after*
/// the attempt either way, so the baseline size+mtime fast path stays valid.
#[derive(Debug)]
enum ApplyCloudError {
    ConfigMarketplaceCollision(String),
    Other(String),
}

impl std::fmt::Display for ApplyCloudError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConfigMarketplaceCollision(name) => write!(
                formatter,
                "portable marketplace '{}' conflicts with a target-local registration",
                name
            ),
            Self::Other(message) => formatter.write_str(message),
        }
    }
}

fn apply_cloud_bytes(
    roots: &Roots,
    rel: &str,
    data: &[u8],
    source_mtime: u64,
) -> Result<FileRecord, ApplyCloudError> {
    if path_or_conflict_shadow_is_never_synced(rel) {
        return Err(ApplyCloudError::Other(format!(
            "'{}' is in the hard Never tier",
            rel
        )));
    }
    let dest = checked_physical_sync_path(roots, rel).map_err(ApplyCloudError::Other)?;
    let physical_data = if rel == codex_config::CONFIG_REL {
        let current = if dest.exists() {
            Some(fs::read(&dest).map_err(|error| {
                ApplyCloudError::Other(format!("read current '{}': {}", rel, error))
            })?)
        } else {
            None
        };
        codex_config::compose_physical_bytes(data, current.as_deref()).map_err(
            |error| match error {
                codex_config::ComposePhysicalError::MarketplaceCollision(name) => {
                    ApplyCloudError::ConfigMarketplaceCollision(name)
                }
                codex_config::ComposePhysicalError::Invalid(message) => {
                    ApplyCloudError::Other(format!("compose '{}': {}", rel, message))
                }
            },
        )?
    } else {
        data.to_vec()
    };
    let parent = dest
        .parent()
        .ok_or_else(|| ApplyCloudError::Other(format!("'{}' has no parent directory", rel)))?;
    fs::create_dir_all(parent)
        .map_err(|e| ApplyCloudError::Other(format!("create dir for '{}': {}", rel, e)))?;
    // Keep the already-open, exclusively-created file handle throughout the
    // write. A predictable temp path plus `fs::write` would follow a planted
    // temp symlink before the final rename.
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        ApplyCloudError::Other(format!("create temporary file for '{}': {}", rel, error))
    })?;
    tmp.as_file_mut()
        .write_all(&physical_data)
        .map_err(|error| ApplyCloudError::Other(format!("write '{}': {}", rel, error)))?;
    if is_sqlite_database(&dest) {
        // Replacing a database while its old WAL survives makes SQLite replay
        // stale pages over the new file; drop sidecars in the same step.
        for suffix in ["-wal", "-shm", "-journal"] {
            let sidecar = sidecar_path(&dest, suffix);
            if sidecar.exists() {
                let _ = fs::remove_file(&sidecar);
            }
        }
    }
    tmp.persist(&dest).map_err(|error| {
        ApplyCloudError::Other(format!("rename into '{}': {}", rel, error.error))
    })?;
    if source_mtime > 0 && !is_sqlite_database(&dest) {
        let _ = fs::File::options()
            .append(true)
            .open(&dest)
            .and_then(|file| file.set_modified(UNIX_EPOCH + Duration::from_secs(source_mtime)));
    }
    // The baseline describes the logical cloud object, not the composed
    // physical config containing this target's machine-local overlay.
    Ok(file_record(&dest, data))
}

// ── State classification ─────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
enum LocalState {
    Missing,
    Unchanged,
    Changed,
}

fn local_state_at(path: &Path, rel: &str, manifest: &SyncManifest) -> LocalState {
    if !path.exists() {
        return LocalState::Missing;
    }
    match manifest.files.get(rel) {
        Some(record) if logical_file_matches_record(path, rel, record) => LocalState::Unchanged,
        _ => LocalState::Changed,
    }
}

fn logical_file_matches_record(path: &Path, rel: &str, record: &FileRecord) -> bool {
    let bytes = if is_sqlite_database(path) {
        sqlite_backup_bytes(path)
    } else {
        read_sync_bytes(rel, path)
    };
    bytes.is_ok_and(|bytes| sha256_bytes(&bytes) == record.sha256)
}

/// Full state-matrix label for one path (see DESIGN2.md), computed against
/// the local baseline and the cached cloud manifest — no network.
fn matrix_status(
    path: &Path,
    rel: &str,
    baseline: &SyncManifest,
    cloud_sha: Option<&str>,
) -> &'static str {
    let record = baseline.files.get(rel);
    let local = local_state_at(path, rel, baseline);
    let cloud_changed = match (cloud_sha, record) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(sha), Some(record)) => sha != recorded_cloud_sha(record),
    };
    match (local, cloud_sha) {
        // Gone everywhere; a stale baseline record drops on the next sync.
        (LocalState::Missing, None) => "synced",
        (LocalState::Missing, Some(_)) => {
            if record.is_some() {
                "local-deleted" // union restores it from the cloud
            } else {
                "cloud-only"
            }
        }
        (_, None) => {
            if record.is_some() {
                if is_conflict_copy_rel(rel)
                    && record.is_some_and(|record| logical_file_matches_record(path, rel, record))
                {
                    // Explicit conflict resolution is the one deletion that
                    // propagates: unchanged replicas remove the review copy.
                    "cloud-ahead"
                } else {
                    "cloud-deleted" // union republishes ordinary files
                }
            } else {
                "local-only"
            }
        }
        (LocalState::Unchanged, Some(_)) => {
            if cloud_changed {
                "cloud-ahead"
            } else {
                "synced"
            }
        }
        (LocalState::Changed, Some(sha)) => {
            if !cloud_changed {
                "local-ahead"
            } else {
                // Both moved: converged when the local content already equals
                // the cloud sha, otherwise a union merge is pending.
                let same = read_sync_bytes(rel, path).is_ok_and(|data| sha256_bytes(&data) == sha);
                if same {
                    "converged"
                } else {
                    "conflict"
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum SyncAction {
    /// In sync — nothing to do.
    Skip,
    /// Local is the newer or only side; push publishes it, pull keeps it.
    UploadLocal,
    /// Cloud is the newer or only side; apply it locally.
    ApplyCloud,
    /// Both sides changed since the baseline; fetch cloud bytes and merge.
    Reconcile,
    /// Gone on both sides; forget the baseline entry.
    DropRecord,
}

fn classify_path(
    local: LocalState,
    cloud_sha: Option<&str>,
    record: Option<&FileRecord>,
) -> SyncAction {
    let cloud_changed = match (cloud_sha, record) {
        (None, _) => false,
        (Some(_), None) => true,
        // The baseline records the cloud-side sha as of the last sync, so
        // cloud drift is a direct content comparison.
        (Some(sha), Some(record)) => sha != recorded_cloud_sha(record),
    };
    match (local, cloud_sha) {
        (LocalState::Missing, None) => {
            if record.is_some() {
                SyncAction::DropRecord
            } else {
                SyncAction::Skip
            }
        }
        // Union never propagates deletions: a file deleted locally is
        // restored from the cloud, and one deleted in the cloud is re-pushed.
        (LocalState::Missing, Some(_)) => SyncAction::ApplyCloud,
        (_, None) => SyncAction::UploadLocal,
        (LocalState::Unchanged, Some(_)) => {
            if cloud_changed {
                SyncAction::ApplyCloud
            } else {
                SyncAction::Skip
            }
        }
        (LocalState::Changed, Some(_)) => {
            if cloud_changed {
                SyncAction::Reconcile
            } else {
                SyncAction::UploadLocal
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum SyncMode {
    Push,
    Pull,
}

enum UploadSource {
    LocalFile(PathBuf),
    Bytes(Vec<u8>),
}

struct PendingUpload {
    rel: String,
    source: UploadSource,
}

#[derive(Default)]
struct ReconcileOutcome {
    uploads: Vec<PendingUpload>,
    applied: usize,
    merged: usize,
    conflicts: usize,
    kept_local: usize,
    unchanged: usize,
    errors: Vec<String>,
}

/// Pin the baseline to the cloud side only: the sha records what the cloud
/// holds while mtime 0 disables the stat fast path, so the diverged local
/// file keeps showing as changed (local ahead) until a push publishes it.
fn record_cloud_side(
    manifest: &mut SyncManifest,
    rel: &str,
    logical_cloud_sha: &str,
    cloud_object_sha: &str,
    size: u64,
) {
    manifest.files.insert(
        rel.to_string(),
        FileRecord {
            sha256: logical_cloud_sha.to_string(),
            size,
            mtime: 0,
            cloud_object_sha256: (logical_cloud_sha != cloud_object_sha)
                .then(|| cloud_object_sha.to_string()),
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn apply_cloud_file(
    app: &AppHandle,
    roots: &Roots,
    backup_run: &Path,
    rel: &str,
    data: &[u8],
    cloud_object_sha: &str,
    source_mtime: u64,
    manifest: &mut SyncManifest,
) -> Result<(), ApplyCloudError> {
    backup_local_file(backup_run, roots, rel)
        .map_err(|error| ApplyCloudError::Other(format!("{}: {}", rel, error)))?;
    let record = apply_cloud_bytes(roots, rel, data, source_mtime)?;
    manifest.files.insert(
        rel.to_string(),
        record_cloud_object_sha(record, cloud_object_sha),
    );
    emit_log(app, "ok", &format!("✓  {}", rel));
    Ok(())
}

/// Materialize a deterministic conflict sibling without ever replacing a
/// different local review copy that already occupies the same hash-derived
/// name. Re-running reconciliation with the same cloud bytes is idempotent;
/// an edited sibling must be resolved explicitly before that name is reused.
fn apply_conflict_copy_bytes(
    roots: &Roots,
    rel: &str,
    data: &[u8],
    source_mtime: u64,
) -> Result<(), ApplyCloudError> {
    let path = checked_physical_sync_path(roots, rel).map_err(ApplyCloudError::Other)?;
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            let existing = read_sync_bytes(rel, &path).map_err(|error| {
                ApplyCloudError::Other(format!("read existing conflict copy '{}': {}", rel, error))
            })?;
            if sha256_bytes(&existing) == sha256_bytes(data) {
                return Ok(());
            }
            return Err(ApplyCloudError::Other(format!(
                "existing conflict copy '{}' differs; resolve it before reusing that review path",
                rel
            )));
        }
        Ok(_) => {
            return Err(ApplyCloudError::Other(format!(
                "existing conflict copy '{}' is not a regular file",
                rel
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(ApplyCloudError::Other(format!(
                "inspect existing conflict copy '{}': {}",
                rel, error
            )));
        }
    }
    apply_cloud_bytes(roots, rel, data, source_mtime).map(|_| ())
}

fn preserve_cloud_config_conflict(
    roots: &Roots,
    rel: &str,
    data: &[u8],
    logical_cloud_sha: &str,
    cloud_object_sha: &str,
    source_mtime: u64,
    manifest: &mut SyncManifest,
) -> Result<String, ApplyCloudError> {
    let copy_rel = conflict_copy_rel(rel, logical_cloud_sha);
    apply_conflict_copy_bytes(roots, &copy_rel, data, source_mtime)?;
    record_cloud_side(
        manifest,
        rel,
        logical_cloud_sha,
        cloud_object_sha,
        data.len() as u64,
    );
    Ok(copy_rel)
}

#[allow(clippy::too_many_arguments)]
fn resolve_cloud_bytes(
    app: &AppHandle,
    roots: &Roots,
    backup_run: &Path,
    mode: SyncMode,
    _action: SyncAction,
    rel: &str,
    data: Vec<u8>,
    cloud_object_sha: &str,
    cloud_needs_republish: bool,
    source_mtime: u64,
    manifest: &mut SyncManifest,
    outcome: &mut ReconcileOutcome,
) {
    let path = match checked_physical_sync_path(roots, rel) {
        Ok(path) => path,
        Err(error) => {
            emit_log(app, "error", &format!("✗  {}", error));
            outcome.errors.push(error);
            return;
        }
    };
    let cloud_sha = sha256_bytes(&data);
    // The cloud GET may take long enough for an agent or editor to change the
    // local file after the first classification pass. Snapshot logical bytes
    // once, then classify and reconcile that same buffer; stale ApplyCloud
    // must become a lossless Reconcile and a second read cannot race it.
    let local_data = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => match read_sync_bytes(rel, &path) {
            Ok(bytes) => Some(bytes),
            Err(error) => {
                emit_log(
                    app,
                    "error",
                    &format!("✗  {} unreadable — skipped: {}", rel, error),
                );
                outcome.errors.push(error);
                return;
            }
        },
        Ok(_) => {
            let error = format!("'{}' is not a regular file", rel);
            emit_log(app, "error", &format!("✗  {}", error));
            outcome.errors.push(error);
            return;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            let error = format!("inspect local '{}': {}", rel, error);
            emit_log(app, "error", &format!("✗  {}", error));
            outcome.errors.push(error);
            return;
        }
    };
    let fresh_local = match (&local_data, manifest.files.get(rel)) {
        (None, _) => LocalState::Missing,
        (Some(local), Some(record)) if sha256_bytes(local) == record.sha256 => {
            LocalState::Unchanged
        }
        (Some(_), _) => LocalState::Changed,
    };
    let action = classify_path(fresh_local, Some(cloud_object_sha), manifest.files.get(rel));

    // A cloud-only/newer canonical plugin lock is executable restore intent,
    // not an opaque file. Validate it before replacing the last-good local
    // lock. Reconcile conflicts intentionally bypass this guard because the
    // generic conflict path quarantines the untrusted side in a sibling
    // rather than activating it.
    if action == SyncAction::ApplyCloud && is_active_plugin_lock(rel) {
        if let Err(error) = parse_active_plugin_lock(rel, &data) {
            let message = format!("{} rejected before apply: {}", rel, error);
            emit_log(app, "error", &format!("✗  {}", message));
            outcome.errors.push(message);
            return;
        }
    }

    // Converged: both sides independently hold the same content.
    if (!is_active_plugin_lock(rel) || parse_active_plugin_lock(rel, &data).is_ok())
        && local_data
            .as_ref()
            .is_some_and(|local| sha256_bytes(local) == cloud_sha)
    {
        let record = record_cloud_object_sha(file_record(&path, &data), cloud_object_sha);
        manifest.files.insert(rel.to_string(), record);
        if mode == SyncMode::Push && cloud_needs_republish {
            outcome.uploads.push(PendingUpload {
                rel: rel.to_string(),
                source: UploadSource::Bytes(data),
            });
        }
        outcome.unchanged += 1;
        return;
    }

    let Some(local_bytes) = local_data else {
        // Cloud-only, or deleted locally: the union restores the file.
        match apply_cloud_file(
            app,
            roots,
            backup_run,
            rel,
            &data,
            cloud_object_sha,
            source_mtime,
            manifest,
        ) {
            Ok(()) => {
                outcome.applied += 1;
                if mode == SyncMode::Push && cloud_needs_republish {
                    outcome.uploads.push(PendingUpload {
                        rel: rel.to_string(),
                        source: UploadSource::Bytes(data),
                    });
                }
            }
            Err(error) => {
                emit_log(app, "error", &format!("✗  {}", error));
                outcome.errors.push(error.to_string());
            }
        }
        return;
    };

    if action == SyncAction::UploadLocal {
        if is_active_plugin_lock(rel) {
            if let Err(error) = parse_active_plugin_lock(rel, &local_bytes) {
                let message = format!("{} rejected after concurrent local change: {}", rel, error);
                emit_log(app, "error", &format!("✗  {}", message));
                outcome.errors.push(message);
                return;
            }
        }
        match mode {
            SyncMode::Push => outcome.uploads.push(PendingUpload {
                rel: rel.to_string(),
                source: UploadSource::Bytes(local_bytes),
            }),
            SyncMode::Pull => outcome.kept_local += 1,
        }
        return;
    }

    // A Tier-2 merge failure must never let malformed/future local bytes win
    // the canonical plugin-lock path. If the cloud side is valid, quarantine
    // the invalid local bytes as a review sibling and activate the cloud lock.
    // If neither side is valid, fail closed without publishing either one as
    // executable restore intent. A valid local + invalid cloud pair continues
    // to the generic conflict path below, which keeps the valid local active.
    if action == SyncAction::Reconcile && is_active_plugin_lock(rel) {
        let local_valid = parse_active_plugin_lock(rel, &local_bytes);
        let cloud_valid = parse_active_plugin_lock(rel, &data);
        match (&local_valid, &cloud_valid) {
            (Err(local_error), Ok(_)) => {
                let local_sha = sha256_bytes(&local_bytes);
                let copy_rel = conflict_copy_rel(rel, &local_sha);
                if let Err(error) = apply_conflict_copy_bytes(roots, &copy_rel, &local_bytes, 0) {
                    let message = format!(
                        "{} invalid local lock could not be quarantined: {}",
                        rel, error
                    );
                    emit_log(app, "error", &format!("✗  {}", message));
                    outcome.errors.push(message);
                    return;
                }
                match apply_cloud_file(
                    app,
                    roots,
                    backup_run,
                    rel,
                    &data,
                    cloud_object_sha,
                    source_mtime,
                    manifest,
                ) {
                    Ok(()) => {
                        emit_log(
                            app,
                            "info",
                            &format!(
                                "⚡  {} — activated valid cloud lock; invalid local copy at {} ({})",
                                rel, copy_rel, local_error
                            ),
                        );
                        outcome.conflicts += 1;
                        if mode == SyncMode::Push {
                            outcome.uploads.push(PendingUpload {
                                rel: copy_rel,
                                source: UploadSource::Bytes(local_bytes),
                            });
                            if cloud_needs_republish {
                                outcome.uploads.push(PendingUpload {
                                    rel: rel.to_string(),
                                    source: UploadSource::Bytes(data),
                                });
                            }
                        }
                    }
                    Err(error) => {
                        emit_log(app, "error", &format!("✗  {}", error));
                        outcome.errors.push(error.to_string());
                    }
                }
                return;
            }
            (Err(local_error), Err(cloud_error)) => {
                let message = format!(
                    "{} has invalid local and cloud variants; neither was activated or published (local: {}; cloud: {})",
                    rel, local_error, cloud_error
                );
                emit_log(app, "error", &format!("✗  {}", message));
                outcome.errors.push(message);
                return;
            }
            _ => {}
        }
    }

    if action == SyncAction::ApplyCloud {
        // Only the cloud side moved since the baseline.
        match apply_cloud_file(
            app,
            roots,
            backup_run,
            rel,
            &data,
            cloud_object_sha,
            source_mtime,
            manifest,
        ) {
            Ok(()) => {
                outcome.applied += 1;
                if mode == SyncMode::Push && cloud_needs_republish {
                    outcome.uploads.push(PendingUpload {
                        rel: rel.to_string(),
                        source: UploadSource::Bytes(data),
                    });
                }
            }
            Err(ApplyCloudError::ConfigMarketplaceCollision(name))
                if rel == codex_config::CONFIG_REL =>
            {
                match preserve_cloud_config_conflict(
                    roots,
                    rel,
                    &data,
                    &cloud_sha,
                    cloud_object_sha,
                    source_mtime,
                    manifest,
                ) {
                    Ok(copy_rel) => {
                        emit_log(
                            app,
                            "info",
                            &format!(
                                "⚡  {} — kept target-local marketplace '{}', cloud copy at {}",
                                rel, name, copy_rel
                            ),
                        );
                        outcome.conflicts += 1;
                        if mode == SyncMode::Push {
                            outcome.uploads.push(PendingUpload {
                                rel: rel.to_string(),
                                source: UploadSource::Bytes(local_bytes),
                            });
                            outcome.uploads.push(PendingUpload {
                                rel: copy_rel,
                                source: UploadSource::Bytes(data),
                            });
                        }
                    }
                    Err(error) => {
                        emit_log(app, "error", &format!("✗  {}", error));
                        outcome.errors.push(error.to_string());
                    }
                }
            }
            Err(error) => {
                emit_log(app, "error", &format!("✗  {}", error));
                outcome.errors.push(error.to_string());
            }
        }
        return;
    }

    // Both sides changed. Merge with a deterministic driver when one exists.
    if let Some(driver) = merge_driver(rel) {
        if let (Ok(local_text), Ok(cloud_text)) = (
            std::str::from_utf8(&local_bytes),
            std::str::from_utf8(&data),
        ) {
            if let Some(merged_text) = driver(local_text, cloud_text) {
                let merged_bytes = merged_text.into_bytes();
                if merged_bytes == data {
                    // The cloud copy already contains the union.
                    match apply_cloud_file(
                        app,
                        roots,
                        backup_run,
                        rel,
                        &data,
                        cloud_object_sha,
                        source_mtime,
                        manifest,
                    ) {
                        Ok(()) => {
                            outcome.merged += 1;
                            if mode == SyncMode::Push && cloud_needs_republish {
                                outcome.uploads.push(PendingUpload {
                                    rel: rel.to_string(),
                                    source: UploadSource::Bytes(data),
                                });
                            }
                        }
                        Err(error) => {
                            emit_log(app, "error", &format!("✗  {}", error));
                            outcome.errors.push(error.to_string());
                        }
                    }
                } else if merged_bytes == local_bytes {
                    // The local copy already contains the union.
                    record_cloud_side(
                        manifest,
                        rel,
                        &cloud_sha,
                        cloud_object_sha,
                        data.len() as u64,
                    );
                    outcome.merged += 1;
                    if mode == SyncMode::Push {
                        outcome.uploads.push(PendingUpload {
                            rel: rel.to_string(),
                            source: UploadSource::Bytes(local_bytes),
                        });
                    }
                } else {
                    if let Err(error) = backup_local_file(backup_run, roots, rel) {
                        emit_log(app, "error", &format!("✗  {}: {}", rel, error));
                        outcome.errors.push(error);
                        return;
                    }
                    // Merged output is new content produced now — no mtime restore.
                    match apply_cloud_bytes(roots, rel, &merged_bytes, 0) {
                        Ok(_) => {
                            emit_log(app, "ok", &format!("⇄  {} (merged local + cloud)", rel));
                            record_cloud_side(
                                manifest,
                                rel,
                                &cloud_sha,
                                cloud_object_sha,
                                data.len() as u64,
                            );
                            outcome.merged += 1;
                            if mode == SyncMode::Push {
                                outcome.uploads.push(PendingUpload {
                                    rel: rel.to_string(),
                                    source: UploadSource::Bytes(merged_bytes),
                                });
                            }
                        }
                        Err(error) => {
                            emit_log(app, "error", &format!("✗  {}", error));
                            outcome.errors.push(error.to_string());
                        }
                    }
                }
                return;
            }
        }
    }

    // No driver: local wins the path; the cloud version is preserved as a
    // deterministic conflict sibling so nothing is lost on either side.
    let copy_rel = conflict_copy_rel(rel, &cloud_sha);
    // The sibling holds the logical cloud version (portable for config
    // artifacts); its source mtime applies.
    match apply_conflict_copy_bytes(roots, &copy_rel, &data, source_mtime) {
        Ok(()) => {
            emit_log(
                app,
                "info",
                &format!("⚡  {} — kept local, cloud copy at {}", rel, copy_rel),
            );
            record_cloud_side(
                manifest,
                rel,
                &cloud_sha,
                cloud_object_sha,
                data.len() as u64,
            );
            outcome.conflicts += 1;
            if mode == SyncMode::Push {
                outcome.uploads.push(PendingUpload {
                    rel: rel.to_string(),
                    source: UploadSource::Bytes(local_bytes),
                });
                outcome.uploads.push(PendingUpload {
                    rel: copy_rel,
                    source: UploadSource::Bytes(data),
                });
            }
        }
        Err(error) => {
            emit_log(app, "error", &format!("✗  {}", error));
            outcome.errors.push(error.to_string());
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn reconcile_with_cloud(
    app: &AppHandle,
    store: &Store,
    opt_ins: &[String],
    roots: &Roots,
    profile_id: &str,
    cloud_manifest: &CloudManifest,
    local_files: &[(PathBuf, String)],
    scope: Option<&[String]>,
    manifest: &mut SyncManifest,
    mode: SyncMode,
) -> Result<ReconcileOutcome, String> {
    // Old app versions could persist opt-in plugin caches in the baseline.
    // Drop them before they can participate in classification or be saved.
    purge_never_synced_baseline(manifest);
    let mut cloud: HashMap<String, ManifestEntry> = HashMap::new();
    let mut lower_seen: HashMap<String, String> = HashMap::new();
    let mut collisions: HashSet<String> = HashSet::new();
    let mut live_cloud_casefolded: HashSet<String> = HashSet::new();
    let mut working_cloud: BTreeMap<String, ManifestEntry> = BTreeMap::new();
    for (path, entry) in &cloud_manifest.files {
        let rel = match validate_cloud_key(path) {
            Ok(rel) => rel,
            Err(error) => {
                emit_log(app, "error", &format!("Skipping cloud entry: {}", error));
                continue;
            }
        };
        if let Err(error) = validate_object_key(&entry.object_key) {
            emit_log(
                app,
                "error",
                &format!("Skipping cloud entry '{}': {}", rel, error),
            );
            continue;
        }
        if !relative_path_is_included(&rel, opt_ins) || !rel_in_scope(&rel, scope) {
            continue;
        }
        working_cloud.insert(rel.clone(), entry.clone());
        live_cloud_casefolded.insert(rel.to_lowercase());
        if let Some(previous) = lower_seen.insert(rel.to_lowercase(), rel.clone()) {
            if previous != rel {
                collisions.insert(previous);
                collisions.insert(rel.clone());
            }
        }
        cloud.insert(rel, entry.clone());
    }
    validate_casefold_unique_manifest(&working_cloud)?;
    for rel in &collisions {
        cloud.remove(rel);
        emit_log(
            app,
            "error",
            &format!("Skipping '{}': case-colliding cloud paths", rel),
        );
    }
    let mut resolved_conflicts: HashMap<String, String> = HashMap::new();
    let mut resolved_casefolded: HashMap<String, String> = HashMap::new();
    let mut resolved_collisions: HashSet<String> = HashSet::new();
    for (path, logical_sha) in &cloud_manifest.resolved_conflicts {
        let Ok(rel) = validate_cloud_key(path) else {
            emit_log(
                app,
                "error",
                &format!("Skipping unsafe resolved-conflict path '{}'", path),
            );
            continue;
        };
        let folded = rel.to_lowercase();
        if live_cloud_casefolded.contains(&folded) {
            // A live file always wins over a stale tombstone, including on
            // the case-insensitive desktop filesystems we target.
            emit_log(
                app,
                "error",
                &format!(
                    "Skipping resolved-conflict record '{}' because a live cloud file aliases it",
                    path
                ),
            );
            continue;
        }
        if resolved_collisions.contains(&folded) {
            continue;
        }
        if let Some(previous) = resolved_casefolded.insert(folded.clone(), rel.clone()) {
            if previous != rel {
                resolved_conflicts.remove(&previous);
                resolved_collisions.insert(folded);
                emit_log(
                    app,
                    "error",
                    &format!(
                        "Skipping case-colliding resolved-conflict paths '{}' and '{}'",
                        previous, rel
                    ),
                );
                continue;
            }
        }
        if is_conflict_copy_rel(&rel)
            && is_lower_hex(logical_sha, 64)
            && relative_path_is_included(&rel, opt_ins)
            && rel_in_scope(&rel, scope)
        {
            resolved_conflicts.insert(rel, logical_sha.clone());
        } else {
            resolved_casefolded.remove(&folded);
            emit_log(
                app,
                "error",
                &format!("Skipping invalid resolved-conflict record '{}'", path),
            );
        }
    }

    let mut domain: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (_, rel) in local_files {
        let rel = normalized_relative_path(rel);
        if !rel.is_empty() && seen.insert(rel.clone()) {
            domain.push(rel);
        }
    }
    for rel in cloud.keys() {
        if seen.insert(rel.clone()) {
            domain.push(rel.clone());
        }
    }
    for rel in resolved_conflicts.keys() {
        if seen.insert(rel.clone()) {
            domain.push(rel.clone());
        }
    }
    for rel in manifest.files.keys() {
        if relative_path_is_included(rel, opt_ins)
            && rel_in_scope(rel, scope)
            && seen.insert(rel.clone())
        {
            domain.push(rel.clone());
        }
    }
    domain.sort();

    let mut outcome = ReconcileOutcome::default();
    let mut to_fetch: Vec<(String, ManifestEntry, SyncAction)> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    for rel in &domain {
        let path = match checked_physical_sync_path(roots, rel) {
            Ok(path) => path,
            Err(error) => {
                emit_log(app, "error", &format!("✗  {}", error));
                outcome.errors.push(error);
                continue;
            }
        };
        let local = local_state_at(&path, rel, manifest);
        let action = classify_path(
            local,
            cloud.get(rel).map(|entry| entry.sha256.as_str()),
            manifest.files.get(rel),
        );
        // Conflict siblings are explicit review artifacts. A durable cloud
        // tombstone records the exact logical bytes reviewed by Resolve, so
        // unchanged replicas remove the copy even after local baseline loss.
        // A locally edited copy remains local-ahead and is never discarded.
        if action == SyncAction::UploadLocal
            && cloud.get(rel).is_none()
            && is_conflict_copy_rel(rel)
            && resolved_conflicts.get(rel).is_some_and(|expected_sha| {
                read_sync_bytes(rel, &path).is_ok_and(|bytes| sha256_bytes(&bytes) == *expected_sha)
            })
        {
            match fs::remove_file(&path) {
                Ok(()) => {
                    emit_log(app, "ok", &format!("✓  {} (resolved)", rel));
                    dropped.push(rel.clone());
                    outcome.applied += 1;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    dropped.push(rel.clone());
                }
                Err(error) => {
                    let message = format!("remove resolved conflict '{}': {}", rel, error);
                    emit_log(app, "error", &format!("✗  {}", message));
                    outcome.errors.push(message);
                }
            }
            continue;
        }
        match action {
            SyncAction::Skip => {
                let republish_projection = manifest
                    .files
                    .get(rel)
                    .is_some_and(|record| push_needs_projection_republish(mode, record));
                if republish_projection {
                    outcome.uploads.push(PendingUpload {
                        rel: rel.clone(),
                        source: UploadSource::LocalFile(path),
                    });
                } else {
                    outcome.unchanged += 1;
                }
            }
            SyncAction::DropRecord => dropped.push(rel.clone()),
            SyncAction::UploadLocal => {
                if is_active_plugin_lock(rel) {
                    let validation = read_sync_bytes(rel, &path)
                        .and_then(|bytes| parse_active_plugin_lock(rel, &bytes).map(|_| ()))
                        .map_err(|error| format!("{} rejected before upload: {}", rel, error));
                    if let Err(error) = validation {
                        emit_log(app, "error", &format!("✗  {}", error));
                        outcome.errors.push(error);
                        continue;
                    }
                }
                match mode {
                    SyncMode::Push => outcome.uploads.push(PendingUpload {
                        rel: rel.clone(),
                        source: UploadSource::LocalFile(path),
                    }),
                    SyncMode::Pull => outcome.kept_local += 1,
                }
            }
            action @ (SyncAction::ApplyCloud | SyncAction::Reconcile) => {
                // Both actions need cloud bytes, so a cloud entry exists.
                if let Some(entry) = cloud.get(rel) {
                    to_fetch.push((rel.clone(), entry.clone(), action));
                }
            }
        }
    }
    for rel in dropped {
        manifest.files.remove(&rel);
    }
    if mode == SyncMode::Pull && outcome.kept_local > 0 {
        emit_log(
            app,
            "info",
            &format!(
                "{} file(s) ahead of cloud — kept; push to publish them",
                outcome.kept_local
            ),
        );
    }

    if to_fetch.is_empty() {
        return Ok(outcome);
    }

    emit_log(
        app,
        "info",
        &format!(
            "Fetching {} changed cloud file(s) — {} at a time…",
            to_fetch.len(),
            SYNC_CONCURRENCY
        ),
    );
    let total = to_fetch.len();
    emit_progress(app, 0, total);

    let backups = backups_root(app)?;
    let backup_run = create_backup_run(&backups)?;

    let sem = Arc::new(Semaphore::new(SYNC_CONCURRENCY));
    let mut set: JoinSet<(
        String,
        SyncAction,
        u64,
        String,
        Result<(Vec<u8>, bool), String>,
    )> = JoinSet::new();
    for (rel, entry, action) in to_fetch {
        let store = store.clone();
        let key = profile_key(profile_id, &entry.object_key);
        let sem = sem.clone();
        let app_h = app.clone();
        set.spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");
            emit_log(&app_h, "info", &format!("↓  {}", rel));
            let fetched = store.get(&key).await.and_then(|(data, _etag)| {
                // The manifest's declared hash and size are the contract;
                // any divergence is detected corruption, never applied.
                if data.len() as u64 != entry.size || sha256_bytes(&data) != entry.sha256 {
                    Err(format!(
                        "'{}' content does not match its manifest entry — skipped",
                        rel
                    ))
                } else {
                    let logical = if codex_config::is_config_artifact(&rel) {
                        codex_config::project_portable_bytes(&data)
                            .map_err(|error| format!("project cloud '{}': {}", rel, error))?
                    } else {
                        data.clone()
                    };
                    let needs_republish = logical != data;
                    Ok((logical, needs_republish))
                }
            });
            (rel, action, entry.source_mtime, entry.sha256, fetched)
        });
    }

    let mut done = 0usize;
    while let Some(joined) = set.join_next().await {
        done += 1;
        match joined {
            Ok((
                rel,
                action,
                source_mtime,
                cloud_object_sha,
                Ok((data, cloud_needs_republish)),
            )) => {
                resolve_cloud_bytes(
                    app,
                    roots,
                    &backup_run,
                    mode,
                    action,
                    &rel,
                    data,
                    &cloud_object_sha,
                    cloud_needs_republish,
                    source_mtime,
                    manifest,
                    &mut outcome,
                );
            }
            Ok((rel, _, _, _, Err(error))) => {
                emit_log(app, "error", &format!("✗  {}: {}", rel, error));
                outcome.errors.push(error);
            }
            Err(error) => {
                emit_log(app, "error", &format!("task panic: {}", error));
                outcome.errors.push(error.to_string());
            }
        }
        emit_progress(app, done, total);
    }

    prune_backup_runs(&backups);
    Ok(outcome)
}

/// Apply this push's uploads to the current cloud manifest. Entries the
/// upload set does not touch — including ordinary opt-in paths ineligible on
/// this machine — are carried forward untouched. Hard-Never paths are purged,
/// and a same-content upload keeps the already-published object instead of
/// re-uploading it.
fn build_desired_manifest(
    current: &BTreeMap<String, ManifestEntry>,
    uploads: &[(String, String, u64, u64)], // (rel, sha256, size, source_mtime)
    upload_id: &str,
) -> (BTreeMap<String, ManifestEntry>, CommitSummary) {
    // Hard-Never paths from manifests published by older versions are not
    // carried into the next generation. Enforce this again for uploads so a
    // scanner or conflict-name bug cannot reintroduce them.
    let mut files: BTreeMap<String, ManifestEntry> = current
        .iter()
        .filter(|(rel, _)| !path_or_conflict_shadow_is_never_synced(rel))
        .map(|(rel, entry)| (rel.clone(), entry.clone()))
        .collect();
    let mut summary = CommitSummary {
        deleted: (current.len() - files.len()) as u64,
        ..CommitSummary::default()
    };
    for (rel, sha, size, source_mtime) in uploads {
        if path_or_conflict_shadow_is_never_synced(rel) {
            continue;
        }
        match files.get(rel) {
            Some(existing) if existing.sha256 == *sha => continue,
            Some(_) => summary.modified += 1,
            None => summary.added += 1,
        }
        files.insert(
            rel.clone(),
            ManifestEntry {
                sha256: sha.clone(),
                size: *size,
                object_key: format!("_uploads/{}/files/{}", upload_id, rel),
                source_mtime: *source_mtime,
            },
        );
    }
    (files, summary)
}

fn validate_casefold_unique_manifest(
    files: &BTreeMap<String, ManifestEntry>,
) -> Result<(), String> {
    let mut seen: HashMap<String, &str> = HashMap::new();
    for path in files.keys() {
        let folded = path.to_lowercase();
        if let Some(previous) = seen.insert(folded, path.as_str()) {
            if previous != path {
                return Err(format!(
                    "manifest paths '{}' and '{}' collide on case-insensitive filesystems",
                    previous, path
                ));
            }
        }
    }
    for path in files.keys() {
        let components: Vec<&str> = path.split('/').collect();
        for end in 1..components.len() {
            let ancestor = components[..end].join("/");
            if let Some(existing) = seen.get(&ancestor.to_lowercase()) {
                return Err(format!(
                    "manifest file '{}' is an ancestor of '{}'",
                    existing, path
                ));
            }
        }
    }
    Ok(())
}

struct PushRootOutcome {
    pushed: usize,
    applied: usize,
    message: String,
}

/// Push one root's selected files to its linked profile. A lost head race
/// restarts from a fresh head: the loser's staged objects stay behind as
/// unpublished orphans and the union re-runs against the winner's
/// generation.
#[allow(clippy::too_many_arguments)]
async fn push_profile(
    app: &AppHandle,
    paused: &Arc<AtomicBool>,
    store: &Store,
    storage: &StorageConfig,
    roots: &Roots,
    local_id: &str,
    profile: &ProfileLink,
    conditional: bool,
    file_list: &[(PathBuf, String)],
    scope: &[String],
) -> Result<PushRootOutcome, String> {
    let baseline_saved =
        load_baseline(app, local_id, &storage.id, &profile.profile_id).unwrap_or_default();
    // One-name model: a custom local profile name renames the cloud profile
    // on push, for every machine syncing it.
    // ponytail: siblings sharing one cloud profile under different names
    // flip the label per push — display-only; last named pusher wins.
    let rename_to = load_sync_config(app)
        .ok()
        .and_then(|c| c.local_profiles.into_iter().find(|p| p.id == local_id))
        .map(|p| p.name.trim().to_string())
        .filter(|name| !name.is_empty());
    let actor_name = if profile.actor_name.is_empty() {
        default_actor_name()
    } else {
        profile.actor_name.clone()
    };
    let machine_name = if profile.machine_name.is_empty() {
        default_machine_name()
    } else {
        profile.machine_name.clone()
    };

    const PUSH_ATTEMPTS: usize = 3;
    for attempt in 1..=PUSH_ATTEMPTS {
        let Some((head, head_etag)) = fetch_head(store, &profile.profile_id).await? else {
            return Err(format!(
                "profile '{}' has no _head.json — corrupted or deleted",
                profile.profile_id
            ));
        };
        let cloud_manifest = fetch_cloud_manifest(store, &profile.profile_id, &head).await?;
        emit_log(
            app,
            "info",
            &format!(
                "{}: cloud at generation {} — reconciling…",
                profile.root, head.generation
            ),
        );

        let mut baseline = baseline_saved.clone();
        let mut outcome = reconcile_with_cloud(
            app,
            store,
            &storage.included_default_exclusions,
            roots,
            &profile.profile_id,
            &cloud_manifest,
            file_list,
            Some(scope),
            &mut baseline,
            SyncMode::Push,
        )
        .await?;

        // Reconciliation errors are transaction blockers. In particular, an
        // invalid active plugin lock must not be bypassed by publishing other
        // selected files and only reporting failure after the head commit.
        if !outcome.errors.is_empty() {
            let detail = outcome.errors[0].clone();
            let mut message = format!(
                "{} sync operation(s) failed before publish: {}",
                outcome.errors.len(),
                detail
            );
            if let Err(error) =
                save_baseline(app, local_id, &storage.id, &profile.profile_id, &baseline)
            {
                message.push_str(&format!(
                    "; failed to save partial-apply baseline: {}",
                    error
                ));
            }
            emit_log(app, "error", &message);
            return Err(message);
        }

        let union_summary = if outcome.applied + outcome.merged + outcome.conflicts > 0 {
            let summary = format!(
                "union: {} pulled in, {} merged, {} conflict copies",
                outcome.applied, outcome.merged, outcome.conflicts
            );
            emit_log(
                app,
                "info",
                &format!("Cloud changed since last sync — {}", summary),
            );
            Some(summary)
        } else {
            None
        };

        let mut to_upload: Vec<(String, Vec<u8>, u64)> = Vec::new();
        for upload in std::mem::take(&mut outcome.uploads) {
            if path_or_conflict_shadow_is_never_synced(&upload.rel) {
                let error = format!("Skipping hard-Never upload '{}'", upload.rel);
                emit_log(app, "error", &error);
                outcome.errors.push(error);
                continue;
            }
            // Revalidate immediately before consuming a queued upload. A
            // network reconcile may have left enough time for a descendant
            // directory or file to be replaced by a symlink after the first
            // classification pass.
            let checked_path = match checked_physical_sync_path(roots, &upload.rel) {
                Ok(path) => path,
                Err(error) => {
                    emit_log(app, "error", &format!("Skipping upload: {}", error));
                    outcome.errors.push(error);
                    continue;
                }
            };
            // Merge outputs and conflict copies produced by this machine
            // (Bytes) genuinely changed now; local files keep their stat.
            let (data, source_mtime) = match upload.source {
                UploadSource::Bytes(data) => (data, now_secs()),
                UploadSource::LocalFile(path) => {
                    if path != checked_path {
                        let error = format!(
                            "upload path '{}' does not match checked destination '{}'",
                            path.display(),
                            checked_path.display()
                        );
                        emit_log(app, "error", &error);
                        outcome.errors.push(error);
                        continue;
                    }
                    if is_sqlite_database(&path) {
                        emit_log(app, "info", &format!("Snapshotting SQLite {}", upload.rel));
                    }
                    match read_sync_bytes(&upload.rel, &path) {
                        Ok(data) => (data, file_mtime_secs(&path)),
                        Err(error) => {
                            emit_log(app, "error", &error);
                            outcome.errors.push(error);
                            continue;
                        }
                    }
                }
            };
            if is_active_plugin_lock(&upload.rel) {
                if let Err(error) = parse_active_plugin_lock(&upload.rel, &data) {
                    let error = format!(
                        "{} rejected at final upload snapshot: {}",
                        upload.rel, error
                    );
                    emit_log(app, "error", &format!("✗  {}", error));
                    outcome.errors.push(error);
                    continue;
                }
            }
            to_upload.push((upload.rel, data, source_mtime));
        }

        if !outcome.errors.is_empty() {
            let detail = outcome.errors[0].clone();
            let mut message = format!(
                "{} sync operation(s) failed before upload staging: {}",
                outcome.errors.len(),
                detail
            );
            if let Err(error) =
                save_baseline(app, local_id, &storage.id, &profile.profile_id, &baseline)
            {
                message.push_str(&format!(
                    "; failed to save partial-apply baseline: {}",
                    error
                ));
            }
            emit_log(app, "error", &message);
            return Err(message);
        }

        let upload_id = new_upload_id()?;
        let uploads_meta: Vec<(String, String, u64, u64)> = to_upload
            .iter()
            .map(|(rel, data, mtime)| (rel.clone(), sha256_bytes(data), data.len() as u64, *mtime))
            .collect();
        let (desired_files, summary) =
            build_desired_manifest(&cloud_manifest.files, &uploads_meta, &upload_id);
        if let Err(error) = validate_casefold_unique_manifest(&desired_files) {
            let _ = save_baseline(app, local_id, &storage.id, &profile.profile_id, &baseline);
            return Err(error);
        }

        if desired_files == cloud_manifest.files {
            // Nothing to publish — but a pending rename still lands, or a
            // rename-then-push with no file changes would silently do
            // nothing (the live bug behind S35).
            if let Some(name) = rename_to.as_deref() {
                rename_tag_best_effort(app, store, &storage.id, &profile.profile_id, &head, name)
                    .await;
            }
            // The union may still have applied cloud changes locally, which
            // only the baseline needs to remember.
            store_cloud_cache(
                app,
                cache_from_manifest(
                    &head,
                    &cloud_manifest.files,
                    &storage.id,
                    &profile.profile_id,
                ),
            );
            baseline.last_push = now_secs();
            if let Err(e) =
                save_baseline(app, local_id, &storage.id, &profile.profile_id, &baseline)
            {
                emit_log(app, "error", &format!("Failed to save baseline: {}", e));
            }
            if !outcome.errors.is_empty() {
                let msg = format!("{} sync operation(s) failed", outcome.errors.len());
                emit_log(app, "error", &msg);
                return Err(msg);
            }
            let message = match &union_summary {
                Some(summary) => format!("up to date after merge ({})", summary),
                None => "everything up to date".to_string(),
            };
            emit_log(app, "ok", &format!("{} — {}", profile.root, message));
            return Ok(PushRootOutcome {
                pushed: 0,
                applied: outcome.applied + outcome.merged,
                message,
            });
        }

        // Snapshot uploads: readable original-path keys under this attempt's
        // batch prefix, invisible to every reader until the head flips.
        let our_prefix = format!("_uploads/{}/", upload_id);
        let mut uploads: Vec<(String, String, Vec<u8>)> = Vec::new();
        for (rel, data, _) in &to_upload {
            if let Some(entry) = desired_files.get(rel) {
                if entry.object_key.starts_with(&our_prefix) {
                    uploads.push((rel.clone(), entry.object_key.clone(), data.clone()));
                }
            }
        }

        let total = uploads.len();
        emit_log(
            app,
            "info",
            &format!(
                "{}: uploading {} changed file(s) — {} at a time…",
                profile.root, total, SYNC_CONCURRENCY
            ),
        );
        emit_progress(app, 0, total);

        let sem = Arc::new(Semaphore::new(SYNC_CONCURRENCY));
        let mut set: JoinSet<(String, Result<Option<String>, String>)> = JoinSet::new();
        for (rel, object_key, data) in &uploads {
            let store_task = store.clone();
            let key = profile_key(&profile.profile_id, object_key);
            let sem = sem.clone();
            let app_h = app.clone();
            let paused = paused.clone();
            let rel = rel.clone();
            let data = data.clone();
            set.spawn(async move {
                let _permit = sem.acquire().await.expect("semaphore closed");
                wait_if_paused(&app_h, &paused).await;
                emit_log(&app_h, "info", &format!("↑  {}", rel));
                let r = store_task.put(&key, data).await;
                (rel, r)
            });
        }
        let mut done = 0usize;
        let mut upload_errors: Vec<String> = Vec::new();
        while let Some(res) = set.join_next().await {
            done += 1;
            match res {
                Ok((_, Ok(_))) => {}
                Ok((rel, Err(e))) => {
                    emit_log(app, "error", &format!("✗  {}: {}", rel, e));
                    upload_errors.push(e);
                }
                Err(e) => upload_errors.push(e.to_string()),
            }
            emit_progress(app, done, total);
        }
        if !upload_errors.is_empty() {
            // Nothing was published; everything written so far is an orphan.
            let msg = format!(
                "{} upload(s) failed — nothing was published",
                upload_errors.len()
            );
            emit_log(app, "error", &msg);
            return Err(msg);
        }

        // Immutable batch metadata, manifest, and commit — written before
        // the head flips, so every published generation has its history.
        let objects: Vec<serde_json::Value> = uploads
            .iter()
            .map(|(rel, object_key, data)| {
                serde_json::json!({
                    "path": rel,
                    "sha256": sha256_bytes(data),
                    "size": data.len(),
                    "object_key": object_key,
                })
            })
            .collect();
        let mut batch = serde_json::json!({
            "schema_version": CLOUD_SCHEMA_VERSION,
            "upload_id": upload_id,
            "created_at": now_secs(),
            "actor_name": actor_name,
            "machine_name": machine_name,
            "base_generation": head.generation,
            "status": "staged",
            "objects": objects,
        });
        let batch_key = format!("_uploads/{}/_upload.json", upload_id);
        store
            .put(
                &profile_key(&profile.profile_id, &batch_key),
                batch.to_string().into_bytes(),
            )
            .await?;

        let generation = head.generation + 1;
        let commit_id = new_commit_id()?;
        let manifest_key = history_object_key("_manifests", generation, &commit_id);
        let commit_key = history_object_key("_commits", generation, &commit_id);
        let new_manifest = CloudManifest {
            schema_version: CLOUD_SCHEMA_VERSION,
            generation,
            commit_id: commit_id.clone(),
            updated_at: now_secs(),
            files: desired_files.clone(),
            resolved_conflicts: cloud_manifest
                .resolved_conflicts
                .iter()
                .filter(|(path, _)| {
                    !desired_files
                        .keys()
                        .any(|live| live.eq_ignore_ascii_case(path))
                })
                .map(|(path, sha)| (path.clone(), sha.clone()))
                .collect(),
        };
        let manifest_bytes = serde_json::to_vec(&new_manifest).map_err(|e| e.to_string())?;
        let manifest_sha256 = sha256_bytes(&manifest_bytes);
        let commit = CommitRecord {
            schema_version: CLOUD_SCHEMA_VERSION,
            commit_id: commit_id.clone(),
            generation,
            created_at: now_secs(),
            actor_name: actor_name.clone(),
            machine_name: machine_name.clone(),
            upload_id: upload_id.clone(),
            message: format!("Push {} changed file(s)", uploads.len()),
            manifest_key: manifest_key.clone(),
            manifest_sha256: manifest_sha256.clone(),
            previous_commit_key: Some(head.commit_key.clone()),
            previous_manifest_sha256: Some(head.manifest_sha256.clone()),
            summary,
        };
        store
            .put(
                &profile_key(&profile.profile_id, &manifest_key),
                manifest_bytes,
            )
            .await?;
        store
            .put(
                &profile_key(&profile.profile_id, &commit_key),
                serde_json::to_vec(&commit).map_err(|e| e.to_string())?,
            )
            .await?;

        // Publish. The head CAS is the sole point where this push becomes
        // visible — it wins or it lost the race, nothing in between.
        let new_head = HeadFile {
            schema_version: CLOUD_SCHEMA_VERSION,
            profile_id: profile.profile_id.clone(),
            root: profile.root.clone(),
            state: "active".to_string(),
            generation,
            commit_id: commit_id.clone(),
            manifest_key,
            commit_key,
            manifest_sha256,
            updated_at: now_secs(),
        };
        let condition = if !conditional {
            PutCondition::Unconditional
        } else {
            match &head_etag {
                Some(etag) => PutCondition::IfMatch(etag.clone()),
                // The store returned no ETag on GET — nothing to CAS against.
                None => PutCondition::Unconditional,
            }
        };
        let mut publish = store
            .put_conditional(
                &profile_key(&profile.profile_id, HEAD_OBJECT),
                serde_json::to_vec(&new_head).map_err(|e| e.to_string())?,
                &condition,
            )
            .await?;
        if matches!(publish, PutOutcome::Ambiguous) {
            emit_log(
                app,
                "info",
                "Head write ambiguous — re-reading to decide the race",
            );
            publish = match fetch_head(store, &profile.profile_id).await? {
                Some((current, _)) if current.commit_id == commit_id => PutOutcome::Written,
                Some(_) => PutOutcome::PreconditionFailed,
                None => {
                    return Err("head vanished while publishing — profile corruption".to_string())
                }
            };
        }
        match publish {
            PutOutcome::Written => {
                emit_log(
                    app,
                    "ok",
                    &format!(
                        "{}: published generation {} (commit {})",
                        profile.root, generation, commit_id
                    ),
                );
                // Post-publish best-effort: display tag and batch status.
                let label = write_tag_best_effort(
                    app,
                    store,
                    &profile.profile_id,
                    &profile.root,
                    &profile.profile_label,
                    rename_to.as_deref(),
                    &commit,
                    desired_files.len() as u64,
                )
                .await;
                if label != profile.profile_label {
                    adopt_profile_label(app, &storage.id, &profile.profile_id, &label);
                }
                batch["status"] = "committed".into();
                let _ = store
                    .put(
                        &profile_key(&profile.profile_id, &batch_key),
                        batch.to_string().into_bytes(),
                    )
                    .await;

                for (rel, data, _) in &to_upload {
                    baseline
                        .files
                        .insert(rel.clone(), file_record(&roots.abs(rel), data));
                }
                baseline.last_push = now_secs();
                if let Err(e) =
                    save_baseline(app, local_id, &storage.id, &profile.profile_id, &baseline)
                {
                    emit_log(app, "error", &format!("Failed to save baseline: {}", e));
                }
                store_cloud_cache(
                    app,
                    cache_from_manifest(
                        &new_head,
                        &desired_files,
                        &storage.id,
                        &profile.profile_id,
                    ),
                );
                if !outcome.errors.is_empty() {
                    let msg = format!("{} sync operation(s) failed", outcome.errors.len());
                    emit_log(app, "error", &msg);
                    return Err(msg);
                }
                let message = match &union_summary {
                    Some(summary) => format!(
                        "pushed {} file(s) at generation {} ({})",
                        uploads.len(),
                        generation,
                        summary
                    ),
                    None => format!(
                        "pushed {} file(s) at generation {}",
                        uploads.len(),
                        generation
                    ),
                };
                return Ok(PushRootOutcome {
                    pushed: uploads.len(),
                    applied: outcome.applied + outcome.merged,
                    message,
                });
            }
            PutOutcome::PreconditionFailed => {
                emit_log(
                    app,
                    "info",
                    &format!(
                        "Cloud changed during push — rebasing and retrying ({}/{})",
                        attempt, PUSH_ATTEMPTS
                    ),
                );
                continue;
            }
            PutOutcome::Ambiguous => unreachable!("ambiguity resolved above"),
        }
    }
    Err(format!(
        "{}: cloud kept changing during push — try again",
        profile.root
    ))
}

/// Publish an explicit conflict-copy resolution as a manifest-only commit.
/// Historical manifests keep the removed object's bytes reachable; the new
/// head omits the sibling. The head CAS makes the deletion race-safe and a
/// retry rebases it over concurrent unrelated changes.
async fn publish_conflict_resolution(
    app: &AppHandle,
    store: &Store,
    storage_id: &str,
    profile: &ProfileLink,
    conditional: bool,
    rel: &str,
    expected_sha256: &str,
) -> Result<bool, String> {
    const ATTEMPTS: usize = 3;
    for attempt in 1..=ATTEMPTS {
        let Some((head, head_etag)) = fetch_head(store, &profile.profile_id).await? else {
            return Err(format!(
                "profile '{}' has no _head.json — corrupted or deleted",
                profile.profile_id
            ));
        };
        let current = fetch_cloud_manifest(store, &profile.profile_id, &head).await?;
        let rel_folded = rel.to_lowercase();
        if let Some(alias) = current.files.keys().find(|path| {
            if path.as_str() == rel {
                return false;
            }
            let folded = path.to_lowercase();
            folded == rel_folded
                || folded.starts_with(&format!("{}/", rel_folded))
                || rel_folded.starts_with(&format!("{}/", folded))
        }) {
            return Err(format!(
                "conflict copy '{}' aliases live cloud path '{}' on case-insensitive filesystems",
                rel, alias
            ));
        }
        if let Some(alias) = current
            .resolved_conflicts
            .keys()
            .find(|path| path.as_str() != rel && path.eq_ignore_ascii_case(rel))
        {
            return Err(format!(
                "conflict resolution '{}' aliases existing tombstone '{}'",
                rel, alias
            ));
        }
        let reviewed_bytes_match = match current.files.get(rel) {
            Some(current_entry) => {
                validate_object_key(&current_entry.object_key)?;
                if current_entry.sha256 == expected_sha256 {
                    true
                } else if codex_config::is_config_artifact(rel) {
                    // Legacy manifests may hold raw config bytes whose
                    // target-local overlay projects away on read. Resolve pins
                    // the reviewed logical bytes while the head CAS still pins
                    // the exact manifest entry.
                    let key = profile_key(&profile.profile_id, &current_entry.object_key);
                    let (raw, _) = store.get(&key).await?;
                    if raw.len() as u64 != current_entry.size
                        || sha256_bytes(&raw) != current_entry.sha256
                    {
                        return Err(format!(
                            "conflict copy '{}' content does not match its manifest entry",
                            rel
                        ));
                    }
                    codex_config::project_portable_bytes(&raw)
                        .is_ok_and(|logical| sha256_bytes(&logical) == expected_sha256)
                } else {
                    false
                }
            }
            None => {
                if current
                    .resolved_conflicts
                    .get(rel)
                    .is_some_and(|sha| sha == expected_sha256)
                {
                    store_cloud_cache(
                        app,
                        cache_from_manifest(&head, &current.files, storage_id, &profile.profile_id),
                    );
                    return Ok(false);
                }
                // The entry disappeared concurrently without a durable Resolve
                // record. Publish the reviewed logical SHA so baseline-less
                // replicas do not resurrect it.
                true
            }
        };
        if !reviewed_bytes_match {
            return Err(format!(
                "conflict copy '{}' does not match the reviewed local copy — refresh and compare it again",
                rel
            ));
        }

        let mut desired_files = current.files.clone();
        desired_files.remove(rel);
        let mut resolved_conflicts = current.resolved_conflicts.clone();
        resolved_conflicts.insert(rel.to_string(), expected_sha256.to_string());
        let upload_id = new_upload_id()?;
        let mut batch = serde_json::json!({
            "schema_version": CLOUD_SCHEMA_VERSION,
            "upload_id": upload_id,
            "created_at": now_secs(),
            "actor_name": if profile.actor_name.is_empty() { default_actor_name() } else { profile.actor_name.clone() },
            "machine_name": if profile.machine_name.is_empty() { default_machine_name() } else { profile.machine_name.clone() },
            "base_generation": head.generation,
            "status": "staged",
            "objects": [],
        });
        let batch_key = format!("_uploads/{}/_upload.json", upload_id);
        store
            .put(
                &profile_key(&profile.profile_id, &batch_key),
                batch.to_string().into_bytes(),
            )
            .await?;

        let generation = head.generation + 1;
        let commit_id = new_commit_id()?;
        let manifest_key = history_object_key("_manifests", generation, &commit_id);
        let commit_key = history_object_key("_commits", generation, &commit_id);
        let manifest = CloudManifest {
            schema_version: CLOUD_SCHEMA_VERSION,
            generation,
            commit_id: commit_id.clone(),
            updated_at: now_secs(),
            files: desired_files.clone(),
            resolved_conflicts,
        };
        let manifest_bytes = serde_json::to_vec(&manifest).map_err(|error| error.to_string())?;
        let manifest_sha256 = sha256_bytes(&manifest_bytes);
        let commit = CommitRecord {
            schema_version: CLOUD_SCHEMA_VERSION,
            commit_id: commit_id.clone(),
            generation,
            created_at: now_secs(),
            actor_name: if profile.actor_name.is_empty() {
                default_actor_name()
            } else {
                profile.actor_name.clone()
            },
            machine_name: if profile.machine_name.is_empty() {
                default_machine_name()
            } else {
                profile.machine_name.clone()
            },
            upload_id: upload_id.clone(),
            message: format!("Resolve conflict copy {}", rel),
            manifest_key: manifest_key.clone(),
            manifest_sha256: manifest_sha256.clone(),
            previous_commit_key: Some(head.commit_key.clone()),
            previous_manifest_sha256: Some(head.manifest_sha256.clone()),
            summary: CommitSummary {
                deleted: 1,
                ..CommitSummary::default()
            },
        };
        store
            .put(
                &profile_key(&profile.profile_id, &manifest_key),
                manifest_bytes,
            )
            .await?;
        store
            .put(
                &profile_key(&profile.profile_id, &commit_key),
                serde_json::to_vec(&commit).map_err(|error| error.to_string())?,
            )
            .await?;

        let new_head = HeadFile {
            schema_version: CLOUD_SCHEMA_VERSION,
            profile_id: profile.profile_id.clone(),
            root: profile.root.clone(),
            state: "active".to_string(),
            generation,
            commit_id: commit_id.clone(),
            manifest_key,
            commit_key,
            manifest_sha256,
            updated_at: now_secs(),
        };
        let condition = if conditional {
            head_etag
                .as_ref()
                .map_or(PutCondition::Unconditional, |etag| {
                    PutCondition::IfMatch(etag.clone())
                })
        } else {
            PutCondition::Unconditional
        };
        let mut publish = store
            .put_conditional(
                &profile_key(&profile.profile_id, HEAD_OBJECT),
                serde_json::to_vec(&new_head).map_err(|error| error.to_string())?,
                &condition,
            )
            .await?;
        if matches!(publish, PutOutcome::Ambiguous) {
            publish = match fetch_head(store, &profile.profile_id).await? {
                Some((current, _)) if current.commit_id == commit_id => PutOutcome::Written,
                Some(_) => PutOutcome::PreconditionFailed,
                None => {
                    return Err("head vanished while publishing — profile corruption".to_string())
                }
            };
        }
        match publish {
            PutOutcome::Written => {
                let label = write_tag_best_effort(
                    app,
                    store,
                    &profile.profile_id,
                    &profile.root,
                    &profile.profile_label,
                    None,
                    &commit,
                    desired_files.len() as u64,
                )
                .await;
                if label != profile.profile_label {
                    adopt_profile_label(app, storage_id, &profile.profile_id, &label);
                }
                batch["status"] = "committed".into();
                let _ = store
                    .put(
                        &profile_key(&profile.profile_id, &batch_key),
                        batch.to_string().into_bytes(),
                    )
                    .await;
                store_cloud_cache(
                    app,
                    cache_from_manifest(&new_head, &desired_files, storage_id, &profile.profile_id),
                );
                emit_log(app, "ok", &format!("Resolved conflict copy {}", rel));
                return Ok(true);
            }
            PutOutcome::PreconditionFailed => {
                emit_log(
                    app,
                    "info",
                    &format!(
                        "Cloud changed during conflict resolution — retrying ({}/{})",
                        attempt, ATTEMPTS
                    ),
                );
            }
            PutOutcome::Ambiguous => unreachable!("ambiguity resolved above"),
        }
    }
    Err(format!(
        "{}: cloud kept changing during conflict resolution — try again",
        profile.root
    ))
}

fn capture_codex_plugin_lock_checked(roots: &Roots) -> Result<bool, String> {
    let initial_path = checked_physical_sync_path(roots, codex_plugins::LOCK_REL)?;
    let codex = codex_plugins::find_binary("codex")?;
    let runner =
        codex_plugins::ProcessRunner::for_binary(codex).with_codex_home(codex_home_override(roots));
    let version = codex_plugins::codex_version(&runner)?;
    let inventory = codex_plugins::fetch_inventory(&runner)?;
    let lock = codex_plugins::capture_lock(&inventory, &version);
    let final_path = checked_physical_sync_path(roots, codex_plugins::LOCK_REL)?;
    if final_path != initial_path {
        return Err("Codex plugin lock changed physical mapping during capture".to_string());
    }
    codex_plugins::save_captured_lock(&final_path, &lock)
}

/// Pre-push hook: refresh this profile's portable plugin locks so current
/// intent rides along with any push touching its root, and force-include
/// the lock files even when the selection did not name them. Capture failure
/// never blocks a push — the last valid lock keeps syncing
/// (PLAN_ENVIRONMENT_RECONCILER.md / PLAN_CLAUDE_PLUGIN_LOCK.md).
async fn refresh_plugin_locks(app: &AppHandle, roots: &Roots, files: &[String]) -> Vec<String> {
    let mut files: Vec<String> = files.to_vec();
    let pushing = files.iter().any(|f| {
        let path = Path::new(f);
        path.starts_with(&roots.dir) || path.starts_with(&roots.remap)
    });
    let codex_push = pushing && roots.root == ".codex";
    let claude_push = pushing && roots.root == ".claude";
    // Locks live in ~/.agent-sync now; clear leftovers a pre-remap build
    // wrote into the root (PLAN_GLOBAL_AGENT_SYNC_DIR.md §5).
    if codex_push {
        remove_stale_in_root_agent_sync(&roots.dir, codex_plugins::LOCK_REL);
    }
    if claude_push {
        remove_stale_in_root_agent_sync(&roots.dir, codex_plugins::CLAUDE_LOCK_REL);
    }
    // Captures are skipped under test: the harness's fake roots must not
    // depend on this machine's real agent installations. Force-include below
    // still runs, so tests cover the companion-file behavior.
    #[cfg(test)]
    let _ = app;
    #[cfg(not(test))]
    {
        if codex_push {
            let path = match checked_physical_sync_path(roots, codex_plugins::LOCK_REL) {
                Ok(path) => Some(path),
                Err(error) => {
                    emit_log(
                        app,
                        "error",
                        &format!("Codex plugin capture skipped: {}", error),
                    );
                    None
                }
            };
            if path
                .as_ref()
                .is_some_and(|path| lock_conflict_siblings(path).is_empty())
            {
                let capture_roots = roots.clone();
                match tauri::async_runtime::spawn_blocking(move || {
                    capture_codex_plugin_lock_checked(&capture_roots)
                })
                .await
                {
                    Ok(Ok(true)) => emit_log(app, "info", "Codex plugin lock refreshed"),
                    Ok(Ok(false)) => {}
                    Ok(Err(e)) => emit_log(
                        app,
                        "info",
                        &format!(
                            "Codex plugin intent not refreshed ({}) — pushing the last captured lock if any",
                            e
                        ),
                    ),
                    Err(e) => {
                        emit_log(app, "info", &format!("Codex plugin capture task failed: {}", e))
                    }
                }
            } else if path.is_some() {
                emit_log(
                    app,
                    "info",
                    "Codex plugin lock has an unresolved conflict — preserving both sides without recapturing",
                );
            }
        }
        if claude_push {
            // Pure file reads — no CLI involved for the Claude side.
            let path = match checked_physical_sync_path(roots, codex_plugins::CLAUDE_LOCK_REL) {
                Ok(path) => Some(path),
                Err(error) => {
                    emit_log(
                        app,
                        "error",
                        &format!("Claude plugin capture skipped: {}", error),
                    );
                    None
                }
            };
            if path
                .as_ref()
                .is_some_and(|path| lock_conflict_siblings(path).is_empty())
            {
                let path = path.expect("checked above");
                match codex_plugins::try_capture_claude_lock(&roots.dir) {
                    Ok(lock) => match codex_plugins::save_captured_claude_lock(&path, &lock) {
                        Ok(true) => emit_log(app, "info", "Claude plugin lock refreshed"),
                        Ok(false) => {}
                        Err(e) => emit_log(
                            app,
                            "info",
                            &format!(
                                "Claude plugin intent not refreshed ({}) — pushing the last captured lock if any",
                                e
                            ),
                        ),
                    },
                    Err(e) => emit_log(
                        app,
                        "info",
                        &format!(
                            "Claude plugin intent not refreshed ({}) — pushing the last captured lock if any",
                            e
                        ),
                    ),
                }
            } else if path.is_some() {
                emit_log(
                    app,
                    "info",
                    "Claude plugin lock has an unresolved conflict — preserving both sides without recapturing",
                );
            }
        }
        if codex_push {
            // Pure file reads of the desktop global-state file.
            let path = match checked_physical_sync_path(roots, codex_sidebar::LOCK_REL) {
                Ok(path) => Some(path),
                Err(error) => {
                    emit_log(app, "error", &format!("Sidebar capture skipped: {}", error));
                    None
                }
            };
            if path
                .as_ref()
                .is_some_and(|path| lock_conflict_siblings(path).is_empty())
            {
                let path = path.expect("checked above");
                match codex_sidebar::capture_to(&path, &roots.dir) {
                    Ok(true) => emit_log(app, "info", "Codex sidebar lock refreshed"),
                    Ok(false) => {}
                    Err(e) => emit_log(
                        app,
                        "info",
                        &format!(
                            "Codex sidebar state not refreshed ({}) — pushing the last captured lock if any",
                            e
                        ),
                    ),
                }
            } else if path.is_some() {
                emit_log(
                    app,
                    "info",
                    "Codex sidebar lock has an unresolved conflict — preserving both sides without recapturing",
                );
            }
        }
    }
    for (pushing, lock_rel) in [
        (codex_push, codex_plugins::LOCK_REL),
        (codex_push, codex_sidebar::LOCK_REL),
        (claude_push, codex_plugins::CLAUDE_LOCK_REL),
    ] {
        if !pushing {
            continue;
        }
        let lock_abs = match checked_physical_sync_path(roots, lock_rel) {
            Ok(path) => path,
            Err(error) => {
                emit_log(app, "error", &format!("Generated lock skipped: {}", error));
                continue;
            }
        };
        let covered = files.iter().any(|f| lock_abs.starts_with(Path::new(f)));
        if lock_abs.is_file() && !covered {
            files.push(lock_abs.to_string_lossy().into_owned());
        }
        for conflict in lock_conflict_siblings(&lock_abs) {
            let covered = files
                .iter()
                .any(|file| conflict.starts_with(Path::new(file)));
            if !covered {
                files.push(conflict.to_string_lossy().into_owned());
            }
        }
    }
    files
}

async fn do_push_link(
    app: &AppHandle,
    paused: Arc<AtomicBool>,
    storage_id: &str,
    profile_id: &str,
    files: &[String],
) -> Result<SyncResult, String> {
    let config = load_sync_config(app)?;
    let (storage, local) = resolve_link_parts(&config, storage_id, profile_id)?;
    let roots = Roots::for_profile(&local)?;
    let files = refresh_plugin_locks(app, &roots, files).await;
    let file_list = collect_upload_files(&files, &roots, &storage.included_default_exclusions);
    let scope: Vec<String> = files
        .iter()
        .filter_map(|file| roots.rel(Path::new(file)))
        .map(|rel| normalized_relative_path(&rel))
        .filter(|rel| !rel.is_empty())
        .collect();
    if file_list.is_empty() && scope.is_empty() {
        return Err("Nothing selected to push".to_string());
    }

    let store = match make_store(&storage, Some(&roots)) {
        Ok(s) => s,
        Err(e) => {
            emit_log(app, "error", &e);
            return Err(e);
        }
    };
    warn_if_agents_running(app);

    let conditional = ensure_conditional_capability(app, &store, &storage).await?;
    if !conditional {
        emit_log(
            app,
            "info",
            "⚠ single-writer mode: safe only if exactly one machine pushes to this profile",
        );
    }

    let profile = resolve_profile_for_link(app, &store, &storage, &local).await?;
    let outcome = push_profile(
        app,
        &paused,
        &store,
        &storage,
        &roots,
        &local.id,
        &profile,
        conditional,
        &file_list,
        &scope,
    )
    .await?;

    // Auto-link may have persisted a new cloud link mid-push; mirror the
    // freshest config into the registry.
    let saved = load_sync_config(app).unwrap_or_else(|_| config.clone());
    write_machine_registry(&roots, &saved);

    let message = format!("{} {}", root_display_label(&local.root), outcome.message);
    emit_log(app, "ok", &format!("Done — {}", message));
    Ok(SyncResult {
        success: true,
        files_synced: outcome.pushed + outcome.applied,
        message,
        timestamp: now_secs(),
        setup_state: None,
    })
}

/// The (storage, local profile) pair behind one matrix link. Ops refuse to
/// run on unlinked cells — the settings matrix is the wiring authority.
fn resolve_link_parts(
    config: &SyncConfig,
    storage_id: &str,
    profile_id: &str,
) -> Result<(StorageConfig, LocalProfile), String> {
    let storage = config
        .storages
        .iter()
        .find(|s| s.id == storage_id)
        .cloned()
        .ok_or_else(|| format!("unknown storage '{}'", storage_id))?;
    let local = config
        .local_profiles
        .iter()
        .find(|p| p.id == profile_id)
        .cloned()
        .ok_or_else(|| format!("unknown profile '{}'", profile_id))?;
    if !config
        .links
        .iter()
        .any(|l| l.profile == profile_id && l.storage == storage_id)
    {
        return Err(format!(
            "profile '{}' is not linked to storage '{}' — link them in Settings first",
            profile_id,
            storage_display_name(&storage)
        ));
    }
    Ok((storage, local))
}

async fn do_pull_link(
    app: &AppHandle,
    storage_id: &str,
    profile_id: &str,
) -> Result<SyncResult, String> {
    let config = load_sync_config(app)?;
    let (storage, local) = resolve_link_parts(&config, storage_id, profile_id)?;
    let roots = Roots::for_profile(&local)?;
    let store = match make_store(&storage, Some(&roots)) {
        Ok(s) => s,
        Err(e) => {
            emit_log(app, "error", &e);
            return Err(e);
        }
    };
    warn_if_agents_running(app);

    let root = local.root.as_str();
    let profile = resolve_profile_for_link(app, &store, &storage, &local).await?;
    // An empty profile should still leave a coherent directory behind.
    if let Err(e) = fs::create_dir_all(&roots.dir) {
        return Err(format!("create '{}': {}", roots.dir.display(), e));
    }

    // Pull is deterministic: read the head, then exactly the manifest it
    // references — never stray bucket objects.
    let Some((head, _)) = fetch_head(&store, &profile.profile_id).await? else {
        return Err(format!(
            "profile '{}' has no _head.json — corrupted or deleted",
            profile.profile_id
        ));
    };
    let cloud_manifest = fetch_cloud_manifest(&store, &profile.profile_id, &head).await?;
    store_cloud_cache(
        app,
        cache_from_manifest(
            &head,
            &cloud_manifest.files,
            &storage.id,
            &profile.profile_id,
        ),
    );
    emit_log(
        app,
        "info",
        &format!(
            "{}: pulling generation {} — {} cloud file(s)…",
            root,
            head.generation,
            cloud_manifest.files.len()
        ),
    );

    let mut baseline =
        load_baseline(app, &local.id, &storage.id, &profile.profile_id).unwrap_or_default();
    let root_scope = vec![root.to_string()];
    let outcome = reconcile_with_cloud(
        app,
        &store,
        &storage.included_default_exclusions,
        &roots,
        &profile.profile_id,
        &cloud_manifest,
        &[],
        Some(&root_scope),
        &mut baseline,
        SyncMode::Pull,
    )
    .await?;

    if let Err(e) = save_baseline(app, &local.id, &storage.id, &profile.profile_id, &baseline) {
        emit_log(app, "error", &format!("Failed to save baseline: {}", e));
    }

    if !outcome.errors.is_empty() {
        let msg = format!("{} pull operation(s) failed", outcome.errors.len());
        emit_log(app, "error", &msg);
        return Err(msg);
    }

    let mut parts = vec![format!("pulled {}", outcome.applied)];
    if outcome.merged > 0 {
        parts.push(format!("merged {}", outcome.merged));
    }
    if outcome.conflicts > 0 {
        parts.push(format!("{} conflict copies", outcome.conflicts));
    }
    if outcome.kept_local > 0 {
        parts.push(format!("kept {} local-ahead", outcome.kept_local));
    }
    parts.push(format!("{} unchanged", outcome.unchanged));
    let files_synced = outcome.applied + outcome.merged;

    let saved = load_sync_config(app).unwrap_or_else(|_| config.clone());
    write_machine_registry(&roots, &saved);

    let message = format!(
        "Union pull — {} gen {}: {}",
        root_display_label(root),
        head.generation,
        parts.join(", ")
    );
    emit_log(app, "ok", &format!("Done — {}", message));
    Ok(SyncResult {
        success: true,
        files_synced,
        message,
        timestamp: now_secs(),
        setup_state: None,
    })
}

// ── Plugin repair ────────────────────────────────────────────────────────────
//
// Restores Claude plugins from the declarative intent that already syncs in
// ~/.claude/settings.json (enabledPlugins + extraKnownMarketplaces) by
// driving Claude Code's own installer — never by copying plugin-manager
// workspaces (see PLAN_PLUGIN_SYNC.md). Only ever run from an explicit user
// action: plugins execute arbitrary code.

#[derive(Serialize, Debug, Default)]
pub struct PluginRepairReport {
    marketplaces_added: Vec<String>,
    plugins_installed: Vec<String>,
    already_present: Vec<String>,
    failed: Vec<String>,
}

#[derive(Debug, Default, PartialEq)]
struct PluginIntent {
    /// (marketplace name, source repo/url/path)
    marketplaces: Vec<(String, String)>,
    /// `plugin@marketplace` ids with enabled == true
    plugins: Vec<String>,
}

fn parse_plugin_intent(settings: &serde_json::Value) -> PluginIntent {
    let mut intent = PluginIntent::default();
    if let Some(map) = settings
        .get("extraKnownMarketplaces")
        .and_then(|v| v.as_object())
    {
        for (name, entry) in map {
            let source = &entry["source"];
            if let Some(location) = source["repo"]
                .as_str()
                .or_else(|| source["url"].as_str())
                .or_else(|| source["path"].as_str())
            {
                intent
                    .marketplaces
                    .push((name.clone(), location.to_string()));
            }
        }
    }
    if let Some(map) = settings.get("enabledPlugins").and_then(|v| v.as_object()) {
        for (id, enabled) in map {
            if enabled.as_bool() == Some(true) && id.contains('@') {
                intent.plugins.push(id.clone());
            }
        }
    }
    intent.marketplaces.sort();
    intent.plugins.sort();
    intent
}

/// Present = the plugin manager's own records point at a directory that
/// exists. These files are read locally only, never synced.
#[cfg(test)]
fn marketplace_is_present(claude_dir: &Path, name: &str) -> bool {
    matches!(
        claude_marketplace_registration(claude_dir, name),
        ClaudeMarketplaceRegistration::Existing {
            install_present: true,
            ..
        }
    )
}

/// Return the manager-recorded portable source and whether its install root
/// exists. Source identity is checked separately from presence so a target
/// registration cannot spoof a lock marketplace by reusing its name.
#[derive(Debug, PartialEq, Eq)]
enum ClaudeMarketplaceRegistration {
    Absent,
    Existing {
        source: Option<String>,
        install_present: bool,
    },
}

fn claude_marketplace_registration(claude_dir: &Path, name: &str) -> ClaudeMarketplaceRegistration {
    let path = claude_dir.join("plugins/known_marketplaces.json");
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ClaudeMarketplaceRegistration::Absent;
        }
        Err(_) => {
            return ClaudeMarketplaceRegistration::Existing {
                source: None,
                install_present: false,
            };
        }
    };
    let Ok(known) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return ClaudeMarketplaceRegistration::Existing {
            source: None,
            install_present: false,
        };
    };
    let Some(entry) = known.get(name) else {
        return ClaudeMarketplaceRegistration::Absent;
    };
    let source = entry
        .get("source")
        .and_then(|source| source.get("repo").or_else(|| source.get("url")))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let install_present = entry["installLocation"]
        .as_str()
        .is_some_and(|location| Path::new(location).exists());
    ClaudeMarketplaceRegistration::Existing {
        source,
        install_present,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ClaudePluginPresence {
    Absent,
    Present,
    Corrupt(String),
}

fn claude_plugin_presence(claude_dir: &Path, id: &str) -> ClaudePluginPresence {
    let path = claude_dir.join("plugins/installed_plugins.json");
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ClaudePluginPresence::Absent;
        }
        Err(error) => return ClaudePluginPresence::Corrupt(error.to_string()),
    };
    let installed = match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(installed) => installed,
        Err(error) => return ClaudePluginPresence::Corrupt(error.to_string()),
    };
    let Some(plugins_value) = installed.get("plugins") else {
        return ClaudePluginPresence::Corrupt("plugins object is missing".to_string());
    };
    let Some(plugins) = plugins_value.as_object() else {
        return ClaudePluginPresence::Corrupt("plugins must be an object".to_string());
    };
    let Some(entries_value) = plugins.get(id) else {
        return ClaudePluginPresence::Absent;
    };
    let Some(entries) = entries_value.as_array() else {
        return ClaudePluginPresence::Corrupt(format!("plugin '{}' record must be an array", id));
    };
    if entries.iter().any(|entry| {
        entry
            .get("installPath")
            .and_then(|value| value.as_str())
            .is_some_and(|path| Path::new(path).exists())
    }) {
        ClaudePluginPresence::Present
    } else {
        ClaudePluginPresence::Absent
    }
}

#[cfg(test)]
fn plugin_is_present(claude_dir: &Path, id: &str) -> bool {
    claude_plugin_presence(claude_dir, id) == ClaudePluginPresence::Present
}

fn claude_marketplace_source_mismatch_message(name: &str) -> String {
    format!(
        "✗ marketplace {}: recorded source does not match the sync lock",
        name
    )
}

fn find_claude_binary() -> Result<PathBuf, String> {
    codex_plugins::find_binary("claude")
}

/// Major version out of `claude --version` output ("2.1.206 (Claude Code)").
/// None when the output doesn't lead with a number — then skip the check
/// rather than block on an unrecognized format.
fn claude_major_version(version: &str) -> Option<u32> {
    version
        .split(['.', ' '])
        .next()
        .and_then(|s| s.parse().ok())
}

/// The synced Claude lock rendered as the intent shape the repair loop
/// consumes. Absence alone permits the legacy settings.json fallback. A
/// present malformed/future lock fails closed so an older client cannot
/// install stale or different executable intent.
fn claude_lock_intent(
    app: &AppHandle,
    lock_path: &Path,
    claude_dir: &Path,
) -> Result<Option<PluginIntent>, String> {
    if !lock_path.is_file() {
        return Ok(None);
    }
    match codex_plugins::read_claude_lock(lock_path) {
        Ok(lock) => {
            let disabled = codex_plugins::explicitly_disabled_claude_plugin_ids(claude_dir)?;
            for entry in &lock.manual {
                if disabled.contains(&entry.id) {
                    continue;
                }
                emit_log(
                    app,
                    "info",
                    &format!("manual follow-up: {} — {}", entry.id, entry.reason),
                );
            }
            let plugins: Vec<String> = lock
                .plugins
                .iter()
                .map(|plugin| plugin.id.clone())
                .filter(|id| !disabled.contains(id))
                .collect();
            let needed_marketplaces: HashSet<&str> = plugins
                .iter()
                .filter_map(|id| id.split_once('@').map(|(_, marketplace)| marketplace))
                .collect();
            Ok(Some(PluginIntent {
                marketplaces: lock
                    .marketplaces
                    .iter()
                    .filter(|marketplace| needed_marketplaces.contains(marketplace.name.as_str()))
                    .map(|marketplace| (marketplace.name.clone(), marketplace.repository.clone()))
                    .collect(),
                plugins,
            }))
        }
        Err(e) => {
            emit_log(
                app,
                "error",
                &format!(
                    "plugin lock unreadable ({}) — refusing executable fallback intent",
                    e,
                ),
            );
            Err(format!("plugin lock unreadable: {}", e))
        }
    }
}

fn run_claude(
    app: &AppHandle,
    claude: &Path,
    args: &[&str],
    config_dir: Option<&Path>,
) -> Result<(), String> {
    emit_log(app, "info", &format!("$ claude {}", args.join(" ")));
    let mut command = std::process::Command::new(claude);
    command.args(args);
    // A custom mount is a real Claude config dir: install into it.
    if let Some(dir) = config_dir {
        command.env("CLAUDE_CONFIG_DIR", dir);
    }
    let output = command
        .output()
        .map_err(|e| format!("spawn claude: {}", e))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stdout.lines().chain(stderr.lines()) {
        if !line.trim().is_empty() {
            emit_log(app, "info", &format!("  {}", line));
        }
    }
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("exited with {}", output.status))
    }
}

fn repair_plugins_blocking(
    app: &AppHandle,
    claude_dir: &Path,
    lock_path: &Path,
    custom_mount: bool,
) -> Result<PluginRepairReport, String> {
    let config_dir = custom_mount.then_some(claude_dir);
    // The synced lock is the cross-machine carrier of plugin intent (it
    // merges as a keyed union, so it holds every machine's plugins —
    // PLAN_CLAUDE_PLUGIN_LOCK.md); settings.json is the pre-lock fallback.
    let intent = match claude_lock_intent(app, lock_path, claude_dir)? {
        Some(intent) => intent,
        None => {
            let settings_path = claude_dir.join("settings.json");
            // A fresh root (empty profile just bootstrapped) has neither
            // file yet — that is "no intent", not an error.
            if !settings_path.is_file() {
                emit_log(
                    app,
                    "info",
                    "No plugin lock or settings.json in this root — no plugin intent to repair",
                );
                return Ok(PluginRepairReport::default());
            }
            let raw = fs::read_to_string(&settings_path)
                .map_err(|e| format!("read {}: {}", settings_path.display(), e))?;
            let settings: serde_json::Value =
                serde_json::from_str(&raw).map_err(|e| format!("parse settings.json: {}", e))?;
            parse_plugin_intent(&settings)
        }
    };
    if intent.marketplaces.is_empty() && intent.plugins.is_empty() {
        emit_log(app, "info", "No plugin intent found — nothing to repair");
        return Ok(PluginRepairReport::default());
    }

    let claude = find_claude_binary()?;
    let version = std::process::Command::new(&claude)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    emit_log(
        app,
        "info",
        &format!(
            "Using {}{}",
            claude.display(),
            version
                .as_deref()
                .map(|v| format!(" ({})", v))
                .unwrap_or_default()
        ),
    );
    // Plugin commands exist since Claude Code 2.x. A pre-plugin 1.x treats
    // `plugin marketplace add …` as a *prompt* and answers with a misleading
    // API auth error, so refuse it by version instead. Typical cause: a stale
    // install shadowing the real one on the login-shell PATH.
    if let Some(v) = version.as_deref() {
        if claude_major_version(v).is_some_and(|major| major < 2) {
            let msg = format!(
                "claude at {} is v{} — plugin commands need Claude Code 2.x. This is usually a stale install shadowing your real one; update it (npm install -g @anthropic-ai/claude-code) or remove it.",
                claude.display(),
                v
            );
            emit_log(app, "error", &msg);
            return Err(msg);
        }
    }

    let mut report = PluginRepairReport::default();
    let mut blocked_marketplaces = HashSet::new();
    for (name, source) in &intent.marketplaces {
        match claude_marketplace_registration(claude_dir, name) {
            ClaudeMarketplaceRegistration::Existing {
                source: Some(actual_source),
                install_present: true,
            } if actual_source == *source => {
                report.already_present.push(format!("marketplace {}", name));
                continue;
            }
            ClaudeMarketplaceRegistration::Existing {
                source: actual_source,
                ..
            } if actual_source.as_deref() != Some(source.as_str()) => {
                emit_log(
                    app,
                    "error",
                    &claude_marketplace_source_mismatch_message(name),
                );
                report.failed.push(format!("marketplace {}", name));
                blocked_marketplaces.insert(name.clone());
                continue;
            }
            ClaudeMarketplaceRegistration::Absent
            | ClaudeMarketplaceRegistration::Existing { .. } => {}
        }
        match run_claude(
            app,
            &claude,
            &["plugin", "marketplace", "add", source],
            config_dir,
        ) {
            Ok(()) => match claude_marketplace_registration(claude_dir, name) {
                ClaudeMarketplaceRegistration::Existing {
                    source: Some(actual_source),
                    install_present: true,
                } if actual_source == *source => {
                    emit_log(app, "ok", &format!("✓ marketplace {}", name));
                    report.marketplaces_added.push(name.clone());
                }
                _ => {
                    emit_log(
                        app,
                        "error",
                        &format!(
                            "✗ marketplace {}: installer did not register the exact lock source",
                            name
                        ),
                    );
                    report.failed.push(format!("marketplace {}", name));
                    blocked_marketplaces.insert(name.clone());
                }
            },
            Err(e) => {
                emit_log(app, "error", &format!("✗ marketplace {}: {}", name, e));
                report.failed.push(format!("marketplace {}", name));
                blocked_marketplaces.insert(name.clone());
            }
        }
    }
    for id in &intent.plugins {
        if id
            .split_once('@')
            .is_some_and(|(_, marketplace)| blocked_marketplaces.contains(marketplace))
        {
            emit_log(
                app,
                "error",
                &format!("✗ {}: marketplace source mismatch", id),
            );
            report.failed.push(id.clone());
            continue;
        }
        match claude_plugin_presence(claude_dir, id) {
            ClaudePluginPresence::Present => {
                report.already_present.push(id.clone());
                continue;
            }
            ClaudePluginPresence::Corrupt(error) => {
                emit_log(
                    app,
                    "error",
                    &format!("✗ {}: plugin manager state is unreadable: {}", id, error),
                );
                report.failed.push(id.clone());
                continue;
            }
            ClaudePluginPresence::Absent => {}
        }
        match run_claude(app, &claude, &["plugin", "install", id], config_dir) {
            Ok(()) => {
                if claude_plugin_presence(claude_dir, id) == ClaudePluginPresence::Present {
                    emit_log(app, "ok", &format!("✓ {}", id));
                    report.plugins_installed.push(id.clone());
                } else {
                    emit_log(
                        app,
                        "error",
                        &format!(
                            "✗ {}: installer exited successfully but presence was not verified",
                            id
                        ),
                    );
                    report.failed.push(id.clone());
                }
            }
            Err(e) => {
                emit_log(app, "error", &format!("✗ {}: {}", id, e));
                report.failed.push(id.clone());
            }
        }
    }

    let summary = format!(
        "Plugin repair — {} marketplace(s) added, {} plugin(s) installed, {} already present, {} failed",
        report.marketplaces_added.len(),
        report.plugins_installed.len(),
        report.already_present.len(),
        report.failed.len()
    );
    emit_log(
        app,
        if report.failed.is_empty() {
            "ok"
        } else {
            "error"
        },
        &summary,
    );
    Ok(report)
}

/// Installs can take minutes; keep the blocking child processes off the
/// async runtime's core threads.
/// A profile id → its `LocalProfile` + `Roots`, from the given config. Use
/// `require_profile_kind` when the command only makes sense for one root.
fn profile_roots(config: &SyncConfig, profile_id: &str) -> Result<(LocalProfile, Roots), String> {
    let local = config
        .local_profiles
        .iter()
        .find(|p| p.id == profile_id)
        .cloned()
        .ok_or_else(|| format!("unknown profile '{}'", profile_id))?;
    let roots = Roots::for_profile(&local)?;
    Ok((local, roots))
}

fn require_profile_kind(local: &LocalProfile, root: &str) -> Result<(), String> {
    if local.root != root {
        return Err(format!(
            "profile '{}' holds {} — this action needs a {} profile",
            local.id, local.root, root
        ));
    }
    Ok(())
}

#[tauri::command]
async fn repair_plugins(app: AppHandle, profile: String) -> Result<PluginRepairReport, String> {
    let config = load_sync_config(&app).unwrap_or_default();
    let (local, roots) = profile_roots(&config, &profile)?;
    require_profile_kind(&local, ".claude")?;
    let claude_dir = roots.dir.clone();
    let lock_path = checked_physical_sync_path(&roots, codex_plugins::CLAUDE_LOCK_REL)?;
    let custom_mount = claude_dir != roots.home.join(".claude");
    let app_task = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        repair_plugins_blocking(&app_task, &claude_dir, &lock_path, custom_mount)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Always pin Codex CLI calls to the selected physical home. Passing the
/// default home explicitly also prevents an inherited CODEX_HOME from making
/// inventory and config_path address different profiles.
fn codex_home_override(roots: &Roots) -> Option<PathBuf> {
    Some(roots.dir.clone())
}

/// Repair Codex plugins from the lock at `lock_path`, streaming logs and a
/// summary. Blocking — run under `spawn_blocking`.
static CODEX_REPAIR_LOCK: Mutex<()> = Mutex::new(());

fn repair_codex_plugins_blocking(
    app: &AppHandle,
    lock_path: &Path,
    target_home: &Path,
    default_home: &Path,
) -> Result<codex_plugins::CodexPluginRepairReport, String> {
    let _guard = CODEX_REPAIR_LOCK
        .try_lock()
        .map_err(|_| "Codex plugin repair is already running".to_string())?;
    let lock = if lock_path.is_file() {
        codex_plugins::read_lock(lock_path)?
    } else {
        codex_plugins::empty_lock()
    };
    let codex = codex_plugins::find_binary("codex")?;
    emit_log(app, "info", &format!("Using {}", codex.display()));
    let target_runner = codex_plugins::ProcessRunner::for_binary(codex.clone())
        .with_codex_home(Some(target_home.to_path_buf()));
    let default_runner = codex_plugins::ProcessRunner::for_binary(codex)
        .with_codex_home(Some(default_home.to_path_buf()));
    let mut log = |level: &str, message: &str| emit_log(app, level, message);
    let report = codex_plugins::apply_managed_plan(
        &target_runner,
        &default_runner,
        &lock,
        target_home,
        default_home,
        &mut log,
    )?;
    let summary = format!(
        "Codex plugins — {} marketplace(s) ready, {} plugin(s) installed, {} already present, {} blocked, {} failed, {} manual",
        report.marketplaces_added.len() + report.managed_marketplaces_provisioned.len(),
        report.plugins_installed.len(),
        report.already_present.len(),
        report.blocked_plugins.len(),
        report.failed.len(),
        report.manual.len(),
    );
    let level = match report.state {
        codex_plugins::CodexRepairState::Ready => "ok",
        codex_plugins::CodexRepairState::Partial => "info",
        codex_plugins::CodexRepairState::Failed => "error",
    };
    emit_log(app, level, &summary);
    if !report.plugins_installed.is_empty() {
        emit_log(
            app,
            "info",
            "Start a new Codex task to use the newly installed plugins",
        );
    }
    Ok(report)
}

fn codex_config_has_managed_restore_intent(config_path: &Path, target_home: &Path) -> bool {
    if !config_path.is_file() {
        return false;
    }
    if !codex_config::inspect_managed_config(config_path, target_home).is_empty() {
        return true;
    }
    match fs::read(config_path) {
        Ok(bytes) => codex_config::enabled_managed_plugin_ids_from_bytes(&bytes)
            .map(|ids| !ids.is_empty())
            .unwrap_or(true),
        Err(_) => true,
    }
}

/// `npm install -g <package>` through the login shell, output streamed to
/// the log. Step zero of a setup flow when the agent CLI is missing.
fn npm_install_global_blocking(app: &AppHandle, package: &str) -> Result<(), String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let output = std::process::Command::new(&shell)
        .args(["-lc", &format!("npm install -g {}", package)])
        .output()
        .map_err(|e| format!("spawn npm: {}", e))?;
    for line in String::from_utf8_lossy(&output.stdout)
        .lines()
        .chain(String::from_utf8_lossy(&output.stderr).lines())
    {
        if !line.trim().is_empty() {
            emit_log(app, "info", &format!("  {}", line));
        }
    }
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "npm install failed — run `npm install -g {}` manually",
            package
        ))
    }
}

/// Snapshot this machine's portable Codex plugin intent into the synced
/// lock. Also runs automatically before every `.codex` push; the explicit
/// command exists for inspection and testing.
#[tauri::command]
async fn capture_codex_plugin_lock(
    app: AppHandle,
    profile: String,
) -> Result<codex_plugins::CodexPluginLock, String> {
    let config = load_sync_config(&app).unwrap_or_default();
    let (local, roots) = profile_roots(&config, &profile)?;
    require_profile_kind(&local, ".codex")?;
    checked_physical_sync_path(&roots, codex_plugins::LOCK_REL)?;
    tauri::async_runtime::spawn_blocking(move || {
        capture_codex_plugin_lock_checked(&roots)?;
        let lock_path = checked_physical_sync_path(&roots, codex_plugins::LOCK_REL)?;
        if lock_path.is_file() {
            codex_plugins::read_lock(&lock_path)
        } else {
            Ok(codex_plugins::CodexPluginLock::default())
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Non-mutating dry run of the Plugins action against the synced lock.
#[tauri::command]
async fn get_codex_plugin_plan(
    app: AppHandle,
    profile: String,
) -> Result<codex_plugins::CodexPluginPlan, String> {
    let config = load_sync_config(&app).unwrap_or_default();
    let (local, roots) = profile_roots(&config, &profile)?;
    require_profile_kind(&local, ".codex")?;
    let lock_path = checked_physical_sync_path(&roots, codex_plugins::LOCK_REL)?;
    let codex_home = codex_home_override(&roots);
    tauri::async_runtime::spawn_blocking(move || {
        codex_plugins::plan_for_lock(&lock_path, codex_home)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Non-mutating dry run of the Claude Repair action against the synced
/// lock. Pure file reads — cheap enough to drive the footer badge.
#[tauri::command]
async fn get_claude_plugin_plan(
    app: AppHandle,
    profile: String,
) -> Result<codex_plugins::CodexPluginPlan, String> {
    let config = load_sync_config(&app).unwrap_or_default();
    let (local, roots) = profile_roots(&config, &profile)?;
    require_profile_kind(&local, ".claude")?;
    let lock_path = checked_physical_sync_path(&roots, codex_plugins::CLAUDE_LOCK_REL)?;
    codex_plugins::plan_for_claude_lock(&lock_path, &roots.dir)
}

fn local_state_path(roots: &Roots) -> Result<PathBuf, String> {
    let path = roots.agent_sync().join("local-state.json");
    ensure_app_owned_path_has_no_symlinks(roots.agent_sync(), &path, "local state")?;
    Ok(path)
}

/// `~/.agent-sync/project-path-mappings.json` — the machine-local mapping
/// file (PLAN_CODEX_MANUAL_PROJECT_PATH_PICKING.md D2). Top-level like
/// `local-state.json`, outside every remap subtree, so it is structurally
/// unsyncable.
fn project_path_mappings_path(home: &Path) -> Result<PathBuf, String> {
    let top = home.join(".agent-sync");
    let path = top.join(project_paths::MAPPINGS_FILE);
    ensure_app_owned_path_has_no_symlinks(top, &path, "project path mappings")?;
    Ok(path)
}

/// Mapping resolver for one Codex profile's sidebar planning: the saved
/// manual target for a captured source path, existence-unchecked (the
/// planner validates the target and re-raises stale ones).
fn codex_project_resolver(home: &Path, profile_id: &str) -> impl Fn(&str) -> Option<String> {
    let mappings = project_path_mappings_path(home)
        .and_then(|path| project_paths::load_mappings(&path))
        .unwrap_or_default();
    let profile_id = profile_id.to_string();
    move |source: &str| {
        project_paths::target_for(&mappings, &profile_id, "codex", source).map(str::to_string)
    }
}

/// Everything one profile's readiness scan reads, resolved outside the
/// blocking task. The "other" agent dir is a guaranteed-absent path so the
/// shared scan skips that side entirely.
struct ProfileScanInput {
    profile_id: String,
    root: String,
    dir: PathBuf,
    remap: PathBuf,
    lock: PathBuf,
    sidebar_lock: Option<PathBuf>,
    codex_home: Option<PathBuf>,
}

/// The Finish-setup "treat folders as foreign" toggle. Session-only by
/// design: a persisted simulation switch left on would silently keep every
/// readiness scan lying about existing folders across restarts.
static FORCE_PATH_REMAP_UI: AtomicBool = AtomicBool::new(false);

/// Simulation switch: treat every synced source project path as foreign even
/// when it exists locally, so the Finish-setup mapping flow can be exercised
/// on one machine. On when either the UI toggle or the
/// readiness::FORCE_PATH_REMAP_ENV boot-time override is set.
fn force_path_remap() -> bool {
    FORCE_PATH_REMAP_UI.load(Ordering::Relaxed)
        || std::env::var_os(readiness::FORCE_PATH_REMAP_ENV).is_some()
}

#[tauri::command]
async fn get_force_path_remap() -> Result<bool, String> {
    Ok(force_path_remap())
}

/// Flip the session-only simulation toggle; returns the effective value
/// (stays true if the env override is set).
#[tauri::command]
async fn set_force_path_remap(enabled: bool) -> Result<bool, String> {
    FORCE_PATH_REMAP_UI.store(enabled, Ordering::Relaxed);
    Ok(force_path_remap())
}

/// Read-only post-pull readiness scan (PLAN_PORTABLE_AGENT_SETUP_V2.md §5),
/// per local profile: aggregate the plugin plans and parse this machine's
/// synced files. Never installs, writes, or trusts anything — not even
/// `local-state.json`. Issue ids are prefixed with the profile id so
/// dismissals on one profile never hide another's identical issue.
#[tauri::command]
async fn get_setup_readiness(app: AppHandle) -> Result<readiness::SetupReadiness, String> {
    let config = load_sync_config(&app).unwrap_or_default();
    let mut scans: Vec<ProfileScanInput> = Vec::new();
    let mut state_path: Option<PathBuf> = None;
    let mut mappings_path: Option<PathBuf> = None;
    let mut none_dir: Option<PathBuf> = None;
    for profile in &config.local_profiles {
        let Ok(roots) = Roots::for_profile(profile) else {
            continue;
        };
        state_path.get_or_insert(local_state_path(&roots)?);
        mappings_path.get_or_insert(project_path_mappings_path(&roots.home)?);
        // Never created; absolute so nothing resolves relative to the CWD.
        none_dir.get_or_insert(roots.agent_sync().join("__none__"));
        let is_codex = profile.root == ".codex";
        let lock_rel = if is_codex {
            codex_plugins::LOCK_REL
        } else {
            codex_plugins::CLAUDE_LOCK_REL
        };
        scans.push(ProfileScanInput {
            profile_id: profile.id.clone(),
            root: profile.root.clone(),
            dir: roots.dir.clone(),
            remap: roots.remap.clone(),
            lock: checked_physical_sync_path(&roots, lock_rel)?,
            sidebar_lock: is_codex
                .then(|| checked_physical_sync_path(&roots, codex_sidebar::LOCK_REL))
                .transpose()?,
            codex_home: is_codex.then(|| roots.dir.clone()),
        });
    }
    let (Some(state_path), Some(none_dir)) = (state_path, none_dir) else {
        return Ok(readiness::SetupReadiness {
            generated_at: now_secs(),
            roots: Vec::new(),
            issues: Vec::new(),
        });
    };
    tauri::async_runtime::spawn_blocking(move || {
        let state = readiness::load_local_state(&state_path);
        // A malformed mapping document is one actionable row, never silently
        // an empty document — resolution would quietly re-raise every mapped
        // project (PLAN_CLAUDE_PROJECT_PATH_REMAP.md §7).
        let (mappings, mappings_error) = match mappings_path
            .as_ref()
            .map(|path| project_paths::load_mappings(path))
        {
            Some(Ok(doc)) => (doc, None),
            Some(Err(error)) => (Default::default(), Some(error)),
            None => (Default::default(), None),
        };
        let resolve = |name: &str| codex_plugins::find_binary(name).is_ok();
        // The scan asks for the force switch through env_present; answer with
        // the effective value so the UI toggle counts, not just the env var.
        let env_present = |name: &str| {
            if name == readiness::FORCE_PATH_REMAP_ENV {
                force_path_remap()
            } else {
                std::env::var_os(name).is_some()
            }
        };
        let mut issues = Vec::new();
        let mut roots_summary = Vec::new();
        for scan in &scans {
            let is_codex = scan.root == ".codex";
            // The codex plan may shell out to the CLI (read-only list
            // commands); failures degrade to "no plugin section", never to
            // a scan error.
            let codex_plan = is_codex
                .then(|| codex_plugins::plan_for_lock(&scan.lock, scan.codex_home.clone()).ok())
                .flatten();
            let claude_plan = (!is_codex)
                .then(|| codex_plugins::plan_for_claude_lock(&scan.lock, &scan.dir).ok())
                .flatten();
            // The sidebar plan splits in two: the aggregate apply issue for
            // adds/titles/prefs, and one structured folder-picker candidate
            // per unmatched project (PLAN_CODEX_MANUAL_PROJECT_PATH_PICKING.md).
            let resolve_mapping = |source: &str| {
                project_paths::target_for(&mappings, &scan.profile_id, "codex", source)
                    .map(str::to_string)
            };
            let sidebar_plan = scan.sidebar_lock.as_ref().and_then(|lock| {
                codex_sidebar::pending_plan(lock, &scan.dir, &resolve_mapping, force_path_remap())
            });
            let sidebar_pending = sidebar_plan
                .as_ref()
                .filter(|plan| plan.has_changes())
                .map(|plan| plan.summary());
            let path_candidates: Vec<readiness::ProjectPathCandidate> = sidebar_plan
                .as_ref()
                .map(|plan| plan.unmatched.as_slice())
                .filter(|unmatched| !unmatched.is_empty())
                .map(|unmatched| {
                    // Rollouts are only read when something is unmatched.
                    let threads = readiness::codex_threads_by_cwd(&scan.dir);
                    unmatched
                        .iter()
                        .map(|project| {
                            let mapped_path = resolve_mapping(&project.path);
                            // An unmatched project with a saved mapping means
                            // the target is gone (a valid one would resolve).
                            let path_state = if mapped_path.is_some() {
                                "missing_target"
                            } else {
                                "unmapped"
                            };
                            readiness::ProjectPathCandidate {
                                provider: "codex".to_string(),
                                source_key: project.path.clone(),
                                source_path: project.path.clone(),
                                git_origin: project.git_origin.clone(),
                                mapped_path,
                                affected_threads: threads
                                    .get(&project.path)
                                    .cloned()
                                    .unwrap_or_default(),
                                path_state: Some(path_state.to_string()),
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            let claude_candidates = (!is_codex)
                .then(|| readiness::claude_path_candidates(&scan.dir, &mappings, &scan.profile_id))
                .unwrap_or_default();
            let mut profile_issues = readiness::scan(&readiness::ScanInput {
                codex_dir: if is_codex { &scan.dir } else { &none_dir },
                claude_dir: if is_codex { &none_dir } else { &scan.dir },
                lock_dirs: &[(scan.root.as_str(), &scan.remap)],
                codex_plan: codex_plan.as_ref(),
                claude_plan: claude_plan.as_ref(),
                state: &state,
                resolve: &resolve,
                env_present: &env_present,
                sidebar_pending: sidebar_pending.as_deref(),
                codex_path_candidates: &path_candidates,
                claude_path_candidates: &claude_candidates,
                mappings_error: (!is_codex).then_some(mappings_error.as_deref()).flatten(),
            });
            for issue in &mut profile_issues {
                issue.id = format!("{}.{}", scan.profile_id, issue.id);
                issue.profile = scan.profile_id.clone();
            }
            profile_issues.retain(|issue| {
                issue.action == "repair_codex_plugins"
                    || !state.dismissed_issues.contains(&issue.id)
            });
            roots_summary.push(readiness::RootReadiness {
                root: scan.root.clone(),
                profile: scan.profile_id.clone(),
                issues: profile_issues.len(),
            });
            issues.extend(profile_issues);
        }
        Ok(readiness::SetupReadiness {
            generated_at: now_secs(),
            roots: roots_summary,
            issues,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Everything hook bookkeeping needs from one profile: its id-prefix, the
/// per-kind dirs (the absent side is a never-created path), and the hook
/// hash resolution scoped to it.
fn hook_dirs_for(local: &LocalProfile, roots: &Roots) -> (PathBuf, PathBuf) {
    let none = roots.agent_sync().join("__none__");
    if local.root == ".codex" {
        (roots.dir.clone(), none)
    } else {
        (none, roots.dir.clone())
    }
}

/// Explicit local bookkeeping (D9): record a hook as reviewed on THIS
/// machine after the native review flow, addressed by its readiness issue
/// id (`{profile}.{hash}`). Prunes hashes whose hook no longer exists
/// locally. Trust never syncs.
#[tauri::command]
async fn mark_hook_reviewed(app: AppHandle, id: String) -> Result<(), String> {
    let config = load_sync_config(&app).unwrap_or_default();
    let mut resolved: Option<(String, Roots)> = None;
    let mut current: HashSet<String> = HashSet::new();
    for profile in &config.local_profiles {
        let Ok(roots) = Roots::for_profile(profile) else {
            continue;
        };
        let (codex_dir, claude_dir) = hook_dirs_for(profile, &roots);
        current.extend(
            readiness::hook_definitions(&codex_dir, &claude_dir)
                .into_iter()
                .map(|(_, _, hash)| hash),
        );
        if resolved.is_none() {
            if let Some(inner) = id.strip_prefix(&format!("{}.", profile.id)) {
                if let Some(hash) = readiness::hook_hash_for_issue(&codex_dir, &claude_dir, inner) {
                    resolved = Some((hash, roots));
                }
            }
        }
    }
    let (hash, roots) = resolved.ok_or("Hook no longer present — rescan")?;
    let state_path = local_state_path(&roots)?;
    let mut state = readiness::load_local_state(&state_path);
    state
        .reviewed_hooks
        .retain(|hash, _| current.contains(hash));
    state.reviewed_hooks.insert(hash, now_secs());
    readiness::save_local_state(&state_path, &state)
}

/// Explicit local bookkeeping (D9): hide a readiness issue on this machine.
#[tauri::command]
async fn dismiss_setup_issue(_app: AppHandle, id: String) -> Result<(), String> {
    let home = dirs::home_dir().ok_or("Cannot find home directory")?;
    let top = home.join(".agent-sync");
    let path = top.join("local-state.json");
    ensure_app_owned_path_has_no_symlinks(top, &path, "local state")?;
    let mut state = readiness::load_local_state(&path);
    if !state.dismissed_issues.contains(&id) {
        state.dismissed_issues.push(id);
    }
    readiness::save_local_state(&path, &state)
}

/// Resolve a reviewed conflict sibling. The explicit action publishes a
/// manifest-only deletion first, then removes the local copy. Ordinary file
/// deletions remain union-restored; only well-formed conflict siblings can use
/// this path, and unchanged replicas propagate the published resolution.
#[tauri::command]
async fn resolve_conflict_copy(app: AppHandle, source_path: String) -> Result<String, String> {
    let config = load_sync_config(&app).unwrap_or_default();
    let path = PathBuf::from(&source_path);
    if !path.is_absolute() {
        return Err("conflict path must be absolute".to_string());
    }
    let roots = roots_for_path(&config, &source_path)?;
    let local = config
        .local_profiles
        .iter()
        .find(|p| Roots::for_profile(p).is_ok_and(|r| r.dir == roots.dir))
        .cloned()
        .ok_or_else(|| format!("no profile owns '{}'", path.display()))?;
    let rel = roots.rel(&path).ok_or_else(|| {
        format!(
            "conflict path '{}' is outside the selected roots",
            path.display()
        )
    })?;
    let rel = validate_cloud_key(&rel)?;
    let checked_path = checked_physical_sync_path(&roots, &rel)?;
    if checked_path != path {
        return Err(format!(
            "conflict path '{}' is not canonical",
            path.display()
        ));
    }
    if !is_conflict_copy_rel(&rel) {
        return Err(format!("'{}' is not a conflict-copy path", rel));
    }
    if !relative_path_is_included(&rel, &profile_opt_in_union(&config, &local.id))
        || path_or_conflict_shadow_is_never_synced(&rel)
    {
        return Err(format!("'{}' is outside the sync allowlist", rel));
    }
    match fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(format!(
                "conflict path '{}' is not a regular file",
                path.display()
            ))
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "conflict path '{}' no longer exists",
                path.display()
            ))
        }
        Err(error) => return Err(format!("inspect '{}': {}", path.display(), error)),
    }
    let expected_sha256 = sha256_bytes(&read_sync_bytes(&rel, &path)?);

    // The resolution must land in EVERY storage this profile syncs with, or
    // the next pull from an unpublished one would restore the copy. The
    // local file is only deleted after all publishes succeed.
    let mut published: Vec<(String, String)> = Vec::new();
    for link in config
        .links
        .iter()
        .filter(|l| l.profile == local.id && !l.cloud.profile_id.is_empty())
    {
        let Some(storage) = config.storages.iter().find(|s| s.id == link.storage) else {
            continue;
        };
        let store = make_store(storage, Some(&roots))?;
        let conditional = ensure_conditional_capability(&app, &store, storage).await?;
        publish_conflict_resolution(
            &app,
            &store,
            &storage.id,
            &link.cloud,
            conditional,
            &rel,
            &expected_sha256,
        )
        .await?;
        published.push((storage.id.clone(), link.cloud.profile_id.clone()));
    }

    // The network/CAS round trip can take seconds. Pin the local bytes too:
    // never delete a review copy that was edited or replaced after the user
    // clicked Resolve. The cloud deletion is safe to leave published; the
    // changed local copy stays local-ahead for another explicit review.
    let checked_after_publish = checked_physical_sync_path(&roots, &rel).map_err(|error| {
        format!(
            "cloud resolution published, but local path became unsafe and was kept: {}",
            error
        )
    })?;
    if checked_after_publish != path {
        return Err(format!(
            "cloud resolution published, but local path '{}' changed mapping and was kept",
            path.display()
        ));
    }
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            let current_sha256 = sha256_bytes(&read_sync_bytes(&rel, &path)?);
            if current_sha256 != expected_sha256 {
                return Err(format!(
                    "cloud resolution published, but '{}' changed locally during Resolve and was kept",
                    path.display()
                ));
            }
        }
        Ok(_) => {
            return Err(format!(
                "cloud resolution published, but '{}' was replaced by a non-file and was kept",
                path.display()
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "cloud resolution published, but re-inspect '{}': {}",
                path.display(),
                error
            ))
        }
    }
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "cloud resolution published, but remove '{}': {}",
                path.display(),
                error
            ))
        }
    }
    for (storage_id, profile_id) in published {
        let mut baseline =
            load_baseline(&app, &local.id, &storage_id, &profile_id).unwrap_or_default();
        baseline.files.remove(&rel);
        baseline.last_push = now_secs();
        save_baseline(&app, &local.id, &storage_id, &profile_id, &baseline)?;
    }
    emit_log(&app, "ok", &format!("Conflict copy resolved: {}", rel));
    Ok("Conflict copy resolved".to_string())
}

/// Additively merge the synced sidebar lock into this machine's Codex
/// desktop state (PLAN_CODEX_THREAD_REBUILD_AND_SIDEBAR.md §4.5). Explicit
/// click only, refused while the desktop app appears to be running — it
/// rewrites the state file on quit and would clobber the merge.
#[tauri::command]
async fn apply_sidebar_state(app: AppHandle, profile: String) -> Result<String, String> {
    let config = load_sync_config(&app).unwrap_or_default();
    let (local, roots) = profile_roots(&config, &profile)?;
    require_profile_kind(&local, ".codex")?;
    if codex_desktop_is_running() {
        return Err(
            "ChatGPT/Codex desktop appears to be running — quit it, then apply".to_string(),
        );
    }
    let lock_path = checked_physical_sync_path(&roots, codex_sidebar::LOCK_REL)?;
    let codex_dir = roots.dir.clone();
    let home = roots.home.clone();
    let profile_id = local.id.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let resolve = codex_project_resolver(&home, &profile_id);
        codex_sidebar::apply_from_lock(&lock_path, &codex_dir, &resolve, force_path_remap())
    })
    .await
    .map_err(|e| e.to_string())?;
    match &result {
        Ok(summary) => emit_log(&app, "ok", &format!("Sidebar state applied — {}", summary)),
        Err(e) => emit_log(&app, "error", &format!("Sidebar apply failed: {}", e)),
    }
    result
}

/// Everything `map_project_path`/`repair_project_path_mapping` reports back
/// — display-safe facts only, tagged by provider so the UI cannot confuse
/// Codex sidebar state with Claude alias state
/// (PLAN_CLAUDE_PROJECT_PATH_REMAP.md §9).
#[derive(Serialize, Debug)]
#[serde(tag = "provider", rename_all = "lowercase")]
pub enum ProjectPathApplyReport {
    /// D4: a saved mapping and the sidebar apply are separate outcomes, so a
    /// running desktop app leaves the mapping saved with `sidebar_pending`.
    Codex {
        source_path: String,
        target_path: String,
        affected_thread_ids: Vec<String>,
        sidebar_applied: bool,
        sidebar_pending: bool,
        resume_commands: Vec<String>,
    },
    Claude {
        source_key: String,
        source_path: String,
        target_path: String,
        affected_session_ids: Vec<String>,
        alias_path: Option<String>,
        state: String,
    },
}

/// Save an explicit source → target project-path mapping
/// (PLAN_CODEX_MANUAL_PROJECT_PATH_PICKING.md §5,
/// PLAN_CLAUDE_PROJECT_PATH_REMAP.md §8.1). Codex applies the mapped sidebar
/// state unless the desktop app is running; Claude materializes one relative
/// alias symlink inside `projects/`. Neither rewrites transcripts, rollouts,
/// databases, or any cloud object. `source_key` is the shared mapping
/// identity — the Codex source path, or the Claude bucket basename.
#[tauri::command]
async fn map_project_path(
    app: AppHandle,
    profile: String,
    provider: String,
    source_key: String,
    target_path: String,
) -> Result<ProjectPathApplyReport, String> {
    match provider.as_str() {
        "codex" => map_codex_project_path(app, profile, source_key, target_path).await,
        "claude" => map_claude_project_path(app, profile, source_key, target_path).await,
        other => Err(format!("unknown provider '{}'", other)),
    }
}

async fn map_codex_project_path(
    app: AppHandle,
    profile: String,
    source_path: String,
    target_path: String,
) -> Result<ProjectPathApplyReport, String> {
    let config = load_sync_config(&app).unwrap_or_default();
    let (local, roots) = profile_roots(&config, &profile)?;
    require_profile_kind(&local, ".codex")?;
    // The frontend identifies a candidate; the synced lock decides what is
    // real. An arbitrary source path cannot be invented.
    let lock_path = checked_physical_sync_path(&roots, codex_sidebar::LOCK_REL)?;
    let lock = codex_sidebar::read_lock(&lock_path)?;
    if !lock.projects.iter().any(|p| p.path == source_path) {
        return Err(format!(
            "'{}' is not a project captured in this profile's sidebar lock",
            source_path
        ));
    }
    project_paths::validate_target_path(&target_path)?;
    let mappings_path = project_path_mappings_path(&roots.home)?;
    let mut mappings = project_paths::load_mappings(&mappings_path)?;
    project_paths::upsert(
        &mut mappings,
        project_paths::ProjectPathMapping {
            profile: local.id.clone(),
            provider: "codex".to_string(),
            source_key: source_path.clone(),
            source_path: source_path.clone(),
            target_path: target_path.clone(),
        },
    )?;
    project_paths::save_mappings(&mappings_path, &mappings)?;

    let affected_thread_ids = readiness::codex_threads_by_cwd(&roots.dir)
        .remove(&source_path)
        .unwrap_or_default();
    let resume_commands = affected_thread_ids
        .iter()
        .map(|id| format!("codex resume {} -C {}", id, target_path))
        .collect();
    let mut sidebar_applied = false;
    let mut sidebar_pending = false;

    if codex_desktop_is_running() {
        sidebar_pending = true;
        emit_log(
            &app,
            "info",
            &format!(
                "Mapping saved: {} → {} — quit ChatGPT/Codex, then apply the sidebar from Finish setup",
                source_path, target_path
            ),
        );
        return Ok(ProjectPathApplyReport::Codex {
            source_path,
            target_path,
            affected_thread_ids,
            sidebar_applied,
            sidebar_pending,
            resume_commands,
        });
    }
    let codex_dir = roots.dir.clone();
    let home = roots.home.clone();
    let profile_id = local.id.clone();
    let apply = tauri::async_runtime::spawn_blocking(move || {
        let resolve = codex_project_resolver(&home, &profile_id);
        codex_sidebar::apply_from_lock(&lock_path, &codex_dir, &resolve, force_path_remap())
    })
    .await
    .map_err(|e| e.to_string())?;
    match apply {
        Ok(summary) => {
            sidebar_applied = true;
            emit_log(
                &app,
                "ok",
                &format!(
                    "Project path mapped: {} → {} — {}",
                    source_path, target_path, summary
                ),
            );
        }
        // D4: the mapping is already saved; a failed apply stays pending
        // instead of making the user pick the folder again.
        Err(e) => {
            sidebar_pending = true;
            emit_log(
                &app,
                "error",
                &format!("Mapping saved, but sidebar apply failed: {}", e),
            );
        }
    }
    Ok(ProjectPathApplyReport::Codex {
        source_path,
        target_path,
        affected_thread_ids,
        sidebar_applied,
        sidebar_pending,
        resume_commands,
    })
}

async fn map_claude_project_path(
    app: AppHandle,
    profile: String,
    source_key: String,
    target_path: String,
) -> Result<ProjectPathApplyReport, String> {
    let config = load_sync_config(&app).unwrap_or_default();
    let (local, roots) = profile_roots(&config, &profile)?;
    require_profile_kind(&local, ".claude")?;
    if claude_cli_is_running() {
        return Err(
            "Claude Code is running — quit it before changing a project mapping".to_string(),
        );
    }

    project_paths::validate_claude_source_key(&source_key)?;
    project_paths::validate_target_path(&target_path)?;
    let probe = readiness::probe_claude_projects(&roots.dir)
        .into_iter()
        .find(|probe| probe.source_key == source_key)
        .ok_or_else(|| {
            format!(
                "'{}' is not a real Claude project bucket in this profile",
                source_key
            )
        })?;
    let source_path = probe
        .cwd_candidates
        .iter()
        .find(|cwd| project_paths::encode_claude_project_path(cwd) == source_key)
        .or_else(|| probe.cwd_candidates.first())
        .cloned()
        .unwrap_or_else(|| source_key.clone());

    let mappings_path = project_path_mappings_path(&roots.home)?;
    let previous = project_paths::load_mappings(&mappings_path)?;
    let mut next = previous.clone();
    let mapping = project_paths::ProjectPathMapping {
        profile: local.id,
        provider: "claude".to_string(),
        source_key: source_key.clone(),
        source_path: source_path.clone(),
        target_path: target_path.clone(),
    };
    project_paths::upsert(&mut next, mapping.clone())?;

    let projects_dir = roots.dir.join("projects");
    let existing_state = project_paths::claude_alias_state(&projects_dir, &mapping);
    if matches!(
        existing_state,
        project_paths::ClaudeAliasState::ConflictingDirectory
            | project_paths::ClaudeAliasState::ConflictingSymlink
            | project_paths::ClaudeAliasState::MissingSource
            | project_paths::ClaudeAliasState::MissingTarget
            | project_paths::ClaudeAliasState::PermissionDenied
    ) {
        return Err(format!(
            "cannot create Claude project mapping: {}",
            existing_state.as_str()
        ));
    }

    project_paths::save_mappings(&mappings_path, &next)?;
    let alias = match project_paths::create_claude_alias(&projects_dir, &mapping) {
        Ok(alias) => alias,
        Err(primary) => {
            let rollback = project_paths::save_mappings(&mappings_path, &previous);
            return match rollback {
                Ok(()) => Err(primary),
                Err(rollback) => Err(format!(
                    "{}; mapping rollback failed: {}",
                    primary, rollback
                )),
            };
        }
    };

    let state = project_paths::claude_alias_state(&projects_dir, &mapping);
    if !state.is_ready() {
        let mut rollback_errors = Vec::new();
        if let Err(error) = project_paths::remove_claude_alias(&projects_dir, &mapping) {
            rollback_errors.push(error);
        }
        if let Err(error) = project_paths::save_mappings(&mappings_path, &previous) {
            rollback_errors.push(format!("mapping rollback failed: {}", error));
        }
        let suffix = if rollback_errors.is_empty() {
            String::new()
        } else {
            format!("; {}", rollback_errors.join("; "))
        };
        return Err(format!(
            "Claude project alias verification failed: {}{}",
            state.as_str(),
            suffix
        ));
    }

    emit_log(
        &app,
        "ok",
        &format!(
            "Claude project path mapped: {} → {}",
            source_path, target_path
        ),
    );
    Ok(ProjectPathApplyReport::Claude {
        source_key,
        source_path,
        target_path,
        affected_session_ids: probe.session_ids,
        alias_path: alias.map(|path| path.to_string_lossy().to_string()),
        state: state.as_str().to_string(),
    })
}

/// Delete one machine-local mapping record. Never removes a project folder,
/// task, sidebar entry, or cloud object — sidebar application is additive
/// and past applies are not undone.
#[tauri::command]
async fn remove_project_path_mapping(
    profile: String,
    provider: String,
    source_path: String,
) -> Result<(), String> {
    let home = dirs::home_dir().ok_or("Cannot find home directory")?;
    let path = project_path_mappings_path(&home)?;
    let mut mappings = project_paths::load_mappings(&path)?;
    if !project_paths::remove(&mut mappings, &profile, &provider, &source_path) {
        return Err(format!("no saved mapping for '{}'", source_path));
    }
    project_paths::save_mappings(&path, &mappings)
}

/// All saved project-path mappings, for the settings editor.
#[tauri::command]
async fn list_project_path_mappings() -> Result<Vec<project_paths::ProjectPathMapping>, String> {
    let home = dirs::home_dir().ok_or("Cannot find home directory")?;
    let path = project_path_mappings_path(&home)?;
    Ok(project_paths::load_mappings(&path)?.mappings)
}

/// Reinstall missing Codex plugins from the synced lock through Codex's own
/// CLI. Explicit click only — plugins and their bundled MCP servers/hooks
/// execute code.
#[tauri::command]
async fn repair_codex_plugins(
    app: AppHandle,
    profile: String,
) -> Result<codex_plugins::CodexPluginRepairReport, String> {
    let config = load_sync_config(&app).unwrap_or_default();
    let (local, roots) = profile_roots(&config, &profile)?;
    require_profile_kind(&local, ".codex")?;
    let lock_path = checked_physical_sync_path(&roots, codex_plugins::LOCK_REL)?;
    let target_home = roots.dir.clone();
    let default_home = roots.home.join(".codex");
    let app_task = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        repair_codex_plugins_blocking(&app_task, &lock_path, &target_home, &default_home)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Bootstrap one link end-to-end: materialize the mount, pull its cloud
/// profile, and reinstall plugins into the mount via the agent's own
/// installer — offering the CLI install first when it is missing.
/// Explicit user action only.
#[tauri::command]
async fn setup_link(
    app: AppHandle,
    storage: String,
    profile: String,
) -> Result<SyncResult, String> {
    let config = load_sync_config(&app).unwrap_or_default();
    let (local, roots) = profile_roots(&config, &profile)?;
    let root = local.root.clone();
    let dir = roots.dir.clone();
    fs::create_dir_all(&dir).map_err(|e| format!("create '{}': {}", dir.display(), e))?;
    emit_log(
        &app,
        "info",
        &format!("Setting up {} at {}", root, dir.display()),
    );

    let mut result = do_pull_link(&app, &storage, &profile).await?;

    // Bootstrap case: empty profile pulled into an empty mount. The agent
    // initializes its own defaults on first launch; the app's job ends at
    // dir + profile + CLI.
    if fs::read_dir(&dir)
        .map(|mut d| d.next().is_none())
        .unwrap_or(false)
    {
        emit_log(
            &app,
            "info",
            &format!(
                "Empty profile — launch {} once to initialize this root, then Push to publish it",
                root_display_label(&root)
            ),
        );
    }

    if root == ".codex" {
        let mut plugin_state = codex_plugins::CodexRepairState::Ready;
        // Missing CLI: offer the install as step zero, streamed.
        if codex_plugins::find_binary("codex").is_err() {
            emit_log(
                &app,
                "info",
                "`codex` not found — installing the Codex CLI (npm install -g @openai/codex)…",
            );
            let app_task = app.clone();
            tauri::async_runtime::spawn_blocking(move || {
                npm_install_global_blocking(&app_task, "@openai/codex")
            })
            .await
            .map_err(|e| e.to_string())??;
        }

        let custom_mount = dir != roots.home.join(".codex");
        let lock_path = checked_physical_sync_path(&roots, codex_plugins::LOCK_REL)?;
        let target_config = dir.join("config.toml");
        let target_config_has_managed_intent =
            codex_config_has_managed_restore_intent(&target_config, &dir);
        if lock_path.is_file() || target_config_has_managed_intent {
            let target_home = dir.clone();
            let default_home = roots.home.join(".codex");
            let app_task = app.clone();
            let report = tauri::async_runtime::spawn_blocking(move || {
                repair_codex_plugins_blocking(&app_task, &lock_path, &target_home, &default_home)
            })
            .await
            .map_err(|e| e.to_string())??;
            plugin_state = report.state;
            result.setup_state = Some(report.state);
            result.message = format!(
                "{} · plugins: {} installed, {} present, {} blocked, {} failed",
                result.message,
                report.marketplaces_added.len()
                    + report.managed_marketplaces_provisioned.len()
                    + report.plugins_installed.len(),
                report.already_present.len(),
                report.blocked_plugins.len(),
                report.failed.len()
            );
        } else {
            emit_log(
                &app,
                "info",
                "No Codex plugin lock in this profile — no plugins to reinstall",
            );
        }

        let sidebar_lock = checked_physical_sync_path(&roots, codex_sidebar::LOCK_REL)?;
        let resolve = codex_project_resolver(&roots.home, &local.id);
        if let Some(summary) =
            codex_sidebar::pending_summary(&sidebar_lock, &dir, &resolve, force_path_remap())
        {
            result.message = format!("{} · sidebar setup required", result.message);
            emit_log(
                &app,
                "info",
                &format!(
                    "Root files restored, but sidebar setup remains ({}). Quit ChatGPT/Codex, then use Finish setup → Sidebar → Apply.",
                    summary
                ),
            );
        } else if custom_mount && plugin_state == codex_plugins::CodexRepairState::Ready {
            // The app can't set another process's environment; hand the
            // launch command over instead.
            emit_log(
                &app,
                "ok",
                &format!(
                    "Root ready. Launch Codex against it with: CODEX_HOME={} codex",
                    dir.display()
                ),
            );
        } else if plugin_state != codex_plugins::CodexRepairState::Ready {
            emit_log(
                &app,
                if plugin_state == codex_plugins::CodexRepairState::Failed {
                    "error"
                } else {
                    "info"
                },
                "Root files restored, but Codex plugin repair is not Ready; use Finish setup → Plugins → Repair",
            );
        }
    }

    if root == ".claude" {
        // Missing CLI: offer the install as step zero, streamed.
        if find_claude_binary().is_err() {
            emit_log(
                &app,
                "info",
                "`claude` not found — installing the Claude Code CLI (npm install -g @anthropic-ai/claude-code)…",
            );
            let app_task = app.clone();
            tauri::async_runtime::spawn_blocking(move || {
                npm_install_global_blocking(&app_task, "@anthropic-ai/claude-code")
            })
            .await
            .map_err(|e| e.to_string())??;
        }

        let custom_mount = dir != roots.home.join(".claude");
        let claude_dir = dir.clone();
        let lock_path = checked_physical_sync_path(&roots, codex_plugins::CLAUDE_LOCK_REL)?;
        let app_task = app.clone();
        let report = tauri::async_runtime::spawn_blocking(move || {
            repair_plugins_blocking(&app_task, &claude_dir, &lock_path, custom_mount)
        })
        .await
        .map_err(|e| e.to_string())??;

        if custom_mount {
            // The app can't set another process's environment; hand the
            // launch command over instead.
            emit_log(
                &app,
                "ok",
                &format!(
                    "Root ready. Launch Claude against it with: CLAUDE_CONFIG_DIR={} claude",
                    dir.display()
                ),
            );
        }
        result.message = format!(
            "{} · plugins: {} installed, {} present, {} failed",
            result.message,
            report.marketplaces_added.len() + report.plugins_installed.len(),
            report.already_present.len(),
            report.failed.len()
        );
    }
    Ok(result)
}

// ── Tauri commands ────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct CloudRootState {
    root: String,
    storage: String,
    /// Identity — the UI matches rows by this, never by the mutable label.
    profile_id: String,
    profile_label: String,
    generation: u64,
    fetched_at: u64,
}

#[derive(Serialize)]
pub struct FileStatusReport {
    /// The compared link's cloud state, when fetched; empty means statuses
    /// degraded to local-vs-baseline.
    clouds: Vec<CloudRootState>,
    statuses: HashMap<String, String>,
}

/// The profile's first configured link, or the one on `storage` when given.
fn pick_profile_link<'a>(
    config: &'a SyncConfig,
    profile_id: &str,
    storage: Option<&str>,
) -> Option<&'a SyncLink> {
    config
        .links
        .iter()
        .filter(|l| l.profile == profile_id)
        .find(|l| storage.is_none_or(|storage| l.storage == storage))
}

/// Statuses are per link: local files vs one storage's baseline + cloud
/// cache. `storage` empty = the profile's first link; an unlinked profile
/// degrades to local-only statuses.
#[tauri::command]
async fn get_file_statuses(
    app: AppHandle,
    profile: String,
    storage: Option<String>,
    paths: Vec<String>,
) -> Result<FileStatusReport, String> {
    let config = load_sync_config(&app).unwrap_or_default();
    let (local, mounts) = profile_roots(&config, &profile)?;
    let link = pick_profile_link(&config, &local.id, storage.as_deref());
    let opt_ins = link
        .and_then(|l| config.storages.iter().find(|s| s.id == l.storage))
        .map(|s| s.included_default_exclusions.clone())
        .unwrap_or_default();
    let (baseline, cache, cloud_id, label, storage_id) = match link {
        Some(link) if !link.cloud.profile_id.is_empty() => (
            load_baseline(&app, &link.profile, &link.storage, &link.cloud.profile_id)
                .unwrap_or_default(),
            load_cloud_cache(&app, &link.storage, &link.cloud.profile_id),
            link.cloud.profile_id.clone(),
            link.cloud.profile_label.clone(),
            link.storage.clone(),
        ),
        _ => (
            SyncManifest::default(),
            None,
            String::new(),
            String::new(),
            String::new(),
        ),
    };

    let file_list = collect_upload_files(&paths, &mounts, &opt_ins);
    let mut statuses = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (path, rel) in &file_list {
        let rel = normalized_relative_path(rel);
        let status = match cache.as_ref() {
            Some(cache) => matrix_status(
                path,
                &rel,
                &baseline,
                cache.files.get(&rel).map(String::as_str),
            ),
            // No cloud state yet: degrade to a local-vs-baseline comparison.
            None => file_status_at_path(path, &rel, &baseline),
        };
        statuses.insert(path.to_string_lossy().to_string(), status.to_string());
        seen.insert(rel);
    }

    // Cloud-side entries with no local file (cloud-only / local-deleted)
    // never appear in a local scan; surface them so pull-side counts and
    // deletion restores are visible before a sync runs.
    let scope: Vec<String> = paths
        .iter()
        .filter_map(|p| mounts.rel(Path::new(p)))
        .map(|rel| normalized_relative_path(&rel))
        .filter(|rel| !rel.is_empty())
        .collect();
    if let Some(cache) = &cache {
        for (rel, sha) in &cache.files {
            if seen.contains(rel)
                || !relative_path_is_included(rel, &opt_ins)
                || !rel_in_scope(rel, Some(&scope))
            {
                continue;
            }
            let Ok(path) = checked_physical_sync_path(&mounts, rel) else {
                continue;
            };
            let status = matrix_status(&path, rel, &baseline, Some(sha));
            if status != "synced" {
                statuses.insert(path.to_string_lossy().to_string(), status.to_string());
            }
        }
    }

    let clouds: Vec<CloudRootState> = cache
        .as_ref()
        .map(|cache| CloudRootState {
            root: local.root.clone(),
            storage: storage_id,
            profile_id: cloud_id,
            profile_label: label,
            generation: cache.generation,
            fetched_at: cache.fetched_at,
        })
        .into_iter()
        .collect();
    Ok(FileStatusReport { clouds, statuses })
}

/// Refetch each linked profile's head + manifest into the in-memory cloud
/// cache. Never called implicitly by status computation — only by the UI,
/// push, and pull.
#[tauri::command]
async fn refresh_cloud_state(app: AppHandle) -> Result<Vec<CloudState>, String> {
    let config = load_sync_config(&app)?;
    let resolved: Vec<&SyncLink> = config
        .links
        .iter()
        .filter(|l| !l.cloud.profile_id.is_empty())
        .collect();
    if resolved.is_empty() {
        return Err("No linked cloud profile yet — push or pull once to link".to_string());
    }
    let mut out = Vec::new();
    for link in resolved {
        let Some(storage) = config.storages.iter().find(|s| s.id == link.storage) else {
            continue;
        };
        let store = make_store(storage, None)?;
        let Some((head, _)) = fetch_head(&store, &link.cloud.profile_id).await? else {
            return Err(format!(
                "profile '{}' has no _head.json in storage '{}'",
                link.cloud.profile_id,
                storage_display_name(storage)
            ));
        };
        let manifest = fetch_cloud_manifest(&store, &link.cloud.profile_id, &head).await?;
        let cache =
            cache_from_manifest(&head, &manifest.files, &storage.id, &link.cloud.profile_id);
        out.push(CloudState {
            storage: storage.id.clone(),
            profile: link.profile.clone(),
            root: link.cloud.root.clone(),
            profile_label: link.cloud.profile_label.clone(),
            generation: cache.generation,
            commit_id: cache.commit_id.clone(),
            fetched_at: cache.fetched_at,
            files: cache.files.len() as u64,
        });
        store_cloud_cache(&app, cache);
    }
    Ok(out)
}

/// Cloud profiles present in one storage — the link editor's pin picker.
#[tauri::command]
async fn list_sync_profiles(app: AppHandle, storage: String) -> Result<Vec<ProfileInfo>, String> {
    let config = load_sync_config(&app)?;
    let storage = config
        .storages
        .iter()
        .find(|s| s.id == storage)
        .ok_or_else(|| format!("unknown storage '{}'", storage))?;
    let store = make_store(storage, None)?;
    discover_profiles(&store).await
}

#[tauri::command]
async fn sync_upload(
    app: AppHandle,
    control: tauri::State<'_, UploadControl>,
    storage: String,
    profile: String,
    files: Vec<String>,
) -> Result<SyncResult, String> {
    control.paused.store(false, Ordering::SeqCst);
    let paused = control.paused.clone();
    let result = do_push_link(&app, paused, &storage, &profile, &files).await;
    control.paused.store(false, Ordering::SeqCst);
    result
}

#[tauri::command]
async fn set_upload_paused(
    app: AppHandle,
    control: tauri::State<'_, UploadControl>,
    paused: bool,
) -> Result<(), String> {
    control.paused.store(paused, Ordering::SeqCst);
    emit_log(
        &app,
        "info",
        if paused {
            "Pause requested; waiting for current file to finish"
        } else {
            "Resume requested"
        },
    );
    Ok(())
}

#[tauri::command]
async fn sync_download(
    app: AppHandle,
    storage: String,
    profile: String,
) -> Result<SyncResult, String> {
    do_pull_link(&app, &storage, &profile).await
}

fn emit_progress(app: &AppHandle, done: usize, total: usize) {
    let _ = app.emit(
        "sync-progress",
        serde_json::json!({ "done": done, "total": total }),
    );
}

pub(crate) fn emit_log<R: tauri::Runtime>(app: &tauri::AppHandle<R>, level: &str, message: &str) {
    activity_log::emit_typed_log(app, activity_log::ActivityLogType::System, level, message);
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_major_version_parses_cli_output() {
        assert_eq!(claude_major_version("2.1.206 (Claude Code)"), Some(2));
        assert_eq!(claude_major_version("1.0.73 (Claude Code)"), Some(1));
        assert_eq!(claude_major_version("not a version"), None);
    }

    #[test]
    fn unlisted_paths_need_opt_in_but_plugin_state_never_syncs() {
        let config: Vec<String> = Vec::new();
        assert!(!relative_path_is_included(".codex/.tmp", &config));
        assert!(!relative_path_is_included(
            ".codex/plugins/cache/plugin/file.json",
            &config
        ));
        assert!(!relative_path_is_included(
            ".codex/models_cache.json",
            &config
        ));
        assert!(!relative_path_is_included(".codex/cache-old/file", &config));
        assert!(!relative_path_is_included(".claude/cache/file", &config));
        assert!(!relative_path_is_included(
            ".claude/plugins/installed_plugins.json",
            &config
        ));
        assert!(!relative_path_is_included(
            ".codex/config.toml.bak-1",
            &config
        ));

        let config = vec![
            ".codex/.tmp".to_string(),
            ".codex/plugins/cache".to_string(),
            ".claude/plugins/installed_plugins.json".to_string(),
        ];
        assert!(!relative_path_is_included(
            ".codex/plugins/cache/plugin/file.json",
            &config
        ));
        assert!(relative_path_is_included(
            ".claude/plugins/installed_plugins.json",
            &config
        ));
        assert!(!relative_path_is_included(".codex/.tmp/file", &config));
    }

    #[test]
    fn allowlist_tiers_sync_by_default() {
        let config: Vec<String> = Vec::new();
        for path in [
            ".codex/sessions/2026/07/05/rollout-x.jsonl",
            ".codex/archived_sessions/rollout-y.jsonl",
            ".codex/session_index.jsonl",
            ".codex/history.jsonl",
            ".codex/memories/notes.md",
            ".codex/rules/default.rules",
            ".codex/prompts/review.md",
            ".codex/AGENTS.md",
            ".codex/hooks.json",
            ".codex/config.toml",
            ".codex/agent-sync/codex-plugins.lock.json",
            ".claude/agent-sync/claude-plugins.lock.json",
            ".claude/projects/-Users-x-proj/abc.jsonl",
            ".claude/projects/-Users-x-proj/abc/tool-results/r.json",
            ".claude/projects/-Users-x-proj/memory/MEMORY.md",
            ".claude/history.jsonl",
            ".claude/file-history/session/file",
            ".claude/todos/todo.json",
            ".claude/CLAUDE.md",
            ".claude/agents/custom.md",
            ".claude/commands/cmd.md",
            ".claude/keybindings.json",
            ".claude/settings.json",
            ".claude/plugins/config.json",
        ] {
            assert!(relative_path_is_included(path, &config), "{}", path);
        }
        // Global skills are schema-3-owned custom-skill snapshots now; the
        // legacy profile allowlist no longer syncs them by default.
        for path in [".codex/skills/foo/SKILL.md", ".claude/skills/s/SKILL.md"] {
            assert!(!relative_path_is_included(path, &config), "{}", path);
        }
    }

    #[test]
    fn conflict_copies_inherit_the_shadowed_files_eligibility() {
        let config: Vec<String> = Vec::new();
        assert!(relative_path_is_included(
            ".codex/AGENTS.sync-conflict-aabbccdd.md",
            &config
        ));
        assert!(relative_path_is_included(
            ".codex/config.sync-conflict-00112233.toml",
            &config
        ));
        let nested = conflict_copy_rel(
            &conflict_copy_rel(".codex/AGENTS.md", "aabbccdd"),
            "00112233",
        );
        assert!(relative_path_is_included(&nested, &config));
        assert!(!relative_path_is_included(
            ".codex/models_cache.sync-conflict-aabbccdd.json",
            &config
        ));
        assert_eq!(
            strip_conflict_marker(".codex/rules.sync-conflict-aabbccdd"),
            ".codex/rules"
        );
        assert_eq!(
            strip_conflict_marker(".codex/AGENTS.md"),
            ".codex/AGENTS.md"
        );
        // Not a real marker (wrong tag length/charset) — untouched.
        assert_eq!(
            strip_conflict_marker(".codex/a.sync-conflict-zz.md"),
            ".codex/a.sync-conflict-zz.md"
        );
        // Seven hex bytes followed by a multibyte character is neither a
        // marker nor a valid UTF-8 slice boundary.
        assert_eq!(
            strip_conflict_marker(".codex/a.sync-conflict-abcdef0é.md"),
            ".codex/a.sync-conflict-abcdef0é.md"
        );
        // An empty stem plus arbitrary trailing text used to strip to the
        // allowlisted `.codex/AGENTS.md` path.
        assert_eq!(
            strip_conflict_marker(".codex/.sync-conflict-deadbeefAGENTS.md"),
            ".codex/.sync-conflict-deadbeefAGENTS.md"
        );
        assert!(!relative_path_is_included(
            ".codex/.sync-conflict-deadbeefAGENTS.md",
            &config
        ));
        for malformed in [
            ".codex/.sync-conflict-deadbeef",
            ".codex/AGENTS.sync-conflict-deadbeefmd",
            ".codex/AGENTS.sync-conflict-deadbeef-md",
        ] {
            assert_eq!(strip_conflict_marker(malformed), malformed);
        }
        assert!(!is_conflict_copy_rel(
            ".codex/agents/review.sync-conflict-deadbeef/ordinary.md"
        ));
        assert!(!relative_path_is_included(
            ".codex/agents.sync-conflict-deadbeef/private.txt",
            &config
        ));
    }

    #[test]
    fn walk_ancestors_of_included_paths_stay_traversable() {
        let config: Vec<String> = Vec::new();
        assert!(dir_may_contain_included(".codex", &config));
        assert!(dir_may_contain_included(".claude/plugins", &config));
        assert!(dir_may_contain_included(".codex/sessions/2026", &config));
        assert!(!dir_may_contain_included(".codex/cache", &config));
        assert!(!dir_may_contain_included(".codex/.tmp", &config));
        assert!(!dir_may_contain_included(".codex/plugins/cache", &config));
        assert!(!dir_may_contain_included(".claude/plugins/repos", &config));
        assert!(!dir_may_contain_included(".codex/memories/.git", &config));

        let config = vec![".codex/cache/tool".to_string()];
        assert!(dir_may_contain_included(".codex/cache", &config));
    }

    #[test]
    fn old_configs_default_to_excluding_recreatable_paths() {
        let storage: StorageConfig =
            serde_json::from_str("{\"id\":\"s1\",\"kind\":\"s3\"}").unwrap();
        assert!(storage.included_default_exclusions.is_empty());
        assert!(!relative_path_is_included(
            ".codex/.local/bin/tool",
            &storage.included_default_exclusions
        ));
    }

    #[test]
    fn runtime_databases_are_opt_in_and_sidecars_never_sync() {
        let config: Vec<String> = Vec::new();
        assert!(!relative_path_is_included(".codex/state_5.sqlite", &config));
        assert!(!relative_path_is_included(
            ".codex/memories_1.sqlite",
            &config
        ));
        assert!(!relative_path_is_included(
            ".codex/state_5.sqlite-wal",
            &config
        ));
        assert!(!relative_path_is_included(
            ".codex/sqlite/state_5.sqlite",
            &config
        ));

        let config = vec![
            ".codex/state_5.sqlite*".to_string(),
            ".codex/memories_1.sqlite*".to_string(),
        ];
        assert!(relative_path_is_included(".codex/state_5.sqlite", &config));
        assert!(relative_path_is_included(
            ".codex/memories_1.sqlite",
            &config
        ));
        // Sidecars are excluded structurally, opt-in or not.
        assert!(!relative_path_is_included(
            ".codex/state_5.sqlite-wal",
            &config
        ));
        assert!(!relative_path_is_included(
            ".codex/memories_1.sqlite-journal",
            &config
        ));
    }

    #[test]
    fn sqlite_upload_snapshot_includes_wal_data() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("source.sqlite");
        let source = Connection::open(&source_path).unwrap();
        source
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE values_to_restore (value TEXT NOT NULL);
                 INSERT INTO values_to_restore VALUES ('from WAL');",
            )
            .unwrap();

        let snapshot = read_upload_data(&source_path).unwrap();
        let snapshot_path = dir.path().join("restored.sqlite");
        fs::write(&snapshot_path, &snapshot).unwrap();
        let restored = Connection::open(snapshot_path).unwrap();
        let value: String = restored
            .query_row("SELECT value FROM values_to_restore", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "from WAL");
    }

    #[test]
    fn config_status_and_reads_use_portable_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let physical_a = br#"model = "gpt-a"

[plugins."sites@openai-bundled"]
enabled = true

[marketplaces.openai-bundled]
source_type = "local"
source = "/machine-a/.codex/.tmp/bundled-marketplaces/openai-bundled"
"#;
        fs::write(&path, physical_a).unwrap();
        let portable = read_sync_bytes(codex_config::CONFIG_REL, &path).unwrap();
        let portable_text = String::from_utf8(portable.clone()).unwrap();
        assert!(portable_text.contains("gpt-a"));
        assert!(portable_text.contains("sites@openai-bundled"));
        assert!(!portable_text.contains("machine-a"));

        let mut baseline = SyncManifest::default();
        baseline.files.insert(
            codex_config::CONFIG_REL.to_string(),
            file_record(&path, &portable),
        );
        assert_eq!(
            file_status_at_path(&path, codex_config::CONFIG_REL, &baseline),
            "synced"
        );

        // Changing only the target-owned overlay does not create sync drift.
        let physical_b =
            String::from_utf8_lossy(physical_a).replace("machine-a", "machine-b-longer");
        fs::write(&path, physical_b).unwrap();
        assert_eq!(
            file_status_at_path(&path, codex_config::CONFIG_REL, &baseline),
            "synced"
        );

        // A portable preference change remains visible.
        let physical_c = fs::read_to_string(&path).unwrap().replace("gpt-a", "gpt-c");
        fs::write(&path, physical_c).unwrap();
        assert_eq!(
            file_status_at_path(&path, codex_config::CONFIG_REL, &baseline),
            "modified"
        );

        let conflict_rel = conflict_copy_rel(codex_config::CONFIG_REL, "aabbccddeeff0011");
        assert!(codex_config::is_config_artifact(&conflict_rel));
        assert!(read_sync_bytes(&conflict_rel, &path).is_ok());

        fs::write(&path, b"[malformed").unwrap();
        assert!(read_sync_bytes(codex_config::CONFIG_REL, &path).is_err());
        assert_eq!(
            file_status_at_path(&path, codex_config::CONFIG_REL, &baseline),
            "modified"
        );
    }

    #[test]
    fn active_config_apply_composes_target_overlay_but_conflicts_stay_portable() {
        let home = tempfile::tempdir().unwrap();
        let roots = Roots::for_home(home.path());
        let active = roots.abs(codex_config::CONFIG_REL);
        fs::create_dir_all(active.parent().unwrap()).unwrap();
        let current = br#"model = "old"

[marketplaces.openai-bundled]
source_type = "local"
source = "/machine-b/.codex/.tmp/bundled-marketplaces/openai-bundled"
"#;
        fs::write(&active, current).unwrap();
        let incoming = codex_config::project_portable_bytes(
            br#"model = "new"

[plugins."sites@openai-bundled"]
enabled = true
"#,
        )
        .unwrap();

        let record = apply_cloud_bytes(&roots, codex_config::CONFIG_REL, &incoming, 0).unwrap();
        let composed = fs::read_to_string(&active).unwrap();
        assert!(composed.contains("model = \"new\""));
        assert!(composed.contains("machine-b"));
        assert!(composed.contains("sites@openai-bundled"));
        assert_eq!(record.sha256, sha256_bytes(&incoming));
        assert_eq!(record.size, incoming.len() as u64);
        assert_eq!(
            read_sync_bytes(codex_config::CONFIG_REL, &active).unwrap(),
            incoming
        );

        let conflict_rel = conflict_copy_rel(codex_config::CONFIG_REL, "0011223344556677");
        apply_cloud_bytes(&roots, &conflict_rel, &incoming, 0).unwrap();
        assert_eq!(fs::read(roots.abs(&conflict_rel)).unwrap(), incoming);
    }

    #[cfg(unix)]
    #[test]
    fn cloud_apply_rejects_symlinked_descendants_of_the_physical_root() {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let roots = Roots::for_home(home.path());
        fs::create_dir_all(home.path().join(".codex")).unwrap();
        symlink(outside.path(), home.path().join(".codex/agents")).unwrap();

        let error = apply_cloud_bytes(&roots, ".codex/agents/cloud.md", b"cloud", 0)
            .unwrap_err()
            .to_string();

        assert!(error.contains("traverses symlink"), "{error}");
        assert!(!outside.path().join("cloud.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn cloud_apply_does_not_follow_a_planted_temp_symlink() {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let roots = Roots::for_home(home.path());
        let destination = roots.abs(".codex/agents/note.md");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        let outside_file = outside.path().join("keep.txt");
        fs::write(&outside_file, b"keep").unwrap();
        let planted = destination.parent().unwrap().join(".note.md.sync-tmp");
        symlink(&outside_file, &planted).unwrap();

        apply_cloud_bytes(&roots, ".codex/agents/note.md", b"cloud", 0).unwrap();

        assert_eq!(fs::read(&destination).unwrap(), b"cloud");
        assert_eq!(fs::read(&outside_file).unwrap(), b"keep");
        assert!(fs::symlink_metadata(&planted)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn backup_copy_never_follows_parent_or_final_symlinks() {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let roots = Roots::for_home(home.path());
        let rel = ".codex/agents/note.md";
        let source = roots.abs(rel);
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, b"local").unwrap();

        let parent_run = tempfile::tempdir().unwrap();
        symlink(outside.path(), parent_run.path().join(".codex")).unwrap();
        assert!(backup_local_file(parent_run.path(), &roots, rel).is_err());
        assert!(!outside.path().join("agents/note.md").exists());

        let final_run = tempfile::tempdir().unwrap();
        let final_parent = final_run.path().join(".codex/agents");
        fs::create_dir_all(&final_parent).unwrap();
        let outside_file = outside.path().join("keep.md");
        fs::write(&outside_file, b"keep").unwrap();
        symlink(&outside_file, final_parent.join("note.md")).unwrap();
        assert!(backup_local_file(final_run.path(), &roots, rel).is_err());
        assert_eq!(fs::read(&outside_file).unwrap(), b"keep");
    }

    #[cfg(unix)]
    #[test]
    fn physical_path_guard_covers_remapped_agent_sync_files() {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let roots = Roots::for_home(home.path());
        let remapped_root = home.path().join(".agent-sync/codex");
        fs::create_dir_all(&remapped_root).unwrap();
        symlink(outside.path(), remapped_root.join("nested")).unwrap();

        let error =
            checked_physical_sync_path(&roots, ".codex/agent-sync/nested/codex-plugins.lock.json")
                .unwrap_err();

        assert!(error.contains("traverses symlink"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn remapped_agent_sync_slug_symlink_blocks_upload_and_cloud_apply() {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let roots = Roots::for_home(home.path());
        fs::create_dir_all(home.path().join(".agent-sync")).unwrap();
        fs::write(
            outside.path().join("codex-plugins.lock.json"),
            b"outside secret",
        )
        .unwrap();
        symlink(outside.path(), home.path().join(".agent-sync/codex")).unwrap();
        let lock_path = roots.abs(codex_plugins::LOCK_REL);

        assert!(
            collect_upload_files(&[lock_path.to_string_lossy().to_string()], &roots, &[],)
                .is_empty()
        );
        let error = apply_cloud_bytes(&roots, codex_plugins::LOCK_REL, b"cloud", 0)
            .unwrap_err()
            .to_string();

        assert!(error.contains("traverses symlink"), "{error}");
        assert_eq!(
            fs::read(outside.path().join("codex-plugins.lock.json")).unwrap(),
            b"outside secret"
        );
    }

    #[test]
    fn config_marketplace_collision_preserves_active_and_writes_portable_conflict() {
        let home = tempfile::tempdir().unwrap();
        let roots = Roots::for_home(home.path());
        let active = roots.abs(codex_config::CONFIG_REL);
        fs::create_dir_all(active.parent().unwrap()).unwrap();
        let current = br#"model = "local"

[marketplaces.team]
source_type = "local"
source = "/target/team"
"#;
        fs::write(&active, current).unwrap();
        let incoming = codex_config::project_portable_bytes(
            br#"model = "cloud"

[marketplaces.team]
source_type = "git"
source = "owner/team"
"#,
        )
        .unwrap();

        let error = apply_cloud_bytes(&roots, codex_config::CONFIG_REL, &incoming, 0).unwrap_err();
        assert!(matches!(
            error,
            ApplyCloudError::ConfigMarketplaceCollision(ref name) if name == "team"
        ));
        assert_eq!(fs::read(&active).unwrap(), current);

        let logical_sha = sha256_bytes(&incoming);
        let mut baseline = SyncManifest::default();
        let copy_rel = preserve_cloud_config_conflict(
            &roots,
            codex_config::CONFIG_REL,
            &incoming,
            &logical_sha,
            &logical_sha,
            0,
            &mut baseline,
        )
        .unwrap();

        assert_eq!(
            copy_rel,
            conflict_copy_rel(codex_config::CONFIG_REL, &logical_sha)
        );
        assert_eq!(fs::read(roots.abs(&copy_rel)).unwrap(), incoming);
        assert_eq!(fs::read(&active).unwrap(), current);
        assert_eq!(
            classify_path(
                local_state_at(&active, codex_config::CONFIG_REL, &baseline),
                Some(&logical_sha),
                baseline.files.get(codex_config::CONFIG_REL),
            ),
            SyncAction::UploadLocal
        );
    }

    #[test]
    fn deterministic_conflict_copy_never_overwrites_an_edited_review() {
        let home = tempfile::tempdir().unwrap();
        let roots = Roots::for_home(home.path());
        let rel = ".codex/AGENTS.sync-conflict-aabbccdd.md";
        let path = roots.abs(rel);

        apply_conflict_copy_bytes(&roots, rel, b"cloud side\n", 0).unwrap();
        // Repeating the same reconciliation is an idempotent no-op.
        apply_conflict_copy_bytes(&roots, rel, b"cloud side\n", 0).unwrap();

        fs::write(&path, b"locally reviewed\n").unwrap();
        let error = apply_conflict_copy_bytes(&roots, rel, b"cloud side\n", 0).unwrap_err();
        assert!(error.to_string().contains("differs"));
        assert_eq!(fs::read(&path).unwrap(), b"locally reviewed\n");
    }

    #[test]
    fn raw_cloud_config_baseline_converges_pull_and_republishes_on_push() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        let logical = codex_config::project_portable_bytes(b"model = 'portable'\n").unwrap();
        fs::write(&path, &logical).unwrap();
        let raw_cloud_sha = sha256_bytes(b"raw cloud bytes with a local overlay");
        let record = record_cloud_object_sha(file_record(&path, &logical), &raw_cloud_sha);
        let mut baseline = SyncManifest::default();
        baseline
            .files
            .insert(codex_config::CONFIG_REL.to_string(), record);
        let record = baseline.files.get(codex_config::CONFIG_REL).unwrap();

        assert_eq!(
            local_state_at(&path, codex_config::CONFIG_REL, &baseline),
            LocalState::Unchanged
        );
        assert_eq!(
            classify_path(LocalState::Unchanged, Some(&raw_cloud_sha), Some(record)),
            SyncAction::Skip
        );
        assert_eq!(
            matrix_status(
                &path,
                codex_config::CONFIG_REL,
                &baseline,
                Some(&raw_cloud_sha)
            ),
            "synced"
        );
        assert!(!push_needs_projection_republish(SyncMode::Pull, record));
        assert!(push_needs_projection_republish(SyncMode::Push, record));

        let restored: SyncManifest =
            serde_json::from_str(&serde_json::to_string(&baseline).unwrap()).unwrap();
        assert_eq!(
            restored.files[codex_config::CONFIG_REL]
                .cloud_object_sha256
                .as_deref(),
            Some(raw_cloud_sha.as_str())
        );
    }

    #[test]
    fn malformed_cloud_config_never_overwrites_the_active_file() {
        let home = tempfile::tempdir().unwrap();
        let roots = Roots::for_home(home.path());
        let active = roots.abs(codex_config::CONFIG_REL);
        fs::create_dir_all(active.parent().unwrap()).unwrap();
        let original = b"model = \"safe\"\n";
        fs::write(&active, original).unwrap();

        assert!(apply_cloud_bytes(&roots, codex_config::CONFIG_REL, b"[broken", 0).is_err());
        assert_eq!(fs::read(active).unwrap(), original);
    }

    #[test]
    fn codex_setup_runs_plugin_repair_only_for_managed_config_intent() {
        let home = tempfile::tempdir().unwrap();
        let codex_home = home.path().join(".codex");
        let config = codex_home.join("config.toml");
        fs::create_dir_all(&codex_home).unwrap();

        fs::write(&config, "model = \"gpt-5\"\n").unwrap();
        assert!(!codex_config_has_managed_restore_intent(
            &config,
            &codex_home
        ));

        fs::write(
            &config,
            "[plugins.\"sites@openai-bundled\"]\nenabled = true\n",
        )
        .unwrap();
        assert!(codex_config_has_managed_restore_intent(
            &config,
            &codex_home
        ));

        fs::write(&config, "[broken\n").unwrap();
        assert!(codex_config_has_managed_restore_intent(
            &config,
            &codex_home
        ));
    }

    #[test]
    fn never_sync_paths_are_hard_denied_even_with_opt_ins() {
        let config = vec![
            ".codex/auth.json".to_string(),
            ".codex/.tmp".to_string(),
            ".codex/plugins/cache".to_string(),
            ".codex/.TMP".to_string(),
            ".codex/Plugins/Cache".to_string(),
            ".codex/memories/.GIT".to_string(),
        ];
        assert!(!relative_path_is_included(".codex/auth.json", &config));
        assert!(!relative_path_is_included(
            ".codex/auth.json.bak-2026",
            &config
        ));
        assert!(!relative_path_is_included(
            ".codex/installation_id",
            &config
        ));
        assert!(!relative_path_is_included(
            ".claude/.credentials.json",
            &config
        ));
        assert!(!relative_path_is_included(
            ".claude/settings.local.json",
            &config
        ));
        assert!(!relative_path_is_included(
            ".claude/sessions/live.json",
            &config
        ));
        assert!(!relative_path_is_included(
            ".codex/.tmp/plugins/.agents/plugins/marketplace.json",
            &config
        ));
        assert!(!relative_path_is_included(
            ".codex/plugins/cache/openai-bundled/sites/1/.mcp.json",
            &config
        ));
        assert!(!relative_path_is_included(
            ".codex/.TMP/plugins/catalog.json",
            &config
        ));
        assert!(!relative_path_is_included(
            ".codex/Plugins/Cache/openai-bundled/plugin.json",
            &config
        ));
        assert!(!relative_path_is_included(
            ".claude/plugins/repos/a/b",
            &config
        ));
        assert!(!relative_path_is_included(
            ".claude/plugins/marketplaces/a/b",
            &config
        ));
        assert!(!relative_path_is_included(
            ".codex/memories/.git/HEAD",
            &config
        ));
        assert!(!relative_path_is_included(
            ".codex/sessions/.DS_Store",
            &config
        ));
        assert!(!relative_path_is_included(
            ".codex/memories/.GIT/config",
            &config
        ));

        assert!(relative_path_is_included(
            ".claude/plugins/config.json",
            &config
        ));
        assert!(relative_path_is_included(
            ".codex/sessions/2026/07/rollout-x.jsonl",
            &config
        ));
        assert!(relative_path_is_included(".codex/history.jsonl", &config));
    }

    #[test]
    fn conflict_markers_cannot_bypass_the_never_tier() {
        let config = vec![".codex".to_string()];
        for path in [
            ".codex/auth.sync-conflict-aabbccdd.json",
            ".codex/.tmp.sync-conflict-aabbccdd/plugins/catalog.json",
            ".codex/plugins/cache.sync-conflict-aabbccdd/plugin/file.json",
            ".codex/plugins/cache.sync-conflict-AABBCCDD/plugin/file.json",
            ".codex/plugins/cache.sync-conflict-aabbccdd/plugin.sync-conflict-00112233.json",
        ] {
            assert!(!relative_path_is_included(path, &config), "{}", path);
            assert!(path_or_conflict_shadow_is_never_synced(path), "{}", path);
        }

        let home = tempfile::tempdir().unwrap();
        let roots = Roots::for_home(home.path());
        let forbidden = ".codex/plugins/cache.sync-conflict-aabbccdd/plugin.json";
        assert!(apply_cloud_bytes(&roots, forbidden, b"payload", 0).is_err());
        assert!(!roots.abs(forbidden).exists());
    }

    #[test]
    fn cloud_keys_are_validated_before_touching_home() {
        assert_eq!(
            validate_cloud_key(".codex/config.toml").unwrap(),
            ".codex/config.toml"
        );
        assert!(validate_cloud_key(".claude/projects/a/b.jsonl").is_ok());
        assert!(validate_cloud_key("../etc/passwd").is_err());
        assert!(validate_cloud_key(".codex/../../etc/passwd").is_err());
        assert!(validate_cloud_key("/etc/passwd").is_err());
        assert!(validate_cloud_key(".codex\\evil").is_err());
        assert!(validate_cloud_key("other/file").is_err());
        assert!(validate_cloud_key(".codex/a//b").is_err());
        assert!(validate_cloud_key(".codex").is_err());
        assert!(validate_cloud_key(".codex/a\u{0}b").is_err());
    }

    #[test]
    fn history_merge_is_a_deterministic_union() {
        let a = "{\"ts\":2,\"text\":\"b\"}\n{\"ts\":1,\"text\":\"a\"}\n";
        let b = "{\"ts\":1,\"text\":\"a\"}\n{\"ts\":3,\"text\":\"c\"}\n";
        let merged = merge_history_jsonl(a, b);
        assert_eq!(
            merged,
            "{\"ts\":1,\"text\":\"a\"}\n{\"ts\":2,\"text\":\"b\"}\n{\"ts\":3,\"text\":\"c\"}\n"
        );
        assert_eq!(merged, merge_history_jsonl(b, a));

        // Claude history uses `timestamp` instead of `ts`.
        let c = "{\"timestamp\":5,\"display\":\"x\"}\n";
        let d = "{\"timestamp\":4,\"display\":\"y\"}\n";
        assert_eq!(
            merge_history_jsonl(c, d),
            "{\"timestamp\":4,\"display\":\"y\"}\n{\"timestamp\":5,\"display\":\"x\"}\n"
        );

        // Equal timestamps break ties by line bytes.
        let e = "{\"ts\":1,\"text\":\"b\"}\n";
        let f = "{\"ts\":1,\"text\":\"a\"}\n";
        assert_eq!(
            merge_history_jsonl(e, f),
            "{\"ts\":1,\"text\":\"a\"}\n{\"ts\":1,\"text\":\"b\"}\n"
        );
    }

    #[test]
    fn session_index_merge_keeps_newest_record_per_id_and_caps() {
        let local = "{\"id\":\"a\",\"updated_at\":10,\"thread_name\":\"old\"}\n{\"id\":\"b\",\"updated_at\":5}\n";
        let cloud = "{\"id\":\"a\",\"updated_at\":20,\"thread_name\":\"new\"}\n{\"id\":\"c\",\"updated_at\":7}\n";
        let merged = merge_session_index_jsonl(local, cloud);
        assert_eq!(
            merged,
            "{\"id\":\"b\",\"updated_at\":5}\n{\"id\":\"c\",\"updated_at\":7}\n{\"id\":\"a\",\"updated_at\":20,\"thread_name\":\"new\"}\n"
        );
        assert_eq!(merged, merge_session_index_jsonl(cloud, local));

        let lines: Vec<String> = (0..150)
            .map(|i| format!("{{\"id\":\"s{:03}\",\"updated_at\":{}}}", i, i))
            .collect();
        let big = lines.join("\n");
        let merged = merge_session_index_jsonl(&big, "");
        assert_eq!(merged.lines().count(), SESSION_INDEX_CAP);
        assert!(merged.lines().next().unwrap().contains("\"updated_at\":50"));
        assert!(merged
            .lines()
            .last()
            .unwrap()
            .contains("\"updated_at\":149"));
    }

    #[test]
    fn conflict_copy_names_are_deterministic() {
        let sha = "aabbccddeeff00112233";
        assert_eq!(
            conflict_copy_rel(".codex/config.toml", sha),
            ".codex/config.sync-conflict-aabbccdd.toml"
        );
        assert_eq!(
            conflict_copy_rel(".codex/AGENTS.md", sha),
            ".codex/AGENTS.sync-conflict-aabbccdd.md"
        );
        assert_eq!(
            conflict_copy_rel(".codex/rules", sha),
            ".codex/rules.sync-conflict-aabbccdd"
        );
        assert_eq!(
            conflict_copy_rel(".claude/.gitignore", sha),
            ".claude/.gitignore.sync-conflict-aabbccdd"
        );
        assert_eq!(
            conflict_copy_rel("config.toml", sha),
            "config.sync-conflict-aabbccdd.toml"
        );
    }

    #[test]
    fn classify_path_implements_the_union_matrix() {
        let baseline = |sha: &str| FileRecord {
            sha256: sha.to_string(),
            size: 1,
            mtime: 1,
            cloud_object_sha256: None,
        };

        // Gone on both sides → forget; never seen → nothing.
        assert_eq!(
            classify_path(LocalState::Missing, None, Some(&baseline("s1"))),
            SyncAction::DropRecord
        );
        assert_eq!(
            classify_path(LocalState::Missing, None, None),
            SyncAction::Skip
        );

        // Union restores deletions instead of propagating them.
        assert_eq!(
            classify_path(LocalState::Missing, Some("s1"), None),
            SyncAction::ApplyCloud
        );
        assert_eq!(
            classify_path(LocalState::Missing, Some("s1"), Some(&baseline("s1"))),
            SyncAction::ApplyCloud
        );
        assert_eq!(
            classify_path(LocalState::Unchanged, None, Some(&baseline("s1"))),
            SyncAction::UploadLocal
        );

        assert_eq!(
            classify_path(LocalState::Changed, None, None),
            SyncAction::UploadLocal
        );
        assert_eq!(
            classify_path(LocalState::Unchanged, Some("s1"), Some(&baseline("s1"))),
            SyncAction::Skip
        );
        assert_eq!(
            classify_path(LocalState::Unchanged, Some("s2"), Some(&baseline("s1"))),
            SyncAction::ApplyCloud
        );
        assert_eq!(
            classify_path(LocalState::Changed, Some("s1"), Some(&baseline("s1"))),
            SyncAction::UploadLocal
        );
        assert_eq!(
            classify_path(LocalState::Changed, Some("s2"), Some(&baseline("s1"))),
            SyncAction::Reconcile
        );
        // Never synced but present on both sides → verify by content.
        assert_eq!(
            classify_path(LocalState::Changed, Some("s1"), None),
            SyncAction::Reconcile
        );
    }

    #[test]
    fn cloud_keys_reject_windows_hazards() {
        assert!(validate_cloud_key(".codex/NUL").is_err());
        assert!(validate_cloud_key(".codex/nul.txt").is_err());
        assert!(validate_cloud_key(".codex/com1.log").is_err());
        assert!(validate_cloud_key(".codex/C:evil").is_err());
        assert!(validate_cloud_key(".codex/console.log").is_ok());
        assert!(validate_cloud_key(".codex/nullable.rs").is_ok());
    }

    #[test]
    fn object_keys_must_match_the_upload_grammar() {
        // Readable snapshot form: original path under the batch prefix.
        assert!(validate_object_key("_uploads/01abc-2/files/.codex/config.toml").is_ok());
        assert!(validate_object_key(
            "_uploads/01abc/files/.codex/sessions/2026/04/01/rollout-x.jsonl"
        )
        .is_ok());
        assert!(validate_object_key("_uploads/01abc/files/../etc/passwd").is_err());
        assert!(validate_object_key("_uploads/01abc/files/other/file").is_err());
        assert!(validate_object_key("_uploads/01ABC/files/.codex/a").is_err());
        assert!(validate_object_key("uploads/01abc/files/.codex/a").is_err());

        // Legacy content-addressed form still accepted on read.
        let sha = "a".repeat(64);
        assert!(validate_object_key(&format!("_uploads/01abc-2/objects/{}", sha)).is_ok());
        assert!(validate_object_key("_uploads/01abc/objects/beef").is_err());
        assert!(validate_object_key(&format!("_uploads/01abc/other/{}", sha)).is_err());
    }

    #[test]
    fn history_keys_must_match_the_generation_grammar() {
        let key = history_object_key("_manifests", 12, "4d58b5a0d3e24e2b");
        assert_eq!(key, "_manifests/000000000012-4d58b5a0d3e24e2b.json");
        assert!(validate_history_key(&key, "_manifests"));
        assert!(!validate_history_key(&key, "_commits"));
        assert!(!validate_history_key(
            "_manifests/12-4d58b5a0d3e24e2b.json",
            "_manifests"
        ));
        assert!(!validate_history_key(
            "_manifests/000000000012-XYZ.json",
            "_manifests"
        ));
        assert!(!validate_history_key(
            "_manifests/000000000012-4d58b5a0d3e24e2b.json/x",
            "_manifests"
        ));

        assert!(validate_profile_id("01habc9").is_ok());
        assert!(validate_profile_id("_head").is_err());
        assert!(validate_profile_id("UPPER").is_err());
        assert!(validate_profile_id("").is_err());
    }

    #[test]
    fn desired_manifest_carries_forward_untouched_entries() {
        let mut current: BTreeMap<String, ManifestEntry> = BTreeMap::new();
        current.insert(
            ".codex/config.toml".to_string(),
            ManifestEntry {
                sha256: "old".to_string(),
                size: 3,
                object_key: "_uploads/batch0/objects/old".to_string(),
                source_mtime: 0,
            },
        );
        current.insert(
            ".claude/opted-in-elsewhere".to_string(),
            ManifestEntry {
                sha256: "keep".to_string(),
                size: 1,
                object_key: "_uploads/batch0/objects/keep".to_string(),
                source_mtime: 0,
            },
        );

        let uploads = vec![
            (
                ".codex/config.toml".to_string(),
                "new".to_string(),
                4u64,
                111u64,
            ),
            (
                ".codex/AGENTS.md".to_string(),
                "added".to_string(),
                5u64,
                222u64,
            ),
            // Same content as published: no new object, no summary entry.
            (
                ".claude/opted-in-elsewhere".to_string(),
                "keep".to_string(),
                1u64,
                333u64,
            ),
        ];
        let (files, summary) = build_desired_manifest(&current, &uploads, "batch1");

        assert_eq!(summary.added, 1);
        assert_eq!(summary.modified, 1);
        assert_eq!(summary.deleted, 0);
        assert_eq!(
            files[".codex/config.toml"].object_key,
            "_uploads/batch1/files/.codex/config.toml"
        );
        assert_eq!(
            files[".codex/AGENTS.md"].object_key,
            "_uploads/batch1/files/.codex/AGENTS.md"
        );
        // Untouched and same-content entries keep their published objects.
        assert_eq!(
            files[".claude/opted-in-elsewhere"].object_key,
            "_uploads/batch0/objects/keep"
        );
        // New entries carry the captured source mtime; same-content entries
        // keep the published one (no mtime-only churn).
        assert_eq!(files[".codex/config.toml"].source_mtime, 111);
        assert_eq!(files[".claude/opted-in-elsewhere"].source_mtime, 0);
    }

    #[test]
    fn manifest_publish_rejects_casefold_collisions() {
        let entry = |path: &str| ManifestEntry {
            sha256: sha256_bytes(path.as_bytes()),
            size: path.len() as u64,
            object_key: format!("_uploads/test/files/{}", path),
            source_mtime: 0,
        };
        let mut files = BTreeMap::new();
        files.insert(
            ".codex/agents/A.md".to_string(),
            entry(".codex/agents/A.md"),
        );
        files.insert(
            ".codex/agents/a.md".to_string(),
            entry(".codex/agents/a.md"),
        );
        assert!(validate_casefold_unique_manifest(&files).is_err());

        let mut files = BTreeMap::new();
        files.insert(".codex/agents".to_string(), entry(".codex/agents"));
        files.insert(
            ".codex/agents/reviewer.md".to_string(),
            entry(".codex/agents/reviewer.md"),
        );
        assert!(validate_casefold_unique_manifest(&files).is_err());
    }

    #[test]
    fn legacy_never_entries_are_purged_from_manifests_and_baselines() {
        let entry = |rel: &str| ManifestEntry {
            sha256: sha256_bytes(rel.as_bytes()),
            size: rel.len() as u64,
            object_key: format!("_uploads/old/files/{}", rel),
            source_mtime: 0,
        };
        let allowed = ".codex/AGENTS.md";
        let forbidden = [
            ".codex/.tmp/plugins/catalog.json",
            ".codex/plugins/cache/plugin/file.json",
            ".codex/plugins/cache.sync-conflict-aabbccdd/plugin/file.json",
        ];
        let mut current = BTreeMap::new();
        current.insert(allowed.to_string(), entry(allowed));
        for rel in forbidden {
            current.insert(rel.to_string(), entry(rel));
        }
        let uploads = vec![(
            ".codex/.tmp/new-catalog.json".to_string(),
            "new".to_string(),
            3,
            0,
        )];

        let (desired, summary) = build_desired_manifest(&current, &uploads, "next");
        assert_eq!(desired.len(), 1);
        assert!(desired.contains_key(allowed));
        assert_eq!(summary.deleted, forbidden.len() as u64);
        assert_eq!(summary.added, 0);

        let mut baseline = SyncManifest {
            files: current
                .keys()
                .map(|rel| {
                    (
                        rel.clone(),
                        FileRecord {
                            sha256: sha256_bytes(rel.as_bytes()),
                            size: rel.len() as u64,
                            mtime: 0,
                            cloud_object_sha256: None,
                        },
                    )
                })
                .collect(),
            last_push: 0,
        };
        purge_never_synced_baseline(&mut baseline);
        assert_eq!(baseline.files.len(), 1);
        assert!(baseline.files.contains_key(allowed));
    }

    #[test]
    fn legacy_never_collisions_are_removed_before_manifest_validation() {
        let entry = |rel: &str| ManifestEntry {
            sha256: sha256_bytes(rel.as_bytes()),
            size: rel.len() as u64,
            object_key: format!("_uploads/old/files/{}", rel),
            source_mtime: 0,
        };
        let allowed = ".codex/AGENTS.md";
        let mut current = BTreeMap::new();
        current.insert(allowed.to_string(), entry(allowed));
        // These collide under the publish validator, but both belong to the
        // hard Never tier and must be cleanup input rather than a permanent
        // obstacle to publishing the eligible manifest.
        for rel in [".codex/.tmp/plugins/A.json", ".codex/.TMP/plugins/a.json"] {
            current.insert(rel.to_string(), entry(rel));
        }

        let (desired, summary) = build_desired_manifest(&current, &[], "next");
        assert_eq!(desired.len(), 1);
        assert!(desired.contains_key(allowed));
        assert_eq!(summary.deleted, 2);
        validate_casefold_unique_manifest(&desired).unwrap();

        // Filtering is limited to hard-Never paths: eligible collisions still
        // reach the validator and fail closed.
        let mut eligible_collision = BTreeMap::new();
        for rel in [".codex/agents/A.md", ".codex/agents/a.md"] {
            eligible_collision.insert(rel.to_string(), entry(rel));
        }
        let (desired, _) = build_desired_manifest(&eligible_collision, &[], "next");
        assert!(validate_casefold_unique_manifest(&desired).is_err());
    }

    #[test]
    fn manifest_entries_deserialize_without_source_mtime() {
        // Manifests written by older builds lack the field; 0 = skip restore.
        let entry: ManifestEntry = serde_json::from_str(
            "{\"sha256\":\"abc\",\"size\":3,\"object_key\":\"_uploads/x/files/f\"}",
        )
        .unwrap();
        assert_eq!(entry.source_mtime, 0);
    }

    #[test]
    fn baseline_records_deserialize_without_optional_fields() {
        let manifest: SyncManifest = serde_json::from_str(
            "{\"files\":{\".codex/config.toml\":{\"sha256\":\"abc\",\"size\":3,\"mtime\":9}},\"last_push\":1}",
        )
        .unwrap();
        assert_eq!(manifest.files[".codex/config.toml"].sha256, "abc");
    }

    #[test]
    fn matrix_statuses_cover_both_sides() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        fs::write(&file, b"local").unwrap();
        let local_sha = sha256_bytes(b"local");
        let mut baseline = SyncManifest::default();

        // Not in baseline: local-only vs cloud-only vs conflict/converged.
        assert_eq!(matrix_status(&file, "f.txt", &baseline, None), "local-only");
        let missing = dir.path().join("missing.txt");
        assert_eq!(
            matrix_status(&missing, "missing.txt", &baseline, Some("abc")),
            "cloud-only"
        );
        assert_eq!(
            matrix_status(&file, "f.txt", &baseline, Some(&local_sha)),
            "converged"
        );
        assert_eq!(
            matrix_status(&file, "f.txt", &baseline, Some("other")),
            "conflict"
        );

        // Baselined: the four drift quadrants.
        baseline
            .files
            .insert("f.txt".to_string(), file_record(&file, b"local"));
        assert_eq!(
            matrix_status(&file, "f.txt", &baseline, Some(&local_sha)),
            "synced"
        );
        assert_eq!(
            matrix_status(&file, "f.txt", &baseline, Some("cloud-moved")),
            "cloud-ahead"
        );
        fs::write(&file, b"edited!").unwrap(); // size change defeats the mtime fast path
        assert_eq!(
            matrix_status(&file, "f.txt", &baseline, Some(&local_sha)),
            "local-ahead"
        );
        assert_eq!(
            matrix_status(&file, "f.txt", &baseline, Some("cloud-moved")),
            "conflict"
        );
        assert_eq!(
            matrix_status(&file, "f.txt", &baseline, Some(&sha256_bytes(b"edited!"))),
            "converged"
        );

        // Deletions: never propagated, but visible as states.
        assert_eq!(
            matrix_status(&file, "f.txt", &baseline, None),
            "cloud-deleted"
        );
        baseline.files.insert(
            "gone.txt".to_string(),
            FileRecord {
                sha256: "s".to_string(),
                size: 1,
                mtime: 0,
                cloud_object_sha256: None,
            },
        );
        assert_eq!(
            matrix_status(
                &dir.path().join("gone.txt"),
                "gone.txt",
                &baseline,
                Some("s")
            ),
            "local-deleted"
        );
        assert_eq!(
            matrix_status(&dir.path().join("gone.txt"), "gone.txt", &baseline, None),
            "synced"
        );
    }

    #[test]
    fn sync_config_roundtrips_v2_links() {
        let config: SyncConfig = serde_json::from_str("{}").unwrap();
        assert!(config.storages.is_empty());
        assert!(config.links.is_empty());

        let config: SyncConfig = serde_json::from_str(
            "{\"schema\":2,\
              \"storages\":[{\"id\":\"s1\",\"kind\":\"s3\",\"bucket\":\"b\",\"supports_conditional_writes\":true}],\
              \"local_profiles\":[{\"id\":\"codex\",\"root\":\".codex\"}],\
              \"links\":[{\"profile\":\"codex\",\"storage\":\"s1\",\
                          \"cloud\":{\"root\":\".codex\",\"profile_id\":\"01abc\"}}]}",
        )
        .unwrap();
        assert_eq!(config.schema, CONFIG_SCHEMA_VERSION);
        assert_eq!(config.storages[0].supports_conditional_writes, Some(true));
        assert_eq!(config.links[0].cloud.profile_id, "01abc");
        assert!(!config.links[0].cloud.pinned);
        // Round-trip preserves the resolved cloud side.
        let json = serde_json::to_string(&config).unwrap();
        let back: SyncConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.links[0].cloud.profile_id, "01abc");
    }

    // ── Fleet convergence invariants ────────────────────────────────────
    //
    // Two machines resolving the same divergence independently must produce
    // byte-identical output; the second machine then lands in `converged`
    // and the fleet stops churning. These invariants are what make the
    // union safe to retry after a lost head race.

    #[test]
    fn merge_drivers_converge_regardless_of_direction_and_are_idempotent() {
        let a = "{\"ts\":1,\"text\":\"a\"}\r\n\n{\"ts\":3,\"text\":\"c\"}\n";
        let b = "{\"ts\":2,\"text\":\"b\"}\n{\"ts\":3,\"text\":\"c\"}\n";
        let ab = merge_history_jsonl(a, b);
        let ba = merge_history_jsonl(b, a);
        assert_eq!(
            ab, ba,
            "history union must not depend on which side is local"
        );
        // CRLF endings and blank lines never survive into the union.
        assert!(!ab.contains('\r') && !ab.contains("\n\n"));
        // Re-merging the union with either input is a no-op.
        assert_eq!(merge_history_jsonl(&ab, a), ab);
        assert_eq!(merge_history_jsonl(&ab, b), ab);
        assert_eq!(merge_history_jsonl(&ab, ""), ab);

        let x = "{\"id\":\"s1\",\"updated_at\":5}\n{\"id\":\"s2\",\"updated_at\":9}\n";
        let y = "{\"id\":\"s1\",\"updated_at\":7}\n{\"id\":\"s3\",\"updated_at\":1}\n";
        let xy = merge_session_index_jsonl(x, y);
        assert_eq!(xy, merge_session_index_jsonl(y, x));
        assert_eq!(merge_session_index_jsonl(&xy, x), xy);
        assert_eq!(merge_session_index_jsonl(&xy, y), xy);
    }

    #[test]
    fn plugin_merge_driver_declines_source_conflicts_but_other_drivers_merge() {
        let make_lock = |repository: &str| {
            codex_plugins::canonical_lock_json(&codex_plugins::CodexPluginLock {
                schema: 1,
                marketplaces: vec![codex_plugins::CodexMarketplaceIntent {
                    name: "team-tools".to_string(),
                    repository: repository.to_string(),
                    git_ref: Some("aaa".to_string()),
                }],
                ..codex_plugins::CodexPluginLock::default()
            })
        };
        let local = make_lock("owner/repo");
        let cloud = make_lock("other/repo");

        let plugin_driver = merge_driver(codex_plugins::LOCK_REL).unwrap();
        assert_eq!(plugin_driver(&local, &cloud), None);
        assert_eq!(plugin_driver(&cloud, &local), None);

        assert!(
            merge_driver(".codex/history.jsonl").unwrap()("{\"ts\":1}\n", "{\"ts\":2}\n").is_some()
        );
        assert!(
            merge_driver(codex_sidebar::LOCK_REL).unwrap()("not json", "also not json").is_some()
        );
    }

    #[test]
    fn session_index_breaks_updated_at_ties_lexically() {
        // Same id, same updated_at, different content: the lexically greater
        // line wins on every machine, so ties cannot ping-pong.
        let a = "{\"id\":\"s1\",\"updated_at\":5,\"thread_name\":\"alpha\"}";
        let b = "{\"id\":\"s1\",\"updated_at\":5,\"thread_name\":\"beta\"}";
        let winner = if a > b { a } else { b };
        let merged_ab = merge_session_index_jsonl(a, b);
        assert_eq!(merged_ab.trim_end(), winner);
        assert_eq!(merged_ab, merge_session_index_jsonl(b, a));
    }

    #[test]
    fn conflict_copies_roundtrip_through_the_marker_strip() {
        for rel in [
            ".codex/config.toml",
            ".codex/AGENTS.md",
            ".codex/rules",
            ".claude/.gitignore",
            ".codex/skills/a.b.c/SKILL.md",
            ".claude/projects/-Users-x/session.jsonl",
        ] {
            let copy = conflict_copy_rel(rel, "aabbccddeeff0011");
            assert_ne!(copy, rel);
            assert_eq!(strip_conflict_marker(&copy), rel, "roundtrip for {}", rel);
        }
    }

    // ── The user's two multi-writer scenarios, over real files ──────────
    //
    // Scenario timeline (see DESIGN2 "Union Conflict Resolution"): A and B
    // share a profile; classification against a real filesystem must send
    // each path down the union arm the scenario demands.

    #[test]
    fn union_scenarios_classify_correctly_over_real_files() {
        let home = tempfile::tempdir().unwrap();
        let mut baseline = SyncManifest::default();
        let sha = |bytes: &[u8]| sha256_bytes(bytes);

        let seed = |rel: &str, content: &[u8]| {
            let path = home.path().join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, content).unwrap();
            path
        };
        let baselined = |baseline: &mut SyncManifest, rel: &str, content: &[u8]| {
            let path = seed(rel, content);
            baseline
                .files
                .insert(rel.to_string(), file_record(&path, content));
            path
        };

        // B pushed while we slept: cloud moved, local untouched → apply.
        let a = baselined(&mut baseline, ".codex/cloud-moved.md", b"v1");
        assert_eq!(
            local_state_at(&a, ".codex/cloud-moved.md", &baseline),
            LocalState::Unchanged
        );
        assert_eq!(
            classify_path(
                LocalState::Unchanged,
                Some(&sha(b"v2")),
                baseline.files.get(".codex/cloud-moved.md")
            ),
            SyncAction::ApplyCloud
        );

        // We edited, cloud unchanged (cloud sha == baseline sha) → upload
        // on push, kept as local-ahead on pull.
        let b = baselined(&mut baseline, ".codex/local-moved.md", b"v1");
        fs::write(&b, b"v2-longer").unwrap();
        assert_eq!(
            local_state_at(&b, ".codex/local-moved.md", &baseline),
            LocalState::Changed
        );
        assert_eq!(
            classify_path(
                LocalState::Changed,
                Some(&sha(b"v1")),
                baseline.files.get(".codex/local-moved.md")
            ),
            SyncAction::UploadLocal
        );

        // Both sides moved since the baseline → fetch and merge.
        let c = baselined(&mut baseline, ".codex/both-moved.jsonl", b"v1");
        fs::write(&c, b"v2-local!").unwrap();
        assert_eq!(
            classify_path(
                local_state_at(&c, ".codex/both-moved.jsonl", &baseline),
                Some(&sha(b"v3-cloud")),
                baseline.files.get(".codex/both-moved.jsonl")
            ),
            SyncAction::Reconcile
        );

        // Deleted locally but alive in the cloud → the union restores it.
        let d = baselined(&mut baseline, ".codex/deleted-here.md", b"v1");
        fs::remove_file(&d).unwrap();
        assert_eq!(
            classify_path(
                local_state_at(&d, ".codex/deleted-here.md", &baseline),
                Some(&sha(b"v1")),
                baseline.files.get(".codex/deleted-here.md")
            ),
            SyncAction::ApplyCloud
        );

        // Deleted in the cloud but untouched here → the next push republishes.
        let e = baselined(&mut baseline, ".codex/deleted-there.md", b"v1");
        assert_eq!(
            classify_path(
                local_state_at(&e, ".codex/deleted-there.md", &baseline),
                None,
                baseline.files.get(".codex/deleted-there.md")
            ),
            SyncAction::UploadLocal
        );

        // Gone on both sides → only the stale record remains; forget it.
        let f = baselined(&mut baseline, ".codex/gone.md", b"v1");
        fs::remove_file(&f).unwrap();
        assert_eq!(
            classify_path(
                local_state_at(&f, ".codex/gone.md", &baseline),
                None,
                baseline.files.get(".codex/gone.md")
            ),
            SyncAction::DropRecord
        );
    }

    #[test]
    fn same_size_same_second_edit_is_classified_by_content() {
        let home = tempfile::tempdir().unwrap();
        let rel = ".codex/same-size.md";
        let path = home.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"before").unwrap();

        let record = file_record(&path, b"before");
        let baseline_mtime = record.mtime;
        let baseline_sha = record.sha256.clone();
        let mut baseline = SyncManifest::default();
        baseline.files.insert(rel.to_string(), record);

        fs::write(&path, b"after!").unwrap();
        fs::File::options()
            .append(true)
            .open(&path)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(baseline_mtime))
            .unwrap();

        assert_eq!(fs::metadata(&path).unwrap().len(), 6);
        assert_eq!(file_mtime_secs(&path), baseline_mtime);
        assert_eq!(local_state_at(&path, rel, &baseline), LocalState::Changed);
        assert_eq!(
            classify_path(
                local_state_at(&path, rel, &baseline),
                Some(&baseline_sha),
                baseline.files.get(rel)
            ),
            SyncAction::UploadLocal
        );
    }

    #[test]
    fn record_cloud_side_pins_local_ahead_until_the_push() {
        // After a merge/conflict-copy the baseline records the *cloud* sha
        // with mtime 0. The mtime fast path must stay disabled so the
        // locally resolved file keeps classifying as changed (local ahead)
        // until a push publishes it — and flips to unchanged the moment the
        // disk content equals the recorded cloud content.
        let home = tempfile::tempdir().unwrap();
        let rel = ".codex/merged.jsonl";
        let path = home.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"merged-union").unwrap();

        let mut baseline = SyncManifest::default();
        let cloud_content = b"cloud-version";
        let cloud_sha = sha256_bytes(cloud_content);
        record_cloud_side(
            &mut baseline,
            rel,
            &cloud_sha,
            &cloud_sha,
            cloud_content.len() as u64,
        );
        assert_eq!(local_state_at(&path, rel, &baseline), LocalState::Changed);

        fs::write(&path, cloud_content).unwrap();
        assert_eq!(local_state_at(&path, rel, &baseline), LocalState::Unchanged);
    }

    // ── Allowlist walking over a realistic tree ──────────────────────────

    #[test]
    fn collect_upload_files_walks_exactly_the_allowlist() {
        let home = tempfile::tempdir().unwrap();
        let seed = |rel: &str| {
            let path = home.path().join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"x").unwrap();
        };
        for rel in [
            // In by default
            ".codex/config.toml",
            ".codex/sessions/2026/07/05/rollout-a.jsonl",
            ".codex/archived_sessions/rollout-b.jsonl",
            ".codex/memories/notes.md",
            ".claude/plugins/config.json", // needs ancestor traversal
            ".claude/projects/-Users-x/session.jsonl",
            // Never tier
            ".codex/auth.json",
            ".codex/.tmp/plugins/.agents/plugins/marketplace.json",
            ".codex/plugins/cache/openai-bundled/sites/1/.mcp.json",
            ".codex/memories/.git/HEAD",
            ".claude/settings.local.json",
            ".claude/plugins/repos/repo/file.rs",
            ".claude/.DS_Store",
            // Unlisted (opt-in only)
            ".codex/models_cache.json",
            ".codex/cache/blob.bin",
            ".codex/state_5.sqlite",
            ".codex/state_5.sqlite-wal",
        ] {
            seed(rel);
        }

        let roots = vec![
            home.path().join(".codex").to_string_lossy().to_string(),
            home.path().join(".claude").to_string_lossy().to_string(),
        ];
        let mounts = Roots::for_home(home.path());
        let claude_mounts = Roots {
            home: home.path().to_path_buf(),
            root: ".claude".to_string(),
            dir: home.path().join(".claude"),
            remap: home.path().join(".agent-sync").join("claude"),
        };
        let collect = |opt_ins: &[String]| -> HashSet<String> {
            collect_upload_files(&roots, &mounts, opt_ins)
                .into_iter()
                .chain(collect_upload_files(&roots, &claude_mounts, opt_ins))
                .map(|(_, rel)| rel)
                .collect()
        };

        let got = collect(&[]);
        let want: HashSet<String> = [
            ".codex/config.toml",
            ".codex/sessions/2026/07/05/rollout-a.jsonl",
            ".codex/archived_sessions/rollout-b.jsonl",
            ".codex/memories/notes.md",
            ".claude/plugins/config.json",
            ".claude/projects/-Users-x/session.jsonl",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(got, want);

        // Opt-ins add exactly the opted runtime database; SQLite sidecars and
        // hard-Never plugin manager trees stay excluded.
        let config = vec![
            ".codex/state_5.sqlite*".to_string(),
            ".codex/.tmp".to_string(),
            ".codex/plugins/cache".to_string(),
        ];
        let got = collect(&config);
        assert!(got.contains(".codex/state_5.sqlite"));
        assert!(!got.contains(".codex/state_5.sqlite-wal"));
        assert!(!got.iter().any(|rel| path_matches_root(rel, ".codex/.tmp")));
        assert!(!got
            .iter()
            .any(|rel| path_matches_root(rel, ".codex/plugins/cache")));
        assert_eq!(got.len(), want.len() + 1);
    }

    #[cfg(unix)]
    #[test]
    fn collect_upload_files_rejects_direct_symlinks_and_symlinked_ancestors() {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let mounts = Roots::for_home(home.path());
        fs::create_dir_all(home.path().join(".codex/agents")).unwrap();

        let outside_file = outside.path().join("secret.md");
        fs::write(&outside_file, b"secret").unwrap();
        let direct_link = home.path().join(".codex/agents/direct.md");
        symlink(&outside_file, &direct_link).unwrap();
        assert!(
            collect_upload_files(&[direct_link.to_string_lossy().to_string()], &mounts, &[],)
                .is_empty()
        );

        let outside_dir = outside.path().join("tree");
        fs::create_dir_all(&outside_dir).unwrap();
        fs::write(outside_dir.join("nested.md"), b"secret").unwrap();
        let ancestor_link = home.path().join(".codex/skills");
        symlink(&outside_dir, &ancestor_link).unwrap();
        assert!(build_tree(&ancestor_link, &mounts, &[], 0).is_none());
        assert!(
            collect_upload_files(&[ancestor_link.to_string_lossy().to_string()], &mounts, &[],)
                .is_empty()
        );
        let nested = ancestor_link.join("nested.md");
        assert!(
            collect_upload_files(&[nested.to_string_lossy().to_string()], &mounts, &[],).is_empty()
        );
    }

    // ── Cloud-input hardening and back-compat ───────────────────────────

    #[test]
    fn cache_from_manifest_drops_invalid_paths() {
        let head = HeadFile {
            schema_version: CLOUD_SCHEMA_VERSION,
            profile_id: "p1".to_string(),
            root: ".codex".to_string(),
            state: "active".to_string(),
            generation: 7,
            commit_id: "c".repeat(16),
            manifest_key: history_object_key("_manifests", 7, &"c".repeat(16)),
            commit_key: history_object_key("_commits", 7, &"c".repeat(16)),
            manifest_sha256: String::new(),
            updated_at: 0,
        };
        let entry = |key: &str| ManifestEntry {
            sha256: "s".to_string(),
            size: 1,
            object_key: key.to_string(),
            source_mtime: 0,
        };
        let mut files = BTreeMap::new();
        files.insert(
            ".codex/ok.md".to_string(),
            entry("_uploads/b1/files/.codex/ok.md"),
        );
        files.insert("../../etc/passwd".to_string(), entry("_uploads/b1/files/e"));
        files.insert("/abs".to_string(), entry("_uploads/b1/files/a"));
        files.insert(".codex/nul.txt".to_string(), entry("_uploads/b1/files/n"));
        files.insert(
            ".codex/.tmp/plugins/catalog.json".to_string(),
            entry("_uploads/b1/files/.codex/.tmp/plugins/catalog.json"),
        );
        files.insert(
            ".codex/plugins/cache.sync-conflict-aabbccdd/plugin.json".to_string(),
            entry("_uploads/b1/files/.codex/plugins/cache.sync-conflict-aabbccdd/plugin.json"),
        );

        let cache = cache_from_manifest(&head, &files, "s1", "p1");
        assert_eq!(cache.generation, 7);
        assert_eq!(cache.files.len(), 1);
        assert!(cache.files.contains_key(".codex/ok.md"));
    }

    #[test]
    fn desired_manifest_object_keys_always_validate() {
        let uploads: Vec<(String, String, u64, u64)> = [
            ".codex/config.toml",
            ".codex/sessions/2026/07/05/rollout-2026-07-05T00-49-06-019d4804.jsonl",
            ".codex/AGENTS.sync-conflict-aabbccdd.md",
            ".claude/projects/-Users-hequ-Desktop-project-memory/abc/tool-results/r.json",
        ]
        .iter()
        .enumerate()
        .map(|(i, rel)| (rel.to_string(), format!("{:064}", i), 1u64, 0u64))
        .collect();
        let (files, _) = build_desired_manifest(&BTreeMap::new(), &uploads, "01batch");
        assert_eq!(files.len(), uploads.len());
        for (rel, entry) in &files {
            validate_object_key(&entry.object_key).unwrap_or_else(|e| panic!("{}: {}", rel, e));
            assert!(
                entry.object_key.ends_with(rel),
                "key embeds the path for {}",
                rel
            );
        }
    }

    #[test]
    fn legacy_cloud_and_config_data_still_deserialize() {
        // Heads published before the per-root split carry no `root`.
        let head: HeadFile = serde_json::from_str(
            "{\"schema_version\":1,\"profile_id\":\"p\",\"state\":\"active\",\
             \"generation\":3,\"commit_id\":\"4d58b5a0d3e24e2b\",\
             \"manifest_key\":\"_manifests/000000000003-4d58b5a0d3e24e2b.json\",\
             \"commit_key\":\"_commits/000000000003-4d58b5a0d3e24e2b.json\",\
             \"manifest_sha256\":\"ab\",\"updated_at\":1}",
        )
        .unwrap();
        assert_eq!(head.root, "");
        assert_eq!(head.generation, 3);

        // Pre-v2 configs parse structurally (unknown fields ignored) but
        // carry no v2 schema tag — `load_sync_config` treats them as
        // unconfigured (clean break, PLAN_MULTI_STORAGE.md §3.1).
        let config: SyncConfig = serde_json::from_str(
            "{\"kind\":\"s3\",\"bucket\":\"b\",\"endpoint\":\"x\",\"token\":\"t\",\
             \"profile\":{\"profile_id\":\"old\"}}",
        )
        .unwrap();
        assert_ne!(config.schema, CONFIG_SCHEMA_VERSION);
        assert!(config.storages.is_empty());
        assert!(config.links.is_empty());

        // Newly linked rows are intentionally unresolved until their first
        // push or pull. The settings UI represents that state as `cloud: {}`;
        // accepting it is what lets save_sync_config run before the sync.
        let config: SyncConfig = serde_json::from_str(
            "{\"schema\":2,\"storages\":[{\"id\":\"storage-2\",\"kind\":\"local\",\"local_dir\":\"/tmp/store\"}],\
             \"local_profiles\":[{\"id\":\"custom\",\"root\":\".claude\",\"path\":\"/tmp/custom\"}],\
             \"links\":[{\"profile\":\"custom\",\"storage\":\"storage-2\",\"cloud\":{}}]}",
        )
        .unwrap();
        assert_eq!(config.links.len(), 1);
        assert!(config.links[0].cloud.profile_id.is_empty());

        // Baseline records with unknown future fields load too.
        let manifest: SyncManifest = serde_json::from_str(
            "{\"files\":{\"a\":{\"sha256\":\"s\",\"size\":1,\"mtime\":2,\"future\":true}}}",
        )
        .unwrap();
        assert_eq!(manifest.files["a"].sha256, "s");
    }

    #[test]
    fn set_link_cloud_replaces_only_its_cell() {
        let cloud = |root: &str, id: &str| ProfileLink {
            root: root.to_string(),
            profile_id: id.to_string(),
            ..Default::default()
        };
        let mut config = default_sync_config();
        set_link_cloud(&mut config, "s1", "codex", &cloud(".codex", "c1"));
        set_link_cloud(&mut config, "s1", "claude", &cloud(".claude", "l1"));
        set_link_cloud(&mut config, "s2", "codex", &cloud(".codex", "other"));
        set_link_cloud(&mut config, "s1", "codex", &cloud(".codex", "c2"));
        assert_eq!(config.links.len(), 3);
        let cell = |storage: &str, profile: &str| {
            config
                .links
                .iter()
                .find(|l| l.storage == storage && l.profile == profile)
                .map(|l| l.cloud.profile_id.as_str())
        };
        assert_eq!(cell("s1", "codex"), Some("c2"));
        assert_eq!(cell("s1", "claude"), Some("l1"));
        assert_eq!(cell("s2", "codex"), Some("other"));
        // Same-storage duplicate targets are valid: baselines are per link,
        // so each local root acts as an independent replica of the profile.
        validate_sync_config(&SyncConfig {
            schema: CONFIG_SCHEMA_VERSION,
            storages: vec![StorageConfig {
                id: "s1".to_string(),
                kind: "s3".to_string(),
                ..Default::default()
            }],
            local_profiles: vec![
                LocalProfile {
                    id: "codex".to_string(),
                    root: ".codex".to_string(),
                    ..Default::default()
                },
                LocalProfile {
                    id: "claude".to_string(),
                    root: ".claude".to_string(),
                    ..Default::default()
                },
            ],
            links: vec![
                SyncLink {
                    profile: "codex".to_string(),
                    storage: "s1".to_string(),
                    cloud: cloud(".codex", "same"),
                },
                SyncLink {
                    profile: "claude".to_string(),
                    storage: "s1".to_string(),
                    cloud: cloud(".claude", "same"),
                },
            ],
        })
        .expect("shared cloud targets are allowed with per-link baselines");
    }

    #[test]
    fn unique_profile_label_suffixes_taken_names() {
        let existing: HashSet<String> = ["Claude".to_string(), "Claude 2".to_string()]
            .into_iter()
            .collect();
        assert_eq!(unique_profile_label("Codex", &existing), "Codex");
        assert_eq!(unique_profile_label("Claude", &existing), "Claude 3");
    }

    #[test]
    fn local_profile_label_prefers_custom_name() {
        let home = tempfile::tempdir().unwrap();
        let mut profile = LocalProfile {
            id: "claude".to_string(),
            root: ".claude".to_string(),
            name: "  Work  ".to_string(),
            ..Default::default()
        };
        let roots = Roots::for_profile_with_home(&profile, home.path().to_path_buf()).unwrap();
        assert_eq!(local_profile_label(&roots, &profile), "Work");
        profile.name = String::new();
        assert_eq!(local_profile_label(&roots, &profile), "~/.claude");
    }

    // ── File editing safety ──────────────────────────────────────────────

    // ── Plugin repair intent ─────────────────────────────────────────────

    #[test]
    fn fresh_mounts_stay_visible_in_the_sidebar() {
        let home = tempfile::tempdir().unwrap();
        let profile = LocalProfile {
            id: "claude".to_string(),
            root: ".claude".to_string(),
            ..Default::default()
        };
        let roots = Roots::for_profile_with_home(&profile, home.path().to_path_buf()).unwrap();
        // The dir doesn't exist yet: the source still appears, empty, so a
        // fresh mount can be pulled into from the UI.
        let source = read_source(&roots, &profile, &[]).unwrap();
        assert!(source.entries.is_empty());
        assert_eq!(source.label, "~/.claude");
        assert_eq!(source.id, "claude");
    }

    #[test]
    fn plugin_intent_parses_only_enabled_plugins_and_sourced_marketplaces() {
        let settings = serde_json::json!({
            "enabledPlugins": {
                "ponytail@ponytail": true,
                "disabled@mkt": false,
                "no-marketplace-id": true
            },
            "extraKnownMarketplaces": {
                "ponytail": { "source": { "source": "github", "repo": "DietrichGebert/ponytail" } },
                "local": { "source": { "source": "path", "path": "/tmp/mkt" } },
                "broken": { "source": { "source": "github" } }
            }
        });
        let intent = parse_plugin_intent(&settings);
        assert_eq!(intent.plugins, vec!["ponytail@ponytail".to_string()]);
        assert_eq!(
            intent.marketplaces,
            vec![
                ("local".to_string(), "/tmp/mkt".to_string()),
                (
                    "ponytail".to_string(),
                    "DietrichGebert/ponytail".to_string()
                ),
            ]
        );

        // No intent keys at all → empty, repair is a no-op.
        assert_eq!(
            parse_plugin_intent(&serde_json::json!({})),
            PluginIntent::default()
        );
    }

    #[test]
    fn plugin_presence_checks_require_existing_install_paths() {
        let home = tempfile::tempdir().unwrap();
        // Presence checks target the mounted .claude dir directly, so a
        // custom mount (container/.claude) works identically.
        let claude_dir = home.path().join(".claude");
        let plugins_dir = claude_dir.join("plugins");
        fs::create_dir_all(&plugins_dir).unwrap();

        // Nothing recorded yet → everything is missing.
        assert!(!marketplace_is_present(&claude_dir, "ponytail"));
        assert!(!plugin_is_present(&claude_dir, "ponytail@ponytail"));

        // Recorded but the directory does not exist → still missing.
        let market_loc = home.path().join(".claude/plugins/marketplaces/ponytail");
        fs::write(
            plugins_dir.join("known_marketplaces.json"),
            serde_json::json!({
                "ponytail": {
                    "source": { "source": "github", "repo": "DietrichGebert/ponytail" },
                    "installLocation": market_loc.to_string_lossy()
                }
            })
            .to_string(),
        )
        .unwrap();
        assert!(!marketplace_is_present(&claude_dir, "ponytail"));
        fs::create_dir_all(&market_loc).unwrap();
        assert!(marketplace_is_present(&claude_dir, "ponytail"));
        assert_eq!(
            claude_marketplace_registration(&claude_dir, "ponytail"),
            ClaudeMarketplaceRegistration::Existing {
                source: Some("DietrichGebert/ponytail".to_string()),
                install_present: true,
            }
        );

        fs::write(
            plugins_dir.join("installed_plugins.json"),
            serde_json::json!({ "version": 2 }).to_string(),
        )
        .unwrap();
        assert_eq!(
            claude_plugin_presence(&claude_dir, "ponytail@ponytail"),
            ClaudePluginPresence::Corrupt("plugins object is missing".to_string())
        );

        let install_path = home
            .path()
            .join(".claude/plugins/cache/ponytail/ponytail/4.8.4");
        fs::write(
            plugins_dir.join("installed_plugins.json"),
            serde_json::json!({
                "version": 2,
                "plugins": {
                    "ponytail@ponytail": [
                        { "scope": "user", "installPath": install_path.to_string_lossy() }
                    ]
                }
            })
            .to_string(),
        )
        .unwrap();
        assert!(!plugin_is_present(&claude_dir, "ponytail@ponytail"));
        fs::create_dir_all(&install_path).unwrap();
        assert!(plugin_is_present(&claude_dir, "ponytail@ponytail"));
        assert!(!plugin_is_present(&claude_dir, "other@mkt"));
        fs::write(plugins_dir.join("installed_plugins.json"), "{broken").unwrap();
        assert!(matches!(
            claude_plugin_presence(&claude_dir, "ponytail@ponytail"),
            ClaudePluginPresence::Corrupt(_)
        ));
    }

    #[test]
    fn claude_marketplace_source_mismatch_diagnostic_redacts_sources() {
        assert_eq!(
            claude_marketplace_source_mismatch_message("ponytail"),
            "✗ marketplace ponytail: recorded source does not match the sync lock"
        );
    }

    #[test]
    fn editable_path_validation_guards_the_write_boundary() {
        let home = tempfile::tempdir().unwrap();
        let mounts = Roots::for_home(home.path());
        let seed = |rel: &str| {
            let path = home.path().join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"x").unwrap();
            path
        };
        let ok = seed(".codex/config.toml");
        assert_eq!(
            validate_editable_path(&mounts, &ok.to_string_lossy()).unwrap(),
            ok
        );

        // Outside the home directory entirely.
        let elsewhere = tempfile::tempdir().unwrap();
        let outside = elsewhere.path().join("f.txt");
        fs::write(&outside, b"x").unwrap();
        assert!(validate_editable_path(&mounts, &outside.to_string_lossy()).is_err());

        // Under home but outside the two config roots.
        let stray = seed("documents/notes.md");
        assert!(validate_editable_path(&mounts, &stray.to_string_lossy()).is_err());

        // SQLite is never text-editable.
        let db = seed(".codex/state_5.sqlite");
        assert!(validate_editable_path(&mounts, &db.to_string_lossy()).is_err());

        // Directories and missing files are rejected.
        let dir = home.path().join(".codex/sessions");
        fs::create_dir_all(&dir).unwrap();
        assert!(validate_editable_path(&mounts, &dir.to_string_lossy()).is_err());
        let missing = home.path().join(".codex/nope.md");
        assert!(validate_editable_path(&mounts, &missing.to_string_lossy()).is_err());

        // A symlink target and a symlinked ancestor both refuse the write.
        #[cfg(unix)]
        {
            let link = home.path().join(".codex/link.md");
            std::os::unix::fs::symlink(&ok, &link).unwrap();
            assert!(validate_editable_path(&mounts, &link.to_string_lossy()).is_err());

            let outside_dir = elsewhere.path().join("real");
            fs::create_dir_all(&outside_dir).unwrap();
            fs::write(outside_dir.join("f.md"), b"x").unwrap();
            let sneaky_dir = home.path().join(".codex/sneaky");
            std::os::unix::fs::symlink(&outside_dir, &sneaky_dir).unwrap();
            let sneaky = sneaky_dir.join("f.md");
            assert!(validate_editable_path(&mounts, &sneaky.to_string_lossy()).is_err());
        }
    }

    #[test]
    fn write_text_file_saves_atomically_under_an_optimistic_lock() {
        let home = tempfile::tempdir().unwrap();
        let mounts = Roots::for_home(home.path());
        let path = home.path().join(".codex/config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"v1").unwrap();
        let path_str = path.to_string_lossy().to_string();

        let doc = read_text_file(&mounts, &path_str).unwrap();
        assert!(doc.editable, "{:?}", doc.reason);
        assert_eq!(doc.content, "v1");
        assert_eq!(doc.sha256, sha256_bytes(b"v1"));

        // Save with the opened sha succeeds and returns the next lock token.
        let sha_v2 = write_text_file(&mounts, &path_str, "v2", &doc.sha256).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "v2");
        assert_eq!(sha_v2, sha256_bytes(b"v2"));

        // A stale sha (the file moved on under the editor) is refused and
        // the on-disk content is untouched.
        fs::write(&path, b"agent-rewrote-this").unwrap();
        let err = write_text_file(&mounts, &path_str, "v3", &sha_v2).unwrap_err();
        assert!(err.contains("changed on disk"), "{}", err);
        assert_eq!(fs::read_to_string(&path).unwrap(), "agent-rewrote-this");

        // Saving with the fresh sha (the overwrite path) succeeds.
        let fresh = read_text_file(&mounts, &path_str).unwrap().sha256;
        #[cfg(unix)]
        let planted_temp_target = {
            use std::os::unix::fs::symlink;

            let outside = home.path().join("outside-edit-target");
            fs::write(&outside, b"keep").unwrap();
            symlink(
                &outside,
                path.parent().unwrap().join(".config.toml.edit-tmp"),
            )
            .unwrap();
            outside
        };
        write_text_file(&mounts, &path_str, "v3", &fresh).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "v3");
        #[cfg(unix)]
        assert_eq!(fs::read(planted_temp_target).unwrap(), b"keep");
    }

    #[test]
    fn read_text_file_rejects_outside_and_symlinked_paths_before_reading() {
        let home = tempfile::tempdir().unwrap();
        let mounts = Roots::for_home(home.path());
        // Outside paths are not readable through this command at all.
        let stray = home.path().join("notes.md");
        fs::write(&stray, b"hello").unwrap();
        assert!(read_text_file(&mounts, &stray.to_string_lossy())
            .unwrap_err()
            .contains("outside"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            fs::create_dir_all(home.path().join(".codex/agents")).unwrap();
            let link = home.path().join(".codex/agents/secret.md");
            symlink(&stray, &link).unwrap();
            assert!(read_text_file(&mounts, &link.to_string_lossy()).is_err());
        }

        // Binary content is an error, matching the old behavior.
        let binary = home.path().join(".codex/blob.bin");
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::write(&binary, [0xff, 0xfe, 0x00, 0x01]).unwrap();
        assert!(read_text_file(&mounts, &binary.to_string_lossy())
            .unwrap_err()
            .contains("UTF-8"));
    }

    // ── Sync-link local side: the Roots mount table ──────────────────────

    #[test]
    fn roots_map_logical_paths_to_custom_mounts() {
        let mounts = Roots {
            home: PathBuf::from("/h"),
            root: ".codex".to_string(),
            dir: PathBuf::from("/scratch/.codex"),
            remap: PathBuf::from("/h/.agent-sync/codex"),
        };
        assert_eq!(
            mounts.abs(".codex/a/b.md"),
            PathBuf::from("/scratch/.codex/a/b.md")
        );
        assert_eq!(mounts.abs(".codex"), PathBuf::from("/scratch/.codex"));
        // Other kinds and unknown first components fall back to home-relative.
        assert_eq!(mounts.abs(".claude/x"), PathBuf::from("/h/.claude/x"));
        assert_eq!(mounts.abs("stray/f"), PathBuf::from("/h/stray/f"));

        assert_eq!(
            mounts.rel(Path::new("/scratch/.codex/a/b.md")).as_deref(),
            Some(".codex/a/b.md")
        );
        assert_eq!(
            mounts.rel(Path::new("/scratch/.codex")).as_deref(),
            Some(".codex")
        );
        // Another kind is outside this profile's mount entirely.
        assert_eq!(mounts.rel(Path::new("/h/.claude/x")), None);
        assert_eq!(mounts.rel(Path::new("/elsewhere/f")), None);
        // The old default location is OUTSIDE the mounts once overridden.
        assert_eq!(mounts.rel(Path::new("/h/.codex/f")), None);
    }

    #[test]
    fn roots_remap_agent_sync_out_of_the_roots() {
        let home = Path::new("/h");
        let roots = Roots::for_home(home);
        // abs: logical agent-sync paths land in the global app dir …
        assert_eq!(
            roots.abs(".codex/agent-sync/codex-plugins.lock.json"),
            home.join(".agent-sync/codex/codex-plugins.lock.json")
        );
        // … even under a custom root mount (the app dir stays home-anchored).
        let custom = Roots {
            home: home.to_path_buf(),
            root: ".codex".to_string(),
            dir: PathBuf::from("/scratch/.codex"),
            remap: home.join(".agent-sync/codex"),
        };
        assert_eq!(
            custom.abs(".codex/agent-sync/codex-plugins.lock.json"),
            home.join(".agent-sync/codex/codex-plugins.lock.json")
        );
        // rel: round-trips from the global dir …
        assert_eq!(
            roots
                .rel(&home.join(".agent-sync/codex/codex-plugins.lock.json"))
                .as_deref(),
            Some(".codex/agent-sync/codex-plugins.lock.json")
        );
        assert_eq!(
            roots.rel(&home.join(".agent-sync/codex")).as_deref(),
            Some(".codex/agent-sync")
        );
        // … a stale in-root copy maps to no logical path (no collision) …
        assert_eq!(
            roots.rel(&home.join(".codex/agent-sync/codex-plugins.lock.json")),
            None
        );
        assert_eq!(roots.rel(&home.join(".claude/agent-sync")), None);
        // … and machine.json, outside both subtrees, can never enter a manifest.
        assert_eq!(roots.rel(&home.join(".agent-sync/machine.json")), None);
        // Ordinary root files are untouched by the remap.
        assert_eq!(
            roots.rel(&home.join(".codex/config.toml")).as_deref(),
            Some(".codex/config.toml")
        );
    }

    #[cfg(unix)]
    #[test]
    fn machine_registry_never_follows_app_dir_or_final_file_symlinks() {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let roots = Roots::for_home(home.path());
        symlink(outside.path(), home.path().join(".agent-sync")).unwrap();
        write_machine_registry(&roots, &SyncConfig::default());
        assert!(!outside.path().join("machine.json").exists());

        fs::remove_file(home.path().join(".agent-sync")).unwrap();
        fs::create_dir_all(home.path().join(".agent-sync")).unwrap();
        let outside_file = outside.path().join("keep.json");
        fs::write(&outside_file, b"keep").unwrap();
        let registry = home.path().join(".agent-sync/machine.json");
        symlink(&outside_file, &registry).unwrap();
        write_machine_registry(&roots, &SyncConfig::default());
        assert_eq!(fs::read(&outside_file).unwrap(), b"keep");
        assert!(fs::symlink_metadata(&registry)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn lock_conflict_siblings_match_only_engine_generated_regular_files() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("codex-plugins.lock.json");
        fs::write(&lock, "{}\n").unwrap();
        let expected = dir
            .path()
            .join("codex-plugins.lock.sync-conflict-a1b2c3d4.json");
        fs::write(&expected, "{}\n").unwrap();
        fs::write(
            dir.path()
                .join("codex-plugins.lock.sync-conflict-too-long.json"),
            "{}\n",
        )
        .unwrap();
        fs::write(
            dir.path()
                .join("codex-plugins.lock.sync-conflict-zzzzzzzz.json"),
            "{}\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("other.lock.sync-conflict-a1b2c3d4.json"),
            "{}\n",
        )
        .unwrap();

        assert_eq!(lock_conflict_siblings(&lock), vec![expected]);
    }

    #[cfg(unix)]
    #[test]
    fn stale_in_root_lock_cleanup_never_follows_symlinks_or_broad_names() {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = home.path().join(".codex");
        fs::create_dir_all(&root).unwrap();
        for name in [
            "codex-plugins.lock.json",
            "codex-plugins.lock.sync-conflict-a1b2c3d4.json",
            "codex-plugins.lock.sync-conflict-too-long.json",
        ] {
            fs::write(outside.path().join(name), b"keep").unwrap();
        }
        symlink(outside.path(), root.join("agent-sync")).unwrap();

        remove_stale_in_root_agent_sync(&root, codex_plugins::LOCK_REL);
        assert_eq!(
            fs::read(outside.path().join("codex-plugins.lock.json")).unwrap(),
            b"keep"
        );

        fs::remove_file(root.join("agent-sync")).unwrap();
        let legacy = root.join("agent-sync");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("codex-plugins.lock.json"), b"old").unwrap();
        fs::write(
            legacy.join("codex-plugins.lock.sync-conflict-a1b2c3d4.json"),
            b"old",
        )
        .unwrap();
        fs::write(
            legacy.join("codex-plugins.lock.sync-conflict-too-long.json"),
            b"keep",
        )
        .unwrap();
        symlink(
            outside.path().join("codex-plugins.lock.json"),
            legacy.join("codex-plugins.lock.sync-conflict-11223344.json"),
        )
        .unwrap();

        remove_stale_in_root_agent_sync(&root, codex_plugins::LOCK_REL);
        assert!(!legacy.join("codex-plugins.lock.json").exists());
        assert!(!legacy
            .join("codex-plugins.lock.sync-conflict-a1b2c3d4.json")
            .exists());
        assert!(legacy
            .join("codex-plugins.lock.sync-conflict-too-long.json")
            .exists());
        assert!(fs::symlink_metadata(
            legacy.join("codex-plugins.lock.sync-conflict-11223344.json")
        )
        .unwrap()
        .file_type()
        .is_symlink());
    }

    fn profile_at(id: &str, root: &str, path: &str) -> LocalProfile {
        LocalProfile {
            id: id.to_string(),
            root: root.to_string(),
            path: path.to_string(),
            ..Default::default()
        }
    }

    /// A validatable v2 config holding just these profiles.
    fn config_with_profiles(profiles: Vec<LocalProfile>) -> SyncConfig {
        SyncConfig {
            schema: CONFIG_SCHEMA_VERSION,
            local_profiles: profiles,
            ..Default::default()
        }
    }

    #[test]
    fn roots_reject_mounts_overlapping_the_app_dir() {
        for over in ["/h/.agent-sync", "/h/.agent-sync/deep"] {
            let err = Roots::for_profile_with_home(
                &profile_at("codex", ".codex", over),
                PathBuf::from("/h"),
            )
            .err()
            .expect("overlapping mount must be rejected");
            assert!(err.contains(".agent-sync"), "{}: {}", over, err);
        }
    }

    #[cfg(unix)]
    #[test]
    fn roots_reject_existing_and_prospective_symlink_aliases() {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        let shared = home.path().join("shared");
        fs::create_dir_all(&shared).unwrap();
        let codex_link = home.path().join("a/.codex");
        let claude_link = home.path().join("b/.claude");
        fs::create_dir_all(codex_link.parent().unwrap()).unwrap();
        fs::create_dir_all(claude_link.parent().unwrap()).unwrap();
        symlink(&shared, &codex_link).unwrap();
        symlink(&shared, &claude_link).unwrap();
        // Two mounts aliasing one dir through symlinks: the pairwise check
        // at save time catches the canonical overlap.
        let aliases = config_with_profiles(vec![
            profile_at("codex", ".codex", &codex_link.to_string_lossy()),
            profile_at("claude", ".claude", &claude_link.to_string_lossy()),
        ]);
        assert!(validate_sync_config(&aliases)
            .unwrap_err()
            .contains("overlap"));

        fs::create_dir_all(home.path().join(".agent-sync")).unwrap();
        let container = home.path().join("app-alias");
        symlink(home.path().join(".agent-sync"), &container).unwrap();
        assert!(Roots::for_profile_with_home(
            &profile_at("codex", ".codex", &container.to_string_lossy()),
            home.path().to_path_buf(),
        )
        .is_err());

        let parent_escape = home.path().join("new/../.agent-sync");
        assert!(Roots::for_profile_with_home(
            &profile_at("codex", ".codex", &parent_escape.to_string_lossy()),
            home.path().to_path_buf(),
        )
        .is_err());
    }

    #[test]
    fn roots_profile_paths_are_validated() {
        let home = PathBuf::from("/h");
        let roots = Roots::for_profile_with_home(
            &profile_at("codex", ".codex", "/scratch/.codex"),
            home.clone(),
        )
        .unwrap();
        assert_eq!(roots.dir, PathBuf::from("/scratch/.codex"));
        assert_eq!(roots.remap, PathBuf::from("/h/.agent-sync/codex"));

        let default_claude =
            Roots::for_profile_with_home(&profile_at("claude", ".claude", ""), home.clone())
                .unwrap();
        assert!(default_claude.dir.ends_with(".claude"));

        assert!(Roots::for_profile_with_home(
            &profile_at("codex", ".codex", "scratch/.codex"),
            home.clone(),
        )
        .is_err());
        // Custom profiles get their own record dir, keyed by id — two
        // same-kind profiles never share locks.
        let custom = Roots::for_profile_with_home(
            &profile_at("abc123", ".claude", "/elsewhere/.claude"),
            home.clone(),
        )
        .unwrap();
        assert_eq!(custom.remap, PathBuf::from("/h/.agent-sync/abc123"));
        // Ids are the record-dir name: enforce the same safe charset.
        assert!(
            Roots::for_profile_with_home(&profile_at("Bad.Id", ".codex", ""), home.clone(),)
                .is_err()
        );

        assert_eq!(
            expand_home_relative_path("~/Desktop/project/myconf2", Path::new("/h")),
            PathBuf::from("/h/Desktop/project/myconf2")
        );
        assert_eq!(
            expand_home_relative_path("~", Path::new("/h")),
            PathBuf::from("/h")
        );
        assert_eq!(
            expand_home_relative_path("~other/config", Path::new("/h")),
            PathBuf::from("~other/config")
        );

        // Container semantics: a folder not named after the root hosts it
        // as a subdirectory, so one container can hold both roots …
        let codex =
            Roots::for_profile_with_home(&profile_at("codex", ".codex", "/x"), home.clone())
                .unwrap();
        let claude =
            Roots::for_profile_with_home(&profile_at("claude", ".claude", "/x"), home.clone())
                .unwrap();
        assert_eq!(codex.dir, PathBuf::from("/x/.codex"));
        assert_eq!(claude.dir, PathBuf::from("/x/.claude"));

        let flat = Roots::for_profile_with_home(
            &profile_at("claude", ".claude", "/x/.claude"),
            home.clone(),
        )
        .unwrap();
        assert_eq!(flat.dir, PathBuf::from("/x/.claude"));
    }

    #[test]
    fn overlapping_profile_mounts_are_rejected_at_save_time() {
        std::env::set_var("HOME", "/tmp");
        let overlap = config_with_profiles(vec![
            profile_at("codex", ".codex", "/x/.codex"),
            profile_at("claude", ".claude", "/x/.codex/deep"),
        ]);
        assert!(validate_sync_config(&overlap)
            .unwrap_err()
            .contains("overlap"));
        // Two same-kind profiles at the same path collide too.
        let twins = config_with_profiles(vec![
            profile_at("codex", ".codex", ""),
            profile_at("claude", ".claude", ""),
            profile_at("second", ".codex", "~"),
        ]);
        assert!(validate_sync_config(&twins).is_err());
    }

    #[test]
    fn local_store_must_not_overlap_the_active_mount() {
        let roots = Roots {
            home: PathBuf::from("/h"),
            root: ".codex".to_string(),
            dir: PathBuf::from("/scratch/.codex"),
            remap: PathBuf::from("/h/.agent-sync/codex"),
        };
        let local_storage = |dir: &str| StorageConfig {
            id: "s1".to_string(),
            kind: "local".to_string(),
            local_dir: dir.to_string(),
            ..Default::default()
        };
        assert!(
            make_store(&local_storage("/scratch/.codex/store"), Some(&roots))
                .err()
                .expect("must fail")
                .contains("overlaps")
        );
        assert!(make_store(&local_storage("/scratch"), Some(&roots))
            .err()
            .expect("must fail")
            .contains("overlaps"));
    }

    #[cfg(unix)]
    #[test]
    fn local_store_rejects_existing_and_fresh_symlink_aliases_into_a_root() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let codex = temp.path().join(".codex");
        fs::create_dir_all(&codex).unwrap();
        let alias = temp.path().join("store-alias");
        symlink(&codex, &alias).unwrap();
        let roots = Roots {
            home: temp.path().to_path_buf(),
            root: ".codex".to_string(),
            dir: codex.clone(),
            remap: temp.path().join(".agent-sync/codex"),
        };
        let local_storage = |dir: String| StorageConfig {
            id: "s1".to_string(),
            kind: "local".to_string(),
            local_dir: dir,
            ..Default::default()
        };
        assert!(make_store(
            &local_storage(alias.to_string_lossy().into_owned()),
            Some(&roots)
        )
        .is_err());

        assert!(make_store(
            &local_storage(alias.join("fresh-store").to_string_lossy().into_owned()),
            Some(&roots)
        )
        .is_err());
        assert!(!codex.join("fresh-store").exists());
    }

    // ── Sync-link cloud side: profile-prefix grammar ─────────────────────

    #[test]
    fn profile_id_grammar_accepts_names_and_hex_ids() {
        for ok in [
            "001",
            "001/.codex",
            "001/.claude",
            "team-a",
            "x.y_z-1",
            "0a9f2e6d1c4b8a3f0a9f2e6d1c4b8a3f", // legacy random hex
        ] {
            validate_profile_id(ok).unwrap_or_else(|e| panic!("{}: {}", ok, e));
        }
        for bad in [
            "",
            "_reserved",
            "001/_reserved",
            "a//b",
            "..",
            "001/..",
            "001/.",
            "A/b",         // uppercase
            "a b",         // space
            "/001",        // leading slash → empty segment
            "001/.codex/", // trailing slash → third empty segment
            "a/b/c",       // too deep
            "a\\b",
        ] {
            assert!(validate_profile_id(bad).is_err(), "must reject '{}'", bad);
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
// The commands are only wired into `generate_handler!` in the non-test
// build below; reference them here so test builds keep dead-code analysis
// active for everything else without flagging them.
#[cfg(test)]
#[allow(dead_code)]
fn commands_used_by_run() {
    let _ = (
        list_config_dirs,
        read_file_content,
        write_file_content,
        get_sync_config,
        save_sync_config,
        get_file_statuses,
        refresh_cloud_state,
        sync_upload,
        set_upload_paused,
        sync_download,
        list_sync_profiles,
        repair_plugins,
        capture_codex_plugin_lock,
        get_codex_plugin_plan,
        get_claude_plugin_plan,
        repair_codex_plugins,
        apply_sidebar_state,
        map_project_path,
        remove_project_path_mapping,
        list_project_path_mappings,
        get_setup_readiness,
        get_force_path_remap,
        set_force_path_remap,
        mark_hook_reviewed,
        dismiss_setup_issue,
        resolve_conflict_copy,
        setup_link,
        activity_log::query_activity_logs,
        activity_log::get_activity_log_stats,
        activity_log::update_activity_log_policy,
        activity_log::cleanup_activity_logs,
        activity_log::get_activity_log_folder,
        project_sync_v3::commands::get_project_sync_config,
        project_sync_v3::commands::save_project_sync_config,
        project_sync_v3::commands::list_local_projects,
        project_sync_v3::commands::get_project,
        project_sync_v3::commands::get_local_project,
        project_sync_v3::commands::list_project_repository_kinds,
        project_sync_v3::commands::get_project_chat_history,
        project_sync_v3::commands::get_project_chat_thread_details,
        project_sync_v3::commands::open_codex_thread_in_app,
        project_sync_v3::commands::open_codex_thread_in_terminal,
        project_sync_v3::commands::validate_codex_thread_ownership,
        project_sync_v3::commands::register_local_project,
        project_sync_v3::commands::remove_local_project,
        project_sync_v3::commands::rename_local_project,
        project_sync_v3::commands::save_bundle_recipe,
        project_sync_v3::commands::save_project_link,
        project_sync_v3::commands::connect_project_to_remote_bundle,
        project_sync_v3::commands::remove_project_link,
        project_sync_v3::commands::list_provider_profiles,
        project_sync_v3::commands::probe_provider_profile,
        project_sync_v3::commands::create_provider_profile,
        project_sync_v3::commands::rename_provider_profile,
        project_sync_v3::commands::remove_provider_profile,
        project_sync_v3::commands::list_project_bindings,
        project_sync_v3::commands::get_project_binding,
        project_sync_v3::commands::audit_codex_conversation_paths,
        project_sync_v3::commands::repair_codex_conversation_paths,
        project_sync_v3::commands::save_project_binding,
        project_sync_v3::commands::remove_project_binding,
        project_sync_v3::commands::list_project_materializations,
        project_sync_v3::commands::get_restore_plan,
        project_sync_v3::commands::discard_restore_plan,
        project_sync_v3::commands::discover_project,
        project_sync_v3::commands::get_bundle_inventory,
        project_sync_v3::commands::inspect_project_files,
        project_sync_v3::commands::list_remote_bundles,
        project_sync_v3::commands::list_remote_bundle_snapshots,
        project_sync_v3::commands::find_remote_bundle_matches,
        project_sync_v3::commands::fetch_bundle,
        project_sync_v3::commands::get_bundle_status,
        project_sync_v3::commands::get_project_capability_status,
        project_sync_v3::commands::get_project_thread_sync_comparison,
        project_sync_v3::commands::push_bundle,
        project_sync_v3::commands::plan_bundle_restore,
        project_sync_v3::commands::apply_bundle_restore,
        project_sync_v3::commands::plan_dependencies,
        project_sync_v3::commands::apply_dependency_actions,
        project_sync_v3::commands::get_bundle_readiness,
        project_sync_v3::commands::get_restore_readiness,
        project_sync_v3::commands::list_setup_drafts,
        project_sync_v3::commands::create_setup_draft,
        project_sync_v3::commands::get_setup_draft,
        project_sync_v3::commands::update_setup_draft,
        project_sync_v3::commands::discard_setup_draft,
        project_sync_v3::commands::inspect_setup_draft,
        project_sync_v3::commands::finalize_project_setup,
        UploadControl::default,
    );
}

// Not compiled under `cfg(test)`: the command signatures there are typed
// against the mock runtime, and `generate_handler!` would instantiate them
// for Wry.
#[cfg(not(test))]
pub fn run() {
    tauri::Builder::default()
        .manage(UploadControl::default())
        .manage(CloudCacheSlot::default())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            list_config_dirs,
            read_file_content,
            write_file_content,
            get_sync_config,
            save_sync_config,
            get_file_statuses,
            refresh_cloud_state,
            sync_upload,
            set_upload_paused,
            sync_download,
            list_sync_profiles,
            repair_plugins,
            capture_codex_plugin_lock,
            get_codex_plugin_plan,
            get_claude_plugin_plan,
            repair_codex_plugins,
            apply_sidebar_state,
            map_project_path,
            remove_project_path_mapping,
            list_project_path_mappings,
            get_setup_readiness,
            get_force_path_remap,
            set_force_path_remap,
            mark_hook_reviewed,
            dismiss_setup_issue,
            resolve_conflict_copy,
            setup_link,
            activity_log::query_activity_logs,
            activity_log::get_activity_log_stats,
            activity_log::update_activity_log_policy,
            activity_log::cleanup_activity_logs,
            activity_log::get_activity_log_folder,
            project_sync_v3::commands::get_project_sync_config,
            project_sync_v3::commands::save_project_sync_config,
            project_sync_v3::commands::list_local_projects,
            project_sync_v3::commands::get_project,
            project_sync_v3::commands::get_local_project,
            project_sync_v3::commands::list_project_repository_kinds,
            project_sync_v3::commands::get_project_chat_history,
            project_sync_v3::commands::get_project_chat_thread_details,
            project_sync_v3::commands::open_codex_thread_in_app,
            project_sync_v3::commands::open_codex_thread_in_terminal,
            project_sync_v3::commands::validate_codex_thread_ownership,
            project_sync_v3::commands::register_local_project,
            project_sync_v3::commands::remove_local_project,
            project_sync_v3::commands::rename_local_project,
            project_sync_v3::commands::save_bundle_recipe,
            project_sync_v3::commands::save_project_link,
            project_sync_v3::commands::connect_project_to_remote_bundle,
            project_sync_v3::commands::remove_project_link,
            project_sync_v3::commands::list_provider_profiles,
            project_sync_v3::commands::probe_provider_profile,
            project_sync_v3::commands::create_provider_profile,
            project_sync_v3::commands::rename_provider_profile,
            project_sync_v3::commands::remove_provider_profile,
            project_sync_v3::commands::list_project_bindings,
            project_sync_v3::commands::get_project_binding,
            project_sync_v3::commands::audit_codex_conversation_paths,
            project_sync_v3::commands::repair_codex_conversation_paths,
            project_sync_v3::commands::save_project_binding,
            project_sync_v3::commands::remove_project_binding,
            project_sync_v3::commands::list_project_materializations,
            project_sync_v3::commands::get_restore_plan,
            project_sync_v3::commands::discard_restore_plan,
            project_sync_v3::commands::discover_project,
            project_sync_v3::commands::get_bundle_inventory,
            project_sync_v3::commands::inspect_project_files,
            project_sync_v3::commands::list_remote_bundles,
            project_sync_v3::commands::list_remote_bundle_snapshots,
            project_sync_v3::commands::find_remote_bundle_matches,
            project_sync_v3::commands::fetch_bundle,
            project_sync_v3::commands::get_bundle_status,
            project_sync_v3::commands::get_project_capability_status,
            project_sync_v3::commands::get_project_thread_sync_comparison,
            project_sync_v3::commands::push_bundle,
            project_sync_v3::commands::plan_bundle_restore,
            project_sync_v3::commands::apply_bundle_restore,
            project_sync_v3::commands::plan_dependencies,
            project_sync_v3::commands::apply_dependency_actions,
            project_sync_v3::commands::get_bundle_readiness,
            project_sync_v3::commands::get_restore_readiness,
            project_sync_v3::commands::list_setup_drafts,
            project_sync_v3::commands::create_setup_draft,
            project_sync_v3::commands::get_setup_draft,
            project_sync_v3::commands::update_setup_draft,
            project_sync_v3::commands::discard_setup_draft,
            project_sync_v3::commands::inspect_setup_draft,
            project_sync_v3::commands::finalize_project_setup,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
