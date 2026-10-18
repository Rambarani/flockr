//! `flockr decision`: answer the questions jobs ask (their `[handoff]`)
//! through the socket API.

use crate::api::schema::{
    DecisionInfo, DecisionListParams, DecisionSnoozeParams, DecisionStatus, DecisionTarget, Method,
    Request, ResponseResult,
};
use crate::jobs::decisions::DEFAULT_SNOOZE_SECS;
use crate::jobs::display;

use super::job::call;

const USAGE: &str =
    "usage: flockr decision <list [--all] [--json]|start <id>|snooze <id> [--for 1h]|dismiss <id>>";

pub(super) fn run_decision_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let rest = &args[1..];
    match subcommand {
        "list" => decision_list(rest),
        "start" => decision_start(rest),
        "snooze" => decision_snooze(rest),
        "dismiss" => decision_dismiss(rest),
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

fn require_id<'a>(args: &'a [String], usage: &str) -> Result<&'a str, i32> {
    match args {
        [id] if !id.starts_with('-') => Ok(id),
        _ => {
            eprintln!("{usage}");
            Err(2)
        }
    }
}

fn status_word(decision: &DecisionInfo, now: u64) -> String {
    match decision.status {
        DecisionStatus::Pending => "pending".into(),
        DecisionStatus::Snoozed => decision.snoozed_until_unix_ms.map_or_else(
            || "snoozed".into(),
            |until| format!("snoozed {}", display::until(now, until)),
        ),
        DecisionStatus::Started => "started".into(),
        DecisionStatus::Dismissed => "dismissed".into(),
    }
}

fn decision_list(args: &[String]) -> std::io::Result<i32> {
    let mut json = false;
    let mut all = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "--all" => all = true,
            _ => {
                eprintln!("usage: flockr decision list [--all] [--json]");
                return Ok(2);
            }
        }
    }
    let params = DecisionListParams {
        include_resolved: all,
        ..DecisionListParams::default()
    };
    if json {
        return super::print_response(&super::send_request(&Request {
            id: "cli:decision:list".into(),
            method: Method::DecisionList(params),
        })?);
    }
    let result = match call("cli:decision:list", Method::DecisionList(params))? {
        Ok(result) => result,
        Err(code) => return Ok(code),
    };
    let ResponseResult::DecisionList { decisions, .. } = result else {
        eprintln!("unexpected response from the server");
        return Ok(1);
    };
    if decisions.is_empty() {
        println!("no pending decisions");
        return Ok(0);
    }
    let now = crate::jobs::now_unix_ms();
    let id_width = decisions
        .iter()
        .map(|decision| decision.decision_id.len())
        .max()
        .unwrap_or(2)
        .max(2);
    let job_width = decisions
        .iter()
        .map(|decision| decision.job_id.len())
        .max()
        .unwrap_or(3)
        .clamp(3, 28);
    println!(
        "{:<id_width$}  {:<job_width$}  {:<7}  {:<14}  ASK",
        "ID", "JOB", "AGE", "STATUS"
    );
    for decision in &decisions {
        println!(
            "{:<id_width$}  {:<job_width$}  {:<7}  {:<14}  {}",
            decision.decision_id,
            crate::ui::truncate_end(&decision.job_id, job_width),
            display::short_span(now.saturating_sub(decision.created_unix_ms)),
            status_word(decision, now),
            decision.ask,
        );
        if let Some(error) = &decision.last_error {
            println!("{:<id_width$}  last start failed: {error}", "");
        }
    }
    Ok(0)
}

fn decision_start(args: &[String]) -> std::io::Result<i32> {
    let id = match require_id(args, "usage: flockr decision start <id>") {
        Ok(id) => id,
        Err(code) => return Ok(code),
    };
    let result = match call(
        "cli:decision:start",
        Method::DecisionStart(DecisionTarget {
            decision_id: id.to_owned(),
        }),
    )? {
        Ok(result) => result,
        Err(code) => return Ok(code),
    };
    let ResponseResult::DecisionStarted {
        decision,
        tab_id,
        pane_id,
    } = result
    else {
        eprintln!("unexpected response from the server");
        return Ok(1);
    };
    println!(
        "started {} in tab {tab_id} ({pane_id}) running: {}",
        decision.decision_id, decision.agent_command
    );
    Ok(0)
}

