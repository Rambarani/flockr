//! Runs a feed task on a worker thread: optionally an item action, then the
//! job's feed command. Both use the job's environment rules, working
//! directory and process-group timeout, through the job runner's `execute`.

use std::path::PathBuf;
use std::time::Duration;

use super::format::{parse_feed, MAX_FEED_BYTES};
use super::manager::{ActionOutcome, RefreshOutcome, TaskResult};
use crate::api::schema::JobRunStatus;
use crate::jobs::definition::JobDef;
use crate::jobs::env::{flockr_job_vars, resolve_job_env, LoginEnvCache, ResolvedEnv};
use crate::jobs::runner::{execute, ExecResult, ExecSpec, TERMINATE_GRACE};

/// An item action gets at most this long, or the job's timeout when shorter.
pub(crate) const ACTION_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub(crate) struct ActionPlan {
    pub(crate) item_id: String,
    pub(crate) key: String,
    pub(crate) command: Vec<String>,
}

/// Everything the worker needs; it owns its data.
#[derive(Debug)]
pub(crate) struct TaskPlan {
    pub(crate) job: JobDef,
    pub(crate) state_dir: PathBuf,
    pub(crate) action: Option<ActionPlan>,
}

/// One short line describing why a command did not succeed.
fn failure_line(result: &ExecResult) -> String {
    let last_stderr = result
        .stderr
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty());
    let base = result.summary.clone();
    match last_stderr {
        Some(line) => {
            let clipped: String = line.chars().take(160).collect();
            format!("{base}: {clipped}")
        }
        None => base,
    }
}

fn exec_spec(
    job: &JobDef,
    env: ResolvedEnv,
    command: Vec<String>,
    timeout: Duration,
    stdout_limit: usize,
) -> ExecSpec {
    ExecSpec {
        command,
        cwd: job.cwd.clone(),
        timeout,
        env_clear: env.clear,
        env: env.vars,
        terminate_grace: TERMINATE_GRACE,
        stdout_limit,
    }
}

fn refresh_outcome(result: &ExecResult) -> RefreshOutcome {
    if result.status != JobRunStatus::Success {
        return RefreshOutcome::Failed(format!("feed command failed: {}", failure_line(result)));
    }
    if result.stdout_truncated {
        return RefreshOutcome::Failed(format!(
            "feed output is larger than {} KiB",
            MAX_FEED_BYTES / 1024
        ));
    }
    match parse_feed(&result.stdout) {
        Ok(parsed) => RefreshOutcome::Feed(parsed),
        Err(errors) => RefreshOutcome::Failed(errors.join("; ")),
    }
}

