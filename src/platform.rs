use std::path::Path;
use std::process::Command;

use thiserror::Error;

use crate::config::{ServiceManager, SupportedPlatform};

pub trait PlatformProbe {
    fn current_platform(&self) -> Result<SupportedPlatform, PlatformProbeError>;
    fn filesystem_type(&self, path: &Path) -> Result<String, PlatformProbeError>;
    fn service_manager_supported(
        &self,
        platform: SupportedPlatform,
        service_manager: ServiceManager,
    ) -> bool;
}

#[derive(Debug, Default)]
pub struct RealPlatformProbe;

impl PlatformProbe for RealPlatformProbe {
    fn current_platform(&self) -> Result<SupportedPlatform, PlatformProbeError> {
        match std::env::consts::OS {
            "macos" => Ok(SupportedPlatform::Macos),
            "linux" => Ok(SupportedPlatform::Linux),
            unsupported => Err(PlatformProbeError::UnsupportedPlatform(
                unsupported.to_owned(),
            )),
        }
    }

    fn filesystem_type(&self, path: &Path) -> Result<String, PlatformProbeError> {
        let (program, args) = match std::env::consts::OS {
            "macos" => ("/usr/bin/stat", vec!["-f", "%T"]),
            "linux" => ("stat", vec!["-f", "-c", "%T"]),
            unsupported => {
                return Err(PlatformProbeError::UnsupportedPlatform(
                    unsupported.to_owned(),
                ))
            }
        };

        let output = Command::new(program)
            .args(args)
            .arg(path)
            .output()
            .map_err(|error| PlatformProbeError::Spawn {
                program: program.to_owned(),
                error,
            })?;
        if !output.status.success() {
            return Err(PlatformProbeError::CommandFailed {
                program: program.to_owned(),
                path: path.to_path_buf(),
                status: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn service_manager_supported(
        &self,
        platform: SupportedPlatform,
        service_manager: ServiceManager,
    ) -> bool {
        matches!(
            (platform, service_manager),
            (SupportedPlatform::Macos, ServiceManager::Launchd)
                | (SupportedPlatform::Linux, ServiceManager::Systemd)
        )
    }
}

#[derive(Debug, Error)]
pub enum PlatformProbeError {
    #[error("unsupported platform {0}; only macOS and Linux are supported")]
    UnsupportedPlatform(String),
    #[error("failed to spawn {program}: {error}")]
    Spawn {
        program: String,
        #[source]
        error: std::io::Error,
    },
    #[error(
        "{program} failed for {path} with status {status:?}: {stderr}",
        path = path.display()
    )]
    CommandFailed {
        program: String,
        path: std::path::PathBuf,
        status: Option<i32>,
        stderr: String,
    },
}
