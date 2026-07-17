//! Integration tests for the DESIGN2 sync engine, run against both storage
//! backends: the stub S3 server (see `stub_s3.rs`) and the app's
//! local-folder mode operating on a directory directly. Machines are temp
//! `$HOME` directories (see `harness.rs`); every scenario holds
//! [`harness::lock_env`] for its whole body because `$HOME` is
//! process-global.
//!
//! Backend-portable scenarios live in `run_*` bodies with one thin wrapper
//! per backend. S6 (network-timeout ambiguity) and S7 (capability-probe
//! fallback) exist only for S3 — the local store has no timeouts and never
//! probes.
//!
//! Debugging: set `KEEP_SYNC_TEST_DIRS=1` to keep the cloud and machine
//! directories after a run (paths are printed on drop).

mod harness;
mod stub_s3;

use harness::{publish_external_commit, Machine, TestCloud};
use std::sync::atomic::Ordering;
use stub_s3::HookAction;

/// RAII guard for the test-only request-timeout override.
struct TimeoutOverride;

impl TimeoutOverride {
    fn set_ms(ms: u64) -> TimeoutOverride {
        crate::TEST_REQUEST_TIMEOUT_MS.store(ms, Ordering::Relaxed);
        TimeoutOverride
    }
}

impl Drop for TimeoutOverride {
    fn drop(&mut self) {
        crate::TEST_REQUEST_TIMEOUT_MS.store(0, Ordering::Relaxed);
    }
}

/// RAII guard for the local-store CAS interposition hook — the local-mode
/// mirror of the stub server's `RunBefore`.
struct LocalCasHookGuard;

impl LocalCasHookGuard {
    fn set(hook: impl FnMut(&str) + Send + 'static) -> LocalCasHookGuard {
        *crate::LOCAL_CAS_HOOK.lock().unwrap() = Some(Box::new(hook));
        LocalCasHookGuard
    }
}

impl Drop for LocalCasHookGuard {
    fn drop(&mut self) {
        *crate::LOCAL_CAS_HOOK.lock().unwrap() = None;
    }
}

/// S1 — bootstrap: first push auto-creates one profile per root, publishes a
/// valid DESIGN2 layout, and a second push with no changes is a no-op.
async fn run_s1_bootstrap_and_idempotent_repush(cloud: TestCloud) {
    let a = Machine::new("A");
    a.seed(".codex/memories/notes.md", "codex note v1\n");
    a.seed(
        ".codex/history.jsonl",
        "{\"ts\":100,\"text\":\"first prompt\"}\n",
    );
    a.seed(".claude/CLAUDE.md", "claude instructions v1\n");
    a.seed(".claude/projects/demo/session.jsonl", "{\"line\":1}\n");
    // Never-sync tier: must not appear in any manifest.
    a.seed(".codex/auth.json", "{\"secret\":\"do-not-sync\"}\n");

    let result = a.push_all(&cloud).await.expect("first push");
    assert!(result.success);

    // One profile per root, discriminated by the head's root field.
    let profiles = cloud.profiles_by_root();
    assert_eq!(profiles.len(), 2, "one profile per root: {:?}", profiles);

    let codex_head = cloud.head(".codex");
    assert_eq!(codex_head.generation, 1);
    assert_eq!(codex_head.state, "active");
    let codex_manifest = cloud.manifest(".codex");
    assert_eq!(codex_manifest.generation, 1);
    assert!(codex_manifest
        .files
        .contains_key(".codex/memories/notes.md"));
    assert!(codex_manifest.files.contains_key(".codex/history.jsonl"));
    // Files never cross profiles, and the never-sync tier never uploads.
    assert!(!codex_manifest
        .files
        .keys()
        .any(|k| k.starts_with(".claude/")));
    assert!(!codex_manifest.files.contains_key(".codex/auth.json"));

    let claude_manifest = cloud.manifest(".claude");
    assert!(claude_manifest.files.contains_key(".claude/CLAUDE.md"));
    assert!(claude_manifest
        .files
        .contains_key(".claude/projects/demo/session.jsonl"));
    assert!(!claude_manifest
        .files
        .keys()
        .any(|k| k.starts_with(".codex/")));

    if cloud.is_local() {
        // Local mode never probes: CAS support is intrinsic, and the
        // per-storage probed flag stays untouched.
        assert_eq!(a.saved_probe(&cloud), None);
    } else {
        // The conditional-write probe ran once, concluded "supported", and
        // cleaned up after itself.
        assert_eq!(a.saved_probe(&cloud), Some(true));
    }
    assert_eq!(a.saved_links(&cloud).len(), 2);
    assert!(
        !cloud.bucket_dir().join("_probe").exists()
            || std::fs::read_dir(cloud.bucket_dir().join("_probe"))
                .unwrap()
                .next()
                .is_none()
    );

    // Uploaded object bytes round-trip.
    let entry = &codex_manifest.files[".codex/memories/notes.md"];
    let object = std::fs::read(
        cloud
            .bucket_dir()
            .join(cloud.profile_for_root(".codex"))
            .join(&entry.object_key),
    )
    .unwrap();
    assert_eq!(object, b"codex note v1\n");

    // Idempotent re-push: nothing changed, so nothing publishes.
    let again = a.push_all(&cloud).await.expect("re-push");
    assert!(again.success);
    assert_eq!(cloud.head(".codex").generation, 1, "no new generation");
    assert_eq!(cloud.head(".claude").generation, 1);
    assert!(again.message.contains("up to date"), "{}", again.message);
}

#[tokio::test]
async fn s1_bootstrap_and_idempotent_repush() {
    let _env = harness::lock_env().await;
    run_s1_bootstrap_and_idempotent_repush(TestCloud::start().await).await;
}

#[tokio::test]
async fn s1_bootstrap_and_idempotent_repush_local() {
    let _env = harness::lock_env().await;
    run_s1_bootstrap_and_idempotent_repush(TestCloud::start_local().await).await;
}

/// S2 — A push, B pull, B push, A pull again; plus union deletion restore.
async fn run_s2_a_push_b_pull_b_push_a_pull(cloud: TestCloud) {
    let a = Machine::new("A");
    let b = Machine::new("B");

    a.seed(".codex/memories/notes.md", "shared note v1\n");
    a.seed(".claude/CLAUDE.md", "claude v1\n");
    a.push_all(&cloud).await.expect("A push");

    // B pulls with nothing but bucket creds: auto-links both roots and
    // receives A's files byte-identical.
    b.pull(&cloud).await.expect("B pull");
    assert_eq!(b.read(".codex/memories/notes.md"), "shared note v1\n");
    assert_eq!(b.read(".claude/CLAUDE.md"), "claude v1\n");
    assert_eq!(
        b.saved_links(&cloud).len(),
        2,
        "B auto-linked both roots"
    );

    // B edits one file, adds another, pushes.
    b.seed(".codex/memories/notes.md", "shared note v2 (from B)\n");
    b.seed(".codex/memories/from-b.md", "new file from B\n");
    b.push_all(&cloud).await.expect("B push");
    assert_eq!(cloud.head(".codex").generation, 2);
    assert_eq!(
        cloud.head(".claude").generation,
        1,
        "untouched root stays put"
    );

    // A pulls again: receives B's edit and the new file.
    a.pull(&cloud).await.expect("A pull again");
    assert_eq!(
        a.read(".codex/memories/notes.md"),
        "shared note v2 (from B)\n"
    );
    assert_eq!(a.read(".codex/memories/from-b.md"), "new file from B\n");
    assert_eq!(a.read(".claude/CLAUDE.md"), "claude v1\n");

    // Union semantics: a local deletion is restored from the cloud.
    a.delete(".codex/memories/from-b.md");
    a.pull(&cloud).await.expect("A restore pull");
    assert_eq!(a.read(".codex/memories/from-b.md"), "new file from B\n");
}

#[tokio::test]
async fn s2_a_push_b_pull_b_push_a_pull() {
    let _env = harness::lock_env().await;
    run_s2_a_push_b_pull_b_push_a_pull(TestCloud::start().await).await;
}

#[tokio::test]
async fn s2_a_push_b_pull_b_push_a_pull_local() {
    let _env = harness::lock_env().await;
    run_s2_a_push_b_pull_b_push_a_pull(TestCloud::start_local().await).await;
}

/// S3 — divergent edits to the same file: second pusher keeps its content
/// and preserves the other side as a deterministic conflict sibling; both
/// machines converge after the next pull.
async fn run_s3_divergent_push_conflict_copy(cloud: TestCloud) {
    let a = Machine::new("A");
    let b = Machine::new("B");

    a.seed(".codex/memories/notes.md", "base v0\n");
    a.push_all(&cloud).await.expect("A base push");
    b.pull(&cloud).await.expect("B pull base");

    a.seed(".codex/memories/notes.md", "from A\n");
    a.push_all(&cloud).await.expect("A push vA");
    assert_eq!(cloud.head(".codex").generation, 2);

    b.seed(".codex/memories/notes.md", "from B\n");
    b.push_all(&cloud).await.expect("B push vB");
    assert_eq!(cloud.head(".codex").generation, 3);

    let sha_a = crate::sha256_bytes(b"from A\n");
    let sha_b = crate::sha256_bytes(b"from B\n");
    let conflict_rel = crate::conflict_copy_rel(".codex/memories/notes.md", &sha_a);
    assert!(conflict_rel.contains("sync-conflict"), "{}", conflict_rel);

    // Cloud holds both sides: B's content on the path, A's as the sibling.
    assert_eq!(
        cloud.manifest_file_sha(".codex", ".codex/memories/notes.md"),
        Some(sha_b.clone())
    );
    assert_eq!(
        cloud.manifest_file_sha(".codex", &conflict_rel),
        Some(sha_a.clone())
    );

    // B's disk: kept local, cloud copy alongside.
    assert_eq!(b.read(".codex/memories/notes.md"), "from B\n");
    assert_eq!(b.read(&conflict_rel), "from A\n");

    // A pulls: converges to B's content plus the sibling — nothing lost.
    a.pull(&cloud).await.expect("A pull conflict");
    assert_eq!(a.read(".codex/memories/notes.md"), "from B\n");
    assert_eq!(a.read(&conflict_rel), "from A\n");
}

#[tokio::test]
async fn s3_divergent_push_conflict_copy() {
    let _env = harness::lock_env().await;
    run_s3_divergent_push_conflict_copy(TestCloud::start().await).await;
}

#[tokio::test]
async fn s3_divergent_push_conflict_copy_local() {
    let _env = harness::lock_env().await;
    run_s3_divergent_push_conflict_copy(TestCloud::start_local().await).await;
}

/// S4 — divergent appends to history.jsonl merge deterministically (union,
/// timestamp-sorted) with no conflict sibling; all replicas converge.
async fn run_s4_divergent_history_jsonl_merges(cloud: TestCloud) {
    let a = Machine::new("A");
    let b = Machine::new("B");

    let l100 = r#"{"ts":100,"text":"first prompt"}"#;
    let l150 = r#"{"ts":150,"text":"from B"}"#;
    let l200 = r#"{"ts":200,"text":"from A"}"#;

    a.seed(".codex/history.jsonl", &format!("{}\n", l100));
    a.push_all(&cloud).await.expect("A base push");
    b.pull(&cloud).await.expect("B pull base");

    a.seed(".codex/history.jsonl", &format!("{}\n{}\n", l100, l200));
    a.push_all(&cloud).await.expect("A push");

    b.seed(".codex/history.jsonl", &format!("{}\n{}\n", l100, l150));
    b.push_all(&cloud).await.expect("B push merges");

    let merged = format!("{}\n{}\n{}\n", l100, l150, l200);
    assert_eq!(b.read(".codex/history.jsonl"), merged);
    assert_eq!(
        cloud.manifest_file_sha(".codex", ".codex/history.jsonl"),
        Some(crate::sha256_bytes(merged.as_bytes()))
    );
    // No conflict sibling for merge-driver files.
    assert!(!b.list(".codex").iter().any(|p| p.contains("sync-conflict")));

    a.pull(&cloud).await.expect("A pull merged");
    assert_eq!(a.read(".codex/history.jsonl"), merged);
}

#[tokio::test]
async fn s4_divergent_history_jsonl_merges() {
    let _env = harness::lock_env().await;
    run_s4_divergent_history_jsonl_merges(TestCloud::start().await).await;
}

#[tokio::test]
async fn s4_divergent_history_jsonl_merges_local() {
    let _env = harness::lock_env().await;
    run_s4_divergent_history_jsonl_merges(TestCloud::start_local().await).await;
}

/// S5 shared setup: machine A with a published base generation.
async fn s5_setup(cloud: &TestCloud) -> (Machine, String, String) {
    let a = Machine::new("A");
    a.seed(".codex/memories/base.md", "base\n");
    a.push(cloud, &[".codex"]).await.expect("A base push");
    let profile_id = cloud.profile_for_root(".codex");
    let head_key = format!("{}/_head.json", profile_id);
    (a, profile_id, head_key)
}

/// S5 shared verification: after losing the head CAS to the external commit
/// once, A rebased onto the winner and published the union.
async fn s5_verify_rebased(cloud: &TestCloud, a: &Machine) {
    // Generations: 1 (base) -> 2 (external winner) -> 3 (A's rebase).
    let head = cloud.head(".codex");
    assert_eq!(head.generation, 3);
    let manifest = cloud.manifest(".codex");
    assert!(manifest.files.contains_key(".codex/memories/a-side.md"));
    assert!(manifest.files.contains_key(".codex/memories/b-side.md"));
    // The union applied the winner's file locally during the rebase.
    assert_eq!(a.read(".codex/memories/b-side.md"), "from B\n");

    // The published chain links every winner: A's rebase sits on top of the
    // external commit. The lost attempt's commit object also exists in
    // _commits/ (written before its CAS failed) but is not in the chain.
    let chain = cloud.commit_chain(".codex");
    let generations: Vec<u64> = chain.iter().map(|c| c.generation).collect();
    assert_eq!(generations, vec![3, 2, 1, 0]);
    assert_eq!(chain[1].actor_name, "machine-b");
    assert_eq!(
        cloud.commit_records(".codex").len(),
        5,
        "4 published + 1 orphan"
    );

    // The lost attempt's staged batch remains an unpublished orphan.
    let batches = cloud.upload_batches(".codex");
    let staged = batches.iter().filter(|(_, s)| s == "staged").count();
    let committed = batches.iter().filter(|(_, s)| s == "committed").count();
    assert_eq!((staged, committed), (1, 2), "batches: {:?}", batches);
}

/// S5 — lost head CAS on the S3 backend: a competing commit lands between
/// A's head read and its publish. A must observe the 412, rebase onto the
/// winner's generation, union in its files, and publish the next generation.
#[tokio::test]
async fn s5_head_cas_race_rebases_and_republishes() {
    let _env = harness::lock_env().await;
    let cloud = TestCloud::start().await;
    let (a, profile_id, head_key) = s5_setup(&cloud).await;

    // Arm: when A's conditional head PUT arrives, an "external machine"
    // publishes first — A's CAS then fails against the moved head.
    let bucket_dir = cloud.bucket_dir();
    let hook_profile = profile_id.clone();
    cloud.stub().add_conditional_put_hook(
        &head_key,
        1,
        HookAction::RunBefore(Box::new(move |_root| {
            publish_external_commit(
                &bucket_dir,
                &hook_profile,
                &[(".codex/memories/b-side.md", b"from B\n" as &[u8])],
                "machine-b",
            );
        })),
    );

    a.seed(".codex/memories/a-side.md", "from A\n");
    a.push(&cloud, &[".codex"])
        .await
        .expect("A push after race");

    s5_verify_rebased(&cloud, &a).await;

    // The race is visible in the wire log: the publish CAS fails once, then
    // the rebased retry lands.
    let head_puts: Vec<u16> = cloud
        .stub()
        .requests()
        .iter()
        .filter(|r| r.method == "PUT" && r.key == head_key && r.conditional)
        .map(|r| r.status)
        .collect();
    assert_eq!(
        head_puts,
        vec![200, 200, 412, 200],
        "create CAS, base publish CAS, lost CAS, rebased CAS"
    );
}

/// S5-local — the same lost head CAS on the local-folder backend, injected
/// through the local store's test hook instead of the HTTP stub.
#[tokio::test]
async fn s5_head_cas_race_rebases_and_republishes_local() {
    let _env = harness::lock_env().await;
    let cloud = TestCloud::start_local().await;
    let (a, profile_id, head_key) = s5_setup(&cloud).await;

    let bucket_dir = cloud.bucket_dir();
    let hook_profile = profile_id.clone();
    let mut fired = false;
    let _hook = LocalCasHookGuard::set(move |key: &str| {
        if fired || key != head_key {
            return;
        }
        fired = true;
        publish_external_commit(
            &bucket_dir,
            &hook_profile,
            &[(".codex/memories/b-side.md", b"from B\n" as &[u8])],
            "machine-b",
        );
    });

    a.seed(".codex/memories/a-side.md", "from A\n");
    a.push(&cloud, &[".codex"])
        .await
        .expect("A push after race");

    s5_verify_rebased(&cloud, &a).await;
}

/// S5b — the cloud keeps moving through all three attempts: push gives up
/// cleanly and the winner's head survives untouched. (S3-only wiring; the
/// retry loop above the Store layer is backend-agnostic.)
#[tokio::test]
async fn s5b_push_gives_up_when_cloud_keeps_changing() {
    let _env = harness::lock_env().await;
    let cloud = TestCloud::start().await;
    let (a, profile_id, head_key) = s5_setup(&cloud).await;

    let bucket_dir = cloud.bucket_dir();
    let hook_profile = profile_id.clone();
    let mut round = 0u32;
    cloud.stub().add_conditional_put_hook(
        &head_key,
        3,
        HookAction::RunBefore(Box::new(move |_root| {
            round += 1;
            let content = format!("hot v{}\n", round);
            publish_external_commit(
                &bucket_dir,
                &hook_profile,
                &[(".codex/memories/hot.md", content.as_bytes())],
                "machine-b",
            );
        })),
    );

    a.seed(".codex/memories/a-side.md", "from A\n");
    let err = a
        .push(&cloud, &[".codex"])
        .await
        .expect_err("push must give up after 3 lost races");
    assert!(err.contains("kept changing"), "{}", err);

    // The winner's state is intact; A's file never published.
    let head = cloud.head(".codex");
    assert_eq!(head.generation, 4, "base + 3 external commits");
    let manifest = cloud.manifest(".codex");
    assert!(!manifest.files.contains_key(".codex/memories/a-side.md"));
    assert_eq!(
        cloud.manifest_file_sha(".codex", ".codex/memories/hot.md"),
        Some(crate::sha256_bytes(b"hot v3\n"))
    );
}

/// S6 — ambiguous publish: the head write lands but the response times out.
/// The pusher re-reads the head, recognizes its own commit, and reports
/// success without writing twice. (S3-only: the local store cannot time out.)
#[tokio::test]
async fn s6_ambiguous_head_publish_resolves_as_written() {
    let _env = harness::lock_env().await;
    let cloud = TestCloud::start().await;
    let (a, _profile_id, head_key) = s5_setup(&cloud).await;

    let _timeout = TimeoutOverride::set_ms(700);
    cloud.stub().add_conditional_put_hook(
        &head_key,
        1,
        HookAction::StallAfterWrite(std::time::Duration::from_millis(2500)),
    );

    a.seed(".codex/memories/a-side.md", "from A\n");
    a.push(&cloud, &[".codex"])
        .await
        .expect("ambiguous push succeeds");

    let head = cloud.head(".codex");
    assert_eq!(head.generation, 2);
    let manifest = cloud.manifest(".codex");
    assert_eq!(manifest.commit_id, head.commit_id);
    assert!(manifest.files.contains_key(".codex/memories/a-side.md"));

    // Exactly one CAS attempt for generation 2 — the write was applied once
    // and resolved by re-reading, not by retrying the PUT.
    let cas_puts = cloud
        .stub()
        .requests()
        .iter()
        .filter(|r| r.method == "PUT" && r.key == head_key && r.conditional)
        .count();
    assert_eq!(
        cas_puts, 3,
        "create CAS + base publish CAS + one ambiguous publish CAS"
    );
}

