# Plan: Portable Agent Setup and Post-Pull Readiness

Status: **PLANNED**

Date: 2026-07-11

Implementation has not started.

Commitment boundary: Phases 1–2 are the implementation commitment in this
plan. Explicit repair actions, project attachment, user-authored
`~/.agents/skills` sync, and cross-agent conversion are follow-up proposals
that require a fresh design check after Phases 1–2 ship.

## 1. Goal

Extend Agent Sync from reliable file/conversation synchronization into a
portable agent-setup workflow that can restore the useful parts of an agent
environment on another machine without copying credentials, machine-local
paths, trust decisions, executable caches, or live runtime databases.

The target experience is:

1. A user pushes their agent profiles.
2. Another machine pulls them without overwriting unrelated local setup.
3. Agent Sync identifies anything that is present in the cloud but not yet
   usable on the target machine.
4. The app shows one **Finish setup** surface for plugins, skills, MCP servers,
   hooks, custom agents, and project paths.
5. Every action that installs code, authorizes a connection, or trusts a hook
   remains an explicit user action.

This plan is informed by ChatGPT's current import flow, which recognizes
instructions, settings, skills, plugins, projects, recent chats, MCP servers,
hooks, slash commands, and subagents, and then calls out items that need
authorization or manual follow-up:

- <https://learn.chatgpt.com/docs/import>
- <https://learn.chatgpt.com/docs/agent-configuration/agents-md>
- <https://learn.chatgpt.com/docs/build-skills>
- <https://learn.chatgpt.com/docs/agent-configuration/subagents>
- <https://learn.chatgpt.com/docs/extend/mcp>
- <https://learn.chatgpt.com/docs/hooks>

## 2. Product Boundary

This remains a cross-machine synchronization feature. It is not a general
environment manager and it is not a full Claude-to-Codex converter.

### Included

- Close the known gaps in the user-level Codex setup file set.
- Sync personal Codex custom-agent definitions from `~/.codex/agents`.
- Represent symlinked skills as intent instead of copying their targets;
  plugin-provided skills restore through plugin repair.
- Detect active instruction overrides without making temporary overrides
  silently permanent on every machine.
- Derive a secret-free portable setup-intent record from synced configuration.
- Add a post-pull readiness scan and explicit follow-up actions.
- Detect MCP authentication/environment dependencies without transferring
  credential values.
- Detect hooks that require review or have broken machine-local commands.
- Detect project-folder and Claude conversation path mismatches.

### Not included

- Copying `auth.json`, `.credentials.json`, OAuth tokens, API keys, Keychain
  entries, cookies, or other credentials.
- Copying persisted hook-trust decisions to another machine.
- Automatically executing or trusting a newly pulled hook.
- Automatically installing a skill, plugin, package, or repository after pull.
- Syncing plugin caches, marketplace clones, npm caches, Homebrew packages, or
  arbitrary binary dependencies by default.
- Syncing `~/.agents` as a third profile. Plugin-managed `~/.agents/skills`
  symlinks are restored by plugin installation; personally authored
  `~/.agents/skills` sync is a deferred follow-up.
- Copying repository source trees. Git or the user's existing source-management
  workflow continues to own project contents.
- Rewriting arbitrary shell scripts, prompt templates, or TOML/JSON config into
  another agent's format.
- Deleting another machine's setup merely because one machine removed a file.
- Making raw SQLite files multi-writer or mergeable.

## 3. Current State

The existing sync engine is the correct foundation and should not be replaced:

- `.codex` and `.claude` are independent cloud profiles.
- `_head.json` is the CAS-protected publish point.
- manifests, commits, and uploads are immutable.
- push and pull reconcile local/cloud/baseline state as a non-destructive
  union.
- known JSONL files and plugin locks have deterministic merge drivers.
- arbitrary divergent files become conflict-copy siblings.
- credentials, runtime databases, symlink destinations, and plugin-manager
  workspaces already have explicit safety treatment.
- Codex and Claude plugin intent already has lock capture, merge, readiness,
  and explicit repair flows.

The relevant gaps are in setup discovery and restoration, not in the cloud
publish protocol.

### Coverage matrix

