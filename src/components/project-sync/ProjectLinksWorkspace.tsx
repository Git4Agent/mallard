import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  BundlePage,
  CodexConversationPathAudit,
  LocalProjectSummary,
  ProjectBinding,
  ProjectProvider,
  ProjectStorageLink,
  ProviderProfileSummary,
  StorageConfigV3,
} from "../../types";
import Icon from "../Icons";
import ProjectChatHistoryPage from "./ProjectChatHistoryPage";
import ConversationPathRepairNotice from "./ConversationPathRepairNotice";
import { newStorage, storageConfigReady, StorageEditor } from "./StorageSettingsV3";
import { projectSyncApi } from "./api";
import {
  compactProjectPath,
  configuredProjectProvider,
  errorMessage,
  formatRelativeTime,
  PROJECT_PROVIDERS,
  projectLabel,
  providerLabel,
} from "./model";

type LinkKey = { projectId: string; storageId: string };
type StorageEditorRequest =
  | { mode: "toggle"; storageId: string; requestId: number }
  | { mode: "create"; storageKind: "local" | "s3"; requestId: number }
  | { mode: "close"; requestId: number };
type ProjectEditorRequest = { mode: "toggle" | "close"; projectId: string; requestId: number };
type InlineStorageReview = {
  kind: "pull" | "push";
  projectId: string;
  storageId: string;
  content: ReactNode;
  onClose: () => void;
};

interface Props {
  projects: LocalProjectSummary[];
  activeProjectId: string | null;
  bindings: ProjectBinding[];
  profiles: ProviderProfileSummary[];
  storages: StorageConfigV3[];
  links: ProjectStorageLink[];
  loading: boolean;
  busy: boolean;
  error: string | null;
  conversationPathAudits: Record<string, CodexConversationPathAudit>;
  conversationPathAuditErrors: Record<string, string>;
  conversationPathAuditLoading: boolean;
  onSelectProject: (projectId: string, storageId?: string | null) => Promise<void> | void;
  onLinkStorage: (projectId: string, storageId: string) => Promise<void> | void;
  onUnlinkStorage: (projectId: string, storageId: string) => Promise<void> | void;
  onPush: (projectId: string, storageId: string) => Promise<void> | void;
  onPull: (projectId: string, storageId: string) => Promise<void> | void;
  onRepairConversationPaths: (projectId: string) => Promise<void> | void;
  onRenameProject: (projectId: string, alias: string | null) => Promise<boolean> | boolean;
  onAssignProfile: (projectId: string, provider: ProjectProvider, profileId: string | null) => Promise<void> | void;
  onAddProfilePath: (projectId: string, provider: ProjectProvider, path: string) => Promise<void> | void;
  onRefresh: () => void;
  onAddProject: () => void;
  onOpenStorageSettings: (storageId?: string) => void;
  onSaveStorage: (storage: StorageConfigV3) => Promise<void> | void;
  storageEditorRequest?: StorageEditorRequest | null;
  onStorageEditorRequestHandled?: () => void;
  onStorageEditorChange?: (storageId: string | null) => void;
  projectEditorRequest?: ProjectEditorRequest | null;
  onProjectEditorRequestHandled?: () => void;
  newProjectSetup?: ReactNode;
  historyRefreshEpoch?: number;
  inlineStorageReview?: InlineStorageReview | null;
}

function storageSubtitle(storage: StorageConfigV3): string {
  if (storage.kind === "local") return compactProjectPath(storage.local_dir || "Folder not configured");
  return storage.bucket || storage.s3_endpoint || "S3 storage not configured";
}

export function conversationPathsBlockSync(
  hasBinding: boolean,
  audit: CodexConversationPathAudit | undefined,
  auditError: string | undefined,
  auditLoading: boolean,
): boolean {
  return hasBinding && (auditLoading || !!auditError || !audit || !audit.ready);
}

