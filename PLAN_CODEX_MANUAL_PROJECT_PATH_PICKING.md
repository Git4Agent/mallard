# Plan: Codex manual project-path picking after pull

Status: **IMPLEMENTED** (2026-07-16) — `project_paths.rs` (shared mapping
schema, validation, atomic persistence), the `codex_sidebar.rs` mapping
resolver with structured unmatched projects, `readiness.rs` candidates with
rollout thread grouping, the three commands in `lib.rs`, the Finish-setup
picker rows, and the Settings `Project paths` editor are all in the tree.
Verified: `cargo test --lib` (261 tests, incl. dual-backend
`codex_project_path_mapping_flow`) and `npm run build`.

Deviations from the text below:

- `remove_project_path_mapping` / `list_project_path_mappings` take no
  profile-side apply step and return `()` / the mapping list — Codex removal
  has nothing to apply (sidebar application is additive and never undone).
- The commands take `source_path` (== `source_key` for Codex); the shared
  candidate struct is `readiness::ProjectPathCandidate` (per the main plan's
  field set, plus `git_origin` and `affected_threads`), with `profile`
  carried by the enclosing `SetupIssue` rather than duplicated.
- A stale mapped target falls through to the Git-origin match before
  re-raising (§4's ordered-resolution semantics); it never resolves the
  source and never adds it.
- The aggregate sidebar readiness issue now fires only for
  adds/titles/prefs; unmatched projects are exclusively `attach_project`
  rows. The pull-time "sidebar setup required" message still counts them.
- If the immediate mapped apply fails after the mapping saved, the command
  returns success with `sidebar_pending: true` (D4) instead of erroring.
- Tests inject desktop-running state via `TEST_CODEX_DESKTOP_RUNNING`
  (cfg(test)) instead of pgrep, keeping the suite deterministic.
- Thread grouping also scans `archived_sessions/`, not only `sessions/`.
- Added later (2026-07-17): a force-remap simulation switch makes readiness
  and sidebar planning treat every source project path as foreign even when
  it exists locally — saved mappings still resolve — so the picker flow can
  be exercised on the machine that pushed the profile (both providers;
  forced rows say they are simulated). Toggled per app session from the
  Finish-setup header ("Treat project folders as foreign", via
  `get_force_path_remap`/`set_force_path_remap`); the
  `AGENT_SYNC_FORCE_PATH_REMAP` env var is the boot-time override. Same
  change fixed the Claude transcript cwd probe to scan past leading metadata
  records (`mode`, `last-prompt`, …) instead of reading only line one, and
  Finish setup now auto-opens after setup when the pulled profile has
  `attach_project` rows.

This is the Codex-specific companion to
`PLAN_MANUAL_PROJECT_PATH_MAPPING.md`. Codex is first-class v1 scope: after a
profile is pulled, users can map a source-machine project path to an existing
folder on this machine through the same folder-picker flow planned for Claude.

## 1. Goal

```text
Machine A                         Machine B
/A/repo                          /B/repo
Codex task 019f... -- push/pull -> same task 019f...
```

Finish setup shows one row per unmatched Codex project:

```text
Codex / ChatGPT
/A/repo
[Choose folder]  /B/repo                                      [Map]
2 tasks
```

One mapping applies to every restored task whose recorded cwd is `/A/repo`.
The user does not pick the same folder separately for every task.

## 2. Current code to extend

- `src-tauri/src/codex_sidebar.rs` captures source project paths and normalized
  Git origins in `CodexSidebarLock.projects`.
- `codex_sidebar::plan_apply` resolves an exact path or Git-origin match and
  puts everything else in `SidebarApplyPlan.unmatched`.
- `apply_plan_to_state` already adds projects to the whitelisted ChatGPT/Codex
  desktop sidebar keys without deleting local state.
- `src-tauri/src/readiness.rs` and `get_setup_readiness` already surface the
  aggregate sidebar work through Finish setup.
- `apply_sidebar_state` already refuses to write while ChatGPT/Codex desktop is
  running and creates a backup before replacing desktop state.
- Codex rollout files are portable. Their recorded `cwd` is historical task
  metadata and must not be rewritten by Agent Sync.

## 3. Decisions

**D1 — Explicit folder selection.** Git origin may provide a suggested local
folder, but Agent Sync does not persist or apply a mapping until the user
clicks `Map`.

**D2 — Machine-local mapping.** Store Codex rows in the shared local mapping
file defined by the main plan:

```text
~/.agent-sync/project-path-mappings.json
```

```json
{
  "profile": "codex-work",
  "provider": "codex",
  "source_key": "/A/repo",
  "source_path": "/A/repo",
  "target_path": "/B/repo"
}
```

The target path is local configuration and never syncs.

**D3 — No rollout or SQLite rewrite.** Mapping never edits rollout JSONL,
`state_5.sqlite`, or undocumented task/project associations. It only:

1. tells sidebar apply that `/B/repo` represents `/A/repo` on this machine;
2. supplies `/B/repo` as the cwd for same-thread continuation.

**D4 — A saved mapping and sidebar apply are separate outcomes.** If desktop
is running, save the valid mapping and report `Sidebar apply pending`. Do not
make the user pick the folder again after quitting the desktop app.

**D5 — Stale targets fail visible.** If `/B/repo` is removed or stops being a
directory, readiness re-raises the project-path issue. It does not erase the
mapping or fall back to adding `/A/repo`.

## 4. Discovery model

Change Codex sidebar readiness from one aggregate unmatched count into one
structured candidate per source project:

```rust
struct CodexProjectPathCandidate {
    profile: String,
    source_path: String,
    git_origin: Option<String>,
    mapped_path: Option<String>,
    affected_threads: Vec<String>,
}
```

Candidate construction:

1. Read each source project from the synced sidebar lock.
2. Scan restored rollout metadata with a bounded first-record parser and group
   thread ids by exact `session_meta.cwd`.
3. Resolve in this order:
   - source path is already saved locally;
   - source path exists and can be added directly;
   - a saved manual mapping points to an existing target directory;
   - normalized Git origin matches an existing local sidebar project;
   - otherwise emit a manual path candidate.
4. Keep sidebar titles/preferences as the existing aggregate sidebar action;
   path candidates are independent rows under `Projects & paths`.

The scanner is read-only. It never saves a Git-origin suggestion implicitly.

## 5. Backend behavior

Use the shared commands from the main plan:

```rust
map_project_path(profile, "codex", source_path, target_path)
remove_project_path_mapping(profile, "codex", source_path)
list_project_path_mappings()
```

For Codex, `map_project_path` performs:

1. Resolve the selected `LocalProfile`; require root `.codex`.
2. Re-read the profile's sidebar lock and verify `source_path` is a real
   captured project. The frontend cannot invent a source path.
3. Require `target_path` to be an absolute existing directory with no control,
   `.` or `..` components. Preserve the user's selected spelling instead of
   silently canonicalizing a symlink path.
4. Reject another Codex source mapping to the same target in the same local
   profile.
5. Atomically save the mapping in the machine-local file.
6. If desktop is not running, call mapped sidebar apply immediately.
7. If desktop is running, return success with `sidebar_pending: true`.
8. Return affected thread ids and continuation commands for display.

```rust
struct CodexPathApplyReport {
    source_path: String,
    target_path: String,
    affected_thread_ids: Vec<String>,
    sidebar_applied: bool,
    sidebar_pending: bool,
    resume_commands: Vec<String>,
}
```

Removing or changing a Codex mapping does not remove either source or target
from the user's sidebar. Sidebar application remains additive. It only changes
how future readiness/apply operations resolve the portable source project.

## 6. Sidebar integration

Pass a mapping resolver into `codex_sidebar::plan_apply`:

```rust
resolve_project(source_path: &str) -> Option<&str>
```

When a valid mapping exists:

- add `target_path` to `electron-saved-workspace-roots` and `project-order`;
- never add the foreign `source_path`;
- preserve existing B-only projects and ordering entries;
- keep thread-title and display-preference behavior unchanged;
- surface an invalid/missing mapped target instead of treating the source as
  resolved.

Update `SidebarApplyPlan` to retain structured unmatched projects instead of
only `Vec<String>`, so readiness can render one picker per project.

## 7. Same-task continuation

For every affected restored thread, show:

```text
codex resume <thread-id> -C /B/repo
```

The command resumes the existing thread id with `/B/repo` as the new working
root. It does not fork or rewrite the rollout. Inside that resumed CLI session,
the user can run:

```text
/app
```

to continue it in ChatGPT desktop.

V1 displays selectable commands but does not spawn an interactive terminal.
Native ChatGPT remote Handoff remains a separate workflow. Do not claim that
sidebar apply alone rebinds an already restored task's internal cwd.

References:

- <https://learn.chatgpt.com/docs/projects>
- <https://learn.chatgpt.com/docs/remote-connections#hand-off-a-task-between-hosts>
- <https://learn.chatgpt.com/docs/developer-commands#built-in-slash-commands>

## 8. UI

### Finish setup

- Render every Codex candidate with source path, task count, chosen target,
  `Choose folder`, and `Map`.
- Use `open({ directory: true, multiple: false })` from the existing Tauri
  dialog plugin.
- Keep the selected folder in component state until `Map` succeeds.
- Show backend validation errors inline.
- On success, show `Mapped to /B/repo`.
- If sidebar apply is pending, show `Quit ChatGPT, then Apply sidebar` without
  reverting the successful mapping.
- Show one continuation command per affected thread below the mapped row.

### Settings

List Codex mappings in the main plan's `Project paths` section:

```text
Codex   /A/repo -> /B/repo                         [Change] [Remove]
```

`Change` opens the same folder picker. `Remove` deletes only the machine-local
mapping after confirmation; it never deletes a project folder, task, sidebar
entry, or cloud object.

## 9. Files

- `src-tauri/src/project_paths.rs`: shared schema, Codex validation, atomic
  persistence.
- `src-tauri/src/codex_sidebar.rs`: mapping resolver and structured unmatched
  projects.
- `src-tauri/src/readiness.rs`: Codex candidates and affected-thread grouping.
- `src-tauri/src/lib.rs`: commands, process guard, mapped sidebar apply.
- `src/types.ts`: candidate, mapping, and apply-report types.
- `src/components/FinishSetup.tsx`: Codex picker rows and command results.
- `src/App.tsx`: invoke handlers and readiness refresh.
- `src/components/SyncPanel.tsx`: manage existing Codex mappings.
- `src/App.css`: layout using the existing shared button system.
- `src-tauri/src/sync_tests/{harness.rs,mod.rs,README.md}`: integration
  coverage.

## 10. Tests

Unit:

- exact path, valid mapping, Git-origin match, stale mapping, and unmatched
  resolution order;
- arbitrary frontend source path rejected;
- missing/file/relative/unsafe target rejected;
- duplicate target mapping rejected within one Codex profile;
- affected rollout ids grouped by exact cwd;
- mapped sidebar apply adds target and never source;
- mapping saved while desktop is running with sidebar pending;
- removing mapping never mutates sidebar or project files.

Dual-backend integration:

1. A pushes Codex rollouts and sidebar lock for `/A/repo`.
2. B pulls and sees one `/A/repo` mapping row.
3. B selects `/B/repo`; mapping persists outside the synced manifest.
4. With desktop closed, sidebar apply adds `/B/repo` and leaves B-only state.
5. With desktop running, mapping succeeds and apply remains pending until quit.
6. `codex resume <same-id> -C /B/repo` continues the restored thread.
7. Another pull/relaunch reuses the mapping without another folder prompt.

## 11. Acceptance criteria

- Codex path issues have a real folder picker, not only a generic sidebar
  message or resume instruction.
- One mapping covers all restored tasks with the same source cwd.
- The selected target is persisted per machine and never synced.
- Sidebar apply adds the selected target and never the foreign source path.
- Mapping succeeds safely even when sidebar apply must wait for desktop exit.
- Same-thread continuation uses the original thread id with `-C <target>`.
- No rollout, project folder, SQLite database, or cloud key is rewritten.
- `npm run build`, `cd src-tauri && cargo test`, and
  `cd src-tauri && cargo check` pass after implementation.
