//! Tests for validate and the dry run, with scripts run through a stub PATH.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::dry_run::{self, FeedOutcome, Options};
use super::scaffold::{self, NewJobOptions, Template};
use super::validate::{self, Context, JobReport, Target};
use super::JOBS_SKILL;
use crate::jobs::definition::parse_job_toml;
use crate::jobs::env::LoginEnvCache;

struct Sandbox {
    root: PathBuf,
    bin: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!(
            "flockr-authoring-{name}-{}-{nanos}",
            std::process::id()
        ));
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let sandbox = Self { root, bin };
        for stub in ["python3", "rust-script", "workbox", "claude"] {
            sandbox.stub(stub);
        }
        sandbox
    }

    fn stub(&self, name: &str) {
        let path = self.bin.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn cache(&self) -> LoginEnvCache {
        let path = format!("{}:/usr/bin:/bin", self.bin.display());
        LoginEnvCache::new(Box::new(move || Ok(vec![("PATH".into(), path.clone())])))
    }

    fn jobs(&self) -> PathBuf {
        self.root.join("jobs")
    }

    fn target(&self, id: &str) -> Target {
        Target {
            id: id.into(),
            dir: self.jobs().join(id),
        }
    }

    /// A job directory with the given files.
    fn job(&self, id: &str, files: &[(&str, &str)]) -> Target {
        let dir = self.jobs().join(id);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, content) in files {
            std::fs::write(dir.join(name), content).unwrap();
        }
        self.target(id)
    }

    fn validate(&self, target: &Target, run_feed: bool) -> JobReport {
        let cache = self.cache();
        validate::validate_job(
            target,
            &Context {
                cache: &cache,
                now_secs: 1_792_000_000,
                run_feed,
            },
        )
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|line| line.contains(needle))
}

fn assert_error(report: &JobReport, needle: &str) {
    assert!(
        has(&report.errors, needle),
        "expected an error containing {needle:?}, got errors {:?} warnings {:?}",
        report.errors,
        report.warnings
    );
}

const SH_JOB: &str = "command = [\"sh\", \"run.sh\"]\nevery = \"1h\"\n";

#[test]
fn every_template_scaffolds_and_validates_clean() {
    let sandbox = Sandbox::new("templates");
    for template in Template::ALL {
        for (feed, handoff) in [(false, false), (true, true)] {
            let id = format!("{}-{feed}-{handoff}", template.name());
            scaffold::write(
                &sandbox.jobs(),
                &NewJobOptions {
                    id: id.clone(),
                    template,
                    schedule: None,
                    feed,
                    handoff,
                },
            )
            .unwrap();
            let report = sandbox.validate(&sandbox.target(&id), false);
            assert!(
                report.ok,
                "{id}: errors {:?}\nwarnings {:?}",
                report.errors, report.warnings
            );
            assert_eq!(report.enabled, Some(false), "{id}");
            assert_eq!(report.next_runs.len(), 3, "{id}");
            assert!(has(&report.notes, "disabled"), "{id}");
        }
    }
}

#[test]
fn run_feed_executes_the_stub_feed_and_checks_its_output() {
    let sandbox = Sandbox::new("runfeed");
    scaffold::write(
        &sandbox.jobs(),
        &NewJobOptions {
            id: "fed".into(),
            template: Template::Shell,
            schedule: None,
            feed: true,
            handoff: false,
        },
    )
    .unwrap();
    let report = sandbox.validate(&sandbox.target("fed"), true);
    assert!(report.ok, "{:?}", report.errors);
    let feed = report.feed.unwrap();
    assert!(feed.ran);
    assert_eq!((feed.items, feed.unread), (Some(1), Some(1)));
    assert_eq!(feed.title.as_deref(), Some("Example feed"));
}

#[test]
fn a_missing_file_a_bad_key_and_bad_toml_are_errors() {
    let sandbox = Sandbox::new("parse");
    let report = sandbox.validate(&sandbox.target("nothing"), false);
    assert_error(&report, "could not read");

    let broken = sandbox.job("broken", &[("job.toml", "command = [\n")]);
    assert!(!sandbox.validate(&broken, false).ok);

    let typo = sandbox.job(
        "typo",
        &[("job.toml", "command = [\"sh\"]\nevry = \"1h\"\n")],
    );
    assert_error(&sandbox.validate(&typo, false), "evry");

    let both = sandbox.job(
        "both",
        &[(
            "job.toml",
            "command = [\"sh\"]\nevery = \"1h\"\nschedule = \"* * * * *\"\n",
        )],
    );
    assert_error(&sandbox.validate(&both, false), "exactly one");
}

