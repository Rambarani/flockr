//! Changing when a job runs: parsing what the user typed (`every 2h` or a
//! cron expression) and editing a `job.toml` safely.
//!
//! The file edit is deliberately narrow. It replaces the one existing
//! `schedule` or `every` line and nothing else, keeps indentation and a
//! trailing comment, and is only applied after the edited text parses to the
//! same TOML document as the original with just that key changed. A file that
//! cannot be edited that way (the key is written in a form the line editor
//! does not understand, or both keys are set) is reported, and the caller
//! stores the schedule as an override in the job's state instead.

use super::cron::CronExpr;
use super::definition::{format_duration, parse_duration, MIN_INTERVAL};

/// A schedule as the user typed it, validated and normalised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScheduleInput {
    /// `every = "2h"`: the normalised duration text.
    Every(String),
    /// `schedule = "0 9 * * 1-5"`: the expression, single-spaced.
    Cron(String),
}

impl ScheduleInput {
    /// The job.toml / config key this schedule is stored under.
    pub(crate) fn key(&self) -> &'static str {
        match self {
            Self::Every(_) => "every",
            Self::Cron(_) => "schedule",
        }
    }

    pub(crate) fn value(&self) -> &str {
        match self {
            Self::Every(text) | Self::Cron(text) => text,
        }
    }

    /// `every 2h` or the cron expression; parses back to the same input.
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::Every(text) => format!("every {text}"),
            Self::Cron(text) => text.clone(),
        }
    }
}

/// Parse `every 2h`, a bare duration such as `90m`, or a five-field cron
/// expression. The error names the problem and the accepted forms.
pub(crate) fn parse_schedule_input(text: &str) -> Result<ScheduleInput, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("type an interval such as `every 2h` or a cron expression such as `0 9 * * 1-5`".into());
    }
    let lower = text.to_ascii_lowercase();
    let duration_text = lower
        .strip_prefix("every ")
        .or_else(|| lower.strip_prefix("every\t"))
        .map(str::trim);
    let looks_like_duration = lower
        .chars()
        .all(|ch| ch.is_ascii_digit() || matches!(ch, 's' | 'm' | 'h' | 'd'))
        && lower.chars().any(|ch| ch.is_ascii_digit());
    if let Some(duration_text) = duration_text.or(looks_like_duration.then_some(lower.as_str())) {
        let interval = parse_duration(duration_text).map_err(|err| format!("interval {err}"))?;
        if interval < MIN_INTERVAL {
            return Err(format!(
                "interval '{duration_text}' is shorter than the 1m minimum"
            ));
        }
        return Ok(ScheduleInput::Every(format_duration(interval)));
    }
    let expression = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match CronExpr::parse(&expression) {
        Ok(_) => Ok(ScheduleInput::Cron(expression)),
        Err(err) => Err(format!(
            "'{text}' is not an interval (`every 2h`) or a valid cron expression: {err}"
        )),
    }
}

/// The interval or cron expression a state override stores, parsed back.
pub(crate) fn schedule_from_text(text: &str) -> Option<super::definition::Schedule> {
    match parse_schedule_input(text).ok()? {
        ScheduleInput::Every(value) => parse_duration(&value)
            .ok()
            .map(super::definition::Schedule::Every),
        ScheduleInput::Cron(value) => CronExpr::parse(&value)
            .ok()
            .map(super::definition::Schedule::Cron),
    }
}

fn toml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Split `key = value # comment` into the parts around the value.
/// Returns the indentation, the key and the text after the value.
fn simple_string_assignment(line: &str, keys: &[&str]) -> Option<(String, String, String)> {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    for key in keys {
        let Some(rest) = trimmed.strip_prefix(key) else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let quote = rest.chars().next()?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        if rest.starts_with("\"\"\"") || rest.starts_with("'''") {
            return None;
        }
        // Find the closing quote of a single-line string.
        let mut escaped = false;
        let mut end = None;
        for (index, ch) in rest.char_indices().skip(1) {
            if quote == '"' && escaped {
                escaped = false;
                continue;
            }
            if quote == '"' && ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == quote {
                end = Some(index);
                break;
            }
        }
        let end = end?;
        let tail = rest[end + 1..].to_owned();
        let tail_trim = tail.trim_start();
        if !(tail_trim.is_empty() || tail_trim.starts_with('#')) {
            return None;
        }
        return Some((indent.to_owned(), (*key).to_owned(), tail));
    }
    None
}

