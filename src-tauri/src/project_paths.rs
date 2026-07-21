//! Machine-local project-path mappings (PLAN_CODEX_MANUAL_PROJECT_PATH_PICKING.md;
//! shared schema from PLAN_MANUAL_PROJECT_PATH_MAPPING.md §4).
//!
//! One record per exact source project the user has attached to a local
//! folder. The file lives at the top of `~/.agent-sync/` — outside every
//! profile remap subtree — so `Roots::rel` maps it to no logical path and it
//! is structurally unsyncable: a chosen target path is local configuration
//! and must never become a cloud-wide setting (D2).
//!
//! Everything here is Tauri-free and filesystem-light: load/save plus pure
//! validation. Deciding *when* a mapping applies (sidebar planning, resume
//! commands) stays with the callers.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Component, Path};

/// File name under `~/.agent-sync/`.
pub const MAPPINGS_FILE: &str = "project-path-mappings.json";

const SCHEMA: u32 = 1;
const MAX_MAPPINGS: usize = 512;
const MAX_STRING: usize = 1024;
const MAX_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectPathMappings {
    pub schema: u32,
    #[serde(default)]
    pub mappings: Vec<ProjectPathMapping>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ProjectPathMapping {
    /// `LocalProfile.id`.
    pub profile: String,
    /// "codex" | "claude".
    pub provider: String,
    /// Stable identity of the source project: the Claude bucket basename or
    /// the Codex source path (== `source_path` for Codex).
    pub source_key: String,
    /// Original transcript/sidebar cwd, for display.
    pub source_path: String,
    /// Exact absolute path selected on this machine. The user's spelling is
    /// preserved — the agent's cwd spelling is part of its project identity,
    /// so symlinks are never silently canonicalized.
    pub target_path: String,
}

fn ok_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_STRING && !value.chars().any(|c| c.is_control())
}

fn validate_mapping(mapping: &ProjectPathMapping) -> Result<(), String> {
    if !matches!(mapping.provider.as_str(), "codex" | "claude") {
        return Err(format!("unknown provider '{}'", mapping.provider));
    }
    for (label, value) in [
        ("profile", &mapping.profile),
        ("source_key", &mapping.source_key),
        ("source_path", &mapping.source_path),
        ("target_path", &mapping.target_path),
    ] {
        if !ok_text(value) {
            return Err(format!("invalid mapping {}: '{}'", label, value));
        }
    }
    Ok(())
}

fn validate(mappings: &ProjectPathMappings) -> Result<(), String> {
    if mappings.schema != SCHEMA {
        return Err(format!(
            "unsupported project-path mappings schema {} (this app understands {})",
            mappings.schema, SCHEMA
        ));
    }
    if mappings.mappings.len() > MAX_MAPPINGS {
        return Err(format!(
            "project-path mappings exceed {} entries",
            MAX_MAPPINGS
        ));
    }
    for mapping in &mappings.mappings {
        validate_mapping(mapping)?;
    }
    Ok(())
}

/// Bounded, strict load. A missing file is an empty document; anything
/// malformed is an error (callers that only display degrade to default).
pub fn load_mappings(path: &Path) -> Result<ProjectPathMappings, String> {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProjectPathMappings {
                schema: SCHEMA,
                mappings: Vec::new(),
            });
        }
        Err(error) => return Err(format!("read {}: {}", path.display(), error)),
    };
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "project-path mappings exceed {} bytes",
            MAX_FILE_BYTES
        ));
    }
    let raw = fs::read_to_string(path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    let mappings: ProjectPathMappings =
        serde_json::from_str(&raw).map_err(|e| format!("parse project-path mappings: {}", e))?;
    validate(&mappings)?;
    Ok(mappings)
}

