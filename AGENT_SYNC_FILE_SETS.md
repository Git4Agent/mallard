# Agent Sync File Sets

Default restore/sync file sets for Codex (`~/.codex`) and Claude Code
(`~/.claude`), plus the merge policy for files that can safely converge across
machines. Sync mechanics (state matrix, per-storage opt-ins, SQLite snapshot
path) are defined in `DESIGN2.md`; this document only decides *which* paths
sync and *how* concurrent changes reconcile.

The tiers below are an allowlist. A path not listed here does not sync by
default; it enters a profile only through an explicit per-storage opt-in (opting a
file into one destination never opts it into another — PLAN_MULTI_STORAGE.md),
and the **Never** tier is hard-denied even for opt-ins. Real trees carry plenty
of unlisted machine state (`models_cache.json`, `generated_images/`,
`worktrees/`, `config.toml.bak-*` backups) — the default answer for all of
it is no. `.codex-global-state.json` is Never-tier outright (it mixes UI
state with machine/account identity); its portable subset travels via the
app-generated sidebar lock instead.

## At A Glance

| Tier | Codex `~/.codex/` | Claude `~/.claude/` |
|---|---|---|
| **Required** — conversations | `sessions/**`, `archived_sessions/**`, `session_index.jsonl`, `history.jsonl` | `projects/**` |
| **Optional** — conversation-adjacent | — | `history.jsonl`, `file-history/**`, `todos/**` |
| **Optional** — behavior/config | `memories/**`, `skills/**`, `rules/**`, `prompts/**`, `agents/**`, `AGENTS.md`, `hooks.json`, `config.toml`, `agent-sync/codex-plugins.lock.json`, `agent-sync/codex-sidebar.lock.json` | `CLAUDE.md`, `agents/**`, `commands/**`, `skills/**`, `keybindings.json`, `settings.json`, `plugins/config.json`, `agent-sync/claude-plugins.lock.json` |
| **Never** | `auth.json*`, `installation_id`, `.codex-global-state.json*`, `.tmp/**`, `plugins/cache/**` | `.credentials.json`, `settings.local.json`, `sessions/**`, `plugins/repos/**`, `plugins/marketplaces/**` |

Merge policy in one line: almost everything converges by file-set union;
three JSONL indexes and the three app-generated locks get a deterministic,
bounded auto-merge; config files and
shared memory files block and ask (`MEMORY.md` gets a best-effort entry merge
first, and derived memory summaries are regenerated); SQLite never merges.

## Codex

### Required For Conversation Restore

```text
~/.codex/sessions/**              # transcripts, nested by date
                                  # (YYYY/MM/DD/rollout-*.jsonl)
~/.codex/archived_sessions/**     # archived transcripts, same file format,
                                  # flat layout (not date-nested)
~/.codex/session_index.jsonl      # resume picker index (id, thread_name,
                                  # updated_at); app-pruned — see Tier 2
~/.codex/history.jsonl            # prompt history; not the transcript source,
                                  # but cheap and expected in a full restore
```

**Thread index restores by rebuild, not by sync.** `state_5.sqlite` (thread
ids, names, previews, git metadata, tasks) stays out of portable sync: it
holds absolute rollout paths and version-dependent runtime tables, and Codex
rebuilds it from the files above on its next `thread/list` (verified on
0.144.1). Pull restores each file's original modification time from the
manifest (`source_mtime`), so the rebuilt index shows real recency instead
of pull-time. The per-remote SQLite-snapshot opt-in remains a same-machine
disaster-recovery path only — it is NOT the portable restore path
(PLAN_CODEX_THREAD_REBUILD_AND_SIDEBAR.md).

### Optional Behavior And Configuration

