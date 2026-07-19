# Gaps and Validation

## Compatibility and assumptions

- V1 is Codex-only and scans only local/restored rollouts belonging to the bound profile. Claude and remote-only conversations are deferred.
- Aliases are machine-local presentation and never influence repository fingerprinting, branch selection, Git identity, or synced metadata.
- Setup drafts are not registered projects; history is unavailable until finalization succeeds.
- Session/commit correlation is best effort. Recorded SHA means “session started from this context,” not “thread authored this commit.”
- The first-parent rail intentionally suppresses the rest of the Git DAG.
- Rebase/amend/cherry-pick operations, shallow history, deleted branches, timestamp skew, and active sessions can reduce confidence or leave a thread unmapped.
- Restored rollouts can be visible to Mallard before Codex rebuilds its own desktop index.

## Functional and technical limitations

- `codex://threads/<id>` is not confirmed by current public Codex documentation. The opener is narrowly scoped and failure has a supported Terminal fallback, but the URI must be verified against the installed desktop app.
- macOS Terminal is the only automated terminal target. Windows/Linux launchers are deferred.
- History is recomputed on request. The 10,000-commit and 10,000-rollout guards prevent unbounded work but very large projects may produce a truncation warning.
- Full-text search, author/causal attribution, full-DAG visualization, remote merge semantics, Claude launch actions, and sync-schema changes are out of scope.
- Commit timestamps can be rewritten and session clocks can skew. Confidence labels communicate the rule used, not a probability.
- A deleted recorded branch is retained as unavailable context; it cannot produce a commit rail unless the ref becomes available again.

## Automated validation

Baseline before implementation:

- `npm run build` — passed.
- `npm run test:frontend-integration` — 2/2 passed.
- `npm run test:backend-integration` — 27/27 passed.
- `cd src-tauri && cargo check` — passed.

Added frontend integration coverage:

- alias as the primary history title and shared repository name as secondary metadata,
- commit/thread/confidence/launch action rendering,
- non-Git flat thread rendering,
- completed-project history action and absence on draft rows,
- existing Pull review behavior remains green.

Added focused Rust coverage includes:

- rollout metadata aliases and fallback first-user-message summary,
- session-index title/time precedence,
- malformed/oversized JSONL partial warnings,
- canonical ownership boundaries,
- inclusive multi-commit session mapping and unique/reference counts,
- first subsequent commit with the 24-hour cutoff,
- recorded-SHA fallback,
- named-branch isolation while allowing legacy sessions with missing branch metadata,
- first-parent pagination and hostile branch/cursor rejection,
- terminal UUID and shell quoting safety,
- recorded historical branch availability behavior.

Final verification after review fixes:

- `npm run build` — passed (the existing Vite large-chunk advisory remains).
- `npm run test:frontend-integration` — 6/6 passed.
- existing schema-3 command integration tests — 27/27 passed.
- focused `project_sync_v3::chat_history::tests` — 13/13 passed.
- `cd src-tauri && cargo check` — passed.
- focused `rustfmt --check` for the new Rust module and `git diff --check` — passed.

## Manual validation still required

- Open the macOS Tauri build and verify dense/long commit histories in dark and light themes.
- Resize the sidebar to its 220px minimum and confirm the branch/settings/remove actions remain operable.
- Test `Open in Codex` against an installed Codex app and a real UUID.
- Test `Open in Terminal` with a project path containing spaces and a single quote.
- Pull a restored rollout, apply it, close the review, and confirm the visible history refreshes.
- Verify a shallow clone, linked Git worktree, detached HEAD, merge commit, and deleted historical branch fixture with real repositories.

## Design-skill notes

`frontend-design` kept the page subject-specific: commit order is the structural device, and the single visual signature is the first-parent rail. The page avoids generic dashboard cards, gradients, new fonts, and decorative metrics.

`ui-ux-pro-max` informed the dense developer-tool layout, stable list keys, visible loading/empty/error recovery, text-backed confidence labels, keyboard focus, and restrained motion. Its suggested generic green-on-slate palette and oversized minimalism were rejected because they conflict with the merged application's established theme tokens.

Superdesign initialization was completed against the merged UI in `.superdesign/init/`, including the landed `Git Info` branch, resizable sidebar, icon system, current tokens, state-driven routes, and local-alias conventions. The local design system is in `.superdesign/design-system.md`.

The required remote Superdesign project/export could not be created because the CLI returned `Not authenticated. Run superdesign login first.` The requester had said they would be away and delegated decisions, so implementation proceeded from the explicitly approved plan. `git-chat-mapping-design/assets/` is intentionally empty: no fabricated or unapproved export was substituted. After login, reproduce the landed placeholder first, create the single compact history variant from the initialized context, and place only that approved export in the folder.
