# Always-Visible Session Summary Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render every local Codex session card as a permanent two-row layout with metrics visible by default and chat history controlled separately.

**Architecture:** Remove the session-summary disclosure state and information button from `ThreadCard`. Always render the existing summary container and `ThreadMetrics`; retain `detailsOpen` exclusively for the independently expandable chat-history region.

**Tech Stack:** React 19, TypeScript, server-rendered Node integration tests, existing Mallard CSS.

## Global Constraints

- Keep “Load chat history” as an independent on-demand action.
- Preserve metric values, formatting, tooltips, icons, and accessible labels.
- Preserve chat loading, error, pagination, and collapse behavior.
- Preserve current palette, typography, spacing scale, and responsive wrapping.
- Do not change stored-only session cards, commit groups, or sync controls.

---

### Task 1: Specify the permanent two-row behavior

**Files:**
- Modify: `tests/frontend/project-chat-history.integration.test.tsx`

**Interfaces:**
- Consumes: `ProjectChatHistoryContent` server-rendered markup.
- Produces: regression assertions for always-visible metrics, absent session-details disclosure, and the independent chat-history button.

- [x] **Step 1: Update the default-card assertions**

Replace the old collapsed-state expectations with:

```ts
assert.doesNotMatch(html, /aria-label="Show session details"/);
assert.doesNotMatch(html, /aria-label="Hide session details"/);
assert.match(html, /aria-label="Started [^"]+"/);
assert.match(html, /aria-label="Ended [^"]+"/);
assert.match(html, /aria-label="User turns: 3"/);
assert.match(html, /data-tooltip="Total tokens · 24\.8K"/);
assert.match(html, /aria-label="Load chat history"/);
```

- [x] **Step 2: Run the focused frontend integration test and verify RED**

Run `node scripts/run-frontend-integration-tests.mjs tests/frontend/project-chat-history.integration.test.tsx`.

Expected: FAIL because the current card still renders `Show session details` and omits metrics and the chat-history button until expanded.

### Task 2: Render the summary row permanently

**Files:**
- Modify: `src/components/project-sync/ProjectChatHistoryPage.tsx`
- Test: `tests/frontend/project-chat-history.integration.test.tsx`

**Interfaces:**
- Consumes: existing `ThreadMetrics`, `detailsOpen`, `onToggleDetails`, and `detailsByThread` state.
- Produces: a `ThreadCard` whose summary row is unconditional and whose chat button alone controls `detailsOpen`.

- [x] **Step 1: Remove session-summary disclosure state**

Delete `localDetailsOpen`, `detailsId`, `sessionDetailsOpen`, `sessionDetailsLabel`, and `toggleSessionDetails`. Remove the info-button element from `.v3-history-thread-actions`.

- [x] **Step 2: Make the second row unconditional**

Render `.v3-history-session-details` without a session-summary condition. Keep `ThreadMetrics` and the chat-history button inside `.v3-history-session-summary`, and retain the existing `{onToggleDetails && detailsOpen && (...)}` chat region beneath it.

- [x] **Step 3: Run the focused frontend integration test and verify GREEN**

Run `node scripts/run-frontend-integration-tests.mjs tests/frontend/project-chat-history.integration.test.tsx`.

Expected: PASS.

### Task 3: Verify UI integration

**Files:**
- Modify only if verification reveals a scoped defect.

**Interfaces:**
- Consumes: completed permanent summary-row implementation.
- Produces: frontend suite, production build, and clean-diff evidence.

- [x] **Step 1: Run all frontend integration tests**

Run `npm run test:frontend-integration`.

Expected: all frontend integration tests PASS.

- [x] **Step 2: Build the production frontend**

Run `npm run build`.

Expected: TypeScript and Vite exit with status 0.

- [x] **Step 3: Inspect the final diff**

Run `git diff --check`, followed by `git status --short`.

Expected: no whitespace errors and only the component, test, and plan files are modified.

- [ ] **Step 4: Commit the implementation**

```bash
git add src/components/project-sync/ProjectChatHistoryPage.tsx tests/frontend/project-chat-history.integration.test.tsx docs/superpowers/plans/2026-07-20-always-visible-session-summary.md
git commit -m "Show session summaries by default"
```
