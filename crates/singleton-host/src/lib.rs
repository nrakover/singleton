use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use singleton_core::{
    CleanupPolicy, CleanupSummary, CloseDisposition, Host, HostCapabilities, HostConnector, HostId,
    HostKind, RepoMetadata, ResourceKind, ResourceStatus, Result, SingletonError, Workspace,
    WorkspaceSpec, new_id, now_rfc3339, resource_uri,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

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
    pub connect_command: String,
    pub trust: SshConfigTrust,
}

pub const DEFAULT_SSH_CONNECT_COMMAND: &str = "singleton serve --stdio";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SshConfigTrust {
    #[default]
    TrustedUser,
    Project,
}

impl SshHostConfig {
    pub fn new(host_id: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            host_id: host_id.into(),
            target: target.into(),
            ssh_args: Vec::new(),
            connect_command: DEFAULT_SSH_CONNECT_COMMAND.to_string(),
            trust: SshConfigTrust::TrustedUser,
        }
    }

    pub fn from_project_config(host_id: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            trust: SshConfigTrust::Project,
            ..Self::new(host_id, target)
        }
    }

    pub fn with_connect_command(mut self, connect_command: impl Into<String>) -> Self {
        self.connect_command = connect_command.into();
        self
    }

    pub fn with_ssh_args<I, S>(mut self, ssh_args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.ssh_args = ssh_args.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_trust(mut self, trust: SshConfigTrust) -> Self {
        self.trust = trust;
        self
    }

    pub fn control_invocation(&self) -> Result<SshInvocation> {
        self.validate()?;
        let mut args = self.ssh_args.clone();
        args.push(self.target.clone());
        args.push(self.connect_command.clone());
        Ok(SshInvocation {
            program: "ssh".to_string(),
            args,
        })
    }

    fn validate(&self) -> Result<()> {
        validate_ssh_target(&self.target)?;
        validate_ssh_args(&self.ssh_args)?;
        validate_remote_command(&self.connect_command, "connect_command")?;
        if self.trust == SshConfigTrust::Project
            && self.connect_command != DEFAULT_SSH_CONNECT_COMMAND
        {
            return Err(SingletonError::InvalidInput(
                "project ssh host config cannot override connect_command; use trusted user config for non-default remote commands"
                    .to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshInvocation {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteProcessExit {
    pub success: bool,
    pub code: Option<i32>,
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
        validate_ssh_target(target)?;
        validate_ssh_args(ssh_args)?;
        validate_remote_command(command, "remote command")?;
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

#[async_trait]
pub trait RemoteProcessTransport: Send + Sync {
    type Process: RemoteStdioProcess;

    async fn spawn(&self, invocation: SshInvocation) -> Result<Self::Process>;
}

#[async_trait]
pub trait RemoteStdioProcess: Send {
    async fn write_stdin(&mut self, bytes: &[u8]) -> Result<()>;

    async fn read_stdout_line(&mut self) -> Result<Option<Vec<u8>>>;

    async fn close_stdin(&mut self) -> Result<()>;

    async fn wait(&mut self) -> Result<RemoteProcessExit>;
}

#[derive(Debug, Clone, Default)]
pub struct SshProcessTransport;

#[async_trait]
impl RemoteProcessTransport for SshProcessTransport {
    type Process = SshChildProcess;

    async fn spawn(&self, invocation: SshInvocation) -> Result<Self::Process> {
        let mut child = Command::new(&invocation.program)
            .args(&invocation.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| SingletonError::Host {
                host: ssh_invocation_target(&invocation)
                    .unwrap_or("unknown")
                    .to_string(),
                message: format!("spawn ssh control surface: {error}"),
            })?;
        let stdin = child.stdin.take().ok_or_else(|| SingletonError::Host {
            host: ssh_invocation_target(&invocation)
                .unwrap_or("unknown")
                .to_string(),
            message: "ssh control surface stdin was not piped".to_string(),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| SingletonError::Host {
            host: ssh_invocation_target(&invocation)
                .unwrap_or("unknown")
                .to_string(),
            message: "ssh control surface stdout was not piped".to_string(),
        })?;
        Ok(SshChildProcess {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
        })
    }
}

pub struct SshChildProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

#[async_trait]
impl RemoteStdioProcess for SshChildProcess {
    async fn write_stdin(&mut self, bytes: &[u8]) -> Result<()> {
        let Some(stdin) = &mut self.stdin else {
            return Err(SingletonError::Host {
                host: "ssh".to_string(),
                message: "ssh control surface stdin is closed".to_string(),
            });
        };
        stdin
            .write_all(bytes)
            .await
            .map_err(|error| SingletonError::Host {
                host: "ssh".to_string(),
                message: format!("write ssh control surface stdin: {error}"),
            })
    }

    async fn read_stdout_line(&mut self) -> Result<Option<Vec<u8>>> {
        let mut line = Vec::new();
        let read = self
            .stdout
            .read_until(b'\n', &mut line)
            .await
            .map_err(|error| SingletonError::Host {
                host: "ssh".to_string(),
                message: format!("read ssh control surface stdout: {error}"),
            })?;
        if read == 0 { Ok(None) } else { Ok(Some(line)) }
    }

    async fn close_stdin(&mut self) -> Result<()> {
        let Some(mut stdin) = self.stdin.take() else {
            return Ok(());
        };
        stdin
            .shutdown()
            .await
            .map_err(|error| SingletonError::Host {
                host: "ssh".to_string(),
                message: format!("close ssh control surface stdin: {error}"),
            })
    }

    async fn wait(&mut self) -> Result<RemoteProcessExit> {
        let status = self
            .child
            .wait()
            .await
            .map_err(|error| SingletonError::Host {
                host: "ssh".to_string(),
                message: format!("wait for ssh control surface: {error}"),
            })?;
        Ok(RemoteProcessExit {
            success: status.success(),
            code: status.code(),
        })
    }
}

#[derive(Clone)]
pub struct SshHostConnector<R, T = SshProcessTransport>
where
    R: RemoteRunner,
    T: RemoteProcessTransport,
{
    config: SshHostConfig,
    runner: R,
    control_transport: T,
}

impl<R> SshHostConnector<R, SshProcessTransport>
where
    R: RemoteRunner,
{
    pub fn new(config: SshHostConfig, runner: R) -> Self {
        Self {
            config,
            runner,
            control_transport: SshProcessTransport,
        }
    }
}

impl<R, T> SshHostConnector<R, T>
where
    R: RemoteRunner,
    T: RemoteProcessTransport,
{
    pub fn with_control_transport(config: SshHostConfig, runner: R, control_transport: T) -> Self {
        Self {
            config,
            runner,
            control_transport,
        }
    }

    pub fn config(&self) -> &SshHostConfig {
        &self.config
    }

    pub async fn connect_control_surface(&self) -> Result<T::Process> {
        self.control_transport
            .spawn(self.config.control_invocation()?)
            .await
    }
}

#[async_trait]
impl<R, T> HostConnector for SshHostConnector<R, T>
where
    R: RemoteRunner,
    T: RemoteProcessTransport,
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
                agent_backends: vec![singleton_core::COPILOT_BACKEND_ID.to_string()],
                supports_reconnect: true,
                supports_ordered_events: true,
            },
        }
    }

    async fn ensure_workspace(&self, spec: WorkspaceSpec) -> Result<Workspace> {
        self.config.validate()?;
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
                let worktree_path = worktree_path_hint.ok_or_else(|| {
                    SingletonError::InvalidInput(
                        "ssh git_worktree requires worktree_path_hint; ssh host config does not carry remote state directories"
                            .to_string(),
                    )
                })?;
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
        self.config.validate()?;
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

fn validate_ssh_target(target: &str) -> Result<()> {
    if target.trim().is_empty() {
        return Err(SingletonError::InvalidInput(
            "ssh target must not be empty".to_string(),
        ));
    }
    if target != target.trim() {
        return Err(SingletonError::InvalidInput(
            "ssh target must not contain leading or trailing whitespace".to_string(),
        ));
    }
    if target.starts_with('-') {
        return Err(SingletonError::InvalidInput(
            "ssh target must not start with '-'".to_string(),
        ));
    }
    validate_no_control_chars(target, "ssh target")
}

fn validate_ssh_args(ssh_args: &[String]) -> Result<()> {
    for arg in ssh_args {
        validate_no_control_chars(arg, "ssh_args")?;
    }
    Ok(())
}

fn validate_remote_command(command: &str, label: &str) -> Result<()> {
    if command.trim().is_empty() {
        return Err(SingletonError::InvalidInput(format!(
            "{label} must not be empty"
        )));
    }
    validate_no_control_chars(command, label)
}

fn validate_no_control_chars(value: &str, label: &str) -> Result<()> {
    if value
        .bytes()
        .any(|byte| byte == 0 || byte == b'\n' || byte == b'\r')
    {
        return Err(SingletonError::InvalidInput(format!(
            "{label} must not contain NUL or newline characters"
        )));
    }
    Ok(())
}

fn ssh_invocation_target(invocation: &SshInvocation) -> Option<&str> {
    invocation.args.iter().rev().nth(1).map(String::as_str)
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
    use std::collections::VecDeque;
    use std::fs;
    use std::sync::{Arc, Mutex, MutexGuard};

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
        commands: Arc<Mutex<Vec<RecordedRemoteCommand>>>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct RecordedRemoteCommand {
        target: String,
        ssh_args: Vec<String>,
        command: String,
    }

    #[async_trait]
    impl RemoteRunner for RecordingRunner {
        async fn run(
            &self,
            target: &str,
            ssh_args: &[String],
            command: &str,
        ) -> Result<RemoteCommandOutput> {
            test_lock(&self.commands)?.push(RecordedRemoteCommand {
                target: target.to_string(),
                ssh_args: ssh_args.to_vec(),
                command: command.to_string(),
            });
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
        let commands = test_lock(&commands)?;

        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].target, "devbox");
        assert!(commands[0].ssh_args.is_empty());
        assert_eq!(
            commands[0].command,
            "mkdir -p $(dirname '/srv/worktrees/feature-x') && git -C '/srv/repo' worktree add -b 'feature-x' '/srv/worktrees/feature-x' 'main'"
        );
        assert_eq!(
            commands[1].command,
            "git -C '/srv/repo' worktree remove --force '/srv/worktrees/feature-x'"
        );
        Ok(())
    }

    #[tokio::test]
    async fn ssh_git_worktree_requires_explicit_worktree_path_hint() -> Result<()> {
        let runner = RecordingRunner::default();
        let commands = runner.commands.clone();
        let connector = SshHostConnector::new(SshHostConfig::new("host_ssh", "devbox"), runner);

        let result = connector
            .ensure_workspace(WorkspaceSpec::GitWorktree {
                repo: "/srv/repo".to_string(),
                base_ref: Some("main".to_string()),
                branch: Some("feature-x".to_string()),
                create_branch: Some(true),
                worktree_path_hint: None,
                host_id: None,
                cleanup_policy: Some(CleanupPolicy::Keep),
            })
            .await;

        assert!(matches!(
            result,
            Err(SingletonError::InvalidInput(message))
                if message.contains("requires worktree_path_hint")
        ));
        assert!(test_lock(&commands)?.is_empty());
        Ok(())
    }

    #[test]
    fn ssh_control_invocation_uses_default_connect_command() -> Result<()> {
        let invocation = SshHostConfig::new("host_ssh", "devbox").control_invocation()?;

        assert_eq!(invocation.program, "ssh");
        assert_eq!(
            invocation.args,
            vec![
                "devbox".to_string(),
                DEFAULT_SSH_CONNECT_COMMAND.to_string()
            ]
        );
        Ok(())
    }

    #[test]
    fn ssh_control_invocation_preserves_optional_ssh_args() -> Result<()> {
        let invocation = SshHostConfig::new("host_ssh", "devbox")
            .with_ssh_args(["-p", "2222", "-o", "BatchMode=yes"])
            .control_invocation()?;

        assert_eq!(
            invocation.args,
            vec![
                "-p".to_string(),
                "2222".to_string(),
                "-o".to_string(),
                "BatchMode=yes".to_string(),
                "devbox".to_string(),
                DEFAULT_SSH_CONNECT_COMMAND.to_string(),
            ]
        );
        Ok(())
    }

    #[test]
    fn ssh_control_invocation_keeps_connect_command_as_single_argument() -> Result<()> {
        let command = "singleton serve --stdio --backend fake; echo trusted user command";
        let invocation = SshHostConfig::new("host_ssh", "devbox")
            .with_connect_command(command)
            .control_invocation()?;

        assert_eq!(
            invocation.args,
            vec!["devbox".to_string(), command.to_string()]
        );
        Ok(())
    }

    #[test]
    fn ssh_control_invocation_rejects_unsafe_target() {
        let result = SshHostConfig::new("host_ssh", "-oProxyCommand=sh").control_invocation();

        assert!(matches!(
            result,
            Err(SingletonError::InvalidInput(message))
                if message.contains("must not start with '-'")
        ));
    }

    #[test]
    fn ssh_control_invocation_rejects_newlines_in_ssh_args() {
        let result = SshHostConfig::new("host_ssh", "devbox")
            .with_ssh_args(["-o", "BatchMode=yes\nProxyCommand=sh"])
            .control_invocation();

        assert!(matches!(
            result,
            Err(SingletonError::InvalidInput(message))
                if message.contains("ssh_args must not contain")
        ));
    }

    #[test]
    fn project_config_cannot_override_connect_command() {
        let result = SshHostConfig::from_project_config("host_ssh", "devbox")
            .with_connect_command("singleton serve --stdio --backend fake")
            .control_invocation();

        assert!(matches!(
            result,
            Err(SingletonError::InvalidInput(message))
                if message.contains("project ssh host config cannot override connect_command")
        ));
    }

    #[test]
    fn project_config_can_use_default_connect_command() -> Result<()> {
        let invocation =
            SshHostConfig::from_project_config("host_ssh", "devbox").control_invocation()?;

        assert_eq!(
            invocation.args,
            vec![
                "devbox".to_string(),
                DEFAULT_SSH_CONNECT_COMMAND.to_string()
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn ssh_control_surface_uses_injected_stdio_transport_for_fake_mcp() -> Result<()> {
        let response = br#"{"jsonrpc":"2.0","id":1,"result":{"session_id":"sess_fake"}}"#.to_vec();
        let transport = ScriptedTransport::with_responses([add_newline(response)]);
        let invocations = transport.invocations.clone();
        let writes = transport.writes.clone();
        let connector = SshHostConnector::with_control_transport(
            SshHostConfig::new("host_ssh", "devbox").with_ssh_args(["-o", "BatchMode=yes"]),
            RecordingRunner::default(),
            transport,
        );

        let mut process = connector.connect_control_surface().await?;
        let request =
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"create_session"}}"#;
        process.write_stdin(&add_newline(request.to_vec())).await?;
        let output = process
            .read_stdout_line()
            .await?
            .ok_or_else(|| SingletonError::Host {
                host: "ssh_test".to_string(),
                message: "fake mcp process returned no output".to_string(),
            })?;
        process.close_stdin().await?;
        let exit = process.wait().await?;

        assert!(exit.success);
        assert_eq!(
            String::from_utf8_lossy(&output),
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"session_id\":\"sess_fake\"}}\n"
        );
        let invocations = test_lock(&invocations)?;
        assert_eq!(invocations.len(), 1);
        assert_eq!(
            invocations[0].args,
            vec![
                "-o".to_string(),
                "BatchMode=yes".to_string(),
                "devbox".to_string(),
                DEFAULT_SSH_CONNECT_COMMAND.to_string(),
            ]
        );
        let writes = test_lock(&writes)?;
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0], add_newline(request.to_vec()));
        Ok(())
    }

    #[test]
    fn shell_quote_handles_single_quotes() {
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
    }

    #[derive(Clone, Default)]
    struct ScriptedTransport {
        invocations: Arc<Mutex<Vec<SshInvocation>>>,
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
        responses: Arc<Mutex<VecDeque<Vec<u8>>>>,
    }

    impl ScriptedTransport {
        fn with_responses<I>(responses: I) -> Self
        where
            I: IntoIterator<Item = Vec<u8>>,
        {
            Self {
                responses: Arc::new(Mutex::new(responses.into_iter().collect())),
                ..Self::default()
            }
        }
    }

    #[async_trait]
    impl RemoteProcessTransport for ScriptedTransport {
        type Process = ScriptedProcess;

        async fn spawn(&self, invocation: SshInvocation) -> Result<Self::Process> {
            test_lock(&self.invocations)?.push(invocation);
            Ok(ScriptedProcess {
                writes: self.writes.clone(),
                responses: self.responses.clone(),
                closed: false,
            })
        }
    }

    struct ScriptedProcess {
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
        responses: Arc<Mutex<VecDeque<Vec<u8>>>>,
        closed: bool,
    }

    #[async_trait]
    impl RemoteStdioProcess for ScriptedProcess {
        async fn write_stdin(&mut self, bytes: &[u8]) -> Result<()> {
            if self.closed {
                return Err(SingletonError::Host {
                    host: "ssh_test".to_string(),
                    message: "scripted process stdin closed".to_string(),
                });
            }
            test_lock(&self.writes)?.push(bytes.to_vec());
            Ok(())
        }

        async fn read_stdout_line(&mut self) -> Result<Option<Vec<u8>>> {
            Ok(test_lock(&self.responses)?.pop_front())
        }

        async fn close_stdin(&mut self) -> Result<()> {
            self.closed = true;
            Ok(())
        }

        async fn wait(&mut self) -> Result<RemoteProcessExit> {
            Ok(RemoteProcessExit {
                success: true,
                code: Some(0),
            })
        }
    }

    fn add_newline(mut bytes: Vec<u8>) -> Vec<u8> {
        bytes.push(b'\n');
        bytes
    }

    fn test_lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>> {
        mutex.lock().map_err(|_| SingletonError::Host {
            host: "ssh_test".to_string(),
            message: "test lock poisoned".to_string(),
        })
    }
}