/// Atomic save: exclusively-created temp file in the same directory, then
/// rename (same shape as `readiness::save_local_state`).
pub fn save_mappings(path: &Path, mappings: &ProjectPathMappings) -> Result<(), String> {
    validate(mappings)?;
    let parent = path
        .parent()
        .ok_or_else(|| format!("no parent directory for {}", path.display()))?;
    fs::create_dir_all(parent).map_err(|e| format!("create {}: {}", parent.display(), e))?;
    let json = serde_json::to_string_pretty(mappings).map_err(|e| e.to_string())?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| format!("temp file in {}: {}", parent.display(), e))?;
    tmp.as_file_mut()
        .write_all(format!("{}\n", json).as_bytes())
        .map_err(|e| format!("write project-path mappings: {}", e))?;
    tmp.persist(path)
        .map_err(|e| format!("replace {}: {}", path.display(), e))?;
    Ok(())
}

/// The target the user must pick: an existing absolute directory with no
/// `.`/`..` components or control characters. Spelling is preserved as-is.
pub fn validate_target_path(target: &str) -> Result<(), String> {
    if !ok_text(target) {
        return Err("target path is empty, too long, or contains control characters".to_string());
    }
    let path = Path::new(target);
    if !path.is_absolute() {
        return Err(format!("target path must be absolute: '{}'", target));
    }
    if path
        .components()
        .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
    {
        return Err(format!(
            "target path must not contain '.' or '..': '{}'",
            target
        ));
    }
    let meta =
        fs::metadata(path).map_err(|_| format!("'{}' does not exist on this machine", target))?;
    if !meta.is_dir() {
        return Err(format!("'{}' is not a directory", target));
    }
    Ok(())
}

/// Insert or replace the mapping for `(profile, provider, source_key)`.
/// Rejects a second source mapped to the same target within one profile ×
/// provider (D6: one source and one target identity).
pub fn upsert(
    mappings: &mut ProjectPathMappings,
    mapping: ProjectPathMapping,
) -> Result<(), String> {
    validate_mapping(&mapping)?;
    // Claude targets collide at the encoded bucket, not the path string —
    // two spellings that encode identically would race for one alias name.
    let same_target = |existing: &ProjectPathMapping| {
        if mapping.provider == "claude" {
            encode_claude_project_path(&existing.target_path)
                == encode_claude_project_path(&mapping.target_path)
        } else {
            existing.target_path == mapping.target_path
        }
    };
    if mappings.mappings.iter().any(|existing| {
        existing.profile == mapping.profile
            && existing.provider == mapping.provider
            && existing.source_key != mapping.source_key
            && same_target(existing)
    }) {
        return Err(format!(
            "'{}' is already the target of another {} mapping in this profile — remove that mapping first",
            mapping.target_path, mapping.provider
        ));
    }
    if let Some(existing) = mappings.mappings.iter_mut().find(|existing| {
        existing.profile == mapping.profile
            && existing.provider == mapping.provider
            && existing.source_key == mapping.source_key
    }) {
        *existing = mapping;
        return Ok(());
    }
    if mappings.mappings.len() >= MAX_MAPPINGS {
        return Err(format!(
            "project-path mappings are limited to {} entries",
            MAX_MAPPINGS
        ));
    }
    mappings.schema = SCHEMA;
    mappings.mappings.push(mapping);
    Ok(())
}

/// Remove the mapping for `(profile, provider, source_key)`; false when absent.
pub fn remove(
    mappings: &mut ProjectPathMappings,
    profile: &str,
    provider: &str,
    source_key: &str,
) -> bool {
    let before = mappings.mappings.len();
    mappings.mappings.retain(|m| {
        !(m.profile == profile && m.provider == provider && m.source_key == source_key)
    });
    mappings.mappings.len() != before
}

/// The full record for `(profile, provider, source_key)`.
pub fn mapping_for<'a>(
    mappings: &'a ProjectPathMappings,
    profile: &str,
    provider: &str,
    source_key: &str,
) -> Option<&'a ProjectPathMapping> {
    mappings
        .mappings
        .iter()
        .find(|m| m.profile == profile && m.provider == provider && m.source_key == source_key)
}

