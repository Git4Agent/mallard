import type { LocalProjectSummary, SetupDraftSummary, StorageConfigV3 } from "../../types";
import Icon from "../Icons";
import { projectLabel } from "./model";

interface Props {
  projects: LocalProjectSummary[];
  drafts: SetupDraftSummary[];
  activeDraftId: string | null;
  storages: StorageConfigV3[];
  storageUsage: Record<string, number>;
  activeProjectId: string | null;
  activeStorageId: string | null;
  loading: boolean;
  busy: boolean;
  activityOpen: boolean;
  unreadLogs: number;
  onSelectProject: (projectId: string) => void;
  onConfigureProject: (projectId: string) => void;
  onRemoveProject: (projectId: string) => void;
  onSelectDraft: (draftId: string) => void;
  onDiscardDraft: (draftId: string) => void;
  onToggleActivity: () => void;
  onAddProject: () => void;
  onRefresh: () => void;
  onOpenStorage: (storageId: string) => void;
  onRemoveStorage: (storageId: string) => void;
  onAddStorage: () => void;
  onOpenLegacy: () => void;
}

export default function ProjectSidebar({
  projects,
  drafts,
  activeDraftId,
  storages,
  storageUsage,
  activeProjectId,
  activeStorageId,
  loading,
  busy,
  activityOpen,
  unreadLogs,
  onSelectProject,
  onConfigureProject,
  onRemoveProject,
  onSelectDraft,
  onDiscardDraft,
  onToggleActivity,
  onAddProject,
  onRefresh,
  onOpenStorage,
  onRemoveStorage,
  onAddStorage,
  onOpenLegacy,
}: Props) {
  // Folders hosting more than one project (one per provider config) show a
  // config badge so same-named siblings stay tellable apart.
  const rootCounts = new Map<string, number>();
  for (const project of projects) {
    const root = project.canonical_project_root?.toLowerCase();
    if (root) rootCounts.set(root, (rootCounts.get(root) ?? 0) + 1);
  }
  return (
    <aside className="v3-sidebar">
      <div className="v3-sidebar-drag" data-tauri-drag-region />
      <div className="v3-sidebar-brand" data-tauri-drag-region>
        <img className="v3-brand-logo" src="/mallard-logo.svg" alt="" data-tauri-drag-region />
        <strong>Mallard</strong>
      </div>

      <nav className="v3-primary-nav" aria-label="Project sync navigation">
        <button
          type="button"
          className="active"
          onClick={() => activeProjectId && onSelectProject(activeProjectId)}
        >
          <Icon name="folder" size={15} /> Projects
        </button>
        <button
          type="button"
          className={activityOpen ? "active" : undefined}
          onClick={onToggleActivity}
          aria-expanded={activityOpen}
        >
          <Icon name="activity" size={15} /> Synclog
          {unreadLogs > 0 && !activityOpen && (
            <span className="sidebar-nav-badge v3-activity-badge">{unreadLogs}</span>
          )}
        </button>
      </nav>

      <div className="v3-sidebar-sections sidebar-settings-sections">
        <div className="sidebar-section-heading">
          <div className="sidebar-section-label">
            <span className="sidebar-section-title">Projects</span>
            <span className="sidebar-section-count">{projects.length}</span>
          </div>
          <div className="sidebar-heading-actions">
            <button
              type="button"
              className="sidebar-section-action"
              onClick={onRefresh}
              disabled={loading}
              title="Refresh projects"
              aria-label="Refresh projects"
            >
              <Icon name="refresh" size={15} className={loading ? "icon-spin" : undefined} />
            </button>
            <button
              type="button"
              className="sidebar-section-action sidebar-add-action"
              onClick={onAddProject}
              disabled={busy}
              title="Add project"
              aria-label="Add project"
            >
              <Icon name="plus" size={16} />
            </button>
          </div>
        </div>

        <div className="sidebar-profile-list">
          {drafts.map((draft) => (
            <div
              key={draft.draft_id}
              className={`sidebar-profile-item v3-sidebar-draft${activeDraftId === draft.draft_id ? " active" : ""}`}
            >
              <button
                type="button"
                className="sidebar-profile-main"
                onClick={() => onSelectDraft(draft.draft_id)}
                title={draft.last_error
                  ? `${draft.project_root} — last attempt failed: ${draft.last_error}`
                  : `${draft.project_root} — resumable setup draft`}
              >
                <Icon name="folder" size={15} />
                <span>{draft.display_name}</span>
                <span className={`v3-draft-badge${draft.status === "attention" ? " attention" : ""}`}>Draft</span>
              </button>
              <div className="sidebar-profile-actions">
                <button
                  type="button"
                  onClick={() => onSelectDraft(draft.draft_id)}
                  disabled={busy}
                  title={`Continue setup for ${draft.display_name}`}
                  aria-label={`Continue setup for ${draft.display_name}`}
                >
                  <Icon name="settings" size={13} />
                </button>
                <button
                  type="button"
                  className="sidebar-profile-remove"
                  onClick={() => onDiscardDraft(draft.draft_id)}
                  disabled={busy}
                  title={`Discard the setup draft for ${draft.display_name}; no project files are touched`}
                  aria-label={`Discard the setup draft for ${draft.display_name}`}
                >
                  <Icon name="trash" size={13} />
                </button>
              </div>
            </div>
          ))}
          {loading && projects.length === 0 && drafts.length === 0 ? (
            <div className="sidebar-msg">Loading projects…</div>
          ) : projects.length === 0 && drafts.length === 0 ? (
            <button type="button" className="v3-sidebar-empty" onClick={onAddProject}>
              <Icon name="plus" size={15} /> Add your first project
            </button>
          ) : projects.map((project) => {
            const profileRequired = Object.keys(project.profile_ids ?? {}).length !== 1;
            const label = projectLabel(project);
            const aliased = label !== project.display_name;
            const configNames = (project.profile_names ?? []).join(" + ");
            const sharedRoot = Boolean(project.canonical_project_root)
              && (rootCounts.get((project.canonical_project_root as string).toLowerCase()) ?? 0) > 1;
            return (
              <div
                key={project.local_project_id}
                className={`sidebar-profile-item${!activeDraftId && !activeStorageId && activeProjectId === project.local_project_id ? " active" : ""}${profileRequired ? " needs-profile" : ""}`}
              >
                <button
                  type="button"
                  className="sidebar-profile-main"
                  onClick={() => onSelectProject(project.local_project_id)}
                  title={[
                    aliased ? `Repo: ${project.display_name}` : null,
                    project.project_root ?? null,
                    configNames ? `Config: ${configNames}` : null,
                  ].filter(Boolean).join("\n") || label}
                >
                  <Icon name="folder" size={15} />
                  <span>{label}</span>
                  {project.is_git_repository === true && (
                    <span className="v3-repository-kind" title="Git repository">
                      <Icon name="git-branch" size={10} />
                      git
                    </span>
                  )}
                  {sharedRoot && configNames && (
                    <span className="v3-repository-kind" title={`Provider config: ${configNames}`}>
                      {configNames}
                    </span>
                  )}
                  {profileRequired && <Icon name="alert-triangle" size={12} className="v3-sidebar-profile-warning" />}
                </button>
                <div className="sidebar-profile-actions">
                  <button
                    type="button"
                    onClick={() => onConfigureProject(project.local_project_id)}
                    disabled={busy}
                    title={`Project settings for ${label}`}
                    aria-label={`Project settings for ${label}`}
                  >
                    <Icon name="settings" size={13} />
                  </button>
                  <button
                    type="button"
                    className="sidebar-profile-remove"
                    onClick={() => onRemoveProject(project.local_project_id)}
                    disabled={busy}
                    title={`Remove ${label} from Mallard; project files stay on disk`}
                    aria-label={`Remove ${label} from Mallard`}
                  >
                    <Icon name="trash" size={13} />
                  </button>
                </div>
              </div>
            );
          })}
        </div>

        <div className="sidebar-section-divider" />

        <div className="sidebar-section-heading">
          <div className="sidebar-section-label">
            <span className="sidebar-section-title">Storage</span>
            <span className="sidebar-section-count">{storages.length}</span>
          </div>
          <button
            type="button"
            className="sidebar-section-action sidebar-add-action"
            onClick={onAddStorage}
            title="Add storage"
            aria-label="Add storage"
          >
            <Icon name="plus" size={16} />
          </button>
        </div>
        <div className="sidebar-links-list">
          {storages.map((storage) => {
            const usage = storageUsage[storage.id] ?? 0;
            const storageName = storage.name || "storage";
            return (
              <div
                key={storage.id}
                className={`sidebar-profile-item sidebar-storage-item${activeStorageId === storage.id ? " active" : ""}`}
              >
                <button
                  type="button"
                  className="sidebar-profile-main"
                  onClick={() => onOpenStorage(storage.id)}
                  title={`Configure ${storageName}`}
                >
                  <Icon name={storage.kind === "local" ? "drive" : "cloud"} size={16} />
                  <span>{storage.name || "(unnamed)"}</span>
                </button>
                <div className="sidebar-profile-actions">
                  <button
                    type="button"
                    onClick={() => onOpenStorage(storage.id)}
                    disabled={busy}
                    title={`Storage settings for ${storageName}`}
                    aria-label={`Storage settings for ${storageName}`}
                  >
                    <Icon name="settings" size={13} />
                  </button>
                  <button
                    type="button"
                    className="sidebar-profile-remove"
                    onClick={() => onRemoveStorage(storage.id)}
                    disabled={busy || usage > 0}
                    title={usage > 0
                      ? `${storageName} is linked to ${usage} project${usage === 1 ? "" : "s"}; unlink it before removing`
                      : `Remove ${storageName}; synced files stay in storage`}
                    aria-label={usage > 0
                      ? `Cannot remove ${storageName} while it is linked to a project`
                      : `Remove ${storageName}`}
                  >
                    <Icon name="trash" size={13} />
                  </button>
                </div>
              </div>
            );
          })}
        </div>
      </div>

      <div className="v3-sidebar-footer">
        <button type="button" onClick={onOpenLegacy}>
          <Icon name="computer" size={14} /> Legacy profiles
        </button>
      </div>
    </aside>
  );
}
