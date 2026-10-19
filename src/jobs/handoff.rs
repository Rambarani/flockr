//! Handoff: after a run, a job may ask the user whether to start an agent
//! tab. This module holds the pure parts: deciding whether a finished run
//! earns an offer (`[handoff].when`, or a dynamic `{"flockr_handoff": {...}}`
//! stdout line), rendering the `{{variable}}` templates, and building the
//! shell command that launches the agent. Nothing here touches the clock, the
//! network or the app; the app turns an `Offer` into a persisted decision
//! (`super::decisions`).
//!
//! Templates use `{{name}}`. Names: `stdout`, `stderr_tail`, `exit_code`,
//! `feed_json`, `count`, `date`, `job.id`, `job.title`, `run.id` and
//! `context.<path>` (the dynamic handoff's `context` object). A name that has
//! no value renders as empty text and is reported in `Offer::warnings`.
//! Substitution is a single pass: a value is never expanded again, so job
//! output cannot inject further placeholders.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::definition::{expand_tilde, resolve_path, HandoffDef, JobDef, Prefill};
use super::state::{RunRecord, HANDOFF_MARKER};
use crate::api::schema::JobRunStatus;

/// Characters of `{{stdout}}` kept.
pub(crate) const MAX_STDOUT_CHARS: usize = 12_000;
/// Characters of `{{stderr_tail}}` kept (the end of the stream).
pub(crate) const MAX_STDERR_CHARS: usize = 4_000;
/// Characters of `{{feed_json}}`; items are dropped from the end to fit.
pub(crate) const MAX_FEED_JSON_CHARS: usize = 48_000;
/// Characters of any one `{{context.*}}` value.
pub(crate) const MAX_CONTEXT_VALUE_CHARS: usize = 8_000;
/// Characters of the rendered prompt.
pub(crate) const MAX_PROMPT_CHARS: usize = 200_000;
pub(crate) const MAX_ASK_CHARS: usize = 200;
pub(crate) const MAX_TAB_TITLE_CHARS: usize = 60;
const TRUNCATED_NOTE: &str = "\n… (truncated)";

/// The `{{name}}` variables every handoff template can use. `context.<path>`
/// is also available, from a dynamic handoff's `context` object.
pub(crate) const TEMPLATE_VARIABLES: &[&str] = &[
    "stdout",
    "stderr_tail",
    "exit_code",
    "feed_json",
    "count",
    "date",
    "job.id",
    "job.title",
    "run.id",
];
/// Prefix of the variables that come from a dynamic handoff's `context`.
pub(crate) const CONTEXT_VARIABLE_PREFIX: &str = "context.";

pub(crate) const DEFAULT_AGENT_COMMAND: &str = "claude";
const DEFAULT_ASK: &str = "{{job.title}} has something for you. Start an agent?";
const DEFAULT_TAB_TITLE: &str = "{{job.title}} · {{date}}";

/// A finished run waiting for its feed refresh before the handoff is
/// evaluated, so `{{feed_json}}` and `on_change` see the new items.
#[derive(Debug)]
pub(crate) struct PendingHandoff {
    pub(crate) record: RunRecord,
    pub(crate) def: JobDef,
    pub(crate) previous_fingerprint: Option<String>,
}

/// The feed as the handoff sees it, taken after the run's feed refresh.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FeedSnapshot {
    /// `{"title": ..., "items": [...]}` without action commands.
    pub(crate) json: String,
    pub(crate) unread: u32,
    /// Changes whenever the feed's visible content does.
    pub(crate) fingerprint: String,
}

pub(crate) struct EvalInput<'a> {
    pub(crate) job: &'a JobDef,
    pub(crate) record: &'a RunRecord,
    pub(crate) feed: Option<&'a FeedSnapshot>,
    /// The fingerprint stored after the previous run; `None` for the first.
    pub(crate) previous_fingerprint: Option<&'a str>,
    /// Local date, `YYYY-MM-DD`.
    pub(crate) date: &'a str,
}

/// What the user is asked to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Offer {
    pub(crate) ask: String,
    pub(crate) tab_title: String,
    /// Expanded working directory; checked for existence when started.
    pub(crate) cwd: String,
    /// The agent command with its variables filled in (shell-quoted).
    pub(crate) agent_command: String,
    /// The rendered prompt; empty when the handoff has none.
    pub(crate) prompt: String,
    pub(crate) context: Option<Value>,
    /// Identifies the content, for de-duplication.
    pub(crate) content_hash: String,
    pub(crate) warnings: Vec<String>,
    /// Submit the prompt to the agent (`[handoff].submit`, default true).
    pub(crate) submit: bool,
    pub(crate) prefill: Prefill,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvalResult {
    pub(crate) offer: Option<Offer>,
    /// Fingerprint of this run's output and feed; store it for the next run.
    pub(crate) fingerprint: String,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn sha256_hex(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0u8]);
    }
    hex(&hasher.finalize())
}

/// The fingerprint of a feed document, for `FeedSnapshot::fingerprint`.
pub(crate) fn feed_fingerprint(json: &str) -> String {
    sha256_hex(&[json])
}

