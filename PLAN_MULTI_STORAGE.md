# Plan: Multiple Storages × Multiple Local Profiles (Link Matrix)

Status: IMPLEMENTED 2026-07-14 (226 tests green; deviations in §12).
Builds on: DESIGN2.md (profile layout, head CAS, union), PLAN_SYNC_LINKS.md
(logical namespace, pinned prefixes, container mounts). Lifts that plan's §9
limit "one link per root kind" — this is the multi-link feature it deferred.
PLAN_FRESH_ROOT.md's container-mount semantics carry over unchanged.

## 1. Goal

Today: one destination, two fixed roots, at most one cloud profile per root.
Target (per the settings mockup):

- **Storages** — a named list of destinations (e.g. `Personal` R2, `Team` R2,
  `Backup / NAS` local folder). Each is what today's whole destination config
  is: kind + connection fields.
- **Local profiles** — a named list of local agent roots. Fresh configs start
  with `~/.codex` and `~/.claude`; users may remove either or add more (e.g.
  `myconf/.claude` at a custom path). Each has a root kind and a mount path.
- **Links** — edges in the profile × storage matrix. A profile may link to
  several storages; a storage may hold several profiles. Settings shows the
  matrix; clicking a cell links/unlinks; selecting a linked cell offers
  **Pull** (storage → profile) and **Push** (profile → storage).

Push/pull/merge semantics per link are exactly today's per-root sync: same
union reconciliation, conflict copies, merge drivers, tier allowlist, CAS.

## 2. Load-bearing decisions

**2a. The cloud schema does not change.** A storage is a self-contained
universe of profiles (`CLOUD_SCHEMA_VERSION` stays 1; heads, manifests,
commits, uploads untouched). Links are purely client-side wiring. Two
storages may both contain a profile named `001/.claude` — they are unrelated
profiles that happen to share a name.

**2b. Per-link sync state is keyed by `(storage, cloud profile)`, not by
cloud profile id alone.** Baselines and cloud caches today live at
`baselines/{profile_id}` / cache keyed by `profile_id`. With one destination
that was unambiguous; with several it is wrong the moment two storages hold
the same prefix (trivially easy with pinned names like `001/.claude` — and
exactly the case 2a legitimizes). A stale baseline applied across storages
would misclassify every file. New key: `{storage_id}__{flattened profile
id}`. This also retires the latent v1 bug where switching destinations kept
old baselines readable under a rediscovered same-name profile.

**2c. `save_sync_config` stays the one mutation API.** The UI already
round-trips the whole config object and the backend already diffs saved vs
incoming (`remote_identity` guard) to drop per-remote state. Keep that shape:
the matrix UI edits one config blob; the backend diffs old → new per storage
id and per link to run cleanups (no `add_storage`/`remove_link`/… command
zoo). `set_sync_link` and `link_sync_profile` are absorbed into this and
removed.

## 3. Config schema v2

```rust
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SyncConfig {
    schema: u32,                        // 2
    storages: Vec<StorageConfig>,
    local_profiles: Vec<LocalProfile>,
    links: Vec<SyncLink>,
}

pub struct StorageConfig {
    id: String,      // random hex-8, stable for the storage's lifetime
    name: String,    // display: "Personal", "Team", "Backup / NAS"
    kind: String,    // "s3" | "local"
    // s3: bucket, access_key_id, secret_access_key, account_id,
    //     s3_endpoint, region — verbatim from v1
    local_dir: String,
    /// Per-storage opt-ins (v1 included_default_exclusions). Deliberately
    /// per-storage, not global: opting sensitive optional files into a
    /// Personal bucket must not leak them into a Team bucket.
    included_default_exclusions: Vec<String>,
    /// Per-storage conditional-write probe (v1 supports_conditional_writes).
    supports_conditional_writes: Option<bool>,
}

pub struct LocalProfile {
    id: String,      // random hex-8, stable
    root: String,    // ".codex" | ".claude"
    path: String,    // "" = ~/{root}; else mount with container semantics
}

pub struct SyncLink {
    profile: String,     // LocalProfile.id
    storage: String,     // StorageConfig.id
    cloud: ProfileLink,  // existing struct: cloud prefix, label, pinned, …
}
```

Notes:
- `LocalProfile` has no `name` field — display name derives from the path
  (`~/.codex`, `myconf/.claude`), same rule the mockup shows. One less thing
  to keep unique.
