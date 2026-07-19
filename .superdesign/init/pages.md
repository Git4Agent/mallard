# Page Dependency Trees

## Default selected-project history destination

Entry: `src/components/project-sync/ProjectLinksWorkspace.tsx`

- `src/components/project-sync/ProjectLinksWorkspace.tsx`
  - `src/components/project-sync/ProjectChatHistoryPage.tsx` (new focused page)
    - `src/components/project-sync/api.ts`
    - `src/components/project-sync/model.ts`
    - `src/components/Icons.tsx`
  - `src/components/project-sync/ResourceInventory.tsx`
  - `src/components/project-sync/StorageSettingsV3.tsx`
  - `src/components/Icons.tsx`
- `src/components/project-sync/ProjectSyncV3.tsx`
  - `src/components/project-sync/ProjectSidebar.tsx`
  - `src/components/project-sync/ProjectLinksWorkspace.tsx`
  - `src/components/project-sync/ProjectSetupWorkspace.tsx`
  - `src/components/project-sync/RestorePlanView.tsx`
  - `src/components/LogPanel.tsx`
- `src/App.css`
- `.superdesign/design-system.md`

Actual render branch: when there is an active completed project and neither dedicated settings page nor setup draft is open, `ProjectLinksWorkspace` renders `ProjectChatHistoryPage`. The completed project row is the only history navigation target; its repository-type chip is informational.

## Project settings

Entry: dedicated `settingsProject` branch in `src/components/project-sync/ProjectLinksWorkspace.tsx`

- `ProjectLinksWorkspace.tsx`
  - `StorageSettingsV3.tsx`
  - `ResourceInventory.tsx`
  - `Icons.tsx`
- `App.css`
