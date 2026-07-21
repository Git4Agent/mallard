import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const projectUrl = new URL("../", import.meta.url);
const read = (path) => readFile(new URL(path, projectUrl), "utf8");

test("desktop releases and updates use the public GitHub repository", async () => {
  const [tauriConfigSource, workflow, releaseGuide, packageSource, packageLockSource, tauriBuildScript] = await Promise.all([
    read("src-tauri/tauri.conf.json"),
    read(".github/workflows/release.yml"),
    read("docs/RELEASING.md"),
    read("package.json"),
    read("package-lock.json"),
    read("scripts/tauri-build.mjs"),
  ]);
  const tauriConfig = JSON.parse(tauriConfigSource);
  const packageConfig = JSON.parse(packageSource);
  const packageLock = JSON.parse(packageLockSource);

  assert.equal(packageLock.version, packageConfig.version);
  assert.equal(packageLock.packages[""].version, packageConfig.version);

  assert.deepEqual(tauriConfig.plugins.updater.endpoints, [
    "https://github.com/Git4Agent/mallard/releases/latest/download/latest.json",
  ]);
  assert.equal(tauriConfig.bundle.createUpdaterArtifacts, true);
  assert.deepEqual(tauriConfig.bundle.macOS, {
    signingIdentity: "-",
  });

  for (const target of [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
  ]) assert.match(workflow, new RegExp(target));
  assert.match(workflow, /tauri-apps\/tauri-action@v1/);
  assert.match(workflow, /releaseDraft: true/);
  assert.match(workflow, /uploadUpdaterJson: true/);
  assert.match(workflow, /MALLARD_VERIFY_MACOS_BUNDLE: \$\{\{ runner\.os == 'macOS' && '1' \|\| '' \}\}/);
  assert.doesNotMatch(workflow, /name: Verify macOS app signature/);
  assert.doesNotMatch(workflow, /^\s+APPLE_[A-Z_]+:/m);
  assert.doesNotMatch(workflow, /TAURI_SIGNING_PRIVATE_KEY_PASSWORD/);
  assert.equal(packageConfig.scripts.tauri, "node scripts/tauri-build.mjs");
  assert.match(tauriBuildScript, /process\.execPath/);
  assert.match(tauriBuildScript, /"@tauri-apps", "cli", "tauri\.js"/);
  assert.doesNotMatch(tauriBuildScript, /tauri\.cmd/);
  assert.doesNotMatch(workflow, /Cloudflare|CLOUDFLARE|\bR2\b|r2 object|api\.mallard-ai\.com/);

  assert.match(releaseGuide, /GitHub Releases is the public source of truth/);
  assert.match(releaseGuide, /releases\/latest\/download\/latest\.json/);
  assert.match(releaseGuide, /ad-hoc tester build/i);
  assert.match(releaseGuide, /manual Gatekeeper approval/i);
  assert.doesNotMatch(releaseGuide, /TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""/);
  assert.doesNotMatch(releaseGuide, /CLOUDFLARE_API_TOKEN|CLOUDFLARE_ACCOUNT_ID/);
});
