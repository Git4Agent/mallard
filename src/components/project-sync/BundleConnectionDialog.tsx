import { useEffect, useState } from "react";
import type { BundleSnapshotSummary, StorageConfigV3 } from "../../types";
import Icon from "../Icons";
import { formatRelativeTime } from "./model";

interface Props {
  projectName: string;
  currentBundleId: string;
  projectFingerprint?: string | null;
  storage: StorageConfigV3;
  matches: BundleSnapshotSummary[];
  reason: "link" | "missing";
  busy: boolean;
  error: string | null;
  onCancel: () => void;
  onUseExisting: (match: BundleSnapshotSummary) => void;
  onKeepCurrent: () => void;
}

export default function BundleConnectionDialog({
  projectName,
  currentBundleId,
  projectFingerprint,
  storage,
  matches,
  reason,
  busy,
  error,
  onCancel,
  onUseExisting,
  onKeepCurrent,
}: Props) {
  const [selectedId, setSelectedId] = useState("");

  useEffect(() => setSelectedId(""), [matches]);

  const selected = matches.find((match) => match.bundle_id === selectedId) ?? null;
  const missing = reason === "missing";
  const selectedFingerprintDiffers = !!projectFingerprint
    && !!selected?.repository_fingerprint
    && projectFingerprint !== selected.repository_fingerprint;
  const selectedFingerprintUnknown = !!projectFingerprint
    && !!selected
    && !selected.repository_fingerprint;

  return (
    <div className="v3-modal-backdrop" role="presentation">
      <section
        className="v3-modal v3-bundle-connection-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="v3-bundle-connection-title"
      >
        <header className="v3-modal-header">
          <div>
            <span className="v3-eyebrow">Repo identity</span>
            <h1 id="v3-bundle-connection-title">
              {missing ? "Choose the remote repo" : "Choose a repo from this storage"}
            </h1>
            <p>
              {missing
                ? `${projectName} has a local-only identity. Pull from an existing repo, or publish this one separately.`
                : `${storage.name} contains the repos below. Git matches are recommended, but every repo remains available.`}
            </p>
          </div>
          <button type="button" className="btn btn-ghost" onClick={onCancel} disabled={busy} aria-label="Close repo chooser">
            <Icon name="x" size={17} />
          </button>
        </header>

        <div className="v3-modal-body">
          <div className="v3-bundle-context">
            <Icon name={storage.kind === "local" ? "drive" : "cloud"} size={17} />
            <span><strong>{storage.name}</strong><small>Current local repo {currentBundleId.slice(0, 12)}…</small></span>
          </div>

          <div className="v3-bundle-choice-list" role="radiogroup" aria-label="Available remote repos">
            {matches.map((match) => {
              const active = selectedId === match.bundle_id;
              const recommended = !!projectFingerprint
                && match.repository_fingerprint === projectFingerprint;
              return (
                <button
                  key={match.bundle_id}
                  type="button"
                  role="radio"
                  aria-checked={active}
                  className={`v3-bundle-choice${active ? " active" : ""}`}
                  onClick={() => setSelectedId(match.bundle_id)}
                  disabled={busy}
                >
                  <span className="v3-bundle-radio"><span /></span>
                  <span className="v3-bundle-choice-copy">
                    <strong>{match.display_name}{recommended && <small className="v3-bundle-recommended">Recommended</small>}</strong>
                    <span>{match.bundle_id}</span>
                  </span>
                  <span className="v3-bundle-choice-meta">
                    <strong>gen {match.generation ?? "—"}</strong>
                    <span>{match.resource_count ?? match.resources?.length ?? 0} resources</span>
                    <small>{formatRelativeTime(match.updated_at)}</small>
                  </span>
                </button>
              );
            })}
          </div>

          {selectedFingerprintDiffers && (
            <div className="v3-callout warning">
              <Icon name="alert-triangle" size={15} />
              <span>This repo has a different Git fingerprint. Connecting will adopt its repository identity.</span>
            </div>
          )}
          {selectedFingerprintUnknown && (
            <div className="v3-callout warning">
              <Icon name="alert-triangle" size={15} />
              <span>This repo has no Git fingerprint, so Mallard cannot verify that it belongs to this checkout.</span>
            </div>
          )}

          <div className="v3-callout warning">
            <Icon name="alert-triangle" size={15} />
            <span>Connecting changes only Mallard metadata. It does not move or delete the checkout or provider profile.</span>
          </div>
          {error && <div className="v3-callout error"><Icon name="alert-triangle" size={15} /> {error}</div>}
        </div>

        <footer className="v3-modal-footer">
          <button type="button" className="btn btn-ghost" onClick={onKeepCurrent} disabled={busy}>
            {missing ? "Review separate Push" : "Create separate repo"}
          </button>
          <div>
            <button type="button" className="btn" onClick={onCancel} disabled={busy}>Cancel</button>
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy || !selected}
              onClick={() => selected && onUseExisting(selected)}
            >
              {busy ? "Connecting…" : "Connect and review Pull"}
            </button>
          </div>
        </footer>
      </section>
    </div>
  );
}