/// S7 — a store that ignores conditional writes: the probe detects it, the
/// remote is marked single-writer, and heads publish unconditionally.
/// (S3-only: local mode never probes — its CAS support is intrinsic.)
#[tokio::test]
async fn s7_single_writer_fallback() {
    let _env = harness::lock_env().await;
    let cloud = TestCloud::start().await;
    cloud.stub().set_ignore_conditions(true);
    let d = Machine::new("D");

    d.seed(".codex/memories/notes.md", "v1\n");
    d.push(&cloud, &[".codex"]).await.expect("first push");
    assert_eq!(
        d.saved_probe(&cloud),
        Some(false),
        "probe must detect the store ignores conditions"
    );
    assert_eq!(cloud.head(".codex").generation, 1);

    // Different length than v1: a same-size edit in the same mtime second
    // would hit the baseline's stat fast path and look unchanged.
    d.seed(".codex/memories/notes.md", "v2, longer than v1\n");
    d.push(&cloud, &[".codex"]).await.expect("second push");
    assert_eq!(cloud.head(".codex").generation, 2);

    // Every head write went out unconditionally (last-writer-wins mode).
    let conditional_head_puts = cloud
        .stub()
        .requests()
        .iter()
        .filter(|r| r.method == "PUT" && r.key.ends_with("_head.json") && r.conditional)
        .count();
    assert_eq!(conditional_head_puts, 0);
}

/// S8 — late joiner: C converges purely from cloud state after A and B's
/// conflict dance, and a follow-up push publishes nothing.
async fn run_s8_late_joiner_converges(cloud: TestCloud) {
    let a = Machine::new("A");
    let b = Machine::new("B");
    let c = Machine::new("C");

    a.seed(".codex/memories/notes.md", "base v0\n");
    a.seed(".claude/CLAUDE.md", "claude v1\n");
    a.push_all(&cloud).await.expect("A base push");
    b.pull(&cloud).await.expect("B pull");

    a.seed(".codex/memories/notes.md", "from A\n");
    a.push_all(&cloud).await.expect("A push vA");
    b.seed(".codex/memories/notes.md", "from B\n");
    b.push_all(&cloud).await.expect("B push vB");

    // C joins with empty agent dirs and only bucket credentials.
    c.pull(&cloud).await.expect("C pull");
    let conflict_rel = crate::conflict_copy_rel(
        ".codex/memories/notes.md",
        &crate::sha256_bytes(b"from A\n"),
    );
    assert_eq!(c.read(".codex/memories/notes.md"), "from B\n");
    assert_eq!(c.read(&conflict_rel), "from A\n");
    assert_eq!(c.read(".claude/CLAUDE.md"), "claude v1\n");

    let codex_gen = cloud.head(".codex").generation;
    let claude_gen = cloud.head(".claude").generation;
    let result = c.push_all(&cloud).await.expect("C push");
    assert!(result.message.contains("up to date"), "{}", result.message);
    assert_eq!(cloud.head(".codex").generation, codex_gen);
    assert_eq!(cloud.head(".claude").generation, claude_gen);
}

#[tokio::test]
async fn s8_late_joiner_converges() {
    let _env = harness::lock_env().await;
    run_s8_late_joiner_converges(TestCloud::start().await).await;
}

#[tokio::test]
async fn s8_late_joiner_converges_local() {
    let _env = harness::lock_env().await;
    run_s8_late_joiner_converges(TestCloud::start_local().await).await;
}

/// S9 — detected corruption: a manifest that no longer matches the head's
/// sha fails the pull loudly and applies nothing.
async fn run_s9_manifest_corruption_fails_pull(cloud: TestCloud) {
    let a = Machine::new("A");
    let b = Machine::new("B");

    a.seed(".codex/memories/notes.md", "v1\n");
    a.push(&cloud, &[".codex"]).await.expect("A push");

    // Corrupt the manifest the head references.
    let profile_id = cloud.profile_for_root(".codex");
    let head = cloud.head_of(&profile_id).unwrap();
    let manifest_path = cloud
        .bucket_dir()
        .join(&profile_id)
        .join(&head.manifest_key);
    let mut bytes = std::fs::read(&manifest_path).unwrap();
    bytes.push(b' ');
    std::fs::write(&manifest_path, bytes).unwrap();

    let err = b.pull(&cloud).await.expect_err("pull must fail");
    assert!(err.contains("corruption"), "{}", err);
    assert!(
        b.list(".codex").is_empty(),
        "nothing may be applied from a corrupt manifest"
    );
}

#[tokio::test]
async fn s9_manifest_corruption_fails_pull() {
    let _env = harness::lock_env().await;
    run_s9_manifest_corruption_fails_pull(TestCloud::start().await).await;
}

#[tokio::test]
async fn s9_manifest_corruption_fails_pull_local() {
    let _env = harness::lock_env().await;
    run_s9_manifest_corruption_fails_pull(TestCloud::start_local().await).await;
}

/// Local-store CAS semantics: 8 concurrent conditional writers against one
/// key — exactly one wins the lock-file CAS; put-if-absent honors existence.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_store_cas_single_winner() {
    let tmp = tempfile::tempdir().unwrap();
    let store = crate::Store::Local {
        root: tmp.path().to_path_buf(),
    };
    store.put("p/_head.json", b"gen0".to_vec()).await.unwrap();
    let etag = crate::sha256_bytes(b"gen0");

    let mut set = tokio::task::JoinSet::new();
    for i in 0..8 {
        let store = store.clone();
        let etag = etag.clone();
        set.spawn(async move {
            store
                .put_conditional(
                    "p/_head.json",
                    format!("winner-{}", i).into_bytes(),
                    &crate::PutCondition::IfMatch(etag),
                )
                .await
                .unwrap()
        });
    }
    let (mut written, mut failed) = (0, 0);
    while let Some(outcome) = set.join_next().await {
        match outcome.unwrap() {
            crate::PutOutcome::Written => written += 1,
            crate::PutOutcome::PreconditionFailed => failed += 1,
            crate::PutOutcome::Ambiguous => panic!("local store is never ambiguous"),
        }
    }
    assert_eq!((written, failed), (1, 7));
    let (data, _) = store.get("p/_head.json").await.unwrap();
    assert!(String::from_utf8(data).unwrap().starts_with("winner-"));

    // Put-if-absent: taken key refuses, fresh key lands.
    let absent = store
        .put_conditional(
            "p/_head.json",
            b"x".to_vec(),
            &crate::PutCondition::IfAbsent,
        )
        .await
        .unwrap();
    assert!(matches!(absent, crate::PutOutcome::PreconditionFailed));
    let fresh = store
        .put_conditional(
            "q/_head.json",
            b"x".to_vec(),
            &crate::PutCondition::IfAbsent,
        )
        .await
        .unwrap();
    assert!(matches!(fresh, crate::PutOutcome::Written));
}

/// S10 — switching the sync destination: stale profile links from the
/// previous destination fall through to rediscovery instead of failing with
/// "no _head.json" (regression: user switched R2 → local folder and every
/// push died on the R2 profile id).
#[tokio::test]
async fn s10_destination_switch_relinks_profiles() {
    let _env = harness::lock_env().await;
    let cloud_s3 = TestCloud::start().await;
    let cloud_local = TestCloud::start_local().await;
    let a = Machine::new("A");

    a.seed(".codex/memories/notes.md", "v1\n");
    a.push(&cloud_s3, &[".codex"]).await.expect("push to S3");
    let s3_profile = cloud_s3.profile_for_root(".codex");

    // Same machine, destination switched to an empty local folder — the
    // saved config still holds the S3 profile link.
    a.push(&cloud_local, &[".codex"])
        .await
        .expect("push to local folder after destination switch");
    let local_profile = cloud_local.profile_for_root(".codex");
    assert_ne!(
        local_profile, s3_profile,
        "fresh profile in the new destination"
    );
    assert_eq!(cloud_local.head(".codex").generation, 1);
    assert_eq!(
        cloud_local.manifest_file_sha(".codex", ".codex/memories/notes.md"),
        Some(crate::sha256_bytes(b"v1\n"))
    );

    // Switching back relinks to the original profile via discovery — no
    // duplicate profile appears in the S3 bucket.
    a.seed(".codex/memories/notes.md", "v2 back on s3\n");
    a.push(&cloud_s3, &[".codex"])
        .await
        .expect("push back to S3");
    assert_eq!(cloud_s3.profile_for_root(".codex"), s3_profile);
    assert_eq!(cloud_s3.head(".codex").generation, 2);
    assert_eq!(cloud_s3.profiles_by_root().len(), 1);
}

/// Saving settings preserves per-storage state only while that storage's
/// identity is unchanged; an identity change clears its links' resolved
/// cloud side and its probe result — other storages untouched.
#[tokio::test]
async fn save_sync_config_scopes_state_to_storage_identity() {
    let _env = harness::lock_env().await;
    let m = Machine::new("M");
    m.activate();

    let cloud_link = |id: &str| crate::ProfileLink {
        root: ".codex".to_string(),
        profile_id: id.to_string(),
        profile_label: "Codex".to_string(),
        actor_name: "alice".to_string(),
        machine_name: "mbp".to_string(),
        pinned: false,
    };
    let storage = |id: &str, bucket: &str| crate::StorageConfig {
        id: id.to_string(),
        name: id.to_string(),
        kind: "s3".to_string(),
        bucket: bucket.to_string(),
        access_key_id: "k".to_string(),
        secret_access_key: "s".to_string(),
        s3_endpoint: "https://acc.r2.cloudflarestorage.com".to_string(),
        supports_conditional_writes: Some(true),
        ..Default::default()
    };
    let mut base = crate::default_sync_config();
    base.storages = vec![storage("s-a", "bucket-a"), storage("s-b", "bucket-b")];
    base.links = vec![
        crate::SyncLink {
            profile: "codex".to_string(),
            storage: "s-a".to_string(),
            cloud: cloud_link("1488fce174063316d1f61de10ed649ac"),
        },
        crate::SyncLink {
            profile: "codex".to_string(),
            storage: "s-b".to_string(),
            cloud: cloud_link("77aa0bd541b09a06f652b52a2ad5c0b1"),
        },
    ];
    // Links and probe flags enter the saved config the way the app writes
    // them: persisted directly by push/pull, never through the settings save.
    crate::persist_sync_config(m.handle(), &base).unwrap();

    // Same identities, per-storage fields resubmitted empty by the UI:
    // resolved links and probe results are preserved.
    let mut resubmitted = base.clone();
    for storage in &mut resubmitted.storages {
        storage.supports_conditional_writes = None;
    }
    for link in &mut resubmitted.links {
        link.cloud = crate::ProfileLink::default();
    }
    crate::save_sync_config(m.handle().clone(), resubmitted)
        .await
        .unwrap();
    let saved = m.saved_config();
    assert_eq!(saved.links.len(), 2);
    assert!(saved
        .links
        .iter()
        .all(|l| !l.cloud.profile_id.is_empty()));
    assert!(saved
        .storages
        .iter()
        .all(|s| s.supports_conditional_writes == Some(true)));

    // One storage's identity changes (new bucket): only its state drops,
    // even though the UI submitted the old resolved links along with it.
    let mut switched = saved.clone();
    switched.storages[0].bucket = "bucket-elsewhere".to_string();
    crate::save_sync_config(m.handle().clone(), switched)
        .await
        .unwrap();
    let saved = m.saved_config();
    let link_a = saved.links.iter().find(|l| l.storage == "s-a").unwrap();
    let link_b = saved.links.iter().find(|l| l.storage == "s-b").unwrap();
    assert!(
        link_a.cloud.profile_id.is_empty(),
        "resolved link must not cross storage identities"
    );
    assert_eq!(
        link_b.cloud.profile_id, "77aa0bd541b09a06f652b52a2ad5c0b1",
        "the untouched storage keeps its link"
    );
    assert_eq!(saved.storages[0].supports_conditional_writes, None);
    assert_eq!(saved.storages[1].supports_conditional_writes, Some(true));
}

/// S11 — sync-link local side: machine B keeps `.codex` at a custom mount.
/// Both machines share one profile; files land under each machine's own
/// mount; union and conflict semantics are unaffected by the mapping.
async fn run_s11_custom_local_root_roundtrip(cloud: TestCloud) {
    let a = Machine::new("A");
    let b = Machine::with_codex_root("B");
    let b_root = b.mount_dir(".codex").unwrap();

    a.seed(".codex/memories/notes.md", "base v0\n");
    a.push(&cloud, &[".codex"]).await.expect("A push");

    // B pulls into its custom mount — the default ~/.codex stays empty.
    b.pull(&cloud).await.expect("B pull");
    assert_eq!(b.read(".codex/memories/notes.md"), "base v0\n");
    assert!(b.path(".codex/memories/notes.md").starts_with(&b_root));
    assert!(
        !b.home().join(".codex/memories/notes.md").exists(),
        "default location must stay untouched"
    );
    // One shared codex profile — the custom mount does not fork the cloud
    // side. (B's pull may also auto-create an empty .claude profile since
    // its default dir exists; that's the ordinary per-root behavior.)
    let a_codex = a.saved_link(&cloud, ".codex").map(|c| c.profile_id);
    let b_codex = b.saved_link(&cloud, ".codex").map(|c| c.profile_id);
    assert!(a_codex.is_some());
    assert_eq!(a_codex, b_codex, "both machines share the codex profile");

    // B edits under the custom mount and pushes; A receives it.
    b.seed(".codex/memories/notes.md", "v1 from B's custom mount\n");
    b.push(&cloud, &[".codex"]).await.expect("B push");
    assert_eq!(cloud.head(".codex").generation, 2);
    a.pull(&cloud).await.expect("A pull");
    assert_eq!(
        a.read(".codex/memories/notes.md"),
        "v1 from B's custom mount\n"
    );

    // Divergent edits still conflict deterministically across mounts.
    a.seed(".codex/memories/notes.md", "from A\n");
    a.push(&cloud, &[".codex"]).await.expect("A push vA");
    b.seed(".codex/memories/notes.md", "from B\n");
    b.push(&cloud, &[".codex"]).await.expect("B push vB");
    let conflict_rel = crate::conflict_copy_rel(
        ".codex/memories/notes.md",
        &crate::sha256_bytes(b"from A\n"),
    );
    assert_eq!(b.read(".codex/memories/notes.md"), "from B\n");
    assert_eq!(b.read(&conflict_rel), "from A\n");
    assert!(b.path(&conflict_rel).starts_with(&b_root));
}

#[tokio::test]
async fn s11_custom_local_root_roundtrip() {
    let _env = harness::lock_env().await;
    run_s11_custom_local_root_roundtrip(TestCloud::start().await).await;
}

#[tokio::test]
async fn s11_custom_local_root_roundtrip_local() {
    let _env = harness::lock_env().await;
    run_s11_custom_local_root_roundtrip(TestCloud::start_local().await).await;
}

/// S13 — pinned cloud prefixes: created literally at the chosen name, and
/// recreated at that exact name if the profile vanishes — never rediscovered
/// (the unpinned fallback is S10's territory).
async fn run_s13_pinned_prefix_created_and_recreated(cloud: TestCloud) {
    let a = Machine::new("A");
    a.pin_cloud_prefix(".codex", "001/.codex");
    a.seed(".codex/memories/notes.md", "v1\n");
    a.push(&cloud, &[".codex"])
        .await
        .expect("push to pinned prefix");

    // The profile lives literally at bucket/001/.codex/.
    assert!(cloud.bucket_dir().join("001/.codex/_head.json").exists());
    let head = cloud.head_of("001/.codex").expect("head at pinned path");
    assert_eq!(head.root, ".codex");
    assert_eq!(head.generation, 1);
    // The pinned path is the ONLY codex profile — no hex profile appeared.
    assert_eq!(
        cloud.profiles_by_root().get(".codex").map(String::as_str),
        Some("001/.codex")
    );

    // Wipe the profile: a pinned link recreates at the SAME name.
    std::fs::remove_dir_all(cloud.bucket_dir().join("001")).unwrap();
    a.seed(".codex/memories/notes.md", "v2 after recreate\n");
    a.push(&cloud, &[".codex"])
        .await
        .expect("recreate at pinned path");
    assert!(cloud.bucket_dir().join("001/.codex/_head.json").exists());
    let head = cloud.head_of("001/.codex").expect("recreated head");
    assert_eq!(head.generation, 1, "fresh profile republished from local");
    assert_eq!(
        cloud.profiles_by_root().get(".codex").map(String::as_str),
        Some("001/.codex")
    );
}

#[tokio::test]
async fn s13_pinned_prefix_created_and_recreated() {
    let _env = harness::lock_env().await;
    run_s13_pinned_prefix_created_and_recreated(TestCloud::start().await).await;
}

#[tokio::test]
async fn s13_pinned_prefix_created_and_recreated_local() {
    let _env = harness::lock_env().await;
    run_s13_pinned_prefix_created_and_recreated(TestCloud::start_local().await).await;
}

/// S14 — nested sync-link profiles are auto-discovered: B links `001/.codex`
/// purely by root kind, never knowing the name in advance.
async fn run_s14_nested_profile_discovery(cloud: TestCloud) {
    let a = Machine::new("A");
    a.pin_cloud_prefix(".codex", "001/.codex");
    a.seed(".codex/memories/notes.md", "from pinned\n");
    a.push(&cloud, &[".codex"])
        .await
        .expect("A push to pinned prefix");

    let b = Machine::new("B");
    b.pull(&cloud)
        .await
        .expect("B pull discovers the nested profile");
    assert_eq!(b.read(".codex/memories/notes.md"), "from pinned\n");
    let b_link = b
        .saved_link(&cloud, ".codex")
        .expect("B linked a codex profile");
    assert_eq!(b_link.profile_id, "001/.codex");
    assert!(!b_link.pinned, "auto-link is not a pin");

    // Roundtrip continues normally across the nested prefix.
    b.seed(".codex/memories/notes.md", "answer from B\n");
    b.push(&cloud, &[".codex"]).await.expect("B push");
    a.pull(&cloud).await.expect("A pull");
    assert_eq!(a.read(".codex/memories/notes.md"), "answer from B\n");
    assert_eq!(cloud.head_of("001/.codex").unwrap().generation, 2);
}

#[tokio::test]
async fn s14_nested_profile_discovery() {
    let _env = harness::lock_env().await;
    run_s14_nested_profile_discovery(TestCloud::start().await).await;
}

#[tokio::test]
async fn s14_nested_profile_discovery_local() {
    let _env = harness::lock_env().await;
    run_s14_nested_profile_discovery(TestCloud::start_local().await).await;
}

