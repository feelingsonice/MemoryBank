use crate::AppError;
use shlex::try_quote;
#[cfg(test)]
use std::collections::BTreeMap;
use std::io;
#[cfg(test)]
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
#[cfg(test)]
use std::process::Stdio;
#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use wait_timeout::ChildExt;

#[derive(Debug)]
pub(crate) struct CommandOutcome {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    #[allow(dead_code)]
    pub(crate) exit_code: Option<i32>,
    pub(crate) success: bool,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

pub(crate) fn current_uid() -> Result<String, AppError> {
    let outcome = run_command_capture("id", &["-u"])?;
    if outcome.success {
        Ok(outcome.stdout)
    } else {
        Err(outcome.into_error())
    }
}

pub(crate) fn run_command(program: &str, args: &[&str]) -> Result<(), AppError> {
    let outcome = run_command_capture(program, args)?;
    if outcome.success {
        Ok(())
    } else {
        Err(outcome.into_error())
    }
}

pub(crate) fn run_command_capture(
    program: &str,
    args: &[&str],
) -> Result<CommandOutcome, AppError> {
    let output = ProcessCommand::new(program)
        .args(args)
        .output()
        .map_err(|error| command_error(program, error))?;

    Ok(command_outcome(program, args, output))
}

fn command_error(program: &str, error: io::Error) -> AppError {
    if error.kind() == io::ErrorKind::NotFound {
        AppError::MissingBinary(program.to_string())
    } else {
        AppError::Io(error)
    }
}

fn command_outcome(program: &str, args: &[&str], output: std::process::Output) -> CommandOutcome {
    CommandOutcome {
        program: program.to_string(),
        args: args.iter().map(|value| (*value).to_string()).collect(),
        exit_code: output.status.code(),
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub(crate) struct CommandRunOptions {
    cwd: Option<PathBuf>,
    env: BTreeMap<String, String>,
    remove_env: Vec<String>,
    timeout: Option<Duration>,
}

#[cfg(test)]
impl CommandRunOptions {
    pub(crate) fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub(crate) fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub(crate) fn with_removed_env(mut self, key: impl Into<String>) -> Self {
        self.remove_env.push(key.into());
        self
    }

    pub(crate) fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

#[cfg(test)]
pub(crate) fn run_command_capture_with_options(
    program: &str,
    args: &[&str],
    options: &CommandRunOptions,
) -> Result<CommandOutcome, AppError> {
    let mut command = ProcessCommand::new(program);
    command.args(args);
    if let Some(cwd) = &options.cwd {
        command.current_dir(cwd);
    }
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    for key in &options.remove_env {
        command.env_remove(key);
    }
    for (key, value) in &options.env {
        command.env(key, value);
    }

    let mut child = command
        .spawn()
        .map_err(|error| command_error(program, error))?;
    let output = match options.timeout {
        Some(timeout) => match child.wait_timeout(timeout).map_err(AppError::Io)? {
            Some(_) => child.wait_with_output()?,
            None => {
                let command_line = format!("{} {}", program, args.join(" "));
                let _ = child.kill();
                let _ = child.wait();
                return Err(AppError::CommandTimedOut(command_line, timeout));
            }
        },
        None => child.wait_with_output()?,
    };

    Ok(command_outcome(program, args, output))
}

pub(crate) fn shell_escape(value: &str) -> String {
    let _ = try_quote(value).expect("shell_quote should reject only invalid shell values");
    format!("'{}'", value.replace('\'', r#"'"'"'"#))
}

pub(crate) fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

impl CommandOutcome {
    pub(crate) fn combined_output(&self) -> String {
        if self.stderr.is_empty() {
            self.stdout.clone()
        } else if self.stdout.is_empty() {
            self.stderr.clone()
        } else {
            format!("{}\n{}", self.stdout, self.stderr)
        }
    }

    pub(crate) fn into_error(self) -> AppError {
        let details = if self.stderr.is_empty() {
            self.stdout
        } else if self.stdout.is_empty() {
            self.stderr
        } else {
            format!("{}\n{}", self.stderr, self.stdout)
        };
        AppError::CommandFailed(format!("{} {}", self.program, self.args.join(" ")), details)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_escape_handles_embedded_single_quotes() {
        assert_eq!(
            shell_escape("it's complicated"),
            r#"'it'"'"'s complicated'"#
        );
    }

    #[test]
    fn combined_output_and_into_error_preserve_stdout_and_stderr() {
        let outcome = CommandOutcome {
            program: "tool".to_string(),
            args: vec!["do".to_string(), "thing".to_string()],
            exit_code: Some(1),
            success: false,
            stdout: "stdout".to_string(),
            stderr: "stderr".to_string(),
        };

        assert_eq!(outcome.combined_output(), "stdout\nstderr");
        assert_eq!(
            outcome.into_error().to_string(),
            "command `tool do thing` failed: stderr\nstdout"
        );
    }

    #[test]
    fn run_command_capture_reports_missing_binary() {
        let error = run_command_capture("memory-bank-cli-test-binary-that-does-not-exist", &[])
            .expect_err("missing binary");

        assert!(matches!(error, AppError::MissingBinary(_)));
    }

    #[test]
    fn run_command_capture_with_options_applies_cwd_and_env() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let expected_cwd = std::fs::canonicalize(temp.path()).expect("canonical cwd");
        let outcome = run_command_capture_with_options(
            "sh",
            &["-c", "printf '%s|%s' \"$PWD\" \"$SPECIAL_VALUE\""],
            &CommandRunOptions::default()
                .with_cwd(temp.path())
                .with_env("SPECIAL_VALUE", "configured")
                .with_timeout(Duration::from_secs(2)),
        )
        .expect("run command");

        assert!(outcome.success);
        assert_eq!(
            outcome.stdout,
            format!("{}|configured", expected_cwd.display())
        );
    }

    #[test]
    fn run_command_capture_with_options_times_out() {
        let error = run_command_capture_with_options(
            "sh",
            &["-c", "sleep 5"],
            &CommandRunOptions::default().with_timeout(Duration::from_millis(100)),
        )
        .expect_err("command should time out");

        assert!(matches!(error, AppError::CommandTimedOut(_, _)));
    }
}
