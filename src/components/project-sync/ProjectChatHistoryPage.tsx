import { useEffect, useMemo, useRef, useState } from "react";
import type {
  CodexThreadDetailsPage,
  CodexThreadSummary,
  LocalProjectSummary,
  ProjectBinding,
  ProjectChatHistory,
} from "../../types";
import Icon from "../Icons";
import { projectSyncApi } from "./api";
import { compactProjectPath, errorMessage, projectLabel } from "./model";

interface PageProps {
  project: LocalProjectSummary;
  binding: ProjectBinding | null;
  refreshEpoch: number;
  onOpenProjectSettings: () => void;
}

interface ThreadDetailsState {
  loading: boolean;
  error: string | null;
  page: CodexThreadDetailsPage | null;
}

interface ContentProps {
  project: LocalProjectSummary;
  binding: ProjectBinding | null;
  history: ProjectChatHistory | null;
  loading: boolean;
  loadingMore: boolean;
  actionError: string | null;
  actionBusyThreadId: string | null;
  detailsByThread?: Record<string, ThreadDetailsState>;
  openDetailOccurrences?: ReadonlySet<string>;
  onBranchChange: (branch: string) => void;
  onRefresh: () => void;
  onLoadMore: () => void;
  onOpenSettings: () => void;
  onOpenCodex: (threadId: string) => void;
  onOpenTerminal: (threadId: string) => void;
  onToggleDetails?: (threadId: string, occurrenceKey: string) => void;
  onLoadMoreDetails?: (threadId: string) => void;
}

const THREAD_UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

function formatDate(value?: number | null): string {
  if (!value) return "Not recorded";
  return new Intl.DateTimeFormat(undefined, {
    month: "short", day: "numeric", year: "numeric", hour: "numeric", minute: "2-digit",
  }).format(new Date(value * 1_000));
}

function formatCount(value?: number | null): string {
  if (value == null) return "Not reported";
  return new Intl.NumberFormat(undefined, { notation: value >= 1_000 ? "compact" : "standard", maximumFractionDigits: 1 }).format(value);
}

interface ThreadCardProps {
  thread: CodexThreadSummary;
  occurrenceKey: string;
  busy?: boolean;
  details?: ThreadDetailsState;
  detailsOpen?: boolean;
  onOpenCodex: (threadId: string) => void;
  onOpenTerminal: (threadId: string) => void;
  onToggleDetails?: (threadId: string, occurrenceKey: string) => void;
  onLoadMoreDetails?: (threadId: string) => void;
}

