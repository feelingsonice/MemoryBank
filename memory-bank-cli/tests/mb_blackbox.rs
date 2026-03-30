use memory_bank_app::AppPaths;
use std::env;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

const MB_BINARY: &str = env!("CARGO_BIN_EXE_mb");

struct MbHarness {
    _home: TempDir,
    _cwd: TempDir,
    _shim_dir: TempDir,
    paths: AppPaths,
    cwd: PathBuf,
    shim_log: PathBuf,
    path: String,
}

impl MbHarness {
    fn new() -> Self {
        let home = TempDir::new().expect("temp home");
        let cwd = TempDir::new().expect("temp cwd");
        let shim_dir = TempDir::new().expect("temp shims");
        let paths = AppPaths::from_home_dir(home.path().to_path_buf());
        paths.ensure_base_dirs().expect("base dirs");
        fs::create_dir_all(home.path().join(".config")).expect("xdg config");
        fs::create_dir_all(home.path().join(".local/state")).expect("xdg state");
        fs::create_dir_all(home.path().join(".local/share")).expect("xdg data");
        let shim_log = home.path().join("shim-invocations.log");
        let cwd_path = cwd.path().to_path_buf();

        write_shim(
            &shim_dir.path().join("tail"),
            &format!(
                r#"#!/bin/sh
printf '%s\n' "tail $*" >> '{}'
last=""
for arg in "$@"; do
  last="$arg"
done
[ -n "$last" ] && cat "$last"
exit 0
"#,
                shim_log.display()
            ),
        );

        #[cfg(target_os = "macos")]
        {
            write_shim(
                &shim_dir.path().join("launchctl"),
                &format!(
                    r#"#!/bin/sh
printf '%s\n' "launchctl $*" >> '{}'
case "$1" in
  print)
    code="${{MB_TEST_LAUNCHCTL_PRINT_EXIT:-1}}"
    exit "$code"
    ;;
  bootstrap|kickstart|bootout)
    exit 0
    ;;
  *)
    exit 0
    ;;
esac
"#,
                    shim_log.display()
                ),
            );
            write_shim(
                &shim_dir.path().join("id"),
                &format!(
                    r#"#!/bin/sh
printf '%s\n' "id $*" >> '{}'
printf '%s\n' "${{MB_TEST_UID:-501}}"
"#,
                    shim_log.display()
                ),
            );
        }

        #[cfg(target_os = "linux")]
        {
            write_shim(
                &shim_dir.path().join("systemctl"),
                &format!(
                    r#"#!/bin/sh
printf '%s\n' "systemctl $*" >> '{}'
case "$1 $2 $3" in
  "--user is-active --quiet")
    code="${{MB_TEST_SYSTEMCTL_IS_ACTIVE_EXIT:-1}}"
    exit "$code"
    ;;
  *)
    exit 0
    ;;
esac
"#,
                    shim_log.display()
                ),
            );
        }

        let path = format!(
            "{}:{}",
            shim_dir.path().display(),
            env::var("PATH").unwrap_or_default()
        );

        Self {
            _home: home,
            _cwd: cwd,
            _shim_dir: shim_dir,
            paths,
            cwd: cwd_path,
            shim_log,
            path,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with_env(args, &[])
    }

    fn run_with_env(&self, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
        let mut command = Command::new(MB_BINARY);
        command.args(args);
        command.current_dir(&self.cwd);
        command.env("HOME", &self.paths.home_dir);
        command.env("PATH", &self.path);
        command.env("NO_COLOR", "1");
        command.env("CI", "1");
        command.env("TERM", "dumb");
        command.env("XDG_CONFIG_HOME", self.paths.home_dir.join(".config"));
        command.env("XDG_STATE_HOME", self.paths.home_dir.join(".local/state"));
        command.env("XDG_DATA_HOME", self.paths.home_dir.join(".local/share"));
        command.env_remove("OPENCLAW_STATE_DIR");
        command.env_remove("OPENCLAW_CONFIG_PATH");
        command.env_remove("OPENCLAW_CONTAINER");
        for (key, value) in extra_env {
            command.env(key, value);
        }
        command.output().expect("run mb")
    }

    fn seed_service_binary_placeholders(&self) {
        for binary in [
            "memory-bank-server",
            "memory-bank-hook",
            "memory-bank-mcp-proxy",
        ] {
            let path = self.paths.binary_path(binary);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("binary dir");
            }
            fs::write(&path, "#!/bin/sh\nexit 0\n").expect("placeholder binary");
            #[cfg(unix)]
            {
                let mut permissions = fs::metadata(&path).expect("metadata").permissions();
                permissions.set_mode(0o755);
                fs::set_permissions(&path, permissions).expect("permissions");
            }
        }
    }

    fn write_log_file(&self, contents: &str) {
        if let Some(parent) = self.paths.log_file.parent() {
            fs::create_dir_all(parent).expect("logs dir");
        }
        fs::write(&self.paths.log_file, contents).expect("log file");
    }

    fn read_shim_log(&self) -> String {
        fs::read_to_string(&self.shim_log).unwrap_or_default()
    }
}

