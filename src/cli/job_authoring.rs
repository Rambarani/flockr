//! `flockr job new|validate|build|schema|skill` and `flockr job run
//! --dry-run`: the authoring commands. They work on the job files directly
//! and need no running server.

use std::path::{Path, PathBuf};

use crate::jobs::authoring::dry_run::{self, Options as DryRunOptions};
use crate::jobs::authoring::program::find_program;
use crate::jobs::authoring::scaffold::{self, NewJobOptions, Template};
use crate::jobs::authoring::validate::{self, Context, Target};
use crate::jobs::authoring::{schema, JOBS_SKILL};
use crate::jobs::definition::{parse_job_toml, JobDef};
use crate::jobs::env::LoginEnvCache;
use crate::jobs::schedule_edit::{parse_schedule_input, ScheduleInput};

pub(super) const NEW_USAGE: &str = "usage: flockr job new <id> [--template shell|python|rust|workbox] [--every 1h | --schedule \"CRON\"] [--feed] [--handoff]";
pub(super) const VALIDATE_USAGE: &str =
    "usage: flockr job validate [<id>|<path>] [--run-feed] [--json]";
pub(super) const DRY_RUN_USAGE: &str = "usage: flockr job run <id> --dry-run [--no-exec]";
pub(super) const BUILD_USAGE: &str = "usage: flockr job build <id>";
pub(super) const SCHEMA_USAGE: &str = "usage: flockr job schema [--feed]";
pub(super) const SKILL_USAGE: &str = "usage: flockr job skill";

fn jobs_dir() -> PathBuf {
    crate::config::config_dir().join("jobs")
}

fn state_root() -> PathBuf {
    crate::config::state_dir().join("jobs")
}

/// Flags that take a value and switches, plus the positional arguments.
struct Parsed {
    values: Vec<(String, String)>,
    switches: Vec<String>,
    positionals: Vec<String>,
}

impl Parsed {
    fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn has(&self, name: &str) -> bool {
        self.switches.iter().any(|switch| switch == name)
    }
}

fn parse_args(
    args: &[String],
    value_flags: &[&str],
    switch_flags: &[&str],
) -> Result<Parsed, String> {
    let mut parsed = Parsed {
        values: Vec::new(),
        switches: Vec::new(),
        positionals: Vec::new(),
    };
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value.to_owned())),
            _ => (arg, None),
        };
        if value_flags.contains(&name) {
            let value = match inline {
                Some(value) => value,
                None => {
                    index += 1;
                    args.get(index)
                        .cloned()
                        .ok_or_else(|| format!("{name} needs a value"))?
                }
            };
            parsed.values.push((name.to_owned(), value));
        } else if switch_flags.contains(&name) && inline.is_none() {
            parsed.switches.push(name.to_owned());
        } else if arg.starts_with('-') {
            return Err(format!("unknown option: {arg}"));
        } else {
            parsed.positionals.push(arg.to_owned());
        }
        index += 1;
    }
    Ok(parsed)
}

fn usage_error(message: &str, usage: &str) -> i32 {
    eprintln!("{message}\n{usage}");
    2
}

