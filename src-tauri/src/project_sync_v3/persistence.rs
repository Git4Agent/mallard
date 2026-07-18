//! Bounded, atomic, schema-3 local persistence.
//!
//! Every file lives below `app_data/v3`; schema-2 configuration, baselines,
//! backups, and machine records are neither read nor overwritten.  All
//! read-modify-write operations share a process lock and revision check so
//! concurrent Tauri commands cannot silently lose changes.

use super::domain::{
    DependencyApplications, DependencyPlan, MachineProjectState, Materializations, PlanId,
    RestorePlan, SyncConfigV3, MACHINE_PROJECT_SCHEMA_V1,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::Manager;

const MAX_CONFIG_BYTES: u64 = 8 * 1024 * 1024;
const MAX_BINDINGS_BYTES: u64 = 8 * 1024 * 1024;
const MAX_MATERIALIZATIONS_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RESTORE_PLAN_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DEPENDENCY_PLAN_BYTES: u64 = 16 * 1024 * 1024;
const MAX_DEPENDENCY_APPLICATIONS_BYTES: u64 = 64 * 1024 * 1024;

static PERSISTENCE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug)]
pub struct V3Repository {
    root: PathBuf,
}

impl V3Repository {
    /// Resolve the clean schema-3 namespace for any Tauri runtime.
    pub fn from_app<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<Self, String> {
        let app_data = app
            .path()
            .app_data_dir()
            .map_err(|error| error.to_string())?;
        Self::from_app_data_dir(app_data)
    }

