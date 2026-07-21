# Plan: Optional Project-File Sync for Non-Git Projects

**Status:** implemented
**Date:** 2026-07-20
**Scope:** add an opt-in project-files step to the existing Push and Pull
reviews using a clean schema-4 bundle format.

The implementation now includes the lazy non-Git scan, destination-specific
selection metadata, schema-4 file/directory resources and tombstones, reviewed
Push tokens, typed Pull actions, backup-pinned writes and deletes, empty-only
directory removal, dynamic review tabs, and integration/stress coverage. The
sections below remain the behavioral contract for future hardening.

## 1. Requirement interpretation

Treat “non-GitHub project” as a project folder that is **not inside a Git work
tree**. GitHub is a hosting provider; the app currently knows whether a folder
is a Git repository, not whether its remote is hosted at `github.com`.

For the first release:

- A Git repository is ineligible regardless of whether its remote is GitHub,
  GitLab, another server, or a local path. Git remains responsible for those
  project files.
- A non-Git project gets an optional **Project files** tab after **Plugins** and
  before **Review** in both Push and Pull. Files and directories are both
  tracked entries.
- The tab itself is the opt-in. Setup does not scan ordinary project files.
  After the user explicitly scans the tab, newly discovered eligible files
  are checked in the pending Push review by default; nothing is uploaded until
  the user completes Push.
- The backend, not the React boolean, is authoritative about eligibility and
  rechecks it immediately before capture or restore.

This deliberately extends the current boundary in `PLAN_PROJECT_SCOPED_SYNC.md`
only for non-Git folders. Existing agent resources such as sessions, skills,
plugins, settings, and instructions keep their current behavior.

## 2. User outcome

A user with a folder that is not backed by Git can explicitly choose regular
files and directories from that folder, push them into the same portable
project bundle, and pull the reviewed tree onto another machine.

The feature must feel like an optional part of the current review, not a second
sync system:

```text
[ Git & sessions ] [ Skills ] [ Plugins ] [ Project files ] [ Review ]
```

For a Git project with no previously stored generic project content, the tab is omitted:

```text
[ Git & sessions ] [ Skills ] [ Plugins ] [ Review ]
```

If a bundle already contains generic project files but the current binding is
now a Git work tree, show a locked **Project files** tab. It must explain why
the remote content cannot be applied; it must not hide pending writes or allow
them through the Review page.

## 3. Product decisions

### 3.1 Exact files and directories, not automatic folder rules, in v1

Each regular file and each real directory is one tracked resource. Directory
checkboxes are also a convenient way to select the entries currently visible
beneath that directory, but they do not create a future wildcard.

Consequences:

- A newly discovered eligible file or directory appears checked after an
  explicit scan,
  unless the user previously excluded that path for this project-storage
  link.
- Auto-selection changes only the pending review. It does not update local
  metadata or storage until Push succeeds.
- Renames appear as one removed path plus one new path; there is no rename
  inference.
- A saved selection is portable and deterministic because it contains exact
  resource IDs.
- A selected directory is not a wildcard. A future child is considered only
  when a later explicit scan discovers it, at which point it is shown as a
  checked `New` suggestion for review.
- Selecting a file automatically requires every ancestor directory between it
  and the project root. Those directory resources cannot be removed while a
  selected descendant depends on them.
- Empty directories are independently selectable and survive Push/Pull.

The tab should state this plainly: `New eligible files and folders found by
Scan are selected for this Push. Review them before continuing.` Automatic
background discovery and folder include/exclude rules can be later features
after deletion and ignore semantics have proven safe.

### 3.2 Selection is destination-specific

Project-content choices belong to the selected `ProjectStorageLink.recipe`, just
like the current successful-Push selection. Choosing `notes.md` for storage A
must not silently add it to storage B.

The recipe alone cannot distinguish a never-seen entry from one the user
intentionally unchecked. Add bounded, destination-specific local metadata:

```ts
interface ProjectContentPreferences {
  schema_version: 1;
  revision: number;
  excluded_resource_ids: string[];
}
```

Store it on `ProjectStorageLink`; do not upload it as bundle content. A scan
seeds its pending selection in this order:

1. files and directories already in the link recipe stay checked;
2. newly discovered eligible entries not in `excluded_resource_ids` are
   checked and labelled `New · selected after scan`;
3. required ancestor directories are checked whenever a descendant is
   checked;
4. explicitly excluded entries stay unchecked unless required by a selected
   descendant; and
5. ignored or blocked entries stay disabled.

Scanning is read-only. After remote publication succeeds, save the published
link recipe and updated exclusion preferences together in the same guarded
local-config mutation. Cancel preserves the prior recipe and preferences, so
merely viewing the tab never changes metadata. Keep the exclusion set within
the existing resource limit and provide a **Reset exclusions** action rather
than silently pruning user decisions.

No separate enable flag is needed:

- zero selected generic entries means the feature is unused for that storage;
- selecting a local-only file or empty directory schedules it for Push;
- an already published entry stays selected until the user explicitly chooses
  **Remove from storage**; and
- the generic **Clear** action must not turn published files into remote
  deletions. Bulk removal requires its own confirmation.

### 3.3 Generic project content is distinct from agent setup files

Add a new resource kind and category rather than overloading the current
`project_file` / `project_setup` meaning:

```ts
type ProjectResourceKind =
  | "project_content_file"
  | "project_content_directory"
  // existing kinds...

type ProjectResourceCategory =
  | "project_files"
  // existing categories...
```

Paths already owned by the provider adapters remain in their existing tabs or
Review details and must not be discovered a second time as generic content.
This includes the current allowlisted settings, hooks, MCP definitions,
skills, agents, commands, rules, and plugin-intent files.

`AGENTS.md` is not currently captured by the provider adapter. In a non-Git
folder it may appear as an ordinary selectable project file.

### 3.4 Every Pull choice is explicit but does not block other categories

Generic project-content actions are optional and start unselected, including
safe new files and directories. The Project files tab offers **Select safe
additions**, per-entry selection, and directory bulk selection.

At final Review, every remote project-content action has one decision:

- `Apply storage version`; or
- `Keep local / skip this pull`.

Skipping project content must not prevent selected session, skill, plugin, or
agent-setup changes from being applied. The final summary must say how many
project files and directories will be created, replaced, deleted, or kept
local.

### 3.5 Portable file metadata

Treat content and portable filesystem metadata separately:

- file bytes and safe POSIX permission bits (`0o777`, with set-id bits already
  stripped) are portable;
- directory presence and safe permission bits are portable, including empty
  directories;
- a permission-only change, including an executable-bit change, is a real
  local update and is included in the reviewed digest;
- size is derived from bytes;
- source mtime is retained for display and may be restored best-effort when a
  file or directory is applied, but an mtime-only change does not create a
  sync update; and
- ownership, ACLs, xattrs, creation time, quarantine flags, and other
  platform-specific metadata are not synced.

Pull must show a file or directory mode change before applying it. File
executable bits continue to require explicit review and restored files are
never executed by Mallard.

## 4. Push experience

The normal Push review remains fast. Do not scan the whole project folder when
the review opens. Invoke the bounded file scan only when the user enters
**Project files** (or when the selected storage already has generic content that
must be compared).

Suggested tab layout:

```text
Project files                                      Non-Git folder
Scan found 3 new eligible entries and selected them for this Push.

[ Search paths... ] [ Rescan ] [ Select shown ]

▾ docs/                       New · required directory
  ▾ specs/                    New · required directory
    ☑ a.md                    New · selected     18 KB
▸ data/                                       0 selected of 7

1 file · 2 directories included
                                           [Next: Review →]
```

Rows show relative path, entry type, size where applicable, modified time,
local/storage state, and any blocker or sensitive-content warning. Expanding a
row shows the portable path, digest, mode, ignore evidence, and why the entry
is or is not selectable. A required ancestor directory has a locked checked
state until all selected descendants are cleared.

