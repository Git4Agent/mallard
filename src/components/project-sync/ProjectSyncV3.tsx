import { useEffect, useMemo, useRef, useState } from "react";
import type {
  CSSProperties,
  KeyboardEvent as ReactKeyboardEvent,
  MouseEvent as ReactMouseEvent,
  PointerEvent as ReactPointerEvent,
} from "react";
import { confirm, open } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import type {
  AppTheme,
  BundleReadiness,
  BundleSnapshotSummary,
  DependencyPlan,
  DependencyResult,
  LocalProjectRegistration,
  LocalProjectSummary,
  LogLine,
  ProjectBinding,
  ProjectDetail,
  ProjectProvider,
  ProjectStorageLink,
  ProviderProfile,
  ProviderProfileSummary,
  ResourceInventory as ResourceInventoryModel,
  ResourceStatusReport,
  RestorePlan,
  RestoreResult,
  SetupDraftSummary,
  SyncConfigV3,
} from "../../types";
import Icon from "../Icons";
import LogPanel from "../LogPanel";
import BundleConnectionDialog from "./BundleConnectionDialog";
import ProjectBindingEditor, { type ProjectBindingDraft } from "./ProjectBindingEditor";
import ProjectLinksWorkspace from "./ProjectLinksWorkspace";
import ProjectSetupWorkspace, { type SetupCompletion } from "./ProjectSetupWorkspace";
import ProjectSidebar from "./ProjectSidebar";
import RestorePlanView from "./RestorePlanView";
import { projectSyncApi } from "./api";
import { beginPullReview } from "./pullReviewFlow";
import {
  errorMessage,
  inventoryResources,
  recipeSelection,
  recipeWithSelection,
  statusMap,
} from "./model";

interface Props {
  theme: AppTheme;
  onThemeChange: (theme: AppTheme) => void;
  onOpenLegacy: () => void;
}

const EMPTY_CONFIG: SyncConfigV3 = {
  schema: 3,
  revision: 0,
  storages: [],
  projects: [],
  links: [],
};

const PROJECT_SIDEBAR_WIDTH_KEY = "agent-sync.project-sidebar-width";
const DEFAULT_PROJECT_SIDEBAR_WIDTH = 318;
const MIN_PROJECT_SIDEBAR_WIDTH = 220;
const MAX_PROJECT_SIDEBAR_WIDTH = 560;

function clampSidebarWidth(width: number, maxWidth = MAX_PROJECT_SIDEBAR_WIDTH): number {
  return Math.min(maxWidth, Math.max(MIN_PROJECT_SIDEBAR_WIDTH, width));
}

function availableProjectSidebarWidth(): number {
  return Math.max(
    MIN_PROJECT_SIDEBAR_WIDTH,
    Math.min(MAX_PROJECT_SIDEBAR_WIDTH, window.innerWidth - 420),
  );
}

function storedSidebarWidth(): number {
  try {
    const value = Number.parseInt(window.localStorage.getItem(PROJECT_SIDEBAR_WIDTH_KEY) ?? "", 10);
    return Number.isFinite(value)
      ? clampSidebarWidth(value, window.innerWidth > 720 ? availableProjectSidebarWidth() : MAX_PROJECT_SIDEBAR_WIDTH)
      : DEFAULT_PROJECT_SIDEBAR_WIDTH;
  } catch {
    return DEFAULT_PROJECT_SIDEBAR_WIDTH;
  }
}

interface PendingBundleConnection {
  projectId: string;
  storageId: string;
  matches: BundleSnapshotSummary[];
  reason: "link" | "missing";
}

function upsertProject(
  projects: LocalProjectRegistration[],
  project: LocalProjectRegistration,
): LocalProjectRegistration[] {
  const found = projects.some((candidate) => candidate.local_project_id === project.local_project_id);
  return found
    ? projects.map((candidate) => candidate.local_project_id === project.local_project_id ? project : candidate)
    : [...projects, project];
}

function upsertBinding(bindings: ProjectBinding[], binding: ProjectBinding): ProjectBinding[] {
  const found = bindings.some((candidate) => candidate.local_project_id === binding.local_project_id);
  return found
    ? bindings.map((candidate) => candidate.local_project_id === binding.local_project_id ? binding : candidate)
    : [...bindings, binding];
}

function defaultProfileIds(): Partial<Record<ProjectProvider, string>> {
  return {};
}