    /// Construct from the application-data directory.  Tests and non-Tauri
    /// helpers use this to exercise exactly the production paths.
    pub fn from_app_data_dir(app_data: impl Into<PathBuf>) -> Result<Self, String> {
        let app_data = app_data.into();
        if !app_data.is_absolute() {
            return Err(format!(
                "app-data directory must be absolute: '{}'",
                app_data.display()
            ));
        }
        let repository = Self {
            root: app_data.join("v3"),
        };
        repository.ensure_root()?;
        Ok(repository)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config_path(&self) -> PathBuf {
        self.root.join("sync_config.json")
    }

    pub fn machine_projects_path(&self) -> PathBuf {
        self.root.join("machine_projects.json")
    }

    pub fn materializations_path(&self) -> PathBuf {
        self.root.join("materializations.json")
    }

    pub fn dependency_applications_path(&self) -> PathBuf {
        self.root.join("dependency_applications.json")
    }

    pub fn restore_plan_path(&self, plan_id: &PlanId) -> PathBuf {
        self.root
            .join("restore_plans")
            .join(format!("{}.json", plan_id.as_str()))
    }

    pub fn dependency_plan_path(&self, plan_id: &PlanId) -> PathBuf {
        self.root
            .join("dependency_plans")
            .join(format!("{}.json", plan_id.as_str()))
    }

    pub fn backups_dir(&self) -> Result<PathBuf, String> {
        let _guard = persistence_guard()?;
        let path = self.root.join("backups");
        self.ensure_directory(&path)?;
        Ok(path)
    }

    pub fn load_config(&self) -> Result<SyncConfigV3, String> {
        let _guard = persistence_guard()?;
        self.load_config_unlocked()
    }

    /// Full-document optimistic save used by storage/settings UI.  The
    /// caller must submit the revision it read; the returned value carries
    /// the next revision.
    pub fn save_config(&self, mut config: SyncConfigV3) -> Result<SyncConfigV3, String> {
        let _guard = persistence_guard()?;
        let current = self.load_config_unlocked()?;
        if config.revision != current.revision {
            return Err(format!(
                "project-sync config changed (expected revision {}, current {})",
                config.revision, current.revision
            ));
        }
        config.schema = super::domain::LOCAL_SCHEMA_V3;
        config.revision = current.revision.saturating_add(1);
        config.validate()?;
        write_json_atomic(&self.root, &self.config_path(), &config, MAX_CONFIG_BYTES)?;
        Ok(config)
    }

    /// Transactional config mutation for focused project/link/recipe
    /// commands.  The closure runs while the process persistence lock is held.
    pub fn mutate_config<T>(
        &self,
        mutate: impl FnOnce(&mut SyncConfigV3) -> Result<T, String>,
    ) -> Result<T, String> {
        let _guard = persistence_guard()?;
        let mut config = self.load_config_unlocked()?;
        let result = mutate(&mut config)?;
        config.schema = super::domain::LOCAL_SCHEMA_V3;
        config.revision = config.revision.saturating_add(1);
        config.validate()?;
        write_json_atomic(&self.root, &self.config_path(), &config, MAX_CONFIG_BYTES)?;
        Ok(result)
    }

    pub fn load_bindings(&self) -> Result<MachineProjectState, String> {
        let _guard = persistence_guard()?;
        let config = self.load_config_unlocked()?;
        self.load_bindings_unlocked(&config)
    }

    pub fn mutate_bindings<T>(
        &self,
        mutate: impl FnOnce(&SyncConfigV3, &mut MachineProjectState) -> Result<T, String>,
    ) -> Result<T, String> {
        let _guard = persistence_guard()?;
        let config = self.load_config_unlocked()?;
        let mut bindings = self.load_bindings_unlocked(&config)?;
        let result = mutate(&config, &mut bindings)?;
        bindings.schema = MACHINE_PROJECT_SCHEMA_V1;
        bindings.revision = bindings.revision.saturating_add(1);
        bindings.validate(&config)?;
        write_json_atomic(
            &self.root,
            &self.machine_projects_path(),
            &bindings,
            MAX_BINDINGS_BYTES,
        )?;
        Ok(result)
    }

    pub fn load_materializations(&self) -> Result<Materializations, String> {
        let _guard = persistence_guard()?;
        let config = self.load_config_unlocked()?;
        self.load_materializations_unlocked(&config)
    }

    pub fn mutate_materializations<T>(
        &self,
        mutate: impl FnOnce(&SyncConfigV3, &mut Materializations) -> Result<T, String>,
    ) -> Result<T, String> {
        let _guard = persistence_guard()?;
        let config = self.load_config_unlocked()?;
        let mut materializations = self.load_materializations_unlocked(&config)?;
        let result = mutate(&config, &mut materializations)?;
        materializations.schema = super::domain::LOCAL_SCHEMA_V3;
        materializations.revision = materializations.revision.saturating_add(1);
        materializations.validate(&config)?;
        write_json_atomic(
            &self.root,
            &self.materializations_path(),
            &materializations,
            MAX_MATERIALIZATIONS_BYTES,
        )?;
        Ok(result)
    }

    pub fn load_dependency_applications(&self) -> Result<DependencyApplications, String> {
        let _guard = persistence_guard()?;
        let config = self.load_config_unlocked()?;
        self.load_dependency_applications_unlocked(&config)
    }

    pub fn mutate_dependency_applications<T>(
        &self,
        mutate: impl FnOnce(&SyncConfigV3, &mut DependencyApplications) -> Result<T, String>,
    ) -> Result<T, String> {
        let _guard = persistence_guard()?;
        let config = self.load_config_unlocked()?;
        let mut applications = self.load_dependency_applications_unlocked(&config)?;
        let result = mutate(&config, &mut applications)?;
        applications.schema = super::domain::LOCAL_SCHEMA_V3;
        applications.revision = applications.revision.saturating_add(1);
        applications.validate(&config)?;
        write_json_atomic(
            &self.root,
            &self.dependency_applications_path(),
            &applications,
            MAX_DEPENDENCY_APPLICATIONS_BYTES,
        )?;
        Ok(result)
    }

    /// Restore plans are immutable, generation-pinned documents.  Reusing a
    /// plan ID is always an error instead of replacing an approval surface.
    pub fn save_restore_plan(&self, plan: &RestorePlan) -> Result<(), String> {
        let _guard = persistence_guard()?;
        plan.validate()?;
        let path = self.restore_plan_path(&plan.plan_id);
        self.ensure_directory(path.parent().ok_or("restore plan has no parent")?)?;
        if fs::symlink_metadata(&path).is_ok() {
            return Err(format!("restore plan '{}' already exists", plan.plan_id));
        }
        write_json_atomic(&self.root, &path, plan, MAX_RESTORE_PLAN_BYTES)
    }

    pub fn load_restore_plan(&self, plan_id: &PlanId) -> Result<RestorePlan, String> {
        let _guard = persistence_guard()?;
        let path = self.restore_plan_path(plan_id);
        let plan: RestorePlan = read_json_bounded(&self.root, &path, MAX_RESTORE_PLAN_BYTES)?
            .ok_or_else(|| format!("restore plan '{}' does not exist", plan_id))?;
        plan.validate()?;
        Ok(plan)
    }

    pub fn discard_restore_plan(&self, plan_id: &PlanId) -> Result<bool, String> {
        let _guard = persistence_guard()?;
        let path = self.restore_plan_path(plan_id);
        ensure_no_symlinks(&self.root, &path)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(format!("remove restore plan '{}': {}", plan_id, error)),
        }
    }