pub(super) fn job_new(args: &[String]) -> std::io::Result<i32> {
    let parsed = match parse_args(
        args,
        &["--template", "--every", "--schedule"],
        &["--feed", "--handoff"],
    ) {
        Ok(parsed) => parsed,
        Err(message) => return Ok(usage_error(&message, NEW_USAGE)),
    };
    let [id] = parsed.positionals.as_slice() else {
        return Ok(usage_error("a job id is required", NEW_USAGE));
    };
    let template = match parsed
        .value("--template")
        .map_or(Ok(Template::Shell), Template::parse)
    {
        Ok(template) => template,
        Err(message) => return Ok(usage_error(&message, NEW_USAGE)),
    };
    let schedule = match (parsed.value("--every"), parsed.value("--schedule")) {
        (Some(_), Some(_)) => {
            return Ok(usage_error(
                "use either --every or --schedule, not both",
                NEW_USAGE,
            ))
        }
        (Some(text), None) => match parse_schedule_input(text) {
            Ok(schedule @ ScheduleInput::Every(_)) => Some(schedule),
            Ok(ScheduleInput::Cron(_)) => {
                return Ok(usage_error(
                    "--every takes an interval such as 1h; use --schedule for a cron expression",
                    NEW_USAGE,
                ))
            }
            Err(message) => return Ok(usage_error(&format!("--every: {message}"), NEW_USAGE)),
        },
        (None, Some(text)) => match parse_schedule_input(text) {
            Ok(schedule @ ScheduleInput::Cron(_)) => Some(schedule),
            Ok(ScheduleInput::Every(_)) => {
                return Ok(usage_error(
                    "--schedule takes a five-field cron expression such as \"0 9 * * 1-5\"; use --every for an interval",
                    NEW_USAGE,
                ))
            }
            Err(message) => {
                return Ok(usage_error(&format!("--schedule: {message}"), NEW_USAGE))
            }
        },
        (None, None) => None,
    };
    let options = NewJobOptions {
        id: id.clone(),
        template,
        schedule,
        feed: parsed.has("--feed"),
        handoff: parsed.has("--handoff"),
    };
    match scaffold::write(&jobs_dir(), &options) {
        Ok(made) => {
            print_next_steps(&options, &made.dir, &made.files);
            Ok(0)
        }
        Err(message) => {
            eprintln!("error: {message}");
            Ok(1)
        }
    }
}

fn print_next_steps(options: &NewJobOptions, dir: &Path, files: &[&str]) {
    let id = &options.id;
    println!("created {} (disabled)", dir.display());
    println!("  files: {}", files.join(", "));
    if options.template == Template::Workbox && (options.feed || options.handoff) {
        println!("  note: the workbox template always has a feed and a handoff");
    }
    if let Some(program) = options.template.interpreter() {
        if program != "sh"
            && find_program(program, dir, std::env::var("PATH").ok().as_deref()).is_none()
        {
            println!();
            match options.template {
                Template::Rust => {
                    println!("note: rust-script is not installed, so this job cannot run yet.");
                    println!("  install it:      cargo install rust-script");
                    println!("  pre-compile it:  flockr job build {id}");
                }
                Template::Workbox => {
                    println!("note: workbox is not on your PATH; the job fails until it is.");
                }
                _ => println!("note: {program} is not on your PATH; the job fails until it is."),
            }
        }
    }
    let script = files
        .iter()
        .find(|name| !matches!(**name, "job.toml" | "prompt.md") && !name.starts_with("feed."))
        .copied()
        .unwrap_or("job.toml");
    println!();
    println!("next steps");
    println!("  1. edit      {}", dir.join(script).display());
    println!(
        "  2. validate  flockr job validate {id}{}",
        if options.feed || options.template == Template::Workbox {
            " --run-feed"
        } else {
            ""
        }
    );
    println!("  3. dry-run   flockr job run {id} --dry-run");
    println!(
        "  4. enable    flockr job enable {id}   (or press e on it in the Jobs overlay, prefix+t)"
    );
}