```text
~/.codex/memories/**              # see memory caveat below
~/.codex/skills/**
~/.codex/rules/**
~/.codex/prompts/**               # custom prompts, if present
~/.codex/agents/**                # personal custom-agent TOMLs (name,
                                  # description, developer_instructions)
~/.codex/AGENTS.md
~/.codex/hooks.json               # review/redact; may contain absolute paths
~/.codex/config.toml              # synced as a portable projection; see below
~/.codex/agent-sync/codex-plugins.lock.json
                                  # app-generated portable plugin intent,
                                  # captured before every .codex push
                                  # (PLAN_ENVIRONMENT_RECONCILER.md); Tier 2.
                                  # Logical path only — stored on disk at
                                  # ~/.agent-sync/codex/, never inside the
                                  # root (PLAN_GLOBAL_AGENT_SYNC_DIR.md)
~/.codex/agent-sync/codex-sidebar.lock.json
                                  # app-generated portable sidebar subset
                                  # (projects, order, thread titles, display
                                  # prefs) of the never-synced
                                  # .codex-global-state.json; Tier 2, applied
                                  # only via the explicit Finish-setup action
                                  # (PLAN_CODEX_THREAD_REBUILD_AND_SIDEBAR.md).
                                  # Same ~/.agent-sync/codex/ storage
```

**`config.toml` uses logical bytes.** The active physical file is split into a
portable projection plus a target-local overlay. Local marketplace tables and
recognized Codex-generated `node_repl` / `computer-use` MCP blocks stay on the
machine; model, feature, plugin enablement, Git marketplace, project, custom
MCP, and unknown tables remain portable. Status, baseline hashes, manifests,
uploads, and conflict copies all use the portable projection. Pull composes
incoming portable content with the selected target home's current local
overlay. A malformed config fails closed and is never uploaded as raw bytes.

Marketplace snapshots and installed payloads are target-owned state, not
portable profile data. `.codex/.tmp/**` and `.codex/plugins/cache/**` are hard
Never paths even when a remote opt-in or conflict-copy name would otherwise
match them. A later push also removes legacy entries for these paths from an
existing cloud manifest.

**Override caveat.** `~/.codex/AGENTS.override.md` is deliberately NOT
default-synced, only offered as a per-remote opt-in: it is a temporary
active override, and union sync never propagates deletions — a
default-synced override would resurrect forever after removal on another
machine (PLAN_PORTABLE_AGENT_SETUP_V2.md D6). Base `AGENTS.md` default-syncs.

**Memory caveat.** `memories/**` alone may not carry the full memory
experience: the store is `~/.codex/memories_1.sqlite`, which is
default-excluded as runtime SQLite state with live sidecars. If memory
continuity matters, make that file a per-remote opt-in and upload it through
the SQLite snapshot path. Never sync `-wal`, `-shm`, or `-journal` sidecars.
The tree may also contain things that must not ride along — a
hand-initialized `.git` repository, `.tmp_codex/`, `.DS_Store` — all covered
by the Never categories, which apply *inside* opted-in directories too.

## Claude

### Required For Conversation Restore

```text
~/.claude/projects/**             # transcripts (<encoded-cwd>/<session-id>.jsonl),
                                  # per-session sidecars (<session-id>/tool-results/),
                                  # per-project memory (<encoded-cwd>/memory/)
```

Three things to know:

- **Path coupling.** Project directories are keyed by encoded absolute cwd
  (`-Users-hequ-Desktop-project-memory`). Resume on the remote machine only
  finds a conversation if the project sits at the same absolute path.
  Mismatched transcripts are inert, not harmful.
- **`~/.claude/sessions/**` is a trap.** Despite the name it is live process
  metadata (PID, process start time), not conversation data. Never sync it.
  The Codex tree is the opposite: there, `sessions/**` is the one thing you
  must sync.
- **`projects/**` is more than transcripts.** Alongside `<session-id>.jsonl`
  sit per-session `tool-results/` directories (session-scoped and union-safe,
  like the transcripts) and a per-project `memory/` directory — Claude Code's
  auto-memory, a `MEMORY.md` index plus note files edited over time on any
  machine. Memory is *not* disjoint by construction; it takes the memory
  merge policy in Tier 1, exactly like Codex `memories/**`.

### Optional Conversation-Adjacent State

```text
~/.claude/history.jsonl           # prompt/up-arrow history only
~/.claude/file-history/**         # /rewind snapshots; can get large
~/.claude/todos/**                # per-session task state, if present
```

