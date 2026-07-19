import { useEffect, useMemo, useRef, useState } from "react";
import type {
  BundleReadiness,
  DependencyPlan,
  DependencyResult,
  ProjectBinding,
  RestorePlan,
  RestoreResult,
} from "../../types";
import Icon from "../Icons";
import { compactProjectPath, providerLabel } from "./model";
import type { PullApplyPhase, PullReviewSelection } from "./pullReviewFlow";
import {
  buildPullReviewItems,
  buildPullReviewSelection,
  defaultSelected,
  dependencyActionIds,
  itemKindLabel,
  pendingIds,
  requiresApproval,
  restoreActionIds,
  type PullReviewItem,
} from "./pullReviewModel";

interface Props {
  projectName: string;
  profileLabel: string;
  plan: RestorePlan;
  binding: ProjectBinding;
  dependencyPlan: DependencyPlan | null;
  readiness: BundleReadiness | null;
  restoreResult: RestoreResult | null;
  dependencyResult: DependencyResult | null;
  phase: PullApplyPhase;
  supportLoading: boolean;
  completedActionIds: ReadonlySet<string>;
  completedResourceIds: ReadonlySet<string>;
  failedResourceIds: ReadonlySet<string>;
  busy: boolean;
  error: string | null;
  onApply: (selection: PullReviewSelection) => void;
  onRefresh: () => void;
  onBack: () => void;
}

function phaseIndex(phase: PullApplyPhase): number {
  if (phase === "restoring") return 0;
  if (phase === "installing") return 1;
  if (phase === "verifying") return 2;
  if (phase === "complete") return 3;
  return -1;
}

function PhaseProgress({
  phase,
  ready,
  restoreFailed,
  installFailed,
}: {
  phase: PullApplyPhase;
  ready: boolean;
  restoreFailed: boolean;
  installFailed: boolean;
}) {
  const activeIndex = phaseIndex(phase);
  const steps = [
    { label: "Restore project data and custom skills", icon: "file" as const },
    { label: "Install selected plugins and tools", icon: "download" as const },
    { label: "Verify project readiness", icon: "refresh" as const },
  ];
  return (
    <section className="v3-pull-progress" aria-label="Pull progress" aria-live="polite">
      {steps.map((step, index) => {
        const current = activeIndex === index;
        const warning = phase === "complete" && (
          (index === 0 && restoreFailed) ||
          (index === 1 && installFailed) ||
          (index === 2 && !ready)
        );
        const complete = !warning && (activeIndex > index || phase === "complete");
        return (
          <div key={step.label} className={`${current ? "active" : ""}${complete ? " complete" : ""}${warning ? " warning" : ""}`}>
            <span>
              <Icon
                name={warning ? "alert-triangle" : complete ? "check-circle" : current ? "refresh" : step.icon}
                size={16}
                className={current ? "icon-spin" : undefined}
              />
            </span>
            <strong>{step.label}</strong>
            <small>{warning ? "Needs attention" : complete ? "Complete" : current ? "In progress" : "Waiting"}</small>
          </div>
        );
      })}
    </section>
  );
}

function ResultDetails({
  restoreResult,
  dependencyResult,
}: {
  restoreResult: RestoreResult | null;
  dependencyResult: DependencyResult | null;
}) {
  const results = [restoreResult, dependencyResult].filter((result): result is RestoreResult | DependencyResult => result !== null);
  if (results.length === 0) return null;
  return (
    <details className="v3-pull-result-details">
      <summary>Technical results</summary>
      {results.map((result, index) => (
        <div key={`${result.message}-${index}`}>
          <strong>{result.message}</strong>
          {(result.failed_actions ?? []).map((failure) => (
            <span key={failure.action_id}><code>{failure.action_id}</code>{failure.message}</span>
          ))}
        </div>
      ))}
    </details>
  );
}

