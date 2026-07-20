export type ConfigKind = "local" | "cloud";
export type AppTheme = "light" | "dark";

export interface ConfigSource {
  id: string;
  label: string;
  path: string;
  kind: ConfigKind;
  entries: FileEntry[];
}

export interface FileEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
  modified: number;
  children?: FileEntry[] | null;
  included: boolean;
}

// Full DESIGN2 state matrix, plus the degraded local-only labels
// ("new" | "modified") used before any cloud state has been fetched.
export type FileStatus =
  | "synced"
  | "new"
  | "modified"
  | "local-only"
  | "local-ahead"
  | "cloud-only"
  | "cloud-ahead"
  | "converged"
  | "conflict"
  | "local-deleted"
  | "cloud-deleted";

export interface ProfileLink {
  root?: string; // ".codex" | ".claude"
  profile_id?: string;
  profile_label?: string;
  actor_name?: string;
  machine_name?: string;
  /** Sync-link cloud side: the user chose this prefix explicitly. */
  pinned?: boolean;
}

/** One named sync destination (PLAN_MULTI_STORAGE.md). */
export interface StorageConfig {
  id: string;
  name: string;
  kind: string; // "s3" | "local"
  bucket?: string;
  access_key_id?: string;
  secret_access_key?: string;
  account_id?: string;
  s3_endpoint?: string;
  region?: string;
  /** Local folder mode: the directory that plays the bucket's role. */
  local_dir?: string;
  /** Per-storage opt-ins. */
  included_default_exclusions?: string[];
  supports_conditional_writes?: boolean | null;
}

/** One local agent root this machine syncs. Defaults: ids "codex"/"claude". */
export interface LocalProfile {
  id: string;
  root: string; // ".codex" | ".claude"
  /** "" = ~/{root}; else a mount with container semantics. */
  path?: string;
  /** Optional user-chosen display name; "" = derive from the path. */
  name?: string;
}

/** A matrix edge: profile × storage, plus its resolved cloud side. */
export interface SyncLink {
  profile: string; // LocalProfile.id
  storage: string; // StorageConfig.id
  cloud?: ProfileLink;
}

export interface SyncConfig {
  schema: number; // 2
  storages: StorageConfig[];
  local_profiles: LocalProfile[];
  links: SyncLink[];
}

/** One cloud profile discovered in a storage (backend `ProfileInfo`). */
export interface CloudProfileInfo {
  profile_id: string;
  root: string;
  label: string;
  files: number;
  generation: number;
  updated_at: number;
  last_actor_name: string;
  last_machine_name: string;
}

export interface CloudRootState {
  root: string;
  storage: string;
  /** Identity — match rows by this, never by the mutable label. */
  profile_id: string;
  profile_label: string;
  generation: number;
  fetched_at: number;
}

export interface FileDocument {
  content: string;
  sha256: string;
  editable: boolean;
  reason?: string | null;
}

export interface FileStatusReport {
  clouds: CloudRootState[];
  statuses: Record<string, string>;
}

export interface CloudState {
  storage: string;
  profile: string;
  root: string;
  profile_label: string;
  generation: number;
  commit_id: string;
  fetched_at: number;
  files: number;
}

export interface PluginRepairReport {
  marketplaces_added: string[];
  plugins_installed: string[];
  already_present: string[];
  failed: string[];
}

export type CodexPluginRestoreState = "ready" | "partial" | "failed";

export interface CodexRepairIssue {
  id: string;
  code: string;
  message: string;
}

export interface CodexPluginPlan {
  missing_marketplaces: string[];
  missing_managed_marketplaces: CodexRepairIssue[];
  missing_plugins: string[];
  blocked_plugins: CodexRepairIssue[];
  config_repairs: CodexRepairIssue[];
  present: string[];
  drift: string[];
  disabled: string[];
  manual: string[];
  warnings: string[];
  blocked: string | null;
}

