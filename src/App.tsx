import { useState, useEffect, useCallback, useRef, useMemo } from "react";
import type { MouseEvent as ReactMouseEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { confirm, open } from "@tauri-apps/plugin-dialog";
import FilesWorkspace from "./components/FilesWorkspace";
import SyncPanel from "./components/SyncPanel";
import LogPanel from "./components/LogPanel";
import FinishSetup from "./components/FinishSetup";
import Icon from "./components/Icons";
import ProjectSyncV3 from "./components/project-sync/ProjectSyncV3";
import { profileLabel } from "./components/SyncPanel";
import { AppTheme, CloudRootState, CodexPluginRepairReport, CodexPluginRestoreState, ConfigSource, FileEntry, FileStatusReport, PluginRepairReport, ProjectPathApplyReport, ProjectPathMapping, SetupIssue, SetupReadiness, SyncConfig, SyncStatus, SyncResult, LogLine, SyncProgress } from "./types";
import { applyTheme, getStoredTheme } from "./theme";
import "./App.css";

const MIN_SIDEBAR_WIDTH = 240;
const MAX_SIDEBAR_WIDTH = 560;
const MIN_LOG_HEIGHT = 140;
const MAX_LOG_HEIGHT = 520;
const IS_MACOS = /Macintosh|Mac OS X/.test(navigator.userAgent);

function collectAllSyncable(entries: FileEntry[]): string[] {
  return entries.flatMap((e) => {
    if (!e.included) return [];
    if (!e.is_dir) return [e.path];
    if (e.children == null) return [e.path];
    if (e.children.length === 0) return [e.path];
    return collectAllSyncable(e.children);
  });
}

function countSyncableFiles(entries: FileEntry[]): number {
  return entries.reduce((count, entry) => {
    if (!entry.included) return count;
    if (!entry.is_dir) return count + 1;
    return count + (entry.children ? countSyncableFiles(entry.children) : 0);
  }, 0);
}

function relativeSourcePath(path: string, sourcePath: string): string | null {
  if (path === sourcePath) return "";
  const prefix = sourcePath.endsWith("/") ? sourcePath : `${sourcePath}/`;
  return path.startsWith(prefix) ? path.slice(prefix.length) : null;
}

function reconcileSyncSelection(
  previousSources: ConfigSource[],
  nextSources: ConfigSource[],
  previousSelection: Set<string>,
): Set<string> {
  if (previousSources.length === 0) {
    return new Set(nextSources.flatMap((source) => collectAllSyncable(source.entries)));
  }

  const previousById = new Map(previousSources.map((source) => [source.id, source]));
  const nextSelection = new Set<string>();

  for (const nextSource of nextSources) {
    const nextPaths = collectAllSyncable(nextSource.entries);
    const previousSource = previousById.get(nextSource.id);
    if (!previousSource) {
      nextPaths.forEach((path) => nextSelection.add(path));
      continue;
    }

    const previousPaths = collectAllSyncable(previousSource.entries);
    // Nothing was selectable before (fresh mount pre-pull, mid-switch reload):
    // an empty selection means "no choice was possible", not "user chose
    // none" — treat the source as new, or the selection collapses to zero
    // permanently and the next push silently publishes a near-empty profile.
    if (previousPaths.length === 0) {
      nextPaths.forEach((path) => nextSelection.add(path));
      continue;
    }
    const selectedPaths = previousPaths.filter((path) => previousSelection.has(path));
    if (selectedPaths.length === 0) continue;

    if (selectedPaths.length === previousPaths.length) {
      nextPaths.forEach((path) => nextSelection.add(path));
      continue;
    }

    const selectedRelativePaths = selectedPaths
      .map((path) => relativeSourcePath(path, previousSource.path))
      .filter((path): path is string => path !== null);

    for (const path of nextPaths) {
      const relativePath = relativeSourcePath(path, nextSource.path);
      if (relativePath === null) continue;
      if (selectedRelativePaths.some(
        (selectedPath) => relativePath === selectedPath || relativePath.startsWith(`${selectedPath}/`),
      )) {
        nextSelection.add(path);
      }
    }
  }

  return nextSelection;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

function codexRepairStatus(state: CodexPluginRestoreState): Pick<SyncStatus, "state" | "message" | "preserveMessage"> {
  switch (state) {
    case "ready":
      return { state: "success", message: "Codex plugins ready", preserveMessage: true };
    case "partial":
      return { state: "partial", message: "Codex plugins partially restored", preserveMessage: true };
    case "failed":
      return { state: "error", message: "Codex plugin restore failed", preserveMessage: true };
  }
}

interface LegacyAppProps {
  theme: AppTheme;
  onThemeChange: (theme: AppTheme) => void;
  onOpenProjects: () => void;
}

function LegacyApp({ theme, onThemeChange, onOpenProjects }: LegacyAppProps) {

  const [sources, setSources] = useState<ConfigSource[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);

  const [selectedFile, setSelectedFile] = useState<string | null>(null);
  const [activeProfileId, setActiveProfileId] = useState<string | null>(null);
  const [activeStorageByProfile, setActiveStorageByProfile] = useState<Record<string, string>>({});
  // The editor reports unsaved changes here; navigation away asks first.
  const editorDirty = useRef(false);

  const [logLines, setLogLines] = useState<LogLine[]>([]);
  const [showLog, setShowLog] = useState(false);
  const [unreadLogs, setUnreadLogs] = useState(0);
  const [progress, setProgress] = useState<SyncProgress | null>(null);
  const showLogRef = useRef(showLog);
  useEffect(() => { showLogRef.current = showLog; }, [showLog]);

  const appendLog = useCallback((line: Omit<LogLine, "ts">) => {
    setLogLines((prev) => [...prev, { ...line, ts: Date.now() }]);
    if (!showLogRef.current) setUnreadLogs((n) => n + 1);
  }, []);

  useEffect(() => {
    const pLog  = listen<LogLine>("sync-log", (ev) => {
      setLogLines((prev) => [...prev, ev.payload]);
      if (!showLogRef.current) setUnreadLogs((n) => n + 1);
    });
    const pProg = listen<SyncProgress>("sync-progress", (ev) => {
      setProgress(ev.payload);
    });
    return () => {
      pLog.then((fn) => fn());
      pProg.then((fn) => fn());
    };
  }, []);

  const [fileStatuses, setFileStatuses] = useState<Map<string, string>>(new Map());
  const [clouds, setClouds] = useState<CloudRootState[]>([]);
  const [statusLoading, setStatusLoading] = useState(false);
  const [statusError, setStatusError] = useState<string | null>(null);
  const statusRequestRef = useRef(0);

  const [syncConfig, setSyncConfig] = useState<SyncConfig>({
    schema: 2, storages: [], local_profiles: [], links: [],
  });
  const syncConfigRef = useRef(syncConfig);
  useEffect(() => { syncConfigRef.current = syncConfig; }, [syncConfig]);
  const [selectedForSync, setSelectedForSync] = useState<Set<string>>(new Set());
  const selectedForSyncRef = useRef(selectedForSync);
  const sourcesRef = useRef<ConfigSource[]>([]);
  useEffect(() => { selectedForSyncRef.current = selectedForSync; }, [selectedForSync]);
  const [syncStatus, setSyncStatus] = useState<SyncStatus>({ state: "idle", message: "Ready" });
  const [showSyncPanel, setShowSyncPanel] = useState(false);
  const [settingsFocus, setSettingsFocus] = useState<{
    kind: "profile" | "storage";
    id: string;
    requestId: number;
  } | null>(null);
  const [settingsCommand, setSettingsCommand] = useState<{
    type: "add-profile" | "add-storage";
    requestId: number;
  } | null>(null);
  const [removingProfileId, setRemovingProfileId] = useState<string | null>(null);
  const [sidebarWidth, setSidebarWidth] = useState(320);
  const [logHeight, setLogHeight] = useState(240);
  const [resizeMode, setResizeMode] = useState<"sidebar" | "log" | null>(null);

  const activeProfileLinks = useMemo(
    () => activeProfileId ? syncConfig.links.filter((link) => link.profile === activeProfileId) : [],
    [activeProfileId, syncConfig.links],
  );
  const activeStorageId = useMemo(() => {
    if (!activeProfileId || activeProfileLinks.length === 0) return null;
    const preferred = activeStorageByProfile[activeProfileId];
    return activeProfileLinks.some((link) => link.storage === preferred)
      ? preferred
      : activeProfileLinks[0].storage;
  }, [activeProfileId, activeProfileLinks, activeStorageByProfile]);

  useEffect(() => {
    if (activeProfileId && syncConfig.local_profiles.some((profile) => profile.id === activeProfileId)) return;
    const next = syncConfig.local_profiles.find((profile) => sources.some((source) => source.id === profile.id))
      ?? syncConfig.local_profiles[0];
    setActiveProfileId(next?.id ?? null);
    setSelectedFile(null);
  }, [activeProfileId, sources, syncConfig.local_profiles]);

  // The Files workspace always compares one local profile with one explicit
  // storage link. A request id prevents a slower previous selection from
  // overwriting a newly selected profile or storage.
  const refreshStatuses = useCallback(async (profileId: string | null, storageId: string | null) => {
    const requestId = ++statusRequestRef.current;
    const source = profileId ? sourcesRef.current.find((candidate) => candidate.id === profileId) : undefined;
    if (!source || !storageId) {
      setFileStatuses(new Map());
      setClouds([]);
      setStatusError(null);
      setStatusLoading(false);
      return;
    }

    const paths = collectAllSyncable(source.entries);
    setStatusLoading(true);
    setStatusError(null);
    try {
      const report = await invoke<FileStatusReport>("get_file_statuses", {
        profile: profileId,
        storage: storageId,
        paths,
      });
      if (statusRequestRef.current !== requestId) return;
      setFileStatuses(new Map(Object.entries(report.statuses)));
      setClouds(report.clouds);
    } catch (error) {
      if (statusRequestRef.current !== requestId) return;
      setFileStatuses(new Map());
      setClouds([]);
      setStatusError(String(error));
    } finally {
      if (statusRequestRef.current === requestId) setStatusLoading(false);
    }
  }, []);

  // Refetch the cloud head + manifest into the backend cache; statuses then
  // include cloud-side states. Fails quietly until configured and linked.
  const refreshCloudState = useCallback(async () => {
    try {
      await invoke("refresh_cloud_state");
    } catch {
      // unconfigured or unlinked — statuses degrade to local-vs-baseline
    }
  }, []);

  const loadFiles = useCallback(async () => {
    setLoading(true);
    setLoadError(null);
    try {
      const result = await invoke<ConfigSource[]>("list_config_dirs");
      const nextSelection = reconcileSyncSelection(
        sourcesRef.current,
        result,
        selectedForSyncRef.current,
      );
      sourcesRef.current = result;
      selectedForSyncRef.current = nextSelection;
      setSources(result);
      setSelectedForSync(nextSelection);
    } catch (e) {
      setLoadError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  const loadConfig = useCallback(async () => {
    try {
      const cfg = await invoke<SyncConfig>("get_sync_config");
      setSyncConfig(cfg);
    } catch {
      // no saved config yet
    }
  }, []);

  // Post-pull readiness (PLAN_PORTABLE_AGENT_SETUP_V2.md): the plugin plans
  // plus local file diagnostics in one read-only scan; drives the footer
  // Finish-setup badge. Advisory only — fail quietly (no config, no CLI).
  const [readiness, setReadiness] = useState<SetupReadiness | null>(null);
  const [showFinishSetup, setShowFinishSetup] = useState(false);
  // Session-only simulation switch: readiness treats every source project
  // path as foreign, so the mapping flow can be tried on one machine.
  const [forceRemap, setForceRemap] = useState(false);
  const refreshReadiness = useCallback(async (): Promise<SetupReadiness | null> => {
    try {
      const next = await invoke<SetupReadiness>("get_setup_readiness");
      setReadiness(next);
      return next;
    } catch {
      setReadiness(null);
      return null;
    }
  }, []);

  // Machine-local project-path mappings (~/.agent-sync/project-path-mappings.json):
  // created from Finish setup after a pull, managed in Settings.
  const [projectPathMappings, setProjectPathMappings] = useState<ProjectPathMapping[]>([]);
  const loadProjectPathMappings = useCallback(async () => {
    try {
      setProjectPathMappings(await invoke<ProjectPathMapping[]>("list_project_path_mappings"));
    } catch {
      setProjectPathMappings([]);
    }
  }, []);

  const toggleForceRemap = useCallback(async (enabled: boolean) => {
    try {
      setForceRemap(await invoke<boolean>("set_force_path_remap", { enabled }));
    } catch {
      // command missing (stale backend) — leave the toggle as-is
    }
    await refreshReadiness();
  }, [refreshReadiness]);

  useEffect(() => {
    (async () => {
      await refreshCloudState();
      await loadFiles();
      await loadConfig();
      // The env override can preset the switch — reflect it, don't assume off.
      try {
        setForceRemap(await invoke<boolean>("get_force_path_remap"));
      } catch {
        // stale backend without the command
      }
      await refreshReadiness();
      await loadProjectPathMappings();
    })();
  }, [loadFiles, loadConfig, refreshCloudState, refreshReadiness, loadProjectPathMappings]);

  useEffect(() => {
    void refreshStatuses(activeProfileId, activeStorageId);
  }, [activeProfileId, activeStorageId, refreshStatuses, sources]);

  // window.confirm is a silent no-op in Tauri v2's webview — always use
  // the dialog plugin's async confirm instead.
  const confirmLeaveEditor = useCallback(async () => {
    if (!editorDirty.current) return true;
    const ok = await confirm("Discard unsaved changes?", { title: "Unsaved changes" });
    if (ok) editorDirty.current = false;
    return ok;
  }, []);

  const handleFileSelect = useCallback(async (path: string) => {
    if (!(await confirmLeaveEditor())) return;
    setSelectedFile(path);
    setSettingsFocus(null);
    setShowSyncPanel(false);
  }, [confirmLeaveEditor]);

  const handleProfileSelect = useCallback(async (profileId: string) => {
    if (profileId === activeProfileId) {
      setShowSyncPanel(false);
      return;
    }
    if (!(await confirmLeaveEditor())) return;
    setActiveProfileId(profileId);
    setSelectedFile(null);
    setSettingsFocus(null);
    setShowSyncPanel(false);
  }, [activeProfileId, confirmLeaveEditor]);

  const handleStorageSelect = useCallback((storageId: string) => {
    if (!activeProfileId) return;
    setActiveStorageByProfile((current) => ({ ...current, [activeProfileId]: storageId }));
  }, [activeProfileId]);

  const handleEditorDirtyChange = useCallback((dirty: boolean) => {
    editorDirty.current = dirty;
  }, []);

  // A saved edit is a local change: refresh statuses so the tree badge and
  // push counter update immediately.
  const handleEditorSaved = useCallback(() => {
    void refreshStatuses(activeProfileId, activeStorageId);
  }, [activeProfileId, activeStorageId, refreshStatuses]);

  const handleToggleSync = useCallback((path: string) => {
    setSelectedForSync((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      selectedForSyncRef.current = next;
      return next;
    });
  }, []);

  const openProfileSettings = useCallback(async (profile: string) => {
    if (!(await confirmLeaveEditor())) return;
    setSettingsCommand(null);
    setSettingsFocus((prev) => ({
      kind: "profile",
      id: profile,
      requestId: (prev?.requestId ?? 0) + 1,
    }));
    setShowSyncPanel(true);
  }, [confirmLeaveEditor]);

  const openStorageSettings = useCallback(async (storage: string) => {
    if (!(await confirmLeaveEditor())) return;
    setSettingsCommand(null);
    setSettingsFocus((prev) => ({
      kind: "storage",
      id: storage,
      requestId: (prev?.requestId ?? 0) + 1,
    }));
    setShowSyncPanel(true);
  }, [confirmLeaveEditor]);

  const requestSettingsCommand = useCallback(async (type: "add-profile" | "add-storage") => {
    if (!(await confirmLeaveEditor())) return;
    setSettingsFocus(null);
    setSettingsCommand((prev) => ({ type, requestId: (prev?.requestId ?? 0) + 1 }));
    setShowSyncPanel(true);
  }, [confirmLeaveEditor]);

  const storageConfigured = useCallback((storageId: string) => {
    const s = syncConfig.storages.find((st) => st.id === storageId);
    if (!s) return false;
    return s.kind === "local"
      ? !!s.local_dir
      : !!s.bucket && !!s.access_key_id && !!s.secret_access_key && (!!s.account_id || !!s.s3_endpoint);
  }, [syncConfig]);

  /** The selected physical paths under one profile's source. */
  const pathsForProfile = useCallback((profileId: string, paths: Iterable<string>): string[] => {
    const src = sourcesRef.current.find((s) => s.id === profileId);
    if (!src) return [];
    return [...paths].filter((p) => p === src.path || p.startsWith(`${src.path}/`));
  }, []);

  // Files actions are scoped to the profile and storage shown in the header.
  // This avoids the previous footer behavior that silently ran every link.
  const handleFilesSync = useCallback(async (
    direction: "push" | "pull",
    storage: string,
    profile: string,
  ) => {
    const files = direction === "push"
      ? pathsForProfile(profile, selectedForSyncRef.current)
      : [];
    if (direction === "push" && files.length === 0) {
      setSyncStatus({ state: "error", message: "Select at least one file to push" });
      return;
    }

    const verb = direction === "push" ? "Push" : "Pull";
    appendLog({ level: "info", message: `${verb} started for ${profile}` });
    setSyncStatus(
      direction === "push"
        ? { state: "uploading", message: "Pushing to storage…" }
        : { state: "downloading", message: "Pulling from storage…" },
    );
    try {
      const result = direction === "push"
        ? await invoke<SyncResult>("sync_upload", { storage, profile, files })
        : await invoke<SyncResult>("sync_download", { storage, profile });
      appendLog({ level: "success", message: result.message });
      setSyncStatus({
        state: "success",
        message: result.message,
        lastSync: result.timestamp,
        filesSynced: result.files_synced,
      });
      await loadConfig();
      if (direction === "pull") {
        await loadFiles();
        await refreshReadiness();
      } else {
        await refreshCloudState();
      }
      await refreshStatuses(profile, storage);
    } catch (error) {
      const message = String(error);
      appendLog({ level: "error", message });
      setSyncStatus({ state: "error", message });
    } finally {
      setProgress(null);
    }
  }, [
    appendLog,
    loadConfig,
    loadFiles,
    pathsForProfile,
    refreshCloudState,
    refreshReadiness,
    refreshStatuses,
  ]);

  // Per-link sync from the settings matrix's selected-link panel.
  const handleLinkSync = useCallback(async (
    direction: "push" | "pull",
    storage: string,
    profile: string,
  ) => {
    setShowLog(true);
    setUnreadLogs(0);
    setSyncStatus(
      direction === "push"
        ? { state: "uploading", message: "Pushing to storage…" }
        : { state: "downloading", message: "Pulling from storage…" },
    );
    try {
      const src = sourcesRef.current.find((s) => s.id === profile);
      const result = direction === "push"
        ? await invoke<SyncResult>("sync_upload", {
            storage,
            profile,
            files: src ? [src.path] : [],
          })
        : await invoke<SyncResult>("sync_download", { storage, profile });
      appendLog({ level: "success", message: result.message });
      setSyncStatus({ state: "success", message: result.message, lastSync: result.timestamp, filesSynced: result.files_synced });
      await loadConfig();
      await loadFiles();
      if (direction === "pull") await refreshReadiness();
    } catch (e) {
      const message = String(e);
      appendLog({ level: "error", message });
      setSyncStatus({ state: "error", message });
    } finally {
      setProgress(null);
    }
  }, [appendLog, loadConfig, loadFiles, refreshReadiness]);

  const [repairing, setRepairing] = useState(false);
  const [settingUp, setSettingUp] = useState(false);
  // Bootstrap a link end-to-end: mkdir, pull, plugin repair into the mount.
  // The settings panel saves the config before calling this.
  const handleSetupLink = useCallback(async (storage: string, profile: string) => {
    setShowLog(true);
    setUnreadLogs(0);
    setSettingUp(true);
    const root = syncConfigRef.current.local_profiles.find((p) => p.id === profile)?.root ?? profile;
    setSyncStatus({ state: "downloading", message: `Setting up ${root}…` });
    try {
      const result = await invoke<SyncResult>("setup_link", { storage, profile });
      const setupStatus = root === ".codex" && result.setup_state
        ? codexRepairStatus(result.setup_state)
        : { state: "success" as const, message: result.message };
      setSyncStatus({ ...setupStatus, lastSync: result.timestamp });
      await loadConfig();
      await loadFiles();
      const nextReadiness = await refreshReadiness();
      const codexSetupIncomplete = result.setup_state === "partial" || result.setup_state === "failed";
      const codexFollowUp = nextReadiness?.issues.some(
        (issue) => issue.profile === profile
          && (issue.category === "plugins" || issue.action === "apply_sidebar_state"),
      );
      // Project paths need a human choice on any provider — surface them
      // right after setup instead of hiding behind the footer badge.
      const pathFollowUp = nextReadiness?.issues.some(
        (issue) => issue.profile === profile && issue.action === "attach_project",
      );
      if (pathFollowUp || (root === ".codex" && (codexSetupIncomplete || codexFollowUp))) {
        setShowFinishSetup(true);
      }
    } catch (e) {
      const message = String(e);
      appendLog({ level: "error", message });
      setSyncStatus({ state: "error", message });
    } finally {
      setSettingUp(false);
    }
  }, [appendLog, loadConfig, loadFiles, refreshReadiness]);
  // Reinstalls missing Claude plugins from the profile's synced lock, via
  // Claude Code's own CLI. Explicit click only — plugins execute arbitrary
  // code.
  const handleRepairPlugins = async (profile: string) => {
    setShowLog(true);
    setUnreadLogs(0);
    setRepairing(true);
    appendLog({ level: "info", message: "Plugin repair started" });
    setSyncStatus({ state: "downloading", message: "Repairing Claude plugins…" });
    try {
      const report = await invoke<PluginRepairReport>("repair_plugins", { profile });
      const message = `Plugin repair — ${report.marketplaces_added.length + report.plugins_installed.length} installed, ${report.already_present.length} present, ${report.failed.length} failed`;
      setSyncStatus({ state: report.failed.length > 0 ? "error" : "success", message });
    } catch (e) {
      const message = String(e);
      appendLog({ level: "error", message });
      setSyncStatus({ state: "error", message });
    } finally {
      setRepairing(false);
      await refreshReadiness();
    }
  };
  // Reinstalls missing Codex plugins from the profile's synced lock
  // (agent-sync/codex-plugins.lock.json), via Codex's own CLI.
  // Explicit click only — plugins execute arbitrary code.
  const [codexRepairing, setCodexRepairing] = useState(false);
  const handleRepairCodexPlugins = async (profile: string) => {
    setShowLog(true);
    setUnreadLogs(0);
    setCodexRepairing(true);
    appendLog({ level: "info", message: "Codex plugin repair started" });
    setSyncStatus({ state: "downloading", message: "Installing Codex plugins…" });
    try {
      const report = await invoke<CodexPluginRepairReport>("repair_codex_plugins", { profile });
      setSyncStatus(codexRepairStatus(report.state));
    } catch (e) {
      const message = String(e);
      appendLog({ level: "error", message });
      setSyncStatus({ state: "error", message });
    } finally {
      setCodexRepairing(false);
      await refreshReadiness();
    }
  };

  // Additively merges the synced sidebar lock into the profile's Codex
  // desktop state. Explicit click only; the backend refuses while the
  // desktop app is running.
  const handleApplySidebar = async (profile: string) => {
    setShowLog(true);
    setUnreadLogs(0);
    try {
      const summary = await invoke<string>("apply_sidebar_state", { profile });
      setSyncStatus({ state: "success", message: `Sidebar — ${summary}` });
    } catch (e) {
      const message = String(e);
      appendLog({ level: "error", message });
      setSyncStatus({ state: "error", message });
    }
    await refreshReadiness();
  };

  const handleSaveSyncConfig = useCallback(async (cfg: SyncConfig) => {
    await invoke("save_sync_config", { config: cfg });
    // Re-read the canonical config: the backend carries over resolved cloud
    // links and probed capabilities per storage identity.
    await loadConfig();
    await refreshCloudState();
    await loadFiles();
    await refreshReadiness();
  }, [loadConfig, loadFiles, refreshCloudState, refreshReadiness]);

  const handleRemoveProfile = useCallback(async (profileId: string) => {
    const profile = syncConfig.local_profiles.find((candidate) => candidate.id === profileId);
    if (!profile) return;

    const source = sources.find((candidate) => candidate.id === profileId);
    const selectedFromProfile = !!source && !!selectedFile && (
      selectedFile === source.path || selectedFile.startsWith(`${source.path}/`)
    );
    if (selectedFromProfile && !(await confirmLeaveEditor())) return;

    const confirmed = await confirm(
      `Remove profile "${profileLabel(profile)}" from the dashboard?\n\n`
      + "Its folder and all files remain on this Mac. Only the dashboard profile, its storage links, and local sync bookkeeping are removed.",
      { title: "Remove profile" },
    );
    if (!confirmed) return;

    setRemovingProfileId(profileId);
    try {
      await handleSaveSyncConfig({
        ...syncConfig,
        local_profiles: syncConfig.local_profiles.filter((candidate) => candidate.id !== profileId),
        links: syncConfig.links.filter((link) => link.profile !== profileId),
      });
      if (selectedFromProfile) setSelectedFile(null);
      if (settingsFocus?.kind === "profile" && settingsFocus.id === profileId) {
        setSettingsFocus(null);
      }
      setSyncStatus({ state: "success", message: "Profile removed" });
    } catch (e) {
      const message = String(e);
      appendLog({ level: "error", message });
      setSyncStatus({ state: "error", message });
    } finally {
      setRemovingProfileId(null);
    }
  }, [
    appendLog,
    confirmLeaveEditor,
    handleSaveSyncConfig,
    selectedFile,
    settingsFocus,
    sources,
    syncConfig,
  ]);

  // Explicit local bookkeeping (D9): both actions write only
  // ~/.agent-sync/local-state.json on this machine; nothing syncs.
  const handleMarkReviewed = async (issue: SetupIssue) => {
    try {
      await invoke("mark_hook_reviewed", { id: issue.id });
    } catch (e) {
      appendLog({ level: "error", message: String(e) });
    }
    await refreshReadiness();
  };
  const handleDismissIssue = async (issue: SetupIssue) => {
    try {
      await invoke("dismiss_setup_issue", { id: issue.id });
    } catch (e) {
      appendLog({ level: "error", message: String(e) });
    }
    await refreshReadiness();
  };
  const handleResolveConflict = async (issue: SetupIssue) => {
    if (!issue.source_path) return;
    setSyncStatus({ state: "downloading", message: "Resolving conflict copy…" });
    try {
      const message = await invoke<string>("resolve_conflict_copy", { sourcePath: issue.source_path });
      setSyncStatus({ state: "success", message, preserveMessage: true });
      await refreshCloudState();
      await loadFiles();
    } catch (e) {
      const message = String(e);
      appendLog({ level: "error", message });
      setSyncStatus({ state: "error", message });
    }
    await refreshReadiness();
  };

  // Save a source → target project-path mapping (machine-local, D2) and let
  // the backend apply the mapped sidebar when the desktop app is closed.
  const handleMapProjectPath = async (issue: SetupIssue, targetPath: string): Promise<ProjectPathApplyReport> => {
    const candidate = issue.project_path;
    if (!candidate) throw new Error("not a project-path issue");
    const report = await invoke<ProjectPathApplyReport>("map_project_path", {
      profile: issue.profile,
      provider: candidate.provider,
      sourceKey: candidate.source_key,
      targetPath,
    });
    await refreshReadiness();
    await loadProjectPathMappings();
    return report;
  };

  // Settings: re-pick the folder for an existing mapping. Same backend
  // command — the mapping record is replaced, never duplicated.
  const handleChangeProjectPath = async (mapping: ProjectPathMapping) => {
    const picked = await open({ directory: true, multiple: false });
    if (typeof picked !== "string" || !picked) return;
    try {
      const report = await invoke<ProjectPathApplyReport>("map_project_path", {
        profile: mapping.profile,
        provider: mapping.provider,
        sourceKey: mapping.source_key,
        targetPath: picked,
      });
      if (report.provider === "codex" && report.sidebar_pending) {
        appendLog({ level: "info", message: "Mapping saved — quit ChatGPT/Codex, then apply the sidebar from Finish setup" });
      }
    } catch (e) {
      appendLog({ level: "error", message: String(e) });
    }
    await refreshReadiness();
    await loadProjectPathMappings();
  };

  const handleRemoveProjectPath = async (mapping: ProjectPathMapping) => {
    const confirmed = await confirm(
      `Remove the mapping ${mapping.source_path} → ${mapping.target_path}?\n\n`
      + "No project folder, task, or sidebar entry is deleted — only this Mac's saved path mapping.",
      { title: "Remove mapping" },
    );
    if (!confirmed) return;
    try {
      await invoke("remove_project_path_mapping", {
        profile: mapping.profile,
        provider: mapping.provider,
        sourcePath: mapping.source_path,
      });
    } catch (e) {
      appendLog({ level: "error", message: String(e) });
    }
    await refreshReadiness();
    await loadProjectPathMappings();
  };

  const startSidebarResize = useCallback((event: ReactMouseEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.preventDefault();

    const startX = event.clientX;
    const startWidth = sidebarWidth;
    const maxWidth = Math.max(
      MIN_SIDEBAR_WIDTH,
      Math.min(MAX_SIDEBAR_WIDTH, window.innerWidth - 360),
    );
    setResizeMode("sidebar");

    const handleMouseMove = (moveEvent: MouseEvent) => {
      setSidebarWidth(clamp(startWidth + moveEvent.clientX - startX, MIN_SIDEBAR_WIDTH, maxWidth));
    };
    const handleMouseUp = () => {
      setResizeMode(null);
      document.removeEventListener("mousemove", handleMouseMove);
      document.removeEventListener("mouseup", handleMouseUp);
    };

    document.addEventListener("mousemove", handleMouseMove);
    document.addEventListener("mouseup", handleMouseUp);
  }, [sidebarWidth]);

  const startLogResize = useCallback((event: ReactMouseEvent<HTMLDivElement>) => {
    if (event.button !== 0 || !showLog) return;
    event.preventDefault();

    const startY = event.clientY;
    const startHeight = logHeight;
    const maxHeight = Math.max(
      MIN_LOG_HEIGHT,
      Math.min(MAX_LOG_HEIGHT, window.innerHeight - 160),
    );
    setResizeMode("log");

    const handleMouseMove = (moveEvent: MouseEvent) => {
      setLogHeight(clamp(startHeight + startY - moveEvent.clientY, MIN_LOG_HEIGHT, maxHeight));
    };
    const handleMouseUp = () => {
      setResizeMode(null);
      document.removeEventListener("mousemove", handleMouseMove);
      document.removeEventListener("mouseup", handleMouseUp);
    };

    document.addEventListener("mousemove", handleMouseMove);
    document.addEventListener("mouseup", handleMouseUp);
  }, [logHeight, showLog]);

  const busy = syncStatus.state === "uploading" || syncStatus.state === "paused" || syncStatus.state === "downloading";
  const profileStats = useMemo(
    () => Object.fromEntries(
      sources.map((source) => [
        source.id,
        { fileCount: countSyncableFiles(source.entries), path: source.path },
      ]),
    ),
    [sources],
  );
  const activeProfile = syncConfig.local_profiles.find((profile) => profile.id === activeProfileId);
  const activeSource = sources.find((source) => source.id === activeProfileId);
  const activeStorageReady = activeStorageId ? storageConfigured(activeStorageId) : false;

  const handleFilesRefresh = () => {
    void (async () => {
      await refreshCloudState();
      await loadFiles();
      await refreshStatuses(activeProfileId, activeStorageId);
    })();
  };

  return (
    <div className={`app${IS_MACOS ? " macos-titlebar-overlay" : ""}${resizeMode ? ` resizing-${resizeMode}` : ""}`}>
      <div className="app-body">
        {/* ── Sidebar ── */}
        <aside className="app-sidebar" style={{ width: sidebarWidth }}>
          <div className="sidebar-chrome">
            <div className="sidebar-titlebar-drag-region" data-tauri-drag-region />
            <div className="sidebar-title-row" data-tauri-drag-region>
              <img className="sidebar-product-logo" src="/mallard-logo.png" alt="" data-tauri-drag-region />
              <span className="sidebar-product-title">Mallard</span>
            </div>

            <div className="sidebar-nav">
              <button
                className="sidebar-nav-item"
                type="button"
                onClick={onOpenProjects}
              >
                <Icon name="folder" size={15} />
                <span>Projects</span>
              </button>
              <button
                className={`sidebar-nav-item${!showSyncPanel ? " active" : ""}`}
                type="button"
                onClick={() => setShowSyncPanel(false)}
              >
                <Icon name="folder" size={15} />
                <span>Files</span>
              </button>
              <button
                className={`sidebar-nav-item${showLog ? " active" : ""}`}
                type="button"
                onClick={() => { setShowLog((v) => !v); setUnreadLogs(0); }}
              >
                <Icon name="activity" size={15} />
                <span>Activity</span>
                {unreadLogs > 0 && !showLog && <span className="sidebar-nav-badge">{unreadLogs}</span>}
              </button>
              <button
                className={`sidebar-nav-item${showSyncPanel ? " active" : ""}`}
                type="button"
                onClick={async () => {
                  if (!(await confirmLeaveEditor())) return;
                  setSettingsFocus(null);
                  setSettingsCommand(null);
                  setShowSyncPanel(true);
                }}
              >
                <Icon name="settings" size={15} />
                <span>Settings</span>
              </button>
            </div>
          </div>
          <div className="sidebar-settings-sections sidebar-unified-sections">
            <div className="sidebar-section-heading">
              <div className="sidebar-section-label">
                <span className="sidebar-section-title">Profiles</span>
                <span className="sidebar-section-count">{syncConfig.local_profiles.length}</span>
              </div>
              <div className="sidebar-heading-actions">
                <button
                  type="button"
                  className="sidebar-section-action"
                  onClick={loadFiles}
                  disabled={loading}
                  title="Refresh profiles"
                  aria-label="Refresh profiles"
                >
                  <Icon name="refresh" size={15} className={loading ? "icon-spin" : undefined} />
                </button>
                <button
                  type="button"
                  className="sidebar-section-action sidebar-add-action"
                  onClick={() => requestSettingsCommand("add-profile")}
                  title="Add profile"
                  aria-label="Add profile"
                >
                  <Icon name="plus" size={16} />
                </button>
              </div>
            </div>

            <div className="sidebar-profile-list">
              {loading && sources.length === 0 ? (
                <div className="sidebar-msg">Loading profiles…</div>
              ) : loadError ? (
                <div className="sidebar-msg error">{loadError}</div>
              ) : (
                syncConfig.local_profiles.map((profile) => {
                  const source = sources.find((candidate) => candidate.id === profile.id);
                  const label = profileLabel(profile);
                  return (
                    <div
                      key={profile.id}
                      className={`sidebar-profile-item${activeProfileId === profile.id ? " active" : ""}`}
                    >
                      <button
                        type="button"
                        className="sidebar-profile-main"
                        onClick={() => void handleProfileSelect(profile.id)}
                        title={source?.path ?? profile.path ?? label}
                      >
                        <Icon name="computer" size={15} />
                        <span>{label}</span>
                      </button>
                      <div className="sidebar-profile-actions">
                        <button
                          type="button"
                          onClick={() => void openProfileSettings(profile.id)}
                          title={`Profile settings for ${label}`}
                          aria-label={`Profile settings for ${label}`}
                        >
                          <Icon name="settings" size={13} />
                        </button>
                        <button
                          type="button"
                          className="sidebar-profile-remove"
                          onClick={() => void handleRemoveProfile(profile.id)}
                          disabled={removingProfileId === profile.id}
                          title={`Remove ${label} from dashboard; files stay on disk`}
                          aria-label={`Remove ${label} from dashboard; files stay on disk`}
                        >
                          <Icon name="trash" size={13} />
                        </button>
                      </div>
                    </div>
                  );
                })
              )}
            </div>

            {showSyncPanel && (
              <>
                <div className="sidebar-section-divider" />

                <div className="sidebar-section-heading">
                  <div className="sidebar-section-label">
                    <span className="sidebar-section-title">Storage</span>
                    <span className="sidebar-section-count">{syncConfig.storages.length}</span>
                  </div>
                  <button
                    type="button"
                    className="sidebar-section-action sidebar-add-action"
                    onClick={() => requestSettingsCommand("add-storage")}
                    title="Add storage"
                    aria-label="Add storage"
                  >
                    <Icon name="plus" size={16} />
                  </button>
                </div>
                <div className="sidebar-links-list">
                  {syncConfig.storages.map((storage) => (
                    <button
                      key={storage.id}
                      type="button"
                      className={`sidebar-link-item${settingsFocus?.kind === "storage" && settingsFocus.id === storage.id ? " active" : ""}`}
                      onClick={() => openStorageSettings(storage.id)}
                      title={`Edit ${storage.name || "storage"}`}
                    >
                      <Icon name={storage.kind === "local" ? "drive" : "cloud"} size={16} />
                      <span>{storage.name || "(unnamed)"}</span>
                    </button>
                  ))}
                </div>
              </>
            )}
          </div>
          {/* Local profile footer is intentionally hidden for now.
          <div className="sidebar-footer">
            <span className="sidebar-profile-icon" aria-hidden="true">
              <Icon name="computer" size={13} />
            </span>
            <span className="sidebar-profile-label">Local profile</span>
            <button
              type="button"
              className="sidebar-footer-action"
              onClick={async () => {
                if (!(await confirmLeaveEditor())) return;
                setSettingsFocus(null);
                setShowSyncPanel(true);
              }}
              title="Open profile settings"
              aria-label="Open profile settings"
            >
              <Icon name="settings" size={14} />
            </button>
          </div>
          */}
        </aside>
        <div
          className="sidebar-resizer"
          role="separator"
          aria-label="Resize sidebar"
          aria-orientation="vertical"
          onMouseDown={startSidebarResize}
          onDoubleClick={() => setSidebarWidth(320)}
        />

        <div className="app-workspace">
          <div className="workspace-titlebar-drag-region" data-tauri-drag-region aria-hidden="true" />
          {/* ── Main ── */}
          <main className="app-main">
            {showSyncPanel ? (
              <SyncPanel
                config={syncConfig}
                theme={theme}
                onThemeChange={onThemeChange}
                profileStats={profileStats}
                onRefresh={handleFilesRefresh}
                onSave={handleSaveSyncConfig}
                focusProfile={settingsFocus?.kind === "profile" ? settingsFocus.id : null}
                focusStorage={settingsFocus?.kind === "storage" ? settingsFocus.id : null}
                focusRequestId={settingsFocus?.requestId ?? 0}
                command={settingsCommand}
                onSyncLink={handleLinkSync}
                onSetupLink={handleSetupLink}
                onRepairProfile={(profile) =>
                  profile.root === ".codex"
                    ? handleRepairCodexPlugins(profile.id)
                    : handleRepairPlugins(profile.id)
                }
                projectPathMappings={projectPathMappings}
                onChangeProjectPath={handleChangeProjectPath}
                onRemoveProjectPath={handleRemoveProjectPath}
                busy={busy}
                setupBusy={settingUp}
                onClose={() => {
                  setSettingsFocus(null);
                  setShowSyncPanel(false);
                }}
              />
            ) : (
              <FilesWorkspace
                profile={activeProfile}
                source={activeSource}
                links={activeProfileLinks}
                storages={syncConfig.storages}
                activeStorageId={activeStorageId}
                onStorageChange={handleStorageSelect}
                selectedFile={selectedFile}
                selectedForSync={selectedForSync}
                statusMap={fileStatuses}
                clouds={clouds}
                theme={theme}
                loading={loading}
                statusLoading={statusLoading}
                loadError={loadError}
                statusError={statusError}
                storageReady={activeStorageReady}
                syncStatus={syncStatus}
                progress={progress}
                busy={busy}
                setupBusy={settingUp}
                repairBusy={repairing || codexRepairing}
                onFileSelect={(path) => void handleFileSelect(path)}
                onToggleSync={handleToggleSync}
                onRefresh={handleFilesRefresh}
                onOpenProfileSettings={() => activeProfile && void openProfileSettings(activeProfile.id)}
                onOpenStorageSettings={() => activeStorageId && void openStorageSettings(activeStorageId)}
                onAddProfile={() => void requestSettingsCommand("add-profile")}
                onLinkStorage={() => activeProfile && void openProfileSettings(activeProfile.id)}
                onPull={() => {
                  if (activeStorageId && activeProfileId) void handleFilesSync("pull", activeStorageId, activeProfileId);
                }}
                onPush={() => {
                  if (activeStorageId && activeProfileId) void handleFilesSync("push", activeStorageId, activeProfileId);
                }}
                onSetup={() => {
                  if (activeStorageId && activeProfileId) void handleSetupLink(activeStorageId, activeProfileId);
                }}
                onRepair={() => {
                  if (!activeProfile) return;
                  if (activeProfile.root === ".codex") void handleRepairCodexPlugins(activeProfile.id);
                  else void handleRepairPlugins(activeProfile.id);
                }}
                onSaved={handleEditorSaved}
                onDirtyChange={handleEditorDirtyChange}
              />
            )}
          </main>

          {/* ── Log drawer ── */}
          <div className={`log-drawer${showLog ? " open" : ""}`} style={{ height: showLog ? logHeight : undefined }}>
            {showLog && (
              <div
                className="log-resizer"
                role="separator"
                aria-label="Resize activity log"
                aria-orientation="horizontal"
                onMouseDown={startLogResize}
                onDoubleClick={() => setLogHeight(240)}
              />
            )}
            <LogPanel
              lines={logLines}
              onClear={() => setLogLines([])}
              onClose={() => setShowLog(false)}
            />
          </div>

          {/* ── Footer ── */}
          {showFinishSetup && readiness && (
            <FinishSetup
              readiness={readiness}
              busy={busy || repairing || codexRepairing}
              onRepair={(action, profile) =>
                action === "repair_codex_plugins"
                  ? handleRepairCodexPlugins(profile)
                  : action === "apply_sidebar_state"
                    ? handleApplySidebar(profile)
                    : handleRepairPlugins(profile)
              }
              onMarkReviewed={handleMarkReviewed}
              onResolveConflict={handleResolveConflict}
              onDismiss={handleDismissIssue}
              onMapProjectPath={handleMapProjectPath}
              forceRemap={forceRemap}
              onToggleForceRemap={(enabled) => void toggleForceRemap(enabled)}
              onClose={() => setShowFinishSetup(false)}
            />
          )}
          {/* The global file footer is intentionally removed. Sync actions
              live in the active profile and storage workspace above. */}
        </div>
      </div>
    </div>
  );
}

export default function App() {
  const [theme, setTheme] = useState<AppTheme>(getStoredTheme);
  const [mode, setMode] = useState<"projects" | "legacy">("projects");

  useEffect(() => applyTheme(theme), [theme]);

  return mode === "projects" ? (
    <ProjectSyncV3
      theme={theme}
      onThemeChange={setTheme}
      onOpenLegacy={() => setMode("legacy")}
    />
  ) : (
    <LegacyApp
      theme={theme}
      onThemeChange={setTheme}
      onOpenProjects={() => setMode("projects")}
    />
  );
}
