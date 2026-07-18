import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import type {
  BundleReadiness,
  BundleSnapshotSummary,
  DependencyPlan,
  ProjectBinding,
  RestorePlan,
} from "../../src/types";
import RestorePlanView from "../../src/components/project-sync/RestorePlanView";
import {
  beginPullReview,
  type PullReviewApi,
} from "../../src/components/project-sync/pullReviewFlow";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((nextResolve, nextReject) => {
    resolve = nextResolve;
    reject = nextReject;
  });
  return { promise, resolve, reject };
}

const binding: ProjectBinding = {
  replica_id: "replica-gam2",
  local_project_id: "project-gam2",
  bundle_id: "df29babc833808e68ad0efa4d01d7d6d",
  project_root: "/Users/test/project/gam2",
  canonical_project_root: "/Users/test/project/gam2",
  profile_ids: { codex: "profile-codex" },
  state: "active",
  revision: 0,
  updated_at: 1,
};

const restorePlan: RestorePlan = {
  schema_version: 1,
  plan_id: "plan-restore",
  storage_id: "storage-local",
  bundle_id: binding.bundle_id,
  replica_id: binding.replica_id,
  generation: 1,
  commit_id: "commit-restore",
  manifest_sha256: "a".repeat(64),
  binding_revision: binding.revision,
  created_at: 1,
  expires_at: 2,
  actions: [{
    action_id: "action-session",
    resource_id: "codex:session:thread-a",
    kind: {
      kind: "materialize_conversation",
      provider: "codex",
      logical_path: "state/codex/sessions/2026/07/18/thread-a.jsonl",
    },
    target_path: "/Users/test/.codex/sessions/2026/07/18/thread-a.jsonl",
    source_sha256: "b".repeat(64),
    expected_target_sha256: null,
    requires_explicit_approval: false,
  }],
};

const snapshot: BundleSnapshotSummary = {
  storage_id: "storage-local",
  bundle_id: binding.bundle_id,
  display_name: "healthGame",
  fetched_at: 1,
};

test("a valid Pull plan renders an enabled Apply button while supporting checks fail", async () => {
  const dependencies = deferred<DependencyPlan>();
  const readiness = deferred<BundleReadiness>();
  const calls: string[] = [];
  const api: PullReviewApi = {
    fetchBundle: async () => {
      calls.push("fetch");
      return snapshot;
    },
    planRestore: async () => {
      calls.push("plan-restore");
      return restorePlan;
    },
    planDependencies: () => {
      calls.push("plan-dependencies");
      return dependencies.promise;
    },
    getReadiness: () => {
      calls.push("readiness");
      return readiness.promise;
    },
  };

  const review = await beginPullReview(api, {
    storageId: "storage-local",
    bundleId: binding.bundle_id,
    binding,
  });
  assert.equal(review.restorePlan, restorePlan);
  assert.deepEqual(calls, ["fetch", "plan-restore", "plan-dependencies", "readiness"]);
  let supportFinished = false;
  void review.support.then(() => {
    supportFinished = true;
  });
  await Promise.resolve();
  assert.equal(supportFinished, false, "supporting checks remain non-blocking context");

  const html = renderToStaticMarkup(
    <RestorePlanView
      projectName="gam2"
      plan={review.restorePlan}
      binding={binding}
      dependencyPlan={null}
      readiness={null}
      restoreResult={null}
      dependencyResult={null}
      busy={false}
      error={null}
      onApplyRestore={() => undefined}
      onApplyDependencies={() => undefined}
      onRefresh={() => undefined}
      onClose={() => undefined}
    />,
  );
  assert.match(html, /Review only — no project files have changed/);
  assert.match(html, /v3-restore-apply-footer/);
  const applyText = html.lastIndexOf("Apply approved changes");
  assert.notEqual(applyText, -1);
  const applyTagStart = html.lastIndexOf("<button", applyText);
  const applyTagEnd = html.indexOf(">", applyTagStart);
  assert.doesNotMatch(
    html.slice(applyTagStart, applyTagEnd),
    /disabled/,
    "the default-approved conversation keeps Apply enabled",
  );

  dependencies.reject(new Error("invalid generated id prefix"));
  readiness.reject(new Error("readiness unavailable"));
  const support = await review.support;
  assert.equal(support.dependencyPlan, null);
  assert.equal(support.readiness, null);
  assert.deepEqual(support.errors, [
    "Dependency checks: invalid generated id prefix",
    "Readiness checks: readiness unavailable",
  ]);
});

test("a primary restore-plan failure still rejects the Pull review", async () => {
  const api: PullReviewApi = {
    fetchBundle: async () => snapshot,
    planRestore: async () => {
      throw new Error("bundle head changed");
    },
    planDependencies: async () => {
      throw new Error("must not run");
    },
    getReadiness: async () => {
      throw new Error("must not run");
    },
  };
  await assert.rejects(
    beginPullReview(api, {
      storageId: "storage-local",
      bundleId: binding.bundle_id,
      binding,
    }),
    /bundle head changed/,
  );
});