#[test]
fn command_and_script_problems_are_errors() {
    let sandbox = Sandbox::new("command");
    let missing = sandbox.job(
        "missing-tool",
        &[(
            "job.toml",
            "command = [\"no-such-tool-xyz\", \"a\"]\nevery = \"1h\"\n",
        )],
    );
    let report = sandbox.validate(&missing, false);
    assert_error(
        &report,
        "'no-such-tool-xyz' was not found on the job's PATH",
    );
    assert!(
        has(&[report.env.clone().unwrap()], "login-shell"),
        "{:?}",
        report.env
    );

    let rust = sandbox.job(
        "no-rust-script",
        &[(
            "job.toml",
            "command = [\"rust-script\", \"job.rs\"]\nenv = \"server\"\nevery = \"1h\"\n",
        )],
    );
    // Server environment: PATH is the test process's, which has no rust-script
    // unless the machine has one installed.
    let report = sandbox.validate(&rust, false);
    assert_error(&report, "job.rs does not exist");

    let no_script = sandbox.job("no-script", &[("job.toml", SH_JOB)]);
    assert_error(
        &sandbox.validate(&no_script, false),
        "the script run.sh does not exist",
    );

    let relative = sandbox.job(
        "relative",
        &[
            ("job.toml", "command = [\"./tool.sh\"]\nevery = \"1h\"\n"),
            ("tool.sh", "#!/bin/sh\n"),
        ],
    );
    assert_error(&sandbox.validate(&relative, false), "no executable file at");
    std::fs::set_permissions(
        relative.dir.join("tool.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(sandbox.validate(&relative, false).ok);

    let cwd = sandbox.job(
        "bad-cwd",
        &[
            ("job.toml", &format!("{SH_JOB}cwd = \"nope\"\n")),
            ("run.sh", ""),
        ],
    );
    assert_error(&sandbox.validate(&cwd, false), "cwd:");

    let env_file = sandbox.job(
        "bad-env-file",
        &[
            ("job.toml", &format!("{SH_JOB}env_file = \"missing.env\"\n")),
            ("run.sh", ""),
        ],
    );
    assert_error(&sandbox.validate(&env_file, false), "env_file");

    let feed = sandbox.job(
        "bad-feed-command",
        &[
            (
                "job.toml",
                &format!("{SH_JOB}[feed]\ncommand = [\"no-such-feed-xyz\"]\n"),
            ),
            ("run.sh", ""),
        ],
    );
    assert_error(
        &sandbox.validate(&feed, false),
        "feed.command: 'no-such-feed-xyz'",
    );
}

#[test]
fn a_cron_that_never_fires_and_a_long_timeout_are_flagged() {
    let sandbox = Sandbox::new("schedule");
    let never = sandbox.job(
        "never",
        &[(
            "job.toml",
            "command = [\"sh\", \"-c\", \"true\"]\nschedule = \"0 0 31 2 *\"\n",
        )],
    );
    assert_error(&sandbox.validate(&never, false), "never fires");

    let slow = sandbox.job(
        "slow",
        &[(
            "job.toml",
            "command = [\"sh\", \"-c\", \"true\"]\nevery = \"5m\"\ntimeout = \"10m\"\n",
        )],
    );
    let report = sandbox.validate(&slow, false);
    assert!(report.ok);
    assert!(
        has(&report.warnings, "not shorter than the interval"),
        "{:?}",
        report.warnings
    );

    let cron = sandbox.job(
        "cron",
        &[(
            "job.toml",
            "command = [\"sh\", \"-c\", \"true\"]\nschedule = \"0 9 * * 1-5\"\n",
        )],
    );
    let report = sandbox.validate(&cron, false);
    assert_eq!(report.next_runs.len(), 3);
    assert_eq!(report.schedule.as_deref(), Some("0 9 * * 1-5"));
}

#[test]
fn feed_work_problems_are_found() {
    let sandbox = Sandbox::new("feed-work");
    let work = |extra: &str| {
        format!("{SH_JOB}[feed]\ncommand = [\"sh\", \"run.sh\"]\n[feed.work]\n{extra}")
    };

    let unknown = sandbox.job(
        "work-unknown-var",
        &[
            ("job.toml", &work("prompt = \"{{items}} {{nope}}\"\n")),
            ("run.sh", ""),
        ],
    );
    assert_error(
        &sandbox.validate(&unknown, false),
        "feed.work.prompt: unknown template variable {{nope}}",
    );

    let missing = sandbox.job(
        "work-missing-prompt",
        &[
            ("job.toml", &work("prompt_file = \"gone.md\"\n")),
            ("run.sh", ""),
        ],
    );
    assert_error(
        &sandbox.validate(&missing, false),
        "feed.work.prompt_file: could not read",
    );

    let in_file = sandbox.job(
        "work-var-in-file",
        &[
            ("job.toml", &work("prompt_file = \"w.md\"\n")),
            ("w.md", "hello {{itemz.title}}"),
            ("run.sh", ""),
        ],
    );
    let report = sandbox.validate(&in_file, false);
    assert_error(&report, "{{itemz.title}}");
    assert!(has(&report.errors, "w.md"), "{:?}", report.errors);

    let both = sandbox.job(
        "work-both",
        &[
            (
                "job.toml",
                &work("prompt = \"a\"\nprompt_file = \"w.md\"\n"),
            ),
            ("run.sh", ""),
        ],
    );
    assert!(!sandbox.validate(&both, false).ok);

    let fine = sandbox.job(
        "work-ok",
        &[
            (
                "job.toml",
                &work(
                    "agent_command = \"claude --file {prompt_file}\"\ncwd = \"/nonexistent-dir/{{item.field.repo}}\"\ntab_title = \"{{job.title}} {{count}}\"\nprompt = \"{{items}} {{items_json}} {{item.url}} {{item.field.repo}}\"\n",
                ),
            ),
            ("run.sh", ""),
        ],
    );
    let report = sandbox.validate(&fine, false);
    assert!(report.ok, "{:?}", report.errors);

    let bad_cwd = sandbox.job(
        "work-cwd",
        &[
            ("job.toml", &work("cwd = \"/nonexistent-dir\"\n")),
            ("run.sh", ""),
        ],
    );
    let report = sandbox.validate(&bad_cwd, false);
    assert!(report.ok);
    assert!(
        has(&report.warnings, "feed.work.cwd"),
        "{:?}",
        report.warnings
    );
}

#[test]
fn handoff_problems_are_found() {
    let sandbox = Sandbox::new("handoff");
    let handoff = |extra: &str| format!("{SH_JOB}[handoff]\n{extra}");

    let unknown = sandbox.job(
        "unknown-var",
        &[
            ("job.toml", &handoff("ask = \"{{nope}} items\"\n")),
            ("run.sh", ""),
        ],
    );
    assert_error(
        &sandbox.validate(&unknown, false),
        "unknown template variable {{nope}}",
    );

    let missing_prompt = sandbox.job(
        "missing-prompt",
        &[
            ("job.toml", &handoff("prompt_file = \"gone.md\"\n")),
            ("run.sh", ""),
        ],
    );
    assert_error(
        &sandbox.validate(&missing_prompt, false),
        "handoff.prompt_file: could not read",
    );

    let in_file = sandbox.job(
        "var-in-file",
        &[
            ("job.toml", &handoff("prompt_file = \"p.md\"\n")),
            ("p.md", "hello {{stdoutt}}"),
            ("run.sh", ""),
        ],
    );
    let report = sandbox.validate(&in_file, false);
    assert_error(&report, "{{stdoutt}}");
    assert!(
        has(&report.errors, "p.md"),
        "names the file: {:?}",
        report.errors
    );

    let placeholder = sandbox.job(
        "placeholder-no-prompt",
        &[
            (
                "job.toml",
                &handoff("agent_command = \"claude --file {prompt_file}\"\n"),
            ),
            ("run.sh", ""),
        ],
    );
    assert_error(
        &sandbox.validate(&placeholder, false),
        "{prompt_file} but the handoff has no prompt",
    );

    let fine = sandbox.job(
        "placeholder-ok",
        &[
            (
                "job.toml",
                &handoff(
                    "agent_command = \"claude --file {prompt_file}\"\nprompt = \"hi {{job.id}}\"\n",
                ),
            ),
            ("run.sh", ""),
        ],
    );
    assert!(sandbox.validate(&fine, false).ok);

    let no_feed = sandbox.job(
        "count-without-feed",
        &[
            (
                "job.toml",
                &handoff("ask = \"{{count}} items\"\ncwd = \"/definitely/not/here\"\n"),
            ),
            ("run.sh", ""),
        ],
    );
    let report = sandbox.validate(&no_feed, false);
    assert!(report.ok, "{:?}", report.errors);
    assert!(has(&report.warnings, "no [feed]"), "{:?}", report.warnings);
    assert!(
        has(&report.warnings, "handoff.cwd"),
        "{:?}",
        report.warnings
    );

    let context = sandbox.job(
        "context",
        &[
            ("job.toml", &handoff("ask = \"{{context.tag}}\"\n")),
            ("run.sh", ""),
        ],
    );
    let report = sandbox.validate(&context, false);
    assert!(report.ok);
    assert!(
        has(&report.warnings, "flockr_handoff"),
        "{:?}",
        report.warnings
    );

    let no_agent = sandbox.job(
        "agent-missing",
        &[
            (
                "job.toml",
                &handoff("agent_command = \"no-such-agent-xyz --go\"\n"),
            ),
            ("run.sh", ""),
        ],
    );
    let report = sandbox.validate(&no_agent, false);
    assert!(report.ok);
    assert!(
        has(&report.warnings, "'no-such-agent-xyz'"),
        "{:?}",
        report.warnings
    );
}

#[test]
fn run_feed_reports_bad_output_and_failures() {
    let sandbox = Sandbox::new("badfeed");
    let feed_job = |script: &str| {
        let id = format!("feed-{}", script.len());
        sandbox.job(
            &id,
            &[
                (
                    "job.toml",
                    &format!("{SH_JOB}[feed]\ncommand = [\"sh\", \"feed.sh\"]\n"),
                ),
                ("run.sh", ""),
                ("feed.sh", script),
            ],
        )
    };

    let noisy = feed_job("echo 'log line'\necho '{\"version\":1,\"items\":[]}'\n");
    assert_error(
        &sandbox.validate(&noisy, true),
        "feed output: not valid JSON",
    );

    let failing = feed_job("echo oops >&2\nexit 3\n");
    let report = sandbox.validate(&failing, true);
    assert_error(&report, "feed.command failed: exit 3: oops");

    let empty = feed_job("true\n");
    assert_error(&sandbox.validate(&empty, true), "printed nothing");

    let wrong_version = feed_job("echo '{\"version\":2,\"items\":[]}'\n#\n#\n");
    assert_error(
        &sandbox.validate(&wrong_version, true),
        "unsupported feed version",
    );

    let diagnostics = feed_job(
        "echo '{\"version\":1,\"items\":[{\"id\":\"a\",\"state\":\"wat\",\"actions\":[{\"key\":\"j\",\"command\":[\"x\"]}]}]}'\n#\n#\n#\n#\n",
    );
    let report = sandbox.validate(&diagnostics, true);
    assert!(report.ok, "{:?}", report.errors);
    assert!(
        has(&report.warnings, "unknown state"),
        "{:?}",
        report.warnings
    );
    assert!(has(&report.warnings, "reserved"), "{:?}", report.warnings);

    // Not run unless asked.
    let report = sandbox.validate(&noisy, false);
    assert!(report.ok);
    assert!(has(&report.notes, "--run-feed"));
}

#[test]
fn targets_resolve_by_id_and_by_path() {
    let sandbox = Sandbox::new("targets");
    let job = sandbox.job("alpha", &[("job.toml", SH_JOB)]);
    sandbox.job("beta", &[("job.toml", SH_JOB)]);
    std::fs::create_dir_all(sandbox.jobs().join("not-a-job")).unwrap();

    assert_eq!(
        validate::resolve_target("alpha", &sandbox.jobs())
            .unwrap()
            .id,
        "alpha"
    );
    let by_dir = validate::resolve_target(job.dir.to_str().unwrap(), &sandbox.jobs()).unwrap();
    assert_eq!(by_dir.id, "alpha");
    let by_file =
        validate::resolve_target(job.dir.join("job.toml").to_str().unwrap(), &sandbox.jobs())
            .unwrap();
    assert_eq!(by_file.id, "alpha");
    assert!(validate::resolve_target("gamma", &sandbox.jobs())
        .unwrap_err()
        .contains("no job 'gamma'"));
    let ids: Vec<_> = validate::all_targets(&sandbox.jobs())
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(ids, ["alpha", "beta"]);
}

#[test]
fn the_report_renders_and_serializes() {
    let sandbox = Sandbox::new("render");
    let job = sandbox.job(
        "r",
        &[("job.toml", "command = [\"no-such-xyz\"]\nevery = \"1h\"\n")],
    );
    let report = sandbox.validate(&job, false);
    let text = validate::render_report(&report);
    assert!(text.contains("error: command: 'no-such-xyz'"), "{text}");
    assert!(text.contains("result: 1 error(s)"), "{text}");
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["id"], "r");
}

// ---- dry run ----

fn dry_job(
    sandbox: &Sandbox,
    id: &str,
    toml_extra: &str,
    script: &str,
) -> crate::jobs::definition::JobDef {
    let dir = sandbox.jobs().join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("run.sh"), script).unwrap();
    std::fs::write(
        dir.join("prompt.md"),
        "{{job.id}} said: {{stdout}} ({{count}} unread, exit {{exit_code}})",
    )
    .unwrap();
    let toml = format!("{SH_JOB}env = \"server\"\n{toml_extra}");
    parse_job_toml(&toml, id, &dir).unwrap_or_else(|errors| panic!("{errors:?}"))
}

