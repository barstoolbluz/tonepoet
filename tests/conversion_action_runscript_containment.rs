#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use tempfile::TempDir;
use tonepoet::convert::script_supervisor::{
    recover_supervised, run_supervised, ContainmentBackend, ContainmentPreference,
    OutputCaptureTerminal, RuntimeDirectoryIdentity,
    ScriptLifecycleEvent, ScriptRecoveryOutcome, ScriptRecoveryRequest, ScriptSupervisorError,
    SupervisedCommand,
};

static TOKEN_COUNTER: AtomicU64 = AtomicU64::new(1);

struct Fixture {
    _temp: TempDir,
    script: PathBuf,
    runtime: PathBuf,
    runtime_identity: RuntimeDirectoryIdentity,
}

impl Fixture {
    fn new(body: &str) -> Self {
        let temp = tempfile::tempdir().expect("create fixture directory");
        let script = temp.path().join("fixture-script");
        fs::write(&script, format!("#!/bin/sh\nset -eu\n{body}\n"))
            .expect("write executable fixture");
        let mut permissions = fs::metadata(&script).expect("script metadata").permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&script, permissions).expect("chmod executable fixture");

        let runtime = temp.path().join("runtime");
        fs::create_dir(&runtime).expect("create private runtime directory");
        let mut runtime_permissions = fs::metadata(&runtime)
            .expect("runtime metadata")
            .permissions();
        runtime_permissions.set_mode(0o700);
        fs::set_permissions(&runtime, runtime_permissions).expect("chmod runtime directory");
        let metadata = fs::metadata(&runtime).expect("runtime metadata after chmod");
        Self {
            _temp: temp,
            script,
            runtime,
            runtime_identity: RuntimeDirectoryIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            },
        }
    }

    fn command(&self, args: Vec<String>, timeout: Duration) -> SupervisedCommand {
        let script_file = Arc::new(
            File::open(&self.script).expect("open retained test script descriptor"),
        );
        let working_directory_file = Arc::new(
            File::open(self._temp.path())
                .expect("open retained test working-directory descriptor"),
        );
        SupervisedCommand {
            token: next_token(),
            runtime_directory: self.runtime.clone(),
            script_file,
            working_directory_file,
            script: self.script.clone(),
            args,
            working_directory: self._temp.path().to_path_buf(),
            environment: BTreeMap::from([
                ("PATH".to_string(), "/usr/bin:/bin".to_string()),
                ("TONEPOET_TEST".to_string(), "literal value".to_string()),
            ]),
            timeout,
            runtime_identity: self.runtime_identity,
            containment_preference: ContainmentPreference::Auto,
            helper_executable: Some(PathBuf::from(env!("CARGO_BIN_EXE_tonepoet"))),
        }
    }
}

fn next_token() -> String {
    let counter = TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:032x}", ((std::process::id() as u128) << 64) | counter as u128)
}

fn run_collect(
    command: &SupervisedCommand,
    cancelled: &AtomicBool,
) -> Result<
    (
        tonepoet::convert::script_supervisor::SupervisedOutcome,
        Vec<ScriptLifecycleEvent>,
    ),
    ScriptSupervisorError,
> {
    let mut events = Vec::new();
    let outcome = run_supervised(
        command,
        || cancelled.load(Ordering::Acquire),
        |event| {
            events.push(event.clone());
            Ok(())
        },
    )?;
    Ok((outcome, events))
}

fn assert_containment_terminal(events: &[ScriptLifecycleEvent]) {
    assert!(events.iter().any(|event| matches!(
        event,
        ScriptLifecycleEvent::ContainmentPrepared { .. }
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ScriptLifecycleEvent::ContainmentEmpty { .. }
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ScriptLifecycleEvent::OutputCaptureCompleted { .. }
    )));
}

