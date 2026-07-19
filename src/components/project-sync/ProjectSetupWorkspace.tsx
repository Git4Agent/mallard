import { useEffect, useMemo, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  BundleSnapshotSummary,
  DraftProfileSelection,
  ProjectDetail,
  ProjectProvider,
  ProjectSetupDraft,
  ProviderProfileSummary,
  SetupDraftInspection,
  SetupSectionStatus,
  StorageConfigV3,
} from "../../types";
import Icon from "../Icons";
import ResourceInventory from "./ResourceInventory";
import { projectSyncApi } from "./api";
import {
  compactProjectPath,
  errorMessage,
  formatRelativeTime,
  inventoryResources,
  recipeSelection,
} from "./model";

export type SetupCompletion = "open" | "push" | "pull";

interface Props {
  draftId: string;
  profiles: ProviderProfileSummary[];
  storages: StorageConfigV3[];
  busy: boolean;
  onClose: () => void;
  onDiscard: (draftId: string) => void;
  onFinalized: (detail: ProjectDetail, completion: SetupCompletion) => void;
}

type SaveState = "idle" | "saving" | "saved" | "error";

const AUTOSAVE_DELAY_MS = 700;

function sectionState(sections: SetupSectionStatus[], id: string): SetupSectionStatus | null {
  return sections.find((section) => section.section === id) ?? null;
}

function stateBadge(state?: string | null): { label: string; className: string } {
  if (state === "ready") return { label: "Ready", className: "ready" };
  if (state === "attention") return { label: "Check", className: "attention" };
  if (state === "blocked") return { label: "Blocked", className: "blocked" };
  return { label: "…", className: "pending" };
}

