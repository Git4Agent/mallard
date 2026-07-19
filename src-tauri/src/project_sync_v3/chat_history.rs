//! Local-only Git-to-Codex history discovery for a registered project.
//!
//! This module deliberately does not participate in bundle capture. Titles,
//! first-message summaries, and inferred Git relationships stay on this
//! machine and are recomputed from the active project binding on demand.

use super::domain::{BindingState, LocalProjectId, Provider};
use super::persistence::V3Repository;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

const DEFAULT_COMMIT_LIMIT: usize = 50;
const MAX_COMMIT_LIMIT: usize = 50;
const MAX_GIT_COMMITS: usize = 10_000;
const MAX_SESSION_FILES: usize = 10_000;
const MAX_ROLLOUT_LINES: usize = 256;
const MAX_INDEX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ROLLOUT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SUMMARY_CHARS: usize = 180;
const ACTIVE_WINDOW_SECS: u64 = 5 * 60;
const AFTER_SESSION_WINDOW_SECS: u64 = 24 * 60 * 60;
const MAX_LINE_BYTES: usize = 1024 * 1024;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThreadMatchKind {
    DuringSession,
    AfterSession,
    StartedFrom,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CodexThreadSummary {
    pub thread_id: String,
    pub title: String,
    pub summary: String,
    pub started_at: u64,
    pub ended_at: u64,
    pub branch: Option<String>,
    pub recorded_sha: Option<String>,
    pub is_active: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CommitThreadReference {
    pub thread_id: String,
    pub match_kind: ThreadMatchKind,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct GitCommitSummary {
    pub sha: String,
    pub short_sha: String,
    pub committed_at: u64,
    pub subject: String,
    pub thread_refs: Vec<CommitThreadReference>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct GitBranchSummary {
    pub name: String,
    pub is_current: bool,
    pub available: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct GitHistoryPage {
    pub selected_branch: String,
    pub branches: Vec<GitBranchSummary>,
    pub commits: Vec<GitCommitSummary>,
    pub next_cursor: Option<String>,
    pub unique_thread_count: usize,
    pub reference_count: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct UnmappedThreadReference {
    pub thread_id: String,
    pub reason: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ProjectChatHistory {
    pub project_id: String,
    pub threads: Vec<CodexThreadSummary>,
    pub git: Option<GitHistoryPage>,
    pub unmapped: Vec<UnmappedThreadReference>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
struct SessionIndexEntry {
    title: Option<String>,
    updated_at: Option<u64>,
}

#[derive(Clone, Debug)]
struct ParsedRollout {
    thread: CodexThreadSummary,
    cwd: PathBuf,
}

#[derive(Debug)]
struct MappingResult {
    unmapped: Vec<UnmappedThreadReference>,
    unique_thread_count: usize,
    reference_count: usize,
}

#[derive(Debug)]
struct GitDiscovery {
    selected_branch: String,
    selected_available: bool,
    branches: Vec<GitBranchSummary>,
    commits: Vec<GitCommitSummary>,
    next_cursor: Option<String>,
}

struct ResolvedProject {
    project_id: LocalProjectId,
    project_root: PathBuf,
    codex_home: PathBuf,
}

/// Recompute a project's local Codex history and its best-effort relationship
/// to the selected Git branch. The result contains no synced metadata.
pub fn get_project_chat_history(
    repository: &V3Repository,
    local_project_id: &LocalProjectId,
    branch: Option<&str>,
    before_commit: Option<&str>,
    limit: Option<usize>,
) -> Result<ProjectChatHistory, String> {
    let resolved = resolve_project(repository, local_project_id)?;
    let mut warnings = Vec::new();
    let mut threads =
        scan_codex_threads(&resolved.codex_home, &resolved.project_root, &mut warnings)?;
    threads.sort_by(|left, right| {
        right
            .ended_at
            .cmp(&left.ended_at)
            .then_with(|| left.thread_id.cmp(&right.thread_id))
    });

    let page_limit = limit
        .unwrap_or(DEFAULT_COMMIT_LIMIT)
        .clamp(1, MAX_COMMIT_LIMIT);
    let recorded_branches = threads
        .iter()
        .filter_map(|thread| thread.branch.as_ref())
        .filter(|branch| !branch.trim().is_empty())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut git = discover_git_history_with_recorded_branches(
        &resolved.project_root,
        branch,
        before_commit,
        page_limit,
        &recorded_branches,
    )?;
    let mut unmapped = Vec::new();

    let git_page = if let Some(discovery) = git.as_mut() {
        // Mapping must use the complete bounded first-parent rail, rather than
        // just the visible page, so pagination does not change classification.
        let all_commits = if discovery.selected_available {
            load_first_parent_commits(
                &resolved.project_root,
                &discovery.selected_branch,
                MAX_GIT_COMMITS + 1,
            )?
        } else {
            Vec::new()
        };
        let was_truncated = all_commits.len() > MAX_GIT_COMMITS;
        let mut all_commits = all_commits
            .into_iter()
            .take(MAX_GIT_COMMITS)
            .collect::<Vec<_>>();
        if was_truncated {
            warnings.push(format!(
                "Git history was limited to the newest {MAX_GIT_COMMITS} first-parent commits"
            ));
        }
        let selected_branch = discovery.selected_branch.clone();
        let root = resolved.project_root.clone();
        let mapping =
            map_threads_to_commits(&threads, &mut all_commits, &selected_branch, |recorded| {
                discovery.selected_available
                    && recorded_commit_resolves_on_branch(&root, recorded, &selected_branch)
            });
        unmapped = mapping.unmapped;

        let references = all_commits
            .iter()
            .map(|commit| (commit.sha.clone(), commit.thread_refs.clone()))
            .collect::<BTreeMap<_, _>>();
        for commit in &mut discovery.commits {
            commit.thread_refs = references.get(&commit.sha).cloned().unwrap_or_default();
        }
        Some(GitHistoryPage {
            selected_branch: discovery.selected_branch.clone(),
            branches: discovery.branches.clone(),
            commits: discovery.commits.clone(),
            next_cursor: discovery.next_cursor.clone(),
            unique_thread_count: mapping.unique_thread_count,
            reference_count: mapping.reference_count,
        })
    } else {
        None
    };

    Ok(ProjectChatHistory {
        project_id: resolved.project_id.to_string(),
        threads,
        git: git_page,
        unmapped,
        warnings,
    })
}

/// Revalidates both project registration and rollout ownership before opening
/// an argument-safe Codex resume command in macOS Terminal.
pub fn open_codex_thread_in_terminal(
    repository: &V3Repository,
    local_project_id: &LocalProjectId,
    thread_id: &str,
) -> Result<(), String> {
    let resolved = resolve_owned_thread(repository, local_project_id, thread_id)?;
    launch_terminal_resume(thread_id, &resolved.project_root)
}

/// Fast launch guard used by the desktop deep-link action. It deliberately
/// skips Git discovery/mapping while enforcing the same registration,
/// binding, profile, UUID, and rollout-ownership checks as Terminal launch.
pub fn validate_codex_thread_ownership(
    repository: &V3Repository,
    local_project_id: &LocalProjectId,
    thread_id: &str,
) -> Result<(), String> {
    resolve_owned_thread(repository, local_project_id, thread_id).map(|_| ())
}

fn resolve_owned_thread(
    repository: &V3Repository,
    local_project_id: &LocalProjectId,
    thread_id: &str,
) -> Result<ResolvedProject, String> {
    validate_thread_uuid(thread_id)?;
    let resolved = resolve_project(repository, local_project_id)?;
    let mut warnings = Vec::new();
    let owned = scan_codex_threads(&resolved.codex_home, &resolved.project_root, &mut warnings)?
        .into_iter()
        .any(|thread| thread.thread_id == thread_id);
    if !owned {
        return Err("Codex thread does not belong to the selected project".to_string());
    }
    Ok(resolved)
}

fn resolve_project(
    repository: &V3Repository,
    local_project_id: &LocalProjectId,
) -> Result<ResolvedProject, String> {
    let config = repository.load_config()?;
    if config.project(local_project_id).is_none() {
        return Err(
            "project is not registered; unfinished setup drafts have no history".to_string(),
        );
    }
    let state = repository.load_bindings()?;
    let binding = state
        .bindings
        .iter()
        .find(|binding| {
            &binding.local_project_id == local_project_id && binding.state == BindingState::Active
        })
        .ok_or_else(|| "project has no active binding on this machine".to_string())?;

    let project_root = fs::canonicalize(&binding.project_root)
        .map_err(|error| format!("resolve project root '{}': {error}", binding.project_root))?;
    if project_root != PathBuf::from(&binding.canonical_project_root) {
        return Err(
            "project root changed since the binding was saved; open Project Settings".to_string(),
        );
    }
    let profile_id = binding
        .profile_ids
        .get(&Provider::Codex)
        .ok_or_else(|| "project has no Codex profile; open Project Settings".to_string())?;
    let profile = state
        .profiles
        .iter()
        .find(|profile| &profile.profile_id == profile_id && profile.provider == Provider::Codex)
        .ok_or_else(|| "project's Codex profile is missing; open Project Settings".to_string())?;
    let codex_home = fs::canonicalize(&profile.path)
        .map_err(|error| format!("resolve Codex profile '{}': {error}", profile.path))?;
    if codex_home != PathBuf::from(&profile.canonical_path) {
        return Err("Codex profile path changed; open Project Settings".to_string());
    }

    Ok(ResolvedProject {
        project_id: local_project_id.clone(),
        project_root,
        codex_home,
    })
}

fn scan_codex_threads(
    codex_home: &Path,
    project_root: &Path,
    warnings: &mut Vec<String>,
) -> Result<Vec<CodexThreadSummary>, String> {
    let index_path = codex_home.join("session_index.jsonl");
    let index = read_session_index(&index_path, warnings)?;
    let now = now_secs();
    let mut by_id = BTreeMap::<String, CodexThreadSummary>::new();
    let mut seen_files = 0usize;

    for directory_name in ["sessions", "archived_sessions"] {
        let directory = codex_home.join(directory_name);
        if !directory.exists() {
            continue;
        }
        for entry in WalkDir::new(&directory)
            .follow_links(false)
            .max_depth(8)
            .into_iter()
        {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    warnings.push(format!("Cannot inspect a Codex session path: {error}"));
                    continue;
                }
            };
            if !entry.file_type().is_file()
                || entry.path().extension().and_then(|value| value.to_str()) != Some("jsonl")
            {
                continue;
            }
            seen_files += 1;
            if seen_files > MAX_SESSION_FILES {
                warnings.push(format!(
                    "Codex session scan stopped after {MAX_SESSION_FILES} rollout files"
                ));
                break;
            }
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    warnings.push(format!("Cannot read '{}': {error}", entry.path().display()));
                    continue;
                }
            };
            if metadata.len() > MAX_ROLLOUT_BYTES {
                warnings.push(format!(
                    "Skipped oversized Codex rollout '{}'",
                    entry.path().display()
                ));
                continue;
            }
            let fallback_time = modified_secs(&metadata).unwrap_or(0);
            let lines = match read_bounded_lines(entry.path(), MAX_ROLLOUT_LINES, warnings) {
                Ok(lines) => lines,
                Err(error) => {
                    warnings.push(error);
                    continue;
                }
            };
            let parsed = match parse_rollout_lines(lines.iter().map(String::as_str), fallback_time)
            {
                Ok(parsed) => parsed,
                Err(error) => {
                    warnings.push(format!("Skipped '{}': {error}", entry.path().display()));
                    continue;
                }
            };
            if !cwd_belongs_to_project(&parsed.cwd, project_root) {
                continue;
            }
            let mut thread = parsed.thread;
            if let Some(index_entry) = index.get(&thread.thread_id) {
                if let Some(title) = &index_entry.title {
                    thread.title = title.clone();
                }
                if let Some(updated) = index_entry.updated_at {
                    thread.ended_at = updated.max(thread.started_at);
                }
            }
            if thread.title.is_empty() {
                thread.title = if thread.summary.is_empty() {
                    format!("Codex thread {}", short_id(&thread.thread_id))
                } else {
                    thread.summary.clone()
                };
            }
            thread.is_active = session_mtime_is_active(now, fallback_time);
            if fallback_time > now.saturating_add(ACTIVE_WINDOW_SECS) {
                warnings.push(format!(
                    "Codex rollout '{}' has a modified time in the future",
                    entry.path().display()
                ));
            }
            match by_id.get(&thread.thread_id) {
                Some(previous) if previous.ended_at > thread.ended_at => {}
                _ => {
                    by_id.insert(thread.thread_id.clone(), thread);
                }
            }
        }
    }
    Ok(by_id.into_values().collect())
}

fn read_session_index(
    path: &Path,
    warnings: &mut Vec<String>,
) -> Result<BTreeMap<String, SessionIndexEntry>, String> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let metadata = fs::metadata(path)
        .map_err(|error| format!("read Codex session index '{}': {error}", path.display()))?;
    if metadata.len() > MAX_INDEX_BYTES {
        warnings.push(format!(
            "Codex session index exceeded {} MiB and was ignored",
            MAX_INDEX_BYTES / (1024 * 1024)
        ));
        return Ok(BTreeMap::new());
    }
    let lines = read_bounded_lines(path, usize::MAX, warnings)?;
    Ok(parse_session_index_lines(lines.iter().map(String::as_str)))
}

fn parse_session_index_lines<'a>(
    lines: impl IntoIterator<Item = &'a str>,
) -> BTreeMap<String, SessionIndexEntry> {
    let mut result = BTreeMap::<String, SessionIndexEntry>::new();
    for line in lines {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(id) = string_alias(&value, &["id", "thread_id", "session_id"]) else {
            continue;
        };
        let entry = result.entry(id.to_string()).or_insert(SessionIndexEntry {
            title: None,
            updated_at: None,
        });
        if let Some(title) = string_alias(&value, &["thread_name", "title", "name"])
            .map(normalize_summary)
            .map(|title| truncate_chars(&title, MAX_SUMMARY_CHARS))
            .filter(|title| !title.is_empty())
        {
            entry.title = Some(title);
        }
        if let Some(updated) = value_alias(&value, &["updated_at", "updatedAt", "last_updated_at"])
            .and_then(parse_timestamp_value)
        {
            entry.updated_at = Some(updated);
        }
    }
    result
}

fn parse_rollout_lines<'a>(
    lines: impl IntoIterator<Item = &'a str>,
    fallback_time: u64,
) -> Result<ParsedRollout, String> {
    let mut thread_id = None::<String>;
    let mut cwd = None::<PathBuf>;
    let mut started_at = None::<u64>;
    let mut branch = None::<String>;
    let mut recorded_sha = None::<String>;
    let mut summary = None::<String>;

    for line in lines {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let payload = value.get("payload").unwrap_or(&value);
        let event_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let payload_type = payload
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if event_type == "session_meta" || payload_type == "session_meta" {
            if thread_id.is_none() {
                thread_id = string_alias(payload, &["id", "thread_id", "session_id"])
                    .map(ToOwned::to_owned);
            }
            if cwd.is_none() {
                cwd = string_alias(payload, &["cwd", "working_directory", "project_path"])
                    .map(PathBuf::from);
            }
            if started_at.is_none() {
                started_at = value_alias(&value, &["timestamp", "created_at", "started_at"])
                    .or_else(|| value_alias(payload, &["timestamp", "created_at", "started_at"]))
                    .and_then(parse_timestamp_value);
            }
            let git = payload.get("git").unwrap_or(payload);
            if branch.is_none() {
                branch = string_alias(git, &["branch", "git_branch"])
                    .or_else(|| string_alias(payload, &["git_branch", "branch"]))
                    .map(ToOwned::to_owned);
            }
            if recorded_sha.is_none() {
                recorded_sha = string_alias(git, &["commit_hash", "commit", "sha", "git_commit"])
                    .or_else(|| string_alias(payload, &["git_commit", "commit_hash"]))
                    .map(ToOwned::to_owned);
            }
        }
        if summary.is_none() {
            summary = extract_user_message(&value)
                .map(|value| truncate_chars(&normalize_summary(&value), MAX_SUMMARY_CHARS))
                .filter(|value| !value.is_empty());
        }
    }
    let thread_id = thread_id.ok_or_else(|| "rollout has no session id metadata".to_string())?;
    let cwd = cwd.ok_or_else(|| "rollout has no cwd metadata".to_string())?;
    let summary = summary.unwrap_or_default();
    let started_at = started_at.unwrap_or(fallback_time);
    Ok(ParsedRollout {
        thread: CodexThreadSummary {
            thread_id,
            title: summary.clone(),
            summary,
            started_at,
            ended_at: fallback_time.max(started_at),
            branch,
            recorded_sha,
            is_active: false,
        },
        cwd,
    })
}

fn extract_user_message(value: &Value) -> Option<String> {
    let payload = value.get("payload").unwrap_or(value);
    if payload.get("role").and_then(Value::as_str) != Some("user") {
        return None;
    }
    if let Some(text) = payload.get("text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    if let Some(content) = payload.get("content") {
        return extract_text(content);
    }
    payload.get("message").and_then(extract_text)
}

fn extract_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let parts = items.iter().filter_map(extract_text).collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join(" "))
        }
        Value::Object(object) => object
            .get("text")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| object.get("content").and_then(extract_text)),
        _ => None,
    }
}

