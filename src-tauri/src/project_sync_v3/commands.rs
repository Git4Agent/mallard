//! Tauri command surface for the schema-3 local vertical slice.
//!
//! This layer composes provider capture, the bundle engine, and local
//! persistence. Schema 3 currently has a complete local-folder storage path;
//! S3 is rejected explicitly until a CAS-capable schema-3 adapter is wired in.

use super::bundle_engine::{
    BundleEngine, BundleObjectStore, CasExpectation, CasOutcome, FetchedBundle,
    ImmutablePutOutcome, LocalBundleObjectStore, ObjectKey, ObjectPrefix, PublishBundleRequest,
    PublishExpectation, RemoteBundlePage, StoreListPage, StoredObject,
};
use super::domain::{
    generated_named_id, validate_absolute_clean_path, ActionId, ActionStatus, ApplyPolicy,
    BindingState, BundleId, BundleIdentity, BundleKind, BundleRecipe, BundleSnapshot, CapturedWith,
    DependencyAction, DependencyActionKind, DependencyApplicationRecord, DependencyApplyReceipt,
    DependencyPlan, LocalProjectId, LocalProjectRegistration, LocalProviderProfileId,
    MaterializationId, MaterializationRecord, MaterializationStatus, PlanId, ProjectBinding,
    ProjectStorageLink, Provenance, Provider, ProviderProfile, RecipeBase, RecipeEntry, ReplicaId,
    ResourceDescriptor, ResourceId, ResourceKind, ResourceScope, RestoreActionType, RestorePlan,
    StorageConfigV3, StorageId, StorageKind, SyncConfigV3, DEPENDENCY_PLAN_SCHEMA_V1,
};
use super::persistence::V3Repository;
use super::provider_capture::{
    self, CaptureApplyPolicy, CaptureRequest, CaptureResourceKind, Provider as CaptureProvider,
    ResourceCandidate,
};
use super::s3_store::S3BundleObjectStore;
use crate::emit_log;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::process::Command;
use tokio::time::{timeout, Duration};