#[test]
fn direct_exec_preserves_literal_argv_environment_and_null_stdin() {
    let fixture = Fixture::new(
        r#"
[ "$#" -eq 2 ]
[ "$1" = "one two" ]
[ "$2" = '$(not-a-shell)' ]
[ "$TONEPOET_TEST" = "literal value" ]
if IFS= read -r unexpected; then
    exit 91
fi
printf '%s|%s\n' "$1" "$2"
"#,
    );
    let cancelled = AtomicBool::new(false);
    let command = fixture.command(
        vec!["one two".to_string(), "$(not-a-shell)".to_string()],
        Duration::from_secs(5),
    );
    let (outcome, events) = run_collect(&command, &cancelled).expect("supervised success");
    assert!(outcome.status.success());
    assert!(!outcome.timed_out);
    assert!(!outcome.cancelled);
    assert!(outcome.script_released);
    assert!(outcome.containment_empty);
    assert!(!outcome.background_descendants);
    assert_eq!(outcome.stdout_tail, b"one two|$(not-a-shell)\n");
    assert_containment_terminal(&events);
}

#[test]
fn retained_working_directory_survives_path_replacement() {
    let fixture = Fixture::new(
        r#"
printf retained > cwd-output
pwd -P > "$TONEPOET_CWD_MARKER"
"#,
    );
    let working = fixture._temp.path().join("working");
    let retained = fixture._temp.path().join("retained-working");
    let cwd_marker = fixture._temp.path().join("observed-cwd.txt");
    fs::create_dir(&working).expect("create working directory");

    let mut command = fixture.command(Vec::new(), Duration::from_secs(5));
    command.working_directory = working.clone();
    command.working_directory_file = Arc::new(
        File::open(&working).expect("open exact working-directory descriptor"),
    );
    command.environment.insert(
        "TONEPOET_CWD_MARKER".to_string(),
        cwd_marker.to_string_lossy().into_owned(),
    );

    fs::rename(&working, &retained).expect("rename retained working directory");
    fs::create_dir(&working).expect("create replacement working directory");

    let cancelled = AtomicBool::new(false);
    let (outcome, events) = run_collect(&command, &cancelled)
        .expect("supervisor must use the retained working directory");
    assert!(outcome.status.success());
    assert!(retained.join("cwd-output").is_file());
    assert!(
        !working.join("cwd-output").exists(),
        "replacement pathname must remain untouched"
    );
    let observed = fs::read_to_string(&cwd_marker).expect("read observed cwd");
    assert!(
        observed.trim_end().ends_with("retained-working"),
        "launcher cwd must be the renamed retained directory: {observed:?}"
    );
    assert_containment_terminal(&events);
}

#[test]
fn nonzero_exit_is_preserved_after_empty_proof() {
    let fixture = Fixture::new("printf 'failure-tail' >&2\nexit 37");
    let cancelled = AtomicBool::new(false);
    let command = fixture.command(Vec::new(), Duration::from_secs(5));
    let (outcome, events) = run_collect(&command, &cancelled).expect("supervised nonzero");
    assert_eq!(outcome.status.code(), Some(37));
    assert!(outcome.containment_empty);
    assert_eq!(outcome.stderr_tail, b"failure-tail");
    assert_containment_terminal(&events);
}

#[test]
fn cancellation_terminates_the_complete_observed_domain() {
    // The script signals readiness AFTER arming its TERM trap; a blind delay
    // races containment setup (the cgroup-unavailable fallback relaunch can
    // consume it), letting TERM kill the leader before the trap exists and
    // making forced escalation unnecessary.
    let fixture = Fixture::new("trap '' TERM\n: > trap-armed\nwhile :; do sleep 1; done");
    let armed_marker = fixture._temp.path().join("trap-armed");
    let cancelled = Arc::new(AtomicBool::new(false));
    let trigger = Arc::clone(&cancelled);
    let setter = thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !armed_marker.exists() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        thread::sleep(Duration::from_millis(50));
        trigger.store(true, Ordering::Release);
    });
    let command = fixture.command(Vec::new(), Duration::from_secs(30));
    let (outcome, events) = run_collect(&command, &cancelled).expect("cancel supervised script");
    setter.join().expect("join cancellation setter");
    assert!(outcome.cancelled);
    assert!(outcome.containment_empty);
    assert!(events.iter().any(|event| matches!(
        event,
        ScriptLifecycleEvent::TerminationRequested { .. }
    )));
    assert!(
        events.iter().any(|event| matches!(
            event,
            ScriptLifecycleEvent::ForcedTerminationRequested { .. }
        )),
        "events: {events:?}"
    );
    assert_containment_terminal(&events);
}

