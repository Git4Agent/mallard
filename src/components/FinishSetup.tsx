import { SetupIssue, SetupReadiness } from "../types";

// One surface for everything post-pull that needs a human: plugins to
// repair, hooks to review, conflict copies to resolve, paths to attach.
// Every button here is an explicit action; the scan itself never mutates.

const CATEGORY_LABELS: Record<string, string> = {
  plugins: "Plugins",
  skills: "Skills",
  mcp: "Connections",
  hooks: "Hooks",
  agents: "Agents",
  conflicts: "Conflicts",
  paths: "Projects & paths",
  instructions: "Instructions",
  sidebar: "Sidebar",
};

interface Props {
  readiness: SetupReadiness;
  busy: boolean;
  onRepair: (action: string, profile: string) => void;
  onResolveConflict: (issue: SetupIssue) => void;
  onMarkReviewed: (issue: SetupIssue) => void;
  onDismiss: (issue: SetupIssue) => void;
  onClose: () => void;
}

export default function FinishSetup({ readiness, busy, onRepair, onResolveConflict, onMarkReviewed, onDismiss, onClose }: Props) {
  const groups = new Map<string, SetupIssue[]>();
  for (const issue of readiness.issues) {
    const key = `${issue.profile}:${issue.category}`;
    const list = groups.get(key) ?? [];
    list.push(issue);
    groups.set(key, list);
  }
  // Default profiles read as their root; extra profiles show their id.
  const profileTag = (issue: SetupIssue) =>
    issue.profile === "codex" || issue.profile === "claude" ? issue.root : `${issue.root} (${issue.profile})`;
  return (
    <div className="finish-setup-panel">
      <div className="finish-setup-header">
        <span>Finish setup</span>
        <button className="status-btn" onClick={onClose}>Close</button>
      </div>
      {readiness.issues.length === 0 && (
        <div className="finish-setup-empty">Everything on this machine is ready.</div>
      )}
      {[...groups.entries()].map(([key, issues]) => {
        const { category } = issues[0];
        const repairAction = issues.find(
          (issue) => issue.action === "repair_codex_plugins" || issue.action === "repair_claude_plugins",
        )?.action;
        return (
          <div key={key} className="finish-setup-group">
            <div className="finish-setup-category">
              <span className="finish-setup-category-label">
                {profileTag(issues[0])} · {CATEGORY_LABELS[category] ?? category} · {issues.length}
              </span>
              {repairAction && (
                <button className="status-btn" disabled={busy} onClick={() => onRepair(repairAction, issues[0].profile)}>
                  Repair
                </button>
              )}
            </div>
            {issues.map((issue) => (
              <div key={issue.id} className="finish-setup-issue">
                <div className="finish-setup-issue-text">
                  <div className="finish-setup-title">{issue.title}</div>
                  <div className="finish-setup-detail">{issue.detail}</div>
                </div>
                <div className="finish-setup-actions">
                  {issue.action === "apply_sidebar_state" && (
                    <button
                      className="status-btn"
                      disabled={busy}
                      onClick={() => onRepair(issue.action, issue.profile)}
                      title="Additively merge synced projects, thread titles, and display prefs into this machine's Codex desktop. Quit the desktop app first; nothing local is removed."
                    >
                      Apply
                    </button>
                  )}
                  {issue.action === "review_hooks" && (
                    <button
                      className="status-btn"
                      disabled={busy}
                      onClick={() => onMarkReviewed(issue)}
                      title="After reviewing in the agent's native /hooks flow, record this hook as reviewed on this machine. Trust never syncs."
                    >
                      Mark reviewed
                    </button>
                  )}
                  {issue.action === "resolve_conflict_copy" && (
                    <button
                      className="status-btn"
                      disabled={busy}
                      onClick={() => onResolveConflict(issue)}
                      title="After folding the version you want into the main file, remove this review copy from this machine and the linked cloud profile."
                    >
                      Resolve
                    </button>
                  )}
                  {issue.action !== "repair_codex_plugins" && issue.action !== "resolve_conflict_copy" && (
                    <button className="status-btn" disabled={busy} onClick={() => onDismiss(issue)}>
                      Dismiss
                    </button>
                  )}
                </div>
              </div>
            ))}
          </div>
        );
      })}
    </div>
  );
}