Push behavior by state:

| State | Default and action |
| --- | --- |
| Newly discovered local-only file/directory | Checked after explicit scan unless previously excluded; user may uncheck it unless it is a required ancestor. |
| Synced | Keep the saved inclusion. |
| Local ahead | Safe to Push when included. |
| Storage only / storage ahead | Block Push and link to Pull review. |
| Diverged | Block Push; resolve through Pull first. |
| Missing locally but previously included | Keep the stored version by default; offer explicit **Remove from storage**. |
| Blocked / unknown | Cannot be included until rescanned or fixed. |

The Review tab adds a separate **Project files** row. It must not bury ordinary
files inside the existing `Project files & tools` disclosure; rename that
existing disclosure to **Agent setup & tools** if needed for clarity.

Remote removals are always called out separately and require confirmation:

```text
Project files       12 files · 5 folders     3 updates · 1 removal
```

## 5. Pull experience

Build the existing immutable restore plan before rendering the tab. Classify
generic files and directories using typed resource descriptors, not filename
or action-ID heuristics.

Suggested states:

| Storage/local relationship | Pull presentation |
| --- | --- |
| Storage directory, no local path | `Create folder`; required when a selected child depends on it. |
| Storage file, no local path | `Add file`; optional and unselected. |
| Existing local directory | `Folder exists`; apply reviewed mode metadata after its children. |
| Directory path occupied by a file or link | Block the subtree; never replace it implicitly. |
| Same digest | `Already matches`; no action. |
| Storage changed, local matches base | `Update available`; optional and unselected. |
| Local changed, storage matches base | `Keep local`; no write selected. |
| Both changed | `Diverged`; keep local, with no overwrite shortcut in v1. |
| Storage tombstone, local matches deleted digest | `Delete requested`; explicit approval and backup required. |
| Storage tombstone, local differs | `Local file changed`; deletion blocked and local file kept. |

All writes and deletes are digest-pinned. If a target changes after the plan is
created, Apply blocks that action and asks for a refreshed review.

For `docs/specs/a.md`, Pull executes in this order:

1. validate that `docs` and `docs/specs` are safe directory targets;
2. create `docs/` if missing;
3. create `docs/specs/` if missing;
4. write and verify `docs/specs/a.md`; and
5. finalize the tracked directory modes and best-effort mtimes deepest first.

Directory creation uses a temporary owner-writable mode so a stored read-only
mode cannot prevent child restoration. Final permissions are applied only
after the subtree succeeds.

For a write over an existing file, show the exact relative target and state
that a backup will be created. For a deletion, always require explicit
approval, copy the existing bytes into the plan backup first, then remove only
the regular file. A tracked directory tombstone may remove only an empty
directory after explicit approval; it never recursively deletes content.

The result view reports project files independently:

```text
Pull complete
  6 project files restored
  4 project directories created or verified
  2 project files kept local
  1 deletion needs attention
```

## 6. Project-content identity and portable representation

Use the normalized project-relative path as stable provenance and a full
SHA-256 of the entry type plus that path in the resource ID:

```text
file resource       project:content-file:<sha256("file\0" + relative-path)>
directory resource  project:content-dir:<sha256("dir\0" + relative-path)>
logical path         project/<normalized-relative-path>
provenance           ProjectLocal { relative_path }
```

The relative path remains visible in the manifest because Pull must know its
target. It must never contain a source-machine absolute path.

The bound project root itself is machine-local and is not a portable directory
resource. Every selected descendant directory below that root is tracked.

Descriptor rules:

- `kind = ProjectContentFile | ProjectContentDirectory`;
- `scope = Project`;
- `apply_policy = ExplicitReview` for Pull, even for a new target;
- `codec_version = 1`;
- file metadata may contain bounded presentation facts such as binary/text,
  executable bit, and sensitive-content warning code;
