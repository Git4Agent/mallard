import { invoke } from "@tauri-apps/api/core";
import type {
  BundlePage,
  BundleReadiness,
  BundleRecipe,
  BundleSnapshotSummary,
  CodexConversationPathAudit,
  CodexConversationPathRepairResult,
  ConnectProjectBundleRequest,
  CodexThreadDetailsPage,
  CreateSetupDraftResult,
  DependencyPlan,
  DependencyResult,
  LocalProjectRegistration,
  ProjectBinding,
  ProjectChatHistory,
  ProjectDiscovery,
  ProjectDetail,
  ProjectOperationResult,
  ProjectProvider,
  ProjectSetupDraft,
  ProjectStorageLink,
  ProviderProfile,
  ProviderProfileProbe,
  ProviderProfileSummary,
  RegisterLocalProjectRequest,
  ResourceInventory,
  ResourceStatusReport,
  RestorePlan,
  RestoreResult,
  SaveProjectBindingRequest,
  SaveProjectLinkRequest,
  SetupDraftInspection,
  SetupDraftList,
  SyncConfigV3,
} from "../../types";

/**
 * Schema-3 Tauri contract. Multi-word Rust parameters use Tauri's camel-case
 * JavaScript names, matching the existing command surface in App.tsx.
 */
export const projectSyncApi = {
  discoverProject: (path: string, profileIds: Partial<Record<ProjectProvider, string>>) =>
    invoke<ProjectDiscovery>("discover_project", { path, profileIds }),

  listProviderProfiles: () =>
    invoke<ProviderProfileSummary[]>("list_provider_profiles"),

  probeProviderProfile: (provider: ProjectProvider, path: string) =>
    invoke<ProviderProfileProbe>("probe_provider_profile", { provider, path }),

  createProviderProfile: (provider: ProjectProvider, displayName: string, path: string) =>
    invoke<ProviderProfile>("create_provider_profile", { provider, displayName, path }),

  renameProviderProfile: (profileId: string, displayName: string, expectedRevision: number) =>
    invoke<ProviderProfile>("rename_provider_profile", { profileId, displayName, expectedRevision }),

  removeProviderProfile: (profileId: string, expectedRevision: number) =>
    invoke<boolean>("remove_provider_profile", { profileId, expectedRevision }),

  listProjects: () =>
    invoke<LocalProjectRegistration[]>("list_local_projects"),

  listProjectRepositoryKinds: () =>
    invoke<Record<string, boolean>>("list_project_repository_kinds"),

  getProject: (localProjectId: string) =>
    invoke<ProjectDetail | null>("get_project", { localProjectId }),

  registerProject: (request: RegisterLocalProjectRequest) =>
    invoke<LocalProjectRegistration>("register_local_project", { request }),

  removeProject: (localProjectId: string) =>
    invoke<boolean>("remove_local_project", { localProjectId }),

  renameProject: (localProjectId: string, localAlias: string | null, expectedRevision: number) =>
    invoke<LocalProjectRegistration>("rename_local_project", { localProjectId, localAlias, expectedRevision }),

  getConfig: () =>
    invoke<SyncConfigV3>("get_project_sync_config"),

  saveConfig: (config: SyncConfigV3) =>
    invoke<SyncConfigV3>("save_project_sync_config", { config }),

  listRemoteBundles: (storageId: string, cursor?: string | null) =>
    invoke<BundlePage>("list_remote_bundles", { storageId, cursor: cursor ?? null }),

  listRemoteBundleSnapshots: (storageId: string) =>
    invoke<BundleSnapshotSummary[]>("list_remote_bundle_snapshots", { storageId }),

  findRemoteBundleMatches: (storageId: string, repositoryFingerprint: string) =>
    invoke<BundleSnapshotSummary[]>("find_remote_bundle_matches", { storageId, repositoryFingerprint }),

  fetchBundle: (storageId: string, bundleId: string) =>
    invoke<BundleSnapshotSummary>("fetch_bundle", { storageId, bundleId }),

  saveLink: (request: SaveProjectLinkRequest) =>
    invoke<ProjectStorageLink>("save_project_link", { request }),

  connectProjectToRemoteBundle: (request: ConnectProjectBundleRequest) =>
    invoke<ProjectDetail>("connect_project_to_remote_bundle", { request }),

  removeLink: (localProjectId: string, storageId: string) =>
    invoke<boolean>("remove_project_link", { localProjectId, storageId }),

  getInventory: (localProjectId: string) =>
    invoke<ResourceInventory>("get_bundle_inventory", { localProjectId }),

  saveRecipe: (localProjectId: string, recipe: BundleRecipe) =>
    invoke<LocalProjectRegistration>("save_bundle_recipe", { localProjectId, recipe }),

  getStatus: (localProjectId: string, storageId: string) =>
    invoke<ResourceStatusReport>("get_bundle_status", { localProjectId, storageId }),

  pushBundle: (localProjectId: string, storageId: string, recipe: BundleRecipe) =>
    invoke<ProjectOperationResult>("push_bundle", { localProjectId, storageId, recipe }),

  getBinding: (localProjectId: string) =>
    invoke<ProjectBinding | null>("get_project_binding", { localProjectId }),

  auditCodexConversationPaths: (localProjectId: string) =>
    invoke<CodexConversationPathAudit>("audit_codex_conversation_paths", { localProjectId }),

  repairCodexConversationPaths: (localProjectId: string) =>
    invoke<CodexConversationPathRepairResult>("repair_codex_conversation_paths", { localProjectId }),

  listBindings: () =>
    invoke<ProjectBinding[]>("list_project_bindings"),

  saveBinding: (request: SaveProjectBindingRequest) =>
    invoke<ProjectBinding>("save_project_binding", { request }),

  planRestore: (storageId: string, bundleId: string, binding: ProjectBinding) =>
    invoke<RestorePlan>("plan_bundle_restore", { storageId, bundleId, binding }),

  applyRestore: (planId: string, approvedActionIds: string[]) =>
    invoke<RestoreResult>("apply_bundle_restore", { planId, approvedActionIds }),

  planDependencies: (restorePlanId: string) =>
    invoke<DependencyPlan>("plan_dependencies", { restorePlanId }),

  applyDependencies: (planId: string, actionIds: string[]) =>
    invoke<DependencyResult>("apply_dependency_actions", { planId, actionIds }),

  getReadiness: (storageId: string, bundleId: string, binding: ProjectBinding) =>
    invoke<BundleReadiness>("get_bundle_readiness", { storageId, bundleId, binding }),

  getRestoreReadiness: (restorePlanId: string) =>
    invoke<BundleReadiness>("get_restore_readiness", { restorePlanId }),

  listSetupDrafts: () =>
    invoke<SetupDraftList>("list_setup_drafts"),

  createSetupDraft: (projectRoot: string) =>
    invoke<CreateSetupDraftResult>("create_setup_draft", { projectRoot }),

  getSetupDraft: (draftId: string) =>
    invoke<ProjectSetupDraft | null>("get_setup_draft", { draftId }),

  updateSetupDraft: (draft: ProjectSetupDraft) =>
    invoke<ProjectSetupDraft>("update_setup_draft", { draft }),

  discardSetupDraft: (draftId: string) =>
    invoke<boolean>("discard_setup_draft", { draftId }),

  inspectSetupDraft: (draftId: string) =>
    invoke<SetupDraftInspection>("inspect_setup_draft", { draftId }),

  finalizeProjectSetup: (draftId: string, expectedRevision: number) =>
    invoke<ProjectDetail>("finalize_project_setup", { draftId, expectedRevision }),

  getProjectChatHistory: (
    localProjectId: string,
    branch?: string | null,
    beforeTime?: number | null,
    windowDays = 30,
    forceRevalidate = false,
  ) => invoke<ProjectChatHistory>("get_project_chat_history", {
    localProjectId,
    branch: branch ?? null,
    beforeTime: beforeTime ?? null,
    windowDays,
    forceRevalidate,
  }),

  getProjectChatThreadDetails: (
    localProjectId: string,
    threadId: string,
    cursor?: number | null,
    limit = 50,
  ) => invoke<CodexThreadDetailsPage>("get_project_chat_thread_details", {
    localProjectId,
    threadId,
    cursor: cursor ?? null,
    limit,
  }),

  openCodexThreadInApp: (localProjectId: string, threadId: string) =>
    invoke<void>("open_codex_thread_in_app", { localProjectId, threadId }),

  openCodexThreadInTerminal: (localProjectId: string, threadId: string) =>
    invoke<void>("open_codex_thread_in_terminal", { localProjectId, threadId }),

  validateCodexThreadOwnership: (localProjectId: string, threadId: string) =>
    invoke<void>("validate_codex_thread_ownership", { localProjectId, threadId }),
};