fn dry(
    sandbox: &Sandbox,
    job: &crate::jobs::definition::JobDef,
    options: Options,
) -> (dry_run::DryRun, String) {
    let result = dry_run::run(job, &sandbox.cache(), options, 1_792_000_000_000).unwrap();
    let text = dry_run::render(job, &result);
    (result, text)
}

#[test]
fn a_dry_run_shows_the_output_the_feed_and_the_rendered_handoff() {
    let sandbox = Sandbox::new("dry");
    let job = dry_job(
        &sandbox,
        "dry",
        "[feed]\ncommand = [\"sh\", \"-c\", \"echo '{\\\"version\\\":1,\\\"title\\\":\\\"T\\\",\\\"items\\\":[{\\\"id\\\":\\\"a\\\",\\\"title\\\":\\\"A\\\",\\\"state\\\":\\\"unread\\\"},{\\\"id\\\":\\\"b\\\"}]}'\"]\n\
         [handoff]\nwhen = \"always\"\nask = \"{{count}} unread\"\nprompt_file = \"prompt.md\"\ncwd = \"/tmp\"\ntab_title = \"{{job.id}} {{date}}\"\n",
        "echo hello\necho oops >&2\n",
    );
    let (result, text) = dry(&sandbox, &job, Options::default());
    assert_eq!(dry_run::exit_code(&result), 0, "{text}");
    assert!(text.contains("result    ok in"), "{text}");
    assert!(text.contains("    hello"), "{text}");
    assert!(text.contains("    oops"), "{text}");
    assert!(text.contains("2 items, 1 unread, title \"T\""), "{text}");
    assert!(text.contains("would ask (from [handoff])"), "{text}");
    assert!(text.contains("ask        1 unread"), "{text}");
    assert!(text.contains("tab title  dry 2026-10-"), "{text}");
    assert!(text.contains("cwd        /tmp"), "{text}");
    assert!(text.contains("dry said: hello"), "{text}");
    assert!(text.contains("(1 unread, exit 0)"), "{text}");
    assert!(text.contains("agent      claude \"$(cat"), "{text}");
    assert!(matches!(result.feed, FeedOutcome::Parsed(_)));
    // The `w` preview uses the built-in defaults without a [feed.work] table.
    assert!(text.contains("work (w in the Feed overlay)"), "{text}");
    assert!(text.contains("built-in defaults; no [feed.work]"), "{text}");
    assert!(
        text.contains("Here are 2 items from T I want to work on:"),
        "{text}"
    );
    assert!(text.contains("tab title  dry · 2 items"), "{text}");
}

