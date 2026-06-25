use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use singleton_core::{
    CapabilityDefaults, CapabilityPermissionDefaults, CleanupPolicy, LOCAL_HOST_ID, Result,
    SingletonError,
};

pub const CONFIG_VERSION: u32 = 1;
pub const DEFAULT_PROFILE: &str = "default";
pub const DEFAULT_BACKEND: &str = "copilot";
pub const DEFAULT_MODE: &str = "interactive";
pub const DEFAULT_STATE_DIR: &str = "~/.singleton";
pub const DEFAULT_DATABASE: &str = "~/.singleton/singleton.db";
pub const DEFAULT_HOST: &str = LOCAL_HOST_ID;
pub const DEFAULT_SSH_CONNECT_COMMAND: &str = "singleton serve --stdio";

const CONFIG_FILE_NAME: &str = "singleton.toml";
const PROJECT_CONFIG_FILE_NAME: &str = ".singleton.toml";

const ENV_CONFIG: &str = "SINGLETON_CONFIG";
const ENV_PROFILE: &str = "SINGLETON_PROFILE";
const ENV_NO_PROJECT_CONFIG: &str = "SINGLETON_NO_PROJECT_CONFIG";
const ENV_BACKEND: &str = "SINGLETON_BACKEND";
const ENV_MODE: &str = "SINGLETON_MODE";
const ENV_STATE_DIR: &str = "SINGLETON_STATE_DIR";
const ENV_DATABASE: &str = "SINGLETON_DATABASE";
const ENV_DEFAULT_HOST: &str = "SINGLETON_DEFAULT_HOST";
const ENV_REPO_WORKSPACE_PROVIDER: &str = "SINGLETON_REPO_WORKSPACE_PROVIDER";
const ENV_CLEANUP_POLICY: &str = "SINGLETON_CLEANUP_POLICY";
const ENV_PERMISSION_DEFAULT: &str = "SINGLETON_PERMISSION_DEFAULT";

#[derive(Debug, Clone, Default)]
pub struct ConfigEnvironment {
    home: Option<PathBuf>,
    xdg_config_home: Option<PathBuf>,
    vars: BTreeMap<String, String>,
}

impl ConfigEnvironment {
    pub fn from_process() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .and_then(trusted_config_root);
        let xdg_config_home = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .and_then(trusted_config_root);
        let mut vars = BTreeMap::new();
        for key in [
            ENV_CONFIG,
            ENV_PROFILE,
            ENV_NO_PROJECT_CONFIG,
            ENV_BACKEND,
            ENV_MODE,
            ENV_STATE_DIR,
            ENV_DATABASE,
            ENV_DEFAULT_HOST,
            ENV_REPO_WORKSPACE_PROVIDER,
            ENV_CLEANUP_POLICY,
            ENV_PERMISSION_DEFAULT,
        ] {
            if let Some(value) = std::env::var_os(key) {
                vars.insert(key.to_string(), value.to_string_lossy().to_string());
            }
        }
        Self {
            home,
            xdg_config_home,
            vars,
        }
    }

    pub fn new(home: Option<PathBuf>, xdg_config_home: Option<PathBuf>) -> Self {
        Self {
            home: home.and_then(trusted_config_root),
            xdg_config_home: xdg_config_home.and_then(trusted_config_root),
            vars: BTreeMap::new(),
        }
    }

    pub fn with_home(home: impl Into<PathBuf>) -> Self {
        Self::new(Some(home.into()), None)
    }

    pub fn set_var(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.vars.insert(key.into(), value.into());
    }

    pub fn home(&self) -> Option<&Path> {
        self.home.as_deref()
    }

    pub fn xdg_config_home(&self) -> Option<&Path> {
        self.xdg_config_home.as_deref()
    }

    fn var(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(String::as_str)
    }
}

fn trusted_config_root(path: PathBuf) -> Option<PathBuf> {
    if path.as_os_str().is_empty() || !path.is_absolute() {
        return None;
    }
    Some(path)
}

#[derive(Debug, Clone)]
pub struct ConfigLoadOptions {
    pub cwd: PathBuf,
    pub env: ConfigEnvironment,
    pub config_path: Option<PathBuf>,
    pub profile: Option<String>,
    pub no_project_config: bool,
    pub cli_overrides: ConfigFieldOverrides,
}

