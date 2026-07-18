import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  BundlePage,
  BundleReadiness,
  LocalProjectSummary,
  ProjectBinding,
  ProjectProvider,
  ProjectResourceDescriptor,
  ProjectStorageLink,
  ProviderProfileSummary,
  StorageConfigV3,
} from "../../types";
import Icon from "../Icons";
import ResourceInventory from "./ResourceInventory";
import { newStorage, StorageEditor } from "./StorageSettingsV3";
import { projectSyncApi } from "./api";
import { compactProjectPath, errorMessage, formatRelativeTime } from "./model";

type LinkKey = { projectId: string; storageId: string };
type StorageEditorRequest =
  | { mode: "toggle"; storageId: string; requestId: number }
  | { mode: "create"; storageKind: "local" | "s3"; requestId: number }
  | { mode: "close"; requestId: number };
type ProjectEditorRequest = { mode: "toggle" | "close"; projectId: string; requestId: number };

interface Props {
  projects: LocalProjectSummary[];
  bindings: ProjectBinding[];
  profiles: ProviderProfileSummary[];
  activeProjectId: string | null;
  resources: ProjectResourceDescriptor[];
  selected: Set<string>;
  statuses: Map<string, string>;
  storages: StorageConfigV3[];
  links: ProjectStorageLink[];
  readiness: BundleReadiness | null;
  loading: boolean;
  busy: boolean;
  selectionDirty: boolean;
  error: string | null;
  onSelectProject: (projectId: string, storageId?: string | null) => Promise<void> | void;
  onToggleResource: (resourceId: string) => void;
  onSaveRecipe: () => void;
  onLinkStorage: (projectId: string, storageId: string) => Promise<void> | void;
  onPush: (projectId: string, storageId: string) => Promise<void> | void;
  onPull: (projectId: string, storageId: string) => Promise<void> | void;
  onRepair: (projectId: string, storageId: string) => Promise<void> | void;
  onSaveProjectPath: (projectId: string, path: string) => Promise<void> | void;
  onAssignProfile: (projectId: string, provider: ProjectProvider, profileId: string | null) => Promise<void> | void;
  onAddProfilePath: (projectId: string, provider: ProjectProvider, path: string) => Promise<void> | void;
  onRemoveProject: (projectId: string) => Promise<void> | void;
  onRefresh: () => void;
  onAddProject: () => void;
  onOpenStorageSettings: (storageId?: string) => void;
  onSaveStorage: (storage: StorageConfigV3) => Promise<void> | void;
  storageEditorRequest?: StorageEditorRequest | null;
  onStorageEditorRequestHandled?: () => void;
  projectEditorRequest?: ProjectEditorRequest | null;
  onProjectEditorRequestHandled?: () => void;
  newProjectSetup?: ReactNode;
}

function storageSubtitle(storage: StorageConfigV3): string {
  if (storage.kind === "local") return compactProjectPath(storage.local_dir || "Folder not configured");
  return storage.bucket || storage.s3_endpoint || "S3 storage not configured";
}

function statusCounts(statuses: Map<string, string>) {
  let local = 0;
  let remote = 0;
  let conflict = 0;
  for (const status of statuses.values()) {
    if (["local_only", "local_ahead", "new", "modified"].includes(status)) local += 1;
    if (["remote_only", "remote_ahead", "cloud_only", "cloud_ahead"].includes(status)) remote += 1;
    if (status === "conflict") conflict += 1;
  }
  return { local, remote, conflict };
}

function sameLink(left: LinkKey | null, projectId: string, storageId: string): boolean {
  return left?.projectId === projectId && left.storageId === storageId;
}