    pub fn save_dependency_plan(&self, plan: &DependencyPlan) -> Result<(), String> {
        let _guard = persistence_guard()?;
        plan.validate()?;
        let path = self.dependency_plan_path(&plan.plan_id);
        self.ensure_directory(path.parent().ok_or("dependency plan has no parent")?)?;
        if fs::symlink_metadata(&path).is_ok() {
            return Err(format!("dependency plan '{}' already exists", plan.plan_id));
        }
        write_json_atomic(&self.root, &path, plan, MAX_DEPENDENCY_PLAN_BYTES)
    }

    pub fn load_dependency_plan(&self, plan_id: &PlanId) -> Result<DependencyPlan, String> {
        let _guard = persistence_guard()?;
        let path = self.dependency_plan_path(plan_id);
        let plan: DependencyPlan = read_json_bounded(&self.root, &path, MAX_DEPENDENCY_PLAN_BYTES)?
            .ok_or_else(|| format!("dependency plan '{}' does not exist", plan_id))?;
        plan.validate()?;
        Ok(plan)
    }

    fn load_config_unlocked(&self) -> Result<SyncConfigV3, String> {
        let config: SyncConfigV3 =
            read_json_bounded(&self.root, &self.config_path(), MAX_CONFIG_BYTES)?
                .unwrap_or_default();
        config.validate()?;
        Ok(config)
    }

    fn load_bindings_unlocked(&self, config: &SyncConfigV3) -> Result<MachineProjectState, String> {
        let bindings: MachineProjectState = read_json_bounded(
            &self.root,
            &self.machine_projects_path(),
            MAX_BINDINGS_BYTES,
        )?
        .unwrap_or_default();
        bindings.validate(config)?;
        Ok(bindings)
    }

    fn load_materializations_unlocked(
        &self,
        config: &SyncConfigV3,
    ) -> Result<Materializations, String> {
        let materializations: Materializations = read_json_bounded(
            &self.root,
            &self.materializations_path(),
            MAX_MATERIALIZATIONS_BYTES,
        )?
        .unwrap_or_default();
        materializations.validate(config)?;
        Ok(materializations)
    }

    fn load_dependency_applications_unlocked(
        &self,
        config: &SyncConfigV3,
    ) -> Result<DependencyApplications, String> {
        let applications: DependencyApplications = read_json_bounded(
            &self.root,
            &self.dependency_applications_path(),
            MAX_DEPENDENCY_APPLICATIONS_BYTES,
        )?
        .unwrap_or_default();
        applications.validate(config)?;
        Ok(applications)
    }