impl ConfigLoadOptions {
    pub fn new(cwd: impl Into<PathBuf>, env: ConfigEnvironment) -> Self {
        Self {
            cwd: cwd.into(),
            env,
            config_path: None,
            profile: None,
            no_project_config: false,
            cli_overrides: ConfigFieldOverrides::default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigFieldOverrides {
    pub backend: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub state_dir: Option<PathBuf>,
    pub database: Option<PathBuf>,
    pub default_host: Option<String>,
    pub repo_workspace_provider: Option<WorkspaceProvider>,
    pub cleanup_policy: Option<CleanupPolicy>,
    pub permissions_default: Option<PermissionDefault>,
}

impl ConfigFieldOverrides {
    pub fn with_backend(mut self, backend: impl Into<String>) -> Self {
        self.backend = Some(backend.into());
        self
    }

    pub fn with_database(mut self, database: impl Into<PathBuf>) -> Self {
        self.database = Some(database.into());
        self
    }

    fn resolved_path_overrides(env: &Self, cli: &Self) -> Self {
        Self {
            state_dir: cli.state_dir.clone().or_else(|| env.state_dir.clone()),
            database: cli.database.clone().or_else(|| env.database.clone()),
            ..Self::default()
        }
    }

    fn without_path_overrides(&self) -> Self {
        let mut overrides = self.clone();
        overrides.state_dir = None;
        overrides.database = None;
        overrides
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceProvider {
    LocalPath,
    GitWorktree,
    BackendDefault,
}

impl WorkspaceProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LocalPath => "local_path",
            Self::GitWorktree => "git_worktree",
            Self::BackendDefault => "backend_default",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDefault {
    Ask,
    Allow,
    Deny,
}

impl PermissionDefault {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectivePermissions {
    pub default: PermissionDefault,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveConfig {
    pub profile: String,
    pub backend: String,
    pub model: Option<String>,
    pub mode: String,
    pub state_dir: PathBuf,
    pub database: PathBuf,
    pub default_host: String,
    pub repo_workspace_provider: WorkspaceProvider,
    pub cleanup_policy: CleanupPolicy,
    pub permissions: EffectivePermissions,
    pub hosts: BTreeMap<String, EffectiveHostConfig>,
    pub repos: BTreeMap<String, EffectiveRepoConfig>,
}

impl EffectiveConfig {
    pub fn capability_defaults(&self) -> CapabilityDefaults {
        CapabilityDefaults {
            backend: self.backend.clone(),
            model: self.model.clone(),
            mode: self.mode.clone(),
            permissions: CapabilityPermissionDefaults {
                default: self.permissions.default.as_str().to_string(),
            },
            default_host: self.default_host.clone(),
            repo_workspace_provider: self.repo_workspace_provider.as_str().to_string(),
            cleanup_policy: self.cleanup_policy.clone(),
        }
    }

    pub fn redacted(&self) -> RedactedEffectiveConfig {
        RedactedEffectiveConfig {
            profile: self.profile.clone(),
            backend: self.backend.clone(),
            model: self.model.clone(),
            mode: self.mode.clone(),
            state_dir: self.state_dir.clone(),
            database: self.database.clone(),
            default_host: self.default_host.clone(),
            repo_workspace_provider: self.repo_workspace_provider.clone(),
            cleanup_policy: self.cleanup_policy.clone(),
            permissions_default: self.permissions.default.clone(),
            hosts: self
                .hosts
                .iter()
                .map(|(id, host)| (id.clone(), host.redacted()))
                .collect(),
        }
    }

    pub fn repo_source_workspace_provider(&self, source: &Path) -> WorkspaceProvider {
        if matches!(self.repo_workspace_provider, WorkspaceProvider::GitWorktree)
            && looks_like_git_repo(source)
        {
            WorkspaceProvider::GitWorktree
        } else if source.is_dir() {
            WorkspaceProvider::LocalPath
        } else {
            self.repo_workspace_provider.clone()
        }
    }

    fn apply_overrides(
        &mut self,
        overrides: &ConfigFieldOverrides,
        env: &ConfigEnvironment,
    ) -> Result<()> {
        if let Some(backend) = &overrides.backend {
            validate_non_empty("backend", backend)?;
            self.backend = backend.clone();
        }
        if let Some(model) = &overrides.model {
            validate_non_empty("model", model)?;
            self.model = Some(model.clone());
        }
        if let Some(mode) = &overrides.mode {
            validate_mode(mode)?;
            self.mode = mode.clone();
        }
        if let Some(state_dir) = &overrides.state_dir {
            self.state_dir = expand_tilde(state_dir, env)?;
        }
        if let Some(database) = &overrides.database {
            self.database = expand_tilde(database, env)?;
        }
        if let Some(default_host) = &overrides.default_host {
            validate_non_empty("default_host", default_host)?;
            self.default_host = default_host.clone();
        }
        if let Some(provider) = &overrides.repo_workspace_provider {
            self.repo_workspace_provider = provider.clone();
        }
        if let Some(cleanup_policy) = &overrides.cleanup_policy {
            self.cleanup_policy = cleanup_policy.clone();
        }
        if let Some(permission_default) = &overrides.permissions_default {
            self.permissions.default = permission_default.clone();
        }
        self.validate_references()
    }

    fn validate_references(&self) -> Result<()> {
        if !self.hosts.contains_key(&self.default_host) {
            return Err(SingletonError::InvalidInput(format!(
                "profile '{}' references unknown default_host '{}'",
                self.profile, self.default_host
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedEffectiveConfig {
    pub profile: String,
    pub backend: String,
    pub model: Option<String>,
    pub mode: String,
    pub state_dir: PathBuf,
    pub database: PathBuf,
    pub default_host: String,
    pub repo_workspace_provider: WorkspaceProvider,
    pub cleanup_policy: CleanupPolicy,
    pub permissions_default: PermissionDefault,
    pub hosts: BTreeMap<String, RedactedHostConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectiveHostConfig {
    Local {
        host_id: String,
    },
    Ssh {
        host_id: String,
        target: String,
        connect_command: String,
        ssh_args: Vec<String>,
    },
}

impl EffectiveHostConfig {
    pub fn host_id(&self) -> &str {
        match self {
            Self::Local { host_id } | Self::Ssh { host_id, .. } => host_id,
        }
    }

    pub fn redacted(&self) -> RedactedHostConfig {
        match self {
            Self::Local { host_id } => RedactedHostConfig::Local {
                host_id: host_id.clone(),
            },
            Self::Ssh {
                host_id,
                target,
                connect_command: _,
                ssh_args,
            } => RedactedHostConfig::Ssh {
                host_id: host_id.clone(),
                target: target.clone(),
                connect_command: "<redacted>".to_string(),
                ssh_args: ssh_args.iter().map(|_| "<redacted>".to_string()).collect(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedactedHostConfig {
    Local {
        host_id: String,
    },
    Ssh {
        host_id: String,
        target: String,
        connect_command: String,
        ssh_args: Vec<String>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffectiveRepoConfig {
    pub path: Option<PathBuf>,
    pub url: Option<String>,
    pub default_host: Option<String>,
    pub repo_workspace_provider: Option<WorkspaceProvider>,
    pub cleanup_policy: Option<CleanupPolicy>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConfigDocument {
    version: u32,
    #[serde(default)]
    default_profile: Option<String>,
    #[serde(default)]
    profiles: BTreeMap<String, ProfilePatch>,
    #[serde(default)]
    hosts: BTreeMap<String, HostPatch>,
    #[serde(default)]
    repos: BTreeMap<String, RepoPatch>,
    #[serde(flatten)]
    extra: BTreeMap<String, toml::Value>,
}

impl ConfigDocument {
    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn default_profile(&self) -> Option<&str> {
        self.default_profile.as_deref()
    }

    pub fn profiles(&self) -> &BTreeMap<String, ProfilePatch> {
        &self.profiles
    }

    fn validate(&self, source: ConfigSource) -> Result<()> {
        if self.version != CONFIG_VERSION {
            return Err(SingletonError::InvalidInput(format!(
                "unsupported singleton config version {}; expected {CONFIG_VERSION}",
                self.version
            )));
        }
        reject_extra("top-level config", &self.extra)?;
        if let Some(profile) = &self.default_profile {
            validate_name("default_profile", profile)?;
        }
        for (name, profile) in &self.profiles {
            validate_name("profile", name)?;
            profile.validate(name)?;
        }
        for (name, host) in &self.hosts {
            validate_name("host", name)?;
            host.validate(name, source)?;
        }
        for (name, repo) in &self.repos {
            validate_name("repo", name)?;
            repo.validate(name)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProfilePatch {
    backend: Option<String>,
    model: Option<String>,
    mode: Option<String>,
    state_dir: Option<String>,
    database: Option<String>,
    default_host: Option<String>,
    repo_workspace_provider: Option<WorkspaceProvider>,
    cleanup_policy: Option<CleanupPolicy>,
    permissions: Option<PermissionsPatch>,
    #[serde(flatten)]
    extra: BTreeMap<String, toml::Value>,
}

impl ProfilePatch {
    fn builtin() -> Self {
        Self {
            backend: Some(DEFAULT_BACKEND.to_string()),
            model: None,
            mode: Some(DEFAULT_MODE.to_string()),
            state_dir: Some(DEFAULT_STATE_DIR.to_string()),
            database: Some(DEFAULT_DATABASE.to_string()),
            default_host: Some(DEFAULT_HOST.to_string()),
            repo_workspace_provider: Some(WorkspaceProvider::GitWorktree),
            cleanup_policy: Some(CleanupPolicy::Keep),
            permissions: Some(PermissionsPatch {
                default: Some(PermissionDefault::Ask),
                extra: BTreeMap::new(),
            }),
            extra: BTreeMap::new(),
        }
    }

    fn merge_from(&mut self, other: Self) {
        if let Some(value) = other.backend {
            self.backend = Some(value);
        }
        if let Some(value) = other.model {
            self.model = Some(value);
        }
        if let Some(value) = other.mode {
            self.mode = Some(value);
        }
        if let Some(value) = other.state_dir {
            self.state_dir = Some(value);
        }
        if let Some(value) = other.database {
            self.database = Some(value);
        }
        if let Some(value) = other.default_host {
            self.default_host = Some(value);
        }
        if let Some(value) = other.repo_workspace_provider {
            self.repo_workspace_provider = Some(value);
        }
        if let Some(value) = other.cleanup_policy {
            self.cleanup_policy = Some(value);
        }
        if let Some(value) = other.permissions {
            match &mut self.permissions {
                Some(current) => current.merge_from(value),
                None => self.permissions = Some(value),
            }
        }
    }

    fn validate(&self, name: &str) -> Result<()> {
        reject_extra(&format!("profile '{name}'"), &self.extra)?;
        if let Some(backend) = &self.backend {
            validate_non_empty("backend", backend)?;
        }
        if let Some(model) = &self.model {
            validate_non_empty("model", model)?;
        }
        if let Some(mode) = &self.mode {
            validate_mode(mode)?;
        }
        if let Some(state_dir) = &self.state_dir {
            validate_non_empty("state_dir", state_dir)?;
        }
        if let Some(database) = &self.database {
            validate_non_empty("database", database)?;
        }
        if let Some(default_host) = &self.default_host {
            validate_name("default_host", default_host)?;
        }
        if let Some(permissions) = &self.permissions {
            permissions.validate(name)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PermissionsPatch {
    default: Option<PermissionDefault>,
    #[serde(flatten)]
    extra: BTreeMap<String, toml::Value>,
}

impl PermissionsPatch {
    fn merge_from(&mut self, other: Self) {
        if let Some(value) = other.default {
            self.default = Some(value);
        }
    }

    fn validate(&self, profile: &str) -> Result<()> {
        reject_extra(&format!("profile '{profile}' permissions"), &self.extra)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ConfigHostKind {
    Local,
    Ssh,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct HostPatch {
    kind: Option<ConfigHostKind>,
    target: Option<String>,
    connect_command: Option<String>,
    ssh_args: Option<Vec<String>>,
    #[serde(flatten)]
    extra: BTreeMap<String, toml::Value>,
}

impl HostPatch {
    fn local() -> Self {
        Self {
            kind: Some(ConfigHostKind::Local),
            target: None,
            connect_command: None,
            ssh_args: None,
            extra: BTreeMap::new(),
        }
    }

    fn merge_from(&mut self, other: Self) {
        if let Some(value) = other.kind {
            self.kind = Some(value);
        }
        if let Some(value) = other.target {
            self.target = Some(value);
        }
        if let Some(value) = other.connect_command {
            self.connect_command = Some(value);
        }
        if let Some(value) = other.ssh_args {
            self.ssh_args = Some(value);
        }
    }

    fn validate(&self, name: &str, source: ConfigSource) -> Result<()> {
        reject_secret_extra_fields(&format!("host '{name}'"), &self.extra)?;
        reject_extra(&format!("host '{name}'"), &self.extra)?;
        if source == ConfigSource::Project
            && let Some(connect_command) = &self.connect_command
            && connect_command != DEFAULT_SSH_CONNECT_COMMAND
        {
            return Err(SingletonError::InvalidInput(format!(
                "project config may not set non-default connect_command for SSH host '{name}'"
            )));
        }
        if source == ConfigSource::Project && self.ssh_args.is_some() {
            return Err(SingletonError::InvalidInput(format!(
                "project config may not set ssh_args for SSH host '{name}'"
            )));
        }
        if let Some(target) = &self.target {
            validate_non_empty("target", target)?;
            validate_no_raw_secret_value(&format!("host '{name}' target"), target)?;
        }
        if let Some(connect_command) = &self.connect_command {
            validate_non_empty("connect_command", connect_command)?;
            validate_no_raw_secret_value(
                &format!("host '{name}' connect_command"),
                connect_command,
            )?;
        }
        if let Some(ssh_args) = &self.ssh_args {
            for arg in ssh_args {
                validate_no_raw_secret_value(&format!("host '{name}' ssh_args"), arg)?;
            }
        }
        Ok(())
    }

    fn validate_project_effective(&self, name: &str) -> Result<()> {
        if let Some(connect_command) = &self.connect_command
            && connect_command != DEFAULT_SSH_CONNECT_COMMAND
        {
            return Err(SingletonError::InvalidInput(format!(
                "project config may not inherit or set non-default connect_command for SSH host '{name}'"
            )));
        }
        if self
            .ssh_args
            .as_ref()
            .is_some_and(|ssh_args| !ssh_args.is_empty())
        {
            return Err(SingletonError::InvalidInput(format!(
                "project config may not inherit or set ssh_args for SSH host '{name}'"
            )));
        }
        Ok(())
    }

    fn resolve(&self, host_id: &str) -> Result<EffectiveHostConfig> {
        match self.kind.as_ref() {
            Some(ConfigHostKind::Local) => {
                if self.target.is_some()
                    || self.connect_command.is_some()
                    || self.ssh_args.is_some()
                {
                    return Err(SingletonError::InvalidInput(format!(
                        "local host '{host_id}' may not include SSH fields"
                    )));
                }
                Ok(EffectiveHostConfig::Local {
                    host_id: host_id.to_string(),
                })
            }
            Some(ConfigHostKind::Ssh) => {
                let target = self.target.clone().ok_or_else(|| {
                    SingletonError::InvalidInput(format!("SSH host '{host_id}' requires a target"))
                })?;
                Ok(EffectiveHostConfig::Ssh {
                    host_id: host_id.to_string(),
                    target,
                    connect_command: self
                        .connect_command
                        .clone()
                        .unwrap_or_else(|| DEFAULT_SSH_CONNECT_COMMAND.to_string()),
                    ssh_args: self.ssh_args.clone().unwrap_or_default(),
                })
            }
            None => Err(SingletonError::InvalidInput(format!(
                "host '{host_id}' requires kind"
            ))),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RepoPatch {
    path: Option<String>,
    url: Option<String>,
    default_host: Option<String>,
    repo_workspace_provider: Option<WorkspaceProvider>,
    cleanup_policy: Option<CleanupPolicy>,
    #[serde(flatten)]
    extra: BTreeMap<String, toml::Value>,
}

impl RepoPatch {
    fn merge_from(&mut self, other: Self) {
        if let Some(value) = other.path {
            self.path = Some(value);
        }
        if let Some(value) = other.url {
            self.url = Some(value);
        }
        if let Some(value) = other.default_host {
            self.default_host = Some(value);
        }
        if let Some(value) = other.repo_workspace_provider {
            self.repo_workspace_provider = Some(value);
        }
        if let Some(value) = other.cleanup_policy {
            self.cleanup_policy = Some(value);
        }
    }

    fn validate(&self, name: &str) -> Result<()> {
        reject_extra(&format!("repo '{name}'"), &self.extra)?;
        if self.path.is_none() && self.url.is_none() {
            return Err(SingletonError::InvalidInput(format!(
                "repo '{name}' requires path or url"
            )));
        }
        if let Some(default_host) = &self.default_host {
            validate_name("repo default_host", default_host)?;
        }
        Ok(())
    }

    fn resolve(&self, env: &ConfigEnvironment) -> Result<EffectiveRepoConfig> {
        Ok(EffectiveRepoConfig {
            path: self
                .path
                .as_ref()
                .map(|path| expand_tilde(Path::new(path), env))
                .transpose()?,
            url: self.url.clone(),
            default_host: self.default_host.clone(),
            repo_workspace_provider: self.repo_workspace_provider.clone(),
            cleanup_policy: self.cleanup_policy.clone(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigSource {
    User,
    Project,
}

#[derive(Debug, Clone)]
struct MergedConfig {
    default_profile: String,
    profiles: BTreeMap<String, ProfilePatch>,
    hosts: BTreeMap<String, HostPatch>,
    repos: BTreeMap<String, RepoPatch>,
}

impl MergedConfig {
    fn synthetic_default() -> Self {
        let mut profiles = BTreeMap::new();
        profiles.insert(DEFAULT_PROFILE.to_string(), ProfilePatch::builtin());
        let mut hosts = BTreeMap::new();
        hosts.insert(DEFAULT_HOST.to_string(), HostPatch::local());
        Self {
            default_profile: DEFAULT_PROFILE.to_string(),
            profiles,
            hosts,
            repos: BTreeMap::new(),
        }
    }

    fn merge_document(&mut self, document: ConfigDocument, source: ConfigSource) -> Result<()> {
        document.validate(source)?;
        if let Some(default_profile) = document.default_profile {
            self.default_profile = default_profile;
        }
        for (name, patch) in document.profiles {
            self.profiles.entry(name).or_default().merge_from(patch);
        }
        for (name, patch) in document.hosts {
            let entry = self.hosts.entry(name.clone()).or_default();
            entry.merge_from(patch);
            if source == ConfigSource::Project {
                entry.validate_project_effective(&name)?;
            }
        }
        for (name, patch) in document.repos {
            self.repos.entry(name).or_default().merge_from(patch);
        }
        Ok(())
    }

    fn into_effective(
        self,
        selected_profile: &str,
        env: &ConfigEnvironment,
        path_overrides: &ConfigFieldOverrides,
    ) -> Result<EffectiveConfig> {
        if !self.profiles.contains_key(&self.default_profile) {
            return Err(SingletonError::InvalidInput(format!(
                "default_profile '{}' does not name a configured profile",
                self.default_profile
            )));
        }
        let selected_patch = self.profiles.get(selected_profile).ok_or_else(|| {
            SingletonError::InvalidInput(format!("profile '{selected_profile}' is not configured"))
        })?;
        let mut profile = ProfilePatch::builtin();
        profile.merge_from(selected_patch.clone());

        let hosts = self
            .hosts
            .iter()
            .map(|(id, host)| Ok((id.clone(), host.resolve(id)?)))
            .collect::<Result<BTreeMap<_, _>>>()?;

        let repos = self
            .repos
            .iter()
            .map(|(id, repo)| Ok((id.clone(), repo.resolve(env)?)))
            .collect::<Result<BTreeMap<_, _>>>()?;

        for (name, repo) in &repos {
            if let Some(host) = &repo.default_host
                && !hosts.contains_key(host)
            {
                return Err(SingletonError::InvalidInput(format!(
                    "repo '{name}' references unknown default_host '{host}'"
                )));
            }
        }

        let permissions = profile.permissions.unwrap_or_else(|| PermissionsPatch {
            default: Some(PermissionDefault::Ask),
            extra: BTreeMap::new(),
        });
        let database = resolve_path_field(
            "database",
            profile.database,
            path_overrides.database.as_ref(),
            env,
        )?;
        let state_dir = resolve_state_dir_field(
            profile.state_dir,
            path_overrides.state_dir.as_ref(),
            &database,
            env,
        )?;

        let effective = EffectiveConfig {
            profile: selected_profile.to_string(),
            backend: required_profile_string("backend", profile.backend)?,
            model: profile.model,
            mode: required_profile_string("mode", profile.mode)?,
            state_dir,
            database,
            default_host: required_profile_string("default_host", profile.default_host)?,
            repo_workspace_provider: profile
                .repo_workspace_provider
                .unwrap_or(WorkspaceProvider::GitWorktree),
            cleanup_policy: profile.cleanup_policy.unwrap_or(CleanupPolicy::Keep),
            permissions: EffectivePermissions {
                default: permissions.default.unwrap_or(PermissionDefault::Ask),
            },
            hosts,
            repos,
        };
        effective.validate_references()?;
        Ok(effective)
    }
}

#[derive(Debug, Clone, Default)]
struct EnvOverrides {
    config_path: Option<PathBuf>,
    profile: Option<String>,
    no_project_config: bool,
    fields: ConfigFieldOverrides,
}

impl EnvOverrides {
    fn from_environment(env: &ConfigEnvironment) -> Result<Self> {
        let mut overrides = Self::default();
        if let Some(value) = env.var(ENV_CONFIG) {
            overrides.config_path = Some(PathBuf::from(value));
        }
        if let Some(value) = env.var(ENV_PROFILE) {
            validate_name(ENV_PROFILE, value)?;
            overrides.profile = Some(value.to_string());
        }
        if let Some(value) = env.var(ENV_NO_PROJECT_CONFIG) {
            overrides.no_project_config = parse_bool_env(ENV_NO_PROJECT_CONFIG, value)?;
        }
        if let Some(value) = env.var(ENV_BACKEND) {
            validate_non_empty(ENV_BACKEND, value)?;
            overrides.fields.backend = Some(value.to_string());
        }
        if let Some(value) = env.var(ENV_MODE) {
            validate_mode(value)?;
            overrides.fields.mode = Some(value.to_string());
        }
        if let Some(value) = env.var(ENV_STATE_DIR) {
            overrides.fields.state_dir = Some(PathBuf::from(value));
        }
        if let Some(value) = env.var(ENV_DATABASE) {
            overrides.fields.database = Some(PathBuf::from(value));
        }
        if let Some(value) = env.var(ENV_DEFAULT_HOST) {
            validate_name(ENV_DEFAULT_HOST, value)?;
            overrides.fields.default_host = Some(value.to_string());
        }
        if let Some(value) = env.var(ENV_REPO_WORKSPACE_PROVIDER) {
            overrides.fields.repo_workspace_provider = Some(parse_workspace_provider(value)?);
        }
        if let Some(value) = env.var(ENV_CLEANUP_POLICY) {
            overrides.fields.cleanup_policy = Some(parse_cleanup_policy(value)?);
        }
        if let Some(value) = env.var(ENV_PERMISSION_DEFAULT) {
            overrides.fields.permissions_default = Some(parse_permission_default(value)?);
        }
        Ok(overrides)
    }
}

pub fn load_effective_config(options: ConfigLoadOptions) -> Result<EffectiveConfig> {
    let env_overrides = EnvOverrides::from_environment(&options.env)?;
    let mut merged = MergedConfig::synthetic_default();

    match options
        .config_path
        .as_ref()
        .or(env_overrides.config_path.as_ref())
    {
        Some(path) => {
            let path = expand_tilde(path, &options.env)?;
            merged.merge_document(read_config_file(&path)?, ConfigSource::User)?;
        }
        None => match user_config_path(&options.env) {
            Ok(path) => {
                if path.exists() {
                    merged.merge_document(read_config_file(&path)?, ConfigSource::User)?;
                }
            }
            Err(_error) if can_skip_home_dependent_user_config(&options, &env_overrides) => {}
            Err(error) => return Err(error),
        },
    }

    if !options.no_project_config
        && !env_overrides.no_project_config
        && let Some(path) = find_project_config(&options.cwd)
    {
        merged.merge_document(read_config_file(&path)?, ConfigSource::Project)?;
    }

    let selected_profile = options
        .profile
        .as_deref()
        .or(env_overrides.profile.as_deref())
        .unwrap_or(&merged.default_profile)
        .to_string();
    validate_name("profile", &selected_profile)?;

    let path_overrides = ConfigFieldOverrides::resolved_path_overrides(
        &env_overrides.fields,
        &options.cli_overrides,
    );
    let mut effective = merged.into_effective(&selected_profile, &options.env, &path_overrides)?;
    effective.apply_overrides(&env_overrides.fields.without_path_overrides(), &options.env)?;
    effective.apply_overrides(
        &options.cli_overrides.without_path_overrides(),
        &options.env,
    )?;
    Ok(effective)
}

pub fn parse_config_toml(text: &str) -> Result<ConfigDocument> {
    let document = toml::from_str::<ConfigDocument>(text).map_err(|error| {
        SingletonError::InvalidInput(format!("parse singleton config: {error}"))
    })?;
    document.validate(ConfigSource::User)?;
    Ok(document)
}

pub fn user_config_path(env: &ConfigEnvironment) -> Result<PathBuf> {
    if let Some(config_home) = env.xdg_config_home() {
        return Ok(config_home.join("singleton").join(CONFIG_FILE_NAME));
    }
    let home = env.home().ok_or_else(|| {
        SingletonError::InvalidInput(
            "HOME is not set; pass --config or --database explicitly".to_string(),
        )
    })?;
    Ok(home
        .join(".config")
        .join("singleton")
        .join(CONFIG_FILE_NAME))
}

pub fn find_project_config(cwd: &Path) -> Option<PathBuf> {
    for ancestor in cwd.ancestors() {
        let candidate = ancestor.join(PROJECT_CONFIG_FILE_NAME);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

pub fn expand_tilde(path: &Path, env: &ConfigEnvironment) -> Result<PathBuf> {
    let Some(text) = path.to_str() else {
        return Ok(path.to_path_buf());
    };
    if text == "~" {
        return env
            .home()
            .map(Path::to_path_buf)
            .ok_or_else(|| SingletonError::InvalidInput("HOME is not set".to_string()));
    }
    if let Some(rest) = text.strip_prefix("~/") {
        let home = env
            .home()
            .ok_or_else(|| SingletonError::InvalidInput("HOME is not set".to_string()))?;
        return Ok(home.join(rest));
    }
    Ok(path.to_path_buf())
}

pub fn default_database_path(env: &ConfigEnvironment) -> Result<PathBuf> {
    expand_tilde(Path::new(DEFAULT_DATABASE), env)
}

fn can_skip_home_dependent_user_config(
    options: &ConfigLoadOptions,
    env_overrides: &EnvOverrides,
) -> bool {
    options.env.xdg_config_home().is_none()
        && options.env.home().is_none()
        && (options.cli_overrides.database.is_some()
            || options.cli_overrides.state_dir.is_some()
            || env_overrides.fields.database.is_some()
            || env_overrides.fields.state_dir.is_some())
}

fn resolve_path_field(
    field: &str,
    profile_value: Option<String>,
    override_value: Option<&PathBuf>,
    env: &ConfigEnvironment,
) -> Result<PathBuf> {
    if let Some(path) = override_value {
        return expand_tilde(path, env);
    }
    expand_tilde(
        Path::new(&required_profile_string(field, profile_value)?),
        env,
    )
}

fn resolve_state_dir_field(
    profile_value: Option<String>,
    override_value: Option<&PathBuf>,
    database: &Path,
    env: &ConfigEnvironment,
) -> Result<PathBuf> {
    if let Some(path) = override_value {
        return expand_tilde(path, env);
    }
    let raw = required_profile_string("state_dir", profile_value)?;
    match expand_tilde(Path::new(&raw), env) {
        Ok(path) => Ok(path),
        Err(_error) if raw == DEFAULT_STATE_DIR && env.home().is_none() => Ok(database
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()),
        Err(error) => Err(error),
    }
}

fn read_config_file(path: &Path) -> Result<ConfigDocument> {
    let text = fs::read_to_string(path).map_err(|error| {
        SingletonError::InvalidInput(format!("read config {}: {error}", path.display()))
    })?;
    parse_config_toml(&text)
        .map_err(|error| SingletonError::InvalidInput(format!("{}: {error}", path.display())))
}

fn required_profile_string(field: &str, value: Option<String>) -> Result<String> {
    let value = value.ok_or_else(|| {
        SingletonError::InvalidInput(format!("profile is missing required field '{field}'"))
    })?;
    validate_non_empty(field, &value)?;
    if field == "mode" {
        validate_mode(&value)?;
    }
    Ok(value)
}

fn validate_name(kind: &str, value: &str) -> Result<()> {
    validate_non_empty(kind, value)?;
    if value.chars().any(char::is_whitespace) {
        return Err(SingletonError::InvalidInput(format!(
            "{kind} may not contain whitespace: '{value}'"
        )));
    }
    Ok(())
}

fn validate_non_empty(kind: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(SingletonError::InvalidInput(format!(
            "{kind} may not be empty"
        )));
    }
    Ok(())
}

fn validate_mode(value: &str) -> Result<()> {
    match value {
        "interactive" | "plan" | "autopilot" => Ok(()),
        other => Err(SingletonError::InvalidInput(format!(
            "unsupported mode '{other}'"
        ))),
    }
}

fn reject_extra(context: &str, extra: &BTreeMap<String, toml::Value>) -> Result<()> {
    if let Some(key) = extra.keys().next() {
        return Err(SingletonError::InvalidInput(format!(
            "unknown {context} key '{key}'"
        )));
    }
    Ok(())
}

fn reject_secret_extra_fields(context: &str, extra: &BTreeMap<String, toml::Value>) -> Result<()> {
    if let Some(key) = extra.keys().find(|key| looks_like_secret_field_name(key)) {
        return Err(SingletonError::InvalidInput(format!(
            "{context} contains raw-secret-looking field '{key}'; store secret material outside singleton and reference it through SSH or provider configuration"
        )));
    }
    Ok(())
}

fn looks_like_secret_field_name(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("password")
        || key.contains("passwd")
        || key.contains("token")
        || key.contains("secret")
        || key.contains("private_key")
        || key.contains("key_material")
}

fn validate_no_raw_secret_value(context: &str, value: &str) -> Result<()> {
    if looks_like_raw_secret_value(value) {
        return Err(SingletonError::InvalidInput(format!(
            "{context} looks like raw secret material; store secrets outside singleton and reference them through SSH or provider configuration"
        )));
    }
    Ok(())
}

fn looks_like_raw_secret_value(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    (lower.contains("-----begin ") && lower.contains("private key"))
        || lower.contains("password=")
        || lower.contains("passwd=")
        || lower.contains("token=")
        || lower.contains("secret=")
        || value.starts_with("ghp_")
        || value.starts_with("github_pat_")
}

fn parse_bool_env(name: &str, value: &str) -> Result<bool> {
    match value {
        "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON" => Ok(true),
        "0" | "false" | "FALSE" | "no" | "NO" | "off" | "OFF" => Ok(false),
        other => Err(SingletonError::InvalidInput(format!(
            "{name} must be 1/0 or true/false, got '{other}'"
        ))),
    }
}

fn parse_workspace_provider(value: &str) -> Result<WorkspaceProvider> {
    match value {
        "local_path" => Ok(WorkspaceProvider::LocalPath),
        "git_worktree" => Ok(WorkspaceProvider::GitWorktree),
        "backend_default" => Ok(WorkspaceProvider::BackendDefault),
        other => Err(SingletonError::InvalidInput(format!(
            "unsupported repo workspace provider '{other}'"
        ))),
    }
}

fn parse_cleanup_policy(value: &str) -> Result<CleanupPolicy> {
    match value {
        "keep" => Ok(CleanupPolicy::Keep),
        "delete_on_archive" => Ok(CleanupPolicy::DeleteOnArchive),
        "delete_on_success" => Ok(CleanupPolicy::DeleteOnSuccess),
        other => Err(SingletonError::InvalidInput(format!(
            "unsupported cleanup policy '{other}'"
        ))),
    }
}

fn parse_permission_default(value: &str) -> Result<PermissionDefault> {
    match value {
        "ask" => Ok(PermissionDefault::Ask),
        "allow" => Ok(PermissionDefault::Allow),
        "deny" => Ok(PermissionDefault::Deny),
        other => Err(SingletonError::InvalidInput(format!(
            "unsupported permission default '{other}'"
        ))),
    }
}

fn looks_like_git_repo(source: &Path) -> bool {
    source.join(".git").exists()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn synthesizes_no_config_defaults() -> Result<()> {
        let temp = TempDir::new()
            .map_err(|error| SingletonError::Store(format!("create temp dir: {error}")))?;
        let home = temp.path().join("home");
        let env = ConfigEnvironment::with_home(&home);
        let effective = load_effective_config(ConfigLoadOptions::new(temp.path(), env))?;

        assert_eq!(effective.profile, "default");
        assert_eq!(effective.backend, "copilot");
        assert_eq!(effective.model, None);
        assert_eq!(effective.mode, "interactive");
        assert_eq!(effective.state_dir, home.join(".singleton"));
        assert_eq!(effective.database, home.join(".singleton/singleton.db"));
        assert_eq!(effective.default_host, "host_local");
        assert_eq!(
            effective.repo_workspace_provider,
            WorkspaceProvider::GitWorktree
        );
        assert_eq!(effective.cleanup_policy, CleanupPolicy::Keep);
        assert_eq!(effective.permissions.default, PermissionDefault::Ask);
        assert!(matches!(
            effective.hosts.get("host_local"),
            Some(EffectiveHostConfig::Local { .. })
        ));
        let defaults = effective.capability_defaults();
        assert_eq!(defaults.default_host, "host_local");
        assert_eq!(defaults.model, None);
        assert_eq!(defaults.permissions.default, "ask");
        Ok(())
    }

    #[test]
    fn parses_toml_profiles_hosts_and_repos() -> Result<()> {
        let parsed = parse_config_toml(
            r#"
version = 1
default_profile = "work"

[profiles.work]
backend = "copilot"
mode = "plan"
default_host = "dev"

[profiles.work.permissions]
default = "deny"

[hosts.dev]
kind = "ssh"
target = "devbox"
connect_command = "singleton serve --stdio"
ssh_args = ["-A"]

[repos.singleton]
path = "~/src/singleton"
default_host = "dev"
repo_workspace_provider = "git_worktree"
"#,
        )?;

        assert_eq!(parsed.version(), 1);
        assert_eq!(parsed.default_profile(), Some("work"));
        assert!(parsed.profiles().contains_key("work"));
        Ok(())
    }

    #[test]
    fn expands_tilde_paths() -> Result<()> {
        let home = PathBuf::from("/tmp/singleton-test-home");
        let env = ConfigEnvironment::with_home(&home);
        assert_eq!(
            expand_tilde(Path::new("~/state/singleton.db"), &env)?,
            home.join("state/singleton.db")
        );
        assert_eq!(
            expand_tilde(Path::new("/var/db/singleton.db"), &env)?,
            PathBuf::from("/var/db/singleton.db")
        );
        Ok(())
    }

    #[test]
    fn locates_user_and_project_config_paths() -> Result<()> {
        let temp = TempDir::new()
            .map_err(|error| SingletonError::Store(format!("create temp dir: {error}")))?;
        let home = temp.path().join("home");
        let xdg = temp.path().join("xdg");
        let env = ConfigEnvironment::new(Some(home.clone()), Some(xdg.clone()));
        assert_eq!(
            user_config_path(&env)?,
            xdg.join("singleton").join("singleton.toml")
        );
        assert_eq!(
            user_config_path(&ConfigEnvironment::with_home(&home))?,
            home.join(".config")
                .join("singleton")
                .join("singleton.toml")
        );

        let project = temp.path().join("repo").join("nested");
        fs::create_dir_all(&project)
            .map_err(|error| SingletonError::Store(format!("create project dir: {error}")))?;
        let project_config = temp.path().join("repo").join(".singleton.toml");
        fs::write(&project_config, "version = 1\n")
            .map_err(|error| SingletonError::Store(format!("write project config: {error}")))?;
        assert_eq!(find_project_config(&project), Some(project_config));
        Ok(())
    }

    #[test]
    fn invalid_config_roots_do_not_load_repo_relative_user_config() -> Result<()> {
        let temp = TempDir::new()
            .map_err(|error| SingletonError::Store(format!("create temp dir: {error}")))?;
        let repo = temp.path().join("repo");
        let malicious_user_config = repo.join("singleton").join("singleton.toml");
        fs::create_dir_all(malicious_user_config.parent().unwrap_or(&repo))
            .map_err(|error| SingletonError::Store(format!("create config dir: {error}")))?;
        fs::write(
            &malicious_user_config,
            r#"
version = 1
[profiles.default]
backend = "fake"
"#,
        )
        .map_err(|error| SingletonError::Store(format!("write config: {error}")))?;
        let database = temp.path().join("singleton.db");
        let mut options = ConfigLoadOptions::new(
            &repo,
            ConfigEnvironment::new(
                Some(PathBuf::from("relative-home")),
                Some(PathBuf::from("")),
            ),
        );
        options.cli_overrides = ConfigFieldOverrides::default().with_database(database);

        let effective = load_effective_config(options)?;

        assert_eq!(effective.backend, DEFAULT_BACKEND);
        Ok(())
    }

    #[test]
    fn repo_workspace_provider_falls_back_for_plain_dirs() -> Result<()> {
        let temp = TempDir::new()
            .map_err(|error| SingletonError::Store(format!("create temp dir: {error}")))?;
        let env = ConfigEnvironment::with_home(temp.path());
        let effective = load_effective_config(ConfigLoadOptions::new(temp.path(), env))?;
        let plain_dir = temp.path().join("plain");
        fs::create_dir_all(&plain_dir)
            .map_err(|error| SingletonError::Store(format!("create plain dir: {error}")))?;
        assert_eq!(
            effective.repo_source_workspace_provider(&plain_dir),
            WorkspaceProvider::LocalPath
        );

        let git_repo = temp.path().join("repo");
        fs::create_dir_all(git_repo.join(".git"))
            .map_err(|error| SingletonError::Store(format!("create git dir: {error}")))?;
        assert_eq!(
            effective.repo_source_workspace_provider(&git_repo),
            WorkspaceProvider::GitWorktree
        );
        Ok(())
    }

    #[test]
    fn merges_user_project_env_and_cli_precedence() -> Result<()> {
        let temp = TempDir::new()
            .map_err(|error| SingletonError::Store(format!("create temp dir: {error}")))?;
        let home = temp.path().join("home");
        let xdg = temp.path().join("xdg");
        let user_config = xdg.join("singleton").join("singleton.toml");
        let project = temp.path().join("repo").join("nested");
        fs::create_dir_all(user_config.parent().unwrap_or(temp.path()))
            .map_err(|error| SingletonError::Store(format!("create user config dir: {error}")))?;
        fs::create_dir_all(&project)
            .map_err(|error| SingletonError::Store(format!("create project dir: {error}")))?;
        fs::write(
            &user_config,
            r#"
version = 1
[profiles.default]
backend = "fake"
mode = "plan"
database = "~/user.db"
"#,
        )
        .map_err(|error| SingletonError::Store(format!("write user config: {error}")))?;
        fs::write(
            temp.path().join("repo").join(".singleton.toml"),
            r#"
version = 1
[profiles.default]
backend = "copilot"
database = "~/project.db"
"#,
        )
        .map_err(|error| SingletonError::Store(format!("write project config: {error}")))?;
        let mut env = ConfigEnvironment::new(Some(home.clone()), Some(xdg));
        env.set_var(ENV_BACKEND, "fake");
        env.set_var(ENV_DATABASE, "~/env.db");
        let mut options = ConfigLoadOptions::new(project, env);
        options.cli_overrides = ConfigFieldOverrides::default()
            .with_backend("copilot")
            .with_database("~/cli.db");

        let effective = load_effective_config(options)?;

        assert_eq!(effective.backend, "copilot");
        assert_eq!(effective.mode, "plan");
        assert_eq!(effective.database, home.join("cli.db"));
        Ok(())
    }

    #[test]
    fn applies_env_overrides_without_files() -> Result<()> {
        let temp = TempDir::new()
            .map_err(|error| SingletonError::Store(format!("create temp dir: {error}")))?;
        let home = temp.path().join("home");
        let mut env = ConfigEnvironment::with_home(&home);
        env.set_var(ENV_BACKEND, "fake");
        env.set_var(ENV_MODE, "autopilot");
        env.set_var(ENV_DATABASE, "~/env.db");
        env.set_var(ENV_PERMISSION_DEFAULT, "allow");

        let effective = load_effective_config(ConfigLoadOptions::new(temp.path(), env))?;

        assert_eq!(effective.backend, "fake");
        assert_eq!(effective.mode, "autopilot");
        assert_eq!(effective.database, home.join("env.db"));
        assert_eq!(effective.permissions.default, PermissionDefault::Allow);
        Ok(())
    }

    #[test]
    fn explicit_database_does_not_require_home() -> Result<()> {
        let temp = TempDir::new()
            .map_err(|error| SingletonError::Store(format!("create temp dir: {error}")))?;
        let database = temp.path().join("custom.db");
        let mut options = ConfigLoadOptions::new(temp.path(), ConfigEnvironment::new(None, None));
        options.cli_overrides = ConfigFieldOverrides::default().with_database(database.clone());

        let effective = load_effective_config(options)?;

        assert_eq!(effective.database, database);
        assert_eq!(effective.state_dir, temp.path());
        Ok(())
    }

    #[test]
    fn project_config_can_be_opted_out() -> Result<()> {
        let temp = TempDir::new()
            .map_err(|error| SingletonError::Store(format!("create temp dir: {error}")))?;
        let home = temp.path().join("home");
        let xdg = temp.path().join("xdg");
        let user_config = xdg.join("singleton").join("singleton.toml");
        let project = temp.path().join("repo");
        fs::create_dir_all(user_config.parent().unwrap_or(temp.path()))
            .map_err(|error| SingletonError::Store(format!("create user config dir: {error}")))?;
        fs::create_dir_all(&project)
            .map_err(|error| SingletonError::Store(format!("create project dir: {error}")))?;
        fs::write(
            &user_config,
            r#"
version = 1
[profiles.default]
backend = "fake"
"#,
        )
        .map_err(|error| SingletonError::Store(format!("write user config: {error}")))?;
        fs::write(
            project.join(".singleton.toml"),
            r#"
version = 1
[profiles.default]
backend = "copilot"
"#,
        )
        .map_err(|error| SingletonError::Store(format!("write project config: {error}")))?;
        let mut options =
            ConfigLoadOptions::new(&project, ConfigEnvironment::new(Some(home), Some(xdg)));
        options.no_project_config = true;

        let effective = load_effective_config(options)?;

        assert_eq!(effective.backend, "fake");

        let mut env = ConfigEnvironment::new(Some(temp.path().join("home-env")), None);
        env.set_var(ENV_CONFIG, user_config.to_string_lossy());
        env.set_var(ENV_NO_PROJECT_CONFIG, "1");
        let effective = load_effective_config(ConfigLoadOptions::new(&project, env))?;
        assert_eq!(effective.backend, "fake");
        Ok(())
    }

    #[test]
    fn invalid_version_is_rejected() -> Result<()> {
        assert_error_contains(
            parse_config_toml("version = 2"),
            "unsupported singleton config version",
        )
    }

    #[test]
    fn invalid_profile_reference_is_rejected() -> Result<()> {
        let temp = TempDir::new()
            .map_err(|error| SingletonError::Store(format!("create temp dir: {error}")))?;
        let config = temp.path().join("singleton.toml");
        fs::write(
            &config,
            r#"
version = 1
default_profile = "missing"
"#,
        )
        .map_err(|error| SingletonError::Store(format!("write config: {error}")))?;
        let mut options =
            ConfigLoadOptions::new(temp.path(), ConfigEnvironment::with_home(temp.path()));
        options.config_path = Some(config);

        assert_error_contains(load_effective_config(options), "default_profile 'missing'")
    }

    #[test]
    fn invalid_host_reference_is_rejected() -> Result<()> {
        let temp = TempDir::new()
            .map_err(|error| SingletonError::Store(format!("create temp dir: {error}")))?;
        let config = temp.path().join("singleton.toml");
        fs::write(
            &config,
            r#"
version = 1
[profiles.default]
default_host = "missing"
"#,
        )
        .map_err(|error| SingletonError::Store(format!("write config: {error}")))?;
        let mut options =
            ConfigLoadOptions::new(temp.path(), ConfigEnvironment::with_home(temp.path()));
        options.config_path = Some(config);

        assert_error_contains(
            load_effective_config(options),
            "unknown default_host 'missing'",
        )
    }

    #[test]
    fn invalid_enum_value_is_rejected() -> Result<()> {
        assert_error_contains(
            parse_config_toml(
                r#"
version = 1
[profiles.default]
cleanup_policy = "delete_immediately"
"#,
            ),
            "unknown variant",
        )
    }

    #[test]
    fn invalid_repo_path_combination_is_rejected() -> Result<()> {
        assert_error_contains(
            parse_config_toml(
                r#"
version = 1
[repos.singleton]
default_host = "host_local"
"#,
            ),
            "requires path or url",
        )
    }

    #[test]
    fn invalid_repo_host_reference_is_rejected() -> Result<()> {
        let temp = TempDir::new()
            .map_err(|error| SingletonError::Store(format!("create temp dir: {error}")))?;
        let config = temp.path().join("singleton.toml");
        fs::write(
            &config,
            r#"
version = 1
[repos.singleton]
path = "~/src/singleton"
default_host = "missing"
"#,
        )
        .map_err(|error| SingletonError::Store(format!("write config: {error}")))?;
        let mut options =
            ConfigLoadOptions::new(temp.path(), ConfigEnvironment::with_home(temp.path()));
        options.config_path = Some(config);

        assert_error_contains(
            load_effective_config(options),
            "repo 'singleton' references unknown default_host 'missing'",
        )
    }

    #[test]
    fn local_host_rejects_ssh_fields() -> Result<()> {
        let temp = TempDir::new()
            .map_err(|error| SingletonError::Store(format!("create temp dir: {error}")))?;
        let config = temp.path().join("singleton.toml");
        fs::write(
            &config,
            r#"
version = 1
[hosts.host_local]
kind = "local"
target = "devbox"
"#,
        )
        .map_err(|error| SingletonError::Store(format!("write config: {error}")))?;
        let mut options =
            ConfigLoadOptions::new(temp.path(), ConfigEnvironment::with_home(temp.path()));
        options.config_path = Some(config);

        assert_error_contains(
            load_effective_config(options),
            "local host 'host_local' may not include SSH fields",
        )
    }

    #[test]
    fn project_config_may_not_set_ssh_connect_command() -> Result<()> {
        let temp = TempDir::new()
            .map_err(|error| SingletonError::Store(format!("create temp dir: {error}")))?;
        let project = temp.path().join("repo");
        fs::create_dir_all(&project)
            .map_err(|error| SingletonError::Store(format!("create project dir: {error}")))?;
        fs::write(
            project.join(".singleton.toml"),
            r#"
version = 1
[hosts.dev]
kind = "ssh"
target = "devbox"
connect_command = "ssh devbox"
"#,
        )
        .map_err(|error| SingletonError::Store(format!("write project config: {error}")))?;

        assert_error_contains(
            load_effective_config(ConfigLoadOptions::new(
                &project,
                ConfigEnvironment::with_home(temp.path()),
            )),
            "non-default connect_command",
        )
    }

    #[test]
    fn project_config_may_not_set_ssh_args() -> Result<()> {
        let temp = TempDir::new()
            .map_err(|error| SingletonError::Store(format!("create temp dir: {error}")))?;
        let project = temp.path().join("repo");
        fs::create_dir_all(&project)
            .map_err(|error| SingletonError::Store(format!("create project dir: {error}")))?;
        fs::write(
            project.join(".singleton.toml"),
            r#"
version = 1
[hosts.dev]
kind = "ssh"
target = "devbox"
ssh_args = ["-A"]
"#,
        )
        .map_err(|error| SingletonError::Store(format!("write project config: {error}")))?;

        assert_error_contains(
            load_effective_config(ConfigLoadOptions::new(
                &project,
                ConfigEnvironment::with_home(temp.path()),
            )),
            "project config may not set ssh_args",
        )
    }

    #[test]
    fn project_config_may_not_inherit_trusted_user_ssh_args() -> Result<()> {
        let temp = TempDir::new()
            .map_err(|error| SingletonError::Store(format!("create temp dir: {error}")))?;
        let home = temp.path().join("home");
        let xdg = temp.path().join("xdg");
        let user_config = xdg.join("singleton").join("singleton.toml");
        let project = temp.path().join("repo");
        fs::create_dir_all(user_config.parent().unwrap_or(temp.path()))
            .map_err(|error| SingletonError::Store(format!("create user config dir: {error}")))?;
        fs::create_dir_all(&project)
            .map_err(|error| SingletonError::Store(format!("create project dir: {error}")))?;
        fs::write(
            &user_config,
            r#"
version = 1
[hosts.dev]
kind = "ssh"
target = "devbox"
ssh_args = ["-A"]
"#,
        )
        .map_err(|error| SingletonError::Store(format!("write user config: {error}")))?;
        fs::write(
            project.join(".singleton.toml"),
            r#"
version = 1
[hosts.dev]
target = "project-devbox"
"#,
        )
        .map_err(|error| SingletonError::Store(format!("write project config: {error}")))?;

        assert_error_contains(
            load_effective_config(ConfigLoadOptions::new(
                &project,
                ConfigEnvironment::new(Some(home), Some(xdg)),
            )),
            "project config may not inherit or set ssh_args",
        )
    }

    #[test]
    fn project_config_may_not_inherit_trusted_user_connect_command() -> Result<()> {
        let temp = TempDir::new()
            .map_err(|error| SingletonError::Store(format!("create temp dir: {error}")))?;
        let home = temp.path().join("home");
        let xdg = temp.path().join("xdg");
        let user_config = xdg.join("singleton").join("singleton.toml");
        let project = temp.path().join("repo");
        fs::create_dir_all(user_config.parent().unwrap_or(temp.path()))
            .map_err(|error| SingletonError::Store(format!("create user config dir: {error}")))?;
        fs::create_dir_all(&project)
            .map_err(|error| SingletonError::Store(format!("create project dir: {error}")))?;
        fs::write(
            &user_config,
            r#"
version = 1
[hosts.dev]
kind = "ssh"
target = "devbox"
connect_command = "/opt/singleton/bin/singleton serve --stdio"
"#,
        )
        .map_err(|error| SingletonError::Store(format!("write user config: {error}")))?;
        fs::write(
            project.join(".singleton.toml"),
            r#"
version = 1
[hosts.dev]
target = "project-devbox"
"#,
        )
        .map_err(|error| SingletonError::Store(format!("write project config: {error}")))?;

        assert_error_contains(
            load_effective_config(ConfigLoadOptions::new(
                &project,
                ConfigEnvironment::new(Some(home), Some(xdg)),
            )),
            "project config may not inherit or set non-default connect_command",
        )
    }

    #[test]
    fn project_config_may_set_default_ssh_connect_command() -> Result<()> {
        let temp = TempDir::new()
            .map_err(|error| SingletonError::Store(format!("create temp dir: {error}")))?;
        let project = temp.path().join("repo");
        fs::create_dir_all(&project)
            .map_err(|error| SingletonError::Store(format!("create project dir: {error}")))?;
        fs::write(
            project.join(".singleton.toml"),
            r#"
version = 1
[profiles.default]
default_host = "dev"

[hosts.dev]
kind = "ssh"
target = "devbox"
connect_command = "singleton serve --stdio"
"#,
        )
        .map_err(|error| SingletonError::Store(format!("write project config: {error}")))?;

        let effective = load_effective_config(ConfigLoadOptions::new(
            &project,
            ConfigEnvironment::with_home(temp.path()),
        ))?;
        assert!(matches!(
            effective.hosts.get("dev"),
            Some(EffectiveHostConfig::Ssh {
                connect_command,
                ..
            }) if connect_command == DEFAULT_SSH_CONNECT_COMMAND
        ));
        Ok(())
    }

    #[test]
    fn raw_secret_looking_ssh_fields_are_rejected() -> Result<()> {
        assert_error_contains(
            parse_config_toml(
                r#"
version = 1
[hosts.dev]
kind = "ssh"
target = "devbox"
ssh_args = ["token=ghp_1234567890"]
"#,
            ),
            "looks like raw secret material",
        )
    }

    #[test]
    fn redacted_summary_hides_ssh_sensitive_fields() -> Result<()> {
        let temp = TempDir::new()
            .map_err(|error| SingletonError::Store(format!("create temp dir: {error}")))?;
        let config = temp.path().join("singleton.toml");
        fs::write(
            &config,
            r#"
version = 1
[profiles.default]
default_host = "dev"

[hosts.dev]
kind = "ssh"
target = "devbox"
connect_command = "ssh devbox"
ssh_args = ["-A"]
"#,
        )
        .map_err(|error| SingletonError::Store(format!("write config: {error}")))?;
        let mut options =
            ConfigLoadOptions::new(temp.path(), ConfigEnvironment::with_home(temp.path()));
        options.config_path = Some(config);

        let redacted = load_effective_config(options)?.redacted();

        assert!(matches!(
            redacted.hosts.get("dev"),
            Some(RedactedHostConfig::Ssh {
                connect_command,
                ssh_args,
                ..
            }) if connect_command == "<redacted>"
                && ssh_args == &vec!["<redacted>".to_string()]
        ));
        Ok(())
    }

    fn assert_error_contains<T>(result: Result<T>, expected: &str) -> Result<()> {
        match result {
            Ok(_) => Err(SingletonError::InvalidState(format!(
                "expected error containing '{expected}'"
            ))),
            Err(error) => {
                assert!(
                    error.to_string().contains(expected),
                    "expected error to contain '{expected}', got '{error}'"
                );
                Ok(())
            }
        }
    }
}
