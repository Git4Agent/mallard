//! Read-only post-pull readiness (PLAN_PORTABLE_AGENT_SETUP_V2.md).
//!
//! Parses this machine's own synced files — raw configs, agent TOMLs,
//! skills, conflict-copy siblings — plus the already-computed plugin plans,
//! and reports what is present but not yet usable locally. No setup lock,
//! no new synced artifact (D8): the sync engine's conflict copies are the
//! conflict representation. Everything here is Tauri-free and filesystem-
//! driven so tests run it on fixtures; binary/env lookups are injected.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use crate::codex_plugins::CodexPluginPlan;
use crate::project_paths;

const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const MAX_PROMPT_BYTES: u64 = 256 * 1024;
const MAX_FIRST_LINE_BYTES: usize = 64 * 1024;
const MAX_DISMISSED: usize = 512;

/// Dev/simulation switch: when this env var is set, project-path readiness
/// treats every synced source path as foreign even if it exists locally, so
/// the mapping flow can be exercised on the machine that pushed the profile.
pub const FORCE_PATH_REMAP_ENV: &str = "AGENT_SYNC_FORCE_PATH_REMAP";

// ── Local persistent state (~/.agent-sync/local-state.json) ─────────────────

/// Machine-local readiness memory. Updated only by the explicit
/// mark-reviewed / dismiss actions, never by the scan; sits at the top of
/// `~/.agent-sync/` so it is structurally unsyncable (outside the remapped
/// subtrees). Losing it re-raises issues but loses no data.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct LocalState {
    #[serde(default)]
    pub schema: u32,
    /// normalized-definition hash → reviewed_at (epoch seconds)
    #[serde(default)]
    pub reviewed_hooks: BTreeMap<String, u64>,
    /// issue ids the user dismissed; FIFO-capped, not pruned against a live
    /// scan (ponytail: a 512-entry cap beats plumbing scan state into every
    /// write; revisit if dismissals ever need exact lifecycle tracking).
    #[serde(default)]
    pub dismissed_issues: Vec<String>,
}

pub fn load_local_state(path: &Path) -> LocalState {
    let Ok(bytes) = fs::read(path) else {
        return LocalState::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

pub fn save_local_state(path: &Path, state: &LocalState) -> Result<(), String> {
    let parent = path.parent().ok_or("local-state path has no parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut trimmed = LocalState {
        schema: 1,
        reviewed_hooks: state.reviewed_hooks.clone(),
        dismissed_issues: state.dismissed_issues.clone(),
    };
    if trimmed.dismissed_issues.len() > MAX_DISMISSED {
        let drop = trimmed.dismissed_issues.len() - MAX_DISMISSED;
        trimmed.dismissed_issues.drain(..drop);
    }
    let json = serde_json::to_string_pretty(&trimmed).map_err(|e| e.to_string())?;
    // Keep the temporary file unpredictable and exclusively created. Writing
    // through its already-open handle cannot follow an attacker-planted
    // predictable temp symlink, and persist atomically replaces the target
    // path itself rather than following a target symlink.
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| format!("create local-state temp file: {}", e))?;
    tmp.as_file_mut()
        .write_all(format!("{}\n", json).as_bytes())
        .map_err(|e| format!("write local-state temp file: {}", e))?;
    tmp.persist(path)
        .map_err(|e| format!("publish local state: {}", e.error))?;
    Ok(())
}

// ── Issue model ──────────────────────────────────────────────────────────────

/// One source-machine project that needs an explicit local folder choice
/// (PLAN_CODEX_MANUAL_PROJECT_PATH_PICKING.md §4). `source_key` mirrors the
/// shared mapping schema (`project_paths.rs`); for Codex it equals
/// `source_path`.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectPathCandidate {
    pub provider: String, // "codex" | "claude"
    pub source_key: String,
    pub source_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_origin: Option<String>,
    /// Saved mapping target whose directory is gone — kept visible instead
    /// of erased or silently resolved (D5).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mapped_path: Option<String>,
    /// Affected conversation ids recorded under this exact cwd: Codex thread
    /// ids (for the `codex resume <id> -C <target>` commands) or Claude
    /// session ids.
    pub affected_threads: Vec<String>,
    /// Provider-neutral row state the UI switches on (`unmapped`,
    /// `missing_alias`, `missing_target`, `conflicting_*`, …); the frontend
    /// never re-inspects the filesystem itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_state: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetupIssue {
    pub id: String,
    pub root: String,
    /// Local profile id the issue belongs to; stamped by the caller (the
    /// scan itself is profile-agnostic).
    #[serde(default)]
    pub profile: String,
    pub category: String, // plugins | skills | mcp | hooks | agents | conflicts | paths | instructions | sidebar
    pub severity: String, // warning | info
    pub title: String,
    pub detail: String,
    pub source_path: Option<String>,
    pub action: String,
    /// Structured payload for `attach_project` issues that have a real
    /// folder picker; absent everywhere else.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_path: Option<ProjectPathCandidate>,
}

#[derive(Debug, Serialize)]
pub struct RootReadiness {
    pub root: String,
    /// Local profile id this summary row describes.
    #[serde(default)]
    pub profile: String,
    pub issues: usize,
}

#[derive(Debug, Serialize)]
pub struct SetupReadiness {
    pub generated_at: u64,
    pub roots: Vec<RootReadiness>,
    pub issues: Vec<SetupIssue>,
}

fn issue_id(root: &str, category: &str, title: &str, source_path: &Option<String>) -> String {
    let key = format!(
        "{}|{}|{}|{}",
        root,
        category,
        title,
        source_path.as_deref().unwrap_or("")
    );
    crate::sha256_hex(&key)[..12].to_string()
}

fn push_issue(
    issues: &mut Vec<SetupIssue>,
    root: &str,
    category: &str,
    severity: &str,
    title: String,
    detail: String,
    source_path: Option<String>,
    action: &str,
) {
    let id = issue_id(root, category, &title, &source_path);
    issues.push(SetupIssue {
        id,
        root: root.to_string(),
        profile: String::new(),
        category: category.to_string(),
        severity: severity.to_string(),
        title,
        detail,
        source_path,
        action: action.to_string(),
        project_path: None,
    });
}

// ── Scan input ───────────────────────────────────────────────────────────────

/// Everything the scan reads, injected so tests are hermetic. `resolve`
/// answers "does this binary exist" (production: login-shell lookup);
/// `env_present` answers "is this env var set here".
pub struct ScanInput<'a> {
    pub codex_dir: &'a Path,
    pub claude_dir: &'a Path,
    /// App-owned record dirs to scan for lock conflict siblings, as
    /// (root label, dir). Logical `.{root}/agent-sync/**` paths are
    /// remapped to per-profile dirs under `~/.agent-sync`, so conflicts
    /// live there instead of under the agent roots.
    pub lock_dirs: &'a [(&'a str, &'a Path)],
    pub codex_plan: Option<&'a CodexPluginPlan>,
    pub claude_plan: Option<&'a CodexPluginPlan>,
    pub state: &'a LocalState,
    pub resolve: &'a dyn Fn(&str) -> bool,
    pub env_present: &'a dyn Fn(&str) -> bool,
    /// Pre-computed sidebar apply summary (codex_sidebar::pending_summary);
    /// Some = the merged lock holds state not yet reflected locally.
    pub sidebar_pending: Option<&'a str>,
    /// Structured unmatched Codex sidebar projects, one folder-picker row
    /// each — computed by the caller from the sidebar plan + saved mappings
    /// (the scan itself stays dependency-free).
    pub codex_path_candidates: &'a [ProjectPathCandidate],
    /// Structured Claude project rows, computed by the caller with
    /// `claude_path_candidates` from the projects dir + saved mappings.
    pub claude_path_candidates: &'a [ProjectPathCandidate],
    /// Set when the machine-local mapping document failed to load: surfaced
    /// as one actionable row instead of silently treated as empty.
    pub mappings_error: Option<&'a str>,
}