#[test]
fn leader_zero_with_inherited_pipe_child_is_rejected_and_drained() {
    let fixture = Fixture::new("(trap '' TERM; sleep 30) &\nexit 0");
    let cancelled = AtomicBool::new(false);
    let command = fixture.command(Vec::new(), Duration::from_secs(10));
    let (outcome, events) = run_collect(&command, &cancelled).expect("background detection");
    assert!(outcome.status.success());
    assert!(outcome.background_descendants);
    assert!(outcome.containment_empty);
    assert_ne!(outcome.output_capture.stdout, OutputCaptureTerminal::Abandoned);
    assert_ne!(outcome.output_capture.stderr, OutputCaptureTerminal::Abandoned);
    assert!(events.iter().any(|event| matches!(
        event,
        ScriptLifecycleEvent::TerminationRequested { .. }
    )));
    assert_containment_terminal(&events);
}

#[test]
fn bounded_capture_reports_truncation() {
    let fixture = Fixture::new("yes x | head -c 98304");
    let cancelled = AtomicBool::new(false);
    let command = fixture.command(Vec::new(), Duration::from_secs(10));
    let (outcome, _) = run_collect(&command, &cancelled).expect("large output");
    assert!(outcome.status.success());
    assert_eq!(outcome.stdout_tail.len(), 64 * 1024);
    assert_eq!(outcome.output_capture.stdout, OutputCaptureTerminal::Truncated);
}

#[test]
fn setup_identity_failure_occurs_before_user_code() {
    let fixture = Fixture::new("printf ran > should-not-exist");
    let marker = fixture._temp.path().join("should-not-exist");
    let cancelled = AtomicBool::new(false);
    let mut command = fixture.command(Vec::new(), Duration::from_secs(5));
    command.runtime_identity.inode = command.runtime_identity.inode.saturating_add(1);
    let error = run_collect(&command, &cancelled).expect_err("identity substitution must fail");
    assert!(error.to_string().contains("planned directory"));
    assert!(!marker.exists());
}

#[test]
fn durable_prepare_rejection_prevents_exec() {
    let fixture = Fixture::new("printf ran > should-not-exist");
    let marker = fixture._temp.path().join("should-not-exist");
    let command = fixture.command(Vec::new(), Duration::from_secs(5));
    let error = run_supervised(
        &command,
        || false,
        |event| {
            if matches!(event, ScriptLifecycleEvent::ContainmentPrepared { .. }) {
                return Err(ScriptSupervisorError::Internal(
                    "injected durable-journal failure".to_string(),
                ));
            }
            Ok(())
        },
    )
    .expect_err("prepared event must be ACK-gated");
    assert!(error.to_string().contains("injected durable-journal failure"));
    assert!(!marker.exists());
}