#[test]
fn a_dry_run_previews_a_configured_feed_work_table() {
    let sandbox = Sandbox::new("dry-work");
    let job = dry_job(
        &sandbox,
        "dry-work",
        "[feed]\ncommand = [\"sh\", \"-c\", \"echo '{\\\"version\\\":1,\\\"items\\\":[{\\\"id\\\":\\\"a\\\",\\\"title\\\":\\\"A\\\"}]}'\"]\n\
         [feed.work]\nagent_command = \"claude --add-dir /tmp {prompt_file}\"\ncwd = \"/tmp\"\ntab_title = \"W {{item.id}}\"\nprompt = \"look at {{item.title}} ({{count}})\"\n",
        "true\n",
    );
    let (result, text) = dry(&sandbox, &job, Options::default());
    assert_eq!(dry_run::exit_code(&result), 0, "{text}");
    assert!(
        text.contains("marking the first 1 item (from [feed.work])"),
        "{text}"
    );
    assert!(text.contains("tab title  W a"), "{text}");
    assert!(text.contains("cwd        /tmp"), "{text}");
    assert!(
        text.contains("agent      claude --add-dir /tmp '<state dir>/work/<time>.md'")
            || text.contains("agent      claude --add-dir /tmp <state dir>/work/<time>.md"),
        "{text}"
    );
    assert!(text.contains("look at A (1)"), "{text}");
}