function ItemList({
  items,
  selected,
  completedActionIds,
  completedResourceIds,
  failedResourceIds,
  phase,
  supportLoading,
  disabled,
  onToggle,
}: {
  items: PullReviewItem[];
  selected: ReadonlySet<string>;
  completedActionIds: ReadonlySet<string>;
  completedResourceIds: ReadonlySet<string>;
  failedResourceIds: ReadonlySet<string>;
  phase: PullApplyPhase;
  supportLoading: boolean;
  disabled: boolean;
  onToggle: (resourceId: string) => void;
}) {
  return (
    <div className="v3-pull-items" role="list">
      {items.map((item) => {
        const pending = pendingIds(item, completedActionIds);
        const completed = completedResourceIds.has(item.resourceId);
        const failed = failedResourceIds.has(item.resourceId);
        const waitingForInstaller =
          item.category === "global_tool" &&
          item.toolKind !== "custom_skill" &&
          pending.length === 0 &&
          !completed &&
          supportLoading;
        const blocked = pending.length === 0 || completed || waitingForInstaller;
        const checked = selected.has(item.resourceId) && !completed;
        const activeRestore = phase === "restoring" && checked && restoreActionIds(item).some((id) => !completedActionIds.has(id));
        const activeInstall = phase === "installing" && checked && dependencyActionIds(item).some((id) => !completedActionIds.has(id));
        const status = item.category === "global_tool"
          ? completed
            ? "Installed"
            : failed
              ? "Failed"
              : activeRestore || activeInstall
                ? "Installing"
                : checked || waitingForInstaller
                  ? "Waiting to install"
                  : "Needs approval"
          : completed
            ? "Restored"
            : failed
              ? "Failed"
              : activeRestore
                ? "Restoring"
                : requiresApproval(item) && !checked
                  ? "Needs approval"
                  : "Ready";
        return (
          <div
            key={item.resourceId}
            className={`v3-pull-item${checked ? " selected" : ""}${completed ? " completed" : ""}${failed ? " failed" : ""}`}
            role="listitem"
          >
            <label className="v3-pull-item-select">
              <input
                type="checkbox"
                aria-label={`Select ${item.title}`}
                checked={checked}
                disabled={disabled || blocked}
                onChange={() => onToggle(item.resourceId)}
              />
            </label>
            <span className="v3-pull-item-icon">
              <Icon
                name={failed ? "alert-triangle" : completed ? "check-circle" : item.toolKind === "plugin" ? "download" : "file"}
                size={17}
              />
            </span>
            <span className="v3-pull-item-copy">
              <strong>{item.title}</strong>
              <small>{itemKindLabel(item)}{item.provider ? ` · ${providerLabel(item.provider)}` : ""}</small>
              <span>{item.detail}</span>
            </span>
            <span className={`v3-pull-item-status ${status.toLowerCase().replace(/\s+/g, "-")}`}>{status}</span>
            <details className="v3-pull-item-details">
              <summary>Details</summary>
              <span><strong>Resource ID</strong><code>{item.resourceId}</code></span>
              {item.restoreActions.map((action) => (
                <span key={action.action_id}><strong>Restore action</strong><code>{action.action_id}</code></span>
              ))}
              {item.dependencyActions.map((action) => (
                <span key={action.action_id}><strong>Native action</strong><code>{action.action_id}</code></span>
              ))}
            </details>
          </div>
        );
      })}
    </div>
  );
}

