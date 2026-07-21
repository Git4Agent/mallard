# Plan: Portable Agent Setup and Post-Pull Readiness (v2, condensed)

Status: **IMPLEMENTED** (2026-07-12) — Phase 1 (`.codex/agents` allowlist,
override opt-in) and Phase 2 (`readiness.rs`, `get_setup_readiness`,
`local-state.json`, mark-reviewed/dismiss commands, Finish setup UI) are in
the working tree; 122 Rust tests + frontend build green.
Date: 2026-07-12

Condensed rewrite of `PLAN_PORTABLE_AGENT_SETUP.md`, reorganized as
decisions-first. Where the two differ, **this file wins**: the portable
setup lock (`.codex/agent-sync/setup.lock.json`,
`.claude/agent-sync/setup.lock.json`) is dropped from Phases 1–2 (D8) and
moved to Follow-up A. The `~/.agent-sync` storage layer this plan builds on
(`PLAN_GLOBAL_AGENT_SYNC_DIR.md`) is **implemented** as of 2026-07-12; the
work below is not started.

## 1. What this plan does

Today Agent Sync reliably syncs files and conversations between machines.
After a pull, however, the target machine may still be missing things the
files *refer to*: plugins, skill sources, MCP logins, trusted hooks, project
folders. This plan adds:

1. **File-set parity** — sync the config path currently missing from the
   allowlist that carries portable setup (`.codex/agents/**`), plus an
   opt-in for `AGENTS.override.md`.
2. **A read-only readiness scan** — after pull, parse the machine's own
   synced files (raw configs, agent TOMLs, skills, plugin locks,
   conflict-copy siblings) and report what is present in the cloud's file
   set but not yet usable locally. No new synced artifact is introduced.
3. **One "Finish setup" UI surface** — a single badge/panel replacing
   scattered setup buttons, listing readiness issues with explicit actions.
4. **Clearly-defined local persistent data** — two machine-local files
   under `~/.agent-sync/` (Section 4): the implemented `machine.json`
   registry and a new `local-state.json` readiness memory. Neither ever
   syncs.

Nothing in Phases 1–2 installs, trusts, logs in, or rewrites anything.
The only mutating actions remain the existing, explicit plugin repair
commands — plus explicit local bookkeeping (mark-reviewed, dismiss) that
writes only `local-state.json`.

Background reference (ChatGPT's import flow, which inspired the readiness
categories): <https://learn.chatgpt.com/docs/import> and the linked
skills/subagents/MCP/hooks pages.

## 2. Decisions

Each decision below is settled; do not relitigate during implementation.

**D1 — Two roots, not three.** `.codex` and `.claude` remain the only
logical roots and cloud profiles. Do NOT add `~/.agents` as a third root.
Rationale: the Codex docs place user skills at `~/.agents/skills`, but the
observed real-world content there is symlinks into plugin repositories
(machine-local absolute targets). The upload walker skips symlinks by
design, so a `.agents` profile would sync nothing useful; plugin repair
already recreates those skills by reinstalling their source plugin.
Personally authored regular directories under `~/.agents/skills` have not
been observed; syncing them is Follow-up B.

**D2 — Sync `~/.codex/agents/**` (custom subagents).** Codex stores
personal custom-agent definitions as TOML files in `~/.codex/agents/`
(required fields: `name`, `description`, `developer_instructions`). This is
a plain file directory, distinct from skills; add it to the default
allowlist like `.claude/agents`.

**D3 — Three kinds of state stay separate.**

- *Portable content*: file bytes useful on another machine (conversations,
  instructions, regular skill dirs, agent TOMLs). Synced as raw files.
- *Portable intent*: a normalized, secret-free declaration of what should
  exist. In v1 this is **only the two plugin locks**, which already exist,
  sync, and merge.
- *Machine readiness*: derived local facts (binary resolves, login present,
  path exists) plus a small local memory of user decisions
  (`local-state.json`). **Never syncs.**