    fn ensure_root(&self) -> Result<(), String> {
        let parent = self
            .root
            .parent()
            .ok_or_else(|| format!("v3 root '{}' has no parent", self.root.display()))?;
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "create app-data directory '{}': {}",
                parent.display(),
                error
            )
        })?;
        if let Ok(metadata) = fs::symlink_metadata(&self.root) {
            if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
                return Err(format!(
                    "schema-3 app directory '{}' is not a real directory",
                    self.root.display()
                ));
            }
        } else {
            fs::create_dir(&self.root).map_err(|error| {
                format!("create v3 directory '{}': {}", self.root.display(), error)
            })?;
        }
        ensure_no_symlinks(&self.root, &self.root)
    }

    fn ensure_directory(&self, directory: &Path) -> Result<(), String> {
        let relative = directory.strip_prefix(&self.root).map_err(|_| {
            format!(
                "schema-3 path '{}' escapes '{}'",
                directory.display(),
                self.root.display()
            )
        })?;
        let mut current = self.root.clone();
        ensure_real_directory(&current)?;
        for component in relative.components() {
            current.push(component.as_os_str());
            match fs::symlink_metadata(&current) {
                Ok(metadata)
                    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {}
                Ok(_) => {
                    return Err(format!(
                        "schema-3 directory '{}' is not a real directory",
                        current.display()
                    ))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    match fs::create_dir(&current) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                            ensure_real_directory(&current)?;
                        }
                        Err(error) => {
                            return Err(format!(
                                "create schema-3 directory '{}': {}",
                                current.display(),
                                error
                            ));
                        }
                    }
                }
                Err(error) => {
                    return Err(format!("inspect '{}': {}", current.display(), error));
                }
            }
        }
        Ok(())
    }
}

fn persistence_guard() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    PERSISTENCE_LOCK
        .lock()
        .map_err(|_| "project-sync persistence lock is poisoned".to_string())
}

fn ensure_real_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("inspect directory '{}': {}", path.display(), error))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(format!("'{}' is not a real directory", path.display()));
    }
    Ok(())
}

fn ensure_no_symlinks(root: &Path, destination: &Path) -> Result<(), String> {
    let relative = destination.strip_prefix(root).map_err(|_| {
        format!(
            "schema-3 path '{}' escapes '{}'",
            destination.display(),
            root.display()
        )
    })?;
    let mut current = root.to_path_buf();
    for component in std::iter::once(None).chain(relative.components().map(Some)) {
        if let Some(component) = component {
            current.push(component.as_os_str());
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "schema-3 path traverses symlink '{}'",
                    current.display()
                ))
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("inspect '{}': {}", current.display(), error)),
        }
    }
    Ok(())
}

fn read_json_bounded<T: DeserializeOwned>(
    root: &Path,
    path: &Path,
    max_bytes: u64,
) -> Result<Option<T>, String> {
    ensure_no_symlinks(root, path)?;
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("inspect '{}': {}", path.display(), error)),
    };
    if !metadata.file_type().is_file() {
        return Err(format!("'{}' is not a regular file", path.display()));
    }
    if metadata.len() > max_bytes {
        return Err(format!(
            "'{}' exceeds the {} byte limit",
            path.display(),
            max_bytes
        ));
    }
    let file =
        fs::File::open(path).map_err(|error| format!("open '{}': {}", path.display(), error))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read '{}': {}", path.display(), error))?;
    if bytes.len() as u64 > max_bytes {
        return Err(format!(
            "'{}' changed beyond the {} byte limit while reading",
            path.display(),
            max_bytes
        ));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| format!("parse '{}': {}", path.display(), error))
}

