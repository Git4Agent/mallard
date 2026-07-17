//! Portable Codex desktop sidebar state: capture → merge → explicit apply.
//!
//! Implements Part B of PLAN_CODEX_THREAD_REBUILD_AND_SIDEBAR.md. The raw
//! `.codex-global-state.json` mixes portable sidebar state with machine and
//! account identity, so it never syncs; this module captures a bounded,
//! secret-free subset into a lock (logical `.codex/agent-sync/
//! codex-sidebar.lock.json`, physically under `~/.agent-sync/` like the
//! plugin locks) and applies it additively on another machine. Exclusion is
//! structural: capture reads only the whitelisted keys below, so account
//! ids, window bounds, heartbeat permissions, prompt drafts/history and the
//! rest of the D5 list cannot ride along.
//!
//! Everything here is Tauri-free; filesystem/process concerns stay in
//! `lib.rs`, and lookups the tests need to fake are injected closures.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Logical (root-relative) path of the lock. Also referenced by the
/// allowlist and the Tier 2 merge-driver dispatch in `lib.rs`.
pub const LOCK_REL: &str = ".codex/agent-sync/codex-sidebar.lock.json";

/// The Codex desktop global-state file, relative to the `.codex` root.
/// Never synced (Never tier) — capture input and apply target only.
pub const GLOBAL_STATE_FILE: &str = ".codex-global-state.json";

const LOCK_SCHEMA: u32 = 1;
const MAX_LOCK_BYTES: u64 = 1024 * 1024;
const MAX_STATE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ENTRIES: usize = 512;
const MAX_STRING: usize = 1024;

// Key names verified against the installed desktop app (2026-07). Projects
// and their order live at the top level; thread titles and display prefs
// live inside the persisted atom map.
const KEY_ROOTS: &str = "electron-saved-workspace-roots";
const KEY_ORDER: &str = "project-order";
const KEY_ATOM: &str = "electron-persisted-atom-state";
const KEY_DESCRIPTIONS: &str = "thread-descriptions-v1";
const KEY_PREFS: &str = "flat-project-sidebar-preferences-v1";

// ── Lock file model ──────────────────────────────────────────────────────────
// Field order is the canonical serialization order; keep it stable.

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct CodexSidebarLock {
    pub schema: u32,
    #[serde(default)]
    pub projects: Vec<SidebarProject>,
    /// Source-machine paths; identity-for-ordering only, filtered to
    /// `projects` at capture. Whole-array on merge collisions (see union).
    #[serde(default)]
    pub project_order: Vec<String>,
    /// thread id → user-edited title.
    #[serde(default)]
    pub thread_descriptions: BTreeMap<String, String>,
    #[serde(default)]
    pub sidebar: SidebarPrefs,
}

/// The desktop stores projects as bare paths (name derives from the path);
/// the normalized git origin is captured from the local clone so another
/// machine can identity-match without the path existing there.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct SidebarProject {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_origin: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct SidebarPrefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_sort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_sort: Option<String>,
}

/// Merge/dedup identity: same repo on two machines matches by origin even
/// when the clone paths differ; origin-less projects fall back to the path.
fn identity(project: &SidebarProject) -> String {
    project
        .git_origin
        .clone()
        .unwrap_or_else(|| project.path.clone())
}

// ── Validation / canonical serialization ────────────────────────────────────

fn ok_text(value: &str) -> bool {
    value.len() <= MAX_STRING && !value.chars().any(|c| c.is_control())
}

