//! Feed work: `w` in the Feed overlay starts an agent tab on the items the user
//! selected. This module holds the pure part: rendering the optional
//! `[feed.work]` templates for a set of feed items. The app turns the result
//! into a prompt file and a tab through the same code as a decision's Start
//! (`crate::app::api::decisions`).
//!
//! Templates use the handoff syntax, `{{name}}`, with these names: `items` (a
//! readable list), `items_json`, `count`, `date`, `job.id`, `job.title`,
//! `feed.title`, and, for the first selected item, `item.id`, `item.title`,
//! `item.subtitle`, `item.badge`, `item.url`, `item.state`, `item.group`,
//! `item.updated_at`, `item.body` and `item.field.<label>` (a field's label
//! in lower case with other characters turned into `_`, for example
//! `item.field.repo`). A name without a value renders empty and is reported in
//! `WorkPlan::warnings`. Substitution is a single pass, so item text cannot
//! inject further placeholders.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::definition::{resolve_path, FeedWorkDef, JobDef, Prefill};
use super::handoff::{
    cap_head, render, single_line, RenderMode, Vars, DEFAULT_AGENT_COMMAND, MAX_PROMPT_CHARS,
    MAX_TAB_TITLE_CHARS,
};
use crate::api::schema::FeedItemInfo;

/// Most items one work request uses; more are left out with a warning.
pub(crate) const MAX_WORK_ITEMS: usize = 50;
/// Characters of one item's body inside `{{items}}`.
pub(crate) const MAX_ITEM_BODY_CHARS: usize = 2_000;
/// Characters of `{{items}}`; items are dropped from the end to fit.
pub(crate) const MAX_ITEMS_TEXT_CHARS: usize = 60_000;
/// Characters of one `{{item.*}}` value.
pub(crate) const MAX_ITEM_VALUE_CHARS: usize = 8_000;

/// The names every work template can use; `item.field.<label>` is also
/// available.
pub(crate) const WORK_TEMPLATE_VARIABLES: &[&str] = &[
    "items",
    "items_json",
    "count",
    "date",
    "job.id",
    "job.title",
    "feed.title",
    "item.id",
    "item.title",
    "item.subtitle",
    "item.badge",
    "item.url",
    "item.state",
    "item.group",
    "item.updated_at",
    "item.body",
];
/// Prefix of the per-field variables of the first item.
pub(crate) const ITEM_FIELD_PREFIX: &str = "item.field.";

pub(crate) const DEFAULT_TAB_TITLE: &str = "{{job.title}} · {{count}} items";
const DEFAULT_TAB_TITLE_ONE: &str = "{{job.title}} · 1 item";
/// The prompt when `[feed.work]` sets none.
pub(crate) const DEFAULT_PROMPT: &str = "Here are {{count}} items from {{feed.title}} I want to work on:\n\n{{items}}\n\nRead each link/ticket, summarize what each needs, and propose a plan. Ask before changing code.\n";
const DEFAULT_PROMPT_ONE: &str = "Here is 1 item from {{feed.title}} I want to work on:\n\n{{items}}\n\nRead the link/ticket, summarize what it needs, and propose a plan. Ask before changing code.\n";

pub(crate) struct WorkInput<'a> {
    pub(crate) job: &'a JobDef,
    pub(crate) feed_title: &'a str,
    /// The selected items, in the order they should be listed.
    pub(crate) items: &'a [FeedItemInfo],
    /// Local date, `YYYY-MM-DD`.
    pub(crate) date: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkPlan {
    pub(crate) tab_title: String,
    /// The working directory the templates asked for, or `None` to use the
    /// default (`default_cwd`).
    pub(crate) cwd: Option<PathBuf>,
    /// The agent command with its variables filled in (shell-quoted).
    pub(crate) agent_command: String,
    pub(crate) prompt: String,
    /// Submit the prompt to the agent (`[feed.work].submit`, default false:
    /// the prompt is left in the input box as a draft).
    pub(crate) submit: bool,
    pub(crate) prefill: Prefill,
    pub(crate) warnings: Vec<String>,
}