export default function RestorePlanView({
  projectName,
  profileLabel,
  plan,
  binding,
  dependencyPlan,
  readiness,
  restoreResult,
  dependencyResult,
  phase,
  supportLoading,
  completedActionIds,
  completedResourceIds,
  failedResourceIds,
  busy,
  error,
  onApply,
  onRefresh,
  onBack,
}: Props) {
  const items = useMemo(() => buildPullReviewItems(plan, dependencyPlan), [plan, dependencyPlan]);
  const projectItems = useMemo(() => items.filter((item) => item.category === "project_data"), [items]);
  const globalItems = useMemo(() => items.filter((item) => item.category === "global_tool"), [items]);
  const knownItems = useRef(new Set<string>());
  const [selected, setSelected] = useState<Set<string>>(() => new Set(
    items.filter(defaultSelected).map((item) => item.resourceId),
  ));

  useEffect(() => {
    const currentIds = new Set(items.map((item) => item.resourceId));
    setSelected((current) => {
      const next = new Set([...current].filter((resourceId) => (
        currentIds.has(resourceId) && !completedResourceIds.has(resourceId)
      )));
      for (const item of items) {
        if (!knownItems.current.has(item.resourceId) && defaultSelected(item)) next.add(item.resourceId);
        if (failedResourceIds.has(item.resourceId)) next.add(item.resourceId);
      }
      return next;
    });
    knownItems.current = currentIds;
  }, [items, completedResourceIds, failedResourceIds]);

  const selection = useMemo(
    () => buildPullReviewSelection(items, selected, completedActionIds, completedResourceIds),
    [items, selected, completedActionIds, completedResourceIds],
  );
  const selectedCount = selection.resourceIds.length;
  const hasResult = phase === "complete" || restoreResult !== null || dependencyResult !== null;
  const failureCount = (restoreResult?.failed_actions?.length ?? 0) + (dependencyResult?.failed_actions?.length ?? 0);
  const skippedTools = globalItems.filter((item) => (
    !selected.has(item.resourceId) && !completedResourceIds.has(item.resourceId)
  )).length;
  const finalReady = hasResult && !error && failureCount === 0 && skippedTools === 0 && readiness?.state === "ready";
  const completedProjectItems = projectItems.filter((item) => completedResourceIds.has(item.resourceId));
  const completedSkills = globalItems.filter((item) => (
    item.toolKind === "custom_skill" && completedResourceIds.has(item.resourceId)
  ));
  const completedPlugins = globalItems.filter((item) => (
    item.toolKind === "plugin" && completedResourceIds.has(item.resourceId)
  ));
  const failedProjectItems = projectItems.filter((item) => failedResourceIds.has(item.resourceId));
  const failedTools = globalItems.filter((item) => failedResourceIds.has(item.resourceId));
  const outcomeTitle = error && hasResult
    ? "Pull needs attention"
    : failureCount > 0
      ? `${failureCount} change${failureCount === 1 ? "" : "s"} need attention`
      : skippedTools > 0
        ? `Project restored; ${skippedTools} setup item${skippedTools === 1 ? "" : "s"} skipped`
        : readiness?.state === "ready"
          ? "Pull complete"
          : "Project restored; setup needs attention";
  const buttonLabel = busy
    ? phase === "installing"
      ? "Installing selected tools…"
      : phase === "verifying"
        ? "Verifying readiness…"
        : "Restoring selected changes…"
    : hasResult && failedTools.length > 0 && selectedCount === failedResourceIds.size
      ? `Retry failed installation${failedTools.length === 1 ? "" : "s"}`
      : finalReady
        ? "Pull complete"
        : hasResult && selectedCount > 0
          ? `Apply ${selectedCount} remaining change${selectedCount === 1 ? "" : "s"}`
          : hasResult
            ? "Review complete"
        : `Apply ${selectedCount} selected change${selectedCount === 1 ? "" : "s"}`;

  const toggle = (resourceId: string) => setSelected((current) => {
    const next = new Set(current);
    if (next.has(resourceId)) next.delete(resourceId);
    else next.add(resourceId);
    return next;
  });
  const selectItems = (chosen: PullReviewItem[]) => setSelected((current) => {
    const next = new Set(current);
    for (const item of chosen) {
      if (pendingIds(item, completedActionIds).length > 0 && !completedResourceIds.has(item.resourceId)) {
        next.add(item.resourceId);
      }
    }
    return next;
  });

  return (
    <main className="v3-main v3-pull-review-workspace" aria-labelledby="v3-pull-review-title">
      <header className="v3-pull-review-header">
        <button type="button" className="btn btn-ghost v3-pull-back" onClick={onBack} disabled={busy}>
          <Icon name="chevron-left" size={15} /> Back to {projectName}
        </button>
        <span className="v3-eyebrow">Pull review · nothing changes until you apply</span>
        <div className="v3-pull-review-heading">
          <div>
            <h1 id="v3-pull-review-title">Restore {projectName}</h1>
            <p>{compactProjectPath(binding.project_root)}</p>
          </div>
          <span className="v3-pull-generation">Generation {plan.generation}</span>
        </div>
        <p className="v3-pull-review-intro">Review project data and global tools together. Each resource appears once, and one Apply action runs every approved phase.</p>
      </header>

      <div className="v3-pull-review-content">
        {phase !== "idle" && (
          <PhaseProgress
            phase={phase}
            ready={finalReady}
            restoreFailed={failedProjectItems.length > 0}
            installFailed={failedTools.length > 0}
          />
        )}

        {hasResult && (
          <section className={`v3-pull-outcome${finalReady ? " success" : " warning"}`} aria-live="polite">
            <Icon name={finalReady ? "check-circle" : "alert-triangle"} size={20} />
            <div>
              <h2>{outcomeTitle}</h2>
              <ul>
                {completedProjectItems.length > 0 && <li>{completedProjectItems.length} project data change{completedProjectItems.length === 1 ? "" : "s"} restored</li>}
                {completedSkills.length > 0 && <li>{completedSkills.length} custom skill{completedSkills.length === 1 ? "" : "s"} restored</li>}
                {completedPlugins.length > 0 && <li>{completedPlugins.map((item) => item.title).join(", ")} installed</li>}
                {finalReady && <li>Project is ready</li>}
              </ul>
            </div>
          </section>
        )}

        <section className="v3-pull-section" aria-labelledby="v3-pull-project-data">
          <div className="v3-pull-section-heading">
            <div>
              <h2 id="v3-pull-project-data">Project data</h2>
              <p>Conversations, files, and definitions. Existing targets are backed up before replacement.</p>
            </div>
            <button type="button" className="btn btn-ghost" disabled={busy || finalReady} onClick={() => selectItems(projectItems)}>Select recommended</button>
          </div>
          <ItemList
            items={projectItems}
            selected={selected}
            completedActionIds={completedActionIds}
            completedResourceIds={completedResourceIds}
            failedResourceIds={failedResourceIds}
            phase={phase}
            supportLoading={supportLoading}
            disabled={busy || finalReady}
            onToggle={toggle}
          />
          {projectItems.length === 0 && <div className="v3-inline-empty">No project data changes are needed.</div>}
        </section>

        <section className="v3-pull-section" aria-labelledby="v3-pull-global-tools">
          <div className="v3-pull-section-heading">
            <div>
              <h2 id="v3-pull-global-tools">Global tools <span>· installs into {profileLabel}</span></h2>
              <p>Custom skills restore from the bundle; plugins use the provider's native installer.</p>
            </div>
            <button type="button" className="btn btn-ghost" disabled={busy || finalReady || supportLoading} onClick={() => selectItems(globalItems)}>Select recommended</button>
          </div>
          <ItemList
            items={globalItems}
            selected={selected}
            completedActionIds={completedActionIds}
            completedResourceIds={completedResourceIds}
            failedResourceIds={failedResourceIds}
            phase={phase}
            supportLoading={supportLoading}
            disabled={busy || finalReady}
            onToggle={toggle}
          />
          {supportLoading && <div className="v3-pull-support-loading"><Icon name="refresh" className="icon-spin" size={14} /> Checking native installers and readiness…</div>}
          {!supportLoading && globalItems.length === 0 && <div className="v3-inline-empty">No global tools need to be installed.</div>}
          {(dependencyPlan?.blockers ?? []).map((blocker) => <div key={blocker} className="v3-callout error"><Icon name="alert-triangle" size={15} /> {blocker}</div>)}
          {(dependencyPlan?.warnings ?? []).map((warning) => <div key={warning} className="v3-callout"><Icon name="alert-triangle" size={15} /> {warning}</div>)}
        </section>

        {((readiness?.issues.length ?? 0) > 0 || (hasResult && !finalReady)) && (
          <section className="v3-pull-section" aria-labelledby="v3-pull-readiness">
            <div className="v3-pull-section-heading">
              <div>
                <h2 id="v3-pull-readiness">Readiness</h2>
                <p>Items that may still require setup after the selected changes run.</p>
              </div>
              <button type="button" className="btn btn-ghost" onClick={onRefresh} disabled={busy}><Icon name="refresh" size={13} /> Recheck</button>
            </div>
            <div className="v3-readiness-list">
              {(readiness?.issues ?? []).map((issue) => (
                <div key={issue.issue_id} className={`severity-${issue.severity ?? "info"}`}>
                  <Icon name={issue.severity === "error" ? "alert-triangle" : "check-circle"} size={14} />
                  <span><strong>{issue.title}</strong><small>{issue.detail}</small></span>
                  {issue.provider && <em>{providerLabel(issue.provider)}</em>}
                </div>
              ))}
              {(readiness?.issues.length ?? 0) === 0 && (
                <div className="severity-info">
                  <Icon name="refresh" size={14} />
                  <span><strong>Readiness is not confirmed</strong><small>Recheck after resolving the message above or selecting remaining setup items.</small></span>
                </div>
              )}
            </div>
          </section>
        )}

        {error && <div className="v3-callout error v3-pull-error" role="alert"><Icon name="alert-triangle" size={15} /> {error}</div>}
        <ResultDetails restoreResult={restoreResult} dependencyResult={dependencyResult} />
      </div>

      <footer className="v3-pull-apply-bar">
        <span>
          <strong>{hasResult ? outcomeTitle : `${selectedCount} selected change${selectedCount === 1 ? "" : "s"}`}</strong>
          <small>{hasResult ? "Review the result above or return to the project." : "Files, skills, plugins, then readiness verification."}</small>
        </span>
        <button
          type="button"
          className="btn btn-primary v3-pull-apply-button"
          disabled={busy || selectedCount === 0 || finalReady}
          onClick={() => onApply(selection)}
        >
          <Icon name={busy ? "refresh" : "check-circle"} size={16} className={busy ? "icon-spin" : undefined} />
          {buttonLabel}
        </button>
      </footer>
    </main>
  );
}