pub fn validate_lock(lock: &CodexSidebarLock) -> Result<(), String> {
    if lock.schema != LOCK_SCHEMA {
        return Err(format!(
            "unsupported sidebar lock schema {} (this app understands {})",
            lock.schema, LOCK_SCHEMA
        ));
    }
    if lock.projects.len() > MAX_ENTRIES
        || lock.project_order.len() > MAX_ENTRIES
        || lock.thread_descriptions.len() > MAX_ENTRIES
    {
        return Err(format!("sidebar lock exceeds {} entries", MAX_ENTRIES));
    }
    for p in &lock.projects {
        if p.path.is_empty() || !ok_text(&p.path) {
            return Err(format!("invalid project path '{}'", p.path));
        }
        if p.git_origin
            .as_deref()
            .is_some_and(|o| o.is_empty() || !ok_text(o))
        {
            return Err(format!("invalid git origin for '{}'", p.path));
        }
    }
    for path in &lock.project_order {
        if path.is_empty() || !ok_text(path) {
            return Err("invalid project_order entry".to_string());
        }
    }
    for (id, title) in &lock.thread_descriptions {
        if id.is_empty()
            || id.len() > 128
            || id.chars().any(|c| c.is_control() || c.is_whitespace())
            || !ok_text(title)
        {
            return Err(format!("invalid thread description entry '{}'", id));
        }
    }
    for pref in [
        &lock.sidebar.mode,
        &lock.sidebar.project_sort,
        &lock.sidebar.chat_sort,
    ] {
        if pref.as_deref().is_some_and(|v| v.len() > 64 || !ok_text(v)) {
            return Err("invalid sidebar preference value".to_string());
        }
    }
    Ok(())
}

/// Symmetric keyed max: collisions resolve to the `Ord`-greater entry no
/// matter which machine merges (same shape as the plugin-lock union).
fn keyed_max(items: impl IntoIterator<Item = SidebarProject>) -> Vec<SidebarProject> {
    let mut map: BTreeMap<String, SidebarProject> = BTreeMap::new();
    for item in items {
        match map.get(&identity(&item)) {
            Some(existing) if *existing >= item => {}
            _ => {
                map.insert(identity(&item), item);
            }
        }
    }
    map.into_values().collect()
}

fn canonicalize(lock: &mut CodexSidebarLock) {
    lock.projects = keyed_max(std::mem::take(&mut lock.projects));
}

/// Byte-deterministic regardless of which machine serializes: identity-
/// unique sorted projects, fixed field order, pretty JSON, trailing newline.
/// Required so independent Tier 2 merges on two machines converge.
pub fn canonical_lock_json(lock: &CodexSidebarLock) -> String {
    let mut lock = lock.clone();
    canonicalize(&mut lock);
    let mut out = serde_json::to_string_pretty(&lock).unwrap_or_else(|_| "{}".to_string());
    out.push('\n');
    out
}

pub fn read_lock(path: &Path) -> Result<CodexSidebarLock, String> {
    let meta = fs::metadata(path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    if meta.len() > MAX_LOCK_BYTES {
        return Err(format!("sidebar lock exceeds {} bytes", MAX_LOCK_BYTES));
    }
    let raw = fs::read_to_string(path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    let lock: CodexSidebarLock =
        serde_json::from_str(&raw).map_err(|e| format!("parse sidebar lock: {}", e))?;
    validate_lock(&lock)?;
    Ok(lock)
}

#[cfg_attr(test, allow(dead_code))] // prod-only caller: the pre-push capture hook
fn lock_is_empty(lock: &CodexSidebarLock) -> bool {
    lock.projects.is_empty()
        && lock.thread_descriptions.is_empty()
        && lock.sidebar == SidebarPrefs::default()
}

/// Atomic write that never clobbers a good lock with an empty capture and
/// skips the write when nothing changed (same contract as the plugin
/// `save_lock`). Returns whether the file changed.
#[cfg_attr(test, allow(dead_code))]
pub fn save_lock(path: &Path, lock: &CodexSidebarLock) -> Result<bool, String> {
    validate_lock(lock)?;
    if lock_is_empty(lock) {
        return Ok(false);
    }
    let existing = path.is_file().then(|| read_lock(path)).and_then(Result::ok);
    let bytes = canonical_lock_json(lock);
    if existing.is_some_and(|e| canonical_lock_json(&e) == bytes) {
        return Ok(false);
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("no parent directory for {}", path.display()))?;
    fs::create_dir_all(parent).map_err(|e| format!("create {}: {}", parent.display(), e))?;
    let tmp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| format!("temp file in {}: {}", parent.display(), e))?;
    fs::write(tmp.path(), &bytes).map_err(|e| format!("write sidebar lock: {}", e))?;
    tmp.persist(path)
        .map_err(|e| format!("replace {}: {}", path.display(), e))?;
    Ok(true)
}

// ── Capture ──────────────────────────────────────────────────────────────────

fn string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str())
                .filter(|s| !s.is_empty() && ok_text(s))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Pure capture from a parsed global-state document. Reads exactly the four
