# Agent Sync

Agent Sync is a Tauri 2 desktop app for moving the Codex and Claude resources
that belong to a project between machines. It syncs a selected project bundle,
not an entire `~/.codex` or `~/.claude` directory.

## Handoff status

The July 18 update makes the schema-3 project workspace the default UI. It now
supports:

- named, machine-local Codex and Claude profiles selected before project
  discovery;
- project resource inventory and persistent selection recipes;
- project links to local-folder and S3/R2 storage;
- global schema-3 machine metadata stored directly under `~/.mallard`;
- remote bundle browsing and repository-fingerprint matching;
- push, verified fetch, restore planning, explicit apply approval, dependency
  planning, and readiness checks; and
- a legacy schema-2 workspace available from the sidebar for reference.

The current work is not committed. It sits on `main` as tracked and untracked
changes, including `src-tauri/src/project_sync_v3/` and
`src/components/project-sync/`. Run `git status --short` first and do not clean
or reset the worktree.

## Run locally

Requirements: Node.js with npm and a stable Rust toolchain.

```sh
npm install
npm run dev          # Vite frontend only at http://localhost:1420
npm run tauri dev    # desktop app with the Rust backend
```

There is no `.env` file. Add projects, provider profiles, and storage through
the app.

## How to use it

For the takeover, please use a local folder as storage and a disposable custom
Codex home. Do not use `~/.codex` or S3/R2 for the first smoke test.

This example assumes the current checkout is at
`$HOME/work/tauri-codex-sync`:

| Purpose | Example path |
|---|---|
| Source checkout | `$HOME/work/tauri-codex-sync` |
| Source Codex home | `$HOME/agent-sync-takeover/codex-source` |
| Local bundle storage | `$HOME/agent-sync-takeover/local-storage` |
| Restore checkout | `$HOME/work/tauri-codex-sync-restore` |
| Restore Codex home | `$HOME/agent-sync-takeover/codex-restore` |

1. Create a Codex home and storage directory outside the project checkout.
   Keep these paths separate from each other and from the checkout.

   ```sh
   mkdir -p "$HOME/agent-sync-takeover/codex-source"
   mkdir -p "$HOME/agent-sync-takeover/codex-restore"
   mkdir -p "$HOME/agent-sync-takeover/local-storage"
   touch "$HOME/agent-sync-takeover/codex-source/config.toml"
   touch "$HOME/agent-sync-takeover/codex-restore/config.toml"
   ```

   `codex-source` is the exact `CODEX_HOME`. It does not need to be named
   `.codex`. The command above creates its config at
   `codex-source/config.toml`, not `~/.codex/config.toml`. Use a normal local
   directory for storage, not Dropbox or iCloud, during the first test.

   If this isolated Codex home is not logged in, authenticate it directly:

   ```sh
   CODEX_HOME="$HOME/agent-sync-takeover/codex-source" codex login
   ```

2. Start Codex with the custom home and complete one small task in the test
   checkout. This gives Agent Sync a project-owned task to discover.

   ```sh
   CODEX_HOME="$HOME/agent-sync-takeover/codex-source" codex -C "$HOME/work/tauri-codex-sync"
   ```

   Reuse that `CODEX_HOME` whenever you reopen the test task. Do not copy
   `auth.json` or other machine state from `~/.codex` into this directory.

3. Start the app with `npm run tauri dev`.
4. In the Storage section, click `+`, select `Local folder`, choose
   `$HOME/agent-sync-takeover/local-storage`, name it `Takeover local`, then
   click `Create storage`.
5. In the Projects section, click `+` and choose
   `$HOME/work/tauri-codex-sync`. On `Choose provider profiles`, click `Add`
   beside Codex and select the exact
   `$HOME/agent-sync-takeover/codex-source` directory. Leave Claude set to
   `Not used`, then click `Discover resources`.
6. Review the discovered task and config resources, choose what should sync,
   link the local storage, and click `Create project`. Open the project row and
   click `Push`. The Activity panel should show the published generation, and
   the storage directory should contain `v3/bundles/<bundle-id>/`.
7. Prepare a second checkout of the same Git repository at
   `$HOME/work/tauri-codex-sync-restore`. Add it as another project, choose
   `$HOME/agent-sync-takeover/codex-restore` as its Codex profile, and link
   `Takeover local`. Choose the matching remote repo, click
   `Connect and review Pull`, inspect the restore plan, then approve only the
   writes and dependency actions you expect. The task and selected agent
   resources should appear under `codex-restore`; Agent Sync should not copy
   the application's source files.

Bundle IDs are opaque. Checkout paths, provider profile paths, credentials,
trust decisions, and apply receipts stay on the local machine. Schema 3 uses
the global `~/.mallard/` directory directly—not a nested Tauri app-data `v3/`
directory—and cloud keys under `v3/bundles/<bundle-id>/`. It does not migrate
or overwrite schema-2 state, and schema-3 files from the former Tauri app-data
location are not imported automatically.

## Metadata layout

"Local metadata" and "local-folder storage" are different things:

```text
Machine                                              Portable storage
checkout + selected provider home -- selected data -> v3/bundles/<bundle-id>/
sync_config: bundle ID + recipe ------ projection --> current manifest
paths, profiles, credentials, plans ------- X         never uploaded
```

The bundle ID, display name, hashed repository fingerprint, and selected
recipe are represented both locally and in the current remote manifest. The
local JSON documents are never copied as objects. Push builds a portable
manifest from the selected fields and captured resources.

### Machine-local metadata

Global machine metadata is stored directly in `~/.mallard`, without an
application-data or nested `v3/` wrapper. Files are created as the corresponding
feature is used:

```text
~/.mallard/
|-- sync_config.json
|-- machine_projects.json
|-- materializations.json
|-- dependency_applications.json
|-- restore_plans/
|   `-- <plan-id>.json
|-- dependency_plans/
|   `-- <plan-id>.json
`-- backups/
    `-- <plan-id>/...
```

- `sync_config.json` stores the schema and revision, storage definitions,
  project registrations, bundle recipes, project-to-storage links, and the
  reviewed remote base for each storage. S3 credentials are local fields in
  this file.
- `machine_projects.json` stores named provider profiles, exact and canonical
  `CODEX_HOME` or `CLAUDE_CONFIG_DIR` paths, checkout bindings, local profile
  IDs, and per-machine replica IDs.
- `restore_plans/` and `dependency_plans/` store generation-pinned approval
  documents. Restore plans contain resolved absolute target paths; dependency
  plans contain typed installer arguments and blockers.
- `materializations.json` records Pull results and per-action receipts,
  including the source manifest hash, binding revision, target paths, target
  hashes, status, and errors.
- `dependency_applications.json` records dependency actions that were applied,
  skipped, blocked, or failed.
- `backups/<plan-id>/` holds pre-write copies made while applying a Pull.

These files are atomically replaced with revision checks and private file
permissions on Unix. They contain machine paths and may contain storage
credentials, so do not copy this directory into the shared storage folder.
The selected Codex or Claude home is separate again: Agent Sync reads and
writes approved provider resources there, but it does not treat that whole
directory as app metadata.

### Portable storage metadata

Local-folder and S3/R2 storage use the same object keys. A local folder also
has a lock file used for compare-and-swap writes:

```text
<local-storage>/
|-- .bundle-store.lock                 # local-folder mode only
`-- v3/bundles/<bundle-id>/
    |-- _tag.json
    |-- _head.json
    |-- _manifests/
    |   `-- <generation>-<commit-id>.json
    |-- _commits/
    |   `-- <generation>-<commit-id>.json
    `-- _uploads/
        `-- <upload-id>/files/<logical-path>
```

- `_tag.json` is a replaceable discovery summary with the display name,
  bundle type, generation, update time, and resource and file counts. It is a
  convenience index, not the source of truth.
- `_head.json` is the only authoritative mutable pointer. It names the current
  generation, commit, manifest key, and manifest SHA-256. Local storage updates
  it under the lock; S3/R2 uses a conditional write with the object's ETag.
- `_manifests/` contains immutable full snapshots. A manifest stores the
  portable bundle identity, repository fingerprint, selected recipe, capture
  tool versions, resource descriptors, provenance, apply policy, relative
  working directories, logical file entries, hashes, sizes, modes, object
  keys, and tombstones.
- `_commits/` contains immutable history records. Each record points to its
  manifest, records the previous commit ID, and counts added, changed, and
  removed files.
- `_uploads/` contains immutable selected file bytes. Paths are logical, such
  as `project/AGENTS.md`, `state/codex/sessions/...`, or
  `state/claude/projects/...`; source-machine absolute paths are not object
  keys.

The remote bundle never contains local storage credentials, provider profile
IDs or paths, checkout paths, restore plans, approval receipts, backups, auth
state, or trust decisions. It does contain selected conversations and project
configuration. Known credential fields are excluded, but opaque conversation
or script content receives only best-effort secret checks.

### Race protection and version history

Agent Sync uses optimistic concurrency and immutable snapshots. This is bundle
versioning, not a replacement for Git. Git remains responsible for the
application's source files.

The version fields have separate jobs:

- `schema` and `schema_version` identify the document format. Readers reject
  unsupported formats instead of guessing.
- Local `revision` fields protect app settings, recipes, profiles, and
  bindings from stale edits. A save must still match the revision that was
  opened, then the revision increments.
- A `RecipeBase` records the remote generation and manifest hash last reviewed
  by this local binding, plus its recipe and binding revisions. Push refuses
  to build on an unknown, missing, or newer remote base.
- Every successful Push creates a new remote `generation` and `commit_id`.
  The commit points to a full immutable manifest and to the previous commit.
- SHA-256 values bind the head to its manifest and each manifest entry to its
  uploaded bytes. Fetch verifies those hashes before planning or applying.

Two Push operations may start from the same head, but only one can publish the
next head:

```text
Pusher A                     Storage                     Pusher B
read generation 7, ETag E1  <--- _head.json ----------> read generation 7, ETag E1
write generation 8 objects  ---> immutable objects <--- write alternate objects
CAS E1 -> generation 8      ---> accepted
                                                         CAS E1 -> generation 8
                                                         rejected: E1 is stale