export interface CodexPluginRepairReport {
  state: CodexPluginRestoreState;
  marketplaces_added: string[];
  managed_marketplaces_provisioned: string[];
  plugins_installed: string[];
  already_present: string[];
  failed: string[];
  blocked_plugins: CodexRepairIssue[];
  config_paths_repaired: string[];
  manual: string[];
  verified: boolean;
}

/** One source project awaiting an explicit local folder choice
 * (PLAN_CODEX_MANUAL_PROJECT_PATH_PICKING.md §4). */
export interface ProjectPathCandidate {
  provider: string; // "codex" | "claude"
  source_key: string;
  source_path: string;
  git_origin?: string | null;
  /** Saved mapping whose target directory is gone (stale, kept visible). */
  mapped_path?: string | null;
  affected_threads: string[];
}

/** One machine-local mapping record (`~/.agent-sync/project-path-mappings.json`). */
export interface ProjectPathMapping {
  profile: string;
  provider: string;
  source_key: string;
  source_path: string;
  target_path: string;
}

/** Provider-tagged result of `map_project_path`. */
export interface CodexProjectPathApplyReport {
  provider: "codex";
  source_path: string;
  target_path: string;
  affected_thread_ids: string[];
  sidebar_applied: boolean;
  sidebar_pending: boolean;
  resume_commands: string[];
}

export interface ClaudeProjectPathApplyReport {
  provider: "claude";
  source_key: string;
  source_path: string;
  target_path: string;
  affected_session_ids: string[];
  alias_path?: string | null;
  state: string;
}

export type ProjectPathApplyReport = CodexProjectPathApplyReport | ClaudeProjectPathApplyReport;

/** One post-pull readiness finding (PLAN_PORTABLE_AGENT_SETUP_V2.md §5). */
export interface SetupIssue {
  id: string;
  root: string;
  /** Local profile id the issue belongs to. */
  profile: string;
  category: string; // plugins | skills | mcp | hooks | agents | conflicts | paths | instructions
  severity: string; // warning | info
  title: string;
  detail: string;
  source_path?: string | null;
  action: string;
  /** Structured payload for `attach_project` issues with a folder picker. */
  project_path?: ProjectPathCandidate | null;
}

export interface RootReadiness {
  root: string;
  profile: string;
  issues: number;
}

export interface SetupReadiness {
  generated_at: number;
  roots: RootReadiness[];
  issues: SetupIssue[];
}

export interface SyncProgress {
  done: number;
  total: number;
}

export interface LogLine {
  level: "info" | "ok" | "error";
  message: string;
  ts: number; // epoch ms
}

export interface SyncResult {
  success: boolean;
  files_synced: number;
  message: string;
  timestamp: number;
  /** Present for Codex root setup when plugin restoration ran. */
  setup_state?: CodexPluginRestoreState | null;
}

export type SyncState = 'idle' | 'uploading' | 'paused' | 'downloading' | 'success' | 'partial' | 'error';

export interface SyncStatus {
  state: SyncState;
  message: string;
  lastSync?: number;
  filesSynced?: number;
  /** Keep a concise operation result instead of collapsing success to "Updated". */
  preserveMessage?: boolean;
}

// ---------------------------------------------------------------------------
// Project-scoped sync (schema 3)
// ---------------------------------------------------------------------------

export type ProjectProvider = "codex" | "claude";

export type ProjectResourceCategory =
  | "conversations"
  | "project_setup"
  | "skills"
  | "plugins"
  | "tools";

export type ProjectResourceState =
  | "synced"
  | "local_only"
  | "local_ahead"
  | "remote_only"
  | "remote_ahead"
  | "conflict"
  | "missing"
  | "blocked"
  | "needs_review"
  | "ready";

export type ProjectResourceKind =
  | "codex_conversation"
  | "claude_conversation"
  | "project_file"
  | "project_memory"
  | "agent"
  | "command"
  | "rule"
  | "prompt"
  | "project_skill"
  | "standalone_skill"
  | "plugin"
  | "mcp_server"
  | "hook"
  | "setting"
  | "requirement";

