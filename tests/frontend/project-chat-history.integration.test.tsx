import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import ProjectChatHistoryPage, {
  ProjectChatHistoryContent,
} from "../../src/components/project-sync/ProjectChatHistoryPage";
import ProjectSidebar from "../../src/components/project-sync/ProjectSidebar";

const project = {
  local_project_id: "project-mallard",
  bundle_id: "bundle-mallard",
  display_name: "mallard",
  local_alias: "Mallard local",
  repository_fingerprint: "fingerprint",
  project_root: "/Users/test/projects/mallard",
  canonical_project_root: "/Users/test/projects/mallard",
  profile_ids: { codex: "profile-codex" },
  profile_names: ["Default Codex"],
  created_at: 1,
  updated_at: 2,
  revision: 1,
};

const history = {
  project_id: project.local_project_id,
  threads: [{
    thread_id: "019f7798-5437-7632-9dbc-5b589cf68bf0",
    title: "Add Git history mapping",
    summary: "Map project-owned Codex sessions onto the first-parent rail.",
    started_at: 1_752_800_000,
    ended_at: 1_752_803_600,
    branch: "main",
    recorded_sha: "a".repeat(40),
    user_round_count: 3,
    agent_message_count: 5,
    tool_call_count: 8,
    total_tokens: 24800,
    metrics_complete: true,
    commit_occurrence_count: 2,
  }],
  git: {
    selected_branch: "main",
    branches: [{ name: "main", is_current: true, available: true }],
    commits: [{
      sha: "b".repeat(40),
      short_sha: "bbbbbbb",
      committed_at: 1_752_802_000,
      subject: "Add project-scoped history",
      thread_refs: [{ thread_id: "019f7798-5437-7632-9dbc-5b589cf68bf0" }],
    }],
    unique_thread_count: 1,
    reference_count: 1,
    next_cursor: null,
  },
  unmapped: [],
  warnings: [],
  window_start: 1_750_000_000,
  window_end: 1_753_000_000,
  next_before: 1_750_000_000,
  codex_home: "/Users/test/config/mallard/.codex",
  storage_sync: [{
    storage_id: "storage-1",
    storage_name: "Local storage 1",
    last_pull_at: 1_752_700_000,
    last_push_at: 1_752_800_000,
  }],
};

