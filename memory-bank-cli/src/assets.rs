use crate::AppError;
use crate::constants::{
    EMBEDDED_MODEL_CATALOG, EMBEDDED_OPENCLAW_INDEX, EMBEDDED_OPENCLAW_MANIFEST,
    EMBEDDED_OPENCLAW_PACKAGE, EMBEDDED_OPENCODE_PLUGIN, HOOK_BINARY_NAME, MB_BINARY_NAME,
    MCP_PROXY_BINARY_NAME, SERVER_BINARY_NAME,
};
use memory_bank_app::AppPaths;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub(crate) fn materialize_install_artifacts(paths: &AppPaths) -> Result<(), AppError> {
    paths.ensure_base_dirs()?;
    let current_exe = std::env::current_exe()?;
    copy_if_needed(&current_exe, &paths.binary_path(MB_BINARY_NAME))?;

    let executable_dir = current_exe.parent().ok_or_else(|| {
        AppError::Message("failed to resolve current executable directory".to_string())
    })?;
    for binary in [SERVER_BINARY_NAME, HOOK_BINARY_NAME, MCP_PROXY_BINARY_NAME] {
        let source = executable_dir.join(binary);
        let target = paths.binary_path(binary);
        if source.exists() {
            copy_if_needed(&source, &target)?;
        } else if !target.exists() {
            return Err(AppError::MissingBinary(binary.to_string()));
        }
    }

    install_assets(paths)?;
    Ok(())
}

pub(crate) fn ensure_path_entry(paths: &AppPaths) -> Result<(), AppError> {
    let shell = std::env::var("SHELL").unwrap_or_default();
    let target_rc = if shell.ends_with("zsh") {
        paths.home_dir.join(".zshrc")
    } else if shell.ends_with("bash") {
        paths.home_dir.join(".bashrc")
    } else {
        paths.home_dir.join(".profile")
    };
    let export_line = r#"export PATH="$HOME/.memory_bank/bin:$PATH""#;
    let existing = fs::read_to_string(&target_rc).unwrap_or_default();
    if existing.contains(".memory_bank/bin") {
        return Ok(());
    }

    let mut updated = existing;
    if !updated.ends_with('\n') && !updated.is_empty() {
        updated.push('\n');
    }
    updated.push_str("# Memory Bank\n");
    updated.push_str(export_line);
    updated.push('\n');
    fs::write(target_rc, updated)?;
    Ok(())
}

pub(crate) fn find_on_path(binary: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for entry in std::env::split_paths(&path_var) {
        let candidate = entry.join(binary);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

pub(crate) fn find_repo_root() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(current_dir) = std::env::current_dir() {
        candidates.push(current_dir);
    }

    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf);
    if let Some(root) = workspace_root {
        candidates.push(root);
    }

    for candidate in candidates {
        let mut current = Some(candidate.as_path());
        while let Some(path) = current {
            if path.join(".opencode/plugins/memory-bank.js").exists()
                && path.join(".openclaw/extensions/memory-bank").exists()
            {
                return Some(path.to_path_buf());
            }
            current = path.parent();
        }
    }

    None
}

pub(crate) fn assets_are_installed(paths: &AppPaths) -> bool {
    paths
        .integrations_dir
        .join("opencode")
        .join("memory-bank.js")
        .is_file()
        && paths
            .integrations_dir
            .join("openclaw")
            .join("memory-bank")
            .join("index.js")
            .is_file()
        && paths
            .integrations_dir
            .join("openclaw")
            .join("memory-bank")
            .join("openclaw.plugin.json")
            .is_file()
        && paths
            .integrations_dir
            .join("openclaw")
            .join("memory-bank")
            .join("package.json")
            .is_file()
        && paths.model_catalog_file.is_file()
}

pub(crate) fn install_embedded_assets(paths: &AppPaths) -> Result<(), AppError> {
    write_asset_file(
        &paths
            .integrations_dir
            .join("opencode")
            .join("memory-bank.js"),
        EMBEDDED_OPENCODE_PLUGIN,
    )?;

    let openclaw_target = paths.integrations_dir.join("openclaw").join("memory-bank");
    write_asset_file(&openclaw_target.join("index.js"), EMBEDDED_OPENCLAW_INDEX)?;
    write_asset_file(
        &openclaw_target.join("openclaw.plugin.json"),
        EMBEDDED_OPENCLAW_MANIFEST,
    )?;
    write_asset_file(
        &openclaw_target.join("package.json"),
        EMBEDDED_OPENCLAW_PACKAGE,
    )?;
    write_asset_file(&paths.model_catalog_file, EMBEDDED_MODEL_CATALOG)?;
    Ok(())
}

