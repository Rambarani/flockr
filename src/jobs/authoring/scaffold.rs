//! `flockr job new`: write a starter job directory. The job is always
//! created disabled, with a `job.toml` whose comments explain every key, so
//! the author edits it, validates it, dry-runs it and only then enables it.

use std::path::{Path, PathBuf};

use crate::jobs::definition::valid_job_id;
use crate::jobs::schedule_edit::ScheduleInput;

const WORKBOX_JOB_TOML: &str = include_str!("../../../examples/jobs/workbox-sync/job.toml");
const WORKBOX_PROMPT: &str = include_str!("../../../examples/jobs/workbox-sync/prompt.md");
/// The interval line in the workbox example, replaced by `--every` or
/// `--schedule`.
const WORKBOX_EVERY_LINE: &str = "every = \"1h\"";

const RUN_SH: &str = include_str!("templates/run.sh");
const FEED_SH: &str = include_str!("templates/feed.sh");
const RUN_PY: &str = include_str!("templates/run.py");
const FEED_PY: &str = include_str!("templates/feed.py");
const JOB_RS: &str = include_str!("templates/job.rs");
const FEED_RS: &str = include_str!("templates/feed.rs");
const PROMPT_MD: &str = include_str!("templates/prompt.md");
const PROMPT_FEED_MD: &str = include_str!("templates/prompt_feed.md");

pub(crate) const DEFAULT_EVERY: &str = "1h";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Template {
    Shell,
    Python,
    Rust,
    Workbox,
}