export default function ProjectLinksWorkspace({
  projects,
  activeProjectId,
  bindings,
  profiles,
  storages,
  links,
  loading,
  busy,
  error,
  conversationPathAudits,
  conversationPathAuditErrors,
  conversationPathAuditLoading,
  onSelectProject,
  onLinkStorage,
  onUnlinkStorage,
  onPush,
  onPull,
  onRepairConversationPaths,
  onRenameProject,
  onAssignProfile,
  onAddProfilePath,
  onRefresh,
  onAddProject,
  onOpenStorageSettings,
  onSaveStorage,
  storageEditorRequest,
  onStorageEditorRequestHandled,
  onStorageEditorChange,
  projectEditorRequest,
  onProjectEditorRequestHandled,
  newProjectSetup,
  historyRefreshEpoch = 0,
  inlineStorageReview,
}: Props) {
  const [expandedProfileProjectId, setExpandedProfileProjectId] = useState<string | null>(null);
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
  const [renamingProjectId, setRenamingProjectId] = useState<string | null>(null);
  const [projectAliasDraft, setProjectAliasDraft] = useState("");
  const bundleRequestRef = useRef(0);
  const storageSettingsRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!editingStorage) return;
    const frame = window.requestAnimationFrame(() => {
      storageSettingsRef.current?.scrollIntoView({ block: "center", behavior: "smooth" });
    });
    return () => window.cancelAnimationFrame(frame);
  }, [editingStorage]);

  useEffect(() => {
    onStorageEditorChange?.(editingStorage?.storageId ?? null);
  }, [editingStorage?.storageId, onStorageEditorChange]);

  const linkedByProject = useMemo(() => new Map(projects.map((project) => [
    project.local_project_id,
    links.filter((link) => link.local_project_id === project.local_project_id),
  ])), [links, projects]);
  const bindingByProject = useMemo(() => new Map(bindings
    .filter((candidate) => candidate.state === "active")
    .map((candidate) => [candidate.local_project_id, candidate])), [bindings]);

  useEffect(() => {
    if (!expandedProfileProjectId || !expandedProvider) return;
    const projectBinding = bindingByProject.get(expandedProfileProjectId);
    const profileId = projectBinding?.profile_ids?.[expandedProvider];
    const profile = profiles.find((candidate) => candidate.profile_id === profileId);
    setProviderPathDraft(profile?.path ?? "");
  }, [expandedProfileProjectId, expandedProvider, bindingByProject, profiles]);
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

  const toggleProfileDetails = async (projectId: string, provider: ProjectProvider) => {
    if (expandedProfileProjectId === projectId) {
      setExpandedProfileProjectId(null);
      setExpandedProvider(null);
      return;
    }

    bundleRequestRef.current += 1;
    setEditingStorage(null);
    setStorageDraft(null);
    setBundleLoading(false);
    setExpandedProfileProjectId(projectId);
    setExpandedProvider(provider);
    await onSelectProject(projectId, null);
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
      setExpandedProfileProjectId(null);
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

    setExpandedProfileProjectId(null);
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
      setRenamingProjectId(null);
      onProjectEditorRequestHandled?.();
      return;
    }
    const project = projects.find((candidate) => candidate.local_project_id === projectEditorRequest.projectId);
    if (!project) return;
    if (editingProjectId === project.local_project_id) {
      setEditingProjectId(null);
      setRenamingProjectId(null);
      onProjectEditorRequestHandled?.();
      return;
    }
    bundleRequestRef.current += 1;
    setExpandedProfileProjectId(null);
    setExpandedProvider(null);
    setEditingStorage(null);
    setStorageDraft(null);
    setRenamingProjectId(null);
    setEditingProjectId(project.local_project_id);
    setProjectAliasDraft(project.local_alias ?? project.display_name);
    void onSelectProject(project.local_project_id);
    onProjectEditorRequestHandled?.();
  }, [projectEditorRequest?.requestId]);

  const toggleStorageEditor = async (storage: StorageConfigV3) => {
    if (editingStorage?.storageId === storage.id) {
      setEditingStorage(null);
      setStorageDraft(null);
      return;
    }

    setExpandedProfileProjectId(null);
    setExpandedProvider(null);
    setEditingStorage({ projectId: "", storageId: storage.id });
    setStorageDraft({ ...storage });
    await loadStorageBundles(storage.id);
  };

  const openProjectSettings = async (projectId: string) => {
    const project = projects.find((candidate) => candidate.local_project_id === projectId);
    if (!project) return;
    bundleRequestRef.current += 1;
    setExpandedProfileProjectId(null);
    setExpandedProvider(null);
    setEditingStorage(null);
    setStorageDraft(null);
    setRenamingProjectId(null);
    setEditingProjectId(projectId);
    setProjectAliasDraft(project.local_alias ?? project.display_name);
    await onSelectProject(projectId);
  };

  const focusedProjectId = editingProjectId ?? inlineStorageReview?.projectId ?? null;
  const settingsProject = focusedProjectId
    ? projects.find((project) => project.local_project_id === focusedProjectId) ?? null
    : null;
  const proposedProjectAlias = settingsProject
    ? projectAliasDraft.trim() && projectAliasDraft.trim() !== settingsProject.display_name
      ? projectAliasDraft.trim()
      : null
    : null;
  const projectNameChanged = !!settingsProject
    && proposedProjectAlias !== (settingsProject.local_alias ?? null);

  const closeProjectSettings = () => {
    inlineStorageReview?.onClose();
    setRenamingProjectId(null);
    setEditingProjectId(null);
  };

  const saveProjectName = async () => {
    if (!settingsProject || !projectNameChanged) return;
    let saved = false;
    await run(
      `rename:${settingsProject.local_project_id}`,
      async () => {
        saved = await onRenameProject(settingsProject.local_project_id, proposedProjectAlias);
      },
    );
    if (saved) setRenamingProjectId(null);
  };

  if (editingStorage && storageDraft && editedStorageConfig) {
    const storageReady = storageConfigReady(storageDraft);
    return (
      <main className="v3-main v3-project-links-page v3-storage-settings-page">
        <section className="profile-links-section" aria-labelledby="storage-settings-heading">
          <div className="profile-links-heading">
            <div className="profile-links-copy">
              <h1 id="storage-settings-heading" className="settings-section-title">
                {creatingStorage ? "New storage" : "Storage settings"}
              </h1>
              <div className="profile-links-subtitle">
                {creatingStorage
                  ? "Connect Cloudflare R2 or choose a local folder."
                  : editedStorageConfig.name || "Unnamed storage"}
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
              {!storageReady && (
                <span>
                  {storageDraft.kind === "local"
                    ? "Choose a folder to continue."
                    : "Enter the bucket, Account ID, and both R2 credentials to continue."}
                </span>
              )}
              <button
                type="button"
                className="btn"
                disabled={busy || !storageReady || (!creatingStorage && JSON.stringify(storageDraft) === JSON.stringify(editedStorageConfig))}
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
    const activeProject = projects.find((project) => project.local_project_id === activeProjectId) ?? null;
    const activeBinding = activeProject ? bindingByProject.get(activeProject.local_project_id) ?? null : null;
    if (activeProject) {
      return (
        <ProjectChatHistoryPage
          project={activeProject}
          binding={activeBinding}
          refreshEpoch={historyRefreshEpoch}
          onOpenProjectSettings={() => void openProjectSettings(activeProject.local_project_id)}
        />
      );
    }
    return (
      <main className="v3-main v3-project-links-page v3-git-info-page">
        <section className="profile-links-section" aria-labelledby="git-info-heading">
          <div className="profile-links-heading">
            <div className="profile-links-copy">
              <h1 id="git-info-heading" className="settings-section-title">Project history</h1>
              <div className="profile-links-subtitle">Select a completed project to view its Codex history.</div>
            </div>
          </div>
        </section>
      </main>
    );
  }

  return (
    <main className={`v3-main v3-project-links-page${settingsProject ? " v3-project-settings-page" : ""}${newProjectSetup ? " v3-project-setup-page" : ""}`}>
      <section
        className="profile-links-section"
        aria-label={newProjectSetup
          ? "Project setup"
          : settingsProject
            ? `${projectLabel(settingsProject)} settings`
            : undefined}
        aria-labelledby={newProjectSetup || settingsProject ? undefined : "project-links-heading"}
      >
        {!newProjectSetup && (
          <div className="profile-links-heading">
            <div className="profile-links-copy">
              {settingsProject ? (
                <div className="v3-project-heading-identity">
                  {renamingProjectId === settingsProject.local_project_id ? (
                    <form
                      className="v3-project-heading-rename"
                      onSubmit={(event) => {
                        event.preventDefault();
                        void saveProjectName();
                      }}
                    >
                      <input
                        value={projectAliasDraft}
                        onChange={(event) => setProjectAliasDraft(event.target.value)}
                        aria-label="Project display name"
                        disabled={busy}
                        autoFocus
                      />
                      <button type="submit" className="btn btn-primary" disabled={busy || !projectNameChanged}>
                        {runningAction === `rename:${settingsProject.local_project_id}` ? "Saving…" : "Save"}
                      </button>
                      <button
                        type="button"
                        className="btn btn-ghost"
                        disabled={busy}
                        onClick={() => {
                          setProjectAliasDraft(settingsProject.local_alias ?? settingsProject.display_name);
                          setRenamingProjectId(null);
                        }}
                      >
                        Cancel
                      </button>
                      {settingsProject.local_alias && (
                        <button
                          type="button"
                          className="btn btn-ghost"
                          disabled={busy}
                          onClick={() => void run(
                            `rename:${settingsProject.local_project_id}`,
                            async () => {
                              const saved = await onRenameProject(settingsProject.local_project_id, null);
                              if (!saved) return;
                              setProjectAliasDraft(settingsProject.display_name);
                              setRenamingProjectId(null);
                            },
                          )}
                        >
                          Use repo name
                        </button>
                      )}
                    </form>
                  ) : (
                    <div className="v3-project-heading-name">
                      <h1>{projectLabel(settingsProject)}</h1>
                      <button
                        type="button"
                        className="btn btn-ghost"
                        disabled={busy}
                        onClick={() => {
                          setProjectAliasDraft(settingsProject.local_alias ?? settingsProject.display_name);
                          setRenamingProjectId(settingsProject.local_project_id);
                        }}
                      >
                        Rename
                      </button>
                    </div>
                  )}
                  <div className="v3-project-heading-meta">
                    {settingsProject.local_alias && <span>Repo · {settingsProject.display_name}</span>}
                    <span title={settingsProject.project_root ?? undefined}>{compactProjectPath(settingsProject.project_root)}</span>
                  </div>
                  {renamingProjectId === settingsProject.local_project_id && error && (
                    <div className="v3-project-heading-error"><Icon name="alert-triangle" size={13} /> {error}</div>
                  )}
                </div>
              ) : (
                <>
                  <h1 id="project-links-heading" className="settings-section-title">Project links</h1>
                  <div className="profile-links-subtitle">Choose where each project repo syncs.</div>
                </>
              )}
            </div>
            {settingsProject ? (
              <button
                type="button"
                className="btn btn-ghost v3-project-settings-close"
                onClick={closeProjectSettings}
                disabled={busy}
                aria-label="Close project settings"
              >
                <Icon name="x" size={15} />
              </button>
            ) : (
              <div className="profile-links-heading-actions">
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
              </div>
            )}
          </div>
        )}

        {newProjectSetup}

        {!newProjectSetup && (projects.length === 0 ? (
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
              const projectBinding = bindingByProject.get(project.local_project_id);
              const configuredProviders = PROJECT_PROVIDERS.filter((provider) => (
                projectBinding?.profile_ids?.[provider]
              ));
              const hasMultipleProviders = configuredProviders.length > 1;
              const projectProvider = configuredProjectProvider(projectBinding?.profile_ids);
              const displayedProvider = projectProvider ?? "codex";
              const projectProfileId = projectProvider
                ? projectBinding?.profile_ids?.[projectProvider]
                : null;
              const projectProfile = profiles.find((candidate) => candidate.profile_id === projectProfileId);
              const profilesReadable = !hasMultipleProviders
                && !!projectProfile?.available
                && projectProfile.readable;
              const profilesWritable = profilesReadable && !!projectProfile?.writable;
              const canSync = !!projectBinding && profilesReadable;
              const canRestore = !!projectBinding && profilesWritable;
              const codexConfigured = projectProvider === "codex" && !hasMultipleProviders;
              const conversationPathAudit = conversationPathAudits[project.local_project_id];
              const conversationPathAuditError = conversationPathAuditErrors[project.local_project_id];
              const conversationPathBlocked = conversationPathsBlockSync(
                codexConfigured,
                conversationPathAudit,
                conversationPathAuditError,
                conversationPathAuditLoading,
              );
              const conversationPathTitle = codexConfigured
                ? conversationPathAuditLoading
                  ? "Checking Codex conversation paths"
                  : conversationPathAuditError
                    ? `Codex conversation paths could not be verified: ${conversationPathAuditError}`
                    : conversationPathAudit && !conversationPathAudit.ready
                      ? "Repair Codex conversation paths before Push or Pull"
                      : null
                : null;
              const profileIssue = hasMultipleProviders
                ? "Choose one project agent before syncing"
                : !profilesReadable && projectBinding
                  ? "The selected agent profile is unavailable"
                  : null;
              const profileDetailsOpen = expandedProfileProjectId === project.local_project_id;

              return (
                <article
                  key={project.local_project_id}
                  className="profile-link-card v3-project-link-card"
                >
                  <div className="profile-link-connections">
                    <section
                      className={`project-profile-group storage-link-provider-${displayedProvider}`}
                      aria-label={`${providerLabel(displayedProvider)} configuration`}
                    >
                      <header className="project-profile-group-header">
                        <span className="project-profile-group-icon">
                          <Icon name="terminal" size={17} />
                        </span>
                        <span className="project-profile-group-copy">
                          <strong className={projectProfile && profilesReadable ? undefined : "warning"}>
                            {hasMultipleProviders
                              ? "Choose one agent"
                              : projectProfile?.display_name ?? "No agent configured"}
                          </strong>
                          <span title={projectProfile?.path}>
                            {hasMultipleProviders
                              ? "Codex and Claude are both assigned"
                              : projectProfile
                                ? `${providerLabel(displayedProvider)} · ${compactProjectPath(projectProfile.path)}${profilesReadable ? "" : " · Unavailable"}`
                                : "Choose a Codex or Claude profile"}
                          </span>
                        </span>
                        <button
                          type="button"
                          className={`project-profile-group-settings${profileDetailsOpen ? " active" : ""}`}
                          onClick={() => void toggleProfileDetails(project.local_project_id, displayedProvider)}
                          title={profileDetailsOpen ? "Hide configuration settings" : "Configure this project profile"}
                          aria-label="Configure project profile"
                          aria-expanded={profileDetailsOpen}
                        >
                          <Icon name="settings" size={16} />
                        </button>
                      </header>

                    {codexConfigured && !conversationPathAuditLoading && (
                      <ConversationPathRepairNotice
                        audit={conversationPathAudit}
                        auditError={conversationPathAuditError}
                        projectName={projectLabel(project)}
                        profileName={projectProfile?.display_name ?? "Codex"}
                        profilePath={projectProfile?.path ?? conversationPathAudit?.profile_path}
                        showScope={false}
                        busy={busy || runningAction === `repair-paths:${project.local_project_id}`}
                        onRepair={() => run(
                          `repair-paths:${project.local_project_id}`,
                          () => onRepairConversationPaths(project.local_project_id),
                        )}
                      />
                    )}

                    {profileDetailsOpen && (
                      <div className="storage-link-detail v3-project-link-detail project-profile-group-detail">
                        {([expandedProvider ?? displayedProvider] as const).map((provider) => {
                          const label = providerLabel(provider);
                          const options = profiles.filter((profile) => profile.provider === provider);
                          const selectedId = projectBinding?.profile_ids?.[provider] ?? "";
                          return (
                            <div key={provider} className="v3-simple-settings">
                              <div className="v3-simple-settings-heading v3-agent-settings-heading">
                                <div>
                                  <strong>{label} profile</strong>
                                  <span>Each project uses one agent profile on this machine.</span>
                                </div>
                                <div className="v3-agent-choice compact" role="radiogroup" aria-label="Agent used by this project">
                                  {PROJECT_PROVIDERS.map((candidate) => (
                                    <button
                                      key={candidate}
                                      type="button"
                                      role="radio"
                                      aria-checked={provider === candidate}
                                      className={provider === candidate ? "active" : undefined}
                                      disabled={busy}
                                      onClick={() => {
                                        setExpandedProvider(candidate);
                                        const existingProfileId = projectBinding?.profile_ids?.[candidate];
                                        if (existingProfileId && (hasMultipleProviders || projectProvider !== candidate)) {
                                          void onAssignProfile(project.local_project_id, candidate, existingProfileId);
                                        }
                                      }}
                                    >
                                      <strong>{providerLabel(candidate)}</strong>
                                    </button>
                                  ))}
                                </div>
                              </div>
                              <div className="v3-simple-settings-grid">
                                <label>
                                  <span>Saved profile</span>
                                  <select
                                    value={selectedId}
                                    disabled={busy}
                                    onChange={(event) => {
                                      if (event.target.value) {
                                        void onAssignProfile(project.local_project_id, provider, event.target.value);
                                      }
                                    }}
                                  >
                                    <option value="">Choose a {label} profile</option>
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
                      </div>
                    )}

                    <div className="project-profile-storage-heading">
                      <span>Linked storage</span>
                      <small>{projectLinks.length}</small>
                    </div>
                    {projectLinks.length === 0 && <div className="profile-link-no-storage">No storage linked yet.</div>}

                    <div className="project-profile-storage-list">
                    {projectLinks.map((link) => {
                      const storage = storages.find((candidate) => candidate.id === link.storage_id);
                      if (!storage) return null;
                      const reviewOpen = inlineStorageReview?.projectId === project.local_project_id
                        && inlineStorageReview.storageId === storage.id;
                      const actionPrefix = `${project.local_project_id}:${storage.id}`;

                      return (
                        <div key={storage.id} className={`storage-link-block${reviewOpen ? " v3-review-open" : ""}`}>
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
                              <div className="storage-link-row-controls">
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
                                <button
                                  type="button"
                                  className="storage-link-unlink"
                                  disabled={busy || !!runningAction}
                                  onClick={() => void run(
                                    `unlink:${actionPrefix}`,
                                    () => onUnlinkStorage(project.local_project_id, storage.id),
                                  )}
                                  title={`Unlink ${storage.name || "storage"} from this project`}
                                  aria-label={`Unlink ${storage.name || "storage"} from ${projectLabel(project)}`}
                                >
                                  <Icon name="x" size={13} />
                                  {runningAction === `unlink:${actionPrefix}` ? "Unlinking…" : "Unlink"}
                                </button>
                              </div>
                            </div>

                            <div className="storage-link-actions">
                              <div className="storage-link-action-group" role="group" aria-label="Project storage actions">
                                <button
                                  type="button"
                                  className={`storage-link-sync storage-link-sync-primary${reviewOpen && inlineStorageReview?.kind === "pull" ? " active" : ""}`}
                                  disabled={busy || !!runningAction || conversationPathBlocked || (!!projectBinding && !canRestore)}
                                  onClick={() => {
                                    if (reviewOpen && inlineStorageReview?.kind === "pull") {
                                      inlineStorageReview.onClose();
                                      return;
                                    }
                                    void run(`pull:${actionPrefix}`, () => onPull(project.local_project_id, storage.id));
                                  }}
                                  title={conversationPathTitle
                                    ?? profileIssue
                                    ?? (!profilesWritable && projectBinding ? "The selected agent profile is read only" : "Review the Pull actions before applying them")}
                                  aria-expanded={reviewOpen && inlineStorageReview?.kind === "pull"}
                                >
                                  <Icon name="download" size={15} />
                                  {runningAction === `pull:${actionPrefix}` ? "Reviewing…" : project.project_root ? "Pull" : "Set up"}
                                </button>
                                <button
                                  type="button"
                                  className={`storage-link-sync${reviewOpen && inlineStorageReview?.kind === "push" ? " active" : ""}`}
                                  disabled={busy || !!runningAction || conversationPathBlocked || !canSync}
                                  onClick={() => {
                                    if (reviewOpen && inlineStorageReview?.kind === "push") {
                                      inlineStorageReview.onClose();
                                      return;
                                    }
                                    void run(`push:${actionPrefix}`, () => onPush(project.local_project_id, storage.id));
                                  }}
                                  title={conversationPathTitle
                                    ?? profileIssue
                                    ?? "Choose resources, then push them to this storage"}
                                  aria-expanded={reviewOpen && inlineStorageReview?.kind === "push"}
                                >
                                  <Icon name="upload" size={15} />
                                  {runningAction === `push:${actionPrefix}` ? "Pushing…" : "Push"}
                                </button>
                              </div>
                            </div>
                          </div>

                          {reviewOpen && (
                            <div className="v3-storage-inline-review">
                              {inlineStorageReview?.content}
                            </div>
                          )}
                        </div>
                      );
                    })}
                    </div>

                    <div className="project-profile-group-footer">
                    {(storages.length === 0 || availableStorages.length > 0) && (
                      <button
                        type="button"
                        className="profile-link-another"
                        disabled={busy}
                        onClick={() => {
                          if (storages.length === 0) {
                            onOpenStorageSettings();
                            return;
                          }
                          setLinkingProjectId((current) => current === project.local_project_id ? null : project.local_project_id);
                        }}
                      >
                        <Icon name="plus" size={15} />
                        {storages.length === 0 ? "Add storage" : "Link storage"}
                      </button>
                    )}
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
                    </section>
                  </div>

                </article>
              );
            })}
          </div>
        ))}
      </section>

    </main>
  );
}
