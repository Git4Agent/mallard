import type { LocalProjectSummary, StorageConfigV3 } from "../../types";
import Icon from "../Icons";

interface Props {
  projects: LocalProjectSummary[];
  storages: StorageConfigV3[];
  activeProjectId: string | null;
  loading: boolean;
  busy: boolean;
  activityOpen: boolean;
  unreadLogs: number;
  onSelectProject: (projectId: string) => void;
  onConfigureProject: (projectId: string) => void;
  onRemoveProject: (projectId: string) => void;
  onToggleActivity: () => void;
  onAddProject: () => void;
  onRefresh: () => void;
  onOpenStorage: (storageId: string) => void;
  onAddStorage: () => void;
  onOpenLegacy: () => void;
}

export default function ProjectSidebar({
  projects,
  storages,
  activeProjectId,
  loading,
  busy,
  activityOpen,
  unreadLogs,
  onSelectProject,
  onConfigureProject,
  onRemoveProject,
  onToggleActivity,
  onAddProject,
  onRefresh,
  onOpenStorage,
  onAddStorage,
  onOpenLegacy,
}: Props) {
  return (
    <aside className="v3-sidebar">
      <div className="v3-sidebar-drag" data-tauri-drag-region />
      <div className="v3-sidebar-brand" data-tauri-drag-region>
        <strong>Agent Sync</strong>
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
          <Icon name="activity" size={15} /> Activity
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
          {loading && projects.length === 0 ? (
            <div className="sidebar-msg">Loading projects…</div>
          ) : projects.length === 0 ? (
            <button type="button" className="v3-sidebar-empty" onClick={onAddProject}>
              <Icon name="plus" size={15} /> Add your first project
            </button>
          ) : projects.map((project) => {
            const profileRequired = Object.keys(project.profile_ids ?? {}).length === 0;
            return (
              <div
                key={project.local_project_id}
                className={`sidebar-profile-item${activeProjectId === project.local_project_id ? " active" : ""}${profileRequired ? " needs-profile" : ""}`}
              >
                <button
                  type="button"
                  className="sidebar-profile-main"
                  onClick={() => onSelectProject(project.local_project_id)}
                  title={project.project_root ?? project.display_name}
                >
                  <Icon name="folder" size={15} />
                  <span>{project.display_name}</span>
                  {profileRequired && <Icon name="alert-triangle" size={12} className="v3-sidebar-profile-warning" />}
                </button>
                <div className="sidebar-profile-actions">
                  <button
                    type="button"
                    onClick={() => onConfigureProject(project.local_project_id)}
                    disabled={busy}
                    title={`Project settings for ${project.display_name}`}
                    aria-label={`Project settings for ${project.display_name}`}
                  >
                    <Icon name="settings" size={13} />
                  </button>
                  <button
                    type="button"
                    className="sidebar-profile-remove"
                    onClick={() => onRemoveProject(project.local_project_id)}
                    disabled={busy}
                    title={`Remove ${project.display_name} from Agent Sync; project files stay on disk`}
                    aria-label={`Remove ${project.display_name} from Agent Sync`}
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
          {storages.map((storage) => (
            <button
              key={storage.id}
              type="button"
              className="sidebar-link-item"
              onClick={() => onOpenStorage(storage.id)}
              title={`Configure ${storage.name || "storage"}`}
            >
              <Icon name={storage.kind === "local" ? "drive" : "cloud"} size={16} />
              <span>{storage.name || "(unnamed)"}</span>
            </button>
          ))}
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