export default function ProjectLinksWorkspace({
  projects,
  bindings,
  profiles,
  activeProjectId,
  resources,
  selected,
  statuses,
  storages,
  links,
  readiness,
  loading,
  busy,
  selectionDirty,
  error,
  onSelectProject,
  onToggleResource,
  onSaveRecipe,
  onLinkStorage,
  onPush,
  onPull,
  onRepair,
  onSaveProjectPath,
  onAssignProfile,
  onAddProfilePath,
  onRemoveProject,
  onRefresh,
  onAddProject,
  onOpenStorageSettings,
  onSaveStorage,
  storageEditorRequest,
  onStorageEditorRequestHandled,
  projectEditorRequest,
  onProjectEditorRequestHandled,
  newProjectSetup,
}: Props) {
  const [expandedLink, setExpandedLink] = useState<LinkKey | null>(null);
  const [expandedProvider, setExpandedProvider] = useState<ProjectProvider | null>(null);
  const [linkingProjectId, setLinkingProjectId] = useState<string | null>(null);
  const [runningAction, setRunningAction] = useState<string | null>(null);
  const [editingStorage, setEditingStorage] = useState<LinkKey | null>(null);
  const [storageDraft, setStorageDraft] = useState<StorageConfigV3 | null>(null);
  const [bundlePage, setBundlePage] = useState<BundlePage | null>(null);
  const [bundleLoading, setBundleLoading] = useState(false);
  const [bundleError, setBundleError] = useState<string | null>(null);
  const [providerPathDraft, setProviderPathDraft] = useState("");
  const [editingProjectId, setEditingProjectId] = useState<string | null>(null);
  const [projectPathDraft, setProjectPathDraft] = useState("");
  const bundleRequestRef = useRef(0);
  const storageSettingsRef = useRef<HTMLDivElement>(null);
  const projectSettingsRef = useRef<HTMLDivElement>(null);
  const counts = statusCounts(statuses);

  useEffect(() => {
    if (!activeProjectId) return;
    const frame = window.requestAnimationFrame(() => {
      document.getElementById(`project-card-${activeProjectId}`)?.scrollIntoView({
        block: "nearest",
        behavior: "smooth",
      });
    });
    return () => window.cancelAnimationFrame(frame);
  }, [activeProjectId]);

  useEffect(() => {
    if (!editingStorage) return;
    const frame = window.requestAnimationFrame(() => {
      storageSettingsRef.current?.scrollIntoView({ block: "center", behavior: "smooth" });
    });
    return () => window.cancelAnimationFrame(frame);
  }, [editingStorage]);

  useEffect(() => {
    if (!editingProjectId) return;
    const frame = window.requestAnimationFrame(() => {
      projectSettingsRef.current?.scrollIntoView({ block: "nearest", behavior: "smooth" });
    });
    return () => window.cancelAnimationFrame(frame);
  }, [editingProjectId]);

  const linkedByProject = useMemo(() => new Map(projects.map((project) => [
    project.local_project_id,
    links.filter((link) => link.local_project_id === project.local_project_id),
  ])), [links, projects]);
  const bindingByProject = useMemo(() => new Map(bindings
    .filter((candidate) => candidate.state === "active")
    .map((candidate) => [candidate.local_project_id, candidate])), [bindings]);

  useEffect(() => {
    if (!expandedLink || !expandedProvider) return;
    const projectBinding = bindingByProject.get(expandedLink.projectId);
    const profileId = projectBinding?.profile_ids?.[expandedProvider];
    const profile = profiles.find((candidate) => candidate.profile_id === profileId);
    setProviderPathDraft(profile?.path ?? "");
  }, [expandedLink, expandedProvider, bindingByProject, profiles]);
  const savedEditedStorage = editingStorage
    ? storages.find((storage) => storage.id === editingStorage.storageId) ?? null
    : null;
  const editedStorageConfig = savedEditedStorage
    ?? (storageDraft?.id === editingStorage?.storageId ? storageDraft : null);
  const creatingStorage = !!editingStorage && !savedEditedStorage;
  const orderedRemoteBundles = [...(bundlePage?.bundles ?? [])].sort((left, right) => {
    return (right.updated_at ?? 0) - (left.updated_at ?? 0);
  });

  const run = async (key: string, action: () => Promise<void> | void) => {
    setRunningAction(key);
    try {
      await action();
    } finally {
      setRunningAction(null);
    }
  };

  const toggleDetails = async (projectId: string, storageId: string, provider: ProjectProvider) => {
    if (sameLink(expandedLink, projectId, storageId) && expandedProvider === provider) {
      setExpandedLink(null);
      setExpandedProvider(null);
      return;
    }

    bundleRequestRef.current += 1;
    setEditingStorage(null);
    setStorageDraft(null);
    setBundleLoading(false);
    setExpandedLink({ projectId, storageId });
    setExpandedProvider(provider);
    await onSelectProject(projectId, storageId);
  };

  const loadStorageBundles = async (storageId: string) => {
    const requestId = ++bundleRequestRef.current;
    setBundlePage(null);
    setBundleError(null);
    setBundleLoading(true);
    try {
      const bundles = await projectSyncApi.listRemoteBundleSnapshots(storageId);
      if (bundleRequestRef.current === requestId) setBundlePage({ bundles });
    } catch (reason) {
      if (bundleRequestRef.current === requestId) {
        setBundleError(errorMessage(reason));
      }
    } finally {
      if (bundleRequestRef.current === requestId) setBundleLoading(false);
    }
  };

  useEffect(() => {
    if (!storageEditorRequest) return;
    if (storageEditorRequest.mode === "close") {
      bundleRequestRef.current += 1;
      setEditingStorage(null);
      setStorageDraft(null);
      setBundlePage(null);
      setBundleError(null);
      setBundleLoading(false);
      onStorageEditorRequestHandled?.();
      return;
    }
    if (storageEditorRequest.mode === "create") {
      const storage = newStorage(storageEditorRequest.storageKind, storages.length + 1);
      bundleRequestRef.current += 1;
      setExpandedLink(null);
      setExpandedProvider(null);
      setEditingProjectId(null);
      setEditingStorage({ projectId: "", storageId: storage.id });
      setStorageDraft(storage);
      setBundlePage(null);
      setBundleError(null);
      setBundleLoading(false);
      onStorageEditorRequestHandled?.();
      return;
    }
    const storage = storages.find((candidate) => candidate.id === storageEditorRequest.storageId);
    if (!storage) return;
    if (editingStorage?.storageId === storage.id) {
      bundleRequestRef.current += 1;
      setEditingStorage(null);
      setStorageDraft(null);
      setBundlePage(null);
      setBundleError(null);
      setBundleLoading(false);
      onStorageEditorRequestHandled?.();
      return;
    }

    setExpandedLink(null);
    setExpandedProvider(null);
    setEditingProjectId(null);
    setEditingStorage({ projectId: "", storageId: storage.id });
    setStorageDraft({ ...storage });
    void loadStorageBundles(storage.id);
    onStorageEditorRequestHandled?.();
  }, [storageEditorRequest?.requestId]);

  useEffect(() => {
    if (!projectEditorRequest) return;
    if (projectEditorRequest.mode === "close") {
      setEditingProjectId(null);
      setProjectPathDraft("");
      onProjectEditorRequestHandled?.();
      return;
    }
    const project = projects.find((candidate) => candidate.local_project_id === projectEditorRequest.projectId);
    if (!project) return;
    if (editingProjectId === project.local_project_id) {
      setEditingProjectId(null);
      setProjectPathDraft("");
      onProjectEditorRequestHandled?.();
      return;
    }
    const projectBinding = bindingByProject.get(project.local_project_id);
    bundleRequestRef.current += 1;
    setExpandedLink(null);
    setExpandedProvider(null);
    setEditingStorage(null);
    setStorageDraft(null);
    setEditingProjectId(project.local_project_id);
    setProjectPathDraft(projectBinding?.project_root ?? project.project_root ?? "");
    void onSelectProject(project.local_project_id);
    onProjectEditorRequestHandled?.();
  }, [projectEditorRequest?.requestId]);

  const toggleProjectEditor = async (projectId: string, currentPath: string) => {
    if (editingProjectId === projectId) {
      setEditingProjectId(null);
      return;
    }
    bundleRequestRef.current += 1;
    setExpandedLink(null);
    setExpandedProvider(null);
    setEditingStorage(null);
    setStorageDraft(null);
    setEditingProjectId(projectId);
    setProjectPathDraft(currentPath);
    await onSelectProject(projectId);
  };

  const toggleStorageEditor = async (storage: StorageConfigV3) => {
    if (editingStorage?.storageId === storage.id) {
      setEditingStorage(null);
      setStorageDraft(null);
      return;
    }

    setExpandedLink(null);
    setExpandedProvider(null);
    setEditingStorage({ projectId: "", storageId: storage.id });
    setStorageDraft({ ...storage });
    await loadStorageBundles(storage.id);
  };

  const settingsProject = editingProjectId
    ? projects.find((project) => project.local_project_id === editingProjectId) ?? null
    : null;

  if (editingStorage && storageDraft && editedStorageConfig) {
    return (
      <main className="v3-main v3-project-links-page v3-storage-settings-page">
        <section className="profile-links-section" aria-labelledby="storage-settings-heading">
          <div className="profile-links-heading">
            <div className="profile-links-copy">
              <h1 id="storage-settings-heading" className="settings-section-title">
                {creatingStorage ? "New storage" : "Storage settings"}
              </h1>
              <div className="profile-links-subtitle">
                {creatingStorage ? "Configure a storage destination." : editedStorageConfig.name || "Unnamed storage"}
              </div>
            </div>
            <button
              type="button"
              className="btn btn-ghost"
              onClick={() => {
                setEditingStorage(null);
                setStorageDraft(null);
              }}
              aria-label="Close storage settings"
            >
              <Icon name="x" size={14} />
            </button>
          </div>

          <div ref={storageSettingsRef} className="v3-storage-settings-below v3-storage-settings-dedicated">
            <StorageEditor storage={storageDraft} disabled={busy} onChange={setStorageDraft} />

            <div className="v3-inline-storage-save">
              <span>Credentials and storage details stay on this machine.</span>
              <button
                type="button"
                className="btn"
                disabled={busy || (!creatingStorage && JSON.stringify(storageDraft) === JSON.stringify(editedStorageConfig))}
                onClick={() => void run(`storage:${editedStorageConfig.id}`, async () => {
                  await onSaveStorage(storageDraft);
                  await loadStorageBundles(editedStorageConfig.id);
                })}
              >
                {runningAction === `storage:${editedStorageConfig.id}` ? "Saving…" : creatingStorage ? "Create storage" : "Save storage"}
              </button>
            </div>

            {!creatingStorage && (
              <section className="v3-storage-repositories" aria-labelledby="storage-repositories-heading">
                <div className="v3-storage-repositories-heading">
                  <div>
                    <h2 id="storage-repositories-heading">Repositories</h2>
                    <span>{orderedRemoteBundles.length} repo{orderedRemoteBundles.length === 1 ? "" : "s"} in this storage</span>
                  </div>
                  <button
                    type="button"
                    className="btn btn-ghost"
                    onClick={() => void loadStorageBundles(editedStorageConfig.id)}
                    disabled={busy || bundleLoading}
                    title="Refresh repositories"
                    aria-label={`Refresh repositories in ${editedStorageConfig.name || "storage"}`}
                  >
                    <Icon name="refresh" size={14} className={bundleLoading ? "icon-spin" : undefined} />
                  </button>
                </div>

                {bundleLoading ? (
                  <div className="v3-storage-repository-state"><span className="status-loader" /> Loading repositories…</div>
                ) : bundleError ? (
                  <div className="v3-callout error"><Icon name="alert-triangle" size={15} /> {bundleError}</div>
                ) : orderedRemoteBundles.length === 0 ? (
                  <div className="v3-storage-repository-state">No repositories in this storage.</div>
                ) : (
                  <div className="v3-storage-repository-list">
                    {orderedRemoteBundles.map((bundle) => (
                      <div key={bundle.bundle_id} className="v3-storage-repository-row">
                        <span className="v3-storage-repository-icon"><Icon name="folder" size={17} /></span>
                        <div className="v3-storage-repository-copy">
                          <strong>{bundle.display_name || "Unnamed repository"}</strong>
                          <code>{bundle.bundle_id}</code>
                        </div>
                        <div className="v3-storage-repository-meta">
                          <strong>Generation {bundle.generation ?? "—"}</strong>
                          <span>{bundle.resource_count ?? 0} resources · {formatRelativeTime(bundle.updated_at)}</span>
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </section>
            )}

            {error && <div className="v3-callout error"><Icon name="alert-triangle" size={15} /> {error}</div>}
          </div>
        </section>
      </main>
    );
  }

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

  return (
    <main className="v3-main v3-project-links-page">
      <section className="profile-links-section" aria-labelledby="project-links-heading">
        <div className="profile-links-heading">
          <div className="profile-links-copy">
            <h1 id="project-links-heading" className="settings-section-title">
              {settingsProject ? "Project settings" : "Project links"}
            </h1>
            <div className="profile-links-subtitle">
              {settingsProject ? settingsProject.display_name : "Choose where each project repo syncs."}
            </div>
          </div>
          {!settingsProject && <div className="profile-links-heading-actions">
            <div className="profile-links-counts">
              {projects.length} projects <span>·</span> {storages.length} storage <span>·</span> {links.length} links
            </div>
            <div className="profile-links-primary-actions">
              <button type="button" className="btn profile-refresh-linkage" onClick={onRefresh} disabled={loading || busy}>
                <Icon name="refresh" size={16} className={loading ? "icon-spin" : undefined} />
                {loading ? "Refreshing…" : "Refresh"}
              </button>
              <button type="button" className="btn profile-add-profile" onClick={onAddProject} disabled={busy}>
                <Icon name="plus" size={16} /> Add project
              </button>
            </div>
          </div>}
        </div>

        {newProjectSetup}

        {projects.length === 0 ? (
          <div className="profile-links-empty">
            <Icon name="folder" size={24} />
            <span>Add a project to choose which resources and storage belong to it.</span>
            <button type="button" className="btn" onClick={onAddProject} disabled={busy}>
              <Icon name="plus" size={15} /> Add project
            </button>
          </div>
        ) : (
          <div className="profile-links-list">
            {(settingsProject ? [settingsProject] : projects).map((project) => {
              const projectLinks = linkedByProject.get(project.local_project_id) ?? [];
              const availableStorages = storages.filter((storage) => (
                !projectLinks.some((link) => link.storage_id === storage.id)
              ));
              const firstLink = projectLinks[0];
              const active = activeProjectId === project.local_project_id;
              const resourceCount = project.selected_resource_count ?? project.resource_count ?? 0;
              const projectBinding = bindingByProject.get(project.local_project_id);
              const assignedProfiles = (["codex", "claude"] as const).flatMap((provider) => {
                const profileId = projectBinding?.profile_ids?.[provider];
                const profile = profiles.find((candidate) => candidate.profile_id === profileId);
                return profile ? [{ provider, profile }] : [];
              });
              const assignedProfileCount = Object.keys(projectBinding?.profile_ids ?? {}).length;
              const profilesReadable = assignedProfileCount > 0
                && assignedProfiles.length === assignedProfileCount
                && assignedProfiles.every(({ profile }) => profile.available && profile.readable);
              const profilesWritable = profilesReadable
                && assignedProfiles.every(({ profile }) => profile.writable);
              const canSync = !!projectBinding && profilesReadable;
              const canRestore = !!projectBinding && profilesWritable;
              const profileTitle = assignedProfiles.length === 0
                ? "Profile required"
                : !profilesReadable
                  ? "Profile unavailable"
                : assignedProfiles.length === 1
                  ? assignedProfiles[0].profile.display_name
                  : `${assignedProfiles.length} provider profiles`;
              const projectProfileSummary = assignedProfiles.length === 1 && profilesReadable
                ? `${assignedProfiles[0].provider === "codex" ? "Codex" : "Claude"} · ${assignedProfiles[0].profile.display_name}`
                : profileTitle;

              return (
                <article
                  id={`project-card-${project.local_project_id}`}
                  key={project.local_project_id}
                  className={`profile-link-card v3-project-link-card${active ? " active" : ""}`}
                >
                  <div className="profile-link-profile">
                    <span className="profile-link-profile-icon"><Icon name="folder" size={25} /></span>
                    <div className="profile-link-profile-copy">
                      <strong>{project.display_name}</strong>
                      <span>{resourceCount} selected resources</span>
                      <span className="profile-link-path" title={project.project_root ?? undefined}>
                        {compactProjectPath(project.project_root)}
                      </span>
                      <span className={`v3-project-profile-summary${canSync ? "" : " missing"}`}>
                        {projectProfileSummary}
                      </span>
                      <div className="profile-link-profile-actions" role="group" aria-label={`Actions for ${project.display_name}`}>
                        <button
                          type="button"
                          className="profile-utility-btn"
                          disabled={!firstLink || !canRestore || busy || !!runningAction || (active && selectionDirty)}
                          onClick={() => firstLink && void run(`pull:${project.local_project_id}:${firstLink.storage_id}`, () => onPull(project.local_project_id, firstLink.storage_id))}
                          title={!profilesWritable && projectBinding ? "Selected provider profile is unavailable or read only" : `Pull ${project.display_name} from its first linked storage`}
                        >
                          <Icon name="download" size={14} />
                        </button>
                        <button
                          type="button"
                          className="profile-utility-btn"
                          disabled={!firstLink || !canSync || busy || !!runningAction}
                          onClick={() => firstLink && void run(`push:${project.local_project_id}:${firstLink.storage_id}`, () => onPush(project.local_project_id, firstLink.storage_id))}
                          title={!profilesReadable && projectBinding ? "Selected provider profile is unavailable" : `Push ${project.display_name} to its first linked storage`}
                        >
                          <Icon name="upload" size={14} />
                        </button>
                        <button
                          type="button"
                          className={`profile-utility-btn${editingProjectId === project.local_project_id ? " active" : ""}`}
                          disabled={busy}
                          onClick={() => void toggleProjectEditor(
                            project.local_project_id,
                            projectBinding?.project_root ?? project.project_root ?? "",
                          )}
                          title={`Project settings for ${project.display_name}`}
                          aria-expanded={editingProjectId === project.local_project_id}
                        >
                          <Icon name="settings" size={13} />
                        </button>
                        <button
                          type="button"
                          className="profile-utility-btn profile-remove-btn"
                          disabled={busy}
                          onClick={() => void onRemoveProject(project.local_project_id)}
                          title={`Remove ${project.display_name} from Agent Sync; files stay on disk`}
                        >
                          <Icon name="trash" size={13} />
                        </button>
                      </div>
                    </div>
                  </div>

                  <div className="profile-link-connections">
                    <div className="profile-link-connections-label">Linked storage</div>
                    {projectLinks.length === 0 && <div className="profile-link-no-storage">No storage linked yet.</div>}

                    {projectLinks.map((link) => {
                      const storage = storages.find((candidate) => candidate.id === link.storage_id);
                      if (!storage) return null;
                      const detailsOpen = sameLink(expandedLink, project.local_project_id, storage.id);
                      const actionPrefix = `${project.local_project_id}:${storage.id}`;

                      return (
                        <div key={storage.id} className={`storage-link-block expanded${detailsOpen ? " v3-details-open" : ""}`}>
                          <div className="storage-link-row">
                            <div className="storage-link-storage-section">
                              <div className="storage-link-main">
                                <span className="storage-link-icon">
                                  <Icon name={storage.kind === "local" ? "drive" : "cloud"} size={23} />
                                </span>
                                <span className="storage-link-copy">
                                  <strong>{storage.name || "(unnamed)"}</strong>
                                  <span title={storageSubtitle(storage)}>{storageSubtitle(storage)}</span>
                                </span>
                              </div>
                              <button
                                type="button"
                                className={`storage-link-configure${editingStorage?.storageId === storage.id ? " active" : ""}`}
                                onClick={() => void toggleStorageEditor(storage)}
                                title={`Configure ${storage.name || "storage"}`}
                                aria-label={`Configure ${storage.name || "storage"}`}
                                aria-expanded={editingStorage?.storageId === storage.id}
                              >
                                <Icon name="settings" size={16} />
                              </button>
                            </div>

                            {(["codex", "claude"] as const).map((provider) => {
                              const providerLabel = provider === "codex" ? "Codex" : "Claude";
                              const profileId = projectBinding?.profile_ids?.[provider];
                              const profile = profiles.find((candidate) => candidate.profile_id === profileId);
                              const profileReady = !!profile?.available && profile.readable;
                              const providerOpen = detailsOpen && expandedProvider === provider;
                              return (
                                <div key={provider} className={`storage-link-profile-section storage-link-provider-${provider}`}>
                                  <span className="storage-link-profile-copy">
                                    <small>{providerLabel}</small>
                                    <strong className={profileReady ? undefined : "warning"}>
                                      {profile?.display_name ?? "Not used"}
                                    </strong>
                                    <span title={profile?.path}>
                                      {profile
                                        ? `${compactProjectPath(profile.path)}${profileReady ? "" : " · Unavailable"}`
                                        : "No profile assigned"}
                                    </span>
                                  </span>
                                  <button
                                    type="button"
                                    className={`storage-link-profile-settings${providerOpen ? " active" : ""}`}
                                    onClick={() => void toggleDetails(project.local_project_id, storage.id, provider)}
                                    title={providerOpen ? `Hide ${providerLabel} settings` : `Configure ${providerLabel} profile`}
                                    aria-label={`Configure ${providerLabel} profile`}
                                    aria-expanded={providerOpen}
                                  >
                                    <Icon name="settings" size={16} />
                                  </button>
                                </div>
                              );
                            })}

                            <div className="storage-link-actions">
                              <div className="storage-link-action-group" role="group" aria-label="Project storage actions">
                                <button
                                  type="button"
                                  className="storage-link-sync storage-link-sync-primary"
                                  disabled={busy || !!runningAction || (!!projectBinding && !canRestore)}
                                  onClick={() => void run(`pull:${actionPrefix}`, () => onPull(project.local_project_id, storage.id))}
                                  title={!profilesWritable && projectBinding ? "Selected provider profile is unavailable or read only" : "Review the Pull actions before applying them"}
                                >
                                  <Icon name="download" size={15} />
                                  {runningAction === `pull:${actionPrefix}` ? "Reviewing…" : project.project_root ? "Pull" : "Set up"}
                                </button>
                                <button
                                  type="button"
                                  className="storage-link-sync"
                                  disabled={busy || !!runningAction || !canSync || resourceCount === 0 || (active && selectionDirty)}
                                  onClick={() => void run(`push:${actionPrefix}`, () => onPush(project.local_project_id, storage.id))}
                                  title={!profilesReadable && projectBinding ? "Selected provider profile is unavailable" : "Push this project's selected resources"}
                                >
                                  <Icon name="upload" size={15} />
                                  {runningAction === `push:${actionPrefix}` ? "Pushing…" : "Push"}
                                </button>
                                <button
                                  type="button"
                                  className="storage-link-sync"
                                  disabled={busy || !!runningAction || !canRestore}
                                  onClick={() => void run(`repair:${actionPrefix}`, () => onRepair(project.local_project_id, storage.id))}
                                  title={!profilesWritable && projectBinding ? "Selected provider profile is unavailable or read only" : "Review missing dependencies and repair this project"}
                                >
                                  <Icon name="refresh" size={14} />
                                  {runningAction === `repair:${actionPrefix}` ? "Checking…" : "Repair"}
                                </button>
                              </div>
                            </div>
                          </div>

                          {detailsOpen && (
                            <div className="storage-link-detail v3-project-link-detail">
                              {([expandedProvider ?? "codex"] as const).map((provider) => {
                                const providerLabel = provider === "codex" ? "Codex" : "Claude";
                                const options = profiles.filter((profile) => profile.provider === provider);
                                const selectedId = projectBinding?.profile_ids?.[provider] ?? "";
                                return (
                                  <div key={provider} className="v3-simple-settings">
                                    <div className="v3-simple-settings-heading">
                                      <strong>{providerLabel} profile</strong>
                                      <span>Choose a saved profile or enter its path.</span>
                                    </div>
                                    <div className="v3-simple-settings-grid">
                                      <label>
                                        <span>Saved profile</span>
                                        <select
                                          value={selectedId}
                                          disabled={busy}
                                          onChange={(event) => void onAssignProfile(project.local_project_id, provider, event.target.value || null)}
                                        >
                                          <option value="">Not used</option>
                                          {options.map((profile) => (
                                            <option key={profile.profile_id} value={profile.profile_id} disabled={!profile.available || !profile.readable}>
                                              {profile.display_name}{!profile.available || !profile.readable ? " (unavailable)" : ""}
                                            </option>
                                          ))}
                                        </select>
                                      </label>
                                      <label>
                                        <span>Profile path</span>
                                        <div className="v3-simple-path-row">
                                          <input
                                            value={providerPathDraft}
                                            onChange={(event) => setProviderPathDraft(event.target.value)}
                                            placeholder={provider === "codex" ? "/Users/name/.codex" : "/Users/name/.claude"}
                                            disabled={busy}
                                          />
                                          <button
                                            type="button"
                                            className="btn"
                                            disabled={busy}
                                            onClick={() => void (async () => {
                                              const picked = await open({ directory: true, multiple: false });
                                              if (typeof picked === "string") setProviderPathDraft(picked);
                                            })()}
                                          >
                                            Browse
                                          </button>
                                          <button
                                            type="button"
                                            className="btn btn-primary"
                                            disabled={busy || !providerPathDraft.trim()}
                                            onClick={() => void onAddProfilePath(project.local_project_id, provider, providerPathDraft)}
                                          >
                                            Use path
                                          </button>
                                        </div>
                                      </label>
                                    </div>
                                  </div>
                                );
                              })}

                              {error && <div className="v3-callout error"><Icon name="alert-triangle" size={15} /> {error}</div>}

                              <details className="v3-resource-details">
                                <summary>
                                  <span><strong>Project resources</strong><small>{selected.size} selected · {counts.local} local · {counts.remote} remote · {counts.conflict} conflicts</small></span>
                                  <span className={`readiness ${readiness?.state ?? "unknown"}`}>{readiness?.state?.replace(/_/g, " ") ?? "Not checked"}</span>
                                  <Icon name="chevron-right" size={14} />
                                </summary>
                                {active && (
                                  <>
                                    {loading && resources.length === 0 ? (
                                      <div className="v3-pane-message"><span className="status-loader" /> Discovering project resources…</div>
                                    ) : (
                                      <ResourceInventory resources={resources} selected={selected} statuses={statuses} disabled={busy} onToggle={onToggleResource} />
                                    )}
                                    {selectionDirty && (
                                      <div className="v3-project-detail-save">
                                        <span>Resource selection changed</span>
                                        <button type="button" className="btn btn-primary" onClick={onSaveRecipe} disabled={busy}>
                                          {busy ? "Saving…" : "Save project recipe"}
                                        </button>
                                      </div>
                                    )}
                                  </>
                                )}
                              </details>
                            </div>
                          )}
                        </div>
                      );
                    })}

                    <button
                      type="button"
                      className="profile-link-another"
                      disabled={(storages.length > 0 && availableStorages.length === 0) || busy}
                      onClick={() => {
                        if (storages.length === 0) {
                          onOpenStorageSettings();
                          return;
                        }
                        setLinkingProjectId((current) => current === project.local_project_id ? null : project.local_project_id);
                      }}
                    >
                      <Icon name="plus" size={15} />
                      {storages.length === 0 ? "Add storage" : availableStorages.length === 0 ? "All storage linked" : "Link another storage"}
                    </button>
                    {linkingProjectId === project.local_project_id && (
                      <div className="storage-link-picker">
                        <span>Choose a storage destination</span>
                        {availableStorages.map((storage) => (
                          <button
                            key={storage.id}
                            type="button"
                            disabled={busy}
                            onClick={() => void run(`link:${project.local_project_id}:${storage.id}`, async () => {
                              await onLinkStorage(project.local_project_id, storage.id);
                              setLinkingProjectId(null);
                            })}
                          >
                            <Icon name={storage.kind === "local" ? "drive" : "cloud"} size={15} />
                            {storage.name || "(unnamed)"}
                            <Icon name="chevron-right" size={13} />
                          </button>
                        ))}
                      </div>
                    )}
                  </div>

                  {editingProjectId === project.local_project_id && (
                    <div ref={projectSettingsRef} className="v3-inline-project-settings">
                      <div className="v3-inline-project-copy">
                        <strong>Project — {project.display_name}</strong>
                        <span>Folder on this machine</span>
                      </div>
                      <label>
                        <span>Project path</span>
                        <div className="v3-simple-path-row">
                          <input
                            value={projectPathDraft}
                            onChange={(event) => setProjectPathDraft(event.target.value)}
                            placeholder="/path/to/project"
                            disabled={busy}
                            autoFocus
                          />
                          <button
                            type="button"
                            className="btn"
                            disabled={busy}
                            onClick={() => void (async () => {
                              const picked = await open({ directory: true, multiple: false });
                              if (typeof picked === "string") setProjectPathDraft(picked);
                            })()}
                          >
                            Browse
                          </button>
                          <button
                            type="button"
                            className="btn btn-primary"
                            disabled={
                              busy
                              || !projectPathDraft.trim()
                              || projectPathDraft.trim() === (projectBinding?.project_root ?? project.project_root ?? "")
                            }
                            onClick={() => void run(
                              `project:${project.local_project_id}`,
                              () => onSaveProjectPath(project.local_project_id, projectPathDraft.trim()),
                            )}
                          >
                            {runningAction === `project:${project.local_project_id}` ? "Saving…" : "Save"}
                          </button>
                          <button
                            type="button"
                            className="btn btn-ghost"
                            onClick={() => setEditingProjectId(null)}
                            aria-label="Close project settings"
                          >
                            <Icon name="x" size={14} />
                          </button>
                        </div>
                      </label>
                      {error && <div className="v3-callout error"><Icon name="alert-triangle" size={15} /> {error}</div>}
                    </div>
                  )}
                </article>
              );
            })}
          </div>
        )}
      </section>

    </main>
  );
}