const DEFAULT_OPERATION_TIMEOUT: Duration = Duration::from_secs(120);
const PLAN_LIFETIME_SECS: u64 = 24 * 60 * 60;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RegisterLocalProjectRequest {
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_fingerprint: Option<String>,
    /// Linking a remote bundle supplies its ID; a local-first project omits
    /// it and gets one generated before any storage publication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<BundleId>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SaveProjectLinkRequest {
    pub local_project_id: LocalProjectId,
    pub storage_id: StorageId,
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ConnectProjectBundleRequest {
    pub local_project_id: LocalProjectId,
    pub storage_id: StorageId,
    pub bundle_id: BundleId,
    /// Reject a stale chooser instead of replacing an identity that changed
    /// while the user was deciding which remote bundle to connect.
    pub expected_bundle_id: BundleId,
    #[serde(default)]
    pub pinned: bool,
    /// A manual chooser may connect a checkout to a repo with a different or
    /// missing Git fingerprint after presenting that mismatch to the user.
    #[serde(default)]
    pub allow_repository_mismatch: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SaveProjectBindingRequest {
    pub local_project_id: LocalProjectId,
    pub project_root: String,
    #[serde(default)]
    pub profile_ids: BTreeMap<Provider, LocalProviderProfileId>,
    /// Required when changing an existing binding.  Creation uses `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
}

#[derive(Serialize, Clone, Debug)]
pub struct ProjectDetail {
    pub project: LocalProjectRegistration,
    pub links: Vec<ProjectStorageLink>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding: Option<ProjectBinding>,
    pub materializations: Vec<MaterializationRecord>,
}

#[derive(Serialize, Clone, Debug)]
pub struct InventoryResource {
    #[serde(flatten)]
    pub descriptor: ResourceDescriptor,
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub logical_paths: Vec<String>,
    pub default_selected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provided_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_behavior: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct ResourceInventory {
    pub project: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<BundleId>,
    pub resources: Vec<InventoryResource>,
    pub recipe: BundleRecipe,
    pub generated_at: u64,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct ProjectDiscovery {
    pub project_root: String,
    pub display_name: String,
    pub inventory: ResourceInventory,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_fingerprint: Option<String>,
    #[serde(default)]
    pub providers: Vec<Provider>,
    #[serde(default)]
    pub profile_ids: BTreeMap<Provider, LocalProviderProfileId>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct ProviderProfileSummary {
    #[serde(flatten)]
    pub profile: ProviderProfile,
    pub available: bool,
    pub readable: bool,
    pub writable: bool,
    #[serde(default)]
    pub used_by_projects: Vec<LocalProjectId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct ProviderProfileProbe {
    pub provider: Provider,
    pub requested_path: String,
    pub resolved_path: String,
    pub canonical_path: String,
    pub suggested_name: String,
    pub readable: bool,
    pub writable: bool,
    pub detected_child: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub existing_profile_id: Option<LocalProviderProfileId>,
}

#[derive(Serialize, Clone, Debug)]
pub struct BundleResourceStatus {
    pub resource_id: ResourceId,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_digest: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct ResourceStatusReport {
    pub project: String,
    pub storage: String,
    pub bundle_id: BundleId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    pub statuses: Vec<BundleResourceStatus>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct OperationResourceResult {
    pub resource_id: ResourceId,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct ProjectOperationResult {
    pub success: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources_changed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(default)]
    pub results: Vec<OperationResourceResult>,
}

#[derive(Serialize, Clone, Debug)]
pub struct RemoteBundleSummaryDto {
    pub bundle_id: BundleId,
    pub display_name: String,
    pub kind: BundleKind,
    pub generation: u64,
    pub updated_at: u64,
    pub resource_count: u64,
}

#[derive(Serialize, Clone, Debug)]
pub struct BundlePage {
    pub bundles: Vec<RemoteBundleSummaryDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct BundleSnapshotSummary {
    pub storage_id: StorageId,
    pub bundle_id: BundleId,
    pub display_name: String,
    pub kind: BundleKind,
    pub generation: u64,
    pub updated_at: u64,
    pub resource_count: usize,
    #[serde(default)]
    pub providers: Vec<Provider>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_fingerprint: Option<String>,
    pub resources: Vec<ResourceDescriptor>,
    pub recipe: BundleRecipe,
    pub fetched_at: u64,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct FailedAction {
    pub action_id: ActionId,
    pub message: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct RestoreResult {
    pub success: bool,
    pub message: String,
    pub plan_id: PlanId,
    #[serde(default)]
    pub applied_action_ids: Vec<ActionId>,
    #[serde(default)]
    pub failed_actions: Vec<FailedAction>,
}

#[derive(Serialize, Clone, Debug)]
pub struct DependencyResult {
    pub success: bool,
    pub message: String,
    #[serde(default)]
    pub applied_action_ids: Vec<ActionId>,
    #[serde(default)]
    pub failed_actions: Vec<FailedAction>,
}

#[derive(Serialize, Clone, Debug)]
pub struct BundleReadinessIssue {
    pub issue_id: String,
    pub category: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub severity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<Provider>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<ResourceId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct BundleReadiness {
    pub bundle_id: BundleId,
    pub state: String,
    pub issues: Vec<BundleReadinessIssue>,
    pub generated_at: u64,
}

fn repository<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<V3Repository, String> {
    V3Repository::from_app(app)
}

fn project_storage_log_labels(
    repository: &V3Repository,
    local_project_id: &LocalProjectId,
    storage_id: &StorageId,
) -> (String, String) {
    let config = repository.load_config().ok();
    let project = config
        .as_ref()
        .and_then(|config| config.project(local_project_id))
        .map(|project| project.display_name.clone())
        .unwrap_or_else(|| local_project_id.to_string());
    let storage = config
        .as_ref()
        .and_then(|config| {
            config
                .storages
                .iter()
                .find(|storage| &storage.id == storage_id)
        })
        .map(|storage| storage.name.clone())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| storage_id.to_string());
    (project, storage)
}

fn storage_log_label(repository: &V3Repository, storage_id: &StorageId) -> String {
    repository
        .load_config()
        .ok()
        .and_then(|config| {
            config
                .storages
                .into_iter()
                .find(|storage| &storage.id == storage_id)
        })
        .map(|storage| storage.name)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| storage_id.to_string())
}

async fn run_blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(operation)
        .await
        .map_err(|error| format!("project-sync worker failed: {}", error))?
}

#[tauri::command]
pub async fn get_project_sync_config(app: tauri::AppHandle) -> Result<SyncConfigV3, String> {
    repository(&app)?.load_config()
}

#[tauri::command]
pub async fn save_project_sync_config(
    app: tauri::AppHandle,
    config: SyncConfigV3,
) -> Result<SyncConfigV3, String> {
    save_project_sync_config_with_repository(&repository(&app)?, config)
}

fn save_project_sync_config_with_repository(
    repository: &V3Repository,
    config: SyncConfigV3,
) -> Result<SyncConfigV3, String> {
    let bindings = repository.load_bindings()?;
    bindings.validate(&config)?;
    validate_config_storage_isolation(repository, &config, &bindings.bindings, &bindings.profiles)?;
    repository.save_config(config)
}

#[tauri::command]
pub async fn list_local_projects(
    app: tauri::AppHandle,
) -> Result<Vec<LocalProjectRegistration>, String> {
    Ok(repository(&app)?.load_config()?.projects)
}

#[tauri::command]
pub async fn get_project(
    app: tauri::AppHandle,
    local_project_id: LocalProjectId,
) -> Result<Option<ProjectDetail>, String> {
    get_project_with_repository(&repository(&app)?, &local_project_id)
}

#[tauri::command]
pub async fn get_local_project(
    app: tauri::AppHandle,
    local_project_id: LocalProjectId,
) -> Result<Option<LocalProjectRegistration>, String> {
    Ok(repository(&app)?
        .load_config()?
        .projects
        .into_iter()
        .find(|project| project.local_project_id == local_project_id))
}

#[tauri::command]
pub async fn register_local_project(
    app: tauri::AppHandle,
    request: RegisterLocalProjectRequest,
) -> Result<LocalProjectRegistration, String> {
    register_local_project_with_repository(&repository(&app)?, request)
}

#[tauri::command]
pub async fn remove_local_project(
    app: tauri::AppHandle,
    local_project_id: LocalProjectId,
) -> Result<bool, String> {
    remove_local_project_with_repository(&repository(&app)?, &local_project_id)
}

#[tauri::command]
pub async fn save_bundle_recipe(
    app: tauri::AppHandle,
    local_project_id: LocalProjectId,
    recipe: BundleRecipe,
) -> Result<LocalProjectRegistration, String> {
    save_bundle_recipe_with_repository(&repository(&app)?, &local_project_id, recipe)
}

#[tauri::command]
pub async fn save_project_link(
    app: tauri::AppHandle,
    request: SaveProjectLinkRequest,
) -> Result<ProjectStorageLink, String> {
    save_project_link_with_repository(&repository(&app)?, request)
}

#[tauri::command]
pub async fn connect_project_to_remote_bundle(
    app: tauri::AppHandle,
    request: ConnectProjectBundleRequest,
) -> Result<ProjectDetail, String> {
    let repository = repository(&app)?;
    let storage = storage_log_label(&repository, &request.storage_id);
    emit_log(
        &app,
        "info",
        &format!(
            "Connecting project to bundle {} in {}…",
            request.bundle_id, storage
        ),
    );
    let result = run_blocking(move || {
        connect_project_to_remote_bundle_with_repository(&repository, request)
    })
    .await;
    match &result {
        Ok(detail) => emit_log(
            &app,
            "ok",
            &format!(
                "Connected {} to remote bundle {}",
                detail.project.display_name, detail.project.bundle_id
            ),
        ),
        Err(error) => emit_log(
            &app,
            "error",
            &format!("Failed to connect remote bundle: {}", error),
        ),
    }
    result
}

#[tauri::command]
pub async fn remove_project_link(
    app: tauri::AppHandle,
    local_project_id: LocalProjectId,
    storage_id: StorageId,
) -> Result<bool, String> {
    repository(&app)?.mutate_config(|config| {
        let before = config.links.len();
        config.links.retain(|link| {
            link.local_project_id != local_project_id || link.storage_id != storage_id
        });
        Ok(before != config.links.len())
    })
}

#[tauri::command]
pub async fn list_provider_profiles(
    app: tauri::AppHandle,
) -> Result<Vec<ProviderProfileSummary>, String> {
    let repository = repository(&app)?;
    run_blocking(move || list_provider_profiles_with_repository(&repository)).await
}

#[tauri::command]
pub async fn probe_provider_profile(
    app: tauri::AppHandle,
    provider: Provider,
    path: String,
) -> Result<ProviderProfileProbe, String> {
    let repository = repository(&app)?;
    run_blocking(move || probe_provider_profile_with_repository(&repository, provider, &path)).await
}

#[tauri::command]
pub async fn create_provider_profile(
    app: tauri::AppHandle,
    provider: Provider,
    display_name: String,
    path: String,
) -> Result<ProviderProfile, String> {
    let repository = repository(&app)?;
    run_blocking(move || {
        create_provider_profile_with_repository(&repository, provider, &display_name, &path)
    })
    .await
}

#[tauri::command]
pub async fn rename_provider_profile(
    app: tauri::AppHandle,
    profile_id: LocalProviderProfileId,
    display_name: String,
    expected_revision: u64,
) -> Result<ProviderProfile, String> {
    rename_provider_profile_with_repository(
        &repository(&app)?,
        &profile_id,
        &display_name,
        expected_revision,
    )
}

#[tauri::command]
pub async fn remove_provider_profile(
    app: tauri::AppHandle,
    profile_id: LocalProviderProfileId,
    expected_revision: u64,
) -> Result<bool, String> {
    remove_provider_profile_with_repository(&repository(&app)?, &profile_id, expected_revision)
}

#[tauri::command]
pub async fn list_project_bindings(app: tauri::AppHandle) -> Result<Vec<ProjectBinding>, String> {
    Ok(repository(&app)?.load_bindings()?.bindings)
}

/// Binding lookup is by local project/replica registration, never by bundle
/// ID: one bundle may have multiple local replicas.
#[tauri::command]
pub async fn get_project_binding(
    app: tauri::AppHandle,
    local_project_id: LocalProjectId,
) -> Result<Option<ProjectBinding>, String> {
    Ok(repository(&app)?
        .load_bindings()?
        .active_for(&local_project_id)
        .cloned())
}

#[tauri::command]
pub async fn save_project_binding(
    app: tauri::AppHandle,
    request: SaveProjectBindingRequest,
) -> Result<ProjectBinding, String> {
    save_project_binding_with_repository(&repository(&app)?, request)
}

/// Detach rather than erase: apply receipts and per-replica baselines remain
/// addressable if the user later rebinds the project.
#[tauri::command]
pub async fn remove_project_binding(
    app: tauri::AppHandle,
    local_project_id: LocalProjectId,
) -> Result<bool, String> {
    detach_project_binding_with_repository(&repository(&app)?, &local_project_id)
}

#[tauri::command]
pub async fn list_project_materializations(
    app: tauri::AppHandle,
    local_project_id: Option<LocalProjectId>,
) -> Result<Vec<MaterializationRecord>, String> {
    let records = repository(&app)?.load_materializations()?.records;
    Ok(match local_project_id {
        Some(id) => records
            .into_iter()
            .filter(|record| record.local_project_id == id)
            .collect(),
        None => records,
    })
}

#[tauri::command]
pub async fn get_restore_plan(
    app: tauri::AppHandle,
    plan_id: PlanId,
) -> Result<RestorePlan, String> {
    repository(&app)?.load_restore_plan(&plan_id)
}

#[tauri::command]
pub async fn discard_restore_plan(app: tauri::AppHandle, plan_id: PlanId) -> Result<bool, String> {
    repository(&app)?.discard_restore_plan(&plan_id)
}

#[tauri::command]
pub async fn discover_project(
    app: tauri::AppHandle,
    path: String,
    profile_ids: BTreeMap<Provider, LocalProviderProfileId>,
) -> Result<ProjectDiscovery, String> {
    let repository = repository(&app)?;
    run_blocking(move || discover_project_with_repository(&repository, &path, &profile_ids)).await
}

#[tauri::command]
pub async fn get_bundle_inventory(
    app: tauri::AppHandle,
    local_project_id: LocalProjectId,
) -> Result<ResourceInventory, String> {
    let repository = repository(&app)?;
    run_blocking(move || get_bundle_inventory_with_repository(&repository, &local_project_id)).await
}

#[tauri::command]
pub async fn list_remote_bundles(
    app: tauri::AppHandle,
    storage_id: StorageId,
    cursor: Option<String>,
) -> Result<BundlePage, String> {
    let repository = repository(&app)?;
    run_blocking(move || {
        list_remote_bundles_with_repository(&repository, &storage_id, cursor.as_deref())
    })
    .await
}

#[tauri::command]
pub async fn list_remote_bundle_snapshots(
    app: tauri::AppHandle,
    storage_id: StorageId,
) -> Result<Vec<BundleSnapshotSummary>, String> {
    let repository = repository(&app)?;
    run_blocking(move || list_remote_bundle_snapshots_with_repository(&repository, &storage_id))
        .await
}

#[tauri::command]
pub async fn find_remote_bundle_matches(
    app: tauri::AppHandle,
    storage_id: StorageId,
    repository_fingerprint: String,
) -> Result<Vec<BundleSnapshotSummary>, String> {
    let repository = repository(&app)?;
    run_blocking(move || {
        find_remote_bundle_matches_with_repository(
            &repository,
            &storage_id,
            &repository_fingerprint,
        )
    })
    .await
}

#[tauri::command]
pub async fn fetch_bundle(
    app: tauri::AppHandle,
    storage_id: StorageId,
    bundle_id: BundleId,
) -> Result<BundleSnapshotSummary, String> {
    let repository = repository(&app)?;
    let storage = storage_log_label(&repository, &storage_id);
    emit_log(
        &app,
        "info",
        &format!("↓  Fetching bundle {} from {}", bundle_id, storage),
    );
    let result = run_blocking(move || {
        let (_, fetched) = fetch_from_storage(&repository, &storage_id, &bundle_id)?;
        bundle_snapshot_summary(fetched)
    })
    .await;
    match &result {
        Ok(snapshot) => emit_log(
            &app,
            "ok",
            &format!(
                "↓  Fetched {} generation {} ({} resources)",
                snapshot.display_name, snapshot.generation, snapshot.resource_count
            ),
        ),
        Err(error) => emit_log(&app, "error", &format!("✗  Pull fetch failed: {}", error)),
    }
    result
}

#[tauri::command]
pub async fn get_bundle_status(
    app: tauri::AppHandle,
    local_project_id: LocalProjectId,
    storage_id: StorageId,
) -> Result<ResourceStatusReport, String> {
    let repository = repository(&app)?;
    run_blocking(move || {
        get_bundle_status_with_repository(&repository, &local_project_id, &storage_id)
    })
    .await
}

#[tauri::command]
pub async fn push_bundle(
    app: tauri::AppHandle,
    local_project_id: LocalProjectId,
    storage_id: StorageId,
) -> Result<ProjectOperationResult, String> {
    let repository = repository(&app)?;
    let (project, storage) =
        project_storage_log_labels(&repository, &local_project_id, &storage_id);
    emit_log(
        &app,
        "info",
        &format!("↑  Push started — {} → {}", project, storage),
    );
    let worker_app = app.clone();
    let result = run_blocking(move || {
        emit_log(
            &worker_app,
            "info",
            "   Scanning selected project resources…",
        );
        push_bundle_with_repository(&repository, &local_project_id, &storage_id)
    })
    .await;
    match &result {
        Ok(operation) => {
            for resource in &operation.results {
                emit_log(
                    &app,
                    if resource.state == "synced" {
                        "ok"
                    } else {
                        "info"
                    },
                    &format!("↑  {} ({})", resource.resource_id, resource.state),
                );
            }
            emit_log(
                &app,
                "ok",
                &format!("✓  Push complete — {}", operation.message),
            );
        }
        Err(error) => emit_log(&app, "error", &format!("✗  Push failed: {}", error)),
    }
    result
}

#[tauri::command]
pub async fn plan_bundle_restore(
    app: tauri::AppHandle,
    storage_id: StorageId,
    bundle_id: BundleId,
    binding: ProjectBinding,
) -> Result<RestorePlan, String> {
    let repository = repository(&app)?;
    let (project, storage) =
        project_storage_log_labels(&repository, &binding.local_project_id, &storage_id);
    emit_log(
        &app,
        "info",
        &format!("↓  Preparing Pull review — {} ← {}", project, storage),
    );
    let result = run_blocking(move || {
        plan_bundle_restore_with_repository(&repository, &storage_id, &bundle_id, &binding)
    })
    .await;
    match &result {
        Ok(plan) => {
            for action in &plan.actions {
                emit_log(&app, "info", &format!("↓  Review {}", action.resource_id));
            }
            emit_log(
                &app,
                "ok",
                &format!(
                    "✓  Pull plan ready — generation {}, {} actions",
                    plan.generation,
                    plan.actions.len()
                ),
            );
            emit_log(
                &app,
                "info",
                "   Nothing has been applied yet — use “Apply approved changes” in the Pull review.",
            );
        }
        Err(error) => emit_log(
            &app,
            "error",
            &format!("✗  Pull planning failed: {}", error),
        ),
    }
    result
}

#[tauri::command]
pub async fn apply_bundle_restore(
    app: tauri::AppHandle,
    plan_id: PlanId,
    approved_action_ids: Vec<ActionId>,
) -> Result<RestoreResult, String> {
    let repository = repository(&app)?;
    let log_repository = repository.clone();
    let plan_for_log = repository.load_restore_plan(&plan_id).ok();
    emit_log(
        &app,
        "info",
        &format!(
            "↓  Applying {} approved Pull actions…",
            approved_action_ids.len()
        ),
    );
    let result = run_blocking(move || {
        apply_bundle_restore_with_repository(&repository, &plan_id, approved_action_ids)
    })
    .await;
    match &result {
        Ok(operation) => {
            for action_id in &operation.applied_action_ids {
                let resource = plan_for_log
                    .as_ref()
                    .and_then(|plan| {
                        plan.actions
                            .iter()
                            .find(|action| &action.action_id == action_id)
                    })
                    .map(|action| action.resource_id.to_string())
                    .unwrap_or_else(|| action_id.to_string());
                emit_log(&app, "ok", &format!("↓  {}", resource));
            }
            for failure in &operation.failed_actions {
                emit_log(
                    &app,
                    "error",
                    &format!("✗  {}: {}", failure.action_id, failure.message),
                );
            }
            emit_log(
                &app,
                if operation.success { "ok" } else { "error" },
                &format!(
                    "{}  Pull finished — {}",
                    if operation.success { "✓" } else { "✗" },
                    operation.message
                ),
            );
            if operation.success {
                match plan_for_log
                    .as_ref()
                    .ok_or_else(|| "restore plan is unavailable".to_string())
                    .and_then(|plan| current_binding_for_restore_plan(&log_repository, plan))
                {
                    Ok(binding) => {
                        emit_log(&app, "info", "   Open this project after Pull:");
                        for (label, command) in project_open_commands(&binding) {
                            emit_log(&app, "info", &format!("   {}: {}", label, command));
                        }
                    }
                    Err(error) => emit_log(
                        &app,
                        "info",
                        &format!("   Open command unavailable: {}", error),
                    ),
                }
            }
        }
        Err(error) => emit_log(&app, "error", &format!("✗  Pull failed: {}", error)),
    }
    result
}

#[tauri::command]
pub async fn plan_dependencies(
    app: tauri::AppHandle,
    bundle_id: BundleId,
    binding: ProjectBinding,
) -> Result<DependencyPlan, String> {
    let repository = repository(&app)?;
    run_blocking(move || plan_dependencies_with_repository(&repository, &bundle_id, &binding)).await
}

#[tauri::command]
pub async fn apply_dependency_actions(
    app: tauri::AppHandle,
    plan_id: PlanId,
    action_ids: Vec<ActionId>,
) -> Result<DependencyResult, String> {
    apply_dependency_actions_with_repository(&repository(&app)?, &plan_id, action_ids).await
}

#[tauri::command]
pub async fn get_bundle_readiness(
    app: tauri::AppHandle,
    bundle_id: BundleId,
    binding: ProjectBinding,
) -> Result<BundleReadiness, String> {
    let repository = repository(&app)?;
    run_blocking(move || get_bundle_readiness_with_repository(&repository, &bundle_id, &binding))
        .await
}

fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "Codex",
        Provider::Claude => "Claude",
    }
}

fn provider_default_directory(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => ".codex",
        Provider::Claude => ".claude",
    }
}

fn suggested_profile_name(provider: Provider, path: &Path) -> String {
    let default = dirs::home_dir().map(|home| home.join(provider_default_directory(provider)));
    if default.as_ref().is_some_and(|default| {
        prospective_canonical(default).ok().as_ref() == prospective_canonical(path).ok().as_ref()
    }) {
        return format!("Default {}", provider_name(provider));
    }
    let stem = if path
        .file_name()
        .is_some_and(|name| name == provider_default_directory(provider))
    {
        path.parent().and_then(Path::file_name)
    } else {
        path.file_name()
    };
    stem.and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(|name| format!("{} · {}", name, provider_name(provider)))
        .unwrap_or_else(|| format!("{} profile", provider_name(provider)))
}

fn probe_provider_profile_with_repository(
    repository: &V3Repository,
    provider: Provider,
    requested_path: &str,
) -> Result<ProviderProfileProbe, String> {
    validate_absolute_clean_path("provider profile path", requested_path)?;
    let requested = PathBuf::from(requested_path);
    let child = requested.join(provider_default_directory(provider));
    let (resolved, detected_child) = if requested
        .file_name()
        .is_none_or(|name| name != provider_default_directory(provider))
        && child.is_dir()
    {
        (child, true)
    } else {
        (requested, false)
    };
    let metadata = fs_profile_metadata(&resolved)?;
    if !metadata.is_dir() {
        return Err(format!(
            "{} profile '{}' is not a directory",
            provider_name(provider),
            resolved.display()
        ));
    }
    let canonical = fs_canonicalize(&resolved)?;
    let repository_root = prospective_canonical(repository.root())?;
    if paths_overlap(&canonical, &repository_root) {
        return Err(format!(
            "{} profile '{}' overlaps schema-3 application data",
            provider_name(provider),
            canonical.display()
        ));
    }
    let config = repository.load_config()?;
    for storage in config
        .storages
        .iter()
        .filter(|storage| storage.kind == StorageKind::Local)
    {
        let storage_root = prospective_canonical(Path::new(&storage.local_dir))?;
        if paths_overlap(&canonical, &storage_root) {
            return Err(format!(
                "{} profile '{}' overlaps local storage '{}'",
                provider_name(provider),
                canonical.display(),
                storage.name
            ));
        }
    }
    let readable = fs::read_dir(&canonical).is_ok();
    let writable = !metadata.permissions().readonly();
    let state = repository.load_bindings()?;
    for binding in state
        .bindings
        .iter()
        .filter(|binding| binding.state == BindingState::Active)
    {
        let project_root = Path::new(&binding.canonical_project_root);
        if paths_overlap(&canonical, project_root) {
            return Err(format!(
                "{} profile '{}' overlaps project '{}'",
                provider_name(provider),
                canonical.display(),
                binding.local_project_id
            ));
        }
    }
    let existing_profile_id = state
        .profiles
        .iter()
        .find(|profile| {
            profile.provider == provider && Path::new(&profile.canonical_path) == canonical
        })
        .map(|profile| profile.profile_id.clone());
    Ok(ProviderProfileProbe {
        provider,
        requested_path: requested_path.to_string(),
        resolved_path: resolved.to_string_lossy().into_owned(),
        canonical_path: canonical.to_string_lossy().into_owned(),
        suggested_name: suggested_profile_name(provider, &resolved),
        readable,
        writable,
        detected_child,
        existing_profile_id,
    })
}

fn create_provider_profile_with_repository(
    repository: &V3Repository,
    provider: Provider,
    display_name: &str,
    path: &str,
) -> Result<ProviderProfile, String> {
    let probe = probe_provider_profile_with_repository(repository, provider, path)?;
    if !probe.readable {
        return Err(format!(
            "{} profile '{}' is not readable",
            provider_name(provider),
            probe.resolved_path
        ));
    }
    if let Some(profile_id) = &probe.existing_profile_id {
        return repository
            .load_bindings()?
            .profiles
            .into_iter()
            .find(|profile| &profile.profile_id == profile_id)
            .ok_or_else(|| "provider profile disappeared during creation".to_string());
    }
    let now = now_secs();
    let profile = ProviderProfile {
        profile_id: LocalProviderProfileId::parse(generated_named_id("profile")?)?,
        provider,
        display_name: if display_name.trim().is_empty() {
            probe.suggested_name
        } else {
            display_name.trim().to_string()
        },
        path: probe.resolved_path,
        canonical_path: probe.canonical_path,
        revision: 0,
        created_at: now,
        updated_at: now,
    };
    profile.validate_structure()?;
    repository.mutate_bindings(|_, state| {
        if let Some(existing) = state.profiles.iter().find(|existing| {
            existing.provider == profile.provider
                && existing.canonical_path == profile.canonical_path
        }) {
            return Ok(existing.clone());
        }
        state.profiles.push(profile.clone());
        Ok(profile)
    })
}

fn ensure_default_provider_profiles(repository: &V3Repository) -> Result<(), String> {
    let Some(home) = dirs::home_dir() else {
        return Ok(());
    };
    for provider in [Provider::Codex, Provider::Claude] {
        let path = home.join(provider_default_directory(provider));
        if path.is_dir() {
            if probe_provider_profile_with_repository(repository, provider, &path.to_string_lossy())
                .is_err()
            {
                continue;
            }
            let _ = create_provider_profile_with_repository(
                repository,
                provider,
                &format!("Default {}", provider_name(provider)),
                &path.to_string_lossy(),
            )?;
        }
    }
    Ok(())
}

fn inspect_provider_profile(profile: &ProviderProfile) -> (bool, bool, bool, Option<String>) {
    let path = Path::new(&profile.path);
    let result = (|| {
        let metadata = fs_profile_metadata(path)?;
        if !metadata.is_dir() {
            return Err("profile path is not a directory".to_string());
        }
        let canonical = fs_canonicalize(path)?;
        if canonical != PathBuf::from(&profile.canonical_path) {
            return Err("profile path now resolves to a different directory".to_string());
        }
        let readable = fs::read_dir(&canonical).is_ok();
        Ok((readable, !metadata.permissions().readonly()))
    })();
    match result {
        Ok((readable, writable)) => (true, readable, writable, None),
        Err(error) => (false, false, false, Some(error)),
    }
}

fn list_provider_profiles_with_repository(
    repository: &V3Repository,
) -> Result<Vec<ProviderProfileSummary>, String> {
    ensure_default_provider_profiles(repository)?;
    let state = repository.load_bindings()?;
    let mut profiles = state
        .profiles
        .iter()
        .cloned()
        .map(|profile| {
            let (available, readable, writable, error) = inspect_provider_profile(&profile);
            let used_by_projects = state
                .bindings
                .iter()
                .filter(|binding| {
                    binding.state == BindingState::Active
                        && binding
                            .profile_ids
                            .values()
                            .any(|id| id == &profile.profile_id)
                })
                .map(|binding| binding.local_project_id.clone())
                .collect();
            ProviderProfileSummary {
                profile,
                available,
                readable,
                writable,
                used_by_projects,
                error,
            }
        })
        .collect::<Vec<_>>();
    profiles.sort_by(|left, right| {
        left.profile
            .provider
            .cmp(&right.profile.provider)
            .then_with(|| left.profile.display_name.cmp(&right.profile.display_name))
    });
    Ok(profiles)
}

fn rename_provider_profile_with_repository(
    repository: &V3Repository,
    profile_id: &LocalProviderProfileId,
    display_name: &str,
    expected_revision: u64,
) -> Result<ProviderProfile, String> {
    let name = display_name.trim();
    if name.is_empty() {
        return Err("provider profile name cannot be empty".to_string());
    }
    repository.mutate_bindings(|_, state| {
        let profile = state
            .profiles
            .iter_mut()
            .find(|profile| &profile.profile_id == profile_id)
            .ok_or_else(|| format!("unknown provider profile '{}'", profile_id))?;
        if profile.revision != expected_revision {
            return Err(format!(
                "provider profile changed (expected revision {}, current {})",
                expected_revision, profile.revision
            ));
        }
        profile.display_name = name.to_string();
        profile.revision = profile.revision.saturating_add(1);
        profile.updated_at = now_secs();
        profile.validate_structure()?;
        Ok(profile.clone())
    })
}

fn remove_provider_profile_with_repository(
    repository: &V3Repository,
    profile_id: &LocalProviderProfileId,
    expected_revision: u64,
) -> Result<bool, String> {
    repository.mutate_bindings(|_, state| {
        let Some(profile) = state
            .profiles
            .iter()
            .find(|profile| &profile.profile_id == profile_id)
        else {
            return Ok(false);
        };
        if profile.revision != expected_revision {
            return Err(format!(
                "provider profile changed (expected revision {}, current {})",
                expected_revision, profile.revision
            ));
        }
        let used_by = state
            .bindings
            .iter()
            .filter(|binding| {
                binding.state == BindingState::Active
                    && binding.profile_ids.values().any(|id| id == profile_id)
            })
            .map(|binding| binding.local_project_id.to_string())
            .collect::<Vec<_>>();
        if !used_by.is_empty() {
            return Err(format!(
                "provider profile '{}' is used by project(s): {}",
                profile.display_name,
                used_by.join(", ")
            ));
        }
        state
            .profiles
            .retain(|profile| &profile.profile_id != profile_id);
        Ok(true)
    })
}

fn resolve_profile_paths(
    repository: &V3Repository,
    profile_ids: &BTreeMap<Provider, LocalProviderProfileId>,
) -> Result<BTreeMap<Provider, String>, String> {
    if profile_ids.is_empty() {
        return Err("choose at least one local provider profile".to_string());
    }
    let state = repository.load_bindings()?;
    let mut resolved = BTreeMap::new();
    for (provider, profile_id) in profile_ids {
        let profile = state
            .profiles
            .iter()
            .find(|profile| &profile.profile_id == profile_id)
            .ok_or_else(|| format!("unknown provider profile '{}'", profile_id))?;
        if &profile.provider != provider {
            return Err(format!(
                "{} cannot use {} profile '{}'",
                provider_name(*provider),
                provider_name(profile.provider),
                profile.display_name
            ));
        }
        let (available, readable, _, error) = inspect_provider_profile(profile);
        if !available || !readable {
            return Err(error.unwrap_or_else(|| {
                format!(
                    "{} profile '{}' is not readable",
                    provider_name(*provider),
                    profile.path
                )
            }));
        }
        resolved.insert(*provider, profile.path.clone());
    }
    Ok(resolved)
}

fn discover_project_with_repository(
    repository: &V3Repository,
    selected_path: &str,
    profile_ids: &BTreeMap<Provider, LocalProviderProfileId>,
) -> Result<ProjectDiscovery, String> {
    validate_absolute_clean_path("project root", selected_path)?;
    let project_root = fs_canonicalize(Path::new(selected_path))?;
    if !project_root.is_dir() {
        return Err(format!(
            "project root '{}' is not a directory",
            selected_path
        ));
    }
    let repository_root = prospective_canonical(repository.root())?;
    if paths_overlap(&project_root, &repository_root) {
        return Err("project root overlaps schema-3 application data".to_string());
    }
    let config = repository.load_config()?;
    for storage in config
        .storages
        .iter()
        .filter(|storage| storage.kind == StorageKind::Local)
    {
        let storage_root = prospective_canonical(Path::new(&storage.local_dir))?;
        if paths_overlap(&project_root, &storage_root) {
            return Err(format!(
                "project root '{}' overlaps local storage '{}'",
                project_root.display(),
                storage.local_dir
            ));
        }
    }

    let profile_paths = resolve_profile_paths(repository, profile_ids)?;
    for (provider, path) in &profile_paths {
        let path = fs_canonicalize(Path::new(path))?;
        if paths_overlap(&project_root, &path) {
            return Err(format!(
                "project root '{}' overlaps {} profile '{}'",
                project_root.display(),
                provider_name(*provider),
                path.display()
            ));
        }
    }
    let request = CaptureRequest {
        project_root: project_root.clone(),
        codex_home: profile_paths.get(&Provider::Codex).map(PathBuf::from),
        claude_home: profile_paths.get(&Provider::Claude).map(PathBuf::from),
        excluded_project_roots: Vec::new(),
        standalone_skills: Vec::new(),
    };
    let discovered = provider_capture::discover_project(&request)?;
    let recipe = default_recipe(&discovered.resources)?;
    let inventory = inventory_from_candidates(
        project_root.to_string_lossy().into_owned(),
        None,
        &discovered.resources,
        recipe,
        discovered.warnings.clone(),
    )?;
    let providers = inventory_providers(&inventory.resources);
    let display_name = project_root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("Project")
        .to_string();
    Ok(ProjectDiscovery {
        project_root: project_root.to_string_lossy().into_owned(),
        display_name,
        inventory,
        repository_fingerprint: repository_fingerprint(&project_root),
        providers,
        profile_ids: profile_ids.clone(),
        warnings: discovered.warnings,
    })
}

fn get_bundle_inventory_with_repository(
    repository: &V3Repository,
    local_project_id: &LocalProjectId,
) -> Result<ResourceInventory, String> {
    let config = repository.load_config()?;
    let project = config
        .project(local_project_id)
        .cloned()
        .ok_or_else(|| format!("unknown local project '{}'", local_project_id))?;
    let bindings = repository.load_bindings()?;
    let binding = bindings.active_for(local_project_id).ok_or_else(|| {
        format!(
            "project '{}' is not mapped on this machine",
            local_project_id
        )
    })?;
    let request = capture_request_for_binding(repository, binding)?;
    let discovered = provider_capture::discover_project(&request)?;
    let project = persist_auto_selected_conversations(
        repository,
        &project.local_project_id,
        &discovered.resources,
    )?;
    let mut inventory = inventory_from_candidates(
        binding.project_root.clone(),
        Some(project.bundle_id.clone()),
        &discovered.resources,
        project.recipe.clone(),
        discovered.warnings,
    )?;
    let visible: BTreeSet<_> = inventory
        .resources
        .iter()
        .map(|resource| resource.descriptor.resource_id.clone())
        .collect();
    for (resource_id, entry) in &project.recipe.entries {
        if visible.contains(resource_id) {
            continue;
        }
        inventory.resources.push(InventoryResource {
            descriptor: ResourceDescriptor {
                resource_id: resource_id.clone(),
                kind: ResourceKind::Requirement,
                provider: None,
                scope: ResourceScope::Requirement,
                display_name: resource_id.to_string(),
                provenance: Provenance::Unknown,
                apply_policy: entry.apply_policy,
                relative_cwd: None,
                codec_version: 1,
                metadata: BTreeMap::new(),
            },
            category: "tools".to_string(),
            description: Some("Selected resource is unavailable on this machine".to_string()),
            logical_paths: Vec::new(),
            default_selected: true,
            blocked_reason: Some("Resource is unavailable at the mapped paths".to_string()),
            provided_by: None,
            install_behavior: None,
        });
    }
    inventory.resources.sort_by(|left, right| {
        left.descriptor
            .resource_id
            .cmp(&right.descriptor.resource_id)
    });
    Ok(inventory)
}

fn inventory_from_candidates(
    project: String,
    bundle_id: Option<BundleId>,
    candidates: &[ResourceCandidate],
    recipe: BundleRecipe,
    warnings: Vec<String>,
) -> Result<ResourceInventory, String> {
    let mut resources = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let resource_id = ResourceId::parse(candidate.resource_id.clone())?;
        let provider = candidate.provider.map(capture_provider);
        let kind = capture_resource_kind(&candidate.kind, candidate.provider);
        let scope = match candidate.kind {
            CaptureResourceKind::Conversation | CaptureResourceKind::Memory => {
                ResourceScope::ProviderState
            }
            CaptureResourceKind::Plugin => ResourceScope::Dependency,
            _ => ResourceScope::Project,
        };
        let provenance = match candidate.kind {
            CaptureResourceKind::Plugin => Provenance::Plugin {
                provider: provider
                    .ok_or_else(|| "plugin candidate lacks a provider".to_string())?,
                plugin_id: candidate.display_name.clone(),
            },
            _ => candidate
                .logical_paths
                .first()
                .and_then(|path| path.strip_prefix("project/"))
                .map(|relative_path| Provenance::ProjectLocal {
                    relative_path: relative_path.to_string(),
                })
                .unwrap_or(Provenance::Unknown),
        };
        let apply_policy = recipe
            .entries
            .get(&resource_id)
            .map(|entry| entry.apply_policy)
            .unwrap_or_else(|| capture_apply_policy(&candidate.apply_policy));
        let descriptor = ResourceDescriptor {
            resource_id,
            kind,
            provider,
            scope,
            display_name: candidate.display_name.clone(),
            provenance,
            apply_policy,
            relative_cwd: candidate.relative_cwd.clone(),
            codec_version: 1,
            metadata: BTreeMap::new(),
        };
        descriptor.validate()?;
        resources.push(InventoryResource {
            category: resource_category(kind).to_string(),
            description: None,
            logical_paths: candidate.logical_paths.clone(),
            default_selected: candidate.selected_by_default,
            blocked_reason: candidate.blocked_reason.clone(),
            provided_by: provider.map(|provider| format!("{:?}", provider)),
            install_behavior: candidate
                .dependency
                .as_ref()
                .map(|_| "Requires explicit approval during restore".to_string()),
            descriptor,
        });
    }
    resources.sort_by(|left, right| {
        left.descriptor
            .resource_id
            .cmp(&right.descriptor.resource_id)
    });
    Ok(ResourceInventory {
        project,
        bundle_id,
        resources,
        recipe,
        generated_at: now_secs(),
        warnings,
    })
}

fn default_recipe(candidates: &[ResourceCandidate]) -> Result<BundleRecipe, String> {
    let mut recipe = BundleRecipe::default();
    for candidate in candidates
        .iter()
        .filter(|candidate| candidate.selected_by_default && candidate.blocked_reason.is_none())
    {
        let resource_id = ResourceId::parse(candidate.resource_id.clone())?;
        recipe.entries.insert(
            resource_id.clone(),
            RecipeEntry {
                resource_id,
                apply_policy: capture_apply_policy(&candidate.apply_policy),
                required: false,
            },
        );
    }
    recipe.validate()?;
    Ok(recipe)
}

fn persist_auto_selected_conversations(
    repository: &V3Repository,
    local_project_id: &LocalProjectId,
    candidates: &[ResourceCandidate],
) -> Result<LocalProjectRegistration, String> {
    let additions = candidates
        .iter()
        .filter(|candidate| {
            candidate.kind == CaptureResourceKind::Conversation
                && candidate.selected_by_default
                && candidate.blocked_reason.is_none()
        })
        .map(|candidate| {
            let resource_id = ResourceId::parse(candidate.resource_id.clone())?;
            Ok((
                resource_id.clone(),
                RecipeEntry {
                    resource_id,
                    apply_policy: capture_apply_policy(&candidate.apply_policy),
                    required: false,
                },
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    repository.mutate_config(|config| {
        let project = config
            .projects
            .iter_mut()
            .find(|project| &project.local_project_id == local_project_id)
            .ok_or_else(|| format!("unknown local project '{}'", local_project_id))?;
        let mut changed = false;
        for (resource_id, entry) in &additions {
            if !project.recipe.entries.contains_key(resource_id) {
                project
                    .recipe
                    .entries
                    .insert(resource_id.clone(), entry.clone());
                changed = true;
            }
        }
        if changed {
            project.recipe.revision = project.recipe.revision.saturating_add(1);
            project.revision = project.revision.saturating_add(1);
            project.updated_at = now_secs();
        }
        Ok(project.clone())
    })
}

fn capture_provider(provider: CaptureProvider) -> Provider {
    match provider {
        CaptureProvider::Codex => Provider::Codex,
        CaptureProvider::Claude => Provider::Claude,
    }
}

fn capture_resource_kind(
    kind: &CaptureResourceKind,
    provider: Option<CaptureProvider>,
) -> ResourceKind {
    match kind {
        CaptureResourceKind::ProjectFile => ResourceKind::ProjectFile,
        CaptureResourceKind::ProjectSettings => ResourceKind::Setting,
        CaptureResourceKind::Conversation => match provider {
            Some(CaptureProvider::Claude) => ResourceKind::ClaudeConversation,
            _ => ResourceKind::CodexConversation,
        },
        CaptureResourceKind::Memory => ResourceKind::ProjectMemory,
        CaptureResourceKind::Agent => ResourceKind::Agent,
        CaptureResourceKind::Command => ResourceKind::Command,
        CaptureResourceKind::Rule => ResourceKind::Rule,
        CaptureResourceKind::Skill => ResourceKind::ProjectSkill,
        CaptureResourceKind::Plugin => ResourceKind::Plugin,
        CaptureResourceKind::Hook => ResourceKind::Hook,
        CaptureResourceKind::McpServer => ResourceKind::McpServer,
    }
}

fn capture_apply_policy(policy: &CaptureApplyPolicy) -> ApplyPolicy {
    match policy {
        CaptureApplyPolicy::SafeFile => ApplyPolicy::SafeFile,
        CaptureApplyPolicy::Merge => ApplyPolicy::Merge,
        CaptureApplyPolicy::Review => ApplyPolicy::ExplicitReview,
        CaptureApplyPolicy::Dependency => ApplyPolicy::ExplicitInstall,
    }
}

fn resource_category(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::CodexConversation
        | ResourceKind::ClaudeConversation
        | ResourceKind::ProjectMemory => "conversations",
        ResourceKind::ProjectSkill | ResourceKind::StandaloneSkill => "skills",
        ResourceKind::Plugin => "plugins",
        ResourceKind::McpServer | ResourceKind::Hook | ResourceKind::Requirement => "tools",
        _ => "project_setup",
    }
}

fn inventory_providers(resources: &[InventoryResource]) -> Vec<Provider> {
    resources
        .iter()
        .filter_map(|resource| resource.descriptor.provider)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

enum ConfiguredStore {
    Local(LocalBundleObjectStore),
    S3(S3BundleObjectStore),
}

impl BundleObjectStore for ConfiguredStore {
    fn get(&self, key: &ObjectKey) -> Result<Option<StoredObject>, String> {
        match self {
            Self::Local(store) => store.get(key),
            Self::S3(store) => store.get(key),
        }
    }

    fn put_immutable(&self, key: &ObjectKey, bytes: &[u8]) -> Result<ImmutablePutOutcome, String> {
        match self {
            Self::Local(store) => store.put_immutable(key, bytes),
            Self::S3(store) => store.put_immutable(key, bytes),
        }
    }

    fn compare_and_swap(
        &self,
        key: &ObjectKey,
        expectation: &CasExpectation,
        bytes: &[u8],
    ) -> Result<CasOutcome, String> {
        match self {
            Self::Local(store) => store.compare_and_swap(key, expectation, bytes),
            Self::S3(store) => store.compare_and_swap(key, expectation, bytes),
        }
    }

    fn list(
        &self,
        prefix: &ObjectPrefix,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<StoreListPage, String> {
        match self {
            Self::Local(store) => store.list(prefix, cursor, limit),
            Self::S3(store) => store.list(prefix, cursor, limit),
        }
    }

    fn local_root(&self) -> Option<&Path> {
        match self {
            Self::Local(store) => store.local_root(),
            Self::S3(store) => store.local_root(),
        }
    }
}

type StorageEngine = BundleEngine<ConfiguredStore>;

fn list_remote_bundles_with_repository(
    repository: &V3Repository,
    storage_id: &StorageId,
    cursor: Option<&str>,
) -> Result<BundlePage, String> {
    let (_, engine) = storage_engine(repository, storage_id)?;
    let RemoteBundlePage {
        bundles,
        next_cursor,
    } = engine.list_remote_bundles(cursor, 100)?;
    Ok(BundlePage {
        bundles: bundles
            .into_iter()
            .map(|bundle| RemoteBundleSummaryDto {
                bundle_id: bundle.bundle_id,
                display_name: bundle.display_name,
                kind: bundle.kind,
                generation: bundle.generation,
                updated_at: bundle.updated_at,
                resource_count: bundle.resources,
            })
            .collect(),
        next_cursor,
    })
}

fn find_remote_bundle_matches_with_repository(
    repository: &V3Repository,
    storage_id: &StorageId,
    repository_fingerprint: &str,
) -> Result<Vec<BundleSnapshotSummary>, String> {
    if repository_fingerprint.len() != 64
        || !repository_fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("repository fingerprint must be a SHA-256 hex digest".to_string());
    }
    Ok(
        list_remote_bundle_snapshots_with_repository(repository, storage_id)?
            .into_iter()
            .filter(|snapshot| {
                snapshot.repository_fingerprint.as_deref() == Some(repository_fingerprint)
            })
            .collect(),
    )
}

fn list_remote_bundle_snapshots_with_repository(
    repository: &V3Repository,
    storage_id: &StorageId,
) -> Result<Vec<BundleSnapshotSummary>, String> {
    let (_, engine) = storage_engine(repository, storage_id)?;
    let mut cursor = None;
    let mut inspected = 0_usize;
    let mut snapshots = Vec::new();
    loop {
        let page = engine.list_remote_bundles(cursor.as_deref(), 100)?;
        for bundle in page.bundles {
            inspected = inspected.saturating_add(1);
            if inspected > 10_000 {
                return Err(
                    "remote bundle inspection exceeds the 10,000 bundle safety limit".to_string(),
                );
            }
            let snapshot = engine.inspect(&bundle.bundle_id)?;
            snapshots.push(bundle_snapshot_summary_from_snapshot(snapshot)?);
        }
        let Some(next) = page.next_cursor else {
            break;
        };
        if cursor.as_deref() == Some(next.as_str()) {
            return Err("remote bundle cursor did not advance".to_string());
        }
        cursor = Some(next);
    }
    snapshots.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.bundle_id.cmp(&right.bundle_id))
    });
    Ok(snapshots)
}

fn fetch_from_storage(
    repository: &V3Repository,
    storage_id: &StorageId,
    bundle_id: &BundleId,
) -> Result<(StorageEngine, FetchedBundle), String> {
    let (_, engine) = storage_engine(repository, storage_id)?;
    let fetched = engine.fetch(bundle_id)?;
    Ok((engine, fetched))
}

fn bundle_snapshot_summary(fetched: FetchedBundle) -> Result<BundleSnapshotSummary, String> {
    bundle_snapshot_summary_from_snapshot(fetched.snapshot)
}

fn bundle_snapshot_summary_from_snapshot(
    snapshot: BundleSnapshot,
) -> Result<BundleSnapshotSummary, String> {
    snapshot.validate()?;
    let storage_id = snapshot.storage_id;
    let fetched_at = snapshot.fetched_at;
    let manifest = snapshot.manifest;
    let providers = manifest
        .resources
        .values()
        .filter_map(|resource| resource.provider)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(BundleSnapshotSummary {
        storage_id,
        bundle_id: manifest.bundle.bundle_id,
        display_name: manifest.bundle.display_name,
        kind: manifest.bundle.kind,
        generation: manifest.generation,
        updated_at: manifest.updated_at,
        resource_count: manifest.resources.len(),
        providers,
        repository_fingerprint: manifest.bundle.repository_fingerprint,
        resources: manifest.resources.into_values().collect(),
        recipe: manifest.recipe,
        fetched_at,
        warnings: Vec::new(),
    })
}

fn get_bundle_status_with_repository(
    repository: &V3Repository,
    local_project_id: &LocalProjectId,
    storage_id: &StorageId,
) -> Result<ResourceStatusReport, String> {
    let config = repository.load_config()?;
    let mut project = config
        .project(local_project_id)
        .cloned()
        .ok_or_else(|| format!("unknown local project '{}'", local_project_id))?;
    require_project_link(&config, &project, storage_id)?;
    let binding = repository
        .load_bindings()?
        .active_for(local_project_id)
        .cloned()
        .ok_or_else(|| {
            format!(
                "project '{}' is not mapped on this machine",
                local_project_id
            )
        })?;
    let capture_request = capture_request_for_binding(repository, &binding)?;
    let discovered = provider_capture::discover_project(&capture_request)?;
    project = persist_auto_selected_conversations(
        repository,
        &project.local_project_id,
        &discovered.resources,
    )?;
    let capture = provider_capture::capture_recipe(&capture_request, &project.recipe)?;
    let local_resources = provider_capture::domain_resources(&capture)?;
    let unavailable: BTreeSet<_> = capture
        .unavailable_resource_ids
        .iter()
        .map(|id| ResourceId::parse(id.clone()))
        .collect::<Result<_, _>>()?;
    let (_, engine) = storage_engine(repository, storage_id)?;
    let remote = match engine.read_head(&project.bundle_id)? {
        Some(_) => Some(engine.fetch(&project.bundle_id)?),
        None => None,
    };
    let generation = remote
        .as_ref()
        .map(|fetched| fetched.snapshot.head.generation);
    let remote_resources = remote
        .as_ref()
        .map(|fetched| &fetched.snapshot.manifest.resources);
    let mut statuses = Vec::with_capacity(project.recipe.entries.len());
    for resource_id in project.recipe.entries.keys() {
        let local_digest = local_resources
            .get(resource_id)
            .and_then(|resource| resource.metadata.get("content_sha256"))
            .cloned();
        let remote_digest = remote_resources
            .and_then(|resources| resources.get(resource_id))
            .and_then(|resource| resource.metadata.get("content_sha256"))
            .cloned();
        let (state, message) = match (&local_digest, &remote_digest) {
            (Some(local), Some(remote)) if local == remote => ("synced", None),
            (Some(_), Some(_)) => (
                "conflict",
                Some("Local and remote resource versions differ".to_string()),
            ),
            (Some(_), None) => ("local_only", None),
            (None, Some(_)) if unavailable.contains(resource_id) => (
                "remote_only",
                Some("Selected resource is unavailable locally".to_string()),
            ),
            (None, Some(_)) => ("remote_only", None),
            (None, None) => (
                "missing",
                Some("Selected resource is unavailable locally and remotely".to_string()),
            ),
        };
        statuses.push(BundleResourceStatus {
            resource_id: resource_id.clone(),
            state: state.to_string(),
            message,
            local_digest,
            remote_digest,
        });
    }
    Ok(ResourceStatusReport {
        project: local_project_id.to_string(),
        storage: storage_id.to_string(),
        bundle_id: project.bundle_id,
        generation,
        statuses,
        warnings: capture.warnings,
    })
}

fn push_bundle_with_repository(
    repository: &V3Repository,
    local_project_id: &LocalProjectId,
    storage_id: &StorageId,
) -> Result<ProjectOperationResult, String> {
    let config = repository.load_config()?;
    let mut project = config
        .project(local_project_id)
        .cloned()
        .ok_or_else(|| format!("unknown local project '{}'", local_project_id))?;
    require_project_link(&config, &project, storage_id)?;
    let binding = repository
        .load_bindings()?
        .active_for(local_project_id)
        .cloned()
        .ok_or_else(|| {
            format!(
                "project '{}' is not mapped on this machine",
                local_project_id
            )
        })?;
    let capture_request = capture_request_for_binding(repository, &binding)?;
    let discovered = provider_capture::discover_project(&capture_request)?;
    project = persist_auto_selected_conversations(
        repository,
        &project.local_project_id,
        &discovered.resources,
    )?;
    let capture = provider_capture::capture_recipe(&capture_request, &project.recipe)?;
    let (_, engine) = storage_engine(repository, storage_id)?;
    let expected_head = match (
        engine.read_head(&project.bundle_id)?,
        project.recipe_bases.get(storage_id),
    ) {
        (None, None) => PublishExpectation::Absent,
        (None, Some(_)) => {
            return Err(
                "remote bundle head is missing but this project has a prior base; review before republishing"
                    .to_string(),
            )
        }
        (Some((_, _)), None) => {
            return Err(
                "remote bundle already exists and this replica has no reviewed base; pull and review it before pushing"
                    .to_string(),
            )
        }
        (Some(_), Some(base)) if base.binding_revision != Some(binding.revision) => {
            return Err(
                "the project checkout or provider profile changed after the last reviewed base; pull and review before pushing"
                    .to_string(),
            )
        }
        (Some((head, token)), Some(base))
            if head.generation == base.generation
                && head.manifest_sha256 == base.manifest_sha256 =>
        {
            PublishExpectation::Match(token)
        }
        (Some(_), Some(_)) => {
            return Err(
                "remote bundle advanced since this replica's base; pull and review before pushing"
                    .to_string(),
            )
        }
    };
    let published = engine.publish(PublishBundleRequest {
        identity: BundleIdentity {
            bundle_id: project.bundle_id.clone(),
            display_name: project.display_name.clone(),
            kind: BundleKind::Project,
            repository_fingerprint: project.repository_fingerprint.clone(),
        },
        recipe: project.recipe.clone(),
        captured_with: CapturedWith {
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            codex_version: None,
            claude_version: None,
            codec_versions: BTreeMap::new(),
        },
        captured: capture,
        expected_head,
        updated_at: now_secs(),
    })?;
    let generation = published.snapshot.head.generation;
    let commit_id = published.snapshot.head.commit_id.clone();
    let manifest_sha256 = published.snapshot.head.manifest_sha256.clone();
    let recipe_revision = project.recipe.revision;
    let expected_project_revision = project.revision;
    let bundle_id = project.bundle_id.clone();
    repository.mutate_config(|config| {
        let current = config
            .projects
            .iter_mut()
            .find(|current| current.local_project_id == *local_project_id)
            .ok_or_else(|| "project was removed while publishing".to_string())?;
        if current.revision != expected_project_revision
            || current.recipe.revision != recipe_revision
            || current.bundle_id != bundle_id
        {
            return Err(
                "project recipe changed while publishing; remote head was written, refresh before pushing again"
                    .to_string(),
            );
        }
        current.recipe_bases.insert(
            storage_id.clone(),
            RecipeBase {
                generation,
                manifest_sha256: manifest_sha256.clone(),
                recipe_revision,
                binding_revision: Some(binding.revision),
            },
        );
        current.revision = current.revision.saturating_add(1);
        current.updated_at = now_secs();
        Ok(())
    })?;
    let results = published
        .snapshot
        .manifest
        .resources
        .keys()
        .cloned()
        .map(|resource_id| OperationResourceResult {
            resource_id,
            state: "synced".to_string(),
            message: None,
        })
        .collect::<Vec<_>>();
    Ok(ProjectOperationResult {
        success: true,
        message: format!(
            "Published generation {} with {} resources",
            generation,
            results.len()
        ),
        operation_id: Some(commit_id),
        resources_changed: Some(results.len()),
        generation: Some(generation),
        results,
    })
}

fn plan_bundle_restore_with_repository(
    repository: &V3Repository,
    storage_id: &StorageId,
    bundle_id: &BundleId,
    binding: &ProjectBinding,
) -> Result<RestorePlan, String> {
    let binding = require_current_binding(repository, binding)?;
    if &binding.bundle_id != bundle_id {
        return Err("binding and requested bundle IDs differ".to_string());
    }
    let (engine, fetched) = fetch_from_storage(repository, storage_id, bundle_id)?;
    let plan = engine.build_restore_plan(&fetched, &binding, now_secs())?;
    repository.save_restore_plan(&plan)?;
    Ok(plan)
}

fn apply_bundle_restore_with_repository(
    repository: &V3Repository,
    plan_id: &PlanId,
    approved_action_ids: Vec<ActionId>,
) -> Result<RestoreResult, String> {
    let plan = repository.load_restore_plan(plan_id)?;
    let binding = current_binding_for_restore_plan(repository, &plan)?;
    let approved = unique_approved_actions(&approved_action_ids, &plan.actions)?;
    if repository
        .load_materializations()?
        .records
        .iter()
        .any(|record| record.plan_id == plan.plan_id)
    {
        return Err(format!(
            "restore plan '{}' was already applied",
            plan.plan_id
        ));
    }
    let (engine, fetched) = fetch_from_storage(repository, &plan.storage_id, &plan.bundle_id)?;
    let applied_at = now_secs();
    let applied = engine.apply_restore_plan(
        &fetched,
        &binding,
        &plan,
        &approved,
        &repository.backups_dir()?,
        applied_at,
    )?;
    let files_complete = applied.receipts.iter().all(|receipt| {
        !matches!(
            receipt.action_type,
            RestoreActionType::WriteFile
                | RestoreActionType::MergeFile
                | RestoreActionType::MaterializeConversation
        ) || receipt.status == ActionStatus::Applied
    });
    let status = if files_complete {
        MaterializationStatus::Complete
    } else {
        MaterializationStatus::Partial
    };
    let record = MaterializationRecord {
        materialization_id: MaterializationId::parse(generated_named_id("materialization")?)?,
        plan_id: plan.plan_id.clone(),
        replica_id: binding.replica_id.clone(),
        local_project_id: binding.local_project_id.clone(),
        storage_id: plan.storage_id.clone(),
        bundle_id: plan.bundle_id.clone(),
        generation: plan.generation,
        commit_id: plan.commit_id.clone(),
        manifest_sha256: plan.manifest_sha256.clone(),
        binding_revision: binding.revision,
        status,
        applied_at,
        receipts: applied.receipts.clone(),
    };
    repository.mutate_materializations(|_, materializations| {
        if materializations
            .records
            .iter()
            .any(|current| current.plan_id == record.plan_id)
        {
            return Err(format!(
                "restore plan '{}' was already recorded",
                record.plan_id
            ));
        }
        materializations.records.push(record);
        Ok(())
    })?;
    if files_complete {
        repository.mutate_config(|config| {
            let project = config
                .projects
                .iter_mut()
                .find(|project| project.local_project_id == binding.local_project_id)
                .ok_or_else(|| "bound project was removed while applying restore".to_string())?;
            project.recipe_bases.insert(
                plan.storage_id.clone(),
                RecipeBase {
                    generation: plan.generation,
                    manifest_sha256: plan.manifest_sha256.clone(),
                    recipe_revision: project.recipe.revision,
                    binding_revision: Some(binding.revision),
                },
            );
            project.revision = project.revision.saturating_add(1);
            project.updated_at = applied_at;
            Ok(())
        })?;
    }
    let applied_action_ids = applied
        .receipts
        .iter()
        .filter(|receipt| receipt.status == ActionStatus::Applied)
        .map(|receipt| receipt.action_id.clone())
        .collect::<Vec<_>>();
    let failed_actions = applied
        .receipts
        .iter()
        .filter(|receipt| matches!(receipt.status, ActionStatus::Failed | ActionStatus::Blocked))
        .map(|receipt| FailedAction {
            action_id: receipt.action_id.clone(),
            message: receipt
                .error
                .clone()
                .unwrap_or_else(|| "Action was not materialized".to_string()),
        })
        .collect::<Vec<_>>();
    let success = failed_actions.is_empty();
    Ok(RestoreResult {
        success,
        message: if success {
            format!("Applied {} restore actions", applied_action_ids.len())
        } else {
            format!(
                "Applied {} actions; {} require attention",
                applied_action_ids.len(),
                failed_actions.len()
            )
        },
        plan_id: plan.plan_id,
        applied_action_ids,
        failed_actions,
    })
}

fn plan_dependencies_with_repository(
    repository: &V3Repository,
    bundle_id: &BundleId,
    binding: &ProjectBinding,
) -> Result<DependencyPlan, String> {
    let binding = require_current_binding(repository, binding)?;
    if &binding.bundle_id != bundle_id {
        return Err("binding and requested bundle IDs differ".to_string());
    }
    let (storage_id, _, fetched) = fetch_from_linked_storage(repository, &binding, bundle_id)?;
    let created_at = now_secs();
    let plan = DependencyPlan {
        schema_version: DEPENDENCY_PLAN_SCHEMA_V1,
        // `generated_named_id` accepts only lowercase alphanumeric prefixes.
        // The plan type and its persistence directory already distinguish
        // dependency plans from restore plans.
        plan_id: PlanId::parse(generated_named_id("plan")?)?,
        storage_id,
        bundle_id: bundle_id.clone(),
        replica_id: binding.replica_id,
        generation: fetched.snapshot.head.generation,
        commit_id: fetched.snapshot.head.commit_id.clone(),
        manifest_sha256: fetched.snapshot.head.manifest_sha256.clone(),
        binding_revision: binding.revision,
        created_at,
        expires_at: created_at.saturating_add(PLAN_LIFETIME_SECS),
        actions: fetched.dependency_actions,
        blockers: Vec::new(),
        warnings: Vec::new(),
    };
    plan.validate()?;
    repository.save_dependency_plan(&plan)?;
    Ok(plan)
}

async fn apply_dependency_actions_with_repository(
    repository: &V3Repository,
    plan_id: &PlanId,
    action_ids: Vec<ActionId>,
) -> Result<DependencyResult, String> {
    let plan = repository.load_dependency_plan(plan_id)?;
    let now = now_secs();
    if now < plan.created_at || now > plan.expires_at {
        return Err("dependency plan has expired or is not active yet".to_string());
    }
    if repository
        .load_dependency_applications()?
        .records
        .iter()
        .any(|record| record.plan_id == plan.plan_id)
    {
        return Err(format!(
            "dependency plan '{}' was already applied",
            plan.plan_id
        ));
    }
    let bindings = repository.load_bindings()?;
    let binding = bindings
        .bindings
        .iter()
        .find(|binding| {
            binding.replica_id == plan.replica_id && binding.state == BindingState::Active
        })
        .cloned()
        .ok_or_else(|| "dependency plan's project binding is no longer active".to_string())?;
    if binding.revision != plan.binding_revision || binding.bundle_id != plan.bundle_id {
        return Err("dependency plan's binding changed after planning".to_string());
    }
    let binding = resolve_project_binding(repository, &binding)?;
    let (_, fetched) = fetch_from_storage(repository, &plan.storage_id, &plan.bundle_id)?;
    validate_dependency_plan_pin(&plan, &fetched, &binding)?;
    let selected = unique_dependency_actions(&action_ids, &plan.actions)?;
    let mut receipts = Vec::with_capacity(plan.actions.len());
    for action in &plan.actions {
        if !selected.contains(&action.action_id) {
            receipts.push(DependencyApplyReceipt {
                action_id: action.action_id.clone(),
                status: ActionStatus::Skipped,
                applied_at: now,
                error: None,
            });
            continue;
        }
        let result = execute_dependency_action(repository, &plan, &binding, action).await;
        receipts.push(DependencyApplyReceipt {
            action_id: action.action_id.clone(),
            status: if result.is_ok() {
                ActionStatus::Applied
            } else {
                ActionStatus::Failed
            },
            applied_at: now,
            error: result.err(),
        });
    }
    let record = DependencyApplicationRecord {
        plan_id: plan.plan_id.clone(),
        local_project_id: binding.local_project_id,
        storage_id: plan.storage_id.clone(),
        bundle_id: plan.bundle_id.clone(),
        replica_id: binding.replica_id,
        generation: plan.generation,
        commit_id: plan.commit_id.clone(),
        manifest_sha256: plan.manifest_sha256.clone(),
        binding_revision: plan.binding_revision,
        applied_at: now,
        receipts: receipts.clone(),
    };
    repository.mutate_dependency_applications(|_, applications| {
        if applications
            .records
            .iter()
            .any(|current| current.plan_id == record.plan_id)
        {
            return Err(format!(
                "dependency plan '{}' was already recorded",
                record.plan_id
            ));
        }
        applications.records.push(record);
        Ok(())
    })?;
    let applied_action_ids = receipts
        .iter()
        .filter(|receipt| receipt.status == ActionStatus::Applied)
        .map(|receipt| receipt.action_id.clone())
        .collect::<Vec<_>>();
    let failed_actions = receipts
        .iter()
        .filter(|receipt| receipt.status == ActionStatus::Failed)
        .map(|receipt| FailedAction {
            action_id: receipt.action_id.clone(),
            message: receipt
                .error
                .clone()
                .unwrap_or_else(|| "Dependency action failed".to_string()),
        })
        .collect::<Vec<_>>();
    let success = failed_actions.is_empty();
    Ok(DependencyResult {
        success,
        message: if success {
            format!("Applied {} dependency actions", applied_action_ids.len())
        } else {
            format!(
                "Applied {} dependencies; {} failed",
                applied_action_ids.len(),
                failed_actions.len()
            )
        },
        applied_action_ids,
        failed_actions,
    })
}

fn get_bundle_readiness_with_repository(
    repository: &V3Repository,
    bundle_id: &BundleId,
    binding: &ProjectBinding,
) -> Result<BundleReadiness, String> {
    let binding = require_current_binding(repository, binding)?;
    if &binding.bundle_id != bundle_id {
        return Err("binding and requested bundle IDs differ".to_string());
    }
    let (_, _, fetched) = fetch_from_linked_storage(repository, &binding, bundle_id)?;
    let head = &fetched.snapshot.head;
    let materialization = repository
        .load_materializations()?
        .records
        .into_iter()
        .rev()
        .find(|record| {
            record.replica_id == binding.replica_id
                && record.bundle_id == *bundle_id
                && record.generation == head.generation
                && record.manifest_sha256 == head.manifest_sha256
                && record.binding_revision == binding.revision
                && record.status != MaterializationStatus::Detached
        });
    let dependency_applications = repository.load_dependency_applications()?;
    let mut applied_dependencies = BTreeSet::new();
    for application in dependency_applications
        .records
        .iter()
        .filter(|application| {
            application.replica_id == binding.replica_id
                && application.bundle_id == *bundle_id
                && application.generation == head.generation
                && application.manifest_sha256 == head.manifest_sha256
                && application.binding_revision == binding.revision
        })
    {
        applied_dependencies.extend(
            application
                .receipts
                .iter()
                .filter(|receipt| receipt.status == ActionStatus::Applied)
                .map(|receipt| receipt.action_id.clone()),
        );
    }
    let mut issues = Vec::new();
    let needs_codex_home = fetched
        .snapshot
        .manifest
        .resources
        .values()
        .any(|resource| resource.kind == ResourceKind::CodexConversation);
    let needs_claude_home = fetched
        .snapshot
        .manifest
        .resources
        .values()
        .any(|resource| resource.kind == ResourceKind::ClaudeConversation);
    add_provider_home_issue(
        &mut issues,
        Provider::Codex,
        needs_codex_home,
        binding.codex_home.as_deref(),
    );
    add_provider_home_issue(
        &mut issues,
        Provider::Claude,
        needs_claude_home,
        binding.claude_home.as_deref(),
    );
    if !fetched.snapshot.manifest.files.is_empty()
        && materialization
            .as_ref()
            .is_none_or(|record| record.status != MaterializationStatus::Complete)
    {
        issues.push(BundleReadinessIssue {
            issue_id: "restore-files".to_string(),
            category: "project_setup".to_string(),
            title: "Project files are not fully materialized".to_string(),
            detail: Some("Build a restore plan and approve the intended file actions.".to_string()),
            severity: "warning".to_string(),
            provider: None,
            resource_id: None,
            action: Some("plan_restore".to_string()),
        });
    }
    for action in &fetched.dependency_actions {
        if !applied_dependencies.contains(&action.action_id) {
            issues.push(BundleReadinessIssue {
                issue_id: format!("dependency-{}", action.action_id),
                category: match action.kind {
                    DependencyActionKind::InstallCodexPlugin
                    | DependencyActionKind::InstallClaudePlugin => "plugins",
                    DependencyActionKind::InstallStandaloneSkill => "skills",
                    _ => "tools",
                }
                .to_string(),
                title: format!("{} needs setup", action.display_name),
                detail: Some("Review and approve the pinned dependency action.".to_string()),
                severity: "warning".to_string(),
                provider: action.provider,
                resource_id: Some(action.resource_id.clone()),
                action: Some("apply_dependency".to_string()),
            });
        }
    }
    let blocked = issues.iter().any(|issue| issue.severity == "error");
    Ok(BundleReadiness {
        bundle_id: bundle_id.clone(),
        state: if blocked {
            "blocked"
        } else if issues.is_empty() {
            "ready"
        } else {
            "needs_setup"
        }
        .to_string(),
        issues,
        generated_at: now_secs(),
    })
}

fn require_current_binding(
    repository: &V3Repository,
    supplied: &ProjectBinding,
) -> Result<ProjectBinding, String> {
    if supplied.state != BindingState::Active {
        return Err("project binding is not active".to_string());
    }
    let bindings = repository.load_bindings()?;
    let current = bindings
        .bindings
        .into_iter()
        .find(|binding| binding.replica_id == supplied.replica_id)
        .ok_or_else(|| "project binding is not registered on this machine".to_string())?;
    if &current != supplied {
        return Err("project binding changed; refresh before continuing".to_string());
    }
    resolve_project_binding(repository, &current)
}

fn resolve_project_binding(
    repository: &V3Repository,
    binding: &ProjectBinding,
) -> Result<ProjectBinding, String> {
    let profile_paths = resolve_profile_paths(repository, &binding.profile_ids)?;
    let mut resolved = binding.clone();
    resolved.codex_home = profile_paths.get(&Provider::Codex).cloned();
    resolved.claude_home = profile_paths.get(&Provider::Claude).cloned();
    resolved.validate_structure()?;
    Ok(resolved)
}

fn current_binding_for_restore_plan(
    repository: &V3Repository,
    plan: &RestorePlan,
) -> Result<ProjectBinding, String> {
    let binding = repository
        .load_bindings()?
        .bindings
        .into_iter()
        .find(|binding| {
            binding.replica_id == plan.replica_id && binding.state == BindingState::Active
        })
        .ok_or_else(|| "restore plan's project binding is no longer active".to_string())?;
    if binding.revision != plan.binding_revision || binding.bundle_id != plan.bundle_id {
        return Err("restore plan's project binding changed after planning".to_string());
    }
    resolve_project_binding(repository, &binding)
}

fn project_open_commands(binding: &ProjectBinding) -> Vec<(&'static str, String)> {
    let project_root = shell_quote(&binding.project_root);
    let mut commands = Vec::new();
    if let Some(codex_home) = &binding.codex_home {
        let environment = format!("CODEX_HOME={}", shell_quote(codex_home));
        commands.push((
            "Codex — new",
            format!("{} codex -C {}", environment, project_root),
        ));
        commands.push((
            "Codex — resume",
            format!("{} codex resume -C {}", environment, project_root),
        ));
    }
    if let Some(claude_home) = &binding.claude_home {
        let environment = format!("CLAUDE_CONFIG_DIR={}", shell_quote(claude_home));
        commands.push((
            "Claude — new",
            format!("cd {} && {} claude", project_root, environment),
        ));
        commands.push((
            "Claude — resume",
            format!("cd {} && {} claude --resume", project_root, environment),
        ));
    }
    commands
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn unique_approved_actions(
    requested: &[ActionId],
    actions: &[super::domain::RestoreAction],
) -> Result<BTreeSet<ActionId>, String> {
    let selected = requested.iter().cloned().collect::<BTreeSet<_>>();
    if selected.len() != requested.len() {
        return Err("approved restore actions contain duplicates".to_string());
    }
    let available = actions
        .iter()
        .map(|action| action.action_id.clone())
        .collect::<BTreeSet<_>>();
    if let Some(unknown) = selected.iter().find(|id| !available.contains(*id)) {
        return Err(format!("restore plan has no action '{}'", unknown));
    }
    Ok(selected)
}

fn unique_dependency_actions(
    requested: &[ActionId],
    actions: &[DependencyAction],
) -> Result<BTreeSet<ActionId>, String> {
    let selected = requested.iter().cloned().collect::<BTreeSet<_>>();
    if selected.len() != requested.len() {
        return Err("approved dependency actions contain duplicates".to_string());
    }
    let available = actions
        .iter()
        .map(|action| action.action_id.clone())
        .collect::<BTreeSet<_>>();
    if let Some(unknown) = selected.iter().find(|id| !available.contains(*id)) {
        return Err(format!("dependency plan has no action '{}'", unknown));
    }
    Ok(selected)
}

fn validate_dependency_plan_pin(
    plan: &DependencyPlan,
    fetched: &FetchedBundle,
    binding: &ProjectBinding,
) -> Result<(), String> {
    if plan.storage_id != fetched.snapshot.storage_id
        || plan.bundle_id != fetched.snapshot.head.bundle_id
        || plan.bundle_id != binding.bundle_id
        || plan.replica_id != binding.replica_id
        || plan.generation != fetched.snapshot.head.generation
        || plan.commit_id != fetched.snapshot.head.commit_id
        || plan.manifest_sha256 != fetched.snapshot.head.manifest_sha256
        || plan.binding_revision != binding.revision
    {
        return Err("dependency plan no longer matches the bundle or binding".to_string());
    }
    Ok(())
}

async fn execute_dependency_action(
    repository: &V3Repository,
    plan: &DependencyPlan,
    binding: &ProjectBinding,
    action: &DependencyAction,
) -> Result<(), String> {
    action.validate()?;
    match action.kind {
        DependencyActionKind::InstallStandaloneSkill => {
            let materialized = repository
                .load_materializations()?
                .records
                .iter()
                .any(|record| {
                    record.replica_id == binding.replica_id
                        && record.bundle_id == plan.bundle_id
                        && record.generation == plan.generation
                        && record.manifest_sha256 == plan.manifest_sha256
                        && record.binding_revision == binding.revision
                        && record.receipts.iter().any(|receipt| {
                            receipt.resource_id == action.resource_id
                                && receipt.status == ActionStatus::Applied
                        })
                });
            if materialized {
                Ok(())
            } else {
                Err("restore the standalone-skill payload before approving it".to_string())
            }
        }
        DependencyActionKind::InstallCodexPlugin => {
            run_plugin_install("codex", action, binding).await
        }
        DependencyActionKind::InstallClaudePlugin => {
            run_plugin_install("claude", action, binding).await
        }
        DependencyActionKind::CheckBinary
        | DependencyActionKind::CheckEnvironment
        | DependencyActionKind::Manual => {
            Err("this dependency requires a provider-specific manual check".to_string())
        }
    }
}

async fn run_plugin_install(
    program: &str,
    action: &DependencyAction,
    binding: &ProjectBinding,
) -> Result<(), String> {
    if !portable_plugin_id(&action.display_name) {
        return Err("plugin identifier is not safe for native installation".to_string());
    }
    let expected_arguments = match action.kind {
        DependencyActionKind::InstallCodexPlugin => vec![
            "plugin".to_string(),
            "add".to_string(),
            action.display_name.clone(),
        ],
        DependencyActionKind::InstallClaudePlugin => vec![
            "plugin".to_string(),
            "install".to_string(),
            action.display_name.clone(),
            "--scope".to_string(),
            "project".to_string(),
        ],
        _ => return Err("dependency is not a plugin installation".to_string()),
    };
    if action.argv != expected_arguments {
        return Err("plugin install arguments differ from the supported intent".to_string());
    }
    let mut command = Command::new(program);
    command
        .args(&expected_arguments)
        .current_dir(&binding.project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(codex_home) = &binding.codex_home {
        command.env("CODEX_HOME", codex_home);
    }
    if let Some(claude_home) = &binding.claude_home {
        command.env("CLAUDE_CONFIG_DIR", claude_home);
    }
    let output = timeout(DEFAULT_OPERATION_TIMEOUT, command.status())
        .await
        .map_err(|_| format!("{} plugin installation timed out", program))?
        .map_err(|error| format!("start {} plugin installer: {}", program, error))?;
    if output.success() {
        Ok(())
    } else {
        Err(format!(
            "{} plugin installer exited with status {}",
            program,
            output
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "terminated".to_string())
        ))
    }
}

fn portable_plugin_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && !value.starts_with('-')
        && !value.contains("..")
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'@' | b'/' | b'.' | b'_' | b'-' | b':')
        })
}

fn add_provider_home_issue(
    issues: &mut Vec<BundleReadinessIssue>,
    provider: Provider,
    required: bool,
    home: Option<&str>,
) {
    if !required {
        return;
    }
    let valid = home.is_some_and(|path| {
        fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
    });
    if valid {
        return;
    }
    let name = match provider {
        Provider::Codex => "Codex",
        Provider::Claude => "Claude",
    };
    issues.push(BundleReadinessIssue {
        issue_id: format!("provider-home-{}", name.to_ascii_lowercase()),
        category: "conversations".to_string(),
        title: format!("{} home is not mapped", name),
        detail: Some(format!(
            "Choose an existing {} home before restoring provider state.",
            name
        )),
        severity: "error".to_string(),
        provider: Some(provider),
        resource_id: None,
        action: Some("edit_binding".to_string()),
    });
}

fn capture_request_for_binding(
    repository: &V3Repository,
    binding: &ProjectBinding,
) -> Result<CaptureRequest, String> {
    let binding = resolve_project_binding(repository, binding)?;
    let resolved_project = fs_canonicalize(Path::new(&binding.project_root))?;
    if resolved_project != PathBuf::from(&binding.canonical_project_root) {
        return Err("project binding resolves to a different checkout".to_string());
    }
    let excluded_project_roots = repository
        .load_bindings()?
        .bindings
        .into_iter()
        .filter(|other| {
            other.state == BindingState::Active && other.replica_id != binding.replica_id
        })
        .map(|other| PathBuf::from(other.canonical_project_root))
        .filter(|root| root.starts_with(&resolved_project) && root != &resolved_project)
        .collect();
    Ok(CaptureRequest {
        project_root: resolved_project,
        codex_home: binding.codex_home.as_ref().map(PathBuf::from),
        claude_home: binding.claude_home.as_ref().map(PathBuf::from),
        excluded_project_roots,
        standalone_skills: Vec::new(),
    })
}

fn repository_fingerprint(project_root: &Path) -> Option<String> {
    let config_path = project_root.join(".git/config");
    let metadata = fs::symlink_metadata(&config_path).ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024 * 1024 {
        return None;
    }
    let contents = fs::read_to_string(config_path).ok()?;
    let mut in_origin = false;
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_origin = line.eq_ignore_ascii_case("[remote \"origin\"]");
            continue;
        }
        if in_origin {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key.trim().eq_ignore_ascii_case("url") {
                let normalized = strip_url_userinfo(value.trim());
                let digest = Sha256::digest(normalized.as_bytes());
                return Some(hex_digest(&digest));
            }
        }
    }
    None
}

fn strip_url_userinfo(value: &str) -> String {
    let Some((scheme, remainder)) = value.split_once("://") else {
        return value.to_string();
    };
    let authority_end = remainder.find('/').unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    let Some(at) = authority.rfind('@') else {
        return value.to_string();
    };
    format!("{}://{}", scheme, &remainder[at + 1..])
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{:02x}", byte);
    }
    output
}