| Capability | Current coverage | Gap addressed here |
|---|---|---|
| Conversations | Full Codex sessions and Claude projects | Claude path diagnostics; attachment is deferred |
| Global instructions | `.codex/AGENTS.md`, `.claude/CLAUDE.md` | Active `AGENTS.override.md` detection and review |
| Project instructions | Usually carried by Git | Detect selected project setup and missing project roots |
| User skills | `.codex/skills`, `.claude/skills` | Symlinked/plugin-provided skills restore via plugin repair; `~/.agents/skills` sync deferred |
| Custom agents | `.claude/agents` | `~/.codex/agents` |
| Plugins | Portable locks plus explicit Repair/Plugins actions | Fold into one readiness model |
| MCP | Codex definitions ride in `config.toml`; Claude has a known root gap | Secret-free intent, dependency/auth checks, manual finish flow |
| Hooks | User-level Codex/Claude config files sync | Trust review and command/path readiness |
| Commands/prompts | `.claude/commands`, `.codex/prompts` | Placeholder/path diagnostics only |
| Projects | No stable attachment model | Local attachment/remap workflow |

## 4. Load-Bearing Design Decisions

### 4.1 Keep three kinds of state separate

The implementation must distinguish:

1. **Portable content** — files whose bytes are useful on another machine,
   such as conversations, instruction files, regular skill directories, and
   custom-agent TOML files.
2. **Portable intent** — a normalized, secret-free declaration of what should
   exist, such as plugin identities, a symlinked skill's source identity, MCP
   server definitions, and required environment-variable names.
3. **Machine readiness** — derived local facts such as whether `node` exists,
   an OAuth login is present, a hook command resolves, or a project path is
   attached. Readiness never syncs.

Do not put machine readiness into the cloud manifest. It changes independently
on each machine and cannot participate in cross-machine merge semantics.

### 4.2 Do not add `.agents` as a third root

Current Codex documentation places user skills under `~/.agents/skills` in
addition to `~/.codex/skills`. That alone does not justify a third logical
root in Phases 1–2:

- Observed real-world content of `~/.agents/skills` is symlinks into plugin
  repositories (machine-local absolute targets). Raw file sync skips symlink
  entries by design, so a `.agents` profile would sync nothing useful there.
  Plugin repair already recreates those skills by reinstalling their source
  plugin.
- Personally authored regular directories under `~/.agents/skills` are the
  only content raw sync could carry, and none have been observed yet.

Phases 1–2 therefore keep exactly two roots and two profiles. Personally
authored `~/.agents/skills` sync is a deferred follow-up (Section 13); if it
is ever built, it needs its own decision between a third profile and mapping
the directory into an existing one.

### 4.3 Do not sync absolute symlink values

Codex follows symlinked skill directories, but the current upload walker uses
`follow_links(false)` and skips symlink entries. That behavior prevents path
escape and must remain the default for ordinary file traversal.

Add a dedicated skill inventory step instead:

- regular skill directory inside a synced `skills` directory -> sync its
  files normally;
- symlink to a target inside the same logical root -> record a safe relative
  link intent, but do not create it during ordinary pull;
- symlink to a known installed plugin payload -> rely on the plugin lock and
  mark the skill as satisfied when plugin repair installs it;
- symlink into a Git worktree -> record only a normalized repository identity,
  optional subdirectory, and observed revision when these can be discovered
  safely;
- absolute/local/unknown source -> mark `manual`; never upload the target
  merely because a symlink points to it.

Materializing an external symlink target into the cloud is a separate explicit
opt-in because the target may contain private source or executable code.

### 4.4 Raw config stays lossless; portable intent is separate

Do not structurally merge another person's entire `config.toml` or
`settings.json`. Those files mix portable preferences with machine-local paths,
environment values, MCP configuration, plugin enablement, and hooks.

Continue the current Tier 3 conflict-copy behavior for raw files. Before push,
also derive an app-generated setup lock containing only the portable subset.
The lock is used for inventory, merge, and readiness; it does not silently
replace the target machine's raw config.

### 4.5 Setup remains non-destructive

Importing or pulling setup must not delete or disable existing local setup.
Repair actions may add missing items, but they do not uninstall plugins,
remove skills, overwrite same-name/different-source entries, trust hooks, or
delete local MCP servers.

Conflicting portable intent is surfaced rather than resolved by last-writer
wins.

### 4.6 Temporary instruction overrides are not ordinary defaults

`AGENTS.override.md` is intentionally a temporary active override. The sync
engine's union semantics do not propagate deletion, so default-syncing the file
would make a removed override reappear indefinitely.

