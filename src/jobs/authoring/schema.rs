//! JSON Schemas for `job.toml` (with `[handoff]`, `[env]`, the feed table and
//! the `[jobs.system.*]` config section) and for the feed format. They are
//! built here from the same constants the parsers use, committed under
//! `docs/next/api/`, and checked by tests, so an editor, a validator or an
//! agent can rely on them. Regenerate the files with
//! `FLOCKR_UPDATE_JOB_SCHEMA=1 just test-one job_schema_artifacts_are_current`.

use serde_json::{json, Value};

use crate::feed::format::{
    FEED_VERSION, GROUPED_RESERVED_CHARS, MAX_ACTIONS, MAX_ACTION_TITLE_CHARS, MAX_ARGV,
    MAX_ARG_CHARS, MAX_BADGE_CHARS, MAX_BODY_BYTES, MAX_FEED_BYTES, MAX_FEED_TITLE_CHARS,
    MAX_FIELDS, MAX_FIELD_LABEL_CHARS, MAX_FIELD_VALUE_CHARS, MAX_GROUPS, MAX_GROUP_ID_CHARS,
    MAX_GROUP_SHORT_TITLE_CHARS, MAX_GROUP_TITLE_CHARS, MAX_ID_CHARS, MAX_ITEMS,
    MAX_SUBTITLE_CHARS, MAX_TITLE_CHARS, MAX_UPDATED_AT_CHARS, MAX_URL_CHARS, OTHER_GROUP_ID,
    RESERVED_CHARS, RESERVED_NAMES,
};
use crate::jobs::definition::{
    format_duration, DEFAULT_TIMEOUT, JOB_ID_MAX_CHARS, MAX_TIMEOUT, MIN_INTERVAL, MIN_TIMEOUT,
};
use crate::jobs::feed_work::{
    MAX_ITEMS_TEXT_CHARS, MAX_ITEM_BODY_CHARS, MAX_WORK_ITEMS, WORK_TEMPLATE_VARIABLES,
};
use crate::jobs::handoff::{
    MAX_ASK_CHARS, MAX_CONTEXT_VALUE_CHARS, MAX_FEED_JSON_CHARS, MAX_PROMPT_CHARS,
    MAX_STDERR_CHARS, MAX_STDOUT_CHARS, MAX_TAB_TITLE_CHARS, TEMPLATE_VARIABLES,
};

const DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";
/// `30s`, `5m`, `1h30m`, `2d`.
const DURATION_PATTERN: &str = "^([0-9]+[smhd])+$";

pub(crate) const NOTIFY_VALUES: &[&str] = &["never", "on_failure", "on_change", "always"];
pub(crate) const ENV_MODES: &[&str] = &["login-shell", "server"];
pub(crate) const HANDOFF_WHEN: &[&str] = &["on_success", "on_change", "on_failure", "always"];
pub(crate) const FEED_STATES: &[&str] = &["unread", "read", "done", "warn", "error"];

fn argv(description: &str) -> Value {
    json!({
        "type": "array",
        "minItems": 1,
        "items": { "type": "string" },
        "description": description,
    })
}

fn text(max: usize, description: &str) -> Value {
    json!({ "type": "string", "maxLength": max, "description": description })
}

fn env_table() -> Value {
    json!({
        "description": "Where the environment comes from. A string sets the mode. A table holds non-secret KEY = \"value\" overrides applied after env_file, with the reserved key `mode` setting the mode (TOML cannot hold both an env string and an [env] table). Values are always strings.",
        "oneOf": [
            { "enum": ENV_MODES },
            {
                "type": "object",
                "propertyNames": { "pattern": "^[A-Za-z_][A-Za-z0-9_]*$" },
                "properties": {
                    "mode": { "enum": ENV_MODES, "default": "login-shell" }
                },
                "additionalProperties": { "type": "string" }
            }
        ]
    })
}

fn feed_table() -> Value {
    json!({
        "type": "object",
        "description": "Gives the job a feed: a list of items shown in the Feed overlay and a sidebar section. The command runs after every successful run, on demand and at server start, and must print one feed v1 JSON object (see flockr-feed.schema.json).",
        "additionalProperties": false,
        "required": ["command"],
        "properties": {
            "command": argv("Required argv array whose stdout is the feed JSON and nothing else."),
            "title": { "type": "string", "description": "Section title; wins over the feed's own title." },
            "work": { "$ref": "#/$defs/feedWork" }
        }
    })
}