### Optional Behavior And Configuration

```text
~/.claude/CLAUDE.md               # user-level instructions
~/.claude/agents/**               # custom subagents
~/.claude/commands/**             # slash commands
~/.claude/skills/**
~/.claude/keybindings.json
~/.claude/settings.json           # review/redact; env, apiKeyHelper, hooks, paths
~/.claude/plugins/config.json     # plugin install state; see below
~/.claude/agent-sync/claude-plugins.lock.json
                                  # app-generated portable plugin intent,
                                  # captured before every .claude push
                                  # (PLAN_CLAUDE_PLUGIN_LOCK.md); Tier 2.
                                  # Logical path only — stored on disk at
                                  # ~/.agent-sync/claude/, never inside the
                                  # root (PLAN_GLOBAL_AGENT_SYNC_DIR.md)
```

**Plugin caveat.** The settled default is `plugins/config.json` only, not a
`plugins/*.json` glob. Plugin restore rides on the synced
`agent-sync/claude-plugins.lock.json` (Tier 2 keyed union, so intent merges
across machines instead of conflict-copying — settings.json is Tier 3 and
cannot carry cross-person intent); the app's **Repair** button replays the
lock through `claude plugin marketplace add` / `claude plugin install` on
the target machine, falling back to `settings.json`'s `enabledPlugins` +
`extraKnownMarketplaces` when no lock exists yet. The manager's own records (`installed_plugins.json`,
`known_marketplaces.json`) are deliberately not offered for sync — they
embed machine-local absolute paths, and overwriting another machine's
copies corrupts its manager state. `blocklist.json` is a re-fetched cache
that never syncs. `plugins/repos/**` and `plugins/marketplaces/**` are the
manager's clone/update workspaces — Never tier; `plugins/cache/**` (the
installed payloads) is available as an explicit opt-in for offline
one-pull restore, with the standing warning that plugins execute with user
privileges. See PLAN_PLUGIN_SYNC.md.

