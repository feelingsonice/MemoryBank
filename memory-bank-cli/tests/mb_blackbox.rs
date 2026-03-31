use memory_bank_app::AppPaths;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
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

struct HealthzServer {
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
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
[ -n "$last" ] && /bin/cat "$last"
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
    if [ "$code" -eq 0 ]; then
      printf 'pid = %s\n' "${{MB_TEST_LAUNCHCTL_PID:-4321}}"
    fi
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
  "--user show memory-bank.service")
    printf '%s\n' "${{MB_TEST_SYSTEMCTL_MAINPID:-0}}"
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
        }

        let path = format!("{}:/usr/bin:/bin", shim_dir.path().display());

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
        command.env("SHELL", "/bin/zsh");
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

impl HealthzServer {
    fn new(namespace: &str, provider: &str, encoder: &str, version: &str) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind health server");
        let server_port = listener.local_addr().expect("local addr").port();
        listener
            .set_nonblocking(true)
            .expect("set nonblocking health listener");
        let body = format!(
            "{{\"ok\":true,\"namespace\":\"{namespace}\",\"port\":{server_port},\"llm_provider\":\"{provider}\",\"encoder_provider\":\"{encoder}\",\"version\":\"{version}\"}}"
        );
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !stop_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buffer = [0_u8; 1024];
                        let _ = stream.read(&mut buffer);
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            port: server_port,
            stop,
            handle: Some(handle),
        }
    }

    fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for HealthzServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
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