export type ProjectResourceScope = "project" | "provider_state" | "dependency" | "requirement";
export type ProjectApplyPolicy = "safe_file" | "merge" | "explicit_install" | "explicit_review" | "manual_only" | "never";

export interface ProjectResourceDescriptor {
  resource_id: string;
  kind: ProjectResourceKind;
  provider?: ProjectProvider | null;
  scope: ProjectResourceScope;
  display_name: string;
  provenance: Record<string, unknown>;
  apply_policy: ProjectApplyPolicy;
  relative_cwd?: string | null;
  codec_version: number;
  metadata: Record<string, string>;
  // Inventory DTO presentation hints; these are not manifest identity.
  category?: ProjectResourceCategory | string;
  description?: string | null;
  logical_paths?: string[];
  default_selected?: boolean;
  selected_by_default?: boolean;
  blocked_reason?: string | null;
  provided_by?: string | null;
  install_behavior?: string | null;
}

export interface RecipeEntry {
  resource_id: string;
  apply_policy: ProjectApplyPolicy;
  required: boolean;
}

export interface BundleRecipe {
  schema_version: 1;
  revision: number;
  entries: Record<string, RecipeEntry>;
}

export interface ResourceInventory {
  project: string;
  bundle_id?: string | null;
  resources?: ProjectResourceDescriptor[];
  candidates?: ProjectResourceDescriptor[];
  recipe: BundleRecipe;
  generated_at?: number;
  warnings?: string[];
}

export interface LocalProjectRegistration {
  local_project_id: string;
  bundle_id: string;
  display_name: string;
  /** Machine-local nickname; never pushed, so it can differ per checkout. */
  local_alias?: string | null;
  repository_fingerprint?: string | null;
  recipe: BundleRecipe;
  recipe_bases: Record<string, {
    generation: number;
    manifest_sha256: string;
    recipe_revision: number;
    binding_revision?: number | null;
    last_pull_at?: number | null;
    last_push_at?: number | null;
  }>;
  revision: number;
  created_at: number;
  updated_at: number;
}

export interface ProjectStorageLink {
  local_project_id: string;
  storage_id: string;
  bundle_id: string;
  /** Last resource selection explicitly published to this storage. */
  recipe?: BundleRecipe | null;
  pinned: boolean;
  created_at: number;
}

export interface ProjectBinding {
  replica_id: string;
  local_project_id: string;
  bundle_id: string;
  project_root: string;
  canonical_project_root: string;
  profile_ids: Partial<Record<ProjectProvider, string>>;
  state: "active" | "detached";
  revision: number;
  updated_at: number;
}

export interface CodexConversationPathIssue {
  thread_id: string;
  transcript_path: string;
  recorded_cwd: string;
  target_cwd: string;
}

export interface CodexConversationPathAudit {
  local_project_id: string;
  profile_id?: string | null;
  profile_path?: string | null;
  project_root: string;
  assigned_thread_count: number;
  matching_thread_count: number;
  issues: CodexConversationPathIssue[];
  blockers: string[];
  warnings: string[];
  ready: boolean;
  can_repair: boolean;
}

export interface CodexConversationPathRepairResult {
  audit: CodexConversationPathAudit;
  repaired_thread_ids: string[];
  backup_dir?: string | null;
}

export interface LocalProjectSummary {
  local_project_id: string;
  bundle_id: string;
  display_name: string;
  local_alias?: string | null;
  revision: number;
  repository_fingerprint?: string | null;
  project_root?: string | null;
  canonical_project_root?: string | null;
  profile_ids?: Partial<Record<ProjectProvider, string>>;
  profile_names?: string[];
  providers?: ProjectProvider[];
  resource_count?: number;
  selected_resource_count?: number;
  linked_storage_ids?: string[];
  readiness_state?: "ready" | "needs_setup" | "blocked" | string;
  is_git_repository?: boolean;
}