function ThreadCard({
  thread, occurrenceKey, busy = false, details, detailsOpen = false, onOpenCodex, onOpenTerminal, onToggleDetails, onLoadMoreDetails,
}: ThreadCardProps) {
  const launchable = THREAD_UUID.test(thread.thread_id);
  const detailsId = `thread-details-${occurrenceKey.replace(/[^a-z0-9_-]/gi, "-")}`;
  return (
    <article className="v3-history-thread-card">
      <div className="v3-history-thread-topline">
        <strong>{thread.title || "Untitled Codex session"}</strong>
        <div className="v3-history-thread-actions">
          <button type="button" className="btn btn-ghost" disabled={!launchable || busy}
            onClick={() => onOpenCodex(thread.thread_id)} title="Open this session in the Codex desktop app">
            <Icon name="external-link" size={13} /> {busy ? "Opening…" : "Open in Codex"}
          </button>
          <button type="button" className="btn btn-ghost" disabled={!launchable || busy}
            onClick={() => onOpenTerminal(thread.thread_id)} title="Resume this session in Terminal">
            <Icon name="terminal" size={13} /> Open in Terminal
          </button>
        </div>
      </div>

      <dl className="v3-history-session-facts">
        <div><dt>Started</dt><dd>{formatDate(thread.started_at)}</dd></div>
        <div><dt>{thread.is_active ? "Last activity" : "Ended"}</dt><dd>{formatDate(thread.ended_at)}{thread.is_active ? " · Active" : ""}</dd></div>
      </dl>
      <dl className="v3-history-metrics" aria-label="Session metrics">
        <div><dt>User rounds</dt><dd>{thread.user_round_count}</dd></div>
        <div><dt>Total tokens</dt><dd>{formatCount(thread.total_tokens)}</dd></div>
        <div><dt>Agent messages</dt><dd>{thread.agent_message_count}</dd></div>
        <div><dt>Tool calls</dt><dd>{thread.tool_call_count}</dd></div>
      </dl>
      {!thread.metrics_complete && <p className="v3-history-partial">Some session metrics are unavailable.</p>}
      {thread.commit_occurrence_count > 1 && (
        <p className="v3-history-occurrences">Appears under {thread.commit_occurrence_count} commits</p>
      )}

      {onToggleDetails && (
        <button type="button" className="v3-history-details-toggle" aria-expanded={detailsOpen}
          aria-controls={detailsId} onClick={() => onToggleDetails(thread.thread_id, occurrenceKey)}>
          <Icon name={detailsOpen ? "chevron-down" : "chevron-right"} size={13} />
          {detailsOpen ? "Hide chat details" : "Show chat details"}
        </button>
      )}
      {detailsOpen && (
        <div id={detailsId} className="v3-history-chat-details" aria-live="polite">
          {details?.loading && !details.page ? <div className="v3-history-detail-state"><span className="status-loader" /> Loading messages…</div> : null}
          {details?.error && <div className="v3-history-detail-state error" role="alert">{details.error}</div>}
          {details?.page?.turns.map((turn) => (
            <div key={`${thread.thread_id}:${turn.ordinal}`} className="v3-history-turn">
              <span>{turn.role === "user" ? "User" : "Codex"}</span>
              <time>{formatDate(turn.timestamp)}</time>
              <p title={turn.preview}>{turn.preview}</p>
            </div>
          ))}
          {details?.page?.next_cursor != null && (
            <button type="button" className="btn btn-ghost" disabled={details.loading}
              onClick={() => onLoadMoreDetails?.(thread.thread_id)}>
              {details.loading ? "Loading…" : "Load more messages"}
            </button>
          )}
        </div>
      )}
    </article>
  );
}

