# Handoff: Agent/CLI-Driven R2 Storage Setup

Status: PROPOSED — ready for handoff, no work started.
Audience: whoever implements this (engineer or a fresh agent session), with
no prior context from the planning conversation assumed. Everything needed
to start Task 1 is above the "Reference" section; rationale and alternatives
considered live below it and aren't required reading to begin.

Builds on: PLAN_MULTI_STORAGE.md (schema-3 `SyncConfigV3.storages`, the
"`save_project_sync_config` is the one mutation API" philosophy). Not
CLOUDFLARE_R2_MANAGED_CLOUD_ARCHITECTURE.md — that's a future keyless
Worker-proxy model where the app never holds R2 credentials; this plan keeps
today's BYO-credential model, same as manual entry in the UI today.

## Goal

Let a user paste a short instruction to a terminal coding agent (Codex CLI /
Claude Code). The agent provisions/verifies an R2 bucket via `wrangler` and
registers a new storage entry in `~/.mallard/sync_config.json` (schema 3 —
the only config that matters; the legacy schema-2 file under
`~/Library/Application Support/com.hequ.agent-sync/` is dead, ignore it) via
a new bundled CLI tool, `mallard-cli` — safely, without hand-editing JSON —
and the change shows up in an already-running instance of the app without a
restart.

## Definition of done

- [ ] `mallard-cli storage add-r2 ...` validates, connectivity-checks, and
      writes a new R2 storage into `~/.mallard/sync_config.json` from
      outside the running app, without corrupting a concurrent write from
      the app itself.
- [ ] A running Mallard instance shows the new storage in the Storages list
      within ~1s of that CLI call — no restart, no manual refresh.
- [ ] The whole flow is invokable from a single pasted instruction to an
      agent, which also handles installing `wrangler` if it's missing.
- [ ] Ships inside the macOS dmg first (Tasks 1–4); Windows parity is a
      separate, later task (Task 5).
- [ ] (stretch) Given a one-time bootstrap Cloudflare API token the user
      already has, adding a new storage needs *zero* dashboard visits at
      all — `mallard-cli` mints its own scoped R2 credentials (Task 6).

## Known infeasible-as-stated / scoped out

- **`wrangler` cannot mint R2 credentials.** Confirmed against Cloudflare's
  own docs: `wrangler` has no subcommand for the S3-style
  `access_key_id`/`secret_access_key` pair — it authenticates R2 bucket
  operations through the account session directly and never needed S3 keys
  itself. Tasks 1–4 treat "the caller already has an R2 API token's
  key/secret in hand" as an input. **This is not a dead end**, though: Task
  6 covers a confirmed-feasible path to mint those credentials via the
  plain Cloudflare REST API instead, with only one unavoidable one-time
  manual step (not per storage) — see Task 6.
- **Generic external-writer live-reload.** Only changes made through
  `mallard-cli` trigger a live UI refresh (Task 3). A hand-edit of the JSON
  with a text editor won't — that still needs a restart or manual refresh.
  Deliberate scope cut, not a bug.
- **Secrets keychain.** Out of scope — plaintext JSON in `sync_config.json`
  is an existing, explicit design decision (PLAN_MULTI_STORAGE.md:291);
  this feature doesn't regress or improve on it.
- **Distributed/networked locking.** Just a single-machine advisory file
  lock (Task 1) to guard the CLI against a concurrent write from a running
  app on the same machine — not a networked lock service.

## Before you start

- Add to `src-tauri/Cargo.toml`:
  - `fd-lock` — cross-platform advisory file lock (wraps `flock` on Unix,
    `LockFileEx` on Windows behind one `RwLock<File>` API). Verified real,
    current crates.io package.
  - `interprocess` — cross-platform local socket (Unix domain socket on
    macOS/Linux, named pipe on Windows behind one `local_socket` API).
    Verified real, current crates.io package; its async support targets
    Tokio, which is already a dependency (`Cargo.toml:23`).
- Required code change before a second binary can compile against the
  existing sync engine: `src-tauri/src/lib.rs:38` currently has `mod
  project_sync_v3;` (private to the crate). Change to `pub mod
  project_sync_v3;` (or add targeted `pub use` re-exports for
  `StorageConfigV3`, `SyncConfigV3`, `V3Repository`). Confirm nothing inside
  that module relied on crate-private visibility before flipping it — do
  this as the first step of Task 1.

