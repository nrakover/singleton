use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};

use singleton_core::{Result, SingletonError};

const LATEST_RELEASE_BASE_URL: &str =
    "https://github.com/nrakover/singleton/releases/latest/download";
const VERSIONED_RELEASE_BASE_URL: &str = "https://github.com/nrakover/singleton/releases/download";

#[derive(Debug, Clone)]
pub(crate) struct UpdateOptions {
    pub(crate) version: Option<String>,
    pub(crate) install_dir: Option<PathBuf>,
    pub(crate) release_base_url: Option<String>,
    pub(crate) dry_run: bool,
    pub(crate) force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpdatePlan {
    pub(crate) target: PathBuf,
    pub(crate) target_triple: String,
    pub(crate) archive: String,
    pub(crate) checksum: String,
    pub(crate) archive_url: String,
    pub(crate) checksum_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UpdateOutcome {
    DryRun(UpdatePlan),
    UpToDate {
        target: PathBuf,
        version: String,
    },
    Updated {
        target: PathBuf,
        previous_version: Option<String>,
        version: String,
    },
}

pub(crate) fn run(options: UpdateOptions) -> Result<UpdateOutcome> {
    let plan = plan_update(&options)?;
    if options.dry_run {
        return Ok(UpdateOutcome::DryRun(plan));
    }

    require_command("curl")?;
    require_command("tar")?;
    let checksum = checksum_tool()?;
    let temp_dir = tempfile::tempdir()
        .map_err(|error| SingletonError::Store(format!("create update tempdir: {error}")))?;
    let archive_path = temp_dir.path().join(&plan.archive);
    let checksum_path = temp_dir.path().join(&plan.checksum);

    download_file(&plan.archive_url, &archive_path)?;
    download_file(&plan.checksum_url, &checksum_path)?;
    verify_checksum(checksum, temp_dir.path(), &plan.checksum)?;
    extract_archive(&archive_path, temp_dir.path())?;
    let candidate = find_singleton_binary(temp_dir.path())?;
    let candidate_version = binary_version(&candidate)?;
    let previous_version = if plan.target.exists() {
        Some(binary_version(&plan.target)?)
    } else {
        None
    };

    if previous_version.as_deref() == Some(candidate_version.as_str()) && !options.force {
        return Ok(UpdateOutcome::UpToDate {
            target: plan.target,
            version: candidate_version,
        });
    }

    install_candidate_binary(&candidate, &plan.target)?;
    Ok(UpdateOutcome::Updated {
        target: plan.target,
        previous_version,
        version: candidate_version,
    })
}

pub(crate) fn plan_update(options: &UpdateOptions) -> Result<UpdatePlan> {
    let target_triple = current_target_triple()?;
    let archive = format!("singleton-{target_triple}.tar.gz");
    let checksum = format!("{archive}.sha256");
    let base_url = release_base_url(
        options.version.as_deref(),
        options.release_base_url.as_deref(),
    );
    Ok(UpdatePlan {
        target: update_target_path(options.install_dir.as_deref())?,
        target_triple,
        archive_url: format!("{}/{}", base_url.trim_end_matches('/'), archive),
        checksum_url: format!("{}/{}", base_url.trim_end_matches('/'), checksum),
        archive,
        checksum,
    })
}

fn release_base_url(version: Option<&str>, release_base_url: Option<&str>) -> String {
    if let Some(release_base_url) = release_base_url {
        return release_base_url.to_string();
    }
    if let Some(version) = version {
        return format!("{VERSIONED_RELEASE_BASE_URL}/{version}");
    }
    LATEST_RELEASE_BASE_URL.to_string()
}

fn update_target_path(install_dir: Option<&Path>) -> Result<PathBuf> {
    if let Some(install_dir) = install_dir {
        return Ok(install_dir.join("singleton"));
    }
    env::current_exe()
        .map_err(|error| SingletonError::InvalidState(format!("locate singleton binary: {error}")))
}

fn current_target_triple() -> Result<String> {
    target_triple_for(env::consts::OS, env::consts::ARCH).map(str::to_string)
}

fn target_triple_for(os: &str, arch: &str) -> Result<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        _ => Err(SingletonError::InvalidInput(format!(
            "unsupported update platform {os}/{arch}; install from source or a manual release archive"
        ))),
    }
}

fn require_command(name: &str) -> Result<()> {
    if command_on_path(name) {
        return Ok(());
    }
    Err(SingletonError::InvalidState(format!(
        "required command '{name}' was not found on PATH"
    )))
}

#[derive(Debug, Clone, Copy)]
enum ChecksumTool {
    Shasum,
    Sha256sum,
}