#[test]
fn draft_options_are_validated() {
    let sandbox = Sandbox::new("draft-keys");
    let work = |extra: &str| {
        format!("{SH_JOB}[feed]\ncommand = [\"sh\", \"run.sh\"]\n[feed.work]\n{extra}")
    };
    let handoff = |extra: &str| format!("{SH_JOB}[handoff]\nprompt = \"p\"\n{extra}");

    let bad = sandbox.job(
        "bad-prefill",
        &[("job.toml", &work("prefill = \"typo\"\n")), ("run.sh", "")],
    );
    assert_error(
        &sandbox.validate(&bad, false),
        "feed.work.prefill: 'typo' is not valid; use paste or mention",
    );
    let bad = sandbox.job(
        "bad-handoff-prefill",
        &[
            ("job.toml", &handoff("prefill = \"all\"\n")),
            ("run.sh", ""),
        ],
    );
    assert_error(
        &sandbox.validate(&bad, false),
        "handoff.prefill: 'all' is not valid",
    );

    let contradiction = sandbox.job(
        "submit-and-prefill",
        &[
            ("job.toml", &work("submit = true\nprefill = \"mention\"\n")),
            ("run.sh", ""),
        ],
    );
    assert_error(
        &sandbox.validate(&contradiction, false),
        "feed.work.prefill: only applies when submit = false",
    );

    let fine = sandbox.job(
        "draft-fine",
        &[
            ("job.toml", &work("prefill = \"mention\"\n")),
            ("run.sh", ""),
        ],
    );
    let report = sandbox.validate(&fine, false);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(!has(&report.warnings, "submit"), "{:?}", report.warnings);

    let not_bool = sandbox.job(
        "submit-text",
        &[("job.toml", &work("submit = \"no\"\n")), ("run.sh", "")],
    );
    assert!(!sandbox.validate(&not_bool, false).errors.is_empty());

    // A {prompt_file} command keeps its own mechanism: say submit is ignored.
    let placeholder = sandbox.job(
        "submit-placeholder",
        &[
            (
                "job.toml",
                &work("agent_command = \"claude {prompt_file}\"\nsubmit = false\n"),
            ),
            ("run.sh", ""),
        ],
    );
    let report = sandbox.validate(&placeholder, false);
    assert!(
        has(&report.warnings, "feed.work.submit = false has no effect"),
        "{:?}",
        report.warnings
    );
    let placeholder = sandbox.job(
        "handoff-submit-placeholder",
        &[
            (
                "job.toml",
                &handoff("agent_command = \"claude {prompt_file}\"\nsubmit = false\n"),
            ),
            ("run.sh", ""),
        ],
    );
    let report = sandbox.validate(&placeholder, false);
    assert!(
        has(&report.warnings, "handoff.submit = false has no effect"),
        "{:?}",
        report.warnings
    );
}