- `ProfileLink` is reused whole; its `root` must equal the local profile's
  `root` (constructor-enforced). Its decision table (pinned / unpinned /
  discover, from PLAN_SYNC_LINKS §5.3) is unchanged, evaluated against the
  link's storage.
- At most one link per `(profile, storage)` cell. Two same-kind profiles may
  link to the same storage — and (revised 2026-07: baselines are keyed per
  link, `(local profile, storage, cloud profile)`) even to the **same cloud
  profile**: each link keeps its own baseline, so it behaves exactly like a
  second machine syncing that profile. The former duplicate-target guard
  ("fight over baselines") is gone.
- The NEVER-sync tier stays global and hard-denied; per-storage opt-ins can
  widen only the Optional/opt-in tiers, exactly as today.

### 3.1 Clean break — no migration

There is no v1→v2 migration. `load_sync_config` accepts `schema: 2` only; a
file that doesn't parse as v2 (including any pre-v2 file) is treated as
unconfigured and the app starts with defaults: the two default local
profiles, no storages, no links. Users re-enter destinations once.
Cloud data is unaffected (2a) — after reconfiguring, existing cloud profiles
relink by discovery or by pinning their prefix, and the first sync
re-verifies everything by hash (no baseline yet): one slower scan, union
semantics, no data loss. Old `baselines/{profile_id}` files are never read
again (all lookups use the new key) — dead files, harmless.

### 3.2 Save-time diff cleanups (2c), replacing the `remote_identity` guard

`storage_identity(s)` = v1 `remote_identity` per storage. On save, diff by
storage id / link identity:

| change detected | cleanup |
|---|---|
| storage's identity fields changed | clear probe; for its links: clear resolved (unpinned) cloud ids + labels; delete their baselines + caches |
| storage removed | remove its links; delete their baselines + caches (cloud data untouched) |
| local profile removed | remove its links (+ baselines/caches); disk files untouched |
| link removed (cell unlinked) | delete its baseline + cache; cloud profile stays (orphan philosophy) |

Re-linking later is safe by construction: no baseline means the hash path
re-verifies everything against the cloud manifest — one slower scan, union
semantics, no data loss.

## 4. Local side: `Roots` becomes a single-profile mount

Engine operations are per-link now, so `Roots` (today a `{codex, claude}`
pair) becomes the mount of **one** local profile:

```rust
struct Roots {           // name kept — ~34 call sites keep compiling
    home: PathBuf,
    root: String,        // ".codex" | ".claude"
    dir: PathBuf,        // resolved mount (container semantics unchanged)
    agent_sync: PathBuf, // per-profile remap target, see below
}
impl Roots {
    fn for_profile(p: &LocalProfile, home: PathBuf) -> Result<Roots, String>;
    // abs()/rel()/dir() keep signatures; they now serve one root and
    // return None / fall back for the other kind (per-link ops are already
    // filtered by path_matches_root, so nothing feeds them foreign paths)
}
```

**Per-profile `agent-sync` remap — correctness, not cosmetics.** Today
logical `.{root}/agent-sync/**` maps to the flat, kind-global
`~/.agent-sync/{codex,claude}/**`. Two `.claude` profiles would share one
physical plugin/sidebar-lock directory and cross-contaminate captures. New
rule:

- default-path profile (`path == ""`): `~/.agent-sync/{slug}` — unchanged,
  existing locks keep working;
- custom-path profile: `~/.agent-sync/{slug}.{local_profile_id}`.

`rel()` reverse-maps only this profile's remap dir. Validation (mirrored at
save): all profile mounts pairwise non-overlapping (canonical-path check as
today), none overlapping `~/.agent-sync`, and local-kind storages'
`local_dir` may not overlap any mount.

## 5. Cloud side: per-link resolution, per-storage capability

- `make_store(&StorageConfig)` — mechanical retarget of today's
  `make_store(&SyncConfig)`.
- `resolve_profile_for_root(app, store, config, root)` →
  `resolve_profile_for_link(app, store, config, link_id)`: same
  pinned/unpinned/discover table, scoped to the link's storage; persists the
  resolved `ProfileLink` back into that link. Discovery/create keep the
  head-root-kind check.
- `ensure_conditional_capability` reads/writes the link's
  `StorageConfig.supports_conditional_writes`.
- `list_sync_profiles`, `refresh_cloud_state`, `discover_profiles` take a
  storage id (UI passes the selected column / iterates storages).
- `write_machine_registry` records all local profiles + their links.

