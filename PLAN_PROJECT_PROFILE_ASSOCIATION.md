# Plan: Associate every project with local agent profiles

**Status:** implemented for the project/profile association — transactional
project creation and an impact-preview screen remain optional hardening work

**Date:** 2026-07-17

**Scope:** schema-3 project sync, machine-local profile catalog, capture/restore
resolution, and the restored project-link UI

This amends the earlier project-scoped plan's removal of profiles only at the
machine-binding/UI layer. Profiles do not regain cloud identity or become a
sync unit.

## 1. Outcome

Every local project must explicitly select the agent profile it reads from and
writes to on this machine.

For Codex, a profile is the exact `CODEX_HOME`, for example:

```text
~/.codex
/Users/hequ/Desktop/project/myconf2/.codex
```

For Claude, the equivalent is the exact `CLAUDE_CONFIG_DIR`, for example:

```text
~/.claude
/Users/hequ/Desktop/project/myconf2/.claude
```

The association is machine-local. Machine A may bind project `vllm` to
`/A/config/.codex`, while machine B binds the same cloud bundle to
`/B/config/.codex`. The bundle ID, storage links, logical resource IDs, and
cloud data do not change.

The project remains the sync unit. A profile is only the local provider home
used to discover, capture, restore, resume, and repair that project's provider
resources. This feature must not bring back whole-profile sync.

## 2. Product definition

"One project has one profile" means one profile **per provider used by that
project**:

- a Codex-only project has exactly one Codex profile;
- a Claude-only project has exactly one Claude profile;
- a mixed project can have one Codex profile and one Claude profile; and
- an active project binding must have at least one provider profile.

Multiple projects may share the same profile. Capture must continue to isolate
sessions by the bound project checkout, so sharing `~/.codex` does not merge
the projects into one bundle.

Profiles are not storage-specific. Changing storage must not change the local
profile assignment, and linking a second storage must reuse the same project
and profile binding.

## 3. Current gap

The schema-3 backend already stores raw provider paths on `ProjectBinding`:

```rust
codex_home: Option<String>
claude_home: Option<String>
```

Those fields are not yet a complete profile feature:

1. `discover_project` always scans the global default `~/.codex` and
   `~/.claude` before the user can choose a profile.
2. The Add Project dialog lets the user edit raw home strings only after that
   discovery, so the displayed inventory can belong to a different profile
   than the one eventually saved.
3. Provider homes have no stable local ID, display name, reusable picker, or
   in-use validation.
4. The middle section of the linked-storage bar repeats the project name. It
   does not show which profile supplies the resources.
5. A project can appear ready even when its selected sessions live in another
   profile.
6. A profile change invalidates the physical capture/apply target, but today
   that change is represented as an undifferentiated binding edit.

The fix should build on the existing capture pipeline: it already receives
`codex_home` and `claude_home` through `CaptureRequest`. The missing layer is a
validated, reusable profile catalog and an explicit project-to-profile
association resolved before discovery.

## 4. Decisions

### D1 — Profiles are machine-local

Profile names, IDs, and absolute paths live only under `app_data/v3/`. They are
never written into a bundle manifest, object key, recipe, or remote snapshot.

### D2 — Profiles are typed

A profile is either `codex` or `claude`. A Codex assignment cannot point to a
Claude profile, even if the selected directory happens to exist.

### D3 — Store stable IDs, resolve paths at the command boundary

Project bindings refer to local profile IDs. Capture, status, push, restore,
repair, and launch commands resolve those IDs to validated paths immediately
before use. Frontend-supplied paths are never trusted as an operation target.

### D4 — The path of an in-use profile is immutable

Users may rename a profile. To change its directory, create/select another
profile and reassign the project. This prevents one profile edit from silently
retargeting several projects. Removing a profile is blocked while any active
project references it.

### D5 — Folder selection identifies the exact provider home

The stored path is the exact `CODEX_HOME` or `CLAUDE_CONFIG_DIR`. If a user
picks a parent containing `.codex` or `.claude`, the probe may offer that
detected child, but persistence stores the resolved provider home rather than
container-style path semantics. The familiar suffix is recommended, not
required: both CLIs permit custom home names, so an explicitly confirmed
directory such as `/profiles/codex-work` remains valid.

