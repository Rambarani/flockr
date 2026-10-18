//! The environment a job runs with.
//!
//! The default, `env = "login-shell"`, is the environment of the user's login
//! shell, because credentials typically live in rc files (`~/.zshrc`,
//! `~/.bash_profile`) that a daemon never sources. It is captured by running
//! `$SHELL -l -i -c '<script>'`, where the script prints a marker, `env -0`
//! and a second marker. `-i` is required because zsh reads `.zshrc` only for
//! interactive shells. Only the bytes between the markers are parsed, so
//! banners, prompt-framework escapes and other rc-file noise are ignored.
//!
//! The capture is cached in memory (10 minutes), refreshed on demand, has a
//! five second timeout, and falls back to the server environment with a
//! diagnostic. Values never reach disk or logs: only counts and diagnostics
//! are recorded.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::definition::{EnvMode, JobDef};
use crate::api::schema::JobEnvStatus;

pub(crate) const LOGIN_ENV_TTL: Duration = Duration::from_secs(10 * 60);
/// A failed capture is retried sooner than a good one expires.
pub(crate) const FALLBACK_ENV_TTL: Duration = Duration::from_secs(60);
pub(crate) const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);
const CAPTURE_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
/// Variables a shell sets about itself; they would be wrong for the job.
const SHELL_LOCAL_VARS: &[&str] = &["_", "SHLVL", "PWD", "OLDPWD"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnvSource {
    LoginShell,
    ServerFallback,
}

#[derive(Clone)]
pub(crate) struct EnvSnapshot {
    pub(crate) vars: Arc<Vec<(String, String)>>,
    pub(crate) source: EnvSource,
    captured_at: Instant,
    pub(crate) diagnostic: Option<String>,
}

// Deliberately no `Debug`: the variable values must not reach a log by accident.
impl std::fmt::Debug for EnvSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvSnapshot")
            .field("variable_count", &self.vars.len())
            .field("source", &self.source)
            .field("diagnostic", &self.diagnostic)
            .finish()
    }
}

impl EnvSnapshot {
    fn ttl(&self) -> Duration {
        match self.source {
            EnvSource::LoginShell => LOGIN_ENV_TTL,
            EnvSource::ServerFallback => FALLBACK_ENV_TTL,
        }
    }

    /// `login-shell (214 variables)` or `server environment (login shell
    /// capture failed: ...)`. Safe to store and display.
    pub(crate) fn note(&self) -> String {
        match (self.source, &self.diagnostic) {
            (EnvSource::LoginShell, _) => {
                format!("login-shell ({} variables)", self.vars.len())
            }
            (EnvSource::ServerFallback, Some(diagnostic)) => {
                format!("server environment (login shell capture failed: {diagnostic})")
            }
            (EnvSource::ServerFallback, None) => "server environment".into(),
        }
    }
}

type Capturer = dyn Fn() -> Result<Vec<(String, String)>, String> + Send + Sync;

/// In-memory cache of the login-shell environment, shared by every job run.
pub(crate) struct LoginEnvCache {
    capture: Box<Capturer>,
    snapshot: Mutex<Option<EnvSnapshot>>,
    status: Mutex<JobEnvStatus>,
    refreshing: AtomicBool,
}

impl LoginEnvCache {
    pub(crate) fn new(capture: Box<Capturer>) -> Self {
        Self {
            capture,
            snapshot: Mutex::new(None),
            status: Mutex::new(JobEnvStatus {
                source: "none".into(),
                captured_unix_ms: None,
                variable_count: 0,
                refreshing: false,
                diagnostic: None,
            }),
            refreshing: AtomicBool::new(false),
        }
    }

    /// The real cache: `$SHELL -l -i -c` with the standard timeout.
    pub(crate) fn system() -> Self {
        Self::new(Box::new(|| {
            let shell = std::env::var("SHELL")
                .ok()
                .filter(|shell| !shell.trim().is_empty())
                .ok_or_else(|| "SHELL is not set".to_string())?;
            capture_login_shell_env(&shell, CAPTURE_TIMEOUT)
        }))
    }

    /// The cached environment, capturing it first when missing or expired.
    /// Blocks while a capture runs; other callers wait for the same capture.
    pub(crate) fn get(&self) -> EnvSnapshot {
        let mut guard = self
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(snapshot) = guard.as_ref() {
            if snapshot.captured_at.elapsed() < snapshot.ttl() {
                return snapshot.clone();
            }
        }
        let snapshot = self.capture_now();
        *guard = Some(snapshot.clone());
        snapshot
    }