/// Deterministic for a fixed filesystem + inputs: issues are sorted, ids are
/// content-derived, and nothing is written.
pub fn scan(input: &ScanInput) -> Vec<SetupIssue> {
    let mut issues = Vec::new();
    plugin_issues(
        &mut issues,
        ".codex",
        input.codex_plan,
        "repair_codex_plugins",
    );
    plugin_issues(
        &mut issues,
        ".claude",
        input.claude_plan,
        "repair_claude_plugins",
    );
    let managed_config = managed_config_issues(&mut issues, input);
    for (root, dir) in [(".codex", input.codex_dir), (".claude", input.claude_dir)] {
        skill_issues(&mut issues, root, dir);
        conflict_issues(&mut issues, root, dir);
    }
    for (root, dir) in input.lock_dirs {
        conflict_issues(&mut issues, root, dir);
    }
    agent_issues(&mut issues, input.codex_dir);
    mcp_issues(&mut issues, input, &managed_config);
    hook_issues(&mut issues, input);
    prompt_issues(&mut issues, ".codex", &input.codex_dir.join("prompts"));
    prompt_issues(&mut issues, ".claude", &input.claude_dir.join("commands"));
    let force_paths = (input.env_present)(FORCE_PATH_REMAP_ENV);
    claude_project_path_issues(&mut issues, input.claude_path_candidates, force_paths);
    codex_project_path_issues(&mut issues, input.codex_path_candidates, force_paths);
    if let Some(error) = input.mappings_error {
        push_issue(
            &mut issues,
            ".claude",
            "paths",
            "warning",
            "Project-path mappings file needs attention".to_string(),
            format!(
                "{} — new mappings and repairs are paused until the file is fixed; existing aliases keep working.",
                error
            ),
            None,
            "review_mappings_file",
        );
    }
    override_issue(&mut issues, input.codex_dir);
    sidebar_issue(&mut issues, input.sidebar_pending);
    // warnings first, then stable text order — deterministic for fixtures.
    issues.sort_by(|a, b| {
        (a.severity != "warning")
            .cmp(&(b.severity != "warning"))
            .then_with(|| a.root.cmp(&b.root))
            .then_with(|| a.category.cmp(&b.category))
            .then_with(|| a.title.cmp(&b.title))
    });
    issues
}

// ── Plugins: aggregate the existing plans, never re-inventory ───────────────

fn plugin_issues(
    issues: &mut Vec<SetupIssue>,
    root: &str,
    plan: Option<&CodexPluginPlan>,
    action: &str,
) {
    let Some(plan) = plan else { return };
    if let Some(reason) = &plan.blocked {
        push_issue(
            issues,
            root,
            "plugins",
            "warning",
            "Plugin lock blocked".to_string(),
            reason.clone(),
            None,
            "manual",
        );
    }
    let missing = plan.missing_marketplaces.len() + plan.missing_plugins.len();
    if missing > 0 {
        push_issue(
            issues,
            root,
            "plugins",
            "warning",
            format!("{} plugin item(s) missing", missing),
            format!(
                "marketplaces: {}; plugins: {}",
                plan.missing_marketplaces.join(", "),
                plan.missing_plugins.join(", ")
            ),
            None,
            action,
        );
    }
    for item in &plan.missing_managed_marketplaces {
        push_issue(
            issues,
            root,
            "plugins",
            "warning",
            format!(
                "Managed marketplace '{}' is unavailable [{}]",
                item.id, item.code
            ),
            item.message.clone(),
            None,
            action,
        );
    }
    for item in &plan.blocked_plugins {
        push_issue(
            issues,
            root,
            "plugins",
            "warning",
            format!("Plugin '{}' is blocked [{}]", item.id, item.code),
            item.message.clone(),
            None,
            action,
        );
    }
    for item in &plan.config_repairs {
        push_issue(
            issues,
            root,
            "plugins",
            "warning",
            format!("Codex config '{}' needs repair [{}]", item.id, item.code),
            item.message.clone(),
            Some("config.toml".to_string()),
            action,
        );
    }
    if !plan.manual.is_empty() {
        push_issue(
            issues,
            root,
            "plugins",
            "info",
            format!("{} plugin item(s) need manual follow-up", plan.manual.len()),
            plan.manual.join(", "),
            None,
            "manual",
        );
    }
}

/// Convert the config codec's machine-local diagnostics into the same Codex
/// Repair group as plugin/catalog findings. The codec owns the fingerprints;
/// readiness only presents its structured, non-secret output.
#[derive(Default)]
struct ManagedConfigReadiness {
    mcp_repairs: BTreeSet<String>,
    config_unusable: bool,
}

fn managed_config_issues(
    issues: &mut Vec<SetupIssue>,
    input: &ScanInput,
) -> ManagedConfigReadiness {
    let config_path = input.codex_dir.join("config.toml");
    let planned: BTreeSet<(&str, &str)> = input
        .codex_plan
        .into_iter()
        .flat_map(|plan| &plan.config_repairs)
        .map(|item| (item.id.as_str(), item.code.as_str()))
        .collect();
    let mut readiness = ManagedConfigReadiness::default();
    for item in crate::codex_config::inspect_managed_config(&config_path, input.codex_dir) {
        if matches!(item.code.as_str(), "config_unreadable" | "config_invalid") {
            readiness.config_unusable = true;
        }
        if let Some(name) = item
            .id
            .strip_prefix("mcp_servers.")
            .and_then(|tail| tail.split('.').next())
        {
            readiness.mcp_repairs.insert(name.to_string());
        }
        if planned.contains(&(item.id.as_str(), item.code.as_str())) {
            continue;
        }
        push_issue(
            issues,
            ".codex",
            "plugins",
            "warning",
            format!("Codex config '{}' needs repair [{}]", item.id, item.code),
            item.message,
            Some(config_path.to_string_lossy().to_string()),
            "repair_codex_plugins",
        );
    }
    readiness
}

// ── Skills: local diagnostics only (D7 — symlink intent does not travel) ────

fn skill_issues(issues: &mut Vec<SetupIssue>, root: &str, dir: &Path) {
    let skills = dir.join("skills");
    let Ok(entries) = fs::read_dir(&skills) else {
        return;
    };
    let mut names: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    names.sort_by_key(|e| e.file_name());
    for entry in names {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            // fs::metadata follows the link; failure = broken target.
            if fs::metadata(&path).is_err() {
                push_issue(
                    issues,
                    root,
                    "skills",
                    "warning",
                    format!("Skill '{}' points at a missing target", name),
                    "The symlink target does not exist on this machine. If the skill came from a plugin, run plugin repair; otherwise restore the source or remove the link.".to_string(),
                    Some(path.to_string_lossy().to_string()),
                    "manual",
                );
            }
        } else if meta.is_dir() && !path.join("SKILL.md").is_file() {
            push_issue(
                issues,
                root,
                "skills",
                "info",
                format!("Skill '{}' has no SKILL.md", name),
                "The agent will not discover this skill without a SKILL.md.".to_string(),
                Some(path.to_string_lossy().to_string()),
                "manual",
            );
        }
    }
}

// ── Custom agents: `.codex/agents/*.toml` required fields ───────────────────