/// stdout without the dynamic handoff line, which is plumbing rather than
/// output the prompt should repeat.
fn visible_stdout(stdout: &str) -> String {
    let mut out = String::with_capacity(stdout.len());
    for line in stdout.lines() {
        if line.trim_start().starts_with(HANDOFF_MARKER) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Whether `when` accepts a finished run.
pub(crate) fn when_matches(
    when: &str,
    status: JobRunStatus,
    exit_code: Option<i32>,
    changed: bool,
) -> bool {
    match when {
        "always" => status != JobRunStatus::Skipped,
        "on_success" => status == JobRunStatus::Success,
        "on_failure" => matches!(
            status,
            JobRunStatus::Failed | JobRunStatus::TimedOut | JobRunStatus::SpawnFailed
        ),
        "on_change" => changed,
        other => other
            .strip_prefix("exit:")
            .and_then(|code| code.parse::<i32>().ok())
            .is_some_and(|code| exit_code == Some(code)),
    }
}

pub(super) fn cap_head(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut kept: String = text.chars().take(max_chars).collect();
    kept.push_str(TRUNCATED_NOTE);
    kept
}

fn cap_tail(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let kept: String = text.chars().skip(count - max_chars).collect();
    format!("(truncated) …{kept}")
}

/// One line of plain text: control characters become spaces, then the length
/// is capped. Used for the ask and the tab title, which are shown in the UI.
pub(crate) fn single_line(text: &str, max_chars: usize) -> String {
    let flat: String = text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_chars {
        return flat;
    }
    let mut kept: String = flat.chars().take(max_chars.saturating_sub(1)).collect();
    kept.push('…');
    kept
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RenderMode {
    /// Values go in as they are.
    Text,
    /// Values with shell-significant characters are single-quoted, for text
    /// that becomes part of a shell command line.
    Shell,
}

pub(super) struct Vars {
    scalars: BTreeMap<&'static str, String>,
    /// Variables whose names are only known at run time, such as
    /// `item.field.<label>` of a feed work prompt.
    pub(super) dynamic: BTreeMap<String, String>,
    context: Option<Value>,
}

impl Vars {
    fn lookup(&self, name: &str) -> Option<String> {
        if let Some(path) = name.strip_prefix("context.") {
            let mut current = self.context.as_ref()?;
            for part in path.split('.') {
                current = current.get(part)?;
            }
            return Some(cap_head(&value_text(current), MAX_CONTEXT_VALUE_CHARS));
        }
        self.scalars
            .get(name)
            .or_else(|| self.dynamic.get(name))
            .cloned()
    }

    /// Variables for a template that has no handoff context.
    pub(super) fn new(
        scalars: BTreeMap<&'static str, String>,
        dynamic: BTreeMap<String, String>,
    ) -> Self {
        Self {
            scalars,
            dynamic,
            context: None,
        }
    }
}

fn value_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        other => other.to_string(),
    }
}

fn shell_safe(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(ch, '.' | '_' | '-' | '/' | ':' | '@' | '+' | ',')
        })
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".into();
    }
    if shell_safe(value) {
        return value.to_owned();
    }
    if cfg!(windows) {
        format!("\"{}\"", value.replace('"', "\\\""))
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

pub(super) struct Rendered {
    pub(super) text: String,
    pub(super) missing: Vec<String>,
}

pub(crate) fn valid_placeholder_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'))
}

/// The `{{name}}` placeholders in `template`, in order of appearance and
/// without repeats, found the way `render` finds them.
pub(crate) fn placeholders(template: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        let name = after[..end].trim();
        if valid_placeholder_name(name) && !found.iter().any(|existing| existing == name) {
            found.push(name.to_owned());
        }
        rest = &after[end + 2..];
    }
    found
}