Policy:

- detect and display `~/.codex/AGENTS.override.md`;
- make it an explicit per-remote opt-in, off by default;
- show an `Active override` warning whenever it exists locally or in cloud;
- do not add it to `DEFAULT_SYNC_FILES` until explicit tombstone/removal
  semantics exist for this class of file.

Base `AGENTS.md` remains default-synced.

## 5. Portable Setup Lock

Create one generated lock per logical root, at logical paths
`.codex/agent-sync/setup.lock.json` and `.claude/agent-sync/setup.lock.json`.
Physically these live in the global `~/.agent-sync/` directory, never inside
the agent roots (see `PLAN_GLOBAL_AGENT_SYNC_DIR.md`):

```text
~/.agent-sync/codex/setup.lock.json    # logical .codex/agent-sync/setup.lock.json
~/.agent-sync/claude/setup.lock.json   # logical .claude/agent-sync/setup.lock.json
```

Plugin locks remain separate. They already have specialized capture and replay
semantics and should only be referenced from the setup lock.

### 5.1 Proposed schema

```json
{
  "schema": 1,
  "root": ".codex",
  "items": [
    {
      "kind": "custom_agent",
      "id": "reviewer",
      "variants": [
        {
          "variant_sha256": "sha256-of-canonical-variant",
          "source_path": "agents/reviewer.toml",
          "content_sha256": "...",
          "requirements": []
        }
      ]
    },
    {
      "kind": "mcp_server",
      "id": "figma",
      "variants": [
        {
          "variant_sha256": "sha256-of-canonical-variant",
          "portable": {
            "transport": "http",
            "url": "https://mcp.example/mcp",
            "required_env": ["FIGMA_OAUTH_TOKEN"]
          },
          "requirements": ["authorization"]
        }
      ]
    },
    {
      "kind": "hook",
      "id": "sha256-of-normalized-definition",
      "variants": [
        {
          "variant_sha256": "sha256-of-canonical-variant",
          "source_path": "hooks.json",
          "definition_sha256": "...",
          "requirements": ["trust_review", "command_check"]
        }
      ]
    }
  ],
  "manual": []
}
```

The exact schema should reuse the bounded-read, schema-version,
canonical-serialization, and charset-validation patterns already implemented
for plugin locks:

- maximum file size and item count;
- bounded string lengths;
- normalized relative paths only;
- sorted/deduplicated arrays;
- canonical JSON serialization;
- no unrecognized executable payloads;
- no environment values, headers containing secrets, tokens, cookies, or
  credential-file contents.

The setup lock adds new enum-typed `kind`, transport, requirement, and source
fields, so it must add strict validation for those enums; that validation is
new rather than inherited from the plugin-lock schema.

The serialized lock has no `generated_at`, capture timestamp, hostname, or
other recapture-varying metadata. It is regenerated before push and merged by
a Tier 2 driver, so a timestamp would guarantee permanent both-changed churn.
Capture time may be logged or returned as local readiness metadata, but it
must not enter the lock bytes.

Every item always contains a sorted, non-empty `variants` array. A single
variant is the normal case. More than one variant is the serialized conflict
representation; `conflicted` is derived as `variants.len() > 1` and is not a
second mutable field that could disagree with the array.

### 5.2 Item kinds in v1

- `custom_agent`
- `skill`
- `mcp_server`
- `hook`
- `command_or_prompt`
- `plugin_lock_ref`

Project attachments are machine-local and are not lock items in v1.

### 5.3 Capture rules

#### Codex

- `AGENTS.md`: raw file only; presence/hash may be inventoried.
- `agents/*.toml`: parse required identity fields and record file hash.
- `config.toml`:
  - record MCP server identity and secret-free transport fields;
  - record required environment-variable names, never their values;
  - record obvious absolute command/cwd/config paths as requirements;
  - record inline hook definitions by normalized hash;
  - reference the existing Codex plugin lock.
- `hooks.json`: record normalized definitions and local command requirements.
- `prompts/**`: record files whose content contains obvious absolute paths,
  shell interpolation, or positional placeholders so readiness can call them
  out; do not attempt to rewrite them.

#### Claude

- `CLAUDE.md`, `agents/**`, `commands/**`, and `skills/**`: inventory presence
  and hashes; bytes continue to sync through the normal allowlist.
