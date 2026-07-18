import type {
  BundleReadiness,
  BundleSnapshotSummary,
  DependencyPlan,
  ProjectBinding,
  RestorePlan,
} from "../../types";
import { errorMessage } from "./model";

export interface PullReviewApi {
  fetchBundle: (storageId: string, bundleId: string) => Promise<BundleSnapshotSummary>;
  planRestore: (storageId: string, bundleId: string, binding: ProjectBinding) => Promise<RestorePlan>;
  planDependencies: (bundleId: string, binding: ProjectBinding) => Promise<DependencyPlan>;
  getReadiness: (bundleId: string, binding: ProjectBinding) => Promise<BundleReadiness>;
}

export interface PullReviewRequest {
  storageId: string;
  bundleId: string;
  binding: ProjectBinding;
}

export interface PullReviewSupport {
  dependencyPlan: DependencyPlan | null;
  readiness: BundleReadiness | null;
  errors: string[];
}

export interface StartedPullReview {
  restorePlan: RestorePlan;
  support: Promise<PullReviewSupport>;
}

/**
 * Start the complete review flow without awaiting non-blocking support. The
 * caller can render `restorePlan` immediately and observe `support` later.
 */
export async function beginPullReview(
  api: PullReviewApi,
  request: PullReviewRequest,
): Promise<StartedPullReview> {
  const restorePlan = await preparePullReview(api, request);
  return {
    restorePlan,
    support: loadPullReviewSupport(api, request),
  };
}

/**
 * Load only the mutation review. This is deliberately separate from optional
 * supporting checks so a dependency/readiness error cannot hide a valid plan
 * or its Apply button.
 */
export async function preparePullReview(
  api: PullReviewApi,
  request: PullReviewRequest,
): Promise<RestorePlan> {
  await api.fetchBundle(request.storageId, request.bundleId);
  return api.planRestore(request.storageId, request.bundleId, request.binding);
}

/** Load non-blocking context shown alongside an already-visible review. */
export async function loadPullReviewSupport(
  api: PullReviewApi,
  request: Pick<PullReviewRequest, "bundleId" | "binding">,
): Promise<PullReviewSupport> {
  const [dependencies, readiness] = await Promise.allSettled([
    api.planDependencies(request.bundleId, request.binding),
    api.getReadiness(request.bundleId, request.binding),
  ]);
  const errors: string[] = [];
  if (dependencies.status === "rejected") {
    errors.push(`Dependency checks: ${errorMessage(dependencies.reason)}`);
  }
  if (readiness.status === "rejected") {
    errors.push(`Readiness checks: ${errorMessage(readiness.reason)}`);
  }
  return {
    dependencyPlan: dependencies.status === "fulfilled" ? dependencies.value : null,
    readiness: readiness.status === "fulfilled" ? readiness.value : null,
    errors,
  };
}