/// whitelisted keys; `project-order` entries that are not captured project
/// paths (the desktop mixes in remote-project UUIDs, which are account-tied)
/// are dropped. `origin_of` maps a project path to its normalized git
/// origin — injected so tests never need real clones.
pub fn capture_from_global_state(
    state: &serde_json::Value,
    origin_of: &dyn Fn(&str) -> Option<String>,
) -> CodexSidebarLock {
    let atom = state.get(KEY_ATOM);
    let mut lock = CodexSidebarLock {
        schema: LOCK_SCHEMA,
        ..CodexSidebarLock::default()
    };
    let paths = string_array(state.get(KEY_ROOTS));
    lock.projects = paths
        .iter()
        .take(MAX_ENTRIES)
        .map(|path| SidebarProject {
            path: path.clone(),
            git_origin: origin_of(path),
        })
        .collect();
    lock.project_order = string_array(state.get(KEY_ORDER))
        .into_iter()
        .filter(|entry| paths.contains(entry))
        .take(MAX_ENTRIES)
        .collect();
    if let Some(map) = atom
        .and_then(|a| a.get(KEY_DESCRIPTIONS))
        .and_then(|v| v.as_object())
    {
        for (id, title) in map {
            if lock.thread_descriptions.len() >= MAX_ENTRIES {
                break;
            }
            let Some(title) = title.as_str() else {
                continue;
            };
            if !id.is_empty()
                && id.len() <= 128
                && !id.chars().any(|c| c.is_control() || c.is_whitespace())
                && !title.is_empty()
                && ok_text(title)
            {
                lock.thread_descriptions
                    .insert(id.clone(), title.to_string());
            }
        }
    }
    if let Some(prefs) = atom.and_then(|a| a.get(KEY_PREFS)) {
        let pref = |key: &str| {
            prefs
                .get(key)
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty() && v.len() <= 64 && ok_text(v))
                .map(str::to_string)
        };
        lock.sidebar = SidebarPrefs {
            mode: pref("mode"),
            project_sort: pref("projectSortMode"),
            chat_sort: pref("chatSortMode"),
        };
    }
    lock
}

fn read_global_state(codex_dir: &Path) -> Result<serde_json::Value, String> {
    let path = codex_dir.join(GLOBAL_STATE_FILE);
    let meta = fs::metadata(&path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    if meta.len() > MAX_STATE_BYTES {
        return Err(format!(
            "{} exceeds {} bytes",
            GLOBAL_STATE_FILE, MAX_STATE_BYTES
        ));
    }
    let raw = fs::read_to_string(&path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    serde_json::from_str(&raw).map_err(|e| format!("parse {}: {}", GLOBAL_STATE_FILE, e))
}

/// Pre-push capture into the lock file. `Ok(false)` when the desktop state
/// file is absent or nothing changed; errors never block a push (the caller
/// logs and keeps the last captured lock).
#[cfg_attr(test, allow(dead_code))]
pub fn capture_to(lock_path: &Path, codex_dir: &Path) -> Result<bool, String> {
    if !codex_dir.join(GLOBAL_STATE_FILE).is_file() {
        return Ok(false);
    }
    let state = read_global_state(codex_dir)?;
    let origin_of = |path: &str| git_origin_of_dir(Path::new(path));
    let lock = capture_from_global_state(&state, &origin_of);
    save_lock(lock_path, &lock)
}

// ── Git origin discovery / normalization ────────────────────────────────────

/// Identity for cross-machine matching, not a fetchable URL: scheme and
/// userinfo/credentials stripped, query/fragment stripped, scp-style
/// `host:path` folded to `host/path`, `.git` suffix dropped, lowercased.
pub fn normalize_git_origin(url: &str) -> Option<String> {
    let url = url.trim().split(['?', '#']).next().unwrap_or("").trim();
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
    let rest = rest.replacen(':', "/", 1);
    let rest = rest.trim_end_matches('/');
    let rest = rest
        .strip_suffix(".git")
        .unwrap_or(rest)
        .trim_end_matches('/');
    (!rest.is_empty() && rest.contains('/')).then(|| rest.to_ascii_lowercase())
}

/// `[remote "origin"] url` from `<dir>/.git/config`, normalized. Best-effort:
/// any missing/odd layout is None.
/// ponytail: plain clones only — a worktree's `.git` *file* (gitdir
/// redirection) is not followed; resolve it if worktree projects show up
/// unmatched in practice.
pub fn git_origin_of_dir(dir: &Path) -> Option<String> {
    let text = fs::read_to_string(dir.join(".git/config")).ok()?;
    let mut in_origin = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_origin = line == "[remote \"origin\"]";
        } else if in_origin {
            if let Some(rest) = line.strip_prefix("url") {
                if let Some(value) = rest.trim_start().strip_prefix('=') {
                    return normalize_git_origin(value);
                }
            }
        }
    }
    None
}