#[test]
fn a_dry_run_says_whether_the_prompt_is_submitted_or_left_as_a_draft() {
    let sandbox = Sandbox::new("dry-draft");
    let feed = "[feed]\ncommand = [\"sh\", \"-c\", \"echo '{\\\"version\\\":1,\\\"items\\\":[{\\\"id\\\":\\\"a\\\",\\\"title\\\":\\\"A\\\"}]}'\"]\n";

    // [feed.work] defaults to a draft; [handoff] defaults to submitting.
    let job = dry_job(
        &sandbox,
        "dry-draft-default",
        &format!("{feed}[handoff]\nwhen = \"always\"\nprompt = \"go\"\n"),
        "true\n",
    );
    let (_, text) = dry(&sandbox, &job, Options::default());
    assert!(
        text.contains("submit     yes: the prompt is passed to the agent"),
        "{text}"
    );
    assert!(
        text.contains("submit     no: the agent starts with no prompt; the prompt is pasted into its input box (prefill = \"paste\"), without Enter"),
        "{text}"
    );

    let job = dry_job(
        &sandbox,
        "dry-draft-mention",
        &format!(
            "{feed}[feed.work]\nprefill = \"mention\"\n[handoff]\nwhen = \"always\"\nprompt = \"go\"\nsubmit = false\n"
        ),
        "true\n",
    );
    let (_, text) = dry(&sandbox, &job, Options::default());
    assert_eq!(
        text.matches("an @mention of the prompt file is typed")
            .count(),
        1,
        "the work preview mentions: {text}"
    );
    assert!(
        text.contains("agent      claude\n"),
        "no prompt argument in the draft launch: {text}"
    );

    let job = dry_job(
        &sandbox,
        "dry-submit-true",
        &format!("{feed}[feed.work]\nsubmit = true\n"),
        "true\n",
    );
    let (_, text) = dry(&sandbox, &job, Options::default());
    assert!(text.contains("submit     yes"), "{text}");
    assert!(text.contains("agent      claude \"$(cat"), "{text}");
}

