# Plan: Manual project-path mapping after pull

Status: proposed (2026-07-15). The Codex half shipped 2026-07-16 via
`PLAN_CODEX_MANUAL_PROJECT_PATH_PICKING.md` (shared schema in
`project_paths.rs`, `map_project_path`/`remove_project_path_mapping`/
`list_project_path_mappings`, Finish-setup picker, Settings editor); the
Claude projection/materialization (§6) remains unimplemented.

## 1. Goal

Let a user restore the same conversation/task on a machine where the project
has a different absolute path:

```text
Machine A                         Machine B
/A/home                          /B/home
session "hello"  -- push/pull -> same session "hello"
```

After Pull, Agent Sync detects source-machine project paths that do not exist
locally and offers an explicit mapping:

```text
Project path needs attention

Claude Code   /A/home   ->   [ /B/home                 ] [Choose folder] [Map]
```

The mapping is local to machine B. It must not change the cloud profile's
portable identity, fork a session, or rewrite transcript contents.

## 2. Verified starting point

The repository already has most of the discovery and UI plumbing:

- `readiness.rs::project_path_issues` reads the first transcript `cwd` under
  each Claude `projects/<encoded-cwd>/` directory and emits a `paths` issue
  with action `attach_project` when that cwd is missing.
- `FinishSetup.tsx` already groups `paths` issues under `Projects & paths`,
  but `attach_project` has no action UI and currently falls through to
  `Dismiss`.
- `codex_sidebar.rs::plan_apply` already detects source-machine Codex project
  paths that have neither an exact local path nor a Git-origin match. It
  reports them in `SidebarApplyPlan.unmatched` but cannot accept a user-chosen
  local path.
- `Roots::abs` and `Roots::rel` are the existing logical-to-physical path
  waist used by every push, pull, baseline, and file-status operation.
- `~/.agent-sync/local-state.json` establishes the pattern for machine-local,
  structurally unsyncable state.
- Ordinary deletions do not propagate. Therefore a one-time post-pull rename
  without a persistent logical/physical projection would be incorrect: the
  next push would publish a second project key and later pulls could restore
  the old one.

## 3. Decisions

**D1 — Mapping is explicit.** Agent Sync may suggest a target using an exact
Git-origin match, but it never invents or applies a path without the user
choosing and confirming it.

**D2 — Mapping is machine-local.** Persist mappings at:

```text
~/.agent-sync/project-path-mappings.json
```

This top-level file is outside every profile remap directory, so `Roots::rel`
cannot place it in a manifest. `/B/home` must never become a cloud-wide
setting for users on machines C or D.

**D3 — Project mappings are a path projection, not a transcript rewrite.**
Claude JSONL, Codex rollouts, session ids, thread ids, and historical `cwd`
fields remain byte-for-byte unchanged.

**D4 — Preserve the cloud logical key.** For Claude, the source bucket name
from machine A stays the manifest key. Machine B projects that logical key
onto Claude's encoded form of `/B/home`. `Roots::rel` performs the inverse on
push, so edits from B update A's existing cloud entry instead of creating a
second B-path entry.

```text
Cloud logical key                         Machine B physical path
.claude/projects/-A-home/<session>.jsonl <->
<claude-root>/projects/-B-home/<session>.jsonl
```

**D5 — One exact project per mapping.** Do not implement prefix replacement
such as `/A -> /B`. Each Claude encoded project directory or Codex project
path gets its own mapping. Exact mappings avoid changing unrelated paths and
make collisions reviewable.

**D6 — One source and one target identity.** Within one local profile, reject
duplicate mappings for the same source key and reject two source keys mapped
to the same provider target. A physical Claude directory cannot invert to two
different cloud logical keys.

**D7 — Applying a mapping is explicit and guarded.** Block Claude directory
materialization while a `claude` process is running. Block Codex desktop-state
apply while ChatGPT/Codex desktop is running, matching the existing sidebar
guard. Detection remains best-effort, and the confirmation copy explains why
the app should be closed.

**D8 — No raw ChatGPT/Codex database edits.** Codex mapping adds the selected
local project to the existing portable sidebar apply and supplies the correct
`-C` resume path. It does not edit `state_5.sqlite` or undocumented task/project
associations. The supported fallback for continuing in the desktop app is:

```bash
codex resume <thread-id> -C /B/home
# then run /app inside the resumed Codex session
```

**D9 — Manual mapping replaces automatic home normalization.** This plan
supersedes the automatic `~` cloud-key normalization proposed in
`PLAN_PORTABLE_PROJECTS.md`. There is no cloud migration or key rewrite.

## 4. Local schema

Add `src-tauri/src/project_paths.rs`, with Tauri-free parsing, validation,
projection, preflight, and apply helpers.