**D4 — Raw config stays lossless; readiness only reads it.** Never
structurally merge `config.toml` or `settings.json`; they keep today's
Tier 3 conflict-copy behavior. The readiness scan parses these files in
place and never rewrites them.

**D5 — Setup is non-destructive.** Pulling setup never deletes or disables
existing local setup. Repair adds missing items only; it never uninstalls,
overwrites same-name/different-source entries, trusts hooks, or deletes
local MCP servers. Conflicting intent is surfaced, never resolved by
last-writer-wins.

**D6 — `AGENTS.override.md` never default-syncs.** It is a deliberately
temporary override, and union semantics never propagate deletions — a
default-synced override would resurrect forever after removal. Detect it,
warn `Active override`, and offer it only as an explicit per-remote opt-in.
Base `AGENTS.md` remains default-synced.

**D7 — Symlinks never sync as content.** The walker keeps
`follow_links(false)`. Skill symlinks are diagnosed locally (target
missing, plugin-backed, external); their *intent* does not travel between
machines in v1 — a skill that exists only as a symlink on machine A creates
no expectation on machine B. Plugin-backed skills reach other machines
through the plugin locks and plugin repair.

**D8 — No setup lock in v1.** The previously proposed
`.codex/agent-sync/setup.lock.json` / `.claude/agent-sync/setup.lock.json`
are not needed and are not built. Everything they would carry is already
derivable on the target machine after a pull: MCP servers, hooks, and
custom agents arrive in the raw synced files; plugin intent arrives in the
plugin locks; conflicts arrive as `*.sync-conflict-*` siblings, which the
sync engine already guarantees are lossless. Dropping the lock deletes
Phase 2's largest component (schema, validation, canonical serialization,
Tier 2 merge driver) and adds no user-visible gap. A portable setup lock
returns in Follow-up A only if repair replay needs a merged intent record;
the implemented `~/.agent-sync` remap already reserves its physical home.

**D9 — Readiness memory is local and explicit.** Reviewed-hook hashes and
dismissed issues live in `~/.agent-sync/local-state.json` (Section 4). The
file is updated only by explicit user actions in the app (mark-reviewed,
dismiss), never by the scan itself, and never syncs — trust and
acknowledgment are per-machine by design.

**D10 — No partial-history restore.** Full history always pulls. A
"recent work first" mode was considered and rejected; see Deferrals.

## 3. Files and sync changes (Phase 1)

### 3.1 Allowlist additions

In `lib.rs`:

- Add `.codex/agents` to `DEFAULT_SYNC_DIRS`.
- Keep `.codex/AGENTS.override.md` outside defaults. Expose it as a
  per-remote opt-in through the existing optional-data mechanism (no new
  selection surface).

No new `DEFAULT_SYNC_FILES` entries: the setup locks are dropped (D8) and
the plugin locks are already allowlisted. Root cardinality stays at two: no
`SyncConfig`, `ProfileLink`, `Roots`, or profile-discovery changes.

### 3.2 Unchanged behavior to preserve

- `follow_links(false)` in file collection; symlink entries skipped.
- Tier 3 conflict-copy behavior for raw config files.
- Never-tier hard denial (credentials, `installation_id`, live SQLite,
  plugin workspaces) — stronger than any opt-in or lock intent.
- Existing plugin-lock capture/merge/repair, untouched.
- The implemented `~/.agent-sync` layer: the `Roots` remap of logical
  `.{root}/agent-sync/**`, the invisibility and pre-push cleanup of legacy
  in-root `agent-sync/` files, and the structurally unsyncable
  `machine.json` registry.

## 4. Local persistent data

Everything the app persists on a machine, in one place. The two files under
`~/.agent-sync/` sit at the top level, **outside** the remapped
`codex/`/`claude/` subtrees, so `Roots::rel` maps them to no logical path —
they structurally cannot enter a manifest, be pushed, or be opted in.