fn install_assets(paths: &AppPaths) -> Result<(), AppError> {
    if assets_are_installed(paths) {
        return Ok(());
    }

    if let Some(repo_root) = find_repo_root()
        && install_repo_assets(paths, &repo_root).is_ok()
    {
        return Ok(());
    }

    install_embedded_assets(paths)
}

fn install_repo_assets(paths: &AppPaths, repo_root: &Path) -> Result<(), AppError> {
    let opencode_target = paths
        .integrations_dir
        .join("opencode")
        .join("memory-bank.js");
    let openclaw_target = paths.integrations_dir.join("openclaw").join("memory-bank");
    let model_catalog_target = &paths.model_catalog_file;
    let opencode_source = repo_root.join(".opencode/plugins/memory-bank.js");
    let openclaw_source = repo_root.join(".openclaw/extensions/memory-bank");
    let model_catalog_source = repo_root.join("config/setup-model-catalog.json");

    if !opencode_source.exists() || !openclaw_source.exists() || !model_catalog_source.exists() {
        return Err(AppError::Message(
            "repo asset sources for installation are missing".to_string(),
        ));
    }

    copy_if_needed(&opencode_source, &opencode_target)?;
    copy_dir_recursive(&openclaw_source, &openclaw_target)?;
    copy_if_needed(&model_catalog_source, model_catalog_target)?;
    Ok(())
}

pub(crate) fn copy_if_needed(source: &Path, target: &Path) -> Result<(), AppError> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }

    if source == target {
        return Ok(());
    }

    fs::copy(source, target)?;
    let permissions = source.metadata()?.permissions();
    fs::set_permissions(target, permissions)?;
    Ok(())
}

fn copy_dir_recursive(source: &Path, target: &Path) -> Result<(), AppError> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        if source_path.is_dir() {
            copy_dir_recursive(&source_path, &target_path)?;
        } else {
            copy_if_needed(&source_path, &target_path)?;
        }
    }
    Ok(())
}

fn write_asset_file(path: &Path, contents: &str) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(path, contents)?;
    Ok(())
}

fn is_executable_file(path: &Path) -> bool {
    path.is_file()
        && path
            .metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn ensure_path_entry_is_idempotent_for_current_shell_rc_file() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());

        ensure_path_entry(&paths).expect("first path update");
        ensure_path_entry(&paths).expect("second path update");

        let shell = std::env::var("SHELL").unwrap_or_default();
        let rc_path = if shell.ends_with("zsh") {
            paths.home_dir.join(".zshrc")
        } else if shell.ends_with("bash") {
            paths.home_dir.join(".bashrc")
        } else {
            paths.home_dir.join(".profile")
        };
        let contents = fs::read_to_string(&rc_path).expect("rc file");

        assert_eq!(contents.matches("# Memory Bank").count(), 1);
        assert_eq!(contents.matches(".memory_bank/bin").count(), 1);
    }

    #[test]
    fn copy_if_needed_is_a_no_op_for_same_path() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join("file.txt");
        fs::write(&path, "hello").expect("write file");

        copy_if_needed(&path, &path).expect("copy");

        assert_eq!(fs::read_to_string(&path).expect("read file"), "hello");
    }

    #[test]
    fn install_embedded_assets_writes_complete_bundle() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());

        install_embedded_assets(&paths).expect("install embedded assets");

        assert!(assets_are_installed(&paths));
        assert_eq!(
            fs::read_to_string(paths.integrations_dir.join("opencode/memory-bank.js"))
                .expect("opencode plugin"),
            EMBEDDED_OPENCODE_PLUGIN
        );
        assert_eq!(
            fs::read_to_string(paths.model_catalog_file).expect("model catalog"),
            EMBEDDED_MODEL_CATALOG
        );
    }

    #[test]
    fn assets_are_not_reported_installed_when_bundle_is_incomplete() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        fs::create_dir_all(paths.integrations_dir.join("opencode")).expect("opencode dir");
        fs::write(
            paths.integrations_dir.join("opencode/memory-bank.js"),
            "plugin",
        )
        .expect("plugin");

        assert!(!assets_are_installed(&paths));
    }
}
