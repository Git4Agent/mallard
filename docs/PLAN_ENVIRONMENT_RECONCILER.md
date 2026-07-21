# Plan: Restore Codex Plugins After Pull

> Implementation note (2026-07-13): the original target-already-initialized
> assumption below was disproven for custom `CODEX_HOME` profiles. The shipped
> implementation follows `PLAN_CODEX_PLUGIN_RESTORE_HARDENING.md`: all managed
> plugin IDs are portable intent, managed catalogs are resolved from this
> machine and provisioned during explicit Repair, and `config.toml` syncs as a
> portable projection plus target-local overlay.

## Scope

Limit this feature to **Codex plugins only**.

After pulling a `.codex` profile onto another machine, one explicit click
should reinstall the missing enabled Codex plugins through Codex's own CLI.

Included:

- capture the portable identity of installed and enabled Codex plugins;
- capture all enabled managed plugin IDs plus portable Git marketplace sources;
- compare desired plugins with the target machine;
- add missing marketplaces;
- install missing plugins;
- verify the final installed/enabled state; and
- report authentication, hook-trust, version-drift, and restart follow-ups.

Not included:

- Claude plugins;
- standalone skills;
- Homebrew, npm, uv, Python, or other binary dependencies;
- a dependency solver or package database;
- arbitrary config-path rewriting outside the narrow managed config codec;
- plugin updates, downgrades, removal, or exact-version resolution;
- connector/MCP credential transfer; or
- automatic trust of plugin hooks.

This is a small, low-to-medium difficulty feature: approximately **2-4
engineer-days** including tests and one fresh-machine manual verification.

## Why This Is Enough

The installed Codex CLI (`0.144.0-alpha.4`) already exposes the required
machine-readable operations:

```text
codex plugin list --json
codex plugin marketplace list --json
codex plugin marketplace add <path|owner/repo|git-url> [--ref <ref>] --json
codex plugin add <plugin>@<marketplace> --json
```

Codex plugins already package skills, MCP server definitions, apps/connectors,
hooks, and assets. Agent Sync therefore only needs to replay plugin intent; it
does not need to understand or separately install the contents of a plugin.

Codex installs payloads into its own target-machine cache and records plugin
enable/disable state in `~/.codex/config.toml`. The payload remains
target-owned, but reinstall alone does not repair every Codex-generated local
marketplace or MCP path. Agent Sync therefore projects known machine-local
tables out of cloud bytes, rebinds only validated managed tables during the
explicit Repair action, and never copies cache paths through the profile.

References:

- https://developers.openai.com/codex/plugins
- https://developers.openai.com/codex/plugins/build

## Desired-State File

Store one portable companion file inside the synced `.codex` root:

```text
~/.codex/agent-sync/codex-plugins.lock.json
```

It is an ordinary cloud-profile file, so the existing manifest/head CAS already
versions and publishes it atomically with the rest of the `.codex` profile. No
new `_environments/` cloud object or head schema is needed.

The app captures this file immediately before a `.codex` push. If capture
fails, keep the last valid lock, continue the normal file push, and report that
plugin intent was not refreshed. Never replace a good lock with an empty or
partial capture.

The lock is a generated, canonical JSON file: sorted marketplaces and plugins,
stable field order, trailing newline, and no absolute paths or credentials.

Suggested schema:

```json
{
  "schema": 1,
  "captured_with": {
    "codex_version": "0.144.0-alpha.4"
  },
  "marketplaces": [
    {
      "name": "team-tools",
      "source": {
        "kind": "git",
        "repository": "owner/repo",
        "git_ref": "4f2c0d9b6f..."
      }
    }
  ],
  "plugins": [
    {
      "id": "my-plugin@team-tools",
      "observed_version": "1.4.2"
    }
  ],
  "manual": [
    {
      "id": "local-helper@personal-local",
      "reason": "local marketplace source is not portable"
    }
  ]
}
```

`observed_version` is informational in v1. `codex plugin add` installs the
version present in the target marketplace snapshot and does not accept an
explicit plugin version. For a custom Git marketplace, an immutable Git commit
is the reproducibility boundary.

## Capture Rules

Build the desired set from:

```text
codex plugin list --json
codex plugin marketplace list --json
```

Capture only plugins where:

```text
installed == true && enabled == true
```

Apply these marketplace rules:

| Marketplace/source | Capture behavior |
|---|---|
| `openai-bundled` | Record `plugin@openai-bundled`; never record its local source. |
| `openai-primary-runtime` | Record `plugin@openai-primary-runtime`; never record its local source. |
| `openai-curated` | Record `plugin@openai-curated`; never record its local source. |
| Configured Git marketplace | Record repository plus immutable ref when available. |
| Custom local path | Put plugin in `manual`; never copy the absolute path. |

Also exclude:

- installed but disabled plugins;
- cached but uninstalled plugin versions;
- marketplace snapshot directories;
- `~/.codex/plugins/cache/**`;
- `.tmp` marketplace data;
- OAuth tokens, connector state, environment values, and other credentials;
- hook trust decisions; and
- absolute `source.path` values returned for bundled/runtime plugins.

Capture is a union-safe snapshot of desired installed plugins. It does not
claim ownership of every plugin on the target machine.

## One-Click Repair Flow

After Pull, read the lock and compare it with the target:

```text
Pull .codex
    |
    v
Read codex-plugins.lock.json
    |
    v
codex plugin list --json
codex plugin marketplace list --json
    |
    v
Footer: Plugins 3
    |
    v explicit click
Resolve managed catalogs -> Add Git marketplaces -> Add plugins
    -> Repair managed MCP paths -> Verify every requested ID -> New task hint
```

Detailed flow:

1. Find `codex` through the login shell, then known fallback locations.
2. Run `codex --version` and confirm the plugin commands are available.
3. Parse and validate `codex-plugins.lock.json`.
4. Union the lock with enabled managed plugin entries from target
   `config.toml` so already-published v1 profiles recover omitted intent.
5. Resolve required `openai-curated`, `openai-bundled`, and
   `openai-primary-runtime` sources from the machine's explicitly selected
   default Codex home. Validate reserved name, approved root, marketplace
   manifest, and the curated Git-HEAD sidecar.
6. For a custom home, provision a target-owned curated catalog copy and rebind
   bundled/runtime local tables. Never trust a pulled absolute source path.
7. Read installed plugins and configured marketplaces through the JSON CLI.
8. Produce an in-memory plan containing:
   - missing marketplaces;
   - missing plugins;
   - already present plugins;
   - installed version drift;
   - manual local-source entries; and
   - malformed/unavailable entries.
9. Add missing Git marketplaces and verify their exact source/ref.
10. Run `codex plugin add <plugin>@<marketplace> --json` for each missing plugin.
11. Continue after individual failures so one bad plugin does not block the rest.
12. Rebuild only fingerprint-matched managed MCP paths for the selected target.
13. Re-run inventory and verify every requested plugin, including entries that
    were initially present or blocked during planning.
14. Return an explicit `ready`, `partial`, or `failed` state; warnings or manual
    requested items can never produce a verified Ready report.
15. Show installed/present/blocked/failed/manual counts and tell the user to
    start a new Codex task before using newly installed plugin capabilities.

The click is the consent boundary: plugins and their bundled MCP servers/hooks
can execute code. Do not run repair automatically as a side effect of Pull.

## Idempotency And Conflict Policy

Applying the same lock repeatedly must be a no-op:

- existing marketplace -> skip;
- installed and enabled plugin -> skip;
- missing marketplace -> add once;
- missing plugin -> add once;
- different installed version -> report drift, do not update in v1;
- extra target-machine plugin -> preserve it;
- plugin listed in `manual` -> report it, do not guess a path;
- malformed entry -> skip and report it.

V1 never removes or disables plugins. This matches the app's union-oriented
sync behavior and avoids destructive cross-machine surprises.

## Config And Path Handling

Do not implement a path-rewrite engine for this feature.

The native Codex installer should own:

- the target cache directory;
- the installed plugin version directory;
- marketplace snapshots;
- plugin enable state in `~/.codex/config.toml`;
- plugin-relative MCP resolution; and
- `PLUGIN_ROOT` / `PLUGIN_DATA` runtime variables.

Agent Sync owns the portable plugin lock plus a narrow semantic codec for
`config.toml`. The codec removes all local marketplace tables and only
fingerprint-matched Codex-managed `node_repl` / `computer-use` blocks from
portable bytes, then composes them from the target-local overlay on pull and
explicit Repair. If a custom plugin or marketplace contains a hard-coded
source-machine path, classify it as unsupported/manual. Do not search-and-
replace arbitrary TOML, JSON, hook commands, project paths, or plugin files.

## Security Boundaries

- Explicit click only; never install during Pull.
- Display plugin and marketplace IDs in the log before running them.
- Accept only the structured source forms supported by `codex plugin
  marketplace add`.
- Prefer immutable Git commit refs for custom marketplaces.
- Never persist credentials, bearer tokens, OAuth state, or environment values.
- Never sync or restore the plugin cache.
- Never auto-trust bundled hooks. Codex separates installation from hook trust;
  preserve that boundary.
- Pass CLI arguments directly through `std::process::Command`; never construct a
  shell command from lock-file content.
- Cap lock size, marketplace count, plugin count, string lengths, and child
  process timeouts.
- Redact child output before writing it to persistent logs.

## Implementation Shape

Keep this separate from the existing Claude repair code:

```text
src-tauri/src/codex_plugins.rs
  CodexPluginLock
  CodexMarketplaceIntent
  CodexPluginIntent
  CodexPluginPlan
  CodexPluginRepairReport
  capture_lock(...)
  build_plan(...)
  apply_plan(...)
  find_codex_binary(...)
  run_codex_json(...)

src-tauri/src/codex_config.rs
  project_portable_bytes(...)
  compose_physical_bytes(...)
  rebind_managed_marketplaces(...)
  repair_managed_mcp_from_default(...)
  inspect_managed_config(...)
```