pub(super) fn render(template: &str, vars: &Vars, mode: RenderMode) -> Rendered {
    let mut out = String::with_capacity(template.len());
    let mut missing = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            // No closing braces: keep the text as it is.
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let name = after[..end].trim();
        if valid_placeholder_name(name) {
            match vars.lookup(name) {
                Some(value) => match mode {
                    RenderMode::Text => out.push_str(&value),
                    RenderMode::Shell => out.push_str(&shell_quote(&value)),
                },
                None => {
                    if !missing.iter().any(|existing| existing == name) {
                        missing.push(name.to_owned());
                    }
                    if mode == RenderMode::Shell {
                        out.push_str("''");
                    }
                }
            }
        } else {
            out.push_str(&rest[start..start + 2 + end + 2]);
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Rendered { text: out, missing }
}

/// The offer's fields before templates are rendered: the `[handoff]` table
/// overlaid by the dynamic line's fields.
#[derive(Debug, Default)]
struct Merged {
    ask: Option<String>,
    agent_command: Option<String>,
    cwd: Option<String>,
    tab_title: Option<String>,
    prompt: Option<String>,
    prompt_file: Option<String>,
    context: Option<Value>,
}

fn string_field(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<String>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(other) => Err(format!(
            "dynamic handoff field {key} must be a string, found {}",
            json_type(other)
        )),
    }
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

fn merge(
    table: Option<&HandoffDef>,
    dynamic: Option<&Value>,
    warnings: &mut Vec<String>,
) -> Merged {
    let mut merged = Merged::default();
    if let Some(table) = table {
        merged.ask = table.ask.clone();
        merged.agent_command = table.agent_command.clone();
        merged.cwd = table.cwd.clone();
        merged.tab_title = table.tab_title.clone();
        merged.prompt = table.prompt.clone();
        merged.prompt_file = table.prompt_file.clone();
    }
    let Some(object) = dynamic.and_then(Value::as_object) else {
        if let Some(other) = dynamic {
            warnings.push(format!(
                "the flockr_handoff line must hold an object, found {}",
                json_type(other)
            ));
        }
        return merged;
    };
    let mut take = |key: &str, slot: &mut Option<String>| match string_field(object, key) {
        Ok(Some(value)) => *slot = Some(value),
        Ok(None) => {}
        Err(warning) => warnings.push(warning),
    };
    take("ask", &mut merged.ask);
    take("agent_command", &mut merged.agent_command);
    take("cwd", &mut merged.cwd);
    take("tab_title", &mut merged.tab_title);
    // A dynamic prompt replaces the table's whichever form it used.
    let mut dynamic_prompt = None;
    let mut dynamic_prompt_file = None;
    take("prompt", &mut dynamic_prompt);
    take("prompt_file", &mut dynamic_prompt_file);
    if dynamic_prompt.is_some() || dynamic_prompt_file.is_some() {
        merged.prompt = dynamic_prompt;
        merged.prompt_file = dynamic_prompt_file;
    }
    match object.get("context") {
        None | Some(Value::Null) => {}
        Some(context @ Value::Object(_)) => merged.context = Some(context.clone()),
        Some(other) => warnings.push(format!(
            "dynamic handoff field context must be an object, found {}",
            json_type(other)
        )),
    }
    merged
}

/// Decide whether `input` earns a handoff offer and, when it does, render it.
///
/// A dynamic `flockr_handoff` line always offers (the job decided so); the
/// `[handoff]` table offers when `when` matches. `Err` means the run earned an
/// offer that could not be built (an unreadable prompt file, a command that
/// needs a prompt the handoff does not have).
pub(crate) fn evaluate(input: &EvalInput<'_>) -> Result<EvalResult, String> {
    let stdout = visible_stdout(&input.record.stdout_tail);
    let feed_fingerprint = input.feed.map_or("", |feed| feed.fingerprint.as_str());
    let fingerprint = sha256_hex(&[stdout.trim(), feed_fingerprint]);
    let changed = input.previous_fingerprint != Some(fingerprint.as_str());

    let table = input.job.handoff.as_ref();
    let dynamic = input.record.handoff.as_ref();
    let wanted = dynamic.is_some()
        || table.is_some_and(|table| {
            when_matches(
                &table.when,
                input.record.info.status,
                input.record.info.exit_code,
                changed,
            )
        });
    if !wanted {
        return Ok(EvalResult {
            offer: None,
            fingerprint,
        });
    }

    let mut warnings = Vec::new();
    let merged = merge(table, dynamic, &mut warnings);

    let prompt_template = match (&merged.prompt, &merged.prompt_file) {
        (Some(inline), _) => Some(inline.clone()),
        (None, Some(file)) => {
            let path = resolve_path(&input.job.dir, file);
            Some(std::fs::read_to_string(&path).map_err(|err| {
                format!("could not read the prompt file {}: {err}", path.display())
            })?)
        }
        (None, None) => None,
    };

    let count = merged
        .context
        .as_ref()
        .and_then(|context| context.get("count"))
        .map(value_text)
        .unwrap_or_else(|| input.feed.map_or(0, |feed| feed.unread).to_string());
    let mut scalars: BTreeMap<&'static str, String> = BTreeMap::new();
    scalars.insert("stdout", cap_head(&stdout, MAX_STDOUT_CHARS));
    scalars.insert(
        "stderr_tail",
        cap_tail(&input.record.stderr_tail, MAX_STDERR_CHARS),
    );
    scalars.insert(
        "exit_code",
        input
            .record
            .info
            .exit_code
            .map(|code| code.to_string())
            .unwrap_or_default(),
    );
    scalars.insert(
        "feed_json",
        input
            .feed
            .map_or_else(|| "{\"items\":[]}".to_owned(), |feed| feed.json.clone()),
    );
    scalars.insert("count", count);
    scalars.insert("date", input.date.to_owned());
    scalars.insert("job.id", input.job.id.clone());
    scalars.insert("job.title", input.job.title.clone());
    scalars.insert("run.id", input.record.info.run_id.to_string());
    let vars = Vars {
        scalars,
        dynamic: BTreeMap::new(),
        context: merged.context.clone(),
    };

    let mut missing = Vec::new();
    let mut take = |rendered: Rendered| {
        for name in rendered.missing {
            if !missing.contains(&name) {
                missing.push(name);
            }
        }
        rendered.text
    };
    let ask = single_line(
        &take(render(
            merged.ask.as_deref().unwrap_or(DEFAULT_ASK),
            &vars,
            RenderMode::Text,
        )),
        MAX_ASK_CHARS,
    );
    let tab_title = single_line(
        &take(render(
            merged.tab_title.as_deref().unwrap_or(DEFAULT_TAB_TITLE),
            &vars,
            RenderMode::Text,
        )),
        MAX_TAB_TITLE_CHARS,
    );
    let agent_command = take(render(
        merged
            .agent_command
            .as_deref()
            .unwrap_or(DEFAULT_AGENT_COMMAND),
        &vars,
        RenderMode::Shell,
    ))
    .trim()
    .to_owned();
    let cwd_text = take(render(
        merged.cwd.as_deref().unwrap_or(""),
        &vars,
        RenderMode::Text,
    ));
    let cwd = if cwd_text.trim().is_empty() {
        input.job.cwd.clone()
    } else {
        resolve_path(&input.job.dir, cwd_text.trim())
    };
    let prompt = match &prompt_template {
        Some(template) => cap_head(
            &take(render(template, &vars, RenderMode::Text)),
            MAX_PROMPT_CHARS,
        ),
        None => String::new(),
    };
    if agent_command.is_empty() {
        return Err("the handoff agent_command is empty".into());
    }
    if prompt.trim().is_empty() && agent_command.contains("{prompt_file}") {
        return Err(
            "agent_command uses {prompt_file} but the handoff has no prompt (set prompt or prompt_file)"
                .into(),
        );
    }
    for name in missing {
        warnings.push(format!("template variable {{{{{name}}}}} has no value"));
    }
    let cwd = cwd.display().to_string();
    let content_hash = sha256_hex(&[
        &input.job.id,
        &ask,
        &agent_command,
        &cwd,
        &tab_title,
        &prompt,
    ]);
    Ok(EvalResult {
        offer: Some(Offer {
            ask,
            tab_title,
            cwd,
            agent_command,
            prompt: prompt.trim().is_empty().then(String::new).unwrap_or(prompt),
            context: merged.context,
            content_hash,
            warnings,
            submit: table.and_then(|table| table.submit).unwrap_or(true),
            prefill: table.and_then(|table| table.prefill).unwrap_or_default(),
        }),
        fingerprint,
    })
}

/// The shell command that starts the agent. With a prompt, a `{prompt_file}`
/// placeholder expands to the quoted prompt path; without the placeholder the
/// prompt text goes in as the first argument, `claude "$(cat <file>)"`. With
/// no prompt the command runs exactly as written.
pub(crate) fn launch_command(agent_command: &str, prompt: Option<&Path>) -> Result<String, String> {
    let agent_command = agent_command.trim();
    if agent_command.is_empty() {
        return Err("the agent command is empty".into());
    }
    let Some(prompt) = prompt else {
        if agent_command.contains("{prompt_file}") {
            return Err("the agent command uses {prompt_file} but there is no prompt".into());
        }
        return Ok(agent_command.to_owned());
    };
    let quoted = crate::upstream::sync::shell_quote_path(prompt);
    if agent_command.contains("{prompt_file}") {
        return Ok(agent_command.replace("{prompt_file}", &quoted));
    }
    if cfg!(windows) {
        return Err("on Windows the agent command must contain a {prompt_file} placeholder".into());
    }
    Ok(format!("{agent_command} \"$(cat {quoted})\""))
}

/// How an agent tab starts: the shell command to type, and, when the prompt
/// is to be left as a draft, how to put it in the input box once the agent is
/// ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentLaunch {
    pub(crate) command: String,
    /// `Some` when the prompt is not on the command line and must be typed
    /// into the agent's input box without Enter.
    pub(crate) draft: Option<Prefill>,
}