export default function ProjectSetupWorkspace({
  draftId,
  profiles,
  storages,
  busy,
  onClose,
  onDiscard,
  onFinalized,
}: Props) {
  const [draft, setDraft] = useState<ProjectSetupDraft | null>(null);
  const [inspection, setInspection] = useState<SetupDraftInspection | null>(null);
  const [inspecting, setInspecting] = useState(false);
  const [saveState, setSaveState] = useState<SaveState>("idle");
  const [error, setError] = useState<string | null>(null);
  const [finalizing, setFinalizing] = useState(false);
  const [resourcesOpen, setResourcesOpen] = useState(false);
  const [remoteBundles, setRemoteBundles] = useState<BundleSnapshotSummary[]>([]);
  const [bundlesLoading, setBundlesLoading] = useState(false);
  const [bundlesError, setBundlesError] = useState<string | null>(null);

  // Autosave bookkeeping: edits mark the draft dirty; one debounced save
  // persists the latest state and refreshes inspection afterwards.
  const dirtyRef = useRef(false);
  const savingRef = useRef(false);
  const inspectRequest = useRef(0);
  const repoAutoSelected = useRef(false);

  const refreshInspection = async (id: string) => {
    const requestId = ++inspectRequest.current;
    setInspecting(true);
    try {
      const next = await projectSyncApi.inspectSetupDraft(id);
      if (requestId !== inspectRequest.current) return;
      setInspection(next);
    } catch (reason) {
      if (requestId === inspectRequest.current) setError(errorMessage(reason));
    } finally {
      if (requestId === inspectRequest.current) setInspecting(false);
    }
  };

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const loaded = await projectSyncApi.getSetupDraft(draftId);
        if (cancelled) return;
        if (!loaded) {
          setError("This setup draft no longer exists.");
          return;
        }
        setDraft(loaded);
        void refreshInspection(draftId);
      } catch (reason) {
        if (!cancelled) setError(errorMessage(reason));
      }
    })();
    return () => {
      cancelled = true;
      inspectRequest.current += 1;
    };
    // The draft page owns one draft for its lifetime.
  }, [draftId]);

  // Debounced autosave whenever an edit lands.
  useEffect(() => {
    if (!draft || !dirtyRef.current || savingRef.current) return;
    const timer = window.setTimeout(() => {
      void (async () => {
        if (!dirtyRef.current || savingRef.current) return;
        savingRef.current = true;
        dirtyRef.current = false;
        setSaveState("saving");
        try {
          const saved = await projectSyncApi.updateSetupDraft(draft);
          setSaveState("saved");
          setError(null);
          // Keep local edits that raced the save; only adopt server-owned
          // fields and the advanced revision.
          setDraft((current) => current
            ? {
              ...current,
              revision: saved.revision,
              canonical_project_root: saved.canonical_project_root,
              repository_fingerprint: saved.repository_fingerprint,
              updated_at: saved.updated_at,
            }
            : saved);
          void refreshInspection(draftId);
        } catch (reason) {
          setSaveState("error");
          setError(errorMessage(reason));
        } finally {
          savingRef.current = false;
        }
      })();
    }, AUTOSAVE_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [draft, draftId]);

  const edit = (mutate: (current: ProjectSetupDraft) => ProjectSetupDraft) => {
    dirtyRef.current = true;
    setSaveState("saving");
    setDraft((current) => (current ? mutate(current) : current));
  };

  const selectedStorage: StorageConfigV3 | null = useMemo(() => {
    if (!draft?.storage) return null;
    if (draft.storage.kind === "existing") {
      const id = draft.storage.storage_id;
      return storages.find((storage) => storage.id === id) ?? null;
    }
    return draft.storage.storage;
  }, [draft?.storage, storages]);
  const storageIsPending = draft?.storage?.kind === "pending";

  // Remote repositories for the selected existing storage. Pending storage
  // is not in the saved config yet, so its repos cannot be listed; the
  // repository choice stays "new repo" until setup finishes.
  const listableStorageId = draft?.storage?.kind === "existing" ? draft.storage.storage_id : null;
  useEffect(() => {
    let cancelled = false;
    if (!listableStorageId) {
      setRemoteBundles([]);
      setBundlesError(null);
      setBundlesLoading(false);
      return () => { cancelled = true; };
    }
    setBundlesLoading(true);
    setBundlesError(null);
    void projectSyncApi.listRemoteBundleSnapshots(listableStorageId)
      .then((bundles) => {
        if (cancelled) return;
        setRemoteBundles(bundles);
      })
      .catch((reason) => {
        if (cancelled) return;
        setRemoteBundles([]);
        setBundlesError(errorMessage(reason));
      })
      .finally(() => {
        if (!cancelled) setBundlesLoading(false);
      });
    return () => { cancelled = true; };
  }, [listableStorageId]);

  // Auto-select a single exact Git match once, without overriding an
  // explicit user choice; otherwise the default stays "create new repo".
  useEffect(() => {
    if (!draft || repoAutoSelected.current || bundlesLoading) return;
    if (draft.repository.kind !== "new" || !draft.repository_fingerprint) return;
    const matches = remoteBundles.filter((bundle) => (
      bundle.repository_fingerprint === draft.repository_fingerprint
    ));
    if (matches.length === 1 && listableStorageId) {
      repoAutoSelected.current = true;
      edit((current) => ({
        ...current,
        repository: {
          kind: "existing",
          storage_id: listableStorageId,
          bundle_id: matches[0].bundle_id,
          display_name: matches[0].display_name,
          repository_fingerprint: matches[0].repository_fingerprint ?? null,
          mismatch_acknowledged: false,
        },
      }));
    }
  }, [bundlesLoading, draft, listableStorageId, remoteBundles]);

  // Adopt the discovered default selection the first time discovery runs.
  useEffect(() => {
    if (!draft || !inspection?.inventory || !inspection.fresh_discovery_signature) return;
    if (draft.discovery_signature) return;
    const defaults = recipeSelection(inspection.inventory.recipe);
    edit((current) => ({
      ...current,
      selected_resource_ids: [...defaults].sort(),
      discovery_signature: inspection.fresh_discovery_signature ?? "",
    }));
  }, [draft, inspection]);

  const sections = inspection?.sections ?? [];
  const resources = useMemo(() => inventoryResources(inspection?.inventory ?? null), [inspection?.inventory]);
  const selectedResources = useMemo(() => new Set(draft?.selected_resource_ids ?? []), [draft?.selected_resource_ids]);
  const usesExistingRepo = draft?.repository.kind === "existing";
  const exactMatches = useMemo(() => (
    draft?.repository_fingerprint
      ? remoteBundles.filter((bundle) => bundle.repository_fingerprint === draft.repository_fingerprint)
      : []
  ), [draft?.repository_fingerprint, remoteBundles]);

  const chooseProfileFolder = async (provider: ProjectProvider) => {
    const picked = await open({ directory: true, multiple: false });
    if (typeof picked !== "string" || !picked) return;
    edit((current) => ({
      ...current,
      profiles: {
        ...current.profiles,
        [provider]: { kind: "pending", path: picked, display_name: "" } satisfies DraftProfileSelection,
      },
    }));
  };

  const changeProjectFolder = async () => {
    const picked = await open({ directory: true, multiple: false });
    if (typeof picked !== "string" || !picked) return;
    edit((current) => ({ ...current, project_root: picked }));
  };

  const addLocalStorageInline = async () => {
    const picked = await open({ directory: true, multiple: false });
    if (typeof picked !== "string" || !picked) return;
    const suffix = Math.random().toString(16).slice(2, 10);
    edit((current) => ({
      ...current,
      repository: { kind: "new" },
      storage: {
        kind: "pending",
        storage: {
          id: `storage-${suffix}`,
          name: picked.split("/").filter(Boolean).pop() ?? "Local storage",
          kind: "local",
          bucket: "",
          access_key_id: "",
          secret_access_key: "",
          account_id: "",
          s3_endpoint: "",
          region: "",
          local_dir: picked,
          included_default_exclusions: [],
          supports_conditional_writes: null,
        },
      },
    }));
  };

  const finalize = async (completion: SetupCompletion) => {
    if (!draft) return;
    // Flush any pending edit before pinning the reviewed revision.
    if (dirtyRef.current && !savingRef.current) {
      savingRef.current = true;
      dirtyRef.current = false;
      try {
        const saved = await projectSyncApi.updateSetupDraft(draft);
        setDraft((current) => current ? { ...current, revision: saved.revision } : saved);
        draft.revision = saved.revision;
      } catch (reason) {
        setError(errorMessage(reason));
        savingRef.current = false;
        return;
      }
      savingRef.current = false;
    }
    setFinalizing(true);
    setError(null);
    try {
      const detail = await projectSyncApi.finalizeProjectSetup(draftId, draft.revision);
      onFinalized(detail, completion);
    } catch (reason) {
      setError(errorMessage(reason));
      // The draft may carry a recorded error and a bumped revision now.
      try {
        const reloaded = await projectSyncApi.getSetupDraft(draftId);
        if (reloaded) setDraft(reloaded);
      } catch {
        // The original error is already shown.
      }
    } finally {
      setFinalizing(false);
    }
  };

  if (!draft) {
    return (
      <section className="v3-inline-new-project v3-setup-workspace" role="region" aria-label="Project setup">
        {error
          ? <div className="v3-callout error"><Icon name="alert-triangle" size={15} /> {error}</div>
          : <div className="v3-storage-repository-state"><span className="status-loader" /> Loading setup draft…</div>}
      </section>
    );
  }

  const projectSection = sectionState(sections, "project");
  const profilesSection = sectionState(sections, "profiles");
  const storageSection = sectionState(sections, "storage");
  const repositorySection = sectionState(sections, "repository");
  const resourcesSection = sectionState(sections, "resources");
  const working = busy || finalizing;
  const canFinalize = !!inspection?.can_finalize && !working && saveState !== "saving" && saveState !== "error";

  const profileRow = (provider: ProjectProvider, label: string) => {
    const selection = draft.profiles[provider] ?? null;
    const options = profiles.filter((profile) => profile.provider === provider);
    const existingProfile = selection?.kind === "existing"
      ? options.find((profile) => profile.profile_id === selection.profile_id) ?? null
      : null;
    const value = selection?.kind === "existing"
      ? selection.profile_id
      : selection?.kind === "pending" ? "__pending" : "";
    const path = selection?.kind === "pending" ? selection.path : existingProfile?.path ?? "";
    const inputId = `setup-${draftId}-${provider}-path`;
    return (
      <div className="v3-agent-profile-row" key={provider}>
        <label className="v3-agent-profile-label" htmlFor={inputId}>
          <strong>{label}</strong>
          <small>machine-local</small>
        </label>
        <select
          value={value}
          disabled={working}
          aria-label={`${label} profile`}
          onChange={(event) => {
            const next = event.target.value;
            edit((current) => {
              const nextProfiles = { ...current.profiles };
              if (!next || next === "__pending") delete nextProfiles[provider];
              else nextProfiles[provider] = { kind: "existing", profile_id: next };
              return { ...current, profiles: nextProfiles };
            });
          }}
        >
          <option value="">Not used</option>
          {selection?.kind === "pending" && <option value="__pending">Custom path</option>}
          {options.map((profile) => (
            <option key={profile.profile_id} value={profile.profile_id} disabled={!profile.available || !profile.readable}>
              {profile.display_name}{!profile.available || !profile.readable ? " (unavailable)" : ""}
            </option>
          ))}
        </select>
        <input
          id={inputId}
          value={path}
          disabled={working}
          placeholder={`Enter ${label} home path`}
          spellCheck={false}
          onChange={(event) => {
            const nextPath = event.target.value;
            edit((current) => {
              const nextProfiles = { ...current.profiles };
              if (nextPath) {
                nextProfiles[provider] = { kind: "pending", path: nextPath, display_name: "" };
              } else {
                delete nextProfiles[provider];
              }
              return { ...current, profiles: nextProfiles };
            });
          }}
        />
        <button type="button" className="btn" onClick={() => void chooseProfileFolder(provider)} disabled={working}>
          <Icon name="folder" size={13} /> Browse…
        </button>
      </div>
    );
  };

  return (
    <section className="v3-inline-new-project v3-setup-workspace" role="region" aria-labelledby="v3-setup-title">
      <header className="v3-modal-header">
        <div>
          <span className="v3-eyebrow">Project setup</span>
          <h1 id="v3-setup-title">Set up {draft.display_name || "project"}</h1>
          <p>Saved automatically. Nothing changes until you finish setup.</p>
        </div>
        <button type="button" className="btn btn-ghost" onClick={onClose} disabled={finalizing} aria-label="Close setup; the draft is kept">
          <Icon name="x" size={17} />
        </button>
      </header>

      <div className="v3-modal-body">
        {draft.last_error && (
          <div className="v3-callout warning">
            <Icon name="alert-triangle" size={15} />
            <span>The last finish attempt failed: {draft.last_error}</span>
          </div>
        )}

        {/* Project */}
        <section className="v3-form-card v3-setup-section">
          <div className="v3-card-heading">
            <div>
              <strong>Project</strong>
              <span title={draft.project_root}>{compactProjectPath(draft.project_root)}</span>
            </div>
            <span className={`v3-setup-state ${stateBadge(projectSection?.state).className}`}>
              {stateBadge(projectSection?.state).label}
            </span>
          </div>
          {projectSection?.message && <div className="v3-callout error"><Icon name="alert-triangle" size={15} /> {projectSection.message}</div>}
          <div className="v3-field-grid three">
            <label>
              <span>Project name</span>
              <input
                value={draft.display_name}
                disabled={working || usesExistingRepo}
                onChange={(event) => edit((current) => ({ ...current, display_name: event.target.value }))}
              />
            </label>
            <label>
              <span>Folder</span>
              <div className="v3-profile-select-row">
                <input value={compactProjectPath(draft.project_root)} readOnly title={draft.project_root} />
                <button type="button" className="btn" onClick={() => void changeProjectFolder()} disabled={working}>Change…</button>
              </div>
            </label>
            <label>
              <span>Git fingerprint</span>
              <input value={draft.repository_fingerprint ? `${draft.repository_fingerprint.slice(0, 16)}…` : "No Git remote"} readOnly />
            </label>
          </div>
          {usesExistingRepo && (
            <small className="v3-setup-hint">The project name follows the connected remote repo.</small>
          )}
        </section>

        {/* Agent profiles */}
        <section className="v3-form-card v3-setup-section">
          <div className="v3-card-heading">
            <div>
              <strong>Agent profiles</strong>
              <span>Choose the agent homes used by this project.</span>
            </div>
            <span className={`v3-setup-state ${stateBadge(profilesSection?.state).className}`}>
              {stateBadge(profilesSection?.state).label}
            </span>
          </div>
          {profilesSection?.message && <div className="v3-callout error"><Icon name="alert-triangle" size={15} /> {profilesSection.message}</div>}
          <div className="v3-agent-profile-list">
            {profileRow("codex", "Codex")}
            {profileRow("claude", "Claude")}
          </div>
        </section>

        {/* Storage */}
        <section className="v3-form-card v3-setup-section">
          <div className="v3-card-heading">
            <div>
              <strong>Storage</strong>
              <span>Choose one destination. Add more later from project settings.</span>
            </div>
            <span className={`v3-setup-state ${stateBadge(storageSection?.state).className}`}>
              {stateBadge(storageSection?.state).label}
            </span>
          </div>
          {storageSection?.message && storageSection.state === "blocked" && (
            <div className="v3-callout error"><Icon name="alert-triangle" size={15} /> {storageSection.message}</div>
          )}
          <div className="v3-profile-select-row">
            <select
              value={draft.storage?.kind === "existing" ? draft.storage.storage_id : draft.storage?.kind === "pending" ? "__pending" : ""}
              disabled={working}
              onChange={(event) => {
                const next = event.target.value;
                edit((current) => ({
                  ...current,
                  // Repository choices are storage-specific.
                  repository: { kind: "new" },
                  storage: !next || next === "__pending"
                    ? (next === "__pending" ? current.storage : null)
                    : { kind: "existing", storage_id: next },
                }));
              }}
            >
              <option value="">No storage yet (finish locally)</option>
              {storageIsPending && selectedStorage && (
                <option value="__pending">{selectedStorage.name} (new local folder)</option>
              )}
              {storages.map((storage) => (
                <option key={storage.id} value={storage.id}>
                  {storage.name || "(unnamed)"} · {storage.kind === "local" ? "local folder" : "S3"}
                </option>
              ))}
            </select>
            <button type="button" className="btn" onClick={() => void addLocalStorageInline()} disabled={working}>
              <Icon name="plus" size={13} /> Local folder…
            </button>
          </div>
          <small className="v3-setup-hint">
            {storageIsPending && selectedStorage
              ? `${selectedStorage.local_dir} · added when setup finishes`
              : "S3 storage is added from Storage settings, then selected here."}
          </small>
        </section>

        {/* Repository */}
        <section className="v3-form-card v3-setup-section">
          <div className="v3-card-heading">
            <div>
              <strong>Repository</strong>
              <span>
                {usesExistingRepo
                  ? "Continue an existing remote repo; Pull review runs after setup."
                  : "A new remote repo is published on first push."}
              </span>
            </div>
            <span className={`v3-setup-state ${stateBadge(repositorySection?.state).className}`}>
              {stateBadge(repositorySection?.state).label}
            </span>
          </div>
          {repositorySection?.message && repositorySection.state === "blocked" && (
            <div className="v3-callout error"><Icon name="alert-triangle" size={15} /> {repositorySection.message}</div>
          )}
          {!listableStorageId ? (
            <div className="v3-inline-empty">
              {storageIsPending
                ? "New storage has no repos yet; this project starts a new repo."
                : "Link a storage to look for existing repos."}
            </div>
          ) : bundlesLoading ? (
            <div className="v3-storage-repository-state"><span className="status-loader" /> Looking for existing repos…</div>
          ) : bundlesError ? (
            <div className="v3-callout error"><Icon name="alert-triangle" size={15} /> {bundlesError}</div>
          ) : (
            <div className="v3-bundle-match-options" role="radiogroup" aria-label="Repo identity for this project">
              <button
                type="button"
                role="radio"
                aria-checked={!usesExistingRepo}
                className={`v3-bundle-match-option separate${!usesExistingRepo ? " active" : ""}`}
                onClick={() => edit((current) => ({ ...current, repository: { kind: "new" } }))}
                disabled={working}
              >
                <span className="v3-bundle-radio"><span /></span>
                <span className="v3-bundle-match-copy">
                  <strong>Create a new repo</strong>
                  <span>{remoteBundles.length === 0 ? "No repos exist in this storage yet." : "Keep this checkout separate from the repos below."}</span>
                </span>
              </button>
              {remoteBundles.map((bundle) => {
                const active = usesExistingRepo
                  && draft.repository.kind === "existing"
                  && draft.repository.bundle_id === bundle.bundle_id;
                const recommended = !!draft.repository_fingerprint
                  && bundle.repository_fingerprint === draft.repository_fingerprint
                  && exactMatches.length === 1;
                return (
                  <button
                    key={bundle.bundle_id}
                    type="button"
                    role="radio"
                    aria-checked={active}
                    className={`v3-bundle-match-option${active ? " active" : ""}`}
                    onClick={() => edit((current) => ({
                      ...current,
                      repository: {
                        kind: "existing",
                        storage_id: bundle.storage_id,
                        bundle_id: bundle.bundle_id,
                        display_name: bundle.display_name,
                        repository_fingerprint: bundle.repository_fingerprint ?? null,
                        mismatch_acknowledged: false,
                      },
                    }))}
                    disabled={working}
                  >
                    <span className="v3-bundle-radio"><span /></span>
                    <span className="v3-bundle-match-copy">
                      <strong>{bundle.display_name}{recommended && <small className="v3-bundle-recommended">Git match</small>}</strong>
                      <span>{bundle.bundle_id}</span>
                    </span>
                    <span className="v3-bundle-match-meta">gen {bundle.generation ?? "—"}<small>{bundle.resource_count ?? 0} resources · {formatRelativeTime(bundle.updated_at)}</small></span>
                  </button>
                );
              })}
            </div>
          )}
          {usesExistingRepo && draft.repository.kind === "existing"
            && !(draft.repository.repository_fingerprint
              && draft.repository_fingerprint
              && draft.repository.repository_fingerprint === draft.repository_fingerprint) && (
            <label className="v3-setup-acknowledge">
              <input
                type="checkbox"
                checked={draft.repository.mismatch_acknowledged}
                disabled={working}
                onChange={(event) => edit((current) => current.repository.kind === "existing"
                  ? { ...current, repository: { ...current.repository, mismatch_acknowledged: event.target.checked } }
                  : current)}
              />
              <span>
                This repo is not verified to belong to this checkout
                {draft.repository.repository_fingerprint ? " (different Git remote)" : " (no Git fingerprint)"}.
                Connect it anyway and review the Pull carefully.
              </span>
            </label>
          )}
        </section>

        {/* Sync contents */}
        <section className="v3-form-card v3-setup-section">
          <div className="v3-card-heading">
            <div>
              <strong>Sync contents</strong>
              <span>
                {usesExistingRepo
                  ? "The connected repo's recipe is adopted; adjust it after the first Pull."
                  : `${selectedResources.size} of ${resources.length} discovered resources selected`}
              </span>
            </div>
            <div className="v3-setup-heading-actions">
              <span className={`v3-setup-state ${stateBadge(resourcesSection?.state).className}`}>
                {stateBadge(resourcesSection?.state).label}
              </span>
              {!usesExistingRepo && (
                <button type="button" className="btn btn-ghost v3-setup-resource-toggle" onClick={() => setResourcesOpen((current) => !current)} disabled={resources.length === 0}>
                  {resourcesOpen ? "Collapse" : "Customize"}
                </button>
              )}
            </div>
          </div>
          {inspection?.selection_stale && (
            <div className="v3-callout warning">
              <Icon name="alert-triangle" size={15} />
              <span>Discovered resources changed since this selection was saved. Review the list, then save by editing it.</span>
              <button
                type="button"
                className="btn"
                disabled={working || !inspection.fresh_discovery_signature}
                onClick={() => edit((current) => ({
                  ...current,
                  discovery_signature: inspection.fresh_discovery_signature ?? current.discovery_signature,
                }))}
              >
                Accept current list
              </button>
            </div>
          )}
          {resourcesSection?.message && resourcesSection.state === "blocked" && (
            <div className="v3-callout error"><Icon name="alert-triangle" size={15} /> {resourcesSection.message}</div>
          )}
          {inspecting && !inspection?.inventory && (
            <div className="v3-storage-repository-state"><span className="status-loader" /> Discovering resources…</div>
          )}
          {!usesExistingRepo && resourcesOpen && resources.length > 0 && (
            <ResourceInventory
              resources={resources}
              selected={selectedResources}
              statuses={new Map()}
              disabled={working}
              onToggle={(resourceId) => edit((current) => {
                const next = new Set(current.selected_resource_ids);
                if (next.has(resourceId)) next.delete(resourceId);
                else next.add(resourceId);
                return {
                  ...current,
                  selected_resource_ids: [...next].sort(),
                  discovery_signature: inspection?.fresh_discovery_signature ?? current.discovery_signature,
                };
              })}
            />
          )}
        </section>

        {(inspection?.warnings?.length ?? 0) > 0 && (
          <div className="v3-callout warning">
            <Icon name="alert-triangle" size={15} />
            <span>{inspection?.warnings?.join(" ")}</span>
          </div>
        )}
        {error && <div className="v3-callout error"><Icon name="alert-triangle" size={15} /> {error}</div>}
      </div>

      <footer className="v3-modal-footer v3-setup-footer">
        <span className="v3-setup-save-state">
          {saveState === "saving" ? "Saving…" : saveState === "error" ? "Draft not saved" : "Draft saved"}
        </span>
        <div>
          <button type="button" className="btn btn-ghost" onClick={() => onDiscard(draftId)} disabled={finalizing}>
            Discard draft
          </button>
          {usesExistingRepo ? (
            <button type="button" className="btn btn-primary" disabled={!canFinalize} onClick={() => void finalize("pull")}>
              {finalizing ? "Finishing…" : "Finish and review Pull"}
            </button>
          ) : selectedStorage ? (
            <>
              <button type="button" className="btn" disabled={!canFinalize} onClick={() => void finalize("open")}>
                {finalizing ? "Finishing…" : "Finish setup"}
              </button>
              <button type="button" className="btn btn-primary" disabled={!canFinalize} onClick={() => void finalize("push")}>
                Finish and push
              </button>
            </>
          ) : (
            <button type="button" className="btn btn-primary" disabled={!canFinalize} onClick={() => void finalize("open")}>
              {finalizing ? "Finishing…" : "Finish locally"}
            </button>
          )}
        </div>
      </footer>
    </section>
  );
}
