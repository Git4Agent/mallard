import { useState } from "react";
import type {
  ProjectProvider,
  ProviderProfile,
  ProviderProfileSummary,
} from "../../types";
import Icon from "../Icons";
import { compactProjectPath } from "./model";

interface Props {
  inline?: boolean;
  projectRoot: string;
  profiles: ProviderProfileSummary[];
  initialProfileIds: Partial<Record<ProjectProvider, string>>;
  busy: boolean;
  error: string | null;
  onAddProfile: (provider: ProjectProvider) => Promise<ProviderProfile | null>;
  onCancel: () => void;
  onContinue: (profileIds: Partial<Record<ProjectProvider, string>>) => void;
}

export default function ProjectProfilePicker({
  inline = false,
  projectRoot,
  profiles,
  initialProfileIds,
  busy,
  error,
  onAddProfile,
  onCancel,
  onContinue,
}: Props) {
  const [profileIds, setProfileIds] = useState<Partial<Record<ProjectProvider, string>>>(initialProfileIds);

  const addProfile = async (provider: ProjectProvider) => {
    const profile = await onAddProfile(provider);
    if (profile) setProfileIds((current) => ({ ...current, [provider]: profile.profile_id }));
  };

  const profileField = (provider: ProjectProvider, label: string) => {
    const options = profiles.filter((profile) => profile.provider === provider);
    const selectedId = profileIds[provider] ?? "";
    const selectedProfile = options.find((profile) => profile.profile_id === selectedId);
    return (
      <label>
        <span>{label} profile</span>
        <div className="v3-profile-select-row">
          <select
            value={selectedId}
            disabled={busy}
            onChange={(event) => setProfileIds((current) => {
              const next = { ...current };
              if (event.target.value) next[provider] = event.target.value;
              else delete next[provider];
              return next;
            })}
          >
            <option value="">Not used</option>
            {options.map((profile) => (
              <option
                key={profile.profile_id}
                value={profile.profile_id}
                disabled={!profile.available || !profile.readable}
              >
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

  const picker = (
      <section
        className={inline ? "v3-inline-new-project v3-profile-picker-dialog" : "v3-modal v3-profile-picker-dialog"}
        role={inline ? "region" : "dialog"}
        aria-modal={inline ? undefined : true}
        aria-labelledby="v3-profile-picker-title"
      >
        <header className="v3-modal-header">
          <div>
            <span className="v3-eyebrow">New project · this machine</span>
            <h1 id="v3-profile-picker-title">Choose provider profiles</h1>
            <p>Agent Sync will discover resources only inside these local profiles.</p>
          </div>
          <button type="button" className="btn btn-ghost" onClick={onCancel} disabled={busy} aria-label="Cancel project setup">
            <Icon name="x" size={17} />
          </button>
        </header>
        <div className="v3-modal-body">
          <div className="v3-binding-safety">
            <Icon name="folder" size={17} />
            <span title={projectRoot}>{compactProjectPath(projectRoot)}</span>
          </div>
          <div className="v3-provider-home-grid">
            {profileField("codex", "Codex")}
            {profileField("claude", "Claude")}
          </div>
          <p className="v3-profile-picker-note">Profiles are machine-local. Their names and paths are never uploaded with the project repo.</p>
          {error && <div className="v3-callout error"><Icon name="alert-triangle" size={15} /> {error}</div>}
        </div>
        <footer className="v3-modal-footer">
          <span>Choose at least one profile before discovery.</span>
          <div>
            <button type="button" className="btn" onClick={onCancel} disabled={busy}>Cancel</button>
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy || Object.keys(profileIds).length === 0}
              onClick={() => onContinue(profileIds)}
            >
              {busy ? "Discovering…" : "Discover resources"}
            </button>
          </div>
        </footer>
      </section>
  );

  if (inline) return picker;
  return (
    <div className="v3-modal-backdrop" role="presentation">
      {picker}
    </div>
  );
}