### D6 — Profile changes never move or delete files

Reassignment changes future capture/apply targets only. Existing files remain
in the old profile. Pull continues through the restore-plan review before any
file is written to the new profile.

### D7 — Project bundles remain provider-composable

Do not collapse a mixed Codex/Claude project into one arbitrary directory.
The UI presents one Profiles section, but the model retains one typed
assignment per provider.

### D8 — Clean break from old profile state

Do not import schema-2 `LocalProfile` rows and do not convert existing raw
`codex_home`/`claude_home` bindings. The new project-profile catalog is
independent. Existing project registrations without a new binding appear as
`Setup required` until the user chooses a checkout and profile.

## 5. Machine-local data model

Keep cloud/config schema 3 unchanged. Replace the experimental raw-path
binding document with a new machine-project state file.

```rust
struct MachineProjectState {
    schema: u32,                // machine-project schema 1
    revision: u64,
    profiles: Vec<ProviderProfile>,
    bindings: Vec<ProjectBinding>,
}

enum ProviderProfileKind {
    Codex,
    Claude,
}

struct ProviderProfile {
    profile_id: LocalProviderProfileId,
    provider: ProviderProfileKind,
    display_name: String,
    path: String,               // user-selected absolute spelling
    canonical_path: String,     // captured during validation
    revision: u64,
    created_at: u64,
    updated_at: u64,
}

struct ProjectBinding {
    replica_id: ReplicaId,
    local_project_id: LocalProjectId,
    bundle_id: BundleId,
    project_root: String,
    canonical_project_root: String,
    profile_ids: BTreeMap<Provider, LocalProviderProfileId>,
    state: BindingState,
    revision: u64,
    updated_at: u64,
}
```

Validation invariants:

- active bindings contain at least one profile assignment;
- no binding contains two profiles of the same provider;
- every assigned ID exists and its provider matches the map key;
- canonical profile directories are unique within one provider catalog;
- every newly created profile path is absolute, clean, an existing directory,
  and not a final symlink;
- provider kind comes from the explicit Codex/Claude choice; recognizable
  contents catch likely mistakes, while an empty profile or nonstandard
  directory name can be accepted after explicit confirmation;
- profile directories do not overlap app data, local bundle storage, the
  project checkout, or one another across provider types;
- a profile can be shared by multiple non-overlapping project roots; and
- existing project-root and bundle-ID invariants remain unchanged.

`ProjectBinding` should no longer be a standalone authorization to write.
Operations load the current `MachineProjectState`, resolve `profile_ids`, verify
the stored canonical paths again, and only then construct `CaptureRequest` or
a restore target.

## 6. Clean-break rollout

Create `app_data/v3/machine_projects.json` for `MachineProjectState` and stop
using the current experimental `bindings.json` for schema-3 operations.

- Do not parse, convert, rename, or delete the old file.
- Do not read schema-2 local profiles.
- Keep existing schema-3 project registrations and storage links visible, but
  treat them as unbound until the user completes project setup in the new UI.
- On a fresh catalog, offer `~/.codex` and `~/.claude` as default profiles only
  when those directories pass the normal profile probe.
- Custom profiles are added explicitly with the folder picker.
- Pull, Push, Repair, inventory, and resume remain blocked for an unbound
  project.

This intentionally trades development-state continuity for a smaller and
safer implementation. No filesystem content is moved or deleted.

## 7. Backend command surface

Add a focused machine-profile API:

```text
list_provider_profiles() -> ProviderProfileSummary[]
probe_provider_profile(provider, path) -> ProviderProfileProbe
create_provider_profile(provider, display_name, path) -> ProviderProfile
rename_provider_profile(profile_id, display_name, expected_revision)
  -> ProviderProfile
remove_provider_profile(profile_id, expected_revision) -> bool
```

`ProviderProfileProbe` returns:

- provider and exact resolved home;
- canonical path and suggested display name;
- exists/readable/writable state;
- whether the user picked a parent containing `.codex` or `.claude`;
- duplicate-profile match, if one exists;
- a small resource summary useful for confirmation; and
- actionable validation/permission errors.

