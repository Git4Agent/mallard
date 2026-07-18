# Sync Integration Tests

Integration tests for the DESIGN2 sync engine (head-CAS publishes, union
reconciliation, conflict resolution) that run entirely on the local machine —
no network, no credentials, no real bucket.

The engine has two storage backends behind the `Store` enum — an
S3-compatible bucket and the local-folder mode (`kind: "local"`) — and the
suite runs the portable scenarios against **both**: `run_*` bodies with one
thin `#[tokio::test]` wrapper per backend (`*_local` suffix). The S3 wrapper
talks to the stub server over HTTP; the local wrapper hands the same bucket
directory to the app's own local store, no server at all.

```sh
cd src-tauri
cargo test --lib sync_tests        # the integration scenarios (~7s)
cargo test --lib                   # scenarios + all unit tests
```

The tests serialize themselves through an internal lock (`$HOME` is
process-global), so a plain `cargo test` is safe — no `--test-threads=1`
needed.

## How it works

### Cloud: a filesystem-backed stub S3 server (`stub_s3.rs`)

Each test starts a tiny HTTP server on an ephemeral localhost port. The
production `make_s3_client` is pointed at it via `SyncConfig.s3_endpoint`, so
requests travel the **real** AWS SDK path (sigv4 signing, path-style URLs,
ETag parsing, 412 mapping). Every object is stored as a plain file at
`<tempdir>/test-bucket/<key>`, which lets assertions read the published cloud
layout (`_head.json`, `_manifests/`, `_commits/`, `_uploads/`) directly with
`serde_json`.

Implemented surface — exactly what `lib.rs` uses, nothing more:

| Operation | Behavior |
|---|---|
| `GET` object | body + `ETag: "<sha256 of bytes>"`; 404 `NoSuchKey` XML if missing |
| `PUT` object | writes the file; honors `If-Match: "<etag>"` (CAS) and `If-None-Match: *` (put-if-absent) with 412 on failure |
| `DELETE` object | removes the file (probe cleanup) |
| `ListObjectsV2` | `delimiter`/`prefix` grouping into `CommonPrefixes` (profile discovery) |

Test hooks, armed per key suffix on the next N *conditional* PUTs:

- `HookAction::RunBefore(callback)` — runs before the precondition is
  evaluated. Used with `publish_external_commit` to move the head between a
  pusher's read and its CAS, producing an organically failing `If-Match` —
  a deterministic lost race.
- `HookAction::StallAfterWrite(duration)` — applies the write, then stalls
  the response past the client timeout, so the pusher sees an ambiguous
  outcome for a write that actually landed.
- `set_ignore_conditions(true)` — simulates a store that accepts conditional
  headers but silently ignores them (the negative case the capability probe
  exists for).
- `requests()` — a wire log of `(method, key, conditional, status)` so tests
  can assert the exact CAS sequence (e.g. `200, 412, 200`).

### Machines: temp `$HOME` directories (`harness.rs`)

A `Machine` is a temp directory acting as `$HOME` — with `~/.codex` and
`~/.claude` trees — plus a mock Tauri app (`tauri::test::MockRuntime`).
Because Tauri's path resolver derives from `$HOME`, everything a machine
persists (per-link baselines, saved config with probe results and resolved
links, pull backups) is isolated inside its temp home. Nothing touches the
real `~/.codex`, `~/.claude`, or `~/Library/Application Support`.

Since PLAN_MULTI_STORAGE.md the config is the v2 link matrix: each
`TestCloud` carries a stable `storage_id`, and before every operation the
machine upserts that cloud's storage, the default profiles ("codex" /
"claude") with their mount paths, and the (profile, storage) link into its
saved config (`ensure_link_config`) — the harness equivalent of clicking
the settings matrix. Pins set via `set_sync_link`/`pin_cloud_prefix` apply
to every storage the machine touches. Two clouds in one test are two
storages with independent baselines (that isolation is exactly what
S21–S23 assert). Beyond the two defaults, `add_profile(id, root, dir, pin)`
registers a custom local profile (matrix row) synced per link with
`push_profile`/`pull_profile` and seeded/read with the `*_profile` file
helpers — S24/S25 use this for a second `.claude` profile.

`machine.push(...)`/`.pull(...)` call the real `do_push_link`/`do_pull_link`
entry points, one link per root kind. Each operation first points `$HOME`
at that machine's home, so machines act strictly one at a time — which is
why every test body holds `harness::lock_env()`.

For a *concurrent* writer (impossible in-process with a global `$HOME`),
`publish_external_commit` fabricates one: it writes upload objects, a
manifest, a commit record, and a new head directly into the bucket directory
— exactly the artifacts a real push publishes.

### Cleanup

Every artifact lives in a `tempfile::TempDir` (machine homes + cloud root)
and is deleted on drop, including when a test fails. Verified: no leftover
`sync-*` directories in `$TMPDIR` after runs.

Debug escape hatch: `KEEP_SYNC_TEST_DIRS=1 cargo test --lib sync_tests`
keeps all directories and prints their paths (`[keep] ...`) so a failure's
cloud layout and machine homes can be inspected.

