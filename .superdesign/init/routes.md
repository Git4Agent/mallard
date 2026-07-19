# Routes

Mallard is a Tauri 2 desktop application with a single React/Vite entry point. It does not use a URL router.

| Destination | Entry/render owner | Shell |
| --- | --- | --- |
| Project history (default selected-project page) | `src/components/project-sync/ProjectLinksWorkspace.tsx` | `ProjectSyncV3` + `ProjectSidebar` |
| Project settings | dedicated `settingsProject` branch in `ProjectLinksWorkspace.tsx` | same shell |
| Storage settings | dedicated `editingStorage` branch in `ProjectLinksWorkspace.tsx` | same shell |
| Resumable project setup | `ProjectSetupWorkspace` passed as `newProjectSetup` | same shell |
| Legacy sync | `LegacyApp` in `src/App.tsx` | legacy app shell |

Navigation is state-driven. `ProjectSyncV3` owns the selected local project ID. Selecting a completed project closes any editor and renders the default history destination. Setup drafts are separate records and never enter the history destination.
