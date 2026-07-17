# Plan: Codex Thread Rebuild (source mtime) and Portable Sidebar State

Status: **IMPLEMENTED** (2026-07-12) — see §9 for implementation notes and
the small deviations found when the desktop state file was verified.
Date: 2026-07-12

Two related pieces of the Codex desktop restore story. Migration/legacy
handling is explicitly out of scope: old manifests and existing cloud
profiles just lack the new field and behave as today.

## 1. Verified findings this plan builds on

Empirically verified on Codex 0.144.1 with a disposable Codex home
containing only backed-up rollout files and `session_index.jsonl`:

- The app-server call `thread/list { useStateDbOnly: false }` scanned the
  restored rollouts and **rebuilt `state_5.sqlite` from scratch**,
  recovering thread ids, working directories, display names, preview text,
  git repository/branch/commit, and stored tasks. This matches the
  documented app-server model: `thread/list` reads stored thread logs;
  `thread/read` / `thread/resume` reopen their history.
- Consequence found in the same test: restored rollout files carry the
  **pull time** as their filesystem mtime, so the rebuilt index treats old
  tasks as newly updated — sidebar recency and ordering are wrong after a
  restore.

## 2. Decisions

**D1 — `state_5.sqlite` stays out of portable sync; rebuild is the restore
path.** The database contains absolute rollout paths and version-dependent
runtime tables, and Codex provably rebuilds it from the files we already
sync (rollouts + `session_index.jsonl`). No new sync surface. The existing
per-remote SQLite-snapshot opt-in remains available for same-machine
disaster recovery, documented as NOT the portable path. The app never
invokes `thread/list` itself in v1 — Codex rebuilds on its own next use.

**D2 — Manifest entries carry `source_mtime`; pull restores it.** The one
correction the rebuild path needs: preserve original modification times
through the cloud round-trip so the rebuilt index keeps real recency.

**D3 — Sidebar state syncs as an app-owned portable subset, never as the
raw file.** `.codex-global-state.json` mixes portable sidebar state with
machine/account identity. It stays unlisted (never raw-synced); the app
captures a bounded, secret-free subset into a lock-style file following the
plugin-lock precedent (capture pre-push, Tier 2 deterministic merge,
physical home `~/.agent-sync/` per PLAN_GLOBAL_AGENT_SYNC_DIR.md).

**D4 — Apply is explicit, additive, and identity-matched.** Restoring
sidebar state on another machine merges only the whitelisted values into
local desktop state, matching projects by Git origin first, then by
configured path mapping. Machine-specific paths are never restored blindly;
nothing local is removed or overwritten destructively (portable-setup D5).

**D5 — Hard exclusions.** Never captured, synced, or applied: account ids,
remote-control installation ids, host selection, window bounds, heartbeat
permissions, client-thread mappings, onboarding state. Prompt
drafts/history are a separate sensitive opt-in, deferred (Section 8).

**D6 — Project paths in the lock are acceptable exposure.** The lock's
`projects[].path` values are absolute source-machine paths. This adds no
new exposure class: the synced rollouts already carry the same cwd values
in their content. Noted so the decision is deliberate, not accidental.

## 3. Part A — `source_mtime` in the cloud manifest

### 3.1 Schema

`ManifestEntry` (`lib.rs`, currently `{ sha256, size, object_key }`) gains:

```rust
/// Source file's modification time (epoch seconds) at upload scan time.
/// 0 = unknown (entry written by an older build) — apply skips restore.
#[serde(default)]
source_mtime: u64,
```

`#[serde(default)]` keeps every existing manifest readable; no migration.

### 3.2 Push

Capture mtime during the upload scan — the walker already stats each file
for the baseline fast path; thread the value into the manifest entry.
Merged Tier 2 outputs and conflict copies published by this machine stamp
the time they were produced (they genuinely changed now).

### 3.3 Pull

After the existing atomic apply (temp file + rename), when
`source_mtime > 0`, restore it via `std::fs::File::set_modified`
(stable std — no new dependency). Then:

- **Baseline**: record the post-restore stat so the size+mtime fast path
  stays valid (stat after the restore, not before).
- **Conflict copies**: the cloud version landing in a
  `*.sync-conflict-*` sibling gets its own `source_mtime` restored too.
- **Merge-driver outputs**: exempt — written as new content, stamped now,
  baseline pins the cloud side exactly as today.
