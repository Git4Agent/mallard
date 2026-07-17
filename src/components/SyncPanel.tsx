import { useCallback, useEffect, useRef, useState, type Ref } from "react";
import { invoke } from "@tauri-apps/api/core";
// window.confirm is a silent no-op in Tauri v2's webview — always use the
// dialog plugin's async confirm instead.
import { confirm } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { AppTheme, CloudProfileInfo, LocalProfile, ProfileLink, StorageConfig, SyncConfig, SyncLink } from "../types";
import Icon from "./Icons";

// Settings = the profile × storage link matrix (PLAN_MULTI_STORAGE.md §7):
// rows are local profiles, columns are storages, cells are links. Click an
// empty cell to link, a linked cell to select; the selected link offers
// Pull / Push / Repair / Set up / Unlink plus its pinned cloud path.

interface Props {
  config: SyncConfig;
  theme: AppTheme;
  onThemeChange: (theme: AppTheme) => void;
  profileStats?: Record<string, { fileCount: number; path: string }>;
  onSave: (config: SyncConfig) => Promise<void>;
  onClose: () => void;
  /** Per-link sync, from the selected-link panel. Config is saved first. */
  onSyncLink?: (direction: "push" | "pull", storage: string, profile: string) => Promise<void>;
  /** Bootstrap the link's mount: pull + plugin repair. Config saved first. */
  onSetupLink?: (storage: string, profile: string) => Promise<void>;
  /** Restore missing plugins for one local profile. Config is saved first. */
  onRepairProfile?: (profile: LocalProfile) => Promise<void>;
  focusProfile?: string | null;
  focusStorage?: string | null;
  focusRequestId?: number;
  command?: { type: "add-profile" | "add-storage"; requestId: number } | null;
  busy?: boolean;
  setupBusy?: boolean;
}

const R2_BUCKETS_DOC = "https://developers.cloudflare.com/r2/buckets/create-buckets/";
const R2_AUTH_DOC = "https://developers.cloudflare.com/r2/api/tokens/";
const R2_TOKEN_CREDS_DOC = "https://developers.cloudflare.com/r2/api/tokens/#get-s3-api-credentials-from-an-api-token";
const ACCOUNT_ID_DOC = "https://developers.cloudflare.com/fundamentals/account/find-account-and-zone-ids/";

async function openHelp(url: string) {
  try {
    await openUrl(url);
  } catch {
    window.open(url, "_blank", "noopener,noreferrer");
  }
}

function HelpButton({ url, label }: { url: string; label: string }) {
  return (
    <button
      type="button"
      className="field-help"
      onClick={() => openHelp(url)}
      title={label}
      aria-label={label}
    >
      ?
    </button>
  );
}