- a directory descriptor owns exactly one directory entry and no file bytes;
  and
- mutable facts such as mtime and size are status data, not resource identity.
  The reviewed content digest includes safe permission bits but excludes mtime.

Extend the manifest with first-class directory entries:

```rust
struct BundleDirectoryEntry {
    resource_id: ResourceId,
    mode: Option<u32>,
    source_mtime: u64,
}

struct BundleManifest {
    // existing fields...
    directories: BTreeMap<LogicalPath, BundleDirectoryEntry>,
}
```

Validate that every directory entry has a `ProjectContentDirectory`
descriptor, every file's ancestors have directory entries, and no logical path
is both a file and a directory. Directories have no upload object because they
carry no content bytes.

The full path hash prevents path-length and resource-ID grammar problems.
Discovery must still detect an impossible hash collision and fail closed.

Add new enum variants instead of serializing generic content as the old
`ProjectFile` kind. Record codec versions for both project-content kinds.
Manifest/head schema 4 is the only supported project-bundle format after this
cutover. The implementation can replace the current bundle format directly;
no compatibility layer is required.

## 7. Bounded discovery and hard exclusions

Create a dedicated scanner in `provider_capture.rs` or a focused
`project_files.rs` module. It must walk without following links and produce
selectable, ignored, blocked, and warning summaries.

Reuse the current capture limits unless measurement justifies lower limits:

- at most 20,000 combined project-content file and directory resources;
- at most 16 MiB per captured file;
- at most 512 MiB across the selected capture;
- at most 32 path components of traversal depth; and
- normalized UTF-8 logical paths with the existing Windows-reserved-name and
  case-fold collision checks.

Hard exclusions cannot be overridden:

- `.git/**`, `.hg/**`, `.svn/**`, and Mallard application/storage metadata;
- symlinks, hard links, sockets, devices, FIFOs, and other special files;
- known credential filenames already denied by `denied_file_name`, including
  `.env*`, private keys, auth/token files, `.npmrc`, and `.netrc`;
- any path escaping the canonical project root;
- the mapped provider homes and schema-4 storage roots;
- any descendant root registered as a separate Mallard project, so a child
  project tree cannot also enter its parent's bundle; and
- project paths already owned by another resource descriptor.

Generated/dependency directories such as `node_modules`, `target`, caches,
and build output are ignored by default. Support root `.mallardignore` rules
and existing `.gitignore` / `.ignore` rules with Git-style matching even for a
non-Git folder. Ignore files themselves remain eligible regular files. Ignored
directories and their descendants do not get directory entries. The UI shows
ignored counts and rule sources; changing an ignore rule never silently turns
a missing candidate into a storage deletion.

Secret handling stays conservative:

- known credential files and private-key material are blocked;
- best-effort token markers in otherwise ordinary content produce a warning
  bound to that exact content digest;
- including a warned file requires an explicit acknowledgement on Review;
- a content change invalidates the acknowledgement; and
- the UI must say that scanning cannot prove a file secret-free.

Binary files are allowed within the same size limits. Executable files are not
run, but their executable mode and warning are shown and require explicit
review on both Push and Pull.

## 8. Eligibility must fail closed

Replace the frontend-only `Record<string, boolean>` interpretation with a
typed backend result:

```ts
interface ProjectFileSyncEligibility {
  state: "eligible" | "git_managed" | "unknown";
  reason: string;
  detected_root?: string | null;
}
```

The probe should canonicalize the binding and use `git rev-parse` to detect a
work tree, including a project folder nested below a repository root. A probe
error is `unknown`, not eligible.

Recheck eligibility:

1. when the Project files tab opens;
2. when a Push review is refreshed;
3. immediately before capture; and
4. while creating and applying a Pull plan.

If a user runs `git init` after reviewing files, Push or Pull fails without
publishing, writing, or creating tombstones. Previously stored generic content
remain visible but locked until the bundle is mapped to an eligible folder or
the user manages them with a supported newer workflow.

