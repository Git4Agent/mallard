# Extractable Components

## ProjectSidebar
- Source: `src/components/project-sync/ProjectSidebar.tsx`
- Category: layout
- Description: resizable-shell sidebar with completed projects, setup drafts, storage destinations, and activity navigation.
- Extractable props: `activeProjectId`, `activityOpen`, project/draft/storage collections, unread count.
- Hardcoded: Agent Sync brand, Projects/Storage labels, outline icon system, row density, settings/remove actions.

## ProjectHistoryHeader
- Source: `src/components/project-sync/ProjectChatHistoryPage.tsx`
- Category: layout
- Description: selected-project title/path, optional shared repository name, refresh, and sticky branch selector.
- Extractable props: project label, path, shared name, branch options, selected branch, loading.
- Hardcoded: “history” suffix, Refresh action, existing theme tokens.

## CommitRail
- Source: `src/components/project-sync/ProjectChatHistoryPage.tsx`
- Category: basic
- Description: first-parent chronological rail with SHA, timestamp, subject, and attached thread cards.
- Extractable props: commits and pagination state.
- Hardcoded: rail geometry, confidence vocabulary, outline icons.
