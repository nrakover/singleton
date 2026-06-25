use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use singleton_core::{
    CleanupPolicy, CleanupSummary, CloseDisposition, Host, HostCapabilities, HostConnector, HostId,
    HostKind, RepoMetadata, ResourceKind, ResourceStatus, Result, SingletonError, Workspace,
    WorkspaceSpec, new_id, now_rfc3339, resource_uri,
};
use tokio::process::Command;

#[derive(Debug, Clone, Default)]
pub struct LocalHostConnector;

#[async_trait]
impl HostConnector for LocalHostConnector {
    fn host(&self) -> Host {
        Host::local()
    }

    async fn ensure_workspace(&self, spec: WorkspaceSpec) -> Result<Workspace> {
        match spec {
            WorkspaceSpec::ExistingWorkspace { workspace_id } => Err(SingletonError::InvalidInput(
                format!("existing workspace {workspace_id} must be resolved by the broker store"),
            )),
            WorkspaceSpec::LocalPath {
                path,
                host_id,
                cleanup_policy,
            } => {
                let path_buf = PathBuf::from(&path);
                if !path_buf.exists() {
                    return Err(SingletonError::Host {
                        host: singleton_core::LOCAL_HOST_ID.to_string(),
                        message: format!("local workspace path does not exist: {path}"),
                    });
                }
                Ok(new_workspace(
                    host_id,
                    Some(path),
                    None,
                    cleanup_policy.unwrap_or_default(),
                ))
            }
            WorkspaceSpec::GitWorktree {
                repo,
                base_ref,
                branch,
                create_branch,
                worktree_path_hint,
                host_id,
                cleanup_policy,
            } => {
                let repo_path = canonicalize_existing(&repo)?;
                let base_ref = base_ref.unwrap_or_else(|| "HEAD".to_string());
                let branch = branch.unwrap_or_else(|| new_id("branch").replace('_', "-"));
                let worktree_path = match worktree_path_hint {
                    Some(path) => PathBuf::from(path),
                    None => repo_path
                        .parent()
                        .ok_or_else(|| SingletonError::Host {
                            host: singleton_core::LOCAL_HOST_ID.to_string(),
                            message: format!(
                                "cannot infer worktree parent for repo {}",
                                repo_path.display()
                            ),
                        })?
                        .join(format!(".singleton-worktree-{branch}")),
                };
                add_worktree(
                    &repo_path,
                    &worktree_path,
                    &base_ref,
                    &branch,
                    create_branch.unwrap_or(true),
                )
                .await?;
                Ok(new_workspace(
                    host_id,
                    Some(worktree_path.to_string_lossy().to_string()),
                    Some(RepoMetadata {
                        root: Some(repo_path.to_string_lossy().to_string()),
                        remote: git_remote(&repo_path).await.ok(),
                        base_ref: Some(base_ref),
                        branch: Some(branch),
                    }),
                    cleanup_policy.unwrap_or_default(),
                ))
            }
            WorkspaceSpec::BackendDefault {
                host_id,
                cleanup_policy,
            } => Ok(new_workspace(
                host_id,
                None,
                None,
                cleanup_policy.unwrap_or_default(),
            )),
        }
    }

    async fn close_workspace(
        &self,
        workspace: &Workspace,
        disposition: CloseDisposition,
        force: bool,
    ) -> Result<CleanupSummary> {
        if disposition != CloseDisposition::Delete {
            return Ok(CleanupSummary {
                deleted_paths: Vec::new(),
                skipped: vec![format!(
                    "workspace archived with disposition {disposition:?}"
                )],
            });
        }

        let Some(path) = &workspace.path else {
            return Ok(CleanupSummary {
                deleted_paths: Vec::new(),
                skipped: vec!["backend-default workspace has no local path".to_string()],
            });
        };

        let path_buf = PathBuf::from(path);
        if !path_buf.exists() {
            return Ok(CleanupSummary {
                deleted_paths: Vec::new(),
                skipped: vec![format!("path already absent: {}", path_buf.display())],
            });
        }

        if let Some(repo) = &workspace.repo
            && let Some(root) = &repo.root
        {
            remove_worktree(Path::new(root), &path_buf, force).await?;
            return Ok(CleanupSummary {
                deleted_paths: vec![path_buf.to_string_lossy().to_string()],
                skipped: Vec::new(),
            });
        }

        Err(SingletonError::Host {
            host: singleton_core::LOCAL_HOST_ID.to_string(),
            message: format!(
                "refusing to delete non-git workspace path without explicit workspace provider support: {}",
                path_buf.display()
            ),
        })
    }
}