```rust
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
struct ProjectPathMappings {
    schema: u32, // 1
    #[serde(default)]
    mappings: Vec<ProjectPathMapping>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
struct ProjectPathMapping {
    profile: String,      // LocalProfile.id
    provider: String,     // "claude" | "codex"
    source_key: String,   // Claude bucket basename or Codex source path
    source_path: String,  // original transcript/sidebar cwd for display
    target_path: String,  // exact absolute path selected on this machine
}
```

Example:

```json
{
  "schema": 1,
  "mappings": [
    {
      "profile": "team_claude",
      "provider": "claude",
      "source_key": "-Users-hequ-Desktop-project-a",
      "source_path": "/Users/hequ/Desktop/project/a",
      "target_path": "/Users/rika/work/a"
    }
  ]
}
```

Validation:

- maximum 512 mappings and bounded strings;
- `profile` must reference a configured `LocalProfile` of the matching root;
- provider must be exactly `claude` or `codex`;
- `source_key` must be re-derived from local synced data, not trusted from an
  arbitrary frontend payload;
- `target_path` must be absolute, contain no `.`/`..` components or control
  characters, exist, and be a directory;
- preserve the exact selected target spelling instead of silently
  canonicalizing symlinks, because the agent's cwd spelling is part of its
  project identity;
- duplicate source or target identities fail with an actionable message;
- load is bounded and strict; save uses a temp file plus atomic rename.

The file contains no credentials, transcript content, or cloud secrets. It is
functional local configuration, so keep it separate from readiness dismissal
memory in `local-state.json`.

## 5. Structured readiness model

Extend `SetupIssue` in Rust and TypeScript with an optional structured payload:

```rust
struct ProjectPathCandidate {
    provider: String,
    source_key: String,
    source_path: String,
    mapped_path: Option<String>,
    affected_sessions: u64,
}

struct SetupIssue {
    // existing fields unchanged
    #[serde(skip_serializing_if = "Option::is_none")]
    project_path: Option<ProjectPathCandidate>,
}
```

Candidate derivation:

### Claude

For each direct child under `<claude-root>/projects/`:

1. Read `cwd` from the first bounded JSONL line, as today; never decode a
   directory name because Claude's dash encoding is ambiguous.
2. Use the directory basename as `source_key`.
3. Count direct `*.jsonl` session files for display.
4. If the original cwd exists, it is ready without a mapping.
5. If a valid mapping exists and its target directory exists, it is ready.
6. Otherwise emit one actionable `attach_project` issue carrying the
   structured candidate.

### Codex / ChatGPT desktop

Replace the single summarized `unmatched` string with one candidate per
`CodexSidebarLock.projects[]` entry that cannot match by:

1. exact existing path;
2. a valid saved manual mapping;
3. normalized Git origin against an already-saved local project.

Use the source absolute path as both `source_key` and `source_path`. Count
rollouts whose `session_meta.cwd` equals that source path for display. Preserve
the existing aggregate sidebar issue for titles/preferences; do not conflate
it with project-path rows.

Readiness remains read-only. It may load mappings but never creates, edits, or
deletes them.

## 6. Claude projection and materialization

### 6.1 Encoding helper

Implement one tested `encode_claude_project_path(path)` helper matching the
observed Claude directory rule: replace each non-ASCII-alphanumeric character
with `-`. Scope v1 to absolute POSIX paths used by the current macOS/Linux app.

Do not decode source keys. `source_path` comes from transcript content, and
`source_key` comes from the actual directory name.

### 6.2 `Roots` integration

Add the profile's validated Claude mappings to `Roots`:

```rust
struct Roots {
    // existing fields
    claude_project_mappings: Vec<ClaudeProjectProjection>,
}
```

`Roots::for_profile` loads and filters the machine-local mapping file.
Test constructors accept mappings explicitly and default to none.

Projection is limited to the first component below `.claude/projects/`:

- `abs(".claude/projects/<source_key>/tail")` returns
  `<claude-root>/projects/<encode(target_path)>/tail` when mapped.
- `rel(<claude-root>/projects/<encode(target_path)>/tail)` returns
  `.claude/projects/<source_key>/tail`.
- `rel(<claude-root>/projects/<source_key>/...)` returns `None` after that
  source is mapped, preventing an old leftover directory from being pushed as
  a second physical representation.
- All non-project and unmapped paths preserve current behavior.

Every path consumer already goes through `Roots`, so this covers collection,
pull apply, baseline checks, file status, backups, conflict copies, and push.
Add a regression test for each of those paths rather than adding a second
mapping layer inside only `do_pull_link`.

### 6.3 First apply after an existing pull

When the user maps a source bucket already materialized under its A-path name:

1. Refuse if `claude` appears to be running.
2. Re-derive the candidate and verify that the source bucket is a direct child
   of this profile's `projects` directory.