fn feed_work_table() -> Value {
    let variables = WORK_TEMPLATE_VARIABLES
        .iter()
        .map(|name| format!("{{{{{name}}}}}"))
        .collect::<Vec<_>>()
        .join(" ");
    json!({
        "type": "object",
        "description": format!(
            "What `w` does in the Feed overlay: start an agent in a new tab on the selected items (or the current item when none is selected). Every key is optional; without the table, `claude` starts in the job's cwd (else your home directory) with a built-in prompt. Template variables in tab_title, cwd, agent_command, prompt and the prompt file: {variables} and {{{{item.field.<label>}}}} (a field of the first item, label in lower case with other characters as _). {{{{item.*}}}} is the first selected item. Substitution is a single pass, so item text cannot inject further placeholders; an unknown or missing variable renders empty with a warning."
        ),
        "additionalProperties": false,
        "properties": {
            "agent_command": {
                "type": "string",
                "minLength": 1,
                "default": "claude",
                "description": "The agent CLI, run in a new tab. Add {prompt_file} to pass the rendered prompt file anywhere; otherwise the prompt becomes the first argument."
            },
            "cwd": { "type": "string", "description": "Directory of the new tab; ~ and templates work. Default: the job's cwd when it sets one, else your home directory. A directory that does not exist falls back to the default." },
            "tab_title": text(MAX_TAB_TITLE_CHARS, "Title of the new tab (templated; cut to this length). Default: \"{{job.title}} · {{count}} items\"."),
            "prompt_file": {
                "type": "string",
                "description": format!("Prompt template, relative to the job directory. The rendered prompt is capped at {MAX_PROMPT_CHARS} characters; {{{{items}}}} at {MAX_ITEMS_TEXT_CHARS} (each body at {MAX_ITEM_BODY_CHARS}), {{{{items_json}}}} at {MAX_FEED_JSON_CHARS}, at most {MAX_WORK_ITEMS} items are used.")
            },
            "prompt": { "type": "string", "description": "Inline prompt template; use this or prompt_file, not both. Default: a built-in prompt that lists the items and asks for a summary and a plan before any change." }
        },
        "not": { "required": ["prompt", "prompt_file"] }
    })
}

fn handoff_table() -> Value {
    let variables = TEMPLATE_VARIABLES
        .iter()
        .map(|name| format!("{{{{{name}}}}}"))
        .collect::<Vec<_>>()
        .join(" ");
    json!({
        "type": "object",
        "description": format!(
            "After a run the job may ask whether to start an agent tab; nothing starts until the user answers. Template variables in ask, tab_title, cwd, agent_command, prompt and the prompt file: {variables} and {{{{context.<path>}}}} (from a dynamic flockr_handoff line's context object). Substitution is a single pass, so job output cannot inject further placeholders; an unknown or missing variable renders empty with a warning."
        ),
        "additionalProperties": false,
        "properties": {
            "when": {
                "description": "When a finished run asks. on_change compares stdout and the feed with the previous run. exit:<code> matches one exit code.",
                "default": "on_change",
                "anyOf": [
                    { "enum": HANDOFF_WHEN },
                    { "type": "string", "pattern": "^exit:-?[0-9]+$" }
                ]
            },
            "ask": text(MAX_ASK_CHARS, "The question shown in the toast and the Decisions tab (templated; one line, cut to this length)."),
            "agent_command": {
                "type": "string",
                "minLength": 1,
                "default": "claude",
                "description": "The agent CLI, run in a new tab. Add {prompt_file} to pass the rendered prompt file anywhere; otherwise the prompt becomes the first argument."
            },
            "cwd": { "type": "string", "description": "Directory of the new tab; ~ and templates work. Default: the job directory." },
            "tab_title": text(MAX_TAB_TITLE_CHARS, "Title of the new tab (templated; cut to this length)."),
            "prompt_file": {
                "type": "string",
                "description": format!("Prompt template, relative to the job directory. The rendered prompt is capped at {MAX_PROMPT_CHARS} characters; {{{{stdout}}}} at {MAX_STDOUT_CHARS}, {{{{stderr_tail}}}} at {MAX_STDERR_CHARS}, {{{{feed_json}}}} at {MAX_FEED_JSON_CHARS}, each context value at {MAX_CONTEXT_VALUE_CHARS}.")
            },
            "prompt": { "type": "string", "description": "Inline prompt template; use this or prompt_file, not both." }
        },
        "not": { "required": ["prompt", "prompt_file"] }
    })
}