    /// The cached environment if there is one, however old; never captures.
    pub(crate) fn peek(&self) -> Option<EnvSnapshot> {
        self.snapshot
            .try_lock()
            .ok()
            .and_then(|guard| guard.as_ref().cloned())
    }

    /// Capture again regardless of age. Used by `flockr job env refresh`.
    pub(crate) fn refresh(&self) -> EnvSnapshot {
        let mut guard = self
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let snapshot = self.capture_now();
        *guard = Some(snapshot.clone());
        snapshot
    }

    pub(crate) fn mark_refreshing(&self) {
        self.refreshing.store(true, Ordering::SeqCst);
        self.status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .refreshing = true;
    }

    fn capture_now(&self) -> EnvSnapshot {
        self.refreshing.store(true, Ordering::SeqCst);
        let captured_at = Instant::now();
        let captured_unix_ms = super::now_unix_ms();
        let (vars, source, diagnostic) = match (self.capture)() {
            Ok(vars) => (vars, EnvSource::LoginShell, None),
            Err(error) => {
                tracing::warn!(error = %error, "login-shell environment capture failed; jobs use the server environment");
                (server_env(), EnvSource::ServerFallback, Some(error))
            }
        };
        let snapshot = EnvSnapshot {
            vars: Arc::new(vars),
            source,
            captured_at,
            diagnostic,
        };
        *self
            .status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = JobEnvStatus {
            source: match source {
                EnvSource::LoginShell => "login-shell".into(),
                EnvSource::ServerFallback => "server-fallback".into(),
            },
            captured_unix_ms: Some(captured_unix_ms),
            variable_count: u32::try_from(snapshot.vars.len()).unwrap_or(u32::MAX),
            refreshing: false,
            diagnostic: snapshot.diagnostic.clone(),
        };
        self.refreshing.store(false, Ordering::SeqCst);
        snapshot
    }

    /// Counts and diagnostics only; never blocks on a running capture.
    pub(crate) fn status(&self) -> JobEnvStatus {
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        status.refreshing = self.refreshing.load(Ordering::SeqCst);
        status
    }
}

fn server_env() -> Vec<(String, String)> {
    std::env::vars().collect()
}

/// A marker that cannot appear in rc-file output by accident.
fn capture_markers() -> (String, String) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let nonce = format!("{:x}{:x}", std::process::id(), nanos);
    (
        format!("__FLOCKR_ENV_BEGIN_{nonce}__"),
        format!("__FLOCKR_ENV_END_{nonce}__"),
    )
}

/// The `-c` script: marker, NUL-separated environment, marker.
pub(crate) fn capture_script(begin: &str, end: &str) -> String {
    format!("printf '%s' {begin}; command env -0; printf '%s' {end}")
}

/// Extract the variables between the markers. Everything outside them
/// (banners, escape sequences, trailing output) is ignored.
pub(crate) fn parse_env_dump(
    output: &[u8],
    begin: &str,
    end: &str,
) -> Result<Vec<(String, String)>, String> {
    let begin_at = find(output, begin.as_bytes(), 0).ok_or_else(|| {
        "the shell never printed the start marker (it may have exited early)".to_string()
    })?;
    let body_start = begin_at + begin.len();
    let end_at = find(output, end.as_bytes(), body_start).ok_or_else(|| {
        "the shell did not print the end marker (it may have timed out or exited early)".to_string()
    })?;
    let mut vars = Vec::new();
    for entry in output[body_start..end_at].split(|byte| *byte == 0) {
        let Some(split) = entry.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        if split == 0 {
            continue;
        }
        let name = String::from_utf8_lossy(&entry[..split]).into_owned();
        if SHELL_LOCAL_VARS.contains(&name.as_str()) {
            continue;
        }
        vars.push((
            name,
            String::from_utf8_lossy(&entry[split + 1..]).into_owned(),
        ));
    }
    if vars.is_empty() {
        return Err("no variables were printed between the markers".into());
    }
    Ok(vars)
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (from..=haystack.len() - needle.len())
        .find(|&index| &haystack[index..index + needle.len()] == needle)
}