#[derive(Debug, Clone)]
pub struct SshHostConfig {
    pub host_id: HostId,
    pub target: String,
    pub ssh_args: Vec<String>,
    pub remote_state_dir: String,
    pub agent_backends: Vec<String>,
    pub auth_ref: Option<String>,
}

impl SshHostConfig {
    pub fn new(host_id: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            host_id: host_id.into(),
            target: target.into(),
            ssh_args: Vec::new(),
            remote_state_dir: "~/.singleton".to_string(),
            agent_backends: vec![singleton_core::COPILOT_BACKEND_ID.to_string()],
            auth_ref: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCommandOutput {
    pub stdout: String,
    pub stderr: String,
}

#[async_trait]
pub trait RemoteRunner: Send + Sync {
    async fn run(
        &self,
        target: &str,
        ssh_args: &[String],
        command: &str,
    ) -> Result<RemoteCommandOutput>;
}

#[derive(Debug, Clone, Default)]
pub struct SshRemoteRunner;

#[async_trait]
impl RemoteRunner for SshRemoteRunner {
    async fn run(
        &self,
        target: &str,
        ssh_args: &[String],
        command: &str,
    ) -> Result<RemoteCommandOutput> {
        let output = Command::new("ssh")
            .args(ssh_args)
            .arg(target)
            .arg(command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|error| SingletonError::Host {
                host: target.to_string(),
                message: format!("run ssh command: {error}"),
            })?;
        if output.status.success() {
            Ok(RemoteCommandOutput {
                stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            })
        } else {
            Err(SingletonError::Host {
                host: target.to_string(),
                message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            })
        }
    }
}

#[derive(Clone)]
pub struct SshHostConnector<R>
where
    R: RemoteRunner,
{
    config: SshHostConfig,
    runner: R,
}

impl<R> SshHostConnector<R>
where
    R: RemoteRunner,
{
    pub fn new(config: SshHostConfig, runner: R) -> Self {
        Self { config, runner }
    }
}

#[async_trait]
impl<R> HostConnector for SshHostConnector<R>
where
    R: RemoteRunner,
{
    fn host(&self) -> Host {
        Host {
            host_id: self.config.host_id.clone(),
            resource_uri: resource_uri(ResourceKind::Host, &self.config.host_id),
            kind: HostKind::Ssh,
            status: ResourceStatus::Ready,
            capabilities: HostCapabilities {
                workspace_providers: vec![
                    "local_path".to_string(),
                    "git_worktree".to_string(),
                    "backend_default".to_string(),
                ],
                agent_backends: self.config.agent_backends.clone(),
                supports_reconnect: true,
                supports_ordered_events: true,
            },
        }
    }

    async fn ensure_workspace(&self, spec: WorkspaceSpec) -> Result<Workspace> {
        match spec {
            WorkspaceSpec::ExistingWorkspace { workspace_id } => Err(SingletonError::InvalidInput(
                format!("existing workspace {workspace_id} must be resolved by the broker store"),
            )),
            WorkspaceSpec::LocalPath {
                path,
                cleanup_policy,
                ..
            } => {
                self.runner
                    .run(
                        &self.config.target,
                        &self.config.ssh_args,
                        &format!("test -d {}", shell_quote(&path)),
                    )
                    .await?;
                Ok(new_remote_workspace(
                    &self.config.host_id,
                    Some(path),
                    None,
                    cleanup_policy.unwrap_or_default(),
                ))
            }
            WorkspaceSpec::GitWorktree {
                repo,
                base_ref,
                branch,
                create_branch,
                worktree_path_hint,
                cleanup_policy,
                ..
            } => {
                let base_ref = base_ref.unwrap_or_else(|| "HEAD".to_string());
                let branch = branch.unwrap_or_else(|| new_id("branch").replace('_', "-"));
                let worktree_path = worktree_path_hint.unwrap_or_else(|| {
                    format!(
                        "{}/worktrees/{}",
                        self.config.remote_state_dir.trim_end_matches('/'),
                        branch
                    )
                });
                let command = remote_worktree_add_command(
                    &repo,
                    &worktree_path,
                    &base_ref,
                    &branch,
                    create_branch.unwrap_or(true),
                );
                self.runner
                    .run(&self.config.target, &self.config.ssh_args, &command)
                    .await?;
                Ok(new_remote_workspace(
                    &self.config.host_id,
                    Some(worktree_path),
                    Some(RepoMetadata {
                        root: Some(repo),
                        remote: None,
                        base_ref: Some(base_ref),
                        branch: Some(branch),
                    }),
                    cleanup_policy.unwrap_or_default(),
                ))
            }
            WorkspaceSpec::BackendDefault { cleanup_policy, .. } => Ok(new_remote_workspace(
                &self.config.host_id,
                None,
                None,
                cleanup_policy.unwrap_or_default(),
            )),
        }
    }

    async fn close_workspace(
        &self,
        workspace: &Workspace,
        disposition: CloseDisposition,
        force: bool,
    ) -> Result<CleanupSummary> {
        if disposition != CloseDisposition::Delete {
            return Ok(CleanupSummary {
                deleted_paths: Vec::new(),
                skipped: vec![format!(
                    "remote workspace archived with disposition {disposition:?}"
                )],
            });
        }
        let Some(path) = &workspace.path else {
            return Ok(CleanupSummary {
                deleted_paths: Vec::new(),
                skipped: vec!["backend-default workspace has no remote path".to_string()],
            });
        };
        let Some(repo) = &workspace.repo else {
            return Err(SingletonError::Host {
                host: self.config.host_id.clone(),
                message: format!("refusing to delete non-git remote workspace: {path}"),
            });
        };
        let Some(root) = &repo.root else {
            return Err(SingletonError::Host {
                host: self.config.host_id.clone(),
                message: format!("remote workspace has no repo root: {path}"),
            });
        };
        let command = remote_worktree_remove_command(root, path, force);
        self.runner
            .run(&self.config.target, &self.config.ssh_args, &command)
            .await?;
        Ok(CleanupSummary {
            deleted_paths: vec![path.clone()],
            skipped: Vec::new(),
        })
    }
}

fn new_remote_workspace(
    host_id: &str,
    path: Option<String>,
    repo: Option<RepoMetadata>,
    cleanup_policy: CleanupPolicy,
) -> Workspace {
    let workspace_id = new_id("work");
    Workspace {
        resource_uri: resource_uri(ResourceKind::Workspace, &workspace_id),
        workspace_id,
        host_id: host_id.to_string(),
        status: ResourceStatus::Ready,
        path,
        repo,
        cleanup_policy,
        created_at: now_rfc3339(),
    }
}

pub fn remote_worktree_add_command(
    repo: &str,
    worktree: &str,
    base_ref: &str,
    branch: &str,
    create_branch: bool,
) -> String {
    let parent = format!("$(dirname {})", shell_quote(worktree));
    let mut command = format!(
        "mkdir -p {parent} && git -C {} worktree add",
        shell_quote(repo)
    );
    if create_branch {
        command.push_str(&format!(" -b {}", shell_quote(branch)));
    }
    command.push_str(&format!(
        " {} {}",
        shell_quote(worktree),
        shell_quote(base_ref)
    ));
    command
}

pub fn remote_worktree_remove_command(repo: &str, worktree: &str, force: bool) -> String {
    let mut command = format!("git -C {} worktree remove", shell_quote(repo));
    if force {
        command.push_str(" --force");
    }
    command.push_str(&format!(" {}", shell_quote(worktree)));
    command
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn new_workspace(
    host_id: Option<String>,
    path: Option<String>,
    repo: Option<RepoMetadata>,
    cleanup_policy: CleanupPolicy,
) -> Workspace {
    let workspace_id = new_id("work");
    Workspace {
        resource_uri: resource_uri(ResourceKind::Workspace, &workspace_id),
        workspace_id,
        host_id: host_id.unwrap_or_else(|| singleton_core::LOCAL_HOST_ID.to_string()),
        status: ResourceStatus::Ready,
        path,
        repo,
        cleanup_policy,
        created_at: now_rfc3339(),
    }
}

fn canonicalize_existing(path: &str) -> Result<PathBuf> {
    let path_buf = PathBuf::from(path);
    path_buf
        .canonicalize()
        .map_err(|error| SingletonError::Host {
            host: singleton_core::LOCAL_HOST_ID.to_string(),
            message: format!("invalid path {path}: {error}"),
        })
}

async fn add_worktree(
    repo: &Path,
    worktree: &Path,
    base_ref: &str,
    branch: &str,
    create_branch: bool,
) -> Result<()> {
    let mut command = Command::new("git");
    command.arg("-C").arg(repo).arg("worktree").arg("add");
    if create_branch {
        command.arg("-b").arg(branch);
    } else if !branch.is_empty() {
        command.arg("-B").arg(branch);
    }
    command.arg(worktree).arg(base_ref);
    run_git(command, "add git worktree").await
}

async fn remove_worktree(repo: &Path, worktree: &Path, force: bool) -> Result<()> {
    let mut command = Command::new("git");
    command.arg("-C").arg(repo).arg("worktree").arg("remove");
    if force {
        command.arg("--force");
    }
    command.arg(worktree);
    run_git(command, "remove git worktree").await
}

async fn git_remote(repo: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("remote")
        .arg("get-url")
        .arg("origin")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|error| SingletonError::Host {
            host: singleton_core::LOCAL_HOST_ID.to_string(),
            message: format!("read git remote: {error}"),
        })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(SingletonError::Host {
            host: singleton_core::LOCAL_HOST_ID.to_string(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

async fn run_git(mut command: Command, action: &str) -> Result<()> {
    let output = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|error| SingletonError::Host {
            host: singleton_core::LOCAL_HOST_ID.to_string(),
            message: format!("{action}: {error}"),
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(SingletonError::Host {
            host: singleton_core::LOCAL_HOST_ID.to_string(),
            message: format!(
                "{action}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::{Arc, Mutex};

    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn local_path_requires_existing_directory() -> Result<()> {
        let temp = TempDir::new().map_err(|error| SingletonError::Host {
            host: singleton_core::LOCAL_HOST_ID.to_string(),
            message: format!("create temp dir: {error}"),
        })?;
        let connector = LocalHostConnector;

        let workspace = connector
            .ensure_workspace(WorkspaceSpec::LocalPath {
                path: temp.path().to_string_lossy().to_string(),
                host_id: None,
                cleanup_policy: None,
            })
            .await?;

        assert_eq!(
            workspace.path,
            Some(temp.path().to_string_lossy().to_string())
        );
        Ok(())
    }

    #[tokio::test]
    async fn git_worktree_create_and_delete_is_idempotent() -> Result<()> {
        let temp = TempDir::new().map_err(|error| SingletonError::Host {
            host: singleton_core::LOCAL_HOST_ID.to_string(),
            message: format!("create temp dir: {error}"),
        })?;
        let repo = temp.path().join("repo");
        let worktree = temp.path().join("worktree");
        fs::create_dir(&repo).map_err(|error| SingletonError::Host {
            host: singleton_core::LOCAL_HOST_ID.to_string(),
            message: format!("create repo dir: {error}"),
        })?;
        git(&repo, &["init"]).await?;
        git(&repo, &["config", "user.email", "test@example.com"]).await?;
        git(&repo, &["config", "user.name", "Test User"]).await?;
        fs::write(repo.join("README.md"), "hello\n").map_err(|error| SingletonError::Host {
            host: singleton_core::LOCAL_HOST_ID.to_string(),
            message: format!("write readme: {error}"),
        })?;
        git(&repo, &["add", "README.md"]).await?;
        git(&repo, &["commit", "-m", "initial"]).await?;

        let connector = LocalHostConnector;
        let workspace = connector
            .ensure_workspace(WorkspaceSpec::GitWorktree {
                repo: repo.to_string_lossy().to_string(),
                base_ref: Some("HEAD".to_string()),
                branch: Some("test-worktree".to_string()),
                create_branch: Some(true),
                worktree_path_hint: Some(worktree.to_string_lossy().to_string()),
                host_id: None,
                cleanup_policy: Some(CleanupPolicy::Keep),
            })
            .await?;

        assert!(worktree.join("README.md").exists());
        let cleanup = connector
            .close_workspace(&workspace, CloseDisposition::Delete, true)
            .await?;
        assert_eq!(cleanup.deleted_paths.len(), 1);
        let cleanup = connector
            .close_workspace(&workspace, CloseDisposition::Delete, true)
            .await?;
        assert_eq!(cleanup.deleted_paths.len(), 0);
        Ok(())
    }

    async fn git(repo: &Path, args: &[&str]) -> Result<()> {
        let mut command = Command::new("git");
        command.arg("-C").arg(repo).args(args);
        run_git(command, "test git").await
    }

    #[derive(Clone, Default)]
    struct RecordingRunner {
        commands: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl RemoteRunner for RecordingRunner {
        async fn run(
            &self,
            _target: &str,
            _ssh_args: &[String],
            command: &str,
        ) -> Result<RemoteCommandOutput> {
            self.commands
                .lock()
                .map_err(|_| SingletonError::Host {
                    host: "ssh_test".to_string(),
                    message: "recording runner lock poisoned".to_string(),
                })?
                .push(command.to_string());
            Ok(RemoteCommandOutput {
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    #[tokio::test]
    async fn ssh_connector_builds_remote_worktree_commands() -> Result<()> {
        let runner = RecordingRunner::default();
        let commands = runner.commands.clone();
        let connector = SshHostConnector::new(SshHostConfig::new("host_ssh", "devbox"), runner);
        let workspace = connector
            .ensure_workspace(WorkspaceSpec::GitWorktree {
                repo: "/srv/repo".to_string(),
                base_ref: Some("main".to_string()),
                branch: Some("feature-x".to_string()),
                create_branch: Some(true),
                worktree_path_hint: Some("/srv/worktrees/feature-x".to_string()),
                host_id: None,
                cleanup_policy: Some(CleanupPolicy::Keep),
            })
            .await?;
        connector
            .close_workspace(&workspace, CloseDisposition::Delete, true)
            .await?;
        let commands = commands.lock().map_err(|_| SingletonError::Host {
            host: "ssh_test".to_string(),
            message: "recording runner lock poisoned".to_string(),
        })?;

        assert_eq!(commands.len(), 2);
        assert_eq!(
            commands[0],
            "mkdir -p $(dirname '/srv/worktrees/feature-x') && git -C '/srv/repo' worktree add -b 'feature-x' '/srv/worktrees/feature-x' 'main'"
        );
        assert_eq!(
            commands[1],
            "git -C '/srv/repo' worktree remove --force '/srv/worktrees/feature-x'"
        );
        Ok(())
    }

    #[test]
    fn shell_quote_handles_single_quotes() {
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
    }
}