## Tasks, in build order

### Task 1 — `mallard-cli` binary + `storage add-r2` write path (no wrangler, no notify yet)

Files: new `src-tauri/src/bin/mallard-cli.rs`; `src-tauri/src/lib.rs:38`;
`src-tauri/Cargo.toml`; `src-tauri/src/project_sync_v3/persistence.rs`.

- Flip `mod project_sync_v3;` → `pub mod project_sync_v3;` in `lib.rs:38`
  (see "Before you start").
- New bin target `mallard-cli` depending on the existing `tauri_app_lib`
  rlib (`Cargo.toml:9-10` already declares `crate-type = [..., "rlib"]`, so
  a second `[[bin]]` in the same package links it for free — no workspace
  restructuring needed).
- Subcommand:
  `mallard-cli storage add-r2 --name <name> --bucket <bucket> --account-id <id> --access-key-id <key> --secret-access-key-stdin [--endpoint <url>] [--region auto] [--json]`
  - `--secret-access-key-stdin` (read one line from stdin) is the
    documented default — avoids leaking the secret into shell history/`ps`.
    Keep `--secret-access-key <value>` as a fallback for scripted/agent use
    that accepts that tradeoff.
  - Validate with `StorageConfigV3::validate()`
    (`src-tauri/src/project_sync_v3/domain.rs:382-401`) — do not
    reimplement id/name/bucket rules by hand.
  - Before writing: run a live connectivity check (HeadBucket, or a small
    put/get/delete round trip) against the given endpoint/bucket/
    credentials. Reuse whatever S3 client construction `Store::S3` already
    uses in `lib.rs` (exact function TBD — grep `Store::S3` at
    implementation time) rather than re-authenticating sigv4 by hand — this
    catches a typo'd key before it's persisted.
  - Write via `V3Repository::mutate_config` (`persistence.rs:125`), wrapped
    in a new cross-process `fd_lock::RwLock` over e.g.
    `~/.mallard/.sync_config.lock`. Put this lock inside `V3Repository`
    itself so both the CLI and the running app share it — today's
    `PERSISTENCE_LOCK` (`persistence.rs:29`) is an in-process `Mutex` only
    and doesn't cover a second process.
  - Reject a duplicate storage id. Also reject a second storage whose
    (bucket, account_id, endpoint) tuple matches an existing one under a
    different id — check at implementation time whether this de-dup rule
    already exists anywhere; add it here if not.
  - `--json` flag: print `{"storage_id": "...", "revision": N}` to stdout.
  - Never print `secret_access_key` to stdout or logs, under any flag.
- Also add `mallard-cli storage list` and `mallard-cli storage remove <id>`
  — small and symmetrical; lets an agent check for an existing entry before
  adding (avoid duplicate storages pointing at the same bucket) and undo a
  mistake.

Acceptance: with the app fully closed, run `mallard-cli storage add-r2 ...`;
`~/.mallard/sync_config.json` gets a new, valid entry with a bumped
`revision`, `0600`/`0700` perms preserved, and the write stays atomic (no
partial-write window visible even under a kill -9 mid-write test).

### Task 2 — Packaging + "Enable CLI" PATH install (macOS)

Files: `src-tauri/tauri.conf.json`; a new Tauri command (name TBD, e.g.
`install_cli_tool`); one button on the Settings page.

- Add the `mallard-cli` bin target to `bundle.resources` in
  `tauri.conf.json` (ships inside `Contents/Resources/` on macOS) — not
  `bundle.externalBin`, which is for binaries the app shells out to; here
  the direction is reversed, a user/agent shells out to this binary.
- New Tauri command that copies/symlinks the bundled binary to
  `~/.local/bin/mallard-cli` (no sudo needed), wired to one Settings-page
  button, mirroring VS Code's "Install 'code' command in PATH."

Acceptance: after clicking the button, a newly opened terminal can run
`mallard-cli --help` with no absolute path needed. (Linux is assumed to
follow the same `~/.local/bin` convention, but hasn't been separately
verified — flag if it diverges.)

### Task 3 — Push-notify socket + frontend live refresh