**Known gap.** Claude Code keeps MCP server definitions, per-project trust,
and account state in `~/.claude.json` — outside the allowed sync roots. A
behavior/config restore therefore carries Codex's MCP setup (it lives in
`config.toml`) but not Claude's; recreate Claude MCP servers on the remote
machine (`claude mcp add …`). Exception: a custom mount driven through
`CLAUDE_CONFIG_DIR` (the app's "Set up Claude here" flow) keeps
`.claude.json` *inside* the root, closing this gap for relocated setups.

## Never Default-Sync

Concrete paths:

```text
~/.codex/auth.json*               # credentials, including *.bak backups
~/.codex/installation_id          # machine identity
~/.claude/.credentials.json       # credentials (Linux; macOS uses the Keychain)
~/.claude/settings.local.json     # machine-local settings by design
~/.claude/sessions/**             # live process metadata
~/.claude/plugins/repos/**        # reinstallable clones
~/.claude/plugins/marketplaces/** # reinstallable clones
```

And by category: logs, caches, tmp directories, plugin caches, shell
snapshots, `session-env` directories, IDE lock files, daemon state, machine
identity files, OS junk (`.DS_Store`), `.git` directories inside the synced
trees (hand-initialized repos are common, e.g. in `~/.codex/memories/`),
secret-bearing backup siblings (`auth.json.bak-*`, `config.toml.bak-*`), and
any runtime SQLite database (plus its `-wal`/`-shm`/`-journal` sidecars)
unless explicitly opted in.

Credentials are recreated on the remote machine, never copied: run
`codex login` there (browser flow), or `codex login --device-auth` on
headless hosts — a device-code sign-in completed from any browser. Reserve
`printenv OPENAI_API_KEY | codex login --with-api-key` for setups that
deliberately use API-key auth; it is not the default restore path. Claude
Code re-authenticates on first run.

## Merge Policy

Three tiers, from most to least common:

1. **File-set union** — the default; no driver needed. Covers all
   transcripts, per-session state, and new files in behavior directories.
2. **Deterministic auto-merge** — three JSONL index files plus the Codex
   plugin lock, merged by bounded, byte-deterministic drivers.
3. **Block and ask** — config files. SQLite never merges at all.

Phasing: the `DESIGN2.md` profile layout implements union reconciliation on
both push and pull; see "Implemented Reconciliation" below. The Tier 2
drivers exist and run automatically. Tier 3 files do **not** block yet —
until a conflict UI exists they resolve losslessly via deterministic
conflict copies (local wins the path, the cloud version lands in a
`name.sync-conflict-<hash8>.ext` sibling). This tier allowlist is enforced
in code: Required + Optional tiers sync by default, unlisted paths need a
per-remote opt-in, the Never tier is hard-denied, and conflict-copy
siblings inherit the eligibility of the file they shadow.

### Implemented Reconciliation

The local baseline `B` is scoped per cloud profile: every record stores the
cloud-side sha256 observed at the last push or pull of that path, and pull
updates it too. Each sync reads `_head.json`, fetches exactly the manifest
it references (verified against `head.manifest_sha256`), validates every
path and object key (path-traversal-safe, allowed roots only,
case-collision pairs skipped), and classifies each path over `(L, C, B)`:

| State | Push | Pull |
|---|---|---|
| unchanged both sides | skip | skip |
| local changed only | upload | keep (reported "local ahead") |
| cloud changed only | apply locally (backup first) | apply locally (backup first) |
| both changed, same bytes | record converged | record converged |
| both changed, Tier 2 file | merge, write locally, upload | merge, write locally |
| both changed, other file | keep local at path, cloud → conflict copy, upload both | keep local at path, cloud → conflict copy |
| deleted on one side | restore (union never propagates deletions) | restore |
| explicitly resolved conflict copy | publish a CAS-protected manifest deletion | remove an unchanged review copy; keep a locally edited one |
| gone both sides | forget baseline entry | forget baseline entry |

So *push* is "download the conflict, resolve locally as a union, then
publish", and *pull* is the same union applied locally without publishing:

- **Concurrent push.** A pushes at 19:00; B pushes at 19:10 from an older
  baseline. B's push detects A's changes (manifest sha ≠ baseline sha),
  downloads them, unions them into `~/.codex`, then publishes — the cloud
  ends as union(A, B) and nothing of A's is overwritten blind.
- **Pull over local edits.** A pulled at 19:00, B pushed at 19:15, A keeps
  editing until 19:20 and pulls. Cloud-side changes apply, A's unpushed edits
  stay (local-ahead), diverged files merge or conflict-copy. A's next push
  publishes the union.

After a pull-side merge or conflict copy, the baseline pins the *cloud* side
(recorded sha = cloud, mtime 0), so the file keeps showing local-ahead
until a push publishes the union — no re-merge loop, no silent freeze.

Conflict-copy deletion is the narrow exception to union deletion semantics.
Readiness exposes an explicit **Resolve** action after the user folds the
wanted content into the main file. Resolve pins the reviewed sibling's full
SHA, publishes a manifest-only deletion plus a durable conflict-resolution
tombstone through the profile head CAS, then removes the local copy. Other
machines remove the sibling only when its logical bytes match that reviewed
SHA, even if their app-data baseline was reset; a locally edited variant stays
local-ahead and clears the stale tombstone when deliberately republished.

Safety in the same pass: overwritten files are copied to
`app_data/backups/<run>/` (last 10 runs kept), writes go temp-file + rename,
SQLite replacement clears `-wal`/`-shm`/`-journal` sidecars first, symlink
destinations are skipped, and the Never tier above is enforced in code (hard
denial, opt-in proof). Racing pushes are serialized by `DESIGN2.md`'s head
CAS: uploads land on keys unique to the attempt and become visible only when
the head pointer flips, so a lost race leaves orphans, not overwrites.

### Tier 1: File-Set Union

```text
~/.codex/sessions/**/*.jsonl      # nested by date: YYYY/MM/DD/rollout-*.jsonl
~/.codex/archived_sessions/*.jsonl
~/.claude/projects/**/*.jsonl
~/.claude/projects/**/tool-results/**
~/.claude/todos/**
~/.claude/file-history/<session-id>/**
```

These filenames embed a session UUID, are written by one machine, and freeze
when the session ends — so two machines produce disjoint file sets and
syncing the union *is* the merge.

The one edge case: the same session resumed on two machines before syncing
appends divergent continuations to the same UUID file. That is a true
conflict — keep both copies, rename one with a conflict suffix, and do not
try to weave the continuations together.

**Moves and retention.** Two normal operations delete "frozen" files. Codex
archiving *moves* a transcript from `sessions/` to `archived_sessions/`;
sync sees a delete plus a create across two roots. Treat a same-UUID pair
that way — propagate the deletion together with the create, without a
separate confirmation — or the other machine keeps both copies until someone
confirms. Claude Code's retention cleanup (`cleanupPeriodDays`) deletes old
transcripts outright; combined with confirm-to-propagate deletions, a pull
can resurrect files the local machine just cleaned, which then show as
`local deleted` again. Skip re-downloading union-tier files older than the
local retention window, and offer retention-age deletions for bulk
confirmation.

**Behavior directories.** New files under `skills/**`, `rules/**`,
`prompts/**`, `agents/**`, and `commands/**` (either tree) also union: a
skill added on one machine and a command added on another merge cleanly. But
the same relative path modified differently on both sides is a real conflict
— block and ask, exactly like config.

**Memory files.** Neither agent's memory is one class of file. For
`~/.codex/memories/**`:

- `rollout_summaries/**` and new note files under `extensions/ad_hoc/**` —
  per-session/per-note, disjoint by construction: file-set union.
- `MEMORY.md` — an index; needs structure-aware dedupe (merge by entry, not
  by line). If the driver cannot parse it, block and ask.
- `raw_memories.md` — never blind-concatenate; divergence blocks and asks.
- `memory_summary.md` — derived state; regenerate from the merged inputs
  where possible rather than merging text.

Claude's per-project memory, `~/.claude/projects/<encoded-cwd>/memory/**`,
follows the same rules: new note files union; its `MEMORY.md` index gets the
same entry-level merge or blocks; the same note file edited differently on
both sides blocks and asks.

### Tier 2: Deterministic Auto-Merge

**Bounded keyed-index union — `~/.codex/session_index.jsonl`:**

1. Parse each line as JSON; key records by `id`.
2. For duplicate `id`, keep the record with the later `updated_at`.
3. If `id` and `updated_at` match but content differs, break the tie by
   canonical JSON lexical order and log it.
4. Serialize sorted by `updated_at`, then `id`, truncated to the newest
   `index_cap` records (default 100 — the bound codex itself maintains).

The bound is not optional. Codex prunes this index in normal operation (a
live tree shows exactly 100 records covering a fraction of the on-disk
sessions, with archived sessions removed), so an unbounded union resurrects
pruned and archived entries and fights the app's own pruning — the merged
file never converges. Truncating after the deterministic sort keeps the
output byte-deterministic. Verify the exact cap against the pinned codex
version before shipping the driver.

**Prompt-history union — `~/.codex/history.jsonl` and
`~/.claude/history.jsonl`** (same rule; only the timestamp field differs —
`ts` for Codex, `timestamp` for Claude):

1. Treat as append-only; take the union of both sides' records.
2. Dedupe exact duplicate records.
3. Sort by the embedded timestamp, then by canonical record bytes.
4. Optionally cap merged length if old-history resurrection becomes a UX
   problem.

Clock skew across machines makes ordering approximate; that is acceptable for
prompt history. The history drivers need no three-way base because these two
files are append-only in normal operation — a two-way union suffices. If a
future agent version starts trimming them, give them the session index's
bounded-output treatment instead.

**Plugin lock union — logical `.codex/agent-sync/codex-plugins.lock.json`
and `.claude/agent-sync/claude-plugins.lock.json`** (physically stored under
the app-owned `~/.agent-sync/`, never inside the agent roots —
PLAN_GLOBAL_AGENT_SYNC_DIR.md)**:**

App-generated portable plugin intent (PLAN_ENVIRONMENT_RECONCILER.md,
PLAN_CLAUDE_PLUGIN_LOCK.md), recaptured immediately before every push of the
owning root — from `codex plugin list --json` for Codex, and from
`settings.json` + the plugin manager's local records for Claude (those
manager files themselves never sync; they are capture inputs only). Because
both machines regenerate the lock from their own installed state, "both
changed" is this file's normal condition, not the exception — without a
driver it would conflict-copy on every cross-machine push cycle and each
machine would only ever see its own plugin intent at the canonical path.
The shared driver takes the keyed union only when a marketplace name has the
same repository and Git ref on both sides: marketplaces by name, plugins by
id, manual entries by id, remaining safe collisions resolved by the
lexically greater entry, and `captured_with` by the greater version string.
If either side fails parsing or validation (including a future schema), a
same-name marketplace has a different source or ref, or the union crosses an
entry/byte cap, the driver declines the merge. The normal Tier 3 path then
keeps the complete local lock active and preserves the complete cloud lock as
a deterministic conflict sibling. Lock conflict siblings remain in the
remapped `~/.agent-sync/{codex,claude}` directory, are surfaced by readiness,
and are force-included with the next push of that agent root. Pre-push capture
pauses while one of these siblings exists so recapture cannot overwrite either
unresolved side. Successful output is the canonical serialization (sorted
entries, fixed field order, trailing newline).