fn require_project_link(
    config: &SyncConfigV3,
    project: &LocalProjectRegistration,
    storage_id: &StorageId,
) -> Result<(), String> {
    if config.links.iter().any(|link| {
        link.local_project_id == project.local_project_id
            && &link.storage_id == storage_id
            && link.bundle_id == project.bundle_id
    }) {
        Ok(())
    } else {
        Err(format!(
            "project '{}' is not linked to storage '{}'",
            project.local_project_id, storage_id
        ))
    }
}

fn storage_engine(
    repository: &V3Repository,
    storage_id: &StorageId,
) -> Result<(StorageConfigV3, StorageEngine), String> {
    let config = repository.load_config()?;
    let storage = config
        .storages
        .iter()
        .find(|storage| &storage.id == storage_id)
        .cloned()
        .ok_or_else(|| format!("unknown storage '{}'", storage_id))?;
    let machine = repository.load_bindings()?;
    validate_config_storage_isolation(repository, &config, &machine.bindings, &machine.profiles)?;
    let store = match storage.kind {
        StorageKind::Local => {
            ConfiguredStore::Local(LocalBundleObjectStore::open(&storage.local_dir)?)
        }
        StorageKind::S3 => {
            ConfiguredStore::S3(S3BundleObjectStore::from_current_runtime(&storage)?)
        }
    };
    Ok((storage, BundleEngine::new(store, storage_id.clone())))
}