/// Decide how the agent starts. `submit = true` is `launch_command` as it
/// always was. With `submit = false` the agent starts with no prompt argument
/// and `draft` says how the prompt follows. A command that contains
/// `{prompt_file}` keeps its explicit file mechanism whatever `submit` says:
/// the user chose how the agent receives the file, and flockr cannot know
/// whether that command submits.
pub(crate) fn agent_launch(
    agent_command: &str,
    prompt: Option<&Path>,
    submit: bool,
    prefill: Prefill,
) -> Result<AgentLaunch, String> {
    let draft = !submit && prompt.is_some() && !agent_command.contains("{prompt_file}");
    if !draft {
        return Ok(AgentLaunch {
            command: launch_command(agent_command, prompt)?,
            draft: None,
        });
    }
    Ok(AgentLaunch {
        command: launch_command(agent_command, None)?,
        draft: Some(prefill),
    })
}

/// One line for a preview: whether the prompt is submitted or left as a draft.
pub(crate) fn submit_note(agent_command: &str, submit: bool, prefill: Prefill) -> String {
    if submit {
        return "submit     yes: the prompt is passed to the agent, which runs it at once".into();
    }
    if agent_command.contains("{prompt_file}") {
        return "submit     no, but the command passes {prompt_file} itself, so the agent gets the file as that command does".into();
    }
    let typed = match prefill {
        Prefill::Paste => "the prompt, pasted",
        Prefill::Mention => "an @mention of the prompt file",
    };
    format!(
        "submit     no: the agent starts with no prompt and {typed} into its input box (prefill = \"{}\"), without Enter",
        prefill.as_str()
    )
}

/// The text a `mention` draft types: `@<path> ` (the trailing space ends the
/// mention). A path with whitespace is quoted, `@"<path>" `.
pub(crate) fn mention_text(prompt: &Path) -> String {
    let path = prompt.display().to_string();
    if path.chars().any(char::is_whitespace) {
        format!("@\"{path}\" ")
    } else {
        format!("@{path} ")
    }
}

/// The program an agent command runs, skipping leading `NAME=value`
/// assignments: `claude` for `FOO=1 /usr/bin/claude --x`.
pub(crate) fn agent_program(agent_command: &str) -> Option<&str> {
    agent_command
        .split_whitespace()
        .find(|word| !word.contains('=') || word.starts_with('/'))
        .and_then(|program| program.rsplit('/').next())
        .filter(|program| !program.is_empty())
}

/// Where the rendered prompt of `run_id` is written.
pub(crate) fn prompt_path(state_dir: &Path, run_id: u64) -> PathBuf {
    state_dir.join("handoffs").join(format!("{run_id}.md"))
}

/// Rendered prompts kept per job; matches the decision list cap.
pub(crate) const PROMPT_FILES_KEPT: usize = 50;

/// Write `prompt` for `run_id` and drop the oldest files beyond the limit.
pub(crate) fn write_prompt(
    state_dir: &Path,
    run_id: u64,
    prompt: &str,
) -> std::io::Result<PathBuf> {
    let path = prompt_path(state_dir, run_id);
    let Some(dir) = path.parent() else {
        return Err(std::io::Error::other("prompt path has no parent"));
    };
    write_numbered_prompt(dir, run_id, prompt)
}

/// Write `<dir>/<number>.md` and drop the oldest numbered files beyond the
/// limit. Shared by handoff prompts (numbered by run) and feed work prompts
/// (numbered by time).
pub(crate) fn write_numbered_prompt(
    dir: &Path,
    number: u64,
    prompt: &str,
) -> std::io::Result<PathBuf> {
    let path = dir.join(format!("{number}.md"));
    std::fs::create_dir_all(dir)?;
    std::fs::write(&path, prompt)?;
    prune_prompts(dir, PROMPT_FILES_KEPT);
    Ok(path)
}

fn prune_prompts(dir: &Path, keep: usize) {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return;
    };
    let mut runs: Vec<(u64, PathBuf)> = read_dir
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let run = path.file_stem()?.to_str()?.parse::<u64>().ok()?;
            (path.extension()?.to_str()? == "md").then_some((run, path))
        })
        .collect();
    if runs.len() <= keep {
        return;
    }
    runs.sort();
    for (_, path) in runs.iter().take(runs.len() - keep) {
        let _ = std::fs::remove_file(path);
    }
}

/// Local date `YYYY-MM-DD` of a Unix millisecond timestamp.
pub(crate) fn local_date(now_ms: u64) -> String {
    let secs = i64::try_from(now_ms / 1000).unwrap_or(0);
    if let Some((year, month, day, ..)) = crate::platform::local_civil_at(secs) {
        return format!("{year:04}-{month:02}-{day:02}");
    }
    let (year, month, day) = super::cron::civil_from_days(secs.div_euclid(86_400));
    format!("{year:04}-{month:02}-{day:02}")
}