/// S12 — the full sync-link story, both plan examples verbatim:
/// `~/.codex ⇄ 001/.codex` on A, `<custom>/.codex ⇄ 001/.codex` on B.
async fn run_s12_sync_links_full_story(cloud: TestCloud) {
    let a = Machine::new("A");
    a.set_sync_link(".codex", "", "001/.codex")
        .await
        .expect("A set link");
    a.seed(".codex/memories/notes.md", "v1 from A\n");
    a.push(&cloud, &[".codex"]).await.expect("A push");
    assert!(cloud.bucket_dir().join("001/.codex/_head.json").exists());

    let b = Machine::with_codex_root("B");
    let b_root = b.mount_dir(".codex").unwrap();
    b.set_sync_link(".codex", &b_root.to_string_lossy(), "001/.codex")
        .await
        .expect("B set link");
    b.pull(&cloud).await.expect("B pull");
    assert_eq!(b.read(".codex/memories/notes.md"), "v1 from A\n");
    assert!(b.path(".codex/memories/notes.md").starts_with(&b_root));

    // Divergent edits: the standard conflict sibling, at the pinned prefix,
    // across two differently-mounted machines.
    a.seed(".codex/memories/notes.md", "from A\n");
    a.push(&cloud, &[".codex"]).await.expect("A push vA");
    b.seed(".codex/memories/notes.md", "from B\n");
    b.push(&cloud, &[".codex"]).await.expect("B push vB");
    let conflict_rel = crate::conflict_copy_rel(
        ".codex/memories/notes.md",
        &crate::sha256_bytes(b"from A\n"),
    );
    assert_eq!(b.read(&conflict_rel), "from A\n");
    assert_eq!(cloud.head_of("001/.codex").unwrap().generation, 3);

    a.pull(&cloud).await.expect("A pull");
    assert_eq!(a.read(".codex/memories/notes.md"), "from B\n");
    assert_eq!(a.read(&conflict_rel), "from A\n");
}

#[tokio::test]
async fn s12_sync_links_full_story() {
    let _env = harness::lock_env().await;
    run_s12_sync_links_full_story(TestCloud::start().await).await;
}

#[tokio::test]
async fn s12_sync_links_full_story_local() {
    let _env = harness::lock_env().await;
    run_s12_sync_links_full_story(TestCloud::start_local().await).await;
}

/// Sync-link state lands in the right scopes: the local mount is
/// per-machine (survives storage identity changes), and so is a pinned
/// cloud prefix — user intent survives while resolved metadata clears
/// (PLAN_MULTI_STORAGE.md §3.2); empty values revert each side to default.
#[tokio::test]
async fn sync_link_state_scopes_correctly() {
    let _env = harness::lock_env().await;
    let m = Machine::new("M");
    m.activate();

    let mut base = crate::default_sync_config();
    base.storages = vec![crate::StorageConfig {
        id: "s-a".to_string(),
        name: "A".to_string(),
        kind: "s3".to_string(),
        bucket: "bucket".to_string(),
        access_key_id: "k".to_string(),
        secret_access_key: "s".to_string(),
        s3_endpoint: "https://acc.r2.cloudflarestorage.com".to_string(),
        ..Default::default()
    }];
    base.links = vec![crate::SyncLink {
        profile: "codex".to_string(),
        storage: "s-a".to_string(),
        cloud: crate::ProfileLink::default(),
    }];
    crate::persist_sync_config(m.handle(), &base).unwrap();

    m.set_sync_link(".codex", "/scratch/.codex", "001/.codex")
        .await
        .unwrap();
    let saved = m.saved_config();
    let codex_path = |config: &crate::SyncConfig| {
        config
            .local_profiles
            .iter()
            .find(|p| p.id == "codex")
            .unwrap()
            .path
            .clone()
    };
    assert_eq!(codex_path(&saved), "/scratch/.codex");
    let link = saved.links.iter().find(|l| l.profile == "codex").unwrap();
    assert_eq!(link.cloud.profile_id, "001/.codex");
    assert!(link.cloud.pinned);

    // Invalid mounts are rejected before anything persists.
    assert!(m
        .set_sync_link(".codex", "relative/path", "001/.codex")
        .await
        .is_err());
    assert!(m.set_sync_link(".weird", "", "").await.is_err());
    assert!(m.set_sync_link(".codex", "", "_reserved").await.is_err());

    // Storage identity change (settings save): the mount is per-machine
    // and survives; the pin is user intent and survives too — only the
    // resolved metadata belongs to the old destination.
    let mut switched = m.saved_config();
    switched.storages[0].bucket = "bucket-elsewhere".to_string();
    crate::save_sync_config(m.handle().clone(), switched)
        .await
        .unwrap();
    let saved = m.saved_config();
    assert_eq!(codex_path(&saved), "/scratch/.codex", "mount survives");
    let link = saved.links.iter().find(|l| l.profile == "codex").unwrap();
    assert_eq!(link.cloud.profile_id, "001/.codex", "pin survives");
    assert!(link.cloud.pinned);

    // Empty values revert both sides to defaults.
    m.set_sync_link(".codex", "", "").await.unwrap();
    let saved = m.saved_config();
    assert_eq!(codex_path(&saved), "");
    assert!(saved.links.iter().all(|l| l.cloud.profile_id.is_empty()));
}

/// S15 — three homes, one bucket: two machines share `001/.codex` (one on a
/// custom mount), a third keeps its own `002/.codex`. Generations and
/// manifests never cross between the profiles.
async fn run_s15_three_homes_one_bucket(cloud: TestCloud) {
    let a = Machine::new("A");
    a.set_sync_link(".codex", "", "001/.codex")
        .await
        .expect("A link");
    a.seed(".codex/memories/notes.md", "shared v1\n");
    a.push(&cloud, &[".codex"]).await.expect("A push");

    let b = Machine::new("B");
    let b_dir = b.home().join("scratch/.codex");
    let b = b.mount(".codex", b_dir.clone());
    b.set_sync_link(".codex", &b_dir.to_string_lossy(), "001/.codex")
        .await
        .expect("B link");
    b.pull(&cloud).await.expect("B pull");
    assert_eq!(b.read(".codex/memories/notes.md"), "shared v1\n");

    let c = Machine::new("C");
    c.set_sync_link(".codex", "", "002/.codex")
        .await
        .expect("C link");
    c.seed(".codex/memories/c-notes.md", "c only v1\n");
    c.push(&cloud, &[".codex"]).await.expect("C push");

    // Exactly the two named profiles hold .codex — no hex strays.
    assert_eq!(
        cloud.profiles_for_root(".codex"),
        vec!["001/.codex".to_string(), "002/.codex".to_string()]
    );

    // C's push did not move 001; B's push does not move 002.
    assert_eq!(cloud.head_of("001/.codex").unwrap().generation, 1);
    b.seed(".codex/memories/notes.md", "shared v2 from B\n");
    b.push(&cloud, &[".codex"]).await.expect("B push");
    assert_eq!(cloud.head_of("001/.codex").unwrap().generation, 2);
    assert_eq!(cloud.head_of("002/.codex").unwrap().generation, 1);

    // Manifests are disjoint.
    let m001 = cloud.manifest_of("001/.codex");
    let m002 = cloud.manifest_of("002/.codex");
    assert!(m001.files.contains_key(".codex/memories/notes.md"));
    assert!(!m001.files.contains_key(".codex/memories/c-notes.md"));
    assert!(m002.files.contains_key(".codex/memories/c-notes.md"));
    assert!(!m002.files.contains_key(".codex/memories/notes.md"));

    // A converges with B; C's file leaks nowhere.
    a.pull(&cloud).await.expect("A pull");
    harness::assert_converged(&[&a, &b], &[".codex/memories/notes.md"]);
    assert!(!a.path(".codex/memories/c-notes.md").exists());
    assert!(!c.path(".codex/memories/notes.md").exists());
}

#[tokio::test]
async fn s15_three_homes_one_bucket() {
    let _env = harness::lock_env().await;
    run_s15_three_homes_one_bucket(TestCloud::start().await).await;
}

#[tokio::test]
async fn s15_three_homes_one_bucket_local() {
    let _env = harness::lock_env().await;
    run_s15_three_homes_one_bucket(TestCloud::start_local().await).await;
}

/// S16 — namespace pairs and the multi-match guard: `001/.codex` +
/// `001/.claude` (both custom-mounted) auto-link while unique per root; a
/// second codex profile forces fresh machines to pin explicitly.
async fn run_s16_namespace_pairs_and_multi_match(cloud: TestCloud) {
    let a = Machine::new("A");
    let a_codex = a.home().join("mounts/codex");
    let a_claude = a.home().join("mounts/claude");
    let a = a
        .mount(".codex", a_codex.clone())
        .mount(".claude", a_claude.clone());
    a.set_sync_link(".codex", &a_codex.to_string_lossy(), "001/.codex")
        .await
        .expect("A codex link");
    a.set_sync_link(".claude", &a_claude.to_string_lossy(), "001/.claude")
        .await
        .expect("A claude link");
    a.seed(".codex/memories/notes.md", "codex v1\n");
    a.seed(".claude/CLAUDE.md", "claude v1\n");
    a.push_all(&cloud).await.expect("A push");
    assert!(cloud.bucket_dir().join("001/.codex/_head.json").exists());
    assert!(cloud.bucket_dir().join("001/.claude/_head.json").exists());

    // D auto-links both roots while exactly one profile per root exists.
    let d = Machine::new("D");
    d.pull(&cloud).await.expect("D pull");
    assert_eq!(d.read(".codex/memories/notes.md"), "codex v1\n");
    assert_eq!(d.read(".claude/CLAUDE.md"), "claude v1\n");
    let d_codex = d.saved_link(&cloud, ".codex").unwrap();
    assert_eq!(d_codex.profile_id, "001/.codex");
    assert!(!d_codex.pinned, "auto-link is not a pin");

    // A second codex profile appears...
    let c = Machine::new("C");
    c.set_sync_link(".codex", "", "002/.codex")
        .await
        .expect("C link");
    c.seed(".codex/memories/c-notes.md", "c v1\n");
    c.push(&cloud, &[".codex"]).await.expect("C push");

    // ...and a fresh machine can no longer auto-link.
    let e = Machine::new("E");
    let err = e.pull(&cloud).await.expect_err("multi-match must fail");
    assert!(err.contains("pin one explicitly"), "{}", err);
    assert!(
        !e.path(".codex/memories/notes.md").exists(),
        "nothing may be applied on a failed pull"
    );

    // An explicit pin resolves it — to C's profile, not A's.
    e.set_sync_link(".codex", "", "002/.codex")
        .await
        .expect("E pin");
    e.pull(&cloud).await.expect("E pull");
    assert_eq!(e.read(".codex/memories/c-notes.md"), "c v1\n");
    assert!(!e.path(".codex/memories/notes.md").exists());
}

#[tokio::test]
async fn s16_namespace_pairs_and_multi_match() {
    let _env = harness::lock_env().await;
    run_s16_namespace_pairs_and_multi_match(TestCloud::start().await).await;
}

#[tokio::test]
async fn s16_namespace_pairs_and_multi_match_local() {
    let _env = harness::lock_env().await;
    run_s16_namespace_pairs_and_multi_match(TestCloud::start_local().await).await;
}

/// S17 — mount relocation. With moved files nothing republishes (baselines
/// are sha-based, so fresh mtimes fall through to the hash). With an empty
/// new mount, the union restores the tree into it and deletes nothing.
async fn run_s17_mount_relocation(cloud: TestCloud) {
    let mut a = Machine::new("A");
    a.seed(".codex/memories/notes.md", "v1\n");
    a.seed(".codex/history.jsonl", "{\"ts\":100,\"text\":\"hi\"}\n");
    a.push(&cloud, &[".codex"]).await.expect("A push");
    let gen_before = cloud.head(".codex").generation;

    // Variant 1 — files move with the mount (fresh mtimes force the hash
    // path): the next push publishes nothing new.
    let scratch = a.home().join("scratch/.codex");
    a.relocate(".codex", scratch.clone(), true);
    let result = a.push(&cloud, &[".codex"]).await.expect("push after move");
    assert!(result.message.contains("up to date"), "{}", result.message);
    assert_eq!(cloud.head(".codex").generation, gen_before);
    assert!(!a.list(".codex").iter().any(|p| p.contains("sync-conflict")));

    // Variant 2 — empty new mount: the push must not delete anything
    // cloud-side; the union restores the full tree into the new location.
    let fresh = a.home().join("fresh/.codex");
    a.relocate(".codex", fresh.clone(), false);
    let result = a
        .push(&cloud, &[".codex"])
        .await
        .expect("push from empty mount");
    assert!(result.message.contains("up to date"), "{}", result.message);
    assert_eq!(
        cloud.head(".codex").generation,
        gen_before,
        "nothing deleted or republished"
    );
    assert_eq!(a.read(".codex/memories/notes.md"), "v1\n");
    assert!(a.path(".codex/memories/notes.md").starts_with(&fresh));
    // The abandoned location keeps its copy.
    assert_eq!(
        std::fs::read_to_string(scratch.join("memories/notes.md")).unwrap(),
        "v1\n"
    );

    // And a pull afterwards is a no-op.
    a.pull(&cloud).await.expect("pull");
    assert_eq!(cloud.head(".codex").generation, gen_before);
}

#[tokio::test]
async fn s17_mount_relocation() {
    let _env = harness::lock_env().await;
    run_s17_mount_relocation(TestCloud::start().await).await;
}

#[tokio::test]
async fn s17_mount_relocation_local() {
    let _env = harness::lock_env().await;
    run_s17_mount_relocation(TestCloud::start_local().await).await;
}

/// S18 — mixed link shapes on one machine: pinned + custom-mounted codex
/// beside auto + default-mounted claude; scopes stay separate both ways.
async fn run_s18_mixed_link_shapes(cloud: TestCloud) {
    let a = Machine::new("A");
    let a_codex = a.home().join("work/codex");
    let a = a.mount(".codex", a_codex.clone());
    a.set_sync_link(".codex", &a_codex.to_string_lossy(), "001/.codex")
        .await
        .expect("A link");
    a.seed(".codex/memories/notes.md", "codex v1\n");
    a.seed(".claude/CLAUDE.md", "claude v1\n");
    a.push_all(&cloud).await.expect("A push");

    // Codex landed at the pin; claude got a top-level auto hex profile.
    assert_eq!(
        cloud.profiles_for_root(".codex"),
        vec!["001/.codex".to_string()]
    );
    let claude_profiles = cloud.profiles_for_root(".claude");
    assert_eq!(claude_profiles.len(), 1);
    let claude_id = claude_profiles[0].clone();
    assert!(!claude_id.contains('/'), "auto profile is top-level");
    assert_eq!(claude_id.len(), 32, "auto profile is a hex id");

    // No cross-root leakage in either manifest.
    assert!(!cloud
        .manifest_of("001/.codex")
        .files
        .keys()
        .any(|k| k.starts_with(".claude/")));
    assert!(!cloud
        .manifest_of(&claude_id)
        .files
        .keys()
        .any(|k| k.starts_with(".codex/")));

    // A default-everything machine converges on both roots, both ways.
    let b = Machine::new("B");
    b.pull(&cloud).await.expect("B pull");
    harness::assert_converged(
        &[&a, &b],
        &[".codex/memories/notes.md", ".claude/CLAUDE.md"],
    );

    b.seed(".codex/memories/notes.md", "codex v2 from B\n");
    b.seed(".claude/CLAUDE.md", "claude v2 from B\n");
    b.push_all(&cloud).await.expect("B push");
    a.pull(&cloud).await.expect("A pull");
    harness::assert_converged(
        &[&a, &b],
        &[".codex/memories/notes.md", ".claude/CLAUDE.md"],
    );
}

#[tokio::test]
async fn s18_mixed_link_shapes() {
    let _env = harness::lock_env().await;
    run_s18_mixed_link_shapes(TestCloud::start().await).await;
}

#[tokio::test]
async fn s18_mixed_link_shapes_local() {
    let _env = harness::lock_env().await;
    run_s18_mixed_link_shapes(TestCloud::start_local().await).await;
}

/// S19 — a pin pointing at a prefix holding the other root fails loudly and
/// writes nothing; correcting the pin recovers.
async fn run_s19_wrong_root_pin_fails(cloud: TestCloud) {
    let a = Machine::new("A");
    a.set_sync_link(".codex", "", "001/.codex")
        .await
        .expect("A link");
    a.seed(".codex/memories/notes.md", "v1\n");
    a.push(&cloud, &[".codex"]).await.expect("A push");

    let w = Machine::new("W");
    w.set_sync_link(".claude", "", "001/.codex")
        .await
        .expect("setting is network-free; validation happens at sync time");
    w.seed(".claude/CLAUDE.md", "w claude\n");
    let err = w
        .push(&cloud, &[".claude"])
        .await
        .expect_err("wrong-root pin must fail");
    assert!(err.contains("cannot sync it as .claude"), "{}", err);
    let err = w.pull(&cloud).await.expect_err("pull fails the same way");
    assert!(err.contains("cannot sync it as .claude"), "{}", err);

    // Store unmodified: 001/.codex untouched, no claude profile appeared.
    assert_eq!(cloud.head_of("001/.codex").unwrap().generation, 1);
    assert!(cloud.profiles_for_root(".claude").is_empty());

    // Correcting the pin recovers.
    w.set_sync_link(".claude", "", "001/.claude")
        .await
        .expect("re-pin");
    w.push(&cloud, &[".claude"]).await.expect("push after fix");
    assert_eq!(cloud.head_of("001/.claude").unwrap().generation, 1);
}

#[tokio::test]
async fn s19_wrong_root_pin_fails() {
    let _env = harness::lock_env().await;
    run_s19_wrong_root_pin_fails(TestCloud::start().await).await;
}

#[tokio::test]
async fn s19_wrong_root_pin_fails_local() {
    let _env = harness::lock_env().await;
    run_s19_wrong_root_pin_fails(TestCloud::start_local().await).await;
}

/// S20 — repointing a pin between prefixes and back: per-profile baselines
/// keep every transition conflict-free.
async fn run_s20_repointing_pin(cloud: TestCloud) {
    let a = Machine::new("A");
    a.set_sync_link(".codex", "", "001/.codex")
        .await
        .expect("pin 001");
    a.seed(".codex/memories/notes.md", "v1\n");
    a.seed(".codex/memories/stable.md", "stable\n");
    a.push(&cloud, &[".codex"]).await.expect("push to 001");

    // Repoint to a fresh prefix: the full tree publishes there.
    a.set_sync_link(".codex", "", "002/.codex")
        .await
        .expect("pin 002");
    a.push(&cloud, &[".codex"]).await.expect("push to 002");
    let m002 = cloud.manifest_of("002/.codex");
    assert!(m002.files.contains_key(".codex/memories/notes.md"));
    assert!(m002.files.contains_key(".codex/memories/stable.md"));
    assert_eq!(
        cloud.head_of("001/.codex").unwrap().generation,
        1,
        "001 untouched by the 002 push"
    );

    // Edit, repoint back: the old baseline is still valid — a plain update,
    // zero conflict siblings.
    a.seed(".codex/memories/notes.md", "v2, longer than v1\n");
    a.set_sync_link(".codex", "", "001/.codex")
        .await
        .expect("pin 001 again");
    a.push(&cloud, &[".codex"]).await.expect("push back to 001");
    assert_eq!(cloud.head_of("001/.codex").unwrap().generation, 2);
    let m001 = cloud.manifest_of("001/.codex");
    assert_eq!(
        m001.files[".codex/memories/notes.md"].sha256,
        crate::sha256_bytes(b"v2, longer than v1\n")
    );
    assert!(!m001.files.keys().any(|k| k.contains("sync-conflict")));
    assert!(!a.list(".codex").iter().any(|p| p.contains("sync-conflict")));
}