- `settings.json`: capture secret-free MCP/hook/plugin intent and required
  local environment names; reference the Claude plugin lock.
- `.claude.json`: only inspect when it is inside an explicitly configured
  `CLAUDE_CONFIG_DIR` root. The default `~/.claude.json` remains outside the
  sync root and is never silently absorbed.

#### Skills (`.codex/skills`, `.claude/skills`)

- enumerate `skills/*` without following arbitrary symlinks;
- inventory regular directories by skill name and `SKILL.md` hash;
- classify symlinks by portable source category;
- put unknown/external targets in `manual` without leaking the absolute target
  into cloud data.

### 5.4 Merge rules

Merge items by `(kind, id)`:

- compute `variant_sha256` from canonical variant content, excluding the hash
  field itself;
- identical variant hash -> one variant;
- item present on only one side -> union it;
- same key with different payload -> retain all bounded variants sorted by
  `variant_sha256`; the item is conflicted when more than one variant remains;
- requirements -> sorted set union;
- manual entries -> keyed union;
- item removal does not propagate in v1.

Do not silently select one MCP URL, skill repository, hook definition, or
same-named custom-agent body over another. The readiness UI must show the
conflict and require a local choice.

This deliberately differs from the plugin-lock merge driver. Plugin locks use
a symmetric `Ord` maximum on same-key collisions because the collision fields
are observed install metadata and the native plugin manager remains the source
of truth. Setup-lock collisions can represent different executable commands,
MCP endpoints, hooks, or agent instructions; choosing one lexically would hide
a meaningful and potentially security-sensitive conflict.

Cap variants per item as well as items per lock. If a merge would exceed the
variant cap, preserve a deterministic bounded set and emit a readiness error;
never allow hostile cloud input to grow one logical item without bound.

## 6. Readiness Model

Add a read-only backend command that derives machine readiness from the
current local files, portable setup locks, plugin locks, and local filesystem:

```rust
struct SetupReadiness {
    generated_at: u64,
    roots: Vec<RootReadiness>,
    issues: Vec<SetupIssue>,
}

struct SetupIssue {
    id: String,
    root: String,
    category: String,
    severity: String,
    title: String,
    detail: String,
    source_path: Option<String>,
    action: String,
}
```

Suggested `action` values:

- `repair_codex_plugins`
- `repair_claude_plugins`
- `attach_project`
- `choose_skill_source`
- `open_mcp_setup`
- `review_hooks`
- `remap_path`
- `resolve_intent_conflict`
- `manual`

The command is idempotent and performs no installations, writes, login flows,
or trust changes.

### 6.1 Plugin checks

Reuse the existing Codex and Claude plugin plans. Readiness aggregates their
missing, present, blocked, version-drift, and manual results instead of
building a second plugin inventory implementation.

### 6.2 Skill checks

- regular directory exists and `SKILL.md` is readable -> ready;
- lock expects a skill but directory is absent -> missing;
- symlink exists and target is readable -> ready;
- symlink target is missing -> repair/manual;
- same skill name has conflicting sources -> blocked pending choice;
- source resolves only to an absolute path from another machine -> manual;
- skill depends on a plugin that is missing -> point to plugin repair rather
  than duplicating installation work.

### 6.3 Custom-agent checks

- source TOML exists and parses;
- required `name`, `description`, and `developer_instructions` fields exist;
- referenced `skills.config.path` values resolve or are marked for remap;
- embedded MCP definitions are passed to the MCP checker;
- same-name/different-hash definitions are surfaced as conflicts;
- sandbox or permission differences are informational, never auto-approved.

### 6.4 MCP checks

Only evaluate non-secret readiness:

- stdio command can be resolved using the same login-shell-aware binary lookup
  pattern used for plugin CLIs;
- configured cwd and referenced files exist;
- required environment-variable names are present locally, without logging
  their values;
- remote URL is syntactically valid;
- configuration indicates authorization is required or the native agent reports
  it as unauthenticated;
- same-name/different-transport/source is blocked as an intent conflict.

During implementation, use native CLI inventory/login commands only after
their exact versioned surface has been verified. Do not invent or depend on an
unverified `--json` flag.

### 6.5 Hook checks

- parse `hooks.json` and supported inline hook tables;
- normalize each definition and compute its trust-review hash;
- resolve obvious absolute command paths, interpreters, cwd values, and repo
  relative script paths;
