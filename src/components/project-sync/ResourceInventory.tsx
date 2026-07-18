import { useMemo, useState } from "react";
import type { ProjectResourceCategory, ProjectResourceDescriptor } from "../../types";
import Icon from "../Icons";
import { RESOURCE_GROUPS, categoryFor, providerLabel } from "./model";

interface Props {
  resources: ProjectResourceDescriptor[];
  selected: Set<string>;
  statuses: Map<string, string>;
  disabled?: boolean;
  onToggle: (resourceId: string) => void;
}

const CATEGORY_ICON: Record<ProjectResourceCategory, "activity" | "settings" | "folder" | "link"> = {
  conversations: "activity",
  project_setup: "settings",
  skills: "folder",
  plugins: "link",
  tools: "settings",
};

function statusLabel(status?: string): string {
  if (!status) return "Not compared";
  return status.replace(/_/g, " ");
}

function ResourceRow({
  resource,
  checked,
  status,
  disabled,
  onToggle,
}: {
  resource: ProjectResourceDescriptor;
  checked: boolean;
  status?: string;
  disabled: boolean;
  onToggle: () => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const blocked = !!resource.blocked_reason;
  const needsInstall = resource.kind.includes("plugin") || resource.kind.includes("skill");

  return (
    <div className={`v3-resource-row${checked ? " selected" : ""}${blocked ? " blocked" : ""}`}>
      <label className="v3-resource-select">
        <input
          type="checkbox"
          checked={checked}
          disabled={disabled || blocked}
          onChange={onToggle}
          aria-label={`Include ${resource.display_name}`}
        />
      </label>
      <button
        type="button"
        className="v3-resource-main"
        onClick={() => setExpanded((current) => !current)}
        aria-expanded={expanded}
      >
        <span className="v3-resource-copy">
          <strong>{resource.display_name}</strong>
          <span>
            {providerLabel(resource.provider)} · {resource.scope.replace(/_/g, " ")}
            {resource.provided_by ? ` · provided by ${resource.provided_by}` : ""}
          </span>
        </span>
        <span className="v3-resource-meta">
          {needsInstall && checked && (
            <span className="v3-resource-install">{resource.install_behavior ?? "install on restore"}</span>
          )}
          <span className={`v3-resource-status status-${status ?? "unknown"}`}>{statusLabel(status)}</span>
          <Icon name={expanded ? "chevron-down" : "chevron-right"} size={13} />
        </span>
      </button>
      {expanded && (
        <div className="v3-resource-detail">
          {resource.description && <p>{resource.description}</p>}
          {resource.logical_paths && resource.logical_paths.length > 0 && (
            <div>
              <span>Portable paths</span>
              <code>{resource.logical_paths.join("\n")}</code>
            </div>
          )}
          <div className="v3-resource-detail-grid">
            <span>Resource ID</span><code>{resource.resource_id}</code>
            <span>Kind</span><code>{resource.kind}</code>
            {resource.apply_policy && <><span>Apply</span><code>{resource.apply_policy}</code></>}
          </div>
          {resource.blocked_reason && (
            <div className="v3-resource-blocker"><Icon name="alert-triangle" size={14} /> {resource.blocked_reason}</div>
          )}
        </div>
      )}
    </div>
  );
}

export default function ResourceInventory({ resources, selected, statuses, disabled = false, onToggle }: Props) {
  const grouped = useMemo(() => {
    const result = new Map<ProjectResourceCategory, ProjectResourceDescriptor[]>();
    for (const group of RESOURCE_GROUPS) result.set(group.id, []);
    for (const resource of resources) result.get(categoryFor(resource))?.push(resource);
    return result;
  }, [resources]);

  if (resources.length === 0) {
    return (
      <div className="v3-empty-state compact">
        <Icon name="folder" size={22} />
        <strong>No project resources discovered</strong>
        <span>Refresh after the provider has created tasks, sessions, or project configuration.</span>
      </div>
    );
  }

  return (
    <div className="v3-resource-groups">
      {RESOURCE_GROUPS.map((group) => {
        const items = grouped.get(group.id) ?? [];
        if (items.length === 0) return null;
        const selectable = items.filter((item) => !item.blocked_reason);
        const selectedCount = selectable.filter((item) => selected.has(item.resource_id)).length;
        const allSelected = selectable.length > 0 && selectedCount === selectable.length;
        return (
          <section key={group.id} className="v3-resource-group">
            <div className="v3-resource-group-header">
              <span className="v3-resource-group-icon"><Icon name={CATEGORY_ICON[group.id]} size={16} /></span>
              <span className="v3-resource-group-copy">
                <strong>{group.label}</strong>
                <span>{group.description}</span>
              </span>
              <span className="v3-resource-group-count">{selectedCount}/{items.length}</span>
              <button
                type="button"
                className="btn btn-ghost"
                disabled={disabled || selectable.length === 0}
                onClick={() => {
                  for (const item of selectable) {
                    if (selected.has(item.resource_id) === allSelected) onToggle(item.resource_id);
                  }
                }}
              >
                {allSelected ? "Clear" : "Include all"}
              </button>
            </div>
            <div className="v3-resource-list">
              {items.map((resource) => (
                <ResourceRow
                  key={resource.resource_id}
                  resource={resource}
                  checked={selected.has(resource.resource_id)}
                  status={statuses.get(resource.resource_id)}
                  disabled={disabled}
                  onToggle={() => onToggle(resource.resource_id)}
                />
              ))}
            </div>
          </section>
        );
      })}
    </div>
  );
}
