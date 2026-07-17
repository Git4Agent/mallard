import { useState } from "react";
import { FileEntry, ConfigKind } from "../types";
import Icon from "./Icons";

interface SharedProps {
  entries: FileEntry[];
  selectedFile: string | null;
  selectedForSync: Set<string>;
  onFileSelect: (path: string) => void;
  onToggleSync: (path: string) => void;
  statusMap?: Map<string, string>;
  forceOpen?: boolean;
}

interface TreeProps extends SharedProps {
  label: string;
  fullPath?: string;
  kind: ConfigKind;
  hideHeader?: boolean;
  onConfigure?: () => void;
  onRemove?: () => void;
  removeBusy?: boolean;
}

interface NodeProps extends SharedProps {
  entry: FileEntry;
  depth: number;
}

function collectSyncable(entry: FileEntry): string[] {
  if (!entry.included) return [];
  if (!entry.is_dir) return [entry.path];
  // children == null → not walked; children.length === 0 → walked but empty.
  if (entry.children == null) return [entry.path];
  if (entry.children.length === 0) return [entry.path];
  return entry.children.flatMap(collectSyncable);
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}K`;
  return `${(bytes / 1024 / 1024).toFixed(1)}M`;
}

// DESIGN2 state matrix → badge. "push" side is green, "pull" side blue,
// union-merge pending is yellow.
const STATUS_BADGE: Record<string, { char: string; cls: string; title: string }> = {
  new: { char: "+", cls: "push", title: "New locally — push uploads it" },
  modified: { char: "●", cls: "push", title: "Changed locally — push uploads it" },
  "local-only": { char: "+", cls: "push", title: "New locally — push uploads it" },
  "local-ahead": { char: "●", cls: "push", title: "Changed locally — push uploads it" },
  "cloud-deleted": { char: "●", cls: "push", title: "Deleted in cloud — push republishes it" },
  "cloud-only": { char: "↓", cls: "pull", title: "New in cloud — sync downloads it" },
  "cloud-ahead": { char: "↓", cls: "pull", title: "Changed in cloud — sync applies it" },
  "local-deleted": { char: "↓", cls: "pull", title: "Deleted locally — sync restores it from cloud" },
  conflict: { char: "⇄", cls: "conflict", title: "Changed on both sides — sync merges (union)" },
  converged: { char: "=", cls: "synced", title: "Same change on both sides — already identical" },
};

function fileIcon(name: string, isDir: boolean): { label: string; cls: string } {
  if (isDir) return { label: "folder", cls: "dir" };
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  if (ext === "json" || ext === "jsonl") return { label: "{}", cls: "code" };
  if (ext === "toml" || ext === "env") return { label: "=", cls: "code" };
  if (ext === "md" || ext === "txt") return { label: "T", cls: "doc" };
  if (ext === "sqlite" || name.includes(".sqlite-")) return { label: "db", cls: "db" };
  if (["png", "jpg", "gif", "svg", "ico"].includes(ext)) return { label: "img", cls: "media" };
  if (ext === "sh") return { label: "$", cls: "code" };
  return { label: "", cls: "" };
}

function TreeNode({
  entry,
  depth,
  selectedFile,
  selectedForSync,
  onFileSelect,
  onToggleSync,
  entries,
  statusMap,
  forceOpen = false,
}: NodeProps) {
  const [open, setOpen] = useState(false);

  const syncable = collectSyncable(entry);
  const checkedCount = syncable.filter((p) => selectedForSync.has(p)).length;
  const isChecked = entry.included && (entry.is_dir
    ? checkedCount === syncable.length && syncable.length > 0
    : selectedForSync.has(entry.path));
  const isIndeterminate = entry.included && entry.is_dir && checkedCount > 0 && checkedCount < syncable.length;

  const handleCheck = (e: React.ChangeEvent<HTMLInputElement>) => {
    e.stopPropagation();
    if (!entry.included) return;
    if (entry.is_dir) {
      if (checkedCount === syncable.length) {
        syncable.forEach((p) => { if (selectedForSync.has(p)) onToggleSync(p); });
      } else {
        syncable.forEach((p) => { if (!selectedForSync.has(p)) onToggleSync(p); });
      }
    } else {
      onToggleSync(entry.path);
    }
  };

  const handleClick = () => {
    if (entry.is_dir) setOpen((v) => !v);
    else onFileSelect(entry.path);
  };

  const { label, cls } = fileIcon(entry.name, entry.is_dir);
  const isActive = selectedFile === entry.path;
  const hasChildren = entry.is_dir && entry.children != null && entry.children.length > 0;

  return (
    <div className="tree-node">
      <div
        className={[
          "tree-item",
          isActive ? "tree-item-active" : "",
          !entry.included ? "tree-item-excluded" : "",
        ].join(" ")}
        style={{ paddingLeft: `${6 + depth * 14}px` }}
        onClick={handleClick}
      >
        <span className="tree-check" onClick={(e) => e.stopPropagation()}>
          {entry.included ? (
            <input
              type="checkbox"
              checked={isChecked}
              ref={(el) => { if (el) el.indeterminate = isIndeterminate; }}
              onChange={handleCheck}
              onClick={(e) => e.stopPropagation()}
            />
          ) : (
            <span
              className="tree-exclusion-icon"
              title="Never synced. Change this in Settings."
              role="img"
              aria-label="Never synced"
            >
              <Icon name="ban" size={14} />
            </span>
          )}
        </span>

        {/* expand arrow — only for dirs with children */}
        <span className="tree-toggle">
          {hasChildren ? (
            <Icon name={forceOpen || open ? "chevron-down" : "chevron-right"} size={12} />
          ) : null}
        </span>

        {/* icon */}
        <span className={`tree-icon ${cls}`}>
          {entry.is_dir ? <Icon name="folder" size={14} /> : label || <Icon name="file" size={13} />}
        </span>

        {/* name */}
        <span className="tree-name">{entry.name}</span>

        {/* change status badge — only for files, hidden when synced */}
        {!entry.is_dir && (() => {
          const s = statusMap?.get(entry.path);
          if (!s || s === "synced") return null;
          const badge = STATUS_BADGE[s];
          if (!badge) return null;
          return (
            <span className={`file-status-badge file-status-${badge.cls}`} title={badge.title}>
              {badge.char}
            </span>
          );
        })()}

        {entry.is_dir && entry.included && (
          <span className="tree-row-actions" onClick={(e) => e.stopPropagation()}>
            <span className="tree-icon-btn" title="More actions">
              <Icon name="more" size={14} />
            </span>
          </span>
        )}

        {/* size for files */}
        {!entry.is_dir && entry.size > 0 && (
          <span className="tree-size">{formatSize(entry.size)}</span>
        )}
      </div>

      {entry.is_dir && (forceOpen || open) && entry.children != null && (
        <>
          {entry.children.map((child) => (
            <TreeNode
              key={child.path}
              entry={child}
              depth={depth + 1}
              entries={entries}
              selectedFile={selectedFile}
              selectedForSync={selectedForSync}
              onFileSelect={onFileSelect}
              onToggleSync={onToggleSync}
              statusMap={statusMap}
              forceOpen={forceOpen}
            />
          ))}
        </>
      )}
    </div>
  );
}

export default function FileTree({
  entries,
  label,
  fullPath,
  kind,
  selectedFile,
  selectedForSync,
  onFileSelect,
  onToggleSync,
  statusMap,
  forceOpen = false,
  hideHeader = false,
  onConfigure,
  onRemove,
  removeBusy = false,
}: TreeProps) {
  const [collapsed, setCollapsed] = useState(true);
  const allSyncable = entries.flatMap(collectSyncable);
  const allChecked = allSyncable.length > 0 && allSyncable.every((p) => selectedForSync.has(p));

  const handleToggleAll = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (allChecked) {
      allSyncable.forEach((p) => { if (selectedForSync.has(p)) onToggleSync(p); });
    } else {
      allSyncable.forEach((p) => { if (!selectedForSync.has(p)) onToggleSync(p); });
    }
  };

  const handleHeaderKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "Enter" && e.key !== " ") return;
    e.preventDefault();
    setCollapsed((v) => !v);
  };

  const kindIcon = kind === "local" ? "computer" : "cloud";
  const kindClass = kind === "local" ? "icon-local" : "icon-cloud";

  const body = (
    <div className="file-tree-body">
      {entries.map((entry) => (
        <TreeNode
          key={entry.path}
          entry={entry}
          depth={0}
          entries={entries}
          selectedFile={selectedFile}
          selectedForSync={selectedForSync}
          onFileSelect={onFileSelect}
          onToggleSync={onToggleSync}
          statusMap={statusMap}
          forceOpen={forceOpen}
        />
      ))}
    </div>
  );

  if (hideHeader) {
    return <div className="file-tree file-tree-workspace">{body}</div>;
  }

  return (
    <div className="file-tree">
      <div
        className="file-tree-header"
        role="button"
        tabIndex={0}
        aria-expanded={!collapsed}
        onClick={() => setCollapsed((v) => !v)}
        onKeyDown={handleHeaderKeyDown}
        title={fullPath || label}
      >
        <Icon name={kindIcon} size={16} className={`file-tree-kind-icon ${kindClass}`} />
        <span className="file-tree-section-copy">
          <span className="file-tree-section-label">{label}</span>
        </span>
        <span className="file-tree-header-actions" onClick={(e) => e.stopPropagation()}>
          {onConfigure && (
            <button
              type="button"
              className="profile-utility-btn"
              onClick={onConfigure}
              title={`Profile settings for ${label}`}
              aria-label={`Profile settings for ${label}`}
            >
              <Icon name="settings" size={13} />
            </button>
          )}
          {onRemove && (
            <button
              type="button"
              className="profile-utility-btn profile-remove-btn"
              onClick={onRemove}
              disabled={removeBusy}
              title={`Remove ${label} from dashboard; files stay on disk`}
              aria-label={`Remove ${label} from dashboard; files stay on disk`}
            >
              <Icon name="trash" size={13} />
            </button>
          )}
          <button type="button" className="btn-link file-tree-all-btn" onClick={handleToggleAll}>
            {allChecked ? "all" : "none"}
          </button>
        </span>
        <Icon
          name={collapsed ? "chevron-right" : "chevron-down"}
          size={13}
          className="file-tree-chevron"
        />
      </div>
      {!collapsed && body}
    </div>
  );
}
