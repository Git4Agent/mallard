import { useEffect, useMemo, useRef, useState } from "react";
import type { MouseEvent as ReactMouseEvent } from "react";
import { confirm, open } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import type {
  AppTheme,
  BundleReadiness,
  BundleRecipe,
  BundleSnapshotSummary,
  DependencyPlan,
  DependencyResult,
  LocalProjectRegistration,
  LocalProjectSummary,
  LogLine,
  ProjectBinding,
  ProjectDetail,
  ProjectDiscovery,
  ProjectProvider,
  ProjectStorageLink,
  ProviderProfile,
  ProviderProfileSummary,
  ResourceInventory as ResourceInventoryModel,
  ResourceStatusReport,
  RestorePlan,
  RestoreResult,
  SyncConfigV3,
} from "../../types";
import Icon from "../Icons";
import LogPanel from "../LogPanel";
import AddProjectDialog from "./AddProjectDialog";
import BundleConnectionDialog from "./BundleConnectionDialog";
import ProjectBindingEditor, { type ProjectBindingDraft } from "./ProjectBindingEditor";
import ProjectLinksWorkspace from "./ProjectLinksWorkspace";
import ProjectProfilePicker from "./ProjectProfilePicker";
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

function snapshotRecipe(snapshot: BundleSnapshotSummary): BundleRecipe {
  if (snapshot.recipe) return snapshot.recipe;
  return {
    schema_version: 1,
    revision: 0,
    entries: Object.fromEntries((snapshot.resources ?? [])
      .filter((resource) => resource.apply_policy !== "never")
      .map((resource) => [resource.resource_id, {
        resource_id: resource.resource_id,
        apply_policy: resource.apply_policy,
        required: false,
      }])),
  };
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
    | { mode: "edit"; storageId: string; requestId: number }
    | { mode: "create"; storageKind: "local" | "s3"; requestId: number }
    | null
  >(null);
  const [projectEditorRequest, setProjectEditorRequest] = useState<
    { projectId: string; requestId: number } | null
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
  const [unreadLogs, setUnreadLogs] = useState(0);
  const activityOpenRef = useRef(false);

  const [discovery, setDiscovery] = useState<ProjectDiscovery | null>(null);
  const [pendingProjectRoot, setPendingProjectRoot] = useState<string | null>(null);
  const [addError, setAddError] = useState<string | null>(null);
  const [editingBinding, setEditingBinding] = useState<ProjectBindingDraft | null>(null);
  const discoveryRequest = useRef(0);

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
      nextProjects = await projectSyncApi.listProjects();
      setRegistrations(nextProjects);
    } catch (reason) {
      failures.push(`Projects: ${errorMessage(reason)}`);
      setRegistrations([]);
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
    setAddError(null);
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
      const message = errorMessage(reason);
      setError(message);
      setAddError(message);
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

  const discoverForProfiles = async (
    projectRoot: string,
    profileIds: Partial<Record<ProjectProvider, string>>,
  ): Promise<boolean> => {
    const requestId = ++discoveryRequest.current;
    setBusy(true);
    setAddError(null);
    try {
      const nextDiscovery = await projectSyncApi.discoverProject(projectRoot, profileIds);
      if (requestId !== discoveryRequest.current) return false;
      setDiscovery(nextDiscovery);
      return true;
    } catch (reason) {
      if (requestId === discoveryRequest.current) setAddError(errorMessage(reason));
      return false;
    } finally {
      if (requestId === discoveryRequest.current) setBusy(false);
    }
  };

  const beginAddProject = async () => {
    const picked = await open({ directory: true, multiple: false });
    if (typeof picked !== "string" || !picked) return;
    setAddError(null);
    setPendingProjectRoot(picked);
  };

  const continueAddProject = async (profileIds: Partial<Record<ProjectProvider, string>>) => {
    if (!pendingProjectRoot) return;
    if (await discoverForProfiles(pendingProjectRoot, profileIds)) {
      setPendingProjectRoot(null);
    }
  };

  const createProject = async (
    displayName: string,
    projectRoot: string,
    profileIds: Partial<Record<ProjectProvider, string>>,
    recipe: BundleRecipe,
    storageIds: string[],
    remoteBundle: BundleSnapshotSummary | null,
  ) => {
    setBusy(true);
    setAddError(null);
    let registered: LocalProjectRegistration | null = null;
    try {
      registered = await projectSyncApi.registerProject({
        display_name: remoteBundle?.display_name ?? displayName,
        // A manually selected remote repo owns the portable identity. Keep a
        // local fingerprint only when the remote manifest does not have one.
        repository_fingerprint: remoteBundle?.repository_fingerprint
          ?? discovery?.repository_fingerprint
          ?? null,
        bundle_id: remoteBundle?.bundle_id ?? null,
      });
      const selectedRecipe = remoteBundle ? snapshotRecipe(remoteBundle) : recipe;
      const savedProject = await projectSyncApi.saveRecipe(registered.local_project_id, {
        ...selectedRecipe,
        revision: registered.recipe.revision,
      });
      const savedBinding = await projectSyncApi.saveBinding({
        local_project_id: registered.local_project_id,
        project_root: projectRoot,
        profile_ids: profileIds,
        expected_revision: null,
      });
      for (const storageId of storageIds) {
        await projectSyncApi.saveLink({
          local_project_id: savedProject.local_project_id,
          storage_id: storageId,
          pinned: true,
        });
      }
      setDiscovery(null);
      setNotice(remoteBundle
        ? `${savedProject.display_name} connected to the existing remote repo. Review Pull before files are applied.`
        : `${savedProject.display_name} added locally${storageIds.length > 0 ? " and linked to storage" : " — link storage when ready"}.`);
      const { nextConfig } = await loadShell();
      setBindings((current) => upsertBinding(current, savedBinding));
      setActiveProjectId(savedProject.local_project_id);
      await loadProjectData(savedProject.local_project_id, nextConfig, storageIds[0]);
      if (remoteBundle) {
        await planRestore(
          remoteBundle.storage_id,
          remoteBundle.bundle_id,
          savedBinding,
          remoteBundle.display_name,
        );
      }
    } catch (reason) {
      if (registered) {
        setDiscovery(null);
        setError(`The project was registered, but setup is incomplete: ${errorMessage(reason)}`);
        const { nextConfig } = await loadShell();
        setActiveProjectId(registered.local_project_id);
        await loadProjectData(registered.local_project_id, nextConfig);
      } else {
        setAddError(errorMessage(reason));
      }
    } finally {
      setBusy(false);
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

  const saveStorageConfig = async (next: SyncConfigV3) => {
    setBusy(true);
    setError(null);
    try {
      const saved = await projectSyncApi.saveConfig(next);
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
      setNotice("Project storage settings saved.");
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const saveInlineStorage = async (storage: SyncConfigV3["storages"][number]) => {
    const exists = config.storages.some((candidate) => candidate.id === storage.id);
    await saveStorageConfig({
      ...config,
      storages: exists
        ? config.storages.map((candidate) => candidate.id === storage.id ? storage : candidate)
        : [...config.storages, storage],
    });
  };

  return (
    <div className="v3-app">
      <ProjectSidebar
        projects={projects}
        storages={config.storages}
        activeProjectId={activeProjectId}
        loading={loading}
        busy={busy}
        activityOpen={activityOpen}
        unreadLogs={unreadLogs}
        onSelectProject={(id) => void selectProject(id)}
        onConfigureProject={(projectId) => {
          setProjectEditorRequest((current) => ({
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
            mode: "edit",
            storageId,
            requestId: (current?.requestId ?? 0) + 1,
          }));
        }}
        onAddStorage={() => {
          setStorageEditorRequest((current) => ({
            mode: "create",
            storageKind: "local",
            requestId: (current?.requestId ?? 0) + 1,
          }));
        }}
        onOpenLegacy={onOpenLegacy}
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
            onSelectBundle={connectStorageBundle}
            storageEditorRequest={storageEditorRequest}
            onStorageEditorRequestHandled={() => setStorageEditorRequest(null)}
            projectEditorRequest={projectEditorRequest}
            onProjectEditorRequestHandled={() => setProjectEditorRequest(null)}
            newProjectSetup={pendingProjectRoot ? (
              <ProjectProfilePicker
                inline
                projectRoot={pendingProjectRoot}
                profiles={profiles}
                initialProfileIds={defaultProfileIds()}
                busy={busy}
                error={addError}
                onAddProfile={chooseAndCreateProfile}
                onCancel={() => {
                  setPendingProjectRoot(null);
                  setAddError(null);
                }}
                onContinue={(profileIds) => void continueAddProject(profileIds)}
              />
            ) : discovery ? (
              <AddProjectDialog
                inline
                discovery={discovery}
                profiles={profiles}
                storages={config.storages}
                busy={busy}
                error={addError}
                onCancel={() => setDiscovery(null)}
                onCreate={(displayName, projectRoot, profileIds, recipe, storageIds, remoteBundle) => (
                  void createProject(displayName, projectRoot, profileIds, recipe, storageIds, remoteBundle)
                )}
                onProfilesChange={(profileIds) => {
                  void discoverForProfiles(discovery.project_root, profileIds);
                }}
                onAddProfile={chooseAndCreateProfile}
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
