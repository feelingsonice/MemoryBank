use crate::AppError;
use chrono::Local;
use jsonc_parser::{ParseOptions, parse_to_serde_value};
use memory_bank_app::{AppPaths, write_json_file};
use serde_json::{Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn load_json_config(path: &Path) -> Result<Value, AppError> {
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }

    let contents = fs::read_to_string(path)?;
    parse_json_config(&contents, path)
}

pub(crate) fn write_json_config_with_backups(
    paths: &AppPaths,
    original_path: &Path,
    value: &Value,
) -> Result<(), AppError> {
    if original_path.exists() {
        backup_existing_file(paths, original_path)?;
    } else if let Some(parent) = original_path.parent() {
        fs::create_dir_all(parent)?;
    }

    write_json_file(original_path, value)?;
    Ok(())
}

pub(crate) fn ensure_object(value: &mut Value) {
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
}

pub(crate) fn object_mut(value: &mut Value) -> Result<&mut Map<String, Value>, AppError> {
    value
        .as_object_mut()
        .ok_or_else(|| AppError::Message("expected JSON object".to_string()))
}

pub(crate) fn array_mut(value: &mut Value) -> Result<&mut Vec<Value>, AppError> {
    value
        .as_array_mut()
        .ok_or_else(|| AppError::Message("expected JSON array".to_string()))
}

fn parse_json_config(contents: &str, path: &Path) -> Result<Value, AppError> {
    if contents.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }

    match serde_json::from_str(contents) {
        Ok(value) => Ok(value),
        Err(strict_error) => match parse_to_serde_value(contents, &jsonc_parse_options()) {
            Ok(Some(value)) => Ok(value),
            Ok(None) => Ok(Value::Object(Map::new())),
            Err(relaxed_error) => Err(AppError::Message(format!(
                "failed to parse {}: {} (also failed with JSONC parser: {})",
                path.display(),
                strict_error,
                relaxed_error
            ))),
        },
    }
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

fn jsonc_parse_options() -> ParseOptions {
    ParseOptions {
        allow_comments: true,
        allow_loose_object_property_names: false,
        allow_trailing_commas: true,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use tempfile::TempDir;

    #[test]
    fn load_json_config_accepts_comments_and_trailing_commas() {
        let temp = TempDir::new().expect("tempdir");
        let config_path = temp.path().join("settings.json");
        fs::write(
            &config_path,
            r#"{
  // comment
  "hooks": {
    "Stop": [
      {
        "hooks": [
          { "command": "echo hi", },
        ],
      },
    ],
  },
}
"#,
        )
        .expect("write config");

        let value = load_json_config(&config_path).expect("load config");
        assert_eq!(
            value["hooks"]["Stop"][0]["hooks"][0]["command"],
            Value::String("echo hi".to_string())
        );
    }

    #[test]
    fn load_json_config_reports_path_on_parse_failure() {
        let temp = TempDir::new().expect("tempdir");
        let config_path = temp.path().join("broken.json");
        fs::write(&config_path, "{ nope").expect("write broken config");

        let error = load_json_config(&config_path).expect_err("expected parse failure");
        let message = error.to_string();
        assert!(message.contains("broken.json"));
        assert!(message.contains("failed to parse"));
    }
}
