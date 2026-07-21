# User-Friendly Project Configuration Workflow

**Status:** implemented (2026-07-18). Backend: setup drafts + `SetupTransaction` in
`domain.rs`, draft/transaction stores in `persistence.rs` (`~/.mallard/project_drafts`,
`~/.mallard/setup_transactions`), commands + `finalize_project_setup` + recovery in
`commands.rs`. Frontend: `ProjectSetupWorkspace.tsx` replaces `ProjectProfilePicker` +
`AddProjectDialog` (both deleted). Deviations from this plan as written:

- Recovery runs at the start of `list_local_projects` (the shell's first call on
  every launch) and again before every finalize, rather than on a dedicated
  "repository open" hook.
- If a transaction fails with **no** records applied, recovery deletes it and
  records the error on the surviving draft; partially applied transactions are
  retried. Pre-existing partial registrations created by the old flow are not
  auto-swept — "registered without binding" is indistinguishable from an
  intentional detached state.
- The `WorkspaceRoute` union was not introduced; the setup page is routed by a
  `setupDraftId` state alongside the existing editor-request objects.
- Inline **Add storage** supports local-folder storage only; S3 storage is
  created in Storage settings and then selected in the draft. (The backend
  accepts pending S3 drafts; only the UI is limited.)
- Existing-repo selection requires a saved (listable) storage; a pending
  storage can only start a new repo in the UI. Remote repos are revalidated at
  finalize, not during draft inspection, which stays local-only.
- Connecting a repo that is *unidentified* (missing fingerprint on either
  side) requires the same explicit acknowledgement as a mismatch.
- A resource newly discovered after the selection was saved flags the draft
  selection as stale via a discovery signature; the user re-accepts the list.

## Recommended Direction

Replace the current multi-screen setup with one resumable, inline **Project setup** workspace.

Today the flow is split between profile selection, discovery, resource review, storage selection, and several independent saves. The final creation sequence can also leave an incomplete project when a later save fails:

- `src/components/project-sync/ProjectSyncV3.tsx`
- `src/components/project-sync/ProjectProfilePicker.tsx`
- `src/components/project-sync/AddProjectDialog.tsx`

## Proposed Experience

After the native folder chooser, open a dedicated inline page:

```text
Set up healthGame                                      Draft saved

Project folder       /Users/.../healthGame             Ready
Agent profiles       Codex: Default Codex              Ready
                     Claude: Not used
Storage              Local storage 1                   Ready
Repository           Existing “healthGame”             Git match
Sync contents        14 resources selected             Customize ▾

[Discard draft]                         [Finish and review Pull]
```

Do not use an app-level wizard or configuration modal. Each section expands inline only when editing.

| Section | Default behavior | User action only when needed |
|---|---|---|
| Project | Derive name, canonical path, and Git fingerprint automatically | Rename or change folder |
| Agent profiles | Preselect an unambiguous existing or default Codex or Claude profile | Change profile or choose a new profile folder inline |
| Storage | Preselect the only available storage | Select another or expand **Add storage** inline |
| Repository | Auto-select one exact Git match; otherwise default to **Create new repo** | Choose among multiple matches or acknowledge a mismatch |
| Sync contents | Use the discovered default recipe and show a collapsed summary | Expand to customize individual resources |
| Review | Show a short readiness checklist | Finish setup |

Use one primary storage during setup. Additional storage links remain available from project settings later. This removes the current multi-storage and repository ambiguity.

The only remaining modal-like surface should be the native folder picker. Pull approval can remain a separate safety workflow after configuration because it writes files. Repository selection and mismatch confirmation should be inline.

## Draft Metadata

Add machine-local, temporary drafts:

```text
~/.mallard/
|-- project_drafts/
|   `-- <draft-id>.json
`-- setup_transactions/
    `-- <draft-id>.json   # exists only during finalization/recovery