fn agent_issues(issues: &mut Vec<SetupIssue>, codex_dir: &Path) {
    let dir = codex_dir.join("agents");
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    let mut files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort_by_key(|e| e.file_name());
    for entry in files {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(text) = bounded_read(&path, MAX_CONFIG_BYTES) else {
            continue;
        };
        match text.parse::<toml::Table>() {
            Err(e) => push_issue(
                issues,
                ".codex",
                "agents",
                "warning",
                format!("Custom agent '{}' does not parse", name),
                e.to_string(),
                Some(path.to_string_lossy().to_string()),
                "manual",
            ),
            Ok(value) => {
                let missing: Vec<&str> = ["name", "description", "developer_instructions"]
                    .into_iter()
                    .filter(|f| value.get(*f).and_then(|v| v.as_str()).is_none())
                    .collect();
                if !missing.is_empty() {
                    push_issue(
                        issues,
                        ".codex",
                        "agents",
                        "warning",
                        format!("Custom agent '{}' is missing required fields", name),
                        format!("missing: {}", missing.join(", ")),
                        Some(path.to_string_lossy().to_string()),
                        "manual",
                    );
                }
            }
        }
    }
}

// ── MCP: non-secret checks over the local raw configs ───────────────────────

fn mcp_issues(
    issues: &mut Vec<SetupIssue>,
    input: &ScanInput,
    managed_config: &ManagedConfigReadiness,
) {
    // Codex: [mcp_servers.<name>] tables in config.toml.
    if !managed_config.config_unusable {
        if let Some(text) = bounded_read(&input.codex_dir.join("config.toml"), MAX_CONFIG_BYTES) {
            match text.parse::<toml::Table>() {
                Err(e) => push_issue(
                    issues,
                    ".codex",
                    "mcp",
                    "info",
                    "config.toml does not parse".to_string(),
                    e.to_string(),
                    Some(
                        input
                            .codex_dir
                            .join("config.toml")
                            .to_string_lossy()
                            .to_string(),
                    ),
                    "manual",
                ),
                Ok(value) => {
                    if let Some(servers) = value.get("mcp_servers").and_then(|v| v.as_table()) {
                        for (name, server) in servers {
                            // A recognized managed block with a stale target path is
                            // repaired by the Codex action above. Avoid presenting a
                            // second, generic MCP setup action for the same problem.
                            if managed_config.mcp_repairs.contains(name) {
                                continue;
                            }
                            check_mcp_server(
                                issues,
                                ".codex",
                                name,
                                &toml_to_json(server),
                                input,
                                "config.toml",
                            );
                        }
                    }
                }
            }
        }
    }
    // Claude: `mcpServers` in settings.json when present. The default
    // ~/.claude.json stays outside the sync root and is not inspected.
    if let Some(text) = bounded_read(&input.claude_dir.join("settings.json"), MAX_CONFIG_BYTES) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(servers) = value.get("mcpServers").and_then(|v| v.as_object()) {
                for (name, server) in servers {
                    check_mcp_server(issues, ".claude", name, server, input, "settings.json");
                }
            }
        }
    }
}

fn check_mcp_server(
    issues: &mut Vec<SetupIssue>,
    root: &str,
    name: &str,
    server: &serde_json::Value,
    input: &ScanInput,
    source: &str,
) {
    if let Some(command) = server.get("command").and_then(|v| v.as_str()) {
        let binary = command.rsplit('/').next().unwrap_or(command);
        let found = if command.contains('/') {
            Path::new(command).exists()
        } else {
            (input.resolve)(binary)
        };
        if !found {
            push_issue(
                issues,
                root,
                "mcp",
                "warning",
                format!("MCP server '{}' command not found", name),
                format!("'{}' does not resolve on this machine.", command),
                Some(source.to_string()),
                "open_mcp_setup",
            );
        }
    }
    if let Some(url) = server.get("url").and_then(|v| v.as_str()) {
        if !(url.starts_with("http://") || url.starts_with("https://")) || url.contains(' ') {
            push_issue(
                issues,
                root,
                "mcp",
                "warning",
                format!("MCP server '{}' has an invalid URL", name),
                format!("'{}' is not a valid http(s) URL.", url),
                Some(source.to_string()),
                "open_mcp_setup",
            );
        }
    }
    // Env references: names only, values never read or logged.
    let mut missing_env: BTreeSet<String> = BTreeSet::new();
    collect_env_refs(server, &mut missing_env);
    missing_env.retain(|n| !(input.env_present)(n));
    if !missing_env.is_empty() {
        push_issue(
            issues,
            root,
            "mcp",
            "warning",
            format!("MCP server '{}' needs environment variables", name),
            format!(
                "not set on this machine: {}",
                missing_env.into_iter().collect::<Vec<_>>().join(", ")
            ),
            Some(source.to_string()),
            "open_mcp_setup",
        );
    }
}

/// `${NAME}` references anywhere in the server definition's string values.
fn collect_env_refs(value: &serde_json::Value, out: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::String(s) => {
            let mut rest = s.as_str();
            while let Some(start) = rest.find("${") {
                let Some(end) = rest[start + 2..].find('}') else {
                    break;
                };
                let name = &rest[start + 2..start + 2 + end];
                if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    out.insert(name.to_string());
                }
                rest = &rest[start + 2 + end + 1..];
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| collect_env_refs(v, out)),
        serde_json::Value::Object(map) => map.values().for_each(|v| collect_env_refs(v, out)),
        _ => {}
    }
}

// ── Hooks: normalized hash vs the locally reviewed set (D9) ─────────────────

/// Every hook definition currently on disk, as (root, source file, hash).
pub fn hook_definitions(codex_dir: &Path, claude_dir: &Path) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    if let Some(text) = bounded_read(&codex_dir.join("hooks.json"), MAX_CONFIG_BYTES) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            for def in top_level_definitions(&value) {
                out.push((
                    ".codex".to_string(),
                    "hooks.json".to_string(),
                    hash_definition(&def),
                ));
            }
        }
    }
    if let Some(text) = bounded_read(&claude_dir.join("settings.json"), MAX_CONFIG_BYTES) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(hooks) = value.get("hooks") {
                for def in top_level_definitions(hooks) {
                    out.push((
                        ".claude".to_string(),
                        "settings.json".to_string(),
                        hash_definition(&def),
                    ));
                }
            }
        }
    }
    out
}

/// One definition per top-level array element, or per key of a top-level
/// object (the key rides inside the hashed value so renames re-review).
fn top_level_definitions(value: &serde_json::Value) -> Vec<serde_json::Value> {
    match value {
        serde_json::Value::Array(items) => items.clone(),
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(k, v)| serde_json::json!({ "event": k, "definition": v }))
            .collect(),
        _ => Vec::new(),
    }
}

/// Stable across JSON object ordering: canonicalize (sorted keys), then hash.
fn hash_definition(value: &serde_json::Value) -> String {
    crate::sha256_hex(&canonical_json(value))
}

fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let fields: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap_or_default(),
                        canonical_json(&map[k])
                    )
                })
                .collect();
            format!("{{{}}}", fields.join(","))
        }
        serde_json::Value::Array(items) => {
            let fields: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", fields.join(","))
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Resolve a hooks-category issue id back to the full definition hash it
/// was derived from, so the mark-reviewed command can take the issue id.
pub fn hook_hash_for_issue(codex_dir: &Path, claude_dir: &Path, id: &str) -> Option<String> {
    hook_definitions(codex_dir, claude_dir)
        .into_iter()
        .find_map(|(root, source, hash)| {
            let title = format!("Unreviewed hook {}", &hash[..12]);
            (issue_id(&root, "hooks", &title, &Some(source)) == id).then_some(hash)
        })
}

fn hook_issues(issues: &mut Vec<SetupIssue>, input: &ScanInput) {
    for (root, source, hash) in hook_definitions(input.codex_dir, input.claude_dir) {
        if input.state.reviewed_hooks.contains_key(&hash) {
            continue;
        }
        push_issue(
            issues,
            &root,
            "hooks",
            "warning",
            format!("Unreviewed hook {}", &hash[..12]),
            format!(
                "A hook definition in {} has not been reviewed on this machine. Open the agent's native hook review (/hooks), then mark it reviewed here. Trust never syncs.",
                source
            ),
            Some(source),
            "review_hooks",
        );
    }
}

// ── Conflicts: the engine's conflict copies ARE the variants ────────────────

fn conflict_issues(issues: &mut Vec<SetupIssue>, root: &str, dir: &Path) {
    // Top-level files plus the behavior dirs; sessions/transcript trees are
    // deliberately out of scope here (their conflicts are per-session data,
    // not setup). Depth-bounded, symlinks not followed.
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            if entry.file_type().is_ok_and(|t| t.is_file()) {
                candidates.push(entry.path());
            }
        }
    }
    for sub in [
        "agents", "commands", "skills", "prompts", "rules", "memories",
    ] {
        for entry in walkdir::WalkDir::new(dir.join(sub))
            .follow_links(false)
            .max_depth(4)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                candidates.push(entry.path().to_path_buf());
            }
        }
    }
    candidates.sort();
    for path in candidates {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !name.contains(".sync-conflict-") {
            continue;
        }
        let original = name
            .split(".sync-conflict-")
            .next()
            .unwrap_or_default()
            .to_string();
        push_issue(
            issues,
            root,
            "conflicts",
            "warning",
            format!("Conflict copy for '{}'", original),
            format!(
                "Both machines changed this file; the other version was kept losslessly as '{}'. Compare the two, fold what you want into the main file, then use Resolve to remove this copy locally and from the linked cloud profile.",
                name
            ),
            Some(path.to_string_lossy().to_string()),
            "resolve_conflict_copy",
        );
    }
}