fn fetch_from_linked_storage(
    repository: &V3Repository,
    binding: &ProjectBinding,
    bundle_id: &BundleId,
) -> Result<(StorageId, StorageEngine, FetchedBundle), String> {
    let config = repository.load_config()?;
    let mut last_error = None;
    for link in config.links.iter().filter(|link| {
        link.local_project_id == binding.local_project_id && &link.bundle_id == bundle_id
    }) {
        if !config
            .storages
            .iter()
            .any(|storage| storage.id == link.storage_id)
        {
            continue;
        }
        match fetch_from_storage(repository, &link.storage_id, bundle_id) {
            Ok((engine, fetched)) => return Ok((link.storage_id.clone(), engine, fetched)),
            Err(error) => last_error = Some(error),
        }
    }
    if let Some(error) = last_error {
        Err(error)
    } else {
        Err(format!("bundle '{}' has no linked storage", bundle_id))
    }
}

fn validate_config_storage_isolation(
    repository: &V3Repository,
    config: &SyncConfigV3,
    bindings: &[ProjectBinding],
    profiles: &[ProviderProfile],
) -> Result<(), String> {
    let repository_root = prospective_canonical(repository.root())?;
    let mut local_roots = Vec::<(&StorageConfigV3, PathBuf)>::new();
    for storage in config
        .storages
        .iter()
        .filter(|storage| storage.kind == StorageKind::Local)
    {
        let root = prospective_canonical(Path::new(&storage.local_dir))?;
        if paths_overlap(&root, &repository_root) {
            return Err(format!(
                "local storage '{}' overlaps schema-3 application data",
                storage.name
            ));
        }
        for (other, other_root) in &local_roots {
            if paths_overlap(&root, other_root) {
                return Err(format!(
                    "local storages '{}' and '{}' overlap",
                    storage.name, other.name
                ));
            }
        }
        for binding in bindings
            .iter()
            .filter(|binding| binding.state == BindingState::Active)
        {
            let target = prospective_canonical(Path::new(&binding.canonical_project_root))?;
            if paths_overlap(&root, &target) {
                return Err(format!(
                    "local storage '{}' overlaps project root '{}'",
                    storage.name,
                    target.display()
                ));
            }
        }
        for profile in profiles {
            let target = prospective_canonical(Path::new(&profile.canonical_path))?;
            if paths_overlap(&root, &target) {
                return Err(format!(
                    "local storage '{}' overlaps {} profile '{}'",
                    storage.name,
                    provider_name(profile.provider),
                    target.display()
                ));
            }
        }
        local_roots.push((storage, root));
    }
    Ok(())
}