/// Run the task and build the result. Blocking: call from a worker thread.
pub(crate) fn run_task(plan: &TaskPlan, cache: &LoginEnvCache) -> TaskResult {
    let job_id = plan.job.id.clone();
    let Some(feed) = &plan.job.feed else {
        return TaskResult {
            job_id,
            action: None,
            refresh: RefreshOutcome::Failed("the job has no feed table".into()),
            finished_unix_ms: crate::jobs::now_unix_ms(),
        };
    };
    let resolve = |extra: Vec<(String, String)>| -> Result<ResolvedEnv, String> {
        let mut vars = flockr_job_vars(&plan.job, &plan.state_dir);
        vars.extend(extra);
        resolve_job_env(&plan.job, cache, vars)
    };
    if let crate::jobs::definition::JobSource::Plugin { plugin_id, .. } = &plan.job.source {
        let _ = crate::plugin_paths::ensure_plugin_user_dirs(plugin_id);
    }
    let _ = std::fs::create_dir_all(&plan.state_dir);

    let action = plan.action.as_ref().map(|action| {
        let outcome = |ok: bool, summary: String| ActionOutcome {
            item_id: action.item_id.clone(),
            key: action.key.clone(),
            ok,
            summary,
        };
        match resolve(vec![
            ("FLOCKR_FEED_ITEM_ID".to_owned(), action.item_id.clone()),
            ("FLOCKR_FEED_ACTION_KEY".to_owned(), action.key.clone()),
        ]) {
            Err(error) => outcome(false, error),
            Ok(env) => {
                let result = execute(&exec_spec(
                    &plan.job,
                    env,
                    action.command.clone(),
                    plan.job.timeout.min(ACTION_TIMEOUT),
                    crate::jobs::state::OUTPUT_TAIL_BYTES,
                ));
                if result.status == JobRunStatus::Success {
                    outcome(true, "ok".into())
                } else {
                    outcome(false, failure_line(&result))
                }
            }
        }
    });

    let refresh = match resolve(Vec::new()) {
        Err(error) => RefreshOutcome::Failed(error),
        Ok(env) => refresh_outcome(&execute(&exec_spec(
            &plan.job,
            env,
            feed.command.clone(),
            plan.job.timeout,
            MAX_FEED_BYTES + 1,
        ))),
    };
    TaskResult {
        job_id,
        action,
        refresh,
        finished_unix_ms: crate::jobs::now_unix_ms(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::jobs::definition::parse_job_toml;
    use crate::jobs::env::LoginEnvCache;

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "flockr-feed-run-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cache() -> LoginEnvCache {
        LoginEnvCache::new(Box::new(|| {
            Ok(vec![("PATH".into(), "/usr/bin:/bin".into())])
        }))
    }

    fn job(dir: &std::path::Path, feed_command: &str, timeout: &str) -> JobDef {
        let toml = format!(
            "command = [\"true\"]\nevery = \"1h\"\nenv = \"server\"\ntimeout = \"{timeout}\"\n\
             feed = {{ command = [\"sh\", \"-c\", {feed_command:?}] }}\n"
        );
        parse_job_toml(&toml, "feedjob", dir).unwrap_or_else(|errors| panic!("job: {errors:?}"))
    }

    fn plan(job: JobDef, dir: &std::path::Path, action: Option<ActionPlan>) -> TaskPlan {
        TaskPlan {
            job,
            state_dir: dir.join("state"),
            action,
        }
    }

    const FEED: &str = r#"{"version":1,"items":[{"id":"a","title":"A"}]}"#;

    #[test]
    fn a_refresh_reads_the_feed_from_stdout() {
        let dir = temp_dir("ok");
        let job = job(&dir, &format!("printf '%s' '{FEED}'"), "10s");
        let result = run_task(&plan(job, &dir, None), &cache());
        let RefreshOutcome::Feed(feed) = result.refresh else {
            panic!("expected a feed, got {:?}", result.refresh);
        };
        assert_eq!(feed.items[0].id, "a");
        assert!(result.action.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failures_say_what_went_wrong() {
        let dir = temp_dir("fail");
        let result = run_task(
            &plan(job(&dir, "echo no network >&2; exit 4", "10s"), &dir, None),
            &cache(),
        );
        assert_eq!(
            result.refresh,
            RefreshOutcome::Failed("feed command failed: exit 4: no network".into())
        );
        let result = run_task(&plan(job(&dir, "echo hello", "10s"), &dir, None), &cache());
        let RefreshOutcome::Failed(message) = result.refresh else {
            panic!("expected a failure");
        };
        assert!(message.contains("not valid JSON"), "{message}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_slow_feed_command_times_out() {
        let dir = temp_dir("timeout");
        let started = std::time::Instant::now();
        let result = run_task(&plan(job(&dir, "sleep 30", "1s"), &dir, None), &cache());
        let RefreshOutcome::Failed(message) = result.refresh else {
            panic!("expected a failure");
        };
        assert!(message.contains("timed out after 1s"), "{message}");
        assert!(started.elapsed() < Duration::from_secs(10));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn feeds_larger_than_a_job_log_tail_still_parse() {
        let dir = temp_dir("big");
        // About 60 KiB of items: well over the 16 KiB job output tail.
        let script = "printf '{\"version\":1,\"items\":['; i=0; while [ $i -lt 600 ]; do \
                      [ $i -gt 0 ] && printf ','; printf '{\"id\":\"i%s\",\"title\":\"item number %s with some padding text to make it long\"}' $i $i; \
                      i=$((i+1)); done; printf ']}'";
        let result = run_task(&plan(job(&dir, script, "30s"), &dir, None), &cache());
        let RefreshOutcome::Feed(feed) = result.refresh else {
            panic!("expected a feed, got {:?}", result.refresh);
        };
        assert_eq!(feed.items.len(), 500, "capped at the item limit");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_action_runs_first_with_the_job_environment_then_the_feed_refreshes() {
        let dir = temp_dir("action");
        let marker = dir.join("marker.txt");
        // The feed command reports the marker's existence in the item title,
        // so the order of action and refresh is observable.
        let script = format!(
            "if [ -f {m} ]; then t=marked; else t=unmarked; fi; \
             printf '{{\"version\":1,\"items\":[{{\"id\":\"a\",\"title\":\"%s\"}}]}}' $t",
            m = marker.display()
        );
        let result = run_task(
            &plan(
                job(&dir, &script, "10s"),
                &dir,
                Some(ActionPlan {
                    item_id: "a".into(),
                    key: "d".into(),
                    command: vec![
                        "sh".into(),
                        "-c".into(),
                        format!(
                            "echo \"$FLOCKR_FEED_ITEM_ID:$FLOCKR_FEED_ACTION_KEY:$FLOCKR_JOB_ID\" > {}",
                            marker.display()
                        ),
                    ],
                }),
            ),
            &cache(),
        );
        let action = result.action.unwrap();
        assert!(action.ok, "{action:?}");
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap().trim(),
            "a:d:feedjob"
        );
        let RefreshOutcome::Feed(feed) = result.refresh else {
            panic!("expected a feed");
        };
        assert_eq!(
            feed.items[0].title, "marked",
            "the refresh ran after the action"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_action_is_reported_and_the_feed_still_refreshes() {
        let dir = temp_dir("action-fail");
        let result = run_task(
            &plan(
                job(&dir, &format!("printf '%s' '{FEED}'"), "10s"),
                &dir,
                Some(ActionPlan {
                    item_id: "a".into(),
                    key: "d".into(),
                    command: vec!["sh".into(), "-c".into(), "echo denied >&2; exit 7".into()],
                }),
            ),
            &cache(),
        );
        let action = result.action.unwrap();
        assert!(!action.ok);
        assert_eq!(action.summary, "exit 7: denied");
        assert!(matches!(result.refresh, RefreshOutcome::Feed(_)));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
