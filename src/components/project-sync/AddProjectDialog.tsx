import { useEffect, useMemo, useState } from "react";
import type {
  BundleRecipe,
  BundleSnapshotSummary,
  ProjectProvider,
  ProjectDiscovery,
  ProviderProfile,
  ProviderProfileSummary,
  StorageConfigV3,
} from "../../types";
import Icon from "../Icons";
import ResourceInventory from "./ResourceInventory";
import { projectSyncApi } from "./api";
import { compactProjectPath, errorMessage, formatRelativeTime, inventoryResources, recipeSelection, recipeWithSelection } from "./model";

interface Props {
  inline?: boolean;
  discovery: ProjectDiscovery;
  profiles: ProviderProfileSummary[];
  storages: StorageConfigV3[];
  busy: boolean;
  error: string | null;
  onCancel: () => void;
  onCreate: (
    displayName: string,
    projectRoot: string,
    profileIds: Partial<Record<ProjectProvider, string>>,
    recipe: BundleRecipe,
    storageIds: string[],
    remoteBundle: BundleSnapshotSummary | null,
  ) => void;
  onProfilesChange: (profileIds: Partial<Record<ProjectProvider, string>>) => Promise<void> | void;
  onAddProfile: (provider: ProjectProvider) => Promise<ProviderProfile | null>;
}