```

Uploads, manifests, and commits are written before `_head.json`. If a process
crashes or loses the CAS, its immutable objects may remain as unreachable
orphans, but the current head never points to a partial generation. In
local-folder mode, `.bundle-store.lock` serializes the immutable writes and
head CAS on one filesystem. In S3/R2 mode, conditional requests use the head
object's ETag.

The current schema-3 Push command also compares `_head.json` with the local
`RecipeBase` before publication. If another machine already advanced the bundle,
Push stops and asks for Pull and review. After a successful head CAS, the app
rechecks the local project and recipe revisions before saving the new base. If
the recipe changed while Push was running, the remote generation remains
valid, but the app reports the race and requires a refresh.

Pull has a second set of guards:

```text
plan = storage + bundle + replica + generation + commit
     + manifest SHA-256 + binding revision + expiry

apply:
  1. reload the current binding
  2. fetch and verify the pinned bundle again
  3. confirm _head.json still names the planned generation
  4. confirm each target path and pre-write hash still match the plan
  5. back up the old bytes, write, then record an apply receipt
```

If the bundle head, checkout binding, provider profile, or target file changes
after planning, apply stops instead of writing over the new state. A plan ID
can be recorded only once. Dependency plans use the same bundle and binding
pins and keep separate application receipts.

Global metadata updates under `~/.mallard` use a process mutex, revision
checks, and a synced temporary file followed by atomic replacement. The mutex
covers one running app process; it is not a cross-process database lock. The
local storage lock also works only when writers see the same filesystem lock,
so Dropbox and iCloud folders must not be used for simultaneous multi-machine
Push.

Current limitation: a stale Push fails closed, but automatic fetch, three-way
rebase, and CAS retry are not finished for schema 3. Per-replica baselines and
class-specific conflict and tombstone rules are also pending. The immutable
history preserves earlier generations, but the current UI does not expose a
rollback or orphan cleanup workflow.

## Verify

```sh
npm run test:integration
npm run build
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --lib
```

`test:integration` covers the Pull review boundary in both layers: the
frontend keeps a valid restore plan and enabled Apply action visible when
supporting checks fail, while the Rust command workflow publishes, plans,
persists, applies, remaps, and records a Codex conversation restore. The full
Rust suite also starts a localhost stub S3 server, so a restricted sandbox may
need permission to bind a local port.

## Code map

- `src/components/project-sync/`: schema-3 project, profile, storage, resource,
  and restore UI.
- `src/components/project-sync/api.ts`: frontend-to-Tauri command contract.
- `src/types.ts`: frontend DTOs matching Rust response shapes.
- `src-tauri/src/project_sync_v3/domain.rs`: IDs, config, recipes, manifests,
  plans, and receipts.
- `src-tauri/src/project_sync_v3/provider_capture.rs`: Codex and Claude project
  discovery, filtering, and portable projections.
- `src-tauri/src/project_sync_v3/bundle_engine.rs`: publish, fetch, CAS,
  pagination, restore planning, remapping, backups, and apply.
- `src-tauri/src/project_sync_v3/commands.rs`: Tauri command orchestration.
- `src-tauri/src/project_sync_v3/s3_store.rs`: S3/R2 transport.
- `src-tauri/src/project_sync_v3/persistence.rs`: global schema-3 state under
  `~/.mallard`.
- `src/App.tsx` and `src-tauri/src/lib.rs`: schema-3 entry point and command
  registration; legacy code still lives in both files.

## Next work

1. Add focused coverage for materialization receipts, stale bases, plugin
   command generation, and secret rejection.
2. Finish plugin provenance and post-install verification. Codex plugin
   inventory still relies mainly on config, and Claude plugin capture is
   partial.
3. Expose selectable global standalone skills. The capture model supports
   them, but command requests currently pass an empty list.
4. Finish per-replica baselines, three-way resource reconciliation, CAS retry,
   and class-specific conflict and tombstone behavior.
5. Run the full desktop flow against a disposable live S3 or R2 bucket. Local
   and stub-S3 coverage pass, but a live transport smoke test is still pending.

## References

- [`PLAN_PROJECT_SCOPED_SYNC.md`](./PLAN_PROJECT_SCOPED_SYNC.md): schema-3
  architecture and safety rules.
- [`PLAN_PROJECT_PROFILE_ASSOCIATION.md`](./PLAN_PROJECT_PROFILE_ASSOCIATION.md):
  machine-local provider profile model.
- [`AGENT_SYNC_FILE_SETS.md`](./AGENT_SYNC_FILE_SETS.md): detailed handoff and
  legacy file-set evidence.
- [`src-tauri/src/sync_tests/README.md`](./src-tauri/src/sync_tests/README.md):
  integration harness and storage coverage.