fn read_bounded_lines(
    path: &Path,
    max_lines: usize,
    warnings: &mut Vec<String>,
) -> Result<Vec<String>, String> {
    let file = File::open(path).map_err(|error| format!("open '{}': {error}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut lines = Vec::new();
    let mut bytes = Vec::new();
    let mut processed = 0usize;
    while processed < max_lines {
        bytes.clear();
        let read = reader
            .read_until(b'\n', &mut bytes)
            .map_err(|error| format!("read '{}': {error}", path.display()))?;
        if read == 0 {
            break;
        }
        processed += 1;
        if bytes.len() > MAX_LINE_BYTES {
            warnings.push(format!(
                "Ignored oversized JSONL line in '{}'",
                path.display()
            ));
            continue;
        }
        while bytes
            .last()
            .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
        {
            bytes.pop();
        }
        if bytes.is_empty() {
            continue;
        }
        match String::from_utf8(bytes.clone()) {
            Ok(line) if serde_json::from_str::<Value>(&line).is_ok() => lines.push(line),
            Ok(_) => warnings.push(format!(
                "Ignored malformed JSONL line in '{}'",
                path.display()
            )),
            Err(_) => warnings.push(format!(
                "Ignored non-UTF-8 JSONL line in '{}'",
                path.display()
            )),
        }
    }
    let has_more = reader.fill_buf().is_ok_and(|buffer| !buffer.is_empty());
    if max_lines != usize::MAX && processed == max_lines && has_more {
        warnings.push(format!(
            "Stopped reading '{}' after {max_lines} JSONL lines",
            path.display()
        ));
    }
    Ok(lines)
}

#[cfg(test)]
fn discover_git_history(
    project_root: &Path,
    requested_branch: Option<&str>,
    before_commit: Option<&str>,
    limit: usize,
) -> Result<Option<GitDiscovery>, String> {
    discover_git_history_with_recorded_branches(
        project_root,
        requested_branch,
        before_commit,
        limit,
        &BTreeSet::new(),
    )
}

fn discover_git_history_with_recorded_branches(
    project_root: &Path,
    requested_branch: Option<&str>,
    before_commit: Option<&str>,
    limit: usize,
    recorded_branches: &BTreeSet<String>,
) -> Result<Option<GitDiscovery>, String> {
    let probe = git_output(project_root, &["rev-parse", "--is-inside-work-tree"])?;
    if !probe.status.success() || String::from_utf8_lossy(&probe.stdout).trim() != "true" {
        return Ok(None);
    }
    let current = git_output(
        project_root,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    )
    .ok()
    .filter(|output| output.status.success())
    .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
    .filter(|value| !value.is_empty());
    let output = git_output(
        project_root,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads/"],
    )?;
    if !output.status.success() {
        return Err(git_error("list branches", &output));
    }
    let available_names = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>();
    let mut names = available_names.clone();
    if let Some(current) = &current {
        names.insert(current.clone());
    }
    names.extend(recorded_branches.iter().cloned());
    let selected = if let Some(requested) = requested_branch {
        if !names.contains(requested) {
            return Err(
                "branch is not one of the repository's enumerated local branches".to_string(),
            );
        }
        requested.to_string()
    } else if let Some(current) = &current {
        current.clone()
    } else {
        names
            .iter()
            .next()
            .cloned()
            .unwrap_or_else(|| "HEAD".to_string())
    };
    let branches = names
        .into_iter()
        .map(|name| GitBranchSummary {
            is_current: current.as_deref() == Some(name.as_str()),
            available: available_names.contains(&name) || current.as_deref() == Some(name.as_str()),
            name,
        })
        .collect::<Vec<_>>();

    let selected_available = branches
        .iter()
        .find(|branch| branch.name == selected)
        .is_some_and(|branch| branch.available);
    let all = if selected_available {
        load_first_parent_commits(project_root, &selected, MAX_GIT_COMMITS + 1)?
    } else {
        Vec::new()
    };
    let start = if let Some(cursor) = before_commit {
        validate_sha(cursor)?;
        all.iter()
            .position(|commit| commit.sha.eq_ignore_ascii_case(cursor))
            .map(|position| position + 1)
            .ok_or_else(|| {
                "commit cursor is not on the selected branch's first-parent history".to_string()
            })?
    } else {
        0
    };
    let limit = limit.clamp(1, MAX_COMMIT_LIMIT);
    let commits = all
        .iter()
        .skip(start)
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    let next_cursor = if start + commits.len() < all.len() {
        commits.last().map(|commit| commit.sha.clone())
    } else {
        None
    };
    Ok(Some(GitDiscovery {
        selected_branch: selected,
        selected_available,
        branches,
        commits,
        next_cursor,
    }))
}

fn load_first_parent_commits(
    project_root: &Path,
    branch: &str,
    limit: usize,
) -> Result<Vec<GitCommitSummary>, String> {
    let count = limit.min(MAX_GIT_COMMITS + 1).to_string();
    let branch_ref = if branch == "HEAD" {
        "HEAD".to_string()
    } else {
        format!("refs/heads/{branch}")
    };
    let output = git_output(
        project_root,
        &[
            "log",
            "--first-parent",
            "--date-order",
            "--format=%H%x1f%ct%x1f%s",
            "-n",
            &count,
            &branch_ref,
            "--",
        ],
    )?;
    if !output.status.success() {
        return Err(git_error("read first-parent history", &output));
    }
    let mut commits = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.splitn(3, '\u{1f}');
        let Some(sha) = fields.next() else { continue };
        let Some(time) = fields.next().and_then(|value| value.parse::<u64>().ok()) else {
            continue;
        };
        let subject = fields.next().unwrap_or_default().to_string();
        if validate_sha(sha).is_err() {
            continue;
        }
        commits.push(GitCommitSummary {
            sha: sha.to_ascii_lowercase(),
            short_sha: sha.chars().take(12).collect(),
            committed_at: time,
            subject,
            thread_refs: Vec::new(),
        });
    }
    Ok(commits)
}

fn map_threads_to_commits(
    threads: &[CodexThreadSummary],
    commits: &mut [GitCommitSummary],
    selected_branch: &str,
    recorded_resolves: impl Fn(&str) -> bool,
) -> MappingResult {
    let mut mapped_threads = BTreeSet::new();
    let mut unmapped = Vec::new();
    for thread in threads {
        // A recorded branch is authoritative routing metadata. Rollouts that
        // predate branch capture stay eligible as a best-effort fallback,
        // but a thread from another named branch must never appear on this
        // branch's commit rail.
        if thread
            .branch
            .as_deref()
            .is_some_and(|branch| branch != selected_branch)
        {
            continue;
        }
        let mut matched = Vec::new();
        for (index, commit) in commits.iter().enumerate() {
            if commit.committed_at >= thread.started_at && commit.committed_at <= thread.ended_at {
                matched.push((index, ThreadMatchKind::DuringSession));
            }
        }
        if matched.is_empty() {
            if let Some((index, _)) = commits
                .iter()
                .enumerate()
                .filter(|(_, commit)| {
                    commit.committed_at > thread.ended_at
                        && commit.committed_at.saturating_sub(thread.ended_at)
                            <= AFTER_SESSION_WINDOW_SECS
                })
                .min_by_key(|(_, commit)| commit.committed_at)
            {
                matched.push((index, ThreadMatchKind::AfterSession));
            }
        }
        if matched.is_empty() {
            if let Some(recorded) = thread
                .recorded_sha
                .as_deref()
                .filter(|sha| recorded_resolves(sha))
            {
                if let Some(index) = commits.iter().position(|commit| {
                    commit.sha.eq_ignore_ascii_case(recorded)
                        || commit
                            .sha
                            .to_ascii_lowercase()
                            .starts_with(&recorded.to_ascii_lowercase())
                }) {
                    matched.push((index, ThreadMatchKind::StartedFrom));
                }
            }
        }
        if matched.is_empty() {
            let reason = if commits.is_empty() {
                "The selected branch has no available first-parent commits"
            } else if thread.recorded_sha.is_some() {
                "No commit fell within the session window, its 24-hour follow-up, or its recorded SHA"
            } else {
                "No commit fell within the session window or its 24-hour follow-up, and the session has no recorded SHA"
            };
            unmapped.push(UnmappedThreadReference {
                thread_id: thread.thread_id.clone(),
                reason: reason.to_string(),
            });
            continue;
        }
        mapped_threads.insert(thread.thread_id.clone());
        for (index, match_kind) in matched {
            commits[index].thread_refs.push(CommitThreadReference {
                thread_id: thread.thread_id.clone(),
                match_kind,
            });
        }
    }
    for commit in commits.iter_mut() {
        commit
            .thread_refs
            .sort_by(|left, right| left.thread_id.cmp(&right.thread_id));
    }
    let reference_count = commits.iter().map(|commit| commit.thread_refs.len()).sum();
    MappingResult {
        unmapped,
        unique_thread_count: mapped_threads.len(),
        reference_count,
    }
}

fn recorded_commit_resolves_on_branch(root: &Path, recorded: &str, branch: &str) -> bool {
    if validate_sha(recorded).is_err() {
        return false;
    }
    let branch_ref = if branch == "HEAD" {
        "HEAD".to_string()
    } else {
        format!("refs/heads/{branch}")
    };
    git_output(
        root,
        &["merge-base", "--is-ancestor", recorded, &branch_ref],
    )
    .is_ok_and(|output| output.status.success())
}

fn git_output(root: &Path, args: &[&str]) -> Result<Output, String> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|error| format!("run Git for '{}': {error}", root.display()))
}