fn system_job_def() -> Value {
    json!({
        "type": "object",
        "description": "Overrides for one built-in job.",
        "additionalProperties": false,
        "properties": {
            "every": { "type": "string", "pattern": DURATION_PATTERN, "description": format!("Interval such as \"24h\" (minimum {}). Set this or schedule, not both.", format_duration(MIN_INTERVAL)) },
            "schedule": { "type": "string", "description": "Five-field cron expression in local time. Set this or every, not both." },
            "enabled": { "type": "boolean" },
            "notify": { "enum": NOTIFY_VALUES }
        },
        "not": { "required": ["every", "schedule"] }
    })
}

/// The `[jobs]` section of config.toml.
fn jobs_config() -> Value {
    let mut names = serde_json::Map::new();
    for name in crate::jobs::system::system_job_names() {
        names.insert(name.to_owned(), json!({ "$ref": "#/$defs/systemJob" }));
    }
    json!({
        "type": "object",
        "description": "The [jobs] section of config.toml. User jobs live in job.toml files; this section only tunes the jobs flockr ships with, keyed by name without the `system.` prefix.",
        "additionalProperties": false,
        "properties": {
            "system": {
                "type": "object",
                "additionalProperties": false,
                "properties": names
            }
        }
    })
}

/// The schema for `~/.config/flockr/jobs/<id>/job.toml`.
pub(crate) fn job_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "flockr job.toml",
        "description": "A scheduled job: ~/.config/flockr/jobs/<id>/job.toml. Unknown keys are errors. The [jobs.system.*] section described under $defs.jobsConfig is not part of job.toml; it belongs in config.toml.",
        "type": "object",
        "additionalProperties": false,
        "required": ["command"],
        "oneOf": [
            { "required": ["schedule"] },
            { "required": ["every"] }
        ],
        "properties": {
            "id": {
                "type": "string",
                "pattern": "^[A-Za-z0-9][A-Za-z0-9._-]*$",
                "maxLength": JOB_ID_MAX_CHARS,
                "description": "Optional. Must equal the job directory's name when present."
            },
            "title": { "type": "string", "description": "Name shown in lists and toasts. Default: the id." },
            "command": argv("Required argv array; the first element is the program. Flockr does not run a shell unless you do ([\"sh\", \"-c\", \"...\"])."),
            "schedule": { "type": "string", "description": "Five-field cron expression in local time (minute hour day-of-month month day-of-week). Exactly one of schedule and every." },
            "every": { "type": "string", "pattern": DURATION_PATTERN, "description": format!("Interval such as 30m, 1h or 1h30m (units s, m, h, d; minimum {}). Exactly one of schedule and every.", format_duration(MIN_INTERVAL)) },
            "timeout": {
                "type": "string",
                "pattern": DURATION_PATTERN,
                "default": format_duration(DEFAULT_TIMEOUT),
                "description": format!("Longest a run may take (minimum {}, maximum {}). On expiry the command and its process group are stopped.", format_duration(MIN_TIMEOUT), format_duration(MAX_TIMEOUT))
            },
            "enabled": { "type": "boolean", "default": true, "description": "`flockr job new` writes false. `flockr job enable` overrides this value." },
            "env": env_table(),
            "env_file": { "type": "string", "description": "KEY=VALUE lines added to the environment; relative to the job directory." },
            "cwd": { "type": "string", "description": "Working directory. Default: the job directory." },
            "notify": { "enum": NOTIFY_VALUES, "default": "on_failure", "description": "When a finished run shows a toast." },
            "feed": { "$ref": "#/$defs/feed" },
            "handoff": { "$ref": "#/$defs/handoff" }
        },
        "$defs": {
            "feed": feed_table(),
            "feedWork": feed_work_table(),
            "handoff": handoff_table(),
            "jobsConfig": jobs_config(),
            "systemJob": system_job_def()
        }
    })
}

