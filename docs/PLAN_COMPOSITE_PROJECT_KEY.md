# Plan: Composite Project Key (project root × provider config)

**Status:** implemented (2026-07-19)
**Date:** 2026-07-19
**Scope of this document:** architecture and implementation plan

Implementation deviations from the plan below:

- The sidebar config badge reuses the existing `v3-repository-kind` styling
  (like the "git" badge) instead of introducing a new CSS class.
- The setup workspace disables claimed configs directly in the profile
  dropdown ("(used by <project>)") rather than rendering a separate
  "this folder is already set up with …" callout; the finalize-time error
  covers the pending-path route.
- The default alias leads with the config name rather than the hostname
  (2026-07-19 follow-up): `"healthGame (conf2)"`, hostname only when no
  config is known, counter as last resort.

## 1. Outcome

A schema-3 project entry is currently keyed by its checkout folder alone: one
active binding per canonical project root. This plan changes the effective
project key to **(project root, provider config home)** so the same checkout
can be registered once per Codex/Claude config directory:

```text
key1: ~/Desktop/project/healthGame + ~/.codex
key2: ~/Desktop/project/game3      + ~/.codex
key3: ~/Desktop/project/healthGame + ~/conf2/.codex
key4: ~/Desktop/project/game3      + ~/conf2/.codex
```

Each key stays a fully independent project: its own `LocalProjectId`, its own
bundle, its own recipe, links, history, and restore state. Nothing about the
cloud layout changes. The sidebar distinguishes same-folder entries by their
config instead of a numeric suffix ("healthGame (hequ-mac) 2").

## 2. What already supports this (no change needed)

- `ProjectBinding.profile_ids` already records which provider profile (config
  home) each project uses; multiple Codex profiles per machine are already
  supported by the profile catalog (`ensure_default_provider_profiles`,
  profile probe dedup).
- Chat history, capture, and conversation audit all resolve the provider home
  **through the binding** (`resolve_project` in `chat_history.rs`,
  `capture_request_for_binding` in `commands.rs`), so two same-folder projects
  with different Codex homes naturally see disjoint session sets.
- Bundles are keyed by generated `BundleId`, never by path, so two projects on
  one folder cannot collide in storage.
- Per-project state (recipe bases, materializations, dependency applications)
  is keyed by `LocalProjectId`/`ReplicaId`, not by path.

## 3. The rule change

New uniqueness invariant for active bindings:

> No two active bindings may share the same **(canonical project root
> (case-folded), provider profile id)** pair.

Consequences:

- Same root + different Codex profiles → allowed (key1 vs key3).
- Same root + same profile → rejected (true duplicate).
- A binding with both Codex and Claude profiles claims the root once per
  profile. Example: project A binds healthGame with `{codex: ~/.codex,
  claude: ~/.claude}`; project B may bind healthGame with
  `{codex: ~/conf2/.codex}`, but B cannot also add `~/.claude` — that
  (root, profile) pair belongs to A.

## 4. Changes by file

### 4.1 `src-tauri/src/project_sync_v3/domain.rs` — invariant (core)

`MachineProjectState::validate` (~line 988): replace the `active_roots:
BTreeMap<String, &LocalProjectId>` map keyed by folded root with a set keyed by
`(folded_root, profile_id)`, built from each active binding's `profile_ids`
values. Update the error message ("projects '{a}' and '{b}' use the same
provider config for one checkout"). No schema bump: this is a relaxation plus
a new narrower check; existing on-disk state stays valid.

### 4.2 `src-tauri/src/project_sync_v3/commands.rs` — setup flow

- `create_setup_draft_with_repository` (~4288): delete the "this folder is
  already set up as project '…'" rejection. Instead, collect the profile ids
  already used by active bindings on this canonical root and:
  - exclude them from the convenience default (today: auto-select the sole
    Codex profile; new: auto-select the sole **unused** Codex profile);
  - keep resuming an existing draft by canonical root (one in-flight draft
    per folder — `ponytail:` simplification; parallel drafts for one folder
    are not worth the resume-key complexity).
- `resolve_draft_profiles` / `build_setup_transaction` (~4782): add a
  finalize-time precheck — for each chosen profile, if an active binding on
  the same canonical root already uses it, fail with a friendly error naming
  the existing project ("healthGame already syncs this folder with that Codex
  config; pick a different config or open the existing project"). The domain
  invariant in 4.1 remains the backstop.
- `validate_binding_request` (~3915): same precheck for the edit-binding path
  (changing a project's root or profile must not collide with a sibling).
  Today root collisions surface only as the opaque domain-validate error.

### 4.3 `commands.rs` — default alias

`default_local_alias` (~3506) currently emits `"{repo} ({hostname})"` and
dedupes with a counter, which is how "healthGame (hequ-mac) 2" appears. Change:
accept an optional config qualifier (the display name of the binding's Codex —
else Claude — profile) and emit `"{repo} ({config})"`:

1. `{repo} ({config})` — the config name abbreviated to its distinctive
   part ("conf2 · Codex" → "conf2", "Default Codex" → "Codex");
2. `{repo} ({hostname})` when no config is known (registration without a
   binding, e.g. adopting a remote bundle);
3. counter suffix as last resort (same repo + same config name).

Callers: setup finalization (`apply_setup_transaction` recovery fill-in,
~5007) and `register_local_project_with_repository` (~3536; no binding yet →
counter fallback as today).

### 4.4 Frontend

- `ProjectSyncV3.tsx` (~304): `LocalProjectSummary` already merges the
  binding's `profile_ids`; also merge `canonical_project_root` and resolved
  profile display names (the profile catalog is already loaded for the
  binding editor).
- `ProjectSidebar.tsx`: when ≥2 projects share a canonical root, render a
  small config badge (profile display name) next to the label; tooltip
  already shows the repo name, add the config path.
- `ProjectSetupWorkspace.tsx`: the folder picker no longer errors on an
  already-registered folder. Show "this folder is already set up with:
  ~/.codex" and preselect an unused config. Surface the new finalize error
  verbatim.
- `model.ts`: extend the summary type accordingly.

### 4.5 Tests (in `commands.rs` / `domain.rs` test modules)

- domain: two active bindings, same root, different Codex profiles →
  validates; same root + same profile → rejected; disjoint provider sets on
  one root → validates.
- commands: draft creation on an already-registered folder succeeds and
  defaults to the unused profile; finalize with a colliding profile fails
  with the friendly error; edit-binding collision fails; alias becomes
  "healthGame · conf2 (hequ-mac)"-style instead of counter.
- frontend: `npm run build` (no committed frontend framework beyond the
  integration tests; extend `tests/frontend` only if sidebar logic gets a
  helper worth asserting).

## 5. Known behaviors to accept (called out, not fixed here)

- **Project-scope files are shared by twins.** key1 and key3 both capture and
  can both restore `AGENTS.md` etc. into the same checkout. Their bundles
  normally agree (same working tree); they diverge only if the remotes
  diverge, and restore is already an explicit, per-action review — no extra
  guard added.
- **Same remote bundle from two twins** (both connecting to one remote
  `BundleId`) stays possible, as it is today for two checkouts; twins default
  to fresh bundles and nothing encourages sharing.
- No migration: existing configs/bindings remain valid unchanged.

## 6. Order of work

1. Domain invariant + domain tests (4.1).
2. Setup/binding prechecks + friendly errors + tests (4.2).
3. Alias qualifier + tests (4.3).
4. Frontend summary/badge/setup UX (4.4), `npm run build`.
5. Doc status flip to "implemented" with deviations noted, per repo
   convention.