## 6. Command surface

| today | v2 |
|---|---|
| `sync_upload { config, files }` | `sync_upload { link: String, files }` — loads **saved** config; the unsaved-form-config path is gone (matrix flow saves first, then syncs) |
| `sync_download { config }` | `sync_download { link: String }` |
| `setup_root { root }` | `setup_link { link }` (fresh-mount pull + repair, same flow) |
| `set_sync_link`, `link_sync_profile` | removed — absorbed into `save_sync_config` (2c); pinning = editing the link's cloud prefix in the link editor |
| `get_sync_config` / `save_sync_config` | same names, v2 shape + §3.2 diff cleanups |
| `list_config_dirs` | one `ConfigSource` per local profile (`id` = profile id, label = derived name) |
| `get_file_statuses { paths }` | `+ link: String` — statuses are per link (baseline + cloud cache of that edge) |
| `repair_codex_plugins`, `apply_sidebar_state`, `resolve_conflict_copy` | `+ profile: String` — operate on that profile's mount (CLI calls get `CODEX_HOME=<mount>`) |
| `get_setup_readiness` | scans every local profile (codex-kind gets plugin/sidebar/hook checks); issues gain a `profile` field for grouping |
| `refresh_cloud_state` | per storage, iterating its links |

There is no "push everything" loop anymore (`ALLOWED_SYNC_ROOTS` iteration in
`do_upload_s3`/`do_download_s3` collapses); the old two-root buttons become
"iterate all links sequentially" in the frontend if we keep them at all —
the mockup's model is per-link ops, so default to that.

## 7. UI

- **Settings → Profile links matrix** (the mockup): rows = local profiles
  (label + file count from `list_config_dirs`), columns = storages (name +
  kind subtitle, gear opens the storage editor: name + connection fields +
  opt-ins). Cell = link icon when linked, empty when not; click empty →
  create link (auto cloud prefix); click linked → select (check badge);
  selected-link footer panel `profile ⇄ storage` with **Pull** / **Push**
  and an "Unlink" affordance (confirm; explains baseline removal + cloud
  kept). Link editor exposes the pinned cloud prefix (text input, existing
  grammar).
- **Sidebar**: `PROFILES n` and `STORAGE n` sections listing names (click →
  settings focused on that row/column); `Add profile` / `Add storage`
  dashed buttons. Existing Files / Activity / Settings nav unchanged.
