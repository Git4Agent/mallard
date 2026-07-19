# Extractable Components

## ProjectSidebar
- Source: `src/components/project-sync/ProjectSidebar.tsx`
- Category: layout
- Description: resizable-shell sidebar with completed projects, setup drafts, storage destinations, and activity navigation.
- Extractable props: `activeProjectId`, `activityOpen`, project/draft/storage collections, unread count.
- Hardcoded: Agent Sync brand, Projects/Storage labels, outline icon system, row density, repository-type indicator, settings/remove actions.

## ProjectHistoryHeader
- Source: `src/components/project-sync/ProjectChatHistoryPage.tsx`
- Category: layout
- Description: selected-project activity title, project/profile/storage facts, refresh, and sticky branch selector for Git projects.
- Extractable props: project label, path, shared name, branch options, selected branch, loading.
- Hardcoded: “activity” suffix, Refresh action, existing theme tokens.

## CommitRail
- Source: `src/components/project-sync/ProjectChatHistoryPage.tsx`
- Category: basic
- Description: first-parent chronological rail with SHA, timestamp, subject, and attached thread cards.
- Extractable props: commits and pagination state.
- Hardcoded: rail geometry and outline icons; mapping badges are intentionally absent.

## SessionCard
- Source: `src/components/project-sync/ProjectChatHistoryPage.tsx`
- Category: basic
- Description: neutral session title, launch actions, explicit start/end dates, metrics, repeated-occurrence explanation, and lazy message disclosure.
- Extractable props: summary DTO, occurrence key, shared detail cache state, launch/detail callbacks.
- Hardcoded: 50-message page label, existing metric vocabulary, one-line 240-character previews.