impl Template {
    pub(crate) const ALL: [Template; 4] = [Self::Shell, Self::Python, Self::Rust, Self::Workbox];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Python => "python",
            Self::Rust => "rust",
            Self::Workbox => "workbox",
        }
    }

    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|template| template.name() == text)
            .ok_or_else(|| {
                format!(
                    "unknown template '{text}'; use {}",
                    Self::ALL.map(Self::name).join(", ")
                )
            })
    }

    /// The program the job's script runs under, for a "not installed" hint.
    pub(crate) fn interpreter(self) -> Option<&'static str> {
        match self {
            Self::Shell => Some("sh"),
            Self::Python => Some("python3"),
            Self::Rust => Some("rust-script"),
            Self::Workbox => Some("workbox"),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct NewJobOptions {
    pub(crate) id: String,
    pub(crate) template: Template,
    /// `None` means `every = "1h"`.
    pub(crate) schedule: Option<ScheduleInput>,
    pub(crate) feed: bool,
    pub(crate) handoff: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScaffoldFile {
    pub(crate) name: &'static str,
    pub(crate) content: String,
    pub(crate) executable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Scaffolded {
    pub(crate) dir: PathBuf,
    pub(crate) files: Vec<&'static str>,
}

struct Scripts {
    command: &'static [&'static str],
    run_name: &'static str,
    run: &'static str,
    feed_command: &'static [&'static str],
    feed_name: &'static str,
    feed: &'static str,
}

fn scripts(template: Template) -> Option<Scripts> {
    match template {
        Template::Shell => Some(Scripts {
            command: &["sh", "run.sh"],
            run_name: "run.sh",
            run: RUN_SH,
            feed_command: &["sh", "feed.sh"],
            feed_name: "feed.sh",
            feed: FEED_SH,
        }),
        Template::Python => Some(Scripts {
            command: &["python3", "run.py"],
            run_name: "run.py",
            run: RUN_PY,
            feed_command: &["python3", "feed.py"],
            feed_name: "feed.py",
            feed: FEED_PY,
        }),
        Template::Rust => Some(Scripts {
            command: &["rust-script", "job.rs"],
            run_name: "job.rs",
            run: JOB_RS,
            feed_command: &["rust-script", "feed.rs"],
            feed_name: "feed.rs",
            feed: FEED_RS,
        }),
        Template::Workbox => None,
    }
}

fn argv(parts: &[&str]) -> String {
    let quoted: Vec<String> = parts.iter().map(|part| format!("{part:?}")).collect();
    format!("[{}]", quoted.join(", "))
}

fn schedule_lines(schedule: &Option<ScheduleInput>) -> String {
    let (active, other) = match schedule {
        Some(ScheduleInput::Cron(expr)) => (
            format!("schedule = {expr:?}"),
            format!("# every = {DEFAULT_EVERY:?}"),
        ),
        Some(ScheduleInput::Every(text)) => (
            format!("every = {text:?}"),
            "# schedule = \"0 10 * * 1-5\"".to_owned(),
        ),
        None => (
            format!("every = {DEFAULT_EVERY:?}"),
            "# schedule = \"0 10 * * 1-5\"".to_owned(),
        ),
    };
    format!("{active}\n{other}\n")
}

fn job_toml(opts: &NewJobOptions, scripts: &Scripts) -> String {
    let id = &opts.id;
    let mut out = String::new();
    out.push_str(&format!(
        "# Job `{id}`, created by `flockr job new {id} --template {}`.\n\
         # This file is ~/.config/flockr/jobs/{id}/job.toml (debug builds use flockr-dev).\n\
         # Edits are picked up within about 30 seconds.\n\
         #\n\
         # Next steps:\n\
         #   flockr job validate {id}          check this file, the command and the templates\n\
         #   flockr job run {id} --dry-run     run it once without recording anything\n\
         #   flockr job enable {id}            switch the schedule on\n\
         # Unknown keys are errors, so a typo is reported instead of ignored.\n\n",
        opts.template.name()
    ));
    out.push_str(
        "# id: optional. When present it must equal this directory's name.\n\
         # id = \"\"\n\n",
    );
    out.push_str(&format!(
        "# title: the name shown in `flockr job list`, the Jobs overlay and toasts.\n\
         # Default: the id.\n\
         title = {id:?}\n\n"
    ));
    out.push_str(&format!(
        "# command: required argv array. The first element is the program and the\n\
         # rest are its arguments. Flockr does not run a shell unless you do\n\
         # (use [\"sh\", \"-c\", \"...\"] for a one-liner). It runs in `cwd`.\n\
         command = {}\n\n",
        argv(scripts.command)
    ));
    out.push_str(
        "# Schedule: set exactly one of `every` and `schedule`.\n\
         #   every    an interval such as \"30m\", \"1h\" or \"1h30m\" (minimum 1m).\n\
         #   schedule a five-field cron expression in local time:\n\
         #            minute hour day-of-month month day-of-week.\n",
    );
    out.push_str(&schedule_lines(&opts.schedule));
    out.push('\n');
    out.push_str(
        "# timeout: how long one run may take (default 10m, minimum 1s, maximum 24h).\n\
         # When it expires Flockr stops the command and everything it started. A run\n\
         # that is still going when the next one is due makes that one \"skipped\".\n\
         timeout = \"10m\"\n\n\
         # enabled: `flockr job new` always writes false. Turn it on with\n\
         # `flockr job enable <id>`, or by setting true here.\n\
         enabled = false\n\n\
         # notify: when a finished run shows a toast.\n\
         #   never | on_failure (default) | on_change | always\n\
         notify = \"on_failure\"\n\n\
         # cwd: the working directory. Default: this directory. `~` and relative\n\
         # paths (relative to this directory) work.\n\
         # cwd = \".\"\n\n\
         # env: where the job's environment comes from.\n\
         #   \"login-shell\" (default) is your login shell's environment, so what\n\
         #   ~/.zshrc exports (tokens, PATH) is available.\n\
         #   \"server\" is the Flockr server's own environment.\n\
         # A file cannot hold both `env = \"...\"` and an [env] table; with the\n\
         # table, put the mode inside it (mode = \"server\").\n\
         # env = \"login-shell\"\n\n\
         # env_file: KEY=VALUE lines added to the environment, relative to this\n\
         # directory. Keep secrets out of job output: stdout is stored in the logs.\n\
         # env_file = \"secrets.env\"\n\n",
    );
    out.push_str(&feed_block(opts, scripts));
    out.push_str(&handoff_block(opts));
    out.push_str(
        "# [env]: non-secret overrides, one KEY = \"value\" per line, applied after\n\
         # env_file. The reserved key `mode` sets the mode instead of a variable.\n\
         # Keep this table last in the file: keys after it would belong to it.\n\
         # [env]\n\
         # mode = \"login-shell\"\n\
         # LOG_LEVEL = \"debug\"\n",
    );
    out
}

fn feed_block(opts: &NewJobOptions, scripts: &Scripts) -> String {
    let explain = "# [feed]: gives the job a feed, a list of items Flockr shows in the Feed\n\
         # overlay and in a sidebar section under the agent list.\n\
         #   command  required argv array that prints ONE JSON object (feed format\n\
         #            v1) on stdout and nothing else. It runs after every successful\n\
         #            run, on demand and when the server starts.\n\
         #   title    the section title (default: the feed's own title, then the\n\
         #            job title).\n";
    if opts.feed {
        format!(
            "{explain}[feed]\ncommand = {}\ntitle = {:?}\n\n",
            argv(scripts.feed_command),
            opts.id
        )
    } else {
        format!(
            "{explain}# [feed]\n# command = {}\n# title = {:?}\n\n",
            argv(scripts.feed_command),
            opts.id
        )
    }
}

fn handoff_block(opts: &NewJobOptions) -> String {
    let explain = "# [handoff]: after a run the job may ask whether to start an agent tab.\n\
         # Nothing starts until you answer (toast, then the Decisions tab of the Jobs\n\
         # overlay). Keys:\n\
         #   when           on_success | on_change (default) | on_failure | always |\n\
         #                  \"exit:<code>\". on_change compares stdout and the feed with\n\
         #                  the previous run.\n\
         #   ask            the question; {{variables}} are filled in.\n\
         #   agent_command  the agent CLI (default claude). Add {prompt_file} to pass\n\
         #                  the rendered prompt file anywhere; otherwise it becomes\n\
         #                  the first argument.\n\
         #   cwd            directory of the new tab (default: this directory).\n\
         #   tab_title      title of the new tab.\n\
         #   prompt_file    prompt template in this directory (or an inline `prompt`,\n\
         #                  not both).\n\
         # Variables: {{stdout}} {{stderr_tail}} {{exit_code}} {{feed_json}} {{count}}\n\
         # {{date}} {{job.id}} {{job.title}} {{run.id}} {{context.*}}. A job can also\n\
         # print one stdout line {\"flockr_handoff\": {...}} to ask with its own text.\n";
    let (ask, prompt_file) = if opts.feed {
        ("{{count}} items need you. Start Claude?", "prompt.md")
    } else {
        ("{{job.title}} finished. Start Claude?", "prompt.md")
    };
    if opts.handoff {
        format!(
            "{explain}[handoff]\nwhen = \"on_change\"\nask = {ask:?}\n\
             agent_command = \"claude\"\n# cwd = \"~/projects/app\"\n\
             tab_title = \"{{{{job.title}}}} · {{{{date}}}}\"\nprompt_file = {prompt_file:?}\n\n"
        )
    } else {
        format!(
            "{explain}# [handoff]\n# when = \"on_change\"\n# ask = {ask:?}\n\
             # agent_command = \"claude\"\n# cwd = \"~/projects/app\"\n\
             # tab_title = \"{{{{job.title}}}} · {{{{date}}}}\"\n# prompt_file = \"prompt.md\"\n\n"
        )
    }
}

/// The files a job directory gets, without touching the disk.
pub(crate) fn plan(opts: &NewJobOptions) -> Result<Vec<ScaffoldFile>, String> {
    if !valid_job_id(&opts.id) {
        return Err(format!(
            "'{}' is not a valid job id; use letters, digits, '.', '_' and '-' (starting with a letter or digit, at most {} characters)",
            opts.id,
            crate::jobs::definition::JOB_ID_MAX_CHARS
        ));
    }
    let Some(scripts) = scripts(opts.template) else {
        return workbox_plan(opts);
    };
    let mut files = vec![
        ScaffoldFile {
            name: "job.toml",
            content: job_toml(opts, &scripts),
            executable: false,
        },
        ScaffoldFile {
            name: scripts.run_name,
            content: scripts.run.to_owned(),
            executable: true,
        },
    ];
    if opts.feed {
        files.push(ScaffoldFile {
            name: scripts.feed_name,
            content: scripts.feed.to_owned(),
            executable: true,
        });
    }
    if opts.handoff {
        files.push(ScaffoldFile {
            name: "prompt.md",
            content: if opts.feed { PROMPT_FEED_MD } else { PROMPT_MD }.to_owned(),
            executable: false,
        });
    }
    Ok(files)
}

/// The workbox template is the tracked example job, with the schedule
/// replaced when one was asked for. It already has a feed and a handoff.
fn workbox_plan(opts: &NewJobOptions) -> Result<Vec<ScaffoldFile>, String> {
    let mut toml = WORKBOX_JOB_TOML.to_owned();
    if let Some(schedule) = &opts.schedule {
        if !toml.contains(WORKBOX_EVERY_LINE) {
            return Err("the workbox example has no `every = \"1h\"` line to replace".into());
        }
        toml = toml.replacen(
            WORKBOX_EVERY_LINE,
            &format!("{} = {:?}", schedule.key(), schedule.value()),
            1,
        );
    }
    Ok(vec![
        ScaffoldFile {
            name: "job.toml",
            content: toml,
            executable: false,
        },
        ScaffoldFile {
            name: "prompt.md",
            content: WORKBOX_PROMPT.to_owned(),
            executable: false,
        },
    ])
}

#[cfg(unix)]
fn make_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Create `<jobs_dir>/<id>/` and write the files. An existing path with that
/// name is never touched.
pub(crate) fn write(jobs_dir: &Path, opts: &NewJobOptions) -> Result<Scaffolded, String> {
    let files = plan(opts)?;
    let dir = jobs_dir.join(&opts.id);
    if dir.exists() {
        return Err(format!(
            "{} already exists; pick another id or remove it first (nothing was changed)",
            dir.display()
        ));
    }
    std::fs::create_dir_all(jobs_dir)
        .map_err(|err| format!("could not create {}: {err}", jobs_dir.display()))?;
    std::fs::create_dir(&dir).map_err(|err| {
        if err.kind() == std::io::ErrorKind::AlreadyExists {
            format!("{} already exists; nothing was changed", dir.display())
        } else {
            format!("could not create {}: {err}", dir.display())
        }
    })?;
    for file in &files {
        let path = dir.join(file.name);
        std::fs::write(&path, &file.content)
            .map_err(|err| format!("could not write {}: {err}", path.display()))?;
        if file.executable {
            make_executable(&path)
                .map_err(|err| format!("could not make {} executable: {err}", path.display()))?;
        }
    }
    Ok(Scaffolded {
        dir,
        files: files.iter().map(|file| file.name).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::definition::{parse_job_toml, Schedule};

    fn opts(template: Template, feed: bool, handoff: bool) -> NewJobOptions {
        NewJobOptions {
            id: "demo".into(),
            template,
            schedule: None,
            feed,
            handoff,
        }
    }

    fn file<'a>(files: &'a [ScaffoldFile], name: &str) -> &'a ScaffoldFile {
        files
            .iter()
            .find(|file| file.name == name)
            .unwrap_or_else(|| {
                panic!(
                    "no {name} in {:?}",
                    files.iter().map(|f| f.name).collect::<Vec<_>>()
                )
            })
    }

    /// Keys a reader of the generated file must find explained.
    const EVERY_KEY: &[&str] = &[
        "id",
        "title",
        "command",
        "schedule",
        "every",
        "timeout",
        "enabled",
        "notify",
        "cwd",
        "env",
        "env_file",
        "[env]",
        "[feed]",
        "[handoff]",
        "when",
        "ask",
        "agent_command",
        "tab_title",
        "prompt_file",
        "prompt",
    ];

    #[test]
    fn every_template_and_option_combination_parses_disabled() {
        for template in Template::ALL {
            for (feed, handoff) in [(false, false), (true, false), (false, true), (true, true)] {
                let files = plan(&opts(template, feed, handoff)).unwrap();
                let toml = &file(&files, "job.toml").content;
                let job = parse_job_toml(toml, "demo", Path::new("/jobs/demo"))
                    .unwrap_or_else(|err| panic!("{template:?} {feed} {handoff}: {err:?}\n{toml}"));
                assert!(!job.enabled, "{template:?} must start disabled");
                if template != Template::Workbox {
                    assert_eq!(job.feed.is_some(), feed, "{template:?}");
                    assert_eq!(job.handoff.is_some(), handoff, "{template:?}");
                    assert_eq!(
                        job.schedule,
                        Schedule::Every(std::time::Duration::from_secs(3600))
                    );
                } else {
                    assert!(job.feed.is_some() && job.handoff.is_some());
                }
                if let Some(handoff) = &job.handoff {
                    let prompt = handoff.prompt_file.clone().unwrap();
                    assert!(
                        files.iter().any(|f| f.name == prompt),
                        "{prompt} is written"
                    );
                }
            }
        }
    }

    #[test]
    fn the_generated_file_explains_every_key() {
        for template in [Template::Shell, Template::Python, Template::Rust] {
            let files = plan(&opts(template, false, false)).unwrap();
            let toml = &file(&files, "job.toml").content;
            for key in EVERY_KEY {
                assert!(
                    toml.contains(key),
                    "{template:?} job.toml never mentions {key}\n{toml}"
                );
            }
            for variable in crate::jobs::handoff::TEMPLATE_VARIABLES {
                assert!(
                    toml.contains(&format!("{{{{{variable}}}}}")) || variable.contains('*'),
                    "{variable}"
                );
            }
        }
    }

    #[test]
    fn schedule_options_set_the_schedule() {
        let mut every = opts(Template::Shell, false, false);
        every.schedule = Some(ScheduleInput::Every("15m".into()));
        let toml = file(&plan(&every).unwrap(), "job.toml").content.clone();
        let job = parse_job_toml(&toml, "demo", Path::new("/j")).unwrap();
        assert_eq!(
            job.schedule,
            Schedule::Every(std::time::Duration::from_secs(900))
        );

        let mut cron = opts(Template::Python, false, false);
        cron.schedule = Some(ScheduleInput::Cron("0 9 * * 1-5".into()));
        let toml = file(&plan(&cron).unwrap(), "job.toml").content.clone();
        let job = parse_job_toml(&toml, "demo", Path::new("/j")).unwrap();
        assert!(matches!(job.schedule, Schedule::Cron(_)));

        let mut workbox = opts(Template::Workbox, false, false);
        workbox.schedule = Some(ScheduleInput::Cron("0 9 * * *".into()));
        let toml = file(&plan(&workbox).unwrap(), "job.toml").content.clone();
        let job = parse_job_toml(&toml, "demo", Path::new("/j")).unwrap();
        assert!(matches!(job.schedule, Schedule::Cron(_)));
        assert!(!job.enabled);
    }

    #[test]
    fn bad_ids_and_templates_are_refused() {
        let mut bad = opts(Template::Shell, false, false);
        for id in ["", "../x", ".hidden", "a b", "a/b"] {
            bad.id = id.into();
            assert!(plan(&bad).is_err(), "{id:?}");
        }
        assert!(Template::parse("go")
            .unwrap_err()
            .contains("shell, python, rust, workbox"));
        assert_eq!(Template::parse("rust").unwrap(), Template::Rust);
    }

    #[test]
    fn scripts_are_the_files_job_toml_names() {
        for template in [Template::Shell, Template::Python, Template::Rust] {
            let files = plan(&opts(template, true, true)).unwrap();
            let job =
                parse_job_toml(&file(&files, "job.toml").content, "demo", Path::new("/j")).unwrap();
            for command in [&job.command, &job.feed.as_ref().unwrap().command] {
                assert!(
                    files.iter().any(|f| f.name == command[1]),
                    "{template:?}: {command:?} names a file that is not written"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn write_creates_the_files_and_refuses_to_overwrite() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("flockr-scaffold-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let jobs = root.join("jobs");
        let made = write(&jobs, &opts(Template::Shell, true, true)).unwrap();
        assert_eq!(made.dir, jobs.join("demo"));
        assert_eq!(made.files, ["job.toml", "run.sh", "feed.sh", "prompt.md"]);
        let mode = std::fs::metadata(made.dir.join("run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);

        std::fs::write(made.dir.join("job.toml"), "mine").unwrap();
        let err = write(&jobs, &opts(Template::Python, false, false)).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(
            std::fs::read_to_string(made.dir.join("job.toml")).unwrap(),
            "mine"
        );
        assert!(!made.dir.join("run.py").exists());

        // A plain file in the way is refused too.
        std::fs::write(jobs.join("file-id"), "x").unwrap();
        let mut other = opts(Template::Shell, false, false);
        other.id = "file-id".into();
        assert!(write(&jobs, &other).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_workbox_example_is_embedded_unchanged_without_a_schedule() {
        let files = plan(&opts(Template::Workbox, false, false)).unwrap();
        assert_eq!(file(&files, "job.toml").content, WORKBOX_JOB_TOML);
        assert_eq!(file(&files, "prompt.md").content, WORKBOX_PROMPT);
    }
}