export function ProjectChatHistoryContent({
  project, binding, history, loading, loadingMore, actionError, actionBusyThreadId,
  detailsByThread = {}, openDetailOccurrences = new Set(), onBranchChange, onRefresh, onLoadMore, onOpenSettings, onOpenCodex,
  onOpenTerminal, onToggleDetails, onLoadMoreDetails,
}: ContentProps) {
  const label = projectLabel(project);
  const aliased = label !== project.display_name;
  const threads = useMemo(() => new Map((history?.threads ?? []).map((thread) => [thread.thread_id, thread])), [history?.threads]);
  const hasCodexProfile = !!binding?.profile_ids?.codex;
  const settingsCanRecover = !!actionError && /profile|active binding|project root/i.test(actionError);
  const renderThread = (thread: CodexThreadSummary, key: string) => (
    <ThreadCard key={key} thread={thread} occurrenceKey={key} busy={actionBusyThreadId === thread.thread_id}
      details={detailsByThread[thread.thread_id]} detailsOpen={openDetailOccurrences.has(key)} onOpenCodex={onOpenCodex} onOpenTerminal={onOpenTerminal}
      onToggleDetails={onToggleDetails} onLoadMoreDetails={onLoadMoreDetails} />
  );
  const orderedReferences = (references: { thread_id: string }[]) => [...references].sort((left, right) => {
    const leftThread = threads.get(left.thread_id);
    const rightThread = threads.get(right.thread_id);
    if (!leftThread || !rightThread) return left.thread_id.localeCompare(right.thread_id);
    return rightThread.ended_at - leftThread.ended_at
      || leftThread.started_at - rightThread.started_at
      || left.thread_id.localeCompare(right.thread_id);
  });

  return (
    <main className="v3-main v3-project-links-page v3-git-info-page v3-history-page">
      <section className="profile-links-section" aria-labelledby="project-activity-heading">
        <header className="profile-links-heading v3-history-header">
          <div className="profile-links-copy">
            <h1 id="project-activity-heading" className="settings-section-title">{label} activity</h1>
            {aliased && <span className="profile-links-subtitle">Repository: {project.display_name}</span>}
          </div>
          <div className="v3-history-toolbar">
            {history?.git && (
              <label className="v3-history-branch-select"><span>Branch</span>
                <select value={history.git.selected_branch} onChange={(event) => onBranchChange(event.target.value)} disabled={loading}>
                  {history.git.branches.map((branch) => <option key={branch.name} value={branch.name}>{branch.name}{branch.is_current ? " (current)" : ""}{!branch.available ? " (unavailable)" : ""}</option>)}
                </select>
              </label>
            )}
            <button type="button" className="btn" onClick={onRefresh} disabled={loading}><Icon name="refresh" size={14} className={loading ? "icon-spin" : undefined} /> Refresh</button>
          </div>
        </header>

        {actionError && <div className="v3-callout error v3-history-error" role="alert"><Icon name="alert-triangle" size={15} /><span>{actionError}</span>{settingsCanRecover && <button type="button" className="btn btn-ghost" onClick={onOpenSettings}>Open Project Settings</button>}</div>}
        {!hasCodexProfile ? (
          <div className="v3-history-state v3-history-profile-state"><Icon name="alert-triangle" size={18} /><div><strong>Choose a Codex profile to view this project’s sessions.</strong><span>Activity only scans the profile bound to this project.</span></div><button type="button" className="btn btn-primary" onClick={onOpenSettings}>Open Project Settings</button></div>
        ) : loading && !history ? <div className="v3-history-state" role="status" aria-live="polite"><span className="status-loader" /> Loading project activity…</div> : !history ? null : (
          <>
            <section className="v3-history-project-summary" aria-label="Project information">
              <dl>
                <div><dt>Project Name</dt><dd>{label}</dd></div>
                <div><dt>Directory</dt><dd title={binding?.canonical_project_root ?? binding?.project_root}>{compactProjectPath(binding?.canonical_project_root ?? binding?.project_root ?? project.project_root ?? "Not configured")}</dd></div>
                <div><dt>Codex configuration</dt><dd title={history.codex_home}>{history.codex_home}</dd><button type="button" className="btn btn-ghost" onClick={onOpenSettings}>Settings</button></div>
              </dl>
              <div className="v3-history-storage-sync">
                <h2>Storage sync</h2>
                {history.storage_sync.length ? history.storage_sync.map((storage) => (
                  <div key={storage.storage_id} className="v3-history-storage-row"><strong>{storage.storage_name}</strong><span>Last Pull <time>{formatDate(storage.last_pull_at)}</time></span><span>Last Push <time>{formatDate(storage.last_push_at)}</time></span></div>
                )) : <p>No storage is linked to this project.</p>}
              </div>
            </section>

            {history.git ? (
              <>
                {history.unmapped.length > 0 && (
                  <section className="v3-history-uncommitted" aria-labelledby="uncommitted-heading">
                    <div className="v3-history-section-heading"><div><h2 id="uncommitted-heading">Uncommitted Changes</h2><span>Sessions not linked to a commit in this branch</span></div></div>
                    <div className="v3-history-thread-list flat">{history.unmapped.map((reference) => { const thread = threads.get(reference.thread_id); return thread ? renderThread(thread, `uncommitted:${thread.thread_id}`) : null; })}</div>
                  </section>
                )}
                <section className="v3-history-commit-section" aria-label={`First-parent commits on ${history.git.selected_branch}`}>
                  <div className="v3-history-section-heading"><div><h2>Commit history</h2><span>{history.git.unique_thread_count} sessions · {history.git.reference_count} commit occurrences</span></div></div>
                  {history.git.commits.length === 0 ? <div className="v3-history-state">No commits are available in this 30-day window.</div> : (
                    <ol className="v3-history-commit-rail">{history.git.commits.map((commit) => (
                      <li key={commit.sha} className="v3-history-commit"><span className="v3-history-commit-node" aria-hidden="true" />
                        <div className="v3-history-commit-heading"><code title={commit.sha}>{commit.short_sha}</code><time dateTime={new Date(commit.committed_at * 1_000).toISOString()}>{formatDate(commit.committed_at)}</time><strong>{commit.subject}</strong></div>
                        {commit.thread_refs.length > 0 && <div className="v3-history-thread-list">{orderedReferences(commit.thread_refs).map((reference) => { const thread = threads.get(reference.thread_id); return thread ? renderThread(thread, `${commit.sha}:${thread.thread_id}`) : null; })}</div>}
                      </li>
                    ))}</ol>
                  )}
                </section>
              </>
            ) : (
              <section aria-labelledby="codex-sessions-heading"><div className="v3-history-section-heading"><div><h2 id="codex-sessions-heading">Codex threads</h2><span>Ordered by last activity</span></div></div><div className="v3-history-thread-list flat">{history.threads.length ? history.threads.map((thread) => renderThread(thread, thread.thread_id)) : <div className="v3-history-state">No project-owned Codex sessions were found in this 30-day window.</div>}</div></section>
            )}
            {history.next_before != null && <button type="button" className="btn v3-history-load-more" disabled={loadingMore} onClick={onLoadMore}>{loadingMore ? "Loading…" : "Load previous 30 days"}</button>}
          </>
        )}
      </section>
    </main>
  );
}