/// Run `shell -l -i -c <script>` and return its environment. Errors are short
/// diagnostics suitable for display; they never contain variable values.
pub(crate) fn capture_login_shell_env(
    shell: &str,
    timeout: Duration,
) -> Result<Vec<(String, String)>, String> {
    let (begin, end) = capture_markers();
    let script = capture_script(&begin, &end);
    let mut command = crate::noninteractive_process::command(shell);
    command
        .args(["-l", "-i", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    crate::platform::configure_detached_session_command(&mut command);
    let mut child = command
        .spawn()
        .map_err(|err| format!("could not start {shell}: {err}"))?;
    let process_id = child.id();
    let output = Arc::new(Mutex::new(Vec::<u8>::new()));
    if let Some(mut stdout) = child.stdout.take() {
        let output = Arc::clone(&output);
        let reader = crate::thread_spawn::spawn_named("flockr-job-env-read", move || {
            let mut buffer = [0u8; 8192];
            loop {
                match stdout.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        let mut output = output
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        if output.len() < CAPTURE_OUTPUT_LIMIT {
                            output.extend_from_slice(&buffer[..count]);
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        });
        if let Err(err) = reader {
            crate::platform::terminate_process_group(process_id, true);
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("could not read shell output: {err}"));
        }
    }

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() >= timeout => break None,
            Ok(None) => std::thread::sleep(Duration::from_millis(15)),
            Err(err) => {
                crate::platform::terminate_process_group(process_id, true);
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("could not wait for the shell: {err}"));
            }
        }
    };
    let Some(status) = status else {
        crate::platform::terminate_process_group(process_id, true);
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!(
            "{shell} did not finish within {}s (a slow rc file?)",
            timeout.as_secs()
        ));
    };
    // The reader may still be draining, or a daemon started by an rc file may
    // hold the pipe open; wait for the end marker, not for EOF.
    let drain_deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let complete = {
            let output = output
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            find(&output, end.as_bytes(), 0).is_some()
        };
        if complete || Instant::now() >= drain_deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Clean up anything the rc files left running in the shell's session.
    crate::platform::terminate_process_group(process_id, true);
    let output = output
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    parse_env_dump(&output, &begin, &end).map_err(|err| {
        if status.success() {
            err
        } else {
            format!("{err}; the shell exited with {status}")
        }
    })
}

/// Parse a dotenv-style file: `KEY=VALUE` lines, optional `export `, `#`
/// comments, and single or double quotes around values.
pub(crate) fn parse_env_file(content: &str) -> Result<Vec<(String, String)>, String> {
    let mut vars = Vec::new();
    for (index, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((name, value)) = line.split_once('=') else {
            return Err(format!("line {}: expected KEY=VALUE", index + 1));
        };
        let name = name.trim();
        let valid = !name.is_empty()
            && !name.starts_with(|ch: char| ch.is_ascii_digit())
            && name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
        if !valid {
            return Err(format!(
                "line {}: '{name}' is not a valid variable name",
                index + 1
            ));
        }
        let value = value.trim();
        let value = match value.as_bytes() {
            [b'"', .., b'"'] | [b'\'', .., b'\''] if value.len() >= 2 => &value[1..value.len() - 1],
            _ => value,
        };
        vars.push((name.to_owned(), value.to_owned()));
    }
    Ok(vars)
}

/// Everything needed to start a job's process, assembled at run time.
pub(crate) struct ResolvedEnv {
    /// When true the child starts from an empty environment.
    pub(crate) clear: bool,
    pub(crate) vars: Vec<(String, String)>,
    /// Safe to store: no values.
    pub(crate) note: String,
}

/// Compose a job's environment: the base (login shell or server), then
/// `env_file`, then the `[env]` table, then flockr's own `FLOCKR_*` variables.
pub(crate) fn resolve_job_env(
    job: &JobDef,
    cache: &LoginEnvCache,
    flockr_vars: Vec<(String, String)>,
) -> Result<ResolvedEnv, String> {
    resolve_job_env_with(job, || Some(cache.get()), flockr_vars)
}

/// Like `resolve_job_env`, for callers that must not wait for a login-shell
/// capture: an environment that was never captured contributes nothing (the
/// shell the caller starts supplies its own).
pub(crate) fn resolve_job_env_cached(
    job: &JobDef,
    cache: &LoginEnvCache,
    flockr_vars: Vec<(String, String)>,
) -> Result<ResolvedEnv, String> {
    resolve_job_env_with(job, || cache.peek(), flockr_vars)
}

