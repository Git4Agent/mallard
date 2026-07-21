import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const projectUrl = new URL("../", import.meta.url);
const read = (path) => readFile(new URL(path, projectUrl), "utf8");

test("desktop releases and updates use the public GitHub repository", async () => {
  const [tauriConfigSource, workflow, releaseGuide] = await Promise.all([
    read("src-tauri/tauri.conf.json"),
    read(".github/workflows/release.yml"),
    read("docs/RELEASING.md"),
  ]);
  const tauriConfig = JSON.parse(tauriConfigSource);

  assert.deepEqual(tauriConfig.plugins.updater.endpoints, [
    "https://github.com/Git4Agent/mallard/releases/latest/download/latest.json",
  ]);
  assert.equal(tauriConfig.bundle.createUpdaterArtifacts, true);

  for (const target of [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
  ]) assert.match(workflow, new RegExp(target));
  assert.match(workflow, /tauri-apps\/tauri-action@v1/);
  assert.match(workflow, /releaseDraft: true/);
  assert.match(workflow, /uploadUpdaterJson: true/);
  assert.doesNotMatch(workflow, /Cloudflare|CLOUDFLARE|\bR2\b|r2 object|api\.mallard-ai\.com/);

  assert.match(releaseGuide, /GitHub Releases is the public source of truth/);
  assert.match(releaseGuide, /releases\/latest\/download\/latest\.json/);
  assert.doesNotMatch(releaseGuide, /CLOUDFLARE_API_TOKEN|CLOUDFLARE_ACCOUNT_ID/);
});