export default function ProjectSyncV3({ theme, onThemeChange, onOpenLegacy }: Props) {
  const [config, setConfig] = useState<SyncConfigV3>(EMPTY_CONFIG);
  const [registrations, setRegistrations] = useState<LocalProjectRegistration[]>([]);
  const [bindings, setBindings] = useState<ProjectBinding[]>([]);
  const [profiles, setProfiles] = useState<ProviderProfileSummary[]>([]);
  const [activeProjectId, setActiveProjectId] = useState<string | null>(null);
  const [detail, setDetail] = useState<ProjectDetail | null>(null);
  const [inventory, setInventory] = useState<ResourceInventoryModel | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [savedSelection, setSavedSelection] = useState("");
  const [status, setStatus] = useState<ResourceStatusReport | null>(null);
  const [readiness, setReadiness] = useState<BundleReadiness | null>(null);
  const [activeStorageByProject, setActiveStorageByProject] = useState<Record<string, string>>({});
  const [storageEditorRequest, setStorageEditorRequest] = useState<
    | { mode: "toggle"; storageId: string; requestId: number }
    | { mode: "create"; storageKind: "local" | "s3"; requestId: number }
    | { mode: "close"; requestId: number }
    | null
  >(null);
  const [projectEditorRequest, setProjectEditorRequest] = useState<
    { mode: "toggle" | "close"; projectId: string; requestId: number } | null
  >(null);

  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [backendError, setBackendError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [pendingBundleConnection, setPendingBundleConnection] = useState<PendingBundleConnection | null>(null);
  const [logLines, setLogLines] = useState<LogLine[]>([]);
  const [activityOpen, setActivityOpen] = useState(false);
  const [logHeight, setLogHeight] = useState(240);
  const [resizingLog, setResizingLog] = useState(false);
  const [sidebarWidth, setSidebarWidth] = useState(storedSidebarWidth);
  const [resizingSidebar, setResizingSidebar] = useState(false);
  const [unreadLogs, setUnreadLogs] = useState(0);
  const activityOpenRef = useRef(false);
  const sidebarResizeRef = useRef<{ pointerId: number; startX: number; startWidth: number } | null>(null);

  const [setupDrafts, setSetupDrafts] = useState<SetupDraftSummary[]>([]);
  const [setupDraftId, setSetupDraftId] = useState<string | null>(null);
  const [editingBinding, setEditingBinding] = useState<ProjectBindingDraft | null>(null);

  const [restorePlan, setRestorePlan] = useState<RestorePlan | null>(null);
  const [restoreBinding, setRestoreBinding] = useState<ProjectBinding | null>(null);
  const [restoreProjectName, setRestoreProjectName] = useState("project");
  const [dependencyPlan, setDependencyPlan] = useState<DependencyPlan | null>(null);
  const [restoreResult, setRestoreResult] = useState<RestoreResult | null>(null);
  const [dependencyResult, setDependencyResult] = useState<DependencyResult | null>(null);
  const [restoreError, setRestoreError] = useState<string | null>(null);
  const restoreRequest = useRef(0);

  useEffect(() => {
    activityOpenRef.current = activityOpen;
  }, [activityOpen]);

  useEffect(() => {
    try {
      window.localStorage.setItem(PROJECT_SIDEBAR_WIDTH_KEY, String(sidebarWidth));
    } catch {
      // Persistence is a convenience; resizing still works if storage is unavailable.
    }
  }, [sidebarWidth]);

  useEffect(() => {
    const fitSidebarToWindow = () => {
      if (window.innerWidth <= 720) return;
      setSidebarWidth((current) => clampSidebarWidth(current, availableProjectSidebarWidth()));
    };
    window.addEventListener("resize", fitSidebarToWindow);
    return () => window.removeEventListener("resize", fitSidebarToWindow);
  }, []);

  useEffect(() => {
    const unlisten = listen<LogLine>("sync-log", (event) => {
      setLogLines((current) => [...current.slice(-1999), event.payload]);
      if (!activityOpenRef.current) setUnreadLogs((current) => Math.min(99, current + 1));
    });
    return () => {
      void unlisten.then((dispose) => dispose());
    };
  }, []);

  const openActivity = () => {
    activityOpenRef.current = true;
    setActivityOpen(true);
    setUnreadLogs(0);
  };

  const startLogResize = (event: ReactMouseEvent<HTMLDivElement>) => {
    if (event.button !== 0 || !activityOpen) return;
    event.preventDefault();

    const startY = event.clientY;
    const startHeight = logHeight;
    const maxHeight = Math.max(140, Math.min(520, window.innerHeight - 120));
    setResizingLog(true);

    const handleMouseMove = (moveEvent: MouseEvent) => {
      const nextHeight = startHeight + startY - moveEvent.clientY;
      setLogHeight(Math.min(maxHeight, Math.max(140, nextHeight)));
    };
    const handleMouseUp = () => {
      setResizingLog(false);
      document.removeEventListener("mousemove", handleMouseMove);
      document.removeEventListener("mouseup", handleMouseUp);
    };

    document.addEventListener("mousemove", handleMouseMove);
    document.addEventListener("mouseup", handleMouseUp);
  };

  const startSidebarResize = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    sidebarResizeRef.current = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startWidth: sidebarWidth,
    };
    setResizingSidebar(true);
  };

  const continueSidebarResize = (event: ReactPointerEvent<HTMLDivElement>) => {
    const resize = sidebarResizeRef.current;
    if (!resize || resize.pointerId !== event.pointerId) return;
    setSidebarWidth(clampSidebarWidth(
      resize.startWidth + event.clientX - resize.startX,
      availableProjectSidebarWidth(),
    ));
  };

  const finishSidebarResize = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (sidebarResizeRef.current?.pointerId !== event.pointerId) return;
    sidebarResizeRef.current = null;
    setResizingSidebar(false);
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
  };

  const resizeSidebarWithKeyboard = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    let nextWidth: number | null = null;
    if (event.key === "ArrowLeft") nextWidth = sidebarWidth - 16;
    if (event.key === "ArrowRight") nextWidth = sidebarWidth + 16;
    if (event.key === "Home") nextWidth = MIN_PROJECT_SIDEBAR_WIDTH;
    if (event.key === "End") nextWidth = availableProjectSidebarWidth();
    if (nextWidth === null) return;
    event.preventDefault();
    setSidebarWidth(clampSidebarWidth(nextWidth, availableProjectSidebarWidth()));
  };

  const binding = detail?.binding ?? null;
  const resources = inventoryResources(inventory);
  const projects = useMemo<LocalProjectSummary[]>(() => registrations.map((registration) => {
    const localBinding = bindings.find((candidate) => (
      candidate.local_project_id === registration.local_project_id && candidate.state === "active"
    ));
    const links = config.links.filter((link) => link.local_project_id === registration.local_project_id);
    const active = registration.local_project_id === activeProjectId;
    const activeResources = active ? resources : [];
    return {
      local_project_id: registration.local_project_id,
      bundle_id: registration.bundle_id,
      display_name: registration.display_name,
      repository_fingerprint: registration.repository_fingerprint,
      project_root: localBinding?.project_root ?? null,
      profile_ids: localBinding?.profile_ids ?? {},
      providers: active
        ? [...new Set(activeResources.flatMap((resource) => resource.provider ? [resource.provider] : []))]
        : undefined,
      resource_count: active ? activeResources.length : undefined,
      selected_resource_count: Object.keys(registration.recipe.entries).length,
      linked_storage_ids: links.map((link) => link.storage_id),
      readiness_state: active ? readiness?.state : undefined,
    };
  }), [activeProjectId, bindings, config.links, readiness?.state, registrations, resources]);

  const activeSummary = projects.find((candidate) => candidate.local_project_id === activeProjectId) ?? null;
  const pendingConnectionProject = pendingBundleConnection
    ? registrations.find((candidate) => candidate.local_project_id === pendingBundleConnection.projectId) ?? null
    : null;
  const pendingConnectionStorage = pendingBundleConnection
    ? config.storages.find((candidate) => candidate.id === pendingBundleConnection.storageId) ?? null
    : null;
  const projectLinks: ProjectStorageLink[] = detail?.project.local_project_id === activeProjectId
    ? detail.links
    : config.links.filter((link) => link.local_project_id === activeProjectId);
  const activeStorageId = activeProjectId
    ? activeStorageByProject[activeProjectId] ?? projectLinks[0]?.storage_id ?? null
    : null;
  const statuses = useMemo(() => statusMap(status), [status]);
  const selectionKey = [...selected].sort().join("\n");
  const selectionDirty = selectionKey !== savedSelection;

  const loadShell = async (): Promise<{
    nextProjects: LocalProjectRegistration[];
    nextConfig: SyncConfigV3;
    nextBindings: ProjectBinding[];
    nextProfiles: ProviderProfileSummary[];
  }> => {
    setLoading(true);
    const failures: string[] = [];
    let nextProjects: LocalProjectRegistration[] = [];
    let nextConfig: SyncConfigV3 = EMPTY_CONFIG;
    let nextBindings: ProjectBinding[] = [];
    let nextProfiles: ProviderProfileSummary[] = [];
    try {
      nextConfig = await projectSyncApi.getConfig();
      if (nextConfig.schema !== 3) {
        throw new Error(`Expected project sync schema 3, received schema ${nextConfig.schema}`);
      }
      setConfig(nextConfig);
    } catch (reason) {
      failures.push(`Configuration: ${errorMessage(reason)}`);
      setConfig(EMPTY_CONFIG);
    }
    try {
      // Listing projects also completes any interrupted setup finalization,
      // so drafts are refreshed afterwards to reflect consumed drafts.
      nextProjects = await projectSyncApi.listProjects();
      setRegistrations(nextProjects);
    } catch (reason) {
      failures.push(`Projects: ${errorMessage(reason)}`);
      setRegistrations([]);
    }
    await refreshSetupDrafts();
    try {
      nextProfiles = await projectSyncApi.listProviderProfiles();
      setProfiles(nextProfiles);
    } catch (reason) {
      failures.push(`Profiles: ${errorMessage(reason)}`);
      setProfiles([]);
    }
    try {
      nextBindings = await projectSyncApi.listBindings();
      setBindings(nextBindings);
    } catch (reason) {
      failures.push(`Bindings: ${errorMessage(reason)}`);
      setBindings([]);
    }
    setBackendError(failures.length > 0 ? failures.join(" · ") : null);
    setLoading(false);
    return { nextProjects, nextConfig, nextBindings, nextProfiles };
  };

  const loadProjectData = async (
    projectId: string,
    cfg = config,
    preferredStorage?: string | null,
  ) => {
    setLoading(true);
    setError(null);
    try {
      const nextDetail = await projectSyncApi.getProject(projectId);
      if (!nextDetail) throw new Error("The selected local project no longer exists.");
      setDetail(nextDetail);
      setRegistrations((current) => upsertProject(current, nextDetail.project));
      if (nextDetail.binding) {
        setBindings((current) => upsertBinding(current, nextDetail.binding as ProjectBinding));
      }

      let nextInventory: ResourceInventoryModel | null = null;
      if (nextDetail.binding) {
        try {
          nextInventory = await projectSyncApi.getInventory(projectId);
        } catch (reason) {
          setError(errorMessage(reason));
        }
      }
      setInventory(nextInventory);
      const included = recipeSelection(nextInventory?.recipe ?? nextDetail.project.recipe);
      setSelected(included);
      setSavedSelection([...included].sort().join("\n"));

      const links = nextDetail.links.length > 0
        ? nextDetail.links
        : cfg.links.filter((link) => link.local_project_id === projectId);
      const remembered = activeStorageByProject[projectId];
      const storageId = preferredStorage && links.some((link) => link.storage_id === preferredStorage)
        ? preferredStorage
        : remembered && links.some((link) => link.storage_id === remembered)
          ? remembered
          : links[0]?.storage_id ?? null;
      if (storageId && nextDetail.binding) {
        setActiveStorageByProject((current) => ({ ...current, [projectId]: storageId }));
        try {
          setStatus(await projectSyncApi.getStatus(projectId, storageId));
        } catch {
          setStatus(null);
        }
      } else {
        setStatus(null);
      }
      if (nextDetail.binding) {
        try {
          setReadiness(await projectSyncApi.getReadiness(nextDetail.project.bundle_id, nextDetail.binding));
        } catch {
          setReadiness(null);
        }
      } else {
        setReadiness(null);
      }
    } catch (reason) {
      setDetail(null);
      setInventory(null);
      setStatus(null);
      setReadiness(null);
      setError(errorMessage(reason));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void (async () => {
      const { nextProjects, nextConfig } = await loadShell();
      const first = nextProjects[0]?.local_project_id ?? null;
      setActiveProjectId(first);
      if (first) await loadProjectData(first, nextConfig);
    })();
  }, []); // ProjectSyncV3 is the schema-3 lifetime boundary.

  const refresh = async () => {
    const current = activeProjectId;
    const { nextProjects, nextConfig } = await loadShell();
    const next = current && nextProjects.some((candidate) => candidate.local_project_id === current)
      ? current
      : nextProjects[0]?.local_project_id ?? null;
    setActiveProjectId(next);
    if (next) await loadProjectData(next, nextConfig);
    else {
      setDetail(null);
      setInventory(null);
    }
  };

  const selectProject = async (projectId: string, preferredStorage?: string | null) => {
    setActiveProjectId(projectId);
    await loadProjectData(projectId, config, preferredStorage);
  };

  const createProfileAtPath = async (provider: ProjectProvider, path: string): Promise<ProviderProfile | null> => {
    if (!path.trim()) return null;
    setBusy(true);
    setError(null);
    try {
      const probe = await projectSyncApi.probeProviderProfile(provider, path.trim());
      const currentProfiles = await projectSyncApi.listProviderProfiles();
      const existing = probe.existing_profile_id
        ? currentProfiles.find((profile) => profile.profile_id === probe.existing_profile_id) ?? null
        : null;
      const profile = existing ?? await projectSyncApi.createProviderProfile(
        provider,
        probe.suggested_name,
        path.trim(),
      );
      setProfiles(existing ? currentProfiles : await projectSyncApi.listProviderProfiles());
      return profile;
    } catch (reason) {
      setError(errorMessage(reason));
      return null;
    } finally {
      setBusy(false);
    }
  };

  const chooseAndCreateProfile = async (provider: ProjectProvider): Promise<ProviderProfile | null> => {
    const picked = await open({ directory: true, multiple: false });
    if (typeof picked !== "string" || !picked) return null;
    return createProfileAtPath(provider, picked);
  };

  const refreshSetupDrafts = async () => {
    try {
      const listed = await projectSyncApi.listSetupDrafts();
      setSetupDrafts(listed.drafts);
    } catch {
      // Drafts are a convenience surface; the projects list stays usable.
    }
  };

  const beginAddProject = async () => {
    const picked = await open({ directory: true, multiple: false });
    if (typeof picked !== "string" || !picked) return;
    setBusy(true);
    setError(null);
    try {
      const created = await projectSyncApi.createSetupDraft(picked);
      setSetupDraftId(created.draft.draft_id);
      if (created.resumed) {
        setNotice(`Resumed the saved setup draft for ${created.draft.display_name}.`);
      }
      await refreshSetupDrafts();
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const openSetupDraft = (draftId: string) => {
    setError(null);
    setSetupDraftId(draftId);
  };

  const closeSetupDraft = async () => {
    setSetupDraftId(null);
    await refreshSetupDrafts();
  };

  const discardSetupDraft = async (draftId: string) => {
    const summary = setupDrafts.find((candidate) => candidate.draft_id === draftId);
    const approved = await confirm(
      `Discard the setup draft for “${summary?.display_name ?? "this project"}”?\n\nOnly the draft is removed. No project files are touched.`,
      { title: "Discard draft" },
    );
    if (!approved) return;
    setBusy(true);
    setError(null);
    try {
      await projectSyncApi.discardSetupDraft(draftId);
      if (setupDraftId === draftId) setSetupDraftId(null);
      await refreshSetupDrafts();
      setNotice("Setup draft discarded. Project files were not touched.");
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const completeSetup = async (detail: ProjectDetail, completion: SetupCompletion) => {
    setSetupDraftId(null);
    const projectId = detail.project.local_project_id;
    const storageId = detail.links[0]?.storage_id ?? null;
    setNotice(completion === "pull"
      ? `${detail.project.display_name} connected. Review Pull before files are applied.`
      : `${detail.project.display_name} is set up${storageId ? "" : " — link storage when ready"}.`);
    const { nextConfig } = await loadShell();
    await refreshSetupDrafts();
    setActiveProjectId(projectId);
    if (detail.binding) setBindings((current) => upsertBinding(current, detail.binding as ProjectBinding));
    await loadProjectData(projectId, nextConfig, storageId);
    if (completion === "pull" && storageId && detail.binding) {
      await planRestore(storageId, detail.project.bundle_id, detail.binding, detail.project.display_name);
    } else if (completion === "push" && storageId) {
      await pushProject(projectId, storageId);
    }
  };

  const saveRecipe = async () => {
    if (!activeProjectId || !inventory) return;
    setBusy(true);
    setError(null);
    const nextRecipe = recipeWithSelection(inventory.recipe, resources, selected);
    try {
      const savedProject = await projectSyncApi.saveRecipe(activeProjectId, nextRecipe);
      setRegistrations((current) => upsertProject(current, savedProject));
      setDetail((current) => current ? { ...current, project: savedProject } : current);
      setInventory({ ...inventory, recipe: savedProject.recipe });
      const saved = recipeSelection(savedProject.recipe);
      setSelected(saved);
      setSavedSelection([...saved].sort().join("\n"));
      setNotice("Project recipe saved. Pushes now use this exact resource selection.");
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const saveStorageLink = async (projectId: string, storageId: string) => {
    await projectSyncApi.saveLink({
      local_project_id: projectId,
      storage_id: storageId,
      pinned: true,
    });
    const { nextConfig } = await loadShell();
    setActiveStorageByProject((current) => ({ ...current, [projectId]: storageId }));
    setActiveProjectId(projectId);
    await loadProjectData(projectId, nextConfig, storageId);
    setNotice("Storage linked to this project repo.");
  };

  const linkStorage = async (projectId: string, storageId: string) => {
    setBusy(true);
    setError(null);
    try {
      const project = registrations.find((candidate) => candidate.local_project_id === projectId);
      if (!project) throw new Error("The selected local project no longer exists.");
      const bundles = await projectSyncApi.listRemoteBundleSnapshots(storageId);
      bundles.sort((left, right) => {
        const leftMatches = !!project.repository_fingerprint
          && left.repository_fingerprint === project.repository_fingerprint;
        const rightMatches = !!project.repository_fingerprint
          && right.repository_fingerprint === project.repository_fingerprint;
        if (leftMatches !== rightMatches) return leftMatches ? -1 : 1;
        return (right.updated_at ?? 0) - (left.updated_at ?? 0);
      });
      if (bundles.some((bundle) => bundle.bundle_id === project.bundle_id) || bundles.length === 0) {
        await saveStorageLink(projectId, storageId);
        return;
      }
      setPendingBundleConnection({ projectId, storageId, matches: bundles, reason: "link" });
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const pushProject = async (projectId: string, storageId: string) => {
    openActivity();
    setBusy(true);
    setError(null);
    try {
      const result = await projectSyncApi.pushBundle(projectId, storageId);
      setNotice(result.message);
      setActiveProjectId(projectId);
      setActiveStorageByProject((current) => ({ ...current, [projectId]: storageId }));
      await loadProjectData(projectId, config, storageId);
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const saveRemap = async (nextBinding: ProjectBindingDraft) => {
    if (!activeProjectId) return;
    setBusy(true);
    setError(null);
    try {
      const saved = await projectSyncApi.saveBinding({
        local_project_id: activeProjectId,
        project_root: nextBinding.project_root,
        profile_ids: nextBinding.profile_ids,
        expected_revision: nextBinding.expected_revision ?? binding?.revision ?? null,
      });
      setEditingBinding(null);
      setBindings((current) => upsertBinding(current, saved));
      setDetail((current) => current ? { ...current, binding: saved } : current);
      setNotice("Machine binding updated. Cloud identity and logical paths were unchanged.");
      await loadProjectData(activeProjectId, config, activeStorageId);
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const assignProjectProfile = async (
    projectId: string,
    provider: ProjectProvider,
    profileId: string | null,
  ) => {
    setBusy(true);
    setError(null);
    try {
      const nextDetail = await projectSyncApi.getProject(projectId);
      if (!nextDetail) throw new Error("The selected local project no longer exists.");
      if (!nextDetail.binding) {
        setActiveProjectId(projectId);
        setEditingBinding({
          local_project_id: projectId,
          bundle_id: nextDetail.project.bundle_id,
          project_root: "",
          profile_ids: profileId ? { [provider]: profileId } : defaultProfileIds(),
          expected_revision: null,
        });
        setNotice("Choose the project folder to finish this machine's setup.");
        return;
      }

      const profileIds = { ...nextDetail.binding.profile_ids };
      if (profileId) profileIds[provider] = profileId;
      else delete profileIds[provider];
      if (Object.keys(profileIds).length === 0) {
        throw new Error("A project must use at least one provider profile.");
      }

      const saved = await projectSyncApi.saveBinding({
        local_project_id: projectId,
        project_root: nextDetail.binding.project_root,
        profile_ids: profileIds,
        expected_revision: nextDetail.binding.revision,
      });
      setBindings((current) => upsertBinding(current, saved));
      setProfiles(await projectSyncApi.listProviderProfiles());
      if (activeProjectId === projectId) {
        setDetail((current) => current ? { ...current, binding: saved } : current);
        await loadProjectData(projectId, config, activeStorageByProject[projectId]);
      }
      const assigned = profiles.find((profile) => profile.profile_id === profileId);
      setNotice(profileId
        ? `${provider === "codex" ? "Codex" : "Claude"} now uses ${assigned?.display_name ?? "the selected profile"}.`
        : `${provider === "codex" ? "Codex" : "Claude"} removed from this project.`);
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const addProfilePathToProject = async (projectId: string, provider: ProjectProvider, path: string) => {
    const profile = await createProfileAtPath(provider, path);
    if (profile) await assignProjectProfile(projectId, provider, profile.profile_id);
  };

  const saveProjectPath = async (projectId: string, projectRoot: string) => {
    setBusy(true);
    setError(null);
    try {
      const nextDetail = await projectSyncApi.getProject(projectId);
      if (!nextDetail?.binding) throw new Error("Choose a provider profile before setting the project path.");
      const saved = await projectSyncApi.saveBinding({
        local_project_id: projectId,
        project_root: projectRoot.trim(),
        profile_ids: nextDetail.binding.profile_ids,
        expected_revision: nextDetail.binding.revision,
      });
      setBindings((current) => upsertBinding(current, saved));
      if (activeProjectId === projectId) {
        setDetail((current) => current ? { ...current, binding: saved } : current);
        await loadProjectData(projectId, config, activeStorageByProject[projectId]);
      }
      setNotice("Project path updated for this machine.");
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const planRestore = async (
    storageId: string,
    bundleId: string,
    targetBinding: ProjectBinding,
    displayName: string,
  ) => {
    const requestId = ++restoreRequest.current;
    openActivity();
    setBusy(true);
    setRestoreError(null);
    setRestoreResult(null);
    setDependencyResult(null);
    setRestorePlan(null);
    setDependencyPlan(null);
    setReadiness(null);
    setRestoreBinding(targetBinding);
    setRestoreProjectName(displayName);
    try {
      // The file review is the primary Pull surface. Render it as soon as it
      // is ready; optional dependency/readiness checks must never hide a valid
      // restore plan or its Apply button.
      const request = { storageId, bundleId, binding: targetBinding };
      const review = await beginPullReview(projectSyncApi, request);
      if (restoreRequest.current !== requestId) return null;
      setRestorePlan(review.restorePlan);
      setNotice(`Pull review ready for ${displayName}. Nothing has been applied yet.`);

      // Supporting context fills in asynchronously after the modal is usable.
      // Its lifecycle is request-scoped so closing or replanning cannot apply
      // stale results to another review.
      void review.support
        .then((support) => {
          if (restoreRequest.current !== requestId) return;
          setDependencyPlan(support.dependencyPlan);
          setReadiness(support.readiness);
          if (support.errors.length > 0) {
            setRestoreError(`The file actions are ready to apply. ${support.errors.join(" ")}`);
          }
        })
        .catch((reason) => {
          if (restoreRequest.current === requestId) {
            setRestoreError(`The file actions are ready to apply. Supporting checks: ${errorMessage(reason)}`);
          }
        });
      return null;
    } catch (reason) {
      const message = errorMessage(reason);
      if (restoreRequest.current === requestId) {
        setRestoreError(message);
        setError(message);
      }
      return message;
    } finally {
      if (restoreRequest.current === requestId) setBusy(false);
    }
  };

  const beginProjectRestore = async (projectId: string, storageId: string) => {
    setError(null);
    try {
      const nextDetail = await projectSyncApi.getProject(projectId);
      if (!nextDetail) throw new Error("The selected local project no longer exists.");
      setActiveProjectId(projectId);
      setActiveStorageByProject((current) => ({ ...current, [projectId]: storageId }));
      setDetail(nextDetail);
      if (!nextDetail.binding) {
        setEditingBinding({
          local_project_id: nextDetail.project.local_project_id,
          bundle_id: nextDetail.project.bundle_id,
          project_root: "",
          profile_ids: defaultProfileIds(),
          expected_revision: null,
        });
        setNotice("Choose this machine's project folder and provider profile before pulling.");
        return;
      }
      const failure = await planRestore(
        storageId,
        nextDetail.project.bundle_id,
        nextDetail.binding,
        nextDetail.project.display_name,
      );
      if (failure?.includes("does not exist")) {
        const bundles = await projectSyncApi.listRemoteBundleSnapshots(storageId);
        if (bundles.length > 0) {
          bundles.sort((left, right) => {
            const leftMatches = !!nextDetail.project.repository_fingerprint
              && left.repository_fingerprint === nextDetail.project.repository_fingerprint;
            const rightMatches = !!nextDetail.project.repository_fingerprint
              && right.repository_fingerprint === nextDetail.project.repository_fingerprint;
            if (leftMatches !== rightMatches) return leftMatches ? -1 : 1;
            return (right.updated_at ?? 0) - (left.updated_at ?? 0);
          });
          setPendingBundleConnection({ projectId, storageId, matches: bundles, reason: "missing" });
        }
      }
    } catch (reason) {
      setError(errorMessage(reason));
    }
  };

  const connectPendingBundle = async (match: BundleSnapshotSummary) => {
    if (!pendingBundleConnection) return;
    const pending = pendingBundleConnection;
    const project = registrations.find((candidate) => candidate.local_project_id === pending.projectId);
    const allowRepositoryMismatch = !!project?.repository_fingerprint
      && project.repository_fingerprint !== match.repository_fingerprint;
    await connectStorageBundle(
      pending.projectId,
      pending.storageId,
      match.bundle_id,
      allowRepositoryMismatch,
    );
  };

  const connectStorageBundle = async (
    projectId: string,
    storageId: string,
    bundleId: string,
    allowRepositoryMismatch = false,
  ) => {
    const project = registrations.find((candidate) => (
      candidate.local_project_id === projectId
    ));
    if (!project) {
      setError("The selected local project no longer exists.");
      return;
    }
    if (project.bundle_id === bundleId) return;
    setBusy(true);
    setError(null);
    try {
      const connected = await projectSyncApi.connectProjectToRemoteBundle({
        local_project_id: projectId,
        storage_id: storageId,
        bundle_id: bundleId,
        expected_bundle_id: project.bundle_id,
        pinned: true,
        allow_repository_mismatch: allowRepositoryMismatch,
      });
      setPendingBundleConnection(null);
      const { nextConfig } = await loadShell();
      setActiveProjectId(projectId);
      setActiveStorageByProject((current) => ({
        ...current,
        [projectId]: storageId,
      }));
      await loadProjectData(projectId, nextConfig, storageId);
      if (!connected.binding) {
        setNotice("Remote repo connected. Configure the project folder and provider profile before Pull.");
        return;
      }
      await planRestore(
        storageId,
        bundleId,
        connected.binding,
        connected.project.display_name,
      );
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const keepPendingBundle = async () => {
    if (!pendingBundleConnection) return;
    const pending = pendingBundleConnection;
    setBusy(true);
    setError(null);
    try {
      if (pending.reason === "link") {
        await saveStorageLink(pending.projectId, pending.storageId);
      } else {
        setNotice("Kept the local-only repo identity. Push will publish it as a separate remote project.");
      }
      setPendingBundleConnection(null);
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const removeProject = async (projectId: string) => {
    const summary = projects.find((candidate) => candidate.local_project_id === projectId);
    const approved = await confirm(
      `Remove “${summary?.display_name ?? "this project"}” from Agent Sync?\n\nThe checkout and provider files stay on disk. Only this app's project registration, links, and active binding are removed.`,
      { title: "Remove project" },
    );
    if (!approved) return;
    setBusy(true);
    setError(null);
    try {
      await projectSyncApi.removeProject(projectId);
      setNotice(`${summary?.display_name ?? "Project"} removed from Agent Sync. Files were not deleted.`);
      await refresh();
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const applyRestore = async (actionIds: string[]) => {
    if (!restorePlan || !restoreBinding) return;
    openActivity();
    setBusy(true);
    setRestoreError(null);
    try {
      const result = await projectSyncApi.applyRestore(restorePlan.plan_id, actionIds);
      setRestoreResult(result);
      setReadiness(await projectSyncApi.getReadiness(restorePlan.bundle_id, restoreBinding));
      if (activeProjectId) await loadProjectData(activeProjectId, config, activeStorageId);
    } catch (reason) {
      setRestoreError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const applyDependencies = async (actionIds: string[]) => {
    if (!dependencyPlan || !restorePlan || !restoreBinding) return;
    setBusy(true);
    setRestoreError(null);
    try {
      setDependencyResult(await projectSyncApi.applyDependencies(dependencyPlan.plan_id, actionIds));
      setReadiness(await projectSyncApi.getReadiness(restorePlan.bundle_id, restoreBinding));
      setDependencyPlan(await projectSyncApi.planDependencies(restorePlan.bundle_id, restoreBinding));
    } catch (reason) {
      setRestoreError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const refreshRestore = async () => {
    if (!restorePlan || !restoreBinding) return;
    await planRestore(restorePlan.storage_id, restorePlan.bundle_id, restoreBinding, restoreProjectName);
  };

  const saveStorageConfig = async (
    update: (current: SyncConfigV3) => SyncConfigV3,
    successNotice = "Project storage settings saved.",
  ): Promise<boolean> => {
    setBusy(true);
    setError(null);
    try {
      // Focused project operations can advance the global config revision.
      // Rebase this storage-only edit on the latest document instead of the
      // potentially stale React snapshot used to render the editor.
      const current = await projectSyncApi.getConfig();
      const saved = await projectSyncApi.saveConfig(update(current));
      setConfig(saved);
      setRegistrations(saved.projects);
      setDetail((current) => {
        if (!current) return current;
        const savedProject = saved.projects.find((candidate) => (
          candidate.local_project_id === current.project.local_project_id
        ));
        return {
          ...current,
          project: savedProject ?? current.project,
          links: saved.links.filter((link) => link.local_project_id === current.project.local_project_id),
        };
      });
      setActiveStorageByProject((current) => Object.fromEntries(Object.entries(current).filter(
        ([localProjectId, storageId]) => saved.links.some((link) => (
          link.local_project_id === localProjectId && link.storage_id === storageId
        )),
      )));
      setNotice(successNotice);
      return true;
    } catch (reason) {
      setError(errorMessage(reason));
      return false;
    } finally {
      setBusy(false);
    }
  };

  const saveInlineStorage = async (storage: SyncConfigV3["storages"][number]) => {
    await saveStorageConfig((current) => {
      const exists = current.storages.some((candidate) => candidate.id === storage.id);
      return {
        ...current,
        storages: exists
          ? current.storages.map((candidate) => candidate.id === storage.id ? storage : candidate)
          : [...current.storages, storage],
      };
    });
  };

  const removeStorage = async (storageId: string) => {
    const storage = config.storages.find((candidate) => candidate.id === storageId);
    if (!storage) return;
    const storageName = storage.name || "Storage";
    const linkedProjects = config.links.filter((link) => link.storage_id === storageId).length;
    if (linkedProjects > 0) {
      setError(`${storageName} is linked to ${linkedProjects} project${linkedProjects === 1 ? "" : "s"}. Unlink it before removing the storage.`);
      return;
    }
    const approved = await confirm(
      `Remove “${storageName}” from Agent Sync?\n\nFiles already written to the storage location will not be deleted.`,
      { title: "Remove storage" },
    );
    if (!approved) return;
    const removed = await saveStorageConfig((current) => {
      if (current.links.some((link) => link.storage_id === storageId)) {
        throw new Error(`${storageName} is linked to a project. Unlink it before removing the storage.`);
      }
      return {
        ...current,
        storages: current.storages.filter((candidate) => candidate.id !== storageId),
      };
    }, `${storageName} removed. Stored files were not deleted.`);
    if (removed) {
      setStorageEditorRequest((current) => ({
        mode: "close",
        requestId: (current?.requestId ?? 0) + 1,
      }));
    }
  };

  return (
    <div
      className={`v3-app${resizingSidebar ? " resizing-sidebar" : ""}`}
      style={{ "--v3-sidebar-width": `${sidebarWidth}px` } as CSSProperties}
    >
      <ProjectSidebar
        projects={projects}
        drafts={setupDrafts}
        activeDraftId={setupDraftId}
        onSelectDraft={(draftId) => openSetupDraft(draftId)}
        onDiscardDraft={(draftId) => void discardSetupDraft(draftId)}
        storages={config.storages}
        storageUsage={Object.fromEntries(config.storages.map((storage) => [
          storage.id,
          config.links.filter((link) => link.storage_id === storage.id).length,
        ]))}
        activeProjectId={activeProjectId}
        loading={loading}
        busy={busy}
        activityOpen={activityOpen}
        unreadLogs={unreadLogs}
        onSelectProject={(id) => {
          setProjectEditorRequest((current) => ({
            mode: "close",
            projectId: id,
            requestId: (current?.requestId ?? 0) + 1,
          }));
          void selectProject(id);
        }}
        onConfigureProject={(projectId) => {
          setProjectEditorRequest((current) => ({
            mode: "toggle",
            projectId,
            requestId: (current?.requestId ?? 0) + 1,
          }));
        }}
        onRemoveProject={(id) => void removeProject(id)}
        onToggleActivity={() => {
          const next = !activityOpen;
          activityOpenRef.current = next;
          if (next) setUnreadLogs(0);
          setActivityOpen(next);
        }}
        onAddProject={() => void beginAddProject()}
        onRefresh={() => void refresh()}
        onOpenStorage={(storageId) => {
          setStorageEditorRequest((current) => ({
            mode: "toggle",
            storageId,
            requestId: (current?.requestId ?? 0) + 1,
          }));
        }}
        onRemoveStorage={(storageId) => void removeStorage(storageId)}
        onAddStorage={() => {
          setStorageEditorRequest((current) => ({
            mode: "create",
            storageKind: "local",
            requestId: (current?.requestId ?? 0) + 1,
          }));
        }}
        onOpenLegacy={onOpenLegacy}
      />

      <div
        className="v3-sidebar-resizer"
        role="separator"
        aria-label="Resize sidebar"
        aria-orientation="vertical"
        aria-valuemin={MIN_PROJECT_SIDEBAR_WIDTH}
        aria-valuemax={availableProjectSidebarWidth()}
        aria-valuenow={sidebarWidth}
        tabIndex={0}
        onPointerDown={startSidebarResize}
        onPointerMove={continueSidebarResize}
        onPointerUp={finishSidebarResize}
        onPointerCancel={finishSidebarResize}
        onKeyDown={resizeSidebarWithKeyboard}
        onDoubleClick={() => setSidebarWidth(DEFAULT_PROJECT_SIDEBAR_WIDTH)}
      />

      <div className={`v3-workspace${resizingLog ? " resizing-log" : ""}`}>
        <div className="v3-titlebar" data-tauri-drag-region>
          <button type="button" className="v3-theme-button" onClick={() => onThemeChange(theme === "dark" ? "light" : "dark")}>
            {theme === "dark" ? "Light" : "Dark"} theme
          </button>
        </div>

        {backendError && (
          <div className="v3-backend-banner">
            <Icon name="alert-triangle" size={15} />
            <span><strong>Project-sync backend unavailable.</strong> {backendError}</span>
            <button type="button" className="btn" onClick={() => void refresh()}>Retry</button>
            <button type="button" className="btn btn-ghost" onClick={onOpenLegacy}>Open legacy</button>
          </div>
        )}
        {notice && (
          <button type="button" className="v3-notice" onClick={() => setNotice(null)} title="Dismiss">
            <Icon name="check-circle" size={14} /> {notice} <Icon name="x" size={12} />
          </button>
        )}

        <ProjectLinksWorkspace
            projects={projects}
            bindings={bindings}
            profiles={profiles}
            activeProjectId={activeProjectId}
            resources={resources}
            selected={selected}
            statuses={statuses}
            storages={config.storages}
            links={config.links}
            readiness={readiness}
            loading={loading}
            busy={busy}
            selectionDirty={selectionDirty}
            error={error}
            onSelectProject={(projectId, storageId) => selectProject(projectId, storageId)}
            onToggleResource={(resourceId) => setSelected((current) => {
              const next = new Set(current);
              if (next.has(resourceId)) next.delete(resourceId);
              else next.add(resourceId);
              return next;
            })}
            onSaveRecipe={() => void saveRecipe()}
            onLinkStorage={(projectId, storageId) => linkStorage(projectId, storageId)}
            onPush={(projectId, storageId) => pushProject(projectId, storageId)}
            onPull={(projectId, storageId) => beginProjectRestore(projectId, storageId)}
            onRepair={(projectId, storageId) => beginProjectRestore(projectId, storageId)}
            onSaveProjectPath={(projectId, path) => saveProjectPath(projectId, path)}
            onAssignProfile={(projectId, provider, profileId) => assignProjectProfile(projectId, provider, profileId)}
            onAddProfilePath={(projectId, provider, path) => addProfilePathToProject(projectId, provider, path)}
            onRemoveProject={(projectId) => removeProject(projectId)}
            onRefresh={() => void refresh()}
            onAddProject={() => void beginAddProject()}
            onOpenStorageSettings={() => {
              setStorageEditorRequest((current) => ({
                mode: "create",
                storageKind: "local",
                requestId: (current?.requestId ?? 0) + 1,
              }));
            }}
            onSaveStorage={saveInlineStorage}
            storageEditorRequest={storageEditorRequest}
            onStorageEditorRequestHandled={() => setStorageEditorRequest(null)}
            projectEditorRequest={projectEditorRequest}
            onProjectEditorRequestHandled={() => setProjectEditorRequest(null)}
            newProjectSetup={setupDraftId ? (
              <ProjectSetupWorkspace
                key={setupDraftId}
                draftId={setupDraftId}
                profiles={profiles}
                storages={config.storages}
                busy={busy}
                onClose={() => void closeSetupDraft()}
                onDiscard={(draftId) => void discardSetupDraft(draftId)}
                onFinalized={(detail, completion) => void completeSetup(detail, completion)}
              />
            ) : null}
        />

        <div
          className={`log-drawer v3-log-drawer${activityOpen ? " open" : ""}`}
          style={{ height: activityOpen ? logHeight : undefined }}
        >
          {activityOpen && (
            <div
              className="log-resizer"
              role="separator"
              aria-label="Resize sync log"
              aria-orientation="horizontal"
              onMouseDown={startLogResize}
              onDoubleClick={() => setLogHeight(240)}
            />
          )}
          <LogPanel
            lines={logLines}
            onClear={() => {
              setLogLines([]);
              setUnreadLogs(0);
            }}
            onClose={() => {
              activityOpenRef.current = false;
              setActivityOpen(false);
            }}
          />
        </div>
      </div>

      {pendingBundleConnection && pendingConnectionProject && pendingConnectionStorage && (
        <BundleConnectionDialog
          projectName={pendingConnectionProject.display_name}
          currentBundleId={pendingConnectionProject.bundle_id}
          projectFingerprint={pendingConnectionProject.repository_fingerprint}
          storage={pendingConnectionStorage}
          matches={pendingBundleConnection.matches}
          reason={pendingBundleConnection.reason}
          busy={busy}
          error={error}
          onCancel={() => setPendingBundleConnection(null)}
          onUseExisting={(match) => void connectPendingBundle(match)}
          onKeepCurrent={() => void keepPendingBundle()}
        />
      )}

      {editingBinding && (
        <ProjectBindingEditor
          title={`Project setup for ${activeSummary?.display_name ?? "project"}`}
          description="Choose this machine's checkout and provider profiles. Existing files and provider state are never moved or deleted."
          binding={editingBinding}
          profiles={profiles}
          busy={busy}
          error={error}
          actionLabel="Save project setup"
          onCancel={() => setEditingBinding(null)}
          onAddProfile={chooseAndCreateProfile}
          onSubmit={(next) => void saveRemap(next)}
        />
      )}

      {restorePlan && restoreBinding && (
        <RestorePlanView
          projectName={restoreProjectName}
          plan={restorePlan}
          binding={restoreBinding}
          dependencyPlan={dependencyPlan}
          readiness={readiness}
          restoreResult={restoreResult}
          dependencyResult={dependencyResult}
          busy={busy}
          error={restoreError}
          onApplyRestore={(ids) => void applyRestore(ids)}
          onApplyDependencies={(ids) => void applyDependencies(ids)}
          onRefresh={() => void refreshRestore()}
          onClose={() => {
            restoreRequest.current += 1;
            setRestorePlan(null);
            setRestoreBinding(null);
            setDependencyPlan(null);
            setRestoreResult(null);
            setDependencyResult(null);
            setRestoreError(null);
          }}
        />
      )}
    </div>
  );
}