test("history content uses the local alias and renders commit/thread actions", () => {
  const html = renderToStaticMarkup(
    <ProjectChatHistoryContent
      project={project}
      binding={{
        replica_id: "replica",
        local_project_id: project.local_project_id,
        bundle_id: project.bundle_id,
        project_root: project.project_root,
        canonical_project_root: project.project_root,
        profile_ids: { codex: "profile-codex" },
        state: "active",
        revision: 1,
        updated_at: 2,
      }}
      history={history}
      loading={false}
      loadingMore={false}
      actionError={null}
      actionBusyThreadId={null}
      onBranchChange={() => undefined}
      onRefresh={() => undefined}
      onLoadMore={() => undefined}
      onOpenSettings={() => undefined}
      onOpenCodex={() => undefined}
      onOpenTerminal={() => undefined}
      onToggleDetails={() => undefined}
    />,
  );
  assert.doesNotMatch(html, /Project Name/);
  assert.match(html, /Mallard local/);
  assert.match(html, /v3-history-project-context/);
  assert.match(html, /title="Codex configuration:/);
  assert.match(html, /\/Users\/test\/config\/mallard\/\.codex/);
  assert.match(html, /Local storage 1/);
  assert.match(html, /aria-label="Last pull:/);
  assert.match(html, /aria-label="Last push:/);
  assert.match(html, /Repository: mallard/);
  assert.match(html, /Add project-scoped history/);
  assert.match(html, /aria-label="Show session details"/);
  assert.doesNotMatch(html, /aria-label="User rounds: 3"/);
  assert.doesNotMatch(html, /24\.8K/);
  assert.doesNotMatch(html, /Appears under 2 commits/);
  assert.doesNotMatch(html, /during session|after session|started from/);
  assert.doesNotMatch(html, /Map project-owned Codex sessions onto/);
  assert.match(html, /aria-label="Show conversation details"/);
  assert.match(html, /aria-label="Open in Codex"/);
  assert.match(html, /aria-label="Open in Terminal"/);
  assert.match(html, /v3-openai-icon/);
  assert.match(html, />Open in Codex</);
  assert.match(html, /> Open in Terminal</);
  assert.doesNotMatch(html, /Show chat details/);
});

test("non-Git history renders a flat Codex thread list", () => {
  const html = renderToStaticMarkup(
    <ProjectChatHistoryContent
      project={{ ...project, local_alias: null }}
      binding={{
        replica_id: "replica",
        local_project_id: project.local_project_id,
        bundle_id: project.bundle_id,
        project_root: project.project_root,
        canonical_project_root: project.project_root,
        profile_ids: { codex: "profile-codex" },
        state: "active",
        revision: 1,
        updated_at: 2,
      }}
      history={{ ...history, git: null }}
      loading={false}
      loadingMore={false}
      actionError={null}
      actionBusyThreadId={null}
      onBranchChange={() => undefined}
      onRefresh={() => undefined}
      onLoadMore={() => undefined}
      onOpenSettings={() => undefined}
      onOpenCodex={() => undefined}
      onOpenTerminal={() => undefined}
    />,
  );
  assert.match(html, /Codex threads/);
  assert.doesNotMatch(html, /Branch/);
});

test("an invalid persisted Codex profile offers Project Settings recovery", () => {
  const html = renderToStaticMarkup(
    <ProjectChatHistoryContent
      project={project}
      binding={{
        replica_id: "replica",
        local_project_id: project.local_project_id,
        bundle_id: project.bundle_id,
        project_root: project.project_root,
        canonical_project_root: project.project_root,
        profile_ids: { codex: "profile-codex" },
        state: "active",
        revision: 1,
        updated_at: 2,
      }}
      history={null}
      loading={false}
      loadingMore={false}
      actionError="Codex profile path changed; open Project Settings"
      actionBusyThreadId={null}
      onBranchChange={() => undefined}
      onRefresh={() => undefined}
      onLoadMore={() => undefined}
      onOpenSettings={() => undefined}
      onOpenCodex={() => undefined}
      onOpenTerminal={() => undefined}
    />,
  );
  assert.match(html, /role="alert"/);
  assert.match(html, /Open Project Settings/);
});

test("completed projects use their main row for history and only mark Git repositories", () => {
  const html = renderToStaticMarkup(
    <ProjectSidebar
      projects={[
        { ...project, is_git_repository: true },
        {
          ...project,
          local_project_id: "project-folder",
          local_alias: "Plain folder",
          profile_names: ["myconf3 · Codex"],
          is_git_repository: false,
        },
      ]}
      drafts={[{
        draft_id: "draft-1",
        display_name: "draft repo",
        project_root: "/tmp/draft",
        updated_at: 1,
        revision: 1,
        status: "draft",
      }]}
      activeDraftId={null}
      storages={[]}
      storageUsage={{}}
      activeProjectId={project.local_project_id}
      loading={false}
      busy={false}
      activityOpen={false}
      unreadLogs={0}
      onSelectProject={() => undefined}
      onConfigureProject={() => undefined}
      onRemoveProject={() => undefined}
      onSelectDraft={() => undefined}
      onDiscardDraft={() => undefined}
      onToggleActivity={() => undefined}
      onAddProject={() => undefined}
      onRefresh={() => undefined}
      onOpenStorage={() => undefined}
      onRemoveStorage={() => undefined}
      onAddStorage={() => undefined}
      onOpenLegacy={() => undefined}
    />,
  );
  assert.match(html, /src="\/mallard-logo\.svg"/);
  assert.doesNotMatch(html, /mallard-logo\.png/);
  assert.doesNotMatch(html, /aria-label="View history for Mallard local"/);
  assert.equal(html.match(/v3-repository-kind/g)?.length, 1);
  assert.match(html, /title="Git repository"/);
  assert.match(html, /git<\/span>/);
  assert.doesNotMatch(html, /Default Codex|myconf3 · Codex/);
  assert.match(html, /role="separator" aria-label="Resize Projects and Storage sections" aria-orientation="horizontal"/);
  assert.match(html, /aria-valuenow="56"/);
  assert.doesNotMatch(html, /Git Based|Non-Git Based/);
  assert.match(html, /Project settings for Mallard local/);
  assert.doesNotMatch(html, /View history for draft repo/);
});

void ProjectChatHistoryPage;
