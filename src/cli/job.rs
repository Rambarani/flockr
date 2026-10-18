//! `flockr job`: inspect and control scheduled jobs through the socket API.

use std::time::{Duration, Instant};

use crate::api::schema::{
    EmptyParams, JobInfo, JobListParams, JobLogsParams, JobRunInfo, JobRunStatus, JobScheduleStore,
    JobSetEnabledParams, JobSetScheduleParams, JobState, JobTarget, Method, Request,
    ResponseResult, SuccessResponse,
};
use crate::jobs::display;

const USAGE: &str = "usage: flockr job <list [--json]|status <id>|run <id> [--wait]|logs <id> [-n LINES]|enable <id>|disable <id>|schedule <id> <every 2h|CRON>|env refresh>";
const WAIT_POLL: Duration = Duration::from_millis(250);
const ENV_REFRESH_WAIT: Duration = Duration::from_secs(20);

pub(super) fn run_job_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let rest = &args[1..];
    match subcommand {
        "list" => job_list(rest),
        "status" => job_status(rest),
        "run" => job_run(rest),
        "logs" => job_logs(rest),
        "enable" => job_set_enabled(rest, true),
        "disable" => job_set_enabled(rest, false),
        "schedule" => job_schedule(rest),
        "env" => job_env(rest),
        "help" | "--help" | "-h" => {
            eprintln!("{USAGE}");
            Ok(0)
        }
        _ => {
            eprintln!("{USAGE}");
            Ok(2)
        }
    }
}

/// Send a request. On an error response, print it and return the exit code.
pub(super) fn call(id: &str, method: Method) -> std::io::Result<Result<ResponseResult, i32>> {
    let response = super::send_request(&Request {
        id: id.into(),
        method,
    })?;
    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&response).unwrap_or_default());
        return Ok(Err(1));
    }
    match serde_json::from_value::<SuccessResponse>(response) {
        Ok(success) => Ok(Ok(success.result)),
        Err(err) => {
            eprintln!("unexpected response from the server: {err}");
            Ok(Err(1))
        }
    }
}

fn require_id(args: &[String]) -> Option<&str> {
    match args {
        [id] if !id.starts_with('-') => Some(id),
        _ => None,
    }
}

fn job_list(args: &[String]) -> std::io::Result<i32> {
    let json = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => {
            eprintln!("usage: flockr job list [--json]");
            return Ok(2);
        }
    };
    if json {
        return super::print_response(&super::send_request(&Request {
            id: "cli:job:list".into(),
            method: Method::JobList(JobListParams::default()),
        })?);
    }
    let result = match call("cli:job:list", Method::JobList(JobListParams::default()))? {
        Ok(result) => result,
        Err(code) => return Ok(code),
    };
    let ResponseResult::JobList { jobs, .. } = result else {
        eprintln!("unexpected response from the server");
        return Ok(1);
    };
    if jobs.is_empty() {
        println!(
            "no jobs; add one at {}/<id>/job.toml",
            crate::config::config_dir().join("jobs").display()
        );
        return Ok(0);
    }
    let now = crate::jobs::now_unix_ms();
    let id_width = jobs
        .iter()
        .map(|job| job.job_id.len())
        .max()
        .unwrap_or(2)
        .max(2);
    println!(
        "{:<id_width$}  {:<7}  {:<8}  {:<14}  {:<9}  LAST RESULT",
        "ID", "SOURCE", "STATE", "SCHEDULE", "NEXT"
    );
    for job in &jobs {
        let next = job
            .next_run_unix_ms
            .map_or_else(|| "-".to_owned(), |at| display::until(now, at));
        println!(
            "{:<id_width$}  {:<7}  {:<8}  {:<14}  {:<9}  {}",
            job.job_id,
            display::source_word(&job.source),
            display::state_word(job.state),
            job.schedule,
            next,
            if job.state == JobState::Invalid {
                job.diagnostics.first().cloned().unwrap_or_default()
            } else {
                display::last_result(job)
            }
        );
    }
    Ok(0)
}

fn print_run_line(run: &JobRunInfo, now: u64) {
    println!(
        "  #{:<4} {:<9} {:<10} {:>8}  {}",
        run.run_id,
        display::status_word(run.status),
        display::ago(now, run.started_unix_ms),
        display::run_duration(run.duration_ms),
        run.summary
    );
}