- **SQLite snapshot path**: exempt (snapshot semantics, fast path already
  disabled).
- Failure to set mtime is a logged warning, never a failed pull.

### 3.4 Effect

A fresh machine that pulls a `.codex` profile gets rollouts with true
modification times; Codex's next `thread/list` rebuild produces correct
sidebar recency and task ordering with zero app involvement.

## 4. Part B — Portable sidebar lock

### 4.1 File

Logical `.codex/agent-sync/codex-sidebar.lock.json`, physically
`~/.agent-sync/codex/codex-sidebar.lock.json` (implemented remap). Exact
file allowlisted in `DEFAULT_SYNC_FILES`; the `agent-sync` directory itself
stays unlisted. Captured in the existing `refresh_plugin_locks` pre-push
hook (only when the source file exists) and added to the force-include
list.

### 4.2 Schema

```json
{
  "schema": 1,
  "projects": [
    {
      "path": "/Users/hequ/Desktop/project/Awesome-PhD-CV",
      "name": "Awesome-PhD-CV",
      "git_origin": "https://github.com/rikaqu0223-arch/Awesome-PhD-CV"
    }
  ],
  "project_order": ["/Users/hequ/Desktop/project/Awesome-PhD-CV"],
  "thread_descriptions": {
    "019f59d4-9eaa-7b62-9073-fcc41e8ae609": "General greeting hello hello"
  },
  "sidebar": { "mode": "project", "project_sort": "priority" }
}
```

Validation reuses the plugin-lock patterns: max file size, max entry
counts, bounded string lengths, canonical JSON serialization (sorted keys,
fixed field order, trailing newline), no recapture-varying metadata
(no timestamps/hostnames — Tier 2 convergence requires byte-identical
regeneration), and none of the D5 exclusions may appear.

### 4.3 Capture

Parse the Codex desktop global state file (unlisted, read-only input —
exact on-disk location and key names to be confirmed against the installed
desktop version at implementation time, same rule as the CLI `--json`
verification note in the portable-setup plan). Extract exactly:

- `projects`: path, display name, git origin (normalized: credentials/
  userinfo, query, fragment stripped);
- `project_order`;
- `thread_descriptions` (thread id → user-edited title);
- `sidebar` display prefs (`mode`, `project_sort`).

Everything else is dropped at capture — exclusion is structural (only
whitelisted fields are read), not a redaction pass.

### 4.4 Merge (Tier 2 driver, plugin-lock precedent)

Registered in the existing `merge_driver` dispatch. Both machines
regenerate the lock before every push, so "both changed" is the normal
case:

- `projects`: keyed union by identity = `git_origin`, else `path`;
  same-key collisions resolve by `Ord`-max of the canonical entry.
- `thread_descriptions`: keyed union by thread id; collisions `Ord`-max.
- `sidebar`: whole-object `Ord`-max on collision.
- `project_order`: whole-array `Ord`-max on collision.
  `// ponytail:` order is one user preference list, not mergeable data —
  per-entry rank merging is the upgrade path if whole-array-wins ever
  annoys in practice.
- Unparseable side loses to the parsing side; both unparseable → lexically
  greater bytes (same fallback as the plugin lock).
- Output must be commutative, associative, idempotent, byte-stable.

### 4.5 Apply — explicit, additive, identity-matched

A Finish setup action (`apply_sidebar_state`, new readiness category
`sidebar`), never automatic on pull:

1. Match each lock project to this machine: exact local path exists →
   matched; else `git_origin` equals a local project's origin → matched to
   that local path; else **unmatched** (surfaced as a manual readiness
   item; no invented paths).
2. Merge into the local global state file: add matched projects that are
   missing locally (using the LOCAL path); merge `thread_descriptions`
   only for thread ids whose rollout exists locally; set `sidebar` prefs.
   Never remove or rename anything local; never write a D5-excluded key.
3. Write temp-file + rename with a backup of the previous file; refuse
   (warn) while the Codex desktop app appears to be running — same
   idle-agent guard as pull.

Readiness shows the action only when the merged lock contains something
not yet reflected locally.

## 5. Backend work

- `lib.rs`: `ManifestEntry.source_mtime` (+ push capture, pull restore,
  baseline stat-after-restore); allowlist entry for the sidebar lock;
  capture + force-include in `refresh_plugin_locks`; merge-driver arm;
  `apply_sidebar_state` command; readiness `sidebar` category.
