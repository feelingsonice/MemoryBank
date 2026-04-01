use crate::AppError;
use chrono::Local;
use memory_bank_app::AppPaths;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn load_toml_config(path: &Path) -> Result<String, AppError> {
    if !path.exists() {
        return Ok(String::new());
    }

    let contents = fs::read_to_string(path)?;
    let contents = strip_utf8_bom(&contents).to_string();
    if is_effectively_empty_toml(&contents) {
        return Ok(String::new());
    }

    toml::from_str::<toml::Table>(&contents).map_err(|error| {
        AppError::Message(format!("failed to parse {}: {}", path.display(), error))
    })?;
    Ok(contents)
}

pub(crate) fn write_toml_config_with_backups(
    paths: &AppPaths,
    original_path: &Path,
    contents: &str,
) -> Result<(), AppError> {
    let rendered = normalize_trailing_newline(contents);
    if !rendered.is_empty() {
        toml::from_str::<toml::Table>(&rendered).map_err(|error| {
            AppError::Message(format!(
                "failed to render TOML for {}: {}",
                original_path.display(),
                error
            ))
        })?;
    }

    if original_path.exists() {
        backup_existing_file(paths, original_path)?;
    } else if let Some(parent) = original_path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(original_path, rendered)?;
    Ok(())
}

fn normalize_trailing_newline(contents: &str) -> String {
    let trimmed = contents.trim_end_matches('\n');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

fn is_effectively_empty_toml(contents: &str) -> bool {
    contents.lines().all(|line| {
        let trimmed = line.trim();
        trimmed.is_empty() || trimmed.starts_with('#')
    })
}

fn strip_utf8_bom(contents: &str) -> &str {
    contents.strip_prefix('\u{feff}').unwrap_or(contents)
}

fn backup_existing_file(paths: &AppPaths, original_path: &Path) -> Result<(), AppError> {
    let timestamp = Local::now().format("%Y%m%d%H%M%S").to_string();
    let relative = original_path
        .strip_prefix(Path::new("/"))
        .unwrap_or(original_path);
    let central_backup = paths.backups_dir.join(timestamp).join(relative);
    if let Some(parent) = central_backup.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(original_path, &central_backup)?;

    let sibling_backup = PathBuf::from(format!("{}.mb_backup", original_path.display()));
    if let Some(parent) = sibling_backup.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(original_path, sibling_backup)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn load_toml_config_treats_comment_only_file_as_empty() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join("config.toml");
        fs::write(&path, "# comment only\n").expect("write config");

        let loaded = load_toml_config(&path).expect("load config");
        assert!(loaded.is_empty());
    }

    #[test]
    fn write_toml_config_with_backups_creates_sibling_backup() {
        let temp = TempDir::new().expect("tempdir");
        let paths = AppPaths::from_home_dir(temp.path().to_path_buf());
        let path = temp.path().join(".codex/config.toml");
        fs::create_dir_all(path.parent().expect("parent")).expect("parent");
        fs::write(&path, "model = \"gpt-5.4\"\n").expect("seed config");

        write_toml_config_with_backups(&paths, &path, "model = \"gpt-5.4\"\n")
            .expect("write config");

        assert!(path.with_extension("toml.mb_backup").exists());
    }
}
