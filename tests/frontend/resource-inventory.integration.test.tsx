import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import ResourceInventory from "../../src/components/project-sync/ResourceInventory";
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
