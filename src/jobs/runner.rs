//! Runs one job command: a non-pty child with an argv command, its own
//! process group, capped output tails and a timeout that kills the whole
//! group. It follows the plugin command runner (`app/api/plugins/runtime.rs`):
//! a named worker thread reports through an `AppEvent`.

use std::io::Read;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::definition::format_duration;
use super::env::{LoginEnvCache, ResolvedEnv};
use super::state::{RunRecord, TailBuffer, OUTPUT_TAIL_BYTES};
use crate::api::schema::{JobRunInfo, JobRunStatus, JobRunTrigger};

/// How long a timed-out job gets to exit after SIGTERM before SIGKILL.
pub(crate) const TERMINATE_GRACE: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
/// How long to wait for output readers after the command exits. A descendant
/// that outlives the command can hold the pipes open.
const READER_DRAIN: Duration = Duration::from_millis(500);

pub(crate) struct ExecSpec {
    pub(crate) command: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) timeout: Duration,
    pub(crate) env_clear: bool,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) terminate_grace: Duration,
}

pub(crate) struct ExecResult {
    pub(crate) status: JobRunStatus,
    pub(crate) exit_code: Option<i32>,
    pub(crate) summary: String,
    pub(crate) error: Option<String>,
    pub(crate) stdout: String,
    pub(crate) stdout_truncated: bool,
    pub(crate) stderr: String,
    pub(crate) stderr_truncated: bool,
    pub(crate) duration: Duration,
}

impl ExecResult {
    fn spawn_failed(error: String, duration: Duration) -> Self {
        Self {
            status: JobRunStatus::SpawnFailed,
            exit_code: None,
            summary: error.clone(),
            error: Some(error),
            stdout: String::new(),
            stdout_truncated: false,
            stderr: String::new(),
            stderr_truncated: false,
            duration,
        }
    }
}

struct Stream {
    tail: Arc<Mutex<TailBuffer>>,
    done: Arc<AtomicBool>,
}

fn spawn_reader(name: &str, mut source: impl Read + Send + 'static) -> std::io::Result<Stream> {
    let tail = Arc::new(Mutex::new(TailBuffer::new(OUTPUT_TAIL_BYTES)));
    let done = Arc::new(AtomicBool::new(false));
    let worker_tail = Arc::clone(&tail);
    let worker_done = Arc::clone(&done);
    crate::thread_spawn::spawn_named(name, move || {
        let mut buffer = [0u8; 8192];
        loop {
            match source.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => worker_tail
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(&buffer[..count]),
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        worker_done.store(true, Ordering::SeqCst);
    })?;
    Ok(Stream { tail, done })
}

/// Execute the command and wait for it, enforcing the timeout.
pub(crate) fn execute(spec: &ExecSpec) -> ExecResult {
    let started = Instant::now();
    let Some(program) = spec.command.first() else {
        return ExecResult::spawn_failed("command is empty".into(), started.elapsed());
    };
    if !spec.cwd.is_dir() {
        return ExecResult::spawn_failed(
            format!("working directory {} does not exist", spec.cwd.display()),
            started.elapsed(),
        );
    }
    let args = spec.command[1..].to_vec();
    let mut command = crate::plugin_command::command_for_argv_in_dir(program, &args, &spec.cwd);
    if spec.env_clear {
        command.env_clear();
    }
    command
        .envs(spec.env.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::platform::configure_job_command(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return ExecResult::spawn_failed(
                format!("could not start {program}: {err}"),
                started.elapsed(),
            )
        }
    };
    let process_id = child.id();
    let stdout = child
        .stdout
        .take()
        .map(|pipe| spawn_reader("flockr-job-stdout", pipe));
    let stderr = child
        .stderr
        .take()
        .map(|pipe| spawn_reader("flockr-job-stderr", pipe));
    let (stdout, stderr) = match (stdout.transpose(), stderr.transpose()) {
        (Ok(stdout), Ok(stderr)) => (stdout, stderr),
        (Err(err), _) | (_, Err(err)) => {
            // Without a reader the child could block on a full pipe forever.
            crate::platform::terminate_process_group(process_id, true);
            let _ = child.kill();
            let _ = child.wait();
            return ExecResult::spawn_failed(
                format!("could not start output reader: {err}"),
                started.elapsed(),
            );
        }
    };

    let mut timed_out = false;
    let wait_result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() >= spec.timeout => {
                timed_out = true;
                crate::platform::terminate_process_group(process_id, false);
                let grace_deadline = Instant::now() + spec.terminate_grace;
                let status = loop {
                    match child.try_wait() {
                        Ok(Some(status)) => break Ok(status),
                        Ok(None) if Instant::now() >= grace_deadline => {
                            crate::platform::terminate_process_group(process_id, true);
                            let _ = child.kill();
                            break child.wait();
                        }
                        Ok(None) => std::thread::sleep(POLL_INTERVAL),
                        Err(err) => break Err(err),
                    }
                };
                // Stop stragglers the command left behind in its group.
                crate::platform::terminate_process_group(process_id, true);
                break status;
            }
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
            Err(err) => break Err(err),
        }
    };
    // The run ends when the command does, not when its output closes.
    let duration = started.elapsed();
    let drain_deadline = Instant::now() + READER_DRAIN;
    for stream in [&stdout, &stderr].into_iter().flatten() {
        while !stream.done.load(Ordering::SeqCst) && Instant::now() < drain_deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let read_tail = |stream: &Option<Stream>| match stream {
        Some(stream) => stream
            .tail
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .text(),
        None => (String::new(), false),
    };
    let (stdout_text, stdout_truncated) = read_tail(&stdout);
    let (stderr_text, stderr_truncated) = read_tail(&stderr);

    let (status, exit_code, summary, error) = match wait_result {
        Err(err) => (
            JobRunStatus::Failed,
            None,
            format!("could not wait for the command: {err}"),
            Some(err.to_string()),
        ),
        Ok(_) if timed_out => (
            JobRunStatus::TimedOut,
            None,
            format!("timed out after {}", format_duration(spec.timeout)),
            None,
        ),
        Ok(status) => match status.code() {
            Some(0) => (JobRunStatus::Success, Some(0), "exit 0".to_string(), None),
            Some(code) => (
                JobRunStatus::Failed,
                Some(code),
                format!("exit {code}"),
                None,
            ),
            None => (
                JobRunStatus::Failed,
                None,
                "terminated by a signal".to_string(),
                None,
            ),
        },
    };
    ExecResult {
        status,
        exit_code,
        summary,
        error,
        stdout: stdout_text,
        stdout_truncated,
        stderr: stderr_text,
        stderr_truncated,
        duration,
    }
}