// ── Prompts/commands: portability diagnostics, never rewritten ──────────────

fn prompt_issues(issues: &mut Vec<SetupIssue>, root: &str, dir: &Path) {
    let mut files: Vec<std::path::PathBuf> = walkdir::WalkDir::new(dir)
        .follow_links(false)
        .max_depth(3)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .collect();
    files.sort();
    for path in files {
        let Some(text) = bounded_read(&path, MAX_PROMPT_BYTES) else {
            continue;
        };
        if let Some(foreign) = foreign_home_path(&text) {
            push_issue(
                issues,
                root,
                "paths",
                "warning",
                format!(
                    "'{}' references another machine's home",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ),
                format!("contains '{}', which does not exist here.", foreign),
                Some(path.to_string_lossy().to_string()),
                "manual",
            );
        }
    }
}

/// First `/Users/<x>/…` or `/home/<x>/…` token whose prefix directory does
/// not exist on this machine. Shell interpolation and `$ARGUMENTS` are
/// deliberately NOT flagged — they are normal prompt syntax, and flagging
/// them would bury real issues in noise.
fn foreign_home_path(text: &str) -> Option<String> {
    for prefix in ["/Users/", "/home/"] {
        let mut rest = text;
        while let Some(pos) = rest.find(prefix) {
            let tail = &rest[pos..];
            let token: String = tail
                .chars()
                .take_while(|c| !c.is_whitespace() && !"\"'`)]}>,;".contains(*c))
                .collect();
            let mut parts = token.splitn(4, '/');
            let user_home: Vec<&str> = (&mut parts).take(3).filter(|s| !s.is_empty()).collect();
            if user_home.len() == 2 {
                let home = format!("/{}/{}", user_home[0], user_home[1]);
                if !Path::new(&home).exists() {
                    return Some(token);
                }
            }
            rest = &rest[pos + prefix.len()..];
        }
    }
    None
}

// ── Claude project paths (PLAN_CLAUDE_PROJECT_PATH_REMAP.md §7) ──────────────

/// One real project bucket under `<claude-root>/projects/`, read-only.
#[derive(Debug, Clone)]
pub struct ClaudeProjectProbe {
    /// Bucket basename — the stable identity; never decoded (lossy).
    pub source_key: String,
    /// Deduped, sorted first-`cwd` of each transcript's bounded head. A
    /// bucket legitimately mixes old A-path and new B-path records after a
    /// mapping, so all values are kept.
    pub cwd_candidates: Vec<String>,
    /// Session ids from direct `*.jsonl` basenames, sorted.
    pub session_ids: Vec<String>,
}

/// Scan the real direct directories below `projects/` — symlinks (mapping
/// aliases among them) are skipped, so an alias never probes as a second
/// project.
pub fn probe_claude_projects(claude_dir: &Path) -> Vec<ClaudeProjectProbe> {
    let projects = claude_dir.join("projects");
    let Ok(entries) = fs::read_dir(&projects) else {
        return Vec::new();
    };
    // read_dir's file_type is the dirent's own type: a symlinked dir is not
    // a dir here, which is exactly the no-follow rule this design needs.
    let mut dirs: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .collect();
    dirs.sort_by_key(|e| e.file_name());
    dirs.iter()
        .filter_map(|entry| probe_claude_project(&entry.path()))
        .collect()
}

fn probe_claude_project(project_dir: &Path) -> Option<ClaudeProjectProbe> {
    let source_key = project_dir.file_name()?.to_str()?.to_string();
    let entries = fs::read_dir(project_dir).ok()?;
    let mut jsonl: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .collect();
    jsonl.sort_by_key(|e| e.file_name());
    let mut cwds = BTreeSet::new();
    let mut session_ids = Vec::new();
    for entry in &jsonl {
        if let Some(stem) = entry.path().file_stem().and_then(|s| s.to_str()) {
            session_ids.push(stem.to_string());
        }
        if let Some(cwd) = transcript_cwd(&entry.path()) {
            cwds.insert(cwd);
        }
    }
    (!session_ids.is_empty()).then(|| ClaudeProjectProbe {
        source_key,
        cwd_candidates: cwds.into_iter().collect(),
        session_ids,
    })
}

/// `cwd` from the earliest record carrying one. Real transcripts open with
/// metadata records (`mode`, `last-prompt`, `queue-operation`, …) that have
/// no cwd — it first appears a few lines in, so scan the capped head, not
/// just line one.
fn transcript_cwd(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    let _ = file.take(MAX_FIRST_LINE_BYTES as u64).read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    for line in text.lines() {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(cwd) = value.get("cwd").and_then(|v| v.as_str()) {
                return Some(cwd.to_string());
            }
        }
    }
    None
}

/// One structured candidate per real Claude project bucket, mapping-aware:
/// the row's `path_state` is what the UI switches on. Read-only — mappings
/// are consulted, never created or edited.
pub fn claude_path_candidates(
    claude_dir: &Path,
    mappings: &project_paths::ProjectPathMappings,
    profile_id: &str,
) -> Vec<ProjectPathCandidate> {
    let projects = claude_dir.join("projects");
    probe_claude_projects(claude_dir)
        .into_iter()
        .filter_map(|probe| {
            // Display cwd: prefer one whose tested encoding equals the key;
            // never decode the key itself. No cwd at all → nothing to show
            // or map.
            let display = probe
                .cwd_candidates
                .iter()
                .find(|cwd| project_paths::encode_claude_project_path(cwd) == probe.source_key)
                .or_else(|| probe.cwd_candidates.first())?
                .clone();
            let mapping =
                project_paths::mapping_for(mappings, profile_id, "claude", &probe.source_key);
            let (state, mapped_path) = match mapping {
                Some(mapping) => (
                    project_paths::claude_alias_state(&projects, mapping)
                        .as_str()
                        .to_string(),
                    Some(mapping.target_path.clone()),
                ),
                None => {
                    // Locally reachable only when an existing cwd encodes to
                    // the real key — an unrelated same-named folder is not
                    // enough (§7.5).
                    let reachable = probe.cwd_candidates.iter().any(|cwd| {
                        project_paths::encode_claude_project_path(cwd) == probe.source_key
                            && Path::new(cwd).is_dir()
                    });
                    (
                        (if reachable { "reachable" } else { "unmapped" }).to_string(),
                        None,
                    )
                }
            };
            Some(ProjectPathCandidate {
                provider: "claude".to_string(),
                source_key: probe.source_key,
                source_path: display,
                git_origin: None,
                mapped_path,
                affected_threads: probe.session_ids,
                path_state: Some(state),
            })
        })
        .collect()
}