/// Replace the job's top-level `schedule` or `every` line with `input`.
/// `Err` says why the file cannot be edited safely.
pub(crate) fn edit_job_toml(content: &str, input: &ScheduleInput) -> Result<String, String> {
    let original: toml::Table = toml::from_str(content)
        .map_err(|err| format!("job.toml does not parse: {}", err.to_string().trim_end()))?;
    let has_schedule = original.contains_key("schedule");
    let has_every = original.contains_key("every");
    if has_schedule == has_every {
        return Err(if has_schedule {
            "job.toml sets both schedule and every".into()
        } else {
            "job.toml has no schedule or every key to edit".into()
        });
    }

    let mut lines: Vec<String> = content.split('\n').map(str::to_owned).collect();
    let mut found = None;
    let mut in_top_level = true;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_top_level = false;
        }
        if !in_top_level {
            break;
        }
        if simple_string_assignment(line, &["schedule", "every"]).is_some() {
            if found.is_some() {
                return Err("job.toml has more than one schedule line".into());
            }
            found = Some(index);
        }
    }
    let Some(index) = found else {
        return Err(
            "the schedule is written in a form the editor does not handle (not a one-line string)"
                .into(),
        );
    };
    let (indent, _, tail) = simple_string_assignment(&lines[index], &["schedule", "every"])
        .ok_or("the schedule line could not be read")?;
    lines[index] = format!(
        "{indent}{} = {}{}",
        input.key(),
        toml_string(input.value()),
        tail
    );
    let edited = lines.join("\n");

    // Safety check: the only difference from the original is this one key.
    let mut expected = original;
    expected.remove("schedule");
    expected.remove("every");
    expected.insert(
        input.key().to_owned(),
        toml::Value::String(input.value().to_owned()),
    );
    let reparsed: toml::Table = toml::from_str(&edited)
        .map_err(|err| format!("the edited job.toml does not parse: {err}"))?;
    if reparsed != expected {
        return Err("the edit would change more than the schedule".into());
    }
    Ok(edited)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn intervals_parse_with_or_without_the_every_word() {
        assert_eq!(
            parse_schedule_input("every 2h").unwrap(),
            ScheduleInput::Every("2h".into())
        );
        assert_eq!(
            parse_schedule_input("  Every  90m ").unwrap(),
            ScheduleInput::Every("1h30m".into())
        );
        assert_eq!(
            parse_schedule_input("24h").unwrap(),
            ScheduleInput::Every("1d".into())
        );
        assert_eq!(parse_schedule_input("every 2h").unwrap().describe(), "every 2h");
    }

    #[test]
    fn cron_expressions_parse_and_are_single_spaced() {
        let input = parse_schedule_input("0  9 * *   1-5").unwrap();
        assert_eq!(input, ScheduleInput::Cron("0 9 * * 1-5".into()));
        assert_eq!(input.key(), "schedule");
        assert_eq!(input.value(), "0 9 * * 1-5");
        assert_eq!(input.describe(), "0 9 * * 1-5");
    }

    #[test]
    fn bad_input_explains_itself() {
        assert!(parse_schedule_input("").unwrap_err().contains("type an interval"));
        assert!(parse_schedule_input("every 30s").unwrap_err().contains("1m minimum"));
        assert!(parse_schedule_input("every soon").unwrap_err().contains("interval"));
        assert!(parse_schedule_input("0 9 * *").unwrap_err().contains("cron"));
        assert!(parse_schedule_input("every").is_err());
        assert!(parse_schedule_input("h").is_err());
    }

    #[test]
    fn override_text_round_trips() {
        assert!(matches!(
            schedule_from_text("every 2h"),
            Some(super::super::definition::Schedule::Every(d)) if d == Duration::from_secs(7200)
        ));
        assert!(matches!(
            schedule_from_text("0 9 * * *"),
            Some(super::super::definition::Schedule::Cron(_))
        ));
        assert!(schedule_from_text("nonsense").is_none());
    }

    #[test]
    fn the_schedule_line_is_replaced_in_place() {
        let original = "# my job\ntitle = \"x\"\ncommand = [\"echo\"]\nevery = \"1h\"   # hourly\nnotify = \"never\"\n\n[env]\nA = \"1\"\n";
        let edited = edit_job_toml(original, &ScheduleInput::Every("2h".into())).unwrap();
        assert_eq!(
            edited,
            original.replace("every = \"1h\"   # hourly", "every = \"2h\"   # hourly")
        );
    }

    #[test]
    fn switching_between_interval_and_cron_changes_the_key() {
        let original = "command = [\"echo\"]\nevery = \"1h\"\n";
        let to_cron =
            edit_job_toml(original, &ScheduleInput::Cron("0 9 * * 1-5".into())).unwrap();
        assert_eq!(to_cron, "command = [\"echo\"]\nschedule = \"0 9 * * 1-5\"\n");
        let back = edit_job_toml(&to_cron, &ScheduleInput::Every("30m".into())).unwrap();
        assert_eq!(back, "command = [\"echo\"]\nevery = \"30m\"\n");
        // Literal strings and indentation are handled too.
        let literal = "command = [\"echo\"]\n  schedule = '0 1 * * *'\n";
        let edited = edit_job_toml(literal, &ScheduleInput::Every("1d".into())).unwrap();
        assert_eq!(edited, "command = [\"echo\"]\n  every = \"1d\"\n");
    }

    #[test]
    fn a_file_that_cannot_be_edited_safely_is_refused() {
        let every = ScheduleInput::Every("2h".into());
        for (content, expect) in [
            ("command = [\"x\"]\n", "no schedule or every key"),
            ("command = [\"x\"]\nevery = \"1h\"\nschedule = \"* * * * *\"\n", "both"),
            ("command = [\"x\"\nevery = ", "does not parse"),
            // Dotted or inline forms the line editor does not understand.
            ("command = [\"x\"]\nevery = \"\"\"1h\"\"\"\n", "form the editor does not handle"),
            ("command = [\"x\"]\n[handoff]\nwhen = \"always\"\n", "no schedule or every key"),
        ] {
            let err = edit_job_toml(content, &every).unwrap_err();
            assert!(err.contains(expect), "{content:?}: {err}");
        }
    }

    #[test]
    fn a_schedule_key_inside_a_table_is_not_touched() {
        // `every` under [feed] would be a different key; only the top level counts.
        let content = "command = [\"x\"]\nschedule = \"0 1 * * *\"\n[env]\nevery = \"keep\"\n";
        let edited = edit_job_toml(content, &ScheduleInput::Every("2h".into())).unwrap();
        assert!(edited.contains("every = \"2h\""));
        assert!(edited.contains("[env]\nevery = \"keep\""));
    }

    #[test]
    fn a_trailing_comment_with_quotes_survives() {
        let content = "command = [\"x\"]\nevery = \"1h\" # \"quoted\" comment\n";
        let edited = edit_job_toml(content, &ScheduleInput::Every("3h".into())).unwrap();
        assert_eq!(edited, "command = [\"x\"]\nevery = \"3h\" # \"quoted\" comment\n");
    }
}