function genId(): string {
  const bytes = new Uint8Array(4);
  crypto.getRandomValues(bytes);
  return [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
}

// Mirrors the backend container rule: a folder not named after the root
// hosts it as a subdirectory.
function effectiveMount(root: string, localDir: string): string {
  if (!localDir) return `~/${root}`;
  const trimmed = localDir.replace(/\/+$/, "");
  const last = trimmed.split("/").pop();
  return last === root ? trimmed : `${trimmed}/${root}`;
}

function compactPath(path: string): string {
  const display = path.replace(/^\/Users\/[^/]+/, "~");
  if (display.length <= 42) return display;
  const parts = display.split("/").filter(Boolean);
  const last = parts[parts.length - 1] ?? "";
  if (display.startsWith("~/") && parts.length > 2) {
    return `~/${parts[1]}/…/${last}`;
  }
  return `${display.slice(0, 20)}…${display.slice(-16)}`;
}

function formatFileCount(count: number | undefined): string {
  if (count == null) return "Scanning files…";
  return `${count.toLocaleString()} ${count === 1 ? "file" : "files"}`;
}

export function profileLabel(profile: LocalProfile): string {
  if (profile.name?.trim()) return profile.name.trim();
  const configuredPath = (profile.path ?? "").replace(/\/+$/, "");
  if (!configuredPath) return `~/${profile.root}`;
  const last = configuredPath.split("/").pop();
  if (last && last !== profile.root) return `${last}/${profile.root}`;
  return compactPath(configuredPath);
}

function storageSubtitle(storage: StorageConfig): string {
  if (storage.kind === "local") return storage.local_dir ? compactPath(storage.local_dir) : "Local folder";
  return "Cloud storage";
}

function storageIsConfigured(storage: StorageConfig): boolean {
  if (storage.kind === "local") return !!storage.local_dir;
  return (
    !!storage.bucket &&
    !!storage.access_key_id &&
    !!storage.secret_access_key &&
    (!!storage.account_id || !!storage.s3_endpoint)
  );
}

// Everything outside the default allowlist stays local; these are the known,
// deliberate opt-ins (see AGENT_SYNC_FILE_SETS.md), now per storage —
// opting a file into one destination never leaks it into another.
// Credentials and machine identity never sync, opted in or not.
const OPTIONAL_DATA = [
  {
    path: ".codex/memories_1.sqlite*",
    label: "memories_1.sqlite",
    reason: "Codex derived memory database",
    warning: "Uploaded as a SQLite snapshot; -wal/-shm sidecars never sync",
  },
  {
    path: ".codex/state_5.sqlite*",
    label: "state_5.sqlite",
    reason: "Codex thread index and runtime state",
  },
  {
    path: ".codex/goals_1.sqlite*",
    label: "goals_1.sqlite",
    reason: "Codex goal continuation state",
  },
  {
    path: ".codex/logs_2.sqlite*",
    label: "logs_2.sqlite",
    reason: "Codex diagnostic event database",
  },
  {
    path: ".codex/AGENTS.override.md",
    label: "AGENTS.override.md",
    reason: "Temporary active instruction override",
    warning:
      "Deletions never propagate — removing it on one machine will not remove cloud copies, so the override can reappear indefinitely",
  },
  // The plugin manager's own JSONs (installed_plugins/known_marketplaces)
  // are deliberately NOT offered: they embed machine-local paths and
  // overwriting another machine's copies corrupts its manager state. Use
  // the Repair button instead (see PLAN_PLUGIN_SYNC.md).
  {
    path: ".claude/plugins/cache",
    label: "plugins/cache/",
    reason: "Installed Claude plugin payloads (offline one-pull restore)",
    warning: "Plugins execute with your privileges on every machine that pulls — prefer the Repair button",
  },
] as const;

/** Storage-side probe result: discovered profiles, or unreachable. */
type StorageProbe = CloudProfileInfo[] | "unreachable";

// The link's cloud side as saved in sync_config.json, checked against what
// the storage actually holds: stale ids (relinked on next sync) and
// unreachable destinations get a warning instead of silence.
function CloudLinkLine({ cloud, probe }: { cloud?: ProfileLink; probe?: StorageProbe }) {
  if (!cloud?.profile_id) {
    return <span className="storage-link-cloud">No cloud profile yet — first push or pull links one</span>;
  }
  const shortId = cloud.profile_id.slice(0, 8);
  const label = cloud.profile_label || cloud.root || shortId;
  const pin = cloud.pinned ? " · pinned" : "";
  if (probe === "unreachable") {
    return (
      <span className="storage-link-cloud stale" title={`Cloud profile ${cloud.profile_id}`}>
        <Icon name="alert-triangle" size={12} />
        {`'${label}' (${shortId}) — storage unreachable, check folder or credentials`}
      </span>
    );
  }
  const info = probe?.find((p) => p.profile_id === cloud.profile_id);
  if (probe && !info) {
    return (
      <span
        className="storage-link-cloud stale"
        title={`Linked cloud profile ${cloud.profile_id} was not found in this storage. The next push or pull relinks by agent root or creates a new profile.`}
      >
        <Icon name="alert-triangle" size={12} />
        {`'${label}' (${shortId}) missing in storage — next sync relinks`}
      </span>
    );
  }
  const detail = info ? ` · gen ${info.generation} · ${formatFileCount(info.files)}` : "";
  return (
    <span className="storage-link-cloud" title={`Cloud profile ${cloud.profile_id}`}>
      {`Cloud profile '${info?.label ?? label}' (${shortId})${detail}${pin}`}
    </span>
  );
}

function CloudProfileSummary({ cloud, probe }: { cloud?: ProfileLink; probe?: StorageProbe }) {
  if (!cloud?.profile_id) {
    return (
      <span className="storage-link-profile-copy" title="Choose a cloud profile">
        <strong>Not linked</strong>
        <span>Cloud profile</span>
      </span>
    );
  }

  const info = probe === "unreachable"
    ? undefined
    : probe?.find((candidate) => candidate.profile_id === cloud.profile_id);
  const label = info?.label ?? cloud.profile_label ?? cloud.root ?? cloud.profile_id.slice(0, 8);
  const detail = info ? `Generation ${info.generation} · ${formatFileCount(info.files)}` : "Cloud profile";

  if (probe === "unreachable") {
    return (
      <span className="storage-link-profile-copy" title="Storage is unreachable">
        <strong>{label}</strong>
        <span className="warning">Check storage</span>
      </span>
    );
  }
  if (probe && !info) {
    return (
      <span className="storage-link-profile-copy" title="The linked cloud profile is missing; the next sync will relink it">
        <strong>{label}</strong>
        <span className="warning">Relink needed</span>
      </span>
    );
  }
  return (
    <span className="storage-link-profile-copy" title={`${label} · ${detail}`}>
      <strong>{label}</strong>
      <span>{detail}</span>
    </span>
  );
}

// Candidate cloud profiles inside one storage, for picking a link's cloud
// side. A profile another local root syncs is selectable — baselines are
// per link — but says so, since edits will flow between the two roots.
function CloudProfilePicker({
  root,
  probe,
  holders,
  currentId,
  onPick,
  onCreateNew,
}: {
  root: string;
  probe?: StorageProbe;
  holders: Map<string, string>;
  currentId?: string;
  onPick: (info: CloudProfileInfo) => void;
  onCreateNew?: () => void;
}) {
  if (!probe) return <span className="storage-link-cloud">Scanning storage…</span>;
  if (probe === "unreachable") {
    return (
      <span className="storage-link-cloud stale">
        <Icon name="alert-triangle" size={12} />
        Storage unreachable — check folder or credentials
      </span>
    );
  }
  const candidates = probe.filter((p) => p.root === root);
  return (
    <>
      {candidates.map((info) => {
        const holder = holders.get(info.profile_id);
        const current = info.profile_id === currentId;
        return (
          <button
            key={info.profile_id}
            type="button"
            onClick={() => onPick(info)}
            disabled={current}
            title={`Cloud profile ${info.profile_id}`}
          >
            <Icon name="cloud" size={17} />
            <span>
              {`${info.label} (${info.profile_id.slice(0, 8)}) · gen ${info.generation} · ${formatFileCount(info.files)}`}
              {current ? " · linked" : holder ? ` · also synced by ${holder}` : ""}
            </span>
          </button>
        );
      })}
      {onCreateNew && (
        <button type="button" onClick={onCreateNew}>
          <Icon name="plus" size={15} />
          <span>Create new profile</span>
        </button>
      )}
    </>
  );
}

function CloudProfileSelect({
  root,
  probe,
  holders,
  currentId,
  onPick,
}: {
  root: string;
  probe?: StorageProbe;
  holders: Map<string, string>;
  currentId?: string;
  onPick: (info: CloudProfileInfo) => void;
}) {
  if (!probe) return <span className="storage-link-select-state">Scanning storage…</span>;
  if (probe === "unreachable") {
    return (
      <span className="storage-link-select-state stale">
        <Icon name="alert-triangle" size={12} />
        Storage unreachable
      </span>
    );
  }

  const candidates = probe.filter((profile) => profile.root === root);
  const currentIsMissing = !!currentId && !candidates.some((profile) => profile.profile_id === currentId);
  return (
    <select
      className="form-input storage-link-profile-select"
      value={currentId ?? ""}
      onChange={(event) => {
        const profile = candidates.find((candidate) => candidate.profile_id === event.target.value);
        if (profile) onPick(profile);
      }}
      disabled={candidates.length === 0}
    >
      {!currentId && <option value="">Choose a cloud profile</option>}
      {currentIsMissing && <option value={currentId}>{`Current profile (${currentId.slice(0, 8)}) — missing`}</option>}
      {candidates.map((profile) => {
        const holder = holders.get(profile.profile_id);
        const shared = holder ? ` · shared with ${holder}` : "";
        return (
          <option key={profile.profile_id} value={profile.profile_id}>
            {`${profile.label} (${profile.profile_id.slice(0, 8)}) · gen ${profile.generation} · ${formatFileCount(profile.files)}${shared}`}
          </option>
        );
      })}
      {candidates.length === 0 && <option value="">No matching cloud profiles</option>}
    </select>
  );
}

function StorageEditor({
  storage,
  onChange,
  onRemove,
}: {
  storage: StorageConfig;
  onChange: (next: StorageConfig) => void;
  onRemove: () => void;
}) {
  const set = (patch: Partial<StorageConfig>) => onChange({ ...storage, ...patch });
  const optIns = new Set(storage.included_default_exclusions ?? []);
  const autoEndpoint = (id: string) => (id ? `https://${id}.r2.cloudflarestorage.com` : "");

  const handleAccountIdChange = (val: string) => {
    const prev = autoEndpoint(storage.account_id ?? "");
    const endpoint = storage.s3_endpoint === "" || storage.s3_endpoint === prev
      ? autoEndpoint(val)
      : storage.s3_endpoint;
    set({ account_id: val, s3_endpoint: endpoint });
  };

  // Paste the full R2 S3 API URL → auto-split into base endpoint + bucket + account ID
  const handleS3EndpointChange = (val: string) => {
    const r2 = val.match(/^(https?:\/\/([a-f0-9A-F]+)\.r2\.cloudflarestorage\.com)\/([^/]+)\/?$/);
    if (r2) {
      set({
        s3_endpoint: r2[1],
        account_id: storage.account_id || r2[2],
        bucket: storage.bucket || r2[3],
      });
    } else {
      set({ s3_endpoint: val });
    }
  };

  const toggleOptIn = (path: string) => {
    const next = new Set(optIns);
    if (next.has(path)) next.delete(path);
    else next.add(path);
    set({ included_default_exclusions: [...next] });
  };
  const includedCount = OPTIONAL_DATA.filter((entry) => optIns.has(entry.path)).length;

  return (
    <div className="storage-editor">
      <div className="form-row-2">
        <div className="form-field">
          <span className="form-label">Name</span>
          <input
            className="form-input"
            type="text"
            value={storage.name}
            onChange={(e) => set({ name: e.target.value })}
            placeholder="Personal"
            spellCheck={false}
            autoComplete="off"
          />
        </div>
        <div className="form-field">
          <span className="form-label">Kind</span>
          <div className="mode-switch" role="radiogroup" aria-label="Storage kind">
            <button
              type="button"
              className={`mode-switch-btn${storage.kind !== "local" ? " active" : ""}`}
              role="radio"
              aria-checked={storage.kind !== "local"}
              onClick={() => set({ kind: "s3" })}
            >
              Cloud (R2 / S3)
            </button>
            <button
              type="button"
              className={`mode-switch-btn${storage.kind === "local" ? " active" : ""}`}
              role="radio"
              aria-checked={storage.kind === "local"}
              onClick={() => set({ kind: "local" })}
            >
              Local folder
            </button>
          </div>
        </div>
      </div>

      {storage.kind === "local" ? (
        <>
          <div className="form-field">
            <div className="form-label-row">
              <span className="form-label">Shared folder</span>
              <span className="form-hint">created if missing</span>
            </div>
            <input
              className="form-input"
              type="text"
              value={storage.local_dir ?? ""}
              onChange={(e) => set({ local_dir: e.target.value })}
              placeholder="/Volumes/backup/agent-sync"
              spellCheck={false}
              autoComplete="off"
            />
          </div>
          <div className="settings-note">
            Uses this folder as the shared sync store. Choose a NAS, USB drive,
            or folder-sync service.
          </div>
        </>
      ) : (
        <>
          <div className="form-field">
            <div className="form-label-row">
              <span className="form-label">Bucket</span>
              <HelpButton url={R2_BUCKETS_DOC} label="Open Cloudflare R2 bucket docs" />
            </div>
            <input
              className="form-input"
              type="text"
              value={storage.bucket ?? ""}
              onChange={(e) => set({ bucket: e.target.value })}
              placeholder="my-bucket"
              spellCheck={false}
              autoComplete="off"
            />
          </div>
          <div className="form-row-2">
            <div className="form-field">
              <div className="form-label-row">
                <span className="form-label">Account ID</span>
                <HelpButton url={ACCOUNT_ID_DOC} label="Open Cloudflare account ID docs" />
              </div>
              <input
                className="form-input"
                type="text"
                value={storage.account_id ?? ""}
                onChange={(e) => handleAccountIdChange(e.target.value)}
                placeholder="9cc0c910ec34cb9a7d…"
                spellCheck={false}
                autoComplete="off"
              />
            </div>
          </div>
          <div className="form-row-2">
            <div className="form-field">
              <div className="form-label-row">
                <span className="form-label">Access key</span>
                <HelpButton url={R2_TOKEN_CREDS_DOC} label="Open R2 Access Key ID docs" />
              </div>
              <input
                className="form-input"
                type="text"
                value={storage.access_key_id ?? ""}
                onChange={(e) => set({ access_key_id: e.target.value })}
                placeholder="Access key ID"
                spellCheck={false}
                autoComplete="off"
              />
            </div>
            <div className="form-field">
              <div className="form-label-row">
                <span className="form-label">Secret key</span>
                <HelpButton url={R2_TOKEN_CREDS_DOC} label="Open R2 Secret Access Key docs" />
              </div>
              <input
                className="form-input"
                type="password"
                value={storage.secret_access_key ?? ""}
                onChange={(e) => set({ secret_access_key: e.target.value })}
                placeholder="Secret access key"
                autoComplete="new-password"
              />
            </div>
          </div>
          <div className="form-field">
            <div className="form-label-row">
              <span className="form-label">Endpoint</span>
              <HelpButton url={R2_AUTH_DOC} label="Open R2 endpoint docs" />
              <span className="form-hint">filled from Account ID</span>
            </div>
            <input
              className="form-input"
              type="url"
              value={storage.s3_endpoint ?? ""}
              onChange={(e) => handleS3EndpointChange(e.target.value)}
              placeholder="https://<account>.r2.cloudflarestorage.com"
              spellCheck={false}
              autoComplete="off"
            />
          </div>
        </>
      )}

      {/* ── Optional data — per storage ── */}
      <details className="exclusion-settings">
        <summary className="exclusion-summary">
          <span>
            <span className="exclusion-title">Optional data</span>
            <span className="exclusion-subtitle">Off by default — enabled for this storage only</span>
          </span>
          <span className="exclusion-count">
            {includedCount > 0 ? `${includedCount} of ${OPTIONAL_DATA.length} enabled` : "none enabled"}
          </span>
        </summary>
        <div className="exclusion-content">
          <div className="exclusion-group">
            <div className="exclusion-list">
              {OPTIONAL_DATA.map((entry) => (
                <label className="exclusion-row" key={entry.path}>
                  <span className="exclusion-copy">
                    <span className="exclusion-path">{entry.label}</span>
                    <span className="exclusion-reason">{entry.reason}</span>
                  </span>
                  {"warning" in entry && entry.warning && (
                    <span className="exclusion-warning" title={entry.warning}>
                      <Icon name="alert-triangle" size={13} />
                    </span>
                  )}
                  <input
                    className="exclusion-switch"
                    type="checkbox"
                    role="switch"
                    checked={optIns.has(entry.path)}
                    onChange={() => toggleOptIn(entry.path)}
                    aria-label={`Include ${entry.label}`}
                  />
                </label>
              ))}
            </div>
          </div>
        </div>
      </details>

      <div className="storage-editor-footer">
        <button
          type="button"
          className="btn-link storage-remove"
          onClick={onRemove}
          title="Remove this storage from sync settings. Local folders and storage data are not deleted."
        >
          Remove storage
        </button>
      </div>
    </div>
  );
}

function ProfileEditor({
  profile,
  isDefault,
  pathInputRef,
  onChange,
  onRemove,
}: {
  profile: LocalProfile;
  isDefault: boolean;
  pathInputRef?: Ref<HTMLInputElement>;
  onChange: (next: LocalProfile) => void;
  onRemove: () => void;
}) {
  return (
    <div className="storage-editor">
      <div className="form-field">
        <div className="form-label-row">
          <span className="form-label">Name</span>
          <span className="form-hint">empty = {profileLabel({ ...profile, name: "" })}</span>
        </div>
        <input
          className="form-input"
          type="text"
          value={profile.name ?? ""}
          onChange={(e) => onChange({ ...profile, name: e.target.value })}
          placeholder={profileLabel({ ...profile, name: "" })}
          spellCheck={false}
          autoComplete="off"
        />
        <span className="form-hint">A custom name also renames the linked cloud profile on the next push.</span>
      </div>
      <div className="form-row-2">
        <div className="form-field">
          <span className="form-label">Agent</span>
          {isDefault ? (
            <span className="sync-link-location-value"><code>{profile.root}</code></span>
          ) : (
            <div className="mode-switch" role="radiogroup" aria-label="Agent root">
              {[".codex", ".claude"].map((root) => (
                <button
                  key={root}
                  type="button"
                  className={`mode-switch-btn${profile.root === root ? " active" : ""}`}
                  role="radio"
                  aria-checked={profile.root === root}
                  onClick={() => onChange({ ...profile, root })}
                >
                  {root}
                </button>
              ))}
            </div>
          )}
        </div>
        <div className="form-field">
          <div className="form-label-row">
            <span className="form-label">Local folder</span>
            {isDefault && <span className="form-hint">empty = ~/{profile.root}</span>}
          </div>
          <input
            ref={pathInputRef}
            className="form-input"
            type="text"
            value={profile.path ?? ""}
            onChange={(e) => onChange({ ...profile, path: e.target.value })}
            placeholder={`~/${profile.root}`}
            spellCheck={false}
            autoComplete="off"
          />
        </div>
      </div>
      <div className="form-hint">
        A folder not named {profile.root} hosts it as a subdirectory
        ({compactPath(effectiveMount(profile.root, profile.path ?? "myconf"))}).
      </div>
      <div className="storage-editor-footer">
        <button
          type="button"
          className="btn-link storage-remove"
          onClick={onRemove}
          title="Remove this profile from sync settings. Its folder and files remain on this Mac."
        >
          Remove profile
        </button>
      </div>
    </div>
  );
}

export default function SyncPanel({
  config,
  theme,
  onThemeChange,
  profileStats,
  onSave,
  onClose,
  onSyncLink,
  onSetupLink,
  onRepairProfile,
  focusProfile,
  focusStorage,
  focusRequestId,
  command,
  busy,
  setupBusy,
}: Props) {
  const [storages, setStorages] = useState<StorageConfig[]>(config.storages ?? []);
  const [profiles, setProfiles] = useState<LocalProfile[]>(config.local_profiles ?? []);
  const [links, setLinks] = useState<SyncLink[]>(config.links ?? []);
  const [selected, setSelected] = useState<{ profile: string; storage: string } | null>(null);
  const [linkingProfile, setLinkingProfile] = useState<string | null>(null);
  /** Step two of linking: the chosen storage whose cloud profiles to pick from. */
  const [linkingStorage, setLinkingStorage] = useState<string | null>(null);
  const [linkSettingsOpen, setLinkSettingsOpen] = useState<{ profile: string; storage: string } | null>(null);
  const [editingStorage, setEditingStorage] = useState<string | null>(null);
  const [editingProfile, setEditingProfile] = useState<string | null>(null);
  const [storageEditRequestId, setStorageEditRequestId] = useState(0);
  const [profileEditRequestId, setProfileEditRequestId] = useState(0);
  const [runningLinkAction, setRunningLinkAction] = useState<{
    direction: "push" | "pull";
    profile: string;
  } | null>(null);
  const [pinInput, setPinInput] = useState("");
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const storageEditorRef = useRef<HTMLElement>(null);
  const profileEditorRef = useRef<HTMLElement>(null);
  const profilePathInputRef = useRef<HTMLInputElement>(null);

  // Re-adopt the canonical config after each save round-trip.
  useEffect(() => {
    setStorages(config.storages ?? []);
    setProfiles(config.local_profiles ?? []);
    setLinks(config.links ?? []);
  }, [config]);

  // Probe each saved storage for the cloud profiles it actually holds, so
  // link rows can show stale/missing cloud sides. Best-effort and read-only;
  // runs on open/save and from the Refresh button.
  const [storageProbes, setStorageProbes] = useState<Record<string, StorageProbe>>({});
  const [probing, setProbing] = useState(false);
  const probeStorages = useCallback(async () => {
    const targets = (config.storages ?? []).filter(storageIsConfigured);
    if (targets.length === 0) return;
    setProbing(true);
    try {
      await Promise.all(targets.map(async (storage) => {
        let probe: StorageProbe;
        try {
          probe = await invoke<CloudProfileInfo[]>("list_sync_profiles", { storage: storage.id });
        } catch {
          probe = "unreachable";
        }
        setStorageProbes((prev) => ({ ...prev, [storage.id]: probe }));
      }));
    } finally {
      setProbing(false);
    }
  }, [config.storages]);
  useEffect(() => { void probeStorages(); }, [probeStorages]);

  useEffect(() => {
    if (!focusProfile) return;
    setEditingStorage(null);
    setEditingProfile(focusProfile);
  }, [focusProfile, focusRequestId]);

  useEffect(() => {
    if (!focusStorage) return;
    setEditingProfile(null);
    setEditingStorage(focusStorage);
  }, [focusStorage, focusRequestId]);

  useEffect(() => {
    if (!editingProfile) return;
    const frame = window.requestAnimationFrame(() => {
      profileEditorRef.current?.scrollIntoView({ block: "center", behavior: "smooth" });
      profilePathInputRef.current?.focus();
      profilePathInputRef.current?.select();
    });
    return () => window.cancelAnimationFrame(frame);
  }, [editingProfile, focusRequestId, profileEditRequestId]);

  useEffect(() => {
    if (!editingStorage) return;
    const frame = window.requestAnimationFrame(() => {
      storageEditorRef.current?.scrollIntoView({ block: "center", behavior: "smooth" });
    });
    return () => window.cancelAnimationFrame(frame);
  }, [editingStorage, focusRequestId, storageEditRequestId]);

  const findLink = (profile: string, storage: string) =>
    links.find((l) => l.profile === profile && l.storage === storage);
  const selectedLink = selected ? findLink(selected.profile, selected.storage) : undefined;
  const selectedProfile = selected ? profiles.find((p) => p.id === selected.profile) : undefined;
  const selectedStorage = selected ? storages.find((s) => s.id === selected.storage) : undefined;

  // The pin editor tracks the selected link.
  useEffect(() => {
    const cloud = selectedLink?.cloud;
    setPinInput(cloud?.pinned ? cloud.profile_id ?? "" : "");
  }, [selectedLink]);

  const assembled = (): SyncConfig => ({
    schema: 2,
    storages,
    local_profiles: profiles,
    links,
  });

  const persist = async (next?: SyncConfig) => {
    setSaving(true);
    try {
      await onSave(next ?? assembled());
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } finally {
      setSaving(false);
    }
  };

  const handleSave = async () => {
    // Fold a pin edit into the selected link before saving.
    let nextLinks = links;
    if (selected && selectedLink) {
      const currentPin = selectedLink.cloud?.pinned ? selectedLink.cloud.profile_id ?? "" : "";
      if (pinInput !== currentPin) {
        nextLinks = links.map((l) =>
          l.profile === selected.profile && l.storage === selected.storage
            ? {
                ...l,
                cloud: pinInput
                  ? { root: selectedProfile?.root, profile_id: pinInput, pinned: true }
                  : {},
              }
            : l,
        );
        setLinks(nextLinks);
      }
    }
    await persist({ schema: 2, storages, local_profiles: profiles, links: nextLinks });
  };

  const unlinkSelected = async () => {
    if (!selected) return;
    const label = `${selectedProfile ? profileLabel(selectedProfile) : selected.profile} ⇄ ${selectedStorage?.name || selected.storage}`;
    if (!(await confirm(`Unlink ${label}?\n\nThis forgets the link's local sync state (baseline). Nothing in the storage is deleted; relinking later re-verifies by content.`, { title: "Unlink" }))) {
      return;
    }
    setLinks((prev) => prev.filter((l) => !(l.profile === selected.profile && l.storage === selected.storage)));
    setSelected(null);
    setLinkSettingsOpen(null);
  };

  const addStorage = (linkProfile?: string) => {
    const id = genId();
    setStorages((prev) => [
      ...prev,
      { id, name: `Storage ${prev.length + 1}`, kind: "s3" },
    ]);
    if (linkProfile) {
      setLinks((prev) => [...prev, { profile: linkProfile, storage: id, cloud: {} }]);
      setSelected({ profile: linkProfile, storage: id });
    }
    setLinkingProfile(null);
    setLinkingStorage(null);
    setLinkSettingsOpen(null);
    setEditingProfile(null);
    setEditingStorage(id);
  };

  const addProfile = () => {
    const id = genId();
    setProfiles((prev) => [...prev, { id, root: ".claude", path: "" }]);
    setLinkingProfile(null);
    setEditingStorage(null);
    setEditingProfile(id);
  };

  useEffect(() => {
    if (!command) return;
    if (command.type === "add-profile") addProfile();
    else addStorage();
  }, [command?.requestId]);

  const addLink = (profile: string, storage: string, cloud?: ProfileLink) => {
    if (findLink(profile, storage)) return;
    setLinks((prev) => [...prev, { profile, storage, cloud: cloud ?? {} }]);
    setSelected({ profile, storage });
    setLinkSettingsOpen(null);
    setLinkingProfile(null);
    setLinkingStorage(null);
    setEditingProfile(null);
    setEditingStorage(null);
  };

  const beginLink = (profile: string) => {
    const available = storages.filter((storage) => !findLink(profile, storage.id));
    if (available.length === 0) {
      if (storages.length === 0) addStorage(profile);
      return;
    }
    setLinkingProfile((current) => (current === profile ? null : profile));
    setLinkingStorage(null);
  };

  /** Sibling links' cloud targets on one storage, as profile_id → local label. */
  const cloudHolders = (storageId: string, excludeProfile?: string) => {
    const holders = new Map<string, string>();
    links.forEach((l) => {
      if (l.storage !== storageId || !l.cloud?.profile_id || l.profile === excludeProfile) return;
      if (holders.has(l.cloud.profile_id)) return;
      const lp = profiles.find((p) => p.id === l.profile);
      holders.set(l.cloud.profile_id, lp ? profileLabel(lp) : l.profile);
    });
    return holders;
  };

  // "Create new profile" must CREATE even when the storage already holds a
  // matching profile — `cloud: {}` (auto) would silently link that one
  // instead. A fresh pinned id takes the pinned-create path, at a label
  // that doesn't collide in this storage.
  const createNewCloud = (storageId: string, root: string): ProfileLink => {
    const probe = storageProbes[storageId];
    const existing = new Set(Array.isArray(probe) ? probe.map((p) => p.label) : []);
    const base = root === ".codex" ? "Codex" : "Claude";
    let label = base;
    for (let n = 2; existing.has(label); n += 1) label = `${base} ${n}`;
    return { root, profile_id: genId(), profile_label: label, pinned: true };
  };

  /** Point the selected link at another cloud profile (baseline resets). */
  const repickCloud = async (info: CloudProfileInfo) => {
    if (!selected || info.profile_id === selectedLink?.cloud?.profile_id) return;
    if (!(await confirm(`Relink to cloud profile '${info.label}' (${info.profile_id.slice(0, 8)})?\n\nThis link's local sync state (baseline) resets; the next sync re-verifies by content. Nothing in the storage is deleted.`, { title: "Relink" }))) {
      return;
    }
    setLinks((prev) => prev.map((l) =>
      l.profile === selected.profile && l.storage === selected.storage
        ? { ...l, cloud: { root: info.root, profile_id: info.profile_id, profile_label: info.label, pinned: true } }
        : l,
    ));
  };

  const beginAnyLink = () => {
    if (profiles.length === 0) {
      addProfile();
      return;
    }
    const profile = profiles.find((candidate) =>
      storages.some((storage) => !findLink(candidate.id, storage.id)),
    );
    if (profile) {
      beginLink(profile.id);
      return;
    }
    if (storages.length === 0) {
      addStorage(profiles[0].id);
    }
  };

  const configureStorage = (storage: string) => {
    setEditingProfile(null);
    setEditingStorage(storage);
    setStorageEditRequestId((requestId) => requestId + 1);
  };

  const configureProfile = (profile: string) => {
    setEditingStorage(null);
    setEditingProfile(profile);
    setProfileEditRequestId((requestId) => requestId + 1);
  };

  const removeStorage = async (id: string) => {
    const storage = storages.find((s) => s.id === id);
    if (!(await confirm(`Remove storage "${storage?.name || id}" from sync settings?\n\nLocal profile folders and storage data are not deleted. Only this app's sync settings and link bookkeeping are removed.`, { title: "Remove storage" }))) {
      return;
    }
    setStorages((prev) => prev.filter((s) => s.id !== id));
    setLinks((prev) => prev.filter((l) => l.storage !== id));
    if (selected?.storage === id) setSelected(null);
    if (linkSettingsOpen?.storage === id) setLinkSettingsOpen(null);
    setEditingStorage(null);
  };

  const removeProfile = async (id: string) => {
    const profile = profiles.find((p) => p.id === id);
    if (!(await confirm(`Remove profile "${profile ? profileLabel(profile) : id}" from sync settings?\n\nIts folder and all files remain on this Mac. Only this app's sync settings and link bookkeeping are removed.`, { title: "Remove profile" }))) {
      return;
    }
    setProfiles((prev) => prev.filter((p) => p.id !== id));
    setLinks((prev) => prev.filter((l) => l.profile !== id));
    if (selected?.profile === id) setSelected(null);
    if (linkSettingsOpen?.profile === id) setLinkSettingsOpen(null);
    setEditingProfile(null);
  };

  const syncLinks = async (direction: "push" | "pull", targets: SyncLink[]) => {
    if (!onSyncLink || targets.length === 0) return;
    const profile = targets[0].profile;
    setRunningLinkAction({ direction, profile });
    try {
      await handleSave();
      for (const target of targets) {
        await onSyncLink(direction, target.storage, target.profile);
      }
    } finally {
      setRunningLinkAction(null);
    }
  };

  const setupLink = async (target: SyncLink) => {
    if (!onSetupLink) return;
    await handleSave();
    await onSetupLink(target.storage, target.profile);
  };

  const setupSelected = async () => {
    if (!selectedLink) return;
    await setupLink(selectedLink);
  };

  const repairProfile = async (profile: LocalProfile) => {
    if (!onRepairProfile) return;
    await handleSave();
    await onRepairProfile(profile);
  };

  const linkCount = links.length;
  const editedStorage = editingStorage ? storages.find((s) => s.id === editingStorage) : undefined;
  const editedProfile = editingProfile ? profiles.find((p) => p.id === editingProfile) : undefined;
  const pendingPin = selected && selectedLink
    ? pinInput !== (selectedLink.cloud?.pinned ? selectedLink.cloud.profile_id ?? "" : "")
    : false;
  const isDirty = pendingPin
    || JSON.stringify(storages) !== JSON.stringify(config.storages ?? [])
    || JSON.stringify(profiles) !== JSON.stringify(config.local_profiles ?? [])
    || JSON.stringify(links) !== JSON.stringify(config.links ?? []);

  const canAddLink = profiles.length === 0 || storages.length === 0 || profiles.some(
    (profile) => storages.some((storage) => !findLink(profile.id, storage.id)),
  );

  return (
    <div className="sync-panel">
      <div className="sync-panel-content">
        <div className="sync-panel-header">
          <h1 className="sync-panel-title">Sync settings</h1>
          <button
            className="btn btn-ghost settings-close"
            onClick={onClose}
            title="Close settings"
            aria-label="Close settings"
          >
            <Icon name="x" size={17} />
          </button>
        </div>

        <div className="sync-panel-body">
          <section className="settings-section appearance-section" aria-labelledby="settings-appearance">
            <div className="appearance-row">
              <div className="appearance-copy">
                <h2 id="settings-appearance" className="settings-section-title">Appearance</h2>
                <div className="settings-note">Choose the interface theme for this Mac.</div>
              </div>
              <div className="theme-toggle" role="group" aria-label="Interface theme">
                <span className={theme === "light" ? "active" : undefined}>Light</span>
                <label className="theme-switch-label">
                  <input
                    type="checkbox"
                    className="exclusion-switch theme-switch"
                    checked={theme === "dark"}
                    onChange={(event) => onThemeChange(event.target.checked ? "dark" : "light")}
                    aria-label="Use dark theme"
                  />
                </label>
                <span className={theme === "dark" ? "active" : undefined}>Dark</span>
              </div>
            </div>
          </section>

          <section className="settings-section profile-links-section" aria-labelledby="settings-links">
            <div className="profile-links-heading">
              <div className="profile-links-copy">
                <h2 id="settings-links" className="settings-section-title">Profile links</h2>
                <div className="profile-links-subtitle">
                  Choose which profiles sync with each storage location.
                </div>
              </div>
              <div className="profile-links-heading-actions">
                <div className="profile-links-counts">
                  {profiles.length} profiles <span>·</span> {storages.length} storage <span>·</span> {linkCount} links
                </div>
                <div className="profile-links-primary-actions">
                  <button
                    type="button"
                    className="btn profile-refresh-linkage"
                    onClick={() => void probeStorages()}
                    disabled={probing}
                    title="Re-check each storage for its cloud profiles and refresh link status"
                    aria-busy={probing}
                  >
                    <Icon name="refresh" size={17} />
                    {probing ? "Refreshing…" : "Refresh"}
                  </button>
                  <button
                    type="button"
                    className="btn profile-add-profile"
                    onClick={addProfile}
                  >
                    <Icon name="plus" size={17} />
                    Add profile
                  </button>
                  <button
                    type="button"
                    className="btn profile-add-link"
                    onClick={beginAnyLink}
                    disabled={!canAddLink}
                  >
                    <Icon name="plus" size={17} />
                    Add link
                  </button>
                </div>
              </div>
            </div>

            {profiles.length === 0 ? (
              <div className="profile-links-empty">
                <Icon name="computer" size={24} />
                <span>Add a profile to choose where its files sync.</span>
                <button type="button" className="btn" onClick={addProfile}>
                  <Icon name="plus" size={15} /> Add profile
                </button>
              </div>
            ) : (
              <div className="profile-links-list">
                {profiles.map((profile) => {
                  const profileLinks = links.filter((link) => link.profile === profile.id);
                  const availableStorages = storages.filter(
                    (storage) => !findLink(profile.id, storage.id),
                  );
                  const stats = profileStats?.[profile.id];
                  const fullPath = stats?.path ?? effectiveMount(profile.root, profile.path ?? "");
                  const isEmptyProfile = stats?.fileCount === 0;

                  return (
                    <article key={profile.id} className="profile-link-card">
                      <div className="profile-link-profile">
                        <span className="profile-link-profile-icon">
                          <Icon name="computer" size={25} />
                        </span>
                        <div className="profile-link-profile-copy">
                          <strong>{profileLabel(profile)}</strong>
                          <span>{formatFileCount(stats?.fileCount)}</span>
                          <span className="profile-link-path" title={fullPath}>{fullPath}</span>
                          <div className="profile-link-profile-actions" role="group" aria-label={`Actions for ${profileLabel(profile)}`}>
                            <button
                              type="button"
                              className="profile-utility-btn"
                              onClick={() => void syncLinks("pull", profileLinks)}
                              disabled={!onSyncLink || profileLinks.length === 0 || busy || saving || !!runningLinkAction}
                              title={`Pull ${profileLabel(profile)} from linked storage`}
                              aria-label={`Pull ${profileLabel(profile)} from linked storage`}
                              aria-busy={runningLinkAction?.profile === profile.id && runningLinkAction.direction === "pull"}
                            >
                              <Icon name="download" size={14} />
                            </button>
                            <button
                              type="button"
                              className="profile-utility-btn"
                              onClick={() => void syncLinks("push", profileLinks)}
                              disabled={!onSyncLink || profileLinks.length === 0 || busy || saving || !!runningLinkAction}
                              title={`Push ${profileLabel(profile)} to linked storage`}
                              aria-label={`Push ${profileLabel(profile)} to linked storage`}
                              aria-busy={runningLinkAction?.profile === profile.id && runningLinkAction.direction === "push"}
                            >
                              <Icon name="upload" size={14} />
                            </button>
                            <button
                              type="button"
                              className={`profile-utility-btn${editingProfile === profile.id ? " active" : ""}`}
                              onClick={() => configureProfile(profile.id)}
                              disabled={saving}
                              title={`Profile settings for ${profileLabel(profile)}`}
                              aria-label={`Profile settings for ${profileLabel(profile)}`}
                            >
                              <Icon name="settings" size={13} />
                            </button>
                            <button
                              type="button"
                              className="profile-utility-btn profile-remove-btn"
                              onClick={() => removeProfile(profile.id)}
                              disabled={saving}
                              title={`Remove ${profileLabel(profile)} from dashboard; files stay on disk`}
                              aria-label={`Remove ${profileLabel(profile)} from dashboard; files stay on disk`}
                            >
                              <Icon name="trash" size={13} />
                            </button>
                          </div>
                        </div>
                      </div>

                      <div className="profile-link-connections">
                        <div className="profile-link-connections-label">Linked storage</div>
                        {profileLinks.length === 0 && (
                          <div className="profile-link-no-storage">No storage linked yet.</div>
                        )}
                        {profileLinks.map((link) => {
                          const storage = storages.find((candidate) => candidate.id === link.storage);
                          if (!storage) {
                            // A link whose storage was removed from settings:
                            // show it instead of hiding it, with a way out.
                            return (
                              <div key={link.storage} className="storage-link-block">
                                <div className="storage-link-row">
                                  <div className="storage-link-main">
                                    <span className="storage-link-icon">
                                      <Icon name="alert-triangle" size={23} />
                                    </span>
                                    <span className="storage-link-copy">
                                      <strong>Missing storage</strong>
                                      <span className="storage-link-cloud stale">
                                        {`Storage '${link.storage}' is no longer in settings — unlink to clean up`}
                                      </span>
                                      <CloudLinkLine cloud={link.cloud} />
                                    </span>
                                  </div>
                                  <div className="storage-link-actions">
                                    <button
                                      type="button"
                                      className="btn-link storage-remove"
                                      onClick={() =>
                                        setLinks((prev) =>
                                          prev.filter((l) => !(l.profile === link.profile && l.storage === link.storage)),
                                        )
                                      }
                                    >
                                      Unlink
                                    </button>
                                  </div>
                                </div>
                              </div>
                            );
                          }
                          const settingsOpen =
                            linkSettingsOpen?.profile === profile.id && linkSettingsOpen.storage === storage.id;

                          return (
                            <div
                              key={storage.id}
                              className="storage-link-block expanded"
                            >
                              <div className="storage-link-row">
                                <div className="storage-link-storage-section">
                                  <div className="storage-link-main">
                                    <span className="storage-link-icon">
                                      <Icon name={storage.kind === "local" ? "drive" : "cloud"} size={23} />
                                    </span>
                                    <span className="storage-link-copy">
                                      <strong>{storage.name || "(unnamed)"}</strong>
                                      <span title={storageSubtitle(storage)}>{storageSubtitle(storage)}</span>
                                    </span>
                                  </div>

                                  <button
                                    type="button"
                                    className="storage-link-configure"
                                    onClick={(event) => {
                                      event.stopPropagation();
                                      configureStorage(storage.id);
                                    }}
                                    title={`Configure ${storage.name || "storage"}`}
                                    aria-label={`Configure ${storage.name || "storage"}`}
                                  >
                                    <Icon name="settings" size={16} />
                                  </button>
                                </div>

                                <div className="storage-link-profile-section">
                                  <CloudProfileSummary cloud={link.cloud} probe={storageProbes[storage.id]} />
                                  <button
                                    type="button"
                                    className={`storage-link-profile-settings${settingsOpen ? " active" : ""}`}
                                    onClick={() => {
                                      if (settingsOpen) {
                                        setLinkSettingsOpen(null);
                                      } else {
                                        const next = { profile: profile.id, storage: storage.id };
                                        setSelected(next);
                                        setLinkSettingsOpen(next);
                                      }
                                    }}
                                    title={settingsOpen ? "Hide cloud profile settings" : "Choose cloud profile"}
                                    aria-label={settingsOpen ? "Hide cloud profile settings" : "Choose cloud profile"}
                                    aria-expanded={settingsOpen}
                                  >
                                    <Icon name="settings" size={16} />
                                  </button>
                                </div>

                                <div className="storage-link-actions">
                                  {isEmptyProfile ? (
                                    <button
                                      type="button"
                                      className="storage-link-sync"
                                      disabled={!onSetupLink || setupBusy || busy || saving || !!runningLinkAction}
                                      onClick={(event) => {
                                        event.stopPropagation();
                                        void setupLink(link);
                                      }}
                                      title="Create this profile, pull its cloud files, install the agent CLI if needed, and restore plugins"
                                      aria-label={`Set up ${profileLabel(profile)} from ${storage.name || "storage"}`}
                                      aria-busy={setupBusy}
                                    >
                                      <Icon name="download" size={16} />
                                      {setupBusy ? "Setting up…" : "Set up"}
                                    </button>
                                  ) : (
                                    <button
                                      type="button"
                                      className="storage-link-sync"
                                      disabled={busy || saving || !!runningLinkAction}
                                      onClick={(event) => {
                                        event.stopPropagation();
                                        void syncLinks("pull", [link]);
                                      }}
                                      title="Get files from this storage"
                                    >
                                      <Icon name="download" size={16} />
                                      Pull
                                    </button>
                                  )}
                                  <button
                                    type="button"
                                    className="storage-link-sync"
                                    disabled={busy || saving || !!runningLinkAction}
                                    onClick={(event) => {
                                      event.stopPropagation();
                                      void syncLinks("push", [link]);
                                    }}
                                    title="Send local changes to this storage"
                                  >
                                    <Icon name="upload" size={16} />
                                    Push
                                  </button>
                                  <button
                                    type="button"
                                    className="storage-link-sync"
                                    disabled={!onRepairProfile || busy || saving || setupBusy || !!runningLinkAction}
                                    onClick={(event) => {
                                      event.stopPropagation();
                                      void repairProfile(profile);
                                    }}
                                    title={`Restore missing ${profile.root === ".codex" ? "Codex" : "Claude"} plugins into this profile`}
                                  >
                                    <Icon name="refresh" size={15} />
                                    Repair
                                  </button>
                                </div>
                              </div>

                              {settingsOpen && selectedLink && selectedProfile && selectedStorage && (
                                <div className="storage-link-detail">
                                  <div className="storage-link-settings">
                                    <label className="storage-link-setting-field">
                                      <span className="storage-link-setting-label">Cloud profile</span>
                                      <CloudProfileSelect
                                        root={selectedProfile.root}
                                        probe={storageProbes[selectedStorage.id]}
                                        holders={cloudHolders(selectedStorage.id, selectedProfile.id)}
                                        currentId={selectedLink.cloud?.profile_id}
                                        onPick={repickCloud}
                                      />
                                    </label>
                                    <label className="storage-link-setting-field">
                                      <span className="storage-link-setting-label">
                                        Cloud path
                                        <small>Optional</small>
                                      </span>
                                      <input
                                        className="form-input"
                                        type="text"
                                        value={pinInput}
                                        onChange={(event) => setPinInput(event.target.value)}
                                        placeholder="Automatic"
                                        spellCheck={false}
                                        autoComplete="off"
                                      />
                                      <span className="storage-link-setting-help">Keep Automatic unless you need a fixed cloud path.</span>
                                    </label>
                                    <div className="storage-link-settings-actions">
                                      {onSetupLink && (
                                        <button
                                          type="button"
                                          className="btn"
                                          onClick={() => void setupSelected()}
                                          disabled={setupBusy || busy || saving}
                                        >
                                          {setupBusy ? "Setting up…" : "Set up"}
                                        </button>
                                      )}
                                      <button
                                        type="button"
                                        className="btn-link storage-remove"
                                        onClick={unlinkSelected}
                                      >
                                        Unlink
                                      </button>
                                    </div>
                                    {!selectedLink.cloud?.profile_id && (
                                      <span className="form-hint storage-link-cloud-hint">
                                        The first pull or push discovers or creates the cloud profile.
                                      </span>
                                    )}
                                  </div>
                                </div>
                              )}
                            </div>
                          );
                        })}

                        {linkingProfile === profile.id && !linkingStorage && (
                          <div className="storage-link-picker" role="group" aria-label="Choose storage">
                            <span>Choose storage</span>
                            {availableStorages.map((storage) => (
                              <button
                                key={storage.id}
                                type="button"
                                onClick={() => setLinkingStorage(storage.id)}
                              >
                                <Icon name={storage.kind === "local" ? "drive" : "cloud"} size={17} />
                                <span>{storage.name || "(unnamed)"}</span>
                                <Icon name="chevron-right" size={15} />
                              </button>
                            ))}
                          </div>
                        )}
                        {linkingProfile === profile.id && linkingStorage && (
                          <div className="storage-link-picker" role="group" aria-label="Choose cloud profile">
                            <span>
                              {`Choose the cloud profile in ${storages.find((s) => s.id === linkingStorage)?.name || "storage"} to sync with`}
                            </span>
                            <CloudProfilePicker
                              root={profile.root}
                              probe={storageProbes[linkingStorage]}
                              holders={cloudHolders(linkingStorage)}
                              onPick={(info) =>
                                addLink(profile.id, linkingStorage, {
                                  root: info.root,
                                  profile_id: info.profile_id,
                                  profile_label: info.label,
                                  pinned: true,
                                })
                              }
                              onCreateNew={() => addLink(profile.id, linkingStorage, createNewCloud(linkingStorage, profile.root))}
                            />
                            <button type="button" onClick={() => setLinkingStorage(null)}>
                              ‹ Back
                            </button>
                          </div>
                        )}

                        <button
                          type="button"
                          className="profile-link-another"
                          onClick={() => beginLink(profile.id)}
                          disabled={availableStorages.length === 0 && storages.length > 0}
                        >
                          <Icon name="plus" size={16} />
                          Link another storage
                        </button>
                      </div>
                    </article>
                  );
                })}
              </div>
            )}
          </section>

          {editedStorage && (
            <section
              ref={storageEditorRef}
              className="settings-section settings-editor-panel"
              aria-label="Storage settings"
            >
              <div className="settings-editor-heading">
                <h2 className="settings-section-title">
                  Storage — {editedStorage.name || "(unnamed)"}
                  {!storageIsConfigured(editedStorage) && <span>Not configured</span>}
                </h2>
                <button
                  type="button"
                  className="btn btn-ghost settings-editor-close"
                  onClick={() => setEditingStorage(null)}
                  aria-label="Close storage settings"
                >
                  <Icon name="x" size={14} />
                </button>
              </div>
              <StorageEditor
                storage={editedStorage}
                onChange={(next) =>
                  setStorages((prev) => prev.map((storage) => (storage.id === next.id ? next : storage)))
                }
                onRemove={() => removeStorage(editedStorage.id)}
              />
            </section>
          )}

          {editedProfile && (
            <section
              ref={profileEditorRef}
              className="settings-section settings-editor-panel"
              aria-label="Profile settings"
            >
              <div className="settings-editor-heading">
                <h2 className="settings-section-title">Profile — {profileLabel(editedProfile)}</h2>
                <button
                  type="button"
                  className="btn btn-ghost settings-editor-close"
                  onClick={() => setEditingProfile(null)}
                  aria-label="Close profile settings"
                >
                  <Icon name="x" size={14} />
                </button>
              </div>
              <ProfileEditor
                profile={editedProfile}
                isDefault={editedProfile.id === "codex" || editedProfile.id === "claude"}
                pathInputRef={profilePathInputRef}
                onChange={(next) =>
                  setProfiles((prev) => prev.map((profile) => (profile.id === next.id ? next : profile)))
                }
                onRemove={() => removeProfile(editedProfile.id)}
              />
            </section>
          )}
        </div>

        {(isDirty || saved) && (
          <div className="sync-panel-footer">
            {isDirty && (
              <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
                {saving ? "Saving…" : "Save changes"}
              </button>
            )}
            {saved && <span className="save-feedback">Saved</span>}
          </div>
        )}
      </div>
    </div>
  );
}