#[tokio::test]
async fn s20_repointing_pin() {
    let _env = harness::lock_env().await;
    run_s20_repointing_pin(TestCloud::start().await).await;
}

#[tokio::test]
async fn s20_repointing_pin_local() {
    let _env = harness::lock_env().await;
    run_s20_repointing_pin(TestCloud::start_local().await).await;
}

/// Statuses read mounts from the SAVED config: after `set_sync_link`, the
/// matrix labels paths under the custom mount correctly.
#[tokio::test]
async fn statuses_under_custom_mounts() {
    let _env = harness::lock_env().await;
    let cloud = TestCloud::start().await;
    let m = Machine::new("M");
    let dir = m.home().join("scratch/.codex");
    let m = m.mount(".codex", dir.clone());
    m.set_sync_link(".codex", &dir.to_string_lossy(), "001/.codex")
        .await
        .unwrap();
    m.seed(".codex/memories/notes.md", "v1\n");
    m.push(&cloud, &[".codex"]).await.unwrap();

    m.activate();
    let scan = vec![m.path(".codex").to_string_lossy().into_owned()];
    let file = m
        .path(".codex/memories/notes.md")
        .to_string_lossy()
        .into_owned();
    let report = crate::get_file_statuses(m.handle().clone(), "codex".to_string(), None, scan.clone())
        .await
        .unwrap();
    assert_eq!(
        report.statuses.get(&file).map(String::as_str),
        Some("synced")
    );

    m.seed(".codex/memories/notes.md", "v2, longer than before\n");
    let report = crate::get_file_statuses(m.handle().clone(), "codex".to_string(), None, scan)
        .await
        .unwrap();
    assert_eq!(
        report.statuses.get(&file).map(String::as_str),
        Some("local-ahead")
    );
}

/// The editor's write boundary follows the mounts: files under the custom
/// mount are editable, files under the abandoned default are rejected before
/// their bytes can cross the UI boundary.
#[tokio::test]
async fn editor_boundary_follows_mounts() {
    let _env = harness::lock_env().await;
    let m = Machine::new("M");
    let dir = m.home().join("scratch/.codex");
    let m = m.mount(".codex", dir.clone());
    m.set_sync_link(".codex", &dir.to_string_lossy(), "")
        .await
        .unwrap();
    m.seed(".codex/config.toml", "x = 1\n");
    m.activate();

    let inside = m.path(".codex/config.toml").to_string_lossy().into_owned();
    let doc = crate::read_file_content(m.handle().clone(), inside)
        .await
        .unwrap();
    assert!(doc.editable, "{:?}", doc.reason);

    let stray = m.home().join(".codex/notes.md");
    std::fs::write(&stray, "y\n").unwrap();
    let error = crate::read_file_content(m.handle().clone(), stray.to_string_lossy().into_owned())
        .await
        .unwrap_err();
    assert!(error.contains("outside the config roots"), "{error}");
}

/// A fresh container mount: machine B points `.claude` at an empty folder
/// not named `.claude`; pull materializes `<container>/.claude/…` instead of
/// spilling Claude's files flat into the folder (the myconf2 bug).
#[tokio::test]
async fn container_mount_pull_nests_the_root() {
    let _env = harness::lock_env().await;
    let cloud = TestCloud::start_local().await;

    let a = Machine::new("A");
    a.seed(".claude/CLAUDE.md", "hello\n");
    a.seed(".claude/settings.json", "{}\n");
    a.push(&cloud, &[".claude"]).await.expect("A push");

    let b = Machine::new("B");
    let container = b.home().join("myconf2");
    let b = b.mount(".claude", container.clone());
    b.pull(&cloud).await.expect("B pull");

    assert!(container.join(".claude/CLAUDE.md").exists());
    assert!(container.join(".claude/settings.json").exists());
    // Nothing spilled flat into the container itself.
    assert!(!container.join("CLAUDE.md").exists());
    assert!(!container.join("settings.json").exists());
}

/// The mount's directory name picks the layout — a dir not named after the
/// root is a container hosting `<dir>/.codex` — but the logical `.codex/…`
/// namespace syncs identically either way.
#[tokio::test]
async fn mount_name_is_cosmetic() {
    let _env = harness::lock_env().await;
    let cloud = TestCloud::start_local().await;
    let m = Machine::new("M");
    let dir = m.home().join("work-codex"); // no ".codex" in the name
    let m = m.mount(".codex", dir);
    m.seed(".codex/memories/notes.md", "v1\n");
    m.push(&cloud, &[".codex"]).await.unwrap();
    assert!(cloud
        .manifest(".codex")
        .files
        .contains_key(".codex/memories/notes.md"));
}

/// The Codex plugin lock is force-included in any `.codex` push even when
/// the selection does not name it, and divergent locks from two machines
/// merge as a Tier 2 keyed union instead of conflict-copying — each machine
/// ends up seeing the other's plugin intent at the canonical path.
#[tokio::test]
async fn codex_plugin_lock_rides_along_and_merges_as_union() {
    const LOCK: &str = ".codex/agent-sync/codex-plugins.lock.json";
    let _env = harness::lock_env().await;
    let cloud = TestCloud::start_local().await;

    let lock_json = |plugin: &str| {
        format!(
            "{{\"schema\":1,\"marketplaces\":[{{\"name\":\"team-tools\",\"repository\":\"owner/repo\"}}],\"plugins\":[{{\"id\":\"{}@team-tools\"}}],\"manual\":[]}}",
            plugin
        )
    };

    let a = Machine::new("lockA");
    a.seed(LOCK, &lock_json("alpha"));
    a.seed(".codex/sessions/2026/07/05/rollout-a.jsonl", "{}\n");
    // Selective push naming only the transcript: the lock must ride along.
    a.push(&cloud, &[".codex/sessions/2026/07/05/rollout-a.jsonl"])
        .await
        .expect("A push");
    assert!(cloud.manifest(".codex").files.contains_key(LOCK));

    let b = Machine::new("lockB");
    b.seed(LOCK, &lock_json("beta"));
    b.seed(".codex/sessions/2026/07/05/rollout-b.jsonl", "{}\n");
    b.push(&cloud, &[".codex"]).await.expect("B push");

    // B's push merged both intents; no conflict copy appeared.
    let merged = b.read(LOCK);
    assert!(merged.contains("alpha@team-tools") && merged.contains("beta@team-tools"));
    assert_eq!(b.list(".codex/agent-sync").len(), 1, "no conflict copies");

    // A pulls and converges to the identical union bytes.
    a.pull(&cloud).await.expect("A pull");
    assert_eq!(a.read(LOCK), merged);
    assert_eq!(a.list(".codex/agent-sync").len(), 1);

    // A has not repaired beta yet. Its pre-push inventory capture therefore
    // sees only alpha; capture persistence must union with the pulled lock so
    // an unrelated push cannot erase beta's remote desired intent.
    let alpha_only: crate::codex_plugins::CodexPluginLock =
        serde_json::from_str(&lock_json("alpha")).unwrap();
    assert!(!crate::codex_plugins::save_captured_lock(&a.path(LOCK), &alpha_only).unwrap());
    assert_eq!(a.read(LOCK), merged);
    a.seed(".codex/sessions/2026/07/05/after-pull.jsonl", "{}\n");
    a.push(&cloud, &[".codex/sessions/2026/07/05/after-pull.jsonl"])
        .await
        .expect("unrelated push preserves pulled plugin intent");
    let c = Machine::new("lockC");
    c.pull(&cloud).await.expect("C pulls preserved union");
    assert!(c.read(LOCK).contains("beta@team-tools"));
}

/// Two meanings for the same marketplace name must never be unioned. A Pull
/// keeps the target's complete lock active, stores the cloud lock as a
/// deterministic remapped conflict sibling, surfaces it in readiness, and
/// force-includes both complete sides on the next Codex push.
#[tokio::test]
async fn codex_plugin_source_conflict_is_visible_and_published_losslessly() {
    use crate::readiness::{scan, LocalState, ScanInput};

    const LOCK: &str = ".codex/agent-sync/codex-plugins.lock.json";
    let _env = harness::lock_env().await;
    let cloud = TestCloud::start_local().await;
    let lock_json = |repository: &str, plugin: &str| {
        format!(
            "{{\"schema\":1,\"marketplaces\":[{{\"name\":\"team-tools\",\"repository\":\"{}\"}}],\"plugins\":[{{\"id\":\"{}@team-tools\"}}],\"manual\":[]}}",
            repository, plugin
        )
    };

    let a = Machine::new("lockConflictA");
    a.seed(LOCK, &lock_json("owner/repo-a", "alpha"));
    a.seed(".codex/sessions/2026/07/13/a.jsonl", "{}\n");
    a.push(&cloud, &[".codex"]).await.expect("A base push");

    let b = Machine::new("lockConflictB");
    b.pull(&cloud).await.expect("B base pull");
    b.seed(LOCK, &lock_json("owner/repo-b", "beta"));

    let cloud_lock = lock_json("owner/repo-a", "alpha-next");
    a.seed(LOCK, &cloud_lock);
    a.seed(".codex/sessions/2026/07/13/a.jsonl", "{\"v\":2}\n");
    a.push(&cloud, &[".codex"]).await.expect("A update push");

    b.pull(&cloud)
        .await
        .expect("B divergent pull preserves both locks");
    let conflict_rel = crate::conflict_copy_rel(LOCK, &crate::sha256_bytes(cloud_lock.as_bytes()));
    assert!(b.read(LOCK).contains("owner/repo-b"));
    assert_eq!(b.read(&conflict_rel), cloud_lock);

    let yes = |_: &str| true;
    let issues = scan(&ScanInput {
        codex_dir: &b.path(".codex"),
        claude_dir: &b.path(".claude"),
        lock_dirs: &[
            (".codex", &b.home().join(".agent-sync/codex")),
            (".claude", &b.home().join(".agent-sync/claude")),
        ],
        codex_plan: None,
        claude_plan: None,
        state: &LocalState::default(),
        resolve: &yes,
        env_present: &yes,
        sidebar_pending: None,
    });
    assert!(issues.iter().any(|issue| {
        issue.action == "resolve_conflict_copy"
            && issue
                .source_path
                .as_deref()
                .is_some_and(|path| path.contains("codex-plugins.lock.sync-conflict-"))
    }));

    b.seed(".codex/sessions/2026/07/13/b.jsonl", "{}\n");
    b.push(&cloud, &[".codex/sessions/2026/07/13/b.jsonl"])
        .await
        .expect("B push publishes both lock sides");
    let manifest = cloud.manifest(".codex");
    assert!(manifest.files.contains_key(LOCK));
    assert!(manifest.files.contains_key(&conflict_rel));
    assert!(b.read(LOCK).contains("owner/repo-b"));
    assert_eq!(b.read(&conflict_rel), cloud_lock);

    // Two replicas receive the published review copy. Resolve is an explicit
    // manifest deletion: an unchanged replica removes it, while a same-size,
    // same-mtime local edit must survive instead of hitting the stat fast path.
    a.pull(&cloud).await.expect("A receives conflict copy");
    assert_eq!(a.read(&conflict_rel), cloud_lock);
    let c = Machine::new("lockConflictC");
    c.pull(&cloud).await.expect("C receives conflict copy");
    assert_eq!(c.read(&conflict_rel), cloud_lock);

    let a_conflict = a.path(&conflict_rel);
    let original_mtime = std::fs::metadata(&a_conflict).unwrap().modified().unwrap();
    let edited = cloud_lock.replace("alpha-next", "gamma-next");
    assert_eq!(edited.len(), cloud_lock.len());
    std::fs::write(&a_conflict, &edited).unwrap();
    std::fs::File::options()
        .append(true)
        .open(&a_conflict)
        .unwrap()
        .set_modified(original_mtime)
        .unwrap();

    b.persist_cloud_config(&cloud);
    b.seed(&conflict_rel, "locally changed review bytes");
    let stale_review = crate::resolve_conflict_copy(
        b.handle().clone(),
        b.path(&conflict_rel).to_string_lossy().into_owned(),
    )
    .await
    .expect_err("resolution must pin the reviewed cloud variant");
    assert!(stale_review.contains("does not match"), "{stale_review}");
    assert!(cloud.manifest(".codex").files.contains_key(&conflict_rel));
    b.seed(&conflict_rel, &cloud_lock);
    crate::resolve_conflict_copy(
        b.handle().clone(),
        b.path(&conflict_rel).to_string_lossy().into_owned(),
    )
    .await
    .expect("resolve published conflict copy");
    assert!(!b.path(&conflict_rel).exists());
    assert!(!cloud.manifest(".codex").files.contains_key(&conflict_rel));
    assert_eq!(
        cloud.manifest(".codex").resolved_conflicts[&conflict_rel],
        crate::sha256_bytes(cloud_lock.as_bytes())
    );

    a.pull(&cloud)
        .await
        .expect("edited review copy remains local-ahead");
    assert_eq!(a.read(&conflict_rel), edited);
    // Resolution remains durable even if this replica's app-data baseline is
    // reset while its agent root (and review copy) survives.
    let profile_id = cloud.profile_for_root(".codex");
    let _ = std::fs::remove_file(crate::baseline_path(c.handle(), "codex", &cloud.storage_id, &profile_id).unwrap());
    c.pull(&cloud)
        .await
        .expect("published resolution reaches unchanged replica");
    assert!(!c.path(&conflict_rel).exists());
}

#[tokio::test]
async fn conflict_resolution_uses_s3_head_cas_and_propagates() {
    let _env = harness::lock_env().await;
    let cloud = TestCloud::start().await;
    let rel = ".codex/memories/notes.md";

    let a = Machine::new("resolveS3A");
    a.seed(rel, "base\n");
    a.push(&cloud, &[".codex"]).await.expect("base push");
    let b = Machine::new("resolveS3B");
    b.pull(&cloud).await.expect("B base pull");

    a.seed(rel, "from A\n");
    a.push(&cloud, &[".codex"]).await.expect("A update");
    b.seed(rel, "from B\n");
    b.push(&cloud, &[".codex"])
        .await
        .expect("B publishes conflict pair");
    let conflict_rel = crate::conflict_copy_rel(rel, &crate::sha256_bytes(b"from A\n"));
    assert_eq!(b.read(&conflict_rel), "from A\n");

    let c = Machine::new("resolveS3C");
    c.pull(&cloud).await.expect("C receives conflict copy");
    b.persist_cloud_config(&cloud);
    crate::resolve_conflict_copy(
        b.handle().clone(),
        b.path(&conflict_rel).to_string_lossy().into_owned(),
    )
    .await
    .expect("resolve through S3 CAS");
    assert!(!cloud.manifest(".codex").files.contains_key(&conflict_rel));

    c.pull(&cloud).await.expect("C consumes resolution");
    assert!(!c.path(&conflict_rel).exists());
}

#[cfg(unix)]
#[tokio::test]
async fn resolve_rechecks_symlinked_ancestors_after_the_head_publish() {
    use std::os::unix::fs::symlink;

    let _env = harness::lock_env().await;
    let cloud = TestCloud::start_local().await;
    let rel = ".codex/memories/notes.md";
    let a = Machine::new("resolvePathSwapA");
    a.seed(rel, "base\n");
    a.push(&cloud, &[".codex"]).await.expect("base push");
    let b = Machine::new("resolvePathSwapB");
    b.pull(&cloud).await.expect("B base pull");

    a.seed(rel, "from A\n");
    a.push(&cloud, &[".codex"]).await.expect("A update");
    b.seed(rel, "from B\n");
    b.push(&cloud, &[".codex"])
        .await
        .expect("B publishes conflict pair");
    let conflict_rel = crate::conflict_copy_rel(rel, &crate::sha256_bytes(b"from A\n"));
    b.persist_cloud_config(&cloud);

    let conflict_path = b.path(&conflict_rel);
    let conflict_parent = conflict_path.parent().unwrap().to_path_buf();
    let conflict_name = conflict_path.file_name().unwrap().to_owned();
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join(&conflict_name);
    std::fs::write(&outside_file, b"from A\n").unwrap();
    let outside_root = outside.path().to_path_buf();
    let mut fired = false;
    let _hook = LocalCasHookGuard::set(move |key: &str| {
        if fired || !key.ends_with("_head.json") {
            return;
        }
        fired = true;
        std::fs::remove_dir_all(&conflict_parent).unwrap();
        symlink(&outside_root, &conflict_parent).unwrap();
    });

    let error = crate::resolve_conflict_copy(
        b.handle().clone(),
        conflict_path.to_string_lossy().into_owned(),
    )
    .await
    .expect_err("ancestor swap after CAS must keep the redirected file");

    assert!(error.contains("became unsafe"), "{error}");
    assert_eq!(std::fs::read(&outside_file).unwrap(), b"from A\n");
    assert!(!cloud.manifest(".codex").files.contains_key(&conflict_rel));
}

#[tokio::test]
async fn resolve_accepts_a_legacy_raw_config_conflict_after_projection() {
    const CONFIG: &str = ".codex/config.toml";
    let _env = harness::lock_env().await;
    let cloud = TestCloud::start_local().await;
    let raw = br#"model = "gpt-5"

[marketplaces.openai-bundled]
source_type = "local"
source = "/machine-a/.codex/.tmp/bundled-marketplaces/openai-bundled"
"#;
    let logical = crate::codex_config::project_portable_bytes(raw).unwrap();
    assert_ne!(logical, raw);
    let conflict_rel = crate::conflict_copy_rel(CONFIG, &crate::sha256_bytes(&logical));

    let a = Machine::new("legacyRawResolveA");
    a.seed(".codex/AGENTS.md", "base\n");
    a.push(&cloud, &[".codex"]).await.expect("create profile");
    let profile_id = cloud.profile_for_root(".codex");
    publish_external_commit(
        &cloud.bucket_dir(),
        &profile_id,
        &[(&conflict_rel, raw as &[u8])],
        "legacy-client",
    );

    let b = Machine::new("legacyRawResolveB");
    b.pull(&cloud).await.expect("project legacy conflict copy");
    assert_eq!(std::fs::read(b.path(&conflict_rel)).unwrap(), logical);
    b.persist_cloud_config(&cloud);
    crate::resolve_conflict_copy(
        b.handle().clone(),
        b.path(&conflict_rel).to_string_lossy().into_owned(),
    )
    .await
    .expect("logical review SHA resolves raw cloud entry");
    assert!(!cloud.manifest(".codex").files.contains_key(&conflict_rel));
}

/// Canonical plugin locks are executable restore intent. A newer cloud value
/// is validated before ApplyCloud, so malformed/future-schema bytes cannot
/// replace the last-good local lock merely because that side was unchanged.
#[tokio::test]
async fn malformed_cloud_plugin_lock_never_replaces_last_good_lock() {
    const LOCK: &str = ".codex/agent-sync/codex-plugins.lock.json";
    let _env = harness::lock_env().await;
    let cloud = TestCloud::start_local().await;
    let valid = "{\"schema\":1,\"marketplaces\":[],\"plugins\":[{\"id\":\"slack@openai-curated\"}],\"manual\":[]}";

    let a = Machine::new("invalidLockA");
    a.seed(LOCK, valid);
    a.seed(".codex/sessions/2026/07/13/a.jsonl", "{}\n");
    a.push(&cloud, &[".codex"]).await.expect("valid base push");

    let b = Machine::new("invalidLockB");
    b.pull(&cloud).await.expect("valid base pull");
    assert_eq!(b.read(LOCK), valid);

    let profile_id = cloud.profile_for_root(".codex");
    publish_external_commit(
        &cloud.bucket_dir(),
        &profile_id,
        &[(LOCK, b"{\"schema\":2,\"plugins\":[]}" as &[u8])],
        "future-client",
    );
    let error = b
        .pull(&cloud)
        .await
        .expect_err("unsupported cloud lock must fail closed");
    assert!(error.contains("operation(s) failed"), "{error}");
    assert_eq!(b.read(LOCK), valid, "last-good canonical lock survives");
}

