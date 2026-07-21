# Mallard

Mallard is a Tauri 2 desktop app for moving selected Codex and Claude project
resources between machines. It syncs project-owned conversations, settings,
skills, plugins, and—when the project is not inside a Git work tree—optional
ordinary files and folders. It never mirrors an entire provider home.

Current contracts: app `0.1.0`, machine-local config schema 3, portable bundle
schema 4, and storage layout 1. Unsupported schemas are rejected; there is no
migration or compatibility path in the current implementation.

## What it supports

- Machine-local Codex and Claude profiles mapped to each project checkout.
- Local-folder and S3-compatible storage, including Cloudflare R2.
- Destination-specific resource recipes and reviewed remote bases.
- Immutable bundle generations with SHA-256 verification and head
  compare-and-swap publication.
- Pull planning, explicit per-resource approval, backups, apply receipts,
  dependency actions, and readiness checks.
- Optional non-Git project-file sync with exact file and directory tracking,
  empty-directory restoration, and safe deletion tombstones.
- Local project chat history, path repair, and retained activity logs.

## Run locally

Requirements: Node.js with npm, a stable Rust toolchain, and Git.

```sh
npm install
npm run dev          # frontend at http://localhost:1420
npm run tauri dev    # desktop app
```

Configuration is created through the app; no `.env` file is required.

## Basic workflow

1. Add a local folder or S3/R2 storage.
2. Add the project and select its local Codex and/or Claude profile.
3. Discover resources, choose the initial recipe, and link the project to a
   storage.
4. Open **Push** and review **Git & sessions**, **Skills**, **Plugins**, and
   **Review**.
5. For a non-Git project, optionally open **Project files** after **Plugins**
   and run **Scan project files**. Newly discovered eligible entries are
   selected for that pending Push by default. Files and directories are
   tracked individually; selecting a file also selects its ancestor folders.
6. On another machine, map the same bundle to a local checkout, open **Pull**,
   and approve the exact actions to apply. Project-file actions start
   unchecked, so unchecked entries stay local.

Git-managed projects do not upload ordinary project content. If a bundle
already contains project files and the mapped folder becomes Git-managed, the
Project files step remains visible but locked.

## Project-file safety

- Scans are explicit and read-only. Selection metadata changes only after a
  successful Push.
- `.gitignore`, `.ignore`, and `.mallardignore` are honored. VCS data, build
  output, provider homes, Mallard metadata/storage, credential filenames,
  links, and special files are excluded or blocked.
- Credential-shaped content and executable files require a byte-bound warning
  acknowledgement; private-key material is blocked.
- Removing a local path does not remove it from storage. The user must choose
  **Remove from storage** for each tracked entry.
- Pull deletions are unselected by default. An approved file deletion verifies
  the unchanged digest, creates a backup, and removes only that regular file.
  An approved directory deletion removes only the exact empty directory and
  is never recursive.

For example, storage entry `project/docs/specs/a.md` restores to
`<project-root>/docs/specs/a.md`. `docs` and `docs/specs` are separate tracked
directory entries, so both are recreated before the file is written.

## Metadata

Machine-only state lives under `~/.mallard/` and includes paths, provider
profiles, storage credentials, project/storage links, recipes, exclusion
preferences, reviewed bases, plans, receipts, backups, caches, and logs. It is
never uploaded as a directory.

Portable storage uses this layout for both local folders and S3/R2:

```text
.mallard/
|-- _storage.json
`-- v1/repositories/<bundle-id>/
    |-- _tag.json
    |-- _head.json
    |-- _manifests/<generation>-<commit-id>.json
    |-- _commits/<generation>-<commit-id>.json
    `-- _uploads/<upload-id>/files/<logical-path>
```

Local-folder storage also uses `.mallard/.storage.lock` to serialize writers
on the same filesystem.

The schema-4 manifest contains portable relative paths, selected resource
descriptors, file hashes/sizes/safe modes/source mtimes, directory entries,
and typed tombstones. Absolute checkout paths, provider-home paths,
credentials, plans, receipts, backups, and local exclusion preferences remain
on the machine.

## Verify

```sh
npm run build
npm run test:frontend-integration
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --lib
```

The Rust suite includes tests that bind localhost for the stub S3 transport;
restricted environments may require permission for that test server.

## Code map

- `src/components/project-sync/` — project setup, Push/Pull review, project
  files, storage, and status UI.
- `src/components/project-sync/api.ts` and `src/types.ts` — Tauri command and
  DTO contracts.
- `src-tauri/src/project_sync_v3/domain.rs` — current local and portable
  schemas, validation, plans, and receipts.
- `src-tauri/src/project_sync_v3/provider_capture.rs` — Codex/Claude discovery
  and no-follow project-content scanning.
- `src-tauri/src/project_sync_v3/bundle_engine.rs` — publish, fetch, CAS,
  restore planning, backups, and apply.
- `src-tauri/src/project_sync_v3/commands.rs` — command orchestration and
  metadata transactions.
- `src-tauri/src/project_sync_v3/persistence.rs` — bounded atomic state under
  `~/.mallard`.
- `src-tauri/src/project_sync_v3/s3_store.rs` — S3/R2 object-store adapter.

See [TECHNICAL_REPORT.md](./TECHNICAL_REPORT.md) for the complete current
architecture and sync semantics.

## License

Licensed under the [Apache License 2.0](./LICENSE).