Use a small command-runner trait so tests can provide recorded JSON and failure
results without touching the real Codex installation or network.

Suggested Tauri commands:

```text
capture_codex_plugin_lock() -> CodexPluginLock
get_codex_plugin_plan() -> CodexPluginPlan
repair_codex_plugins() -> CodexPluginRepairReport
```

Capture should also run automatically immediately before a `.codex` push, but
the explicit command is useful for inspection and testing.

Frontend types:

```text
CodexPluginPlan
CodexPluginRepairReport
```

UI behavior:

- show a compact footer button only when the pulled `.codex` profile has a
  valid lock and the plan contains missing/manual/drift items;
- label it `Plugins` with a numeric badge for missing plugins;
- one click runs repair and opens the existing log panel;
- use terse terminal states such as `Installed`, `Partial`, or `Ready`; and
- keep marketplace names, versions, failures, and new-task guidance in the log.

## Delivery Plan

### 1. Lock capture and parsing — 0.5-1 day

- Add lock structs and canonical serialization.
- Parse the two Codex JSON inventory commands.
- Filter disabled/uninstalled entries while retaining managed plugin IDs
  without their machine-local source paths.
- Convert local-path marketplaces to manual entries.
- Write atomically and preserve the last valid lock on failure.
- Force-include the lock as companion metadata in `.codex` pushes.

### 2. Repair backend — 0.5-1 day

- Add binary discovery and JSON command runner.
- Build the missing/present/drift/manual plan.
- Replay missing marketplaces and plugins.
- Verify final state and stream logs.
- Register the Tauri commands.

### 3. UI — 0.5 day

- Add plan/report types.
- Add `Plugins` footer action and missing-count badge.
- Reuse existing busy state, log panel, and short status copy.

### 4. Tests and fresh-machine check — 0.5-1.5 days

- Run Rust tests, `cargo check`, and `npm run build`.
- Exercise one curated and one Git-marketplace plugin on a disposable/fresh
  Codex profile.
- Confirm a second click is a no-op and a new Codex task sees the plugin.

## Test Matrix

Automated tests:

- canonical output is stable regardless of CLI array ordering;
- bundled, curated, and primary-runtime plugin IDs are captured without local
  source paths;
- enabled installed plugins are captured; disabled/uninstalled ones are not;
- Git marketplace source/ref round-trips;
- local-path marketplace becomes a manual entry with no absolute path leaked;
- malformed/oversized JSON is rejected without replacing the last good lock;
- an old/missing lock can recover explicit enabled managed intent from
  `config.toml`;
- missing or invalid managed catalogs produce structured blocked items and
  never a verified success;
- curated provisioning validates manifest name, Git HEAD, and sidecar and is
  idempotent;
- unknown target catalog directories are never replaced;
- managed marketplace and MCP paths are projected out of uploaded config and
  rebuilt for the selected target;
- `.codex/.tmp/**` and `.codex/plugins/cache/**` remain absent even when opted
  in or present in a legacy manifest;
- missing Codex binary/version/plugin command produces a clear blocked result;
- existing marketplace/plugin is skipped;
- missing marketplace is installed before its plugin;
- one plugin failure does not stop later plugins;
- version difference reports drift without update;
- extra target plugin is preserved;
- repair twice performs no second install;
- child arguments are passed without a shell;
- output redaction and process timeout work; and
- lock capture is included with a selective `.codex` push.

Manual test:

1. Install a portable curated or Git-marketplace plugin on machine A.
2. Push the `.codex` profile and inspect the lock for paths/secrets.
3. Pull on a fresh machine B with Codex installed but no plugin cache.
4. Confirm the footer shows the missing plugin count.
5. Click **Plugins** once.
6. Start a new Codex task and exercise a bundled skill or MCP capability.
7. Click **Plugins** again and confirm a no-op.

## Acceptance Criteria

- A valid enabled portable Codex plugin on machine A can be installed on
  machine B after Pull with one explicit click.
- The target uses its own Codex cache/config paths.
- No plugin cache, absolute source-machine path, or credential is synced.
- Bundled/runtime plugin intent is replayed against validated target-machine
  managed sources.
- Local-only marketplaces are reported without being guessed or copied.
- Partial failures are visible and retryable.
- Repeated repair converges to a no-op.
- Newly installed plugins are available after starting a new Codex task.

## Final Recommendation

Implement this directly on top of the native `codex plugin` CLI and the
existing `.codex` profile manifest. Do not add an RPM-like environment layer
or binary adapters. The portable lock, narrow config projection/local overlay,
and target-owned managed-catalog resolver are the smallest design that handles
fresh custom Codex homes without copying machine-local caches.