## Backend coverage

| Scenario | S3 (stub server) | Local folder |
|---|---|---|
| S1–S4, S8, S9 | ✓ | ✓ (shared `run_*` body) |
| S5 lost head CAS | ✓ (stub `RunBefore` hook, wire-log assert) | ✓ (`LOCAL_CAS_HOOK` injection) |
| S5b give-up after 3 races | ✓ | — (retry loop lives above the Store layer) |
| S6 ambiguous publish | ✓ | n/a — the local store cannot time out |
| S7 probe fallback | ✓ | n/a — local mode never probes; its lock-file CAS is intrinsic |
| `local_store_cas_single_winner` | — | ✓ (8 concurrent writers, one wins) |
| S10 destination switch | ✓ (S3 → local → back, stale links relink) | ✓ |
| S11 custom local mount (sync-link left side) | ✓ | ✓ |
| S12 full sync-link story (`~/.codex ⇄ 001/.codex`, `<custom> ⇄ 001/.codex`) | ✓ | ✓ |
| S13 pinned prefix created/recreated at its exact name | ✓ | ✓ |
| S14 nested-prefix auto-discovery (`001/.codex` found by root) | ✓ | ✓ |
| `save_sync_config_scopes_state_to_storage_identity` | ✓ | ✓ |
| `sync_link_state_scopes_correctly` (mount + pin per-machine, resolved state per-identity) | ✓ | ✓ |
| S15 three homes, one bucket (001/.codex ×2 + 002/.codex, disjoint) | ✓ | ✓ |
| S16 namespace pairs + "pin one explicitly" multi-match guard | ✓ | ✓ |
| S17 mount relocation (moved files publish nothing; empty mount restores) | ✓ | ✓ |
| S18 mixed shapes (pinned+custom codex beside auto+default claude) | ✓ | ✓ |
| S19 wrong-root pin fails loudly, store untouched, re-pin recovers | ✓ | ✓ |
| S20 repointing pins 001→002→001, zero conflict siblings | ✓ | ✓ |
| S21 same pinned prefix in two storages, no baseline cross-talk | ✓ | ✓ |
| S22 fan-out: one profile pushed to two storages, hash re-verify | ✓ | ✓ |
| S23 unlink/identity-change drops the baseline; relink re-verifies | ✓ | ✓ |
| S24 matrix: 2 storages × 3 profiles (fan-out + same-name pins + same-kind neighbors, per-link statuses) | ✓ | ✓ |
| S25 storage/profile removal forgets link state only; local/cloud data stays | ✓ | ✓ |
| S26 two local roots share ONE cloud profile (per-link baselines: stale sibling converges, never clobbers; divergent edits conflict normally) | ✓ | ✓ |
| S27 shared-profile relay convergence; a sibling root == a second machine | ✓ | ✓ |
| S28 shared cloud cache, per-link statuses (sibling reads cloud-ahead, pusher reads synced) | ✓ | ✓ |
| S29 conflict-copy resolution from one sibling root reaches the other via the shared profile | ✓ | ✓ |
| S30 picker: pinned pick among several profiles pulls exactly it (incl. plugin lock), creates/relinks nothing | ✓ | ✓ |
| S31 auto link (`cloud: {}`): one candidate links unpinned; two error "pin one explicitly", storage untouched | ✓ | ✓ |
| S32 picker create-new: fresh pinned id + deduped label lands alongside existing profile (P1 fix) | ✓ | ✓ |
| S33 re-pick drops the old baseline via settings save, re-verifies cleanly, sibling unaffected (P2 fix) | ✓ | ✓ |
| `codex_project_path_mapping_flow` (manual project-path picking: lock-derived source validation, machine-local mapping outside every manifest, pending vs immediate mapped sidebar apply, `codex resume -C` report, removal never mutates the sidebar) | ✓ | ✓ |
| `statuses_under_custom_mounts` / `editor_boundary_follows_mounts` / `mount_name_is_cosmetic` | ✓ (single backend) | partial |

The local store implements CAS as check-then-write under an exclusive lock on
`<dir>/.lock` (std `File::lock`), with temp-file + rename writes so a crash
leaves either the old or the new object, never a torn one. Caveat mirrored in
the UI copy: folder-sync services (Dropbox/iCloud) don't propagate locks
across machines — simultaneous multi-machine pushes there degrade to the
service's conflict handling, losing at worst a generation pointer, never
object bytes.

## Scenarios (`mod.rs`)

**S1 — `s1_bootstrap_and_idempotent_repush`.** Machine A's first push
auto-creates one profile per root (`.codex` and `.claude` never share a
profile), publishes generation 1 with a valid head→manifest→objects chain,
excludes the never-sync tier (`auth.json` seeded, never uploaded), records
`supports_conditional_writes: true` from the probe, and cleans the probe
object up. A second push with no changes publishes nothing ("up to date",
generation unchanged).

**S2 — `s2_a_push_b_pull_b_push_a_pull`.** The requested round-trip: A
pushes; B (empty dirs, only bucket creds) pulls and auto-links both roots,
receiving A's files byte-identical; B edits one file and adds another, then
pushes (codex generation bumps, untouched claude root stays put); A pulls
again and receives both changes. Finale: A deletes a file locally and pulls —
the union restores it (deletions never propagate).