3. Calculate the target bucket using `encode_claude_project_path`.
4. Preflight the entire source/target tree without following symlinks.
5. If the target bucket is absent, rename source to target atomically, then
   atomically save the mapping; roll the rename back if the save fails.
6. If the target bucket exists, allow only source-only files and byte-identical
   overlaps. Back up every affected local file using the existing app-data
   backup machinery, merge source-only files, remove identical duplicates,
   then save the mapping.
7. If any same-relative-path file differs, abort before mutation and show the
   colliding paths. Do not manufacture another Claude session file.
8. Refresh readiness and file status after success.

Editing an existing mapping runs the same preflight from the old projected
bucket to the new one. Removing a mapping first projects the directory back to
the source bucket using the same guarded operation; never delete only the JSON
record while files still depend on it.

## 7. Codex and ChatGPT desktop behavior

Pass a mapping resolver into `codex_sidebar::plan_apply`:

```rust
resolve_project(source_path) -> Option<target_path>
```

Resolution order becomes:

1. source path already saved locally;
2. source path exists and can be added as-is;
3. saved manual mapping whose target exists -> add the **target** path;
4. normalized Git-origin match against an existing local project;
5. unmatched -> emit a manual mapping candidate.

`apply_plan_to_state` continues to write only the existing whitelisted sidebar
keys. It adds `/B/home`, never `/A/home`, and preserves additive behavior.

Mapping does not rewrite Codex rollouts. For every affected thread id, the UI
shows a selectable continuation command:

```text
codex resume <thread-id> -C /B/home
```

It also explains that `/app` continues that resumed session in ChatGPT desktop.
Do not launch an interactive shell, mutate `state_5.sqlite`, or claim that the
copied desktop task has been natively rebound. Native remote Handoff remains a
separate supported workflow.

References:

- <https://learn.chatgpt.com/docs/projects>
- <https://learn.chatgpt.com/docs/remote-connections#hand-off-a-task-between-hosts>
- <https://learn.chatgpt.com/docs/developer-commands#built-in-slash-commands>

## 8. Backend commands

Add and register:

```rust
#[tauri::command]
async fn map_project_path(
    app: AppHandle,
    profile: String,
    provider: String,
    source_key: String,
    target_path: String,
) -> Result<ProjectPathApplyReport, String>;

#[tauri::command]
async fn remove_project_path_mapping(
    app: AppHandle,
    profile: String,
    provider: String,
    source_key: String,
) -> Result<ProjectPathApplyReport, String>;

#[tauri::command]
async fn list_project_path_mappings(
    app: AppHandle,
) -> Result<Vec<ProjectPathMapping>, String>;
```

`map_project_path` must re-derive the source candidate from the selected local
profile. The frontend is allowed to identify a candidate, not invent one.

`ProjectPathApplyReport` contains only display-safe facts:

```rust
struct ProjectPathApplyReport {
    provider: String,
    source_path: String,
    target_path: String,
    affected_sessions: u64,
    moved_files: u64,
    reused_files: u64,
    sidebar_changes: u64,
    resume_commands: Vec<String>,
}
```

Log the operation without transcript contents. Never include environment
values, tokens, or JSONL lines.

## 9. UI

### 9.1 Finish setup

In `FinishSetup.tsx`, render `attach_project` issues as compact mapping rows:

```text
Projects & paths · 2

Claude Code
/A/home
[Choose folder]  /B/home                                      [Map]
3 sessions

Codex / ChatGPT
/A/repo
[Choose folder]  /B/repo                                      [Map]
2 tasks
```

- Use `open({ directory: true, multiple: false })` from the already-installed
  Tauri dialog plugin.
- The frontend holds the selected folder only until the user clicks `Map`.
- Disable `Map` until a folder is selected and while an action is running.
- Display backend validation/collision errors inline on the row.
- After success, replace the row with `Mapped to /B/...` and refresh readiness.
- Keep `Dismiss`, but place it behind a small secondary action; dismissal does
  not create a mapping.
- For Codex results, show affected `codex resume ... -C ...` commands in a
  selectable monospace block and the `/app` follow-up.

### 9.2 Settings

Add a simple `Project paths` section below Appearance:

```text
Project paths
Paths used for restored sessions on this Mac.

Claude   /A/home -> /B/home                         [Change] [Remove]
```

This is an editor for existing machine-local mappings, not a second discovery
surface. New mappings originate from Finish setup after a pull.

## 10. File-level implementation order

1. **`src-tauri/src/project_paths.rs`**
   - schema, bounded load/save, validation;
   - Claude encoder and bidirectional projection;
   - candidate/migration preflight and apply report;
   - unit tests.