- New `src-tauri/src/codex_sidebar.rs` (Tauri-free, like `codex_plugins`):
  schema, validation, canonical serialization, capture-from-global-state,
  merge, match/apply planning. Process/Tauri stays out of parse/merge code.
- Docs: `AGENT_SYNC_FILE_SETS.md` (Tier 2 table + lock entry + a
  "state_5.sqlite restores by rebuild" note replacing any implication that
  the opt-in is the portable path), `DESIGN2.md` only for the
  `ManifestEntry` field.

## 6. Tests

Unit:

- `source_mtime` serde round-trip; old manifest JSON (field absent) parses
  with 0.
- Apply restores mtime; baseline fast path validates against the restored
  stat; merge outputs and SQLite snapshots exempt.
- Sidebar capture extracts exactly the whitelisted keys from a fixture
  global-state file; D5-excluded keys never appear in the serialized lock.
- Sidebar merge: commutative/associative/idempotent/byte-stable;
  keyed-union collisions; whole-value order collision; unparseable-side
  rules.
- Apply plan: path match, origin match, unmatched → manual; descriptions
  limited to locally-present threads; additive only.

Integration (dual-backend, existing harness):

- A pushes rollouts with known mtimes; B pulls; B's file mtimes equal A's
  (and survive a second no-op pull).
- Divergent sidebar locks on two machines converge to identical bytes; raw
  `.codex-global-state.json` never appears in any manifest.
- Apply on B adds A's origin-matched project under B's local path and
  leaves B-only state untouched.

## 7. Acceptance criteria

- A fresh machine pulling `.codex` gets rollouts with original mtimes, and
  Codex's own rebuild yields correct sidebar recency/ordering — no synced
  database, no app-driven RPC.
- Sidebar projects, order, thread titles, and display prefs travel via the
  lock; account/machine identity provably cannot (structural whitelist +
  serialization test).
- Apply never invents paths, never deletes local state, never runs while
  the desktop app is active without a warning.
- All Rust checks, dual-backend suite, and `npm run build` pass.

## 8. Deferrals

- Prompt drafts / prompt history sync — separate sensitive opt-in with its
  own review.
- Any row-level merge or portable sync of `state_5.sqlite` (rebuild wins).
- App-triggered `thread/list` rebuild (Codex does it on next use; a
  "Rebuild index" button is a possible later convenience).
- Per-entry `project_order` rank merging (see 4.4 ceiling note).
- Cross-agent generalization (Claude has no equivalent desktop sidebar
  state today).

## 9. Implementation notes (2026-07-12)

Landed as planned: `ManifestEntry.source_mtime` (+ push capture, pull
restore inside `apply_cloud_bytes`, baseline stat-after-restore, merge/
SQLite exemptions), new `codex_sidebar.rs`, allowlist + Tier 2 driver +
`refresh_plugin_locks` capture + `apply_sidebar_state` command + readiness
`sidebar` category, docs (`AGENT_SYNC_FILE_SETS.md`, `DESIGN2.md`), unit +
dual-backend integration tests.

The §4.3 verification against the installed desktop app found these
deviations from the sketch:

- **Real key names.** Projects are bare paths in top-level
  `electron-saved-workspace-roots`; order is top-level `project-order`;
  titles are `electron-persisted-atom-state["thread-descriptions-v1"]`;
  prefs are `flat-project-sidebar-preferences-v1` (`mode`,
  `projectSortMode`, `chatSortMode`).
- **No `name` field.** The desktop stores no display name (it derives from
  the path), so the lock's project entries are `{path, git_origin}` only.
- **Git origin is derived, then normalized to an identity.** The state file
  stores no origin; capture reads `<path>/.git/config` and normalizes to
  `host/owner/repo` (scheme/userinfo/query/fragment/`.git` stripped,
  lowercased) so https and ssh clones of the same repo match. Plain clones
  only — worktree `.git`-file redirection is a noted ceiling.
- **`project-order` mixes in remote-project UUIDs** (account-tied); capture
  keeps only entries that are captured local project paths.
- **`.codex-global-state.json` was promoted to the Never tier** (not just
  unlisted) so even a per-remote opt-in cannot raw-sync it.
- `chatSortMode` is captured alongside `mode`/`projectSortMode` — same
  display-pref class.