fn item_schema() -> Value {
    let reserved_names = RESERVED_NAMES;
    let reserved_chars: Vec<String> = RESERVED_CHARS.iter().map(char::to_string).collect();
    json!({
        "type": "object",
        "required": ["id"],
        "description": "One row. Unknown fields are ignored. A bad item or action is dropped with a diagnostic instead of failing the feed.",
        "properties": {
            "id": { "type": "string", "minLength": 1, "maxLength": MAX_ID_CHARS, "description": "Unique within the feed (a repeated id keeps the first)." },
            "title": text(MAX_TITLE_CHARS, "Row text; defaults to the id."),
            "subtitle": text(MAX_SUBTITLE_CHARS, "Dimmed second line."),
            "state": { "enum": FEED_STATES, "default": "read", "description": "unread shows a dot and counts as unread; an unknown value becomes read." },
            "badge": text(MAX_BADGE_CHARS, "Short tag such as a ticket key."),
            "url": { "type": "string", "pattern": "^https?://", "maxLength": MAX_URL_CHARS, "description": "Opened by enter or a click; http or https only." },
            "updated_at": text(MAX_UPDATED_AT_CHARS, "The item's own timestamp, RFC 3339."),
            "body": {
                "type": "string",
                "description": format!("Plain text for the detail pane; line breaks are kept, other control characters are removed. Cut at {} KiB (bytes of UTF-8).", MAX_BODY_BYTES / 1024)
            },
            "fields": {
                "type": "array",
                "maxItems": MAX_FIELDS,
                "description": "Labelled values for the detail pane, shown as a table in order. An entry without a non-empty label and value is dropped.",
                "items": {
                    "type": "object",
                    "required": ["label", "value"],
                    "properties": {
                        "label": text(MAX_FIELD_LABEL_CHARS, "Left column."),
                        "value": text(MAX_FIELD_VALUE_CHARS, "Right column; wrapped.")
                    }
                }
            },
            "group": {
                "type": "string",
                "maxLength": MAX_GROUP_ID_CHARS,
                "description": format!("The id of one of the feed's `groups`; the item shows under that category tab. A missing or undeclared id puts the item in the implicit `{OTHER_GROUP_ID}` group. Ignored when the feed declares no groups.")
            },
            "actions": {
                "type": "array",
                "maxItems": MAX_ACTIONS,
                "description": "Commands run from the Feed overlay with the job's environment; the feed refreshes afterwards.",
                "items": {
                    "type": "object",
                    "required": ["key", "command"],
                    "properties": {
                        "key": {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": 1,
                            "not": { "enum": reserved_chars },
                            "description": format!("One visible character, unique per item. Reserved for the overlay: {} and these key names: {}.", RESERVED_CHARS.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(" "), reserved_names.join(", ")) + &format!(" In a feed that declares groups these are reserved too: {}.", GROUPED_RESERVED_CHARS.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(" "))
                        },
                        "title": text(MAX_ACTION_TITLE_CHARS, "Menu text; defaults to the key."),
                        "command": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": MAX_ARGV,
                            "items": { "type": "string", "maxLength": MAX_ARG_CHARS },
                            "description": "argv array; the first element is the program."
                        }
                    }
                }
            }
        }
    })
}