// ── Tier 2 merge driver ──────────────────────────────────────────────────────

fn union(a: CodexSidebarLock, b: CodexSidebarLock) -> CodexSidebarLock {
    let mut descriptions = a.thread_descriptions;
    for (id, title) in b.thread_descriptions {
        match descriptions.get(&id) {
            Some(existing) if *existing >= title => {}
            _ => {
                descriptions.insert(id, title);
            }
        }
    }
    CodexSidebarLock {
        schema: a.schema.max(b.schema),
        projects: keyed_max(a.projects.into_iter().chain(b.projects)),
        // ponytail: order is one user preference list, not mergeable data —
        // whole-array max on collision; per-entry rank merging is the
        // upgrade path if whichever-sorts-greater ever annoys in practice.
        project_order: a.project_order.max(b.project_order),
        thread_descriptions: descriptions,
        sidebar: a.sidebar.max(b.sidebar),
    }
}

/// Tier 2 driver (see AGENT_SYNC_FILE_SETS.md): keyed union so both
/// machines' sidebar state survives a concurrent push. Byte-deterministic
/// and symmetric; a side that fails to parse loses to the side that parses,
/// and two unparseable sides fall back to the lexically greater bytes.
pub fn merge_sidebar_lock(local: &str, cloud: &str) -> String {
    let parse = |raw: &str| -> Option<CodexSidebarLock> {
        if raw.len() as u64 > MAX_LOCK_BYTES {
            return None;
        }
        let lock: CodexSidebarLock = serde_json::from_str(raw).ok()?;
        validate_lock(&lock).ok()?;
        Some(lock)
    };
    match (parse(local), parse(cloud)) {
        (Some(a), Some(b)) => canonical_lock_json(&union(a, b)),
        (Some(a), None) => canonical_lock_json(&a),
        (None, Some(b)) => canonical_lock_json(&b),
        (None, None) => std::cmp::max(local, cloud).to_string(),
    }
}

// ── Apply: explicit, additive, identity-matched ──────────────────────────────

#[derive(Debug, Default, PartialEq)]
pub struct SidebarApplyPlan {
    /// Local paths to add to the saved projects (lock projects whose path
    /// exists here but is not in the sidebar yet).
    pub add_projects: Vec<String>,
    /// thread id → title, limited to ids absent locally whose rollout
    /// exists on this machine.
    pub set_descriptions: BTreeMap<String, String>,
    /// Atom-state pref key → value, only where the local value differs.
    pub set_prefs: Vec<(String, String)>,
    /// Lock paths with no local match — surfaced, never invented.
    pub unmatched: Vec<String>,
}

impl SidebarApplyPlan {
    pub fn has_changes(&self) -> bool {
        !self.add_projects.is_empty()
            || !self.set_descriptions.is_empty()
            || !self.set_prefs.is_empty()
    }

    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.add_projects.is_empty() {
            parts.push(format!("{} project(s) to add", self.add_projects.len()));
        }
        if !self.set_descriptions.is_empty() {
            parts.push(format!("{} thread title(s)", self.set_descriptions.len()));
        }
        if !self.set_prefs.is_empty() {
            parts.push("display preferences".to_string());
        }
        if !self.unmatched.is_empty() {
            parts.push(format!(
                "{} project(s) have no local folder (clone or attach them manually)",
                self.unmatched.len()
            ));
        }
        parts.join(", ")
    }
}

