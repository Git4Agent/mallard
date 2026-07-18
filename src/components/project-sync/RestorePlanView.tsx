import { useEffect, useMemo, useState } from "react";
import type {
  BundleReadiness,
  DependencyAction,
  DependencyPlan,
  DependencyResult,
  PlannedAction,
  ProjectBinding,
  RestoreAction,
  RestorePlan,
  RestoreResult,
} from "../../types";
import Icon from "../Icons";
import { compactProjectPath, providerLabel } from "./model";

interface Props {
  projectName: string;
  plan: RestorePlan;
  binding: ProjectBinding;
  dependencyPlan: DependencyPlan | null;
  readiness: BundleReadiness | null;
  restoreResult: RestoreResult | null;
  dependencyResult: DependencyResult | null;
  busy: boolean;
  error: string | null;
  onApplyRestore: (actionIds: string[]) => void;
  onApplyDependencies: (actionIds: string[]) => void;
  onRefresh: () => void;
  onClose: () => void;
}

function restoreActionView(action: RestoreAction): PlannedAction {
  const kind = action.kind.kind;
  const provider = "provider" in action.kind ? action.kind.provider : null;
  const detail = "logical_path" in action.kind
    ? action.kind.logical_path
    : "plugin_id" in action.kind
      ? action.kind.plugin_id
      : "target_relative_path" in action.kind
        ? action.kind.target_relative_path
        : "semantic_key" in action.kind
          ? action.kind.semantic_key
          : "message" in action.kind
            ? action.kind.message
            : action.target_path;
  const title = kind.split("_").map((part) => part.charAt(0).toUpperCase() + part.slice(1)).join(" ");
  const review = kind === "review_hook" || kind === "review_mcp" || kind === "apply_setting";
  const executable = kind === "install_plugin" || kind === "install_standalone_skill";
  return {
    action_id: action.action_id,
    resource_id: action.resource_id,
    kind,
    title,
    detail,
    provider,
    target_path: action.target_path,
    requires_explicit_approval: action.requires_explicit_approval,
    default_approved: !action.requires_explicit_approval,
    risk: executable ? "executable" : review ? "review" : "safe",
  };
}

function dependencyActionView(action: DependencyAction): PlannedAction {
  return {
    action_id: action.action_id,
    resource_id: action.resource_id,
    kind: action.kind,
    title: action.display_name,
    detail: action.argv.length > 0 ? action.argv.join(" ") : action.kind.replace(/_/g, " "),
    provider: action.provider,
    requires_explicit_approval: action.requires_explicit_approval,
    default_approved: !action.requires_explicit_approval,
    risk: action.kind.startsWith("install_") ? "executable" : "review",
  };
}

function selectable(action: PlannedAction): boolean {
  return action.risk !== "blocked" && !action.blocked_reason;
}

function initialSelection(actions: PlannedAction[]): Set<string> {
  return new Set(actions.filter((action) => selectable(action) && action.default_approved && !action.requires_explicit_approval).map((action) => action.action_id));
}

function ActionList({
  actions,
  selected,
  disabled,
  onToggle,
}: {
  actions: PlannedAction[];
  selected: Set<string>;
  disabled: boolean;
  onToggle: (actionId: string) => void;
}) {
  return (
    <div className="v3-plan-actions">
      {actions.map((action) => {
        const blocked = !selectable(action);
        return (
          <label key={action.action_id} className={`v3-plan-action risk-${action.risk ?? "review"}${blocked ? " blocked" : ""}`}>
            <input
              type="checkbox"
              checked={selected.has(action.action_id)}
              disabled={disabled || blocked}
              onChange={() => onToggle(action.action_id)}
            />
            <span className="v3-plan-action-icon">
              <Icon name={blocked ? "alert-triangle" : action.kind.includes("install") ? "download" : "file"} size={16} />
            </span>
            <span className="v3-plan-action-copy">
              <strong>{action.title}</strong>
              <span>{action.detail ?? action.target_path ?? action.kind.replace(/_/g, " ")}</span>
              {action.blocked_reason && <small>{action.blocked_reason}</small>}
            </span>
            <span className="v3-plan-action-meta">
              {action.provider && <small>{providerLabel(action.provider)}</small>}
              <span>{action.risk ?? "review"}</span>
            </span>
          </label>
        );
      })}
    </div>
  );
}

function ResultSummary({ result }: { result: RestoreResult | DependencyResult }) {
  const failures = result.failed_actions ?? [];
  return (
    <div className={`v3-result-summary ${result.success && failures.length === 0 ? "success" : "warning"}`}>
      <Icon name={result.success && failures.length === 0 ? "check-circle" : "alert-triangle"} size={17} />
      <div>
        <strong>{result.message}</strong>
        {failures.map((failure) => <span key={failure.action_id}>{failure.action_id}: {failure.message}</span>)}
      </div>
    </div>
  );
}

