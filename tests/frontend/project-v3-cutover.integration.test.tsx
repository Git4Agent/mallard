import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import test from "node:test";

test("ProjectSyncV3 is the only application interface", () => {
  const app = readFileSync("src/App.tsx", "utf8");
  assert.match(app, /<ProjectSyncV3/);
  assert.doesNotMatch(app, /LegacyApp|onOpenLegacy|mode.*legacy|SyncPanel|FilesWorkspace|FinishSetup/);

  const sidebar = readFileSync("src/components/project-sync/ProjectSidebar.tsx", "utf8");
  const workspace = readFileSync("src/components/project-sync/ProjectSyncV3.tsx", "utf8");
  assert.doesNotMatch(`${sidebar}\n${workspace}`, /Legacy profiles|Open legacy|onOpenLegacy/);

  for (const removed of [
    "src/components/SyncPanel.tsx",
    "src/components/FilesWorkspace.tsx",
    "src/components/FileTree.tsx",
    "src/components/FilePreview.tsx",
    "src/components/FinishSetup.tsx",
  ]) {
    assert.equal(existsSync(removed), false, `${removed} must remain removed`);
  }
});

test("the production Tauri handler exposes only V3 sync and shared log commands", () => {
  const backend = readFileSync("src-tauri/src/lib.rs", "utf8");
  assert.match(backend, /project_sync_v3::commands::get_project_sync_config/);
  assert.match(backend, /activity_log::query_activity_logs/);
  assert.doesNotMatch(
    backend,
    /list_config_dirs|read_file_content|write_file_content|get_sync_config,|save_sync_config,|sync_upload|sync_download|get_setup_readiness|setup_link/,
  );

  const persistence = readFileSync("src-tauri/src/project_sync_v3/persistence.rs", "utf8");
  assert.match(persistence, /schema-2 configuration, baselines,[\s\S]*are neither read nor overwritten/);
});