**Sidebar lock union — logical `.codex/agent-sync/codex-sidebar.lock.json`**
(same `~/.agent-sync/` storage as the plugin locks)**:**

App-generated portable sidebar state
(PLAN_CODEX_THREAD_REBUILD_AND_SIDEBAR.md Part B), recaptured before every
`.codex` push from the never-synced `.codex-global-state.json` — capture
reads only the whitelisted keys (saved project paths + derived git origins,
project order filtered to local paths, thread titles, display prefs), so the
file's account/machine identity cannot ride along. Same regenerate-per-push
dynamics as the plugin locks, same driver shape: projects keyed by git
origin (else path), thread titles keyed by thread id, collisions to the
`Ord`-greater entry; `project_order` and the prefs object resolve
whole-value on collision (order is one preference list, not mergeable
data). Unparseable sides lose to parsing sides; canonical serialization
throughout. Applying the merged lock to a machine's desktop state is never
part of sync — it is the explicit, additive `apply_sidebar_state` action.

**Canonical output requirement.** Drivers must produce byte-identical output
regardless of which machine runs them: fixed ordering, fixed JSON
serialization, fixed tie-breakers, no whitespace or timestamp rewrites. Then
when both machines merge independently, the second one lands in the existing
`converged` state (`local_changed && cloud_changed && same_content`) and the
baseline advances without another conflict round.

