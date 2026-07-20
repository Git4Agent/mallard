import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type {
  CSSProperties,
  KeyboardEvent as ReactKeyboardEvent,
  MouseEvent as ReactMouseEvent,
  PointerEvent as ReactPointerEvent,
} from "react";
import { confirm, open } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import type {
  ActivityLogLevel,
  ActivityLogType,
  AppTheme,
  BundleRecipe,
  BundleReadiness,
  BundleSnapshotSummary,
  CodexConversationPathAudit,
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
  RestorePlan,
  RestoreResult,
  SetupDraftSummary,
  StorageConfigV3,
  SyncConfigV3,
} from "../../types";
import Icon from "../Icons";
import LogPanel, { ACTIVITY_LOG_TYPES } from "../LogPanel";
import BundleConnectionDialog from "./BundleConnectionDialog";
import LogManagerDialog from "./LogManagerDialog";
import ProjectBindingEditor, { type ProjectBindingDraft } from "./ProjectBindingEditor";
import ProjectLinksWorkspace from "./ProjectLinksWorkspace";
import PushResourceWorkspace from "./PushResourceWorkspace";
import ProjectSetupWorkspace, { type SetupCompletion } from "./ProjectSetupWorkspace";
import ProjectSidebar from "./ProjectSidebar";
import RestorePlanView from "./RestorePlanView";
import { projectSyncApi } from "./api";
import {
  applyPullReview,
  beginPullReview,
  loadPullReviewSupport,
  type PullApplyPhase,
  type PullReviewSelection,
} from "./pullReviewFlow";
import {
  errorMessage,
  inventoryResources,
  projectLabel,
  recipeSelection,
  recipeWithSelection,
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

interface PendingPush {
  projectId: string;
  storageId: string;
  projectName: string;
  storage: StorageConfigV3;
  inventory: ResourceInventoryModel;
  savedRecipe: BundleRecipe | null;
  projectDefaults: Set<string>;
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

function logMatchesFilters(
  line: LogLine,
  types: readonly ActivityLogType[],
  level: ActivityLogLevel | "all",
  search = "",
): boolean {
  const needle = search.trim().toLocaleLowerCase();
  return types.includes(line.type ?? "system")
    && (level === "all" || line.level === level)
    && (!needle
      || line.message.toLocaleLowerCase().includes(needle)
      || !!line.event?.toLocaleLowerCase().includes(needle));
}

function mergeLogLines(...groups: LogLine[][]): LogLine[] {
  const entries = new Map<string, LogLine>();
  for (const line of groups.flat()) {
    const key = line.id ?? `${line.ts}:${line.level}:${line.type ?? "system"}:${line.message}`;
    entries.set(key, line);
  }
  return [...entries.values()].sort((left, right) => (
    left.ts - right.ts || (left.id ?? "").localeCompare(right.id ?? "")
  ));
}

export default function ProjectSyncV3({ theme, onThemeChange, onOpenLegacy }: Props) {
  const [config, setConfig] = useState<SyncConfigV3>(EMPTY_CONFIG);
  const [registrations, setRegistrations] = useState<LocalProjectRegistration[]>([]);
  const [repositoryKinds, setRepositoryKinds] = useState<Record<string, boolean>>({});
  const [bindings, setBindings] = useState<ProjectBinding[]>([]);
  const [profiles, setProfiles] = useState<ProviderProfileSummary[]>([]);
  const [conversationPathAudits, setConversationPathAudits] = useState<Record<string, CodexConversationPathAudit>>({});
  const [conversationPathAuditErrors, setConversationPathAuditErrors] = useState<Record<string, string>>({});
  const [conversationPathAuditLoading, setConversationPathAuditLoading] = useState(true);
  const [activeProjectId, setActiveProjectId] = useState<string | null>(null);
  const [detail, setDetail] = useState<ProjectDetail | null>(null);
  const [inventory, setInventory] = useState<ResourceInventoryModel | null>(null);
  const [readiness, setReadiness] = useState<BundleReadiness | null>(null);
  const [activeStorageByProject, setActiveStorageByProject] = useState<Record<string, string>>({});
  const [activeStorageSettingsId, setActiveStorageSettingsId] = useState<string | null>(null);
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
  const [pendingPush, setPendingPush] = useState<PendingPush | null>(null);
  const [pushSelected, setPushSelected] = useState<Set<string>>(new Set());
  const [pushError, setPushError] = useState<string | null>(null);
  const [logLines, setLogLines] = useState<LogLine[]>([]);
  const [logTypeFilters, setLogTypeFilters] = useState<ActivityLogType[]>(() => [...ACTIVITY_LOG_TYPES]);
  const [logLevelFilter, setLogLevelFilter] = useState<ActivityLogLevel | "all">("all");
  const [logSearchInput, setLogSearchInput] = useState("");
  const [logSearch, setLogSearch] = useState("");
  const [logCursor, setLogCursor] = useState<string | null>(null);
  const [logLoading, setLogLoading] = useState(true);
  const [logLoadingOlder, setLogLoadingOlder] = useState(false);
  const [logLoadError, setLogLoadError] = useState<string | null>(null);
  const [logManagerOpen, setLogManagerOpen] = useState(false);
  const [activityOpen, setActivityOpen] = useState(false);
  const [logHeight, setLogHeight] = useState(240);
  const [resizingLog, setResizingLog] = useState(false);
  const [sidebarWidth, setSidebarWidth] = useState(storedSidebarWidth);
  const [resizingSidebar, setResizingSidebar] = useState(false);
  const [unreadLogs, setUnreadLogs] = useState(0);
  const activityOpenRef = useRef(false);
  const logFiltersRef = useRef({ types: logTypeFilters, level: logLevelFilter, search: logSearch });
  const logRequestRef = useRef(0);
  const sidebarResizeRef = useRef<{ pointerId: number; startX: number; startWidth: number } | null>(null);

  const [setupDrafts, setSetupDrafts] = useState<SetupDraftSummary[]>([]);
  const [setupDraftId, setSetupDraftId] = useState<string | null>(null);
  const [editingBinding, setEditingBinding] = useState<ProjectBindingDraft | null>(null);

  const [restorePlan, setRestorePlan] = useState<RestorePlan | null>(null);
  const [restoreBinding, setRestoreBinding] = useState<ProjectBinding | null>(null);
  const [restoreProjectName, setRestoreProjectName] = useState("project");
  const [dependencyPlan, setDependencyPlan] = useState<DependencyPlan | null>(null);
  const [restoreResult, setRestoreResult] = useState<RestoreResult | null>(null);
  const [historyRefreshEpoch, setHistoryRefreshEpoch] = useState(0);
  const [dependencyResult, setDependencyResult] = useState<DependencyResult | null>(null);
  const [restoreError, setRestoreError] = useState<string | null>(null);
  const [pullApplyPhase, setPullApplyPhase] = useState<PullApplyPhase>("idle");
  const [reviewSupportLoading, setReviewSupportLoading] = useState(false);
  const [completedPullActionIds, setCompletedPullActionIds] = useState<Set<string>>(new Set());
  const [completedPullResourceIds, setCompletedPullResourceIds] = useState<Set<string>>(new Set());
  const [failedPullResourceIds, setFailedPullResourceIds] = useState<Set<string>>(new Set());
  const restoreRequest = useRef(0);

  useEffect(() => {
    activityOpenRef.current = activityOpen;
  }, [activityOpen]);

  useEffect(() => {
    logFiltersRef.current = { types: logTypeFilters, level: logLevelFilter, search: logSearch };
  }, [logLevelFilter, logSearch, logTypeFilters]);

  useEffect(() => {
    const timer = window.setTimeout(() => setLogSearch(logSearchInput), 180);
    return () => window.clearTimeout(timer);
  }, [logSearchInput]);

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

  const loadActivityLogs = useCallback(async (cursor: string | null = null) => {
    const requestId = ++logRequestRef.current;
    const startedAt = Date.now();
    const loadingOlder = cursor !== null;
    if (loadingOlder) {
      setLogLoadingOlder(true);
    } else {
      setLogLoading(true);
      setLogLines([]);
    }
    setLogLoadError(null);
    if (logTypeFilters.length === 0) {
      setLogCursor(null);
      setLogLines([]);
      setLogLoading(false);
      setLogLoadingOlder(false);
      return;
    }
    try {
      const page = await projectSyncApi.queryActivityLogs({
        types: logTypeFilters,
        levels: logLevelFilter === "all" ? [] : [logLevelFilter],
        search: logSearch.trim() || null,
        cursor,
        limit: 500,
      });
      if (requestId !== logRequestRef.current) return;
      setLogCursor(page.next_cursor ?? null);
      setLogLines((current) => {
        const next = loadingOlder
          ? mergeLogLines(page.entries, current)
          : mergeLogLines(
            page.entries,
            current.filter((line) => line.ts >= startedAt
              && logMatchesFilters(line, logTypeFilters, logLevelFilter, logSearch)),
          );
        return next.slice(-2_000);
      });
    } catch (reason) {
      if (requestId === logRequestRef.current) setLogLoadError(errorMessage(reason));
    } finally {
      if (requestId === logRequestRef.current) {
        setLogLoading(false);
        setLogLoadingOlder(false);
      }
    }
  }, [logLevelFilter, logSearch, logTypeFilters]);

  useEffect(() => {
    void loadActivityLogs();
  }, [loadActivityLogs]);

  useEffect(() => {
    const unlisten = listen<LogLine>("sync-log", (event) => {
      const filters = logFiltersRef.current;
      if (logMatchesFilters(event.payload, filters.types, filters.level, filters.search)) {
        setLogLines((current) => mergeLogLines(current, [event.payload]).slice(-2_000));
      }
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
    void loadActivityLogs();
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
      local_alias: registration.local_alias,
      revision: registration.revision,
      repository_fingerprint: registration.repository_fingerprint,
      project_root: localBinding?.project_root ?? null,
      canonical_project_root: localBinding?.canonical_project_root ?? null,
      profile_ids: localBinding?.profile_ids ?? {},
      profile_names: [...new Set(Object.values(localBinding?.profile_ids ?? {})
        .map((profileId) => profiles.find((profile) => profile.profile_id === profileId)?.display_name)
        .filter((name): name is string => Boolean(name)))],
      providers: active
        ? [...new Set(activeResources.flatMap((resource) => resource.provider ? [resource.provider] : []))]
        : undefined,
      resource_count: active ? activeResources.length : undefined,
      selected_resource_count: Object.keys(registration.recipe.entries).length,
      linked_storage_ids: links.map((link) => link.storage_id),
      readiness_state: active ? readiness?.state : undefined,
      is_git_repository: repositoryKinds[registration.local_project_id],
    };
  }), [activeProjectId, bindings, config.links, profiles, readiness?.state, registrations, repositoryKinds, resources]);

  const activeSummary = projects.find((candidate) => candidate.local_project_id === activeProjectId) ?? null;
  const activeDraftSummary = setupDraftId
    ? setupDrafts.find((candidate) => candidate.draft_id === setupDraftId) ?? null
    : null;
  const workspaceTitle = activeDraftSummary?.display_name
    ?? (activeSummary ? projectLabel(activeSummary) : "Projects");
  const restoreProfileLabel = restoreBinding
    ? [...new Set(Object.values(restoreBinding.profile_ids)
      .map((profileId) => profiles.find((profile) => profile.profile_id === profileId)?.display_name)
      .filter((name): name is string => Boolean(name)))]
      .join(" + ") || "the assigned provider profile"
    : "the assigned provider profile";
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
  const loadConversationPathAudits = async (nextBindings: ProjectBinding[]) => {
    setConversationPathAuditLoading(true);
    const activeBindings = nextBindings.filter((candidate) => candidate.state === "active");
    const results = await Promise.all(activeBindings.map(async (candidate) => {
      try {
        const audit = await projectSyncApi.auditCodexConversationPaths(candidate.local_project_id);
        return { projectId: candidate.local_project_id, audit, error: null };
      } catch (reason) {
        return {
          projectId: candidate.local_project_id,
          audit: null,
          error: errorMessage(reason),
        };
      }
    }));
    const nextAudits: Record<string, CodexConversationPathAudit> = {};
    const nextErrors: Record<string, string> = {};
    for (const result of results) {
      if (result.audit) nextAudits[result.projectId] = result.audit;
      if (result.error) nextErrors[result.projectId] = result.error;
    }
    setConversationPathAudits(nextAudits);
    setConversationPathAuditErrors(nextErrors);
    setConversationPathAuditLoading(false);
  };
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
      setRepositoryKinds(await projectSyncApi.listProjectRepositoryKinds());
    } catch (reason) {
      failures.push(`Repository types: ${errorMessage(reason)}`);
      setRepositoryKinds({});
    }
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
    await loadConversationPathAudits(nextBindings);
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
          setReadiness(await projectSyncApi.getReadiness(
            storageId,
            nextDetail.project.bundle_id,
            nextDetail.binding,
          ));
        } catch {
          setReadiness(null);
        }
      } else {
        setReadiness(null);
      }
    } catch (reason) {
      setDetail(null);
      setInventory(null);
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

  const closeWorkspaceSettings = () => {
    setPendingPush(null);
    setPushError(null);
    if (restorePlan) closePullReview();
    setStorageEditorRequest((current) => ({
      mode: "close",
      requestId: (current?.requestId ?? 0) + 1,
    }));
    setProjectEditorRequest((current) => ({
      mode: "close",
      projectId: "",
      requestId: (current?.requestId ?? 0) + 1,
    }));
  };

  const beginAddProject = async () => {
    const picked = await open({ directory: true, multiple: false });
    if (typeof picked !== "string" || !picked) return;
    setBusy(true);
    setError(null);
    try {
      const created = await projectSyncApi.createSetupDraft(picked);
      closeWorkspaceSettings();
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
    closeWorkspaceSettings();
    setSetupDraftId((current) => current === draftId ? null : draftId);
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
      ? `${projectLabel(detail.project)} connected. Review Pull before files are applied.`
      : `${projectLabel(detail.project)} is set up${storageId ? "" : " — link storage when ready"}.`);
    const { nextConfig } = await loadShell();
    await refreshSetupDrafts();
    setActiveProjectId(projectId);
    if (detail.binding) setBindings((current) => upsertBinding(current, detail.binding as ProjectBinding));
    await loadProjectData(projectId, nextConfig, storageId);
    if (completion === "pull" && storageId && detail.binding) {
      await planRestore(storageId, detail.project.bundle_id, detail.binding, projectLabel(detail.project));
    } else if (completion === "push" && storageId) {
      await publishProject(projectId, storageId, detail.project.recipe);
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

  const repairConversationPaths = async (projectId: string) => {
    const audit = conversationPathAudits[projectId];
    if (!audit?.can_repair) return;
    const registration = registrations.find((candidate) => candidate.local_project_id === projectId);
    const projectName = registration ? projectLabel(registration) : "this project";
    const approved = await confirm(
      `Repair ${audit.issues.length} Codex conversation path${audit.issues.length === 1 ? "" : "s"} for “${projectName}”?\n\nOnly conversations explicitly assigned to this project by Codex Desktop will be changed. Mallard will back up every affected rollout first, then update structural cwd fields without changing messages or history.\n\nClose the affected Codex tasks before continuing so they are not written at the same time.`,
      { title: "Repair conversation paths" },
    );
    if (!approved) return;

    setBusy(true);
    setError(null);
    try {
      const result = await projectSyncApi.repairCodexConversationPaths(projectId);
      setConversationPathAudits((current) => ({
        ...current,
        [projectId]: result.audit,
      }));
      setConversationPathAuditErrors((current) => {
        const next = { ...current };
        delete next[projectId];
        return next;
      });
      setActiveProjectId(projectId);
      await loadProjectData(projectId, config, activeStorageByProject[projectId]);
      setNotice(
        `Repaired ${result.repaired_thread_ids.length} Codex conversation path${result.repaired_thread_ids.length === 1 ? "" : "s"}. Original rollouts were backed up.`,
      );
    } catch (reason) {
      setError(errorMessage(reason));
      try {
        const refreshed = await projectSyncApi.auditCodexConversationPaths(projectId);
        setConversationPathAudits((current) => ({ ...current, [projectId]: refreshed }));
      } catch {
        // Keep the prior gate visible when the follow-up audit also fails.
      }
    } finally {
      setBusy(false);
    }
  };

  const openPushChooser = async (projectId: string, storageId: string) => {
    if (restorePlan) closePullReview();
    setBusy(true);
    setError(null);
    setPushError(null);
    try {
      const [nextInventory, nextDetail] = await Promise.all([
        projectSyncApi.getInventory(projectId),
        projectSyncApi.getProject(projectId),
      ]);
      if (!nextDetail) throw new Error("The selected local project no longer exists.");
      const storage = config.storages.find((candidate) => candidate.id === storageId);
      if (!storage) throw new Error("The selected storage no longer exists.");
      const link = nextDetail.links.find((candidate) => candidate.storage_id === storageId);
      if (!link) throw new Error("This project is no longer linked to the selected storage.");

      const savedRecipe = link.recipe ?? null;
      setPendingPush({
        projectId,
        storageId,
        projectName: projectLabel(nextDetail.project),
        storage,
        inventory: nextInventory,
        savedRecipe,
        projectDefaults: recipeSelection(nextInventory.recipe),
      });
      setPushSelected(savedRecipe ? recipeSelection(savedRecipe) : new Set());
      setActiveProjectId(projectId);
      setActiveStorageByProject((current) => ({ ...current, [projectId]: storageId }));
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const refreshRepositoryKinds = async () => {
    try {
      setRepositoryKinds(await projectSyncApi.listProjectRepositoryKinds());
    } catch {
      setRepositoryKinds({});
    }
  };

  const publishProject = async (
    projectId: string,
    storageId: string,
    recipe: BundleRecipe,
  ) => {
    openActivity();
    setBusy(true);
    setError(null);
    setPushError(null);
    try {
      const result = await projectSyncApi.pushBundle(projectId, storageId, recipe);
      setNotice(result.message);
      setPendingPush(null);
      setActiveProjectId(projectId);
      setActiveStorageByProject((current) => ({ ...current, [projectId]: storageId }));
      const { nextConfig } = await loadShell();
      await loadProjectData(projectId, nextConfig, storageId);
    } catch (reason) {
      const message = errorMessage(reason);
      if (pendingPush) setPushError(message);
      else setError(message);
    } finally {
      setBusy(false);
    }
  };

  const publishPendingPush = async () => {
    if (!pendingPush) return;
    const resources = inventoryResources(pendingPush.inventory);
    const baseRecipe = pendingPush.savedRecipe ?? {
      ...pendingPush.inventory.recipe,
      revision: 0,
      entries: {},
    };
    const recipe = recipeWithSelection(baseRecipe, resources, pushSelected);
    await publishProject(pendingPush.projectId, pendingPush.storageId, recipe);
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
      const nextBindings = upsertBinding(bindings, saved);
      setBindings(nextBindings);
      setDetail((current) => current ? { ...current, binding: saved } : current);
      await refreshRepositoryKinds();
      setNotice("Machine binding updated. Cloud identity and logical paths were unchanged.");
      await loadConversationPathAudits(nextBindings);
      await loadProjectData(activeProjectId, config, activeStorageId);
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const renameProject = async (projectId: string, alias: string | null): Promise<boolean> => {
    setBusy(true);
    setError(null);
    try {
      const registration = registrations.find((candidate) => candidate.local_project_id === projectId);
      if (!registration) throw new Error("Project not found.");
      const updated = await projectSyncApi.renameProject(projectId, alias, registration.revision);
      setRegistrations((current) => current.map((candidate) => (
        candidate.local_project_id === projectId ? updated : candidate
      )));
      if (activeProjectId === projectId) {
        setDetail((current) => current ? { ...current, project: updated } : current);
      }
      setNotice(alias
        ? `Project shown as “${alias}” on this machine. The shared repo name is unchanged.`
        : "Custom name cleared; showing the shared repo name again.");
      return true;
    } catch (reason) {
      setError(errorMessage(reason));
      return false;
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
    setPullApplyPhase("idle");
    setReviewSupportLoading(true);
    setCompletedPullActionIds(new Set());
    setCompletedPullResourceIds(new Set());
    setFailedPullResourceIds(new Set());
    setRestorePlan(null);
    setDependencyPlan(null);
    setReadiness(null);
    setRestoreBinding(targetBinding);
    setRestoreProjectName(displayName);
    try {
      // The project-data review is the primary Pull surface. Render it as soon as it
      // is ready; optional dependency/readiness checks must never hide a valid
      // restore plan or its Apply button.
      const request = { storageId, bundleId, binding: targetBinding };
      const review = await beginPullReview(projectSyncApi, request);
      if (restoreRequest.current !== requestId) return null;
      setRestorePlan(review.restorePlan);
      setNotice(`Pull review ready for ${displayName}. Nothing has been applied yet.`);

      // Supporting context fills in asynchronously after the workspace is usable.
      // Its lifecycle is request-scoped so closing or replanning cannot apply
      // stale results to another review.
      void review.support
        .then((support) => {
          if (restoreRequest.current !== requestId) return;
          setDependencyPlan(support.dependencyPlan);
          setReadiness(support.readiness);
          if (support.errors.length > 0) {
            setRestoreError(`The project changes are ready to apply. ${support.errors.join(" ")}`);
          }
        })
        .catch((reason) => {
          if (restoreRequest.current === requestId) {
            setRestoreError(`The project changes are ready to apply. Supporting checks: ${errorMessage(reason)}`);
          }
        })
        .finally(() => {
          if (restoreRequest.current === requestId) setReviewSupportLoading(false);
        });
      return null;
    } catch (reason) {
      const message = errorMessage(reason);
      if (restoreRequest.current === requestId) {
        setReviewSupportLoading(false);
        setRestoreError(message);
        setError(message);
      }
      return message;
    } finally {
      if (restoreRequest.current === requestId) setBusy(false);
    }
  };

  const beginProjectRestore = async (projectId: string, storageId: string) => {
    setPendingPush(null);
    setPushError(null);
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
        projectLabel(nextDetail.project),
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
        projectLabel(connected.project),
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
      `Remove “${summary ? projectLabel(summary) : "this project"}” from Mallard?\n\nThe checkout and provider files stay on disk. Only this app's project registration, links, and active binding are removed.`,
      { title: "Remove project" },
    );
    if (!approved) return;
    setBusy(true);
    setError(null);
    try {
      await projectSyncApi.removeProject(projectId);
      setNotice(`${summary ? projectLabel(summary) : "Project"} removed from Mallard. Files were not deleted.`);
      await refresh();
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const closePullReview = () => {
    restoreRequest.current += 1;
    setRestorePlan(null);
    setRestoreBinding(null);
    setDependencyPlan(null);
    setRestoreResult(null);
    setDependencyResult(null);
    setRestoreError(null);
    setPullApplyPhase("idle");
    setReviewSupportLoading(false);
    setCompletedPullActionIds(new Set());
    setCompletedPullResourceIds(new Set());
    setFailedPullResourceIds(new Set());
  };

  const unlinkStorage = async (projectId: string, storageId: string) => {
    const project = projects.find((candidate) => candidate.local_project_id === projectId);
    const storage = config.storages.find((candidate) => candidate.id === storageId);
    if (!project || !storage) return;

    const projectName = projectLabel(project);
    const storageName = storage.name || "Storage";
    const approved = await confirm(
      `Unlink “${storageName}” from “${projectName}”?\n\nPush and Pull will stop for this pairing. Files and repositories already stored there will not be deleted, and you can link the storage again later.`,
      { title: "Unlink storage" },
    );
    if (!approved) return;

    setBusy(true);
    setError(null);
    try {
      const removed = await projectSyncApi.removeLink(projectId, storageId);

      if (pendingPush?.projectId === projectId && pendingPush.storageId === storageId) {
        setPendingPush(null);
        setPushError(null);
      }
      if (restoreBinding?.local_project_id === projectId && restorePlan?.storage_id === storageId) {
        closePullReview();
      }
      if (pendingBundleConnection?.projectId === projectId && pendingBundleConnection.storageId === storageId) {
        setPendingBundleConnection(null);
      }

      const { nextConfig } = await loadShell();
      const remainingLinks = nextConfig.links.filter((link) => link.local_project_id === projectId);
      const rememberedStorageId = activeStorageByProject[projectId];
      const nextStorageId = rememberedStorageId
        && rememberedStorageId !== storageId
        && remainingLinks.some((link) => link.storage_id === rememberedStorageId)
        ? rememberedStorageId
        : remainingLinks[0]?.storage_id ?? null;

      setActiveStorageByProject((current) => {
        const next = { ...current };
        if (nextStorageId) next[projectId] = nextStorageId;
        else delete next[projectId];
        return next;
      });
      setActiveProjectId(projectId);
      await loadProjectData(projectId, nextConfig, nextStorageId);
      setNotice(removed
        ? `${storageName} unlinked from ${projectName}. Stored files were not deleted.`
        : `${storageName} was already unlinked from ${projectName}.`);
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const applyReview = async (selection: PullReviewSelection) => {
    if (!restorePlan || !restoreBinding) return;
    const currentRestorePlan = restorePlan;
    const currentDependencyPlan = dependencyPlan;
    openActivity();
    setFailedPullResourceIds((current) => {
      const next = new Set(current);
      for (const resourceId of selection.resourceIds) next.delete(resourceId);
      return next;
    });
    if (selection.restoreActionIds.length > 0) setRestoreResult(null);
    if (selection.dependencyActionIds.length > 0) setDependencyResult(null);
    setBusy(true);
    setRestoreError(null);
    try {
      const result = await applyPullReview(
        projectSyncApi,
        currentRestorePlan,
        currentDependencyPlan,
        selection,
        setPullApplyPhase,
      );
      if (result.restoreResult) {
        setRestoreResult(result.restoreResult);
        if (result.restoreResult.success) setHistoryRefreshEpoch((epoch) => epoch + 1);
      }
      if (result.dependencyResult) setDependencyResult(result.dependencyResult);
      if (result.readiness) setReadiness(result.readiness);
      if (result.error) setRestoreError(result.error);

      const resourceByAction = new Map<string, string>();
      for (const action of currentRestorePlan.actions) {
        resourceByAction.set(action.action_id, action.resource_id);
      }
      for (const action of currentDependencyPlan?.actions ?? []) {
        resourceByAction.set(action.action_id, action.resource_id);
      }
      const appliedActionIds = new Set([
        ...(result.restoreResult?.applied_action_ids ?? []),
        ...(result.dependencyResult?.applied_action_ids ?? []),
      ]);
      const failedActionIds = new Set([
        ...(result.restoreResult?.failed_actions ?? []).map((failure) => failure.action_id),
        ...(result.dependencyResult?.failed_actions ?? []).map((failure) => failure.action_id),
      ]);
      const interruptedActionIds = result.failedPhase === "restoring"
        ? selection.restoreActionIds
        : result.failedPhase === "installing"
          ? selection.dependencyActionIds
          : [];
      for (const actionId of interruptedActionIds) {
        if (!appliedActionIds.has(actionId)) failedActionIds.add(actionId);
      }
      const failedResources = new Set(
        [...failedActionIds]
          .map((actionId) => resourceByAction.get(actionId))
          .filter((resourceId): resourceId is string => Boolean(resourceId)),
      );
      const selectedActions = [...selection.restoreActionIds, ...selection.dependencyActionIds];
      const successfulResources = new Set(selection.resourceIds.filter((resourceId) => {
        const actionIds = selectedActions.filter((actionId) => resourceByAction.get(actionId) === resourceId);
        return actionIds.length > 0 && actionIds.every((actionId) => appliedActionIds.has(actionId));
      }));
      setCompletedPullActionIds((current) => new Set([...current, ...appliedActionIds]));
      setCompletedPullResourceIds((current) => {
        const next = new Set(current);
        for (const resourceId of successfulResources) next.add(resourceId);
        for (const resourceId of failedResources) next.delete(resourceId);
        return next;
      });
      setFailedPullResourceIds((current) => {
        const next = new Set(current);
        for (const resourceId of selection.resourceIds) next.delete(resourceId);
        for (const resourceId of failedResources) next.add(resourceId);
        return next;
      });

      // A plan is single-use even when the user skipped or failed an action.
      // Prepare fresh generation-pinned plans so remaining setup can be
      // selected or retried without reopening the review.
      const approvedThisPass = new Set([
        ...selection.restoreActionIds,
        ...selection.dependencyActionIds,
      ]);
      const deferredRestoreKinds = new Set(["install_plugin", "install_standalone_skill"]);
      const hasSkippedActions = [
        ...currentRestorePlan.actions
          .filter((action) => !deferredRestoreKinds.has(action.kind.kind))
          .map((action) => action.action_id),
        ...(currentDependencyPlan?.actions ?? []).map((action) => action.action_id),
      ].some((actionId) => (
        !completedPullActionIds.has(actionId) && !approvedThisPass.has(actionId)
      ));
      if (!result.success || hasSkippedActions) {
        const nextRestore = await projectSyncApi.planRestore(
          currentRestorePlan.storage_id,
          currentRestorePlan.bundle_id,
          restoreBinding,
        );
        setRestorePlan(nextRestore);
        const support = await loadPullReviewSupport(projectSyncApi, nextRestore);
        setDependencyPlan(support.dependencyPlan);
        setReadiness(support.readiness);
        if (support.errors.length > 0) {
          setRestoreError(`Changes were applied, but remaining setup could not be prepared: ${support.errors.join(" ")}`);
        }
      }
      if (activeProjectId) await loadProjectData(activeProjectId, config, activeStorageId);
    } catch (reason) {
      setPullApplyPhase("complete");
      setRestoreError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const refreshRestore = async () => {
    if (!restorePlan || !restoreBinding) return;
    const currentPlan = restorePlan;
    setBusy(true);
    setRestoreError(null);
    try {
      const nextRestore = await projectSyncApi.planRestore(
        currentPlan.storage_id,
        currentPlan.bundle_id,
        restoreBinding,
      );
      const support = await loadPullReviewSupport(projectSyncApi, nextRestore);
      if (nextRestore.generation !== currentPlan.generation || nextRestore.manifest_sha256 !== currentPlan.manifest_sha256) {
        setRestoreResult(null);
        setDependencyResult(null);
        setPullApplyPhase("idle");
        setCompletedPullActionIds(new Set());
        setCompletedPullResourceIds(new Set());
        setFailedPullResourceIds(new Set());
      }
      setRestorePlan(nextRestore);
      setDependencyPlan(support.dependencyPlan);
      setReadiness(support.readiness);
      if (support.errors.length > 0) setRestoreError(support.errors.join(" "));
    } catch (reason) {
      setRestoreError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
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
      `Remove “${storageName}” from Mallard?\n\nFiles already written to the storage location will not be deleted.`,
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

  const openNewStorageConfiguration = () => {
    setSetupDraftId(null);
    setPendingPush(null);
    setPushError(null);
    if (restorePlan) closePullReview();
    setStorageEditorRequest((current) => ({
      mode: "create",
      storageKind: "local",
      requestId: (current?.requestId ?? 0) + 1,
    }));
  };

  const inlineStorageReview = pendingPush ? {
    kind: "push" as const,
    projectId: pendingPush.projectId,
    storageId: pendingPush.storageId,
    onClose: () => {
      if (busy) return;
      setPendingPush(null);
      setPushError(null);
    },
    content: (
      <PushResourceWorkspace
        resources={inventoryResources(pendingPush.inventory)}
        selected={pushSelected}
        projectDefaults={pendingPush.projectDefaults}
        busy={busy}
        error={pushError}
        onToggle={(resourceId) => setPushSelected((current) => {
          const next = new Set(current);
          if (next.has(resourceId)) next.delete(resourceId);
          else next.add(resourceId);
          return next;
        })}
        onUseProjectDefaults={() => setPushSelected(new Set(pendingPush.projectDefaults))}
        onClear={() => setPushSelected(new Set())}
        onClose={() => {
          if (busy) return;
          setPendingPush(null);
          setPushError(null);
        }}
        onPush={() => void publishPendingPush()}
      />
    ),
  } : restorePlan && restoreBinding ? {
    kind: "pull" as const,
    projectId: restoreBinding.local_project_id,
    storageId: restorePlan.storage_id,
    onClose: closePullReview,
    content: (
      <RestorePlanView
        embedded
        projectName={restoreProjectName}
        profileLabel={restoreProfileLabel}
        plan={restorePlan}
        binding={restoreBinding}
        dependencyPlan={dependencyPlan}
        readiness={readiness}
        restoreResult={restoreResult}
        dependencyResult={dependencyResult}
        phase={pullApplyPhase}
        supportLoading={reviewSupportLoading}
        completedActionIds={completedPullActionIds}
        completedResourceIds={completedPullResourceIds}
        failedResourceIds={failedPullResourceIds}
        busy={busy}
        error={restoreError}
        onApply={(selection) => void applyReview(selection)}
        onRefresh={() => void refreshRestore()}
        onBack={closePullReview}
      />
    ),
  } : null;

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
        activeStorageId={activeStorageSettingsId}
        loading={loading}
        busy={busy}
        activityOpen={activityOpen}
        unreadLogs={unreadLogs}
        onSelectProject={(id) => {
          setSetupDraftId(null);
          closeWorkspaceSettings();
          if (restorePlan) closePullReview();
          void selectProject(id);
        }}
        onConfigureProject={(projectId) => {
          setSetupDraftId(null);
          setPendingPush(null);
          setPushError(null);
          if (restorePlan) closePullReview();
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
          setSetupDraftId(null);
          setPendingPush(null);
          setPushError(null);
          if (restorePlan) closePullReview();
          setStorageEditorRequest((current) => ({
            mode: "toggle",
            storageId,
            requestId: (current?.requestId ?? 0) + 1,
          }));
        }}
        onRemoveStorage={(storageId) => void removeStorage(storageId)}
        onAddStorage={openNewStorageConfiguration}
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
        <header className="v3-titlebar" data-tauri-drag-region>
          <div className="v3-titlebar-context" data-tauri-drag-region title={workspaceTitle}>
            <Icon name="folder" size={16} />
            <strong>{workspaceTitle}</strong>
            {activeDraftSummary && <span>Draft setup</span>}
          </div>
          <button
            type="button"
            className="v3-theme-button"
            onClick={() => onThemeChange(theme === "dark" ? "light" : "dark")}
          >
            {theme === "dark" ? "Light" : "Dark"} theme
          </button>
        </header>

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
            activeProjectId={activeProjectId}
            bindings={bindings}
            profiles={profiles}
            storages={config.storages}
            links={config.links}
            loading={loading}
            busy={busy}
            error={error}
            conversationPathAudits={conversationPathAudits}
            conversationPathAuditErrors={conversationPathAuditErrors}
            conversationPathAuditLoading={conversationPathAuditLoading}
            onSelectProject={(projectId, storageId) => selectProject(projectId, storageId)}
            onLinkStorage={(projectId, storageId) => linkStorage(projectId, storageId)}
            onUnlinkStorage={(projectId, storageId) => unlinkStorage(projectId, storageId)}
            onPush={(projectId, storageId) => openPushChooser(projectId, storageId)}
            onPull={(projectId, storageId) => beginProjectRestore(projectId, storageId)}
            onRepairConversationPaths={(projectId) => repairConversationPaths(projectId)}
            onRenameProject={(projectId, alias) => renameProject(projectId, alias)}
            onRefresh={() => void refresh()}
            onAddProject={() => void beginAddProject()}
            onOpenStorageSettings={openNewStorageConfiguration}
            onSaveStorage={saveInlineStorage}
            storageEditorRequest={storageEditorRequest}
            onStorageEditorRequestHandled={() => setStorageEditorRequest(null)}
            onStorageEditorChange={setActiveStorageSettingsId}
            projectEditorRequest={projectEditorRequest}
            onProjectEditorRequestHandled={() => setProjectEditorRequest(null)}
            historyRefreshEpoch={historyRefreshEpoch}
            inlineStorageReview={inlineStorageReview}
            newProjectSetup={setupDraftId ? (
              <ProjectSetupWorkspace
                key={setupDraftId}
                draftId={setupDraftId}
                profiles={profiles}
                projects={projects}
                storages={config.storages}
                busy={busy}
                onClose={() => void closeSetupDraft()}
                onDiscard={(draftId) => void discardSetupDraft(draftId)}
                onAddStorage={openNewStorageConfiguration}
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
            typeFilters={logTypeFilters}
            levelFilter={logLevelFilter}
            search={logSearchInput}
            loading={logLoading}
            loadingOlder={logLoadingOlder}
            hasOlder={!!logCursor}
            error={logLoadError}
            onTypeFiltersChange={setLogTypeFilters}
            onLevelFilterChange={setLogLevelFilter}
            onSearchChange={setLogSearchInput}
            onLoadOlder={() => logCursor && void loadActivityLogs(logCursor)}
            onManage={() => setLogManagerOpen(true)}
            onClose={() => {
              activityOpenRef.current = false;
              setActivityOpen(false);
            }}
          />
        </div>
      </div>

      {pendingBundleConnection && pendingConnectionProject && pendingConnectionStorage && (
        <BundleConnectionDialog
          projectName={projectLabel(pendingConnectionProject)}
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
          title={`Project setup for ${activeSummary ? projectLabel(activeSummary) : "project"}`}
          description="Choose this machine's checkout and one agent profile: Codex or Claude. Existing files and provider state are never moved or deleted."
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

      {logManagerOpen && (
        <LogManagerDialog
          onClose={() => setLogManagerOpen(false)}
          onLogsChanged={() => void loadActivityLogs()}
        />
      )}

    </div>
  );
}
