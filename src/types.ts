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
