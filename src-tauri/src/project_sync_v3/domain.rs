//! Schema-3 local project-sync state and schema-4 portable bundle model.
//!
//! This module intentionally contains no Tauri or filesystem code.  All
//! strings which can become cloud namespace components are validated on
//! deserialization, so persistence and transport code cannot accidentally
//! turn untrusted JSON into an unchecked path or identifier.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::path::{Component, Path};

pub const LOCAL_SCHEMA_V3: u32 = 3;
pub const MACHINE_PROJECT_SCHEMA_V1: u32 = 1;
pub const BUNDLE_SCHEMA_V4: u32 = 4;
pub const RECIPE_SCHEMA_V1: u32 = 1;
pub const RESTORE_PLAN_SCHEMA_V1: u32 = 1;
pub const DEPENDENCY_PLAN_SCHEMA_V1: u32 = 1;
pub const SETUP_DRAFT_SCHEMA_V1: u32 = 1;
pub const SETUP_TRANSACTION_SCHEMA_V1: u32 = 1;
pub const MAX_SETUP_DRAFTS: usize = 64;

pub const MAX_PROJECTS: usize = 1_024;
pub const MAX_STORAGES: usize = 128;
pub const MAX_LINKS: usize = 4_096;
pub const MAX_BINDINGS: usize = 4_096;
pub const MAX_PROVIDER_PROFILES: usize = 1_024;
pub const MAX_RESOURCES: usize = 20_000;
pub const MAX_FILES: usize = 100_000;
pub const MAX_ACTIONS: usize = 100_000;
pub const MAX_LOGICAL_PATH_BYTES: usize = 1_024;
pub const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_PROJECT_CONTENT_FILE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_PROJECT_CONTENT_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

fn valid_named_id(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-._:".contains(&byte)
        })
        && !value
            .chars()
            .last()
            .is_some_and(|character| matches!(character, '-' | '.' | '_' | ':'))
        && !value.contains("..")
}

fn validate_named_id(label: &str, value: &str) -> Result<(), String> {
    if valid_named_id(value) {
        Ok(())
    } else {
        Err(format!(
            "{} must be 1-128 lowercase ASCII identifier characters: '{}'",
            label,
            value.escape_debug()
        ))
    }
}