fn assert_contains_all(haystack: &str, needles: &[&str]) {
    for needle in needles {
        assert!(
            haystack.contains(needle),
            "expected help output to contain `{needle}`\n\n{haystack}"
        );
    }
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[test]
fn mb_top_level_help_describes_commands_and_quick_start() {
    let harness = MbHarness::new();

    let output = harness.run(&["--help"]);
    assert!(output.status.success(), "{}", stderr_string(&output));

    let help = stdout_string(&output);
    assert_contains_all(
        &help,
        &[
            "Run guided setup for provider, service, and agent integrations",
            "Check current Memory Bank health and configuration",
            "Diagnose common install and configuration problems",
            "Read the managed service log",
            "Manage memory namespaces",
            "Manage the user-scoped background service",
            "Inspect and edit saved configuration",
            "Quick start:",
            "mb setup",
            "mb doctor --fix",
            "mb config show",
        ],
    );
}

#[test]
fn mb_help_for_status_doctor_and_logs_explains_behavior_and_flags() {
    let harness = MbHarness::new();

    let status = harness.run(&["status", "--help"]);
    assert!(status.status.success(), "{}", stderr_string(&status));
    assert_contains_all(
        &stdout_string(&status),
        &[
            "Show the current Memory Bank status.",
            "reports the active namespace",
            "mb doctor",
            "mb logs",
        ],
    );

    let doctor = harness.run(&["doctor", "--help"]);
    assert!(doctor.status.success(), "{}", stderr_string(&doctor));
    assert_contains_all(
        &stdout_string(&doctor),
        &[
            "Diagnose common install and configuration problems.",
            "Doctor checks CLI exposure",
            "Attempt safe repairs such as exposing `mb`",
            "mb doctor --fix",
        ],
    );

    let logs = harness.run(&["logs", "--help"]);
    assert!(logs.status.success(), "{}", stderr_string(&logs));
    assert_contains_all(
        &stdout_string(&logs),
        &[
            "Read the managed service log at `~/.memory_bank/logs/server.log`.",
            "Keep streaming `~/.memory_bank/logs/server.log` as new lines arrive",
            "mb logs --follow",
        ],
    );
}

#[test]
fn mb_help_for_namespace_service_and_config_includes_examples_and_guidance() {
    let harness = MbHarness::new();

    let namespace = harness.run(&["namespace", "--help"]);
    assert!(namespace.status.success(), "{}", stderr_string(&namespace));
    assert_contains_all(
        &stdout_string(&namespace),
        &[
            "Manage Memory Bank namespaces.",
            "List known namespaces and mark the active one",
            "Switch the active namespace",
            "Namespace names are sanitized",
            "mb namespace use work-project",
        ],
    );

    let service = harness.run(&["service", "--help"]);
    assert!(service.status.success(), "{}", stderr_string(&service));
    assert_contains_all(
        &stdout_string(&service),
        &[
            "Manage the user-scoped Memory Bank background service.",
            "launchd on macOS and systemd --user on Linux",
            "Install the managed service definition",
            "Read the managed service log",
            "mb service logs --follow",
            "`mb service start` installs the service definition first if it is missing.",
        ],
    );

    let config = harness.run(&["config", "--help"]);
    assert!(config.status.success(), "{}", stderr_string(&config));
    assert_contains_all(
        &stdout_string(&config),
        &[
            "Inspect and edit saved Memory Bank settings.",
            "Configuration is stored in `~/.memory_bank/settings.toml`.",
            "service.port",
            "server.llm_provider",
            "integrations.openclaw.configured",
            "Default namespace: default",
            "server.llm_provider: anthropic | gemini | open-ai | ollama",
            "mb config set server.llm_provider gemini",
        ],
    );
}

#[test]
fn mb_nested_help_describes_arguments_and_non_obvious_behavior() {
    let harness = MbHarness::new();

    let namespace_use = harness.run(&["namespace", "use", "--help"]);
    assert!(
        namespace_use.status.success(),
        "{}",
        stderr_string(&namespace_use)
    );
    assert_contains_all(
        &stdout_string(&namespace_use),
        &[
            "Switch the active namespace.",
            "restarts it when already running or starts it when stopped",
            "Namespace name to activate",
        ],
    );

    let service_install = harness.run(&["service", "install", "--help"]);
    assert!(
        service_install.status.success(),
        "{}",
        stderr_string(&service_install)
    );
    assert_contains_all(
        &stdout_string(&service_install),
        &[
            "Install the user-scoped Memory Bank service definition for the current platform.",
            "launchd agent on macOS or a systemd --user unit on Linux",
        ],
    );

    let config_set = harness.run(&["config", "set", "--help"]);
    assert!(
        config_set.status.success(),
        "{}",
        stderr_string(&config_set)
    );
    assert_contains_all(
        &stdout_string(&config_set),
        &[
            "Update a single saved config value in `~/.memory_bank/settings.toml`.",
            "Config key to update. Run `mb config --help` to see supported keys",
            "New value for the selected key",
            "server.encoder_provider: fast-embed | local-api | remote-api",
            "mb config set server.llm_model \"\"",
            "Use an empty string to clear optional string overrides",
        ],
    );
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
    assert_contains_all(
        &stdout_string(&set_namespace),
        &[
            "Updated `active_namespace`.",
            "Old value: default",
            "New value: team_a_1",
            "next time the managed service starts",
        ],
    );

    let set_port = harness.run(&["config", "set", "service.port", "4545"]);
    assert!(set_port.status.success(), "{}", stderr_string(&set_port));
    assert_contains_all(
        &stdout_string(&set_port),
        &[
            "Updated `service.port`.",
            "Old value: 3737",
            "New value: 4545",
            "next time the managed service starts",
        ],
    );

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
    assert_contains_all(
        &stdout_string(&created),
        &[
            "Created namespace `team_a_1`.",
            "Directory:",
            "Warning: Requested name `team a/1` was sanitized to `team_a_1`.",
        ],
    );

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
    assert_contains_all(
        &stdout_string(&used),
        &[
            "Active namespace is now `team_a_1`.",
            "Warning: The managed service is not installed, so this namespace will apply on the next service start.",
            "mb service start",
        ],
    );

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
fn mb_internal_bootstrap_install_creates_a_managed_launcher() {
    let harness = MbHarness::new();
    harness.seed_service_binary_placeholders();
    let launcher_dir = harness.paths.home_dir.join(".local/bin");
    let path = format!(
        "{launcher_dir}:/usr/bin:/bin",
        launcher_dir = launcher_dir.display()
    );

    let output = harness.run_with_env(
        &["internal", "bootstrap-install"],
        &[("PATH", &path), ("SHELL", "/bin/bash")],
    );

    assert!(output.status.success(), "{}", stderr_string(&output));
    let launcher = fs::read_to_string(launcher_dir.join("mb")).expect("launcher");
    assert!(launcher.contains("Memory Bank managed launcher"));
    assert!(stdout_string(&output).contains("managed `mb` launcher"));
}

#[test]
fn mb_internal_bootstrap_install_falls_back_to_managed_shell_init() {
    let harness = MbHarness::new();
    harness.seed_service_binary_placeholders();

    let output = harness.run_with_env(
        &["internal", "bootstrap-install"],
        &[("PATH", "/usr/bin:/bin"), ("SHELL", "/bin/zsh")],
    );

    assert!(output.status.success(), "{}", stderr_string(&output));
    let env_file = fs::read_to_string(harness.paths.root.join("env.sh")).expect("env file");
    assert!(env_file.contains("Memory Bank managed environment"));
    let zprofile = fs::read_to_string(harness.paths.home_dir.join(".zprofile")).expect("zprofile");
    let zshrc = fs::read_to_string(harness.paths.home_dir.join(".zshrc")).expect("zshrc");
    assert!(zprofile.contains("# >>> Memory Bank >>>"));
    assert!(zshrc.contains("# >>> Memory Bank >>>"));
}

#[test]
fn mb_internal_bootstrap_install_fails_on_unrelated_mb_collision() {
    let harness = MbHarness::new();
    harness.seed_service_binary_placeholders();
    let launcher_dir = harness.paths.home_dir.join(".local/bin");
    fs::create_dir_all(&launcher_dir).expect("launcher dir");
    let collision = launcher_dir.join("mb");
    fs::write(&collision, "#!/bin/sh\nexit 0\n").expect("collision");
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&collision).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&collision, permissions).expect("permissions");
    }
    let path = format!(
        "{launcher_dir}:/usr/bin:/bin",
        launcher_dir = launcher_dir.display()
    );

    let output = harness.run_with_env(
        &["internal", "bootstrap-install"],
        &[("PATH", &path), ("SHELL", "/bin/bash")],
    );

    assert!(!output.status.success());
    assert!(
        stderr_string(&output).contains("another executable already exists on PATH"),
        "{}",
        stderr_string(&output)
    );
}