#[test]
fn a_failing_command_exits_non_zero_and_failure_handoffs_ask() {
    let sandbox = Sandbox::new("dryfail");
    let job = dry_job(
        &sandbox,
        "failing",
        "[handoff]\nwhen = \"on_failure\"\nprompt = \"it exited {{exit_code}}: {{stderr_tail}}\"\n",
        "echo broken >&2\nexit 7\n",
    );
    let (result, text) = dry(&sandbox, &job, Options::default());
    assert_eq!(dry_run::exit_code(&result), 1);
    assert!(text.contains("failed in"), "{text}");
    assert!(text.contains("(exit 7)"), "{text}");
    assert!(text.contains("it exited 7: broken"), "{text}");

    let quiet = dry_job(
        &sandbox,
        "quiet",
        "[handoff]\nwhen = \"on_success\"\n",
        "exit 0\n",
    );
    let (result, text) = dry(&sandbox, &quiet, Options::default());
    assert_eq!(dry_run::exit_code(&result), 0);
    assert!(text.contains("would ask"), "{text}");
    let off = dry_job(
        &sandbox,
        "off",
        "[handoff]\nwhen = \"on_failure\"\n",
        "exit 0\n",
    );
    let (_, text) = dry(&sandbox, &off, Options::default());
    assert!(
        text.contains("no offer for this run (when = \"on_failure\""),
        "{text}"
    );
}

#[test]
fn a_dynamic_handoff_line_always_asks_and_feeds_context() {
    let sandbox = Sandbox::new("drydyn");
    let job = dry_job(
        &sandbox,
        "dyn",
        "",
        "echo '{\"flockr_handoff\": {\"ask\": \"tag {{context.tag}}\", \"prompt\": \"go {{context.tag}}\", \"cwd\": \"/tmp\", \"context\": {\"tag\": \"v9\"}}}'\n",
    );
    let (result, text) = dry(&sandbox, &job, Options::default());
    assert_eq!(dry_run::exit_code(&result), 0, "{text}");
    assert!(
        text.contains("would ask (from the flockr_handoff line)"),
        "{text}"
    );
    assert!(text.contains("ask        tag v9"), "{text}");
    assert!(text.contains("go v9"), "{text}");
}

#[test]
fn a_handoff_that_cannot_render_is_an_error_exit() {
    let sandbox = Sandbox::new("dryerr");
    let job = dry_job(
        &sandbox,
        "broken-handoff",
        "[handoff]\nwhen = \"always\"\nprompt_file = \"missing.md\"\n",
        "echo hi\n",
    );
    let (result, text) = dry(&sandbox, &job, Options::default());
    assert_eq!(dry_run::exit_code(&result), 1);
    assert!(
        text.contains("error: could not read the prompt file"),
        "{text}"
    );
}

#[test]
fn a_feed_that_fails_is_reported_and_exits_non_zero() {
    let sandbox = Sandbox::new("dryfeed");
    let job = dry_job(
        &sandbox,
        "badfeed",
        "[feed]\ncommand = [\"sh\", \"-c\", \"echo not json\"]\n",
        "echo ok\n",
    );
    let (result, text) = dry(&sandbox, &job, Options::default());
    assert_eq!(dry_run::exit_code(&result), 1);
    assert!(text.contains("failed: not valid JSON"), "{text}");
    assert!(result.feed_snapshot.is_none());
}

#[test]
fn no_exec_runs_nothing_and_renders_the_handoff_for_empty_stdout() {
    let sandbox = Sandbox::new("drynoexec");
    let marker = sandbox.root.join("ran");
    let job = dry_job(
        &sandbox,
        "noexec",
        &format!(
            "[feed]\ncommand = [\"sh\", \"-c\", \"touch {0}.feed\"]\n[handoff]\nwhen = \"on_success\"\nprompt_file = \"prompt.md\"\ncwd = \"/tmp\"\n",
            marker.display()
        ),
        &format!("touch {}\n", marker.display()),
    );
    let (result, text) = dry(&sandbox, &job, Options { no_exec: true });
    assert!(!marker.exists(), "the command must not run");
    assert!(
        !sandbox.root.join("ran.feed").exists(),
        "the feed must not run"
    );
    assert_eq!(dry_run::exit_code(&result), 0, "{text}");
    assert!(
        text.contains("--no-exec: the command was not run"),
        "{text}"
    );
    assert!(text.contains("noexec said:  (0 unread, exit 0)"), "{text}");
}

