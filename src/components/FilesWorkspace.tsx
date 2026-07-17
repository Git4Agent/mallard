import { useEffect, useMemo, useState } from "react";
import type {
  AppTheme,
  CloudRootState,
  ConfigSource,
  FileEntry,
  LocalProfile,
  StorageConfig,
  SyncLink,
  SyncProgress,
  SyncStatus,
} from "../types";
import FilePreview from "./FilePreview";
import FileTree from "./FileTree";
import Icon from "./Icons";
import { profileLabel } from "./SyncPanel";

type FileView = "changes" | "all" | "local" | "remote" | "conflicts" | "selected";

const LOCAL_STATUSES = new Set(["new", "modified", "local-only", "local-ahead", "cloud-deleted"]);
const REMOTE_STATUSES = new Set(["cloud-only", "cloud-ahead", "local-deleted"]);
const CHANGED_STATUSES = new Set([...LOCAL_STATUSES, ...REMOTE_STATUSES, "conflict"]);

interface Props {
  profile?: LocalProfile;
  source?: ConfigSource;
  links: SyncLink[];
  storages: StorageConfig[];
  activeStorageId: string | null;
  onStorageChange: (storage: string) => void;
  selectedFile: string | null;
  selectedForSync: Set<string>;
  statusMap: Map<string, string>;
  clouds: CloudRootState[];
  theme: AppTheme;
  loading: boolean;
  statusLoading: boolean;
  loadError: string | null;
  statusError: string | null;
  storageReady: boolean;
  syncStatus: SyncStatus;
  progress: SyncProgress | null;
  busy: boolean;
  setupBusy: boolean;
  repairBusy: boolean;
  onFileSelect: (path: string) => void;
  onToggleSync: (path: string) => void;
  onRefresh: () => void;
  onOpenProfileSettings: () => void;
  onOpenStorageSettings: () => void;
  onAddProfile: () => void;
  onLinkStorage: () => void;
  onPull: () => void;
  onPush: () => void;
  onSetup: () => void;
  onRepair: () => void;
  onSaved: () => void;
  onDirtyChange: (dirty: boolean) => void;
}

function collectSyncable(entries: FileEntry[]): string[] {
  return entries.flatMap((entry) => {
    if (!entry.included) return [];
    if (!entry.is_dir) return [entry.path];
    if (entry.children == null || entry.children.length === 0) return [entry.path];
    return collectSyncable(entry.children);
  });
}

function pathSelected(path: string, selected: Set<string>): boolean {
  for (const candidate of selected) {
    if (path === candidate || path.startsWith(`${candidate}/`)) return true;
  }
  return false;
}

function matchesView(entry: FileEntry, view: FileView, statuses: Map<string, string>, selected: Set<string>): boolean {
  if (view === "all") return true;
  if (view === "selected") return pathSelected(entry.path, selected);
  const status = statuses.get(entry.path);
  if (!status) return false;
  if (view === "changes") return CHANGED_STATUSES.has(status);
  if (view === "local") return LOCAL_STATUSES.has(status);
  if (view === "remote") return REMOTE_STATUSES.has(status);
  return status === "conflict";
}

function filterEntries(
  entries: FileEntry[],
  query: string,
  view: FileView,
  statuses: Map<string, string>,
  selected: Set<string>,
): FileEntry[] {
  const normalized = query.trim().toLowerCase();

  return entries.flatMap((entry) => {
    const searchMatch = !normalized || entry.name.toLowerCase().includes(normalized) || entry.path.toLowerCase().includes(normalized);
    if (!entry.is_dir) {
      return searchMatch && matchesView(entry, view, statuses, selected) ? [entry] : [];
    }

    const children = entry.children
      ? filterEntries(entry.children, normalized, view, statuses, selected)
      : entry.children;
    if (children && children.length > 0) return [{ ...entry, children }];

    if (!searchMatch || !matchesView(entry, view, statuses, selected)) return [];
    return [{ ...entry, children: view === "all" ? entry.children : children }];
  });
}

function compactPath(path: string): string {
  return path.replace(/^\/Users\/[^/]+/, "~");
}