export default function RestorePlanView({
  projectName,
  plan,
  binding,
  dependencyPlan,
  readiness,
  restoreResult,
  dependencyResult,
  busy,
  error,
  onApplyRestore,
  onApplyDependencies,
  onRefresh,
  onClose,
}: Props) {
  const restoreActions = useMemo(() => plan.actions.map(restoreActionView), [plan.actions]);
  const dependencyActions = useMemo(() => (dependencyPlan?.actions ?? []).map(dependencyActionView), [dependencyPlan]);
  const [restoreSelection, setRestoreSelection] = useState<Set<string>>(() => initialSelection(restoreActions));
  const [dependencySelection, setDependencySelection] = useState<Set<string>>(() => initialSelection(dependencyActions));

  useEffect(() => setRestoreSelection(initialSelection(restoreActions)), [restoreActions]);
  useEffect(() => setDependencySelection(initialSelection(dependencyActions)), [dependencyActions]);

  const restoreSelectable = useMemo(() => restoreActions.filter(selectable), [restoreActions]);
  const dependencySelectable = useMemo(() => dependencyActions.filter(selectable), [dependencyActions]);
  const restoreFinished = restoreResult !== null;
  const toggle = (setter: React.Dispatch<React.SetStateAction<Set<string>>>, actionId: string) => setter((current) => {
    const next = new Set(current);
    if (next.has(actionId)) next.delete(actionId);
    else next.add(actionId);
    return next;
  });

  return (
    <div className="v3-modal-backdrop" role="presentation">
      <section className="v3-modal v3-restore-dialog" role="dialog" aria-modal="true" aria-labelledby="v3-restore-title">
        <header className="v3-modal-header">
          <div>
            <span className="v3-eyebrow">Pull review · nothing applied yet</span>
            <h1 id="v3-restore-title">Restore {projectName}</h1>
            <p>{compactProjectPath(binding.project_root)} · select the actions you want, then apply them using the persistent button below.</p>
          </div>
          <button type="button" className="btn btn-ghost" onClick={onClose} disabled={busy} aria-label="Close restore plan"><Icon name="x" size={17} /></button>
        </header>

        <div className="v3-modal-body">
          {!restoreFinished && (
            <div className="v3-restore-apply-notice" role="note">
              <Icon name="alert-triangle" size={18} />
              <span>
                <strong>Review only — no project files have changed.</strong>
                <small>Select the actions to restore, then click <b>Apply approved changes</b> at the bottom of this window.</small>
              </span>
            </div>
          )}
          <section className="v3-plan-section">
            <div className="v3-card-heading">
              <div><strong>Files, conversations & definitions</strong><span>Backups are created before an existing target is changed.</span></div>
              <div className="v3-plan-select-actions">
                <button type="button" className="btn btn-ghost" disabled={busy} onClick={() => setRestoreSelection(new Set(restoreSelectable.filter((action) => action.risk === "safe").map((action) => action.action_id)))}>Safe only</button>
                <button type="button" className="btn btn-ghost" disabled={busy} onClick={() => setRestoreSelection(new Set(restoreSelectable.map((action) => action.action_id)))}>All available</button>
              </div>
            </div>
            <ActionList actions={restoreActions} selected={restoreSelection} disabled={busy} onToggle={(id) => toggle(setRestoreSelection, id)} />
            {restoreActions.length === 0 && <div className="v3-inline-empty">No file changes are needed.</div>}
            {restoreResult && <ResultSummary result={restoreResult} />}
          </section>

          {dependencyPlan && (
            <section className="v3-plan-section">
              <div className="v3-card-heading">
                <div><strong>Plugins & standalone skills</strong><span>Native installers run only for checked actions. Plugin payloads are never copied.</span></div>
                <button type="button" className="btn btn-ghost" disabled={busy} onClick={() => setDependencySelection(new Set(dependencySelectable.map((action) => action.action_id)))}>Select available</button>
              </div>
              <ActionList actions={dependencyActions} selected={dependencySelection} disabled={busy} onToggle={(id) => toggle(setDependencySelection, id)} />
              {dependencyActions.length === 0 && <div className="v3-inline-empty">All selected dependencies are ready.</div>}
              <div className="v3-plan-footer">
                <span>{dependencySelection.size} install actions approved</span>
                <button type="button" className="btn" disabled={busy || dependencySelection.size === 0} onClick={() => onApplyDependencies([...dependencySelection])}>
                  <Icon name="download" size={14} /> {busy ? "Installing…" : "Install selected"}
                </button>
              </div>
              {dependencyResult && <ResultSummary result={dependencyResult} />}
            </section>
          )}

          {readiness && (
            <section className="v3-plan-section">
              <div className="v3-card-heading">
                <div><strong>Readiness</strong><span className={`v3-readiness-label ${readiness.state}`}>{readiness.state.replace(/_/g, " ")}</span></div>
                <button type="button" className="btn btn-ghost" onClick={onRefresh} disabled={busy}><Icon name="refresh" size={13} /> Recheck</button>
              </div>
              <div className="v3-readiness-list">
                {readiness.issues.map((issue) => (
                  <div key={issue.issue_id} className={`severity-${issue.severity ?? "info"}`}>
                    <Icon name={issue.severity === "error" ? "alert-triangle" : "check-circle"} size={14} />
                    <span><strong>{issue.title}</strong><small>{issue.detail}</small></span>
                    {issue.provider && <em>{providerLabel(issue.provider)}</em>}
                  </div>
                ))}
                {readiness.issues.length === 0 && <div className="v3-inline-empty">Everything selected for this project is ready.</div>}
              </div>
            </section>
          )}

          {error && <div className="v3-callout error"><Icon name="alert-triangle" size={15} /> {error}</div>}
        </div>

        <footer className="v3-modal-footer v3-restore-apply-footer">
          <span className="v3-restore-apply-summary">
            <strong>{restoreFinished ? "Pull review finished" : "Ready to apply"}</strong>
            <small>
              {restoreFinished
                ? restoreResult.message
                : `${restoreSelection.size} of ${restoreSelectable.length} available actions approved`}
            </small>
          </span>
          <div>
            <button
              type="button"
              className="btn btn-primary v3-restore-apply-button"
              disabled={busy || restoreSelection.size === 0 || restoreFinished}
              onClick={() => onApplyRestore([...restoreSelection])}
            >
              <Icon name="check-circle" size={16} />
              {busy ? "Applying approved changes…" : restoreFinished ? "Pull finished" : "Apply approved changes"}
            </button>
          </div>
        </footer>
      </section>
    </div>
  );
}
