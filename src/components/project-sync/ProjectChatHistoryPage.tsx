import { useEffect, useMemo, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import type {
  CodexThreadSummary,
  LocalProjectSummary,
  ProjectBinding,
  ProjectChatHistory,
  ThreadMatchKind,
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

interface ContentProps {
  project: LocalProjectSummary;
  binding: ProjectBinding | null;
  history: ProjectChatHistory | null;
  loading: boolean;
  loadingMore: boolean;
  actionError: string | null;
  actionBusyThreadId: string | null;
  onBranchChange: (branch: string) => void;
  onRefresh: () => void;
  onLoadMore: () => void;
  onOpenSettings: () => void;
  onOpenCodex: (threadId: string) => void;
  onOpenTerminal: (threadId: string) => void;
}

const THREAD_UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

function formatDate(value: number): string {
  if (!value) return "Unknown";
  return new Intl.DateTimeFormat(undefined, {
    month: "short", day: "numeric", year: "numeric", hour: "numeric", minute: "2-digit",
  }).format(new Date(value * 1_000));
}

function formatRange(thread: CodexThreadSummary): string {
  const start = formatDate(thread.started_at);
  const end = thread.is_active ? "Active" : formatDate(thread.ended_at);
  return start === end ? start : `${start} – ${end}`;
}

function matchLabel(kind: ThreadMatchKind): string {
  if (kind === "during_session") return "during session";
  if (kind === "after_session") return "after session";
  return "started from";
}

interface ThreadCardProps {
  thread: CodexThreadSummary;
  matchKind?: ThreadMatchKind;
  reason?: string;
  busy?: boolean;
  onOpenCodex: (threadId: string) => void;
  onOpenTerminal: (threadId: string) => void;
}

function ThreadCard({ thread, matchKind, reason, busy = false, onOpenCodex, onOpenTerminal }: ThreadCardProps) {
  const launchable = THREAD_UUID.test(thread.thread_id);
  return (
    <article className="v3-history-thread-card">
      <div className="v3-history-thread-copy">
        <div className="v3-history-thread-title-row">
          <strong>{thread.title || thread.summary || "Untitled Codex thread"}</strong>
          {matchKind && <span className={`v3-history-confidence ${matchKind}`}>{matchLabel(matchKind)}</span>}
          {thread.is_active && <span className="v3-history-active">Active</span>}
        </div>
        {thread.summary && thread.summary !== thread.title && <p>{thread.summary}</p>}
        <div className="v3-history-thread-meta">
          <span>{formatRange(thread)}</span>
          {thread.recorded_sha && <code title={thread.recorded_sha}>from {thread.recorded_sha.slice(0, 8)}</code>}
          {reason && <span className="v3-history-unmapped-reason">{reason}</span>}
        </div>
      </div>
      <div className="v3-history-thread-actions">
        <button type="button" className="btn btn-ghost" disabled={!launchable || busy}
          onClick={() => onOpenCodex(thread.thread_id)} title={launchable ? "Open this thread in the Codex desktop app" : "This thread ID cannot be launched"}>
          <Icon name="external-link" size={13} /> {busy ? "Opening…" : "Open in Codex"}
        </button>
        <button type="button" className="btn btn-ghost" disabled={!launchable || busy}
          onClick={() => onOpenTerminal(thread.thread_id)} title={launchable ? "Resume this thread in Terminal" : "This thread ID cannot be launched"}>
          <Icon name="terminal" size={13} /> Open in Terminal
        </button>
      </div>
    </article>
  );
}

export function ProjectChatHistoryContent({
  project, binding, history, loading, loadingMore, actionError, actionBusyThreadId, onBranchChange, onRefresh,
  onLoadMore, onOpenSettings, onOpenCodex, onOpenTerminal,
}: ContentProps) {
  const label = projectLabel(project);
  const aliased = label !== project.display_name;
  const threads = useMemo(() => new Map((history?.threads ?? []).map((thread) => [thread.thread_id, thread])), [history?.threads]);
  const hasCodexProfile = !!binding?.profile_ids?.codex;
  const settingsCanRecover = !!actionError && /profile|active binding|project root/i.test(actionError);

  return (
    <main className="v3-main v3-project-links-page v3-git-info-page v3-history-page">
      <section className="profile-links-section" aria-labelledby="git-info-heading">
        <header className="profile-links-heading v3-history-header">
          <div className="profile-links-copy">
            <h1 id="git-info-heading" className="settings-section-title">{label} history</h1>
            <div className="profile-links-subtitle v3-history-project-meta">
              <span>{compactProjectPath(binding?.project_root ?? project.project_root ?? "Project path unavailable")}</span>
              {aliased && <span>Repository: {project.display_name}</span>}
            </div>
          </div>
          <div className="v3-history-toolbar">
            {history?.git && (
              <label className="v3-history-branch-select">
                <span>Branch</span>
                <select value={history.git.selected_branch} onChange={(event) => onBranchChange(event.target.value)} disabled={loading}>
                  {history.git.branches.map((branch) => (
                    <option key={branch.name} value={branch.name}>{branch.name}{branch.is_current ? " (current)" : ""}{!branch.available ? " (unavailable)" : ""}</option>
                  ))}
                </select>
              </label>
            )}
            <button type="button" className="btn" onClick={onRefresh} disabled={loading}>
              <Icon name="refresh" size={14} className={loading ? "icon-spin" : undefined} /> Refresh
            </button>
          </div>
        </header>

        {actionError && (
          <div className="v3-callout error v3-history-error" role="alert">
            <Icon name="alert-triangle" size={15} />
            <span>{actionError}</span>
            {settingsCanRecover && <button type="button" className="btn btn-ghost" onClick={onOpenSettings}>Open Project Settings</button>}
          </div>
        )}

        {!hasCodexProfile ? (
          <div className="v3-history-state v3-history-profile-state">
            <Icon name="alert-triangle" size={18} />
            <div><strong>Choose a Codex profile to view this project’s threads.</strong><span>History only scans the profile bound to this project.</span></div>
            <button type="button" className="btn btn-primary" onClick={onOpenSettings}>Open Project Settings</button>
          </div>
        ) : loading && !history ? (
          <div className="v3-history-state"><span className="status-loader" /> Loading project history…</div>
        ) : !history ? null : (
          <>
            {history.warnings.length > 0 && (
              <div className="v3-callout warning" role="status"><Icon name="alert-triangle" size={15} /> {history.warnings.join(" ")}</div>
            )}
            {history.git ? (
              <section className="v3-history-commit-section" aria-label={`First-parent commits on ${history.git.selected_branch}`}>
                <div className="v3-history-section-heading">
                  <div><h2>First-parent history</h2><span>{history.git.unique_thread_count} threads · {history.git.reference_count} commit references</span></div>
                </div>
                {history.git.commits.length === 0 ? (
                  <div className="v3-history-state">No commits are available on this branch.</div>
                ) : (
                  <ol className="v3-history-commit-rail">
                    {history.git.commits.map((commit) => (
                      <li key={commit.sha} className="v3-history-commit">
                        <span className="v3-history-commit-node" aria-hidden="true" />
                        <div className="v3-history-commit-heading">
                          <code title={commit.sha}>{commit.short_sha}</code>
                          <time dateTime={new Date(commit.committed_at * 1_000).toISOString()}>{formatDate(commit.committed_at)}</time>
                          <strong>{commit.subject}</strong>
                        </div>
                        {commit.thread_refs.length > 0 && (
                          <div className="v3-history-thread-list">
                            {commit.thread_refs.map((reference) => {
                              const thread = threads.get(reference.thread_id);
                              return thread ? <ThreadCard key={`${commit.sha}:${reference.thread_id}`} thread={thread} matchKind={reference.match_kind} busy={actionBusyThreadId === thread.thread_id} onOpenCodex={onOpenCodex} onOpenTerminal={onOpenTerminal} /> : null;
                            })}
                          </div>
                        )}
                      </li>
                    ))}
                  </ol>
                )}
                {history.git.next_cursor && <button type="button" className="btn v3-history-load-more" disabled={loadingMore} onClick={onLoadMore}>{loadingMore ? "Loading…" : "Load older commits"}</button>}
              </section>
            ) : (
              <section aria-labelledby="codex-threads-heading">
                <div className="v3-history-section-heading"><div><h2 id="codex-threads-heading">Codex threads</h2><span>Ordered by last update</span></div></div>
                <div className="v3-history-thread-list flat">
                  {history.threads.length > 0 ? history.threads.map((thread) => <ThreadCard key={thread.thread_id} thread={thread} busy={actionBusyThreadId === thread.thread_id} onOpenCodex={onOpenCodex} onOpenTerminal={onOpenTerminal} />) : <div className="v3-history-state">No project-owned Codex threads were found.</div>}
                </div>
              </section>
            )}

            {history.unmapped.length > 0 && (
              <section className="v3-history-unmapped" aria-labelledby="unmapped-heading">
                <div className="v3-history-section-heading"><div><h2 id="unmapped-heading">Unmapped threads</h2><span>Not attached to a visible commit</span></div></div>
                <div className="v3-history-thread-list flat">
                  {history.unmapped.map((reference) => {
                    const thread = threads.get(reference.thread_id);
                    return thread ? <ThreadCard key={reference.thread_id} thread={thread} reason={reference.reason} busy={actionBusyThreadId === thread.thread_id} onOpenCodex={onOpenCodex} onOpenTerminal={onOpenTerminal} /> : null;
                  })}
                </div>
              </section>
            )}
          </>
        )}
      </section>
    </main>
  );
}

function mergePages(previous: ProjectChatHistory | null, next: ProjectChatHistory): ProjectChatHistory {
  if (!previous || !previous.git || !next.git || previous.git.selected_branch !== next.git.selected_branch) return next;
  const threads = new Map(previous.threads.map((thread) => [thread.thread_id, thread]));
  next.threads.forEach((thread) => threads.set(thread.thread_id, thread));
  const commits = [...previous.git.commits, ...next.git.commits];
  return {
    ...next,
    threads: [...threads.values()],
    git: {
      ...next.git,
      commits,
    },
  };
}

export default function ProjectChatHistoryPage({ project, binding, refreshEpoch, onOpenProjectSettings }: PageProps) {
  const [history, setHistory] = useState<ProjectChatHistory | null>(null);
  const [branch, setBranch] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [actionBusyThreadId, setActionBusyThreadId] = useState<string | null>(null);
  const requestRef = useRef(0);

  const load = async (beforeCommit?: string | null, requestedBranch = branch) => {
    if (!binding?.profile_ids?.codex) return;
    const requestId = ++requestRef.current;
    if (beforeCommit) setLoadingMore(true); else setLoading(true);
    setActionError(null);
    try {
      const next = await projectSyncApi.getProjectChatHistory(project.local_project_id, requestedBranch, beforeCommit);
      if (requestId !== requestRef.current) return;
      setHistory((current) => beforeCommit ? mergePages(current, next) : next);
      setBranch(next.git?.selected_branch ?? null);
    } catch (reason) {
      if (requestId === requestRef.current) setActionError(errorMessage(reason));
    } finally {
      if (requestId === requestRef.current) { setLoading(false); setLoadingMore(false); }
    }
  };

  useEffect(() => {
    setHistory(null);
    setBranch(null);
    void load(null, null);
    return () => { requestRef.current += 1; };
    // Project selection and restore completion are intentional reload boundaries.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [project.local_project_id, binding?.revision, refreshEpoch]);

  const changeBranch = (nextBranch: string) => {
    setBranch(nextBranch);
    setHistory(null);
    void load(null, nextBranch);
  };

  const openCodex = async (threadId: string) => {
    setActionError(null);
    if (!THREAD_UUID.test(threadId)) { setActionError("This thread has an invalid ID and cannot be opened."); return; }
    setActionBusyThreadId(threadId);
    try {
      // Re-read through the ownership-enforcing backend immediately before
      // opening the custom URI; a stale card cannot launch another project's
      // thread after a binding/profile change.
      await projectSyncApi.validateCodexThreadOwnership(project.local_project_id, threadId);
      try {
        await openUrl(`codex://threads/${threadId}`);
      } catch {
        setActionError("The Codex app could not open this thread. Use Open in Terminal instead.");
      }
    }
    catch (reason) { setActionError(`Could not verify this thread: ${errorMessage(reason)}`); }
    finally { setActionBusyThreadId(null); }
  };

  const openTerminal = async (threadId: string) => {
    setActionError(null);
    setActionBusyThreadId(threadId);
    try { await projectSyncApi.openCodexThreadInTerminal(project.local_project_id, threadId); }
    catch (reason) { setActionError(errorMessage(reason)); }
    finally { setActionBusyThreadId(null); }
  };

  return <ProjectChatHistoryContent project={project} binding={binding} history={history} loading={loading} loadingMore={loadingMore} actionError={actionError} actionBusyThreadId={actionBusyThreadId} onBranchChange={changeBranch} onRefresh={() => void load(null, branch)} onLoadMore={() => void load(history?.git?.next_cursor ?? null, branch)} onOpenSettings={onOpenProjectSettings} onOpenCodex={(id) => void openCodex(id)} onOpenTerminal={(id) => void openTerminal(id)} />;
}
