# Apple Silicon Manual Release Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Produce Mallard 0.1.2 as one ad-hoc-signed Apple Silicon DMG with no automatic updater or updater signing-key requirement.

**Architecture:** Remove the updater vertically from React, Tauri capabilities, Rust initialization, and both dependency graphs. Keep Tauri's explicit macOS ad-hoc identity and verify the application mounted from the finished DMG, while narrowing the tag workflow to one `aarch64-apple-darwin` job. Build and inspect the same DMG locally before creating any release tag.

**Tech Stack:** React 19, TypeScript 5.8, Vite 7, Tauri 2, Rust, Node test runner, GitHub Actions, macOS `codesign`, `file`, and `hdiutil`.

## Global Constraints

- Set the public version to exactly `0.1.2` in all four version sources.
- Produce only an Apple Silicon DMG.
- Keep `bundle.macOS.signingIdentity: "-"`.
- Do not claim Developer ID signing or notarization.
- Remove every updater package, behavior, key, endpoint, signature, and manifest.
- Keep the GitHub release as a draft until the local DMG is installed.
- Do not create or push `v0.1.2` before the user verifies the local DMG.
- Preserve the user's unrelated document moves in the working tree.

---

### Task 1: Define the updater-free release contract

**Files:**
- Modify: `tests/release-config.test.mjs`
- Test: `tests/release-config.test.mjs`

**Interfaces:**
- Consumes: repository files as text and parsed JSON.
- Produces: regression coverage for version consistency, updater removal, Apple Silicon-only packaging, and signature validation.

- [ ] **Step 1: Replace the updater-oriented test with three failing contracts**

Replace the file with:

```js
import assert from "node:assert/strict";
import { access, readFile } from "node:fs/promises";
import test from "node:test";

const projectUrl = new URL("../", import.meta.url);
const read = (path) => readFile(new URL(path, projectUrl), "utf8");

test("release version is 0.1.2 in every source", async () => {
  const [tauriConfigSource, cargoSource, packageSource, packageLockSource] = await Promise.all([
    read("src-tauri/tauri.conf.json"),
    read("src-tauri/Cargo.toml"),
    read("package.json"),
    read("package-lock.json"),
  ]);
  const tauriConfig = JSON.parse(tauriConfigSource);
  const packageConfig = JSON.parse(packageSource);
  const packageLock = JSON.parse(packageLockSource);

  assert.equal(tauriConfig.version, "0.1.2");
  assert.match(cargoSource, /^version = "0\.1\.2"$/m);
  assert.equal(packageConfig.version, "0.1.2");
  assert.equal(packageLock.version, "0.1.2");
  assert.equal(packageLock.packages[""].version, "0.1.2");
});

test("application contains no automatic updater", async () => {
  const [tauriConfigSource, cargoSource, libSource, capabilitySource, packageSource,
    packageLockSource, appSource, projectSyncSource, testRunnerSource] = await Promise.all([
    read("src-tauri/tauri.conf.json"),
    read("src-tauri/Cargo.toml"),
    read("src-tauri/src/lib.rs"),
    read("src-tauri/capabilities/default.json"),
    read("package.json"),
    read("package-lock.json"),
    read("src/App.tsx"),
    read("src/components/project-sync/ProjectSyncV3.tsx"),
    read("scripts/run-frontend-integration-tests.mjs"),
  ]);
  const tauriConfig = JSON.parse(tauriConfigSource);
  const packageConfig = JSON.parse(packageSource);

  assert.equal(tauriConfig.bundle.createUpdaterArtifacts, undefined);
  assert.equal(tauriConfig.plugins?.updater, undefined);
  assert.equal(packageConfig.dependencies["@tauri-apps/plugin-updater"], undefined);
  assert.equal(packageConfig.dependencies["@tauri-apps/plugin-process"], undefined);
  for (const source of [cargoSource, libSource, capabilitySource,
    packageLockSource, appSource, projectSyncSource, testRunnerSource]) {
    assert.doesNotMatch(source, /updater|AppUpdater|onBusyChange|tauri-plugin-process|plugin-process|allow-restart/i);
  }
  await assert.rejects(access(new URL("src/components/AppUpdater.tsx", projectUrl)),
    (error) => error?.code === "ENOENT");
  await assert.rejects(access(new URL("tests/frontend/app-updater.integration.test.tsx", projectUrl)),
    (error) => error?.code === "ENOENT");
});

test("desktop release builds one verified Apple Silicon DMG", async () => {
  const [tauriConfigSource, workflow, releaseGuide, packageSource, tauriBuildScript] = await Promise.all([
    read("src-tauri/tauri.conf.json"),
    read(".github/workflows/release.yml"),
    read("docs/RELEASING.md"),
    read("package.json"),
    read("scripts/tauri-build.mjs"),
  ]);
  const tauriConfig = JSON.parse(tauriConfigSource);
  const packageConfig = JSON.parse(packageSource);

  assert.deepEqual(tauriConfig.bundle.macOS, { signingIdentity: "-" });
  assert.match(workflow, /target: aarch64-apple-darwin/);
  assert.doesNotMatch(workflow, /x86_64-apple-darwin|windows-latest|x86_64-pc-windows-msvc/);
  assert.match(workflow, /bundles: dmg/);
  assert.match(workflow, /releaseDraft: true/);
  assert.match(workflow, /MALLARD_VERIFY_MACOS_BUNDLE: "1"/);
  assert.doesNotMatch(workflow, /TAURI_SIGNING|uploadUpdater|updaterJson|APPLE_[A-Z_]+/);
  assert.equal(packageConfig.scripts.tauri, "node scripts/tauri-build.mjs");
  assert.match(tauriBuildScript, /"codesign", \["--verify", "--deep", "--strict", "--verbose=4"/);
  assert.match(releaseGuide, /Apple Silicon/i);
  assert.match(releaseGuide, /Privacy & Security/i);
  assert.doesNotMatch(releaseGuide, /updater|TAURI_SIGNING|latest\.json/i);
});
```