/// The schema for the JSON a feed command prints.
pub(crate) fn feed_schema() -> Value {
    json!({
        "$schema": DRAFT,
        "title": "flockr feed v1",
        "description": format!(
            "What a job's feed command prints on stdout: ONE JSON object and nothing else, at most {} KiB. Longer strings are cut, items beyond {MAX_ITEMS} are dropped.",
            MAX_FEED_BYTES / 1024
        ),
        "type": "object",
        "required": ["version", "items"],
        "properties": {
            "version": { "const": FEED_VERSION, "description": "A missing version is read as 1 with a diagnostic; any other value is rejected." },
            "title": text(MAX_FEED_TITLE_CHARS, "Section title; the job's feed.title wins."),
            "groups": {
                "type": "array",
                "maxItems": MAX_GROUPS,
                "description": format!("Optional categories, in tab order. Items point at one with `group`; items without a known group go to an implicit `{OTHER_GROUP_ID}` group shown last. Without groups the feed is one flat list."),
                "items": {
                    "type": "object",
                    "required": ["id"],
                    "properties": {
                        "id": { "type": "string", "minLength": 1, "maxLength": MAX_GROUP_ID_CHARS, "description": "Unique within the feed (a repeated id keeps the first)." },
                        "title": text(MAX_GROUP_TITLE_CHARS, "Tab text; defaults to the id."),
                        "short_title": text(MAX_GROUP_SHORT_TITLE_CHARS, "A shorter tab text for narrow places such as the sidebar.")
                    }
                }
            },
            "items": {
                "type": "array",
                "maxItems": MAX_ITEMS,
                "items": { "$ref": "#/$defs/item" }
            }
        },
        "$defs": { "item": item_schema() }
    })
}

fn pretty(value: &Value) -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_owned())
    )
}

pub(crate) fn job_schema_text() -> String {
    pretty(&job_schema())
}

