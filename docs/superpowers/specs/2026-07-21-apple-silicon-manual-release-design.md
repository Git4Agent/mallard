# Apple Silicon Manual Release Design

**Date:** 2026-07-21
**Target version:** 0.1.2
**Status:** Approved for implementation planning

## Objective

Produce Mallard 0.1.2 as one directly installable Apple Silicon DMG built by
GitHub Actions. Mallard will use macOS ad-hoc code signing because no Apple
Developer account is available. Users may need to approve the app manually in
System Settings, but the downloaded app must not fail because its bundle has no
valid structural signature.

The release will not include automatic updates, Intel macOS artifacts, Windows
artifacts, Apple Developer ID signing, or notarization.

## Root Cause

The v0.1.0 Tauri configuration did not specify a macOS signing identity. Tauri's
GitHub distribution guidance warns that Apple Silicon applications downloaded
from GitHub can be treated as damaged when they are built without an Apple
certificate and without an explicit ad-hoc identity.

The current branch already addresses that failure mode with
`bundle.macOS.signingIdentity: "-"`. Its build wrapper verifies the generated
application using `codesign --verify --deep --strict` before release artifacts
are uploaded. Both protections must remain.

## Application Changes

Automatic updating will be removed as a product capability rather than merely
disabled in the workflow:

- remove `AppUpdater` and its frontend integration test;
- remove the updater UI from `App`;
- remove the updater-only busy-state callback between `App` and
  `ProjectSyncV3`;
- remove `@tauri-apps/plugin-updater` and the updater-only process/relaunch
  package;
- remove the corresponding Rust dependencies and plugin initialization;
- remove updater and restart capabilities; and
- remove updater endpoints, public key, and updater artifact generation from
  the Tauri configuration.

No updater request, notification, signing key, signature file, or `latest.json`
manifest will remain in the application or release path.

## Release Pipeline

The tag-triggered GitHub Actions workflow will have one build job:

- runner: `macos-latest`;
- Rust target: `aarch64-apple-darwin`;
- Tauri bundle selection: `dmg`;
- output intended for publication: one Apple Silicon `.dmg`; and
- release state: draft until manually tested.

The workflow will retain only the automatically provided `GITHUB_TOKEN`.
`TAURI_SIGNING_PRIVATE_KEY`, updater-upload options, Apple certificate secrets,
Intel targets, and Windows targets will not be referenced.

The existing build wrapper will continue to locate the generated `Mallard.app`
and run `codesign --verify --deep --strict --verbose=4` before the release draft
can be created or updated. Ad-hoc signing uses no private key and does not make
the app notarized.

## Versioning and Documentation

Version `0.1.2` will be set consistently in `package.json`, `package-lock.json`,
`src-tauri/Cargo.toml`, and `src-tauri/tauri.conf.json`.

The release guide will describe manual DMG distribution and the expected user
flow:

1. Download the Apple Silicon DMG from GitHub Releases.
2. Drag Mallard to Applications.
3. Attempt to open Mallard.
4. If macOS blocks it, open System Settings, select Privacy & Security, and use
   Open Anyway for Mallard.

The guide must accurately state that the app is ad-hoc signed and unnotarized.

## Testing and Verification

Static release tests will require:

- version equality across all four version sources;
- exactly one Apple Silicon target in the release workflow;
- `--bundles dmg` in the release build;
- `signingIdentity: "-"` in Tauri configuration;
- the pre-upload `codesign` verification gate;
- absence of updater packages, Rust plugins, capabilities, configuration,
  secrets, manifest uploads, and documentation; and
- absence of Apple Developer signing secrets.

Implementation verification will run the frontend integration tests, release
configuration tests, frontend production build, and Rust checks.

On the local Apple Silicon Mac, the final checkpoint will build the v0.1.2 DMG,
verify the DMG container, confirm the executable is `arm64`, and validate the
application bundle with `codesign --verify --deep --strict`. The resulting DMG
path will be provided to the user for a real installation test.

No v0.1.2 tag will be created or pushed until the user confirms that the local
DMG can be installed using the documented manual Gatekeeper approval flow.

## Success Criteria

- Mallard contains no automatic-update behavior or updater signing-key
  requirement.
- Local and GitHub builds produce an Apple Silicon DMG for version 0.1.2.
- The packaged application has a valid ad-hoc signature and an `arm64`
  executable.
- The release workflow refuses to upload an invalid app bundle.
- The user can copy Mallard to Applications and open it after macOS manual
  approval.
- GitHub creates a draft containing the Apple Silicon DMG and no updater,
  Intel, or Windows artifacts.