fn claude_project_path_issues(
    issues: &mut Vec<SetupIssue>,
    candidates: &[ProjectPathCandidate],
    force: bool,
) {
    for candidate in candidates {
        let state = candidate.path_state.as_deref().unwrap_or("unmapped");
        let sessions = candidate.affected_threads.len();
        let target = candidate.mapped_path.as_deref().unwrap_or("?");
        let missing_title = format!("Project folder missing for '{}'", candidate.source_path);
        let (title, detail) = match state {
            // A valid mapping stays resolved even under the simulation
            // switch — force must never invite replacing it (§7.7).
            "ready" | "ready_without_alias" | "missing_source" => continue,
            "reachable" => {
                if !force {
                    continue;
                }
                (
                    missing_title,
                    format!(
                        "Transcripts reference '{}'. The folder exists here, but {} is set, so it is treated as foreign for simulation.",
                        candidate.source_path, FORCE_PATH_REMAP_ENV
                    ),
                )
            }
            "unmapped" => (
                missing_title,
                format!(
                    "Transcripts reference '{}', which does not exist on this machine. Choose the local folder that holds this project — {} session(s) can then resume from it.",
                    candidate.source_path, sessions
                ),
            ),
            "missing_target" => (
                missing_title,
                format!(
                    "Mapped to '{}', which no longer exists on this machine. Choose the folder again; the saved mapping stays until you change or remove it.",
                    target
                ),
            ),
            "missing_alias" => (
                format!("Mapping needs repair for '{}'", candidate.source_path),
                format!(
                    "The mapping to '{}' is saved, but its local alias link is missing. Repair recreates the link; transcripts are never touched.",
                    target
                ),
            ),
            "conflicting_directory" => (
                format!("Mapping conflict for '{}'", candidate.source_path),
                format!(
                    "The alias for '{}' is blocked: a real project directory already uses that name, and Claude histories are never auto-merged. Choose a different folder, or move that directory aside and repair.",
                    target
                ),
            ),
            "conflicting_symlink" => (
                format!("Mapping conflict for '{}'", candidate.source_path),
                format!(
                    "The alias for '{}' is blocked by a link pointing somewhere else. Remove the stale link, then repair the mapping.",
                    target
                ),
            ),
            "permission_denied" => (
                format!("Access needed for '{}'", candidate.source_path),
                "macOS denied access to this profile's projects folder. Grant Mallard access to it (or Full Disk Access when required), then repair the mapping.".to_string(),
            ),
            _ => continue,
        };
        push_issue(
            issues,
            ".claude",
            "paths",
            "warning",
            title,
            detail,
            Some(candidate.source_key.clone()),
            "attach_project",
        );
        if let Some(issue) = issues.last_mut() {
            issue.project_path = Some(candidate.clone());
        }
    }
}

// ── Codex project paths: one folder-picker row per unmatched project ────────

fn codex_project_path_issues(
    issues: &mut Vec<SetupIssue>,
    candidates: &[ProjectPathCandidate],
    force: bool,
) {
    for candidate in candidates {
        let tasks = candidate.affected_threads.len();
        let mut detail = match &candidate.mapped_path {
            Some(target) => format!(
                "Mapped to '{}', which no longer exists on this machine. Choose the folder again; the saved mapping stays until you change or remove it.",
                target
            ),
            None => format!(
                "The synced sidebar references '{}', which does not exist here. Choose the local folder that holds this project — {} task(s) recorded under it can then continue via `codex resume -C`.",
                candidate.source_path, tasks
            ),
        };
        if force && Path::new(&candidate.source_path).exists() {
            detail = format!(
                "{} (The folder exists here, but {} is set, so it is treated as foreign for simulation.)",
                detail, FORCE_PATH_REMAP_ENV
            );
        }
        push_issue(
            issues,
            ".codex",
            "paths",
            "warning",
            format!("Project folder missing for '{}'", candidate.source_path),
            detail,
            Some(candidate.source_path.clone()),
            "attach_project",
        );
        if let Some(issue) = issues.last_mut() {
            issue.project_path = Some(candidate.clone());
        }
    }
}

/// Thread ids grouped by the exact `session_meta.cwd` of each rollout's
/// first record (bounded read, same cap as the Claude transcript probe).
/// Read-only and deterministic: BTree ordering throughout.
pub fn codex_threads_by_cwd(codex_dir: &Path) -> BTreeMap<String, Vec<String>> {
    let mut grouped: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for dir in ["sessions", "archived_sessions"] {
        for entry in walkdir::WalkDir::new(codex_dir.join(dir))
            .follow_links(false)
            .max_depth(5)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy();
            if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
                continue;
            }
            if let Some((id, cwd)) = rollout_first_record(entry.path()) {
                grouped.entry(cwd).or_default().insert(id);
            }
        }
    }
    grouped
        .into_iter()
        .map(|(cwd, ids)| (cwd, ids.into_iter().collect()))
        .collect()
}

/// (thread id, cwd) from a rollout's first record. Codex writes a
/// `session_meta` record with an id + cwd payload first; files without one
/// are skipped rather than guessed at from the filename.
fn rollout_first_record(path: &Path) -> Option<(String, String)> {
    let file = fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    let _ = file.take(MAX_FIRST_LINE_BYTES as u64).read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    let line = text.lines().next()?;
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let payload = value.get("payload").unwrap_or(&value);
    let cwd = payload.get("cwd").and_then(|v| v.as_str())?;
    let id = payload.get("id").and_then(|v| v.as_str())?;
    (!id.is_empty() && !cwd.is_empty()).then(|| (id.to_string(), cwd.to_string()))
}

// ── Active override (D6) ─────────────────────────────────────────────────────

fn override_issue(issues: &mut Vec<SetupIssue>, codex_dir: &Path) {
    let path = codex_dir.join("AGENTS.override.md");
    if path.is_file() {
        push_issue(
            issues,
            ".codex",
            "instructions",
            "info",
            "Active AGENTS.override.md".to_string(),
            "A temporary instruction override is active. It does not sync by default; enabling it is a per-remote opt-in, and removing it on one machine will not remove cloud copies (deletions never propagate).".to_string(),
            Some(path.to_string_lossy().to_string()),
            "manual",
        );
    }
}

// ── Sidebar: another machine's desktop state awaits an explicit apply ────────

fn sidebar_issue(issues: &mut Vec<SetupIssue>, pending: Option<&str>) {
    if let Some(detail) = pending {
        push_issue(
            issues,
            ".codex",
            "sidebar",
            "info",
            "Sidebar state from another machine".to_string(),
            format!(
                "{}. Apply merges additively — nothing local is removed or renamed.",
                detail
            ),
            None,
            "apply_sidebar_state",
        );
    }
}

// ── Shared helpers ───────────────────────────────────────────────────────────

fn bounded_read(path: &Path, max: u64) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max {
        return None;
    }
    fs::read_to_string(path).ok()
}