## 9. Backend model and commands

### 9.1 Lazy inventory API

Add a command shaped like:

```ts
inspectProjectFiles(
  localProjectId: string,
  storageId: string,
): Promise<ProjectContentInventory>
```

The response contains:

- eligibility and reason;
- canonical project root identity/revision;
- local file and directory candidates with descriptors, entry type, relative
  paths, mode, mtime, file size, and warning codes;
- storage-only and base-only paths needed for review;
- three-way state and local/storage/base digests;
- selected-in-link-recipe flags;
- the current per-link exclusions plus `newly_discovered` and
  `selected_after_scan` presentation flags;
- ignored/blocked counts and bounded warnings; and
- a review token covering eligibility, binding revision, storage head,
  selected entry set, file content digests, and directory metadata digests.

Do not put unbounded file bytes in this DTO.

### 9.2 Three-way status

The current generic `get_bundle_status` is a two-way equality check and calls
all differing versions `conflict`. Generic project content needs the same reviewed
base discipline already used for sessions and capabilities.

Load the exact manifest named by `RecipeBase.commit_id` and classify each path
and entry type using local, storage, and base digests. A directory digest
covers its normalized path and safe mode, not its children or mtime. If the
binding revision differs or the base manifest cannot be loaded, return
`unknown` and block affected Push actions.

### 9.3 Review-pinned Push

Extend the Push request with the project-content review token and the acknowledged
warning digests. The backend must:

1. revalidate binding and non-Git eligibility;
2. rediscover only requested generic file and directory resources;
3. read and hash file bytes and verify directory metadata without following
   links;
4. reject if any reviewed digest, warning, path, or storage head changed;
5. enforce existing total-size and logical-path limits; and
6. publish through the existing expected-head CAS flow.

Never trust a client-supplied relative path or resource ID without rediscovery.
Only a user-initiated full scan adds new candidates to the pending selection.
A targeted status refresh for already-published files must not discover and
select unrelated files behind the user's back.

For a selected generic entry that is now missing, unreadable, or ignored,
retain the previous stored version and block Push. Only a separately tracked,
confirmed **Remove from storage** decision may remove the recipe entry and
create its file/directory tombstones.

### 9.4 Restore actions and explicit skips

Add typed restore actions:

```rust
EnsureProjectDirectory {
    logical_path: LogicalPath,
    mode: u32,
    source_mtime: u64,
}

DeleteProjectFile {
    logical_path: LogicalPath,
    last_sha256: String,
}

DeleteProjectDirectory {
    logical_path: LogicalPath,
}
```

`EnsureProjectDirectory` is automatically required when an approved child
depends on it. It validates that an existing target is a real no-follow
directory, creates a missing target shallowest-first, and journals prior mode
metadata. `DeleteProjectFile` is built only from a validated file tombstone;
it verifies the target digest again, writes a recoverable backup, and deletes
only that file. `DeleteProjectDirectory` is built only from a validated
directory tombstone and can remove only an empty real directory. Both deletion
actions always require explicit approval.

Apply project content in a deterministic order:

1. validate the whole selected target set;
2. ensure directories shallowest-first with temporary owner-writable modes;
3. write and verify files;
4. apply approved file deletions;
5. remove approved empty directories deepest-first; and
6. finalize surviving directory modes and best-effort mtimes deepest-first.

Extend Pull submission so project-content actions can be reported as either
approved or explicitly kept local. An explicit keep-local decision receives a
review receipt; it is not confused with an action the user never saw. Once all
generic content actions have an apply/keep-local decision, the bundle base may
advance even when entries were skipped. This lets the optional tab coexist
with a successful Pull of other resource categories while preserving correct
directional status on the next Push.

Include typed resource kind/category context in the restore-plan DTO (for
example a plan-level descriptor map). `pullReviewModel.ts` must not infer
generic project files from a path prefix because existing agent setup files
also use `project/...`.