/// The directory a work tab opens in when `[feed.work].cwd` is unset or does
/// not exist: the job's `cwd` when the job file sets one, else `$HOME`, else
/// the job's own directory.
pub(crate) fn default_cwd(job: &JobDef) -> PathBuf {
    if job.cwd != job.dir {
        return job.cwd.clone();
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .unwrap_or_else(|| job.cwd.clone())
}

/// A field label as a template name: lower case, runs of other characters
/// turned into one `_`.
pub(crate) fn field_slug(label: &str) -> String {
    let mut slug = String::new();
    for ch in label.chars() {
        if ch.is_alphanumeric() {
            slug.extend(ch.to_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('_') {
            slug.push('_');
        }
    }
    slug.trim_end_matches('_').to_owned()
}

fn state_text(item: &FeedItemInfo) -> &'static str {
    use crate::api::schema::FeedItemState::{Done, Error, Read, Unread, Warn};
    match item.state {
        Unread => "unread",
        Read => "read",
        Done => "done",
        Warn => "warn",
        Error => "error",
    }
}

/// One item of `{{items}}`.
fn item_text(number: usize, item: &FeedItemInfo) -> String {
    let mut out = String::new();
    let heading = match item.badge.as_deref().filter(|badge| !badge.is_empty()) {
        Some(badge) => format!("{badge} — {}", item.title),
        None => item.title.clone(),
    };
    out.push_str(&format!(
        "{number}. {}\n",
        single_line(&heading, MAX_ITEM_VALUE_CHARS)
    ));
    if let Some(url) = item.url.as_deref().filter(|url| !url.is_empty()) {
        out.push_str(&format!("   Link: {url}\n"));
    }
    if let Some(subtitle) = item.subtitle.as_deref().filter(|text| !text.is_empty()) {
        out.push_str(&format!(
            "   Summary: {}\n",
            single_line(subtitle, MAX_ITEM_VALUE_CHARS)
        ));
    }
    out.push_str(&format!(
        "   Id: {}\n   State: {}\n",
        single_line(&item.id, MAX_ITEM_VALUE_CHARS),
        state_text(item)
    ));
    for field in &item.fields {
        out.push_str(&format!(
            "   {}: {}\n",
            single_line(&field.label, 80),
            single_line(&field.value, MAX_ITEM_VALUE_CHARS)
        ));
    }
    if let Some(body) = item.body.as_deref().filter(|body| !body.trim().is_empty()) {
        out.push_str("   Details:\n");
        for line in cap_head(body.trim(), MAX_ITEM_BODY_CHARS).lines() {
            out.push_str("     ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// The `{{items}}` text: items from the start are kept while they fit.
fn items_text(items: &[FeedItemInfo]) -> String {
    let mut out = String::new();
    for (index, item) in items.iter().enumerate() {
        let block = item_text(index + 1, item);
        if out.chars().count() + block.chars().count() + 1 > MAX_ITEMS_TEXT_CHARS {
            let left = items.len() - index;
            out.push_str(&format!(
                "\n… ({left} more item{} left out to keep the prompt short)\n",
                if left == 1 { "" } else { "s" }
            ));
            return out;
        }
        if index > 0 {
            out.push('\n');
        }
        out.push_str(&block);
    }
    out.trim_end().to_owned()
}

fn first_item_vars(
    item: &FeedItemInfo,
    scalars: &mut BTreeMap<&'static str, String>,
    dynamic: &mut BTreeMap<String, String>,
) {
    let cap = |text: &str| cap_head(text, MAX_ITEM_VALUE_CHARS);
    scalars.insert("item.id", cap(&item.id));
    scalars.insert("item.title", cap(&item.title));
    scalars.insert("item.subtitle", cap(item.subtitle.as_deref().unwrap_or("")));
    scalars.insert("item.badge", cap(item.badge.as_deref().unwrap_or("")));
    scalars.insert("item.url", cap(item.url.as_deref().unwrap_or("")));
    scalars.insert("item.state", state_text(item).to_owned());
    scalars.insert("item.group", cap(item.group.as_deref().unwrap_or("")));
    scalars.insert(
        "item.updated_at",
        cap(item.updated_at.as_deref().unwrap_or("")),
    );
    scalars.insert("item.body", cap(item.body.as_deref().unwrap_or("")));
    for field in &item.fields {
        let slug = field_slug(&field.label);
        if !slug.is_empty() {
            // The first field with a label wins.
            dynamic
                .entry(format!("{ITEM_FIELD_PREFIX}{slug}"))
                .or_insert_with(|| cap(&field.value));
        }
    }
}

/// Render the `[feed.work]` templates (or the defaults) for `input.items`.
/// `Err` is a problem the user must fix, such as an unreadable prompt file.
pub(crate) fn plan(input: &WorkInput<'_>) -> Result<WorkPlan, String> {
    if input.items.is_empty() {
        return Err("no items to work on".into());
    }
    let mut warnings = Vec::new();
    let mut items = input.items;
    if items.len() > MAX_WORK_ITEMS {
        warnings.push(format!(
            "{} items were selected; only the first {MAX_WORK_ITEMS} are used",
            items.len()
        ));
        items = &items[..MAX_WORK_ITEMS];
    }
    let work = input
        .job
        .feed
        .as_ref()
        .and_then(|feed| feed.work.as_ref())
        .cloned()
        .unwrap_or_else(FeedWorkDef::default);
    let single = items.len() == 1;

    let prompt_template = match (&work.prompt, &work.prompt_file) {
        (Some(inline), _) => inline.clone(),
        (None, Some(file)) => {
            let path = resolve_path(&input.job.dir, file);
            std::fs::read_to_string(&path).map_err(|err| {
                format!(
                    "could not read the feed.work prompt file {}: {err}",
                    path.display()
                )
            })?
        }
        (None, None) if single => DEFAULT_PROMPT_ONE.to_owned(),
        (None, None) => DEFAULT_PROMPT.to_owned(),
    };

    let mut scalars: BTreeMap<&'static str, String> = BTreeMap::new();
    let mut dynamic: BTreeMap<String, String> = BTreeMap::new();
    scalars.insert("items", items_text(items));
    scalars.insert(
        "items_json",
        super::handoff::feed_json(input.feed_title, items),
    );
    scalars.insert("count", items.len().to_string());
    scalars.insert("date", input.date.to_owned());
    scalars.insert("job.id", input.job.id.clone());
    scalars.insert("job.title", input.job.title.clone());
    scalars.insert("feed.title", input.feed_title.to_owned());
    first_item_vars(&items[0], &mut scalars, &mut dynamic);
    let vars = Vars::new(scalars, dynamic);

    let mut missing: Vec<String> = Vec::new();
    let mut take = |rendered: super::handoff::Rendered| {
        for name in rendered.missing {
            if !missing.contains(&name) {
                missing.push(name);
            }
        }
        rendered.text
    };
    let tab_title = single_line(
        &take(render(
            work.tab_title.as_deref().unwrap_or(if single {
                DEFAULT_TAB_TITLE_ONE
            } else {
                DEFAULT_TAB_TITLE
            }),
            &vars,
            RenderMode::Text,
        )),
        MAX_TAB_TITLE_CHARS,
    );
    let agent_command = take(render(
        work.agent_command
            .as_deref()
            .unwrap_or(DEFAULT_AGENT_COMMAND),
        &vars,
        RenderMode::Shell,
    ))
    .trim()
    .to_owned();
    let cwd_text = take(render(
        work.cwd.as_deref().unwrap_or(""),
        &vars,
        RenderMode::Text,
    ));
    let cwd = (!cwd_text.trim().is_empty()).then(|| resolve_path(&input.job.dir, cwd_text.trim()));
    let prompt = cap_head(
        &take(render(&prompt_template, &vars, RenderMode::Text)),
        MAX_PROMPT_CHARS,
    );
    if agent_command.is_empty() {
        return Err("the feed.work agent_command is empty".into());
    }
    if prompt.trim().is_empty() {
        return Err("the feed.work prompt renders empty; set prompt or prompt_file".into());
    }
    for name in missing {
        warnings.push(format!("template variable {{{{{name}}}}} has no value"));
    }
    Ok(WorkPlan {
        tab_title,
        cwd,
        agent_command,
        prompt,
        submit: work.submit.unwrap_or(false),
        prefill: work.prefill.unwrap_or_default(),
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{FeedField, FeedItemState};
    use crate::jobs::definition::parse_job_toml;

    fn job(extra: &str) -> JobDef {
        parse_job_toml(
            &format!(
                "command = [\"true\"]\nevery = \"1h\"\ntitle = \"Inbox\"\n[feed]\ncommand = [\"true\"]\n{extra}"
            ),
            "inbox",
            std::path::Path::new("/jobs/inbox"),
        )
        .unwrap_or_else(|diagnostics| panic!("job did not parse: {diagnostics:?}"))
    }

    fn item(id: &str, title: &str) -> FeedItemInfo {
        FeedItemInfo {
            id: id.into(),
            title: title.into(),
            subtitle: Some("github · review requested".into()),
            state: FeedItemState::Unread,
            badge: Some(format!("PR-{id}")),
            url: Some(format!("https://example.test/{id}")),
            updated_at: None,
            body: Some("first line\nsecond line".into()),
            fields: vec![
                FeedField {
                    label: "Repo".into(),
                    value: "platform-ui".into(),
                },
                FeedField {
                    label: "Where".into(),
                    value: "acme/platform-ui#571".into(),
                },
            ],
            group: Some("prs".into()),
            also_groups: Vec::new(),
            actions: Vec::new(),
        }
    }

    fn run(job: &JobDef, items: &[FeedItemInfo]) -> Result<WorkPlan, String> {
        plan(&WorkInput {
            job,
            feed_title: "Today (workbox)",
            items,
            date: "2026-10-19",
        })
    }

    #[test]
    fn a_work_plan_is_a_draft_unless_submit_is_true() {
        let items = [item("1", "Fix login")];
        let plan = run(&job(""), &items).unwrap();
        assert!(
            !plan.submit,
            "feed.work leaves the prompt as a draft by default"
        );
        assert_eq!(plan.prefill, Prefill::Paste);
        let plan = run(&job("[feed.work]\nsubmit = true\n"), &items).unwrap();
        assert!(plan.submit);
        let plan = run(&job("[feed.work]\nprefill = \"mention\"\n"), &items).unwrap();
        assert!(!plan.submit);
        assert_eq!(plan.prefill, Prefill::Mention);
    }

    #[test]
    fn the_default_prompt_lists_every_item_with_its_link() {
        let plan = run(&job(""), &[item("1", "Fix login"), item("2", "Add tests")]).unwrap();
        assert!(plan
            .prompt
            .starts_with("Here are 2 items from Today (workbox) I want to work on:"));
        assert!(plan.prompt.contains("1. PR-1 — Fix login"));
        assert!(plan.prompt.contains("2. PR-2 — Add tests"));
        assert!(plan.prompt.contains("Link: https://example.test/2"));
        assert!(plan.prompt.contains("Repo: platform-ui"));
        assert!(plan.prompt.contains("     second line"));
        assert!(plan.prompt.contains("Ask before changing code."));
        assert_eq!(plan.tab_title, "Inbox · 2 items");
        assert_eq!(plan.agent_command, "claude");
        assert_eq!(plan.cwd, None);
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    }

    #[test]
    fn one_item_reads_as_singular() {
        let plan = run(&job(""), &[item("1", "Fix login")]).unwrap();
        assert!(plan
            .prompt
            .starts_with("Here is 1 item from Today (workbox) I want to work on:"));
        assert_eq!(plan.tab_title, "Inbox · 1 item");
    }

    #[test]
    fn configured_templates_fill_every_variable() {
        let job = job(
            "[feed.work]\nagent_command = \"claude --model {{item.field.repo}}\"\ncwd = \"/code/{{item.field.repo}}\"\ntab_title = \"{{job.id}} {{count}} {{date}}\"\nprompt = \"{{count}}|{{job.title}}|{{feed.title}}|{{item.id}}|{{item.badge}}|{{item.url}}|{{item.state}}|{{item.group}}|{{item.field.where}}\"\n",
        );
        let plan = run(&job, &[item("7", "A"), item("8", "B")]).unwrap();
        assert_eq!(
            plan.prompt,
            "2|Inbox|Today (workbox)|7|PR-7|https://example.test/7|unread|prs|acme/platform-ui#571"
        );
        assert_eq!(plan.tab_title, "inbox 2 2026-10-19");
        assert_eq!(plan.agent_command, "claude --model platform-ui");
        assert_eq!(plan.cwd, Some(PathBuf::from("/code/platform-ui")));
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    }

    #[test]
    fn items_json_is_the_feed_document_of_the_selection() {
        let job = job("[feed.work]\nprompt = \"{{items_json}}\"\n");
        let plan = run(&job, &[item("1", "A"), item("2", "B")]).unwrap();
        let value: serde_json::Value = serde_json::from_str(&plan.prompt).unwrap();
        assert_eq!(value["title"], "Today (workbox)");
        assert_eq!(value["items"].as_array().unwrap().len(), 2);
        assert_eq!(value["items"][1]["id"], "2");
    }

    #[test]
    fn missing_variables_render_empty_and_are_reported() {
        let job = job("[feed.work]\nprompt = \"a {{item.field.nope}} b {{bogus}}\"\n");
        let mut bare = item("1", "A");
        bare.fields.clear();
        let plan = run(&job, &[bare]).unwrap();
        assert_eq!(plan.prompt, "a  b ");
        assert_eq!(plan.warnings.len(), 2, "{:?}", plan.warnings);
        assert!(plan.warnings[0].contains("item.field.nope"));
    }

    #[test]
    fn item_text_cannot_inject_placeholders() {
        let job = job("");
        let mut sneaky = item("1", "{{job.id}}");
        sneaky.body = Some("{{items_json}}".into());
        let plan = run(&job, &[sneaky]).unwrap();
        assert!(plan.prompt.contains("{{job.id}}"));
        assert!(plan.prompt.contains("{{items_json}}"));
    }

    #[test]
    fn long_bodies_and_many_items_are_capped() {
        let mut long = item("1", "A");
        long.body = Some("x".repeat(MAX_ITEM_BODY_CHARS * 3));
        let plan = run(&job(""), &[long]).unwrap();
        assert!(plan.prompt.contains("(truncated)"));
        assert!(plan.prompt.chars().count() < MAX_ITEM_BODY_CHARS + 1_000);

        let many: Vec<FeedItemInfo> = (0..MAX_WORK_ITEMS + 5)
            .map(|n| item(&n.to_string(), "T"))
            .collect();
        let plan = run(&job(""), &many).unwrap();
        assert!(plan.prompt.contains(&format!("{MAX_WORK_ITEMS}. PR-")));
        assert!(!plan
            .prompt
            .contains(&format!("{}. PR-", MAX_WORK_ITEMS + 1)));
        assert!(plan.prompt.contains("Here are 50 items"));
        assert!(plan.warnings[0].contains("only the first 50"));

        let big_body = "y".repeat(MAX_ITEM_BODY_CHARS);
        let heavy: Vec<FeedItemInfo> = (0..MAX_WORK_ITEMS)
            .map(|n| {
                let mut one = item(&n.to_string(), "T");
                one.body = Some(big_body.clone());
                one
            })
            .collect();
        let plan = run(&job(""), &heavy).unwrap();
        assert!(plan.prompt.contains("left out to keep the prompt short"));
        assert!(plan.prompt.chars().count() <= MAX_PROMPT_CHARS);
    }

    #[test]
    fn command_values_are_shell_quoted() {
        let job = job("[feed.work]\nagent_command = \"claude --title {{item.title}}\"\n");
        let plan = run(&job, &[item("1", "a b; rm -rf /")]).unwrap();
        assert_eq!(plan.agent_command, "claude --title 'a b; rm -rf /'");
    }

    #[test]
    fn the_prompt_file_is_read_from_the_job_directory() {
        let dir = std::env::temp_dir().join(format!(
            "flockr-feed-work-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("work.md"), "files: {{count}} {{item.title}}").unwrap();
        let parsed = parse_job_toml(
            "command = [\"true\"]\nevery = \"1h\"\n[feed]\ncommand = [\"true\"]\n[feed.work]\nprompt_file = \"work.md\"\n",
            "inbox",
            &dir,
        )
        .unwrap();
        let plan = run(&parsed, &[item("1", "Fix")]).unwrap();
        assert_eq!(plan.prompt, "files: 1 Fix");
        let missing = parse_job_toml(
            "command = [\"true\"]\nevery = \"1h\"\n[feed]\ncommand = [\"true\"]\n[feed.work]\nprompt_file = \"nope.md\"\n",
            "inbox",
            &dir,
        )
        .unwrap();
        let error = run(&missing, &[item("1", "Fix")]).unwrap_err();
        assert!(error.contains("nope.md"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_items_is_an_error_and_a_blank_prompt_is_refused() {
        assert!(run(&job(""), &[]).is_err());
        let job = job("[feed.work]\nprompt = \"   \"\n");
        assert!(run(&job, &[item("1", "A")]).unwrap_err().contains("empty"));
    }

    #[test]
    fn the_default_cwd_is_the_job_cwd_when_set_else_home() {
        let plain = job("");
        let home = std::env::var_os("HOME").map(PathBuf::from);
        if let Some(home) = home {
            assert_eq!(default_cwd(&plain), home);
        }
        let set = parse_job_toml(
            "command = [\"true\"]\nevery = \"1h\"\ncwd = \"/work/here\"\n[feed]\ncommand = [\"true\"]\n",
            "inbox",
            std::path::Path::new("/jobs/inbox"),
        )
        .unwrap();
        assert_eq!(default_cwd(&set), PathBuf::from("/work/here"));
    }

    #[test]
    fn field_slugs_are_template_names() {
        assert_eq!(field_slug("Repo"), "repo");
        assert_eq!(field_slug("Snoozed until"), "snoozed_until");
        assert_eq!(field_slug("  A / B  "), "a_b");
        assert_eq!(field_slug("***"), "");
    }

    #[test]
    fn the_workbox_example_work_block_is_valid_when_uncommented() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/jobs/workbox-sync");
        let text = std::fs::read_to_string(dir.join("job.toml"))
            .expect("examples/jobs/workbox-sync/job.toml is tracked");
        // The shipped file has no active [feed.work]: the built-in prompt applies.
        let shipped = parse_job_toml(&text, "workbox-sync", &dir).unwrap();
        assert!(shipped.feed.as_ref().unwrap().work.is_none());
        // Uncomment from `# [feed.work]` to the closing `# """`.
        let mut lines = Vec::new();
        let mut inside = false;
        let mut quotes = 0;
        for line in text.lines() {
            if line == "# [feed.work]" {
                inside = true;
            }
            if inside {
                let plain = line
                    .strip_prefix("# ")
                    .or_else(|| line.strip_prefix('#'))
                    .unwrap();
                quotes += plain.matches("\"\"\"").count();
                lines.push(plain.to_owned());
                if quotes >= 2 {
                    break;
                }
            }
        }
        assert!(lines.len() > 8, "found the example block: {lines:?}");
        let (head, _) = text.split_once("# [feed.work]").unwrap();
        let combined = format!("{head}{}\n", lines.join("\n"));
        let job =
            parse_job_toml(&combined, "workbox-sync", &dir).expect("uncommented block parses");
        let work = job
            .feed
            .as_ref()
            .unwrap()
            .work
            .as_ref()
            .expect("work table");
        assert_eq!(work.agent_command.as_deref(), Some("claude"));
        let mut one = item("1", "Fix login");
        one.fields = vec![FeedField {
            label: "Repo".into(),
            value: "platform-ui".into(),
        }];
        let two = item("2", "Review retry");
        let plan = run(&job, &[one, two]).unwrap();
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
        let cwd = plan.cwd.expect("cwd from the repo field");
        assert!(cwd.ends_with("code/work/platform-ui"), "{cwd:?}");
        assert_eq!(plan.tab_title, "workbox · 2 items");
        assert!(plan
            .prompt
            .starts_with("Today is 2026-10-19. I picked 2 items"));
        assert!(plan.prompt.contains("Id: 1") && plan.prompt.contains("/review-pr"));
        assert!(plan.prompt.contains("2. PR-2 — Review retry"));
    }
}