fn decision_snooze(args: &[String]) -> std::io::Result<i32> {
    let usage = "usage: flockr decision snooze <id> [--for 1h]";
    let (id, for_secs) = match args {
        [id] if !id.starts_with('-') => (id.as_str(), DEFAULT_SNOOZE_SECS),
        [id, flag, value] | [flag, value, id] if flag == "--for" && !id.starts_with('-') => {
            match crate::jobs::definition::parse_duration(value) {
                Ok(duration) if duration.as_secs() >= 1 => (id.as_str(), duration.as_secs()),
                Ok(_) => {
                    eprintln!("--for must be at least 1s");
                    return Ok(2);
                }
                Err(err) => {
                    eprintln!("--for {err}");
                    return Ok(2);
                }
            }
        }
        _ => {
            eprintln!("{usage}");
            return Ok(2);
        }
    };
    let result = match call(
        "cli:decision:snooze",
        Method::DecisionSnooze(DecisionSnoozeParams {
            decision_id: id.to_owned(),
            for_secs: Some(for_secs),
        }),
    )? {
        Ok(result) => result,
        Err(code) => return Ok(code),
    };
    let ResponseResult::DecisionUpdated { decision } = result else {
        eprintln!("unexpected response from the server");
        return Ok(1);
    };
    println!(
        "{} snoozed for {}",
        decision.decision_id,
        display::short_span(for_secs * 1000)
    );
    Ok(0)
}

fn decision_dismiss(args: &[String]) -> std::io::Result<i32> {
    let id = match require_id(args, "usage: flockr decision dismiss <id>") {
        Ok(id) => id,
        Err(code) => return Ok(code),
    };
    let result = match call(
        "cli:decision:dismiss",
        Method::DecisionDismiss(DecisionTarget {
            decision_id: id.to_owned(),
        }),
    )? {
        Ok(result) => result,
        Err(code) => return Ok(code),
    };
    let ResponseResult::DecisionUpdated { decision } = result else {
        eprintln!("unexpected response from the server");
        return Ok(1);
    };
    println!("{} dismissed", decision.decision_id);
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_errors_exit_two_without_calling_the_server() {
        for args in [
            vec![],
            vec!["bogus".to_string()],
            vec!["list".to_string(), "--nope".to_string()],
            vec!["start".to_string()],
            vec!["start".to_string(), "--x".to_string()],
            vec!["dismiss".to_string()],
            vec!["snooze".to_string()],
            vec!["snooze".to_string(), "d1".to_string(), "--for".to_string()],
            vec![
                "snooze".to_string(),
                "d1".to_string(),
                "--for".to_string(),
                "soon".to_string(),
            ],
            vec![
                "snooze".to_string(),
                "d1".to_string(),
                "--for".to_string(),
                "0s".to_string(),
            ],
        ] {
            assert_eq!(run_decision_command(&args).unwrap(), 2, "{args:?}");
        }
        assert_eq!(run_decision_command(&["--help".to_string()]).unwrap(), 0);
    }

    #[test]
    fn status_words_show_the_snooze_countdown() {
        let mut decision = DecisionInfo {
            decision_id: "d1".into(),
            job_id: "j".into(),
            job_title: "j".into(),
            run_id: 1,
            ask: "a".into(),
            tab_title: "t".into(),
            cwd: "/".into(),
            agent_command: "claude".into(),
            status: DecisionStatus::Snoozed,
            created_unix_ms: 0,
            snoozed_until_unix_ms: Some(1_000_000 + 600_000),
            resolved_unix_ms: None,
            outcome: None,
            last_error: None,
            prompt_file: None,
            context: None,
            prompt: None,
        };
        assert_eq!(status_word(&decision, 1_000_000), "snoozed in 10m");
        decision.status = DecisionStatus::Pending;
        assert_eq!(status_word(&decision, 0), "pending");
    }
}