fn prospective_canonical(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    {
        return Err(format!(
            "path '{}' is not absolute and clean",
            path.display()
        ));
    }
    let mut cursor = path.to_path_buf();
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(&cursor) {
            Ok(_) => {
                let mut resolved = fs::canonicalize(&cursor)
                    .map_err(|error| format!("resolve '{}': {}", cursor.display(), error))?;
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = cursor.file_name().ok_or_else(|| {
                    format!("cannot resolve prospective path '{}'", path.display())
                })?;
                missing.push(name.to_os_string());
                cursor = cursor
                    .parent()
                    .ok_or_else(|| format!("'{}' has no existing ancestor", path.display()))?
                    .to_path_buf();
            }
            Err(error) => {
                return Err(format!("inspect '{}': {}", cursor.display(), error));
            }
        }
    }
}

fn get_project_with_repository(
    repository: &V3Repository,
    local_project_id: &LocalProjectId,
) -> Result<Option<ProjectDetail>, String> {
    let config = repository.load_config()?;
    let Some(project) = config.project(local_project_id).cloned() else {
        return Ok(None);
    };
    let links = config
        .links
        .into_iter()
        .filter(|link| &link.local_project_id == local_project_id)
        .collect();
    let binding = repository
        .load_bindings()?
        .active_for(local_project_id)
        .cloned();
    let materializations = repository
        .load_materializations()?
        .records
        .into_iter()
        .filter(|record| &record.local_project_id == local_project_id)
        .collect();
    Ok(Some(ProjectDetail {
        project,
        links,
        binding,
        materializations,
    }))
}