export interface ApplyReceipt {
  action_id: string;
  resource_id: string;
  action_type: string;
  logical_path?: string | null;
  source_sha256?: string | null;
  target_path?: string | null;
  target_sha256_after?: string | null;
  status: "applied" | "skipped" | "failed" | "blocked";
  applied_at: number;
  error?: string | null;
}

export interface MaterializationRecord {
  materialization_id: string;
  plan_id: string;
  replica_id: string;
  local_project_id: string;
  storage_id: string;
  bundle_id: string;
  generation: number;
  commit_id: string;
  manifest_sha256: string;
  binding_revision: number;
  status: "partial" | "complete" | "detached";
  applied_at: number;
  receipts: ApplyReceipt[];
}

/** Exact response from the schema-3 `get_project` command. */
export interface ProjectDetail {
  project: LocalProjectRegistration;
  links: ProjectStorageLink[];
  binding?: ProjectBinding | null;
  materializations: MaterializationRecord[];
}

export interface ProjectDiscovery {
  project_root: string;
  display_name: string;
  inventory: ResourceInventory;
  repository_fingerprint?: string | null;
  providers?: ProjectProvider[];
  profile_ids: Partial<Record<ProjectProvider, string>>;
  warnings?: string[];
}

export type DraftProfileSelection =
  | { kind: "existing"; profile_id: string }
  | { kind: "pending"; path: string; display_name: string };

export type DraftStorageSelection =
  | { kind: "existing"; storage_id: string }
  | { kind: "pending"; storage: StorageConfigV3 };

export type DraftRepositoryChoice =
  | { kind: "new" }
  | {
    kind: "existing";
    storage_id: string;
    bundle_id: string;
    display_name: string;
    repository_fingerprint?: string | null;
    mismatch_acknowledged: boolean;
  };

/** Machine-local, resumable project setup draft (schema-3 `project_drafts`). */
export interface ProjectSetupDraft {
  schema: number;
  draft_id: string;
  local_project_id: string;
  new_bundle_id: string;
  project_root: string;
  canonical_project_root: string;
  display_name: string;
  repository_fingerprint?: string | null;
  profiles: Partial<Record<ProjectProvider, DraftProfileSelection>>;
  storage?: DraftStorageSelection | null;
  repository: DraftRepositoryChoice;
  selected_resource_ids: string[];
  discovery_signature: string;
  revision: number;
  created_at: number;
  updated_at: number;
  last_error?: string | null;
}

export interface SetupDraftSummary {
  draft_id: string;
  display_name: string;
  project_root: string;
  updated_at: number;
  revision: number;
  status: "draft" | "attention" | string;
  last_error?: string | null;
}

export interface SetupDraftList {
  drafts: SetupDraftSummary[];
  warnings?: string[];
}

export interface CreateSetupDraftResult {
  draft: ProjectSetupDraft;
  resumed: boolean;
}

export interface SetupSectionStatus {
  section: "project" | "profiles" | "storage" | "repository" | "resources" | string;
  state: "ready" | "attention" | "blocked" | string;
  message?: string | null;
}

export interface SetupDraftInspection {
  draft: ProjectSetupDraft;
  sections: SetupSectionStatus[];
  inventory?: ResourceInventory | null;
  fresh_discovery_signature?: string | null;
  selection_stale: boolean;
  can_finalize: boolean;
  warnings?: string[];
}

export interface ProviderProfile {
  profile_id: string;
  provider: ProjectProvider;
  display_name: string;
  path: string;
  canonical_path: string;
  revision: number;
  created_at: number;
  updated_at: number;
}

export interface ProviderProfileSummary extends ProviderProfile {
  available: boolean;
  readable: boolean;
  writable: boolean;
  used_by_projects: string[];
  error?: string | null;
}

export interface ProviderProfileProbe {
  provider: ProjectProvider;
  requested_path: string;
  resolved_path: string;
  canonical_path: string;
  suggested_name: string;
  readable: boolean;
  writable: boolean;
  detected_child: boolean;
  existing_profile_id?: string | null;
}