```text
~/.agent-sync/
  machine.json        # IMPLEMENTED. Per logical root: local_path and the
                      # linked profile_id. Rewritten on config save, push,
                      # and pull. Informational mirror only — never read
                      # back as configuration.
  local-state.json    # NEW (Phase 2). Machine-local readiness memory:
                      #   reviewed_hooks:   { "<normalized-hash>": reviewed_at }
                      #   dismissed_issues: ["<issue id>", ...]
                      # Written via temp-file + rename, only on explicit
                      # user actions (mark-reviewed after the native /hooks
                      # flow, dismiss issue). The scan reads it; it never
                      # writes it. Prune entries whose hook/issue no longer
                      # exists locally, so it cannot grow unbounded.
  codex/              # remapped logical .codex/agent-sync/**  (plugin lock)
  claude/             # remapped logical .claude/agent-sync/** (plugin lock)
```

Unchanged, in Tauri app data (also machine-local, already outside the
agent roots): `sync_config.json` (configuration authority), per-profile
baselines, pull backups.

Update rules:

- `machine.json` — regenerated whole on config save / push / pull
  (implemented behavior; this plan adds no new writers).
- `local-state.json` — read by every readiness scan; mutated only by the
  two explicit actions above; contains hashes and issue ids only, never
  secrets, env values, or absolute paths from other machines; losing the
  file re-raises issues but loses no data.

## 5. Readiness scan (Phase 2)

One read-only Tauri command, `get_setup_readiness`. Idempotent; performs no
installs, writes, logins, or trust changes (it does not even write
`local-state.json` — only the explicit actions do). Derivation logic lives
in a new `src-tauri/src/readiness.rs`, Tauri-free so tests can run it on
filesystem fixtures.

Inputs — all local, nothing fetched:

- raw synced files: `config.toml`, `hooks.json`, `settings.json`,
  `agents/**`, `skills/**`, `commands/**`, `prompts/**`;