fn print_job_detail(job: &JobInfo, history: &[JobRunInfo]) {
    let now = crate::jobs::now_unix_ms();
    println!("{} ({})", job.title, job.job_id);
    println!("  state     {}", display::state_word(job.state));
    println!("  source    {}", job.source);
    println!("  command   {}", job.command.join(" "));
    if job.schedule_overridden {
        println!(
            "  schedule  {} (override stored by flockr job schedule)",
            job.schedule
        );
    } else {
        println!("  schedule  {}", job.schedule);
    }
    println!("  timeout   {}s", job.timeout_secs);
    println!("  env       {}", job.env);
    println!("  notify    {}", job.notify);
    println!("  dir       {}", job.dir);
    if job.enabled_overridden {
        println!(
            "  enabled   {} (set with flockr job enable/disable)",
            job.enabled
        );
    }
    if let Some(next) = job.next_run_unix_ms {
        println!("  next run  {}", display::until(now, next));
    }
    for diagnostic in &job.diagnostics {
        println!("  problem   {diagnostic}");
    }
    if !history.is_empty() {
        println!("  history (newest last):");
        for run in history {
            print_run_line(run, now);
        }
    }
}

fn job_status(args: &[String]) -> std::io::Result<i32> {
    let Some(id) = require_id(args) else {
        eprintln!("usage: flockr job status <id>");
        return Ok(2);
    };
    let result = match call(
        "cli:job:status",
        Method::JobGet(JobTarget {
            job_id: id.to_owned(),
        }),
    )? {
        Ok(result) => result,
        Err(code) => return Ok(code),
    };
    let ResponseResult::JobDetail { job, history } = result else {
        eprintln!("unexpected response from the server");
        return Ok(1);
    };
    print_job_detail(&job, &history);
    Ok(0)
}

fn job_run(args: &[String]) -> std::io::Result<i32> {
    let (id, wait) = match args {
        [id] if !id.starts_with('-') => (id.as_str(), false),
        [id, flag] | [flag, id] if flag == "--wait" && !id.starts_with('-') => (id.as_str(), true),
        _ => {
            eprintln!("usage: flockr job run <id> [--wait]");
            return Ok(2);
        }
    };
    let result = match call(
        "cli:job:run",
        Method::JobRun(JobTarget {
            job_id: id.to_owned(),
        }),
    )? {
        Ok(result) => result,
        Err(code) => return Ok(code),
    };
    let ResponseResult::JobRunRequested {
        started,
        run_id,
        reason,
        ..
    } = result
    else {
        eprintln!("unexpected response from the server");
        return Ok(1);
    };
    let Some(run_id) = run_id.filter(|_| started) else {
        println!(
            "not started: {}",
            reason.unwrap_or_else(|| "unknown".into())
        );
        return Ok(1);
    };
    println!("started {id} (run #{run_id})");
    if !wait {
        return Ok(0);
    }
    // The run's own timeout bounds the wait; poll until it is recorded.
    loop {
        std::thread::sleep(WAIT_POLL);
        let result = match call(
            "cli:job:wait",
            Method::JobGet(JobTarget {
                job_id: id.to_owned(),
            }),
        )? {
            Ok(result) => result,
            Err(code) => return Ok(code),
        };
        let ResponseResult::JobDetail { job, history } = result else {
            return Ok(1);
        };
        let Some(run) = history.iter().find(|run| run.run_id == run_id) else {
            if job.state == JobState::Running {
                continue;
            }
            eprintln!("run #{run_id} is no longer in the job's history");
            return Ok(1);
        };
        println!(
            "{}: {} ({})",
            display::status_word(run.status),
            run.summary,
            display::run_duration(run.duration_ms)
        );
        return Ok(i32::from(run.status != JobRunStatus::Success));
    }
}

