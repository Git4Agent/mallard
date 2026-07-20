import { Fragment, useEffect, useMemo, useRef, useState } from "react";
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
  embedded?: boolean;
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
  const [expandedItems, setExpandedItems] = useState<Set<string>>(() => new Set());
  const toggleDetails = (resourceId: string) => setExpandedItems((current) => {
    const next = new Set(current);
    if (next.has(resourceId)) next.delete(resourceId);
    else next.add(resourceId);
    return next;
  });

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
        const expanded = expandedItems.has(item.resourceId);
        const activeRestore = phase === "restoring" && checked && restoreActionIds(item).some((id) => !completedActionIds.has(id));
        const activeInstall = phase === "installing" && checked && dependencyActionIds(item).some((id) => !completedActionIds.has(id));
        const status: string | null = item.category === "global_tool"
          ? completed
            ? "Installed"
            : failed
              ? "Failed"
              : activeRestore || activeInstall
                ? "Installing"
                : checked || waitingForInstaller
                  ? null
                  : "Needs approval"
          : completed
            ? "Restored"
            : failed
              ? "Failed"
              : activeRestore
                ? "Restoring"
                : requiresApproval(item) && !checked
                  ? "Needs approval"
                  : checked
                    ? null
                    : "Not selected";
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
            <button
              type="button"
              className="v3-pull-item-main"
              onClick={() => toggleDetails(item.resourceId)}
              aria-expanded={expanded}
              aria-label={`${expanded ? "Hide" : "Show"} details for ${item.title}`}
            >
              <span className="v3-pull-item-toggle">
                <Icon name={expanded ? "chevron-down" : "chevron-right"} size={12} />
              </span>
              <span className="v3-pull-item-icon">
                <Icon
                  name={failed ? "alert-triangle" : completed ? "check-circle" : item.toolKind === "plugin" ? "download" : item.toolKind === "custom_skill" ? "folder" : "file"}
                  size={14}
                />
              </span>
              <span className="v3-pull-item-copy">
                <strong>{item.title}</strong>
                <span>{itemKindLabel(item)}{item.provider ? ` · ${providerLabel(item.provider)}` : ""}</span>
              </span>
              {status && <span className={`v3-pull-item-status ${status.toLowerCase().replace(/\s+/g, "-")}`}>{status}</span>}
            </button>
            {expanded && (
              <div className="v3-pull-item-detail">
                <p>{item.detail}</p>
                <div className="v3-pull-item-detail-grid">
                  <span>Type</span><code>{itemKindLabel(item)}{item.provider ? ` · ${providerLabel(item.provider)}` : ""}</code>
                  <span>Resource</span><code>{item.resourceId}</code>
                  {item.restoreActions.map((action) => (
                    <Fragment key={action.action_id}><span>Restore</span><code>{action.action_id}</code></Fragment>
                  ))}
                  {item.dependencyActions.map((action) => (
                    <Fragment key={action.action_id}><span>Installer</span><code>{action.action_id}</code></Fragment>
                  ))}
                </div>
              </div>
            )}
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
  embedded = false,
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
        : `Apply ${selectedCount} change${selectedCount === 1 ? "" : "s"}`;

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
  const clearItems = (chosen: PullReviewItem[]) => setSelected((current) => {
    const next = new Set(current);
    for (const item of chosen) next.delete(item.resourceId);
    return next;
  });
  const pendingProjectItems = projectItems.filter((item) => (
    pendingIds(item, completedActionIds).length > 0 && !completedResourceIds.has(item.resourceId)
  ));
  const pendingGlobalItems = globalItems.filter((item) => (
    pendingIds(item, completedActionIds).length > 0 && !completedResourceIds.has(item.resourceId)
  ));
  const allProjectItemsSelected = pendingProjectItems.length > 0 && pendingProjectItems.every((item) => selected.has(item.resourceId));
  const allGlobalItemsSelected = pendingGlobalItems.length > 0 && pendingGlobalItems.every((item) => selected.has(item.resourceId));
  const dependencyMessages = (dependencyPlan?.blockers?.length ?? 0) + (dependencyPlan?.warnings?.length ?? 0);

  const ReviewContainer = embedded ? "section" : "main";

  return (
    <ReviewContainer
      className={embedded ? "v3-inline-action-review v3-inline-pull-review" : "v3-main v3-pull-review-workspace"}
      aria-labelledby="v3-pull-review-title"
    >
      <header className={embedded ? "v3-inline-action-header v3-inline-pull-header" : "v3-pull-review-header"}>
        {!embedded && (
          <button type="button" className="btn btn-ghost v3-pull-back" onClick={onBack} disabled={busy}>
            <Icon name="chevron-left" size={15} /> Back to {projectName}
          </button>
        )}
        <div className={embedded ? "v3-inline-action-heading" : "v3-pull-review-heading"}>
          <div>
            {embedded
              ? <h2 id="v3-pull-review-title">Pull changes</h2>
              : <h1 id="v3-pull-review-title">Restore {projectName}</h1>}
            <p className="v3-pull-review-meta">
              <span>{compactProjectPath(binding.project_root)}</span>
              <span aria-hidden="true">·</span>
              <span>Generation {plan.generation}</span>
            </p>
          </div>
        </div>
        {embedded && (
          <button
            type="button"
            className="btn btn-ghost v3-inline-action-close"
            onClick={onBack}
            disabled={busy}
            aria-label={`Close pull review for ${projectName}`}
          >
            <Icon name="x" size={15} />
          </button>
        )}
      </header>

      <div className={embedded ? "v3-pull-review-content v3-inline-action-content" : "v3-pull-review-content"}>
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

        {projectItems.length > 0 && (
          <section className="v3-pull-section" aria-labelledby="v3-pull-project-data">
            <div className="v3-pull-section-heading">
              <span className="v3-pull-section-icon"><Icon name="folder" size={16} /></span>
              <div>
                <h2 id="v3-pull-project-data">Project files</h2>
                <p>Backups are created before existing files are replaced.</p>
              </div>
              {pendingProjectItems.length > 0 && (
                <button
                  type="button"
                  className="btn btn-ghost v3-pull-group-action"
                  disabled={busy || finalReady}
                  onClick={() => allProjectItemsSelected ? clearItems(projectItems) : selectItems(projectItems)}
                >
                  {allProjectItemsSelected ? "Clear" : "Select all"}
                </button>
              )}
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
          </section>
        )}

        {(supportLoading || globalItems.length > 0 || dependencyMessages > 0) && (
          <section className="v3-pull-section" aria-labelledby="v3-pull-global-tools">
            <div className="v3-pull-section-heading">
              <span className="v3-pull-section-icon"><Icon name="folder" size={16} /></span>
              <div>
                <h2 id="v3-pull-global-tools">Tools</h2>
                <p>Install into {profileLabel}</p>
              </div>
              {pendingGlobalItems.length > 0 && (
                <button
                  type="button"
                  className="btn btn-ghost v3-pull-group-action"
                  disabled={busy || finalReady || supportLoading}
                  onClick={() => allGlobalItemsSelected ? clearItems(globalItems) : selectItems(globalItems)}
                >
                  {allGlobalItemsSelected ? "Clear" : "Select all"}
                </button>
              )}
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
            {supportLoading && <div className="v3-pull-support-loading"><Icon name="refresh" className="icon-spin" size={14} /> Checking installers…</div>}
            {(dependencyPlan?.blockers ?? []).map((blocker) => <div key={blocker} className="v3-callout error"><Icon name="alert-triangle" size={15} /> {blocker}</div>)}
            {(dependencyPlan?.warnings ?? []).map((warning) => <div key={warning} className="v3-callout"><Icon name="alert-triangle" size={15} /> {warning}</div>)}
          </section>
        )}

        {!supportLoading && items.length === 0 && (
          <div className="v3-pull-empty">
            <Icon name="check-circle" size={16} />
            <span><strong>Nothing to pull</strong><small>This project already matches the selected generation.</small></span>
          </div>
        )}

        {((readiness?.issues.length ?? 0) > 0 || (hasResult && !finalReady)) && (
          <section className="v3-pull-section" aria-labelledby="v3-pull-readiness">
            <div className="v3-pull-section-heading">
              <span className="v3-pull-section-icon"><Icon name="check-circle" size={16} /></span>
              <div>
                <h2 id="v3-pull-readiness">Readiness</h2>
                <p>{readiness?.issues.length ?? 0} check{readiness?.issues.length === 1 ? "" : "s"} may still need attention after pull.</p>
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

      <footer className={embedded ? "v3-pull-apply-bar v3-inline-action-footer" : "v3-pull-apply-bar"}>
        <span>
          <strong>{hasResult ? outcomeTitle : `${selectedCount} selected`}</strong>
          <small>{hasResult ? "Review the result above or return to the project." : "Nothing changes until you apply."}</small>
        </span>
        <button
          type="button"
          className="btn btn-primary v3-pull-apply-button"
          disabled={busy || selectedCount === 0 || finalReady}
          onClick={() => onApply(selection)}
        >
          {busy && <Icon name="refresh" size={16} className="icon-spin" />}
          {buttonLabel}
        </button>
      </footer>
    </ReviewContainer>
  );
}