- **Files page**: source list is now all local profiles; a small link
  selector (defaulting to the profile's only/first link) scopes status
  chips and Push/Pull. Selection count footer unchanged.
- **Finish setup**: issues grouped by profile display name.
- `types.ts` mirrors all v2 shapes by hand, as today (update both sides
  together).

Out of scope for v1 of this plan (cheap follow-ups): per-cell status chips
(synced/ahead/behind derivable from caches), drag-reorder of rows/columns,
per-link schedules.

## 8. Tests (dual-backend via the existing `run_*` wrappers)

LANDED (test plan: PLAN_MULTI_STORAGE_TESTS.md; the S15–S19 numbers this
table originally used were already taken by the sync-link scenarios):

| test | landed as |
|---|---|
| config unit | `legacy_cloud_and_config_data_still_deserialize` + config round-trip units in `lib.rs` |
| roots unit | `roots_profile_paths_are_validated`, `roots_remap_agent_sync_out_of_the_roots`, `overlapping_profile_mounts_are_rejected_at_save_time` |
| same-name isolation | S21 `s21_same_name_profiles_in_two_storages` |
| fan-out | S22 `s22_fan_out_one_profile_two_storages` |
| unlink/relink + identity edit | S23 `s23_unlink_drops_state_and_relink_reverifies` |
| same-kind neighbors + per-link statuses (the matrix) | S24 `s24_matrix_two_storages_three_profiles` |
| storage/profile removal cleanups | S25 `s25_storage_and_profile_removal_cleanups` |
| duplicate-target guard | unit in `set_link_cloud_replaces_only_its_cell` ("fight over baselines") |

Harness: `Machine` upserts the v2 matrix per operation (`ensure_link_config`)
and supports custom local profiles via `add_profile` +
`push_profile`/`pull_profile`; the pre-v2 suite migrated with behavior green
throughout.

## 9. Work order (each step ends with a green suite)

1. Config v2 structs (clean break) + `storage_identity` + baseline/cache
   keying + §3.2 save-diff cleanups. Single-storage behavior unchanged.
2. `Roots::for_profile` refactor + per-profile agent-sync remap +
   validation. **The risk step — same playbook as the `Store` and first
   `Roots` refactors; the suite is the net.**
3. Per-link engine surface: `make_store(&StorageConfig)`,
   `resolve_profile_for_link`, per-storage probe/opt-ins, `sync_upload` /
   `sync_download` / `setup_link` by link id.
4. Multi-profile periphery: readiness scan per profile, plugin/sidebar
   commands take `profile`, `list_config_dirs` / `get_file_statuses` /
   `refresh_cloud_state` retarget, machine registry.
5. New tests (S15–S19 + units), suite green.
6. Frontend: `types.ts` v2, settings matrix + editors, sidebar sections,
   Files link selector, FinishSetup grouping.
7. Docs: DESIGN2 implementation-status note, AGENT_SYNC_FILE_SETS (opt-ins
   are per-storage), PLAN_SYNC_LINKS supersession note, README, memory.

## 10. Compatibility

- Cloud: zero change (2a). Existing profiles/heads/manifests work as-is;
  machines on old builds still sync against the same storages.
- Local: none, by design (§3.1). Pre-v2 config, baselines, and caches are
  ignored; reconfigure once, relink, first sync re-verifies by hash.
- The relaxed profile-prefix grammar, container mounts, tier allowlist,
  merge drivers, `source_mtime` restore: all untouched.

## 11. Limits (deliberate, say-so-if-wrong)

- Ops are sequential per link; no parallel multi-link sync (the engine is
  process-global via `$HOME`-independent mounts, but the UI model is one
  link at a time; a "sync all" is a frontend loop).
- No storage-to-storage copy/replication; fan-out goes through the local
  profile (push the same profile to each storage).
- Deleting a storage or link never deletes cloud data (orphan philosophy,
  as everywhere else).
- Secrets stay in `sync_config.json` per storage, as today; a keychain
  move is orthogonal.
- Matrix UI targets single-digit row/column counts; no virtualization.

## 12. Implementation notes (deviations from the plan as written)

- **Commands take `(storage, profile)` id pairs**, not a synthetic link id
  (`sync_upload { storage, profile, files }` etc.) — no third id to invent
  or parse.
- **Starter profiles have fixed ids** `"codex"` / `"claude"`; custom profiles
  get random hex-8 ids. They are present in a fresh config but may be removed
  like any other profile. Random ids for the starters would drift between
  loads before the first save.
- **The per-profile record dir is `~/.agent-sync/{profile.id}` uniformly**
  (§4 planned `{slug}` for defaults and `{slug}.{id}` for custom). One rule;
  the default ids reproduce the familiar `codex`/`claude` layout exactly.
- **Pinned prefixes survive a storage identity change** (they are user
  intent); only resolved metadata, probe, baselines, and caches clear. §3.2
  said this; it flips the old v1 test expectation ("pin does not survive")
  and the suite now asserts the new rule.
- `resolve_conflict_copy` **publishes the resolution to every resolved link
  of the owning profile** before deleting the local copy — otherwise the
  next pull from an unpublished storage would resurrect the sibling.
- `make_store` takes `Option<&Roots>` and re-checks the local-folder/mount
  overlap at the narrow waist for the active pair (save-time validation
  covers cross-profile overlap).
- Readiness: `ScanInput.agent_sync_dir` became `lock_dirs: &[(root, dir)]`;
  issues carry a `profile` field and their ids are prefixed
  `{profile}.{hash}` so dismissals never cross profiles.
- Removed commands: `set_sync_link`, `link_sync_profile`,
  `create_sync_profile`, `list_profile_commits` (no UI callers).
  `list_sync_profiles` stays, storage-scoped. `setup_root` became
  `setup_link { storage, profile }`.
- New scenarios landed as **S21–S23** (S15–S20 were taken): same-name
  isolation across storages, fan-out, unlink/identity-change baseline
  cleanup + hash re-verify. The same-kind-neighbors lock isolation (§8
  "S17") is covered at the unit level (per-profile remap dir assertions)
  rather than by a multi-profile harness integration test; the harness still
  models one profile per kind per machine.
- Frontend: the footer Push/Pull iterate **all configured links**
  sequentially (the old two-root behavior generalized); per-link Pull/Push
  live on the settings matrix's selected-link panel, which saves the config
  before syncing. `get_file_statuses` takes `(profile, storage?)` and the
  Files page queries each source against its first link.