fn register_local_project_with_repository(
    repository: &V3Repository,
    request: RegisterLocalProjectRequest,
) -> Result<LocalProjectRegistration, String> {
    let now = now_secs();
    let project = LocalProjectRegistration {
        local_project_id: LocalProjectId::parse(generated_named_id("project")?)?,
        bundle_id: match request.bundle_id {
            Some(bundle_id) => bundle_id,
            None => BundleId::generate()?,
        },
        display_name: request.display_name,
        repository_fingerprint: request.repository_fingerprint,
        recipe: BundleRecipe::default(),
        recipe_bases: Default::default(),
        revision: 0,
        created_at: now,
        updated_at: now,
    };
    project.validate()?;
    repository.mutate_config(|config| {
        if config
            .projects
            .iter()
            .any(|existing| existing.local_project_id == project.local_project_id)
        {
            return Err("generated local project id collision; try again".to_string());
        }
        config.projects.push(project.clone());
        Ok(project)
    })
}

fn save_bundle_recipe_with_repository(
    repository: &V3Repository,
    local_project_id: &LocalProjectId,
    mut recipe: BundleRecipe,
) -> Result<LocalProjectRegistration, String> {
    recipe.validate()?;
    repository.mutate_config(|config| {
        let project = config
            .projects
            .iter_mut()
            .find(|project| &project.local_project_id == local_project_id)
            .ok_or_else(|| format!("unknown local project '{}'", local_project_id))?;
        if recipe.revision != project.recipe.revision {
            return Err(format!(
                "bundle recipe changed (expected revision {}, current {})",
                recipe.revision, project.recipe.revision
            ));
        }
        recipe.revision = recipe.revision.saturating_add(1);
        project.recipe = recipe;
        project.revision = project.revision.saturating_add(1);
        project.updated_at = now_secs();
        Ok(project.clone())
    })
}

fn save_project_link_with_repository(
    repository: &V3Repository,
    request: SaveProjectLinkRequest,
) -> Result<ProjectStorageLink, String> {
    repository.mutate_config(|config| {
        let project = config
            .project(&request.local_project_id)
            .ok_or_else(|| format!("unknown local project '{}'", request.local_project_id))?;
        if !config
            .storages
            .iter()
            .any(|storage| storage.id == request.storage_id)
        {
            return Err(format!("unknown storage '{}'", request.storage_id));
        }
        let link = ProjectStorageLink {
            local_project_id: request.local_project_id.clone(),
            storage_id: request.storage_id.clone(),
            bundle_id: project.bundle_id.clone(),
            pinned: request.pinned,
            created_at: now_secs(),
        };
        if let Some(existing) = config.links.iter_mut().find(|existing| {
            existing.local_project_id == link.local_project_id
                && existing.storage_id == link.storage_id
        }) {
            // Preserve creation time while updating user intent.
            let created_at = existing.created_at;
            *existing = link.clone();
            existing.created_at = created_at;
            let mut returned = link;
            returned.created_at = created_at;
            Ok(returned)
        } else {
            config.links.push(link.clone());
            Ok(link)
        }
    })
}

fn connect_project_to_remote_bundle_with_repository(
    repository: &V3Repository,
    request: ConnectProjectBundleRequest,
) -> Result<ProjectDetail, String> {
    let config = repository.load_config()?;
    let project = config
        .project(&request.local_project_id)
        .cloned()
        .ok_or_else(|| format!("unknown local project '{}'", request.local_project_id))?;
    if project.bundle_id != request.expected_bundle_id {
        return Err(format!(
            "project bundle changed (expected '{}', current '{}')",
            request.expected_bundle_id, project.bundle_id
        ));
    }
    if !config
        .storages
        .iter()
        .any(|storage| storage.id == request.storage_id)
    {
        return Err(format!("unknown storage '{}'", request.storage_id));
    }
    if project.bundle_id == request.bundle_id {
        save_project_link_with_repository(
            repository,
            SaveProjectLinkRequest {
                local_project_id: request.local_project_id.clone(),
                storage_id: request.storage_id,
                pinned: request.pinned,
            },
        )?;
        return get_project_with_repository(repository, &request.local_project_id)?
            .ok_or_else(|| "project disappeared while linking storage".to_string());
    }

    let other_links = config
        .links
        .iter()
        .filter(|link| {
            link.local_project_id == request.local_project_id
                && link.storage_id != request.storage_id
        })
        .count();
    if other_links > 0 {
        return Err(
            "cannot change bundle identity while the project is linked to another storage; unlink the other storage first"
                .to_string(),
        );
    }
    if !project.recipe_bases.is_empty() {
        return Err(
            "cannot change bundle identity after this project established a reviewed sync base"
                .to_string(),
        );
    }
    if repository
        .load_materializations()?
        .records
        .iter()
        .any(|record| record.local_project_id == request.local_project_id)
    {
        return Err(
            "cannot change bundle identity after project resources were materialized".to_string(),
        );
    }
    if repository
        .load_dependency_applications()?
        .records
        .iter()
        .any(|record| record.local_project_id == request.local_project_id)
    {
        return Err(
            "cannot change bundle identity after project dependencies were applied".to_string(),
        );
    }

    let (_, engine) = storage_engine(repository, &request.storage_id)?;
    let remote = engine.inspect(&request.bundle_id)?;
    let remote_fingerprint = remote.manifest.bundle.repository_fingerprint.clone();
    if let (Some(local), Some(remote)) = (
        project.repository_fingerprint.as_deref(),
        remote_fingerprint.as_deref(),
    ) {
        if local != remote && !request.allow_repository_mismatch {
            return Err("remote bundle belongs to a different repository".to_string());
        }
    } else if project.repository_fingerprint.is_some() && !request.allow_repository_mismatch {
        return Err("remote bundle does not declare a repository fingerprint".to_string());
    }
    if engine.read_head(&project.bundle_id)?.is_some() {
        return Err(
            "the current bundle already exists in this storage; refusing to abandon published history"
                .to_string(),
        );
    }

    let machine = repository.load_bindings()?;
    let active_binding = machine.active_for(&request.local_project_id).cloned();
    if let Some(binding) = &active_binding {
        validate_binding_request(
            repository,
            &SaveProjectBindingRequest {
                local_project_id: request.local_project_id.clone(),
                project_root: binding.project_root.clone(),
                profile_ids: binding.profile_ids.clone(),
                expected_revision: None,
            },
        )?;
    }
    let removed_bindings = machine
        .bindings
        .iter()
        .filter(|binding| binding.local_project_id == request.local_project_id)
        .cloned()
        .collect::<Vec<_>>();
    repository.mutate_bindings(|_, bindings| {
        bindings
            .bindings
            .retain(|binding| binding.local_project_id != request.local_project_id);
        Ok(())
    })?;

    let remote_recipe = remote.manifest.recipe.clone();
    let new_bundle_id = request.bundle_id.clone();
    let config_result = repository.mutate_config(|config| {
        let project = config
            .projects
            .iter_mut()
            .find(|project| project.local_project_id == request.local_project_id)
            .ok_or_else(|| "project was removed while connecting the remote bundle".to_string())?;
        if project.bundle_id != request.expected_bundle_id {
            return Err("project bundle changed while connecting remote storage".to_string());
        }
        project.bundle_id = new_bundle_id.clone();
        project.recipe = remote_recipe.clone();
        if project.repository_fingerprint.is_none()
            || (request.allow_repository_mismatch && remote_fingerprint.is_some())
        {
            project.repository_fingerprint = remote_fingerprint.clone();
        }
        project.revision = project.revision.saturating_add(1);
        project.updated_at = now_secs();
        for link in config
            .links
            .iter_mut()
            .filter(|link| link.local_project_id == request.local_project_id)
        {
            link.bundle_id = new_bundle_id.clone();
            link.pinned = request.pinned;
        }
        if !config.links.iter().any(|link| {
            link.local_project_id == request.local_project_id
                && link.storage_id == request.storage_id
        }) {
            config.links.push(ProjectStorageLink {
                local_project_id: request.local_project_id.clone(),
                storage_id: request.storage_id.clone(),
                bundle_id: new_bundle_id.clone(),
                pinned: request.pinned,
                created_at: now_secs(),
            });
        }
        Ok(())
    });
    if let Err(error) = config_result {
        let rollback = repository.mutate_bindings(|_, bindings| {
            bindings.bindings.extend(removed_bindings.clone());
            Ok(())
        });
        return Err(match rollback {
            Ok(()) => error,
            Err(rollback_error) => format!(
                "{}; restoring the previous project binding also failed: {}",
                error, rollback_error
            ),
        });
    }

    if let Some(binding) = active_binding {
        save_project_binding_with_repository(
            repository,
            SaveProjectBindingRequest {
                local_project_id: request.local_project_id.clone(),
                project_root: binding.project_root,
                profile_ids: binding.profile_ids,
                expected_revision: None,
            },
        )?;
    }
    get_project_with_repository(repository, &request.local_project_id)?
        .ok_or_else(|| "project disappeared after connecting remote storage".to_string())
}

fn save_project_binding_with_repository(
    repository: &V3Repository,
    request: SaveProjectBindingRequest,
) -> Result<ProjectBinding, String> {
    let validated = validate_binding_request(repository, &request)?;
    let binding = repository.mutate_bindings(|config, bindings| {
        let project = config
            .project(&request.local_project_id)
            .ok_or_else(|| format!("unknown local project '{}'", request.local_project_id))?;
        let position = bindings
            .bindings
            .iter()
            .position(|binding| binding.local_project_id == request.local_project_id);
        let (replica_id, revision) = match position {
            Some(index) => {
                let current = &bindings.bindings[index];
                let expected = request.expected_revision.ok_or_else(|| {
                    "expected_revision is required when changing a binding".to_string()
                })?;
                if expected != current.revision {
                    return Err(format!(
                        "project binding changed (expected revision {}, current {})",
                        expected, current.revision
                    ));
                }
                (
                    current.replica_id.clone(),
                    current.revision.saturating_add(1),
                )
            }
            None => {
                if request.expected_revision.is_some() {
                    return Err("new binding must not provide expected_revision".to_string());
                }
                (ReplicaId::parse(generated_named_id("replica")?)?, 0)
            }
        };
        let next = ProjectBinding {
            replica_id,
            local_project_id: request.local_project_id.clone(),
            bundle_id: project.bundle_id.clone(),
            project_root: request.project_root.clone(),
            canonical_project_root: validated.canonical_project_root.clone(),
            profile_ids: request.profile_ids.clone(),
            codex_home: validated.profile_paths.get(&Provider::Codex).cloned(),
            claude_home: validated.profile_paths.get(&Provider::Claude).cloned(),
            state: BindingState::Active,
            revision,
            updated_at: now_secs(),
        };
        next.validate_structure()?;
        match position {
            Some(index) => bindings.bindings[index] = next.clone(),
            None => bindings.bindings.push(next.clone()),
        }
        Ok(next)
    })?;

    // A remap never erases history, but materializations produced for an
    // older binding revision are detached and cannot satisfy a new plan.
    repository.mutate_materializations(|_, materializations| {
        for record in &mut materializations.records {
            if record.replica_id == binding.replica_id
                && record.binding_revision != binding.revision
                && record.status != MaterializationStatus::Detached
            {
                record.status = MaterializationStatus::Detached;
            }
        }
        Ok(())
    })?;
    Ok(binding)
}

struct ValidatedBindingRequest {
    canonical_project_root: String,
    profile_paths: BTreeMap<Provider, String>,
}

fn validate_binding_request(
    repository: &V3Repository,
    request: &SaveProjectBindingRequest,
) -> Result<ValidatedBindingRequest, String> {
    validate_absolute_clean_path("project root", &request.project_root)?;
    let profile_paths = resolve_profile_paths(repository, &request.profile_ids)?;
    let metadata = fs_metadata_no_final_symlink(Path::new(&request.project_root))?;
    if !metadata.is_dir() {
        return Err(format!(
            "project root '{}' is not a directory",
            request.project_root
        ));
    }
    let canonical = fs_canonicalize(Path::new(&request.project_root))?;
    let repository_root = prospective_canonical(repository.root())?;
    if paths_overlap(&canonical, &repository_root) {
        return Err(format!(
            "project root '{}' overlaps schema-3 app data '{}'",
            canonical.display(),
            repository_root.display()
        ));
    }
    let config = repository.load_config()?;
    let local_stores = config
        .storages
        .iter()
        .filter(|storage| storage.kind == super::domain::StorageKind::Local)
        .map(|storage| {
            prospective_canonical(Path::new(&storage.local_dir))
                .map(|path| (storage.name.as_str(), path))
        })
        .collect::<Result<Vec<_>, _>>()?;
    for (storage_name, storage_path) in &local_stores {
        if paths_overlap(&canonical, &storage_path) {
            return Err(format!(
                "project root '{}' overlaps local storage '{}'",
                canonical.display(),
                storage_name
            ));
        }
    }
    let mut provider_roots: Vec<PathBuf> = Vec::new();
    for (provider, path) in &profile_paths {
        let label = format!("{} profile", provider_name(*provider));
        let path = prospective_canonical(Path::new(path))?;
        if paths_overlap(&path, &repository_root) {
            return Err(format!(
                "{} '{}' overlaps schema-3 app data",
                label,
                path.display()
            ));
        }
        if paths_overlap(&path, &canonical) {
            return Err(format!(
                "{} '{}' overlaps the project root",
                label,
                path.display()
            ));
        }
        for (storage_name, storage_path) in &local_stores {
            if paths_overlap(&path, storage_path) {
                return Err(format!(
                    "{} '{}' overlaps local storage '{}'",
                    label,
                    path.display(),
                    storage_name
                ));
            }
        }
        if provider_roots
            .iter()
            .any(|other| paths_overlap(&path, other))
        {
            return Err("Codex and Claude homes must not overlap".to_string());
        }
        provider_roots.push(path);
    }
    Ok(ValidatedBindingRequest {
        canonical_project_root: canonical.to_string_lossy().into_owned(),
        profile_paths,
    })
}

fn detach_project_binding_with_repository(
    repository: &V3Repository,
    local_project_id: &LocalProjectId,
) -> Result<bool, String> {
    let detached_replica = repository.mutate_bindings(|_, bindings| {
        let Some(binding) = bindings.bindings.iter_mut().find(|binding| {
            &binding.local_project_id == local_project_id && binding.state == BindingState::Active
        }) else {
            return Ok(None);
        };
        binding.state = BindingState::Detached;
        binding.revision = binding.revision.saturating_add(1);
        binding.updated_at = now_secs();
        Ok(Some(binding.replica_id.clone()))
    })?;
    let Some(replica_id) = detached_replica else {
        return Ok(false);
    };
    repository.mutate_materializations(|_, materializations| {
        for record in &mut materializations.records {
            if record.replica_id == replica_id {
                record.status = MaterializationStatus::Detached;
            }
        }
        Ok(())
    })?;
    Ok(true)
}

fn remove_local_project_with_repository(
    repository: &V3Repository,
    local_project_id: &LocalProjectId,
) -> Result<bool, String> {
    if repository
        .load_config()?
        .project(local_project_id)
        .is_none()
    {
        return Ok(false);
    }
    // Detach local application state before removing the registration.  A
    // crash between files leaves a valid unbound project, never an active
    // binding to an unknown project.
    let _ = detach_project_binding_with_repository(repository, local_project_id)?;
    repository.mutate_materializations(|_, materializations| {
        for record in &mut materializations.records {
            if &record.local_project_id == local_project_id {
                record.status = MaterializationStatus::Detached;
            }
        }
        Ok(())
    })?;
    repository.mutate_config(|config| {
        config
            .projects
            .retain(|project| &project.local_project_id != local_project_id);
        config
            .links
            .retain(|link| &link.local_project_id != local_project_id);
        Ok(true)
    })
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

fn fs_metadata_no_final_symlink(path: &Path) -> Result<std::fs::Metadata, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("inspect '{}': {}", path.display(), error))?;
    if metadata.file_type().is_symlink() {
        // Binding spelling may itself be a symlink because provider cwd
        // identity uses the spelling.  Follow it only after proving the final
        // component is a link; canonical containment is stored separately.
        std::fs::metadata(path).map_err(|error| format!("follow '{}': {}", path.display(), error))
    } else {
        Ok(metadata)
    }
}

