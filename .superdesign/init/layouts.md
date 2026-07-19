# Shared Layouts

## ProjectSyncV3 shell

- Path: `src/components/project-sync/ProjectSyncV3.tsx`
- Description: resizable project sidebar, titlebar/theme control, selected-project workspace, optional sync log drawer, and modal restore review.

```tsx
return (
  <div className={`v3-app${resizingSidebar ? " resizing-sidebar" : ""}`}
    style={{ "--v3-sidebar-width": `${sidebarWidth}px` } as CSSProperties}>
    <ProjectSidebar projects={projects} drafts={setupDrafts} activeProjectId={activeProjectId} />
    <div className="v3-sidebar-resizer" role="separator" aria-label="Resize sidebar"
      aria-orientation="vertical" tabIndex={0} />
    <div className="v3-workspace">
      <div className="v3-titlebar" data-tauri-drag-region>
        <button className="v3-theme-button">Light theme</button>
      </div>
      <ProjectLinksWorkspace projects={projects} bindings={bindings}
        activeProjectId={activeProjectId} />
      <div className={`log-drawer v3-log-drawer${activityOpen ? " open" : ""}`}>
        <LogPanel />
      </div>
    </div>
  </div>
);
```

## ProjectSidebar completed-project row

- Path: `src/components/project-sync/ProjectSidebar.tsx`
- Description: stable machine-local project navigation. Draft rows use a different render branch and only expose discard.

```tsx
const label = project.local_alias?.trim() || project.display_name;
const aliased = label !== project.display_name;
return (
  <div className={`sidebar-profile-item${active ? " active" : ""}`}>
    <button className="sidebar-profile-main" onClick={() => onSelectProject(project.local_project_id)}>
      <Icon name="folder" size={15} />
      <span>{label}</span>
    </button>
    <div className="sidebar-profile-actions">
      <button aria-label={`Project settings for ${label}`}><Icon name="settings" size={13}/></button>
      <button className="sidebar-profile-remove"><Icon name="trash" size={13}/></button>
    </div>
  </div>
);
```

## Current default destination

- Path: `src/components/project-sync/ProjectLinksWorkspace.tsx`
- Description: landed main-branch placeholder that the history component replaces.

```tsx
if (!settingsProject && !editingStorage && !newProjectSetup) {
  return (
    <main className="v3-main v3-project-links-page v3-git-info-page">
      <section className="profile-links-section" aria-labelledby="git-info-heading">
        <div className="profile-links-heading">
          <div className="profile-links-copy">
            <h1 id="git-info-heading" className="settings-section-title">Git Info</h1>
          </div>
        </div>
      </section>
    </main>
  );
}
```