/// If both sides moved, an invalid local canonical lock must not win merely
/// because the Tier-2 union rejects it. The valid cloud lock becomes active;
/// the invalid local bytes survive only as a conflict sibling for review.
#[tokio::test]
async fn malformed_local_plugin_lock_never_overwrites_a_valid_cloud_lock() {
    const LOCK: &str = ".codex/agent-sync/codex-plugins.lock.json";
    let _env = harness::lock_env().await;
    let cloud = TestCloud::start_local().await;
    let base = "{\"schema\":1,\"marketplaces\":[],\"plugins\":[{\"id\":\"slack@openai-curated\"}],\"manual\":[]}";
    let cloud_next = "{\"schema\":1,\"marketplaces\":[],\"plugins\":[{\"id\":\"google-calendar@openai-curated\"}],\"manual\":[]}";
    let invalid_local = "{\"schema\":2,\"plugins\":[]}";

    let a = Machine::new("invalidLocalLockA");
    a.seed(LOCK, base);
    a.seed(".codex/AGENTS.md", "base\n");
    a.push(&cloud, &[".codex"]).await.expect("base push");

    let b = Machine::new("invalidLocalLockB");
    b.pull(&cloud).await.expect("B baseline");
    let profile_id = cloud.profile_for_root(".codex");
    publish_external_commit(
        &cloud.bucket_dir(),
        &profile_id,
        &[(LOCK, cloud_next.as_bytes())],
        "valid-newer-client",
    );

    b.seed(LOCK, invalid_local);
    b.seed(".codex/AGENTS.md", "unrelated update\n");
    b.push(&cloud, &[".codex/AGENTS.md"])
        .await
        .expect("push quarantines invalid local lock");
    assert_eq!(b.read(LOCK), cloud_next);

    let conflict_rel =
        crate::conflict_copy_rel(LOCK, &crate::sha256_bytes(invalid_local.as_bytes()));
    assert_eq!(b.read(&conflict_rel), invalid_local);
    let manifest = cloud.manifest(".codex");
    assert_eq!(
        manifest.files[LOCK].sha256,
        crate::sha256_bytes(cloud_next.as_bytes())
    );
    assert!(manifest.files.contains_key(&conflict_rel));

    let c = Machine::new("invalidLocalLockC");
    c.pull(&cloud)
        .await
        .expect("C receives valid canonical lock");
    assert_eq!(c.read(LOCK), cloud_next);
}

#[tokio::test]
async fn malformed_local_only_plugin_lock_blocks_an_unrelated_push() {
    const LOCK: &str = ".codex/agent-sync/codex-plugins.lock.json";
    let _env = harness::lock_env().await;
    let cloud = TestCloud::start_local().await;
    let valid = "{\"schema\":1,\"marketplaces\":[],\"plugins\":[{\"id\":\"slack@openai-curated\"}],\"manual\":[]}";

    let a = Machine::new("invalidLocalOnlyA");
    a.seed(LOCK, valid);
    a.seed(".codex/AGENTS.md", "base\n");
    a.push(&cloud, &[".codex"]).await.expect("base push");

    let b = Machine::new("invalidLocalOnlyB");
    b.pull(&cloud).await.expect("B baseline");
    b.seed(LOCK, "{\"schema\":2,\"plugins\":[]}");
    b.seed(".codex/AGENTS.md", "unrelated update\n");
    let error = b
        .push(&cloud, &[".codex/AGENTS.md"])
        .await
        .expect_err("invalid active lock must block publication");
    assert!(error.contains("rejected before upload"), "{error}");
    assert_eq!(
        cloud.manifest(".codex").files[LOCK].sha256,
        crate::sha256_bytes(valid.as_bytes())
    );
    assert_eq!(
        cloud.manifest(".codex").files[".codex/AGENTS.md"].sha256,
        crate::sha256_bytes(b"base\n"),
        "the unrelated update must not publish around lock validation"
    );

    let c = Machine::new("invalidLocalOnlyC");
    c.pull(&cloud).await.expect("cloud still holds valid lock");
    assert_eq!(c.read(LOCK), valid);
}

/// A fresh root (empty profile just bootstrapped, no settings.json yet) has
/// no plugin intent: Claude repair is a clean no-op report, not a hard
/// error — "Set up Claude here" on an empty bucket must not end in failure.
#[tokio::test]
async fn claude_repair_on_fresh_empty_root_is_a_noop() {
    let _env = harness::lock_env().await;
    let m = Machine::new("freshRepair");
    m.activate();
    let claude_dir = m.home().join(".claude");
    let lock_path = m.path(".claude/agent-sync/claude-plugins.lock.json");
    let report = crate::repair_plugins_blocking(m.handle(), &claude_dir, &lock_path, false)
        .expect("missing settings.json must not error");
    assert!(report.marketplaces_added.is_empty());
    assert!(report.plugins_installed.is_empty());
    assert!(report.failed.is_empty());
}

/// Claude lock, same guarantees as the Codex one: force-included in any
/// `.claude` push and Tier 2 keyed-union merged, so person A's plugin
/// intent reaches person B's canonical lock instead of a conflict copy.
#[tokio::test]
async fn claude_plugin_lock_rides_along_and_merges_as_union() {
    const LOCK: &str = ".claude/agent-sync/claude-plugins.lock.json";
    let _env = harness::lock_env().await;
    let cloud = TestCloud::start_local().await;

    let lock_json = |plugin: &str| {
        format!(
            "{{\"schema\":1,\"marketplaces\":[{{\"name\":\"ponytail\",\"repository\":\"DietrichGebert/ponytail\"}}],\"plugins\":[{{\"id\":\"{}@ponytail\"}}],\"manual\":[]}}",
            plugin
        )
    };

    let a = Machine::new("clockA");
    a.seed(LOCK, &lock_json("alpha"));
    a.seed(".claude/CLAUDE.md", "a\n");
    a.push(&cloud, &[".claude/CLAUDE.md"])
        .await
        .expect("A push");
    assert!(cloud.manifest(".claude").files.contains_key(LOCK));

    let b = Machine::new("clockB");
    b.seed(LOCK, &lock_json("beta"));
    b.seed(".claude/CLAUDE.md", "b\n");
    b.push(&cloud, &[".claude"]).await.expect("B push");

    let merged = b.read(LOCK);
    assert!(merged.contains("alpha@ponytail") && merged.contains("beta@ponytail"));
    assert_eq!(b.list(".claude/agent-sync").len(), 1, "no conflict copies");

    a.pull(&cloud).await.expect("A pull");
    assert_eq!(a.read(LOCK), merged);
}

/// Repair reads intent from the synced lock when present (the cross-person
/// carrier), falling back to settings.json only when it is absent.
#[tokio::test]
async fn claude_repair_intent_prefers_lock_over_settings() {
    let _env = harness::lock_env().await;
    let m = Machine::new("intentM");
    m.activate();
    let claude_dir = m.path(".claude");
    let lock_path = m.path(".claude/agent-sync/claude-plugins.lock.json");
    m.seed(
        ".claude/settings.json",
        "{\"enabledPlugins\":{\"settings-only@mkt\":true},\"extraKnownMarketplaces\":{\"mkt\":{\"source\":{\"repo\":\"o/r\"}}}}",
    );
    // No lock → None → caller falls back to settings.
    assert!(
        crate::claude_lock_intent(m.handle(), &lock_path, &claude_dir)
            .unwrap()
            .is_none()
    );
    m.seed(
        ".claude/agent-sync/claude-plugins.lock.json",
        "{\"schema\":1,\"marketplaces\":[{\"name\":\"ponytail\",\"repository\":\"DietrichGebert/ponytail\"}],\"plugins\":[{\"id\":\"ponytail@ponytail\"}],\"manual\":[{\"id\":\"x@local\",\"reason\":\"local\"}]}",
    );
    let intent = crate::claude_lock_intent(m.handle(), &lock_path, &claude_dir)
        .unwrap()
        .expect("lock intent");
    assert_eq!(intent.plugins, ["ponytail@ponytail"]);
    assert_eq!(
        intent.marketplaces,
        [(
            "ponytail".to_string(),
            "DietrichGebert/ponytail".to_string()
        )]
    );
    m.seed(
        ".claude/settings.json",
        "{\"enabledPlugins\":{\"ponytail@ponytail\":false}}",
    );
    let disabled = crate::claude_lock_intent(m.handle(), &lock_path, &claude_dir)
        .unwrap()
        .expect("lock still readable");
    assert!(disabled.plugins.is_empty());
    assert!(disabled.marketplaces.is_empty());
    // A present broken lock fails closed; it never falls back to stale
    // settings-based executable intent.
    m.seed(".claude/agent-sync/claude-plugins.lock.json", "{broken");
    assert!(crate::claude_lock_intent(m.handle(), &lock_path, &claude_dir).is_err());
}

/// PLAN_PORTABLE_AGENT_SETUP_V2.md: `.codex/agents` syncs by default; a
/// same-name/different-content agent produces a lossless conflict-copy
/// sibling that the readiness scan surfaces as `resolve_conflict_copy`;
/// hook review state is machine-local — reviewing on B clears B's issue
/// while A still sees its own.
#[tokio::test]
async fn custom_agents_sync_and_readiness_flags_conflicts_and_hooks() {
    use crate::readiness::{hook_definitions, scan, LocalState, ScanInput};
    let _env = harness::lock_env().await;
    let cloud = TestCloud::start_local().await;
    let yes = |_: &str| true;

    let a = Machine::new("readyA");
    a.seed(
        ".codex/agents/reviewer.toml",
        "name = \"reviewer\"\ndescription = \"a\"\ndeveloper_instructions = \"i\"\n",
    );
    a.seed(".codex/hooks.json", "[{\"cmd\":\"echo hi\"}]");
    a.push(&cloud, &[".codex"]).await.expect("A push");
    assert!(cloud
        .manifest(".codex")
        .files
        .contains_key(".codex/agents/reviewer.toml"));

    let b = Machine::new("readyB");
    b.seed(
        ".codex/agents/reviewer.toml",
        "name = \"reviewer\"\ndescription = \"b\"\ndeveloper_instructions = \"j\"\n",
    );
    // B's push unions A's changes in first: divergent agent → conflict copy,
    // A's hooks.json arrives as a new local file.
    b.push(&cloud, &[".codex"]).await.expect("B push");
    assert_eq!(b.list(".codex/agents").len(), 2, "conflict sibling kept");

    let issues_b = scan(&ScanInput {
        codex_dir: &b.path(".codex"),
        claude_dir: &b.path(".claude"),
        lock_dirs: &[
            (".codex", &b.home().join(".agent-sync/codex")),
            (".claude", &b.home().join(".agent-sync/claude")),
        ],
        codex_plan: None,
        claude_plan: None,
        state: &LocalState::default(),
        resolve: &yes,
        env_present: &yes,
        sidebar_pending: None,
    });
    assert!(
        issues_b.iter().any(|i| i.action == "resolve_conflict_copy"),
        "conflict sibling must surface: {:?}",
        issues_b
    );
    assert!(
        issues_b.iter().any(|i| i.category == "hooks"),
        "pulled hook needs review"
    );

    // Review on B is local bookkeeping; B clears, A still sees its own issue.
    let mut reviewed = LocalState::default();
    for (_, _, hash) in hook_definitions(&b.path(".codex"), &b.path(".claude")) {
        reviewed.reviewed_hooks.insert(hash, 1);
    }
    let issues_b = scan(&ScanInput {
        codex_dir: &b.path(".codex"),
        claude_dir: &b.path(".claude"),
        lock_dirs: &[
            (".codex", &b.home().join(".agent-sync/codex")),
            (".claude", &b.home().join(".agent-sync/claude")),
        ],
        codex_plan: None,
        claude_plan: None,
        state: &reviewed,
        resolve: &yes,
        env_present: &yes,
        sidebar_pending: None,
    });
    assert!(!issues_b.iter().any(|i| i.category == "hooks"));
    let issues_a = scan(&ScanInput {
        codex_dir: &a.path(".codex"),
        claude_dir: &a.path(".claude"),
        lock_dirs: &[
            (".codex", &a.home().join(".agent-sync/codex")),
            (".claude", &a.home().join(".agent-sync/claude")),
        ],
        codex_plan: None,
        claude_plan: None,
        state: &LocalState::default(),
        resolve: &yes,
        env_present: &yes,
        sidebar_pending: None,
    });
    assert!(
        issues_a.iter().any(|i| i.category == "hooks"),
        "review must not propagate to A"
    );
}

/// PLAN_GLOBAL_AGENT_SYNC_DIR.md: app records live in `~/.agent-sync`,
/// never inside the agent roots. A leftover in-root lock from a pre-remap
/// build is invisible to the engine (no logical-path collision) and its
/// known filenames are removed on the next push, while unknown files
/// survive. `machine.json` appears after a sync and never enters the
/// manifest.
#[tokio::test]
async fn app_records_live_outside_roots_and_legacy_dir_is_cleaned() {
    const LOCK: &str = ".codex/agent-sync/codex-plugins.lock.json";
    let _env = harness::lock_env().await;
    let cloud = TestCloud::start_local().await;

    let a = Machine::new("globalA");
    a.seed(
        LOCK,
        "{\"schema\":1,\"marketplaces\":[{\"name\":\"m\",\"repository\":\"o/r\"}],\"plugins\":[{\"id\":\"p@m\"}],\"manual\":[]}",
    );
    // Physical location is the global app dir, not the root.
    assert!(a
        .home()
        .join(".agent-sync/codex/codex-plugins.lock.json")
        .is_file());
    assert!(!a.home().join(".codex/agent-sync").exists());

    // Legacy leftovers a pre-remap build wrote into the root.
    let legacy = a.home().join(".codex/agent-sync");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("codex-plugins.lock.json"), "{\"stale\":true}").unwrap();
    std::fs::write(
        legacy.join("codex-plugins.lock.sync-conflict-deadbeef.json"),
        "{}",
    )
    .unwrap();
    std::fs::write(legacy.join("keep.txt"), "not ours").unwrap();

    a.seed(".codex/sessions/2026/07/11/rollout-a.jsonl", "{}\n");
    a.push(&cloud, &[".codex"]).await.expect("A push");

    // Cloud carries the canonical lock, not the stale in-root bytes.
    let manifest = cloud.manifest(".codex");
    assert!(manifest.files.contains_key(LOCK));
    assert!(a.read(LOCK).contains("p@m"));
    // Known app filenames removed; the unknown file (and thus the dir) kept.
    assert!(!legacy.join("codex-plugins.lock.json").exists());
    assert!(!legacy
        .join("codex-plugins.lock.sync-conflict-deadbeef.json")
        .exists());
    assert!(legacy.join("keep.txt").is_file());
    // The machine registry exists locally and never syncs.
    assert!(a.home().join(".agent-sync/machine.json").is_file());
    assert!(manifest.files.keys().all(|k| !k.contains("machine.json")));

    // Fresh machine: pull materializes the lock in the global dir only.
    let b = Machine::new("globalB");
    b.pull(&cloud).await.expect("B pull");
    assert!(b
        .home()
        .join(".agent-sync/codex/codex-plugins.lock.json")
        .is_file());
    assert!(!b.home().join(".codex/agent-sync").exists());
    assert!(b.home().join(".agent-sync/machine.json").is_file());
}

/// Stress test from the field: wipe the entire store (head, manifests,
/// uploads gone), then push with a full selection. The relink creates a
/// fresh profile with an empty manifest and empty baseline, so every local
/// file must classify as upload — the new generation carries a full copy,
/// not just the files that changed since the old baseline.
#[tokio::test]
async fn store_wipe_then_push_republishes_everything() {
    let _env = harness::lock_env().await;
    let cloud = TestCloud::start_local().await;
    let m = Machine::new("wipeM");
    m.seed(".claude/settings.json", "{\"a\":1}\n");
    m.seed(".claude/projects/-p/s1.jsonl", "{}\n");
    m.seed(".claude/history.jsonl", "{\"ts\":1}\n");
    m.push(&cloud, &[".claude"]).await.expect("first push");
    let old_profile = cloud.profile_for_root(".claude");
    assert!(cloud.manifest(".claude").files.len() >= 3);

    std::fs::remove_dir_all(cloud.bucket_dir().join(&old_profile)).unwrap();

    m.push(&cloud, &[".claude"]).await.expect("push after wipe");
    let new_profile = cloud.profile_for_root(".claude");
    assert_ne!(
        new_profile, old_profile,
        "stale link must relink to a fresh profile"
    );
    let manifest = cloud.manifest(".claude");
    for rel in [
        ".claude/settings.json",
        ".claude/projects/-p/s1.jsonl",
        ".claude/history.jsonl",
    ] {
        assert!(
            manifest.files.contains_key(rel),
            "{} missing after wipe+push",
            rel
        );
    }
}

/// The fresh-mount lifecycle that triggered the sticky-selection bug in the
/// field (myconf2): machine B mounts the root at an EMPTY container dir,
/// pulls an existing profile into it, edits one file and adds another, then
/// pushes the whole root. The published generation must carry the full file
/// set — a fresh mount must never lead to a partial profile, and the
/// original machine must converge to the union on its next pull.
async fn run_fresh_mount_pull_edit_push(root: &'static str, seeds: &[(&str, &str)]) {
    let cloud = TestCloud::start_local().await;

    let a = Machine::new("freshA");
    for (rel, content) in seeds {
        a.seed(rel, content);
    }
    a.push(&cloud, &[root]).await.expect("A push");
    let full = cloud.manifest(root).files.len();
    assert_eq!(full, seeds.len(), "A publishes every seeded file");

    // B: fresh, empty container mount — the dir exists but holds nothing
    // until the pull materializes the root inside it.
    let b = Machine::new("freshB");
    let container = b.home().join("myconf-fresh");
    let b = b.mount(root, container);
    b.pull(&cloud).await.expect("B pull");
    for (rel, content) in seeds {
        assert_eq!(
            &b.read(rel),
            content,
            "{} restored into the fresh mount",
            rel
        );
    }

    // Edit one file (content length must change — same-size same-second
    // edits hide from the stat fast path) and add a new one.
    let (edited_rel, _) = seeds[0];
    b.seed(edited_rel, "edited to a different length\n");
    let added_rel = format!("{}/skills/added/SKILL.md", root);
    b.seed(&added_rel, "new\n");

    b.push(&cloud, &[root]).await.expect("B push");
    let manifest = cloud.manifest(root);
    for (rel, _) in seeds {
        assert!(
            manifest.files.contains_key(*rel),
            "{} lost after fresh-mount push",
            rel
        );
    }
    assert!(manifest.files.contains_key(added_rel.as_str()));
    assert_eq!(
        manifest.files.len(),
        full + 1,
        "full set plus the added file"
    );

    a.pull(&cloud).await.expect("A pull");
    assert_eq!(a.read(edited_rel), "edited to a different length\n");
    assert_eq!(a.read(&added_rel), "new\n");
}