/// Saved target for a source key, existence-unchecked — callers decide what
/// a stale target means (the sidebar planner re-raises it, D5).
pub fn target_for<'a>(
    mappings: &'a ProjectPathMappings,
    profile: &str,
    provider: &str,
    source_key: &str,
) -> Option<&'a str> {
    mapping_for(mappings, profile, provider, source_key).map(|m| m.target_path.as_str())
}

// ── Claude bucket aliases (PLAN_CLAUDE_PROJECT_PATH_REMAP.md) ────────────────
//
// A Claude mapping materializes as one relative symlink inside the profile's
// `projects/` directory: `<encode(target_path)>` → `<source_key>`. Claude
// Code computes the bucket name from its cwd and follows the link to the
// real source bucket, so transcripts never move and sync (strictly
// no-follow) keeps publishing only the source key. Verified against CLI
// 2.1.211 in the Phase 0 spike.

/// Claude Code's project bucket encoding, observed on CLI 2.1.211 (§4.1
/// fixtures): every character that is not ASCII-alphanumeric becomes one
/// `-`. The CLI is JavaScript, so "one character" is one UTF-16 code unit —
/// replicated exactly (an astral-plane char yields two dashes).
pub fn encode_claude_project_path(path: &str) -> String {
    path.encode_utf16()
        .map(|unit| match u8::try_from(unit) {
            Ok(byte) if byte.is_ascii_alphanumeric() => byte as char,
            _ => '-',
        })
        .collect()
}

/// A Claude source key is one normal directory basename under `projects/`.
pub fn validate_claude_source_key(key: &str) -> Result<(), String> {
    if !ok_text(key) || key == "." || key == ".." || key.contains('/') || key.contains('\\') {
        return Err(format!("invalid Claude project key '{}'", key));
    }
    Ok(())
}

/// Where a saved Claude mapping stands on disk right now. Read-only, and
/// never follows the alias — the alias is only ever inspected with
/// `symlink_metadata`/`read_link`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeAliasState {
    /// Exact expected alias in place, source and target both present.
    Ready,
    /// Target encodes to the source key itself — no alias needed.
    ReadyWithoutAlias,
    /// Mapping saved but the alias link is absent (crash window; Repair).
    MissingAlias,
    /// Chosen target directory is gone (stale; Change/Remove, D5).
    MissingTarget,
    /// Source bucket is absent or not a real directory.
    MissingSource,
    /// A real directory already occupies the alias name (never merged).
    ConflictingDirectory,
    /// A symlink pointing somewhere else occupies the alias name.
    ConflictingSymlink,
    /// The OS refused to inspect the projects directory.
    PermissionDenied,
}

impl ClaudeAliasState {
    pub fn as_str(self) -> &'static str {
        match self {
            ClaudeAliasState::Ready => "ready",
            ClaudeAliasState::ReadyWithoutAlias => "ready_without_alias",
            ClaudeAliasState::MissingAlias => "missing_alias",
            ClaudeAliasState::MissingTarget => "missing_target",
            ClaudeAliasState::MissingSource => "missing_source",
            ClaudeAliasState::ConflictingDirectory => "conflicting_directory",
            ClaudeAliasState::ConflictingSymlink => "conflicting_symlink",
            ClaudeAliasState::PermissionDenied => "permission_denied",
        }
    }

    pub fn is_ready(self) -> bool {
        matches!(
            self,
            ClaudeAliasState::Ready | ClaudeAliasState::ReadyWithoutAlias
        )
    }
}

fn permission_error(path: &Path, error: &std::io::Error) -> String {
    format!(
        "macOS denied access to '{}' ({}). Grant Mallard access to this folder (or Full Disk Access when required), then use Repair mapping.",
        path.display(),
        error
    )
}