fn resolve_job_env_with(
    job: &JobDef,
    login_snapshot: impl FnOnce() -> Option<EnvSnapshot>,
    flockr_vars: Vec<(String, String)>,
) -> Result<ResolvedEnv, String> {
    let mut merged: BTreeMap<String, String> = BTreeMap::new();
    let (clear, mut note) = match job.env {
        EnvMode::Server => (false, "server environment".to_owned()),
        EnvMode::LoginShell => match login_snapshot() {
            Some(snapshot) => {
                for (name, value) in snapshot.vars.iter() {
                    merged.insert(name.clone(), value.clone());
                }
                (true, snapshot.note())
            }
            None => (true, "login shell not captured yet".to_owned()),
        },
    };
    if let Some(path) = &job.env_file {
        let content = std::fs::read_to_string(path)
            .map_err(|err| format!("env_file {}: {err}", path.display()))?;
        let vars = parse_env_file(&content)
            .map_err(|err| format!("env_file {}: {err}", path.display()))?;
        note.push_str(&format!(" + env_file ({} variables)", vars.len()));
        merged.extend(vars);
    }
    if !job.env_overrides.is_empty() {
        note.push_str(&format!(" + {} overrides", job.env_overrides.len()));
        merged.extend(job.env_overrides.clone());
    }
    merged.extend(flockr_vars);
    Ok(ResolvedEnv {
        clear,
        vars: merged.into_iter().collect(),
        note,
    })
}

