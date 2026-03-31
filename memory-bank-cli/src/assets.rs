use crate::AppError;
use crate::constants::{
    EMBEDDED_MODEL_CATALOG, EMBEDDED_OPENCLAW_INDEX, EMBEDDED_OPENCLAW_MANIFEST,
    EMBEDDED_OPENCLAW_PACKAGE, EMBEDDED_OPENCODE_PLUGIN, HOOK_BINARY_NAME, MB_BINARY_NAME,
    MCP_PROXY_BINARY_NAME, SERVER_BINARY_NAME,
};
use memory_bank_app::AppPaths;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Component;
use std::path::{Path, PathBuf};

const MANAGED_LAUNCHER_MARKER: &str = "# Memory Bank managed launcher";
const MANAGED_LAUNCHER_EXEC_LINE: &str = r#"exec "$HOME/.memory_bank/bin/mb" "$@""#;
const MANAGED_ENV_MARKER: &str = "# Memory Bank managed environment";
const RC_BLOCK_START: &str = "# >>> Memory Bank >>>";
const RC_BLOCK_END: &str = "# <<< Memory Bank <<<";
const RC_SOURCE_LINE: &str =
    r#"[ -f "$HOME/.memory_bank/env.sh" ] && . "$HOME/.memory_bank/env.sh""#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExposureMode {
    Direct,
    Launcher,
    ShellInitFallback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExposureOutcome {
    pub(crate) mode: ExposureMode,
    pub(crate) bare_command_works_now: bool,
    pub(crate) command_prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExposureCheck {
    Active(ExposureOutcome),
    Missing,
    Collision(PathBuf),
}

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

pub(crate) fn materialize_and_expose_cli(paths: &AppPaths) -> Result<ExposureOutcome, AppError> {
    materialize_install_artifacts(paths)?;
    ensure_cli_exposure(paths)
}

pub(crate) fn ensure_cli_exposure(paths: &AppPaths) -> Result<ExposureOutcome, AppError> {
    let path_entries = current_path_entries();
    let shell = current_shell();
    ensure_cli_exposure_with_context(paths, &path_entries, &shell)
}

pub(crate) fn inspect_cli_exposure(paths: &AppPaths) -> Result<ExposureCheck, AppError> {
    let path_entries = current_path_entries();
    let shell = current_shell();
    inspect_cli_exposure_with_context(paths, &path_entries, &shell)
}

pub(crate) fn find_on_path(binary: &str) -> Option<PathBuf> {
    which::which(binary).ok()
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

fn ensure_cli_exposure_with_context(
    paths: &AppPaths,
    path_entries: &[PathBuf],
    shell: &str,
) -> Result<ExposureOutcome, AppError> {
    let resolved_mb = find_on_entries(path_entries, MB_BINARY_NAME);
    let real_binary = paths.binary_path(MB_BINARY_NAME);

    if let Some(path) = resolved_mb {
        if same_path(&path, &real_binary) {
            return Ok(direct_exposure(paths));
        }
        if is_managed_launcher(&path, paths) {
            return Ok(launcher_exposure(paths));
        }
        return Err(cli_collision_error(&path));
    }

    if is_path_entry_present(path_entries, &paths.bin_dir) {
        return Ok(direct_exposure(paths));
    }

    if let Some(launcher_dir) = first_launcher_dir(paths, path_entries) {
        install_managed_launcher(&launcher_dir.join(MB_BINARY_NAME), paths)?;
        return Ok(launcher_exposure(paths));
    }

    install_shell_init_fallback(paths, shell)?;
    Ok(shell_init_exposure(paths))
}

fn inspect_cli_exposure_with_context(
    paths: &AppPaths,
    path_entries: &[PathBuf],
    shell: &str,
) -> Result<ExposureCheck, AppError> {
    let resolved_mb = find_on_entries(path_entries, MB_BINARY_NAME);
    let real_binary = paths.binary_path(MB_BINARY_NAME);

    if let Some(path) = resolved_mb {
        if same_path(&path, &real_binary) {
            return Ok(ExposureCheck::Active(direct_exposure(paths)));
        }
        if is_managed_launcher(&path, paths) {
            return Ok(ExposureCheck::Active(launcher_exposure(paths)));
        }
        return Ok(ExposureCheck::Collision(path));
    }

    if is_path_entry_present(path_entries, &paths.bin_dir) && is_executable_file(&real_binary) {
        return Ok(ExposureCheck::Active(direct_exposure(paths)));
    }

    if shell_init_fallback_is_managed(paths, shell)? {
        return Ok(ExposureCheck::Active(shell_init_exposure(paths)));
    }

    Ok(ExposureCheck::Missing)
}

fn direct_exposure(_paths: &AppPaths) -> ExposureOutcome {
    ExposureOutcome {
        mode: ExposureMode::Direct,
        bare_command_works_now: true,
        command_prefix: MB_BINARY_NAME.to_string(),
    }
}

fn launcher_exposure(_paths: &AppPaths) -> ExposureOutcome {
    ExposureOutcome {
        mode: ExposureMode::Launcher,
        bare_command_works_now: true,
        command_prefix: MB_BINARY_NAME.to_string(),
    }
}

fn shell_init_exposure(paths: &AppPaths) -> ExposureOutcome {
    ExposureOutcome {
        mode: ExposureMode::ShellInitFallback,
        bare_command_works_now: false,
        command_prefix: paths.binary_path(MB_BINARY_NAME).display().to_string(),
    }
}

fn cli_collision_error(path: &Path) -> AppError {
    AppError::Message(format!(
        "cannot expose `mb` because another executable already exists on PATH at {}",
        path.display()
    ))
}

fn current_path_entries() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default()
}

fn current_shell() -> String {
    std::env::var("SHELL").unwrap_or_default()
}

fn find_on_entries(entries: &[PathBuf], binary: &str) -> Option<PathBuf> {
    for entry in entries {
        let candidate = entry.join(binary);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn first_launcher_dir(paths: &AppPaths, path_entries: &[PathBuf]) -> Option<PathBuf> {
    let local_bin = paths.home_dir.join(".local/bin");
    let home_bin = paths.home_dir.join("bin");

    path_entries.iter().find_map(|entry| {
        if same_path(entry, &local_bin) || same_path(entry, &home_bin) {
            Some(entry.clone())
        } else {
            None
        }
    })
}

fn install_managed_launcher(path: &Path, paths: &AppPaths) -> Result<(), AppError> {
    if path.exists() && !is_managed_launcher(path, paths) {
        return Err(cli_collision_error(path));
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, render_managed_launcher())?;
    set_mode(path, 0o755)?;
    Ok(())
}

fn render_managed_launcher() -> String {
    format!("#!/usr/bin/env sh\n{MANAGED_LAUNCHER_MARKER}\n{MANAGED_LAUNCHER_EXEC_LINE}\n")
}

fn is_managed_launcher(path: &Path, _paths: &AppPaths) -> bool {
    let Ok(contents) = fs::read_to_string(path) else {
        return false;
    };
    contents.contains(MANAGED_LAUNCHER_MARKER)
        && contents.contains(MANAGED_LAUNCHER_EXEC_LINE)
        && path.file_name().and_then(|name| name.to_str()) == Some(MB_BINARY_NAME)
}

fn install_shell_init_fallback(paths: &AppPaths, shell: &str) -> Result<(), AppError> {
    let env_file = shell_env_file(paths);
    if let Some(parent) = env_file.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&env_file, render_managed_env_file())?;

    for target in shell_init_targets_for_shell(paths, shell) {
        ensure_source_block(&target)?;
    }
    Ok(())
}

fn shell_init_fallback_is_managed(paths: &AppPaths, shell: &str) -> Result<bool, AppError> {
    let env_file = shell_env_file(paths);
    if !matches!(fs::read_to_string(&env_file), Ok(contents) if contents == render_managed_env_file())
    {
        return Ok(false);
    }

    for target in shell_init_targets_for_shell(paths, shell) {
        if !has_source_block(&target)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn shell_env_file(paths: &AppPaths) -> PathBuf {
    paths.root.join("env.sh")
}

fn render_managed_env_file() -> String {
    format!(
        "{MANAGED_ENV_MARKER}\ncase \":$PATH:\" in\n  *\":$HOME/.memory_bank/bin:\"*) ;;\n  *) export PATH=\"$HOME/.memory_bank/bin:$PATH\" ;;\nesac\n"
    )
}

fn shell_init_targets_for_shell(paths: &AppPaths, shell: &str) -> Vec<PathBuf> {
    if shell.ends_with("zsh") {
        vec![
            paths.home_dir.join(".zprofile"),
            paths.home_dir.join(".zshrc"),
        ]
    } else if shell.ends_with("bash") {
        vec![bash_login_file(paths), paths.home_dir.join(".bashrc")]
    } else {
        vec![paths.home_dir.join(".profile")]
    }
}

fn bash_login_file(paths: &AppPaths) -> PathBuf {
    for candidate in [
        paths.home_dir.join(".bash_profile"),
        paths.home_dir.join(".bash_login"),
        paths.home_dir.join(".profile"),
    ] {
        if candidate.exists() {
            return candidate;
        }
    }
    paths.home_dir.join(".bash_profile")
}

fn ensure_source_block(path: &Path) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let block = render_source_block();
    let existing = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(AppError::Io(error)),
    };
    if existing.contains(&block) {
        return Ok(());
    }

    let updated = replace_or_append_block(&existing, &block);
    fs::write(path, updated)?;
    Ok(())
}

fn has_source_block(path: &Path) -> Result<bool, AppError> {
    Ok(fs::read_to_string(path)
        .map(|contents| contents.contains(&render_source_block()))
        .unwrap_or(false))
}

fn render_source_block() -> String {
    format!("{RC_BLOCK_START}\n{RC_SOURCE_LINE}\n{RC_BLOCK_END}\n")
}

fn replace_or_append_block(existing: &str, block: &str) -> String {
    if let Some(start) = existing.find(RC_BLOCK_START)
        && let Some(end_offset) = existing[start..].find(RC_BLOCK_END)
    {
        let end = start + end_offset + RC_BLOCK_END.len();
        let mut updated = String::new();
        updated.push_str(&existing[..start]);
        if !updated.ends_with('\n') && !updated.is_empty() {
            updated.push('\n');
        }
        updated.push_str(block);
        let suffix = &existing[end..];
        if !suffix.is_empty() && !suffix.starts_with('\n') {
            updated.push('\n');
        }
        updated.push_str(suffix.trim_start_matches('\n'));
        if !updated.ends_with('\n') {
            updated.push('\n');
        }
        return updated;
    }

    let mut updated = existing.to_string();
    if !updated.ends_with('\n') && !updated.is_empty() {
        updated.push('\n');
    }
    updated.push_str(block);
    updated
}

fn is_path_entry_present(entries: &[PathBuf], target: &Path) -> bool {
    entries.iter().any(|entry| same_path(entry, target))
}

fn same_path(left: &Path, right: &Path) -> bool {
    normalize_components(left).eq(normalize_components(right))
}

fn normalize_components(path: &Path) -> impl Iterator<Item = Component<'_>> {
    path.components()
        .filter(|component| *component != Component::CurDir)
}

fn set_mode(path: &Path, mode: u32) -> Result<(), AppError> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)?;
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
    fn ensure_cli_exposure_uses_direct_mode_when_app_bin_is_on_path() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        paths.ensure_base_dirs().expect("base dirs");
        fs::write(paths.binary_path(MB_BINARY_NAME), "#!/bin/sh\n").expect("write mb");
        set_mode(&paths.binary_path(MB_BINARY_NAME), 0o755).expect("chmod");

        let outcome = ensure_cli_exposure_with_context(
            &paths,
            std::slice::from_ref(&paths.bin_dir),
            "/bin/zsh",
        )
        .expect("direct exposure");

        assert_eq!(outcome.mode, ExposureMode::Direct);
        assert!(outcome.bare_command_works_now);
        assert_eq!(outcome.command_prefix, "mb");
    }

    #[test]
    fn ensure_cli_exposure_creates_managed_launcher_in_first_path_match() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());

        let launcher_dir = paths.home_dir.join(".local/bin");
        let other_dir = paths.home_dir.join("bin");
        let outcome = ensure_cli_exposure_with_context(
            &paths,
            &[launcher_dir.clone(), other_dir],
            "/bin/bash",
        )
        .expect("launcher exposure");

        assert_eq!(outcome.mode, ExposureMode::Launcher);
        assert!(outcome.bare_command_works_now);
        assert_eq!(
            fs::read_to_string(launcher_dir.join(MB_BINARY_NAME)).expect("launcher"),
            render_managed_launcher()
        );
    }

    #[test]
    fn ensure_cli_exposure_rejects_unrelated_mb_collision() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let collision_dir = paths.home_dir.join(".local/bin");
        fs::create_dir_all(&collision_dir).expect("collision dir");
        let collision_path = collision_dir.join(MB_BINARY_NAME);
        fs::write(&collision_path, "#!/bin/sh\nexit 0\n").expect("write collision");
        set_mode(&collision_path, 0o755).expect("chmod");

        let error = ensure_cli_exposure_with_context(&paths, &[collision_dir], "/bin/bash")
            .expect_err("collision");

        assert!(
            error
                .to_string()
                .contains("another executable already exists on PATH")
        );
    }

    #[test]
    fn ensure_cli_exposure_falls_back_to_managed_shell_init() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());

        let outcome =
            ensure_cli_exposure_with_context(&paths, &[PathBuf::from("/usr/bin")], "/bin/zsh")
                .expect("shell init fallback");

        assert_eq!(outcome.mode, ExposureMode::ShellInitFallback);
        assert!(!outcome.bare_command_works_now);
        assert_eq!(
            outcome.command_prefix,
            paths.binary_path(MB_BINARY_NAME).display().to_string()
        );
        assert_eq!(
            fs::read_to_string(shell_env_file(&paths)).expect("env file"),
            render_managed_env_file()
        );
        assert!(has_source_block(&paths.home_dir.join(".zprofile")).expect("zprofile"));
        assert!(has_source_block(&paths.home_dir.join(".zshrc")).expect("zshrc"));
    }

    #[test]
    fn shell_init_fallback_is_idempotent() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());

        install_shell_init_fallback(&paths, "/bin/bash").expect("first fallback");
        install_shell_init_fallback(&paths, "/bin/bash").expect("second fallback");

        let bashrc = fs::read_to_string(paths.home_dir.join(".bashrc")).expect("bashrc");
        let bash_profile =
            fs::read_to_string(paths.home_dir.join(".bash_profile")).expect("bash profile");

        assert_eq!(bashrc.matches(RC_BLOCK_START).count(), 1);
        assert_eq!(bash_profile.matches(RC_BLOCK_START).count(), 1);
    }

    #[test]
    fn ensure_source_block_errors_instead_of_clobbering_unreadable_paths() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join(".profile");
        fs::create_dir_all(&path).expect("directory placeholder");

        let error = ensure_source_block(&path).expect_err("expected read failure");

        assert!(matches!(error, AppError::Io(_)));
        assert!(path.is_dir());
    }

    #[test]
    fn inspect_cli_exposure_reports_shell_init_fallback_as_healthy() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        install_shell_init_fallback(&paths, "/bin/bash").expect("fallback");

        let inspection =
            inspect_cli_exposure_with_context(&paths, &[PathBuf::from("/usr/bin")], "/bin/bash")
                .expect("inspection");

        assert_eq!(
            inspection,
            ExposureCheck::Active(shell_init_exposure(&paths))
        );
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