Change discovery and binding commands:

```text
discover_project({ project_root, profile_ids }) -> ProjectDiscovery

save_project_binding({
  local_project_id,
  project_root,
  profile_ids,
  expected_revision
}) -> ProjectDetail

create_project_with_binding({
  display_name,
  project_root,
  profile_ids,
  recipe,
  storage_ids,
  discovery_token
}) -> ProjectDetail
```

Returning `ProjectDetail` after save avoids a frontend race between the saved
binding, the profile catalog, and a newly discovered inventory.

Add resolved profile summaries to `ProjectDetail` and
`LocalProjectSummary`. The frontend should receive display data, validity, and
compact paths without reconstructing catalog joins itself.

## 8. Capture, status, push, restore, and repair changes

Create one backend resolver:

```rust
fn resolve_binding(
    repository: &V3Repository,
    binding: &ProjectBinding,
) -> Result<ResolvedProjectBinding, String>
```

`ResolvedProjectBinding` contains the revalidated checkout, optional resolved
Codex/Claude homes, profile IDs/revisions, and excluded nested checkouts.
Every path consumer must use it:

- project discovery and inventory;
- local/remote status;
- bundle capture and Push;
- restore planning and apply revalidation;
- readiness and Repair;
- continuation commands that set `CODEX_HOME` or `CLAUDE_CONFIG_DIR`; and
- resource preview/edit commands added later.

Remove `binding.codex_home` and `binding.claude_home` from the new operation
path entirely. Only resolved catalog profiles can authorize provider access.

### Association-change barrier

Changing a project's profile assignment increments the binding revision and:

- detaches materializations created by older revisions;
- expires outstanding restore/dependency plans;
- clears cached inventory/status/readiness for that project;
- marks every linked storage as `profile_changed`; and
- blocks Push against an existing remote head until the user fetches and
  reviews the current bundle from the new profile context.

Extend `RecipeBase` (or an equivalent local capture-base record) with the
binding/profile revision used to establish it. Push must check this on the
backend; a disabled frontend button is not a safety boundary. A brand-new
bundle with no remote head may still make its first Push after a valid profile
assignment and fresh inventory scan.

Unavailable selected resources must remain selected and visible. Switching to
an empty profile must not silently publish their deletion.

## 9. UI plan

Keep the restored legacy visual language. Do not replace the project cards or
introduce another dashboard.

### 9.1 Project card

The project stays in the left side of the card. Add a compact profile summary
below the checkout:

```text
vllm
7 selected resources
~/Desktop/project/vllm
Codex · myconf2
```

If both providers are used, show `2 profiles` and expose full names in the
tooltip. Missing/invalid assignments use a warning color and the label
`Profile required`.

### 9.2 Three-section linked-storage bar

Use the existing three-part row as:

```text
Storage + storage gear | Profile assignment + profile gear | Pull · Push · Repair
```

The middle section currently repeats the project name. Replace it with:

```text
Default Codex
~/.codex
```

or, for a mixed project:

```text
2 profiles
Codex: myconf2 · Claude: Default
```

The profile gear expands the project-level profile editor inline. Because the
association belongs to the project, changing it updates every storage row for
that project.

### 9.3 Profile editor

For each provider used by the recipe, show:

- a named profile dropdown;
- the resolved compact path;
- validity/read/write status;
- `Choose another folder…`;
- `Add profile…`; and
- a clear note that this selection affects only this machine.

Keep checkout mapping separate from profile assignment. `Change checkout`
changes the project cwd; `Change profile` changes `CODEX_HOME` or
`CLAUDE_CONFIG_DIR`.

Saving first probes the new profile, then re-runs project discovery with the
proposed assignments. Show an impact summary before commit:

```text
Codex profile changed
12 selected resources still available
2 selected conversations are missing in the new profile
Push will remain blocked until Pull/Review
```

Do not silently remove unavailable recipe entries.

### 9.4 Add Project flow

Reorder the current flow:

1. Choose project checkout.
2. Choose at least one provider profile. Preselect a valid default when there
   is only one obvious choice.
3. Run discovery using those exact profile IDs.
4. Review resources and storage links.
5. Submit the registration, recipe, and machine binding through one backend
   workflow using the user's confirmed discovery revision. If the existing
   two-file persistence cannot commit both documents together, use a bounded
   intent record plus rollback/recovery instead of relying on frontend call
   order.

Changing a profile in the dialog invalidates the previous inventory and shows
a loading state until the new discovery completes. `Create project` remains
disabled if the inventory was produced for an older profile selection.

### 9.5 New-machine restore

When a remote bundle has no local binding, ask for:

1. the local checkout;
2. a local profile for every provider present in the bundle; and
3. restore-plan review.

Do not carry machine A's profile path into this picker. Suggested defaults come
only from machine B's local catalog.

## 10. Error and permission behavior

Profile errors must name the affected profile and operation:

- `Codex profile “myconf2” is not readable`;
- `The selected folder is a parent; use …/myconf2/.codex`;
- `Profile path overlaps local storage “Local storage 1”`;
- `Profile is used by vllm and 2 other projects`;
- `Operation not permitted while reading … — choose an accessible folder or
  grant the app Files and Folders access`.

Do not translate permission errors into “no resources.” A failed scan and an
empty valid profile are different states.

An unreadable profile blocks inventory, status, Push, Pull, and Repair. A
readable but non-writable profile may still support inventory and Push; Pull
and Repair remain blocked with the precise write-permission reason.

The backend must re-check permissions and canonical containment at operation
time, because access and symlink state can change after the picker probe.

## 11. Implementation sequence

### Phase 1 — model and profile catalog

- Add typed profile IDs, `ProviderProfile`, `MachineProjectState`, and
  validation in the new state file.
- Add profile catalog CRUD/probe commands and command registration.
- Add Rust tests before switching any operation path.

**Gate:** a fresh project can bind to either `~/.codex` or a custom profile;
an existing unbound registration clearly requests setup and no cloud or
provider file changes.

### Phase 2 — one resolved binding path

- Add `ResolvedProjectBinding`.
- Route inventory, status, Push, restore, readiness, Repair, and launch through
  it.
- Add the association-change barrier and binding-revision checks.
- Remove operational use of raw home paths.

**Gate:** two profiles containing different sessions for the same checkout
produce different inventories, and every operation uses the selected one.

### Phase 3 — discovery and creation flow

- Change the discovery request to require profile IDs.
- Make Add Project select profiles before resource discovery.
- Add stale-discovery tokens/revisions so an old response cannot be saved.
- Update remote-bundle setup to select local profiles before planning restore.

**Gate:** a project created with `myconf2/.codex` never briefly scans or saves
resources from `~/.codex`.

### Phase 4 — project-card UI

- Replace the repeated project section in each storage row with profile
  assignment details.
- Add the inline profile editor, custom-folder creation, validation states,
  and impact preview.
- Surface invalid/missing profiles in the sidebar and project card.
- Refresh project inventory/counts immediately after a successful save.

**Gate:** users can see and change the profile without leaving the old-style
project-link screen.

### Phase 5 — integration and hardening

- Exercise shared profiles, multiple projects, multiple storages, custom
  provider homes, new-machine remapping, permission failures, and rollback.
- Verify manifests and object keys contain no local profile IDs, names, or
  absolute paths.
- Run `npm run build`, `cargo check`, Rust unit tests, and both local-folder and
  S3 integration scenarios.

## 12. Expected file-level changes

Backend:

- `src-tauri/src/project_sync_v3/domain.rs`: profile IDs/catalog, binding
  format, validation, and capture-base revision metadata;
- `src-tauri/src/project_sync_v3/persistence.rs`: the new bounded
  `machine_projects.json` store and atomic machine-state writes;
- `src-tauri/src/project_sync_v3/commands.rs`: profile CRUD/probe,
  resolved binding use, discovery request, composite project creation, and
  reassignment barriers;
- `src-tauri/src/project_sync_v3/provider_capture.rs`: accept only resolved
  provider homes and preserve permission failures distinctly from empty
  discovery;
