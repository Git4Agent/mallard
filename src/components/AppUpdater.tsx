import { useCallback, useEffect, useRef, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { isTauri } from "@tauri-apps/api/core";
import { confirm } from "@tauri-apps/plugin-dialog";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { APP_UPDATE_CHECK_EVENT } from "./AppUpdateControl";
import Icon from "./Icons";

const UPDATE_DEFERRED_KEY = "mallard.update.deferred";
const PENDING_CHANGELOG_KEY = "mallard.update.pending-changelog";
const LAST_SEEN_CHANGELOG_KEY = "mallard.update.last-seen-changelog";
const AUTO_CHECK_DELAY_MS = 8_000;
const AUTO_CHECK_INTERVAL_MS = 6 * 60 * 60 * 1_000;
const DEFER_UPDATE_MS = 24 * 60 * 60 * 1_000;
const LATEST_NOTICE_MS = 3_000;

export interface UpdateSummary {
  version: string;
  currentVersion: string;
  notes: string;
  date: string | null;
}

interface DeferredUpdate {
  version: string;
  until: number;
}

type UpdatePhase =
  | "idle"
  | "checking"
  | "latest"
  | "available"
  | "downloading"
  | "installing"
  | "error";

function readJson<T>(key: string): T | null {
  try {
    const raw = window.localStorage.getItem(key);
    return raw ? JSON.parse(raw) as T : null;
  } catch {
    return null;
  }
}

function writeJson(key: string, value: unknown): void {
  try {
    window.localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // Update persistence is helpful but must never prevent installation.
  }
}

function removeStoredValue(key: string): void {
  try {
    window.localStorage.removeItem(key);
  } catch {
    // Storage may be unavailable in hardened webviews.
  }
}

export function shouldDeferUpdate(
  deferred: DeferredUpdate | null,
  version: string,
  now = Date.now(),
): boolean {
  return deferred?.version === version && deferred.until > now;
}

export function downloadPercentage(downloaded: number, total: number): number | null {
  if (!Number.isFinite(downloaded) || !Number.isFinite(total) || total <= 0) return null;
  return Math.min(100, Math.max(0, Math.round((downloaded / total) * 100)));
}

export function releaseNotePreview(notes: string): string {
  const firstContentLine = notes
    .split(/\r?\n/)
    .map((line) => line.replace(/^\s*([#>*-]|\d+\.)+\s*/, "").trim())
    .find(Boolean);
  return firstContentLine || "This release includes improvements and fixes.";
}

export function describeUpdateError(reason: unknown): string {
  if (reason instanceof Error && reason.message.trim()) return reason.message.trim();
  if (typeof reason === "string" && reason.trim()) return reason.trim();
  try {
    const serialized = JSON.stringify(reason);
    if (serialized && serialized !== "{}") return serialized;
  } catch {
    // Host-owned errors may not be serializable.
  }
  return "Mallard could not complete the update. Check your connection and try again.";
}

interface UpdatePromptProps {
  summary: UpdateSummary;
  busy: boolean;
  error: string | null;
  onInstall: () => void;
  onLater: () => void;
  onRetry: () => void;
}

export function UpdatePrompt({
  summary,
  busy,
  error,
  onInstall,
  onLater,
  onRetry,
}: UpdatePromptProps) {
  return (
    <aside className={`app-update-notice${error ? " error" : ""}`} role="status" aria-live="polite">
      <div className="app-update-notice-icon" aria-hidden="true">
        <Icon name={error ? "alert-triangle" : "download"} size={17} />
      </div>
      <div className="app-update-notice-copy">
        <strong>{error ? "Update interrupted" : `Mallard ${summary.version} is available`}</strong>
        <span>{error || releaseNotePreview(summary.notes)}</span>
        {busy && !error && <small>Finish the current operation before restarting.</small>}
      </div>
      <div className="app-update-notice-actions">
        <button type="button" className="btn btn-ghost" onClick={onLater}>Later</button>
        {error ? (
          <button type="button" className="btn btn-primary" onClick={onRetry}>Try again</button>
        ) : (
          <button type="button" className="btn btn-primary" onClick={onInstall} disabled={busy}>
            Update and restart
          </button>
        )}
      </div>
    </aside>
  );
}

interface UpdateCheckNoticeProps {
  phase: "checking" | "latest" | "error";
  error: string | null;
  onDismiss: () => void;
  onRetry: () => void;
}

export function UpdateCheckNotice({
  phase,
  error,
  onDismiss,
  onRetry,
}: UpdateCheckNoticeProps) {
  const failed = phase === "error";
  const latest = phase === "latest";
  return (
    <aside className={`app-update-notice app-update-check-notice${failed ? " error" : ""}`} role="status" aria-live="polite">
      <div className="app-update-notice-icon" aria-hidden="true">
        <Icon
          name={failed ? "alert-triangle" : latest ? "check-circle" : "refresh"}
          size={17}
          className={phase === "checking" ? "icon-spin" : undefined}
        />
      </div>
      <div className="app-update-notice-copy">
        <strong>{failed ? "Couldn’t check for updates" : latest ? "Mallard is up to date" : "Checking for updates…"}</strong>
        <span>{failed ? error : latest ? "You already have the newest available version." : "Looking for a newer signed release on GitHub."}</span>
      </div>
      {phase !== "checking" && (
        <div className="app-update-notice-actions">
          <button type="button" className="btn btn-ghost" onClick={onDismiss}>Dismiss</button>
          {failed && <button type="button" className="btn btn-primary" onClick={onRetry}>Try again</button>}
        </div>
      )}
    </aside>
  );
}

interface UpdateProgressProps {
  phase: "downloading" | "installing";
  downloaded: number;
  total: number;
  version: string;
}

export function UpdateProgress({ phase, downloaded, total, version }: UpdateProgressProps) {
  const percentage = downloadPercentage(downloaded, total);
  const installing = phase === "installing";
  return (
    <div className="app-update-backdrop" role="dialog" aria-modal="true" aria-labelledby="app-update-title">
      <section className="app-update-progress-card">
        <div className="app-update-progress-icon" aria-hidden="true">
          <Icon name={installing ? "refresh" : "download"} size={20} className={installing ? "icon-spin" : undefined} />
        </div>
        <div>
          <h2 id="app-update-title">{installing ? "Installing update…" : `Downloading Mallard ${version}…`}</h2>
          <p>{installing ? "Mallard will restart when the update is ready." : "Keep Mallard open while the signed update is downloaded."}</p>
        </div>
        <div
          className={`app-update-progress-track${percentage === null ? " indeterminate" : ""}`}
          role="progressbar"
          aria-label="Update download progress"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={percentage ?? undefined}
        >
          <span style={percentage === null ? undefined : { width: `${percentage}%` }} />
        </div>
        <small>{installing ? "Verifying and replacing the application…" : percentage === null ? "Preparing download…" : `${percentage}% downloaded`}</small>
      </section>
    </div>
  );
}

interface WhatsNewProps {
  summary: UpdateSummary;
  onClose: () => void;
}

function WhatsNew({ summary, onClose }: WhatsNewProps) {
  return (
    <div className="app-update-backdrop" role="dialog" aria-modal="true" aria-labelledby="app-whats-new-title">
      <section className="app-whats-new-card">
        <header>
          <div className="app-update-progress-icon" aria-hidden="true"><Icon name="check-circle" size={20} /></div>
          <div>
            <span>Update complete</span>
            <h2 id="app-whats-new-title">What’s new in Mallard {summary.version}</h2>
          </div>
        </header>
        <div className="app-whats-new-body">
          {summary.notes.trim() ? (
            <ReactMarkdown
              remarkPlugins={[remarkGfm]}
              components={{
                a: ({ children }) => <span>{children}</span>,
                img: ({ alt }) => <span>{alt ?? ""}</span>,
              }}
            >
              {summary.notes}
            </ReactMarkdown>
          ) : (
            <p>Mallard was updated successfully with the latest improvements and fixes.</p>
          )}
        </div>
        <footer><button type="button" className="btn btn-primary" onClick={onClose}>Continue</button></footer>
      </section>
    </div>
  );
}

interface AppUpdaterProps {
  busy: boolean;
}

export default function AppUpdater({ busy }: AppUpdaterProps) {
  const [phase, setPhase] = useState<UpdatePhase>("idle");
  const [summary, setSummary] = useState<UpdateSummary | null>(null);
  const [changelog, setChangelog] = useState<UpdateSummary | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [downloaded, setDownloaded] = useState(0);
  const [total, setTotal] = useState(0);
  const updateRef = useRef<Update | null>(null);
  const summaryRef = useRef<UpdateSummary | null>(null);
  const mountedRef = useRef(true);
  const checkingRef = useRef(false);
  const installingRef = useRef(false);
  const lastCheckRef = useRef(0);
  const latestNoticeTimerRef = useRef<number | null>(null);
  const busyRef = useRef(busy);
  busyRef.current = busy;

  const clearLatestNoticeTimer = useCallback(() => {
    if (latestNoticeTimerRef.current !== null) {
      window.clearTimeout(latestNoticeTimerRef.current);
      latestNoticeTimerRef.current = null;
    }
  }, []);

  const closeUpdateHandle = useCallback(async () => {
    const current = updateRef.current;
    updateRef.current = null;
    if (current) {
      try {
        await current.close();
      } catch {
        // The updater resource may already be closed after an install failure.
      }
    }
  }, []);

  const checkForUpdates = useCallback(async (interactive = false) => {
    if (!isTauri() || checkingRef.current || installingRef.current) return;
    checkingRef.current = true;
    lastCheckRef.current = Date.now();
    clearLatestNoticeTimer();
    if (interactive) {
      setError(null);
      setPhase("checking");
    }
    try {
      const update = await check({ timeout: 30_000 });
      if (!mountedRef.current) {
        await update?.close();
        return;
      }
      if (!update) {
        await closeUpdateHandle();
        summaryRef.current = null;
        setSummary(null);
        setError(null);
        if (interactive) {
          setPhase("latest");
          latestNoticeTimerRef.current = window.setTimeout(() => {
            latestNoticeTimerRef.current = null;
            if (mountedRef.current) setPhase((current) => current === "latest" ? "idle" : current);
          }, LATEST_NOTICE_MS);
        } else {
          setPhase("idle");
        }
        return;
      }

      const nextSummary: UpdateSummary = {
        version: update.version,
        currentVersion: update.currentVersion,
        notes: update.body ?? "",
        date: update.date ?? null,
      };
      const deferred = readJson<DeferredUpdate>(UPDATE_DEFERRED_KEY);
      if (!interactive && shouldDeferUpdate(deferred, nextSummary.version)) {
        await update.close();
        return;
      }

      await closeUpdateHandle();
      updateRef.current = update;
      summaryRef.current = nextSummary;
      setSummary(nextSummary);
      setError(null);
      setPhase("available");
    } catch (reason) {
      if (interactive && mountedRef.current) {
        setError(describeUpdateError(reason));
        setPhase("error");
      }
    } finally {
      checkingRef.current = false;
    }
  }, [clearLatestNoticeTimer, closeUpdateHandle]);

  useEffect(() => {
    mountedRef.current = true;
    if (!isTauri()) return () => { mountedRef.current = false; };

    void getVersion()
      .then((currentVersion) => {
        if (!mountedRef.current) return;
        const pending = readJson<UpdateSummary>(PENDING_CHANGELOG_KEY);
        let lastSeen: string | null = null;
        try {
          lastSeen = window.localStorage.getItem(LAST_SEEN_CHANGELOG_KEY);
        } catch {
          // Keep the changelog visible if storage is unavailable.
        }
        if (pending?.version === currentVersion && lastSeen !== currentVersion) {
          setChangelog(pending);
        } else if (pending && pending.version !== currentVersion) {
          removeStoredValue(PENDING_CHANGELOG_KEY);
        }
      })
      .catch(() => {
        // App metadata failures must not affect startup.
      });

    const initialTimer = window.setTimeout(() => void checkForUpdates(), AUTO_CHECK_DELAY_MS);
    const interval = window.setInterval(() => void checkForUpdates(), AUTO_CHECK_INTERVAL_MS);
    const handleFocus = () => {
      if (Date.now() - lastCheckRef.current >= AUTO_CHECK_INTERVAL_MS) void checkForUpdates();
    };
    const handleManualCheck = () => void checkForUpdates(true);
    window.addEventListener("focus", handleFocus);
    window.addEventListener(APP_UPDATE_CHECK_EVENT, handleManualCheck);

    return () => {
      mountedRef.current = false;
      window.clearTimeout(initialTimer);
      window.clearInterval(interval);
      clearLatestNoticeTimer();
      window.removeEventListener("focus", handleFocus);
      window.removeEventListener(APP_UPDATE_CHECK_EVENT, handleManualCheck);
      void closeUpdateHandle();
    };
  }, [checkForUpdates, clearLatestNoticeTimer, closeUpdateHandle]);

  const deferCurrentUpdate = useCallback(() => {
    if (summary) {
      writeJson(UPDATE_DEFERRED_KEY, { version: summary.version, until: Date.now() + DEFER_UPDATE_MS });
    }
    void closeUpdateHandle();
    summaryRef.current = null;
    setSummary(null);
    setError(null);
    setPhase("idle");
  }, [closeUpdateHandle, summary]);

  const installCurrentUpdate = useCallback(async () => {
    const update = updateRef.current;
    if (!update || !summary || busy || installingRef.current) return;
    const approved = await confirm(
      `Install Mallard ${summary.version} now?\n\nMallard will restart. Save any unfinished edits before continuing.`,
      { title: "Install update" },
    );
    if (!approved || busyRef.current) return;
    installingRef.current = true;
    setError(null);
    setDownloaded(0);
    setTotal(0);
    setPhase("downloading");
    writeJson(PENDING_CHANGELOG_KEY, summary);

    let received = 0;
    let expected = 0;
    try {
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          expected = event.data.contentLength ?? 0;
          setTotal(expected);
        } else if (event.event === "Progress") {
          received += event.data.chunkLength;
          setDownloaded(received);
        } else if (event.event === "Finished") {
          if (expected > 0) setDownloaded(expected);
          setPhase("installing");
        }
      }, { timeout: 5 * 60_000 });
      setPhase("installing");
      await relaunch();
    } catch (reason) {
      installingRef.current = false;
      await closeUpdateHandle();
      setError(describeUpdateError(reason));
      setPhase("error");
    }
  }, [busy, closeUpdateHandle, summary]);

  const retryUpdate = useCallback(() => {
    setError(null);
    setPhase("idle");
    void closeUpdateHandle().then(() => checkForUpdates(true));
  }, [checkForUpdates, closeUpdateHandle]);

  const dismissCheckNotice = useCallback(() => {
    clearLatestNoticeTimer();
    setError(null);
    setPhase("idle");
  }, [clearLatestNoticeTimer]);

  const closeChangelog = useCallback(() => {
    if (changelog) {
      try {
        window.localStorage.setItem(LAST_SEEN_CHANGELOG_KEY, changelog.version);
      } catch {
        // Closing the dialog should always work.
      }
    }
    removeStoredValue(PENDING_CHANGELOG_KEY);
    setChangelog(null);
  }, [changelog]);

  return (
    <>
      {(phase === "checking" || phase === "latest" || (!summary && phase === "error")) && (
        <UpdateCheckNotice
          phase={phase}
          error={error}
          onDismiss={dismissCheckNotice}
          onRetry={retryUpdate}
        />
      )}
      {summary && (phase === "available" || phase === "error") && (
        <UpdatePrompt
          summary={summary}
          busy={busy}
          error={error}
          onInstall={() => void installCurrentUpdate()}
          onLater={deferCurrentUpdate}
          onRetry={retryUpdate}
        />
      )}
      {summary && (phase === "downloading" || phase === "installing") && (
        <UpdateProgress
          phase={phase}
          downloaded={downloaded}
          total={total}
          version={summary.version}
        />
      )}
      {changelog && <WhatsNew summary={changelog} onClose={closeChangelog} />}
    </>
  );
}