fn checksum_tool() -> Result<ChecksumTool> {
    if command_on_path("shasum") {
        return Ok(ChecksumTool::Shasum);
    }
    if command_on_path("sha256sum") {
        return Ok(ChecksumTool::Sha256sum);
    }
    Err(SingletonError::InvalidState(
        "neither shasum nor sha256sum is available for checksum verification".to_string(),
    ))
}

fn command_on_path(name: &str) -> bool {
    env::var_os("PATH")
        .map(|path| {
            env::split_paths(&path).any(|directory| {
                let candidate = directory.join(name);
                candidate
                    .metadata()
                    .map(|metadata| {
                        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                    })
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn download_file(url: &str, destination: &Path) -> Result<()> {
    let status = ProcessCommand::new("curl")
        .args(["-fsSL", url, "-o"])
        .arg(destination)
        .status()
        .map_err(|error| SingletonError::InvalidState(format!("download {url}: {error}")))?;
    if !status.success() {
        return Err(SingletonError::InvalidState(format!(
            "download {url} exited with {status}"
        )));
    }
    Ok(())
}

fn verify_checksum(tool: ChecksumTool, directory: &Path, checksum: &str) -> Result<()> {
    let mut command = match tool {
        ChecksumTool::Shasum => {
            let mut command = ProcessCommand::new("shasum");
            command.args(["-a", "256", "-c", checksum]);
            command
        }
        ChecksumTool::Sha256sum => {
            let mut command = ProcessCommand::new("sha256sum");
            command.args(["-c", checksum]);
            command
        }
    };
    let status = command
        .current_dir(directory)
        .status()
        .map_err(|error| SingletonError::InvalidState(format!("verify checksum: {error}")))?;
    if !status.success() {
        return Err(SingletonError::InvalidState(format!(
            "checksum verification exited with {status}"
        )));
    }
    Ok(())
}

fn extract_archive(archive: &Path, directory: &Path) -> Result<()> {
    let status = ProcessCommand::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(directory)
        .status()
        .map_err(|error| {
            SingletonError::InvalidState(format!("extract {}: {error}", archive.display()))
        })?;
    if !status.success() {
        return Err(SingletonError::InvalidState(format!(
            "extract {} exited with {status}",
            archive.display()
        )));
    }
    Ok(())
}

fn find_singleton_binary(directory: &Path) -> Result<PathBuf> {
    let mut stack = vec![directory.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(&path)
            .map_err(|error| SingletonError::Store(format!("read {}: {error}", path.display())))?
        {
            let entry = entry
                .map_err(|error| SingletonError::Store(format!("read directory entry: {error}")))?;
            let path = entry.path();
            let metadata = entry.metadata().map_err(|error| {
                SingletonError::Store(format!("read metadata {}: {error}", path.display()))
            })?;
            if metadata.is_dir() {
                stack.push(path);
            } else if metadata.is_file() && path.file_name().is_some_and(|name| name == "singleton")
            {
                return Ok(path);
            }
        }
    }
    Err(SingletonError::InvalidState(
        "release archive did not contain a singleton binary".to_string(),
    ))
}

fn binary_version(binary: &Path) -> Result<String> {
    let output = ProcessCommand::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            SingletonError::InvalidState(format!("read version from {}: {error}", binary.display()))
        })?;
    if !output.status.success() {
        return Err(SingletonError::InvalidState(format!(
            "{} --version exited with {}",
            binary.display(),
            output.status
        )));
    }
    let stdout = String::from_utf8(output.stdout).map_err(|error| {
        SingletonError::InvalidState(format!(
            "decode {} --version output: {error}",
            binary.display()
        ))
    })?;
    parse_version_output(&stdout)
}

fn parse_version_output(output: &str) -> Result<String> {
    output
        .split_whitespace()
        .last()
        .filter(|version| !version.is_empty())
        .map(str::to_string)
        .ok_or_else(|| SingletonError::InvalidState("empty singleton version output".to_string()))
}

fn install_candidate_binary(candidate: &Path, target: &Path) -> Result<()> {
    let parent = target.parent().ok_or_else(|| {
        SingletonError::InvalidInput(format!(
            "update target {} does not have a parent directory",
            target.display()
        ))
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        SingletonError::Store(format!(
            "create install directory {}: {error}",
            parent.display()
        ))
    })?;
    let file_name = target.file_name().ok_or_else(|| {
        SingletonError::InvalidInput(format!(
            "update target {} has no file name",
            target.display()
        ))
    })?;
    let temp_target = parent.join(format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        std::process::id()
    ));
    let install_result = (|| {
        fs::copy(candidate, &temp_target).map_err(|error| {
            SingletonError::Store(format!(
                "copy {} to {}: {error}",
                candidate.display(),
                temp_target.display()
            ))
        })?;
        fs::set_permissions(&temp_target, fs::Permissions::from_mode(0o755)).map_err(|error| {
            SingletonError::Store(format!(
                "set executable permissions on {}: {error}",
                temp_target.display()
            ))
        })?;
        fs::rename(&temp_target, target).map_err(|error| {
            SingletonError::Store(format!(
                "replace {} with {}: {error}",
                target.display(),
                temp_target.display()
            ))
        })
    })();
    if install_result.is_err() {
        let _ = fs::remove_file(&temp_target);
    }
    install_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use singleton_core::Result;

    #[test]
    fn maps_supported_targets() -> Result<()> {
        assert_eq!(
            target_triple_for("macos", "aarch64")?,
            "aarch64-apple-darwin"
        );
        assert_eq!(
            target_triple_for("linux", "x86_64")?,
            "x86_64-unknown-linux-gnu"
        );
        assert!(target_triple_for("macos", "x86_64").is_err());
        Ok(())
    }

    #[test]
    fn builds_latest_release_plan() -> Result<()> {
        let options = UpdateOptions {
            version: None,
            install_dir: Some(PathBuf::from("/tmp/bin")),
            release_base_url: None,
            dry_run: true,
            force: false,
        };
        let plan = plan_update(&options)?;
        assert_eq!(plan.target, PathBuf::from("/tmp/bin/singleton"));
        assert_eq!(
            plan.archive_url,
            format!("{LATEST_RELEASE_BASE_URL}/{}", plan.archive)
        );
        assert_eq!(
            plan.checksum_url,
            format!("{LATEST_RELEASE_BASE_URL}/{}", plan.checksum)
        );
        Ok(())
    }

    #[test]
    fn builds_versioned_release_plan() -> Result<()> {
        let options = UpdateOptions {
            version: Some("v1.2.3".to_string()),
            install_dir: Some(PathBuf::from("/tmp/bin")),
            release_base_url: None,
            dry_run: true,
            force: false,
        };
        let plan = plan_update(&options)?;
        assert_eq!(
            plan.archive_url,
            format!("{VERSIONED_RELEASE_BASE_URL}/v1.2.3/{}", plan.archive)
        );
        Ok(())
    }

    #[test]
    fn custom_release_base_url_wins() -> Result<()> {
        let options = UpdateOptions {
            version: Some("v1.2.3".to_string()),
            install_dir: Some(PathBuf::from("/tmp/bin")),
            release_base_url: Some("https://example.invalid/releases".to_string()),
            dry_run: true,
            force: false,
        };
        let plan = plan_update(&options)?;
        assert_eq!(
            plan.archive_url,
            format!("https://example.invalid/releases/{}", plan.archive)
        );
        Ok(())
    }

    #[test]
    fn parses_version_output() -> Result<()> {
        assert_eq!(parse_version_output("singleton 1.2.3\n")?, "1.2.3");
        assert_eq!(parse_version_output("singleton-cli 0.4.0")?, "0.4.0");
        assert!(parse_version_output("").is_err());
        Ok(())
    }

    #[test]
    fn reads_executable_version() -> Result<()> {
        let temp = tempfile::tempdir()
            .map_err(|error| SingletonError::Store(format!("create tempdir: {error}")))?;
        let binary = temp.path().join("singleton");
        fs::write(&binary, "#!/usr/bin/env sh\necho singleton 9.8.7\n")
            .map_err(|error| SingletonError::Store(format!("write test binary: {error}")))?;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))
            .map_err(|error| SingletonError::Store(format!("chmod test binary: {error}")))?;
        assert_eq!(binary_version(&binary)?, "9.8.7");
        Ok(())
    }

    #[test]
    fn installs_candidate_atomically() -> Result<()> {
        let temp = tempfile::tempdir()
            .map_err(|error| SingletonError::Store(format!("create tempdir: {error}")))?;
        let candidate = temp.path().join("candidate");
        let target = temp.path().join("bin").join("singleton");
        fs::write(&candidate, "new binary")
            .map_err(|error| SingletonError::Store(format!("write candidate: {error}")))?;
        install_candidate_binary(&candidate, &target)?;
        assert_eq!(
            fs::read_to_string(&target)
                .map_err(|error| SingletonError::Store(format!("read target: {error}")))?,
            "new binary"
        );
        assert_eq!(
            fs::metadata(&target)
                .map_err(|error| SingletonError::Store(format!("stat target: {error}")))?
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        Ok(())
    }
}