2. **`src-tauri/src/lib.rs`**
   - register the module and three commands;
   - add mappings to `Roots` construction and `abs`/`rel`;
   - reuse process guards and app-data backups;
   - refresh readiness after pull remains unchanged because scanning already
     runs after every pull.
3. **`src-tauri/src/readiness.rs`**
   - add structured project candidates;
   - make Claude checks mapping-aware;
   - keep the scanner read-only and deterministic.
4. **`src-tauri/src/codex_sidebar.rs`**
   - accept a mapping resolver in plan/apply;
   - return unmatched projects individually;
   - map source paths to local target paths before additive apply.
5. **`src/types.ts`**
   - mirror candidate, mapping, and apply-report response types.
6. **`src/components/FinishSetup.tsx` and `src/App.tsx`**
   - folder chooser, Map action, inline result/error, resume-command display.
7. **Settings component (`SyncPanel.tsx`) and `src/App.css`**
   - list/change/remove existing mappings using the shared restrained button
     system; no new bespoke button style.
8. **Docs/tests**
   - mark `PLAN_PORTABLE_PROJECTS.md` superseded;
   - document the unsynced mapping file in `AGENT_SYNC_FILE_SETS.md`;
   - add integration scenarios and update `src-tauri/src/sync_tests/README.md`.

## 11. Tests

### Unit tests

- mapping schema round-trip, unknown schema, oversized file/entries/strings;
- provider/profile mismatch and arbitrary frontend `source_key` rejection;
- absolute target validation, missing target, file target, `.`/`..`, control
  characters;
- duplicate source and duplicate target rejection;
- Claude encoding for `/`, dots, spaces, repeated punctuation, and Unicode;
- `Roots::abs`/`rel` mapped round-trip and unmapped behavior;
- old source bucket becomes invisible to `rel` after mapping;
- mapping file itself can never map to a cloud-relative path;
- readiness: missing, valid mapped, stale mapped target, and affected counts;
- Claude migration: absent target, byte-identical merge, divergent collision,
  save failure rollback, change mapping, remove mapping;
- Codex sidebar: exact, mapped, Git-origin, and unmatched resolution order;
- mapped Codex order adds the target path and never the source path.

### Dual-backend integration tests

Run each core scenario against the S3 stub and local-folder backend:

1. A starts a Claude session under `/A/home`, pushes; B pulls, maps to
   `/B/home`; the same JSONL/session id appears under B's encoded project
   bucket.
2. B appends to that session and pushes; assert the cloud manifest still has
   only A's original logical project key. A pulls and sees B's append.
3. Mapping exists before B pulls; the pull materializes directly under B's
   encoded bucket with no intermediate A bucket.
4. A/B target collision with different bytes aborts with no file or mapping
   change.
5. Mapping file never appears in the cloud manifest or opt-in file list.
6. Codex sidebar lock from A plus B mapping applies `/B/home`, preserves B-only
   sidebar state, and leaves rollouts byte-identical.

### Manual smoke tests

- With a disposable `CLAUDE_CONFIG_DIR`, create/resume a named session on A,
  sync, map on B, run `claude --resume` from B's target, and verify the original
  session id continues.
- With a disposable `CODEX_HOME`, restore a thread, map it, run
  `codex resume <id> -C <target>`, then `/app`, and verify the same thread opens
  in ChatGPT desktop.
- Quit/relaunch Agent Sync and verify mappings persist and readiness stays
  clear.

## 12. Acceptance criteria

- After Pull, every missing Claude cwd and unmatched Codex sidebar path appears
  as a concrete mapping row with a folder chooser.
- Mapping `/A/home` to `/B/home` preserves the original session/thread id; no
  transcript or rollout content is rewritten.
- Claude can resume from `/B/home`, and subsequent pushes use the original
  cloud logical key with no A/B duplicate-key churn.
- Codex sidebar apply adds `/B/home`; the generated `resume -C` command resumes
  the same thread, and `/app` can continue it in ChatGPT desktop.
- Mapping data remains machine-local and never enters a manifest.
- Conflicting target files fail closed before mutation and identify the paths
  the user must resolve.
- Editing/removing a mapping cannot strand its Claude directory under a path
  that no longer has an inverse projection.
- `npm run build`, `cd src-tauri && cargo test`, and
  `cd src-tauri && cargo check` pass.

## 13. Non-goals

- No automatic `/A` -> `/B` prefix rule.
- No transcript `cwd` rewrite or session fork.
- No cloud-key migration, deletion, or cleanup of historic duplicate project
  keys.
- No direct mutation of Codex runtime SQLite or undocumented ChatGPT task
  associations.
- No automatic repository clone.
- No cross-device syncing of target paths.
- No Windows path encoding in v1; add it only with verified Claude behavior and
  fixtures from Windows.