- `src-tauri/src/project_sync_v3/bundle_engine.rs`: update binding fixtures and
  any restore-plan revision checks; and
- `src-tauri/src/lib.rs`: register the new Tauri commands without coupling v3
  state to schema 2.

Frontend:

- `src/types.ts` and `src/components/project-sync/api.ts`: typed profile
  summaries, probes, assignments, and request contracts;
- `src/components/project-sync/AddProjectDialog.tsx`: profile-first discovery
  and stale-result prevention;
- `src/components/project-sync/ProjectBindingEditor.tsx`: checkout mapping and
  provider profile selection as separate controls;
- `src/components/project-sync/ProjectLinksWorkspace.tsx`: profile summary in
  the middle row section plus the inline editor/impact state;
- `src/components/project-sync/ProjectSyncV3.tsx`: profile catalog loading,
  creation/reassignment workflows, refresh, and operation gating; and
- `src/App.css`: small additions using the existing project-card visual
  language rather than a new screen.

## 13. Test plan

### Rust unit tests

- fresh machine-project state creation and bounded atomic round-trips;
- default/custom profile naming and canonical-path deduplication;
- provider mismatch and duplicate same-provider assignment rejection;
- missing, symlinked, relative, wrong-kind, and overlapping profile paths;
- profile deletion blocked while referenced;
- profile reassignment preserves bundle/replica identity and increments the
  binding revision;
- old materializations/plans become unusable after reassignment;
- Push rejects a capture base established with another profile revision; and
- permission denial is an error, not an empty inventory.

### Provider capture tests

- one checkout with sessions in `~/.codex` and custom `.codex`: only the
  selected profile is discovered/captured;
- two projects sharing one Codex profile remain isolated by cwd;
- nested project registrations remain excluded correctly;
- the equivalent custom/default matrix for Claude; and
- selected-but-unavailable resources survive a profile switch.

### End-to-end scenarios

1. Machine A binds `/A/vllm` to `/A/myconf2/.codex`, pushes, and the cloud
   manifest contains neither absolute path.
2. Machine B pulls the same bundle, binds `/B/vllm` to `~/.codex`, restores,
   resumes the same task, changes it, and pushes without creating a second
   project bundle.
3. One machine links a project to two storages; both rows show and use the same
   profile assignment.
4. Reassigning a populated project to an empty profile blocks Push until
   Pull/Review and never deletes data in the old profile.
5. Removing a project unregisters its association but leaves both the checkout
   and profile directories untouched.
6. An inaccessible macOS folder reports the real permission failure and
   recovers after the user chooses an accessible profile.

### Frontend tests

- Add Project cannot save before profile-scoped discovery completes;
- custom profile picking uses the backend probe result, not the typed path;
- the middle link section renders one or two assigned profiles correctly;
- profile settings update every storage row for the project;
- missing/invalid profile disables Pull, Push, and Repair with an explanation;
- unavailable resources remain selected in the impact preview; and
- cancel leaves the existing binding unchanged.

## 14. Acceptance criteria

The feature is complete when:

- every active project visibly names at least one local provider profile;
- the user can select `~/.codex` or a custom path such as
  `/Users/hequ/Desktop/project/myconf2/.codex` from the project card;
- inventory, counts, Push, Pull, Repair, and resume commands all resolve the
  same saved profile assignment;
- profile selection happens before discovery in Add Project and remote setup;
- changing profile cannot silently delete, move, or republish missing
  resources;
- the same cloud project can bind to different checkouts and profiles on two
  machines;
- multiple projects can safely share one provider profile;
- multiple storage links do not duplicate or override the association; and
- no machine-local profile path, ID, or display name appears in cloud data.

## 15. Out of scope

- syncing an entire provider profile as part of a project bundle;
- copying authentication, caches, trust, approvals, or provider-global state;
- moving files from the old profile when a project is reassigned;
- selecting a different local profile per storage;
- importing schema-2 profiles or converting the experimental raw-home
  `bindings.json`;
- automatically inferring a profile from whichever shell last launched Codex
  or Claude.
