import { useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { ProjectPathApplyReport, SetupIssue, SetupReadiness } from "../types";

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
  /** Persist a source → target project-path mapping and (maybe) apply it. */
  onMapProjectPath: (issue: SetupIssue, targetPath: string) => Promise<ProjectPathApplyReport>;
  /** Session-only simulation: readiness treats every project path as foreign. */
  forceRemap: boolean;
  onToggleForceRemap: (enabled: boolean) => void;
  onClose: () => void;
}

export default function FinishSetup({ readiness, busy, onRepair, onResolveConflict, onMarkReviewed, onDismiss, onMapProjectPath, forceRemap, onToggleForceRemap, onClose }: Props) {
  // Folder-picker state per attach_project issue id. Successful reports are
  // kept locally so resume commands stay visible after the readiness
  // refresh removes the mapped row.
  const [pathTargets, setPathTargets] = useState<Record<string, string>>({});
  const [pathErrors, setPathErrors] = useState<Record<string, string>>({});
  const [pathReports, setPathReports] = useState<ProjectPathApplyReport[]>([]);
  const [mappingBusy, setMappingBusy] = useState<string | null>(null);

  const chooseFolder = async (issue: SetupIssue) => {
    const picked = await open({ directory: true, multiple: false });
    if (typeof picked === "string" && picked) {
      setPathTargets((current) => ({ ...current, [issue.id]: picked }));
    }
  };

  const mapPath = async (issue: SetupIssue) => {
    const target = pathTargets[issue.id];
    if (!target) return;
    setMappingBusy(issue.id);
    setPathErrors(({ [issue.id]: _dropped, ...rest }) => rest);
    try {
      const report = await onMapProjectPath(issue, target);
      setPathReports((current) => [
        ...current.filter((existing) => existing.source_path !== report.source_path),
        report,
      ]);
    } catch (e) {
      setPathErrors((current) => ({ ...current, [issue.id]: String(e) }));
    } finally {
      setMappingBusy(null);
    }
  };

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
        <div className="finish-setup-header-actions">
          <label
            className="finish-setup-simulate"
            title="Simulation for this app session: readiness treats every synced project path as foreign even if the folder exists here, so path mapping can be tried on one machine. Mappings you save are real."
          >
            <input
              type="checkbox"
              checked={forceRemap}
              disabled={busy}
              onChange={(e) => onToggleForceRemap(e.target.checked)}
            />
            Treat project folders as foreign
          </label>
          <button className="status-btn" onClick={onClose}>Close</button>
        </div>
      </div>
      {readiness.issues.length === 0 && pathReports.length === 0 && (
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
                  {issue.action === "attach_project" && issue.project_path && (
                    <div className="finish-setup-map">
                      <div className="finish-setup-map-row">
                        <button
                          className="status-btn"
                          disabled={busy || mappingBusy === issue.id}
                          onClick={() => void chooseFolder(issue)}
                          title="Pick the folder on this Mac that holds this project"
                        >
                          Choose folder
                        </button>
                        <span className={`finish-setup-map-target${pathTargets[issue.id] ? "" : " placeholder"}`}>
                          {pathTargets[issue.id] ?? issue.project_path.mapped_path ?? "No folder selected"}
                        </span>
                        <button
                          className="status-btn"
                          disabled={busy || !pathTargets[issue.id] || mappingBusy === issue.id}
                          onClick={() => void mapPath(issue)}
                          title="Save the mapping on this Mac and add the chosen folder to the Codex sidebar. Nothing is rewritten or deleted."
                        >
                          {mappingBusy === issue.id ? "Mapping…" : "Map"}
                        </button>
                      </div>
                      {pathErrors[issue.id] && (
                        <div className="finish-setup-map-error">{pathErrors[issue.id]}</div>
                      )}
                    </div>
                  )}
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
      {pathReports.map((report) => (
        <div key={`${report.provider}:${report.source_path}`} className="finish-setup-group">
          <div className="finish-setup-category">
            <span className="finish-setup-category-label">
              Mapped to {report.target_path}
            </span>
          </div>
          <div className="finish-setup-issue">
            <div className="finish-setup-issue-text">
              <div className="finish-setup-title">{report.source_path}</div>
              {report.provider === "codex" && report.sidebar_pending && (
                <div className="finish-setup-detail">
                  Quit ChatGPT/Codex, then Apply sidebar — the mapping is saved and will not need re-picking.
                </div>
              )}
              {report.provider === "codex" && report.resume_commands.length > 0 && (
                <>
                  <div className="finish-setup-detail">
                    Continue each restored task with its original thread id (run /app inside the resumed session to hand it to ChatGPT desktop):
                  </div>
                  <pre className="finish-setup-commands">{report.resume_commands.join("\n")}</pre>
                </>
              )}
              {report.provider === "claude" && (
                <div className="finish-setup-detail">
                  Claude mapping {report.state}
                  {report.alias_path ? ` · alias ${report.alias_path}` : ""}
                  {report.affected_session_ids.length > 0
                    ? ` · ${report.affected_session_ids.length} session${report.affected_session_ids.length === 1 ? "" : "s"}`
                    : ""}
                </div>
              )}
            </div>
          </div>
        </div>
      ))}
    </div>
  );
}