#[tokio::test]
async fn fresh_mount_pull_edit_push_keeps_full_profile_codex() {
    let _env = harness::lock_env().await;
    run_fresh_mount_pull_edit_push(
        ".codex",
        &[
            (".codex/sessions/2026/07/05/rollout-a.jsonl", "{}\n"),
            (".codex/history.jsonl", "{\"ts\":1}\n"),
            (".codex/config.toml", "model = \"gpt\"\n"),
            (".codex/skills/foo/SKILL.md", "skill\n"),
            (
                ".codex/agent-sync/codex-plugins.lock.json",
                "{\"schema\":1,\"marketplaces\":[{\"name\":\"team-tools\",\"repository\":\"owner/repo\"}],\"plugins\":[{\"id\":\"a@team-tools\"}],\"manual\":[]}",
            ),
        ],
    )
    .await;
}

#[tokio::test]
async fn fresh_mount_pull_edit_push_keeps_full_profile_claude() {
    let _env = harness::lock_env().await;
    run_fresh_mount_pull_edit_push(
        ".claude",
        &[
            (".claude/projects/-p/s1.jsonl", "{}\n"),
            (".claude/history.jsonl", "{\"timestamp\":1}\n"),
            (".claude/settings.json", "{\"a\":1}\n"),
            (".claude/todos/t.json", "[]\n"),
            (
                ".claude/agent-sync/claude-plugins.lock.json",
                "{\"schema\":1,\"marketplaces\":[{\"name\":\"ponytail\",\"repository\":\"DietrichGebert/ponytail\"}],\"plugins\":[{\"id\":\"ponytail@ponytail\"}],\"manual\":[]}",
            ),
        ],
    )
    .await;
}