function mergePages(previous: ProjectChatHistory | null, next: ProjectChatHistory): ProjectChatHistory {
  if (!previous || previous.git?.selected_branch !== next.git?.selected_branch) return next;
  const threads = new Map(previous.threads.map((thread) => [thread.thread_id, thread]));
  next.threads.forEach((thread) => threads.set(thread.thread_id, thread));
  const unmapped = new Map(previous.unmapped.map((item) => [item.thread_id, item]));
  next.unmapped.forEach((item) => unmapped.set(item.thread_id, item));
  if (!previous.git || !next.git) return { ...next, threads: [...threads.values()], unmapped: [...unmapped.values()] };
  const commits = new Map(previous.git.commits.map((commit) => [commit.sha, commit]));
  next.git.commits.forEach((commit) => {
    const existing = commits.get(commit.sha);
    if (!existing) { commits.set(commit.sha, commit); return; }
    const references = new Map(existing.thread_refs.map((reference) => [reference.thread_id, reference]));
    commit.thread_refs.forEach((reference) => references.set(reference.thread_id, reference));
    commits.set(commit.sha, { ...commit, thread_refs: [...references.values()] });
  });
  const mergedCommits = [...commits.values()].sort((left, right) => right.committed_at - left.committed_at);
  const references = mergedCommits.reduce((count, commit) => count + commit.thread_refs.length, 0);
  const unique = new Set(mergedCommits.flatMap((commit) => commit.thread_refs.map((reference) => reference.thread_id))).size;
  const occurrences = new Map<string, number>();
  mergedCommits.forEach((commit) => commit.thread_refs.forEach((reference) => occurrences.set(reference.thread_id, (occurrences.get(reference.thread_id) ?? 0) + 1)));
  const mergedThreads = [...threads.values()].map((thread) => ({ ...thread, commit_occurrence_count: occurrences.get(thread.thread_id) ?? 0 }));
  return { ...next, threads: mergedThreads, unmapped: [...unmapped.values()], git: { ...next.git, commits: mergedCommits, reference_count: references, unique_thread_count: unique } };
}

