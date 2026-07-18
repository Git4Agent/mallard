# Plan: Claude project-path remapping with machine-local aliases

Status: **proposed** (2026-07-17).

This plan replaces only the unimplemented Claude section of
`PLAN_MANUAL_PROJECT_PATH_MAPPING.md`. The shipped Codex mapping behavior stays
unchanged.

## 1. Outcome

Make one synced Claude Code session portable across different absolute project
paths without forking or rewriting its transcript:

```text
Machine A                              Machine B
/A/home                               /B/home
session id 123, name "hello"   --->   same id 123, same name "hello"
```

The user flow is:

1. Machine A creates `hello` under `/A/home` and pushes its Claude profile.
2. Machine B pulls the profile.
3. Finish setup reports that `/A/home` is unavailable.
4. The user chooses `/B/home` and clicks **Map**.
5. Running `claude --resume hello` from `/B/home` finds and continues the
   original session.

The implementation must preserve all of these invariants:

- no `--fork-session` and no new session id;
- no edits to JSONL contents, historical `cwd` values, sidecars, or memory;
- no second B-path project key in the cloud manifest;
- the chosen B path is machine-local and never syncs;
- removing a mapping never deletes a transcript or project folder.

## 2. Architecture decision

Use a machine-local **filesystem alias** as the first implementation, gated by
a real Claude CLI compatibility spike.

After B maps `/A/home` to `/B/home`, its Claude profile contains:

```text
<claude-root>/projects/
  -A-home/              # real directory pulled from cloud
  -B-home -> -A-home    # relative symlink created by Agent Sync
```

Claude Code computes `-B-home` from its current working directory and follows
the alias to the real `-A-home` bucket. Agent Sync continues to sync only the
real source bucket:

```text
Claude at /B/home
        |
        v
projects/-B-home  --local alias-->  projects/-A-home
                                            |
                                            v
                         cloud key .claude/projects/-A-home/**
```

This is simpler and safer than changing `Roots::abs`/`Roots::rel`:

- transcripts do not move;
- the source cloud key remains the physical real directory;
- `WalkDir::follow_links(false)` already ignores the alias;
- `checked_physical_sync_path` already rejects traversal through descendant
  symlinks, while normal pull operations target the real source bucket and do
  not traverse the alias;
- a mapping can be removed by unlinking one verified symlink.

Do **not** implement the permanent `Roots` projection and the alias approach
at the same time. Phase 0 decides which architecture ships.

## 3. Verified current gap

The repository already has the shared mapping and UI foundation:

- `project_paths.rs` stores bounded, atomically saved machine-local records in
  `~/.agent-sync/project-path-mappings.json`; its schema already accepts
  provider `claude`.
- `readiness.rs::project_path_issues` finds real direct children of
  `<claude-root>/projects`, reads transcript `cwd`, and emits
  `attach_project`, but it does not populate `SetupIssue.project_path` for
  Claude.
- `FinishSetup.tsx` already has the folder picker and Map row whenever that
  structured payload exists.
- `lib.rs::map_project_path` explicitly rejects every provider except Codex.
- `remove_project_path_mapping` removes only the JSON record; Claude removal
  will also need to remove its verified alias.
- the sync collector and file tree do not follow symlinks. This is a required
  property of the alias design and must be protected by regression tests.

No new frontend state system is needed.

## 4. Phase 0: compatibility spike

Before production code, verify the design with a disposable
`CLAUDE_CONFIG_DIR` and the installed Claude Code CLI. Do not touch a real
profile.

### 4.1 Determine the actual bucket encoder

Create sessions from several absolute directories and observe the direct child
created under `projects/`:

- ordinary POSIX path;
- spaces and dots;
- repeated punctuation;
- non-ASCII characters;
- two paths that may collide under a lossy dash encoding.

Record the Claude Code version and turn the observations into fixtures for one
`encode_claude_project_path` helper. Do not decode existing bucket names and
do not ship an encoder inferred only from a directory name; Claude's encoding
is lossy.

If the CLI behavior cannot be reproduced deterministically, stop this design
before implementation and use the fallback in section 15.