fn toml_to_json(value: &toml::Value) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_fixture(codex: &Path, claude: &Path, state: &LocalState) -> Vec<SetupIssue> {
        let resolve = |name: &str| name == "present-binary";
        let env_present = |name: &str| name == "PRESENT_ENV";
        // Same composition as get_setup_readiness: candidates prefiltered
        // from the projects dir + saved mappings (none in fixtures).
        let claude_candidates = claude_path_candidates(
            claude,
            &project_paths::ProjectPathMappings::default(),
            "claude",
        );
        scan(&ScanInput {
            codex_dir: codex,
            claude_dir: claude,
            lock_dirs: &[],
            codex_plan: None,
            claude_plan: None,
            state,
            resolve: &resolve,
            env_present: &env_present,
            sidebar_pending: None,
            codex_path_candidates: &[],
            claude_path_candidates: &claude_candidates,
            mappings_error: None,
        })
    }

    #[test]
    fn scan_is_deterministic_and_covers_categories() {
        let dir = tempfile::tempdir().unwrap();
        let codex = dir.path().join(".codex");
        let claude = dir.path().join(".claude");
        fs::create_dir_all(codex.join("agents")).unwrap();
        fs::create_dir_all(codex.join("skills/broken")).unwrap();
        fs::create_dir_all(claude.join("commands")).unwrap();

        // agents: one bad TOML, one missing fields
        fs::write(codex.join("agents/bad.toml"), "not = [toml").unwrap();
        fs::write(codex.join("agents/thin.toml"), "name = \"x\"").unwrap();
        // mcp: missing command + missing env; present binary passes
        fs::write(
            codex.join("config.toml"),
            "[mcp_servers.gone]\ncommand = \"missing-binary\"\n[mcp_servers.ok]\ncommand = \"present-binary\"\nenv = { KEY = \"${MISSING_ENV}\" }\n",
        )
        .unwrap();
        // hooks: one unreviewed definition
        fs::write(codex.join("hooks.json"), "[{\"cmd\":\"echo hi\"}]").unwrap();
        // conflicts: sibling at top level
        fs::write(codex.join("config.sync-conflict-abcd1234.toml"), "x").unwrap();
        // prompts: foreign home reference
        fs::create_dir_all(codex.join("prompts")).unwrap();
        fs::write(
            codex.join("prompts/p.md"),
            "read /Users/nobody-here/notes.md",
        )
        .unwrap();
        // override present
        fs::write(codex.join("AGENTS.override.md"), "override").unwrap();
        // claude project with missing cwd
        let proj = claude.join("projects/-tmp-gone");
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join("s1.jsonl"),
            "{\"cwd\":\"/tmp/definitely-gone-dir\"}\n",
        )
        .unwrap();

        let state = LocalState::default();
        let a = scan_fixture(&codex, &claude, &state);
        let b = scan_fixture(&codex, &claude, &state);
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap(),
            "scan must be deterministic"
        );
        let categories: BTreeSet<&str> = a.iter().map(|i| i.category.as_str()).collect();
        for expected in [
            "agents",
            "mcp",
            "hooks",
            "conflicts",
            "paths",
            "instructions",
        ] {
            assert!(
                categories.contains(expected),
                "missing category {}: {:?}",
                expected,
                a
            );
        }
        // The resolvable server produced no command issue.
        assert!(!a.iter().any(|i| i.title.contains("'ok' command")));
        // The env issue names the variable but never its value.
        let env_issue = a
            .iter()
            .find(|i| i.title.contains("environment"))
            .unwrap_or_else(|| panic!("no env issue among {:#?}", a));
        assert!(env_issue.detail.contains("MISSING_ENV"));
    }

    #[test]
    fn transcript_cwd_found_past_leading_metadata_records() {
        // Real transcripts open with metadata records (mode, last-prompt, …)
        // that carry no cwd; the probe must scan past them.
        let dir = tempfile::tempdir().unwrap();
        let codex = dir.path().join(".codex");
        let claude = dir.path().join(".claude");
        fs::create_dir_all(&codex).unwrap();
        let proj = claude.join("projects/-tmp-meta-first");
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join("s1.jsonl"),
            "{\"type\":\"mode\",\"mode\":\"default\"}\n{\"type\":\"queue-operation\"}\n{\"type\":\"user\",\"cwd\":\"/tmp/definitely-gone-dir\"}\n",
        )
        .unwrap();

        let issues = scan_fixture(&codex, &claude, &LocalState::default());
        assert!(
            issues
                .iter()
                .any(|i| i.action == "attach_project"
                    && i.detail.contains("/tmp/definitely-gone-dir")),
            "missing-path issue not raised from a metadata-first transcript: {:?}",
            issues
        );
    }

    #[test]
    fn force_path_remap_raises_issues_for_existing_paths() {
        let dir = tempfile::tempdir().unwrap();
        let codex = dir.path().join(".codex");
        let claude = dir.path().join(".claude");
        fs::create_dir_all(&codex).unwrap();
        let exists = dir.path().join("here");
        fs::create_dir_all(&exists).unwrap();
        let proj = claude
            .join("projects")
            .join(project_paths::encode_claude_project_path(
                &exists.to_string_lossy(),
            ));
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join("s1.jsonl"),
            format!("{{\"cwd\":\"{}\"}}\n", exists.display()),
        )
        .unwrap();

        let quiet = scan_fixture(&codex, &claude, &LocalState::default());
        assert!(
            !quiet.iter().any(|i| i.action == "attach_project"),
            "existing path must stay quiet without the switch: {:?}",
            quiet
        );

        let resolve = |_: &str| false;
        let env_present = |name: &str| name == FORCE_PATH_REMAP_ENV;
        let claude_candidates = claude_path_candidates(
            &claude,
            &project_paths::ProjectPathMappings::default(),
            "claude",
        );
        assert_eq!(
            claude_candidates[0].path_state.as_deref(),
            Some("reachable"),
            "existing cwd encoding to the key is reachable"
        );
        let forced = scan(&ScanInput {
            codex_dir: &codex,
            claude_dir: &claude,
            lock_dirs: &[],
            codex_plan: None,
            claude_plan: None,
            state: &LocalState::default(),
            resolve: &resolve,
            env_present: &env_present,
            sidebar_pending: None,
            codex_path_candidates: &[],
            claude_path_candidates: &claude_candidates,
            mappings_error: None,
        });
        let issue = forced
            .iter()
            .find(|i| i.action == "attach_project")
            .unwrap_or_else(|| panic!("forced scan raised no path issue: {:?}", forced));
        assert!(
            issue.detail.contains(FORCE_PATH_REMAP_ENV),
            "forced issue must say it is simulated: {}",
            issue.detail
        );
        assert!(
            issue.project_path.is_some(),
            "forced Claude row carries the folder-picker payload"
        );
    }

    #[test]
    fn claude_candidates_follow_mapping_state() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join(".claude");
        let projects = claude.join("projects");
        let target = dir.path().join("local-repo");
        fs::create_dir_all(&target).unwrap();
        let bucket = projects.join("-a-gone-repo");
        fs::create_dir_all(&bucket).unwrap();
        fs::write(
            bucket.join("6d1a1c1e-0000-4000-8000-000000000001.jsonl"),
            "{\"type\":\"mode\"}\n{\"cwd\":\"/a/gone/repo\"}\n",
        )
        .unwrap();
        fs::write(
            bucket.join("6d1a1c1e-0000-4000-8000-000000000002.jsonl"),
            "{\"cwd\":\"/a/gone/repo\"}\n",
        )
        .unwrap();

        let mut mappings = project_paths::ProjectPathMappings::default();
        mappings.schema = 1;

        // Unmapped + missing cwd → one picker row with both session ids.
        let unmapped = claude_path_candidates(&claude, &mappings, "claude");
        assert_eq!(unmapped.len(), 1);
        assert_eq!(unmapped[0].source_key, "-a-gone-repo");
        assert_eq!(unmapped[0].source_path, "/a/gone/repo");
        assert_eq!(unmapped[0].path_state.as_deref(), Some("unmapped"));
        assert_eq!(unmapped[0].affected_threads.len(), 2);

        // Saved mapping without its alias → missing_alias (Repair row).
        project_paths::upsert(
            &mut mappings,
            project_paths::ProjectPathMapping {
                profile: "claude".to_string(),
                provider: "claude".to_string(),
                source_key: "-a-gone-repo".to_string(),
                source_path: "/a/gone/repo".to_string(),
                target_path: target.to_string_lossy().to_string(),
            },
        )
        .unwrap();
        let saved = claude_path_candidates(&claude, &mappings, "claude");
        assert_eq!(saved[0].path_state.as_deref(), Some("missing_alias"));
        let mut issues = Vec::new();
        claude_project_path_issues(&mut issues, &saved, false);
        assert!(issues[0].title.contains("needs repair"), "{:?}", issues);

        // Alias in place → ready → suppressed, force or not. An alias is
        // also never probed as a second project.
        std::os::unix::fs::symlink(
            "-a-gone-repo",
            projects.join(project_paths::encode_claude_project_path(
                &target.to_string_lossy(),
            )),
        )
        .unwrap();
        let ready = claude_path_candidates(&claude, &mappings, "claude");
        assert_eq!(ready.len(), 1, "alias must not probe as a project");
        assert_eq!(ready[0].path_state.as_deref(), Some("ready"));
        let mut quiet = Vec::new();
        claude_project_path_issues(&mut quiet, &ready, true);
        assert!(quiet.is_empty(), "{:?}", quiet);

        // Target gone → stale row that keeps the saved mapping visible.
        fs::remove_dir(&target).unwrap();
        let stale = claude_path_candidates(&claude, &mappings, "claude");
        assert_eq!(stale[0].path_state.as_deref(), Some("missing_target"));
        let mut rows = Vec::new();
        claude_project_path_issues(&mut rows, &stale, false);
        assert!(
            rows[0].detail.contains("Choose the folder again"),
            "{:?}",
            rows
        );
    }

    #[test]
    fn scan_surfaces_conflicts_from_remapped_plugin_locks() {
        let dir = tempfile::tempdir().unwrap();
        let codex = dir.path().join(".codex");
        let claude = dir.path().join(".claude");
        let agent_sync = dir.path().join(".agent-sync");
        fs::create_dir_all(&codex).unwrap();
        fs::create_dir_all(&claude).unwrap();
        fs::create_dir_all(agent_sync.join("codex")).unwrap();
        fs::write(
            agent_sync
                .join("codex")
                .join("codex-plugins.lock.sync-conflict-abcd1234.json"),
            "{}\n",
        )
        .unwrap();

        let resolve = |_: &str| true;
        let env_present = |_: &str| true;
        let codex_locks = agent_sync.join("codex");
        let issues = scan(&ScanInput {
            codex_dir: &codex,
            claude_dir: &claude,
            lock_dirs: &[(".codex", &codex_locks)],
            codex_plan: None,
            claude_plan: None,
            state: &LocalState::default(),
            resolve: &resolve,
            env_present: &env_present,
            sidebar_pending: None,
            codex_path_candidates: &[],
            claude_path_candidates: &[],
            mappings_error: None,
        });

        let conflict = issues
            .iter()
            .find(|issue| issue.action == "resolve_conflict_copy")
            .expect("remapped lock conflict must be visible");
        assert_eq!(conflict.root, ".codex");
        assert!(conflict
            .source_path
            .as_deref()
            .is_some_and(|path| path.contains(".agent-sync/codex")));
    }

    #[test]
    fn plugin_plan_structured_repairs_are_actionable_and_visible() {
        use crate::codex_plugins::CodexRepairIssue;

        let plan = CodexPluginPlan {
            missing_managed_marketplaces: vec![CodexRepairIssue {
                id: "openai-curated".into(),
                code: "managed_catalog_missing".into(),
                message: "initialize Codex on this machine".into(),
            }],
            blocked_plugins: vec![CodexRepairIssue {
                id: "slack@openai-curated".into(),
                code: "managed_catalog_missing".into(),
                message: "openai-curated is unavailable".into(),
            }],
            config_repairs: vec![CodexRepairIssue {
                id: "mcp_servers.node_repl".into(),
                code: "managed_config_path_mismatch".into(),
                message: "CODEX_HOME points at another profile".into(),
            }],
            ..CodexPluginPlan::default()
        };

        let mut issues = Vec::new();
        plugin_issues(&mut issues, ".codex", Some(&plan), "repair_codex_plugins");

        assert_eq!(issues.len(), 3);
        assert!(issues
            .iter()
            .all(|issue| issue.action == "repair_codex_plugins"));
        assert!(issues.iter().all(|issue| issue.severity == "warning"));
        assert!(issues.iter().any(|issue| {
            issue.title.contains("slack@openai-curated")
                && issue.title.contains("managed_catalog_missing")
        }));
        assert!(issues.iter().any(|issue| {
            issue.title.contains("mcp_servers.node_repl") && issue.detail.contains("CODEX_HOME")
        }));
    }

    #[test]
    fn plugin_lock_block_does_not_hide_structured_repairs() {
        use crate::codex_plugins::CodexRepairIssue;

        let plan = CodexPluginPlan {
            blocked: Some("inventory unavailable".into()),
            blocked_plugins: vec![CodexRepairIssue {
                id: "google-calendar@openai-curated".into(),
                code: "managed_catalog_missing".into(),
                message: "openai-curated is unavailable".into(),
            }],
            ..CodexPluginPlan::default()
        };
        let mut issues = Vec::new();
        plugin_issues(&mut issues, ".codex", Some(&plan), "repair_codex_plugins");

        assert_eq!(issues.len(), 2);
        assert!(issues
            .iter()
            .any(|issue| issue.title == "Plugin lock blocked"));
        assert!(issues
            .iter()
            .any(|issue| issue.title.contains("google-calendar@openai-curated")));
    }

    #[test]
    fn invalid_codex_config_is_routed_to_codex_repair() {
        let dir = tempfile::tempdir().unwrap();
        let codex = dir.path().join(".codex");
        let claude = dir.path().join(".claude");
        fs::create_dir_all(&codex).unwrap();
        fs::create_dir_all(&claude).unwrap();
        fs::write(codex.join("config.toml"), "not = [valid toml").unwrap();

        let issues = scan_fixture(&codex, &claude, &LocalState::default());
        let issue = issues
            .iter()
            .find(|issue| issue.category == "plugins" && issue.title.contains("config_invalid"))
            .unwrap_or_else(|| panic!("no managed-config issue among {:#?}", issues));
        assert_eq!(issue.action, "repair_codex_plugins");
        assert_eq!(issue.severity, "warning");
        assert!(!issues
            .iter()
            .any(|issue| issue.title == "config.toml does not parse"));
    }

    #[test]
    fn stale_managed_mcp_paths_use_one_codex_repair_action() {
        let dir = tempfile::tempdir().unwrap();
        let codex = dir.path().join("machine-b/.codex");
        let claude = dir.path().join(".claude");
        let source_home = dir.path().join("machine-a/.codex");
        fs::create_dir_all(&codex).unwrap();
        fs::create_dir_all(&claude).unwrap();
        fs::create_dir_all(&source_home).unwrap();
        let missing_runtime = source_home.join("plugins/cache/node_repl");
        fs::write(
            codex.join("config.toml"),
            format!(
                r#"
[mcp_servers.node_repl]
command = "{}"

[mcp_servers.node_repl.env]
CODEX_HOME = "{}"
NODE_REPL_TRUSTED_CODE_PATHS = "{}"
NODE_REPL_NODE_PATH = "{}"
"#,
                missing_runtime.display(),
                source_home.display(),
                source_home.display(),
                source_home.join("node_modules").display(),
            ),
        )
        .unwrap();

        let issues = scan_fixture(&codex, &claude, &LocalState::default());
        let managed: Vec<&SetupIssue> = issues
            .iter()
            .filter(|issue| issue.title.contains("mcp_servers.node_repl"))
            .collect();
        assert!(
            !managed.is_empty(),
            "no managed MCP issue among {issues:#?}"
        );
        assert!(managed
            .iter()
            .all(|issue| issue.action == "repair_codex_plugins"));
        assert!(!issues
            .iter()
            .any(|issue| issue.title == "MCP server 'node_repl' command not found"));
    }

    #[test]
    fn hook_hash_is_stable_across_key_order_and_review_clears_it() {
        let a = serde_json::from_str::<serde_json::Value>("{\"a\":1,\"b\":[{\"x\":1,\"y\":2}]}")
            .unwrap();
        let b = serde_json::from_str::<serde_json::Value>("{\"b\":[{\"y\":2,\"x\":1}],\"a\":1}")
            .unwrap();
        assert_eq!(hash_definition(&a), hash_definition(&b));

        let dir = tempfile::tempdir().unwrap();
        let codex = dir.path().join(".codex");
        let claude = dir.path().join(".claude");
        fs::create_dir_all(&codex).unwrap();
        fs::create_dir_all(&claude).unwrap();
        fs::write(codex.join("hooks.json"), "[{\"cmd\":\"echo hi\"}]").unwrap();

        let mut state = LocalState::default();
        let issues = scan_fixture(&codex, &claude, &state);
        let hook = issues
            .iter()
            .find(|i| i.category == "hooks")
            .expect("unreviewed hook");
        assert_eq!(hook.action, "review_hooks");

        let defs = hook_definitions(&codex, &claude);
        assert_eq!(defs.len(), 1);
        state.reviewed_hooks.insert(defs[0].2.clone(), 1);
        let issues = scan_fixture(&codex, &claude, &state);
        assert!(
            !issues.iter().any(|i| i.category == "hooks"),
            "reviewed hook must clear"
        );
    }

    #[test]
    fn local_state_roundtrip_caps_and_tolerates_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local-state.json");
        assert!(load_local_state(&path).reviewed_hooks.is_empty());
        fs::write(&path, "{broken").unwrap();
        assert!(load_local_state(&path).dismissed_issues.is_empty());

        let mut state = LocalState::default();
        state.reviewed_hooks.insert("h1".into(), 7);
        state.dismissed_issues = (0..600).map(|i| format!("id{}", i)).collect();
        save_local_state(&path, &state).unwrap();
        let loaded = load_local_state(&path);
        assert_eq!(loaded.reviewed_hooks.get("h1"), Some(&7));
        assert_eq!(loaded.dismissed_issues.len(), MAX_DISMISSED);
        assert_eq!(loaded.dismissed_issues.last().unwrap(), "id599");
    }

    #[cfg(unix)]
    #[test]
    fn local_state_save_ignores_predictable_temp_symlink() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local-state.json");
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, "do not replace").unwrap();
        symlink(&victim, &path).unwrap();
        // This is the predictable path used by the old implementation.
        let planted = dir.path().join("local-state.json.tmp");
        symlink(&victim, &planted).unwrap();

        let mut state = LocalState::default();
        state.reviewed_hooks.insert("safe".into(), 9);
        save_local_state(&path, &state).unwrap();

        assert_eq!(fs::read_to_string(&victim).unwrap(), "do not replace");
        assert!(fs::symlink_metadata(&planted)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(!fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(load_local_state(&path).reviewed_hooks.get("safe"), Some(&9));
    }

    #[test]
    fn codex_threads_group_by_exact_first_record_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let codex = dir.path().join(".codex");
        let sessions = codex.join("sessions/2026/07/15");
        fs::create_dir_all(&sessions).unwrap();
        let archived = codex.join("archived_sessions");
        fs::create_dir_all(&archived).unwrap();

        let meta = |id: &str, cwd: &str| {
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{}\",\"cwd\":\"{}\"}}}}\n{{\"type\":\"turn\"}}\n",
                id, cwd
            )
        };
        fs::write(
            sessions.join("rollout-2026-07-15T10-00-00-019f-bbbb.jsonl"),
            meta("019f-bbbb", "/a/repo"),
        )
        .unwrap();
        fs::write(
            sessions.join("rollout-2026-07-15T11-00-00-019f-aaaa.jsonl"),
            meta("019f-aaaa", "/a/repo"),
        )
        .unwrap();
        fs::write(
            archived.join("rollout-2026-07-01T09-00-00-019f-cccc.jsonl"),
            meta("019f-cccc", "/a/other"),
        )
        .unwrap();
        // Ignored: no session_meta cwd, non-rollout name, prefix-only cwd.
        fs::write(
            sessions.join("rollout-2026-07-15T12-00-00-broken.jsonl"),
            "{}\n",
        )
        .unwrap();
        fs::write(sessions.join("notes.txt"), "x").unwrap();
        fs::write(
            sessions.join("rollout-2026-07-15T13-00-00-019f-dddd.jsonl"),
            meta("019f-dddd", "/a/repo-sibling"),
        )
        .unwrap();

        let grouped = codex_threads_by_cwd(&codex);
        assert_eq!(
            grouped.get("/a/repo").cloned().unwrap_or_default(),
            vec!["019f-aaaa".to_string(), "019f-bbbb".to_string()],
            "sorted ids, exact cwd only: {:?}",
            grouped
        );
        assert_eq!(grouped["/a/other"], vec!["019f-cccc"]);
        assert_eq!(grouped["/a/repo-sibling"], vec!["019f-dddd"]);
    }

    #[test]
    fn codex_path_candidates_become_structured_attach_project_issues() {
        let dir = tempfile::tempdir().unwrap();
        let codex = dir.path().join(".codex");
        let claude = dir.path().join(".claude");
        fs::create_dir_all(&codex).unwrap();
        fs::create_dir_all(&claude).unwrap();

        let candidates = vec![
            ProjectPathCandidate {
                provider: "codex".to_string(),
                source_key: "/a/repo".to_string(),
                source_path: "/a/repo".to_string(),
                git_origin: None,
                mapped_path: None,
                affected_threads: vec!["019f-aaaa".to_string(), "019f-bbbb".to_string()],
                path_state: Some("unmapped".to_string()),
            },
            ProjectPathCandidate {
                provider: "codex".to_string(),
                source_key: "/a/stale".to_string(),
                source_path: "/a/stale".to_string(),
                git_origin: None,
                mapped_path: Some("/b/deleted".to_string()),
                affected_threads: Vec::new(),
                path_state: Some("missing_target".to_string()),
            },
        ];
        let resolve = |_: &str| true;
        let env_present = |_: &str| true;
        let issues = scan(&ScanInput {
            codex_dir: &codex,
            claude_dir: &claude,
            lock_dirs: &[],
            codex_plan: None,
            claude_plan: None,
            state: &LocalState::default(),
            resolve: &resolve,
            env_present: &env_present,
            sidebar_pending: None,
            codex_path_candidates: &candidates,
            claude_path_candidates: &[],
            mappings_error: None,
        });

        let rows: Vec<&SetupIssue> = issues
            .iter()
            .filter(|issue| issue.action == "attach_project")
            .collect();
        assert_eq!(rows.len(), 2, "{:#?}", issues);
        assert!(rows.iter().all(|issue| issue.category == "paths"
            && issue.root == ".codex"
            && issue.severity == "warning"));
        let fresh = rows
            .iter()
            .find(|issue| issue.source_path.as_deref() == Some("/a/repo"))
            .unwrap();
        let candidate = fresh.project_path.as_ref().expect("structured payload");
        assert_eq!(candidate.affected_threads.len(), 2);
        assert!(fresh.detail.contains("2 task(s)"), "{}", fresh.detail);
        let stale = rows
            .iter()
            .find(|issue| issue.source_path.as_deref() == Some("/a/stale"))
            .unwrap();
        assert!(stale.detail.contains("/b/deleted"), "{}", stale.detail);
        assert_eq!(
            stale.project_path.as_ref().unwrap().mapped_path.as_deref(),
            Some("/b/deleted")
        );
    }

    #[test]
    fn foreign_home_detection_ignores_existing_home() {
        let home = dirs::home_dir().unwrap();
        let ours = format!("see {}/notes.md", home.display());
        assert_eq!(foreign_home_path(&ours), None);
        assert!(foreign_home_path("see /Users/not-a-real-user-xyz/notes.md").is_some());
    }
}