```

A draft should contain:

- Preallocated draft, project, bundle, and replica IDs.
- Project path, canonical path, editable name, and Git fingerprint.
- Existing or pending Codex and Claude profile selections.
- Existing or pending storage configuration.
- Repository choice: new or an existing bundle ID.
- Explicit repository-mismatch acknowledgement.
- Selected resource IDs and discovery signature.
- Revision, status, timestamps, and the last localized error.

Do not persist discovered file contents, remote bundle listings, or resource payloads in a draft. Rescan those on resume. Pending credentials may be present for a newly drafted S3 storage, so draft files need the same private permissions as the current metadata.

Draft behavior:

- Autosave after a short debounce.
- Survive navigation and application restart.
- Appear in the sidebar with a small **Draft** status.
- Selecting the same canonical folder resumes its existing draft.
- Closing the setup page saves it; it does not discard it.
- Explicit discard removes only draft metadata and does not touch project files.
- Successful finalization deletes the draft.
- Do not silently delete old drafts; mark stale drafts and let the user resume or discard them.

## Safe Finalization

Introduce one backend operation, `finalize_project_setup`, instead of the frontend calling register, recipe, binding, and link commands sequentially.

It should:

1. Reload and revalidate the draft, current config, profiles, storage, paths, and remote repository.
2. Re-run project discovery and rebuild the recipe from the saved resource selection.
3. Write an idempotent setup transaction containing the deterministic records to create.
4. Add pending profiles and storage, project registration, recipe, storage link, and machine binding.
5. Recover automatically if the app stops between writing `sync_config.json` and `machine_projects.json`.
6. Remove the transaction and draft only after both documents contain the expected records.

Retries must reconcile by the preallocated IDs rather than creating duplicate projects, profiles, storage, or links. This addresses the current partial-registration behavior while preserving the revision protections in `src-tauri/src/project_sync_v3/persistence.rs`.

## Frontend Architecture

Replace the overlapping editor booleans with one explicit workspace route:

```ts
type WorkspaceRoute =
  | { page: "git-info" }
  | { page: "project"; projectId: string }
  | { page: "storage"; storageId: string | "new" }
  | { page: "project-setup"; draftId: string };
```

Add `ProjectSetupWorkspace.tsx` and retire the setup responsibilities currently split between `ProjectProfilePicker` and `AddProjectDialog`.

The inline page should:

- Keep all sections visible as a readable checklist.
- Show errors beside the affected section.
- Show discovery and repository loading without blocking unrelated editing.
- Keep a sticky footer with draft status and the final action.
- Explain exactly why finalization is unavailable.
- Use restrained expansion transitions and a subtle `Saving…` to `Draft saved` state.

## Completion Actions

For a new remote repository:

- Primary: **Finish setup**
- Secondary explicit action: **Finish and push**

For an existing remote repository:

- Primary: **Finish and review Pull**

Never push or apply a Pull automatically.

Storage can remain optional through a secondary **Finish locally** action, but the page should explain that the project will not be ready to sync until storage is linked.

## Implementation Phases

### Phase 1: Navigation and Draft Foundation

- Add the explicit workspace route.
- Add draft domain types and bounded draft persistence.
- Add list, create, load, update, and discard draft commands.
- Resume an existing draft when the same canonical project folder is selected.

### Phase 2: Draft Inspection

- Inspect pending profile paths without permanently creating profiles.
- Validate pending storage without permanently adding it to global storage.
- Discover resources from a mixture of existing and pending profile selections.
- Re-run discovery when relevant draft inputs change.

### Phase 3: Inline Setup Workspace

- Build `ProjectSetupWorkspace.tsx`.
- Add inline project, profile, storage, repository, and resource sections.
- Add autosave and localized validation errors.
- Show resumable drafts in the sidebar.
- Remove project-configuration app modals from the setup path.

### Phase 4: Repository Selection

- Load repositories for the selected primary storage.
- Auto-select a single exact Git fingerprint match.
- Default to a new repository when there is no exact match.
- Require explicit selection when multiple matches exist.
- Require inline acknowledgement before connecting a mismatched or unidentified repository.

### Phase 5: Finalization and Recovery

- Add `finalize_project_setup`.
- Preallocate stable IDs and make finalization idempotent.
- Add a recoverable setup transaction across global config and machine bindings.
- Hand existing repositories to Pull review after successful finalization.
- Offer an explicit push action for newly created repositories.

### Phase 6: Cleanup and Documentation

- Remove or repurpose `ProjectProfilePicker.tsx` and `AddProjectDialog.tsx`.
- Remove obsolete pending-project and discovery orchestration from `ProjectSyncV3.tsx`.
- Update the README setup instructions and metadata layout.
- Keep project settings and storage settings independent from the setup draft UI.

## Verification

Automated and manual verification should cover:

- Draft save, revision conflict, restart, and resume.
- Selecting the same canonical project folder resumes rather than duplicates.
- Discard removes draft metadata without touching project files.
- Pending profiles and storage do not appear in permanent metadata before finalization.
- Existing profile and storage changes invalidate or refresh a draft safely.
- A single Git match is recommended automatically.
- Multiple Git matches require an explicit choice.
- Repository fingerprint mismatches require acknowledgement.
- Invalid local-storage paths and S3 credentials produce inline storage errors.
- Finalization creates profiles, storage, project, recipe, link, and binding exactly once.
- Retrying finalization does not create duplicates.
- Simulated interruption between config and binding writes recovers successfully.
- Existing-repository setup opens Pull review without applying changes automatically.
- New-repository setup never pushes without an explicit action.

Before handoff, run:

```sh
npm run build
cd src-tauri && cargo check
```