pub(crate) fn feed_schema_text() -> String {
    pretty(&feed_schema())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::format::{action_key_problem, parse_feed};
    use crate::jobs::definition::parse_job_toml;
    use std::path::Path;

    fn artifact(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("docs/next/api")
            .join(name)
    }

    fn check_artifact(name: &str, actual: &str) {
        let path = artifact(name);
        if std::env::var_os("FLOCKR_UPDATE_JOB_SCHEMA").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, actual).unwrap();
            return;
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|err| {
            panic!(
                "failed to read {}; run `FLOCKR_UPDATE_JOB_SCHEMA=1 just test-one job_schema_artifacts_are_current`: {err}",
                path.display()
            )
        });
        assert_eq!(
            expected, actual,
            "{name} is stale; run `FLOCKR_UPDATE_JOB_SCHEMA=1 just test-one job_schema_artifacts_are_current`"
        );
    }

    #[test]
    fn job_schema_artifacts_are_current() {
        check_artifact("flockr-job.schema.json", &job_schema_text());
        check_artifact("flockr-feed.schema.json", &feed_schema_text());
    }

    /// The names serde lists in "expected one of ..." for an unknown key.
    fn accepted_keys(toml: &str) -> Vec<String> {
        let errors = parse_job_toml(toml, "x", Path::new("/x")).unwrap_err();
        let message = errors.join("\n");
        let (_, list) = message
            .split_once("expected ")
            .unwrap_or_else(|| panic!("no key list in: {message}"));
        let list = list.lines().next().unwrap();
        let list = list.strip_prefix("one of ").unwrap_or(list);
        list.replace(" or ", ", ")
            .split(", ")
            .map(|name| name.trim_matches('`').to_owned())
            .collect()
    }

    fn property_names(value: &Value) -> Vec<String> {
        let mut names: Vec<String> = value["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        names.sort();
        names
    }

    fn sorted(mut names: Vec<String>) -> Vec<String> {
        names.sort();
        names
    }

    #[test]
    fn job_schema_properties_are_exactly_the_keys_the_parser_accepts() {
        let schema = job_schema();
        // `env` is a string or a table; the parser splits it out first and
        // hands its mode to serde under the same name.
        assert_eq!(property_names(&schema), sorted(accepted_keys("zzz = 1\n")));
        assert_eq!(
            property_names(&schema["$defs"]["handoff"]),
            sorted(accepted_keys(
                "command = [\"x\"]\nevery = \"1h\"\n[handoff]\nzzz = 1\n"
            ))
        );
        assert_eq!(
            property_names(&schema["$defs"]["feed"]),
            sorted(accepted_keys(
                "command = [\"x\"]\nevery = \"1h\"\n[feed]\nzzz = 1\n"
            ))
        );
        assert_eq!(
            property_names(&schema["$defs"]["feedWork"]),
            sorted(accepted_keys(
                "command = [\"x\"]\nevery = \"1h\"\n[feed]\ncommand = [\"y\"]\n[feed.work]\nzzz = 1\n"
            ))
        );
    }

    #[test]
    fn schema_enums_and_limits_match_the_parser() {
        let base = "command = [\"x\"]\nevery = \"1h\"\n";
        let ok = |extra: &str| parse_job_toml(&format!("{base}{extra}"), "x", Path::new("/x"));
        for value in NOTIFY_VALUES {
            assert!(ok(&format!("notify = \"{value}\"\n")).is_ok(), "{value}");
        }
        assert!(ok("notify = \"sometimes\"\n").is_err());
        for value in ENV_MODES {
            assert!(ok(&format!("env = \"{value}\"\n")).is_ok(), "{value}");
            assert!(
                ok(&format!("[env]\nmode = \"{value}\"\n")).is_ok(),
                "{value}"
            );
        }
        assert!(ok("env = \"other\"\n").is_err());
        for value in HANDOFF_WHEN.iter().copied().chain(["exit:2", "exit:-1"]) {
            assert!(
                ok(&format!("[handoff]\nwhen = \"{value}\"\n")).is_ok(),
                "{value}"
            );
        }
        assert!(ok("[handoff]\nwhen = \"exit:x\"\n").is_err());
        assert!(ok("[handoff]\nprompt = \"a\"\nprompt_file = \"b\"\n").is_err());
        assert!(ok(
            "[feed]\ncommand = [\"y\"]\n[feed.work]\nprompt = \"a\"\nprompt_file = \"b\"\n"
        )
        .is_err());
        assert!(
            ok("[feed]\ntitle = \"t\"\n").is_err(),
            "feed.command is required"
        );

        let schema = job_schema();
        assert_eq!(schema["properties"]["id"]["maxLength"], JOB_ID_MAX_CHARS);
        let long_id = "a".repeat(JOB_ID_MAX_CHARS + 1);
        assert!(parse_job_toml(base, &long_id, Path::new("/x")).is_err());
        assert!(parse_job_toml(base, &long_id[1..], Path::new("/x")).is_ok());
        assert_eq!(schema["required"], json!(["command"]));
        assert!(parse_job_toml("every = \"1h\"\n", "x", Path::new("/x")).is_err());
        assert!(parse_job_toml(
            "command = [\"x\"]\nevery = \"1h\"\nschedule = \"* * * * *\"\n",
            "x",
            Path::new("/x")
        )
        .is_err());
    }

    #[test]
    fn system_config_schema_matches_the_config_types() {
        let schema = job_schema();
        let system = &schema["$defs"]["systemJob"];
        let all_keys = r#"every = "1h"
schedule = "0 9 * * *"
enabled = true
notify = "never"
"#;
        let parsed: crate::config::SystemJobConfig = toml::from_str(all_keys).unwrap();
        assert!(parsed.every.is_some() && parsed.schedule.is_some());
        assert!(parsed.enabled.is_some() && parsed.notify.is_some());
        assert_eq!(
            property_names(system),
            ["enabled", "every", "notify", "schedule"]
        );
        let names = property_names(&schema["$defs"]["jobsConfig"]["properties"]["system"]);
        assert_eq!(
            names,
            sorted(
                crate::jobs::system::system_job_names()
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            )
        );
    }

    fn feed_with(item: &str) -> Result<crate::feed::format::ParsedFeed, Vec<String>> {
        parse_feed(&format!("{{\"version\":1,\"items\":[{item}]}}"))
    }

    #[test]
    fn feed_schema_caps_match_the_parser() {
        let schema = feed_schema();
        let item = &schema["$defs"]["item"]["properties"];
        let cap = |name: &str| item[name]["maxLength"].as_u64().unwrap() as usize;
        for (field, max) in [
            ("title", MAX_TITLE_CHARS),
            ("subtitle", MAX_SUBTITLE_CHARS),
            ("badge", MAX_BADGE_CHARS),
            ("updated_at", MAX_UPDATED_AT_CHARS),
        ] {
            assert_eq!(cap(field), max, "{field}");
            let fits = feed_with(&format!(
                "{{\"id\":\"a\",\"title\":\"t\",\"{field}\":\"{}\"}}",
                "x".repeat(max)
            ))
            .unwrap();
            assert!(
                fits.diagnostics.is_empty(),
                "{field}: {:?}",
                fits.diagnostics
            );
            let over = feed_with(&format!(
                "{{\"id\":\"a\",\"title\":\"t\",\"{field}\":\"{}\"}}",
                "x".repeat(max + 1)
            ))
            .unwrap();
            assert!(
                over.diagnostics.iter().any(|d| d.contains("cut")),
                "{field}: {:?}",
                over.diagnostics
            );
        }
        assert_eq!(cap("id"), MAX_ID_CHARS);
        assert_eq!(schema["properties"]["items"]["maxItems"], MAX_ITEMS);
        assert_eq!(item["actions"]["maxItems"], MAX_ACTIONS);
        assert_eq!(schema["properties"]["version"]["const"], FEED_VERSION);
        let many: Vec<String> = (0..=MAX_ITEMS)
            .map(|n| format!("{{\"id\":\"{n}\"}}"))
            .collect();
        let parsed = feed_with(&many.join(",")).unwrap();
        assert_eq!(parsed.items.len(), MAX_ITEMS);
        let states = item["state"]["enum"].as_array().unwrap();
        for state in states {
            let parsed = feed_with(&format!(
                "{{\"id\":\"a\",\"title\":\"t\",\"state\":{state}}}"
            ))
            .unwrap();
            assert!(parsed.diagnostics.is_empty(), "{state}");
        }
        assert_eq!(states.len(), FEED_STATES.len());
    }

    #[test]
    fn feed_schema_body_and_fields_caps_match_the_parser() {
        let schema = feed_schema();
        let item = &schema["$defs"]["item"]["properties"];
        assert_eq!(item["fields"]["maxItems"], MAX_FIELDS);
        let field = &item["fields"]["items"]["properties"];
        assert_eq!(field["label"]["maxLength"], MAX_FIELD_LABEL_CHARS);
        assert_eq!(field["value"]["maxLength"], MAX_FIELD_VALUE_CHARS);
        assert!(item["body"]["description"]
            .as_str()
            .unwrap()
            .contains(&format!("{} KiB", MAX_BODY_BYTES / 1024)));
        let fits = feed_with(&format!(
            "{{\"id\":\"a\",\"title\":\"t\",\"body\":\"{}\"}}",
            "x".repeat(MAX_BODY_BYTES)
        ))
        .unwrap();
        assert!(fits.diagnostics.is_empty(), "{:?}", fits.diagnostics);
        let over = feed_with(&format!(
            "{{\"id\":\"a\",\"title\":\"t\",\"body\":\"{}\"}}",
            "x".repeat(MAX_BODY_BYTES + 1)
        ))
        .unwrap();
        assert!(over.diagnostics.iter().any(|d| d.contains("cut")));
    }

    #[test]
    fn feed_schema_reserved_keys_are_the_ones_the_parser_rejects() {
        let schema = feed_schema();
        let key = &schema["$defs"]["item"]["properties"]["actions"]["items"]["properties"]["key"];
        let reserved: Vec<&str> = key["not"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect();
        assert!(!reserved.is_empty());
        for char_key in &reserved {
            assert!(action_key_problem(char_key).is_some(), "{char_key}");
        }
        for name in RESERVED_NAMES {
            assert!(action_key_problem(name).is_some(), "{name}");
        }
        for fine in ["d", "s", "t", "u", "1", "!"] {
            assert!(action_key_problem(fine).is_none(), "{fine}");
        }
        assert_eq!(key["maxLength"], 1);
    }

    #[test]
    fn schemas_are_valid_json_with_local_refs_only() {
        fn refs(value: &Value, out: &mut Vec<String>) {
            match value {
                Value::Object(map) => {
                    for (key, child) in map {
                        if key == "$ref" {
                            out.push(child.as_str().unwrap().to_owned());
                        }
                        refs(child, out);
                    }
                }
                Value::Array(items) => items.iter().for_each(|item| refs(item, out)),
                _ => {}
            }
        }
        for schema in [job_schema(), feed_schema()] {
            let mut found = Vec::new();
            refs(&schema, &mut found);
            assert!(!found.is_empty());
            for reference in found {
                let name = reference.strip_prefix("#/$defs/").expect("local ref");
                assert!(schema["$defs"].get(name).is_some(), "{reference}");
            }
        }
    }
}