/// Calculate the alias state for a saved mapping. `projects_dir` is the
/// profile's `<claude-root>/projects`.
pub fn claude_alias_state(projects_dir: &Path, mapping: &ProjectPathMapping) -> ClaudeAliasState {
    let source = projects_dir.join(&mapping.source_key);
    match fs::symlink_metadata(&source) {
        Ok(meta) if meta.file_type().is_dir() => {}
        Ok(_) => return ClaudeAliasState::MissingSource,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return ClaudeAliasState::PermissionDenied
        }
        Err(_) => return ClaudeAliasState::MissingSource,
    }
    if !fs::metadata(&mapping.target_path).is_ok_and(|m| m.is_dir()) {
        return ClaudeAliasState::MissingTarget;
    }
    let bucket = encode_claude_project_path(&mapping.target_path);
    if bucket == mapping.source_key {
        return ClaudeAliasState::ReadyWithoutAlias;
    }
    let alias = projects_dir.join(&bucket);
    match fs::symlink_metadata(&alias) {
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            ClaudeAliasState::PermissionDenied
        }
        Err(_) => ClaudeAliasState::MissingAlias,
        Ok(meta) if meta.file_type().is_symlink() => match fs::read_link(&alias) {
            Ok(dest) if dest == Path::new(mapping.source_key.as_str()) => ClaudeAliasState::Ready,
            _ => ClaudeAliasState::ConflictingSymlink,
        },
        Ok(_) => ClaudeAliasState::ConflictingDirectory,
    }
}

/// Materialize the alias for a saved mapping: an atomic, relative symlink
/// whose target is exactly `source_key`. Idempotent when the exact expected
/// link already exists; anything else occupying the name is a fail-closed
/// collision — never replaced, never merged (§6). Returns the alias path,
/// or None when the target encodes to the source key itself.
#[cfg(unix)]
pub fn create_claude_alias(
    projects_dir: &Path,
    mapping: &ProjectPathMapping,
) -> Result<Option<std::path::PathBuf>, String> {
    let bucket = encode_claude_project_path(&mapping.target_path);
    if bucket == mapping.source_key {
        return Ok(None);
    }
    let alias = projects_dir.join(&bucket);
    match std::os::unix::fs::symlink(&mapping.source_key, &alias) {
        Ok(()) => Ok(Some(alias)),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let meta = fs::symlink_metadata(&alias)
                .map_err(|e| format!("inspect '{}': {}", alias.display(), e))?;
            if meta.file_type().is_symlink()
                && fs::read_link(&alias).is_ok_and(|d| d == Path::new(mapping.source_key.as_str()))
            {
                return Ok(Some(alias));
            }
            if meta.file_type().is_symlink() {
                Err(format!(
                    "'{}' is already a link to something else — it was left alone; remove it manually if it is stale",
                    alias.display()
                ))
            } else {
                Err(format!(
                    "'{}' already exists as a real directory with its own Claude history — histories are never auto-merged; choose a different folder or move that directory aside first",
                    alias.display()
                ))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            Err(permission_error(&alias, &error))
        }
        Err(error) => Err(format!("create alias '{}': {}", alias.display(), error)),
    }
}

/// Windows does not offer the same permission-free relative directory-link
/// behavior as Unix. Keep the project mapping operation fail-closed until a
/// Windows-native alias strategy is implemented.
#[cfg(not(unix))]
pub fn create_claude_alias(
    _projects_dir: &Path,
    mapping: &ProjectPathMapping,
) -> Result<Option<std::path::PathBuf>, String> {
    if encode_claude_project_path(&mapping.target_path) == mapping.source_key {
        return Ok(None);
    }
    Err(
        "Claude project folder remapping is not supported on Windows yet; use the original project path"
            .to_string(),
    )
}