/// Part A of PLAN_CODEX_THREAD_REBUILD_AND_SIDEBAR.md — push captures each
/// file's mtime into the manifest and pull restores it, so Codex's
/// rollout-based thread rebuild sees real recency on a fresh machine. A
/// second pull is a no-op against the restored stat, and merge-driver
/// outputs stay exempt (stamped at merge time, not with a cloud mtime).
async fn run_source_mtime_round_trip(cloud: TestCloud) {
    const OLD_MTIME: u64 = 1_600_000_000;
    let rollout = ".codex/sessions/2026/07/05/rollout-a.jsonl";

    let a = Machine::new("mtimeA");
    a.seed(rollout, "{\"line\":1}\n");
    a.seed(".codex/history.jsonl", "{\"ts\":1,\"text\":\"a\"}\n");
    let set_mtime = |path: &std::path::Path, secs: u64| {
        std::fs::File::options()
            .append(true)
            .open(path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
            .unwrap();
    };
    set_mtime(&a.path(rollout), OLD_MTIME);
    a.push(&cloud, &[".codex"]).await.expect("A push");
    assert_eq!(
        cloud.manifest(".codex").files[rollout].source_mtime,
        OLD_MTIME,
        "push captures the source mtime"
    );

    let b = Machine::new("mtimeB");
    b.pull(&cloud).await.expect("B pull");
    let restored = |m: &Machine| {
        std::fs::metadata(m.path(rollout))
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    };
    assert_eq!(restored(&b), OLD_MTIME, "pull restores the source mtime");

    // No-op second pull: the baseline recorded the post-restore stat, so
    // nothing re-applies and the mtime stays put.
    b.pull(&cloud).await.expect("B second pull");
    assert_eq!(
        restored(&b),
        OLD_MTIME,
        "no-op pull keeps the restored mtime"
    );

    // Divergent history.jsonl: the merged output is new content produced at
    // merge time — its mtime must be recent, never the cloud entry's.
    a.seed(
        ".codex/history.jsonl",
        "{\"ts\":1,\"text\":\"a\"}\n{\"ts\":2,\"text\":\"a2\"}\n",
    );
    set_mtime(&a.path(".codex/history.jsonl"), OLD_MTIME);
    a.push(&cloud, &[".codex"]).await.expect("A history push");
    b.seed(
        ".codex/history.jsonl",
        "{\"ts\":1,\"text\":\"a\"}\n{\"ts\":3,\"text\":\"b3\"}\n",
    );
    b.pull(&cloud).await.expect("B merge pull");
    let merged_mtime = std::fs::metadata(b.path(".codex/history.jsonl"))
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(
        merged_mtime > OLD_MTIME,
        "merge output stamped now, not with the cloud mtime ({})",
        merged_mtime
    );
}

#[tokio::test]
async fn source_mtime_round_trip() {
    let _env = harness::lock_env().await;
    run_source_mtime_round_trip(TestCloud::start().await).await;
}

#[tokio::test]
async fn source_mtime_round_trip_local() {
    let _env = harness::lock_env().await;
    run_source_mtime_round_trip(TestCloud::start_local().await).await;
}

/// Part B of PLAN_CODEX_THREAD_REBUILD_AND_SIDEBAR.md — divergent sidebar
/// locks merge by keyed union to identical bytes on both machines, the raw
/// desktop state file never enters a manifest, and the explicit apply adds
/// the other machine's state additively (rollout-gated titles, no local
/// removals).
async fn run_sidebar_lock_converges_and_applies(cloud: TestCloud) {
    const LOCK: &str = ".codex/agent-sync/codex-sidebar.lock.json";
    let a = Machine::new("sidebarA");
    let b = Machine::new("sidebarB");

    // Present on disk on B so the lock project path-matches there.
    let shared_project = b.home().join("shared-project");
    std::fs::create_dir_all(&shared_project).unwrap();
    let shared_path = shared_project.to_string_lossy().to_string();

    a.seed(".codex/config.toml", "model = \"gpt\"\n");
    // Never-sync tier: the raw desktop state must not appear in any manifest.
    a.seed(
        ".codex/.codex-global-state.json",
        "{\"electron-local-remote-control-installation-id\":\"secret\"}\n",
    );
    a.seed(
        LOCK,
        &format!(
            "{{\"schema\":1,\"projects\":[{{\"path\":\"{}\",\"git_origin\":\"github.com/x/shared\"}}],\"project_order\":[\"{}\"],\"thread_descriptions\":{{\"019f-aaaa\":\"Title from A\"}},\"sidebar\":{{\"mode\":\"project\",\"project_sort\":\"priority\"}}}}",
            shared_path, shared_path
        ),
    );
    a.push(&cloud, &[".codex"]).await.expect("A push");
    let manifest = cloud.manifest(".codex");
    assert!(manifest.files.contains_key(LOCK), "lock syncs");
    assert!(
        !manifest
            .files
            .contains_key(".codex/.codex-global-state.json"),
        "raw desktop state must never sync"
    );

    b.seed(".codex/config.toml", "model = \"gpt\"\n");
    b.seed(
        LOCK,
        "{\"schema\":1,\"projects\":[{\"path\":\"/b/own\"}],\"project_order\":[\"/b/own\"],\"thread_descriptions\":{\"019f-bbbb\":\"Title from B\"},\"sidebar\":{}}",
    );
    b.push(&cloud, &[".codex"]).await.expect("B push unions");
    a.pull(&cloud).await.expect("A pull");
    harness::assert_converged(&[&a, &b], &[LOCK]);
    let merged: crate::codex_sidebar::CodexSidebarLock =
        serde_json::from_str(&b.read(LOCK)).unwrap();
    assert_eq!(
        merged.projects.len(),
        2,
        "keyed union keeps both: {:?}",
        merged.projects
    );
    assert_eq!(merged.thread_descriptions.len(), 2);
    assert_eq!(merged.sidebar.mode.as_deref(), Some("project"));

    // Explicit apply on B: adds the path-matched project and the
    // rollout-gated title, leaves B-only state untouched.
    b.seed(
        ".codex/.codex-global-state.json",
        "{\"electron-saved-workspace-roots\":[\"/b/existing\"],\"electron-persisted-atom-state\":{\"thread-descriptions-v1\":{\"local-t\":\"local title\"}}}\n",
    );
    b.seed(
        ".codex/sessions/2026/07/05/rollout-2026-07-05T00-00-00-019f-aaaa.jsonl",
        "{}\n",
    );
    let summary =
        crate::codex_sidebar::apply_from_lock(&b.path(LOCK), &b.path(".codex")).expect("apply");
    assert!(summary.contains("project"), "{}", summary);
    let state: serde_json::Value =
        serde_json::from_str(&b.read(".codex/.codex-global-state.json")).unwrap();
    let roots: Vec<&str> = state["electron-saved-workspace-roots"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        roots,
        vec!["/b/existing", shared_path.as_str()],
        "additive only"
    );
    let titles = &state["electron-persisted-atom-state"]["thread-descriptions-v1"];
    assert_eq!(titles["local-t"], "local title", "local state untouched");
    assert_eq!(
        titles["019f-aaaa"], "Title from A",
        "rollout present → title applies"
    );
    assert!(
        titles.get("019f-bbbb").is_none(),
        "no rollout on B → title gated out"
    );
}

#[tokio::test]
async fn sidebar_lock_converges_and_applies() {
    let _env = harness::lock_env().await;
    run_sidebar_lock_converges_and_applies(TestCloud::start().await).await;
}

#[tokio::test]
async fn sidebar_lock_converges_and_applies_local() {
    let _env = harness::lock_env().await;
    run_sidebar_lock_converges_and_applies(TestCloud::start_local().await).await;
}

/// A cloud/baseline path remains in the reconcile domain even when WalkDir
/// correctly skips a local directory symlink. Push must reject that physical
/// path before status/hash reads instead of uploading the symlink target.
#[cfg(unix)]
#[tokio::test]
async fn manifest_driven_push_never_reads_a_symlinked_local_tree() {
    use std::os::unix::fs::symlink;

    const REL: &str = ".codex/skills/team/private.md";
    let _env = harness::lock_env().await;
    let cloud = TestCloud::start_local().await;
    let a = Machine::new("symlinkManifestA");
    a.seed(REL, "published\n");
    a.push(&cloud, &[".codex"]).await.expect("seed cloud");

    let b = Machine::new("symlinkManifestB");
    b.pull(&cloud).await.expect("establish B baseline");
    std::fs::remove_dir_all(b.path(".codex/skills")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(outside.path().join("team")).unwrap();
    std::fs::write(outside.path().join("team/private.md"), b"outside secret\n").unwrap();
    symlink(outside.path(), b.path(".codex/skills")).unwrap();

    let published_sha = cloud.manifest_file_sha(".codex", REL).unwrap();
    let error = b
        .push(&cloud, &[".codex"])
        .await
        .expect_err("symlinked manifest path must fail closed");

    assert!(error.contains("operation(s) failed"), "{error}");
    assert_eq!(
        cloud.manifest_file_sha(".codex", REL).unwrap(),
        published_sha
    );
    assert_eq!(
        std::fs::read(outside.path().join("team/private.md")).unwrap(),
        b"outside secret\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn prepush_remapped_lock_scan_never_follows_a_symlinked_slug() {
    use std::os::unix::fs::symlink;

    let _env = harness::lock_env().await;
    let cloud = TestCloud::start_local().await;
    let machine = Machine::new("remappedSlugPush");
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(machine.home().join(".agent-sync")).unwrap();
    for name in [
        "codex-plugins.lock.json",
        "codex-sidebar.lock.json",
        "codex-plugins.lock.sync-conflict-a1b2c3d4.json",
    ] {
        std::fs::write(outside.path().join(name), b"outside").unwrap();
    }
    symlink(outside.path(), machine.home().join(".agent-sync/codex")).unwrap();
    machine.seed(".codex/AGENTS.md", "safe\n");

    machine
        .push(&cloud, &[".codex"])
        .await
        .expect("ordinary push skips unsafe generated locks");

    for name in [
        "codex-plugins.lock.json",
        "codex-sidebar.lock.json",
        "codex-plugins.lock.sync-conflict-a1b2c3d4.json",
    ] {
        assert_eq!(
            std::fs::read(outside.path().join(name)).unwrap(),
            b"outside"
        );
        assert!(!cloud
            .manifest(".codex")
            .files
            .contains_key(&format!(".codex/agent-sync/{name}")));
    }
}

/// S21 — same pinned prefix in two storages, divergent content: baselines
/// and caches are keyed per (storage, cloud profile), so one machine linked
/// to both storages never cross-talks between them (PLAN_MULTI_STORAGE.md
/// 2b). A v1 profile-id-keyed baseline would have claimed the file was
/// already synced and clobbered it.
async fn run_s21_same_name_profiles_in_two_storages(c1: TestCloud, c2: TestCloud) {
    let a = Machine::new("A");
    a.pin_cloud_prefix(".codex", "001/.codex");
    a.seed(".codex/memories/notes.md", "from storage one\n");
    a.push(&c1, &[".codex"]).await.expect("A push to c1");

    // Another machine publishes different bytes at the SAME prefix in the
    // second storage — two unrelated universes sharing a name.
    let b = Machine::new("B");
    b.pin_cloud_prefix(".codex", "001/.codex");
    b.seed(".codex/memories/notes.md", "from storage two\n");
    b.push(&c2, &[".codex"]).await.expect("B push to c2");

    // A pulls the same-named profile from c2: no baseline exists for THAT
    // link, so the divergence is a fresh both-present union — local kept,
    // cloud side landing as a conflict sibling.
    a.pull_root(&c2, ".codex").await.expect("A pull from c2");
    assert_eq!(a.read(".codex/memories/notes.md"), "from storage one\n");
    let conflict_rel = crate::conflict_copy_rel(
        ".codex/memories/notes.md",
        &crate::sha256_bytes(b"from storage two\n"),
    );
    assert_eq!(a.read(&conflict_rel), "from storage two\n");
    // The pull published nothing to either storage.
    assert_eq!(c1.head_of("001/.codex").unwrap().generation, 1);
    assert_eq!(c2.head_of("001/.codex").unwrap().generation, 1);

    // The c1 link's state is untouched by the c2 pull: pulling c1 keeps the
    // local file and the (local-only) sibling; nothing conflicts again.
    a.pull_root(&c1, ".codex").await.expect("A pull from c1");
    assert_eq!(a.read(".codex/memories/notes.md"), "from storage one\n");
    assert_eq!(a.read(&conflict_rel), "from storage two\n");
}

#[tokio::test]
async fn s21_same_name_profiles_in_two_storages() {
    let _env = harness::lock_env().await;
    run_s21_same_name_profiles_in_two_storages(TestCloud::start().await, TestCloud::start().await)
        .await;
}

#[tokio::test]
async fn s21_same_name_profiles_in_two_storages_local() {
    let _env = harness::lock_env().await;
    run_s21_same_name_profiles_in_two_storages(
        TestCloud::start_local().await,
        TestCloud::start_local().await,
    )
    .await;
}

/// S22 — fan-out: one local profile pushes to two storages; each link has
/// its own generation clock, and a machine that syncs through one storage
/// converges with the other through the shared local tree, hash-verified,
/// no conflict churn.
async fn run_s22_fan_out_one_profile_two_storages(c1: TestCloud, c2: TestCloud) {
    let a = Machine::new("A");
    a.seed(".codex/memories/notes.md", "v1\n");
    a.push(&c1, &[".codex"]).await.expect("A push c1");
    a.push(&c2, &[".codex"]).await.expect("A push c2");
    assert_eq!(c1.head(".codex").generation, 1);
    assert_eq!(c2.head(".codex").generation, 1);

    // Edit lands in c1 only; c2 stays behind.
    a.seed(".codex/memories/notes.md", "v2, longer than v1\n");
    a.push(&c1, &[".codex"]).await.expect("A push c1 v2");
    assert_eq!(c1.head(".codex").generation, 2);
    assert_eq!(c2.head(".codex").generation, 1, "c2 not pushed yet");

    // Catch c2 up from the same tree.
    a.push(&c2, &[".codex"]).await.expect("A push c2 v2");
    assert_eq!(c2.head(".codex").generation, 2);

    // A machine syncing via c2 sees the same bytes; a later pull of the
    // OTHER storage's identical content re-verifies by hash — unchanged,
    // no conflict copies.
    let b = Machine::new("B");
    b.pull_root(&c2, ".codex").await.expect("B pull c2");
    assert_eq!(b.read(".codex/memories/notes.md"), "v2, longer than v1\n");
    let result = b.pull_root(&c1, ".codex").await.expect("B pull c1");
    assert!(result.message.contains("0 conflict") || !result.message.contains("conflict"));
    assert!(!b.list(".codex").iter().any(|p| p.contains("sync-conflict")));
    harness::assert_converged(&[&a, &b], &[".codex/memories/notes.md"]);
}

#[tokio::test]
async fn s22_fan_out_one_profile_two_storages() {
    let _env = harness::lock_env().await;
    run_s22_fan_out_one_profile_two_storages(TestCloud::start().await, TestCloud::start().await)
        .await;
}

#[tokio::test]
async fn s22_fan_out_one_profile_two_storages_local() {
    let _env = harness::lock_env().await;
    run_s22_fan_out_one_profile_two_storages(
        TestCloud::start_local().await,
        TestCloud::start_local().await,
    )
    .await;
}

/// S23 — unlinking a cell (or changing a storage's identity) forgets that
/// link's baseline; relinking re-verifies everything by hash: no republish,
/// no conflicts, no data loss (PLAN_MULTI_STORAGE.md §3.2).
async fn run_s23_unlink_drops_state_and_relink_reverifies(cloud: TestCloud) {
    let m = Machine::new("M");
    m.seed(".codex/memories/notes.md", "v1\n");
    m.push(&cloud, &[".codex"]).await.expect("push");
    let profile_id = cloud.profile_for_root(".codex");
    m.activate();
    let baseline = crate::baseline_path(m.handle(), "codex", &cloud.storage_id, &profile_id).unwrap();
    assert!(baseline.exists(), "push must persist a baseline");

    // Unlink the cell through the real settings save: the baseline drops;
    // the cloud profile stays.
    let mut config = m.saved_config();
    config.links.retain(|l| l.storage != cloud.storage_id);
    crate::save_sync_config(m.handle().clone(), config)
        .await
        .unwrap();
    assert!(!baseline.exists(), "unlink must forget the baseline");
    assert_eq!(cloud.head(".codex").generation, 1, "cloud data untouched");

    // Relink (the harness re-creates the link on push) — the tree matches
    // the cloud byte-for-byte, so the hash path re-verifies everything.
    let result = m.push(&cloud, &[".codex"]).await.expect("relink push");
    assert!(result.message.contains("up to date"), "{}", result.message);
    assert_eq!(cloud.head(".codex").generation, 1, "nothing republished");
    assert!(baseline.exists(), "baseline rebuilt");
    assert!(!m.list(".codex").iter().any(|p| p.contains("sync-conflict")));

    // A storage identity change also invalidates its baselines: they
    // described a different destination.
    let mut config = m.saved_config();
    let storage = config
        .storages
        .iter_mut()
        .find(|s| s.id == cloud.storage_id)
        .unwrap();
    if storage.kind == "local" {
        storage.local_dir = format!("{}-elsewhere", storage.local_dir);
    } else {
        storage.bucket = "bucket-elsewhere".to_string();
    }
    crate::save_sync_config(m.handle().clone(), config)
        .await
        .unwrap();
    assert!(
        !baseline.exists(),
        "identity change must invalidate the baseline"
    );
}

#[tokio::test]
async fn s23_unlink_drops_state_and_relink_reverifies() {
    let _env = harness::lock_env().await;
    run_s23_unlink_drops_state_and_relink_reverifies(TestCloud::start().await).await;
}

#[tokio::test]
async fn s23_unlink_drops_state_and_relink_reverifies_local() {
    let _env = harness::lock_env().await;
    run_s23_unlink_drops_state_and_relink_reverifies(TestCloud::start_local().await).await;
}

/// S24 — the matrix: two storages × three local profiles on one machine
/// (default codex fanning out to both, default claude in storage 1, a
/// second custom `.claude` profile in storage 2, both claude links pinned
/// to the SAME cloud name). Proves per-link baselines and statuses,
/// same-kind neighbors with separate app-record dirs, and that a storage
/// is a self-contained universe (PLAN_MULTI_STORAGE.md §1, 2a, 2b).
async fn run_s24_matrix_two_storages_three_profiles(c1: TestCloud, c2: TestCloud) {
    let m = Machine::new("M");
    m.pin_cloud_prefix(".claude", "001/.claude");
    m.add_profile("work", ".claude", m.home().join("myconf"), Some("001/.claude"));
    m.seed(".codex/memories/notes.md", "codex notes v1\n");
    m.seed(".codex/auth.json", "{\"secret\":\"never\"}\n");
    m.seed(".claude/CLAUDE.md", "personal claude\n");
    m.seed_profile("work", ".claude/CLAUDE.md", "work claude, different bytes\n");

    // Four links: codex -> both storages, personal claude -> c1, work -> c2.
    m.push(&c1, &[".codex", ".claude"]).await.expect("push c1");
    m.push(&c2, &[".codex"]).await.expect("push codex c2");
    m.push_profile(&c2, "work", &[".claude"])
        .await
        .expect("push work c2");

    // Each storage is a self-contained two-profile universe; the two
    // pinned `001/.claude` profiles share a name and nothing else.
    let codex1 = c1.profile_for_root(".codex");
    let codex2 = c2.profile_for_root(".codex");
    assert_eq!(c1.profiles_for_root(".claude"), vec!["001/.claude"]);
    assert_eq!(c2.profiles_for_root(".claude"), vec!["001/.claude"]);
    let claude_sha =
        |cloud: &TestCloud| cloud.manifest_of("001/.claude").files[".claude/CLAUDE.md"]
            .sha256
            .clone();
    assert_eq!(claude_sha(&c1), crate::sha256_bytes(b"personal claude\n"));
    assert_eq!(
        claude_sha(&c2),
        crate::sha256_bytes(b"work claude, different bytes\n")
    );
    // Fan-out published identical codex bytes to both storages...
    assert_eq!(
        c1.manifest_of(&codex1).files[".codex/memories/notes.md"].sha256,
        c2.manifest_of(&codex2).files[".codex/memories/notes.md"].sha256
    );
    // ...and the Never tier held per link.
    assert!(!c1.manifest_of(&codex1).files.contains_key(".codex/auth.json"));
    assert!(!c2.manifest_of(&codex2).files.contains_key(".codex/auth.json"));

    // Per-link sync state: four baselines keyed (local profile, storage,
    // cloud profile) — in particular the same-named claude profiles have
    // separate files.
    m.activate();
    let baseline = |local: &str, cloud: &TestCloud, profile: &str| {
        crate::baseline_path(m.handle(), local, &cloud.storage_id, profile).unwrap()
    };
    let claude_b1 = baseline("claude", &c1, "001/.claude");
    let claude_b2 = baseline("work", &c2, "001/.claude");
    assert_ne!(claude_b1, claude_b2);
    for path in [
        baseline("codex", &c1, &codex1),
        baseline("codex", &c2, &codex2),
        claude_b1,
        claude_b2,
    ] {
        assert!(path.exists(), "missing baseline {}", path.display());
    }

    // Fan-out staleness is per link: an edit pushed to c1 leaves c2 behind,
    // and file statuses answer per link.
    m.seed(".codex/memories/notes.md", "codex notes v2, now longer\n");
    m.push(&c1, &[".codex"]).await.expect("push codex v2 c1");
    assert_eq!(c1.head_of(&codex1).unwrap().generation, 2);
    assert_eq!(c2.head_of(&codex2).unwrap().generation, 1, "c2 behind");
    let scan = vec![m.path(".codex").to_string_lossy().into_owned()];
    let file = m
        .path(".codex/memories/notes.md")
        .to_string_lossy()
        .into_owned();
    let report = crate::get_file_statuses(
        m.handle().clone(),
        "codex".to_string(),
        Some(c1.storage_id.clone()),
        scan.clone(),
    )
    .await
    .unwrap();
    assert_eq!(report.statuses.get(&file).map(String::as_str), Some("synced"));
    let report = crate::get_file_statuses(
        m.handle().clone(),
        "codex".to_string(),
        Some(c2.storage_id.clone()),
        scan,
    )
    .await
    .unwrap();
    assert_eq!(
        report.statuses.get(&file).map(String::as_str),
        Some("local-ahead")
    );
    m.push(&c2, &[".codex"]).await.expect("catch c2 up");
    assert_eq!(c2.head_of(&codex2).unwrap().generation, 2);

    // The claude neighbors never cross-talk: both mounts keep their own
    // bytes, and re-pulling each link applies nothing new.
    m.pull_root(&c1, ".claude").await.expect("pull claude c1");
    m.pull_profile(&c2, "work").await.expect("pull work c2");
    assert_eq!(m.read(".claude/CLAUDE.md"), "personal claude\n");
    assert_eq!(
        m.read_profile("work", ".claude/CLAUDE.md"),
        "work claude, different bytes\n"
    );
    assert!(!m.list(".claude").iter().any(|p| p.contains("sync-conflict")));
    assert!(!m
        .list_profile("work", ".claude")
        .iter()
        .any(|p| p.contains("sync-conflict")));

    // Same-kind neighbors keep separate app-record dirs (per-profile
    // agent-sync remap).
    let config = m.saved_config();
    let remap_of = |id: &str| {
        let row = config.local_profiles.iter().find(|p| p.id == id).unwrap();
        crate::Roots::for_profile_with_home(row, m.home().to_path_buf())
            .unwrap()
            .remap
    };
    assert_eq!(remap_of("claude"), m.home().join(".agent-sync/claude"));
    assert_eq!(remap_of("work"), m.home().join(".agent-sync/work"));

    // A second machine linked only to storage 2 lives in that universe
    // alone: it receives the work profile's claude bytes and the fanned-out
    // codex tree; the personal claude content is nowhere on it.
    let n = Machine::new("N");
    n.pin_cloud_prefix(".claude", "001/.claude");
    n.pull_root(&c2, ".claude").await.expect("N pull claude");
    n.pull_root(&c2, ".codex").await.expect("N pull codex");
    assert_eq!(n.read(".claude/CLAUDE.md"), "work claude, different bytes\n");
    assert_eq!(n.read(".codex/memories/notes.md"), "codex notes v2, now longer\n");
}

#[tokio::test]
async fn s24_matrix_two_storages_three_profiles() {
    let _env = harness::lock_env().await;
    run_s24_matrix_two_storages_three_profiles(TestCloud::start().await, TestCloud::start().await)
        .await;
}

#[tokio::test]
async fn s24_matrix_two_storages_three_profiles_local() {
    let _env = harness::lock_env().await;
    run_s24_matrix_two_storages_three_profiles(
        TestCloud::start_local().await,
        TestCloud::start_local().await,
    )
    .await;
}

/// S25 — the remaining §3.2 save-diff rows: removing a storage forgets its
/// links' local state but never its bucket bytes; removing any local profile,
/// including a starter profile, forgets its links but never its disk files.
async fn run_s25_storage_and_profile_removal_cleanups(c1: TestCloud, c2: TestCloud) {
    let m = Machine::new("M");
    m.seed(".codex/memories/notes.md", "notes v1\n");
    m.seed(".claude/CLAUDE.md", "default claude\n");
    m.push(&c1, &[".codex"]).await.expect("push c1");
    m.push(&c2, &[".codex"]).await.expect("push c2");
    m.add_profile("work", ".claude", m.home().join("myconf"), None);
    m.seed_profile("work", ".claude/CLAUDE.md", "work claude\n");
    m.push_profile(&c1, "work", &[".claude"])
        .await
        .expect("push work");

    m.activate();
    let codex1 = c1.profile_for_root(".codex");
    let codex2 = c2.profile_for_root(".codex");
    let work_cloud = c1.profile_for_root(".claude");
    let b_codex1 = crate::baseline_path(m.handle(), "codex", &c1.storage_id, &codex1).unwrap();
    let b_codex2 = crate::baseline_path(m.handle(), "codex", &c2.storage_id, &codex2).unwrap();
    let b_work = crate::baseline_path(m.handle(), "work", &c1.storage_id, &work_cloud).unwrap();
    assert!(b_codex1.exists() && b_codex2.exists() && b_work.exists());

    // Remove storage c2 (and, as the settings UI does, its links): its
    // link state drops; its bucket and the sibling storage are untouched.
    let mut config = m.saved_config();
    config.storages.retain(|s| s.id != c2.storage_id);
    config.links.retain(|l| l.storage != c2.storage_id);
    crate::save_sync_config(m.handle().clone(), config)
        .await
        .unwrap();
    assert!(!b_codex2.exists(), "removed storage's baseline must drop");
    assert!(b_codex1.exists() && b_work.exists(), "c1 state untouched");
    assert_eq!(
        c2.head_of(&codex2).unwrap().generation,
        1,
        "cloud data stays (orphan philosophy)"
    );
    let result = m.push(&c1, &[".codex"]).await.expect("c1 still syncs");
    assert!(result.message.contains("up to date"), "{}", result.message);

    // Remove the custom profile: its link state drops; its mount's files
    // and its cloud profile are untouched.
    let mut config = m.saved_config();
    config.local_profiles.retain(|p| p.id != "work");
    config.links.retain(|l| l.profile != "work");
    crate::save_sync_config(m.handle().clone(), config)
        .await
        .unwrap();
    assert!(!b_work.exists(), "removed profile's baseline must drop");
    assert_eq!(m.read_profile("work", ".claude/CLAUDE.md"), "work claude\n");
    assert!(c1.head_of(&work_cloud).is_some(), "cloud profile stays");

    // Starter profiles are removable too, and removing one never touches its
    // local profile directory or files.
    let mut config = m.saved_config();
    config.local_profiles.retain(|p| p.id != "claude");
    config.links.retain(|l| l.profile != "claude");
    crate::save_sync_config(m.handle().clone(), config)
        .await
        .unwrap();
    assert!(!m
        .saved_config()
        .local_profiles
        .iter()
        .any(|p| p.id == "claude"));
    assert_eq!(m.read(".claude/CLAUDE.md"), "default claude\n");
}

#[tokio::test]
async fn s25_storage_and_profile_removal_cleanups() {
    let _env = harness::lock_env().await;
    run_s25_storage_and_profile_removal_cleanups(TestCloud::start().await, TestCloud::start().await)
        .await;
}

#[tokio::test]
async fn s25_storage_and_profile_removal_cleanups_local() {
    let _env = harness::lock_env().await;
    run_s25_storage_and_profile_removal_cleanups(
        TestCloud::start_local().await,
        TestCloud::start_local().await,
    )
    .await;
}

/// S26 — two local roots on ONE machine share one cloud profile (the
/// simulate-two-machines case): baselines are per link, so a stale sibling
/// never pushes old bytes back over a newer generation — it converges
/// instead, exactly like a second machine would.
async fn run_s26_two_local_roots_share_one_cloud_profile(cloud: TestCloud) {
    let m = Machine::new("M");
    m.seed(".claude/CLAUDE.md", "v1\n");
    m.push(&cloud, &[".claude"]).await.expect("push v1");
    let profile_id = cloud.profile_for_root(".claude");

    // Second root, same machine, pinned to the SAME cloud profile.
    m.add_profile("conf4", ".claude", m.home().join("myconf4"), Some(&profile_id));
    m.pull_profile(&cloud, "conf4").await.expect("pull conf4");
    assert_eq!(m.read_profile("conf4", ".claude/CLAUDE.md"), "v1\n");
    assert_eq!(
        cloud.profiles_for_root(".claude"),
        vec![profile_id.clone()],
        "linking must not create a second profile"
    );

    // The two links keep independent baselines.
    m.activate();
    let b_claude =
        crate::baseline_path(m.handle(), "claude", &cloud.storage_id, &profile_id).unwrap();
    let b_conf4 =
        crate::baseline_path(m.handle(), "conf4", &cloud.storage_id, &profile_id).unwrap();
    assert_ne!(b_claude, b_conf4);
    assert!(b_claude.exists() && b_conf4.exists());

    // Default root advances the profile; conf4 is now one generation stale.
    m.seed(".claude/CLAUDE.md", "v2, longer than before\n");
    m.push(&cloud, &[".claude"]).await.expect("push v2");
    assert_eq!(cloud.head(".claude").generation, 2);

    // Stale conf4 pushes WITHOUT pulling. Its file matches its own baseline
    // (v1 == v1), so the union classifies it cloud-ahead and converges —
    // with a shared baseline it would read as a local edit and republish v1.
    m.push_profile(&cloud, "conf4", &[".claude"])
        .await
        .expect("stale push converges");
    assert_eq!(
        cloud.manifest_file_sha(".claude", ".claude/CLAUDE.md"),
        Some(crate::sha256_bytes(b"v2, longer than before\n")),
        "stale sibling must never clobber the newer generation"
    );
    assert_eq!(m.read_profile("conf4", ".claude/CLAUDE.md"), "v2, longer than before\n");

    // Divergent edits still go through the normal conflict machinery: both
    // sides survive (union), nothing is silently lost.
    m.seed(".claude/CLAUDE.md", "default edit\n");
    m.seed_profile("conf4", ".claude/CLAUDE.md", "conf4 edit\n");
    m.push(&cloud, &[".claude"]).await.expect("push default edit");
    m.push_profile(&cloud, "conf4", &[".claude"])
        .await
        .expect("push conf4 edit");
    m.pull(&cloud).await.expect("pull default");
    m.pull_profile(&cloud, "conf4").await.expect("pull conf4");
    assert_eq!(
        m.read(".claude/CLAUDE.md"),
        m.read_profile("conf4", ".claude/CLAUDE.md"),
        "both roots converge on the conflict winner"
    );
    let siblings = m.list(".claude");
    assert!(
        siblings.iter().any(|rel| rel.contains("conflict")),
        "the losing side must survive as a conflict sibling: {:?}",
        siblings
    );
}

#[tokio::test]
async fn s26_two_local_roots_share_one_cloud_profile() {
    let _env = harness::lock_env().await;
    run_s26_two_local_roots_share_one_cloud_profile(TestCloud::start().await).await;
}

#[tokio::test]
async fn s26_two_local_roots_share_one_cloud_profile_local() {
    let _env = harness::lock_env().await;
    run_s26_two_local_roots_share_one_cloud_profile(TestCloud::start_local().await).await;
}

// ── Shared cloud profiles & picker semantics (PLAN_SHARED_PROFILE_TESTS.md) ──

/// S27 — relay convergence through one shared profile, and a sibling root
/// behaves exactly like a second machine joining the same profile.
async fn run_s27_shared_profile_relay_convergence(cloud: TestCloud) {
    let m = Machine::new("M");
    m.seed(".claude/CLAUDE.md", "v1\n");
    m.push(&cloud, &[".claude"]).await.expect("A push v1");
    let p = cloud.profile_for_root(".claude");

    m.add_profile("conf4", ".claude", m.home().join("myconf4"), Some(&p));
    m.pull_profile(&cloud, "conf4").await.expect("B pull v1");
    assert_eq!(m.read_profile("conf4", ".claude/CLAUDE.md"), "v1\n");

    // B edits and pushes; A pulls — the relay through one profile.
    m.seed_profile("conf4", ".claude/CLAUDE.md", "v2 from conf4\n");
    m.push_profile(&cloud, "conf4", &[".claude"])
        .await
        .expect("B push v2");
    m.pull_root(&cloud, ".claude").await.expect("A pull v2");
    assert_eq!(m.read(".claude/CLAUDE.md"), "v2 from conf4\n");

    // A real second machine joins the same profile: same behavior class.
    let n = Machine::new("N");
    n.pin_cloud_prefix(".claude", &p);
    n.pull_root(&cloud, ".claude").await.expect("N pull v2");
    assert_eq!(n.read(".claude/CLAUDE.md"), "v2 from conf4\n");
    n.seed(".claude/CLAUDE.md", "v3 from machine N\n");
    n.push(&cloud, &[".claude"]).await.expect("N push v3");
    m.pull_root(&cloud, ".claude").await.expect("A pull v3");
    m.pull_profile(&cloud, "conf4").await.expect("B pull v3");
    assert_eq!(m.read(".claude/CLAUDE.md"), "v3 from machine N\n");
    assert_eq!(m.read_profile("conf4", ".claude/CLAUDE.md"), "v3 from machine N\n");

    // Sequential edits: one profile, linear history, zero conflict copies.
    assert_eq!(cloud.profiles_for_root(".claude"), vec![p.clone()]);
    assert!(m.list(".claude").iter().all(|r| !r.contains("sync-conflict")));
    assert!(m.list_profile("conf4", ".claude").iter().all(|r| !r.contains("sync-conflict")));

    // Per-link state: two distinct baselines on M, both live.
    m.activate();
    let ba = crate::baseline_path(m.handle(), "claude", &cloud.storage_id, &p).unwrap();
    let bb = crate::baseline_path(m.handle(), "conf4", &cloud.storage_id, &p).unwrap();
    assert_ne!(ba, bb);
    assert!(ba.exists() && bb.exists());
}

#[tokio::test]
async fn s27_shared_profile_relay_convergence() {
    let _env = harness::lock_env().await;
    run_s27_shared_profile_relay_convergence(TestCloud::start().await).await;
}

#[tokio::test]
async fn s27_shared_profile_relay_convergence_local() {
    let _env = harness::lock_env().await;
    run_s27_shared_profile_relay_convergence(TestCloud::start_local().await).await;
}

/// S28 — the cloud cache is shared per (storage, cloud profile), but
/// statuses stay per link: after sibling A pushes, B reads `cloud-ahead`
/// against its own baseline while A reads `synced` from the same cache.
async fn run_s28_per_link_statuses_shared_cache(cloud: TestCloud) {
    let m = Machine::new("M");
    m.seed(".claude/CLAUDE.md", "v1\n");
    m.push(&cloud, &[".claude"]).await.expect("A push v1");
    let p = cloud.profile_for_root(".claude");
    m.add_profile("conf4", ".claude", m.home().join("myconf4"), Some(&p));
    m.pull_profile(&cloud, "conf4").await.expect("B pull v1");

    // A advances the shared profile; its push refreshes the shared cache.
    m.seed(".claude/CLAUDE.md", "v2, longer than before\n");
    m.push(&cloud, &[".claude"]).await.expect("A push v2");

    let status_of = |profile: &str, file: &str| {
        let file = file.to_string();
        let profile = profile.to_string();
        let storage = cloud.storage_id.clone();
        let handle = m.handle().clone();
        async move {
            let report = crate::get_file_statuses(handle, profile, Some(storage), vec![file.clone()])
                .await
                .unwrap();
            report.statuses.get(&file).cloned()
        }
    };
    let file_a = m.path(".claude/CLAUDE.md").to_string_lossy().into_owned();
    let file_b = m
        .profile_path("conf4", ".claude/CLAUDE.md")
        .to_string_lossy()
        .into_owned();
    assert_eq!(status_of("claude", &file_a).await.as_deref(), Some("synced"));
    assert_eq!(status_of("conf4", &file_b).await.as_deref(), Some("cloud-ahead"));

    m.pull_profile(&cloud, "conf4").await.expect("B pull v2");
    assert_eq!(status_of("conf4", &file_b).await.as_deref(), Some("synced"));
    assert_eq!(m.read_profile("conf4", ".claude/CLAUDE.md"), "v2, longer than before\n");
}

#[tokio::test]
async fn s28_per_link_statuses_shared_cache() {
    let _env = harness::lock_env().await;
    run_s28_per_link_statuses_shared_cache(TestCloud::start().await).await;
}

#[tokio::test]
async fn s28_per_link_statuses_shared_cache_local() {
    let _env = harness::lock_env().await;
    run_s28_per_link_statuses_shared_cache(TestCloud::start_local().await).await;
}

/// S29 — resolving a conflict sibling from one local root propagates the
/// (manifest-only) deletion through the shared profile: the sibling copy
/// disappears on the other root's next pull, main content intact.
async fn run_s29_conflict_resolution_reaches_sibling(cloud: TestCloud) {
    let m = Machine::new("M");
    m.seed(".claude/CLAUDE.md", "base\n");
    m.push(&cloud, &[".claude"]).await.expect("A push base");
    let p = cloud.profile_for_root(".claude");
    m.add_profile("conf4", ".claude", m.home().join("myconf4"), Some(&p));
    m.pull_profile(&cloud, "conf4").await.expect("B pull base");

    // Divergent edits; union keeps both sides, loser as a conflict sibling.
    m.seed(".claude/CLAUDE.md", "edited by A\n");
    m.seed_profile("conf4", ".claude/CLAUDE.md", "edited by B, differently\n");
    m.push(&cloud, &[".claude"]).await.expect("A push edit");
    m.push_profile(&cloud, "conf4", &[".claude"])
        .await
        .expect("B push edit");
    m.pull_root(&cloud, ".claude").await.expect("A pull union");
    m.pull_profile(&cloud, "conf4").await.expect("B pull union");
    assert_eq!(m.read(".claude/CLAUDE.md"), m.read_profile("conf4", ".claude/CLAUDE.md"));
    let sibling = m
        .list_profile("conf4", ".claude")
        .into_iter()
        .find(|rel| rel.contains("sync-conflict"))
        .expect("conflict sibling must exist on B");
    assert!(
        m.list(".claude").iter().any(|rel| rel.contains("sync-conflict")),
        "conflict sibling must exist on A too"
    );

    // Resolve from B: explicit action, publishes to B's links on P.
    m.activate();
    let source = m
        .profile_path("conf4", &sibling)
        .to_string_lossy()
        .into_owned();
    crate::resolve_conflict_copy(m.handle().clone(), source)
        .await
        .expect("resolve from B");
    assert!(!m.profile_path("conf4", &sibling).exists());

    // A pulls: the published resolution removes A's unchanged review copy.
    m.pull_root(&cloud, ".claude").await.expect("A pull resolution");
    assert!(
        m.list(".claude").iter().all(|rel| !rel.contains("sync-conflict")),
        "resolution must reach the sibling root"
    );
    assert_eq!(m.read(".claude/CLAUDE.md"), m.read_profile("conf4", ".claude/CLAUDE.md"));
}

#[tokio::test]
async fn s29_conflict_resolution_reaches_sibling() {
    let _env = harness::lock_env().await;
    run_s29_conflict_resolution_reaches_sibling(TestCloud::start().await).await;
}

#[tokio::test]
async fn s29_conflict_resolution_reaches_sibling_local() {
    let _env = harness::lock_env().await;
    run_s29_conflict_resolution_reaches_sibling(TestCloud::start_local().await).await;
}

/// S30 — the picker's core case (the myconf4 fix): with several profiles in
/// the storage, an explicit pinned pick pulls exactly that profile's files —
/// including the plugin lock, the plugin-repair precondition — and neither
/// creates nor relinks anything.
async fn run_s30_pick_existing_profile_among_several(cloud: TestCloud) {
    let m = Machine::new("M");
    m.seed(".claude/CLAUDE.md", "real content\n");
    m.seed(".claude/agent-sync/claude-plugins.lock.json", "{\"schema\":1}\n");
    m.push(&cloud, &[".claude"]).await.expect("seed P1");
    let p1 = cloud.profile_for_root(".claude");

    // The old bug's leftover shape: a second, empty profile.
    m.add_profile("junk", ".claude", m.home().join("junk"), Some("claude-empty"));
    m.pull_profile(&cloud, "junk").await.expect("materialize empty profile");
    assert_eq!(cloud.profiles_for_root(".claude").len(), 2);

    // Fresh root picks P1 explicitly — exactly what the picker persists.
    m.add_profile("conf4", ".claude", m.home().join("myconf4"), None);
    m.pick_cloud_profile(&cloud, "conf4", &p1, "Claude");
    m.pull_profile(&cloud, "conf4").await.expect("pull the picked profile");
    assert_eq!(m.read_profile("conf4", ".claude/CLAUDE.md"), "real content\n");
    assert!(
        m.profile_path("conf4", ".claude/agent-sync/claude-plugins.lock.json")
            .exists(),
        "the synced plugin lock must land — plugin repair's precondition"
    );

    // Nothing created, nothing silently relinked, empty profile untouched.
    assert_eq!(cloud.profiles_for_root(".claude").len(), 2);
    assert_eq!(cloud.head_of("claude-empty").unwrap().generation, 0);
    let link = m
        .saved_config()
        .links
        .iter()
        .find(|l| l.profile == "conf4" && l.storage == cloud.storage_id)
        .map(|l| l.cloud.clone())
        .expect("conf4 link");
    assert_eq!(link.profile_id, p1);
    assert!(link.pinned);
}

#[tokio::test]
async fn s30_pick_existing_profile_among_several() {
    let _env = harness::lock_env().await;
    run_s30_pick_existing_profile_among_several(TestCloud::start().await).await;
}

#[tokio::test]
async fn s30_pick_existing_profile_among_several_local() {
    let _env = harness::lock_env().await;
    run_s30_pick_existing_profile_among_several(TestCloud::start_local().await).await;
}

/// S31 — the auto path (`cloud: {}`) is only safe alone: one candidate
/// auto-links (unpinned, nothing created); two candidates fail loudly and
/// leave the storage untouched. This is why the picker writes explicit picks.
async fn run_s31_auto_link_single_candidate_only(cloud: TestCloud) {
    let m = Machine::new("M");
    m.seed(".claude/CLAUDE.md", "v1\n");
    m.push(&cloud, &[".claude"]).await.expect("seed P1");
    let p1 = cloud.profile_for_root(".claude");

    m.add_profile("auto1", ".claude", m.home().join("auto1"), None);
    m.pull_profile(&cloud, "auto1").await.expect("auto links the only profile");
    assert_eq!(m.read_profile("auto1", ".claude/CLAUDE.md"), "v1\n");
    assert_eq!(cloud.profiles_for_root(".claude"), vec![p1.clone()]);
    let link = m
        .saved_config()
        .links
        .iter()
        .find(|l| l.profile == "auto1" && l.storage == cloud.storage_id)
        .map(|l| l.cloud.clone())
        .expect("auto1 link");
    assert_eq!(link.profile_id, p1);
    assert!(!link.pinned, "auto-discovered links stay unpinned");

    // A second candidate turns auto ambiguous: loud error, no side effects.
    m.add_profile("junk", ".claude", m.home().join("junk"), Some("claude-empty"));
    m.pull_profile(&cloud, "junk").await.expect("materialize second profile");
    m.add_profile("auto2", ".claude", m.home().join("auto2"), None);
    let err = m.pull_profile(&cloud, "auto2").await.unwrap_err();
    assert!(err.contains("pin one explicitly"), "unexpected error: {}", err);
    assert_eq!(cloud.profiles_for_root(".claude").len(), 2, "nothing created");
}

#[tokio::test]
async fn s31_auto_link_single_candidate_only() {
    let _env = harness::lock_env().await;
    run_s31_auto_link_single_candidate_only(TestCloud::start().await).await;
}

#[tokio::test]
async fn s31_auto_link_single_candidate_only_local() {
    let _env = harness::lock_env().await;
    run_s31_auto_link_single_candidate_only(TestCloud::start_local().await).await;
}

/// S32 — "Create new profile" actually creates (P1 fix): a fresh pinned id
/// with a deduped label lands alongside the existing profile, which stays
/// untouched; the probe the picker consumes shows both, distinctly labeled.
async fn run_s32_create_new_alongside_existing(cloud: TestCloud) {
    let m = Machine::new("M");
    m.seed(".claude/CLAUDE.md", "existing profile\n");
    m.push(&cloud, &[".claude"]).await.expect("seed P1");
    let p1 = cloud.profile_for_root(".claude");
    let gen_p1 = cloud.head_of(&p1).unwrap().generation;

    m.add_profile("confd", ".claude", m.home().join("confd"), None);
    m.pick_cloud_profile(&cloud, "confd", "ab12cd34", "Claude 2");
    m.seed_profile("confd", ".claude/CLAUDE.md", "brand new profile\n");
    m.push_profile(&cloud, "confd", &[".claude"])
        .await
        .expect("push creates the new profile");

    let mut ids = cloud.profiles_for_root(".claude");
    ids.sort();
    let mut expected = vec![p1.clone(), "ab12cd34".to_string()];
    expected.sort();
    assert_eq!(ids, expected);
    assert_eq!(cloud.head_of(&p1).unwrap().generation, gen_p1, "P1 untouched");
    assert_eq!(
        cloud.manifest_of("ab12cd34").files[".claude/CLAUDE.md"].sha256,
        crate::sha256_bytes(b"brand new profile\n")
    );
    m.activate();
    assert!(crate::baseline_path(m.handle(), "confd", &cloud.storage_id, "ab12cd34")
        .unwrap()
        .exists());

    let infos = crate::list_sync_profiles(m.handle().clone(), cloud.storage_id.clone())
        .await
        .unwrap();
    let labels: Vec<String> = infos
        .iter()
        .filter(|info| info.root == ".claude")
        .map(|info| info.label.clone())
        .collect();
    assert!(
        labels.contains(&"Claude".to_string()) && labels.contains(&"Claude 2".to_string()),
        "distinct labels expected, got {:?}",
        labels
    );
}

#[tokio::test]
async fn s32_create_new_alongside_existing() {
    let _env = harness::lock_env().await;
    run_s32_create_new_alongside_existing(TestCloud::start().await).await;
}

#[tokio::test]
async fn s32_create_new_alongside_existing_local() {
    let _env = harness::lock_env().await;
    run_s32_create_new_alongside_existing(TestCloud::start_local().await).await;
}

/// S33 — re-picking a link's cloud profile through the real settings save
/// drops the old baseline (P2 fix), re-verifies against the new profile
/// with zero conflict copies, and a later pick back starts clean; a sibling
/// link on the old profile keeps its baseline throughout.
async fn run_s33_repick_resets_link_state(cloud: TestCloud) {
    let m = Machine::new("M");
    m.seed(".claude/CLAUDE.md", "p1 content\n");
    m.push(&cloud, &[".claude"]).await.expect("A push P1");
    let p1 = cloud.profile_for_root(".claude");
    m.add_profile("other", ".claude", m.home().join("other"), Some("claude-2"));
    m.seed_profile("other", ".claude/settings.json", "{\"p2\":true}\n");
    m.push_profile(&cloud, "other", &[".claude"]).await.expect("push P2");

    // The sibling that must stay unaffected.
    m.add_profile("conf4", ".claude", m.home().join("myconf4"), Some(&p1));
    m.pull_profile(&cloud, "conf4").await.expect("sibling pull");

    m.activate();
    let baseline = |local: &str, profile: &str| {
        crate::baseline_path(m.handle(), local, &cloud.storage_id, profile).unwrap()
    };
    assert!(baseline("claude", &p1).exists());
    assert!(baseline("conf4", &p1).exists());

    // Re-pick A's cell P1 → P2 through the real settings save.
    let repick = |mut config: crate::SyncConfig, target: String| {
        let link = config
            .links
            .iter_mut()
            .find(|l| l.profile == "claude" && l.storage == cloud.storage_id)
            .expect("claude link");
        link.cloud = crate::ProfileLink {
            root: ".claude".to_string(),
            profile_id: target,
            pinned: true,
            ..Default::default()
        };
        config
    };
    crate::save_sync_config(m.handle().clone(), repick(m.saved_config(), "claude-2".to_string()))
        .await
        .unwrap();
    assert!(!baseline("claude", &p1).exists(), "re-pick must drop the old baseline");
    assert!(baseline("conf4", &p1).exists(), "sibling baseline untouched");

    // Re-verify against P2: its file lands, local extras stay, no conflicts.
    m.pull_root(&cloud, ".claude").await.expect("pull P2");
    assert_eq!(m.read(".claude/settings.json"), "{\"p2\":true}\n");
    assert_eq!(m.read(".claude/CLAUDE.md"), "p1 content\n");
    assert!(m.list(".claude").iter().all(|r| !r.contains("sync-conflict")));

    // Pick back to P1: the old baseline is gone, so this starts clean too.
    crate::save_sync_config(m.handle().clone(), repick(m.saved_config(), p1.clone()))
        .await
        .unwrap();
    assert!(!baseline("claude", "claude-2").exists(), "P2 baseline dropped too");
    m.pull_root(&cloud, ".claude").await.expect("pull P1 again");
    assert_eq!(m.read(".claude/CLAUDE.md"), "p1 content\n");
    assert_eq!(m.read(".claude/settings.json"), "{\"p2\":true}\n");
    assert!(m.list(".claude").iter().all(|r| !r.contains("sync-conflict")));
    assert!(baseline("conf4", &p1).exists(), "sibling still untouched");
}

#[tokio::test]
async fn s33_repick_resets_link_state() {
    let _env = harness::lock_env().await;
    run_s33_repick_resets_link_state(TestCloud::start().await).await;
}

#[tokio::test]
async fn s33_repick_resets_link_state_local() {
    let _env = harness::lock_env().await;
    run_s33_repick_resets_link_state(TestCloud::start_local().await).await;
}

/// S34 — one-name model (PLAN_PROFILE_NAMES.md): naming a local profile
/// renames the shared cloud profile on the next push (tag label + the
/// renamer's saved link); another machine's later push preserves the new
/// name instead of stamping its stale cached copy back, and adopts it into
/// that machine's saved link.
async fn run_s34_rename_propagates_and_survives_push(cloud: TestCloud) {
    let a = Machine::new("A");
    a.seed(".claude/CLAUDE.md", "v1\n");
    a.push(&cloud, &[".claude"]).await.expect("A creates profile");
    let profile = cloud.profile_for_root(".claude");

    let b = Machine::new("B");
    b.pull(&cloud).await.expect("B links");

    // A names its local profile; the next push renames the cloud profile.
    a.activate();
    let mut cfg = a.saved_config();
    cfg.local_profiles
        .iter_mut()
        .find(|p| p.id == "claude")
        .unwrap()
        .name = "Team Claude".to_string();
    crate::save_sync_config(a.handle().clone(), cfg).await.unwrap();
    a.seed(".claude/CLAUDE.md", "v2 longer\n");
    a.push(&cloud, &[".claude"]).await.expect("A renaming push");

    a.activate();
    let infos = crate::list_sync_profiles(a.handle().clone(), cloud.storage_id.clone())
        .await
        .unwrap();
    let label = &infos.iter().find(|i| i.profile_id == profile).unwrap().label;
    assert_eq!(label, "Team Claude", "push must rename the cloud profile");
    assert_eq!(
        a.saved_link(&cloud, ".claude").unwrap().profile_label,
        "Team Claude",
        "renamer's saved link adopts on push"
    );

    // B pushes without a custom name: preserve, never revert.
    b.seed(".claude/settings.json", "{\"b\":true}\n");
    b.push(&cloud, &[".claude"]).await.expect("B push");

    b.activate();
    let infos = crate::list_sync_profiles(b.handle().clone(), cloud.storage_id.clone())
        .await
        .unwrap();
    let label = &infos.iter().find(|i| i.profile_id == profile).unwrap().label;
    assert_eq!(label, "Team Claude", "B's push must not revert the rename");
    assert_eq!(
        b.saved_link(&cloud, ".claude").unwrap().profile_label,
        "Team Claude",
        "B's cached link label heals on push"
    );
}

#[tokio::test]
async fn s34_rename_propagates_and_survives_push() {
    let _env = harness::lock_env().await;
    run_s34_rename_propagates_and_survives_push(TestCloud::start().await).await;
}

#[tokio::test]
async fn s34_rename_propagates_and_survives_push_local() {
    let _env = harness::lock_env().await;
    run_s34_rename_propagates_and_survives_push(TestCloud::start_local().await).await;
}

/// S35 — the live bug repro (PLAN_PROFILE_NAMES.md revision): ONE machine,
/// two local profiles sharing one cloud profile (the user's myconf2 +
/// myconf4 setup). Renaming one local profile and pushing must rename the
/// cloud profile (the tag label the storage rows display) and heal BOTH
/// links' cached labels in the saved config; the unnamed sibling's own
/// later push keeps the new name.
async fn run_s35_shared_profile_rename_one_machine(cloud: TestCloud) {
    let m = Machine::new("M");
    m.seed(".claude/CLAUDE.md", "base\n");
    m.push(&cloud, &[".claude"]).await.expect("create profile");
    let profile = cloud.profile_for_root(".claude");

    // Sibling local profile on the same machine, sharing the cloud profile.
    m.add_profile("conf4", ".claude", m.home().join("myconf4"), Some(&profile));
    m.pull_profile(&cloud, "conf4").await.expect("sibling links");

    // Rename the default profile; its next push renames the cloud profile
    // even with NOTHING to publish (the user's exact repro: rename, push,
    // zero changed files — the early no-change exit must still land it).
    m.activate();
    let mut cfg = m.saved_config();
    cfg.local_profiles
        .iter_mut()
        .find(|p| p.id == "claude")
        .unwrap()
        .name = "testclaude".to_string();
    crate::save_sync_config(m.handle().clone(), cfg).await.unwrap();
    let generation = cloud.head_of(&profile).unwrap().generation;
    m.push(&cloud, &[".claude"]).await.expect("renaming push");
    assert_eq!(
        cloud.head_of(&profile).unwrap().generation,
        generation,
        "no-change push must not publish a generation"
    );

    // The probe the storage rows display must show the new name.
    m.activate();
    let infos = crate::list_sync_profiles(m.handle().clone(), cloud.storage_id.clone())
        .await
        .unwrap();
    let label = &infos.iter().find(|i| i.profile_id == profile).unwrap().label;
    assert_eq!(label, "testclaude", "push must rename the cloud profile");

    // One push heals every cached link label on this machine — the
    // sibling's row too, since both point at the same cloud profile.
    let links = m.saved_links(&cloud);
    assert_eq!(links.len(), 2, "both links resolved, got {:?}", links);
    for (local_id, link) in &links {
        assert_eq!(
            link.profile_label, "testclaude",
            "link '{}' must carry the renamed label",
            local_id
        );
    }

    // The unnamed sibling's own push must not revert the name.
    m.seed_profile("conf4", ".claude/settings.json", "{\"conf4\":true}\n");
    m.push_profile(&cloud, "conf4", &[".claude"])
        .await
        .expect("sibling push");
    m.activate();
    let infos = crate::list_sync_profiles(m.handle().clone(), cloud.storage_id.clone())
        .await
        .unwrap();
    let label = &infos.iter().find(|i| i.profile_id == profile).unwrap().label;
    assert_eq!(label, "testclaude", "sibling push must not revert the rename");
}

#[tokio::test]
async fn s35_shared_profile_rename_one_machine() {
    let _env = harness::lock_env().await;
    run_s35_shared_profile_rename_one_machine(TestCloud::start().await).await;
}

#[tokio::test]
async fn s35_shared_profile_rename_one_machine_local() {
    let _env = harness::lock_env().await;
    run_s35_shared_profile_rename_one_machine(TestCloud::start_local().await).await;
}
