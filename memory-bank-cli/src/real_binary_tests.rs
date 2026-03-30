use crate::AppError;
use crate::assets::{find_on_path, install_embedded_assets};
use crate::command_utils::{CommandOutcome, CommandRunOptions, run_command_capture_with_options};
use crate::constants::{HOOK_BINARY_NAME, MCP_PROXY_BINARY_NAME};
use memory_bank_app::AppPaths;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::TempDir;

const REAL_BINARY_TEST_ENV: &str = "MEMORY_BANK_REAL_BIN_TESTS";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) struct RealBinaryTestEnv {
    _home: TempDir,
    _cwd: TempDir,
    pub(crate) paths: AppPaths,
    cwd: PathBuf,
}

impl RealBinaryTestEnv {
    pub(crate) fn for_binary(binary: &str) -> Option<Self> {
        if std::env::var(REAL_BINARY_TEST_ENV).ok().as_deref() != Some("1") {
            eprintln!(
                "skipping real-binary test for `{binary}` because {REAL_BINARY_TEST_ENV}=1 is not set"
            );
            return None;
        }

        if find_on_path(binary).is_none() {
            eprintln!("skipping real-binary test for `{binary}` because it is not installed");
            return None;
        }

        Some(Self::new().expect("real binary test environment"))
    }

    fn new() -> Result<Self, AppError> {
        let home = TempDir::new()?;
        let cwd = TempDir::new()?;
        let paths = AppPaths::from_home_dir(home.path().to_path_buf());
        paths.ensure_base_dirs()?;
        fs::create_dir_all(home.path().join(".config"))?;
        fs::create_dir_all(home.path().join(".local/state"))?;
        fs::create_dir_all(home.path().join(".local/share"))?;

        Ok(Self {
            paths,
            cwd: cwd.path().to_path_buf(),
            _home: home,
            _cwd: cwd,
        })
    }

    pub(crate) fn seed_agent_artifacts(&self) -> Result<(), AppError> {
        install_embedded_assets(&self.paths)?;
        for binary in [HOOK_BINARY_NAME, MCP_PROXY_BINARY_NAME] {
            self.write_stub_binary(&self.paths.binary_path(binary))?;
        }
        Ok(())
    }

    pub(crate) fn seed_gemini_home(&self) -> Result<(), AppError> {
        fs::create_dir_all(self.paths.home_dir.join(".gemini"))?;
        Ok(())
    }

    fn write_stub_binary(&self, path: &Path) -> Result<(), AppError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, "#!/bin/sh\nexit 0\n")?;
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(path)?.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions)?;
        }
        Ok(())
    }

    pub(crate) fn command_options(&self) -> CommandRunOptions {
        CommandRunOptions::default()
            .with_cwd(&self.cwd)
            .with_env("HOME", self.paths.home_dir.to_string_lossy().to_string())
            .with_env(
                "XDG_CONFIG_HOME",
                self.paths
                    .home_dir
                    .join(".config")
                    .to_string_lossy()
                    .to_string(),
            )
            .with_env(
                "XDG_STATE_HOME",
                self.paths
                    .home_dir
                    .join(".local/state")
                    .to_string_lossy()
                    .to_string(),
            )
            .with_env(
                "XDG_DATA_HOME",
                self.paths
                    .home_dir
                    .join(".local/share")
                    .to_string_lossy()
                    .to_string(),
            )
            .with_env("NO_COLOR", "1")
            .with_env("CI", "1")
            .with_env("TERM", "dumb")
            .with_removed_env("OPENCLAW_STATE_DIR")
            .with_removed_env("OPENCLAW_CONFIG_PATH")
            .with_removed_env("OPENCLAW_CONTAINER")
            .with_timeout(DEFAULT_TIMEOUT)
    }

    pub(crate) fn run_cli(&self, program: &str, args: &[&str]) -> Result<CommandOutcome, AppError> {
        run_command_capture_with_options(program, args, &self.command_options())
    }
}
