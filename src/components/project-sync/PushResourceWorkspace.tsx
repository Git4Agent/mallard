import { useEffect } from "react";
import type { ProjectResourceDescriptor } from "../../types";
import Icon from "../Icons";
import ResourceInventory from "./ResourceInventory";

const EMPTY_STATUSES = new Map<string, string>();

interface Props {
  resources: ProjectResourceDescriptor[];
  selected: Set<string>;
  projectDefaults: Set<string>;
  busy: boolean;
  error: string | null;
  onToggle: (resourceId: string) => void;
  onUseProjectDefaults: () => void;
  onClear: () => void;
  onClose: () => void;
  onPush: () => void;
}

export default function PushResourceWorkspace({
  resources,
  selected,
  projectDefaults,
  busy,
  error,
  onToggle,
  onUseProjectDefaults,
  onClear,
  onClose,
  onPush,
}: Props) {
  useEffect(() => {
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !busy) onClose();
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [busy, onClose]);

  const selectedCount = selected.size;

  return (
    <section className="v3-inline-action-review v3-inline-push-review" aria-labelledby="v3-push-resource-title">
      <header className="v3-inline-action-header v3-push-resource-header">
        <h2 id="v3-push-resource-title">Push resources</h2>
        <div className="v3-push-resource-actions">
          <button
            type="button"
            className="btn btn-ghost"
            onClick={onUseProjectDefaults}
            disabled={busy || projectDefaults.size === 0}
            title="Use project defaults"
          >
            Defaults ({projectDefaults.size})
          </button>
          <button type="button" className="btn btn-ghost" onClick={onClear} disabled={busy || selectedCount === 0}>
            Clear
          </button>
        </div>
        <button
          type="button"
          className="btn btn-ghost v3-inline-action-close"
          onClick={onClose}
          disabled={busy}
          aria-label="Close push resources"
        >
          <Icon name="x" size={15} />
        </button>
      </header>

      <div className="v3-inline-action-content v3-push-resource-content">
        <div className="v3-inline-action-scroll">
          <ResourceInventory
            resources={resources}
            selected={selected}
            statuses={EMPTY_STATUSES}
            disabled={busy}
            onToggle={onToggle}
          />
        </div>

        {error && <div className="v3-callout error v3-pull-error"><Icon name="alert-triangle" size={15} /> {error}</div>}
      </div>

      <footer className="v3-inline-action-footer v3-push-resource-footer">
        <button type="button" className="btn btn-primary v3-pull-apply-button" onClick={onPush} disabled={busy}>
          <Icon name={busy ? "refresh" : "upload"} size={16} className={busy ? "icon-spin" : undefined} />
          {busy
            ? "Pushing…"
            : selectedCount === 0
              ? "Push empty"
              : `Push ${selectedCount} resource${selectedCount === 1 ? "" : "s"}`}
        </button>
      </footer>
    </section>
  );
}
