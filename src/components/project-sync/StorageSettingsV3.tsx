import { open } from "@tauri-apps/plugin-dialog";
import type { StorageConfigV3 } from "../../types";
import Icon from "../Icons";

export function newStorage(kind: "s3" | "local", index: number): StorageConfigV3 {
  return {
    id: `storage-${crypto.randomUUID()}`,
    name: kind === "local" ? `Local storage ${index}` : `Cloud storage ${index}`,
    kind,
    bucket: "",
    access_key_id: "",
    secret_access_key: "",
    account_id: "",
    s3_endpoint: "",
    region: "",
    local_dir: "",
    included_default_exclusions: [],
  };
}

export function StorageEditor({
  storage,
  disabled,
  onChange,
  onRemove,
}: {
  storage: StorageConfigV3;
  disabled: boolean;
  onChange: (storage: StorageConfigV3) => void;
  onRemove?: () => void;
}) {
  const set = (patch: Partial<StorageConfigV3>) => onChange({ ...storage, ...patch });
  const chooseFolder = async () => {
    const picked = await open({ directory: true, multiple: false });
    if (typeof picked === "string" && picked) set({ local_dir: picked });
  };

  return (
    <article className="v3-storage-card">
      <header>
        <span className="v3-storage-icon"><Icon name={storage.kind === "local" ? "drive" : "cloud"} size={18} /></span>
        <input value={storage.name} onChange={(event) => set({ name: event.target.value })} disabled={disabled} aria-label="Storage name" />
        <div className="v3-segmented">
          <button type="button" className={storage.kind !== "local" ? "active" : undefined} onClick={() => set({ kind: "s3" })} disabled={disabled}>S3 / R2</button>
          <button type="button" className={storage.kind === "local" ? "active" : undefined} onClick={() => set({ kind: "local" })} disabled={disabled}>Local folder</button>
        </div>
        {onRemove && (
          <button type="button" className="btn btn-ghost danger" onClick={onRemove} disabled={disabled} title="Remove storage"><Icon name="trash" size={14} /></button>
        )}
      </header>
      {storage.kind === "local" ? (
        <label className="v3-folder-field compact">
          <span>Shared folder</span>
          <div>
            <input value={storage.local_dir ?? ""} onChange={(event) => set({ local_dir: event.target.value })} placeholder="/Volumes/backup/agent-sync-v3" disabled={disabled} />
            <button type="button" className="btn" onClick={() => void chooseFolder()} disabled={disabled}>Choose</button>
          </div>
        </label>
      ) : (
        <div className="v3-storage-fields">
          <label><span>Bucket</span><input value={storage.bucket ?? ""} onChange={(event) => set({ bucket: event.target.value })} disabled={disabled} /></label>
          <label><span>Endpoint</span><input value={storage.s3_endpoint ?? ""} onChange={(event) => set({ s3_endpoint: event.target.value })} placeholder="https://…" disabled={disabled} /></label>
          <label><span>Account ID</span><input value={storage.account_id ?? ""} onChange={(event) => set({ account_id: event.target.value })} disabled={disabled} /></label>
          <label><span>Region</span><input value={storage.region ?? ""} onChange={(event) => set({ region: event.target.value })} placeholder="auto" disabled={disabled} /></label>
          <label><span>Access key</span><input value={storage.access_key_id ?? ""} onChange={(event) => set({ access_key_id: event.target.value })} disabled={disabled} autoComplete="off" /></label>
          <label><span>Secret key</span><input type="password" value={storage.secret_access_key ?? ""} onChange={(event) => set({ secret_access_key: event.target.value })} disabled={disabled} autoComplete="new-password" /></label>
        </div>
      )}
      <footer><code>{storage.id}</code><span>Schema 3 only · never included in project repos</span></footer>
    </article>
  );
}