#[test]
fn remote_host_recovery_never_signals_local_processes() {
    let fixture = Fixture::new("exit 0");
    let cancelled = AtomicBool::new(false);
    let command = fixture.command(Vec::new(), Duration::from_secs(5));
    let (outcome, _) = run_collect(&command, &cancelled).expect("complete script");
    let mut descriptor = outcome.descriptor;
    descriptor.host.host_identity.push_str("-remote");
    let recovery = recover_supervised(&ScriptRecoveryRequest {
        token: command.token,
        runtime_directory: fixture.runtime,
        descriptor,
    })
    .expect("remote recovery classification");
    assert!(matches!(
        recovery,
        ScriptRecoveryOutcome::ManualRecoveryRequired(_)
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn forced_supervisor_fallback_is_explicit_and_operational() {
    let fixture = Fixture::new("exit 0");
    let cancelled = AtomicBool::new(false);
    let mut command = fixture.command(Vec::new(), Duration::from_secs(5));
    command.containment_preference = ContainmentPreference::ForceSupervisorFallback;
    let (outcome, _) = run_collect(&command, &cancelled).expect("forced fallback");
    assert_eq!(outcome.descriptor.backend, ContainmentBackend::LinuxSubreaper);
    assert!(outcome.descriptor.warning.is_some());
    assert!(outcome.containment_empty);
}

#[cfg(target_os = "linux")]
#[test]
fn required_cgroup_either_arms_without_control_fd_leak_or_fails_before_user_code() {
    let fixture = Fixture::new(
        r#"
for descriptor in /proc/self/fd/*; do
    target=$(readlink "$descriptor" 2>/dev/null || true)
    case "$target" in
        *cgroup*) exit 88 ;;
    esac
done
printf ran > cgroup-ran
"#,
    );
    let marker = fixture._temp.path().join("cgroup-ran");
    let cancelled = AtomicBool::new(false);
    let mut command = fixture.command(Vec::new(), Duration::from_secs(5));
    command.containment_preference = ContainmentPreference::RequireLinuxCgroupV2;
    match run_collect(&command, &cancelled) {
        Ok((outcome, _)) => {
            assert_eq!(outcome.descriptor.backend, ContainmentBackend::LinuxCgroupV2);
            assert!(outcome.containment_empty);
            assert!(marker.exists());
        }
        Err(error) => {
            assert!(error.to_string().contains("cgroup"));
            assert!(!marker.exists());
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn timeout_contains_setsid_double_fork_and_term_ignoring_grandchild() {
    if !Path::new("/usr/bin/setsid").exists() && !Path::new("/bin/setsid").exists() {
        eprintln!("setsid utility unavailable; platform fixture skipped");
        return;
    }
    let fixture = Fixture::new(
        r#"
trap '' TERM
(
    trap '' TERM
    setsid /bin/sh -c 'trap "" TERM; (trap "" TERM; while :; do sleep 1; done) & exit 0' &
    exit 0
) &
while :; do sleep 1; done
"#,
    );
    let cancelled = AtomicBool::new(false);
    // The timeout clock includes containment setup (and the cgroup-
    // unavailable fallback relaunch); a sub-second budget can expire before
    // the fixture's TERM traps are armed, so no forced escalation would be
    // needed. Give setup room; the traps still guarantee escalation.
    let command = fixture.command(Vec::new(), Duration::from_secs(3));
    let (outcome, events) = run_collect(&command, &cancelled).expect("setsid timeout containment");
    assert!(outcome.timed_out);
    assert!(outcome.containment_empty);
    assert!(events.iter().any(|event| matches!(
        event,
        ScriptLifecycleEvent::ForcedTerminationRequested { .. }
    )));
    assert_containment_terminal(&events);
}

fn wait_for_path(path: &Path, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {}", path.display());
}

#[test]
#[ignore = "internal crash-driver subprocess; invoked by application_crash_after_release_recovers_live_containment"]
fn application_crash_driver() {
    if std::env::var_os("TONEPOET_CRASH_DRIVER").is_none() {
        return;
    }
    let script = PathBuf::from(std::env::var_os("TONEPOET_CRASH_SCRIPT").unwrap());
    let runtime = PathBuf::from(std::env::var_os("TONEPOET_CRASH_RUNTIME").unwrap());
    let working = PathBuf::from(std::env::var_os("TONEPOET_CRASH_WORKING").unwrap());
    let descriptor_path = PathBuf::from(
        std::env::var_os("TONEPOET_CRASH_DESCRIPTOR").unwrap(),
    );
    let released_path = PathBuf::from(std::env::var_os("TONEPOET_CRASH_RELEASED").unwrap());
    let heartbeat = PathBuf::from(std::env::var_os("TONEPOET_CRASH_HEARTBEAT").unwrap());
    let metadata = fs::metadata(&runtime).unwrap();
    let script_file = Arc::new(File::open(&script).expect("open retained crash-driver script"));
    let working_directory_file = Arc::new(
        File::open(&working).expect("open retained crash-driver working directory"),
    );
    let command = SupervisedCommand {
        token: std::env::var("TONEPOET_CRASH_TOKEN").unwrap(),
        runtime_directory: runtime,
        script_file,
        working_directory_file,
        script,
        args: Vec::new(),
        working_directory: working,
        environment: BTreeMap::from([
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            (
                "TONEPOET_HEARTBEAT".to_string(),
                heartbeat.to_string_lossy().into_owned(),
            ),
        ]),
        timeout: Duration::from_secs(60),
        runtime_identity: RuntimeDirectoryIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
        containment_preference: ContainmentPreference::Auto,
        helper_executable: Some(PathBuf::from(env!("CARGO_BIN_EXE_tonepoet"))),
    };
    let result = run_supervised(
        &command,
        || false,
        |event| {
            match event {
                ScriptLifecycleEvent::ContainmentPrepared { descriptor, .. } => {
                    fs::write(&descriptor_path, serde_json::to_vec(descriptor).unwrap()).unwrap();
                }
                ScriptLifecycleEvent::UserCodeReleased { .. } => {
                    fs::write(&released_path, b"released").unwrap();
                }
                _ => {}
            }
            Ok(())
        },
    );
    panic!("crash driver unexpectedly returned instead of being killed: {result:?}");
}

#[test]
fn application_crash_after_release_recovers_live_containment() {
    let fixture = Fixture::new(
        r#"
trap '' TERM
while :; do
    printf x >> "$TONEPOET_HEARTBEAT"
    sleep 0.05
done
"#,
    );
    let descriptor_path = fixture._temp.path().join("descriptor.json");
    let released_path = fixture._temp.path().join("released");
    let heartbeat = fixture._temp.path().join("heartbeat");
    let token = next_token();
    let test_binary = std::env::current_exe().expect("locate integration-test binary");
    let mut driver = std::process::Command::new(test_binary)
        .arg("--exact")
        .arg("application_crash_driver")
        .arg("--ignored")
        .arg("--nocapture")
        .env("TONEPOET_CRASH_DRIVER", "1")
        .env("TONEPOET_CRASH_SCRIPT", &fixture.script)
        .env("TONEPOET_CRASH_RUNTIME", &fixture.runtime)
        .env("TONEPOET_CRASH_WORKING", fixture._temp.path())
        .env("TONEPOET_CRASH_DESCRIPTOR", &descriptor_path)
        .env("TONEPOET_CRASH_RELEASED", &released_path)
        .env("TONEPOET_CRASH_HEARTBEAT", &heartbeat)
        .env("TONEPOET_CRASH_TOKEN", &token)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn crash driver");

    wait_for_path(&descriptor_path, Duration::from_secs(10));
    wait_for_path(&released_path, Duration::from_secs(10));
    wait_for_path(&heartbeat, Duration::from_secs(5));
    driver.kill().expect("simulate application SIGKILL");
    let _ = driver.wait();

    let descriptor = serde_json::from_slice(&fs::read(&descriptor_path).unwrap()).unwrap();
    let recovery = recover_supervised(&ScriptRecoveryRequest {
        token,
        runtime_directory: fixture.runtime.clone(),
        descriptor,
    })
    .expect("recover live containment after parent death");
    assert!(matches!(
        recovery,
        ScriptRecoveryOutcome::ContainmentTerminated
            | ScriptRecoveryOutcome::ContainmentAlreadyEmpty
    ));

    let size_after_recovery = fs::metadata(&heartbeat).map(|m| m.len()).unwrap_or(0);
    thread::sleep(Duration::from_millis(300));
    let size_after_wait = fs::metadata(&heartbeat).map(|m| m.len()).unwrap_or(0);
    assert_eq!(
        size_after_wait, size_after_recovery,
        "a descendant continued mutating after recovery reported containment empty"
    );
}

#[test]
fn vanished_or_pid_reused_supervisor_without_result_requires_manual_recovery() {
    let fixture = Fixture::new("exit 0");
    let cancelled = AtomicBool::new(false);
    let command = fixture.command(Vec::new(), Duration::from_secs(5));
    let (outcome, _) = run_collect(&command, &cancelled).expect("complete script");
    fs::remove_file(fixture.runtime.join("result.json")).expect("remove durable result fixture");
    let mut descriptor = outcome.descriptor;
    // Model pid reuse with a numerically valid but different start tick;
    // a malformed identity is a fail-closed protocol error, not a mismatch.
    let ticks: u64 = descriptor
        .supervisor
        .start_identity
        .parse()
        .expect("linux start identity is tick-valued");
    descriptor.supervisor.start_identity = (ticks + 1).to_string();
    let recovery = recover_supervised(&ScriptRecoveryRequest {
        token: command.token,
        runtime_directory: fixture.runtime,
        descriptor,
    })
    .expect("classify vanished/reused supervisor");
    assert!(matches!(
        recovery,
        ScriptRecoveryOutcome::ManualRecoveryRequired(_)
    ));
}
