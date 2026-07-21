import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import {
  describeUpdateError,
  downloadPercentage,
  releaseNotePreview,
  shouldDeferUpdate,
  UpdateCheckNotice,
  UpdateProgress,
  UpdatePrompt,
  type UpdateSummary,
} from "../../src/components/AppUpdater";
import {
  APP_UPDATE_CHECK_EVENT,
  requestAppUpdateCheck,
} from "../../src/components/AppUpdateControl";

const summary: UpdateSummary = {
  version: "0.2.0",
  currentVersion: "0.1.0",
  notes: "## Improvements\n\n- Safer project restores",
  date: "2026-07-20T12:00:00Z",
};

test("an available update waits until active sync work finishes", () => {
  const html = renderToStaticMarkup(
    <UpdatePrompt
      summary={summary}
      busy
      error={null}
      onInstall={() => undefined}
      onLater={() => undefined}
      onRetry={() => undefined}
    />,
  );

  assert.match(html, /Mallard 0\.2\.0 is available/);
  assert.match(html, /Finish the current operation before restarting/);
  assert.match(html, /<button[^>]*disabled=""[^>]*>Update and restart<\/button>/);
});

test("an interrupted update offers a retry without hiding the failure", () => {
  const html = renderToStaticMarkup(
    <UpdatePrompt
      summary={summary}
      busy={false}
      error="Signature verification failed"
      onInstall={() => undefined}
      onLater={() => undefined}
      onRetry={() => undefined}
    />,
  );

  assert.match(html, /Update interrupted/);
  assert.match(html, /Signature verification failed/);
  assert.match(html, />Try again<\/button>/);
  assert.doesNotMatch(html, /Update and restart/);
});

test("download progress is bounded and rendered accessibly", () => {
  assert.equal(downloadPercentage(25, 100), 25);
  assert.equal(downloadPercentage(150, 100), 100);
  assert.equal(downloadPercentage(10, 0), null);

  const html = renderToStaticMarkup(
    <UpdateProgress phase="downloading" downloaded={25} total={100} version="0.2.0" />,
  );
  assert.match(html, /aria-valuenow="25"/);
  assert.match(html, /25% downloaded/);
});

test("deferral applies only to the matching version and expiry window", () => {
  assert.equal(shouldDeferUpdate({ version: "0.2.0", until: 2_000 }, "0.2.0", 1_000), true);
  assert.equal(shouldDeferUpdate({ version: "0.2.0", until: 2_000 }, "0.3.0", 1_000), false);
  assert.equal(shouldDeferUpdate({ version: "0.2.0", until: 2_000 }, "0.2.0", 2_000), false);
  assert.equal(releaseNotePreview(summary.notes), "Improvements");
});

test("manual checks report checking, current, and retryable failure states", () => {
  const checking = renderToStaticMarkup(
    <UpdateCheckNotice
      phase="checking"
      error={null}
      onDismiss={() => undefined}
      onRetry={() => undefined}
    />,
  );
  const latest = renderToStaticMarkup(
    <UpdateCheckNotice
      phase="latest"
      error={null}
      onDismiss={() => undefined}
      onRetry={() => undefined}
    />,
  );
  const failed = renderToStaticMarkup(
    <UpdateCheckNotice
      phase="error"
      error="GitHub is unavailable"
      onDismiss={() => undefined}
      onRetry={() => undefined}
    />,
  );

  assert.match(checking, /Checking for updates/);
  assert.match(latest, /Mallard is up to date/);
  assert.match(failed, /GitHub is unavailable/);
  assert.match(failed, />Try again<\/button>/);
  assert.equal(describeUpdateError(new Error("Signature verification failed")), "Signature verification failed");
});

test("the sidebar update control dispatches an interactive update check", () => {
  const target = new EventTarget();
  let checks = 0;
  target.addEventListener(APP_UPDATE_CHECK_EVENT, () => { checks += 1; });
  requestAppUpdateCheck(target);
  assert.equal(checks, 1);
});