- [ ] **Step 2: Run the test and watch it fail**

Run: `npm run test:release-config`

Expected: FAIL because version 0.1.1, updater files, and extra release targets still exist.

- [ ] **Step 3: Commit the red contract**

```bash
git add tests/release-config.test.mjs
git commit -m "Test manual Apple Silicon releases"
```

### Task 2: Remove automatic updating from the frontend

**Files:**
- Modify: `src/App.tsx`
- Modify: `src/components/project-sync/ProjectSyncV3.tsx`
- Modify: `src/components/project-sync/ProjectSidebar.tsx`
- Modify: `src/App.css`
- Modify: `scripts/run-frontend-integration-tests.mjs`
- Delete: `src/components/AppUpdater.tsx`
- Delete: `src/components/AppUpdateControl.tsx`
- Delete: `tests/frontend/app-updater.integration.test.tsx`

**Interfaces:**
- Consumes: `ProjectSyncV3` with `theme` and `onThemeChange`.
- Produces: an application root with no update checks, dialogs, events, or relaunch behavior.

- [ ] **Step 1: Remove updater rendering and busy propagation**

Make the application root exactly:

```tsx
export default function App() {
  const [theme, setTheme] = useState<AppTheme>(getStoredTheme);
  useEffect(() => applyTheme(theme), [theme]);
  return <ProjectSyncV3 theme={theme} onThemeChange={setTheme} />;
}
```

Remove `onBusyChange` from `ProjectSyncV3`'s props, parameters, and its two busy-reporting effects.

- [ ] **Step 2: Delete updater-owned frontend artifacts**

Delete `AppUpdater.tsx`, `AppUpdateControl.tsx`, and the updater integration
test. Remove the sidebar control import/render and the test-runner entry.
Remove the two `.app-update-control` rules and the complete
`/* Signed application updates */` section from `src/App.css`.

- [ ] **Step 3: Verify the frontend**

Run: `npm run test:frontend-integration`

Expected: PASS.

Run: `npm run build`

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/App.tsx src/App.css src/components/project-sync/ProjectSyncV3.tsx scripts/run-frontend-integration-tests.mjs src/components/AppUpdater.tsx tests/frontend/app-updater.integration.test.tsx
git commit -m "Remove automatic update UI"
```

### Task 3: Remove updater infrastructure from Tauri

**Files:**
- Modify: `package.json`
- Modify: `package-lock.json`
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/Cargo.lock`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/capabilities/default.json`
- Modify: `src-tauri/tauri.conf.json`

**Interfaces:**
- Consumes: Tauri core, opener, and dialog plugins.
- Produces: a desktop bundle without updater/relaunch APIs or updater artifacts.

- [ ] **Step 1: Remove updater and process dependencies**

Remove `@tauri-apps/plugin-process`, `@tauri-apps/plugin-updater`,
`tauri-plugin-process`, and `tauri-plugin-updater`. Update both lockfiles.

- [ ] **Step 2: Remove runtime initialization and permissions**

Delete these builder calls:

```rust
.plugin(tauri_plugin_process::init())
.plugin(tauri_plugin_updater::Builder::new().build())
```

Delete `updater:default` and `process:allow-restart` from the default capability.

- [ ] **Step 3: Remove updater configuration but retain ad-hoc signing**

Remove `createUpdaterArtifacts` and the complete `plugins.updater` object. Keep:

```json
"macOS": {
  "signingIdentity": "-"
}
```

- [ ] **Step 4: Verify native code and dependency graphs**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`

