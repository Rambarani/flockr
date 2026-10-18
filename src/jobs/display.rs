//! Text shared by the CLI and the Jobs overlay.

use crate::api::schema::{JobInfo, JobRunStatus, JobState};

/// `user`, `system` or `plugin` for a job's `source`.
pub(crate) fn source_word(source: &str) -> &str {
    if source.starts_with("plugin:") {
        "plugin"
    } else {
        source
    }
}

/// `5s`, `3m`, `2h`, `4d`.
pub(crate) fn short_span(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3_599 => format!("{}m", secs / 60),
        3_600..=86_399 => format!("{}h", secs / 3_600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// `in 5m` for a future time, `due` once it has passed.
pub(crate) fn until(now_ms: u64, at_ms: u64) -> String {
    if at_ms <= now_ms {
        "due".into()
    } else {
        format!("in {}", short_span(at_ms - now_ms))
    }
}

/// `5m ago`.
pub(crate) fn ago(now_ms: u64, at_ms: u64) -> String {
    format!("{} ago", short_span(now_ms.saturating_sub(at_ms)))
}

/// `850ms`, `1.2s`, `2m05s`.
pub(crate) fn run_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 10_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if ms < 60_000 {
        format!("{}s", ms / 1000)
    } else {
        format!("{}m{:02}s", ms / 60_000, ms % 60_000 / 1000)
    }
}

pub(crate) fn status_word(status: JobRunStatus) -> &'static str {
    match status {
        JobRunStatus::Success => "ok",
        JobRunStatus::Failed => "failed",
        JobRunStatus::TimedOut => "timed out",
        JobRunStatus::SpawnFailed => "not started",
        JobRunStatus::Skipped => "skipped",
    }
}

/// `ok 1.2s`, `failed (exit 3) 4.1s`, `skipped: still running`, or `never run`.
pub(crate) fn last_result(job: &JobInfo) -> String {
    let Some(run) = &job.last_run else {
        return "never run".into();
    };
    match run.status {
        JobRunStatus::Success => format!("ok {}", run_duration(run.duration_ms)),
        JobRunStatus::Skipped => run.summary.clone(),
        JobRunStatus::SpawnFailed => format!("not started: {}", run.summary),
        JobRunStatus::TimedOut => format!("{} ", run.summary).trim_end().to_owned(),
        JobRunStatus::Failed => {
            format!("failed ({}) {}", run.summary, run_duration(run.duration_ms))
        }
    }
}

pub(crate) fn state_word(state: JobState) -> &'static str {
    match state {
        JobState::Idle => "idle",
        JobState::Running => "running",
        JobState::Disabled => "disabled",
        JobState::Invalid => "invalid",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_and_durations() {
        assert_eq!(short_span(5_000), "5s");
        assert_eq!(short_span(180_000), "3m");
        assert_eq!(short_span(7_300_000), "2h");
        assert_eq!(short_span(200_000_000), "2d");
        assert_eq!(until(10, 5), "due");
        assert_eq!(until(0, 120_000), "in 2m");
        assert_eq!(ago(200_000, 80_000), "2m ago");
        assert_eq!(run_duration(850), "850ms");
        assert_eq!(run_duration(1_234), "1.2s");
        assert_eq!(run_duration(45_000), "45s");
        assert_eq!(run_duration(125_000), "2m05s");
    }
}