- mark newly pulled or changed non-managed hooks as `trust_review`;
- never transfer or synthesize a trusted state;
- Finish setup should open instructions for the native `/hooks` review flow,
  not bypass it.

### 6.6 Prompt/command checks

Detect and report, but do not rewrite:

- absolute paths from a different home directory;
- missing referenced scripts;
- shell interpolation;
- positional or named arguments that require user input;
- agent-specific command syntax that is not portable to another agent.

## 7. UI Plan

### 7.1 Replace scattered setup buttons with one readiness entry point

Keep the underlying plugin commands, but present one footer action:

```text
Finish setup  3
```

It appears only when readiness has actionable or warning items. Opening it
shows compact categories rather than raw file paths:

```text
Plugins       1 missing       [Repair]
Skills        1 needs source  [Review]
Connections   1 sign-in       [Finish]
Hooks         1 changed       [Review]
Projects      1 not attached  [Attach]
```

Keep terse footer status. Detailed diagnostics and command output continue to
live in the activity log.

### 7.2 Active override treatment

When `AGENTS.override.md` is detected:

- show `Active override` in the Codex root details;
- leave its sync toggle off by default;
- explain that removing it on one machine will not remove cloud copies under
  current union semantics;
- require explicit selection before upload.

Use the existing per-remote opt-in mechanism for this file. Do not add a new
category-level selection surface in Phases 1–2; the file tree and existing
optional-data settings already own selection policy.

## 8. Deferred Follow-up: Project Attachments

Project source stays outside Agent Sync. Add a local attachment layer that
helps restored conversations and project-scoped setup find the correct folder.

### 8.1 Discovery

Build a project candidate list from:

- Codex session cwd/repository metadata when safely available;
- Claude encoded project directory names and transcript cwd fields;
- currently existing local repositories selected by the user;
- optional Git remote identity read from an attached repository.

Do not recursively scan the entire home directory by default.

### 8.2 Local attachment record

Persist per-machine mappings in app data, not in the synced agent profile:

```json
{
  "cloud_project_key": "sha256-of-source-identity",
  "observed_source_path": "/Users/a/work/project",
  "local_path": "/Users/b/src/project",
  "git_remote": "owner/repo",
  "attached_at": 0
}
```

Define the identity deterministically:

```text
source_identity = "git:" + normalized_git_remote     # when present
               or "cwd:" + encoded_original_cwd      # fallback
cloud_project_key = sha256(UTF-8(source_identity))
```

Git-remote normalization removes credentials/userinfo, query strings, and
fragments; normalizes supported SSH/HTTPS forms to the same host/repository
identity; and preserves enough host information to avoid conflating repositories
from different forges. If no safe remote is available, use the already-observed
encoded original cwd from the conversation store. Do not invent a random key,
because another machine could not independently match it.

The cloud profile may carry a secret-free project hint containing a display
name, stable key, and Git remote. It must not carry private filesystem paths
unless the user explicitly includes them; raw transcript content may already
contain paths, but new metadata should not expand that exposure.

### 8.3 Apply behavior

For the deferred attachment feature:

- detect a missing old project path;
- offer to attach an existing local folder;
- validate optional Git remote match;
- use the mapping for readiness and navigation;
- do not rewrite transcript JSONL in place.

Transcript relocation/reindexing is a later feature requiring format-specific
tests. Until then, report when Claude resume remains path-coupled even after a
project is attached.

Phase 2 only detects the mismatch and emits a manual `attach_project` readiness
issue. Persistence, folder selection, Git verification, and navigation changes
require a fresh implementation decision before this deferred section begins.

## 9. Backend Work

### `src-tauri/src/lib.rs`

- Add `.codex/agents` to `DEFAULT_SYNC_DIRS`.
- Keep `.codex/AGENTS.override.md` outside defaults; expose it as a deliberate
  optional entry.
- Add both exact lock paths to `DEFAULT_SYNC_FILES`:
  `.codex/agent-sync/setup.lock.json` and
  `.claude/agent-sync/setup.lock.json`. Do not allowlist any entire
  `agent-sync` directory. Their physical home is the global `~/.agent-sync/`
  directory, not the agent roots (`PLAN_GLOBAL_AGENT_SYNC_DIR.md`).