/// The variables flockr sets for every job.
pub(crate) fn flockr_job_vars(job: &JobDef, state_dir: &Path) -> Vec<(String, String)> {
    let mut vars = vec![
        ("FLOCKR_ENV".to_owned(), "1".to_owned()),
        (
            crate::api::SOCKET_PATH_ENV_VAR.to_owned(),
            crate::api::socket_path().display().to_string(),
        ),
        ("FLOCKR_JOB_ID".to_owned(), job.id.clone()),
        ("FLOCKR_JOB_DIR".to_owned(), job.dir.display().to_string()),
        (
            "FLOCKR_JOB_STATE_DIR".to_owned(),
            state_dir.display().to_string(),
        ),
    ];
    if let Ok(current_exe) = crate::platform::launch_executable() {
        vars.push((
            "FLOCKR_BIN_PATH".to_owned(),
            current_exe.display().to_string(),
        ));
    }
    if let super::definition::JobSource::Plugin {
        plugin_id,
        plugin_root,
    } = &job.source
    {
        vars.push(("FLOCKR_PLUGIN_ID".to_owned(), plugin_id.clone()));
        vars.push((
            "FLOCKR_PLUGIN_ROOT".to_owned(),
            plugin_root.display().to_string(),
        ));
        vars.push((
            "FLOCKR_PLUGIN_CONFIG_DIR".to_owned(),
            crate::plugin_paths::plugin_config_dir(plugin_id)
                .display()
                .to_string(),
        ));
        vars.push((
            "FLOCKR_PLUGIN_STATE_DIR".to_owned(),
            crate::plugin_paths::plugin_state_dir(plugin_id)
                .display()
                .to_string(),
        ));
    }
    vars
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "flockr-jobs-env-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A stand-in for `$SHELL` that prints noise around the real script, the
    /// way prompt frameworks and rc files do.
    fn fake_shell(dir: &Path, body: &str) -> String {
        let path = dir.join("fake-shell");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    #[test]
    fn parses_only_between_markers_and_ignores_noise() {
        let dump = b"Welcome!\x1b[1mfancy\x1b[0m\n__B__A=1\0B=two words\0MULTI=line1\nline2\0EMPTY=\0_=/usr/bin/env\0SHLVL=3\0PWD=/x\0not-a-var\0=novar\0__E__trailing __B__ junk";
        let vars = parse_env_dump(dump, "__B__", "__E__").unwrap();
        assert_eq!(
            vars,
            vec![
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "two words".to_string()),
                ("MULTI".to_string(), "line1\nline2".to_string()),
                ("EMPTY".to_string(), String::new()),
            ]
        );
    }

    #[test]
    fn missing_markers_are_diagnosed_without_values() {
        let no_begin = parse_env_dump(b"A=secret\0", "__B__", "__E__").unwrap_err();
        assert!(no_begin.contains("start marker"));
        assert!(!no_begin.contains("secret"));
        let no_end = parse_env_dump(b"__B__A=secret\0", "__B__", "__E__").unwrap_err();
        assert!(no_end.contains("end marker"));
        assert!(!no_end.contains("secret"));
        let empty = parse_env_dump(b"__B____E__", "__B__", "__E__").unwrap_err();
        assert!(empty.contains("no variables"));
    }

    #[test]
    fn captures_through_a_noisy_login_shell() {
        let dir = temp_dir("noisy");
        let shell = fake_shell(
            &dir,
            r#"echo "motd: welcome back"
printf '\033]1337;ShellIntegration=1\007'
export FLOCKR_TEST_TOKEN=hunter2
export FLOCKR_TEST_MULTI='a
b'
eval "$4"
echo "bye"
"#,
        );
        let vars = capture_login_shell_env(&shell, Duration::from_secs(5)).unwrap();
        let value = |name: &str| {
            vars.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(value("FLOCKR_TEST_TOKEN"), Some("hunter2"));
        assert_eq!(value("FLOCKR_TEST_MULTI"), Some("a\nb"));
        assert!(value("PATH").is_some(), "inherited variables are included");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_shell_that_hangs_times_out_and_is_killed() {
        let dir = temp_dir("hang");
        let shell = fake_shell(&dir, "sleep 30");
        let started = Instant::now();
        let error = capture_login_shell_env(&shell, Duration::from_millis(300)).unwrap_err();
        assert!(error.contains("did not finish"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_shell_that_cannot_start_is_a_diagnostic() {
        let error =
            capture_login_shell_env("/nonexistent/shell", Duration::from_secs(1)).unwrap_err();
        assert!(error.contains("could not start"), "{error}");
    }

    #[test]
    fn a_daemon_holding_the_pipe_open_does_not_hang_the_capture() {
        let dir = temp_dir("daemon");
        // The background sleep inherits stdout and outlives the shell.
        let shell = fake_shell(&dir, "sleep 30 &\neval \"$4\"");
        let started = Instant::now();
        let vars = capture_login_shell_env(&shell, Duration::from_secs(5)).unwrap();
        assert!(!vars.is_empty());
        assert!(started.elapsed() < Duration::from_secs(4));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cache_reuses_a_capture_until_it_is_refreshed() {
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = Arc::clone(&counter);
        let cache = LoginEnvCache::new(Box::new(move || {
            let n = seen.fetch_add(1, Ordering::SeqCst);
            Ok(vec![("CAPTURE".to_string(), n.to_string())])
        }));
        assert_eq!(cache.status().source, "none");
        let first = cache.get();
        let second = cache.get();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert_eq!(first.vars, second.vars);
        assert_eq!(first.source, EnvSource::LoginShell);
        let status = cache.status();
        assert_eq!(status.source, "login-shell");
        assert_eq!(status.variable_count, 1);
        assert!(!status.refreshing);
        let refreshed = cache.refresh();
        assert_eq!(counter.load(Ordering::SeqCst), 2);
        assert_eq!(refreshed.vars[0].1, "1");
        assert_eq!(cache.get().vars[0].1, "1");
    }

    #[test]
    fn cache_entries_expire_after_the_ttl() {
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = Arc::clone(&counter);
        let cache = LoginEnvCache::new(Box::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(vec![("A".to_string(), "1".to_string())])
        }));
        let mut snapshot = cache.get();
        // Age the entry past the TTL.
        snapshot.captured_at = Instant::now() - LOGIN_ENV_TTL - Duration::from_secs(1);
        *cache.snapshot.lock().unwrap() = Some(snapshot);
        cache.get();
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn failed_capture_falls_back_to_the_server_environment_with_a_diagnostic() {
        std::env::set_var("FLOCKR_TEST_SERVER_VAR", "present");
        let cache = LoginEnvCache::new(Box::new(|| Err("shell timed out".to_string())));
        let snapshot = cache.get();
        assert_eq!(snapshot.source, EnvSource::ServerFallback);
        assert!(snapshot
            .vars
            .iter()
            .any(|(name, value)| name == "FLOCKR_TEST_SERVER_VAR" && value == "present"));
        assert_eq!(snapshot.diagnostic.as_deref(), Some("shell timed out"));
        let status = cache.status();
        assert_eq!(status.source, "server-fallback");
        assert_eq!(status.diagnostic.as_deref(), Some("shell timed out"));
        assert!(snapshot
            .note()
            .contains("login shell capture failed: shell timed out"));
    }

    #[test]
    fn captured_values_never_reach_status_notes_debug_or_logs() {
        let secret = "s3cr3t-value-9f2c";
        let cache = LoginEnvCache::new(Box::new(move || {
            Ok(vec![("API_TOKEN".to_string(), secret.to_string())])
        }));
        let logs = Arc::new(Mutex::new(Vec::<u8>::new()));
        let writer = {
            let logs = Arc::clone(&logs);
            move || LogWriter(Arc::clone(&logs))
        };
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(writer)
            .finish();
        let snapshot = tracing::subscriber::with_default(subscriber, || {
            let snapshot = cache.refresh();
            let _ = format!("{snapshot:?}");
            snapshot
        });
        let status = serde_json::to_string(&cache.status()).unwrap();
        let rendered = [
            format!("{snapshot:?}"),
            snapshot.note(),
            status,
            String::from_utf8_lossy(&logs.lock().unwrap()).into_owned(),
        ]
        .join("\n");
        assert!(!rendered.contains(secret), "secret leaked: {rendered}");
        assert!(snapshot.vars.iter().any(|(_, value)| value == secret));
    }

    struct LogWriter(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for LogWriter {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn env_file_parsing() {
        let vars = parse_env_file(
            "# comment\n\nA=1\nexport B=\"two words\"\nC='three'\nD = spaced \nE=\n",
        )
        .unwrap();
        assert_eq!(
            vars,
            vec![
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "two words".to_string()),
                ("C".to_string(), "three".to_string()),
                ("D".to_string(), "spaced".to_string()),
                ("E".to_string(), String::new()),
            ]
        );
        assert!(parse_env_file("novalue\n").unwrap_err().contains("line 1"));
        assert!(parse_env_file("1A=x\n")
            .unwrap_err()
            .contains("not a valid variable name"));
    }

    fn job(env: EnvMode, dir: &Path) -> JobDef {
        crate::jobs::definition::parse_job_toml(
            &format!(
                "command = [\"x\"]\nevery = \"1h\"\nenv_file = \"extra.env\"\n[env]\nmode = \"{}\"\nOVERRIDE = \"job\"\n",
                env.as_str()
            ),
            "demo",
            dir,
        )
        .unwrap()
    }

    #[test]
    fn precedence_is_base_then_env_file_then_overrides_then_flockr() {
        let dir = temp_dir("precedence");
        std::fs::write(
            dir.join("extra.env"),
            "FROM_FILE=file\nSHARED=file\nOVERRIDE=file\n",
        )
        .unwrap();
        let cache = LoginEnvCache::new(Box::new(|| {
            Ok(vec![
                ("FROM_SHELL".to_string(), "shell".to_string()),
                ("SHARED".to_string(), "shell".to_string()),
            ])
        }));
        let resolved = resolve_job_env(
            &job(EnvMode::LoginShell, &dir),
            &cache,
            vec![("FLOCKR_JOB_ID".to_string(), "demo".to_string())],
        )
        .unwrap();
        assert!(resolved.clear);
        let map: BTreeMap<_, _> = resolved.vars.iter().cloned().collect();
        assert_eq!(map["FROM_SHELL"], "shell");
        assert_eq!(map["FROM_FILE"], "file");
        assert_eq!(map["SHARED"], "file");
        assert_eq!(map["OVERRIDE"], "job");
        assert_eq!(map["FLOCKR_JOB_ID"], "demo");
        assert!(resolved.note.starts_with("login-shell (2 variables)"));
        assert!(resolved.note.contains("env_file (3 variables)"));
        assert!(resolved.note.contains("1 overrides"));

        let server = resolve_job_env(&job(EnvMode::Server, &dir), &cache, Vec::new()).unwrap();
        assert!(!server.clear);
        assert!(server.vars.iter().all(|(name, _)| name != "FROM_SHELL"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_env_file_fails_the_run_with_a_clear_message() {
        let dir = temp_dir("missing-env-file");
        let cache = LoginEnvCache::new(Box::new(|| Ok(vec![("A".to_string(), "1".to_string())])));
        let error = resolve_job_env(&job(EnvMode::Server, &dir), &cache, Vec::new())
            .err()
            .unwrap();
        assert!(error.starts_with("env_file "), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
