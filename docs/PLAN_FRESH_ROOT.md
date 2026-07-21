# Plan: Fresh Custom Roots — Layout Fix + "Set Up Claude Here" Button

Status: IMPLEMENTED 2026-07-08 (85 tests green). Deviations: the sidebar
empty-root state got no extra button (the settings card covers it); the
old `mount_name_is_cosmetic` contract was replaced by container semantics
and its test updated accordingly.

Covers the reported failure (custom local dir without `.claude`: pull spills
Claude's files flat into the folder, and the root is invisible before the
first pull) and the follow-up idea: a per-root button that (re)installs a
working Claude setup into the chosen root.

Builds on PLAN_SYNC_LINKS.md (mount table `Roots`, lib.rs:592) and
PLAN_PLUGIN_SYNC.md (`repair_plugins`, settings.json as install intent).

## What happened

`Roots` maps logically: `.claude/history.jsonl` → `<claude_root>/history.jsonl`
— the mount dir *is* the dot-dir. Picking `~/myconf2` therefore dumps
`settings.json`, `projects/`, `plugins/`… bare into `myconf2/`, and the same
folder can never host `.codex` too. Second failure: `read_source`
(lib.rs:913) returns `None` when the mount dir doesn't exist yet, so a fresh
custom root disappears from the sidebar until after the first pull.

## Verified enabler (probed on this machine)

```
CLAUDE_CONFIG_DIR=/tmp/x claude plugin list   # works; materializes .claude.json in /tmp/x
```

Claude Code fully honors `CLAUDE_CONFIG_DIR`. So a custom root is not just a
sync mirror — with that env var it is a **live, launchable Claude config
dir**, and `claude plugin marketplace add` / `install` run with it will
install marketplaces, cache, and manager state *into the root*. Bonus: when
redirected, `.claude.json` (MCP servers — the "known gap" in
AGENT_SYNC_FILE_SETS.md) lives inside the root too.

## Part A — Container mounts (the layout fix)

1. **Auto-nest by root name** in `Roots::from_config` (lib.rs:600), the
   single choke point: if the custom path's last component is not the root
   name, treat it as a container — effective mount `<picked>/<root>`.
   - `myconf2` → `myconf2/.claude/…`; one container can host both roots
     (the overlap check already permits siblings).
   - Escape hatch unchanged: a path literally named `.claude` stays flat.
   - Config stores what the user typed; only resolution changes.
   - Existing flat spills (like today's `myconf2`) become inert leftovers —
     documented manual cleanup, no auto-migration.
2. **Fresh-mount visibility**: `read_source` returns the source with empty
   entries (honest custom-path label) instead of `None` when the dir is
   missing; pull `create_dir_all`s the effective mount up front so an empty
   profile still leaves a coherent directory.
3. **Effective-path preview** in `RootLinkCard`: hint under the input —
   "syncs into `~/myconf2/.claude`" — mirroring the same ends-with rule in
   TS, so the layout is never a surprise.

## Part B — Per-root "Set up Claude here" button

Shown on the `.claude` root card (and the sidebar empty-root state) when the
root looks unbootstrapped: mount dir missing/empty, or enabled plugins from
the synced `settings.json` are not installed *in that root*.

One backend command, `setup_root(root)`:

1. Create the effective mount dir if missing.
2. **Pull that root** (existing per-root pull path) so `settings.json`,
   `CLAUDE.md`, `projects/`, etc. arrive.
3. **Repair plugins into the root**: generalize `repair_plugins` to take the
   root and run every `claude plugin …` child with
   `CLAUDE_CONFIG_DIR=<effective mount>` when the mount is custom (default
   mount = env untouched, today's behavior). Intent is read from the
   mount's `settings.json`; presence checks run against the mount's
   `plugins/` records instead of hardcoded `~/.claude` (fixes a latent
   home-vs-mount mismatch in yesterday's repair code).
4. Stream everything into the log panel; per-step report. Explicit click
   only — plugins execute arbitrary code.

Missing `claude` binary ("missing claude" case): the same button detects it
(login-shell `command -v claude` already exists) and offers the CLI install
as its first step — runs `npm install -g @anthropic-ai/claude-code` via the
login shell, streamed, with the manual command shown on failure. No version
pinning.

Finish with a hint, not magic: to *use* the root, launch
`CLAUDE_CONFIG_DIR=~/myconf2/.claude claude` — shown once in the log tail
(copyable), since the app can't set another process's environment.

## Tests (existing Machine/stub harness)

- Container path resolves to `<container>/.claude`; explicit `.claude`-named
  path stays flat; both roots share one container past the overlap check.
- Fresh nonexistent custom mount: pull lands files under
  `<container>/.claude/…`; `read_source` shows the root before any pull.
- `repair` presence checks read the mount's `plugins/` records, not
  `~/.claude` (fixture with diverging mount vs home).
- setup_root on an already-healthy root is a no-op (idempotent).
- CLI-spawn paths (env var plumbing, npm install) stay untested — network +
  real binary; manual click-through once.

## Out of scope

- Auto-running setup after pull (consent stays a click).
- Auto-migrating existing flat spills.
- Launching Claude from the app / persisting `CLAUDE_CONFIG_DIR` shell
  config for the user.
- A `.codex` equivalent (codex has no `CODEX_HOME`-style plugin repair to
  drive; revisit if needed).

## Order

1. Part A in lib.rs + tests → `cargo test`.
2. Repair generalization (root + env) + `setup_root` + tests.
3. UI: preview hint + button + empty-root state → `npm run build`.
4. PLAN_SYNC_LINKS.md cross-reference + AGENT_SYNC_FILE_SETS.md note
   (`CLAUDE_CONFIG_DIR` closes the `.claude.json` MCP gap for custom roots).