fn fs_profile_metadata(path: &Path) -> Result<std::fs::Metadata, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("inspect provider profile '{}': {}", path.display(), error))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "provider profile '{}' cannot be a symlink; choose the resolved directory",
            path.display()
        ));
    }
    Ok(metadata)
}

fn fs_canonicalize(path: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(path).map_err(|error| format!("resolve '{}': {}", path.display(), error))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_sync_v3::domain::{StorageConfigV3, StorageKind};

    fn repository(temp: &tempfile::TempDir) -> V3Repository {
        V3Repository::from_app_data_dir(temp.path()).unwrap()
    }

    fn register(repo: &V3Repository) -> LocalProjectRegistration {
        register_local_project_with_repository(
            repo,
            RegisterLocalProjectRequest {
                display_name: "Project A".to_string(),
                repository_fingerprint: None,
                bundle_id: Some(BundleId::parse("0123456789abcdef0123456789abcdef").unwrap()),
            },
        )
        .unwrap()
    }

    fn add_profile(repo: &V3Repository, provider: Provider, path: &Path) -> LocalProviderProfileId {
        std::fs::create_dir_all(path).unwrap();
        create_provider_profile_with_repository(
            repo,
            provider,
            &format!("Test {}", provider_name(provider)),
            &path.to_string_lossy(),
        )
        .unwrap()
        .profile_id
    }

    #[test]
    fn project_open_commands_pin_the_checkout_and_assigned_profiles() {
        let binding = ProjectBinding {
            replica_id: ReplicaId::parse("replica-test").unwrap(),
            local_project_id: LocalProjectId::parse("project-test").unwrap(),
            bundle_id: BundleId::parse("0123456789abcdef0123456789abcdef").unwrap(),
            project_root: "/tmp/client's project".to_string(),
            canonical_project_root: "/tmp/client's project".to_string(),
            profile_ids: BTreeMap::new(),
            codex_home: Some("/tmp/custom codex/.codex".to_string()),
            claude_home: Some("/tmp/custom claude/.claude".to_string()),
            state: BindingState::Active,
            revision: 1,
            updated_at: 1,
        };

        assert_eq!(
            project_open_commands(&binding),
            vec![
                (
                    "Codex — new",
                    "CODEX_HOME='/tmp/custom codex/.codex' codex -C '/tmp/client'\"'\"'s project'"
                        .to_string(),
                ),
                (
                    "Codex — resume",
                    "CODEX_HOME='/tmp/custom codex/.codex' codex resume -C '/tmp/client'\"'\"'s project'"
                        .to_string(),
                ),
                (
                    "Claude — new",
                    "cd '/tmp/client'\"'\"'s project' && CLAUDE_CONFIG_DIR='/tmp/custom claude/.claude' claude"
                        .to_string(),
                ),
                (
                    "Claude — resume",
                    "cd '/tmp/client'\"'\"'s project' && CLAUDE_CONFIG_DIR='/tmp/custom claude/.claude' claude --resume"
                        .to_string(),
                ),
            ]
        );
    }

    #[test]
    fn refresh_and_push_auto_select_new_project_conversations() {
        let temp = tempfile::tempdir().unwrap();
        let repo = V3Repository::from_app_data_dir(temp.path().join("app-data")).unwrap();
        let storage_id = StorageId::parse("shared").unwrap();
        let mut config = repo.load_config().unwrap();
        config.storages.push(StorageConfigV3 {
            id: storage_id.clone(),
            name: "Shared".to_string(),
            kind: StorageKind::Local,
            bucket: String::new(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            account_id: String::new(),
            s3_endpoint: String::new(),
            region: String::new(),
            local_dir: temp.path().join("store").to_string_lossy().into_owned(),
            included_default_exclusions: Vec::new(),
            supports_conditional_writes: None,
        });
        repo.save_config(config).unwrap();

        let project = register(&repo);
        let project_root = temp.path().join("project");
        let codex_home = temp.path().join("codex-home");
        std::fs::create_dir_all(&project_root).unwrap();
        let profile_id = add_profile(&repo, Provider::Codex, &codex_home);
        save_project_binding_with_repository(
            &repo,
            SaveProjectBindingRequest {
                local_project_id: project.local_project_id.clone(),
                project_root: project_root.to_string_lossy().into_owned(),
                profile_ids: BTreeMap::from([(Provider::Codex, profile_id)]),
                expected_revision: None,
            },
        )
        .unwrap();
        save_project_link_with_repository(
            &repo,
            SaveProjectLinkRequest {
                local_project_id: project.local_project_id.clone(),
                storage_id: storage_id.clone(),
                pinned: true,
            },
        )
        .unwrap();

        let first_id = "019f742a-a206-7932-876c-9db8d8ce575a";
        let second_id = "019f742b-0e23-7f95-aab5-124bdbdf6b42";
        let sessions = codex_home.join("sessions/2026/07/18");
        std::fs::create_dir_all(&sessions).unwrap();
        for id in [first_id, second_id] {
            std::fs::write(
                sessions.join(format!("rollout-{}.jsonl", id)),
                format!(
                    "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{}\",\"cwd\":{}}}}}\n",
                    id,
                    serde_json::to_string(project_root.to_str().unwrap()).unwrap()
                ),
            )
            .unwrap();
        }
        std::fs::write(
            codex_home.join("session_index.jsonl"),
            format!(
                "{{\"id\":\"{}\",\"thread_name\":\"First\"}}\n{{\"id\":\"{}\",\"thread_name\":\"Second\"}}\n",
                first_id, second_id
            ),
        )
        .unwrap();

        // Refresh persists default conversation selection without requiring a
        // separate Save project recipe click.
        let inventory =
            get_bundle_inventory_with_repository(&repo, &project.local_project_id).unwrap();
        for resource_id in [
            format!("codex:session:{}", first_id),
            format!("codex:session:{}", second_id),
            "codex:session-index".to_string(),
        ] {
            assert!(inventory
                .recipe
                .entries
                .contains_key(&ResourceId::parse(resource_id).unwrap()));
        }

        // A newly-created conversation is also reconciled at Push time, even
        // when the UI has not refreshed since it appeared.
        let third_id = "019f742c-0e23-7f95-aab5-124bdbdf6b43";
        std::fs::write(
            sessions.join(format!("rollout-{}.jsonl", third_id)),
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{}\",\"cwd\":{}}}}}\n",
                third_id,
                serde_json::to_string(project_root.to_str().unwrap()).unwrap()
            ),
        )
        .unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(codex_home.join("session_index.jsonl"))
            .and_then(|mut file| {
                use std::io::Write as _;
                writeln!(
                    file,
                    "{{\"id\":\"{}\",\"thread_name\":\"Third\"}}",
                    third_id
                )
            })
            .unwrap();

        let pushed =
            push_bundle_with_repository(&repo, &project.local_project_id, &storage_id).unwrap();
        assert!(pushed
            .results
            .iter()
            .any(|result| result.resource_id.as_str() == format!("codex:session:{}", third_id)));
        let persisted = repo
            .load_config()
            .unwrap()
            .project(&project.local_project_id)
            .unwrap()
            .recipe
            .entries
            .keys()
            .map(ToString::to_string)
            .collect::<BTreeSet<_>>();
        assert!(persisted.contains(&format!("codex:session:{}", third_id)));
    }

    #[test]
    fn registration_generates_local_identity_and_persists_recipe() {
        let temp = tempfile::tempdir().unwrap();
        let repo = repository(&temp);
        let project = register(&repo);
        assert!(project.local_project_id.as_str().starts_with("project-"));
        assert_eq!(project.recipe.revision, 0);
        assert_eq!(repo.load_config().unwrap().projects, vec![project]);
    }

    #[test]
    fn multiple_local_checkouts_can_share_one_remote_bundle_identity() {
        let temp = tempfile::tempdir().unwrap();
        let repo = repository(&temp);
        let bundle_id = BundleId::parse("0123456789abcdef0123456789abcdef").unwrap();
        let first = register_local_project_with_repository(
            &repo,
            RegisterLocalProjectRequest {
                display_name: "Project A".to_string(),
                repository_fingerprint: Some("a".repeat(64)),
                bundle_id: Some(bundle_id.clone()),
            },
        )
        .unwrap();
        let second = register_local_project_with_repository(
            &repo,
            RegisterLocalProjectRequest {
                display_name: "Project A replica".to_string(),
                repository_fingerprint: Some("a".repeat(64)),
                bundle_id: Some(bundle_id.clone()),
            },
        )
        .unwrap();
        assert_ne!(first.local_project_id, second.local_project_id);
        assert_eq!(first.bundle_id, bundle_id);
        assert_eq!(second.bundle_id, bundle_id);
        assert_eq!(repo.load_config().unwrap().projects.len(), 2);
    }

    #[test]
    fn recipe_updates_require_the_opened_revision() {
        let temp = tempfile::tempdir().unwrap();
        let repo = repository(&temp);
        let project = register(&repo);
        let saved = save_bundle_recipe_with_repository(
            &repo,
            &project.local_project_id,
            project.recipe.clone(),
        )
        .unwrap();
        assert_eq!(saved.recipe.revision, 1);
        assert!(save_bundle_recipe_with_repository(
            &repo,
            &project.local_project_id,
            project.recipe,
        )
        .unwrap_err()
        .contains("changed"));
    }

    #[test]
    fn every_storage_link_reuses_the_registration_bundle_id() {
        let temp = tempfile::tempdir().unwrap();
        let repo = repository(&temp);
        let project = register(&repo);
        let mut config = repo.load_config().unwrap();
        config.storages.push(StorageConfigV3 {
            id: StorageId::parse("backup").unwrap(),
            name: "Backup".to_string(),
            kind: StorageKind::Local,
            bucket: String::new(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            account_id: String::new(),
            s3_endpoint: String::new(),
            region: String::new(),
            local_dir: temp.path().join("store").to_string_lossy().into_owned(),
            included_default_exclusions: Vec::new(),
            supports_conditional_writes: None,
        });
        repo.save_config(config).unwrap();
        let link = save_project_link_with_repository(
            &repo,
            SaveProjectLinkRequest {
                local_project_id: project.local_project_id,
                storage_id: StorageId::parse("backup").unwrap(),
                pinned: false,
            },
        )
        .unwrap();
        assert_eq!(link.bundle_id, project.bundle_id);
    }

    #[test]
    fn unlinked_project_can_adopt_a_matching_remote_bundle_when_storage_is_added_later() {
        let temp = tempfile::tempdir().unwrap();
        let repo = V3Repository::from_app_data_dir(temp.path().join("app-data")).unwrap();
        let storage_id = StorageId::parse("shared").unwrap();
        let mut config = repo.load_config().unwrap();
        config.storages.push(StorageConfigV3 {
            id: storage_id.clone(),
            name: "Shared".to_string(),
            kind: StorageKind::Local,
            bucket: String::new(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            account_id: String::new(),
            s3_endpoint: String::new(),
            region: String::new(),
            local_dir: temp.path().join("store").to_string_lossy().into_owned(),
            included_default_exclusions: Vec::new(),
            supports_conditional_writes: None,
        });
        repo.save_config(config).unwrap();

        let fingerprint = "a".repeat(64);
        let source_bundle = BundleId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let source = register_local_project_with_repository(
            &repo,
            RegisterLocalProjectRequest {
                display_name: "Project A".to_string(),
                repository_fingerprint: Some(fingerprint.clone()),
                bundle_id: Some(source_bundle.clone()),
            },
        )
        .unwrap();
        let source_root = temp.path().join("project-a");
        std::fs::create_dir_all(&source_root).unwrap();
        let profile_id = add_profile(&repo, Provider::Codex, &temp.path().join("codex-home"));
        save_project_binding_with_repository(
            &repo,
            SaveProjectBindingRequest {
                local_project_id: source.local_project_id.clone(),
                project_root: source_root.to_string_lossy().into_owned(),
                profile_ids: BTreeMap::from([(Provider::Codex, profile_id.clone())]),
                expected_revision: None,
            },
        )
        .unwrap();
        save_project_link_with_repository(
            &repo,
            SaveProjectLinkRequest {
                local_project_id: source.local_project_id.clone(),
                storage_id: storage_id.clone(),
                pinned: true,
            },
        )
        .unwrap();
        push_bundle_with_repository(&repo, &source.local_project_id, &storage_id).unwrap();

        let other_bundle = BundleId::parse("cccccccccccccccccccccccccccccccc").unwrap();
        let other = register_local_project_with_repository(
            &repo,
            RegisterLocalProjectRequest {
                display_name: "Other repo".to_string(),
                repository_fingerprint: Some("c".repeat(64)),
                bundle_id: Some(other_bundle.clone()),
            },
        )
        .unwrap();
        let other_root = temp.path().join("other-repo");
        std::fs::create_dir_all(&other_root).unwrap();
        save_project_binding_with_repository(
            &repo,
            SaveProjectBindingRequest {
                local_project_id: other.local_project_id.clone(),
                project_root: other_root.to_string_lossy().into_owned(),
                profile_ids: BTreeMap::from([(Provider::Codex, profile_id.clone())]),
                expected_revision: None,
            },
        )
        .unwrap();
        save_project_link_with_repository(
            &repo,
            SaveProjectLinkRequest {
                local_project_id: other.local_project_id.clone(),
                storage_id: storage_id.clone(),
                pinned: true,
            },
        )
        .unwrap();
        push_bundle_with_repository(&repo, &other.local_project_id, &storage_id).unwrap();

        let available = list_remote_bundle_snapshots_with_repository(&repo, &storage_id).unwrap();
        assert_eq!(available.len(), 2);
        assert!(available.iter().any(|bundle| {
            bundle.bundle_id == source_bundle
                && bundle.repository_fingerprint.as_deref() == Some(fingerprint.as_str())
        }));
        assert!(available
            .iter()
            .any(|bundle| bundle.bundle_id == other_bundle));

        let matches =
            find_remote_bundle_matches_with_repository(&repo, &storage_id, &fingerprint).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].bundle_id, source_bundle);

        let local_bundle = BundleId::parse("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
        let replica = register_local_project_with_repository(
            &repo,
            RegisterLocalProjectRequest {
                display_name: "Project B".to_string(),
                repository_fingerprint: Some(fingerprint.clone()),
                bundle_id: Some(local_bundle.clone()),
            },
        )
        .unwrap();
        let replica_root = temp.path().join("project-b");
        std::fs::create_dir_all(&replica_root).unwrap();
        save_project_binding_with_repository(
            &repo,
            SaveProjectBindingRequest {
                local_project_id: replica.local_project_id.clone(),
                project_root: replica_root.to_string_lossy().into_owned(),
                profile_ids: BTreeMap::from([(Provider::Codex, profile_id.clone())]),
                expected_revision: None,
            },
        )
        .unwrap();
        // This is the reported failure mode: linking first stores a fresh
        // local-only bundle ID which Pull cannot find remotely.
        save_project_link_with_repository(
            &repo,
            SaveProjectLinkRequest {
                local_project_id: replica.local_project_id.clone(),
                storage_id: storage_id.clone(),
                pinned: true,
            },
        )
        .unwrap();

        let connected = connect_project_to_remote_bundle_with_repository(
            &repo,
            ConnectProjectBundleRequest {
                local_project_id: replica.local_project_id.clone(),
                storage_id: storage_id.clone(),
                bundle_id: source_bundle.clone(),
                expected_bundle_id: local_bundle,
                pinned: true,
                allow_repository_mismatch: false,
            },
        )
        .unwrap();
        assert_eq!(connected.project.bundle_id, source_bundle);
        assert_eq!(connected.links.len(), 1);
        assert_eq!(connected.links[0].bundle_id, connected.project.bundle_id);
        let binding = connected.binding.unwrap();
        assert_eq!(binding.bundle_id, connected.project.bundle_id);
        assert_eq!(binding.project_root, replica_root.to_string_lossy());
        assert_eq!(binding.profile_ids.get(&Provider::Codex), Some(&profile_id));
        assert_eq!(
            get_bundle_status_with_repository(&repo, &replica.local_project_id, &storage_id)
                .unwrap()
                .generation,
            Some(1)
        );

        let override_local_bundle = BundleId::parse("dddddddddddddddddddddddddddddddd").unwrap();
        let override_replica = register_local_project_with_repository(
            &repo,
            RegisterLocalProjectRequest {
                display_name: "Explicit repo override".to_string(),
                repository_fingerprint: Some(fingerprint.clone()),
                bundle_id: Some(override_local_bundle.clone()),
            },
        )
        .unwrap();
        let override_root = temp.path().join("override-replica");
        std::fs::create_dir_all(&override_root).unwrap();
        save_project_binding_with_repository(
            &repo,
            SaveProjectBindingRequest {
                local_project_id: override_replica.local_project_id.clone(),
                project_root: override_root.to_string_lossy().into_owned(),
                profile_ids: BTreeMap::from([(Provider::Codex, profile_id)]),
                expected_revision: None,
            },
        )
        .unwrap();
        save_project_link_with_repository(
            &repo,
            SaveProjectLinkRequest {
                local_project_id: override_replica.local_project_id.clone(),
                storage_id: storage_id.clone(),
                pinned: true,
            },
        )
        .unwrap();

        let rejected = connect_project_to_remote_bundle_with_repository(
            &repo,
            ConnectProjectBundleRequest {
                local_project_id: override_replica.local_project_id.clone(),
                storage_id: storage_id.clone(),
                bundle_id: other_bundle.clone(),
                expected_bundle_id: override_local_bundle.clone(),
                pinned: true,
                allow_repository_mismatch: false,
            },
        )
        .unwrap_err();
        assert!(rejected.contains("different repository"));

        let overridden = connect_project_to_remote_bundle_with_repository(
            &repo,
            ConnectProjectBundleRequest {
                local_project_id: override_replica.local_project_id,
                storage_id,
                bundle_id: other_bundle,
                expected_bundle_id: override_local_bundle,
                pinned: true,
                allow_repository_mismatch: true,
            },
        )
        .unwrap();
        assert_eq!(
            overridden.project.repository_fingerprint.as_deref(),
            Some("cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc")
        );
    }

    #[test]
    fn dependency_plan_generation_uses_a_valid_persisted_plan_id() {
        let temp = tempfile::tempdir().unwrap();
        let repo = repository(&temp);
        let storage_id = StorageId::parse("dependency-store").unwrap();
        let mut config = repo.load_config().unwrap();
        config.storages.push(StorageConfigV3 {
            id: storage_id.clone(),
            name: "Dependency store".to_string(),
            kind: StorageKind::Local,
            bucket: String::new(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            account_id: String::new(),
            s3_endpoint: String::new(),
            region: String::new(),
            local_dir: temp.path().join("store").to_string_lossy().into_owned(),
            included_default_exclusions: Vec::new(),
            supports_conditional_writes: None,
        });
        repo.save_config(config).unwrap();

        let project = register(&repo);
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let profile_id = add_profile(&repo, Provider::Codex, &temp.path().join("codex-home"));
        save_project_binding_with_repository(
            &repo,
            SaveProjectBindingRequest {
                local_project_id: project.local_project_id.clone(),
                project_root: project_root.to_string_lossy().into_owned(),
                profile_ids: BTreeMap::from([(Provider::Codex, profile_id)]),
                expected_revision: None,
            },
        )
        .unwrap();
        save_project_link_with_repository(
            &repo,
            SaveProjectLinkRequest {
                local_project_id: project.local_project_id.clone(),
                storage_id: storage_id.clone(),
                pinned: true,
            },
        )
        .unwrap();
        push_bundle_with_repository(&repo, &project.local_project_id, &storage_id).unwrap();

        let binding = repo
            .load_bindings()
            .unwrap()
            .active_for(&project.local_project_id)
            .cloned()
            .unwrap();
        let plan = plan_dependencies_with_repository(&repo, &project.bundle_id, &binding).unwrap();
        assert!(plan.plan_id.as_str().starts_with("plan-"));
        assert_eq!(repo.load_dependency_plan(&plan.plan_id).unwrap(), plan);
    }

    #[test]
    fn pull_review_apply_flow_materializes_a_codex_session_for_the_replica() {
        let temp = tempfile::tempdir().unwrap();
        let repo = repository(&temp);
        let storage_id = StorageId::parse("pull-store").unwrap();
        let bundle_id = BundleId::parse("df29babc833808e68ad0efa4d01d7d6d").unwrap();
        let mut config = repo.load_config().unwrap();
        config.storages.push(StorageConfigV3 {
            id: storage_id.clone(),
            name: "Pull store".to_string(),
            kind: StorageKind::Local,
            bucket: String::new(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            account_id: String::new(),
            s3_endpoint: String::new(),
            region: String::new(),
            local_dir: temp.path().join("store").to_string_lossy().into_owned(),
            included_default_exclusions: Vec::new(),
            supports_conditional_writes: None,
        });
        repo.save_config(config).unwrap();

        let source = register_local_project_with_repository(
            &repo,
            RegisterLocalProjectRequest {
                display_name: "healthGame".to_string(),
                repository_fingerprint: Some("a".repeat(64)),
                bundle_id: Some(bundle_id.clone()),
            },
        )
        .unwrap();
        let source_root = temp.path().join("healthGame");
        let source_codex = temp.path().join("source-codex");
        std::fs::create_dir_all(&source_root).unwrap();
        std::fs::create_dir_all(&source_codex).unwrap();
        let session_id = "019f0fb9-b140-7af3-8b7c-5d75c974b230";
        let session_relative = format!(
            "sessions/2026/07/18/rollout-2026-07-18T00-00-00-{}.jsonl",
            session_id
        );
        let source_cwd = source_root.to_string_lossy().into_owned();
        let transcript = format!(
            "{}\n{}\n",
            serde_json::json!({
                "type": "session_meta",
                "payload": { "id": session_id, "cwd": source_cwd },
            }),
            serde_json::json!({
                "type": "turn_context",
                "payload": { "cwd": source_cwd },
            }),
        );
        let source_session = source_codex.join(&session_relative);
        std::fs::create_dir_all(source_session.parent().unwrap()).unwrap();
        std::fs::write(&source_session, transcript).unwrap();
        std::fs::write(
            source_codex.join("session_index.jsonl"),
            format!(
                "{{\"id\":\"{}\",\"thread_name\":\"Change glb color to yellow\"}}\n",
                session_id
            ),
        )
        .unwrap();
        let source_profile = add_profile(&repo, Provider::Codex, &source_codex);
        save_project_binding_with_repository(
            &repo,
            SaveProjectBindingRequest {
                local_project_id: source.local_project_id.clone(),
                project_root: source_root.to_string_lossy().into_owned(),
                profile_ids: BTreeMap::from([(Provider::Codex, source_profile)]),
                expected_revision: None,
            },
        )
        .unwrap();
        save_project_link_with_repository(
            &repo,
            SaveProjectLinkRequest {
                local_project_id: source.local_project_id.clone(),
                storage_id: storage_id.clone(),
                pinned: true,
            },
        )
        .unwrap();
        push_bundle_with_repository(&repo, &source.local_project_id, &storage_id).unwrap();

        let target = register_local_project_with_repository(
            &repo,
            RegisterLocalProjectRequest {
                display_name: "gam2".to_string(),
                repository_fingerprint: Some("a".repeat(64)),
                bundle_id: Some(bundle_id.clone()),
            },
        )
        .unwrap();
        let target_root = temp.path().join("gam2");
        let target_codex = temp.path().join("target-codex");
        std::fs::create_dir_all(&target_root).unwrap();
        std::fs::create_dir_all(&target_codex).unwrap();
        let target_profile = add_profile(&repo, Provider::Codex, &target_codex);
        save_project_binding_with_repository(
            &repo,
            SaveProjectBindingRequest {
                local_project_id: target.local_project_id.clone(),
                project_root: target_root.to_string_lossy().into_owned(),
                profile_ids: BTreeMap::from([(Provider::Codex, target_profile)]),
                expected_revision: None,
            },
        )
        .unwrap();
        save_project_link_with_repository(
            &repo,
            SaveProjectLinkRequest {
                local_project_id: target.local_project_id.clone(),
                storage_id: storage_id.clone(),
                pinned: true,
            },
        )
        .unwrap();
        let target_binding = repo
            .load_bindings()
            .unwrap()
            .active_for(&target.local_project_id)
            .cloned()
            .unwrap();

        let restore =
            plan_bundle_restore_with_repository(&repo, &storage_id, &bundle_id, &target_binding)
                .unwrap();
        assert_eq!(restore.actions.len(), 2, "session plus filtered index");
        let dependencies =
            plan_dependencies_with_repository(&repo, &bundle_id, &target_binding).unwrap();
        assert!(dependencies.actions.is_empty());
        let before =
            get_bundle_readiness_with_repository(&repo, &bundle_id, &target_binding).unwrap();
        assert_eq!(before.state, "needs_setup");

        let approved = restore
            .actions
            .iter()
            .map(|action| action.action_id.clone())
            .collect::<Vec<_>>();
        let applied =
            apply_bundle_restore_with_repository(&repo, &restore.plan_id, approved).unwrap();
        assert!(applied.success, "{}", applied.message);
        assert_eq!(applied.applied_action_ids.len(), 2);

        let restored_session =
            std::fs::read_to_string(target_codex.join(session_relative)).unwrap();
        let rows = restored_session
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(rows[0]["payload"]["cwd"], target_root.to_str().unwrap());
        assert_eq!(rows[1]["payload"]["cwd"], target_root.to_str().unwrap());
        assert!(target_codex.join("session_index.jsonl").is_file());
        let materializations = repo.load_materializations().unwrap();
        assert_eq!(materializations.records.len(), 1);
        assert_eq!(
            materializations.records[0].status,
            MaterializationStatus::Complete
        );
        let after =
            get_bundle_readiness_with_repository(&repo, &bundle_id, &target_binding).unwrap();
        assert_eq!(after.state, "ready");
    }

    #[test]
    fn profile_probe_resolves_a_provider_child_and_deduplicates_it() {
        let temp = tempfile::tempdir().unwrap();
        let repo = repository(&temp);
        let container = temp.path().join("myconf2");
        let codex_home = container.join(".codex");
        std::fs::create_dir_all(&codex_home).unwrap();

        let probe = probe_provider_profile_with_repository(
            &repo,
            Provider::Codex,
            &container.to_string_lossy(),
        )
        .unwrap();
        assert!(probe.detected_child);
        assert_eq!(Path::new(&probe.resolved_path), codex_home);

        let first = create_provider_profile_with_repository(
            &repo,
            Provider::Codex,
            "myconf2",
            &container.to_string_lossy(),
        )
        .unwrap();
        let duplicate = create_provider_profile_with_repository(
            &repo,
            Provider::Codex,
            "ignored duplicate name",
            &codex_home.to_string_lossy(),
        )
        .unwrap();
        assert_eq!(first.profile_id, duplicate.profile_id);
        assert_eq!(repo.load_bindings().unwrap().profiles.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn profile_probe_rejects_a_final_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let repo = repository(&temp);
        let target = temp.path().join("real-codex-home");
        let link = temp.path().join("linked-codex-home");
        std::fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let error =
            probe_provider_profile_with_repository(&repo, Provider::Codex, &link.to_string_lossy())
                .unwrap_err();
        assert!(error.contains("cannot be a symlink"));
    }

    #[test]
    fn profile_kind_must_match_the_binding_provider() {
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let repo = V3Repository::from_app_data_dir(temp.path().join("app-data")).unwrap();
        let project = register(&repo);
        let codex_profile = add_profile(&repo, Provider::Codex, &temp.path().join("codex-home"));

        let error = save_project_binding_with_repository(
            &repo,
            SaveProjectBindingRequest {
                local_project_id: project.local_project_id,
                project_root: project_root.to_string_lossy().into_owned(),
                profile_ids: BTreeMap::from([(Provider::Claude, codex_profile)]),
                expected_revision: None,
            },
        )
        .unwrap_err();
        assert!(error.contains("cannot use Codex profile"));
    }

    #[test]
    fn profile_removal_is_blocked_only_while_an_active_project_uses_it() {
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let repo = V3Repository::from_app_data_dir(temp.path().join("app-data")).unwrap();
        let project = register(&repo);
        let profile_id = add_profile(&repo, Provider::Codex, &temp.path().join("codex-home"));
        save_project_binding_with_repository(
            &repo,
            SaveProjectBindingRequest {
                local_project_id: project.local_project_id.clone(),
                project_root: project_root.to_string_lossy().into_owned(),
                profile_ids: BTreeMap::from([(Provider::Codex, profile_id.clone())]),
                expected_revision: None,
            },
        )
        .unwrap();

        let error = remove_provider_profile_with_repository(&repo, &profile_id, 0).unwrap_err();
        assert!(error.contains("used by project"));
        detach_project_binding_with_repository(&repo, &project.local_project_id).unwrap();
        assert!(remove_provider_profile_with_repository(&repo, &profile_id, 0).unwrap());
    }

    #[test]
    fn remap_keeps_replica_identity_and_detaches_old_materializations() {
        let temp = tempfile::tempdir().unwrap();
        let first_root = temp.path().join("first");
        let second_root = temp.path().join("second");
        std::fs::create_dir_all(&first_root).unwrap();
        std::fs::create_dir_all(&second_root).unwrap();
        let app_data = temp.path().join("app-data");
        let repo = V3Repository::from_app_data_dir(&app_data).unwrap();
        let project = register(&repo);
        let profile_id = add_profile(&repo, Provider::Codex, &temp.path().join("codex-home"));
        let profile_ids = BTreeMap::from([(Provider::Codex, profile_id)]);
        let first = save_project_binding_with_repository(
            &repo,
            SaveProjectBindingRequest {
                local_project_id: project.local_project_id.clone(),
                project_root: first_root.to_string_lossy().into_owned(),
                profile_ids: profile_ids.clone(),
                expected_revision: None,
            },
        )
        .unwrap();
        let second = save_project_binding_with_repository(
            &repo,
            SaveProjectBindingRequest {
                local_project_id: project.local_project_id,
                project_root: second_root.to_string_lossy().into_owned(),
                profile_ids,
                expected_revision: Some(first.revision),
            },
        )
        .unwrap();
        assert_eq!(first.replica_id, second.replica_id);
        assert_eq!(second.revision, first.revision + 1);
        assert_eq!(second.project_root, second_root.to_string_lossy());
    }

    #[test]
    fn binding_rejects_local_store_overlap() {
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let app_data = temp.path().join("app-data");
        let repo = V3Repository::from_app_data_dir(&app_data).unwrap();
        let project = register(&repo);
        let profile_id = add_profile(&repo, Provider::Codex, &temp.path().join("codex-home"));
        let mut config = repo.load_config().unwrap();
        config.storages.push(StorageConfigV3 {
            id: StorageId::parse("inside").unwrap(),
            name: "Inside".to_string(),
            kind: StorageKind::Local,
            bucket: String::new(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            account_id: String::new(),
            s3_endpoint: String::new(),
            region: String::new(),
            local_dir: project_root.join("store").to_string_lossy().into_owned(),
            included_default_exclusions: Vec::new(),
            supports_conditional_writes: None,
        });
        repo.save_config(config).unwrap();
        let error = save_project_binding_with_repository(
            &repo,
            SaveProjectBindingRequest {
                local_project_id: project.local_project_id,
                project_root: project_root.to_string_lossy().into_owned(),
                profile_ids: BTreeMap::from([(Provider::Codex, profile_id)]),
                expected_revision: None,
            },
        )
        .unwrap_err();
        assert!(error.contains("overlaps local storage"));
    }

    #[test]
    fn binding_requires_and_persists_an_explicit_provider_profile() {
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let app_data = temp.path().join("app-data");
        let repo = V3Repository::from_app_data_dir(&app_data).unwrap();
        let project = register(&repo);
        let error = save_project_binding_with_repository(
            &repo,
            SaveProjectBindingRequest {
                local_project_id: project.local_project_id.clone(),
                project_root: project_root.to_string_lossy().into_owned(),
                profile_ids: BTreeMap::new(),
                expected_revision: None,
            },
        )
        .unwrap_err();
        assert!(error.contains("choose at least one local provider profile"));

        let codex_home = temp.path().join("codex-home");
        let profile_id = add_profile(&repo, Provider::Codex, &codex_home);
        let binding = save_project_binding_with_repository(
            &repo,
            SaveProjectBindingRequest {
                local_project_id: project.local_project_id,
                project_root: project_root.to_string_lossy().into_owned(),
                profile_ids: BTreeMap::from([(Provider::Codex, profile_id.clone())]),
                expected_revision: None,
            },
        )
        .unwrap();

        assert_eq!(binding.profile_ids.get(&Provider::Codex), Some(&profile_id));
        assert_eq!(
            binding.codex_home.as_deref(),
            Some(codex_home.to_str().unwrap())
        );
        let persisted = repo.load_bindings().unwrap();
        assert_eq!(persisted.bindings[0].profile_ids, binding.profile_ids);
        assert!(persisted.bindings[0].codex_home.is_none());
    }

    #[test]
    fn config_save_rejects_store_added_over_an_existing_binding() {
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path().join("project");
        let codex_home = temp.path().join("codex-home");
        let claude_home = temp.path().join("claude-home");
        for directory in [&project_root, &codex_home, &claude_home] {
            std::fs::create_dir_all(directory).unwrap();
        }
        let app_data = temp.path().join("app-data");
        let repo = V3Repository::from_app_data_dir(&app_data).unwrap();
        let project = register(&repo);
        let codex_profile = add_profile(&repo, Provider::Codex, &codex_home);
        let claude_profile = add_profile(&repo, Provider::Claude, &claude_home);
        save_project_binding_with_repository(
            &repo,
            SaveProjectBindingRequest {
                local_project_id: project.local_project_id,
                project_root: project_root.to_string_lossy().into_owned(),
                profile_ids: BTreeMap::from([
                    (Provider::Codex, codex_profile),
                    (Provider::Claude, claude_profile),
                ]),
                expected_revision: None,
            },
        )
        .unwrap();

        let mut config = repo.load_config().unwrap();
        config.storages.push(StorageConfigV3 {
            id: StorageId::parse("overlap").unwrap(),
            name: "Overlap".to_string(),
            kind: StorageKind::Local,
            bucket: String::new(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            account_id: String::new(),
            s3_endpoint: String::new(),
            region: String::new(),
            local_dir: project_root
                .join("bundle-store")
                .to_string_lossy()
                .into_owned(),
            included_default_exclusions: Vec::new(),
            supports_conditional_writes: None,
        });
        let error = save_project_sync_config_with_repository(&repo, config).unwrap_err();
        assert!(error.contains("overlaps project root"));
    }
}