### 4.2 Prove alias behavior with a real session

Using disposable A and B project directories:

1. Create a named session with `claude --name hello` under A.
2. Record its session UUID and real project bucket.
3. Create B's encoded bucket as a relative symlink to A's bucket.
4. From B, verify both `claude --resume hello` and
   `claude --resume <uuid>` select the same session.
5. Send one turn and verify it appends to the same JSONL through the alias.
6. Create a new session from B and verify its JSONL, session sidecars, and
   per-project memory all land in A's real bucket.
7. Verify Claude does not replace the alias with a real directory.

### 4.3 Prove Agent Sync isolation

Against the same disposable tree:

- file-tree construction hides the alias;
- status and push collect only `.claude/projects/<source_key>/**`;
- pull to the source key succeeds while the alias exists;
- no baseline, backup, manifest, or conflict-copy path contains the alias key;
- selecting the alias path directly cannot make the collector follow it;
- a cloud object under the alias key fails closed instead of traversing the
  local alias.

### 4.4 Gate

Proceed with sections 5–14 only if every check above passes. A permissions
error such as macOS `Operation not permitted` is a separately reported access
problem, not evidence that `Roots` projection is safer; both designs require
write access to the configured Claude profile.

## 5. Mapping model

Reuse the existing record unchanged:

```rust
ProjectPathMapping {
    profile,       // LocalProfile.id
    provider,      // "claude"
    source_key,    // real project bucket basename, e.g. "-A-home"
    source_path,   // original transcript cwd, display only
    target_path,   // exact absolute directory chosen on this machine
}
```

The record is the authoritative intent. Its alias is a derived local artifact:

```text
alias name   = encode_claude_project_path(target_path)
alias target = source_key                 # relative, one path component
```

Keeping the derivation deterministic avoids a second state file. Codex records
remain byte-for-byte compatible.

Add an internal, non-serialized state calculation:

```rust
enum ClaudeAliasState {
    Ready,
    ReadyWithoutAlias, // target encoding already equals source_key
    MissingAlias,
    MissingTarget,
    MissingSource,
    ConflictingDirectory,
    ConflictingSymlink,
    PermissionDenied,
}
```

Readiness and Settings use this state for copy and available actions. They do
not infer readiness merely because `target_path` exists.

## 6. Safety rules

All alias planning and mutation lives in Tauri-free helpers in
`project_paths.rs`.

For every operation:

- require a `.claude` local profile and re-resolve its root from
  `SyncConfig`;
- re-derive `source_key` from a real direct child of `projects/`; the frontend
  cannot invent it;
