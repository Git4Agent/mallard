import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import ProjectBindingEditor from "../../src/components/project-sync/ProjectBindingEditor";
import ProjectLinksWorkspace from "../../src/components/project-sync/ProjectLinksWorkspace";
import {
  configuredProjectProvider,
  singleProviderSelection,
} from "../../src/components/project-sync/model";
import type {
  CodexConversationPathAudit,
  ProjectBinding,
  ProviderProfileSummary,
} from "../../src/types";

const codexProfile: ProviderProfileSummary = {
  profile_id: "profile-codex",
  provider: "codex",
  display_name: "Default Codex",
  path: "/Users/test/.codex",
  canonical_path: "/Users/test/.codex",
  revision: 1,
  created_at: 1,
  updated_at: 1,
  available: true,
  readable: true,
  writable: true,
  used_by_projects: [],
};

const claudeProfile: ProviderProfileSummary = {
  ...codexProfile,
  profile_id: "profile-claude",
  provider: "claude",
  display_name: "Default Claude",
  path: "/Users/test/.claude",
  canonical_path: "/Users/test/.claude",
};

test("provider selection keeps exactly one project profile", () => {
  assert.equal(configuredProjectProvider({ claude: "profile-claude" }), "claude");
  assert.deepEqual(
    singleProviderSelection(
      { codex: "profile-codex", claude: "profile-claude" },
      "claude",
    ),
    { claude: "profile-claude" },
  );
});

test("binding setup renders one profile editor behind an explicit agent choice", () => {
  const html = renderToStaticMarkup(
    <ProjectBindingEditor
      title="Project setup"
      description="Choose one agent."
      binding={{
        bundle_id: "bundle-one",
        project_root: "/Users/test/project",
        profile_ids: {
          codex: codexProfile.profile_id,
          claude: claudeProfile.profile_id,
        },
      }}
      busy={false}
      actionLabel="Save project setup"
      profiles={[codexProfile, claudeProfile]}
      onAddProfile={async () => null}
      onCancel={() => undefined}
      onSubmit={() => undefined}
    />,
  );

  assert.match(html, /role="radiogroup" aria-label="Agent used by this project"/);
  assert.equal(html.match(/<select/g)?.length, 1);
  assert.match(html, /aria-label="Codex profile"/);
  assert.doesNotMatch(html, /aria-label="Claude profile"/);
});

test("the configured profile groups its warning above simple storage rows", () => {
  const projectId = "project-one";
  const storageId = "storage-one";
  const secondStorageId = "storage-two";
  const binding: ProjectBinding = {
    replica_id: "replica-one",
    local_project_id: projectId,
    bundle_id: "bundle-one",
    project_root: "/Users/test/project",
    canonical_project_root: "/Users/test/project",
    profile_ids: { codex: codexProfile.profile_id },
    state: "active",
    revision: 1,
    updated_at: 1,
  };
  const audit: CodexConversationPathAudit = {
    local_project_id: projectId,
    profile_id: codexProfile.profile_id,
    profile_path: codexProfile.path,
    project_root: binding.project_root,
    assigned_thread_count: 1,
    matching_thread_count: 0,
    issues: [{
      thread_id: "thread-one",
      transcript_path: "/Users/test/.codex/sessions/rollout.jsonl",
      recorded_cwd: "/Users/test/old-project",
      target_cwd: binding.project_root,
    }],
    blockers: [],
    warnings: [],
    ready: false,
    can_repair: true,
  };
  const html = renderToStaticMarkup(
    <ProjectLinksWorkspace
      projects={[{
        local_project_id: projectId,
        bundle_id: "bundle-one",
        display_name: "Project one",
        revision: 1,
        project_root: binding.project_root,
      }]}
      activeProjectId={projectId}
      bindings={[binding]}
      profiles={[codexProfile, claudeProfile]}
      storages={[
        {
          id: storageId,
          name: "Local storage",
          kind: "local",
          bucket: "",
          access_key_id: "",
          secret_access_key: "",
          account_id: "",
          s3_endpoint: "",
          region: "",
          local_dir: "/Users/test/storage",
          included_default_exclusions: [],
        },
        {
          id: secondStorageId,
          name: "R2 storage",
          kind: "s3",
          bucket: "agent",
          access_key_id: "key",
          secret_access_key: "secret",
          account_id: "account",
          s3_endpoint: "https://account.r2.cloudflarestorage.com",
          region: "auto",
          local_dir: "",
          included_default_exclusions: [],
        },
      ]}
      links={[
        {
          local_project_id: projectId,
          storage_id: storageId,
          bundle_id: "bundle-one",
          pinned: true,
          created_at: 1,
        },
        {
          local_project_id: projectId,
          storage_id: secondStorageId,
          bundle_id: "bundle-one",
          pinned: true,
          created_at: 2,
        },
      ]}
      loading={false}
      busy={false}
      error={null}
      conversationPathAudits={{ [projectId]: audit }}
      conversationPathAuditErrors={{}}
      conversationPathAuditLoading={false}
      onSelectProject={() => undefined}
      onLinkStorage={() => undefined}
      onUnlinkStorage={() => undefined}
      onPush={() => undefined}
      onPull={() => undefined}
      onRepairConversationPaths={() => undefined}
      onRenameProject={() => true}
      onAssignProfile={() => undefined}
      onAddProfilePath={() => undefined}
      onRefresh={() => undefined}
      onAddProject={() => undefined}
      onOpenStorageSettings={() => undefined}
      onSaveStorage={() => undefined}
      inlineStorageReview={{
        kind: "push",
        projectId,
        storageId,
        content: <span>Push review</span>,
        onClose: () => undefined,
      }}
    />,
  );

  const profileIndex = html.indexOf("project-profile-group-header");
  const warningIndex = html.indexOf("conversation-path-repair-notice");
  const storageIndex = html.indexOf("storage-link-block");

  assert.ok(profileIndex >= 0);
  assert.ok(warningIndex > profileIndex);
  assert.ok(storageIndex > warningIndex);
  assert.equal(html.match(/conversation-path-repair-notice/g)?.length, 1);
  assert.equal(html.match(/class="storage-link-block/g)?.length, 2);
  assert.equal(html.match(/class="storage-link-unlink"/g)?.length, 2);
  assert.match(html, /aria-label="Unlink Local storage from Project one"/);
  assert.match(html, /aria-label="Unlink R2 storage from Project one"/);
  assert.equal(html.match(/class="storage-link-profile-section/g)?.length ?? 0, 0);
  assert.equal(html.match(/Default Codex/g)?.length, 1);
  assert.match(html, /Codex · ~\/\.codex/);
  assert.match(html, /project-profile-group-icon/);
  assert.doesNotMatch(html, />CLAUDE</);
  assert.doesNotMatch(html, /Not used/);
});