fn job_logs(args: &[String]) -> std::io::Result<i32> {
    let usage = "usage: flockr job logs <id> [-n LINES]";
    let mut id = None;
    let mut lines = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-n" | "--lines" => {
                let Some(value) = args.get(index + 1).and_then(|v| v.parse::<u32>().ok()) else {
                    eprintln!("{usage}");
                    return Ok(2);
                };
                lines = Some(value);
                index += 2;
            }
            other if !other.starts_with('-') && id.is_none() => {
                id = Some(other.to_owned());
                index += 1;
            }
            _ => {
                eprintln!("{usage}");
                return Ok(2);
            }
        }
    }
    let Some(id) = id else {
        eprintln!("{usage}");
        return Ok(2);
    };
    let result = match call(
        "cli:job:logs",
        Method::JobLogs(JobLogsParams {
            job_id: id,
            run_id: None,
            lines,
        }),
    )? {
        Ok(result) => result,
        Err(code) => return Ok(code),
    };
    let ResponseResult::JobLogs {
        run,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
        ..
    } = result
    else {
        eprintln!("unexpected response from the server");
        return Ok(1);
    };
    eprintln!(
        "run #{}: {} ({})",
        run.run_id,
        display::status_word(run.status),
        run.summary
    );
    if stdout_truncated {
        eprintln!("[earlier stdout was truncated]");
    }
    print!("{stdout}");
    if stderr_truncated {
        eprintln!("[earlier stderr was truncated]");
    }
    eprint!("{stderr}");
    Ok(0)
}

fn job_set_enabled(args: &[String], enabled: bool) -> std::io::Result<i32> {
    let Some(id) = require_id(args) else {
        eprintln!(
            "usage: flockr job {} <id>",
            if enabled { "enable" } else { "disable" }
        );
        return Ok(2);
    };
    let result = match call(
        "cli:job:set_enabled",
        Method::JobSetEnabled(JobSetEnabledParams {
            job_id: id.to_owned(),
            enabled,
        }),
    )? {
        Ok(result) => result,
        Err(code) => return Ok(code),
    };
    let ResponseResult::JobUpdated { job } = result else {
        eprintln!("unexpected response from the server");
        return Ok(1);
    };
    println!(
        "{} is {}",
        job.job_id,
        if job.enabled { "enabled" } else { "disabled" }
    );
    Ok(0)
}

/// `flockr job schedule <id> every 2h` or `flockr job schedule <id> "0 9 * * *"`.
fn job_schedule(args: &[String]) -> std::io::Result<i32> {
    let usage = "usage: flockr job schedule <id> <every 2h|CRON>   (for example `every 2h` or `0 9 * * 1-5`)";
    let [id, rest @ ..] = args else {
        eprintln!("{usage}");
        return Ok(2);
    };
    if id.starts_with('-') || rest.is_empty() {
        eprintln!("{usage}");
        return Ok(2);
    }
    let text = rest.join(" ");
    if let Err(err) = crate::jobs::schedule_edit::parse_schedule_input(&text) {
        eprintln!("{err}");
        return Ok(2);
    }
    let result = match call(
        "cli:job:set_schedule",
        Method::JobSetSchedule(JobSetScheduleParams {
            job_id: id.clone(),
            schedule: text,
        }),
    )? {
        Ok(result) => result,
        Err(code) => return Ok(code),
    };
    let ResponseResult::JobScheduleUpdated {
        job,
        stored_in,
        note,
    } = result
    else {
        eprintln!("unexpected response from the server");
        return Ok(1);
    };
    println!(
        "{} now runs {} (stored in {})",
        job.job_id,
        job.schedule,
        match stored_in {
            JobScheduleStore::Config => "config.toml",
            JobScheduleStore::JobFile => "its job.toml",
            JobScheduleStore::State => "the job's state as an override",
        }
    );
    if let Some(note) = note {
        println!("{note}");
    }
    Ok(0)
}

fn job_env(args: &[String]) -> std::io::Result<i32> {
    if args != ["refresh"] {
        eprintln!("usage: flockr job env refresh");
        return Ok(2);
    }
    if let Err(code) = call(
        "cli:job:env_refresh",
        Method::JobEnvRefresh(EmptyParams::default()),
    )? {
        return Ok(code);
    }
    let deadline = Instant::now() + ENV_REFRESH_WAIT;
    loop {
        std::thread::sleep(WAIT_POLL);
        let result = match call(
            "cli:job:env_status",
            Method::JobList(JobListParams::default()),
        )? {
            Ok(result) => result,
            Err(code) => return Ok(code),
        };
        let ResponseResult::JobList { env, .. } = result else {
            return Ok(1);
        };
        if !env.refreshing {
            return Ok(match env.source.as_str() {
                "login-shell" => {
                    println!(
                        "captured {} variables from the login shell",
                        env.variable_count
                    );
                    0
                }
                _ => {
                    println!(
                        "login shell capture failed; jobs use the server environment: {}",
                        env.diagnostic.unwrap_or_default()
                    );
                    1
                }
            });
        }
        if Instant::now() >= deadline {
            eprintln!("still refreshing; check `flockr job list` later");
            return Ok(1);
        }
    }
}