fn git_error(action: &str, output: &Output) -> String {
    let detail = String::from_utf8_lossy(&output.stderr);
    format!("Git could not {action}: {}", detail.trim())
}

fn launch_terminal_resume(thread_id: &str, project_root: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let codex_cli = resolve_codex_cli()?;
        let shell = terminal_resume_command(&codex_cli, thread_id, project_root)?;
        let script = format!(
            "tell application \"Terminal\" to do script \"{}\"",
            apple_script_string(&shell)
        );
        let output = Command::new("/usr/bin/osascript")
            .args([
                "-e",
                &script,
                "-e",
                "tell application \"Terminal\" to activate",
            ])
            .output()
            .map_err(|error| format!("open Terminal: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "Terminal could not resume the Codex thread: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (thread_id, project_root);
        Err("Open in Terminal is currently supported only on macOS".to_string())
    }
}

#[cfg(target_os = "macos")]
fn resolve_codex_cli() -> Result<PathBuf, String> {
    // GUI applications inherit launchd's sparse PATH. A login shell sees the
    // same Homebrew/npm/user configuration as the Terminal window we open.
    let output = Command::new("/bin/zsh")
        .args(["-lc", "command -v codex"])
        .output()
        .map_err(|error| format!("check for Codex CLI in the login shell: {error}"))?;
    if !output.status.success() {
        return Err(
            "Codex CLI is unavailable in the login shell; install it or use Open in Codex"
                .to_string(),
        );
    }
    let value = String::from_utf8_lossy(&output.stdout);
    let path = PathBuf::from(value.lines().next().unwrap_or_default().trim());
    if !path.is_absolute() {
        return Err("Codex CLI did not resolve to an absolute executable path".to_string());
    }
    let resolved = fs::canonicalize(&path)
        .map_err(|error| format!("resolve Codex CLI '{}': {error}", path.display()))?;
    if !resolved.is_file() {
        return Err(format!(
            "Codex CLI '{}' is not an executable file",
            resolved.display()
        ));
    }
    Ok(resolved)
}

fn terminal_resume_command(
    codex_cli: &Path,
    thread_id: &str,
    project_root: &Path,
) -> Result<String, String> {
    validate_thread_uuid(thread_id)?;
    if !codex_cli.is_absolute() || !project_root.is_absolute() {
        return Err("Codex CLI and project root must be absolute".to_string());
    }
    Ok(format!(
        "{} resume {} -C {}",
        shell_quote(&codex_cli.to_string_lossy()),
        shell_quote(thread_id),
        shell_quote(&project_root.to_string_lossy())
    ))
}

fn validate_thread_uuid(value: &str) -> Result<(), String> {
    let bytes = value.as_bytes();
    let valid = bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        });
    valid
        .then_some(())
        .ok_or_else(|| "Codex thread id must be a UUID".to_string())
}

