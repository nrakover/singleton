use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use singleton_core::{
    CleanupPolicy, CleanupSummary, CloseDisposition, Host, HostConnector, RepoMetadata,
    ResourceKind, ResourceStatus, Result, SingletonError, Workspace, WorkspaceSpec, new_id,
    now_rfc3339, resource_uri,
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
}