export default function AddProjectDialog({ inline = false, discovery, profiles, storages, busy, error, onCancel, onCreate, onProfilesChange, onAddProfile }: Props) {
  const resources = useMemo(() => inventoryResources(discovery.inventory), [discovery.inventory]);
  const [name, setName] = useState(discovery.display_name);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [storageIds, setStorageIds] = useState<Set<string>>(new Set());
  const [profileIds, setProfileIds] = useState<Partial<Record<ProjectProvider, string>>>(discovery.profile_ids ?? {});
  const [remoteBundles, setRemoteBundles] = useState<BundleSnapshotSummary[]>([]);
  const [bundleChoice, setBundleChoice] = useState("new");
  const [bundlesLoading, setBundlesLoading] = useState(false);
  const [bundlesError, setBundlesError] = useState<string | null>(null);

  useEffect(() => {
    const saved = recipeSelection(discovery.inventory.recipe);
    setSelected(new Set(saved.size > 0
      ? [...saved]
      : resources.filter((resource) => (
        resource.default_selected !== false
        && resource.selected_by_default !== false
        && !resource.blocked_reason
      )).map((resource) => resource.resource_id)));
    setName(discovery.display_name);
    setProfileIds(discovery.profile_ids ?? {});
  }, [discovery, resources]);

  const storageKey = [...storageIds].sort().join("\n");

  useEffect(() => {
    let cancelled = false;
    const fingerprint = discovery.repository_fingerprint;
    const selectedStorageIds = storageKey ? storageKey.split("\n") : [];
    if (selectedStorageIds.length === 0) {
      setRemoteBundles([]);
      setBundleChoice("new");
      setBundlesError(null);
      setBundlesLoading(false);
      return () => { cancelled = true; };
    }
    setBundlesLoading(true);
    setBundlesError(null);
    setBundleChoice("");
    void Promise.all(selectedStorageIds.map((storageId) => (
      projectSyncApi.listRemoteBundleSnapshots(storageId)
    ))).then((pages) => {
      if (cancelled) return;
      const unique = new Map<string, BundleSnapshotSummary>();
      for (const bundle of pages.flat()) unique.set(`${bundle.storage_id}:${bundle.bundle_id}`, bundle);
      const bundles = [...unique.values()].sort((left, right) => {
        const leftMatches = !!fingerprint && left.repository_fingerprint === fingerprint;
        const rightMatches = !!fingerprint && right.repository_fingerprint === fingerprint;
        if (leftMatches !== rightMatches) return leftMatches ? -1 : 1;
        return (right.updated_at ?? 0) - (left.updated_at ?? 0);
      });
      setRemoteBundles(bundles);
      setBundleChoice(bundles.length === 0 ? "new" : "");
    }).catch((reason) => {
      if (cancelled) return;
      setRemoteBundles([]);
      setBundlesError(errorMessage(reason));
    }).finally(() => {
      if (!cancelled) setBundlesLoading(false);
    });
    return () => { cancelled = true; };
  }, [discovery.repository_fingerprint, storageKey]);

  const recipe = recipeWithSelection(discovery.inventory.recipe, resources, selected);
  const selectedRemote = remoteBundles.find((bundle) => (
    `${bundle.storage_id}:${bundle.bundle_id}` === bundleChoice
  )) ?? null;
  const selectedFingerprintDiffers = !!discovery.repository_fingerprint
    && !!selectedRemote?.repository_fingerprint
    && discovery.repository_fingerprint !== selectedRemote.repository_fingerprint;
  const selectedFingerprintUnknown = !!discovery.repository_fingerprint
    && !!selectedRemote
    && !selectedRemote.repository_fingerprint;
  const matchingBundleCount = discovery.repository_fingerprint
    ? remoteBundles.filter((bundle) => bundle.repository_fingerprint === discovery.repository_fingerprint).length
    : 0;
  const bundleChoiceReady = remoteBundles.length === 0 || bundleChoice === "new" || !!selectedRemote;
  const valid = !!name.trim()
    && !!discovery.project_root
    && Object.keys(profileIds).length > 0
    && !bundlesLoading
    && !bundlesError
    && bundleChoiceReady;

  const updateProfiles = (next: Partial<Record<ProjectProvider, string>>) => {
    setProfileIds(next);
    void onProfilesChange(next);
  };

  const addProfile = async (provider: ProjectProvider) => {
    const profile = await onAddProfile(provider);
    if (!profile) return;
    updateProfiles({ ...profileIds, [provider]: profile.profile_id });
  };

  const profileField = (provider: ProjectProvider, label: string) => {
    const options = profiles.filter((profile) => profile.provider === provider);
    const selected = profileIds[provider] ?? "";
    const selectedProfile = options.find((profile) => profile.profile_id === selected);
    return (
      <label>
        <span>{label} profile <small>machine-local</small></span>
        <div className="v3-profile-select-row">
          <select
            value={selected}
            disabled={busy}
            onChange={(event) => {
              const next = { ...profileIds };
              if (event.target.value) next[provider] = event.target.value;
              else delete next[provider];
              updateProfiles(next);
            }}
          >
            <option value="">Not used</option>
            {options.map((profile) => (
              <option key={profile.profile_id} value={profile.profile_id} disabled={!profile.available || !profile.readable}>
                {profile.display_name}{!profile.available || !profile.readable ? " (unavailable)" : ""}
              </option>
            ))}
          </select>
          <button type="button" className="btn" onClick={() => void addProfile(provider)} disabled={busy}>
            <Icon name="plus" size={13} /> Add
          </button>
        </div>
        <small title={selectedProfile?.error ?? selectedProfile?.path}>
          {selectedProfile
            ? `${compactProjectPath(selectedProfile.path)}${!selectedProfile.available || !selectedProfile.readable ? " · Unavailable" : !selectedProfile.writable ? " · Read only" : ""}`
            : `Choose the ${label} home to scan.`}
        </small>
      </label>
    );
  };

  const dialog = (
      <section
        className={inline ? "v3-inline-new-project v3-add-project-dialog" : "v3-modal v3-add-project-dialog"}
        role={inline ? "region" : "dialog"}
        aria-modal={inline ? undefined : true}
        aria-labelledby="v3-add-project-title"
      >
        <header className="v3-modal-header">
          <div>
            <span className="v3-eyebrow">New portable repo</span>
            <h1 id="v3-add-project-title">Review project resources</h1>
            <p>Only selected resources become part of this project. The repository itself is never uploaded.</p>
          </div>
          <button type="button" className="btn btn-ghost" onClick={onCancel} disabled={busy} aria-label="Cancel add project">
            <Icon name="x" size={17} />
          </button>
        </header>

        <div className="v3-modal-body">
          <section className="v3-form-card">
            <div className="v3-field-grid three">
              <label>
                <span>Project name</span>
                <input value={name} onChange={(event) => setName(event.target.value)} autoFocus />
              </label>
              <label>
                <span>Checkout on this machine</span>
                <input value={compactProjectPath(discovery.project_root)} readOnly title={discovery.project_root} />
              </label>
              <label>
                <span>Repository match</span>
                <input value={discovery.repository_fingerprint ? `${discovery.repository_fingerprint.slice(0, 16)}…` : "No Git fingerprint"} readOnly />
              </label>
            </div>
            <div className="v3-provider-home-grid">
              {profileField("codex", "Codex")}
              {profileField("claude", "Claude")}
            </div>
          </section>

          {(discovery.warnings?.length ?? 0) > 0 && (
            <div className="v3-callout warning">
              <Icon name="alert-triangle" size={15} />
              <span>{discovery.warnings?.join(" ")}</span>
            </div>
          )}

          {selectedRemote && (
            <div className="v3-callout info">
              <Icon name="cloud" size={15} />
              <span>The selected remote repo’s {selectedRemote.resource_count ?? selectedRemote.resources?.length ?? 0}-resource recipe will replace this local draft during Pull review.</span>
            </div>
          )}
          {selectedFingerprintDiffers && selectedRemote && (
            <div className="v3-callout warning">
              <Icon name="alert-triangle" size={15} />
              <span>This repo has a different Git fingerprint. Connecting will adopt its repository identity; review the Pull carefully before applying anything.</span>
            </div>
          )}
          {selectedFingerprintUnknown && (
            <div className="v3-callout warning">
              <Icon name="alert-triangle" size={15} />
              <span>This repo has no Git fingerprint, so Agent Sync cannot verify that it belongs to this checkout. Review the Pull carefully before applying anything.</span>
            </div>
          )}

          <ResourceInventory
            resources={resources}
            selected={selected}
            statuses={new Map()}
            disabled={busy || !!selectedRemote}
            onToggle={(resourceId) => setSelected((current) => {
              const next = new Set(current);
              if (next.has(resourceId)) next.delete(resourceId);
              else next.add(resourceId);
              return next;
            })}
          />

          <section className="v3-form-card">
            <div className="v3-card-heading">
              <div><strong>Storage links</strong><span>Publish the same repo identity to one or more destinations.</span></div>
            </div>
            {storages.length === 0 ? (
              <div className="v3-inline-empty">No schema-3 storage configured. Add the project now and link storage later.</div>
            ) : (
              <div className="v3-storage-checks">
                {storages.map((storage) => (
                  <label key={storage.id}>
                    <input
                      type="checkbox"
                      checked={storageIds.has(storage.id)}
                      onChange={() => setStorageIds((current) => {
                        const next = new Set(current);
                        if (next.has(storage.id)) next.delete(storage.id);
                        else next.add(storage.id);
                        return next;
                      })}
                    />
                    <Icon name={storage.kind === "local" ? "drive" : "cloud"} size={15} />
                    <span><strong>{storage.name}</strong><small>{storage.kind === "local" ? storage.local_dir : storage.bucket}</small></span>
                  </label>
                ))}
              </div>
            )}

            {bundlesLoading && (
              <div className="v3-bundle-match-status"><span className="status-loader" /> Loading available repos from selected storage…</div>
            )}
            {bundlesError && (
              <div className="v3-callout error"><Icon name="alert-triangle" size={15} /> Could not list available repos: {bundlesError}</div>
            )}
            {!bundlesLoading && remoteBundles.length > 0 && (
              <div className="v3-bundle-match-panel">
                <div className="v3-bundle-match-heading">
                  <div>
                    <strong>Choose a repo</strong>
                    <span>{discovery.repository_fingerprint
                      ? matchingBundleCount > 0
                        ? `${matchingBundleCount} matching Git ${matchingBundleCount === 1 ? "repo is" : "repos are"} recommended; every repo remains available.`
                        : "No matching Git repo was found; choose any repo or create a separate one."
                      : "No Git fingerprint was found; choose any repo or create a separate one."}</span>
                  </div>
                  <span>{remoteBundles.length} found</span>
                </div>
                <div className="v3-bundle-match-options" role="radiogroup" aria-label="Repo identity for this project">
                  {remoteBundles.map((bundle) => {
                    const key = `${bundle.storage_id}:${bundle.bundle_id}`;
                    const storage = storages.find((candidate) => candidate.id === bundle.storage_id);
                    const active = bundleChoice === key;
                    const recommended = !!discovery.repository_fingerprint
                      && bundle.repository_fingerprint === discovery.repository_fingerprint;
                    return (
                      <button
                        key={key}
                        type="button"
                        role="radio"
                        aria-checked={active}
                        className={`v3-bundle-match-option${active ? " active" : ""}`}
                        onClick={() => setBundleChoice(key)}
                        disabled={busy}
                      >
                        <span className="v3-bundle-radio"><span /></span>
                        <span className="v3-bundle-match-copy">
                          <strong>{bundle.display_name}{recommended && <small className="v3-bundle-recommended">Recommended</small>}</strong>
                          <span>{storage?.name ?? "Storage"} · {bundle.bundle_id}</span>
                        </span>
                        <span className="v3-bundle-match-meta">gen {bundle.generation ?? "—"}<small>{bundle.resource_count ?? bundle.resources?.length ?? 0} resources · {formatRelativeTime(bundle.updated_at)}</small></span>
                      </button>
                    );
                  })}
                  <button
                    type="button"
                    role="radio"
                    aria-checked={bundleChoice === "new"}
                    className={`v3-bundle-match-option separate${bundleChoice === "new" ? " active" : ""}`}
                    onClick={() => setBundleChoice("new")}
                    disabled={busy}
                  >
                    <span className="v3-bundle-radio"><span /></span>
                    <span className="v3-bundle-match-copy">
                      <strong>Create a separate repo</strong>
                      <span>This checkout will not share remote history with the repos above.</span>
                    </span>
                  </button>
                </div>
              </div>
            )}
          </section>

          {error && <div className="v3-callout error"><Icon name="alert-triangle" size={15} /> {error}</div>}
        </div>

        <footer className="v3-modal-footer">
          <span>{selectedRemote ? `Continue ${selectedRemote.display_name} · generation ${selectedRemote.generation ?? "—"}` : `${selected.size} resources selected`}</span>
          <div>
            <button type="button" className="btn" onClick={onCancel} disabled={busy}>Cancel</button>
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy || !valid}
              onClick={() => onCreate(
                name.trim(),
                discovery.project_root,
                profileIds,
                recipe,
                [...storageIds],
                selectedRemote,
              )}
            >
              {busy ? "Creating…" : selectedRemote ? "Connect and review Pull" : "Create project"}
            </button>
          </div>
        </footer>
      </section>
  );

  if (inline) return dialog;
  return (
    <div className="v3-modal-backdrop" role="presentation">
      {dialog}
    </div>
  );
}