Files: `src-tauri/src/lib.rs` (or a new small module) for the listener
setup; `src/components/project-sync/ProjectSyncV3.tsx` for the frontend
listener.

A filesystem watcher would also work but is unnecessary machinery — the
only writer that matters is `mallard-cli` itself, so it pushes instead of
the app watching for changes it can't attribute.

- App side: at startup, bind a local socket via `interprocess::local_socket`
  (name it `~/.mallard/notify.sock` on Unix — see Task 5 for the Windows
  named-pipe equivalent, same call site) in a background tokio task. No
  protocol — the connection itself is the signal. On any incoming
  connection: close it, `load_config()`, and
  `app.emit("project-sync-config-changed", revision)` (same emit mechanism
  already used for `"sync-log"`, `src-tauri/src/activity_log.rs:792`). No
  debounce or last-broadcast bookkeeping needed backend-side — the
  frontend's own revision check (below) makes repeat emits free.
- CLI side: after Task 1's write succeeds, best-effort connect-and-close to
  the same socket as the very last internal step; ignore any error (app not
  running is expected, not a failure). This is the tail end of `storage
  add-r2` itself — not a separate subcommand an agent has to remember to
  call.
- Frontend: new `listen("project-sync-config-changed", ...)` in
  `ProjectSyncV3.tsx`, registered/torn down in a `useEffect` right next to
  the existing `listen("sync-log", ...)` (`ProjectSyncV3.tsx:351-362`). On
  event: if the incoming revision ≤ the revision already in React state,
  no-op. Otherwise call `projectSyncApi.getConfig()` (the same invoke used
  on initial load) and merge into state — unless the storage editor has an
  unsaved edit open, in which case show a small non-blocking "storage list
  changed on disk — refresh?" banner instead of clobbering it (mirrors the
  existing rebase-before-save pattern in `saveStorageConfig`,
  `ProjectSyncV3.tsx:1504-1542`).
- When the app itself saves (user clicks "Save"), nothing pings the socket
  — that path already updates React state directly from the save response.
  Only external writers go through the socket, so there's no self-write
  feedback loop to guard against.
- Scope: storages array only, not projects/links.

Acceptance: with the app open and the Storages page visible, run `mallard-cli
storage add-r2 ...` from a terminal; the new storage appears in the UI
within ~1s, no manual refresh, no restart.

### Task 4 — `--wrangler` convenience mode

Combines with Task 6's `--cf-api-token-stdin` for a fully automated flow —
this task on its own still expects the caller to supply
`--access-key-id`/`--secret-access-key-stdin`.

- `mallard-cli storage add-r2 --wrangler ...`: if `wrangler` is on PATH and
  `wrangler whoami` succeeds, shell out to `wrangler r2 bucket create
  <bucket>` (treat "already exists" as success) and parse the account id
  from `wrangler whoami`, so the caller only supplies the two
  dashboard-sourced credential fields.
- If `wrangler` is missing: exit with a clear, actionable, non-zero-status
  message (`wrangler not found on PATH. Install it with: npm install -g
  wrangler`). Do **not** have `mallard-cli` run that install itself — a
  background helper binary silently reaching for a global npm install is a
  bigger, less visible action than an agent running the same command in a
  terminal the user is watching. The install step belongs in the pasted
  agent instruction (see Reference: Example instruction, below).
- Never block the plain `--account-id`-supplied path on wrangler being
  present at all.

Acceptance: with `wrangler` not installed, `mallard-cli storage add-r2
--wrangler ...` fails fast with the install hint and does not touch
`sync_config.json`. With `wrangler` installed and logged in, the bucket gets
created (or is confirmed to already exist) and the storage entry is written
with account id auto-filled.

### Task 5 — Windows parity

Nothing here blocks the feature on Windows — `wrangler` is a cross-platform
npm package, the config format and `~/.mallard`-equivalent path resolution
(`dirs::home_dir()` → `C:\Users\<user>\.mallard`) are cross-platform, and
the app's own code already follows a "Unix does X behind `#[cfg(unix)]`,
Windows takes a different branch" pattern throughout `persistence.rs`,
`lib.rs`, `activity_log.rs`, `codex_plugins.rs`, etc. Concrete deltas from
Tasks 1–4:

- **Task 1's lock**: already covered — `fd-lock` wraps `LockFileEx` on
  Windows behind the same API, no change needed.
- **Task 3's socket**: already covered by using `interprocess::local_socket`
  instead of raw `std::os::unix::net::UnixListener` (which is
  `#[cfg(unix)]`-gated in std and doesn't exist on Windows at all) — one
  call site picks a named pipe on Windows automatically. Open item to
  verify before shipping: a `CreateNamedPipe`-backed pipe's default
  security descriptor needs to actually scope to the current user, the
  Windows analogue of the Unix socket inheriting `~/.mallard`'s `0700`
  directory permission — confirm what `interprocess` sets by default and
  pass an explicit security descriptor if it doesn't already restrict to
  the current user session.
- **Task 2's PATH install**: no PATH mutation by default on Windows. Ship
  `mallard-cli.exe` under `%LOCALAPPDATA%\Mallard\bin\` (same
  `bundle.resources` mechanism; Tauri's NSIS/MSI bundler is already covered
  by `"targets": "all"` in `tauri.conf.json`, and its default per-user
  install under `%LOCALAPPDATA%` needs no admin rights). Surface the full
  path in the app for the user to paste into the agent instruction, instead
  of a bare command name — this avoids a registry mutation entirely for
  users who don't need one. An explicit opt-in "Enable CLI" button (same
  idea as macOS's, not on by default) can additionally:
  1. Read the current `HKEY_CURRENT_USER\Environment\Path` via the `winreg`
     crate (not `setx`, which silently truncates values over 1024
     characters — a known footgun specifically for PATH edits).
  2. Append the CLI's `bin` directory if not already present, write back.
  3. Broadcast `WM_SETTINGCHANGE` (`SendMessageTimeoutW` to
     `HWND_BROADCAST`) so newly spawned processes pick it up.
  4. Tell the user any already-open terminal still needs reopening — this
     is inherent Windows behavior (the same limitation VS Code's own
     Windows PATH installer has), not something to work around.

Acceptance: same as Tasks 1–4, run on Windows; the CLI works via the
absolute path with zero PATH edits as the default success path, and via the
bare command name if the opt-in PATH step was taken.

### Task 6 — Cloudflare-API-based credential minting (`--cf-api-token-stdin`)

Confirmed feasible against Cloudflare's own docs (not a spike anymore):
R2's S3-style credentials are deterministically derived from a plain
Cloudflare API token —

> Access Key ID: the token's `id`. Secret Access Key: the SHA-256 hash of
> the token's `value`.

— so `mallard-cli` can mint its own scoped R2 credentials via the standard
Cloudflare Tokens API instead of requiring the caller to already have a
dashboard-created key/secret. Independent of Task 4 (`--wrangler`); the two
combine for a fully automated flow.

- New flag: `mallard-cli storage add-r2 --cf-api-token-stdin --account-id
  <id> --bucket <bucket> --name <name> [--wrangler] [...]` — reads a
  Cloudflare **bootstrap** API token from stdin (never a plain flag — same
  shell-history/`ps` reasoning as `--secret-access-key-stdin`). An
  equivalent `CF_API_TOKEN` env var is a reasonable alternative input for
  an agent's session, since env vars aren't written to disk the way a flag
  in shell history is.
- Call `POST https://api.cloudflare.com/client/v4/accounts/{account_id}/tokens`
  with the bootstrap token as `Authorization: Bearer`, requesting a new
  token scoped to permission group **"Workers R2 Storage Bucket Item
  Write"** limited to just `--bucket` (least privilege) — fall back to the
  account-scoped **"Workers R2 Storage Write"** only if the bucket doesn't
  exist yet at token-creation time (e.g. `--wrangler` wasn't also passed),
  and say so in the tool's output so the caller knows a broader grant was
  used.
- From the response: `access_key_id = id`; `secret_access_key =
  sha256_hex(value)`. Verify the hex-vs-other encoding empirically against
  one real token before shipping — Cloudflare's docs state the formula but
  not the exact string encoding, and R2 secret keys are conventionally
  64-character hex, consistent with a raw SHA-256 hex digest, but don't
  ship on inference alone.
- Feed the derived credentials into Task 1's existing connectivity
  self-test before writing — this catches a derivation mistake immediately
  instead of persisting bad credentials.
- Never print the bootstrap token or the derived secret to stdout or logs,
  under any flag.
- **The one remaining manual step, done once ever, not per storage**:
  obtaining the bootstrap token itself. Cloudflare restricts the
  permission needed to create other tokens via the API (something like
  "API Tokens Write") to tokens created from the dashboard's **"Create
  additional tokens"** template specifically — their docs say this
  permission "is not available in any other template or in the Custom
  Token builder," so it cannot be scripted away. Document this as a
  one-time setup step (e.g. a Settings-page link/instructions in the app),
  clearly distinct from the per-storage flow it then unblocks.
- Don't have `mallard-cli` persist the bootstrap token anywhere by
  default — read it fresh each invocation (stdin or env var). Persistent
  storage of it is a keychain question, out of scope here (see "Known
  infeasible-as-stated": no secrets keychain).

Acceptance: given a valid bootstrap token (env var or stdin) and no
pre-existing access-key-id/secret, `mallard-cli storage add-r2 --wrangler
--cf-api-token-stdin --bucket <bucket> --name <name>` alone creates the
bucket, mints a bucket-scoped R2 token, derives S3 credentials,
connectivity-checks them, and writes the storage entry — zero dashboard
visits for this call, given the one-time bootstrap token already exists.

## Reference (background for the tasks above — not required to start Task 1)

### Example instruction (what a user actually pastes to an agent)

```
Set up a new R2 storage called "team-backup" in Mallard:
1. Check whether `wrangler` is installed (`wrangler --version`). If it isn't,
   install it with `npm install -g wrangler`.
2. Make sure it's logged in (`wrangler whoami`); if not, run `wrangler login`
   and wait for me to complete the browser flow.
3. Create the R2 bucket: `wrangler r2 bucket create team-backup` (fine if it
   already exists).
4. I already have an R2 API token's access key ID and secret — ask me for
   them, then run:
   `mallard-cli storage add-r2 --wrangler --name "Team Backup" --bucket team-backup --access-key-id <id> --secret-access-key-stdin`
5. Confirm it printed a storage_id and that Mallard's Storages list now shows it.
```

Step 1 is the load-bearing part for Task 4: the pasted instruction carries
the "install wrangler if missing" step itself, rather than `mallard-cli`
attempting that silently. Steps 2–3 are optional — if the agent skips them
(or wrangler is never installed), `mallard-cli storage add-r2` still works
with plain `--account-id`/`--bucket` flags and no wrangler dependency at
all. This instruction is deliberately OS-agnostic prose, not a literal
shell script — an agent naturally translates it into PowerShell on Windows
or bash/zsh on macOS/Linux without the plan needing two versions of it.

Once Task 6 lands, the same request can skip the "ask me for the key/secret"
step entirely, given the one-time bootstrap token already exists (e.g. in
`CF_API_TOKEN`):

```
Set up a new R2 storage called "team-backup" in Mallard, fully automated —
I've already got a Cloudflare bootstrap API token in $CF_API_TOKEN:
1. Check whether `wrangler` is installed; install it with `npm install -g
   wrangler` if not, and make sure it's logged in.
2. Run:
   `mallard-cli storage add-r2 --wrangler --cf-api-token-stdin --name "Team Backup" --bucket team-backup <<< "$CF_API_TOKEN"`
3. Confirm it printed a storage_id and that Mallard's Storages list now shows it.
```

No dashboard visit, no pasted key/secret — the only human input ever needed
was creating that one bootstrap token, once, from the dashboard's "Create
additional tokens" template (Task 6).

### Safety checklist (cuts across every task that touches the write path)

- Reuse `StorageConfigV3::validate()` — don't reimplement id/name rules.
- Reject duplicate storage ids and duplicate (bucket, account_id, endpoint)
  tuples under different ids (Task 1).
- Preserve the existing atomic write + `0600`/`0700` perms — don't hand-roll
  a writer that skips them.
- Cross-process `fd-lock` around every read-modify-write, on every platform
  (Task 1, Task 5).
- Never echo `secret_access_key` to stdout/logs, anywhere — and, once Task 6
  lands, never echo the Cloudflare bootstrap API token either.