- the two plugin locks plus the existing plugin plan helpers;
- `*.sync-conflict-*` siblings under the config files and behavior
  directories (the engine's lossless conflict representation);
- the cloud manifest cache (for cloud-side entries with no local file);
- `local-state.json` (reviewed hashes, dismissals).

```rust
struct SetupReadiness {
    generated_at: u64,            // local display only; never synced
    roots: Vec<RootReadiness>,
    issues: Vec<SetupIssue>,
}

struct SetupIssue {
    id: String,
    root: String,
    category: String,             // plugins | skills | mcp | hooks | agents | conflicts | paths
    severity: String,
    title: String,
    detail: String,
    source_path: Option<String>,
    action: String,
}
```

`action` values: `repair_codex_plugins`, `repair_claude_plugins`,
`attach_project`, `open_mcp_setup`, `review_hooks`, `mark_reviewed`,
`resolve_conflict_copy`, `dismiss`, `manual`.

### Checks by category

**Plugins** — aggregate the existing `CodexPluginPlan` results (missing /
present / drift / disabled / manual / blocked) for both agents through a
small internal interface on `codex_plugins.rs`. No second plugin inventory.
No lock-schema changes there.

**Skills** — local diagnostics only (D7): regular dir with readable
`SKILL.md` → ready; symlink with readable target → ready; symlink target
missing → issue (plugin-backed target → point at plugin repair; external
target → manual).

**Custom agents** — TOML exists and parses; required fields present;
referenced `skills.config.path` values resolve; embedded MCP definitions
feed the MCP checker.

**MCP** — non-secret checks over the local raw configs: stdio command
resolves via the existing login-shell-aware binary lookup; cwd/referenced
files exist; required env-var names present locally (values never read into
logs); remote URL syntactically valid; authorization requirement surfaced.
Use native CLI inventory/login commands only after verifying their exact
versioned surface — do not depend on an unverified `--json` flag.

**Hooks** — parse `hooks.json` and supported inline hook tables; normalize
and hash each definition; a hash absent from `local-state.json`'s
`reviewed_hooks` → `trust_review` issue with a `review_hooks` handoff to
the native `/hooks` flow, then an explicit `mark_reviewed` action records
the hash. Never transfer or synthesize trust; review state never reaches
another machine (D9).

**Conflicts** — every `*.sync-conflict-*` sibling under config files or
behavior directories becomes a `resolve_conflict_copy` issue naming both
paths. Resolution stays manual (open both in the editor); readiness only
surfaces the pair. This replaces the dropped setup-lock variant model: the
siblings *are* the variants.

**Prompts/commands** — report (never rewrite): absolute paths from another
home directory, missing referenced scripts, shell interpolation, required
arguments, non-portable agent-specific syntax.

**Project paths** — detect that a transcript's project directory does not
exist locally and emit a manual `attach_project` issue. Attachment
persistence and remapping are Follow-up A.

## 6. UI (Phase 2)

### 6.1 Finish setup

Replace scattered setup buttons with one footer action, shown only when
readiness has actionable or warning items:

```text
Finish setup  3
```

Opening it shows compact categories, not raw file paths:

```text
Plugins       1 missing       [Repair]
Skills        1 broken link   [Review]
Connections   1 sign-in       [Finish]
Hooks         1 changed       [Review]
Conflicts     1 to resolve    [Open]
Projects      1 not attached  [Attach]
```

Footer copy stays terse; detailed diagnostics and command output stay in
the activity log. Build a focused readiness component; do not grow the
footer button logic. `App.tsx` reloads readiness after startup, pull,
setup, config save, and repair. `types.ts` gains the readiness/issue types.
Dismiss and mark-reviewed are per-issue actions that write
`local-state.json` and refresh the scan.

### 6.2 Active override

When `AGENTS.override.md` exists locally or in cloud: show `Active
override` in the Codex root details, keep its sync toggle off by default,
explain that removing it on one machine will not remove cloud copies, and
require explicit selection before upload.

## 7. Security gates (acceptance requirements, not hardening)

- No credential value in any readiness payload, log line, conflict detail,
  or local-state entry. Env-var names may be checked; values may not be
  logged or stored.
- `local-state.json` holds hashes and issue ids only, and never syncs
  (structurally, per Section 4).
- Absolute symlink targets never sync; readiness never copies or uploads a
  symlink target.
- Same-name/different-source plugin entries keep blocking repair
  (existing plugin-plan behavior).
- Pulled executable content never runs automatically. Hook trust is local
  and hash-bound, never copied from cloud.
- Never-tier rules stay stronger than any opt-in.
- Readiness parsers use bounded reads and item counts against hostile or
  corrupted files (reuse the plugin lock's bounded-read patterns).
- Logs redact bearer tokens, API keys, authorization headers, and
  secret-looking assignments.

## 8. Tests

### Unit (pure, `readiness.rs` + allowlist)

- `.codex/agents/**` included by default; `AGENTS.override.md` detected but
  excluded by default.
- Readiness output deterministic for a fixed filesystem fixture.
- Hook normalization/hash stable across JSON object ordering; unreviewed
  hash → `trust_review`; hash present in `local-state.json` → no issue.
- `local-state.json` round-trip; pruning drops entries for hooks/issues
  that no longer exist; scan never writes the file.
- Conflict-copy siblings under config files and behavior dirs produce
  `resolve_conflict_copy` issues naming both paths.
- MCP checks read env-var names without logging values; malformed
  `config.toml`/`settings.json` degrades to an issue, not an error.
- Custom-agent parser reports missing required fields and unresolved paths.
- Bounded reads: oversized or deeply-nested input files produce a bounded
  issue, not a hang or crash.

### Integration (dual-backend: local + stub-S3, existing harness)

- A Codex custom agent syncs A→B and reports ready on B.
- Same agent name edited differently on two machines → conflict-copy
  sibling on the second push → `resolve_conflict_copy` issue on both.
- A broken skill symlink yields a readiness issue without copying target
  bytes or creating a symlink.
- A changed hook stays untrusted (`trust_review`) until mark-reviewed
  locally; the review does not propagate — the other machine still sees its
  own `trust_review` issue.
- Existing plugin-lock merge/repair unchanged and visible in aggregated
  readiness.
- A missing project path produces a manual Attach issue without writing
  any mapping or touching transcripts.
- `machine.json` and `local-state.json` never appear in a manifest, push,
  or opt-in surface.
- Credentials and Never-tier paths cannot be included through any setting.

### Frontend and repo checks

```sh
npm run build
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --lib
```

Finish setup absent at zero issues, badged otherwise; each action opens the
right flow; dismiss/mark-reviewed persist across restarts; override warning
visible but not noisy; errors in the log, terse footer.

## 9. Delivery

**Phase 1 — file-set parity.** Section 3 (`.codex/agents` allowlist entry,
override opt-in), documentation updates (`AGENT_SYNC_FILE_SETS.md`,
`README.md`), dual-backend tests. No readiness surface, no raw config
behavior change.

**Phase 2 — read-only readiness + Finish setup.** Sections 4–6:
`readiness.rs`, `get_setup_readiness`, `local-state.json`, the Finish setup
UI. No new synced artifact, no new merge driver, no new mutating command
beyond the existing plugin repair plus the two local bookkeeping actions.

Doc updates: `DESIGN2.md` only where eligibility changes; document
`local-state.json` alongside `machine.json` in
`PLAN_GLOBAL_AGENT_SYNC_DIR.md`'s layout when built.

## 10. Follow-ups (require re-approval after Phase 2)

**A — Explicit finish actions and the setup lock.** Skill source selection
and safe install/relink; native MCP login/setup after verifying exact CLI
support; project attachment persistence and UI (identity:
`sha256("git:" + normalized_remote)` when a safe remote exists, else
`sha256("cwd:" + encoded_original_cwd)`; mappings live in app data, never
in the synced profile); hook review handoff; user-confirmed path remapping.
If repair replay needs a merged cross-machine intent record, this is where
the portable setup lock returns — logical
`.{root}/agent-sync/setup.lock.json` through the implemented remap, with
the variant-per-payload conflict schema and no recapture-varying metadata,
as specified in the original plan §5. Every action remains independently
consented and idempotent.

**B — User-authored `~/.agents/skills` sync.** Only if personally authored
regular skill directories appear there. Decide then between a third
`.agents` profile and mapping into an existing one; plugin-provided
symlinks stay owned by plugin repair.

**C — Cross-agent import adapters.** Claude commands → Codex skill drafts,
Claude agents → Codex TOML drafts, compatible MCP definitions → target
intent, unsupported fields → review report. Own design required; not to be
smuggled into Phases 1–2.

## 11. Deferrals (decided against for now)

- The portable setup lock (dropped from v1 by D8; returns only via
  Follow-up A).
- Recent-work-first / partial-history restore. Full history always pulls;
  any future partial mode needs a persistent eligibility policy on both
  push and pull, not a pull-time skip.
- Tombstone/removal semantics for setup intent; automatic
  uninstall/disable propagation.
- Full semantic merge of `config.toml` / `settings.json`.
- Credential escrow or secret-manager integration.
- Automatic Git cloning for projects or symlinked skills.
- Transcript JSONL path rewriting.
- Syncing `~/.agents` (see Follow-up B), `/etc/codex/skills`, or
  OpenAI-bundled skills.
- A generic package/dependency manager.

## 12. Acceptance criteria

Done when:

- A new machine pulls conversations and portable setup for `.codex` and
  `.claude` with no credentials or machine identities copied.
- Personal Codex custom agents and regular user skills restore; symlinked
  skills never leak external contents or absolute paths to cloud.
- Missing plugins, broken skill links, MCP auth/env, hook review, agent
  parse problems, conflict copies, and project paths all appear in one
  readiness surface.
- Plugin repair stays explicit and idempotent; no pulled hook is trusted or
  executed automatically; hook review state stays per-machine.
- Same-name/different-source conflicts stay visible and lossless via
  conflict copies, and each one surfaces as a readiness issue.
- `AGENTS.override.md` is never silently made permanent across machines.
- `machine.json` and `local-state.json` are the only new local persistent
  files, both under `~/.agent-sync/` and both unsyncable by construction.
- Existing profiles work unchanged, no migration.
- All unit, dual-backend integration, frontend build, and Rust checks pass.