Expected: PASS and update `Cargo.lock` without updater/process plugin packages.

Run: `npm run build`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add package.json package-lock.json src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/lib.rs src-tauri/capabilities/default.json src-tauri/tauri.conf.json
git commit -m "Remove updater infrastructure"
```

### Task 4: Configure the v0.1.2 Apple Silicon release

**Files:**
- Modify: `package.json`
- Modify: `package-lock.json`
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/Cargo.lock`
- Modify: `src-tauri/tauri.conf.json`
- Modify: `.github/workflows/release.yml`
- Modify: `docs/RELEASING.md`

**Interfaces:**
- Consumes: tags matching `v*` and GitHub's automatic token.
- Produces: a draft release containing the v0.1.2 Apple Silicon DMG.

- [ ] **Step 1: Set all versions to 0.1.2**

Update the root versions in both npm files, the Mallard version in both Cargo
files, and the Tauri configuration version.

- [ ] **Step 2: Collapse the workflow to one job**

Use `runs-on: macos-latest`, Rust target `aarch64-apple-darwin`,
`MALLARD_VERIFY_MACOS_BUNDLE: "1"`, and:

```yaml
args: >-
  --target aarch64-apple-darwin
  --bundles dmg
```

Keep `GITHUB_TOKEN`, `tauri-action@v1`, tests, version checking, and
`releaseDraft: true`. Remove the matrix and all updater inputs. State in the
release body that this is an ad-hoc-signed, unnotarized Apple Silicon build.

- [ ] **Step 3: Rewrite release documentation**

Document the tag-to-draft flow, local DMG build, `codesign`, `file`, and
`hdiutil verify`, plus manual approval through System Settings > Privacy &
Security > Open Anyway. Remove updater, Intel, Windows, and Apple-secret setup.

- [ ] **Step 4: Make the red release contract green**

Run: `npm run check:release-version -- v0.1.2`

Expected: PASS.

Run: `npm run test:release-config`

Expected: all tests PASS.

- [ ] **Step 5: Commit**

```bash
git add package.json package-lock.json src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/tauri.conf.json .github/workflows/release.yml docs/RELEASING.md
git commit -m "Prepare Apple Silicon v0.1.2 release"
```

### Task 5: Build, inspect, and hand off the local DMG

**Files:**
- Verify: `src-tauri/target/aarch64-apple-darwin/release/bundle/dmg/Mallard_0.1.2_aarch64.dmg`
- Verify: `src-tauri/target/aarch64-apple-darwin/release/bundle/macos/Mallard.app`

**Interfaces:**
- Consumes: completed v0.1.2 source and the local macOS toolchain.
- Produces: a verified Apple Silicon DMG for installation testing.

- [ ] **Step 1: Run all non-packaging verification**

Run: `npm run test:integration`

Expected: PASS.

Run: `npm run build`

Expected: PASS.

Run: `cargo check --manifest-path src-tauri/Cargo.toml`

Expected: PASS.

- [ ] **Step 2: Build through the signature-checking wrapper**

```bash
MALLARD_VERIFY_MACOS_BUNDLE=1 npm run tauri build -- --target aarch64-apple-darwin --bundles dmg
```

Expected: Tauri creates `Mallard_0.1.2_aarch64.dmg`; the wrapper verifies and
mounts that DMG, runs strict `codesign` validation against its `Mallard.app`,
and detaches it successfully.

- [ ] **Step 3: Inspect the outputs**

```bash
hdiutil verify src-tauri/target/aarch64-apple-darwin/release/bundle/dmg/Mallard_0.1.2_aarch64.dmg
file src-tauri/target/aarch64-apple-darwin/release/bundle/macos/Mallard.app/Contents/MacOS/mallard
codesign --verify --deep --strict --verbose=4 src-tauri/target/aarch64-apple-darwin/release/bundle/macos/Mallard.app
codesign -dvvv src-tauri/target/aarch64-apple-darwin/release/bundle/macos/Mallard.app
```

Expected: valid DMG checksum, `arm64` executable, valid ad-hoc bundle signature.

- [ ] **Step 4: Hand off without tagging**

Provide the absolute DMG path for the user to install. Do not tag or push.

- [ ] **Step 5: Tag only after successful user verification**

After the user confirms installation, create `v0.1.2`, push `main`, then push
the tag. GitHub should create one draft release with one DMG; publishing remains
a separate human action.