## 10. Bundle and deletion semantics

Reuse immutable uploads, manifests, expected-head CAS, backups, and apply
receipts. Do not create a second storage namespace or an independent project
files head.

The existing manifest already carries resource and file tombstones, but Pull
does not currently materialize deletions. In schema 4, add a typed directory
tombstone containing resource ID and logical path. Add deletion planning only
for the new project-content file/directory kinds in the first implementation.
Do not change deletion behavior for conversations, skills, or provider
configuration as a side effect.

Safety rules:

- deselecting a never-published local candidate is harmless;
- removing a published resource is a visible Push deletion;
- a missing or unreadable selected source is not evidence of deletion;
- a remote file tombstone deletes a local target only after explicit approval
  and only when its digest still matches the tombstone's last digest;
- a remote directory tombstone removes only the exact empty directory after
  explicit approval and never removes untracked descendants;
- a divergent local target is always kept;
- backups are retained under the existing restore-plan backup directory; and
- directory removals run deepest-first and never recurse.

## 11. Frontend component plan

Make review steps dynamic and shared instead of adding another hard-coded
array to each workflow.

- `SyncReviewTabs.tsx`
  - add `project_files` to `SyncReviewStep`;
  - accept an ordered `steps` array;
  - include scroll-position state for the new tab; and
  - keep keyboard Home/End/Arrow navigation within visible steps.
- `PushResourceWorkspace.tsx`
  - compute `history -> skills -> plugins -> project_files -> review` only
    when eligible or previously stored project content requires attention;
  - load the lazy inventory on first entry;
  - seed new eligible file/directory candidates as checked after the explicit
    scan and automatically include required ancestors;
  - keep selected entries, excluded entries, and explicit removal IDs
    separate; and
  - include project-content blockers and warning acknowledgements in Review.
- `RestorePlanView.tsx`
  - group typed directory/create, file/write, and delete actions into the new
    tab;
  - initialize all of them to keep-local;
  - submit apply versus keep-local decisions; and
  - report applied, skipped, blocked, and failed file/directory results
    separately.
- Add `ProjectFilesReviewPage.tsx`
  - render the relative-path tree, tri-state directory controls, search,
    status rows, ignored summary, and locked eligibility state;
  - label automatically checked candidates as `New · selected after scan` and
    provide a one-click way to clear only those new suggestions;
  - render only expanded branches so a bounded 20,000-entry response does not
    create 20,000 DOM rows;
  - show required ancestor directories, empty directories, and per-type
    file/folder counts; and
  - share row presentation between Push and Pull while keeping their actions
    mode-specific.
- `ProjectSyncV3.tsx`
  - carry eligibility, lazy file inventory, review token, removals, and
    acknowledgement/exclusion state in the pending Push/Pull session; and
  - reset it when project, storage, binding, or storage head changes.
- `model.ts`, `pullReviewModel.ts`, and `types.ts`
  - add typed category/state mapping and avoid resource-ID-prefix routing in
    presentation code.
- `App.css`
  - add the tree, status, locked-state, sensitive warning, and deletion review
    styles with existing light/dark theme variables.

Do not add the file picker to `ProjectSetupWorkspace` in v1. The first Push is
the correct explicit moment to opt in, and it avoids scanning every non-Git
folder during setup.

## 12. Implementation phases and gates

### Phase 1: Contracts and schema cutover

- Add typed eligibility, file/directory resource kinds, project-content
  inventory and states, review token, manifest directory entries, restore
  descriptor context, and directory/file actions.
- Implement schema-4 validation and publication before writing generic
  project-content manifests.
- Refactor Push and Pull to consume one dynamic step-order helper.

**Gate:** schema-4 manifests validate the file/directory tree and Git projects
retain the four-tab UI in the new build.

### Phase 2: Safe scanner and lazy inventory

- Implement no-follow discovery, ignore handling, ownership exclusions,
  stable IDs, limits, warnings, and eligibility checks.