#[test]
fn mb_doctor_fix_provisions_cli_exposure() {
    let harness = MbHarness::new();
    harness.seed_service_binary_placeholders();
    let launcher_dir = harness.paths.home_dir.join(".local/bin");
    let path = format!(
        "{launcher_dir}:{}",
        harness.path,
        launcher_dir = launcher_dir.display()
    );

    let output = harness.run_with_env(
        &["doctor", "--fix"],
        &[("PATH", &path), ("SHELL", "/bin/bash")],
    );

    assert!(output.status.success(), "{}", stderr_string(&output));
    let launcher = fs::read_to_string(launcher_dir.join("mb")).expect("launcher");
    assert!(launcher.contains("Memory Bank managed launcher"));
}

#[test]
fn mb_service_install_writes_the_managed_service_definition() {
    let harness = MbHarness::new();
    harness.seed_service_binary_placeholders();

    let autostart = harness.run(&["config", "set", "service.autostart", "true"]);
    assert!(autostart.status.success(), "{}", stderr_string(&autostart));

    let output = harness.run(&["service", "install"]);
    assert!(output.status.success(), "{}", stderr_string(&output));
    assert_contains_all(
        &stdout_string(&output),
        &[
            "Installing the managed service definition...",
            "Success: Installed the managed service definition.",
            "Autostart: yes",
            "Log file:",
        ],
    );

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
fn mb_service_install_does_not_poll_for_activation() {
    let harness = MbHarness::new();
    harness.seed_service_binary_placeholders();

    let output = harness.run(&["service", "install"]);
    assert!(output.status.success(), "{}", stderr_string(&output));

    let log = harness.read_shim_log();
    #[cfg(target_os = "macos")]
    assert!(count_occurrences(&log, "launchctl print") <= 2, "{log}");
    #[cfg(target_os = "linux")]
    assert!(
        count_occurrences(
            &log,
            "systemctl --user is-active --quiet memory-bank.service"
        ) <= 2,
        "{log}"
    );
}

#[test]
fn mb_service_status_shows_runtime_summary_when_health_is_available() {
    let harness = MbHarness::new();
    harness.seed_service_binary_placeholders();
    let healthz = HealthzServer::new("default", "anthropic", "fast-embed", "test");

    let set_port = harness.run(&["config", "set", "service.port", &healthz.port().to_string()]);
    assert!(set_port.status.success(), "{}", stderr_string(&set_port));
    let install = harness.run(&["service", "install"]);
    assert!(install.status.success(), "{}", stderr_string(&install));

    let status = harness.run_with_env(
        &["service", "status"],
        &[
            #[cfg(target_os = "macos")]
            ("MB_TEST_LAUNCHCTL_PRINT_EXIT", "0"),
            #[cfg(target_os = "macos")]
            ("MB_TEST_LAUNCHCTL_PID", "4242"),
            #[cfg(target_os = "linux")]
            ("MB_TEST_SYSTEMCTL_IS_ACTIVE_EXIT", "0"),
            #[cfg(target_os = "linux")]
            ("MB_TEST_SYSTEMCTL_MAINPID", "4242"),
        ],
    );
    assert!(status.status.success(), "{}", stderr_string(&status));
    assert_contains_all(
        &stdout_string(&status),
        &[
            "Memory Bank service",
            "Manager:",
            "Installed: yes",
            "Active: yes",
            "URL:",
            "Log file:",
            "PID: 4242",
            "Health: yes",
            "Namespace: default",
            &format!("Port: {}", healthz.port()),
            "Provider: anthropic",
            "Encoder: fast-embed",
            "Version: test",
        ],
    );
}

#[test]
fn mb_service_commands_report_progress_and_outcomes() {
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
    assert_contains_all(
        &stdout_string(&start),
        &[
            "Starting Memory Bank service...",
            "Warning: Sent the service request, but the managed service does not appear active yet.",
            "mb service status",
        ],
    );

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

    let restart = harness.run_with_env(
        &["service", "restart"],
        &[
            #[cfg(target_os = "macos")]
            ("MB_TEST_LAUNCHCTL_PRINT_EXIT", "1"),
            #[cfg(target_os = "linux")]
            ("MB_TEST_SYSTEMCTL_IS_ACTIVE_EXIT", "1"),
        ],
    );
    assert!(restart.status.success(), "{}", stderr_string(&restart));
    assert_contains_all(
        &stdout_string(&restart),
        &[
            "Restarting Memory Bank service...",
            "Warning: Sent the service request, but the managed service does not appear active yet.",
        ],
    );

    #[cfg(target_os = "macos")]
    {
        let log = harness.read_shim_log();
        assert!(log.contains("launchctl bootstrap"));
    }

    #[cfg(target_os = "linux")]
    {
        let log = harness.read_shim_log();
        assert!(log.contains("systemctl --user start memory-bank.service"));
    }

    let stop = harness.run(&["service", "stop"]);
    assert!(stop.status.success(), "{}", stderr_string(&stop));
    assert_contains_all(
        &stdout_string(&stop),
        &[
            "Stopping Memory Bank service...",
            "Warning: The managed service is already stopped.",
        ],
    );

    #[cfg(target_os = "macos")]
    assert!(!harness.read_shim_log().contains("launchctl bootout"));
    #[cfg(target_os = "linux")]
    assert!(
        !harness
            .read_shim_log()
            .contains("systemctl --user stop memory-bank.service")
    );
}

#[test]
fn mb_service_start_when_already_active_avoids_transition_poll_loops() {
    let harness = MbHarness::new();
    harness.seed_service_binary_placeholders();
    let healthz = HealthzServer::new("default", "anthropic", "fast-embed", "test");

    let set_port = harness.run(&["config", "set", "service.port", &healthz.port().to_string()]);
    assert!(set_port.status.success(), "{}", stderr_string(&set_port));
    let install = harness.run(&["service", "install"]);
    assert!(install.status.success(), "{}", stderr_string(&install));
    fs::write(&harness.shim_log, "").expect("clear shim log");

    let output = harness.run_with_env(
        &["service", "start"],
        &[
            #[cfg(target_os = "macos")]
            ("MB_TEST_LAUNCHCTL_PRINT_EXIT", "0"),
            #[cfg(target_os = "macos")]
            ("MB_TEST_LAUNCHCTL_PID", "4242"),
            #[cfg(target_os = "linux")]
            ("MB_TEST_SYSTEMCTL_IS_ACTIVE_EXIT", "0"),
            #[cfg(target_os = "linux")]
            ("MB_TEST_SYSTEMCTL_MAINPID", "4242"),
        ],
    );
    assert!(output.status.success(), "{}", stderr_string(&output));
    assert_contains_all(
        &stdout_string(&output),
        &[
            "Starting Memory Bank service...",
            "Success: Memory Bank service was already active and is still running.",
            "Health: yes",
        ],
    );

    let log = harness.read_shim_log();
    #[cfg(target_os = "macos")]
    assert!(count_occurrences(&log, "launchctl print") <= 4, "{log}");
    #[cfg(target_os = "linux")]
    assert!(
        count_occurrences(
            &log,
            "systemctl --user is-active --quiet memory-bank.service"
        ) <= 4,
        "{log}"
    );
}