fn write_shim(path: &Path, contents: &str) {
    fs::write(path, contents).expect("shim file");
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("permissions");
    }
}

fn stdout_string(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr_string(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

#[test]
fn mb_config_round_trips_values_through_the_binary() {
    let harness = MbHarness::new();

    let set_namespace = harness.run(&["config", "set", "active_namespace", "team a/1"]);
    assert!(
        set_namespace.status.success(),
        "{}",
        stderr_string(&set_namespace)
    );

    let set_port = harness.run(&["config", "set", "service.port", "4545"]);
    assert!(set_port.status.success(), "{}", stderr_string(&set_port));

    let get_namespace = harness.run(&["config", "get", "active_namespace"]);
    assert!(
        get_namespace.status.success(),
        "{}",
        stderr_string(&get_namespace)
    );
    assert_eq!(stdout_string(&get_namespace).trim(), "team_a_1");

    let get_port = harness.run(&["config", "get", "service.port"]);
    assert!(get_port.status.success(), "{}", stderr_string(&get_port));
    assert_eq!(stdout_string(&get_port).trim(), "4545");

    let show = harness.run(&["config", "show"]);
    assert!(show.status.success(), "{}", stderr_string(&show));
    let rendered = stdout_string(&show);
    assert!(rendered.contains("active_namespace = \"team_a_1\""));
    assert!(rendered.contains("[service]"));
    assert!(rendered.contains("port = 4545"));
}

#[test]
fn mb_namespace_commands_manage_sanitized_names_without_starting_services() {
    let harness = MbHarness::new();

    let created = harness.run(&["namespace", "create", "team a/1"]);
    assert!(created.status.success(), "{}", stderr_string(&created));
    assert!(stdout_string(&created).contains("Created namespace `team_a_1`."));

    let used = harness.run_with_env(
        &["namespace", "use", "team a/1"],
        &[
            #[cfg(target_os = "macos")]
            ("MB_TEST_LAUNCHCTL_PRINT_EXIT", "1"),
            #[cfg(target_os = "linux")]
            ("MB_TEST_SYSTEMCTL_IS_ACTIVE_EXIT", "1"),
        ],
    );
    assert!(used.status.success(), "{}", stderr_string(&used));

    let current = harness.run(&["namespace", "current"]);
    assert!(current.status.success(), "{}", stderr_string(&current));
    assert_eq!(stdout_string(&current).trim(), "team_a_1");

    let list = harness.run(&["namespace", "list"]);
    assert!(list.status.success(), "{}", stderr_string(&list));
    let listing = stdout_string(&list);
    assert!(listing.contains("team_a_1 (active)"));
    assert_eq!(listing.matches("team_a_1").count(), 1);

    assert!(
        harness
            .paths
            .namespace_dir(&memory_bank_app::Namespace::new("team a/1"))
            .is_dir()
    );
}

#[test]
fn mb_logs_reads_the_log_file_through_the_tail_shim() {
    let harness = MbHarness::new();
    harness.write_log_file("line one\nline two\n");

    let output = harness.run(&["logs"]);
    assert!(output.status.success(), "{}", stderr_string(&output));
    assert!(stdout_string(&output).contains("line one"));
    assert!(stdout_string(&output).contains("line two"));
    assert!(harness.read_shim_log().contains("tail -n 200"));
}

#[test]
fn mb_setup_fails_cleanly_without_a_tty() {
    let harness = MbHarness::new();

    let output = harness.run(&["setup"]);

    assert!(!output.status.success());
    assert!(stderr_string(&output).contains("interactive terminal"));
}

#[test]
fn mb_service_install_writes_the_managed_service_definition() {
    let harness = MbHarness::new();
    harness.seed_service_binary_placeholders();

    let autostart = harness.run(&["config", "set", "service.autostart", "true"]);
    assert!(autostart.status.success(), "{}", stderr_string(&autostart));

    let output = harness.run(&["service", "install"]);
    assert!(output.status.success(), "{}", stderr_string(&output));

    #[cfg(target_os = "macos")]
    let service_path = harness
        .paths
        .home_dir
        .join("Library/LaunchAgents/com.memory-bank.mb.plist");
    #[cfg(target_os = "linux")]
    let service_path = harness
        .paths
        .home_dir
        .join(".config/systemd/user/memory-bank.service");

    let rendered = fs::read_to_string(service_path).expect("service definition");
    assert!(rendered.contains("internal"));
    assert!(rendered.contains("run-server"));
    assert!(rendered.contains("server.log"));
    #[cfg(target_os = "macos")]
    assert!(rendered.contains("<true/>"));
}

#[test]
fn mb_service_start_status_restart_and_stop_use_only_shims() {
    let harness = MbHarness::new();
    harness.seed_service_binary_placeholders();

    let start = harness.run_with_env(
        &["service", "start"],
        &[
            #[cfg(target_os = "macos")]
            ("MB_TEST_LAUNCHCTL_PRINT_EXIT", "1"),
            #[cfg(target_os = "linux")]
            ("MB_TEST_SYSTEMCTL_IS_ACTIVE_EXIT", "1"),
        ],
    );
    assert!(start.status.success(), "{}", stderr_string(&start));

    #[cfg(target_os = "macos")]
    {
        let log = harness.read_shim_log();
        assert!(log.contains("id -u"));
        assert!(log.contains("launchctl print"));
        assert!(log.contains("launchctl bootstrap"));
        assert!(log.contains("launchctl kickstart -k"));
    }

    #[cfg(target_os = "linux")]
    {
        let log = harness.read_shim_log();
        assert!(log.contains("systemctl --user start memory-bank.service"));
    }

    let status = harness.run_with_env(
        &["service", "status"],
        &[
            #[cfg(target_os = "macos")]
            ("MB_TEST_LAUNCHCTL_PRINT_EXIT", "0"),
            #[cfg(target_os = "linux")]
            ("MB_TEST_SYSTEMCTL_IS_ACTIVE_EXIT", "0"),
        ],
    );
    assert!(status.status.success(), "{}", stderr_string(&status));
    assert!(stdout_string(&status).contains("Installed: yes"));
    assert!(stdout_string(&status).contains("Active: yes"));

    let restart = harness.run_with_env(
        &["service", "restart"],
        &[
            #[cfg(target_os = "macos")]
            ("MB_TEST_LAUNCHCTL_PRINT_EXIT", "0"),
            #[cfg(target_os = "linux")]
            ("MB_TEST_SYSTEMCTL_IS_ACTIVE_EXIT", "0"),
        ],
    );
    assert!(restart.status.success(), "{}", stderr_string(&restart));

    #[cfg(target_os = "macos")]
    {
        let log = harness.read_shim_log();
        assert!(log.contains("launchctl kickstart -k"));
    }

    #[cfg(target_os = "linux")]
    {
        let log = harness.read_shim_log();
        assert!(log.contains("systemctl --user restart memory-bank.service"));
    }

    let stop = harness.run(&["service", "stop"]);
    assert!(stop.status.success(), "{}", stderr_string(&stop));

    #[cfg(target_os = "macos")]
    assert!(harness.read_shim_log().contains("launchctl bootout"));
    #[cfg(target_os = "linux")]
    assert!(
        harness
            .read_shim_log()
            .contains("systemctl --user stop memory-bank.service")
    );
}