fn validate_sha(value: &str) -> Result<(), String> {
    ((value.len() == 40 || value.len() == 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(())
        .ok_or_else(|| "commit cursor must be a full hexadecimal Git object id".to_string())
}

fn cwd_belongs_to_project(cwd: &Path, project_root: &Path) -> bool {
    if !cwd.is_absolute()
        || cwd
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return false;
    }
    let resolved = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    resolved == project_root || resolved.starts_with(project_root)
}

fn value_alias<'a>(value: &'a Value, names: &[&str]) -> Option<&'a Value> {
    names.iter().find_map(|name| value.get(*name))
}

fn string_alias<'a>(value: &'a Value, names: &[&str]) -> Option<&'a str> {
    value_alias(value, names).and_then(Value::as_str)
}

fn parse_timestamp_value(value: &Value) -> Option<u64> {
    if let Some(number) = value.as_u64() {
        return Some(if number > 10_000_000_000 {
            number / 1000
        } else {
            number
        });
    }
    if let Some(number) = value.as_i64() {
        return (number >= 0).then_some(number as u64).map(|value| {
            if value > 10_000_000_000 {
                value / 1000
            } else {
                value
            }
        });
    }
    value.as_str().and_then(parse_rfc3339)
}

fn parse_rfc3339(value: &str) -> Option<u64> {
    let (date, rest) = value.split_once('T')?;
    let mut date_parts = date.split('-');
    let year = date_parts.next()?.parse::<i64>().ok()?;
    let month = date_parts.next()?.parse::<u32>().ok()?;
    let day = date_parts.next()?.parse::<u32>().ok()?;
    let zone_index = rest
        .char_indices()
        .skip(1)
        .find(|(_, character)| matches!(character, 'Z' | '+' | '-'))
        .map(|(index, _)| index)?;
    let (clock, zone) = rest.split_at(zone_index);
    let mut clock_parts = clock.split(':');
    let hour = clock_parts.next()?.parse::<u32>().ok()?;
    let minute = clock_parts.next()?.parse::<u32>().ok()?;
    let second = clock_parts.next()?.split('.').next()?.parse::<u32>().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let offset = if zone == "Z" {
        0i64
    } else {
        let sign = if zone.starts_with('+') {
            1
        } else if zone.starts_with('-') {
            -1
        } else {
            return None;
        };
        let mut parts = zone[1..].split(':');
        let hours = parts.next()?.parse::<i64>().ok()?;
        let minutes = parts.next()?.parse::<i64>().ok()?;
        if hours > 23 || minutes > 59 {
            return None;
        }
        sign * (hours * 3600 + minutes * 60)
    };
    let days = days_from_civil(year, month, day);
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second))?
        .checked_sub(offset)?;
    (seconds >= 0).then_some(seconds as u64)
}