export interface RegisterLocalProjectRequest {
  display_name: string;
  repository_fingerprint?: string | null;
  bundle_id?: string | null;
}

export interface SaveProjectLinkRequest {
  local_project_id: string;
  storage_id: string;
  pinned: boolean;
}

export interface ConnectProjectBundleRequest {
  local_project_id: string;
  storage_id: string;
  bundle_id: string;
  expected_bundle_id: string;
  pinned: boolean;
  allow_repository_mismatch?: boolean;
}

export interface SaveProjectBindingRequest {
  local_project_id: string;
  project_root: string;
  profile_ids: Partial<Record<ProjectProvider, string>>;
  expected_revision?: number | null;
}

export interface SyncConfigV3 {
  schema: 3;
  revision: number;
  storages: StorageConfigV3[];
  projects: LocalProjectRegistration[];
  links: ProjectStorageLink[];
}

export interface StorageConfigV3 {
  id: string;
  name: string;
  kind: "s3" | "local";
  bucket: string;
  access_key_id: string;
  secret_access_key: string;
  account_id: string;
  s3_endpoint: string;
  region: string;
  local_dir: string;
  included_default_exclusions: string[];
  supports_conditional_writes?: boolean | null;
}

export interface BundleResourceStatus {
  resource_id: string;
  state: ProjectResourceState | string;
  message?: string | null;
  local_digest?: string | null;
  remote_digest?: string | null;
}

export interface ResourceStatusReport {
  project: string;
  storage: string;
  bundle_id?: string | null;
  generation?: number;
  statuses: Record<string, ProjectResourceState | string> | BundleResourceStatus[];
  warnings?: string[];
}

export interface ProjectOperationResult {
  success: boolean;
  message: string;
  operation_id?: string;
  resources_changed?: number;
  generation?: number;
  results?: Array<{
    resource_id: string;
    state: string;
    message?: string;
  }>;
}

export interface RemoteBundleSummary {
  bundle_id: string;
  display_name: string;
  kind?: string;
  generation?: number;
  updated_at?: number;
  resource_count?: number;
  providers?: ProjectProvider[];
  repository_fingerprint?: string | null;
}

export interface BundlePage {
  bundles: RemoteBundleSummary[];
  next_cursor?: string | null;
}

export interface BundleSnapshotSummary extends RemoteBundleSummary {
  storage_id: string;
  resources?: ProjectResourceDescriptor[];
  recipe?: BundleRecipe;
  fetched_at?: number;
  warnings?: string[];
}

export type PlanActionRisk = "safe" | "review" | "executable" | "blocked" | string;

export interface PlannedAction {
  action_id: string;
  kind: string;
  title: string;
  detail?: string | null;
  category?: ProjectResourceCategory | string;
  provider?: ProjectProvider | null;
  resource_id?: string | null;
  target_path?: string | null;
  risk?: PlanActionRisk;
  default_approved?: boolean;
  requires_explicit_approval?: boolean;
  blocked_reason?: string | null;
  state?: string;
}

export type RestoreActionKind =
  | { kind: "write_file"; logical_path: string }
  | { kind: "merge_file"; logical_path: string }
  | { kind: "materialize_conversation"; provider: ProjectProvider; logical_path: string }
  | { kind: "install_standalone_skill"; provider: ProjectProvider; target_relative_path: string }
  | { kind: "install_custom_skill"; provider: ProjectProvider; skill_name: string }
  | { kind: "overwrite_custom_skill"; provider: ProjectProvider; skill_name: string }
  | { kind: "install_plugin"; provider: ProjectProvider; plugin_id: string }
  | { kind: "review_hook"; definition_sha256: string }
  | { kind: "review_mcp"; definition_sha256: string }
  | { kind: "apply_setting"; provider: ProjectProvider; semantic_key: string }
  | { kind: "manual"; message: string };