#[test]
fn a_dry_run_leaves_no_state_behind_and_sandboxes_the_state_dir() {
    let sandbox = Sandbox::new("drystate");
    let probe = sandbox.root.join("state-dir-seen");
    let job = dry_job(
        &sandbox,
        "stateful",
        "",
        &format!(
            "echo \"$FLOCKR_JOB_STATE_DIR\" > {}\ntouch \"$FLOCKR_JOB_STATE_DIR/scratch-file\"\n",
            probe.display()
        ),
    );
    let before = std::fs::read_dir(&sandbox.root).unwrap().count();
    let (result, _) = dry(&sandbox, &job, Options::default());
    assert_eq!(dry_run::exit_code(&result), 0);
    let seen = PathBuf::from(std::fs::read_to_string(&probe).unwrap().trim());
    assert!(
        !seen.exists(),
        "the scratch state dir is removed: {}",
        seen.display()
    );
    assert!(
        !seen.starts_with(crate::config::state_dir()),
        "the job must not be pointed at the real state dir"
    );
    assert!(!sandbox.jobs().join("stateful/state.json").exists());
    // Only the probe file was added next to the job directories.
    assert_eq!(
        std::fs::read_dir(&sandbox.root).unwrap().count(),
        before + 1
    );
}

#[test]
fn the_snapshot_matches_what_the_server_builds() {
    let sandbox = Sandbox::new("snap");
    let job = dry_job(
        &sandbox,
        "snap",
        "[feed]\ncommand = [\"true\"]\ntitle = \"Job title wins\"\n",
        "",
    );
    let parsed = crate::feed::format::parse_feed(
        "{\"version\":1,\"title\":\"Own\",\"items\":[{\"id\":\"a\",\"state\":\"unread\",\"actions\":[{\"key\":\"d\",\"command\":[\"x\"]}]}]}",
    )
    .unwrap();
    let snapshot = dry_run::snapshot(&job, &parsed);
    assert_eq!(snapshot.unread, 1);
    assert!(
        snapshot.json.contains("Job title wins"),
        "{}",
        snapshot.json
    );
    assert!(
        !snapshot.json.contains("\"command\""),
        "action commands stay out"
    );
}

// ---- skill ----

#[test]
fn the_skill_is_embedded_and_covers_the_contract() {
    assert!(JOBS_SKILL.starts_with("---\nname: flockr-jobs\n"));
    for required in [
        "flockr job new",
        "flockr job validate",
        "flockr job run <id> --dry-run",
        "flockr job enable",
        "prefix+t",
        "flockr job logs",
        "flockr decision list",
        "flockr job schedule",
        "flockr_handoff",
        "single pass",
        "{{count}}",
        "~/.zshrc",
        "secrets",
        "only one JSON object",
        "j k q g G R S / x X w",
        "[feed.work]",
        "feed.work",
        "workbox-sync",
        "gh search prs",
        "rust-script",
        "flockr job build",
        "--no-exec",
    ] {
        assert!(
            JOBS_SKILL.contains(required),
            "the skill never mentions {required:?}"
        );
    }
    for template in Template::ALL {
        assert!(JOBS_SKILL.contains(template.name()), "{}", template.name());
    }
    for variable in crate::jobs::handoff::TEMPLATE_VARIABLES {
        assert!(
            JOBS_SKILL.contains(&format!("{{{{{variable}}}}}")),
            "the skill never lists {{{{{variable}}}}}"
        );
    }
    for cap in [
        crate::feed::format::MAX_ITEMS,
        crate::feed::format::MAX_ACTIONS,
        crate::feed::format::MAX_TITLE_CHARS,
        crate::feed::format::MAX_URL_CHARS,
    ] {
        assert!(
            JOBS_SKILL.contains(&cap.to_string()),
            "cap {cap} is not in the skill"
        );
    }
}

#[test]
fn the_skill_toml_examples_parse() {
    // Every fenced toml block in the skill must be a valid job.toml fragment
    // once the worked-example scaffolding is filled in.
    let mut checked = 0;
    for block in JOBS_SKILL.split("```toml\n").skip(1) {
        let toml = block.split("```").next().unwrap();
        let full = if toml.contains("command =") {
            toml.to_owned()
        } else {
            format!("command = [\"x\"]\nevery = \"1h\"\n{toml}")
        };
        // The reference block lists both `every` and a commented `schedule`.
        parse_job_toml(&full, "my-job", Path::new("/jobs/my-job"))
            .unwrap_or_else(|errors| panic!("{errors:?}\n{full}"));
        checked += 1;
    }
    assert!(checked >= 3, "found {checked} toml blocks");
}