pub(super) fn job_validate(args: &[String]) -> std::io::Result<i32> {
    let parsed = match parse_args(args, &[], &["--json", "--run-feed"]) {
        Ok(parsed) => parsed,
        Err(message) => return Ok(usage_error(&message, VALIDATE_USAGE)),
    };
    if parsed.positionals.len() > 1 {
        return Ok(usage_error("give one job id or path", VALIDATE_USAGE));
    }
    let jobs_dir = jobs_dir();
    let targets: Vec<Target> = match parsed.positionals.first() {
        Some(arg) => {
            if arg.starts_with(crate::jobs::system::SYSTEM_PREFIX) {
                eprintln!(
                    "error: {arg} is a system job; it has no job.toml. Tune it with [jobs.system.<name>] in config.toml (`flockr job schema` describes the keys)"
                );
                return Ok(1);
            }
            match validate::resolve_target(arg, &jobs_dir) {
                Ok(target) => vec![target],
                Err(message) => {
                    eprintln!("error: {message}");
                    return Ok(1);
                }
            }
        }
        None => validate::all_targets(&jobs_dir),
    };
    if targets.is_empty() {
        println!(
            "no jobs in {}; create one with `flockr job new <id>`",
            jobs_dir.display()
        );
        return Ok(0);
    }
    let cache = LoginEnvCache::system();
    let ctx = Context {
        cache: &cache,
        now_secs: i64::try_from(crate::jobs::now_unix_ms() / 1000).unwrap_or(0),
        run_feed: parsed.has("--run-feed"),
    };
    let reports: Vec<_> = targets
        .iter()
        .map(|target| validate::validate_job(target, &ctx))
        .collect();
    let failed = reports.iter().any(|report| !report.ok);
    if parsed.has("--json") {
        let document = serde_json::json!({ "ok": !failed, "jobs": reports });
        println!(
            "{}",
            serde_json::to_string_pretty(&document).unwrap_or_default()
        );
    } else {
        for report in &reports {
            print!("{}", validate::render_report(report));
            println!();
        }
        let errors: usize = reports.iter().map(|report| report.errors.len()).sum();
        println!(
            "{} job(s) checked, {}",
            reports.len(),
            if failed {
                format!("{errors} error(s)")
            } else {
                "no errors".to_owned()
            }
        );
    }
    Ok(i32::from(failed))
}

/// Parse a user job for a command that runs it locally.
fn load_job(id: &str) -> Result<JobDef, String> {
    if id.starts_with(crate::jobs::system::SYSTEM_PREFIX) {
        return Err(format!(
            "{id} is a system job; this command works on jobs with a job.toml"
        ));
    }
    let target = validate::resolve_target(id, &jobs_dir())?;
    let toml = target.dir.join("job.toml");
    let content = std::fs::read_to_string(&toml)
        .map_err(|err| format!("could not read {}: {err}", toml.display()))?;
    parse_job_toml(&content, &target.id, &target.dir).map_err(|diagnostics| {
        format!(
            "{} is not valid:\n  {}\n(`flockr job validate {id}` shows more)",
            toml.display(),
            diagnostics.join("\n  ")
        )
    })
}

pub(super) fn job_dry_run(args: &[String]) -> std::io::Result<i32> {
    let parsed = match parse_args(args, &[], &["--dry-run", "--no-exec"]) {
        Ok(parsed) => parsed,
        Err(message) => return Ok(usage_error(&message, DRY_RUN_USAGE)),
    };
    let [id] = parsed.positionals.as_slice() else {
        return Ok(usage_error("a job id is required", DRY_RUN_USAGE));
    };
    let job = match load_job(id) {
        Ok(job) => job,
        Err(message) => {
            eprintln!("error: {message}");
            return Ok(1);
        }
    };
    let cache = LoginEnvCache::system();
    let options = DryRunOptions {
        no_exec: parsed.has("--no-exec"),
    };
    match dry_run::run(&job, &cache, options, crate::jobs::now_unix_ms()) {
        Ok(dry) => {
            print!("{}", dry_run::render(&job, &dry));
            Ok(dry_run::exit_code(&dry))
        }
        Err(message) => {
            eprintln!("error: {message}");
            Ok(1)
        }
    }
}

/// The script files of `rust-script` commands in the job (the command and the
/// feed command), without repeats.
fn rust_scripts(job: &JobDef) -> Vec<String> {
    let mut scripts: Vec<String> = Vec::new();
    let feed = job.feed.iter().map(|feed| &feed.command);
    for command in std::iter::once(&job.command).chain(feed) {
        let is_rust_script = command.first().is_some_and(|program| {
            Path::new(program)
                .file_name()
                .is_some_and(|name| name == "rust-script")
        });
        if !is_rust_script {
            continue;
        }
        if let Some(script) = command[1..].iter().find(|arg| !arg.starts_with('-')) {
            if !scripts.contains(script) {
                scripts.push(script.clone());
            }
        }
    }
    scripts
}

/// The arguments that make `rust-script` compile a script without running it.
///
/// Assumption: `--build-only`, which `rust-script --help` documents as "Build
/// the script but don't run it" in the releases this was written against.
/// rust-script was not installed where this was written, so the flag was not
/// run; change it here if a future release renames it.
const RUST_SCRIPT_BUILD_FLAG: &str = "--build-only";

