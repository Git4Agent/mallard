import { useMemo, useState } from "react";
import type {
  BundleReadiness,
  LocalProjectRegistration,
  LocalProjectSummary,
  ProjectBinding,
  ProjectResourceDescriptor,
  ProjectStorageLink,
  StorageConfigV3,
} from "../../types";
import Icon from "../Icons";
import ResourceInventory from "./ResourceInventory";
import { compactProjectPath, providerLabel } from "./model";

interface Props {
  summary: LocalProjectSummary;
  project: LocalProjectRegistration | null;
  binding: ProjectBinding | null;
  resources: ProjectResourceDescriptor[];
  selected: Set<string>;
  statuses: Map<string, string>;
  storages: StorageConfigV3[];
  links: ProjectStorageLink[];
  activeStorageId: string | null;
  readiness: BundleReadiness | null;
  loading: boolean;
  busy: boolean;
  selectionDirty: boolean;
  error: string | null;
  onToggleResource: (resourceId: string) => void;
  onSaveRecipe: () => void;
  onStorageChange: (storageId: string) => void;
  onLinkStorage: (storageId: string) => void;
  onPush: () => void;
  onRestore: () => void;
  onRemap: () => void;
  onRefresh: () => void;
  onOpenStorageSettings: () => void;
}

function statusCounts(statuses: Map<string, string>) {
  let local = 0;
  let remote = 0;
  let conflict = 0;
  for (const status of statuses.values()) {
    if (status === "local_only" || status === "local_ahead" || status === "new" || status === "modified") local += 1;
    if (status === "remote_only" || status === "remote_ahead" || status === "cloud_only" || status === "cloud_ahead") remote += 1;
    if (status === "conflict") conflict += 1;
  }
  return { local, remote, conflict };
}

export default function ProjectWorkspace({
  summary,
  project,
  binding,
  resources,
  selected,
  statuses,
  storages,
  links,
  activeStorageId,
  readiness,
  loading,
  busy,
  selectionDirty,
  error,
  onToggleResource,
  onSaveRecipe,
  onStorageChange,
  onLinkStorage,
  onPush,
  onRestore,
  onRemap,
  onRefresh,
  onOpenStorageSettings,
}: Props) {
  const [showLinks, setShowLinks] = useState(false);
  const linked = useMemo(() => links.flatMap((link) => {
    const storage = storages.find((candidate) => candidate.id === link.storage_id);
    return storage ? [{ link, storage }] : [];
  }), [links, storages]);
  const available = storages.filter((storage) => !links.some((link) => link.storage_id === storage.id));
  const counts = statusCounts(statuses);
  const providers = summary.providers ?? [];

  return (
    <main className="v3-main v3-project-workspace">
      <header className="v3-project-header">
        <div className="v3-project-identity">
          <span className="v3-project-large-icon"><Icon name="folder" size={22} /></span>
          <div>
            <span className="v3-eyebrow">Portable project repo</span>
            <h1>{summary.display_name}</h1>
            <button type="button" className="v3-path-button" onClick={onRemap} title="Change this machine's checkout binding">
              {compactProjectPath(binding?.project_root ?? summary.project_root)} <Icon name="settings" size={12} />
            </button>
          </div>
        </div>
        <div className="v3-project-actions">
          <div className="v3-storage-picker">
            {linked.length > 0 ? (
              <select value={activeStorageId ?? ""} onChange={(event) => onStorageChange(event.target.value)} aria-label="Active project storage">
                {linked.map(({ storage }) => <option key={storage.id} value={storage.id}>{storage.name}</option>)}
              </select>
            ) : (
              <button type="button" className="btn" onClick={() => setShowLinks(true)}><Icon name="link" size={14} /> Link storage</button>
            )}
            <button type="button" className="btn btn-ghost" onClick={() => setShowLinks((current) => !current)} title="Manage project storage links"><Icon name="settings" size={14} /></button>
            {showLinks && (
              <div className="v3-link-popover">
                <strong>Project storage</strong>
                {linked.map(({ storage }) => <span key={storage.id}><Icon name={storage.kind === "local" ? "drive" : "cloud"} size={13} /> {storage.name}<small>linked</small></span>)}
                {available.map((storage) => (
                  <button key={storage.id} type="button" onClick={() => onLinkStorage(storage.id)} disabled={busy || !project?.bundle_id}>
                    <Icon name={storage.kind === "local" ? "drive" : "cloud"} size={13} /> Link {storage.name}
                  </button>
                ))}
                {storages.length === 0 && <button type="button" onClick={onOpenStorageSettings}>Create schema-3 storage</button>}
                {available.length === 0 && storages.length > 0 && <small>All configured storage is linked.</small>}
              </div>
            )}
          </div>
          <button type="button" className="btn" onClick={onRestore} disabled={busy || !activeStorageId || !binding || !project?.bundle_id}>
            <Icon name="download" size={14} /> Pull & review
          </button>
          <button type="button" className="btn btn-primary" onClick={onPush} disabled={busy || !activeStorageId || selected.size === 0 || selectionDirty} title={selectionDirty ? "Save resource selection before pushing" : undefined}>
            <Icon name="upload" size={14} /> Push project
          </button>
        </div>
      </header>

      <section className="v3-scope-strip">
        <div>
          <span>Providers</span>
          <strong>{providers.map(providerLabel).join(" + ") || "No provider state discovered"}</strong>
        </div>
        <div>
          <span>Codex profile</span>
          <strong title={binding?.profile_ids?.codex}>{binding?.profile_ids?.codex ? "Assigned" : "Not used"}</strong>
        </div>
        <div>
          <span>Claude profile</span>
          <strong title={binding?.profile_ids?.claude}>{binding?.profile_ids?.claude ? "Assigned" : "Not used"}</strong>
        </div>
        <div>
          <span>Repo identity</span>
          <strong><code>{project?.bundle_id.slice(0, 12) ?? summary.bundle_id.slice(0, 12)}</code></strong>
        </div>
        <button type="button" onClick={onRefresh} disabled={loading || busy} title="Refresh inventory and status"><Icon name="refresh" size={14} className={loading ? "icon-spin" : undefined} /></button>
      </section>

      <section className="v3-project-summary">
        <div><strong>{selected.size}</strong><span>selected resources</span></div>
        <div><strong>{counts.local}</strong><span>local changes</span></div>
        <div><strong>{counts.remote}</strong><span>remote changes</span></div>
        <div className={counts.conflict > 0 ? "warning" : undefined}><strong>{counts.conflict}</strong><span>conflicts</span></div>
        <div className={`v3-readiness-summary ${readiness?.state ?? summary.readiness_state ?? "unknown"}`}>
          <strong>{(readiness?.state ?? summary.readiness_state ?? "Not checked").replace(/_/g, " ")}</strong>
          <span>restore readiness</span>
        </div>
      </section>

      {error && <div className="v3-callout error"><Icon name="alert-triangle" size={15} /> {error}</div>}
      {loading && resources.length === 0 ? (
        <div className="v3-pane-message"><span className="status-loader" /> Discovering project resources…</div>
      ) : (
        <ResourceInventory resources={resources} selected={selected} statuses={statuses} disabled={busy} onToggle={onToggleResource} />
      )}

      {selectionDirty && (
        <div className="v3-sticky-save">
          <span>Resource selection changed · selection becomes the portable project recipe</span>
          <button type="button" className="btn btn-primary" onClick={onSaveRecipe} disabled={busy}>{busy ? "Saving…" : "Save project recipe"}</button>
        </div>
      )}
    </main>
  );
}