export default function ProjectChatHistoryPage({ project, binding, refreshEpoch, onOpenProjectSettings }: PageProps) {
  const [history, setHistory] = useState<ProjectChatHistory | null>(null);
  const [branch, setBranch] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [actionBusyThreadId, setActionBusyThreadId] = useState<string | null>(null);
  const [detailsByThread, setDetailsByThread] = useState<Record<string, ThreadDetailsState>>({});
  const [openDetailOccurrences, setOpenDetailOccurrences] = useState<Set<string>>(new Set());
  const requestRef = useRef(0);
  const detailRequestRef = useRef<Record<string, number>>({});
  const detailSequenceRef = useRef(0);
  const contextKey = `${project.local_project_id}:${binding?.revision ?? "none"}:${binding?.profile_ids?.codex ?? "none"}`;
  const contextKeyRef = useRef(contextKey);
  contextKeyRef.current = contextKey;

  const load = async (beforeTime?: number | null, requestedBranch = branch, forceRevalidate = false) => {
    if (!binding?.profile_ids?.codex) return;
    const requestId = ++requestRef.current;
    if (beforeTime) setLoadingMore(true); else setLoading(true);
    setActionError(null);
    try {
      const next = await projectSyncApi.getProjectChatHistory(project.local_project_id, requestedBranch, beforeTime, 30, forceRevalidate);
      if (requestId !== requestRef.current) return;
      setHistory((current) => beforeTime ? mergePages(current, next) : next);
      setBranch(next.git?.selected_branch ?? null);
    } catch (reason) {
      if (requestId === requestRef.current) setActionError(errorMessage(reason));
    } finally {
      if (requestId === requestRef.current) { setLoading(false); setLoadingMore(false); }
    }
  };

  useEffect(() => {
    detailRequestRef.current = {};
    setHistory(null); setBranch(null); setDetailsByThread({}); setOpenDetailOccurrences(new Set()); void load(null, null);
    return () => { requestRef.current += 1; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [project.local_project_id, binding?.revision, refreshEpoch]);

  const changeBranch = (nextBranch: string) => { setBranch(nextBranch); setHistory(null); setDetailsByThread({}); setOpenDetailOccurrences(new Set()); void load(null, nextBranch); };
  const openCodex = async (threadId: string) => {
    setActionError(null);
    if (!THREAD_UUID.test(threadId)) { setActionError("This session has an invalid ID and cannot be opened."); return; }
    setActionBusyThreadId(threadId);
    try { await projectSyncApi.openCodexThreadInApp(project.local_project_id, threadId); }
    catch (reason) { setActionError(errorMessage(reason)); }
    finally { setActionBusyThreadId(null); }
  };
  const openTerminal = async (threadId: string) => {
    setActionError(null); setActionBusyThreadId(threadId);
    try { await projectSyncApi.openCodexThreadInTerminal(project.local_project_id, threadId); }
    catch (reason) { setActionError(errorMessage(reason)); }
    finally { setActionBusyThreadId(null); }
  };
  const loadDetails = async (threadId: string, cursor?: number | null) => {
    const projectId = project.local_project_id;
    const requestContext = contextKey;
    const requestId = ++detailSequenceRef.current;
    detailRequestRef.current[threadId] = requestId;
    setDetailsByThread((current) => ({ ...current, [threadId]: { loading: true, error: null, page: current[threadId]?.page ?? null } }));
    try {
      const next = await projectSyncApi.getProjectChatThreadDetails(projectId, threadId, cursor);
      if (contextKeyRef.current !== requestContext || detailRequestRef.current[threadId] !== requestId) return;
      setDetailsByThread((current) => {
        const previous = current[threadId]?.page;
        const turns = cursor && previous ? [...previous.turns, ...next.turns] : next.turns;
        return { ...current, [threadId]: { loading: false, error: null, page: { ...next, turns } } };
      });
    } catch (reason) {
      if (contextKeyRef.current !== requestContext || detailRequestRef.current[threadId] !== requestId) return;
      setDetailsByThread((current) => ({ ...current, [threadId]: { loading: false, error: errorMessage(reason), page: current[threadId]?.page ?? null } }));
    }
  };
  const toggleDetails = (threadId: string, occurrenceKey: string) => {
    const wasOpen = openDetailOccurrences.has(occurrenceKey);
    setOpenDetailOccurrences((current) => {
      const next = new Set(current);
      if (wasOpen) next.delete(occurrenceKey); else next.add(occurrenceKey);
      return next;
    });
    if (!wasOpen && !detailsByThread[threadId]?.page && !detailsByThread[threadId]?.loading) void loadDetails(threadId);
  };

  return <ProjectChatHistoryContent project={project} binding={binding} history={history} loading={loading} loadingMore={loadingMore} actionError={actionError} actionBusyThreadId={actionBusyThreadId} detailsByThread={detailsByThread} openDetailOccurrences={openDetailOccurrences} onBranchChange={changeBranch} onRefresh={() => void load(null, branch, true)} onLoadMore={() => void load(history?.next_before ?? null, branch)} onOpenSettings={onOpenProjectSettings} onOpenCodex={(id) => void openCodex(id)} onOpenTerminal={(id) => void openTerminal(id)} onToggleDetails={toggleDetails} onLoadMoreDetails={(id) => void loadDetails(id, detailsByThread[id]?.page?.next_cursor)} />;
}