/// A feed document for `{{feed_json}}`: title and items, without action
/// commands, cut by dropping items from the end until it fits.
pub(crate) fn feed_json(title: &str, items: &[crate::api::schema::FeedItemInfo]) -> String {
    let document = |count: usize, truncated: bool| {
        let mut value = serde_json::json!({
            "title": title,
            "total": items.len(),
            "items": &items[..count],
        });
        if truncated {
            value["truncated"] = Value::Bool(true);
        }
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{\"items\":[]}".to_owned())
    };
    let full = document(items.len(), false);
    if full.chars().count() <= MAX_FEED_JSON_CHARS {
        return full;
    }
    let (mut low, mut high) = (0usize, items.len());
    while low < high {
        let mid = (low + high).div_ceil(2);
        if document(mid, true).chars().count() <= MAX_FEED_JSON_CHARS {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    document(low, true)
}

/// Expand `~` in a decision's working directory.
pub(crate) fn expand_cwd(cwd: &str) -> PathBuf {
    expand_tilde(cwd.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{JobRunInfo, JobRunTrigger};
    use crate::jobs::definition::parse_job_toml;

    fn job(extra: &str) -> JobDef {
        parse_job_toml(
            &format!("command = [\"x\"]\nevery = \"1h\"\ntitle = \"Sync\"\n{extra}"),
            "sync",
            Path::new("/jobs/sync"),
        )
        .unwrap()
    }

    fn record(status: JobRunStatus, exit: Option<i32>, stdout: &str) -> RunRecord {
        RunRecord {
            info: JobRunInfo {
                run_id: 4,
                trigger: JobRunTrigger::Schedule,
                status,
                started_unix_ms: 0,
                finished_unix_ms: 0,
                duration_ms: 0,
                exit_code: exit,
                summary: String::new(),
                error: None,
                skipped_count: 0,
                env_note: None,
            },
            stdout_tail: stdout.into(),
            stderr_tail: "boom".into(),
            stdout_truncated: false,
            stderr_truncated: false,
            handoff: crate::jobs::state::find_handoff_line(stdout),
        }
    }

    fn eval(
        job: &JobDef,
        record: &RunRecord,
        feed: Option<&FeedSnapshot>,
        previous: Option<&str>,
    ) -> Result<EvalResult, String> {
        evaluate(&EvalInput {
            job,
            record,
            feed,
            previous_fingerprint: previous,
            date: "2026-10-18",
        })
    }

    #[test]
    fn when_covers_every_form() {
        use JobRunStatus::*;
        assert!(when_matches("on_success", Success, Some(0), false));
        assert!(!when_matches("on_success", Failed, Some(1), true));
        assert!(when_matches("on_failure", Failed, Some(1), false));
        assert!(when_matches("on_failure", TimedOut, None, false));
        assert!(when_matches("on_failure", SpawnFailed, None, false));
        assert!(!when_matches("on_failure", Success, Some(0), true));
        assert!(when_matches("always", Failed, Some(1), false));
        assert!(!when_matches("always", Skipped, None, false));
        assert!(when_matches("on_change", Success, Some(0), true));
        assert!(!when_matches("on_change", Success, Some(0), false));
        assert!(when_matches("exit:3", Failed, Some(3), false));
        assert!(!when_matches("exit:3", Failed, Some(2), false));
        assert!(!when_matches("exit:3", TimedOut, None, false));
        assert!(when_matches("exit:0", Success, Some(0), false));
    }

    #[test]
    fn on_change_compares_stdout_with_the_previous_run() {
        let job = job("[handoff]\nwhen = \"on_change\"\nask = \"changed\"\n");
        let first = eval(
            &job,
            &record(JobRunStatus::Success, Some(0), "a\n"),
            None,
            None,
        )
        .unwrap();
        assert!(first.offer.is_some(), "the first run counts as a change");
        let same = eval(
            &job,
            &record(JobRunStatus::Success, Some(0), "a\n"),
            None,
            Some(&first.fingerprint),
        )
        .unwrap();
        assert!(same.offer.is_none());
        assert_eq!(same.fingerprint, first.fingerprint);
        let different = eval(
            &job,
            &record(JobRunStatus::Success, Some(0), "b\n"),
            None,
            Some(&first.fingerprint),
        )
        .unwrap();
        assert!(different.offer.is_some());
        assert_ne!(different.fingerprint, first.fingerprint);
    }

    #[test]
    fn on_change_also_sees_the_feed_changing() {
        let job = job("[handoff]\nwhen = \"on_change\"\n");
        let record = record(JobRunStatus::Success, Some(0), "same\n");
        let feed_a = FeedSnapshot {
            json: "{}".into(),
            unread: 1,
            fingerprint: feed_fingerprint("a"),
        };
        let feed_b = FeedSnapshot {
            fingerprint: feed_fingerprint("b"),
            ..feed_a.clone()
        };
        let first = eval(&job, &record, Some(&feed_a), None).unwrap();
        let unchanged = eval(&job, &record, Some(&feed_a), Some(&first.fingerprint)).unwrap();
        assert!(unchanged.offer.is_none());
        let changed = eval(&job, &record, Some(&feed_b), Some(&first.fingerprint)).unwrap();
        assert!(
            changed.offer.is_some(),
            "stdout is the same but the feed moved"
        );
    }

    #[test]
    fn a_job_without_handoff_or_dynamic_line_offers_nothing() {
        let plain = job("");
        let result = eval(
            &plain,
            &record(JobRunStatus::Success, Some(0), "x"),
            None,
            None,
        )
        .unwrap();
        assert!(result.offer.is_none());
    }

    #[test]
    fn the_dynamic_line_offers_and_overrides_table_fields() {
        let job = job("[handoff]\nwhen = \"on_failure\"\nask = \"table ask\"\ncwd = \"/tmp\"\ntab_title = \"table tab\"\nagent_command = \"claude\"\n");
        let stdout = "noise\n{\"flockr_handoff\":{\"ask\":\"{{context.tag}} is out\",\"agent_command\":\"flockr upstream sync {{context.tag}}\",\"context\":{\"tag\":\"v0.9.7\",\"count\":3}}}\n";
        let result = eval(
            &job,
            &record(JobRunStatus::Success, Some(0), stdout),
            None,
            None,
        )
        .unwrap()
        .offer
        .expect("a dynamic line always offers");
        assert_eq!(result.ask, "v0.9.7 is out");
        assert_eq!(result.agent_command, "flockr upstream sync v0.9.7");
        assert_eq!(
            result.tab_title, "table tab",
            "unset dynamic fields keep the table's"
        );
        assert_eq!(result.cwd, "/tmp");
        assert_eq!(result.context.unwrap()["count"], 3);
        assert!(result.prompt.is_empty());
    }

    #[test]
    fn a_dynamic_line_without_a_table_uses_defaults() {
        let job = job("");
        let stdout = "{\"flockr_handoff\":{\"context\":{\"x\":1}}}\n";
        let offer = eval(
            &job,
            &record(JobRunStatus::Success, Some(0), stdout),
            None,
            None,
        )
        .unwrap()
        .offer
        .unwrap();
        assert_eq!(offer.agent_command, "claude");
        assert_eq!(offer.ask, "Sync has something for you. Start an agent?");
        assert_eq!(offer.tab_title, "Sync · 2026-10-18");
        assert_eq!(offer.cwd, "/jobs/sync");
    }

    #[test]
    fn bad_dynamic_fields_are_ignored_with_warnings() {
        let job = job("");
        let stdout = "{\"flockr_handoff\":{\"ask\":5,\"context\":[1]}}\n";
        let offer = eval(
            &job,
            &record(JobRunStatus::Success, Some(0), stdout),
            None,
            None,
        )
        .unwrap()
        .offer
        .unwrap();
        assert_eq!(offer.warnings.len(), 2, "{:?}", offer.warnings);
        assert!(offer.warnings[0].contains("ask must be a string"));
        assert!(offer.warnings[1].contains("context must be an object"));
    }

    #[test]
    fn templates_fill_every_variable() {
        let job = job("[handoff]\nwhen = \"always\"\nask = \"{{count}} for {{job.title}}\"\nprompt = \"id={{job.id}} run={{run.id}} exit={{exit_code}} date={{date}}\\nout={{stdout}}\\nerr={{ stderr_tail }}\\nfeed={{feed_json}}\\nctx={{context.a.b}}\"\n");
        let feed = FeedSnapshot {
            json: "{\"items\":[1]}".into(),
            unread: 7,
            fingerprint: "f".into(),
        };
        let stdout = "hello\n{\"flockr_handoff\":{\"context\":{\"a\":{\"b\":\"deep\"}}}}\n";
        let offer = eval(
            &job,
            &record(JobRunStatus::Failed, Some(2), stdout),
            Some(&feed),
            None,
        )
        .unwrap()
        .offer
        .unwrap();
        assert_eq!(
            offer.ask, "7 for Sync",
            "count falls back to the unread count"
        );
        assert_eq!(
            offer.prompt,
            "id=sync run=4 exit=2 date=2026-10-18\nout=hello\n\nerr=boom\nfeed={\"items\":[1]}\nctx=deep"
        );
        assert!(
            !offer.prompt.contains("flockr_handoff"),
            "the plumbing line is not output"
        );
        assert!(offer.warnings.is_empty(), "{:?}", offer.warnings);
    }

    #[test]
    fn count_prefers_the_context_value() {
        let job = job("[handoff]\nwhen = \"always\"\nask = \"{{count}}\"\n");
        let feed = FeedSnapshot {
            unread: 7,
            ..FeedSnapshot::default()
        };
        let stdout = "{\"flockr_handoff\":{\"context\":{\"count\":3}}}\n";
        let offer = eval(
            &job,
            &record(JobRunStatus::Success, Some(0), stdout),
            Some(&feed),
            None,
        )
        .unwrap()
        .offer
        .unwrap();
        assert_eq!(offer.ask, "3");
    }

    #[test]
    fn missing_variables_render_empty_and_are_reported() {
        let job = job("[handoff]\nwhen = \"always\"\nprompt = \"a={{context.nope}} b={{bogus}} c={{context.nope}}\"\n");
        let offer = eval(
            &job,
            &record(JobRunStatus::Success, Some(0), ""),
            None,
            None,
        )
        .unwrap()
        .offer
        .unwrap();
        assert_eq!(offer.prompt, "a= b= c=");
        assert_eq!(
            offer.warnings.len(),
            2,
            "each name is reported once: {:?}",
            offer.warnings
        );
    }

    #[test]
    fn substitution_is_one_pass_and_odd_braces_stay_literal() {
        let vars = Vars {
            scalars: BTreeMap::from([("stdout", "{{job.id}}".to_owned())]),
            dynamic: BTreeMap::new(),
            context: None,
        };
        let out = render(
            "{{stdout}} {{ not valid }} {{unclosed",
            &vars,
            RenderMode::Text,
        );
        assert_eq!(out.text, "{{job.id}} {{ not valid }} {{unclosed");
        assert!(out.missing.is_empty());
    }

    #[test]
    fn long_values_are_capped() {
        let big = "x".repeat(MAX_STDOUT_CHARS + 500);
        let job = job("[handoff]\nwhen = \"always\"\nprompt = \"{{stdout}}|{{stderr_tail}}|{{context.big}}\"\n");
        let mut rec = record(JobRunStatus::Success, Some(0), &big);
        rec.stderr_tail = "e".repeat(MAX_STDERR_CHARS + 100);
        rec.handoff =
            Some(serde_json::json!({"context": {"big": "y".repeat(MAX_CONTEXT_VALUE_CHARS + 10)}}));
        let prompt = eval(&job, &rec, None, None).unwrap().offer.unwrap().prompt;
        let parts: Vec<&str> = prompt.split('|').collect();
        assert!(parts[0].ends_with("(truncated)"));
        assert!(
            parts[0].chars().count() <= MAX_STDOUT_CHARS + 20,
            "{}",
            parts[0].len()
        );
        assert!(parts[1].starts_with("(truncated) …"));
        assert!(parts[1].matches('e').count() <= MAX_STDERR_CHARS + 20);
        assert!(parts[2].ends_with("(truncated)"));
        assert!(parts[2].matches('y').count() <= MAX_CONTEXT_VALUE_CHARS);
    }

    #[test]
    fn ask_and_title_are_flattened_to_one_short_line() {
        let job =
            job("[handoff]\nwhen = \"always\"\nask = \"{{stdout}}\"\ntab_title = \"{{stdout}}\"\n");
        let stdout = format!("line one\n\u{1b}[31mline two\n{}", "z".repeat(500));
        let offer = eval(
            &job,
            &record(JobRunStatus::Success, Some(0), &stdout),
            None,
            None,
        )
        .unwrap()
        .offer
        .unwrap();
        assert!(!offer.ask.contains('\n') && !offer.ask.contains('\u{1b}'));
        assert!(offer.ask.chars().count() <= MAX_ASK_CHARS);
        assert!(offer.tab_title.chars().count() <= MAX_TAB_TITLE_CHARS);
    }

    #[test]
    fn command_values_are_shell_quoted() {
        let job = job("[handoff]\nwhen = \"always\"\nagent_command = \"run {{context.tag}} {{context.evil}} {{context.empty}}\"\n");
        let mut rec = record(JobRunStatus::Success, Some(0), "");
        rec.handoff = Some(
            serde_json::json!({"context": {"tag": "v0.9.7", "evil": "x; rm -rf ~ 'q'", "empty": ""}}),
        );
        let offer = eval(&job, &rec, None, None).unwrap().offer.unwrap();
        #[cfg(unix)]
        assert_eq!(
            offer.agent_command,
            "run v0.9.7 'x; rm -rf ~ '\\''q'\\''' ''"
        );
        assert!(!offer.agent_command.contains("\n"));
    }

    #[test]
    fn prompt_file_is_read_from_the_job_directory() {
        let dir = std::env::temp_dir().join(format!("flockr-handoff-pf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("prompt.md"),
            "Triage {{count}} items for {{job.id}}",
        )
        .unwrap();
        let job = parse_job_toml(
            "command = [\"x\"]\nevery = \"1h\"\n[handoff]\nwhen = \"always\"\nprompt_file = \"prompt.md\"\n",
            "sync",
            &dir,
        )
        .unwrap();
        let offer = eval(
            &job,
            &record(JobRunStatus::Success, Some(0), ""),
            None,
            None,
        )
        .unwrap()
        .offer
        .unwrap();
        assert_eq!(offer.prompt, "Triage 0 items for sync");

        std::fs::remove_file(dir.join("prompt.md")).unwrap();
        let err = eval(
            &job,
            &record(JobRunStatus::Success, Some(0), ""),
            None,
            None,
        )
        .unwrap_err();
        assert!(err.contains("could not read the prompt file"), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_prompt_placeholder_without_a_prompt_is_refused() {
        let job = job("[handoff]\nwhen = \"always\"\nagent_command = \"cat {prompt_file}\"\n");
        let err = eval(
            &job,
            &record(JobRunStatus::Success, Some(0), ""),
            None,
            None,
        )
        .unwrap_err();
        assert!(err.contains("has no prompt"), "{err}");
    }

    #[test]
    fn same_content_hashes_the_same_and_different_content_does_not() {
        let job = job("[handoff]\nwhen = \"always\"\nprompt = \"{{stdout}}\"\n");
        let hash = |out: &str| {
            eval(
                &job,
                &record(JobRunStatus::Success, Some(0), out),
                None,
                None,
            )
            .unwrap()
            .offer
            .unwrap()
            .content_hash
        };
        assert_eq!(hash("a"), hash("a"));
        assert_ne!(hash("a"), hash("b"));
    }

    #[cfg(unix)]
    #[test]
    fn launch_command_uses_the_placeholder_or_appends_the_prompt() {
        let file = Path::new("/tmp/a b/1.md");
        assert_eq!(
            launch_command("claude", Some(file)).unwrap(),
            "claude \"$(cat '/tmp/a b/1.md')\""
        );
        assert_eq!(
            launch_command("sh -c 'cat {prompt_file}; sleep 30'", Some(file)).unwrap(),
            "sh -c 'cat '/tmp/a b/1.md'; sleep 30'"
        );
        assert_eq!(
            launch_command("flockr upstream sync v0.9.7", None).unwrap(),
            "flockr upstream sync v0.9.7",
            "no prompt: the command runs as written"
        );
        assert!(launch_command("cat {prompt_file}", None).is_err());
        assert!(launch_command("  ", None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn agent_launch_submits_by_default_and_drafts_without_a_prompt_argument() {
        let file = Path::new("/tmp/w/1.md");
        // submit = true is launch_command, exactly as before.
        let submitted = agent_launch("claude", Some(file), true, Prefill::Paste).unwrap();
        assert_eq!(submitted.command, "claude \"$(cat '/tmp/w/1.md')\"");
        assert_eq!(submitted.draft, None);
        // submit = false: the agent alone, the prompt follows as a draft.
        let draft = agent_launch("claude --model x", Some(file), false, Prefill::Mention).unwrap();
        assert_eq!(draft.command, "claude --model x");
        assert_eq!(draft.draft, Some(Prefill::Mention));
        let draft = agent_launch("claude", Some(file), false, Prefill::Paste).unwrap();
        assert_eq!(draft.draft, Some(Prefill::Paste));
        // An explicit {prompt_file} keeps its mechanism whatever submit says.
        let explicit =
            agent_launch("agent --f {prompt_file}", Some(file), false, Prefill::Paste).unwrap();
        assert_eq!(explicit.command, "agent --f '/tmp/w/1.md'");
        assert_eq!(explicit.draft, None);
        // No prompt, nothing to draft.
        let none = agent_launch("flockr upstream sync v1", None, false, Prefill::Paste).unwrap();
        assert_eq!(
            (none.command.as_str(), none.draft),
            ("flockr upstream sync v1", None)
        );
        assert!(agent_launch("  ", Some(file), false, Prefill::Paste).is_err());
    }

    #[test]
    fn mention_text_ends_with_a_space_and_quotes_paths_with_whitespace() {
        assert_eq!(mention_text(Path::new("/s/work/1.md")), "@/s/work/1.md ");
        assert_eq!(
            mention_text(Path::new("/s/my work/1.md")),
            "@\"/s/my work/1.md\" "
        );
    }

    #[test]
    fn the_agent_program_skips_assignments_and_directories() {
        assert_eq!(agent_program("claude --x"), Some("claude"));
        assert_eq!(
            agent_program("FOO=1 BAR=/x /usr/local/bin/claude -p"),
            Some("claude")
        );
        assert_eq!(agent_program("  "), None);
    }

    #[test]
    fn a_handoff_submits_by_default_and_honours_submit_and_prefill() {
        let run = |extra: &str| {
            let job = job(&format!(
                "[handoff]\nwhen = \"always\"\nprompt = \"p\"\n{extra}"
            ));
            eval(
                &job,
                &record(JobRunStatus::Success, Some(0), "x"),
                None,
                None,
            )
            .unwrap()
            .offer
            .unwrap()
        };
        let default = run("");
        assert!(default.submit, "a handoff still runs its prompt by default");
        assert_eq!(default.prefill, Prefill::Paste);
        let draft = run("submit = false\nprefill = \"mention\"\n");
        assert!(!draft.submit);
        assert_eq!(draft.prefill, Prefill::Mention);
        // A dynamic handoff with no table also submits.
        let dynamic = job("");
        let stdout = "{\"flockr_handoff\":{\"ask\":\"a\",\"prompt\":\"p\"}}\n";
        let offer = eval(
            &dynamic,
            &record(JobRunStatus::Success, Some(0), stdout),
            None,
            None,
        )
        .unwrap()
        .offer
        .unwrap();
        assert!(offer.submit);
    }

    #[test]
    fn prompt_files_are_written_and_the_oldest_pruned() {
        let dir = std::env::temp_dir().join(format!("flockr-handoff-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for run in 1..=(PROMPT_FILES_KEPT as u64 + 5) {
            let path = write_prompt(&dir, run, &format!("run {run}")).unwrap();
            assert_eq!(path, prompt_path(&dir, run));
        }
        let kept = std::fs::read_dir(dir.join("handoffs")).unwrap().count();
        assert_eq!(kept, PROMPT_FILES_KEPT);
        assert!(!prompt_path(&dir, 1).exists());
        assert_eq!(
            std::fs::read_to_string(prompt_path(&dir, PROMPT_FILES_KEPT as u64 + 5)).unwrap(),
            format!("run {}", PROMPT_FILES_KEPT + 5)
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn feed_json_drops_items_from_the_end_to_fit() {
        use crate::api::schema::{FeedItemInfo, FeedItemState};
        let item = |n: usize| FeedItemInfo {
            id: format!("i{n}"),
            title: "t".repeat(400),
            subtitle: None,
            state: FeedItemState::Unread,
            badge: None,
            url: None,
            updated_at: None,
            body: None,
            fields: Vec::new(),
            group: None,
            actions: Vec::new(),
        };
        let few: Vec<_> = (0..3).map(item).collect();
        let text = feed_json("Today", &few);
        assert!(serde_json::from_str::<Value>(&text).is_ok());
        assert!(!text.contains("truncated"));
        let many: Vec<_> = (0..500).map(item).collect();
        let text = feed_json("Today", &many);
        assert!(text.chars().count() <= MAX_FEED_JSON_CHARS);
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["truncated"], true);
        assert_eq!(value["total"], 500);
        assert!(!value["items"].as_array().unwrap().is_empty());
    }

    #[test]
    fn local_date_is_a_calendar_date() {
        let date = local_date(1_792_000_000_000);
        assert_eq!(date.len(), 10);
        assert!(date.starts_with("2026-10-"), "{date}");
    }

    #[test]
    fn the_workbox_example_parses_and_renders() {
        // The example is tracked, so a missing file is a failure, not a skip.
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/jobs/workbox-sync");
        let text = std::fs::read_to_string(dir.join("job.toml"))
            .expect("examples/jobs/workbox-sync/job.toml is tracked");
        let job = parse_job_toml(&text, "workbox-sync", &dir).expect("the example job is valid");
        let handoff = job.handoff.as_ref().expect("the example has a [handoff]");
        assert_eq!(handoff.when, "on_change");
        assert_eq!(handoff.prompt_file.as_deref(), Some("prompt.md"));

        let feed = FeedSnapshot {
            json: "{\"items\":[{\"id\":\"jira:NH-1\",\"title\":\"Fix login\"}]}".into(),
            unread: 3,
            fingerprint: feed_fingerprint("x"),
        };
        let offer = eval(
            &job,
            &record(JobRunStatus::Success, Some(0), "synced 12 items\n"),
            Some(&feed),
            None,
        )
        .unwrap()
        .offer
        .expect("a first sync with unread items asks");
        assert_eq!(offer.ask, "3 items need you. Start Claude?");
        assert_eq!(offer.tab_title, "workbox · 2026-10-18");
        assert_eq!(offer.agent_command, "claude");
        assert!(offer.cwd.ends_with("Desktop/poc/workbox"), "{}", offer.cwd);
        assert!(offer.prompt.contains("Today is 2026-10-18."));
        assert!(
            offer.prompt.contains("jira:NH-1"),
            "feed json is in the prompt"
        );
        assert!(offer.prompt.contains("/plan-day"));
        assert!(offer.prompt.contains("synced 12 items"));
        assert!(offer.warnings.is_empty(), "{:?}", offer.warnings);
    }
}
