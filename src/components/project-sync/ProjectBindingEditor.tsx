import { useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import type { ProjectProvider, ProviderProfile, ProviderProfileSummary } from "../../types";
import Icon from "../Icons";
import { compactProjectPath } from "./model";

export interface ProjectBindingDraft {
  local_project_id?: string;
  bundle_id: string;
  project_root: string;
  profile_ids: Partial<Record<ProjectProvider, string>>;
  expected_revision?: number | null;
}

interface Props {
  title: string;
  description: string;
  binding: ProjectBindingDraft;
  busy: boolean;
  error?: string | null;
  actionLabel: string;
  profiles: ProviderProfileSummary[];
  requiredProviders?: ProjectProvider[];
  onAddProfile: (provider: ProjectProvider) => Promise<ProviderProfile | null>;
  onCancel: () => void;
  onSubmit: (binding: ProjectBindingDraft) => void;
}

export default function ProjectBindingEditor({
  title,
  description,
  binding,
  busy,
  error,
  actionLabel,
  profiles,
  requiredProviders = [],
  onAddProfile,
  onCancel,
  onSubmit,
}: Props) {
  const [projectRoot, setProjectRoot] = useState(binding.project_root);
  const [profileIds, setProfileIds] = useState<Partial<Record<ProjectProvider, string>>>(binding.profile_ids ?? {});

  const chooseFolder = async () => {
    const picked = await open({ directory: true, multiple: false });
    if (typeof picked === "string" && picked) setProjectRoot(picked);
  };

  const next: ProjectBindingDraft = {
    ...binding,
    project_root: projectRoot,
    profile_ids: profileIds,
  };
  const profilesComplete = Object.keys(profileIds).length > 0
    && requiredProviders.every((provider) => !!profileIds[provider]);

  const addProfile = async (provider: ProjectProvider) => {
    const profile = await onAddProfile(provider);
    if (profile) setProfileIds((current) => ({ ...current, [provider]: profile.profile_id }));
  };

  const profileField = (provider: ProjectProvider, label: string) => {
    const options = profiles.filter((profile) => profile.provider === provider);
    const selected = profileIds[provider] ?? "";
    const selectedProfile = options.find((profile) => profile.profile_id === selected);
    return (
      <label>
        <span>{label} profile <small>{requiredProviders.includes(provider) ? "required" : "this machine"}</small></span>
        <div className="v3-profile-select-row">
          <select
            value={selected}
            disabled={busy}
            onChange={(event) => setProfileIds((current) => {
              const nextIds = { ...current };
              if (event.target.value) nextIds[provider] = event.target.value;
              else delete nextIds[provider];
              return nextIds;
            })}
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
            : `Choose the ${label} home used by this project.`}
        </small>
      </label>
    );
  };

  return (
    <div className="v3-modal-backdrop" role="presentation">
      <section className="v3-modal v3-binding-dialog" role="dialog" aria-modal="true" aria-labelledby="v3-binding-title">
        <header className="v3-modal-header">
          <div>
            <span className="v3-eyebrow">Machine-local binding</span>
            <h1 id="v3-binding-title">{title}</h1>
            <p>{description}</p>
          </div>
          <button type="button" className="btn btn-ghost" onClick={onCancel} disabled={busy} aria-label="Close binding editor">
            <Icon name="x" size={17} />
          </button>
        </header>
        <div className="v3-modal-body">
          <div className="v3-binding-safety">
            <Icon name="check-circle" size={17} />
            <span>Only this machine changes. Repo identity and cloud logical paths stay the same.</span>
          </div>
          <label className="v3-folder-field">
            <span>Project checkout</span>
            <div>
              <input value={projectRoot} onChange={(event) => setProjectRoot(event.target.value)} placeholder="/path/to/project" />
              <button type="button" className="btn" onClick={() => void chooseFolder()} disabled={busy}>Choose folder</button>
            </div>
            <small>{compactProjectPath(projectRoot)} becomes the root for every project-relative task and setting.</small>
          </label>
          <div className="v3-provider-home-grid">
            {profileField("codex", "Codex")}
            {profileField("claude", "Claude")}
          </div>
          <dl className="v3-fact-grid compact">
            <div><dt>Local project</dt><dd><code>{binding.local_project_id ?? "created after confirmation"}</code></dd></div>
            <div><dt>Repo</dt><dd><code>{binding.bundle_id}</code></dd></div>
          </dl>
          {error && <div className="v3-callout error"><Icon name="alert-triangle" size={15} /> {error}</div>}
        </div>
        <footer className="v3-modal-footer">
          <span>Absolute paths are never uploaded.</span>
          <div>
            <button type="button" className="btn" onClick={onCancel} disabled={busy}>Cancel</button>
            <button type="button" className="btn btn-primary" disabled={busy || !projectRoot.trim() || !profilesComplete} onClick={() => onSubmit(next)}>
              {busy ? "Saving…" : actionLabel}
            </button>
          </div>
        </footer>
      </section>
    </div>
  );
}