/// Unlink a mapping's alias only when it is exactly the expected relative
/// link. A missing alias is fine (already gone); anything unexpected at the
/// name is left alone and reported.
pub fn remove_claude_alias(
    projects_dir: &Path,
    mapping: &ProjectPathMapping,
) -> Result<(), String> {
    let bucket = encode_claude_project_path(&mapping.target_path);
    if bucket == mapping.source_key {
        return Ok(());
    }
    let alias = projects_dir.join(&bucket);
    match fs::symlink_metadata(&alias) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            Err(permission_error(&alias, &error))
        }
        Err(error) => Err(format!("inspect '{}': {}", alias.display(), error)),
        Ok(meta) if meta.file_type().is_symlink() => {
            let dest = fs::read_link(&alias)
                .map_err(|e| format!("read alias '{}': {}", alias.display(), e))?;
            if dest == Path::new(mapping.source_key.as_str()) {
                fs::remove_file(&alias)
                    .map_err(|e| format!("remove alias '{}': {}", alias.display(), e))
            } else {
                Err(format!(
                    "'{}' points somewhere else and was left alone",
                    alias.display()
                ))
            }
        }
        Ok(_) => Err(format!(
            "'{}' is not the expected alias link and was left alone",
            alias.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping(source: &str, target: &str) -> ProjectPathMapping {
        ProjectPathMapping {
            profile: "codex".to_string(),
            provider: "codex".to_string(),
            source_key: source.to_string(),
            source_path: source.to_string(),
            target_path: target.to_string(),
        }
    }

    #[test]
    fn round_trip_missing_file_and_strict_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("project-path-mappings.json");
        let loaded = load_mappings(&path).unwrap();
        assert_eq!(loaded.schema, SCHEMA);
        assert!(loaded.mappings.is_empty());

        let mut doc = loaded;
        upsert(&mut doc, mapping("/a/repo", "/b/repo")).unwrap();
        save_mappings(&path, &doc).unwrap();
        assert_eq!(load_mappings(&path).unwrap(), doc);

        fs::write(&path, "{broken").unwrap();
        assert!(load_mappings(&path).is_err());
        fs::write(&path, "{\"schema\":99,\"mappings\":[]}").unwrap();
        assert!(load_mappings(&path)
            .unwrap_err()
            .contains("unsupported project-path mappings schema"));
    }

    #[test]
    fn upsert_replaces_rejects_duplicate_target_and_caps() {
        let mut doc = ProjectPathMappings::default();
        doc.schema = SCHEMA;
        upsert(&mut doc, mapping("/a/one", "/b/one")).unwrap();
        // Same source: replaced in place, no duplicate row.
        upsert(&mut doc, mapping("/a/one", "/b/other")).unwrap();
        assert_eq!(doc.mappings.len(), 1);
        assert_eq!(
            target_for(&doc, "codex", "codex", "/a/one"),
            Some("/b/other")
        );

        // Another source onto the same target in the same profile: rejected.
        let err = upsert(&mut doc, mapping("/a/two", "/b/other")).unwrap_err();
        assert!(err.contains("already the target"), "{}", err);
        // ...but the same target in a different profile is fine.
        let mut other_profile = mapping("/a/two", "/b/other");
        other_profile.profile = "codex-work".to_string();
        upsert(&mut doc, other_profile).unwrap();

        // Invalid provider and control characters are rejected.
        let mut bad = mapping("/a/three", "/b/three");
        bad.provider = "vim".to_string();
        assert!(upsert(&mut doc, bad).is_err());
        assert!(upsert(&mut doc, mapping("/a/\u{7}", "/b/x")).is_err());

        let mut full = ProjectPathMappings {
            schema: SCHEMA,
            mappings: (0..MAX_MAPPINGS)
                .map(|i| mapping(&format!("/a/{}", i), &format!("/b/{}", i)))
                .collect(),
        };
        assert!(upsert(&mut full, mapping("/a/overflow", "/b/overflow"))
            .unwrap_err()
            .contains("limited"));
    }

    #[test]
    fn remove_only_drops_the_named_mapping() {
        let mut doc = ProjectPathMappings::default();
        doc.schema = SCHEMA;
        upsert(&mut doc, mapping("/a/one", "/b/one")).unwrap();
        upsert(&mut doc, mapping("/a/two", "/b/two")).unwrap();
        assert!(!remove(&mut doc, "codex", "codex", "/a/none"));
        assert!(remove(&mut doc, "codex", "codex", "/a/one"));
        assert_eq!(doc.mappings.len(), 1);
        assert_eq!(target_for(&doc, "codex", "codex", "/a/two"), Some("/b/two"));
    }

    #[test]
    fn claude_encoder_matches_observed_cli_buckets() {
        // Real pairs observed on this machine plus the Phase 0 spike
        // (Claude Code 2.1.211, PLAN_CLAUDE_PROJECT_PATH_REMAP.md §4.1).
        for (path, bucket) in [
            (
                "/Users/hequ/Desktop/project/memory/tauri-codex-sync",
                "-Users-hequ-Desktop-project-memory-tauri-codex-sync",
            ),
            (
                "/Users/hequ/.ccgui/workspace",
                "-Users-hequ--ccgui-workspace",
            ),
            (
                "/Users/hequ/Desktop/project/danci_nextjs",
                "-Users-hequ-Desktop-project-danci-nextjs",
            ),
            (
                "/private/tmp/agent-sync-spike/wei rd.dir_\u{fc}",
                "-private-tmp-agent-sync-spike-wei-rd-dir--",
            ),
        ] {
            assert_eq!(encode_claude_project_path(path), bucket, "{}", path);
        }
        // JS replaces per UTF-16 code unit: an astral char is two dashes.
        assert_eq!(encode_claude_project_path("/a/😀b"), "-a---b");
    }

    #[test]
    fn claude_source_key_must_be_one_normal_basename() {
        validate_claude_source_key("-Users-a-repo").unwrap();
        for bad in ["", ".", "..", "a/b", "a\\b", "a\u{7}b"] {
            assert!(validate_claude_source_key(bad).is_err(), "{:?}", bad);
        }
    }

    fn claude_mapping(source_key: &str, target: &Path) -> ProjectPathMapping {
        ProjectPathMapping {
            profile: "claude".to_string(),
            provider: "claude".to_string(),
            source_key: source_key.to_string(),
            source_path: "/a/repo".to_string(),
            target_path: target.to_string_lossy().to_string(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn claude_alias_state_matrix() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        let target = dir.path().join("target-repo");
        fs::create_dir_all(&projects).unwrap();
        fs::create_dir_all(&target).unwrap();
        let mapping = claude_mapping("-a-repo", &target);
        let bucket = encode_claude_project_path(&mapping.target_path);

        // Source bucket absent → the mapping has nothing to alias.
        assert_eq!(
            claude_alias_state(&projects, &mapping),
            ClaudeAliasState::MissingSource
        );
        fs::create_dir_all(projects.join("-a-repo")).unwrap();
        assert_eq!(
            claude_alias_state(&projects, &mapping),
            ClaudeAliasState::MissingAlias
        );

        // Exact expected relative link → Ready.
        std::os::unix::fs::symlink("-a-repo", projects.join(&bucket)).unwrap();
        assert_eq!(
            claude_alias_state(&projects, &mapping),
            ClaudeAliasState::Ready
        );

        // Stale target: the folder the user picked is gone.
        fs::remove_dir(&target).unwrap();
        assert_eq!(
            claude_alias_state(&projects, &mapping),
            ClaudeAliasState::MissingTarget
        );
        fs::create_dir_all(&target).unwrap();

        // Wrong link and real directory at the alias name are collisions.
        fs::remove_file(projects.join(&bucket)).unwrap();
        std::os::unix::fs::symlink("-somewhere-else", projects.join(&bucket)).unwrap();
        assert_eq!(
            claude_alias_state(&projects, &mapping),
            ClaudeAliasState::ConflictingSymlink
        );
        fs::remove_file(projects.join(&bucket)).unwrap();
        fs::create_dir_all(projects.join(&bucket)).unwrap();
        assert_eq!(
            claude_alias_state(&projects, &mapping),
            ClaudeAliasState::ConflictingDirectory
        );
        fs::remove_dir(projects.join(&bucket)).unwrap();

        // A source bucket that is itself a symlink is never trusted.
        fs::remove_dir(projects.join("-a-repo")).unwrap();
        std::os::unix::fs::symlink("elsewhere", projects.join("-a-repo")).unwrap();
        assert_eq!(
            claude_alias_state(&projects, &mapping),
            ClaudeAliasState::MissingSource
        );

        // Target encoding == source key needs no alias at all.
        let same = claude_mapping(
            &encode_claude_project_path(&target.to_string_lossy()),
            &target,
        );
        fs::create_dir_all(projects.join(&same.source_key)).unwrap();
        assert_eq!(
            claude_alias_state(&projects, &same),
            ClaudeAliasState::ReadyWithoutAlias
        );
    }

    #[cfg(unix)]
    #[test]
    fn claude_alias_create_and_remove_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        let target = dir.path().join("target-repo");
        fs::create_dir_all(&projects).unwrap();
        fs::create_dir_all(&target).unwrap();
        let mapping = claude_mapping("-a-repo", &target);
        fs::create_dir_all(projects.join("-a-repo")).unwrap();

        // Create is atomic and idempotent for the exact expected link.
        let alias = create_claude_alias(&projects, &mapping).unwrap().unwrap();
        assert_eq!(fs::read_link(&alias).unwrap(), Path::new("-a-repo"));
        assert!(create_claude_alias(&projects, &mapping).unwrap().is_some());
        assert_eq!(
            claude_alias_state(&projects, &mapping),
            ClaudeAliasState::Ready
        );

        // Remove unlinks only the exact expected link; absent is fine.
        remove_claude_alias(&projects, &mapping).unwrap();
        assert!(fs::symlink_metadata(&alias).is_err());
        remove_claude_alias(&projects, &mapping).unwrap();

        // A foreign link or a real directory is never replaced or removed.
        std::os::unix::fs::symlink("-somewhere-else", &alias).unwrap();
        assert!(create_claude_alias(&projects, &mapping)
            .unwrap_err()
            .contains("link to something else"));
        assert!(remove_claude_alias(&projects, &mapping)
            .unwrap_err()
            .contains("left alone"));
        fs::remove_file(&alias).unwrap();
        fs::create_dir_all(&alias).unwrap();
        assert!(create_claude_alias(&projects, &mapping)
            .unwrap_err()
            .contains("never auto-merged"));
        assert!(remove_claude_alias(&projects, &mapping)
            .unwrap_err()
            .contains("left alone"));

        // Same-key mapping: no alias to create or remove.
        let same = claude_mapping(
            &encode_claude_project_path(&target.to_string_lossy()),
            &target,
        );
        assert!(create_claude_alias(&projects, &same).unwrap().is_none());
        remove_claude_alias(&projects, &same).unwrap();
    }

    #[test]
    fn claude_duplicate_encoded_bucket_rejected() {
        let mut doc = ProjectPathMappings::default();
        doc.schema = SCHEMA;
        let mut first = mapping("-a-one", "/b/repo one");
        first.provider = "claude".to_string();
        first.profile = "claude".to_string();
        upsert(&mut doc, first).unwrap();
        // Different spelling, same encoded bucket ("-b-repo-one") → rejected.
        let mut second = mapping("-a-two", "/b/repo.one");
        second.provider = "claude".to_string();
        second.profile = "claude".to_string();
        let err = upsert(&mut doc, second).unwrap_err();
        assert!(err.contains("already the target"), "{}", err);
    }

    #[test]
    fn target_validation_requires_existing_clean_absolute_directory() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("repo");
        fs::create_dir_all(&good).unwrap();
        let file = dir.path().join("file.txt");
        fs::write(&file, "x").unwrap();

        validate_target_path(good.to_str().unwrap()).unwrap();
        assert!(validate_target_path("relative/path").is_err());
        assert!(validate_target_path(&format!("{}/../repo", good.display())).is_err());
        assert!(validate_target_path("").is_err());
        assert!(validate_target_path("/definitely/not/a/real/dir-xyz").is_err());
        assert!(validate_target_path(file.to_str().unwrap()).is_err());
        assert!(validate_target_path("/tmp/\u{7}bell").is_err());
    }
}