pub(super) fn job_build(args: &[String]) -> std::io::Result<i32> {
    let [id] = args else {
        return Ok(usage_error("a job id is required", BUILD_USAGE));
    };
    if id.starts_with('-') {
        return Ok(usage_error("a job id is required", BUILD_USAGE));
    }
    let job = match load_job(id) {
        Ok(job) => job,
        Err(message) => {
            eprintln!("error: {message}");
            return Ok(1);
        }
    };
    let scripts = rust_scripts(&job);
    if scripts.is_empty() {
        println!(
            "nothing to build: {id} does not run a rust-script file (command = {:?})",
            job.command
        );
        return Ok(0);
    }
    let cache = LoginEnvCache::system();
    let env = match crate::jobs::env::resolve_job_env(
        &job,
        &cache,
        crate::jobs::env::flockr_job_vars(&job, &state_root().join(&job.id)),
    ) {
        Ok(env) => env,
        Err(message) => {
            eprintln!("error: {message}");
            return Ok(1);
        }
    };
    let path_var = crate::jobs::authoring::path_of(&env.vars, !env.clear);
    let Some(program) = find_program("rust-script", &job.cwd, path_var.as_deref()) else {
        eprintln!(
            "error: rust-script was not found on the job's PATH ({}).",
            env.note
        );
        eprintln!("install it with: cargo install rust-script");
        return Ok(1);
    };
    for script in scripts {
        println!("building {script} ...");
        let mut command = std::process::Command::new(&program);
        command
            .arg(RUST_SCRIPT_BUILD_FLAG)
            .arg(&script)
            .current_dir(&job.cwd);
        if env.clear {
            command.env_clear();
        }
        command.envs(env.vars.iter().map(|(name, value)| (name, value)));
        match command.status() {
            Ok(status) if status.success() => println!("built {script}"),
            Ok(status) => {
                eprintln!("error: rust-script failed to build {script} ({status})");
                return Ok(status.code().unwrap_or(1));
            }
            Err(err) => {
                eprintln!("error: could not start {}: {err}", program.display());
                return Ok(1);
            }
        }
    }
    Ok(0)
}

pub(super) fn job_schema(args: &[String]) -> std::io::Result<i32> {
    match args {
        [] => print!("{}", schema::job_schema_text()),
        [flag] if flag == "--feed" => print!("{}", schema::feed_schema_text()),
        _ => return Ok(usage_error("unexpected arguments", SCHEMA_USAGE)),
    }
    Ok(0)
}

pub(super) fn job_skill(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        return Ok(usage_error("unexpected arguments", SKILL_USAGE));
    }
    print!("{JOBS_SKILL}");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    #[test]
    fn flags_take_values_inline_or_separately() {
        let parsed = parse_args(
            &strings(&["demo", "--template=rust", "--every", "30m", "--feed"]),
            &["--template", "--every"],
            &["--feed"],
        )
        .unwrap();
        assert_eq!(parsed.positionals, ["demo"]);
        assert_eq!(parsed.value("--template"), Some("rust"));
        assert_eq!(parsed.value("--every"), Some("30m"));
        assert!(parsed.has("--feed") && !parsed.has("--handoff"));
        assert!(parse_args(&strings(&["--bogus"]), &[], &[]).is_err());
        assert!(parse_args(&strings(&["--every"]), &["--every"], &[]).is_err());
    }

    #[test]
    fn rust_scripts_are_found_in_the_command_and_the_feed() {
        let job = parse_job_toml(
            "command = [\"rust-script\", \"job.rs\"]\nevery = \"1h\"\n[feed]\ncommand = [\"rust-script\", \"-q\", \"feed.rs\"]\n",
            "demo",
            Path::new("/jobs/demo"),
        )
        .unwrap();
        assert_eq!(rust_scripts(&job), ["job.rs", "feed.rs"]);
        let shell = parse_job_toml(
            "command = [\"sh\", \"run.sh\"]\nevery = \"1h\"\n",
            "demo",
            Path::new("/jobs/demo"),
        )
        .unwrap();
        assert!(rust_scripts(&shell).is_empty());
    }
}