### Tier 3: Block And Ask

Never auto-merge:

```text
~/.codex/config.toml
~/.codex/hooks.json
~/.codex/AGENTS.md
~/.claude/settings.json
~/.claude/CLAUDE.md
~/.claude/keybindings.json
~/.claude/plugins/config.json
```

These need a real three-way merge and routinely conflict semantically even
when text merges cleanly: both machines edit the same key, hook or MCP paths
are valid on only one machine, env or helper commands differ, instructions
contradict. Default policy: surface the conflict and let the user pick.
(`plugins/config.json` divergence usually just means the two machines
installed different plugins; a keyed union by repository is a plausible
future driver, but the default is still to ask.)

### SQLite

Never merge at the row or record level. If a runtime database is explicitly
opted in: upload via the SQLite backup/snapshot path, never sync sidecars,
and resolve conflicts through the existing conflict prompt (effectively
last-writer-wins). Do not infer schema-level merge behavior for Codex or
Claude private state.

### Pull-Time Safety

Merge drivers run only under the same safety boundary as pull: warn if Codex
or Claude appears to be running, avoid merging files an agent may be actively
appending to, write merged output via temp file plus rename, and back up
overwritten local files. A deterministic driver does not remove the
lost-append race on a live file — the idle-agent confirmation still matters.
