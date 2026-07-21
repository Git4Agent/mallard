# Ad-Hoc macOS Release Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish Mallard v0.1.1 as a valid, ad-hoc-signed macOS tester build that Gatekeeper can manually whitelist instead of rejecting as damaged.

**Architecture:** Tauri's `bundle.macOS.signingIdentity` explicitly uses the ad-hoc identity (`-`) when no Apple Developer certificate is available. A Tauri command wrapper validates the built macOS app bundle before `tauri-action` can create or upload a draft; a static Node test protects the required signing configuration and workflow guard.

**Tech Stack:** Tauri 2, GitHub Actions, Node.js built-in test runner, macOS `codesign`.

## Global Constraints

- Do not add Apple Developer credentials or claim that the tester build is notarized.
- Preserve updater signing as a separate mechanism.
- Publish a new tag; do not replace v0.1.0 artifacts.
- The release must keep producing the existing platform matrix.

---

### Task 1: Protect the ad-hoc signing contract

**Files:**
- Modify: `tests/release-config.test.mjs`
- Modify: `src-tauri/tauri.conf.json`

**Interfaces:**
- Consumes: Tauri `bundle.macOS.signingIdentity`.
- Produces: an explicit `"-"` identity that Tauri uses to create a valid ad-hoc macOS signature.

- [ ] **Step 1: Write the failing test**

Assert that `tauriConfig.bundle.macOS` equals `{ signingIdentity: "-" }`.

- [ ] **Step 2: Run the release configuration test to verify it fails**

Run: `npm run test:release-config`

Expected: failure because `bundle.macOS` is not configured.

- [ ] **Step 3: Add the minimal Tauri configuration**

Add the `macOS.signingIdentity` property under the existing `bundle` object.

- [ ] **Step 4: Run the release configuration test to verify it passes**

Run: `npm run test:release-config`

Expected: one passing test and zero failures.

### Task 2: Fail macOS release jobs before upload when the app is invalid

**Files:**
- Modify: `.github/workflows/release.yml`
- Create: `scripts/tauri-build.mjs`
- Modify: `tests/release-config.test.mjs`

**Interfaces:**
- Consumes: `target/${target}/release/bundle/macos/Mallard.app` produced by Tauri.
- Produces: a macOS-only guard in the Tauri build command that runs `codesign --verify --deep --strict --verbose=4` before `tauri-action` creates or uploads a release draft.

- [ ] **Step 1: Extend the failing test**

Assert that the workflow enables the macOS build guard and that the `tauri` package script uses the wrapper.

- [ ] **Step 2: Run the release configuration test to verify it fails**

Run: `npm run test:release-config`

Expected: failure because the validation step is absent.

- [ ] **Step 3: Add the minimal workflow validation step**

Add a Tauri command wrapper that runs the CLI, locates the exact bundle for `--target`, and verifies it before returning success to `tauri-action`. Enable that wrapper only for macOS workflow jobs.

- [ ] **Step 4: Run the release configuration test to verify it passes**

Run: `npm run test:release-config`

Expected: one passing test and zero failures.

### Task 3: Publish the tested release

**Files:**
- Modify: `package.json`
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/tauri.conf.json`
- Modify: `docs/RELEASING.md`

**Interfaces:**
- Consumes: the release-version script's three-version equality requirement.
- Produces: tag `v0.1.1`, which triggers the release workflow.

- [ ] **Step 1: Update all three version sources and release instructions**

Set the version to `0.1.1` and document that ad-hoc tester builds require manual Gatekeeper approval.

- [ ] **Step 2: Run local release checks**

Run: `npm run check:release-version -- v0.1.1 && npm run test:release-config && npm run build && cd src-tauri && cargo check`

Expected: all commands exit successfully.

- [ ] **Step 3: Commit and publish**

Commit the release fix, push the current branch, merge or fast-forward it into `main`, then create and push `v0.1.1` from `main`.

- [ ] **Step 4: Inspect the GitHub Actions release output**

Expected: all required release jobs complete; download the macOS DMG and validate the packaged app with `codesign` before publishing the GitHub Release.
