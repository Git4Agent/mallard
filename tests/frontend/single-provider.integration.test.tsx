import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import ProjectBindingEditor from "../../src/components/project-sync/ProjectBindingEditor";
import ProjectLinksWorkspace, {
  projectPushActionLabel,
  storageActionLockedForReview,
} from "../../src/components/project-sync/ProjectLinksWorkspace";
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

test("the project Push action describes its workflow state", () => {
  assert.equal(projectPushActionLabel({ reviewOpen: false, preparing: false, publishing: false }), "Push");
  assert.equal(projectPushActionLabel({ reviewOpen: false, preparing: true, publishing: false }), "Preparing…");
  assert.equal(projectPushActionLabel({ reviewOpen: true, preparing: false, publishing: false }), "Continue push");
  assert.equal(projectPushActionLabel({ reviewOpen: true, preparing: false, publishing: true }), "Pushing…");
});

test("an open sync review locks storage controls and the opposite sync action", () => {
  assert.equal(storageActionLockedForReview("push", "storage"), true);
  assert.equal(storageActionLockedForReview("push", "pull"), true);
  assert.equal(storageActionLockedForReview("push", "push"), false);
  assert.equal(storageActionLockedForReview("pull", "storage"), true);
  assert.equal(storageActionLockedForReview("pull", "push"), true);
  assert.equal(storageActionLockedForReview("pull", "pull"), false);
  assert.equal(storageActionLockedForReview(null, "storage"), false);
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

test("the configured profile keeps a repair action inline with the storage heading", () => {
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
      activeStorageId={storageId}
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
      onSelectStorage={() => undefined}
      onLinkStorage={() => undefined}
      onUnlinkStorage={() => undefined}
      onPush={() => undefined}
      onPull={() => undefined}
      onRepairConversationPaths={() => undefined}
      onRenameProject={() => true}
      onRefresh={() => undefined}
      onAddProject={() => undefined}
      onOpenStorageSettings={() => undefined}
      onSaveStorage={() => undefined}
    />,
  );

  const warningIndex = html.indexOf("conversation-path-repair-notice");
  const storageHeadingIndex = html.indexOf("project-profile-storage-heading");
  const storageActionsIndex = html.indexOf("project-profile-storage-actions", storageHeadingIndex);
  const addStorageIndex = html.indexOf(">Add storage<", storageHeadingIndex);
  const storageIndex = html.indexOf("storage-link-block");
  const activityIndex = html.indexOf(">Activity<");

  assert.ok(warningIndex >= 0);
  assert.ok(warningIndex > storageHeadingIndex);
  assert.ok(storageActionsIndex > warningIndex);
  assert.ok(addStorageIndex > storageActionsIndex);
  assert.ok(addStorageIndex < storageIndex);
  assert.ok(storageIndex > warningIndex);
  assert.ok(activityIndex > storageIndex);
  assert.doesNotMatch(html, /Show project settings|Hide project settings|Close project settings/);
  assert.equal(html.match(/conversation-path-repair-notice/g)?.length, 1);
  assert.equal(html.match(/class="storage-link-block/g)?.length, 1);
  assert.equal(html.match(/class="storage-link-unlink"/g)?.length, 1);
  assert.doesNotMatch(html, /type="radio"/);
  assert.match(html, /aria-label="Active storage: Local storage\. Choose another storage"/);
  assert.match(html, /role="menu" aria-label="Choose active storage" hidden=""/);
  assert.equal(html.match(/class="storage-link-menu-option/g)?.length, 2);
  assert.equal(html.match(/role="menuitemradio"/g)?.length, 2);
  assert.match(html, /role="menuitemradio" aria-checked="true" class="storage-link-menu-option selected"/);
  assert.match(html, /storage-link-menu-copy"><strong>Local storage<\/strong><span[^>]*>~\/storage<\/span>/);
  assert.match(html, /storage-link-menu-copy"><strong>R2 storage<\/strong><span[^>]*>agent<\/span>/);
  assert.match(html, /aria-label="Unlink Local storage from Project one"/);
  assert.doesNotMatch(html, /aria-label="Unlink R2 storage from Project one"/);
  assert.equal(html.match(/class="storage-link-profile-section/g)?.length ?? 0, 0);
  assert.match(html, /aria-label="2 linked storage locations"/);
  assert.match(html, /project-profile-storage-icon/);
  assert.match(html, /project-profile-storage-actions/);
  assert.doesNotMatch(html, /Linked storage/);
  assert.doesNotMatch(html, /project-profile-group-footer/);
  assert.equal(html.match(/Default Codex/g)?.length, 1);
  assert.match(html, /title="Codex agent home: \/Users\/test\/\.codex"/);
  assert.match(html, /v3-project-heading-agent/);
  assert.doesNotMatch(html, /Codex ·/);
  assert.doesNotMatch(html, /project-profile-group-header|project-profile-group-icon|project-profile-group-path/);
  assert.doesNotMatch(html, /Agent home is fixed after project setup/);
  assert.doesNotMatch(html, /Configure project profile/);
  assert.doesNotMatch(html, />CLAUDE</);
  assert.doesNotMatch(html, /Not used/);
});