- Preserve `follow_links(false)` in normal file collection.
- Invoke setup-lock capture before each affected root push.
- Register a deterministic setup-lock merge driver.
- Add the read-only readiness command. Project-attachment persistence and
  mutation commands remain deferred.

Root cardinality stays at two: no `SyncConfig`, `ProfileLink`, `Roots`, or
profile-discovery changes are needed.

### `src-tauri/src/setup_intent.rs` (new)

- Setup-lock schema and validation.
- Canonical serialization.
- Capture adapters for Codex and Claude.
- Secret/path redaction and portable-field normalization.
- Deterministic merge with explicit variants.
- Readiness issue derivation shared by commands and tests.

Keep process spawning and Tauri event emission out of pure parsing/merge code.

### `src-tauri/src/codex_plugins.rs`

- No lock-schema rewrite.
- Expose existing plan/readiness helpers through a small internal interface so
  setup readiness can aggregate results.
- Keep explicit repair behavior and trust boundaries unchanged.

### Frontend

- `src/types.ts`: setup-readiness and issue types.
- `src/components/SyncPanel.tsx`: override opt-in using the existing
  per-remote optional-data mechanism.
- `src/App.tsx`: load readiness after startup, pull, setup, config save, and
  repair; add the Finish setup surface.
- Add a focused readiness component rather than expanding footer button logic
  further.
- Reuse the activity log for repair/install output.

### Documentation

- Update `AGENT_SYNC_FILE_SETS.md` with `.codex/agents`, override policy,
  setup locks, and merge rules.
- Update `README.md` with the Finish setup behavior.
- Update `DESIGN2.md` only where eligibility or merge-driver coverage changes;
  do not duplicate the full setup-lock specification there.

## 10. Security and Privacy Requirements

These are acceptance gates, not optional hardening:

- No credential value appears in a setup lock, readiness payload, log line,
  conflict detail, or cloud metadata.
- Environment-variable names may sync; values may not.
- HTTP header names may be inventoried only when useful; header values may not.
- Absolute symlink targets do not sync by default.
- A setup lock cannot reference a path outside its logical root as a file to
  apply.
- Same-name/different-source plugin, skill, MCP, or agent entries block repair.
- Pulled executable content is never run automatically.
- Hook trust is local and hash-bound; it is never copied from cloud.
- Project attachment verifies directory existence and optionally Git identity
  before being treated as ready.
- Existing Never-tier rules remain stronger than file-tree selection,
  per-remote opt-ins, or lock intent.
- Readiness scanners use bounded reads and item counts for hostile or corrupted
  cloud files.
- Logs redact bearer tokens, API keys, authorization headers, secret-looking
  environment assignments, and user-entered remote credentials.

## 11. Test Plan

### Pure unit tests

- `.codex/agents/**` is included by default.
- both exact setup-lock paths are included, while neighboring unknown
  files under each `agent-sync` directory stay excluded;
- `AGENTS.override.md` is detected but excluded by default.
- regular skill capture is deterministic.
- absolute external symlink target never appears in serialized lock.
- known plugin-backed symlink maps to plugin intent.
- unknown symlink becomes a manual item.
- setup-lock validation rejects oversized, malformed, absolute-path, duplicate,
  and secret-bearing input.
- setup-lock merge is commutative, associative, idempotent, and byte-stable.
- setup-lock capture contains no timestamp or other recapture-varying field.
- same-key/different-payload variants serialize in stable hash order and derive
  conflict state from variant count.
- same-key/different-payload becomes an explicit variant conflict.
- MCP capture preserves env names but removes values.
- hook normalization/hash is stable across JSON object ordering.
- custom-agent parser reports missing required fields and unresolved paths.
- readiness generation is deterministic for a fixed filesystem fixture.

### Integration scenarios on local and stub-S3 backends

- A symlinked external skill produces a readiness issue without copying target
  bytes or creating an unsafe symlink.
- A Codex custom agent syncs and becomes ready on B.
- Two machines define the same custom-agent name differently; both definitions
  remain recoverable and readiness reports a conflict.
- Pulling MCP intent does not overwrite B's local server config and reports
  missing authorization/environment requirements.
- A changed hook remains untrusted and produces a Review action.
- Existing plugin-lock merge/repair behavior is unchanged and appears in the
  aggregated readiness result.