**S3 — `s3_divergent_push_conflict_copy`.** The requested conflict case,
on a file with no merge driver: from a shared base, A pushes "from A", then B
pushes "from B". B's push detects both-changed, keeps its local content on
the path, and preserves A's version as the deterministic sibling
`notes.sync-conflict-<sha8-of-A's-content>.md`, publishing **both**. After A
pulls, both machines hold identical state and neither side's content was
lost.

**S4 — `s4_divergent_history_jsonl_merges`.** Same divergence shape on
`.codex/history.jsonl`, which has a deterministic merge driver: the result is
the deduplicated, timestamp-sorted union of both sides' lines — no conflict
sibling — and A, B, and the cloud converge to identical bytes.

**S5 — `s5_head_cas_race_rebases_and_republishes`.** The head-CAS crown
jewel. A hook publishes an external commit at the exact moment A's head CAS
arrives, so A's `If-Match` fails against the moved head. Asserts: the wire
log shows the lost CAS then the rebased retry (`..., 412, 200`); A's retry
reconciled against the winner's generation (the external file was applied to
A's disk); the final manifest is the union of both writers; the published
commit chain (walked via `previous_commit_key`) links gen 3→2→1→0 with the
external actor at gen 2; the lost attempt's staged upload batch remains an
unpublished orphan — exactly the crash/race debris DESIGN2 promises.

**S5b — `s5b_push_gives_up_when_cloud_keeps_changing`.** The same hook fires
on all three CAS attempts. The push fails cleanly with "cloud kept changing",
the winner's head/manifest survive untouched, and A's file is never
published.

**S6 — `s6_ambiguous_head_publish_resolves_as_written`.** The head write
lands but the response stalls past the (test-shortened) client timeout. The
pusher re-reads the head, recognizes its own commit id, and reports success —
with exactly one CAS attempt for that generation on the wire.

**S7 — `s7_single_writer_fallback`.** Against a store that ignores
conditional writes, the probe's stale `If-Match` succeeds, so the remote is
marked `supports_conditional_writes: false` and every subsequent head write
goes out unconditionally (last-writer-wins mode). Note: the second edit
changes the file's length — see the fast-path gotcha below.

**S8 — `s8_late_joiner_converges`.** After A and B's conflict dance, machine
C joins with empty dirs and only bucket credentials: one pull converges C to
the fully-merged state (B's content, A's conflict sibling, the claude root),
and C's follow-up push publishes nothing.

**S10 — `s10_destination_switch_relinks_profiles`.** Regression for switching
the sync destination (R2 → local folder): profile links are per-remote state.
A saved link whose profile doesn't exist in the current store falls through
to rediscovery/creation instead of failing with "no `_head.json`"; switching
back relinks the original profile with no duplicates. The companion
`save_sync_config_scopes_state_to_storage_identity` pins the settings-save
rule: an unchanged storage identity preserves resolved links + probe flag; a
changed identity drops that storage's state (pinned prefixes survive as
user intent) and leaves other storages untouched.

**S9 — `s9_manifest_corruption_fails_pull`.** A byte appended to the
manifest object the head references breaks its sha against
`head.manifest_sha256`: the pull fails loudly with a corruption error and
applies nothing locally.

## Production-code accommodations (type-level / test-only)

- `TauriRuntime` alias in `lib.rs`: `tauri::Wry` in production,
  `tauri::test::MockRuntime` under `cfg(test)` — the sync functions take
  concrete `AppHandle`s, and this is what lets a mock app drive them.
- `run()` is `#[cfg(not(test))]` (its `generate_handler!` would instantiate
  the commands for Wry); a test-only keepalive references the commands so
  dead-code analysis stays active for everything else.
- `r2_request_timeout()` reads the test-only `TEST_REQUEST_TIMEOUT_MS`
  override (S6); production behavior — the 120s constant — is unchanged.
- `LOCAL_CAS_HOOK` (`cfg(test)`): a callback invoked with the key before a
  local conditional put evaluates its precondition — the local-mode mirror of
  the stub server's `RunBefore` hook, used by S5-local. Armed via the
  `LocalCasHookGuard` RAII guard.

## Gotchas for future tests

- **Stat fast path:** the baseline treats a file with unchanged size *and*
  mtime (second granularity) as synced without hashing. A test that edits a
  file to same-length content within the same second as the previous sync
  will see the edit ignored. Always change the content length.
- **Machines are sequential.** `$HOME` is process-global; do not try to run
  two machines' operations concurrently. For a concurrent writer, use
  `publish_external_commit` (optionally from a `RunBefore` hook).
- **Commit counting:** losers of head races leave orphaned commit/manifest/
  batch objects by design. Count published history via
  `TestCloud::commit_chain` (head-linked), not by listing `_commits/`.

## Not covered here

A smoke test against a live R2/S3 bucket is still pending: real TLS, real
ETag formats, and the store's actual conditional-write behavior are exactly
the things a local stub cannot prove.