/// Everything the worker thread needs; owns its data so it can outlive the
/// manager that created it.
#[derive(Debug)]
pub(crate) struct RunPlan {
    pub(crate) job: super::definition::JobDef,
    pub(crate) run_id: u64,
    pub(crate) trigger: JobRunTrigger,
    pub(crate) state_dir: PathBuf,
    pub(crate) started_unix_ms: u64,
}

/// A finished run, delivered to the app loop.
#[derive(Debug)]
pub(crate) struct JobRunResult {
    pub(crate) job_id: String,
    pub(crate) record: RunRecord,
}

/// Resolve the environment, run the command and build the record. Blocking:
/// call from a worker thread.
pub(crate) fn run_plan(plan: &RunPlan, cache: &LoginEnvCache) -> JobRunResult {
    let started = Instant::now();
    let record = |result: ExecResult, env_note: Option<String>| RunRecord {
        info: JobRunInfo {
            run_id: plan.run_id,
            trigger: plan.trigger,
            status: result.status,
            started_unix_ms: plan.started_unix_ms,
            finished_unix_ms: super::now_unix_ms().max(plan.started_unix_ms),
            duration_ms: u64::try_from(result.duration.as_millis()).unwrap_or(u64::MAX),
            exit_code: result.exit_code,
            summary: result.summary,
            error: result.error,
            skipped_count: 0,
            env_note,
        },
        stdout_tail: result.stdout,
        stderr_tail: result.stderr,
        stdout_truncated: result.stdout_truncated,
        stderr_truncated: result.stderr_truncated,
        handoff: None,
    };
    let flockr_vars = super::env::flockr_job_vars(&plan.job, &plan.state_dir);
    let record = match super::env::resolve_job_env(&plan.job, cache, flockr_vars) {
        Err(error) => record(ExecResult::spawn_failed(error, started.elapsed()), None),
        Ok(ResolvedEnv { clear, vars, note }) => {
            if let super::definition::JobSource::Plugin { plugin_id, .. } = &plan.job.source {
                let _ = crate::plugin_paths::ensure_plugin_user_dirs(plugin_id);
            }
            let _ = std::fs::create_dir_all(&plan.state_dir);
            let result = execute(&ExecSpec {
                command: plan.job.command.clone(),
                cwd: plan.job.cwd.clone(),
                timeout: plan.job.timeout,
                env_clear: clear,
                env: vars,
                terminate_grace: TERMINATE_GRACE,
            });
            record(result, Some(note))
        }
    };
    let mut record = record;
    record.handoff = super::state::find_handoff_line(&record.stdout_tail);
    JobRunResult {
        job_id: plan.job.id.clone(),
        record,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "flockr-jobs-run-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn spec(dir: &std::path::Path, script: &str) -> ExecSpec {
        ExecSpec {
            command: vec!["sh".into(), "-c".into(), script.into()],
            cwd: dir.to_path_buf(),
            timeout: Duration::from_secs(10),
            env_clear: false,
            env: Vec::new(),
            terminate_grace: Duration::from_millis(300),
        }
    }

    fn process_alive(pid: i32) -> bool {
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[test]
    fn captures_stdout_stderr_and_a_zero_exit() {
        let dir = temp_dir("ok");
        let result = execute(&spec(&dir, "echo out; echo err >&2"));
        assert_eq!(result.status, JobRunStatus::Success);
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.summary, "exit 0");
        assert_eq!(result.stdout, "out\n");
        assert_eq!(result.stderr, "err\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_nonzero_exit_is_a_failure_with_its_code() {
        let dir = temp_dir("fail");
        let result = execute(&spec(&dir, "echo boom >&2; exit 3"));
        assert_eq!(result.status, JobRunStatus::Failed);
        assert_eq!(result.exit_code, Some(3));
        assert_eq!(result.summary, "exit 3");
        assert_eq!(result.stderr, "boom\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_command_runs_in_the_given_directory() {
        let dir = temp_dir("cwd");
        std::fs::write(dir.join("marker.txt"), "x").unwrap();
        let result = execute(&spec(&dir, "ls"));
        assert!(result.stdout.contains("marker.txt"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn timeout_kills_the_whole_process_group() {
        let dir = temp_dir("timeout");
        let pid_file = dir.join("child.pid");
        // The shell starts a background sleeper in the same group, then waits.
        let mut run = spec(
            &dir,
            &format!("sleep 60 & echo $! > {}; wait", pid_file.display()),
        );
        run.timeout = Duration::from_millis(400);
        let started = Instant::now();
        let result = execute(&run);
        assert_eq!(result.status, JobRunStatus::TimedOut);
        assert_eq!(result.exit_code, None);
        assert_eq!(result.summary, "timed out after 400ms");
        assert!(started.elapsed() < Duration::from_secs(5));
        let background: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // Give the kernel a moment to reap the killed descendant.
        let deadline = Instant::now() + Duration::from_secs(3);
        while process_alive(background) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!process_alive(background), "background descendant survived");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_command_that_ignores_sigterm_is_killed_after_the_grace_period() {
        let dir = temp_dir("stubborn");
        let mut run = spec(&dir, "trap '' TERM; while true; do sleep 1; done");
        run.timeout = Duration::from_millis(200);
        run.terminate_grace = Duration::from_millis(300);
        let started = Instant::now();
        let result = execute(&run);
        assert_eq!(result.status, JobRunStatus::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn huge_output_is_capped_to_a_tail_and_flagged() {
        let dir = temp_dir("huge");
        let result = execute(&spec(
            &dir,
            "i=0; while [ $i -lt 40000 ]; do echo line-$i-0123456789; i=$((i+1)); done",
        ));
        assert_eq!(result.status, JobRunStatus::Success);
        assert!(result.stdout_truncated);
        assert!(result.stdout.len() <= OUTPUT_TAIL_BYTES);
        assert!(result.stdout.contains("line-39999-0123456789"));
        assert!(!result.stdout.contains("line-0-0123456789"));
        assert!(!result.stderr_truncated);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_program_and_a_missing_directory_are_spawn_failures() {
        let dir = temp_dir("spawn");
        let mut missing = spec(&dir, "true");
        missing.command = vec!["/definitely/not/here".into()];
        let result = execute(&missing);
        assert_eq!(result.status, JobRunStatus::SpawnFailed);
        assert!(result
            .summary
            .starts_with("could not start /definitely/not/here"));

        let mut nowhere = spec(&dir, "true");
        nowhere.cwd = dir.join("nope");
        let result = execute(&nowhere);
        assert_eq!(result.status, JobRunStatus::SpawnFailed);
        assert!(result.summary.contains("does not exist"));

        let mut empty = spec(&dir, "true");
        empty.command.clear();
        assert_eq!(execute(&empty).status, JobRunStatus::SpawnFailed);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_cleared_environment_contains_only_what_was_given() {
        let dir = temp_dir("env");
        std::env::set_var("FLOCKR_TEST_PARENT_ONLY", "leak");
        let mut run = spec(
            &dir,
            "echo \"${FLOCKR_TEST_PARENT_ONLY:-unset}:${GIVEN:-unset}\"",
        );
        run.env_clear = true;
        run.env = vec![
            ("GIVEN".into(), "yes".into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ];
        let result = execute(&run);
        assert_eq!(result.stdout.trim(), "unset:yes");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn program_lookup_uses_the_job_path() {
        let dir = temp_dir("path");
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let tool = bin.join("flockr-job-test-tool");
        std::fs::write(&tool, "#!/bin/sh\necho found-it\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut run = spec(&dir, "true");
        run.command = vec!["flockr-job-test-tool".into()];
        run.env_clear = true;
        run.env = vec![("PATH".into(), bin.display().to_string())];
        let result = execute(&run);
        assert_eq!(result.status, JobRunStatus::Success, "{}", result.summary);
        assert_eq!(result.stdout.trim(), "found-it");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