macro_rules! named_id {
    ($name:ident, $label:literal) => {
        #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: impl Into<String>) -> Result<Self, String> {
                let value = value.into();
                validate_named_id($label, &value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = String;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::parse(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> String {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

named_id!(ResourceId, "resource id");
named_id!(LocalProjectId, "local project id");
named_id!(StorageId, "storage id");
named_id!(ReplicaId, "replica id");
named_id!(PlanId, "plan id");
named_id!(ActionId, "action id");
named_id!(MaterializationId, "materialization id");
named_id!(LocalProviderProfileId, "local provider profile id");
named_id!(SetupDraftId, "setup draft id");

/// A bundle ID is an opaque, generated 128-bit value.  Requiring its exact
/// lowercase hexadecimal representation makes it safe as one cloud key
/// component and prevents paths or user labels from becoming identity.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(try_from = "String", into = "String")]
pub struct BundleId(String);

impl BundleId {
    pub fn parse(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        if value.len() != 32
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(format!(
                "bundle id must be exactly 32 lowercase hexadecimal characters: '{}'",
                value.escape_debug()
            ));
        }
        Ok(Self(value))
    }

    pub fn generate() -> Result<Self, String> {
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes).map_err(|error| format!("random bundle id: {}", error))?;
        Self::parse(hex_lower(&bytes))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for BundleId {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<BundleId> for String {
    fn from(value: BundleId) -> String {
        value.0
    }
}

impl fmt::Display for BundleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

pub fn generated_named_id(prefix: &str) -> Result<String, String> {
    if prefix.is_empty()
        || !prefix
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return Err(format!("invalid generated id prefix '{}'", prefix));
    }
    let mut bytes = [0_u8; 12];
    getrandom::fill(&mut bytes).map_err(|error| format!("random id: {}", error))?;
    Ok(format!("{}-{}", prefix, hex_lower(&bytes)))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}

/// A portable path inside one bundle.  It is never joined directly to a
/// machine path; provider adapters own that mapping.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(try_from = "String", into = "String")]
pub struct LogicalPath(String);

impl LogicalPath {
    pub fn parse(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        validate_logical_path(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for LogicalPath {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<LogicalPath> for String {
    fn from(value: LogicalPath) -> String {
        value.0
    }
}

impl fmt::Display for LogicalPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn validate_logical_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.len() > MAX_LOGICAL_PATH_BYTES
        || path.starts_with('/')
        || path.ends_with('/')
        || path.contains('\\')
        || path.contains(':')
        || path.chars().any(char::is_control)
    {
        return Err(format!("unsafe logical path '{}'", path.escape_debug()));
    }
    let mut components = path.split('/');
    let root = components.next().unwrap_or_default();
    if !matches!(root, "project" | "state" | "dependencies" | "requirements") {
        return Err(format!("unknown logical namespace '{}'", root));
    }
    let mut saw_child = false;
    for component in components {
        saw_child = true;
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.len() > 255
            || component
                .chars()
                .last()
                .is_some_and(|character| matches!(character, ' ' | '.'))
        {
            return Err(format!("unsafe logical path '{}'", path.escape_debug()));
        }
        let stem = component.split('.').next().unwrap_or_default();
        if WINDOWS_RESERVED_NAMES
            .iter()
            .any(|reserved| stem.eq_ignore_ascii_case(reserved))
        {
            return Err(format!("reserved device name in logical path '{}'", path));
        }
    }
    if !saw_child {
        return Err(format!("logical namespace '{}' is not a file path", root));
    }
    Ok(())
}

pub fn validate_sha256(label: &str, digest: &str) -> Result<(), String> {
    if digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(format!("{} is not a lowercase SHA-256 digest", label))
    }
}

fn validate_display_text(label: &str, value: &str, allow_empty: bool) -> Result<(), String> {
    if (!allow_empty && value.trim().is_empty())
        || value.len() > 1_024
        || value.chars().any(char::is_control)
    {
        Err(format!("invalid {}", label))
    } else {
        Ok(())
    }
}

/// Validate either the effective name or install-directory component of a
/// global custom skill. They are separate values, but both use the providers'
/// current portable single-component grammar.
pub fn validate_skill_name(label: &str, value: &str) -> Result<(), String> {
    let valid_start = value
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric());
    if !valid_start
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || value == "."
        || value == ".."
        || value.ends_with('.')
    {
        return Err(format!("invalid {}: '{}'", label, value));
    }
    Ok(())
}

pub fn validate_absolute_clean_path(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 4_096 || value.chars().any(char::is_control) {
        return Err(format!("invalid {}", label));
    }
    let path = Path::new(value);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(format!(
            "{} must be an absolute clean path: '{}'",
            label, value
        ));
    }
    Ok(())
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Codex,
    Claude,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BundleKind {
    Project,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StorageKind {
    S3,
    Local,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct StorageConfigV3 {
    pub id: StorageId,
    pub name: String,
    pub kind: StorageKind,
    #[serde(default)]
    pub bucket: String,
    #[serde(default)]
    pub access_key_id: String,
    #[serde(default)]
    pub secret_access_key: String,
    #[serde(default)]
    pub account_id: String,
    #[serde(default)]
    pub s3_endpoint: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub local_dir: String,
    #[serde(default)]
    pub included_default_exclusions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_conditional_writes: Option<bool>,
}

impl StorageConfigV3 {
    pub fn validate(&self) -> Result<(), String> {
        validate_display_text("storage name", &self.name, true)?;
        match self.kind {
            StorageKind::Local => {
                if self.local_dir.is_empty() {
                    return Err(format!("storage '{}' needs a local directory", self.id));
                }
                validate_absolute_clean_path("local storage directory", &self.local_dir)?;
            }
            StorageKind::S3 => {
                if self.bucket.trim().is_empty() {
                    return Err(format!("storage '{}' needs a bucket", self.id));
                }
            }
        }
        if self.included_default_exclusions.len() > 1_024 {
            return Err(format!("storage '{}' has too many exclusions", self.id));
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    CodexConversation,
    ClaudeConversation,
    ProjectFile,
    ProjectContentFile,
    ProjectContentDirectory,
    ProjectMemory,
    Agent,
    Command,
    Rule,
    Prompt,
    ProjectSkill,
    StandaloneSkill,
    Plugin,
    McpServer,
    Hook,
    Setting,
    Requirement,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceScope {
    Project,
    ProviderState,
    Dependency,
    Requirement,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApplyPolicy {
    SafeFile,
    Merge,
    ExplicitInstall,
    ExplicitReview,
    ManualOnly,
    Never,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Provenance {
    ProjectLocal {
        relative_path: String,
    },
    StandaloneSnapshot {
        stable_key: String,
        source_digest: String,
    },
    Git {
        repository_fingerprint: String,
        rev: String,
        subdir: String,
    },
    Plugin {
        provider: Provider,
        plugin_id: String,
    },
    Unknown,
}

impl Provenance {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::ProjectLocal { relative_path } => {
                if relative_path.is_empty()
                    || Path::new(relative_path).is_absolute()
                    || Path::new(relative_path).components().any(|component| {
                        matches!(component, Component::CurDir | Component::ParentDir)
                    })
                {
                    return Err("invalid project-local provenance path".to_string());
                }
            }
            Self::StandaloneSnapshot {
                stable_key,
                source_digest,
            } => {
                validate_display_text("standalone skill stable key", stable_key, false)?;
                validate_sha256("standalone skill source digest", source_digest)?;
            }
            Self::Git {
                repository_fingerprint,
                rev,
                subdir,
            } => {
                validate_sha256("repository fingerprint", repository_fingerprint)?;
                validate_display_text("Git revision", rev, false)?;
                if Path::new(subdir).is_absolute()
                    || Path::new(subdir).components().any(|component| {
                        matches!(component, Component::CurDir | Component::ParentDir)
                    })
                {
                    return Err("invalid Git provenance subdirectory".to_string());
                }
            }
            Self::Plugin { plugin_id, .. } => {
                validate_display_text("plugin id", plugin_id, false)?;
            }
            Self::Unknown => {}
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ResourceDescriptor {
    pub resource_id: ResourceId,
    pub kind: ResourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<Provider>,
    pub scope: ResourceScope,
    pub display_name: String,
    pub provenance: Provenance,
    pub apply_policy: ApplyPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_cwd: Option<String>,
    pub codec_version: u32,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

impl ResourceDescriptor {
    pub fn validate(&self) -> Result<(), String> {
        validate_display_text("resource display name", &self.display_name, false)?;
        self.provenance.validate()?;
        if self.codec_version == 0 {
            return Err(format!(
                "resource '{}' has codec version 0",
                self.resource_id
            ));
        }
        if let Some(relative) = &self.relative_cwd {
            // Provider adapters use `.` as the sole canonical spelling for
            // the bound project root. Embedded `.` and every `..` remain
            // forbidden.
            let invalid_component = relative != "."
                && Path::new(relative)
                    .components()
                    .any(|component| matches!(component, Component::CurDir | Component::ParentDir));
            if relative.is_empty() || Path::new(relative).is_absolute() || invalid_component {
                return Err(format!(
                    "resource '{}' has invalid relative cwd",
                    self.resource_id
                ));
            }
        }
        if self.metadata.len() > 256
            || self.metadata.iter().any(|(key, value)| {
                key.is_empty()
                    || key.len() > 256
                    || value.len() > 4_096
                    || key.chars().any(char::is_control)
                    || value.chars().any(char::is_control)
            })
        {
            return Err(format!(
                "resource '{}' has invalid metadata",
                self.resource_id
            ));
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RecipeEntry {
    pub resource_id: ResourceId,
    pub apply_policy: ApplyPolicy,
    #[serde(default)]
    pub required: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BundleRecipe {
    pub schema_version: u32,
    pub revision: u64,
    #[serde(default)]
    pub entries: BTreeMap<ResourceId, RecipeEntry>,
}

impl Default for BundleRecipe {
    fn default() -> Self {
        Self {
            schema_version: RECIPE_SCHEMA_V1,
            revision: 0,
            entries: BTreeMap::new(),
        }
    }
}

impl BundleRecipe {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != RECIPE_SCHEMA_V1 {
            return Err(format!(
                "unsupported recipe schema {} (expected {})",
                self.schema_version, RECIPE_SCHEMA_V1
            ));
        }
        if self.entries.len() > MAX_RESOURCES {
            return Err(format!("recipe exceeds {} resources", MAX_RESOURCES));
        }
        for (id, entry) in &self.entries {
            if id != &entry.resource_id {
                return Err(format!(
                    "recipe key '{}' does not match entry '{}'",
                    id, entry.resource_id
                ));
            }
            if entry.apply_policy == ApplyPolicy::Never {
                return Err(format!("recipe cannot select Never resource '{}'", id));
            }
        }
        Ok(())
    }

    pub fn selected_ids(&self) -> BTreeSet<ResourceId> {
        self.entries.keys().cloned().collect()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RecipeBase {
    pub generation: u64,
    pub manifest_sha256: String,
    /// Immutable commit that established this reviewed base. Keeping the
    /// commit ID locally lets status views reload the exact historical
    /// manifest after the storage head advances.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_id: Option<String>,
    pub recipe_revision: u64,
    /// The machine binding that established this reviewed remote base. Older
    /// experimental bases deserialize as `None` and cannot authorize Push.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_pull_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_push_at: Option<u64>,
}

impl RecipeBase {
    pub fn validate(&self) -> Result<(), String> {
        validate_sha256("recipe base manifest", &self.manifest_sha256)?;
        if let Some(commit_id) = &self.commit_id {
            validate_named_id("recipe base commit", commit_id)?;
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LocalProjectRegistration {
    pub local_project_id: LocalProjectId,
    pub bundle_id: BundleId,
    pub display_name: String,
    /// Machine-local nickname shown instead of `display_name`. Never pushed:
    /// the remote bundle keeps `display_name`, so two checkouts of the same
    /// repo can be told apart without renaming it for every replica.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_fingerprint: Option<String>,
    pub recipe: BundleRecipe,
    #[serde(default)]
    pub recipe_bases: BTreeMap<StorageId, RecipeBase>,
    pub revision: u64,
    pub created_at: u64,
    pub updated_at: u64,
}

impl LocalProjectRegistration {
    pub fn validate(&self) -> Result<(), String> {
        validate_display_text("project display name", &self.display_name, false)?;
        if let Some(alias) = &self.local_alias {
            validate_display_text("project local alias", alias, false)?;
        }
        if let Some(fingerprint) = &self.repository_fingerprint {
            validate_sha256("repository fingerprint", fingerprint)?;
        }
        self.recipe.validate()?;
        for base in self.recipe_bases.values() {
            base.validate()?;
        }
        if self.created_at > self.updated_at {
            return Err(format!(
                "project '{}' has invalid timestamps",
                self.local_project_id
            ));
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ProjectStorageLink {
    pub local_project_id: LocalProjectId,
    pub storage_id: StorageId,
    /// Must equal the registration's bundle ID.  Keeping it explicit makes
    /// remote requests self-contained while validation prevents split identity.
    pub bundle_id: BundleId,
    /// Destination-specific selection from the last explicit Push. Older
    /// schema-3 links fall back to the project's default recipe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<BundleRecipe>,
    /// Machine-local, destination-specific decisions for generic project
    /// content. These preferences are never serialized into a remote bundle.
    #[serde(default)]
    pub project_content_preferences: ProjectContentPreferences,
    #[serde(default)]
    pub pinned: bool,
    pub created_at: u64,
}

pub const PROJECT_CONTENT_PREFERENCES_SCHEMA_V1: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ProjectContentPreferences {
    pub schema_version: u32,
    pub revision: u64,
    #[serde(default)]
    pub excluded_resource_ids: BTreeSet<ResourceId>,
}

impl Default for ProjectContentPreferences {
    fn default() -> Self {
        Self {
            schema_version: PROJECT_CONTENT_PREFERENCES_SCHEMA_V1,
            revision: 0,
            excluded_resource_ids: BTreeSet::new(),
        }
    }
}

impl ProjectContentPreferences {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != PROJECT_CONTENT_PREFERENCES_SCHEMA_V1 {
            return Err(format!(
                "unsupported project-content preferences schema {}",
                self.schema_version
            ));
        }
        if self.excluded_resource_ids.len() > MAX_RESOURCES {
            return Err(format!(
                "project-content preferences exceed {} exclusions",
                MAX_RESOURCES
            ));
        }
        if let Some(resource_id) = self
            .excluded_resource_ids
            .iter()
            .find(|resource_id| !is_project_content_resource_id(resource_id))
        {
            return Err(format!(
                "project-content preferences contain non-content resource '{}'",
                resource_id
            ));
        }
        Ok(())
    }
}

pub fn is_project_content_resource_id(resource_id: &ResourceId) -> bool {
    resource_id.as_str().starts_with("project:content-file:")
        || resource_id.as_str().starts_with("project:content-dir:")
}

fn project_content_identity(
    entry_type: &str,
    logical_path: &LogicalPath,
) -> Result<(String, ResourceId), String> {
    let relative_path = logical_path
        .as_str()
        .strip_prefix("project/")
        .filter(|relative| !relative.is_empty())
        .ok_or_else(|| {
            format!(
                "project-content path '{}' is outside the project namespace",
                logical_path
            )
        })?;
    let digest = Sha256::digest(format!("{}\0{}", entry_type, relative_path).as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{:02x}", byte);
    }
    let prefix = match entry_type {
        "file" => "project:content-file:",
        "dir" => "project:content-dir:",
        _ => {
            return Err(format!(
                "unsupported project-content entry type '{}'",
                entry_type
            ))
        }
    };
    Ok((
        relative_path.to_string(),
        ResourceId::parse(format!("{}{}", prefix, hex))?,
    ))
}

fn validate_project_content_descriptor_identity(
    descriptor: &ResourceDescriptor,
    logical_path: &LogicalPath,
    entry_type: &str,
) -> Result<(), String> {
    let (relative_path, expected_id) = project_content_identity(entry_type, logical_path)?;
    if descriptor.resource_id != expected_id {
        return Err(format!(
            "project-content resource '{}' does not match {} path '{}'",
            descriptor.resource_id, entry_type, logical_path
        ));
    }
    match &descriptor.provenance {
        Provenance::ProjectLocal {
            relative_path: provenance,
        } if provenance == &relative_path => {}
        _ => {
            return Err(format!(
                "project-content resource '{}' has mismatched path provenance",
                descriptor.resource_id
            ));
        }
    }
    if descriptor.scope != ResourceScope::Project
        || descriptor.apply_policy != ApplyPolicy::ExplicitReview
        || descriptor.codec_version != 1
        || descriptor.provider.is_some()
        || descriptor.relative_cwd.is_some()
    {
        return Err(format!(
            "project-content resource '{}' has invalid descriptor policy",
            descriptor.resource_id
        ));
    }
    Ok(())
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SyncConfigV3 {
    pub schema: u32,
    pub revision: u64,
    #[serde(default)]
    pub storages: Vec<StorageConfigV3>,
    #[serde(default)]
    pub projects: Vec<LocalProjectRegistration>,
    #[serde(default)]
    pub links: Vec<ProjectStorageLink>,
}

impl Default for SyncConfigV3 {
    fn default() -> Self {
        Self {
            schema: LOCAL_SCHEMA_V3,
            revision: 0,
            storages: Vec::new(),
            projects: Vec::new(),
            links: Vec::new(),
        }
    }
}

impl SyncConfigV3 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != LOCAL_SCHEMA_V3 {
            return Err(format!(
                "unsupported project-sync schema {} (expected {})",
                self.schema, LOCAL_SCHEMA_V3
            ));
        }
        if self.storages.len() > MAX_STORAGES
            || self.projects.len() > MAX_PROJECTS
            || self.links.len() > MAX_LINKS
        {
            return Err("project-sync config exceeds collection limits".to_string());
        }
        let mut storage_ids = HashSet::new();
        for storage in &self.storages {
            storage.validate()?;
            if !storage_ids.insert(storage.id.clone()) {
                return Err(format!("duplicate storage id '{}'", storage.id));
            }
        }
        let mut local_ids = HashSet::new();
        let mut projects = BTreeMap::new();
        for project in &self.projects {
            project.validate()?;
            if !local_ids.insert(project.local_project_id.clone()) {
                return Err(format!(
                    "duplicate local project id '{}'",
                    project.local_project_id
                ));
            }
            projects.insert(project.local_project_id.clone(), project);
        }
        let mut cells = HashSet::new();
        for link in &self.links {
            let project = projects.get(&link.local_project_id).ok_or_else(|| {
                format!(
                    "link references unknown project '{}'",
                    link.local_project_id
                )
            })?;
            if !storage_ids.contains(&link.storage_id) {
                return Err(format!(
                    "link references unknown storage '{}'",
                    link.storage_id
                ));
            }
            if link.bundle_id != project.bundle_id {
                return Err(format!(
                    "link bundle '{}' differs from project bundle '{}'",
                    link.bundle_id, project.bundle_id
                ));
            }
            if let Some(recipe) = &link.recipe {
                recipe.validate()?;
            }
            link.project_content_preferences.validate()?;
            if !cells.insert((link.local_project_id.clone(), link.storage_id.clone())) {
                return Err(format!(
                    "duplicate project/storage link '{}'/ '{}'",
                    link.local_project_id, link.storage_id
                ));
            }
        }
        for project in &self.projects {
            for storage_id in project.recipe_bases.keys() {
                if !storage_ids.contains(storage_id) {
                    return Err(format!(
                        "project '{}' has a recipe base for unknown storage '{}'",
                        project.local_project_id, storage_id
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn project(&self, id: &LocalProjectId) -> Option<&LocalProjectRegistration> {
        self.projects
            .iter()
            .find(|project| &project.local_project_id == id)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BindingState {
    Active,
    Detached,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ProviderProfile {
    pub profile_id: LocalProviderProfileId,
    pub provider: Provider,
    pub display_name: String,
    /// The exact CODEX_HOME or CLAUDE_CONFIG_DIR spelling selected locally.
    pub path: String,
    /// Canonical path captured when the profile was created.
    pub canonical_path: String,
    pub revision: u64,
    pub created_at: u64,
    pub updated_at: u64,
}

impl ProviderProfile {
    pub fn validate_structure(&self) -> Result<(), String> {
        validate_display_text("provider profile name", &self.display_name, false)?;
        validate_absolute_clean_path("provider profile path", &self.path)?;
        validate_absolute_clean_path("canonical provider profile path", &self.canonical_path)?;
        if self.created_at > self.updated_at {
            return Err(format!(
                "provider profile '{}' has invalid timestamps",
                self.profile_id
            ));
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ProjectBinding {
    pub replica_id: ReplicaId,
    pub local_project_id: LocalProjectId,
    pub bundle_id: BundleId,
    /// User-selected spelling, used for provider cwd behavior.
    pub project_root: String,
    /// Canonical path captured when the binding was validated.
    pub canonical_project_root: String,
    #[serde(default)]
    pub profile_ids: BTreeMap<Provider, LocalProviderProfileId>,
    /// Runtime-only resolved provider homes. Machine state persists profile
    /// IDs; commands populate these fields after revalidating the catalog.
    #[serde(skip)]
    pub codex_home: Option<String>,
    #[serde(skip)]
    pub claude_home: Option<String>,
    pub state: BindingState,
    pub revision: u64,
    pub updated_at: u64,
}

impl ProjectBinding {
    pub fn validate_structure(&self) -> Result<(), String> {
        validate_absolute_clean_path("project root", &self.project_root)?;
        validate_absolute_clean_path("canonical project root", &self.canonical_project_root)?;
        if let Some(path) = &self.codex_home {
            validate_absolute_clean_path("Codex home", path)?;
        }
        if let Some(path) = &self.claude_home {
            validate_absolute_clean_path("Claude home", path)?;
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct MachineProjectState {
    pub schema: u32,
    pub revision: u64,
    #[serde(default)]
    pub profiles: Vec<ProviderProfile>,
    #[serde(default)]
    pub bindings: Vec<ProjectBinding>,
}

impl Default for MachineProjectState {
    fn default() -> Self {
        Self {
            schema: MACHINE_PROJECT_SCHEMA_V1,
            revision: 0,
            profiles: Vec::new(),
            bindings: Vec::new(),
        }
    }
}

impl MachineProjectState {
    pub fn validate(&self, config: &SyncConfigV3) -> Result<(), String> {
        if self.schema != MACHINE_PROJECT_SCHEMA_V1
            || self.bindings.len() > MAX_BINDINGS
            || self.profiles.len() > MAX_PROVIDER_PROFILES
        {
            return Err("invalid machine project state document".to_string());
        }
        let mut profile_ids = HashSet::new();
        let mut profile_paths: Vec<&ProviderProfile> = Vec::new();
        let mut profiles = BTreeMap::new();
        for profile in &self.profiles {
            profile.validate_structure()?;
            if !profile_ids.insert(profile.profile_id.clone()) {
                return Err(format!(
                    "duplicate provider profile id '{}'",
                    profile.profile_id
                ));
            }
            let path = Path::new(&profile.canonical_path);
            if let Some(other) = profile_paths.iter().find(|other| {
                let other_path = Path::new(&other.canonical_path);
                path.starts_with(other_path) || other_path.starts_with(path)
            }) {
                return Err(format!(
                    "provider profiles '{}' and '{}' overlap",
                    other.profile_id, profile.profile_id
                ));
            }
            profile_paths.push(profile);
            profiles.insert(profile.profile_id.clone(), profile);
        }
        let mut replicas = HashSet::new();
        let mut active_projects = HashSet::new();
        let mut active_root_profiles: BTreeMap<(String, LocalProviderProfileId), &LocalProjectId> =
            BTreeMap::new();
        for binding in &self.bindings {
            binding.validate_structure()?;
            if !replicas.insert(binding.replica_id.clone()) {
                return Err(format!("duplicate replica id '{}'", binding.replica_id));
            }
            let project = config.project(&binding.local_project_id);
            if binding.state == BindingState::Active && project.is_none() {
                return Err(format!(
                    "active binding references unknown project '{}'",
                    binding.local_project_id
                ));
            }
            if let Some(project) = project {
                if binding.bundle_id != project.bundle_id {
                    return Err(format!(
                        "binding bundle '{}' differs from project bundle '{}'",
                        binding.bundle_id, project.bundle_id
                    ));
                }
            }
            if binding.state == BindingState::Active {
                if binding.profile_ids.is_empty() {
                    return Err(format!(
                        "project '{}' has no provider profile",
                        binding.local_project_id
                    ));
                }
                for (provider, profile_id) in &binding.profile_ids {
                    let profile = profiles.get(profile_id).ok_or_else(|| {
                        format!(
                            "project '{}' references unknown provider profile '{}'",
                            binding.local_project_id, profile_id
                        )
                    })?;
                    if &profile.provider != provider {
                        return Err(format!(
                            "project '{}' assigns {:?} to a {:?} profile",
                            binding.local_project_id, provider, profile.provider
                        ));
                    }
                }
                if !active_projects.insert(binding.local_project_id.clone()) {
                    return Err(format!(
                        "project '{}' has multiple active bindings",
                        binding.local_project_id
                    ));
                }
                // Project identity is (checkout, provider config): one folder
                // may host several active projects as long as no two of them
                // use the same provider profile.
                let folded = binding.canonical_project_root.to_lowercase();
                for profile_id in binding.profile_ids.values() {
                    if let Some(other) = active_root_profiles.insert(
                        (folded.clone(), profile_id.clone()),
                        &binding.local_project_id,
                    ) {
                        return Err(format!(
                            "projects '{}' and '{}' use the same provider config for one checkout",
                            other, binding.local_project_id
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn active_for(&self, id: &LocalProjectId) -> Option<&ProjectBinding> {
        self.bindings.iter().find(|binding| {
            &binding.local_project_id == id && binding.state == BindingState::Active
        })
    }
}

/// Which local provider profile a setup draft intends to use.  A pending
/// selection stays draft-only; the profile record is created during
/// finalization so an abandoned draft never pollutes the profile catalog.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DraftProfileSelection {
    Existing {
        profile_id: LocalProviderProfileId,
    },
    Pending {
        path: String,
        #[serde(default)]
        display_name: String,
    },
}

/// Which storage a setup draft intends to link.  Pending storage carries the
/// full (possibly still incomplete) configuration, including credentials, so
/// drafts require the same private file permissions as the config itself.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DraftStorageSelection {
    Existing { storage_id: StorageId },
    Pending { storage: StorageConfigV3 },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DraftRepositoryChoice {
    /// Publish a new remote repo under the draft's preallocated bundle ID.
    New,
    /// Connect to an existing remote bundle.  A fingerprint mismatch between
    /// checkout and remote must be explicitly acknowledged before finalize.
    Existing {
        storage_id: StorageId,
        bundle_id: BundleId,
        #[serde(default)]
        display_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repository_fingerprint: Option<String>,
        #[serde(default)]
        mismatch_acknowledged: bool,
    },
}

/// A resumable, machine-local project setup draft.  Drafts hold selections
/// and preallocated identities only — never discovered file contents, remote
/// listings, or resource payloads; those are rescanned on resume.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ProjectSetupDraft {
    pub schema: u32,
    pub draft_id: SetupDraftId,
    /// Preallocated so every finalize retry reconciles to the same records
    /// instead of creating duplicates.
    pub local_project_id: LocalProjectId,
    /// Bundle identity used when `repository` is `New`.
    pub new_bundle_id: BundleId,
    pub project_root: String,
    pub canonical_project_root: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_fingerprint: Option<String>,
    #[serde(default)]
    pub profiles: BTreeMap<Provider, DraftProfileSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<DraftStorageSelection>,
    pub repository: DraftRepositoryChoice,
    /// Resource IDs checked in the setup inventory.
    #[serde(default)]
    pub selected_resource_ids: Vec<ResourceId>,
    /// Digest over the discovered candidate IDs at selection time.  A changed
    /// signature flags the saved selection for re-review, never silent reuse.
    #[serde(default)]
    pub discovery_signature: String,
    pub revision: u64,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl ProjectSetupDraft {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SETUP_DRAFT_SCHEMA_V1 {
            return Err(format!(
                "unsupported setup draft schema {} (expected {})",
                self.schema, SETUP_DRAFT_SCHEMA_V1
            ));
        }
        validate_display_text("draft display name", &self.display_name, false)?;
        validate_absolute_clean_path("draft project root", &self.project_root)?;
        validate_absolute_clean_path("draft canonical project root", &self.canonical_project_root)?;
        if let Some(fingerprint) = &self.repository_fingerprint {
            validate_sha256("draft repository fingerprint", fingerprint)?;
        }
        for selection in self.profiles.values() {
            if let DraftProfileSelection::Pending { path, display_name } = selection {
                validate_absolute_clean_path("draft profile path", path)?;
                validate_display_text("draft profile name", display_name, true)?;
            }
        }
        if let Some(DraftStorageSelection::Pending { storage }) = &self.storage {
            // A draft may hold a half-edited storage; bound the text instead
            // of demanding completeness.  Finalize runs the strict validation.
            validate_display_text("draft storage name", &storage.name, true)?;
            for value in [
                &storage.bucket,
                &storage.account_id,
                &storage.s3_endpoint,
                &storage.region,
                &storage.local_dir,
            ] {
                if value.len() > 4_096 || value.chars().any(char::is_control) {
                    return Err("invalid draft storage field".to_string());
                }
            }
        }
        if let DraftRepositoryChoice::Existing {
            display_name,
            repository_fingerprint,
            ..
        } = &self.repository
        {
            validate_display_text("draft repository name", display_name, true)?;
            if let Some(fingerprint) = repository_fingerprint {
                validate_sha256("draft remote repository fingerprint", fingerprint)?;
            }
        }
        if self.selected_resource_ids.len() > MAX_RESOURCES {
            return Err(format!("draft exceeds {} resources", MAX_RESOURCES));
        }
        if !self.discovery_signature.is_empty() {
            validate_sha256("draft discovery signature", &self.discovery_signature)?;
        }
        if let Some(error) = &self.last_error {
            if error.len() > 4_096 || error.chars().any(char::is_control) {
                return Err("invalid draft error text".to_string());
            }
        }
        if self.created_at > self.updated_at {
            return Err(format!("draft '{}' has invalid timestamps", self.draft_id));
        }
        Ok(())
    }
}

/// The deterministic records one finalize attempt will create.  Written
/// before the first document mutation so an interrupted finalization can be
/// completed (or safely discarded when nothing was applied) on recovery.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SetupTransaction {
    pub schema: u32,
    pub draft_id: SetupDraftId,
    pub draft_revision: u64,
    pub created_at: u64,
    #[serde(default)]
    pub profiles: Vec<ProviderProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<StorageConfigV3>,
    pub project: LocalProjectRegistration,
    #[serde(default)]
    pub links: Vec<ProjectStorageLink>,
    pub binding: ProjectBinding,
}

impl SetupTransaction {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SETUP_TRANSACTION_SCHEMA_V1 {
            return Err(format!(
                "unsupported setup transaction schema {} (expected {})",
                self.schema, SETUP_TRANSACTION_SCHEMA_V1
            ));
        }
        for profile in &self.profiles {
            profile.validate_structure()?;
        }
        if let Some(storage) = &self.storage {
            storage.validate()?;
        }
        self.project.validate()?;
        for link in &self.links {
            if link.local_project_id != self.project.local_project_id
                || link.bundle_id != self.project.bundle_id
            {
                return Err("setup transaction link does not match its project".to_string());
            }
        }
        self.binding.validate_structure()?;
        if self.binding.local_project_id != self.project.local_project_id
            || self.binding.bundle_id != self.project.bundle_id
        {
            return Err("setup transaction binding does not match its project".to_string());
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BundleIdentity {
    pub bundle_id: BundleId,
    pub display_name: String,
    pub kind: BundleKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_fingerprint: Option<String>,
}

impl BundleIdentity {
    pub fn validate(&self) -> Result<(), String> {
        validate_display_text("bundle display name", &self.display_name, false)?;
        if let Some(fingerprint) = &self.repository_fingerprint {
            validate_sha256("repository fingerprint", fingerprint)?;
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct CapturedWith {
    pub app_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_version: Option<String>,
    #[serde(default)]
    pub codec_versions: BTreeMap<String, u32>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BundleFileEntry {
    pub resource_id: ResourceId,
    pub sha256: String,
    pub size: u64,
    pub source_mtime: u64,
    pub object_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BundleDirectoryEntry {
    pub resource_id: ResourceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    pub source_mtime: u64,
}

impl BundleDirectoryEntry {
    pub fn validate(&self) -> Result<(), String> {
        if self.mode.is_some_and(|mode| mode & !0o777 != 0) {
            return Err("bundle directory mode contains unsafe bits".to_string());
        }
        Ok(())
    }
}

impl BundleFileEntry {
    pub fn validate(&self) -> Result<(), String> {
        validate_sha256("bundle file", &self.sha256)?;
        if self.size > MAX_FILE_BYTES {
            return Err(format!("bundle file exceeds {} bytes", MAX_FILE_BYTES));
        }
        if self.object_key.is_empty()
            || self.object_key.len() > 2_048
            || self.object_key.starts_with('/')
            || self.object_key.contains('\\')
            || self
                .object_key
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(format!("unsafe bundle object key '{}'", self.object_key));
        }
        if self.mode.is_some_and(|mode| mode & !0o777 != 0) {
            return Err("bundle file mode contains unsafe bits".to_string());
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TombstoneTarget {
    Resource {
        resource_id: ResourceId,
    },
    File {
        resource_id: ResourceId,
        logical_path: LogicalPath,
    },
    ProjectContentFile {
        resource_id: ResourceId,
        logical_path: LogicalPath,
    },
    ProjectContentDirectory {
        resource_id: ResourceId,
        logical_path: LogicalPath,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Tombstone {
    pub target: TombstoneTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sha256: Option<String>,
    pub deleted_at: u64,
}

impl Tombstone {
    pub fn canonical_key(&self) -> String {
        match &self.target {
            TombstoneTarget::Resource { resource_id } => format!("resource:{}", resource_id),
            TombstoneTarget::File {
                resource_id,
                logical_path,
            } => format!("file:{}:{}", resource_id, logical_path),
            TombstoneTarget::ProjectContentFile {
                resource_id,
                logical_path,
            } => format!("project-content-file:{}:{}", resource_id, logical_path),
            TombstoneTarget::ProjectContentDirectory {
                resource_id,
                logical_path,
            } => format!("project-content-dir:{}:{}", resource_id, logical_path),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if let Some(digest) = &self.last_sha256 {
            validate_sha256("tombstone digest", digest)?;
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BundleHead {
    pub schema_version: u32,
    pub bundle_id: BundleId,
    pub kind: BundleKind,
    pub generation: u64,
    pub commit_id: String,
    pub manifest_key: String,
    pub manifest_sha256: String,
    pub updated_at: u64,
}

impl BundleHead {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != BUNDLE_SCHEMA_V4 {
            return Err(format!(
                "unsupported bundle head schema {}",
                self.schema_version
            ));
        }
        validate_named_id("commit id", &self.commit_id)?;
        validate_sha256("head manifest", &self.manifest_sha256)?;
        if self.manifest_key.is_empty()
            || self.manifest_key.starts_with('/')
            || self.manifest_key.contains("..")
            || self.manifest_key.contains('\\')
        {
            return Err("unsafe manifest key".to_string());
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BundleManifest {
    pub schema_version: u32,
    pub generation: u64,
    pub commit_id: String,
    pub updated_at: u64,
    pub bundle: BundleIdentity,
    pub recipe: BundleRecipe,
    pub captured_with: CapturedWith,
    #[serde(default)]
    pub resources: BTreeMap<ResourceId, ResourceDescriptor>,
    #[serde(default)]
    pub files: BTreeMap<LogicalPath, BundleFileEntry>,
    #[serde(default)]
    pub directories: BTreeMap<LogicalPath, BundleDirectoryEntry>,
    #[serde(default)]
    pub tombstones: BTreeMap<String, Tombstone>,
}

impl BundleManifest {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != BUNDLE_SCHEMA_V4 {
            return Err(format!(
                "unsupported bundle manifest schema {}",
                self.schema_version
            ));
        }
        validate_named_id("commit id", &self.commit_id)?;
        self.bundle.validate()?;
        self.recipe.validate()?;
        if self.resources.len() > MAX_RESOURCES
            || self.files.len() > MAX_FILES
            || self.directories.len() > MAX_FILES
            || self.tombstones.len() > MAX_FILES
        {
            return Err("bundle manifest exceeds collection limits".to_string());
        }
        for (id, descriptor) in &self.resources {
            if id != &descriptor.resource_id {
                return Err(format!("resource key '{}' does not match descriptor", id));
            }
            descriptor.validate()?;
        }
        let recipe_ids = self.recipe.selected_ids();
        let resource_ids: BTreeSet<_> = self.resources.keys().cloned().collect();
        if recipe_ids != resource_ids {
            return Err("manifest recipe and live resources differ".to_string());
        }
        let mut folded_paths = BTreeMap::<String, (&LogicalPath, &'static str)>::new();
        let mut file_owners = BTreeMap::<ResourceId, usize>::new();
        let mut directory_owners = BTreeMap::<ResourceId, usize>::new();
        let mut project_content_bytes = 0_u64;
        for (path, entry) in &self.directories {
            entry.validate()?;
            let descriptor = self.resources.get(&entry.resource_id).ok_or_else(|| {
                format!(
                    "directory '{}' references missing resource '{}'",
                    path, entry.resource_id
                )
            })?;
            if descriptor.kind != ResourceKind::ProjectContentDirectory {
                return Err(format!(
                    "directory '{}' is owned by non-directory resource '{}'",
                    path, entry.resource_id
                ));
            }
            validate_project_content_descriptor_identity(descriptor, path, "dir")?;
            *directory_owners
                .entry(entry.resource_id.clone())
                .or_default() += 1;
            let components = path.as_str().split('/').collect::<Vec<_>>();
            for end in 2..components.len() {
                let ancestor = LogicalPath::parse(components[..end].join("/"))?;
                if !self.directories.contains_key(&ancestor) {
                    return Err(format!(
                        "project-content directory '{}' lacks directory entry '{}'",
                        path, ancestor
                    ));
                }
            }
            let folded = path.as_str().to_lowercase();
            if let Some((previous, previous_kind)) =
                folded_paths.insert(folded, (path, "directory"))
            {
                return Err(format!(
                    "portable path collision between {} '{}' and directory '{}'",
                    previous_kind, previous, path
                ));
            }
        }
        for (path, entry) in &self.files {
            entry.validate()?;
            let descriptor = self.resources.get(&entry.resource_id).ok_or_else(|| {
                format!(
                    "file '{}' references missing resource '{}'",
                    path, entry.resource_id
                )
            })?;
            *file_owners.entry(entry.resource_id.clone()).or_default() += 1;
            let folded = path.as_str().to_lowercase();
            if let Some((previous, previous_kind)) = folded_paths.insert(folded, (path, "file")) {
                return Err(format!(
                    "portable path collision between {} '{}' and file '{}'",
                    previous_kind, previous, path
                ));
            }
            if descriptor.kind == ResourceKind::ProjectContentFile {
                validate_project_content_descriptor_identity(descriptor, path, "file")?;
                if entry.size > MAX_PROJECT_CONTENT_FILE_BYTES {
                    return Err(format!(
                        "project-content file '{}' exceeds {} bytes",
                        path, MAX_PROJECT_CONTENT_FILE_BYTES
                    ));
                }
                project_content_bytes = project_content_bytes
                    .checked_add(entry.size)
                    .ok_or_else(|| "project-content byte total overflow".to_string())?;
                if project_content_bytes > MAX_PROJECT_CONTENT_TOTAL_BYTES {
                    return Err(format!(
                        "project content exceeds {} bytes",
                        MAX_PROJECT_CONTENT_TOTAL_BYTES
                    ));
                }
                let components = path.as_str().split('/').collect::<Vec<_>>();
                for end in 2..components.len() {
                    let ancestor = LogicalPath::parse(components[..end].join("/"))?;
                    if !self.directories.contains_key(&ancestor) {
                        return Err(format!(
                            "project-content file '{}' lacks directory entry '{}'",
                            path, ancestor
                        ));
                    }
                }
            }
        }
        for (resource_id, descriptor) in &self.resources {
            let file_count = file_owners.get(resource_id).copied().unwrap_or_default();
            let directory_count = directory_owners
                .get(resource_id)
                .copied()
                .unwrap_or_default();
            match descriptor.kind {
                ResourceKind::ProjectContentFile if file_count != 1 || directory_count != 0 => {
                    return Err(format!(
                        "project-content file resource '{}' must own exactly one file",
                        resource_id
                    ));
                }
                ResourceKind::ProjectContentDirectory
                    if directory_count != 1 || file_count != 0 =>
                {
                    return Err(format!(
                        "project-content directory resource '{}' must own exactly one directory",
                        resource_id
                    ));
                }
                _ => {}
            }
        }
        for path in self.files.keys() {
            let components: Vec<_> = path.as_str().split('/').collect();
            for end in 1..components.len() {
                let ancestor = components[..end].join("/").to_lowercase();
                if let Some((existing, kind)) = folded_paths.get(&ancestor) {
                    if *kind == "directory" {
                        continue;
                    }
                    return Err(format!(
                        "manifest file '{}' is ancestor of '{}'",
                        existing, path
                    ));
                }
            }
        }
        for (key, tombstone) in &self.tombstones {
            tombstone.validate()?;
            if key != &tombstone.canonical_key() {
                return Err(format!("non-canonical tombstone key '{}'", key));
            }
            match &tombstone.target {
                TombstoneTarget::Resource { resource_id } => {
                    if self.resources.contains_key(resource_id) {
                        return Err(format!(
                            "live resource '{}' also has a tombstone",
                            resource_id
                        ));
                    }
                }
                TombstoneTarget::File {
                    resource_id,
                    logical_path,
                } => {
                    if self.files.contains_key(logical_path) {
                        return Err(format!("live file '{}' also has a tombstone", logical_path));
                    }
                    if self.resources.contains_key(resource_id)
                        && !self.recipe.entries.contains_key(resource_id)
                    {
                        return Err(format!("invalid file tombstone resource '{}'", resource_id));
                    }
                }
                TombstoneTarget::ProjectContentFile {
                    resource_id,
                    logical_path,
                } => {
                    if self.files.contains_key(logical_path) {
                        return Err(format!(
                            "live project-content file '{}' also has a tombstone",
                            logical_path
                        ));
                    }
                    let (_, expected_id) = project_content_identity("file", logical_path)?;
                    if resource_id != &expected_id {
                        return Err(format!(
                            "project-content file tombstone resource '{}' does not match '{}'",
                            resource_id, logical_path
                        ));
                    }
                    if self.resources.contains_key(resource_id) {
                        return Err(format!(
                            "live resource '{}' also has a project-content tombstone",
                            resource_id
                        ));
                    }
                    if tombstone.last_sha256.is_none() {
                        return Err(format!(
                            "project-content file tombstone '{}' lacks its last digest",
                            logical_path
                        ));
                    }
                }
                TombstoneTarget::ProjectContentDirectory {
                    resource_id,
                    logical_path,
                } => {
                    if self.directories.contains_key(logical_path) {
                        return Err(format!(
                            "live project-content directory '{}' also has a tombstone",
                            logical_path
                        ));
                    }
                    let (_, expected_id) = project_content_identity("dir", logical_path)?;
                    if resource_id != &expected_id {
                        return Err(format!(
                            "project-content directory tombstone resource '{}' does not match '{}'",
                            resource_id, logical_path
                        ));
                    }
                    if self.resources.contains_key(resource_id) {
                        return Err(format!(
                            "live resource '{}' also has a project-content tombstone",
                            resource_id
                        ));
                    }
                    if tombstone.last_sha256.is_some() {
                        return Err(format!(
                            "project-content directory tombstone '{}' cannot have a digest",
                            logical_path
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn validate_against_head(&self, head: &BundleHead) -> Result<(), String> {
        self.validate()?;
        head.validate()?;
        if self.bundle.bundle_id != head.bundle_id
            || self.bundle.kind != head.kind
            || self.generation != head.generation
            || self.commit_id != head.commit_id
        {
            return Err("bundle head and manifest identity differ".to_string());
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BundleSnapshot {
    pub storage_id: StorageId,
    pub head: BundleHead,
    pub manifest: BundleManifest,
    pub fetched_at: u64,
}

impl BundleSnapshot {
    pub fn validate(&self) -> Result<(), String> {
        self.manifest.validate_against_head(&self.head)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RestoreActionType {
    WriteFile,
    WriteProjectFile,
    EnsureProjectDirectory,
    DeleteProjectFile,
    DeleteProjectDirectory,
    MergeFile,
    MaterializeConversation,
    InstallStandaloneSkill,
    InstallCustomSkill,
    OverwriteCustomSkill,
    InstallPlugin,
    ReviewHook,
    ReviewMcp,
    ApplySetting,
    Manual,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RestoreActionKind {
    WriteFile {
        logical_path: LogicalPath,
    },
    WriteProjectFile {
        logical_path: LogicalPath,
        mode: u32,
        source_mtime: u64,
    },
    EnsureProjectDirectory {
        logical_path: LogicalPath,
        mode: u32,
        source_mtime: u64,
    },
    DeleteProjectFile {
        logical_path: LogicalPath,
        last_sha256: String,
    },
    DeleteProjectDirectory {
        logical_path: LogicalPath,
    },
    MergeFile {
        logical_path: LogicalPath,
    },
    MaterializeConversation {
        provider: Provider,
        logical_path: LogicalPath,
    },
    InstallStandaloneSkill {
        provider: Provider,
        target_relative_path: String,
    },
    /// Materialize a complete custom-skill directory into the mapped provider
    /// home's `skills/<name>`. The whole directory is one unit: staging,
    /// verification, and rename happen inside a single approved action, never
    /// as independently selectable file writes.
    InstallCustomSkill {
        provider: Provider,
        skill_name: String,
    },
    /// Replace an existing, different custom-skill directory. Approval is
    /// pinned to the expected target tree digest and a recoverable local
    /// backup is taken before the swap.
    OverwriteCustomSkill {
        provider: Provider,
        skill_name: String,
    },
    InstallPlugin {
        provider: Provider,
        plugin_id: String,
    },
    ReviewHook {
        definition_sha256: String,
    },
    ReviewMcp {
        definition_sha256: String,
    },
    ApplySetting {
        provider: Provider,
        semantic_key: String,
    },
    Manual {
        message: String,
    },
}

impl RestoreActionKind {
    pub fn action_type(&self) -> RestoreActionType {
        match self {
            Self::WriteFile { .. } => RestoreActionType::WriteFile,
            Self::WriteProjectFile { .. } => RestoreActionType::WriteProjectFile,
            Self::EnsureProjectDirectory { .. } => RestoreActionType::EnsureProjectDirectory,
            Self::DeleteProjectFile { .. } => RestoreActionType::DeleteProjectFile,
            Self::DeleteProjectDirectory { .. } => RestoreActionType::DeleteProjectDirectory,
            Self::MergeFile { .. } => RestoreActionType::MergeFile,
            Self::MaterializeConversation { .. } => RestoreActionType::MaterializeConversation,
            Self::InstallStandaloneSkill { .. } => RestoreActionType::InstallStandaloneSkill,
            Self::InstallCustomSkill { .. } => RestoreActionType::InstallCustomSkill,
            Self::OverwriteCustomSkill { .. } => RestoreActionType::OverwriteCustomSkill,
            Self::InstallPlugin { .. } => RestoreActionType::InstallPlugin,
            Self::ReviewHook { .. } => RestoreActionType::ReviewHook,
            Self::ReviewMcp { .. } => RestoreActionType::ReviewMcp,
            Self::ApplySetting { .. } => RestoreActionType::ApplySetting,
            Self::Manual { .. } => RestoreActionType::Manual,
        }
    }

    fn validate(&self) -> Result<(), String> {
        match self {
            Self::WriteProjectFile { mode, .. } | Self::EnsureProjectDirectory { mode, .. } => {
                if mode & !0o777 != 0 {
                    Err("project-content mode contains unsafe bits".to_string())
                } else {
                    Ok(())
                }
            }
            Self::DeleteProjectFile { last_sha256, .. } => {
                validate_sha256("project-content deletion", last_sha256)
            }
            Self::ReviewHook { definition_sha256 } | Self::ReviewMcp { definition_sha256 } => {
                validate_sha256("definition", definition_sha256)
            }
            Self::InstallStandaloneSkill {
                target_relative_path,
                ..
            } => {
                if target_relative_path.is_empty()
                    || Path::new(target_relative_path).is_absolute()
                    || Path::new(target_relative_path)
                        .components()
                        .any(|component| {
                            matches!(component, Component::CurDir | Component::ParentDir)
                        })
                {
                    Err("invalid standalone-skill target".to_string())
                } else {
                    Ok(())
                }
            }
            Self::InstallCustomSkill { skill_name, .. }
            | Self::OverwriteCustomSkill { skill_name, .. } => {
                validate_skill_name("custom skill name", skill_name)
            }
            Self::InstallPlugin { plugin_id, .. } => {
                validate_display_text("plugin id", plugin_id, false)
            }
            Self::ApplySetting { semantic_key, .. } => {
                validate_display_text("semantic setting key", semantic_key, false)
            }
            Self::Manual { message } => validate_display_text("manual action", message, false),
            _ => Ok(()),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RestoreAction {
    pub action_id: ActionId,
    pub resource_id: ResourceId,
    pub kind: RestoreActionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_target_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_target_mode: Option<u32>,
    pub requires_explicit_approval: bool,
}

impl RestoreAction {
    pub fn validate(&self) -> Result<(), String> {
        self.kind.validate()?;
        if let Some(path) = &self.target_path {
            validate_absolute_clean_path("restore target", path)?;
        }
        if let Some(digest) = &self.source_sha256 {
            validate_sha256("restore source", digest)?;
        }
        if let Some(digest) = &self.expected_target_sha256 {
            validate_sha256("restore target", digest)?;
        }
        if self
            .expected_target_mode
            .is_some_and(|mode| mode & !0o777 != 0)
        {
            return Err("restore target mode contains unsafe bits".to_string());
        }
        if matches!(
            self.kind.action_type(),
            RestoreActionType::InstallPlugin
                | RestoreActionType::InstallStandaloneSkill
                | RestoreActionType::InstallCustomSkill
                | RestoreActionType::OverwriteCustomSkill
                | RestoreActionType::ReviewHook
                | RestoreActionType::ReviewMcp
                | RestoreActionType::ApplySetting
                | RestoreActionType::WriteProjectFile
                | RestoreActionType::EnsureProjectDirectory
                | RestoreActionType::DeleteProjectFile
                | RestoreActionType::DeleteProjectDirectory
        ) && !self.requires_explicit_approval
        {
            return Err(format!(
                "action '{}' requires explicit approval",
                self.action_id
            ));
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RestorePlan {
    pub schema_version: u32,
    pub plan_id: PlanId,
    pub storage_id: StorageId,
    pub bundle_id: BundleId,
    pub replica_id: ReplicaId,
    pub generation: u64,
    pub commit_id: String,
    pub manifest_sha256: String,
    pub binding_revision: u64,
    pub created_at: u64,
    pub expires_at: u64,
    #[serde(default)]
    pub actions: Vec<RestoreAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_content_eligibility: Option<ProjectFileSyncEligibility>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectFileSyncEligibilityState {
    Eligible,
    GitManaged,
    Unknown,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ProjectFileSyncEligibility {
    pub state: ProjectFileSyncEligibilityState,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detected_root: Option<String>,
}

impl ProjectFileSyncEligibility {
    pub fn validate(&self) -> Result<(), String> {
        validate_display_text("project-file eligibility reason", &self.reason, false)?;
        if let Some(root) = &self.detected_root {
            validate_absolute_clean_path("detected Git root", root)?;
        }
        Ok(())
    }
}

impl RestorePlan {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != RESTORE_PLAN_SCHEMA_V1 {
            return Err(format!(
                "unsupported restore-plan schema {}",
                self.schema_version
            ));
        }
        validate_named_id("commit id", &self.commit_id)?;
        validate_sha256("restore-plan manifest", &self.manifest_sha256)?;
        if self.created_at > self.expires_at || self.actions.len() > MAX_ACTIONS {
            return Err("invalid restore plan lifetime or action count".to_string());
        }
        let mut action_ids = HashSet::new();
        if let Some(eligibility) = &self.project_content_eligibility {
            eligibility.validate()?;
        }
        for action in &self.actions {
            action.validate()?;
            if !action_ids.insert(action.action_id.clone()) {
                return Err(format!("duplicate restore action '{}'", action.action_id));
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DependencyActionKind {
    InstallCodexPlugin,
    InstallClaudePlugin,
    InstallStandaloneSkill,
    CheckBinary,
    CheckEnvironment,
    Manual,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DependencyAction {
    pub action_id: ActionId,
    pub resource_id: ResourceId,
    pub kind: DependencyActionKind,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<Provider>,
    #[serde(default)]
    pub argv: Vec<String>,
    pub requires_explicit_approval: bool,
}

impl DependencyAction {
    pub fn validate(&self) -> Result<(), String> {
        validate_display_text("dependency display name", &self.display_name, false)?;
        if self.argv.len() > 64
            || self.argv.iter().any(|argument| {
                argument.len() > 4_096 || argument.chars().any(|character| character == '\0')
            })
        {
            return Err(format!(
                "dependency action '{}' has invalid arguments",
                self.action_id
            ));
        }
        match self.kind {
            DependencyActionKind::InstallCodexPlugin => {
                if self.provider != Some(Provider::Codex) {
                    return Err("Codex plugin dependency has the wrong provider".to_string());
                }
            }
            DependencyActionKind::InstallClaudePlugin => {
                if self.provider != Some(Provider::Claude) {
                    return Err("Claude plugin dependency has the wrong provider".to_string());
                }
            }
            DependencyActionKind::InstallStandaloneSkill if self.provider.is_none() => {
                return Err("standalone-skill dependency lacks a provider".to_string());
            }
            _ => {}
        }
        if matches!(
            self.kind,
            DependencyActionKind::InstallCodexPlugin
                | DependencyActionKind::InstallClaudePlugin
                | DependencyActionKind::InstallStandaloneSkill
        ) && !self.requires_explicit_approval
        {
            return Err(format!(
                "dependency action '{}' requires explicit approval",
                self.action_id
            ));
        }
        Ok(())
    }
}

/// Immutable, generation-pinned dependency approval surface. It is separate
/// from a restore plan because plugin installation can be retried without
/// rebuilding or reapplying project files.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DependencyPlan {
    pub schema_version: u32,
    pub plan_id: PlanId,
    pub storage_id: StorageId,
    pub bundle_id: BundleId,
    pub replica_id: ReplicaId,
    pub generation: u64,
    pub commit_id: String,
    pub manifest_sha256: String,
    pub binding_revision: u64,
    pub created_at: u64,
    pub expires_at: u64,
    #[serde(default)]
    pub actions: Vec<DependencyAction>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl DependencyPlan {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != DEPENDENCY_PLAN_SCHEMA_V1 {
            return Err(format!(
                "unsupported dependency-plan schema {}",
                self.schema_version
            ));
        }
        validate_named_id("commit id", &self.commit_id)?;
        validate_sha256("dependency-plan manifest", &self.manifest_sha256)?;
        if self.created_at > self.expires_at
            || self.actions.len() > MAX_ACTIONS
            || self.blockers.len() > MAX_ACTIONS
            || self.warnings.len() > MAX_ACTIONS
        {
            return Err("invalid dependency plan lifetime or collection size".to_string());
        }
        if self.blockers.iter().chain(&self.warnings).any(|message| {
            message.len() > 8_192 || message.chars().any(|character| character == '\0')
        }) {
            return Err("dependency plan contains an invalid message".to_string());
        }
        let mut ids = HashSet::new();
        for action in &self.actions {
            action.validate()?;
            if !ids.insert(action.action_id.clone()) {
                return Err(format!(
                    "duplicate dependency action '{}'",
                    action.action_id
                ));
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DependencyApplyReceipt {
    pub action_id: ActionId,
    pub status: ActionStatus,
    pub applied_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl DependencyApplyReceipt {
    pub fn validate(&self) -> Result<(), String> {
        if self.error.as_ref().is_some_and(|error| {
            error.len() > 8_192 || error.chars().any(|character| character == '\0')
        }) {
            return Err("invalid dependency receipt error".to_string());
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DependencyApplicationRecord {
    pub plan_id: PlanId,
    pub local_project_id: LocalProjectId,
    pub storage_id: StorageId,
    pub bundle_id: BundleId,
    pub replica_id: ReplicaId,
    pub generation: u64,
    pub commit_id: String,
    pub manifest_sha256: String,
    pub binding_revision: u64,
    pub applied_at: u64,
    #[serde(default)]
    pub receipts: Vec<DependencyApplyReceipt>,
}

impl DependencyApplicationRecord {
    pub fn validate(&self) -> Result<(), String> {
        validate_named_id("commit id", &self.commit_id)?;
        validate_sha256("dependency application manifest", &self.manifest_sha256)?;
        if self.receipts.len() > MAX_ACTIONS {
            return Err("dependency application has too many receipts".to_string());
        }
        let mut ids = HashSet::new();
        for receipt in &self.receipts {
            receipt.validate()?;
            if !ids.insert(receipt.action_id.clone()) {
                return Err(format!(
                    "duplicate dependency receipt '{}'",
                    receipt.action_id
                ));
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DependencyApplications {
    pub schema: u32,
    pub revision: u64,
    #[serde(default)]
    pub records: Vec<DependencyApplicationRecord>,
}

impl Default for DependencyApplications {
    fn default() -> Self {
        Self {
            schema: LOCAL_SCHEMA_V3,
            revision: 0,
            records: Vec::new(),
        }
    }
}

impl DependencyApplications {
    pub fn validate(&self, config: &SyncConfigV3) -> Result<(), String> {
        if self.schema != LOCAL_SCHEMA_V3 || self.records.len() > 100_000 {
            return Err("invalid dependency applications document".to_string());
        }
        let mut plans = HashSet::new();
        for record in &self.records {
            record.validate()?;
            if !plans.insert(record.plan_id.clone()) {
                return Err(format!(
                    "duplicate dependency application for plan '{}'",
                    record.plan_id
                ));
            }
            if let Some(project) = config.project(&record.local_project_id) {
                if project.bundle_id != record.bundle_id {
                    return Err("dependency application/project bundle mismatch".to_string());
                }
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Applied,
    Skipped,
    Failed,
    Blocked,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ApplyReceipt {
    pub action_id: ActionId,
    pub resource_id: ResourceId,
    pub action_type: RestoreActionType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_path: Option<LogicalPath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_sha256_after: Option<String>,
    pub status: ActionStatus,
    pub applied_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ApplyReceipt {
    pub fn validate(&self) -> Result<(), String> {
        for (label, digest) in [
            ("receipt source", self.source_sha256.as_deref()),
            ("receipt target", self.target_sha256_after.as_deref()),
        ] {
            if let Some(digest) = digest {
                validate_sha256(label, digest)?;
            }
        }
        if let Some(path) = &self.target_path {
            validate_absolute_clean_path("receipt target", path)?;
        }
        if self.error.as_ref().is_some_and(|error| {
            error.len() > 8_192 || error.chars().any(|character| character == '\0')
        }) {
            return Err("invalid receipt error".to_string());
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MaterializationStatus {
    Partial,
    Complete,
    Detached,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct MaterializationRecord {
    pub materialization_id: MaterializationId,
    pub plan_id: PlanId,
    pub replica_id: ReplicaId,
    pub local_project_id: LocalProjectId,
    pub storage_id: StorageId,
    pub bundle_id: BundleId,
    pub generation: u64,
    pub commit_id: String,
    pub manifest_sha256: String,
    pub binding_revision: u64,
    pub status: MaterializationStatus,
    pub applied_at: u64,
    #[serde(default)]
    pub receipts: Vec<ApplyReceipt>,
}

impl MaterializationRecord {
    pub fn validate(&self) -> Result<(), String> {
        validate_named_id("commit id", &self.commit_id)?;
        validate_sha256("materialization manifest", &self.manifest_sha256)?;
        if self.receipts.len() > MAX_ACTIONS {
            return Err("materialization has too many receipts".to_string());
        }
        let mut actions = HashSet::new();
        for receipt in &self.receipts {
            receipt.validate()?;
            if !actions.insert(receipt.action_id.clone()) {
                return Err(format!("duplicate apply receipt '{}'", receipt.action_id));
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Materializations {
    pub schema: u32,
    pub revision: u64,
    #[serde(default)]
    pub records: Vec<MaterializationRecord>,
}

impl Default for Materializations {
    fn default() -> Self {
        Self {
            schema: LOCAL_SCHEMA_V3,
            revision: 0,
            records: Vec::new(),
        }
    }
}

impl Materializations {
    pub fn validate(&self, config: &SyncConfigV3) -> Result<(), String> {
        if self.schema != LOCAL_SCHEMA_V3 || self.records.len() > 100_000 {
            return Err("invalid materializations document".to_string());
        }
        let mut ids = HashSet::new();
        for record in &self.records {
            record.validate()?;
            if !ids.insert(record.materialization_id.clone()) {
                return Err(format!(
                    "duplicate materialization id '{}'",
                    record.materialization_id
                ));
            }
            let project = config.project(&record.local_project_id);
            if record.status != MaterializationStatus::Detached && project.is_none() {
                return Err(format!(
                    "live materialization references unknown project '{}'",
                    record.local_project_id
                ));
            }
            if let Some(project) = project {
                if project.bundle_id != record.bundle_id {
                    return Err("materialization/project bundle mismatch".to_string());
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_recipe_base_defaults_sync_activity_timestamps() {
        let base: RecipeBase = serde_json::from_str(
            r#"{"generation":1,"manifest_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","recipe_revision":2}"#,
        )
        .unwrap();
        assert_eq!(base.last_pull_at, None);
        assert_eq!(base.last_push_at, None);
    }

    fn bundle_id() -> BundleId {
        BundleId::parse("0123456789abcdef0123456789abcdef").unwrap()
    }

    fn resource_id(value: &str) -> ResourceId {
        ResourceId::parse(value).unwrap()
    }

    fn project_content_descriptor(entry_type: &str, relative_path: &str) -> ResourceDescriptor {
        let logical_path = LogicalPath::parse(format!("project/{}", relative_path)).unwrap();
        let (_, resource_id) = project_content_identity(entry_type, &logical_path).unwrap();
        ResourceDescriptor {
            resource_id,
            kind: match entry_type {
                "file" => ResourceKind::ProjectContentFile,
                "dir" => ResourceKind::ProjectContentDirectory,
                _ => panic!("unsupported test entry type"),
            },
            provider: None,
            scope: ResourceScope::Project,
            display_name: relative_path.to_string(),
            provenance: Provenance::ProjectLocal {
                relative_path: relative_path.to_string(),
            },
            apply_policy: ApplyPolicy::ExplicitReview,
            relative_cwd: None,
            codec_version: 1,
            metadata: BTreeMap::new(),
        }
    }

    fn project_content_manifest() -> BundleManifest {
        let mut manifest = BundleManifest {
            schema_version: BUNDLE_SCHEMA_V4,
            generation: 1,
            commit_id: "commit-project-content".to_string(),
            updated_at: 1,
            bundle: BundleIdentity {
                bundle_id: bundle_id(),
                display_name: "Project".to_string(),
                kind: BundleKind::Project,
                repository_fingerprint: None,
            },
            recipe: BundleRecipe::default(),
            captured_with: CapturedWith::default(),
            resources: BTreeMap::new(),
            files: BTreeMap::new(),
            directories: BTreeMap::new(),
            tombstones: BTreeMap::new(),
        };
        for relative_path in ["docs", "docs/specs", "docs/empty"] {
            let descriptor = project_content_descriptor("dir", relative_path);
            let resource_id = descriptor.resource_id.clone();
            manifest.recipe.entries.insert(
                resource_id.clone(),
                RecipeEntry {
                    resource_id: resource_id.clone(),
                    apply_policy: ApplyPolicy::ExplicitReview,
                    required: relative_path != "docs/empty",
                },
            );
            manifest.resources.insert(resource_id.clone(), descriptor);
            manifest.directories.insert(
                LogicalPath::parse(format!("project/{}", relative_path)).unwrap(),
                BundleDirectoryEntry {
                    resource_id,
                    mode: Some(0o755),
                    source_mtime: 1,
                },
            );
        }
        let descriptor = project_content_descriptor("file", "docs/specs/a.md");
        let resource_id = descriptor.resource_id.clone();
        manifest.recipe.entries.insert(
            resource_id.clone(),
            RecipeEntry {
                resource_id: resource_id.clone(),
                apply_policy: ApplyPolicy::ExplicitReview,
                required: false,
            },
        );
        manifest.resources.insert(resource_id.clone(), descriptor);
        manifest.files.insert(
            LogicalPath::parse("project/docs/specs/a.md").unwrap(),
            BundleFileEntry {
                resource_id,
                sha256: "a".repeat(64),
                size: 1,
                source_mtime: 1,
                object_key: "_uploads/upload-1/files/project/docs/specs/a.md".to_string(),
                mode: Some(0o644),
            },
        );
        manifest
    }

    #[test]
    fn ids_fail_closed_during_deserialization() {
        assert!(BundleId::parse("../escape").is_err());
        assert!(BundleId::parse("ABCDEF0123456789ABCDEF0123456789").is_err());
        assert!(ResourceId::parse("skill:release").is_ok());
        assert!(ResourceId::parse("Skill/Release").is_err());
        assert!(serde_json::from_str::<BundleId>("\"../../etc/passwd\"").is_err());
    }

    #[test]
    fn logical_paths_are_portable_and_namespace_bounded() {
        assert!(LogicalPath::parse("project/.agents/skills/release/SKILL.md").is_ok());
        assert!(LogicalPath::parse("state/claude/projects/root/session.jsonl").is_ok());
        for path in [
            "/project/a",
            "project/../secret",
            "project/a\\b",
            "project/CON/file",
            "unknown/file",
            "state",
        ] {
            assert!(LogicalPath::parse(path).is_err(), "{}", path);
        }
    }

    #[test]
    fn config_rejects_split_bundle_identity_and_duplicate_cells() {
        let storage = StorageConfigV3 {
            id: StorageId::parse("personal").unwrap(),
            name: "Personal".to_string(),
            kind: StorageKind::Local,
            bucket: String::new(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            account_id: String::new(),
            s3_endpoint: String::new(),
            region: String::new(),
            local_dir: "/tmp/project-sync-store".to_string(),
            included_default_exclusions: Vec::new(),
            supports_conditional_writes: None,
        };
        let project = LocalProjectRegistration {
            local_project_id: LocalProjectId::parse("project-a").unwrap(),
            bundle_id: bundle_id(),
            display_name: "Project A".to_string(),
            local_alias: None,
            repository_fingerprint: None,
            recipe: BundleRecipe::default(),
            recipe_bases: BTreeMap::new(),
            revision: 0,
            created_at: 1,
            updated_at: 1,
        };
        let mut config = SyncConfigV3 {
            schema: LOCAL_SCHEMA_V3,
            revision: 0,
            storages: vec![storage],
            projects: vec![project.clone()],
            links: vec![ProjectStorageLink {
                local_project_id: project.local_project_id.clone(),
                storage_id: StorageId::parse("personal").unwrap(),
                bundle_id: BundleId::parse("11111111111111111111111111111111").unwrap(),
                recipe: None,
                project_content_preferences: Default::default(),
                pinned: false,
                created_at: 1,
            }],
        };
        assert!(config.validate().unwrap_err().contains("differs"));
        config.links[0].bundle_id = bundle_id();
        config.links.push(config.links[0].clone());
        assert!(config.validate().unwrap_err().contains("duplicate"));
    }

    #[test]
    fn active_bindings_share_a_checkout_only_through_distinct_provider_configs() {
        let project = |id: &str, bundle: &str| LocalProjectRegistration {
            local_project_id: LocalProjectId::parse(id).unwrap(),
            bundle_id: BundleId::parse(bundle).unwrap(),
            display_name: id.to_string(),
            local_alias: None,
            repository_fingerprint: None,
            recipe: BundleRecipe::default(),
            recipe_bases: BTreeMap::new(),
            revision: 0,
            created_at: 1,
            updated_at: 1,
        };
        let config = SyncConfigV3 {
            schema: LOCAL_SCHEMA_V3,
            revision: 0,
            storages: Vec::new(),
            projects: vec![
                project("project-a", "0123456789abcdef0123456789abcdef"),
                project("project-b", "11111111111111111111111111111111"),
            ],
            links: Vec::new(),
        };
        let profile = |id: &str, path: &str| ProviderProfile {
            profile_id: LocalProviderProfileId::parse(id).unwrap(),
            provider: Provider::Codex,
            display_name: id.to_string(),
            path: path.to_string(),
            canonical_path: path.to_string(),
            revision: 0,
            created_at: 1,
            updated_at: 1,
        };
        let binding = |replica: &str, project: &str, bundle: &str, profile: &str| ProjectBinding {
            replica_id: ReplicaId::parse(replica).unwrap(),
            local_project_id: LocalProjectId::parse(project).unwrap(),
            bundle_id: BundleId::parse(bundle).unwrap(),
            project_root: "/home/user/projects/health-game".to_string(),
            canonical_project_root: "/home/user/projects/health-game".to_string(),
            profile_ids: BTreeMap::from([(
                Provider::Codex,
                LocalProviderProfileId::parse(profile).unwrap(),
            )]),
            codex_home: None,
            claude_home: None,
            state: BindingState::Active,
            revision: 0,
            updated_at: 1,
        };
        let mut machine = MachineProjectState {
            schema: MACHINE_PROJECT_SCHEMA_V1,
            revision: 0,
            profiles: vec![
                profile("profile-main", "/home/user/.codex"),
                profile("profile-alt", "/home/user/conf2/.codex"),
            ],
            bindings: vec![
                binding(
                    "replica-a",
                    "project-a",
                    "0123456789abcdef0123456789abcdef",
                    "profile-main",
                ),
                binding(
                    "replica-b",
                    "project-b",
                    "11111111111111111111111111111111",
                    "profile-alt",
                ),
            ],
        };
        machine.validate(&config).unwrap();
        machine.bindings[1].profile_ids = BTreeMap::from([(
            Provider::Codex,
            LocalProviderProfileId::parse("profile-main").unwrap(),
        )]);
        let error = machine.validate(&config).unwrap_err();
        assert!(error.contains("same provider config"), "{}", error);
    }

    #[test]
    fn recipe_key_is_identity_and_digest_is_only_version_metadata() {
        let id = resource_id("skill:release");
        let mut recipe = BundleRecipe::default();
        recipe.entries.insert(
            id.clone(),
            RecipeEntry {
                resource_id: id,
                apply_policy: ApplyPolicy::ExplicitInstall,
                required: false,
            },
        );
        assert!(recipe.validate().is_ok());
        let encoded = serde_json::to_string(&recipe).unwrap();
        assert!(encoded.contains("skill:release"));
        assert!(!encoded.contains("source_digest"));
    }

    #[test]
    fn manifest_requires_recipe_resource_and_file_referential_integrity() {
        let id = resource_id("project:agents");
        let path = LogicalPath::parse("project/AGENTS.md").unwrap();
        let mut recipe = BundleRecipe::default();
        recipe.entries.insert(
            id.clone(),
            RecipeEntry {
                resource_id: id.clone(),
                apply_policy: ApplyPolicy::Merge,
                required: false,
            },
        );
        let mut resources = BTreeMap::new();
        resources.insert(
            id.clone(),
            ResourceDescriptor {
                resource_id: id.clone(),
                kind: ResourceKind::ProjectFile,
                provider: None,
                scope: ResourceScope::Project,
                display_name: "AGENTS.md".to_string(),
                provenance: Provenance::ProjectLocal {
                    relative_path: "AGENTS.md".to_string(),
                },
                apply_policy: ApplyPolicy::Merge,
                relative_cwd: None,
                codec_version: 1,
                metadata: BTreeMap::new(),
            },
        );
        let mut files = BTreeMap::new();
        files.insert(
            path,
            BundleFileEntry {
                resource_id: id,
                sha256: "a".repeat(64),
                size: 1,
                source_mtime: 1,
                object_key: "_uploads/upload-1/files/project/AGENTS.md".to_string(),
                mode: Some(0o644),
            },
        );
        let manifest = BundleManifest {
            schema_version: BUNDLE_SCHEMA_V4,
            generation: 1,
            commit_id: "commit-1".to_string(),
            updated_at: 1,
            bundle: BundleIdentity {
                bundle_id: bundle_id(),
                display_name: "Project".to_string(),
                kind: BundleKind::Project,
                repository_fingerprint: None,
            },
            recipe,
            captured_with: CapturedWith::default(),
            resources,
            files,
            directories: BTreeMap::new(),
            tombstones: BTreeMap::new(),
        };
        assert!(manifest.validate().is_ok());
        let mut broken = manifest.clone();
        broken.resources.clear();
        assert!(broken.validate().is_err());
    }

    #[test]
    fn schema_four_project_content_manifest_validates_exact_tree_identity() {
        let manifest = project_content_manifest();
        manifest.validate().unwrap();

        let mut legacy = manifest.clone();
        legacy.schema_version = 3;
        assert!(legacy.validate().unwrap_err().contains("unsupported"));

        let mut missing_ancestor = manifest.clone();
        let docs = LogicalPath::parse("project/docs").unwrap();
        let docs_resource = missing_ancestor
            .directories
            .remove(&docs)
            .unwrap()
            .resource_id;
        missing_ancestor.resources.remove(&docs_resource);
        missing_ancestor.recipe.entries.remove(&docs_resource);
        assert!(missing_ancestor
            .validate()
            .unwrap_err()
            .contains("lacks directory entry"));

        let mut orphan = manifest.clone();
        orphan
            .files
            .remove(&LogicalPath::parse("project/docs/specs/a.md").unwrap());
        assert!(orphan
            .validate()
            .unwrap_err()
            .contains("must own exactly one file"));

        let mut forged = manifest.clone();
        let file_id = forged.files.values().next().unwrap().resource_id.clone();
        forged.resources.get_mut(&file_id).unwrap().provenance = Provenance::ProjectLocal {
            relative_path: "docs/specs/other.md".to_string(),
        };
        assert!(forged
            .validate()
            .unwrap_err()
            .contains("mismatched path provenance"));
    }

    #[test]
    fn project_content_manifest_rejects_path_collision_and_oversized_file() {
        let mut collision = project_content_manifest();
        let descriptor = project_content_descriptor("file", "docs/empty");
        let resource_id = descriptor.resource_id.clone();
        collision.recipe.entries.insert(
            resource_id.clone(),
            RecipeEntry {
                resource_id: resource_id.clone(),
                apply_policy: ApplyPolicy::ExplicitReview,
                required: false,
            },
        );
        collision.resources.insert(resource_id.clone(), descriptor);
        collision.files.insert(
            LogicalPath::parse("project/docs/empty").unwrap(),
            BundleFileEntry {
                resource_id,
                sha256: "b".repeat(64),
                size: 1,
                source_mtime: 1,
                object_key: "_uploads/upload-1/files/project/docs/empty".to_string(),
                mode: Some(0o644),
            },
        );
        assert!(collision.validate().unwrap_err().contains("path collision"));

        let mut oversized = project_content_manifest();
        oversized.files.values_mut().next().unwrap().size = MAX_PROJECT_CONTENT_FILE_BYTES + 1;
        assert!(oversized.validate().unwrap_err().contains("exceeds"));
    }

    #[test]
    fn project_content_tombstones_are_typed_and_path_bound() {
        let mut manifest = project_content_manifest();
        let path = LogicalPath::parse("project/docs/specs/a.md").unwrap();
        let entry = manifest.files.remove(&path).unwrap();
        manifest.resources.remove(&entry.resource_id);
        manifest.recipe.entries.remove(&entry.resource_id);
        let tombstone = Tombstone {
            target: TombstoneTarget::ProjectContentFile {
                resource_id: entry.resource_id,
                logical_path: path,
            },
            last_sha256: Some(entry.sha256),
            deleted_at: 2,
        };
        manifest
            .tombstones
            .insert(tombstone.canonical_key(), tombstone);
        manifest.validate().unwrap();

        let key = manifest.tombstones.keys().next().unwrap().clone();
        let tombstone = manifest.tombstones.get_mut(&key).unwrap();
        if let TombstoneTarget::ProjectContentFile { resource_id, .. } = &mut tombstone.target {
            *resource_id = project_content_descriptor("file", "docs/specs/other.md").resource_id;
        }
        let changed = manifest.tombstones.remove(&key).unwrap();
        manifest.tombstones.insert(changed.canonical_key(), changed);
        assert!(manifest.validate().unwrap_err().contains("does not match"));
    }

    #[test]
    fn executable_restore_actions_cannot_be_implicitly_approved() {
        let action = RestoreAction {
            action_id: ActionId::parse("action-1").unwrap(),
            resource_id: resource_id("plugin:github"),
            kind: RestoreActionKind::InstallPlugin {
                provider: Provider::Codex,
                plugin_id: "github@managed".to_string(),
            },
            target_path: None,
            source_sha256: None,
            expected_target_sha256: None,
            expected_target_mode: None,
            requires_explicit_approval: false,
        };
        assert!(action.validate().unwrap_err().contains("explicit approval"));
    }
}