export interface RestoreAction {
  action_id: string;
  resource_id: string;
  kind: RestoreActionKind;
  target_path?: string | null;
  source_sha256?: string | null;
  expected_target_sha256?: string | null;
  requires_explicit_approval: boolean;
}

export interface RestorePlan {
  schema_version: 1;
  plan_id: string;
  storage_id: string;
  bundle_id: string;
  replica_id: string;
  generation: number;
  commit_id: string;
  manifest_sha256: string;
  binding_revision: number;
  created_at: number;
  expires_at: number;
  actions: RestoreAction[];
}

export interface RestoreResult {
  success: boolean;
  message: string;
  plan_id?: string;
  applied_action_ids?: string[];
  failed_actions?: Array<{ action_id: string; message: string }>;
}

export interface DependencyPlan {
  schema_version: 1;
  plan_id: string;
  storage_id: string;
  bundle_id: string;
  replica_id: string;
  generation: number;
  commit_id: string;
  manifest_sha256: string;
  binding_revision: number;
  created_at: number;
  expires_at: number;
  actions: DependencyAction[];
  blockers?: string[];
  warnings?: string[];
}

export interface DependencyAction {
  action_id: string;
  resource_id: string;
  kind: "install_codex_plugin" | "install_claude_plugin" | "install_standalone_skill" | "check_binary" | "check_environment" | "manual";
  display_name: string;
  provider?: ProjectProvider | null;
  argv: string[];
  requires_explicit_approval: boolean;
}

export interface DependencyResult {
  success: boolean;
  message: string;
  applied_action_ids?: string[];
  failed_actions?: Array<{ action_id: string; message: string }>;
}

export interface BundleReadinessIssue {
  issue_id: string;
  category: ProjectResourceCategory | string;
  title: string;
  detail?: string | null;
  severity?: "info" | "warning" | "error" | string;
  provider?: ProjectProvider | null;
  resource_id?: string | null;
  action?: string | null;
}

export interface BundleReadiness {
  bundle_id: string;
  state: "ready" | "needs_setup" | "blocked" | string;
  issues: BundleReadinessIssue[];
  generated_at?: number;
}

export interface CodexThreadSummary {
  thread_id: string;
  title: string;
  summary: string;
  started_at: number;
  ended_at: number;
  branch?: string | null;
  recorded_sha?: string | null;
  is_active?: boolean;
  user_round_count: number;
  agent_message_count: number;
  tool_call_count: number;
  total_tokens?: number | null;
  metrics_complete: boolean;
  commit_occurrence_count: number;
}

export interface CommitThreadReference {
  thread_id: string;
}

export type ChatTurnRole = "user" | "assistant";

export interface ChatTurnPreview {
  ordinal: number;
  role: ChatTurnRole;
  timestamp?: number | null;
  preview: string;
}

export interface CodexThreadDetailsPage {
  thread_id: string;
  turns: ChatTurnPreview[];
  next_cursor?: number | null;
}

export interface StorageSyncSummary {
  storage_id: string;
  storage_name: string;
  last_pull_at?: number | null;
  last_push_at?: number | null;
}

export interface GitCommitSummary {
  sha: string;
  short_sha: string;
  committed_at: number;
  subject: string;
  thread_refs: CommitThreadReference[];
}

export interface GitBranchSummary {
  name: string;
  is_current: boolean;
  available: boolean;
}

export interface GitHistoryPage {
  selected_branch: string;
  branches: GitBranchSummary[];
  commits: GitCommitSummary[];
  next_cursor?: string | null;
  unique_thread_count: number;
  reference_count: number;
}

export interface UnmappedThreadReference {
  thread_id: string;
  reason: string;
}

export interface ProjectChatHistory {
  project_id: string;
  codex_home: string;
  threads: CodexThreadSummary[];
  git?: GitHistoryPage | null;
  unmapped: UnmappedThreadReference[];
  warnings: string[];
  window_start: number;
  window_end: number;
  next_before?: number | null;
  storage_sync: StorageSyncSummary[];
}
