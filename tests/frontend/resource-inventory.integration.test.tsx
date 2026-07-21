import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import PushResourceWorkspace, {
  nextPushReviewStep,
} from "../../src/components/project-sync/PushResourceWorkspace";
import ResourceInventory from "../../src/components/project-sync/ResourceInventory";
import { recipeWithSelection } from "../../src/components/project-sync/model";
import type { ProjectResourceDescriptor } from "../../src/types";

function standaloneSkill(
  effectiveName: string,
  installDirectory: string,
  blockedReason: string | null = null,
): ProjectResourceDescriptor {
  return {
    resource_id: `codex:standalone-skill:${effectiveName}`,
    kind: "standalone_skill",
    provider: "codex",
    scope: "provider_state",
    display_name: effectiveName,
    provenance: {
      kind: "standalone_snapshot",
      stable_key: `custom-skill:v1:codex:${effectiveName}`,
    },
    apply_policy: blockedReason ? "explicit_review" : "safe_file",
    codec_version: 1,
    metadata: {
      effective_name: effectiveName,
      install_dir_name: installDirectory,
      provider_adapter_version: "2",
    },
    category: "skills",
    logical_paths: [`state/codex/skills/${installDirectory}/SKILL.md`],
    default_selected: false,
    blocked_reason: blockedReason,
    install_behavior: "install on restore",
  };
}

function inputTagFor(html: string, accessibleName: string): string {
  const marker = `aria-label="${accessibleName}"`;
  const markerIndex = html.indexOf(marker);
  assert.notEqual(markerIndex, -1, `missing input '${accessibleName}'`);
  const start = html.lastIndexOf("<input", markerIndex);
  const end = html.indexOf(">", markerIndex);
  assert.notEqual(start, -1);
  assert.notEqual(end, -1);
  return html.slice(start, end + 1);
}

test("a declared skill name may differ from its selectable install folder", () => {
  const resource = standaloneSkill(
    "get-real-hardware-rh-service",
    "capture-lsservice-detail",
  );
  const html = renderToStaticMarkup(
    <ResourceInventory
      resources={[resource]}
      selected={new Set()}
      statuses={new Map()}
      onToggle={() => undefined}
    />,
  );

  assert.match(html, /get-real-hardware-rh-service/);
  assert.match(html, /folder capture-lsservice-detail/);
  assert.doesNotMatch(html, /contradicts directory/);
  assert.doesNotMatch(
    inputTagFor(html, "Include get-real-hardware-rh-service"),
    /\bdisabled\b/,
    "a valid declared-name/folder mismatch must stay selectable",
  );
});

test("duplicate effective-name claims remain unselectable", () => {
  const reason = "effective skill name 'review' is declared by multiple directories";
  const html = renderToStaticMarkup(
    <ResourceInventory
      resources={[
        standaloneSkill("review", "review-one", reason),
        {
          ...standaloneSkill("review", "review-two", reason),
          resource_id: "codex:standalone-skill:review-two-blocked",
        },
      ]}
      selected={new Set()}
      statuses={new Map()}
      onToggle={() => undefined}
    />,
  );

  assert.match(html, /folder review-one/);
  assert.match(html, /folder review-two/);
  assert.equal((html.match(/v3-resource-row blocked/g) ?? []).length, 2);
  const disabledInputs = html.match(/<input[^>]*disabled=""[^>]*>/g) ?? [];
  assert.equal(disabledInputs.length, 2);
});

test("blocked resources cannot enter a published recipe", () => {
  const allowed = standaloneSkill("frontend-skill", "frontend-skill");
  const blocked = standaloneSkill("broken-skill", "broken-skill", "SKILL.md is unreadable");
  const recipe = recipeWithSelection(
    { schema_version: 1, revision: 3, entries: {} },
    [allowed, blocked],
    new Set([allowed.resource_id, blocked.resource_id]),
  );

  assert.deepEqual(Object.keys(recipe.entries), [allowed.resource_id]);
});

test("the push chooser shows one concise selection summary", () => {
  const resource = standaloneSkill("review", "review");
  const html = renderToStaticMarkup(
    <PushResourceWorkspace
      resources={[resource]}
      selected={new Set([resource.resource_id])}
      projectDefaults={new Set([resource.resource_id])}
      busy={false}
      error={null}
      initialStep="review"
      onToggle={() => undefined}
      onUseProjectDefaults={() => undefined}
      onClear={() => undefined}
      onClose={() => undefined}
      onPush={() => undefined}
    />,
  );

  assert.match(html, />Push review</);
  assert.match(html, /v3-sync-review-title/);
  assert.match(html, /Push review<\/h2><span class="v3-sync-review-hint">Choose what to include<\/span>/);
  assert.match(html, />Recommended \(1\)</);
  assert.match(html, />Review</);
  assert.match(html, /Back: Plugins/);
  assert.match(html, />Push 1 resource</);
  assert.doesNotMatch(html, /Choose resources to push/);
  assert.doesNotMatch(html, /last selection|selection will be remembered|resource selected/i);
});

test("the push chooser provides contextual back and next actions", () => {
  const resource = standaloneSkill("review", "review");
  const html = renderToStaticMarkup(
    <PushResourceWorkspace
      resources={[resource]}
      selected={new Set([resource.resource_id])}
      projectDefaults={new Set([resource.resource_id])}
      busy={false}
      error={null}
      initialStep="skills"
      onToggle={() => undefined}
      onUseProjectDefaults={() => undefined}
      onClear={() => undefined}
      onClose={() => undefined}
      onPush={() => undefined}
    />,
  );

  assert.match(html, /Back: Git &amp; sessions/);
  assert.match(html, /Next: Plugins/);
  assert.equal(nextPushReviewStep("history"), "skills");
  assert.equal(nextPushReviewStep("skills"), "plugins");
  assert.equal(nextPushReviewStep("plugins"), "review");
  assert.equal(nextPushReviewStep("review"), "review");
});