- A project path mismatch produces a manual Attach action without writing a
  mapping or rewriting transcripts; attachment behavior is a deferred test.
- Credentials and Never-tier paths cannot be included through any category or
  optional setting.

### Frontend verification

- `npm run build`.
- Finish setup is absent at zero issues and badged at one or more issues.
- Each action opens the correct in-context flow.
- Active override warning is visible but not noisy.
- Detailed errors stay in the log; footer copy remains terse.

### Required repository checks

```sh
npm run build
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --lib
```

## 12. Delivery Phases

### Phase 1 — File-set parity

- Default-sync `.codex/agents`.
- Add optional `AGENTS.override.md` detection.
- Keep symlink entries excluded exactly as today; Phase 1 has no readiness
  surface and does not attempt to represent them.
- Update file-set documentation and dual-backend tests.

This phase closes the largest concrete coverage gaps without changing raw
config behavior or root cardinality.

### Phase 2 — Setup lock and read-only readiness

- Add setup-lock capture, validation, canonical serialization, and merge.
- Aggregate existing plugin plans.
- Add symlink inventory with manual readiness only; no automated Git repair.
- Add skill, custom-agent, MCP, hook, and path diagnostics.
- Expose `get_setup_readiness`.
- Add Finish setup UI with manual instructions where no safe native action
  exists.

No new installer beyond the existing plugin repair commands is required in
this phase.

## 13. Follow-up Proposals Requiring Re-approval

Phases 1–2 stop after read-only diagnosis plus the already-existing plugin
repair operations. The sections below are not part of the implementation
commitment and must be re-justified from usage observed after Phase 2.

### Follow-up A — Explicit finish actions

- Skill source selection and safe install/relink flow.
- Native MCP login/setup actions after verifying exact CLI support.
- Project attachment UI.
- Hook review handoff.
- Config path remapping helpers that edit only a user-confirmed field.

Every action remains independently consented and idempotent.

### Follow-up B — User-authored `~/.agents/skills` sync

Only if personally authored regular skill directories under `~/.agents/skills`
become common:

- decide between a third `.agents` profile and mapping the directory into an
  existing profile;
- reuse the symlink classification rules in Section 4.3;
- plugin-provided symlinked skills stay owned by plugin repair either way.

### Follow-up C — Optional cross-agent import adapters

Only if the product explicitly expands beyond cross-machine sync:

- Claude commands -> Codex/OpenAI skill drafts;
- Claude agents -> Codex custom-agent TOML drafts;
- compatible MCP definitions -> target-agent setup intent;
- unsupported fields -> review report, never guessed conversion.

This follow-up requires its own design and should not be smuggled into Phases
1–2.

## 14. Acceptance Criteria

The feature is complete when:

- A new machine can pull conversations and portable setup for `.codex` and
  `.claude` without copying credentials or machine identities.
- Personal Codex custom agents and regular user skills are restored.
- Symlinked skills never leak arbitrary external target contents or absolute
  paths into the cloud.
- Missing plugins, skills, MCP authorization/environment, hook review, custom
  agent dependencies, and project paths appear in one readiness surface.
- Plugin repair remains explicit and idempotent.
- No pulled hook is trusted or executed automatically.
- Same-name/different-source setup conflicts are visible and non-destructive.
- `AGENTS.override.md` is not silently made permanent across machines.
- Raw config conflicts remain lossless through conflict copies.
- Existing profiles remain readable and do not require migration.
- All unit, dual-backend integration, frontend build, and Rust checks pass.

## 15. Explicit Deferrals

- Tombstone/removal semantics for setup intent.
- Automatic uninstall/disable propagation.
- Full semantic merge of `config.toml` or `settings.json`.
- Credential escrow or secret-manager integration.
- Automatic Git cloning for project folders or symlinked skills.
- Transcript JSONL path rewriting.
- Syncing `~/.agents` (including personally authored `~/.agents/skills`);
  see Follow-up B.
- Syncing system/admin skills from `/etc/codex/skills`.
- Syncing OpenAI-bundled/system skills.
- A generic package/dependency manager.
- Recent-work-first / partial-history restore. Full history always pulls; any
  future partial-history mode needs its own design (a persistent eligibility
  policy on both push and pull, not a pull-time skip).

These can be revisited after the setup-intent and readiness model proves useful
without weakening the current non-destructive sync guarantees.