export default function FilesWorkspace({
  profile,
  source,
  links,
  storages,
  activeStorageId,
  onStorageChange,
  selectedFile,
  selectedForSync,
  statusMap,
  clouds,
  theme,
  loading,
  statusLoading,
  loadError,
  statusError,
  storageReady,
  syncStatus,
  progress,
  busy,
  setupBusy,
  repairBusy,
  onFileSelect,
  onToggleSync,
  onRefresh,
  onOpenProfileSettings,
  onOpenStorageSettings,
  onAddProfile,
  onLinkStorage,
  onPull,
  onPush,
  onSetup,
  onRepair,
  onSaved,
  onDirtyChange,
}: Props) {
  const [query, setQuery] = useState("");
  const [view, setView] = useState<FileView>("changes");

  useEffect(() => {
    setQuery("");
    setView("changes");
  }, [profile?.id]);

  const linkedStorages = useMemo(
    () => links.flatMap((link) => {
      const storage = storages.find((candidate) => candidate.id === link.storage);
      return storage ? [{ link, storage }] : [];
    }),
    [links, storages],
  );
  const activePair = linkedStorages.find(({ storage }) => storage.id === activeStorageId);
  const activeCloud = clouds.find((cloud) =>
    cloud.storage === activeStorageId
      && (!activePair?.link.cloud?.profile_id || cloud.profile_id === activePair.link.cloud.profile_id),
  );

  const allPaths = useMemo(() => collectSyncable(source?.entries ?? []), [source]);
  const activeSelection = useMemo(
    () => new Set(allPaths.filter((path) => pathSelected(path, selectedForSync))),
    [allPaths, selectedForSync],
  );

  const counts = useMemo(() => {
    let local = 0;
    let remote = 0;
    let conflicts = 0;
    let push = 0;
    let pull = 0;
    for (const [path, status] of statusMap) {
      if (LOCAL_STATUSES.has(status)) local += 1;
      if (REMOTE_STATUSES.has(status)) remote += 1;
      if (status === "conflict") conflicts += 1;
      if ((LOCAL_STATUSES.has(status) || status === "conflict") && pathSelected(path, activeSelection)) push += 1;
      if (REMOTE_STATUSES.has(status) || status === "conflict") pull += 1;
    }
    return { local, remote, conflicts, push, pull };
  }, [activeSelection, statusMap]);

  const filteredEntries = useMemo(
    () => filterEntries(source?.entries ?? [], query, view, statusMap, activeSelection),
    [activeSelection, query, source, statusMap, view],
  );
  const visiblePaths = useMemo(() => collectSyncable(filteredEntries), [filteredEntries]);
  const allVisibleSelected = visiblePaths.length > 0 && visiblePaths.every((path) => pathSelected(path, activeSelection));
  const isEmptyProfile = allPaths.length === 0;
  const hasLink = linkedStorages.length > 0;
  const canSync = !!activePair && storageReady;
  const statusLabel = syncStatus.state === "success" && !syncStatus.preserveMessage ? "Updated" : syncStatus.message;

  useEffect(() => {
    if (!hasLink) setView("all");
  }, [hasLink, profile?.id]);

  const setSummaryView = (next: Exclude<FileView, "all" | "changes">) => {
    setView((current) => current === next ? "changes" : next);
  };

  if (!profile) {
    return (
      <div className="files-workspace files-workspace-empty">
        <Icon name="computer" size={28} />
        <h1>No profiles</h1>
        <p>Add a profile to browse and sync its files.</p>
        <button type="button" className="btn" onClick={onAddProfile}>
          <Icon name="plus" size={15} /> Add profile
        </button>
      </div>
    );
  }

  return (
    <div className="files-workspace">
      <header className="files-workspace-header">
        <div className="files-context">
          <div className="files-profile-heading" title={source?.path ?? profile.path ?? profileLabel(profile)}>
            <span className="files-profile-icon"><Icon name="computer" size={18} /></span>
            <span className="files-profile-copy">
              <strong>{profileLabel(profile)}</strong>
              <span>{source?.path ? compactPath(source.path) : "Profile folder unavailable"}</span>
            </span>
          </div>
          <button
            type="button"
            className="files-icon-action"
            onClick={onOpenProfileSettings}
            title="Profile settings"
            aria-label="Profile settings"
          >
            <Icon name="settings" size={15} />
          </button>
        </div>

        <div className="files-destination">
          <span className="files-toolbar-label">Storage</span>
          {hasLink ? (
            <>
              <select
                className="files-storage-select"
                value={activeStorageId ?? ""}
                onChange={(event) => onStorageChange(event.target.value)}
                aria-label="Active storage"
              >
                {linkedStorages.map(({ link, storage }) => (
                  <option key={storage.id} value={storage.id}>
                    {storage.name || "(unnamed)"}{link.cloud?.profile_label ? ` · ${link.cloud.profile_label}` : ""}
                  </option>
                ))}
              </select>
              <button
                type="button"
                className="files-icon-action"
                onClick={onOpenStorageSettings}
                title="Storage settings"
                aria-label="Storage settings"
              >
                <Icon name="settings" size={15} />
              </button>
            </>
          ) : (
            <button type="button" className="btn" onClick={onLinkStorage}>
              <Icon name="link" size={14} /> Link storage
            </button>
          )}
        </div>

        <div className="files-workspace-actions">
          {hasLink && !storageReady ? (
            <button type="button" className="btn" onClick={onOpenStorageSettings}>
              Configure storage
            </button>
          ) : hasLink ? (
            <>
              {isEmptyProfile ? (
                <button type="button" className="btn" onClick={onSetup} disabled={!canSync || busy || setupBusy}>
                  <Icon name="download" size={14} /> {setupBusy ? "Setting up…" : "Set up"}
                </button>
              ) : (
                <button type="button" className="btn" onClick={onPull} disabled={!canSync || busy}>
                  <Icon name="download" size={14} /> Pull
                  {counts.pull > 0 && <span className="files-action-count">{counts.pull}</span>}
                </button>
              )}
              <button type="button" className="btn btn-primary" onClick={onPush} disabled={!canSync || busy || activeSelection.size === 0}>
                <Icon name="upload" size={14} /> Push
                {counts.push > 0 && <span className="files-action-count">{counts.push}</span>}
              </button>
              <button type="button" className="btn" onClick={onRepair} disabled={busy || repairBusy}>
                <Icon name="refresh" size={14} className={repairBusy ? "icon-spin" : undefined} /> Repair
              </button>
            </>
          ) : null}
        </div>
      </header>

      <div className="files-summary-bar">
        <div className="files-summary-filters" aria-label="File status filters">
          <button type="button" className={view === "local" ? "active" : undefined} onClick={() => setSummaryView("local")}>
            Local <span>{counts.local}</span>
          </button>
          <button type="button" className={view === "remote" ? "active" : undefined} onClick={() => setSummaryView("remote")}>
            Remote <span>{counts.remote}</span>
          </button>
          <button type="button" className={view === "conflicts" ? "active warning" : counts.conflicts > 0 ? "warning" : undefined} onClick={() => setSummaryView("conflicts")}>
            Conflicts <span>{counts.conflicts}</span>
          </button>
          <button type="button" className={view === "selected" ? "active" : undefined} onClick={() => setSummaryView("selected")}>
            Selected <span>{activeSelection.size}</span>
          </button>
        </div>
        <div className="files-link-meta">
          {activeCloud && <span>{activeCloud.profile_label} · gen {activeCloud.generation}</span>}
          {statusLoading && <span className="status-loader" aria-label="Refreshing status" />}
          <button type="button" className="files-refresh-action" onClick={onRefresh} disabled={loading || statusLoading} title="Refresh files and status" aria-label="Refresh files and status">
            <Icon name="refresh" size={14} className={loading || statusLoading ? "icon-spin" : undefined} />
          </button>
          <span className={`files-sync-state state-${syncStatus.state}`}>{statusLabel}</span>
          {progress && progress.total > 0 && <span>{progress.done}/{progress.total}</span>}
        </div>
      </div>

      {(loadError || statusError || (hasLink && !storageReady)) && (
        <div className="files-notice" role="status">
          {loadError || statusError || "This storage needs configuration before it can sync."}
        </div>
      )}

      <div className="files-workspace-body">
        <section className="files-browser-pane" aria-label="Files">
          <div className="files-browser-toolbar">
            <input
              type="search"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Search files"
              aria-label="Search files"
            />
            <div className="files-view-toggle" role="group" aria-label="File view">
              <button type="button" className={view !== "all" ? "active" : undefined} onClick={() => setView("changes")}>Changes</button>
              <button type="button" className={view === "all" ? "active" : undefined} onClick={() => setView("all")}>All</button>
            </div>
            <button
              type="button"
              className="files-select-visible"
              onClick={() => visiblePaths.forEach((path) => {
                if (allVisibleSelected === pathSelected(path, activeSelection)) onToggleSync(path);
              })}
              disabled={visiblePaths.length === 0}
            >
              {allVisibleSelected ? "None" : "Select"}
            </button>
          </div>

          <div className="files-browser-content">
            {loading && !source ? (
              <div className="files-pane-message">Loading files…</div>
            ) : !source ? (
              <div className="files-pane-message">
                <strong>Profile folder unavailable</strong>
                <span>Check this profile’s folder in Settings.</span>
                <button type="button" className="btn" onClick={onOpenProfileSettings}>Profile settings</button>
              </div>
            ) : statusLoading && view !== "all" && filteredEntries.length === 0 ? (
              <div className="files-pane-message">
                <span className="status-loader" aria-hidden="true" />
                <strong>Checking changes…</strong>
              </div>
            ) : filteredEntries.length === 0 ? (
              <div className="files-pane-message">
                <strong>{view === "changes" && !query ? "No changes" : "No matching files"}</strong>
                <span>{view === "changes" && !query ? "This profile is up to date for the selected storage." : "Try another search or file filter."}</span>
              </div>
            ) : (
              <FileTree
                entries={filteredEntries}
                label={profileLabel(profile)}
                fullPath={source.path}
                kind={source.kind}
                selectedFile={selectedFile}
                selectedForSync={activeSelection}
                onFileSelect={onFileSelect}
                onToggleSync={onToggleSync}
                statusMap={statusMap}
                forceOpen={!!query || view !== "all"}
                hideHeader
              />
            )}
          </div>
        </section>

        <section className="files-preview-pane" aria-label="File preview">
          {selectedFile ? (
            <FilePreview
              path={selectedFile}
              theme={theme}
              onSaved={onSaved}
              onDirtyChange={onDirtyChange}
            />
          ) : (
            <div className="files-preview-placeholder">
              <Icon name="file" size={24} />
              <strong>Select a file</strong>
              <span>Preview or edit it here.</span>
            </div>
          )}
        </section>
      </div>
    </div>
  );
}