/// Plan the additive merge of a lock into a local global-state document.
/// Match order per the plan: exact local path on disk, then git origin
/// against a locally-listed project, else unmatched. `rollout_exists`
/// answers "does this thread id have a rollout on this machine".
pub fn plan_apply(
    lock: &CodexSidebarLock,
    state: &serde_json::Value,
    path_exists: &dyn Fn(&str) -> bool,
    local_origin_of: &dyn Fn(&str) -> Option<String>,
    rollout_exists: &dyn Fn(&str) -> bool,
) -> SidebarApplyPlan {
    let mut plan = SidebarApplyPlan::default();
    let local_paths = string_array(state.get(KEY_ROOTS));
    for project in &lock.projects {
        if local_paths.contains(&project.path) {
            continue; // already listed
        }
        if path_exists(&project.path) {
            plan.add_projects.push(project.path.clone());
            continue;
        }
        let origin_matched = project.git_origin.as_deref().is_some_and(|origin| {
            local_paths
                .iter()
                .any(|local| local_origin_of(local).as_deref() == Some(origin))
        });
        if !origin_matched {
            plan.unmatched.push(project.path.clone());
        }
    }
    let local_descriptions = state
        .get(KEY_ATOM)
        .and_then(|a| a.get(KEY_DESCRIPTIONS))
        .and_then(|v| v.as_object());
    for (id, title) in &lock.thread_descriptions {
        let present = local_descriptions.is_some_and(|map| {
            map.get(id)
                .and_then(|v| v.as_str())
                .is_some_and(|v| !v.is_empty())
        });
        if !present && rollout_exists(id) {
            plan.set_descriptions.insert(id.clone(), title.clone());
        }
    }
    let local_prefs = state.get(KEY_ATOM).and_then(|a| a.get(KEY_PREFS));
    for (key, value) in [
        ("mode", &lock.sidebar.mode),
        ("projectSortMode", &lock.sidebar.project_sort),
        ("chatSortMode", &lock.sidebar.chat_sort),
    ] {
        if let Some(value) = value {
            let local = local_prefs
                .and_then(|p| p.get(key))
                .and_then(|v| v.as_str());
            if local != Some(value.as_str()) {
                plan.set_prefs.push((key.to_string(), value.clone()));
            }
        }
    }
    plan
}

/// Mutate the global-state document per the plan. Strictly additive: appends
/// projects (and their order entries), inserts missing thread titles, sets
/// the three display prefs — never removes or renames anything, and never
/// touches a key outside the whitelist.
pub fn apply_plan_to_state(state: &mut serde_json::Value, plan: &SidebarApplyPlan) {
    let Some(root) = state.as_object_mut() else {
        return;
    };
    if !plan.add_projects.is_empty() {
        for key in [KEY_ROOTS, KEY_ORDER] {
            let entry = root.entry(key).or_insert_with(|| serde_json::json!([]));
            if !entry.is_array() {
                *entry = serde_json::json!([]);
            }
            let items = entry.as_array_mut().expect("ensured above");
            for path in &plan.add_projects {
                if !items.iter().any(|v| v.as_str() == Some(path)) {
                    items.push(serde_json::Value::String(path.clone()));
                }
            }
        }
    }
    if plan.set_descriptions.is_empty() && plan.set_prefs.is_empty() {
        return;
    }
    let atom = root
        .entry(KEY_ATOM)
        .or_insert_with(|| serde_json::json!({}));
    let Some(atom) = atom.as_object_mut() else {
        return;
    };
    if !plan.set_descriptions.is_empty() {
        let descriptions = atom
            .entry(KEY_DESCRIPTIONS)
            .or_insert_with(|| serde_json::json!({}));
        if let Some(map) = descriptions.as_object_mut() {
            for (id, title) in &plan.set_descriptions {
                map.insert(id.clone(), serde_json::Value::String(title.clone()));
            }
        }
    }
    if !plan.set_prefs.is_empty() {
        let prefs = atom
            .entry(KEY_PREFS)
            .or_insert_with(|| serde_json::json!({}));
        if let Some(map) = prefs.as_object_mut() {
            for (key, value) in &plan.set_prefs {
                map.insert(key.clone(), serde_json::Value::String(value.clone()));
            }
        }
    }
}