fn write_json_atomic<T: Serialize>(
    root: &Path,
    path: &Path,
    value: &T,
    max_bytes: u64,
) -> Result<(), String> {
    ensure_no_symlinks(root, path)?;
    let parent = path
        .parent()
        .ok_or_else(|| format!("'{}' has no parent", path.display()))?;
    if !parent.exists() {
        return Err(format!(
            "parent directory '{}' does not exist",
            parent.display()
        ));
    }
    ensure_real_directory(parent)?;
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    if bytes.len() as u64 > max_bytes {
        return Err(format!(
            "serialized '{}' exceeds the {} byte limit",
            path.display(),
            max_bytes
        ));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| format!("create temporary file in '{}': {}", parent.display(), error))?;
    temporary
        .as_file_mut()
        .write_all(&bytes)
        .map_err(|error| format!("write temporary '{}': {}", path.display(), error))?;
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(|error| format!("sync temporary '{}': {}", path.display(), error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("secure temporary '{}': {}", path.display(), error))?;
    }
    temporary
        .persist(path)
        .map_err(|error| format!("publish '{}': {}", path.display(), error.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_sync_v3::domain::{
        generated_named_id, BundleId, BundleRecipe, LocalProjectId, LocalProjectRegistration,
        LOCAL_SCHEMA_V3,
    };
    use std::collections::BTreeMap;

    fn repository(temp: &tempfile::TempDir) -> V3Repository {
        V3Repository::from_app_data_dir(temp.path()).unwrap()
    }

    fn project() -> LocalProjectRegistration {
        LocalProjectRegistration {
            local_project_id: LocalProjectId::parse("project-a").unwrap(),
            bundle_id: BundleId::parse("0123456789abcdef0123456789abcdef").unwrap(),
            display_name: "Project A".to_string(),
            repository_fingerprint: None,
            recipe: BundleRecipe::default(),
            recipe_bases: BTreeMap::new(),
            revision: 0,
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn v3_persistence_never_touches_schema2_files() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("sync_config.json");
        fs::write(&old, b"{\"schema\":2,\"keep\":true}\n").unwrap();
        let repo = repository(&temp);
        let saved = repo.save_config(SyncConfigV3::default()).unwrap();

        assert_eq!(saved.schema, LOCAL_SCHEMA_V3);
        assert_eq!(saved.revision, 1);
        assert_eq!(fs::read(&old).unwrap(), b"{\"schema\":2,\"keep\":true}\n");
        assert!(repo.config_path().starts_with(temp.path().join("v3")));
    }

    #[test]
    fn config_writes_are_revision_guarded_and_roundtrip() {
        let temp = tempfile::tempdir().unwrap();
        let repo = repository(&temp);
        let mut first = repo.load_config().unwrap();
        first.projects.push(project());
        let saved = repo.save_config(first.clone()).unwrap();
        assert_eq!(saved.revision, 1);
        assert_eq!(repo.load_config().unwrap(), saved);

        first.projects.clear();
        assert!(repo.save_config(first).unwrap_err().contains("changed"));
        assert_eq!(repo.load_config().unwrap().projects.len(), 1);
    }

    #[test]
    fn focused_mutation_is_atomic_at_the_document_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let repo = repository(&temp);
        let returned = repo
            .mutate_config(|config| {
                config.projects.push(project());
                Ok(config.projects[0].local_project_id.clone())
            })
            .unwrap();
        assert_eq!(returned.as_str(), "project-a");
        assert_eq!(repo.load_config().unwrap().revision, 1);
    }

    #[test]
    fn restore_plan_ids_get_private_namespaced_paths() {
        let temp = tempfile::tempdir().unwrap();
        let repo = repository(&temp);
        let plan = PlanId::parse(generated_named_id("plan").unwrap()).unwrap();
        let path = repo.restore_plan_path(&plan);
        assert!(path.starts_with(temp.path().join("v3/restore_plans")));
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            format!("{}.json", plan)
        );
    }

    #[cfg(unix)]
    #[test]
    fn app_owned_v3_root_must_not_be_a_symlink() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), temp.path().join("v3")).unwrap();
        let error = V3Repository::from_app_data_dir(temp.path()).unwrap_err();
        assert!(error.contains("not a real directory"));
    }
}
