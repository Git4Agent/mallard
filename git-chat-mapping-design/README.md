# Git–Codex History Mapping

## Approved behavior

The selected completed project now opens its history page by default. The landed `Git Info` placeholder in `ProjectLinksWorkspace` is replaced in place; there is no second navigation state and no Back button. Clicking a project name or its Git-branch action invokes the same project-selection callback. Project and Storage Settings remain dedicated alternate pages, and closing either returns to the selected project's history.

The primary display name is always `projectLabel(project)`, so a machine-local alias wins. When an alias is active, the shared repository name is shown as `Repository: <shared name>` beneath the title. Setup drafts stay separate from completed projects: they have no history action and cannot reach the history command.

After setup finalization, normal project selection mounts a fresh history page. A successful Pull apply increments a refresh epoch so newly restored conversations are loaded without a manual page change.

## Annotated UI structure

```text
Resizable project sidebar                 Selected-project destination
┌─────────────────────────────┐          ┌────────────────────────────────────┐
│ Project row                 │          │ <local alias> history    [Refresh] │
│ [folder] name [branch][⚙][×]├─────────▶│ ~/compact/path          [Branch ▾]│
│                             │ same     │ Repository: shared-name (optional) │
│ Draft row                   │ handler  ├────────────────────────────────────┤
│ [folder] name [Draft]    [×]│          │ First-parent history               │
└─────────────────────────────┘          │ ● abc123  timestamp  subject       │
                                         │   ┌ thread title  [confidence] ┐   │
                                         │   │ dates · recorded SHA       │   │
                                         │   │ [Open Codex] [Terminal]    │   │
                                         │   └────────────────────────────┘   │
                                         │ ● older commit …                   │
                                         ├────────────────────────────────────┤
                                         │ Unmapped threads + explicit reason │
                                         └────────────────────────────────────┘
```

- Header controls use the existing button/select language and theme tokens.
- The branch selector stays with the sticky page header while commit content scrolls.
- The one new visual signature is the restrained one-pixel first-parent rail. It encodes real commit order; it is not a decorative timeline or a full Git DAG.
- Thread confidence is written as text as well as color: `during session`, `after session`, or `started from`.
- Loading, empty, warning, error, missing-profile, non-Git, unavailable-branch, and pagination states keep the same page geometry.
- For a non-Git directory, the Git selector/rail are omitted and a flat `Codex threads` list is ordered by last update.

## Mapping semantics

For the selected branch's bounded first-parent history, each project-owned thread is classified in this order:

Threads with a recorded branch are considered only on that branch. Older threads without branch metadata remain eligible as a best-effort fallback; a named thread is never attached to an unrelated branch.

1. Attach it to every commit whose commit time falls inclusively between the thread start and end times (`during_session`).
2. If no commit matched, attach only the first subsequent commit within 24 hours (`after_session`).
3. If still unmatched, attach its recorded SHA when that commit resolves on the selected branch (`started_from`).
4. Otherwise return it in `unmapped` with an explicit reason.

A recorded SHA is session context, not proof that the conversation authored a commit. `unique_thread_count` counts thread IDs once; `reference_count` counts every commit attachment.

## Main-branch findings incorporated

The current branch already contains main through merge `2edc284`. The relevant main commits are:

- `826d890` — schema-3 setup/synchronization changes, resumable setup drafts, and global persistence.
- `60e0ea5` — local project aliases.
- `d4d794a` — dedicated project settings and profile-management UI.

This implementation preserves the resizable sidebar, dedicated settings render branches, `~/.mallard` repository, optimistic revisions, active bindings, resumable setup workflow, and the landed default `Git Info` destination. It does not add `links | chat-history` state.

## Decisions made while the requester was away

- Kept the existing compact native desktop aesthetic in both themes; introduced no font, icon package, or new palette.
- Used a focused page component with a pure content renderer so Git/non-Git/error states can be tested without a Tauri runtime.
- Kept all summary extraction local and recomputed; no bundle manifest or sync-schema fields were added.
- Limited commit pages to 50 and bounded full-rail correlation to 10,000 first-parent commits.
- Disabled launch buttons for malformed thread IDs and revalidated UUID/project ownership again in Rust before Terminal launch.
- Continued implementation from the user-approved plan when the Superdesign service required an interactive login. No substitute or invented export was placed in `assets/`.