/// Plan against the real filesystem: existing dirs, local `.git` origins,
/// and rollout filenames under `<codex_dir>/sessions` (rollout names embed
/// the thread id).
pub fn plan_for_codex_dir(
    lock: &CodexSidebarLock,
    codex_dir: &Path,
    state: &serde_json::Value,
) -> SidebarApplyPlan {
    let rollouts: Vec<String> = walkdir::WalkDir::new(codex_dir.join("sessions"))
        .follow_links(false)
        .max_depth(5)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    let path_exists = |path: &str| Path::new(path).is_dir();
    let local_origin_of = |path: &str| git_origin_of_dir(Path::new(path));
    let rollout_exists = |id: &str| rollouts.iter().any(|name| name.contains(id));
    plan_apply(lock, state, &path_exists, &local_origin_of, &rollout_exists)
}

/// The pending-work summary for readiness: Some(detail) when the merged
/// lock holds something not yet reflected in this machine's desktop state.
pub fn pending_summary(lock_path: &Path, codex_dir: &Path) -> Option<String> {
    let lock = read_lock(lock_path).ok()?;
    let state = read_global_state(codex_dir).unwrap_or_else(|_| serde_json::json!({}));
    let plan = plan_for_codex_dir(&lock, codex_dir, &state);
    (plan.has_changes() || !plan.unmatched.is_empty()).then(|| plan.summary())
}

