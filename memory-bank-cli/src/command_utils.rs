use crate::AppError;
use std::io;
use std::process::Command as ProcessCommand;

#[derive(Debug)]
pub(crate) struct CommandOutcome {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
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
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                AppError::MissingBinary(program.to_string())
            } else {
                AppError::Io(error)
            }
        })?;

    Ok(CommandOutcome {
        program: program.to_string(),
        args: args.iter().map(|value| (*value).to_string()).collect(),
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

pub(crate) fn shell_escape(value: &str) -> String {
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
}