// Howard Hinnant's proleptic Gregorian conversion, offset to Unix epoch.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let adjusted_year = year - i64::from(month <= 2);
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = adjusted_year - era * 400;
    let month_prime = i64::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn normalize_summary(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut result = value
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    result.push('…');
    result
}

fn short_id(value: &str) -> &str {
    value.get(..8).unwrap_or(value)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(target_os = "macos")]
fn apple_script_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn modified_secs(metadata: &fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn session_mtime_is_active(now: u64, modified: u64) -> bool {
    modified <= now.saturating_add(ACTIVE_WINDOW_SECS)
        && now.saturating_sub(modified) <= ACTIVE_WINDOW_SECS
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    fn commit(sha: &str, time: u64) -> GitCommitSummary {
        GitCommitSummary {
            sha: sha.repeat(40 / sha.len()),
            short_sha: sha.repeat(12 / sha.len()),
            committed_at: time,
            subject: format!("commit {time}"),
            thread_refs: Vec::new(),
        }
    }

    fn thread(id: &str, start: u64, end: u64) -> CodexThreadSummary {
        CodexThreadSummary {
            thread_id: id.to_string(),
            title: id.to_string(),
            summary: String::new(),
            started_at: start,
            ended_at: end,
            branch: Some("main".to_string()),
            recorded_sha: None,
            is_active: false,
        }
    }

    #[test]
    fn mapping_attaches_all_inclusive_commits_and_counts_unique_threads() {
        let mut commits = vec![commit("a", 100), commit("b", 150), commit("c", 200)];
        let threads = vec![thread("one", 100, 200), thread("two", 150, 150)];
        let result = map_threads_to_commits(&threads, &mut commits, "main", |_| false);

        assert_eq!(result.unmapped, Vec::<UnmappedThreadReference>::new());
        assert_eq!(result.unique_thread_count, 2);
        assert_eq!(result.reference_count, 4);
        assert_eq!(
            commits[0].thread_refs[0].match_kind,
            ThreadMatchKind::DuringSession
        );
        assert_eq!(commits[1].thread_refs.len(), 2);
        assert_eq!(
            commits[2].thread_refs[0].match_kind,
            ThreadMatchKind::DuringSession
        );
    }

    #[test]
    fn mapping_uses_first_subsequent_commit_only_within_24_hours() {
        let mut commits = vec![commit("a", 86_500), commit("b", 86_501)];
        let near = thread("near", 1, 100);
        let far = thread("far", 0, 99);
        let result = map_threads_to_commits(&[near, far], &mut commits, "main", |_| false);

        assert_eq!(commits[0].thread_refs.len(), 1);
        assert_eq!(commits[0].thread_refs[0].thread_id, "near");
        assert_eq!(
            commits[0].thread_refs[0].match_kind,
            ThreadMatchKind::AfterSession
        );
        assert_eq!(result.unmapped[0].thread_id, "far");
    }

    #[test]
    fn mapping_falls_back_to_a_recorded_sha_reachable_on_branch() {
        let mut base = thread("recorded", 1, 2);
        base.recorded_sha = Some("a".repeat(40));
        base.branch = Some("main".to_string());
        let mut commits = vec![commit("a", 200_000)];
        let result =
            map_threads_to_commits(&[base], &mut commits, "main", |sha| sha == "a".repeat(40));

        assert!(result.unmapped.is_empty());
        assert_eq!(
            commits[0].thread_refs[0].match_kind,
            ThreadMatchKind::StartedFrom
        );
    }

    #[test]
    fn mapping_never_attaches_a_named_thread_to_another_branch() {
        let mut other = thread("feature-thread", 1, 300);
        other.branch = Some("feature/a".to_string());
        let mut unknown = thread("legacy-thread", 1, 300);
        unknown.branch = None;
        let mut commits = vec![commit("a", 200)];

        let result = map_threads_to_commits(&[other, unknown], &mut commits, "main", |_| false);

        assert_eq!(commits[0].thread_refs.len(), 1);
        assert_eq!(commits[0].thread_refs[0].thread_id, "legacy-thread");
        assert_eq!(result.unique_thread_count, 1);
        assert!(result.unmapped.is_empty());
    }

    #[test]
    fn rollout_parsing_accepts_metadata_aliases_and_first_user_message() {
        let lines = vec![
            json!({"timestamp":"2026-07-18T16:20:00Z","type":"session_meta","payload":{"id":"019f742a-a206-7932-876c-9db8d8ce575a","cwd":"/tmp/project","git":{"branch":"feature/x","commit_hash":"abc"}}}).to_string(),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"  Build   the history page please  "}]}}).to_string(),
        ];
        let parsed = parse_rollout_lines(lines.iter().map(String::as_str), 123).unwrap();

        assert_eq!(
            parsed.thread.thread_id,
            "019f742a-a206-7932-876c-9db8d8ce575a"
        );
        assert_eq!(parsed.cwd, Path::new("/tmp/project"));
        assert_eq!(parsed.thread.branch.as_deref(), Some("feature/x"));
        assert_eq!(parsed.thread.recorded_sha.as_deref(), Some("abc"));
        assert_eq!(parsed.thread.summary, "Build the history page please");
        assert_eq!(parsed.thread.started_at, 1_784_391_600);
    }

    #[test]
    fn rollout_uses_mtime_fallback_and_truncates_unicode_summary() {
        let long_message = "界".repeat(MAX_SUMMARY_CHARS + 20);
        let lines = vec![
            json!({"type":"session_meta","payload":{"session_id":"019f742a-a206-7932-876c-9db8d8ce575a","working_directory":"/tmp/project"}}).to_string(),
            json!({"payload":{"role":"user","content":long_message}}).to_string(),
        ];
        let parsed = parse_rollout_lines(lines.iter().map(String::as_str), 456).unwrap();

        assert_eq!(parsed.thread.started_at, 456);
        assert_eq!(parsed.thread.ended_at, 456);
        assert_eq!(parsed.thread.summary.chars().count(), MAX_SUMMARY_CHARS);
        assert!(parsed.thread.summary.ends_with('…'));
    }

    #[test]
    fn index_title_and_time_override_rollout_fallbacks() {
        let index = parse_session_index_lines([
            r#"{"id":"019f742a-a206-7932-876c-9db8d8ce575a","thread_name":"First","updated_at":100}"#,
            r#"{"thread_id":"019f742a-a206-7932-876c-9db8d8ce575a","title":"Renamed","updatedAt":"2026-07-18T16:20:00Z"}"#,
        ]);
        let item = index.get("019f742a-a206-7932-876c-9db8d8ce575a").unwrap();
        assert_eq!(item.title.as_deref(), Some("Renamed"));
        assert_eq!(item.updated_at, Some(1_784_391_600));

        let long_title = "界".repeat(MAX_SUMMARY_CHARS + 20);
        let line = json!({"id":"thread-long","title":long_title}).to_string();
        let truncated = parse_session_index_lines([line.as_str()]);
        let title = truncated["thread-long"].title.as_deref().unwrap();
        assert_eq!(title.chars().count(), MAX_SUMMARY_CHARS);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn oversized_or_malformed_lines_become_partial_warnings() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session_index.jsonl");
        fs::write(
            &path,
            format!("not json\n{{\"id\":\"{}\"}}\n", "x".repeat(MAX_LINE_BYTES)),
        )
        .unwrap();
        let mut warnings = Vec::new();
        let index = read_session_index(&path, &mut warnings).unwrap();
        assert!(index.is_empty());
        assert_eq!(warnings.len(), 2);
    }

    #[test]
    fn ownership_uses_canonical_path_boundaries() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let child = project.join("packages/app");
        let sibling = temp.path().join("project-copy");
        fs::create_dir_all(&child).unwrap();
        fs::create_dir_all(&sibling).unwrap();
        let canonical = fs::canonicalize(&project).unwrap();

        assert!(cwd_belongs_to_project(&child, &canonical));
        assert!(!cwd_belongs_to_project(&sibling, &canonical));
        assert!(!cwd_belongs_to_project(Path::new("../project"), &canonical));
    }

    #[test]
    fn git_history_is_first_parent_and_cursor_is_validated() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        git(root, &["init", "-b", "main"]);
        git(root, &["config", "user.name", "Test"]);
        git(root, &["config", "user.email", "test@example.com"]);
        for (name, value) in [("first", "1"), ("second", "2"), ("third", "3")] {
            fs::write(root.join("file"), value).unwrap();
            git(root, &["add", "file"]);
            git(root, &["commit", "-m", name]);
        }
        let discovery = discover_git_history(root, None, None, 2).unwrap().unwrap();
        assert_eq!(discovery.commits.len(), 2);
        assert!(discovery.next_cursor.is_some());
        let next = discover_git_history(root, Some("main"), discovery.next_cursor.as_deref(), 2)
            .unwrap()
            .unwrap();
        assert_eq!(next.commits.len(), 1);
        assert!(discover_git_history(root, Some("main; touch /tmp/nope"), None, 2).is_err());
        assert!(discover_git_history(root, Some("main"), Some("not-a-sha"), 2).is_err());
    }

    #[test]
    fn recorded_deleted_branch_remains_selectable_but_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        git(root, &["init", "-b", "main"]);
        git(root, &["config", "user.name", "Test"]);
        git(root, &["config", "user.email", "test@example.com"]);
        fs::write(root.join("file"), "one").unwrap();
        git(root, &["add", "file"]);
        git(root, &["commit", "-m", "first"]);
        let recorded = BTreeSet::from(["deleted/history".to_string()]);

        let discovery = discover_git_history_with_recorded_branches(
            root,
            Some("deleted/history"),
            None,
            50,
            &recorded,
        )
        .unwrap()
        .unwrap();
        assert!(!discovery.selected_available);
        assert!(discovery.commits.is_empty());
        assert_eq!(
            discovery
                .branches
                .iter()
                .find(|branch| branch.name == "deleted/history")
                .map(|branch| branch.available),
            Some(false)
        );
    }

    #[test]
    fn active_session_detection_rejects_large_clock_skew() {
        assert!(session_mtime_is_active(1_000, 900));
        assert!(session_mtime_is_active(1_000, 1_100));
        assert!(!session_mtime_is_active(1_000, 699));
        assert!(!session_mtime_is_active(1_000, 1_301));
    }

    #[test]
    fn terminal_command_is_shell_safe_and_uuid_is_strict() {
        let id = "019f742a-a206-7932-876c-9db8d8ce575a";
        let command = terminal_resume_command(
            Path::new("/opt/homebrew/bin/codex"),
            id,
            Path::new("/tmp/client's project"),
        )
        .unwrap();
        assert_eq!(
            command,
            "'/opt/homebrew/bin/codex' resume '019f742a-a206-7932-876c-9db8d8ce575a' -C '/tmp/client'\"'\"'s project'"
        );
        assert!(terminal_resume_command(
            Path::new("/opt/homebrew/bin/codex"),
            "bad; open -a Calculator",
            Path::new("/tmp/p"),
        )
        .is_err());
        assert!(terminal_resume_command(Path::new("codex"), id, Path::new("/tmp/p")).is_err());
    }

    fn git(root: &Path, args: &[&str]) {
        let result = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