- require `source_key` to be one normal basename, not empty, `.`, `..`, or a
  string containing `/`, `\`, or control characters;
- inspect with `symlink_metadata`; the source must be a real directory, not a
  symlink;
- validate the selected target with the existing absolute-path checks and
  preserve its exact spelling;
- derive one target bucket with the Phase-0-tested encoder;
- reject two mappings in a profile that resolve to the same target bucket,
  even if their target path strings differ;
- create only a **relative** symlink whose target is exactly `source_key`;
- never follow the alias while validating, changing, or removing it;
- never merge with, rename, or delete a pre-existing real target bucket;
- use the existing conservative `process_is_running("claude")` guard and
  refuse mutation while any Claude CLI is active; profile-specific process
  attribution is not reliable in v1;
- keep readiness scans read-only.

For a new mapping, any existing target bucket is a collision. The only
idempotent exception is an exact expected symlink that is already backed by
the same saved mapping. A link created manually before a mapping exists is not
claimed as app-owned.

Use `std::os::unix::fs::symlink` directly. Creating a symlink is atomic and
fails on `EEXIST`; do not use `rename` in a way that could replace a raced-in
directory or link.

Scope v1 to macOS and Linux. Windows junctions and cross-platform link
semantics are a separate design.

## 7. Claude candidate discovery

Replace `project_cwd(project_dir) -> Option<String>` with a bounded project
probe that returns:

```rust
struct ClaudeProjectProbe {
    source_key: String,
    cwd_candidates: Vec<String>,
    session_ids: Vec<String>,
}
```

Rules:

1. Scan only real direct directories below `projects/`; skip every symlink.
2. For each direct `*.jsonl`, scan only its bounded head and collect its first
   valid `cwd`. Deduplicate and sort values deterministically.
3. Prefer for display a cwd whose tested encoding equals `source_key`; fall
   back to a deterministic transcript cwd without trying to decode the key.
4. Count session ids from direct JSONL basenames. A bucket can legitimately
   contain old A-path and new B-path `cwd` records after mapping.
5. Without a mapping, a project is locally reachable only when an existing
   cwd encodes to the real `source_key`. An existing B directory whose encoding
   differs from the source key is not enough.
6. With a mapping, calculate `ClaudeAliasState`. `Ready` and
   `ReadyWithoutAlias` suppress the issue; every other state remains visible.
7. Preserve the force-remap simulation switch, but do not let it mutate or
   replace a valid mapping.

Populate the existing `ProjectPathCandidate` for Claude:

- `provider = "claude"`;
- `source_key = probe.source_key`;
- `source_path = chosen display cwd`;
- `mapped_path = saved target, if any`;
- reuse `affected_threads` for the affected session ids and update its comment
  to be provider-neutral.

Add an optional provider-neutral `path_state` field carrying `unmapped`,
`missing_alias`, `missing_target`, or `collision` on actionable rows. The UI
uses the state to choose Map, Repair, or Change; it never re-inspects the
filesystem itself.

Keep `readiness::scan` deterministic and dependency-free. In
`get_setup_readiness`, load the mapping document once, build the prefiltered
Claude candidates for each profile, and pass them through a new
`ScanInput.claude_path_candidates` slice, just as Codex candidates are passed
today. A malformed mapping document becomes one actionable project-path
configuration issue; it must not be replaced with an empty document via
`unwrap_or_default()`.

This immediately activates the existing folder picker.

## 8. Apply, repair, change, and remove

### 8.1 First map

`map_project_path(profile, "claude", source_key, target_path)` performs:

1. Load config, require the selected `.claude` profile, and re-probe the
   source bucket.
2. Refuse while Claude is running.
3. Validate the target directory and derive its target bucket.
4. Preflight source, target collision, and duplicate target-bucket mappings
   without changing files. The actual symlink operation remains authoritative
   for ACL/TCC write permission.
5. Atomically save the mapping intent.
6. If target key differs from source key, create the exact relative symlink.
7. Re-read it with `symlink_metadata` + `read_link` and verify the state is
   `Ready`.
8. If a normal error occurs after step 5, remove any newly created exact alias
   and restore the previous mapping file; return the primary error plus every
   rollback error if cleanup also fails.

A process crash between steps 5 and 6 leaves a saved mapping plus a missing
alias. The next read-only readiness scan emits **Repair mapping**; it never
silently claims success.

### 8.2 Repair

Add an idempotent repair path that uses the saved target and repeats the full
preflight. It may create a missing alias, but it must not replace a collision.
The user invokes it from Finish setup or Settings.

### 8.3 Change target

For an existing Claude mapping:

1. Preflight the new target completely.
2. Verify and unlink the old exact alias; if it is not exactly app-owned, stop.
3. Atomically replace the mapping record.
4. Create and verify the new alias.
5. On a normal failure, restore the old record and old alias.

Crash behavior is deliberately repairable:

- crash after old unlink but before save: the old mapping remains and Repair
  recreates its alias;
- crash after save but before new link: the new mapping remains and Repair
  creates its alias.

### 8.4 Remove mapping

1. Refuse while Claude is running.
2. Load the exact mapping and derive its expected alias.
3. If an alias is required, unlink it only when `read_link` exactly equals the
   expected relative `source_key`.
4. Remove and atomically save the mapping record.
5. If saving fails, recreate the verified alias and retain the record.

A crash after unlink leaves the record in place, so Repair is possible. Never
delete the real source bucket, target project directory, JSONL, memory, or
cloud object.

## 9. Backend API changes

Keep the three existing command names, but make them provider-aware:

```rust
map_project_path(app, profile, provider, source_key, target_path)
remove_project_path_mapping(app, profile, provider, source_key)
list_project_path_mappings()
```

For Codex, `source_key == source_path`; update its caller without changing
behavior. For Claude, `source_key` is the real bucket basename and is always
re-derived before mutation.

Replace the Codex-only response with a tagged result so the UI cannot confuse
sidebar state with alias state:

```rust
#[serde(tag = "provider", rename_all = "lowercase")]
enum ProjectPathApplyReport {
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
```

Add a small `repair_project_path_mapping` command or route Repair through the
same idempotent map helper with the saved target. Prefer the explicit command
so the frontend never needs to reconstruct intent.

Preserve OS error kind and the denied path in backend errors. Translate
`PermissionDenied`/`EPERM` into actionable copy such as:

```text
macOS denied access to <claude-root>/projects. Grant Agent Sync access to this
folder (or Full Disk Access when required), then click Repair mapping.
```

Do not report a permission failure as a sync conflict or silently fall back to
copying transcripts.

## 10. UI changes

### Finish setup

Use the existing mapping row and make its copy provider-specific:

```text
Claude Code · 3 sessions
/A/home
[Choose folder]  /B/home                                      [Map]
```

- **Map** explains that it creates a local alias and does not move files.
- A saved mapping with `MissingAlias` shows **Repair mapping**, not another
  folder chooser.
- A stale target shows **Choose folder again**.
- A collision names the existing target bucket and offers Change/Remove; it
  does not offer destructive merge.
- Success reads `Mapped — Claude can resume these sessions from /B/home`.
- Do not show Codex sidebar or `resume -C` copy for Claude.

### Settings

Keep the existing `Project paths` section:

```text
Claude   /A/home -> /B/home       Ready              [Change] [Remove]
Claude   /X/repo -> /Y/repo       Needs repair       [Repair] [Change] [Remove]
```

Update the removal confirmation for Claude: it removes one local alias and
mapping record, never the real transcripts. Refresh readiness and file status
after every successful operation.

## 11. File-level implementation order

1. **Compatibility spike**
   - document CLI version and black-box encoder/alias results;
   - stop if the gate fails.
2. **`src-tauri/src/project_paths.rs`**
   - tested encoder fixtures;
   - `ClaudeProjectProbe`, alias plan/state, validation, map/change/remove/
     repair helpers;
   - Unix-only symlink materialization and permission-error classification.
3. **`src-tauri/src/readiness.rs`**
   - bounded multi-session Claude probe;
   - mapping-aware reachability and structured candidates;
   - read-only missing/collision/repair issues.
4. **`src-tauri/src/lib.rs`**
   - provider dispatch in map/remove;
   - repair command, process guard, config/profile revalidation, tagged report;
   - refresh/logging with no transcript contents.
5. **`src/types.ts`, `src/App.tsx`, `FinishSetup.tsx`, `SyncPanel.tsx`**
   - provider-aware report union and source-key payload;
   - Claude Map/Repair/Change/Remove behavior and copy.
6. **Tests and docs**
   - protect the no-follow sync invariant;
   - update `AGENT_SYNC_FILE_SETS.md` to describe local aliases;
   - mark Claude section 6 of the old combined plan as superseded by this
     document.

Do not modify `Roots` in this implementation path.

## 12. Automated tests

### Unit tests

- encoder fixtures captured in Phase 0, including collision cases;
- source key basename validation and target absolute-path validation;
- duplicate target paths and duplicate encoded target buckets;
- alias state for absent, exact link, wrong link, real directory, missing
  source, missing target, and same-key no-op;
- initial map, idempotent repair, change, remove, and normal-error rollback;
- refuse to claim or delete a pre-existing/manual symlink;
- `PermissionDenied` retains the denied path and actionable classification;
- readiness with unmapped, ready mapped, missing alias, stale target,
  collision, mixed A/B cwd records, and force-remap cases;
- mappings remain structurally outside every `Roots::rel` namespace.

### Sync regression tests

Run the core cases against both the local-folder backend and S3 stub:

1. Push A's real source bucket, pull to B, map to B, and verify the alias.
2. Write a new JSONL line and sidecar through B's alias, push, and assert the
   manifest still contains only A's source key.
3. Pull another cloud change while the alias exists and verify it updates the
   real source directory.
4. Verify file tree, status, selected-file collection, backup, baseline, and
   conflict-copy paths never include or traverse the alias.
5. Pre-existing B real directory and wrong B symlink both fail before any
   mapping or file mutation.
6. Mapping file never appears in a manifest or opt-in list.
7. Remove deletes only the exact alias; source transcripts remain byte-for-byte
   unchanged and are still synced under A's key.
8. Inject failures between lifecycle steps and verify the resulting state is
   explicitly repairable.

### Manual smoke test

With disposable profiles on two paths:

1. `claude --name hello` on A; record UUID and transcript hash.
2. Push/pull, choose B, and Map.
3. `claude --resume hello` on B; confirm the same UUID.
4. Continue one turn, push from B, pull on A, and resume the same UUID on A.
5. Confirm transcript history includes both turns, no second project key was
   published, and no file was rewritten during mapping.
6. Quit/relaunch Agent Sync; Ready state persists.

## 13. Failure behavior

| Condition | Required behavior |
|---|---|
| Claude is running | Refuse mutation; ask user to quit it and retry. |
| `Operation not permitted` | Keep data unchanged; identify the denied path and access remedy. |
| Target folder is gone | Keep mapping, show stale target, offer Change/Remove. |
| Alias is missing | Keep mapping, show Repair. |
| Target bucket is a real directory | Fail closed; never auto-merge histories. |
| Target bucket is another symlink | Fail closed; never replace it. |
| Source bucket is absent/symlink | Refuse; do not trust frontend identity. |
| Two paths encode to one bucket | Reject the second mapping with both paths named. |
| Mapping file is malformed | Fail closed for mutation and show a settings error; do not treat it as empty. |
| Rollback also fails | Return both errors and leave a visible Repair issue. |

## 14. Acceptance criteria

- A pulled Claude project produces a real folder-picker row with session count.
- Mapping A to B lets the current Claude CLI resume the exact original UUID
  and display name from B.
- Continuing from B writes through the local alias into A's real source bucket.
- Push after continuation publishes only the original A logical key.
- No transcript, historical cwd, memory file, or sidecar is rewritten by Map,
  Repair, Change, or Remove.
- Settings accurately distinguishes Ready, stale, repairable, and collision
  states after relaunch.
- Mapping, change, repair, and removal fail closed under races, symlink attacks,
  process activity, and permissions errors.
- `npm run build`, `cd src-tauri && cargo test --lib`, and
  `cd src-tauri && cargo check` pass.

## 15. Fallback only if Phase 0 rejects aliases

If the real Claude CLI does not discover or write through the target alias,
replace the alias design with the permanent `Roots` projection from the old
plan. Do not fall back merely because alias creation lacks filesystem
permission.

The fallback must add safeguards that the old plan did not fully specify:

- strict, fail-closed mapping loads for every sync operation;
- one immutable mapping snapshot held for the entire push/pull/status action;
- a per-profile operation lock preventing mapping edits during sync;
- explicit materialization states (`pending`, `active`, `rollback_pending`)
  with startup recovery;
- collision preflight before any directory move;
- rollback tests at every save/move boundary;
- regression coverage for collection, pull apply, baselines, status, backups,
  conflict copies, and mapping removal.

That fallback is a separate implementation phase because it changes the path
waist of the entire sync engine. It must not be smuggled into the alias patch.

## 16. Non-goals for v1

- rewriting JSONL `cwd` fields or Claude session ids;
- forking sessions;
- automatic prefix maps such as `/A -> /B`;
- automatically merging an existing B history bucket with A;
- deleting obsolete cloud keys;
- choosing or creating the user's project checkout;
- Windows junction support;
- remapping Claude account, trust, or MCP state in `.claude.json`.