/// Execute the additive apply against the desktop state file: backup, plan,
/// mutate, temp-file + rename. The caller guards against a running desktop
/// app; this stays pure file work.
pub fn apply_from_lock(lock_path: &Path, codex_dir: &Path) -> Result<String, String> {
    let lock = read_lock(lock_path)?;
    let state_path = codex_dir.join(GLOBAL_STATE_FILE);
    let mut state = if state_path.is_file() {
        read_global_state(codex_dir)?
    } else {
        serde_json::json!({})
    };
    let plan = plan_for_codex_dir(&lock, codex_dir, &state);
    if !plan.has_changes() {
        return Ok(if plan.unmatched.is_empty() {
            "Sidebar state already reflected locally".to_string()
        } else {
            plan.summary()
        });
    }
    if state_path.is_file() {
        let backup = codex_dir.join(format!("{}.agent-sync.bak", GLOBAL_STATE_FILE));
        fs::copy(&state_path, &backup)
            .map_err(|e| format!("backup {}: {}", backup.display(), e))?;
    }
    apply_plan_to_state(&mut state, &plan);
    let bytes = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
    let parent = state_path
        .parent()
        .ok_or_else(|| format!("no parent directory for {}", state_path.display()))?;
    let tmp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| format!("temp file in {}: {}", parent.display(), e))?;
    fs::write(tmp.path(), &bytes).map_err(|e| format!("write desktop state: {}", e))?;
    tmp.persist(&state_path)
        .map_err(|e| format!("replace {}: {}", state_path.display(), e))?;
    Ok(plan.summary())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// The verified shape of the desktop file, including D5-excluded keys
    /// that must never leak into a capture.
    fn fixture_state() -> serde_json::Value {
        serde_json::json!({
            "electron-saved-workspace-roots": ["/work/repo-a", "/work/repo-b"],
            "project-order": ["/work/repo-b", "beb76333-3c14-42d4-89a5-3577d2e52da6", "/work/repo-a"],
            "electron-persisted-atom-state": {
                "thread-descriptions-v1": { "019f-aaaa": "First thread", "019f-bbbb": "Second thread" },
                "flat-project-sidebar-preferences-v1": {
                    "chatSortMode": "priority", "initialized": true,
                    "mode": "project", "projectSortMode": "priority"
                },
                "heartbeat-thread-permissions-by-id": { "019f-aaaa": { "allow": true } },
                "prompt-history": { "global": ["secret prompt"] },
                "composer-prompt-drafts-v1": { "d": "draft text" }
            },
            "electron-main-window-bounds": { "x": 1, "y": 2 },
            "electron-local-remote-control-installation-id": "install-123",
            "selected-remote-host-id": "host-9"
        })
    }

    fn fixture_origin(path: &str) -> Option<String> {
        match path {
            "/work/repo-a" => Some("github.com/acme/repo-a".to_string()),
            _ => None,
        }
    }

    #[test]
    fn capture_extracts_whitelist_and_excludes_d5_keys() {
        let lock = capture_from_global_state(&fixture_state(), &fixture_origin);
        assert_eq!(lock.projects.len(), 2);
        assert_eq!(lock.projects[0].path, "/work/repo-a");
        assert_eq!(
            lock.projects[0].git_origin.as_deref(),
            Some("github.com/acme/repo-a")
        );
        // Remote-project UUIDs (account-tied) drop out of the order.
        assert_eq!(lock.project_order, vec!["/work/repo-b", "/work/repo-a"]);
        assert_eq!(lock.thread_descriptions.len(), 2);
        assert_eq!(lock.sidebar.mode.as_deref(), Some("project"));
        assert_eq!(lock.sidebar.project_sort.as_deref(), Some("priority"));

        let serialized = canonical_lock_json(&lock);
        for excluded in [
            "heartbeat",
            "prompt",
            "draft",
            "window",
            "installation",
            "install-123",
            "host-9",
            "remote-control",
            "onboarding",
            "secret",
        ] {
            assert!(
                !serialized.contains(excluded),
                "'{}' leaked into the lock:\n{}",
                excluded,
                serialized
            );
        }
        assert!(validate_lock(&lock).is_ok());
    }

    #[test]
    fn merge_is_symmetric_idempotent_and_unions_by_identity() {
        let a_lock = CodexSidebarLock {
            schema: 1,
            projects: vec![
                SidebarProject {
                    path: "/a/shared".into(),
                    git_origin: Some("github.com/x/shared".into()),
                },
                SidebarProject {
                    path: "/a/only-a".into(),
                    git_origin: None,
                },
            ],
            project_order: vec!["/a/shared".into(), "/a/only-a".into()],
            thread_descriptions: [("t1".to_string(), "from A".to_string())].into(),
            sidebar: SidebarPrefs {
                mode: Some("project".into()),
                ..Default::default()
            },
        };
        let b_lock = CodexSidebarLock {
            schema: 1,
            projects: vec![
                // Same repo, different clone path: one entry survives.
                SidebarProject {
                    path: "/b/shared".into(),
                    git_origin: Some("github.com/x/shared".into()),
                },
                SidebarProject {
                    path: "/b/only-b".into(),
                    git_origin: None,
                },
            ],
            project_order: vec!["/b/only-b".into()],
            thread_descriptions: [
                ("t1".to_string(), "from B".to_string()),
                ("t2".to_string(), "b only".to_string()),
            ]
            .into(),
            sidebar: SidebarPrefs::default(),
        };
        let a = canonical_lock_json(&a_lock);
        let b = canonical_lock_json(&b_lock);

        let ab = merge_sidebar_lock(&a, &b);
        let ba = merge_sidebar_lock(&b, &a);
        assert_eq!(ab, ba, "merge must be symmetric");
        assert_eq!(merge_sidebar_lock(&ab, &b), ab, "merge must be idempotent");
        assert_eq!(merge_sidebar_lock(&ab, &a), ab);

        let merged: CodexSidebarLock = serde_json::from_str(&ab).unwrap();
        assert_eq!(
            merged.projects.len(),
            3,
            "identity union: {:?}",
            merged.projects
        );
        // Ord-max collision on the shared identity and on t1.
        assert!(merged.projects.iter().any(|p| p.path == "/b/shared"));
        assert_eq!(merged.thread_descriptions["t1"], "from B");
        assert_eq!(merged.thread_descriptions["t2"], "b only");
        // Whole-value winners: greater order array, greater prefs object.
        assert_eq!(merged.project_order, vec!["/b/only-b"]);
        assert_eq!(merged.sidebar.mode.as_deref(), Some("project"));

        // Unparseable side loses to the parsing side.
        assert_eq!(merge_sidebar_lock("not json", &a), a);
        assert_eq!(merge_sidebar_lock(&a, "not json"), a);
        assert_eq!(merge_sidebar_lock("zz", "aa"), "zz");
    }

    #[test]
    fn plan_is_additive_and_identity_matched() {
        let lock = CodexSidebarLock {
            schema: 1,
            projects: vec![
                SidebarProject {
                    path: "/local/listed".into(),
                    git_origin: None,
                },
                SidebarProject {
                    path: "/exists/on-disk".into(),
                    git_origin: None,
                },
                SidebarProject {
                    path: "/other/clone".into(),
                    git_origin: Some("github.com/x/shared".into()),
                },
                SidebarProject {
                    path: "/gone/nowhere".into(),
                    git_origin: Some("github.com/x/gone".into()),
                },
            ],
            thread_descriptions: [
                ("t-here".to_string(), "portable title".to_string()),
                ("t-local".to_string(), "cloud title".to_string()),
                ("t-norollout".to_string(), "no rollout here".to_string()),
            ]
            .into(),
            sidebar: SidebarPrefs {
                mode: Some("project".into()),
                project_sort: Some("priority".into()),
                chat_sort: None,
            },
            ..Default::default()
        };
        let state = serde_json::json!({
            "electron-saved-workspace-roots": ["/local/listed"],
            "electron-persisted-atom-state": {
                "thread-descriptions-v1": { "t-local": "local edit wins" },
                "flat-project-sidebar-preferences-v1": { "mode": "project" }
            }
        });
        let path_exists = |p: &str| p == "/exists/on-disk";
        let local_origin =
            |p: &str| (p == "/local/listed").then(|| "github.com/x/shared".to_string());
        let rollout = |id: &str| id == "t-here" || id == "t-local";
        let plan = plan_apply(&lock, &state, &path_exists, &local_origin, &rollout);

        assert_eq!(plan.add_projects, vec!["/exists/on-disk"]);
        // Origin matched an already-listed project: satisfied, not unmatched.
        assert_eq!(plan.unmatched, vec!["/gone/nowhere"]);
        // Only the id with a local rollout and no local title lands.
        assert_eq!(plan.set_descriptions.len(), 1);
        assert_eq!(plan.set_descriptions["t-here"], "portable title");
        // Only the pref that differs locally is set.
        assert_eq!(
            plan.set_prefs,
            vec![("projectSortMode".to_string(), "priority".to_string())]
        );

        let mut applied = state.clone();
        apply_plan_to_state(&mut applied, &plan);
        let roots: Vec<&str> = applied["electron-saved-workspace-roots"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(roots, vec!["/local/listed", "/exists/on-disk"]);
        let atom = &applied["electron-persisted-atom-state"];
        assert_eq!(atom["thread-descriptions-v1"]["t-local"], "local edit wins");
        assert_eq!(atom["thread-descriptions-v1"]["t-here"], "portable title");
        assert_eq!(
            atom["flat-project-sidebar-preferences-v1"]["projectSortMode"],
            "priority"
        );
        // Nothing outside the whitelist appears.
        assert!(applied.get("electron-main-window-bounds").is_none());

        // Re-planning after the apply is a no-op.
        let again = plan_apply(&lock, &applied, &path_exists, &local_origin, &rollout);
        assert!(!again.has_changes(), "{:?}", again);
    }

    #[test]
    fn git_origin_normalization_matches_across_url_forms() {
        for (input, expected) in [
            (
                "https://github.com/Acme/Repo.git",
                Some("github.com/acme/repo"),
            ),
            (
                "https://user:token@github.com/acme/repo?x=1#frag",
                Some("github.com/acme/repo"),
            ),
            ("git@github.com:acme/repo.git", Some("github.com/acme/repo")),
            (
                "ssh://git@github.com/acme/repo/",
                Some("github.com/acme/repo"),
            ),
            ("", None),
            ("no-slashes", None),
        ] {
            assert_eq!(
                normalize_git_origin(input).as_deref(),
                expected,
                "{}",
                input
            );
        }
    }
}
