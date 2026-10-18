//! Authoring kit for jobs: scaffold a job (`flockr job new`), check it
//! (`flockr job validate`), try it without side effects (`flockr job run
//! --dry-run`), and publish the JSON Schemas and the Claude skill that
//! describe the file formats. Everything here runs locally and needs no
//! server: it reuses the parsers and evaluators the server itself uses, so
//! what passes here behaves the same when the schedule runs it.

pub(crate) mod dry_run;
pub(crate) mod program;
pub(crate) mod scaffold;
pub(crate) mod schema;
pub(crate) mod validate;

#[cfg(all(test, unix))]
mod tests;

/// The authoring skill, bundled so the printed text always matches this
/// binary. Install it with `flockr job skill`.
pub(crate) const JOBS_SKILL: &str = include_str!("../../../skills/flockr-jobs/SKILL.md");

use std::path::{Path, PathBuf};

use super::definition::JobDef;
use super::env::{flockr_job_vars, resolve_job_env, LoginEnvCache};
use super::runner::{execute, ExecResult, ExecSpec, TERMINATE_GRACE};

/// A throwaway directory standing in for `FLOCKR_JOB_STATE_DIR` while a job
/// runs for validation or a dry run, so nothing lands in the real state
/// directory. Removed when dropped.
pub(crate) struct Scratch(PathBuf);

impl Scratch {
    pub(crate) fn new() -> std::io::Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("flockr-job-scratch-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The job's PATH for finding programs: the one in its resolved environment,
/// else the process's when the job inherits it, else a minimal default.
pub(crate) fn path_of(vars: &[(String, String)], inherits_process_env: bool) -> Option<String> {
    vars.iter()
        .find(|(name, _)| name == "PATH")
        .map(|(_, value)| value.clone())
        .or_else(|| {
            inherits_process_env
                .then(|| std::env::var("PATH").ok())
                .flatten()
        })
        .or_else(|| Some("/usr/bin:/bin".to_owned()))
}

/// Run `command` with the job's real environment rules, working directory and
/// timeout, but with a scratch state directory. `Err` is a problem resolving
/// the environment (an unreadable `env_file`); `Ok` carries the environment
/// note, which holds no values.
pub(crate) fn run_in_job_env(
    job: &JobDef,
    cache: &LoginEnvCache,
    scratch: &Scratch,
    command: Vec<String>,
    stdout_limit: usize,
) -> Result<(ExecResult, String), String> {
    let env = resolve_job_env(job, cache, flockr_job_vars(job, scratch.path()))?;
    let note = env.note.clone();
    let result = execute(&ExecSpec {
        command,
        cwd: job.cwd.clone(),
        timeout: job.timeout,
        env_clear: env.clear,
        env: env.vars,
        terminate_grace: TERMINATE_GRACE,
        stdout_limit,
    });
    Ok((result, note))
}

/// `2026-10-18 11:00` in local time, for a Unix timestamp.
pub(crate) fn local_time_text(unix_secs: i64) -> String {
    match crate::platform::local_civil_at(unix_secs) {
        Some((year, month, day, hour, minute, _)) => {
            format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
        }
        None => format!("unix {unix_secs}"),
    }
}