- Add exact local/storage/base comparison and review-token generation.
- Seed newly discovered eligible candidates into the pending selection while
  preserving per-link explicit exclusions.
- Return blocked entries without failing the entire scan where it is safe to
  continue.

**Gate:** fixtures containing VCS metadata, symlinks, hard links, credential
files, nested ignored paths, case collisions, oversized files, and 20,000+
entries never escape the scanner's limits.

### Phase 3: Push

- Add the conditional Project files tab and lazy tree.
- Merge selected file and directory resources into the destination-specific
  recipe, including required ancestors.
- Add warning acknowledgement, explicit remote removal, reviewed-digest
  validation, and existing CAS publication.
- Refresh file state after success without changing selections for other tabs.

**Gate:** a non-Git folder scan preselects new eligible directories plus text
and binary files; `docs/`, `docs/specs/`, and `docs/specs/a.md` are all present
in the manifest, while an explicitly excluded, ignored, or
changed-after-review entry does not publish.

### Phase 4: Pull and deletion

- Classify file and directory actions from typed descriptors.
- Add apply/keep-local decisions, ordered directory creation, backup-pinned
  writes, digest-pinned `DeleteProjectFile`, and empty-only
  `DeleteProjectDirectory`.
- Allow other selected Pull categories to complete when project files are
  explicitly kept local.
- Lock all project-content actions if eligibility changes to Git-managed.

**Gate:** nested/empty directory creation, add, update, keep-local, divergent,
safe file/directory deletion, changed-before-apply, and
Git-initialized-after-plan cases all produce the expected receipts without
silent overwrite or recursive deletion.

### Phase 5: Hardening, performance, and documentation

- Add activity-log events with counts and no file contents or secret values.
- Measure scans on representative trees and keep file hashing off the main UI
  thread.
- Update README architecture, security boundary, and manual smoke steps.
- Document recovery of project-content backups and exact-entry selection.

**Gate:** two replicas converge through both local-folder storage and the stub
S3 harness, with stale-head, partial Pull, deletion, and retry coverage.

## 13. Test plan

The integration-first, fault-injection, and stress specification is maintained
in [TEST_PLAN_NON_GIT_PROJECT_CONTENT_SYNC.md](TEST_PLAN_NON_GIT_PROJECT_CONTENT_SYNC.md).
The cases below are the compact implementation checklist; the linked plan is
the release gate.

### Rust unit tests

Add focused tests near the scanner and bundle engine for:

- stable path-based IDs across two absolute project roots;
- Git work-tree rejection, including a project nested below the Git root;
- no-follow traversal and path-escape/case-collision rejection;
- hard exclusions and ignore precedence;
- exact file/directory selection, required ancestors, new-entry-after-scan
  default inclusion, and persistent explicit exclusions;
- directory entry validation, empty-directory round trip, and file/directory
  path collision rejection;
- per-file and total capture limits;
- sensitive warning digest invalidation;
- local/storage/base state classification;
- selected-missing retention versus explicit tombstone publication;
- schema-4 manifest validation;
- restore write backup and stale target rejection; and
- shallow-to-deep directory creation, deep-to-shallow metadata finalization,
  empty-only directory deletion, file deletion backup, and divergent deletion
  blocking.

### Command/integration tests

Extend `src-tauri/src/sync_tests` and command tests for:

1. Replica A scans `docs/specs/a.md` plus two other files, sees the files and
   every directory preselected, explicitly excludes one file, and Pushes.
2. The manifest independently contains `docs`, `docs/specs`, and
   `docs/specs/a.md`, plus an explicitly selected empty directory.
3. Replica B maps the same non-Git bundle and sees file writes unselected in
   Pull until explicitly approved.
4. B Pull creates `docs/`, then `docs/specs/`, then writes `a.md`; it also
   recreates the empty directory.
5. B keeps one local conflict, applies one file, and still applies sessions.
6. B changes the applied file and sees `local_ahead` against the reviewed base.
7. A explicitly removes a stored file and empty directory; B receives deletion
   review actions.
8. B's unchanged file is backed up and deleted only after approval; the
   directory is removed only when empty.
9. B's modified file or non-empty directory blocks deletion and remains
   unchanged.
10. Initializing Git between review and Push/Apply blocks the operation.
11. Selections remain isolated across two storages.
12. A stale storage head or changed local digest rejects publication.

### Frontend integration tests

Extend the current lightweight React tests to assert:

- the visible order is Plugins, Project files, Review for eligible projects;
- Git projects retain the existing Plugins, Review order;
- remote generic project content forces a visible locked tab when the binding
  is Git;
- before Scan there are zero pending generic entries, and after Scan every new
  eligible file/directory is selected and clearly labelled;
- selecting a nested file locks all ancestor directory rows as required;
- empty directories are visible and selectable;
- an explicitly excluded entry remains unchecked on later scans after a
  successful Push;
- directory bulk selection affects only currently listed descendants;
- Clear does not schedule deletion of published files;
- storage-only/diverged states block Push and link to Pull;
- Pull project content defaults to keep-local, with required directory actions
  selected when their child file is selected;
- Review separates file/folder creates, replacements, deletions, and skips;
  and
- keyboard tab navigation uses only the dynamic visible steps.

### Verification commands

```sh
npm run build
npm run test:frontend-integration
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --lib project_sync_v3::commands::tests
cargo test --manifest-path src-tauri/Cargo.toml --lib project_sync_v3::chat_history::tests
cargo test --manifest-path src-tauri/Cargo.toml --lib sync_tests
```

## 14. Acceptance criteria

- A non-Git project shows an optional Project files tab immediately after
  Plugins in Push and Pull.
- A Git project cannot capture or restore generic project files, even through
  a forged frontend request or a Git repository initialized after review.
- No full-folder discovery or new-file auto-selection occurs during setup or
  merely by opening a Push review. An explicit Project files scan preselects
  new eligible files and directories in the pending review, and only a
  successful final Push persists them or uploads file bytes.
  Already-published entries may still receive a targeted status check.
- The user can choose exact regular files and real directories anywhere
  beneath the canonical project root, subject to visible safety limits and
  exclusions.
- Every selected file has independently tracked ancestor directories in the
  manifest; Pull recreates them shallowest-first before copying the file.
- Empty selected directories round-trip without a placeholder file.
- New entries are never silently added by a previously selected directory;
  they are included only after a later explicit scan visibly preselects them.
- Project-file choices are isolated per project-storage link.
- Paths in storage are normalized and project-relative; no local checkout path
  enters resource identity or object keys.
- Credentials, VCS metadata, links, special files, escapes, collisions, and
  oversized captures are excluded or blocked.
- Changed local or storage state invalidates the review rather than publishing
  or applying unreviewed bytes.
- Pull never overwrites a different local file without explicit approval and a
  backup.
- Pull never deletes a file without explicit approval, matching digest, and a
  backup. It removes a tracked directory only with explicit approval and only
  when empty; it never recursively deletes directories.
- Skipping project files does not block selected non-file Pull work, and the
  next comparison still reports the correct direction.
- Git projects retain their four-step review behavior in the schema-4 build.

## 15. Non-goals for v1

- Replacing Git, cloning repositories, or syncing files from any Git work tree.
- Detecting or treating `github.com` differently from another Git remote.
- Automatic/background file sync or filesystem watching.
- Folder wildcard rules that automatically include future files.
- Rename detection, text merging, or conflict-marker generation for ordinary
  files.
- Syncing symlinks, hard links, devices, sockets, FIFOs, ACLs, xattrs, or file
  ownership.
- Syncing known credentials or claiming perfect secret detection.
- Executing restored files or granting trust/approval because a file was
  restored.
- Force-deleting a divergent local file.
- Changing tombstone behavior for existing conversations, skills, plugins, or
  provider configuration as part of this feature.
