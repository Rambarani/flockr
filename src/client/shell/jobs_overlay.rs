//! Jobs overlay: a full-screen table of the server's scheduled jobs (state,
//! schedule, next run, last result), with `r` run now, `l` or Enter for the
//! last run's output, `e` enable or disable, `t` to change the schedule, and
//! Esc to close. Tab switches to the Decisions list: the questions jobs asked
//! (their `[handoff]`), answered with Enter or `s` (start the agent tab), `z`
//! (snooze an hour), `x` (dismiss) and `p` (preview the prompt).
//!
//! The data comes from the server's `job.*` methods on the client command
//! lane, never from the snapshot. While the overlay is open the client asks
//! `job.list` every two seconds with the revision it already holds, so an
//! idle server answers with a few bytes. Servers without the methods get a
//! one-line explanation instead of an error.

use std::time::{Duration, Instant};

use crossterm::event::MouseEvent;
use ratatui::layout::Constraint;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Cell, Row, StatefulWidget, Table, TableState};

use super::popup_frame::{base_style, dim_style, draw_panel, heading_style, selected_style};
use super::render::{display_width, put_text};
use super::text_editor::TextEditor;
use super::*;
use crate::api::schema::{
    DecisionInfo, DecisionListParams, DecisionSnoozeParams, DecisionStatus, DecisionTarget,
    JobInfo, JobListParams, JobLogsParams, JobRunStatus, JobScheduleStore, JobSetEnabledParams,
    JobSetScheduleParams, JobState, JobTarget, Method, ResponseResult,
};
use crate::jobs::display;
use crate::jobs::schedule_edit::parse_schedule_input;

const POLL_GAP: Duration = Duration::from_secs(2);
const REPAINT_INTERVAL: Duration = Duration::from_secs(1);
/// The palette's "run job" entries refresh when the cached list is older.
const CACHE_MAX_AGE: Duration = Duration::from_secs(10);
const LOG_LINES: u32 = 1_000;
const MIN_PANEL_WIDTH: u16 = 30;
const MIN_PANEL_HEIGHT: u16 = 6;

/// What a pending `job.*` request was for.
#[derive(Debug, Clone)]
pub(super) enum JobsRequest {
    List,
    Logs {
        job_id: String,
    },
    Run {
        job_id: String,
        from_palette: bool,
    },
    /// `job.run` for the Feed overlay's `S` or a feed menu's "Sync now".
    FeedSync {
        job_id: String,
        origin: super::feed_state::FeedOrigin,
    },
    /// `job.list` while a sync waits for its run to finish.
    FeedSyncCheck {
        job_id: String,
    },
    Toggle,
    /// `decision.list`: the Decisions tab, or the palette's count.
    Decisions,
    /// `decision.list` for one decision with its prompt.
    DecisionPrompt {
        decision_id: String,
    },
    DecisionAnswer {
        decision_id: String,
        action: DecisionAction,
    },
    Schedule,
}

/// What the user chose for a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DecisionAction {
    Start,
    Snooze,
    Dismiss,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum JobsTab {
    #[default]
    Jobs,
    Decisions,
}

/// How long `z` hides a decision.
const SNOOZE_SECS: u64 = 60 * 60;

/// Client-side memory of the jobs the active server reported, for the
/// palette's "run job" entries.
#[derive(Debug, Default)]
pub(super) struct JobsClientState {
    pub(super) cache: Vec<(String, String)>,
    pub(super) cache_endpoint: Option<ClientEndpointId>,
    cache_at: Option<Instant>,
    /// The server rejected or does not know `job.list`.
    unsupported: bool,
    /// Decisions waiting for an answer, as the server last said; shown in the
    /// palette's "decisions (N)" entry.
    pub(super) decision_count: u32,
    decision_count_at: Option<Instant>,
    pub(super) decision_count_endpoint: Option<ClientEndpointId>,
    /// The server does not know `decision.list`.
    pub(super) decisions_unsupported: bool,
}

#[derive(Debug)]
pub(super) struct JobLogsView {
    job_id: String,
    title: String,
    header: String,
    lines: Vec<String>,
    scroll: usize,
    loading: bool,
}

/// A decision's rendered prompt, read-only.
#[derive(Debug)]
pub(super) struct DecisionPreview {
    decision_id: String,
    title: String,
    lines: Vec<String>,
    scroll: usize,
    loading: bool,
}

/// The inline prompt for a job's new schedule.
#[derive(Debug)]
pub(super) struct ScheduleEditor {
    job_id: String,
    input: TextEditor,
}

#[derive(Debug)]
pub(super) struct ClientJobsOverlay {
    jobs: Vec<JobInfo>,
    revision: Option<u64>,
    loaded: bool,
    unsupported: bool,
    pub(super) selected: Option<String>,
    pub(super) scroll: usize,
    logs: Option<JobLogsView>,
    status: Option<String>,
    last_poll: Option<Instant>,
    last_paint: Option<Instant>,
    tab: JobsTab,
    decisions: Vec<DecisionInfo>,
    decisions_revision: Option<u64>,
    decisions_loaded: bool,
    decisions_unsupported: bool,
    /// Waiting for an answer (snoozed ones are not counted).
    pending_decisions: u32,
    decision_selected: Option<String>,
    decision_scroll: usize,
    preview: Option<DecisionPreview>,
    schedule_edit: Option<ScheduleEditor>,
}

#[derive(Default)]
pub(super) struct JobsHits {
    pub(super) popup: Rect,
    pub(super) rows: Vec<(Rect, usize)>,
    pub(super) viewport_rows: usize,
    pub(super) offset: usize,
}

pub(super) struct JobsRender {
    pub(super) area: Rect,
    pub(super) rows: Vec<(Rect, usize)>,
    pub(super) viewport_rows: usize,
    pub(super) offset: usize,
    /// The schedule prompt's caret.
    pub(super) cursor: Option<crate::protocol::CursorState>,
}

/// Drop terminal escape sequences and control characters from job output so
/// it cannot move the cursor or recolor the overlay.
pub(super) fn sanitize_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\u{1b}' => match chars.peek() {
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(next) = chars.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' {
                            chars.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            },
            '\t' => out.push_str("    "),
            ch if ch.is_control() => {}
            ch => out.push(ch),
        }
    }
    out
}

fn build_log_lines(stdout: &str, stderr: &str) -> Vec<String> {
    let mut lines = Vec::new();
    if stdout.is_empty() && stderr.is_empty() {
        lines.push("(no output)".to_owned());
        return lines;
    }
    for (name, text) in [("stdout", stdout), ("stderr", stderr)] {
        if text.is_empty() {
            continue;
        }
        lines.push(format!("── {name} ──"));
        lines.extend(text.lines().map(sanitize_line));
    }
    lines
}

fn build_prompt_lines(decision: &DecisionInfo) -> Vec<String> {
    let mut lines = vec![
        format!("agent  {}", sanitize_line(&decision.agent_command)),
        format!("cwd    {}", sanitize_line(&decision.cwd)),
        format!("tab    {}", sanitize_line(&decision.tab_title)),
        String::new(),
    ];
    match decision.prompt.as_deref() {
        Some(prompt) if !prompt.trim().is_empty() => {
            lines.push("── prompt ──".to_owned());
            lines.extend(prompt.lines().map(sanitize_line));
        }
        _ => lines.push("(no prompt: Start runs the agent command as it is)".to_owned()),
    }
    lines
}

fn row_name(job: &JobInfo) -> String {
    if job.title == job.job_id || job.title.is_empty() {
        job.job_id.clone()
    } else {
        format!("{} · {}", job.job_id, job.title)
    }
}

impl ClientShellState {
    pub(super) fn open_jobs_overlay(&mut self) {
        self.open_jobs_overlay_on(JobsTab::Jobs);
    }

    /// The palette's "decisions (N)" entry.
    pub(super) fn open_decisions_overlay(&mut self) {
        self.open_jobs_overlay_on(JobsTab::Decisions);
    }

    fn open_jobs_overlay_on(&mut self, tab: JobsTab) {
        self.overlay = Some(ClientShellOverlay::Jobs(Box::new(ClientJobsOverlay {
            jobs: Vec::new(),
            revision: None,
            loaded: false,
            unsupported: self.jobs.unsupported,
            selected: None,
            scroll: 0,
            logs: None,
            status: None,
            last_poll: None,
            last_paint: None,
            tab,
            decisions: Vec::new(),
            decisions_revision: None,
            decisions_loaded: false,
            decisions_unsupported: self.jobs.decisions_unsupported,
            pending_decisions: self.jobs.decision_count,
            decision_selected: None,
            decision_scroll: 0,
            preview: None,
            schedule_edit: None,
        })));
    }

    fn jobs_overlay(&self) -> Option<&ClientJobsOverlay> {
        match self.overlay.as_ref() {
            Some(ClientShellOverlay::Jobs(overlay)) => Some(overlay),
            _ => None,
        }
    }

    fn jobs_overlay_mut(&mut self) -> Option<&mut ClientJobsOverlay> {
        match self.overlay.as_mut() {
            Some(ClientShellOverlay::Jobs(overlay)) => Some(overlay),
            _ => None,
        }
    }

    fn selected_job_index(overlay: &ClientJobsOverlay) -> usize {
        overlay
            .selected
            .as_ref()
            .and_then(|id| overlay.jobs.iter().position(|job| job.job_id == *id))
            .unwrap_or(0)
    }

    fn selected_job(&self) -> Option<JobInfo> {
        let overlay = self.jobs_overlay()?;
        overlay.jobs.get(Self::selected_job_index(overlay)).cloned()
    }

    fn selected_decision_index(overlay: &ClientJobsOverlay) -> usize {
        overlay
            .decision_selected
            .as_ref()
            .and_then(|id| {
                overlay
                    .decisions
                    .iter()
                    .position(|decision| decision.decision_id == *id)
            })
            .unwrap_or(0)
    }

    fn selected_decision(&self) -> Option<DecisionInfo> {
        let overlay = self.jobs_overlay()?;
        overlay
            .decisions
            .get(Self::selected_decision_index(overlay))
            .cloned()
    }

    /// Move the selection in whichever list the active tab shows.
    fn move_job_selection(&mut self, delta: isize) {
        let viewport = self.hits.jobs.viewport_rows.max(1);
        let offset = self.hits.jobs.offset;
        let Some(overlay) = self.jobs_overlay_mut() else {
            return;
        };
        let scroll_to = |next: usize| {
            if next < offset {
                next
            } else if next >= offset + viewport {
                next + 1 - viewport
            } else {
                offset
            }
        };
        match overlay.tab {
            JobsTab::Jobs => {
                if overlay.jobs.is_empty() {
                    return;
                }
                let current = Self::selected_job_index(overlay);
                let next =
                    (current as isize + delta).clamp(0, overlay.jobs.len() as isize - 1) as usize;
                overlay.selected = Some(overlay.jobs[next].job_id.clone());
                overlay.scroll = scroll_to(next);
            }
            JobsTab::Decisions => {
                if overlay.decisions.is_empty() {
                    return;
                }
                let current = Self::selected_decision_index(overlay);
                let next = (current as isize + delta).clamp(0, overlay.decisions.len() as isize - 1)
                    as usize;
                overlay.decision_selected = Some(overlay.decisions[next].decision_id.clone());
                overlay.decision_scroll = scroll_to(next);
            }
        }
    }

    /// Scroll the output or prompt view that is open.
    fn scroll_job_logs(&mut self, delta: isize) {
        let viewport = self.hits.jobs.viewport_rows.max(1);
        let Some(overlay) = self.jobs_overlay_mut() else {
            return;
        };
        let (len, scroll) = match (&mut overlay.logs, &mut overlay.preview) {
            (Some(logs), _) => (logs.lines.len(), &mut logs.scroll),
            (None, Some(preview)) => (preview.lines.len(), &mut preview.scroll),
            (None, None) => return,
        };
        let max = len.saturating_sub(viewport) as isize;
        *scroll = (*scroll as isize + delta).clamp(0, max.max(0)) as usize;
    }

    fn switch_jobs_tab(&mut self) {
        let Some(overlay) = self.jobs_overlay_mut() else {
            return;
        };
        overlay.tab = match overlay.tab {
            JobsTab::Jobs => JobsTab::Decisions,
            JobsTab::Decisions => JobsTab::Jobs,
        };
        overlay.status = None;
        // Force fresh data for the tab being shown.
        overlay.last_poll = None;
        overlay.revision = None;
        overlay.decisions_revision = None;
    }

    /// Ask the server for something; a server without the method turns the
    /// overlay into an explanation instead of raising a notice.
    pub(super) fn push_jobs_request(
        &mut self,
        method: Method,
        request: JobsRequest,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !self.supports_endpoint_method(&method) {
            self.jobs.unsupported = true;
            if let Some(overlay) = self.jobs_overlay_mut() {
                overlay.unsupported = true;
            }
            outcome.repaint = true;
            return false;
        }
        self.push_endpoint_method_with_kind(method, PendingEndpointKind::Jobs { request }, outcome)
    }

    /// Like `push_jobs_request` for the decision and schedule methods, which
    /// older servers lack: the feature is disabled, not the whole overlay.
    fn push_decision_request(
        &mut self,
        method: Method,
        request: JobsRequest,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !self.supports_endpoint_method(&method) {
            let is_schedule = matches!(request, JobsRequest::Schedule);
            if is_schedule {
                if let Some(overlay) = self.jobs_overlay_mut() {
                    overlay.status = Some(
                        "this server cannot change schedules yet; update and restart it".into(),
                    );
                }
            } else {
                self.jobs.decisions_unsupported = true;
                if let Some(overlay) = self.jobs_overlay_mut() {
                    overlay.decisions_unsupported = true;
                }
            }
            outcome.repaint = true;
            return false;
        }
        self.push_endpoint_method_with_kind(method, PendingEndpointKind::Jobs { request }, outcome)
    }

    fn decisions_in_flight(&self) -> bool {
        self.pending_requests.values().any(|pending| {
            matches!(
                pending.kind,
                PendingEndpointKind::Jobs {
                    request: JobsRequest::Decisions
                }
            )
        })
    }

    fn request_decision_list(&mut self, revision: Option<u64>, outcome: &mut ClientShellInput) {
        self.push_decision_request(
            Method::DecisionList(DecisionListParams {
                since_revision: revision,
                include_resolved: false,
                decision_id: None,
                include_prompt: false,
            }),
            JobsRequest::Decisions,
            outcome,
        );
    }

    fn answer_selected_decision(&mut self, action: DecisionAction, outcome: &mut ClientShellInput) {
        let Some(decision) = self.selected_decision() else {
            return;
        };
        let target = DecisionTarget {
            decision_id: decision.decision_id.clone(),
        };
        let method = match action {
            DecisionAction::Start => Method::DecisionStart(target),
            DecisionAction::Snooze => Method::DecisionSnooze(DecisionSnoozeParams {
                decision_id: decision.decision_id.clone(),
                for_secs: Some(SNOOZE_SECS),
            }),
            DecisionAction::Dismiss => Method::DecisionDismiss(target),
        };
        if let Some(overlay) = self.jobs_overlay_mut() {
            overlay.status = Some(match action {
                DecisionAction::Start => format!("starting {}…", decision.decision_id),
                DecisionAction::Snooze => format!("snoozing {}…", decision.decision_id),
                DecisionAction::Dismiss => format!("dismissing {}…", decision.decision_id),
            });
        }
        self.push_decision_request(
            method,
            JobsRequest::DecisionAnswer {
                decision_id: decision.decision_id,
                action,
            },
            outcome,
        );
    }

    fn open_decision_preview(&mut self, outcome: &mut ClientShellInput) {
        let Some(decision) = self.selected_decision() else {
            return;
        };
        if let Some(overlay) = self.jobs_overlay_mut() {
            overlay.preview = Some(DecisionPreview {
                decision_id: decision.decision_id.clone(),
                title: format!("{} · {}", decision.decision_id, decision.job_id),
                lines: Vec::new(),
                scroll: 0,
                loading: true,
            });
        }
        self.push_decision_request(
            Method::DecisionList(DecisionListParams {
                since_revision: None,
                include_resolved: true,
                decision_id: Some(decision.decision_id.clone()),
                include_prompt: true,
            }),
            JobsRequest::DecisionPrompt {
                decision_id: decision.decision_id,
            },
            outcome,
        );
    }

    fn start_schedule_edit(&mut self) {
        let Some(job) = self.selected_job() else {
            return;
        };
        let Some(overlay) = self.jobs_overlay_mut() else {
            return;
        };
        if job.state == JobState::Invalid {
            overlay.status = Some("fix the job file first (see the last result column)".into());
            return;
        }
        overlay.status = None;
        overlay.schedule_edit = Some(ScheduleEditor {
            job_id: job.job_id,
            input: TextEditor::new(&job.schedule, true),
        });
    }

    /// Enter in the schedule prompt: save when it parses, otherwise say why not.
    fn submit_schedule_edit(&mut self, outcome: &mut ClientShellInput) {
        let Some(editor) = self
            .jobs_overlay()
            .and_then(|overlay| overlay.schedule_edit.as_ref())
        else {
            return;
        };
        let text = editor.input.as_str().trim().to_owned();
        let job_id = editor.job_id.clone();
        if parse_schedule_input(&text).is_err() {
            // Keep the prompt open; the validation line already says why.
            outcome.repaint = true;
            return;
        }
        if let Some(overlay) = self.jobs_overlay_mut() {
            overlay.schedule_edit = None;
            overlay.status = Some(format!("saving the schedule of {job_id}…"));
        }
        self.push_decision_request(
            Method::JobSetSchedule(JobSetScheduleParams {
                job_id: job_id.clone(),
                schedule: text,
            }),
            JobsRequest::Schedule,
            outcome,
        );
    }

    fn jobs_list_in_flight(&self) -> bool {
        self.pending_requests.values().any(|pending| {
            matches!(
                pending.kind,
                PendingEndpointKind::Jobs {
                    request: JobsRequest::List
                }
            )
        })
    }

    fn request_job_list(&mut self, revision: Option<u64>, outcome: &mut ClientShellInput) {
        self.push_jobs_request(
            Method::JobList(JobListParams {
                since_revision: revision,
            }),
            JobsRequest::List,
            outcome,
        );
    }

    fn run_selected_job(&mut self, outcome: &mut ClientShellInput) {
        let Some(job) = self.selected_job() else {
            return;
        };
        if let Some(overlay) = self.jobs_overlay_mut() {
            overlay.status = Some(format!("starting {}…", job.job_id));
        }
        self.push_jobs_request(
            Method::JobRun(JobTarget {
                job_id: job.job_id.clone(),
            }),
            JobsRequest::Run {
                job_id: job.job_id,
                from_palette: false,
            },
            outcome,
        );
    }

    /// Run a job from the command palette.
    pub(super) fn run_job_from_palette(&mut self, job_id: String, outcome: &mut ClientShellInput) {
        self.push_jobs_request(
            Method::JobRun(JobTarget {
                job_id: job_id.clone(),
            }),
            JobsRequest::Run {
                job_id,
                from_palette: true,
            },
            outcome,
        );
    }

    fn toggle_selected_job(&mut self, outcome: &mut ClientShellInput) {
        let Some(job) = self.selected_job() else {
            return;
        };
        if job.state == JobState::Invalid {
            if let Some(overlay) = self.jobs_overlay_mut() {
                overlay.status = Some("fix the job file first (see the last result column)".into());
            }
            return;
        }
        self.push_jobs_request(
            Method::JobSetEnabled(JobSetEnabledParams {
                job_id: job.job_id.clone(),
                enabled: !job.enabled,
            }),
            JobsRequest::Toggle,
            outcome,
        );
    }

    fn open_selected_job_logs(&mut self, outcome: &mut ClientShellInput) {
        let Some(job) = self.selected_job() else {
            return;
        };
        if job.last_run.is_none() {
            if let Some(overlay) = self.jobs_overlay_mut() {
                overlay.status = Some(format!("{} has not run yet", job.job_id));
            }
            return;
        }
        if let Some(overlay) = self.jobs_overlay_mut() {
            overlay.logs = Some(JobLogsView {
                job_id: job.job_id.clone(),
                title: job.title.clone(),
                header: String::new(),
                lines: Vec::new(),
                scroll: 0,
                loading: true,
            });
        }
        self.push_jobs_request(
            Method::JobLogs(JobLogsParams {
                job_id: job.job_id.clone(),
                run_id: None,
                lines: Some(LOG_LINES),
            }),
            JobsRequest::Logs { job_id: job.job_id },
            outcome,
        );
    }

    /// Handle a key while the overlay is open. Returns false when it is not.
    pub(super) fn route_jobs_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        use crossterm::event::KeyModifiers;

        let Some(overlay) = self.jobs_overlay() else {
            return false;
        };
        let in_detail = overlay.logs.is_some() || overlay.preview.is_some();
        let tab = overlay.tab;
        let editing = overlay.schedule_edit.is_some();
        let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
        let plain = modifiers.is_empty();
        let ctrl = modifiers == KeyModifiers::CONTROL;
        let page = self.hits.jobs.viewport_rows.max(1) as isize;
        outcome.repaint = true;
        if editing {
            self.route_schedule_key(key, outcome);
            return true;
        }
        let typed = {
            let shifted_without_evidence = key.modifiers.contains(KeyModifiers::SHIFT)
                && key.generated_text.is_none()
                && key.shifted_codepoint.is_none();
            (!shifted_without_evidence)
                .then(|| crate::input::keybind_help_text_char(key))
                .flatten()
        };
        if in_detail {
            let close_detail = |state: &mut Self| {
                if let Some(overlay) = state.jobs_overlay_mut() {
                    overlay.logs = None;
                    overlay.preview = None;
                }
            };
            match code {
                KeyCode::Esc => close_detail(self),
                KeyCode::Up if plain => self.scroll_job_logs(-1),
                KeyCode::Down if plain => self.scroll_job_logs(1),
                KeyCode::Char('p') if ctrl => self.scroll_job_logs(-1),
                KeyCode::Char('n') if ctrl => self.scroll_job_logs(1),
                KeyCode::PageUp if plain => self.scroll_job_logs(-page),
                KeyCode::PageDown if plain => self.scroll_job_logs(page),
                KeyCode::Home if plain => self.scroll_job_logs(isize::MIN / 2),
                KeyCode::End if plain => self.scroll_job_logs(isize::MAX / 2),
                _ => match typed {
                    Some('j') => self.scroll_job_logs(1),
                    Some('k') => self.scroll_job_logs(-1),
                    Some('g') => self.scroll_job_logs(isize::MIN / 2),
                    Some('G') => self.scroll_job_logs(isize::MAX / 2),
                    Some('q' | 'l' | 'h' | 'p') => close_detail(self),
                    _ => outcome.repaint = false,
                },
            }
            return true;
        }
        match code {
            KeyCode::Esc => self.overlay = None,
            KeyCode::Tab | KeyCode::BackTab if plain || code == KeyCode::BackTab => {
                self.switch_jobs_tab();
            }
            KeyCode::Enter if plain => match tab {
                JobsTab::Jobs => self.open_selected_job_logs(outcome),
                JobsTab::Decisions => self.answer_selected_decision(DecisionAction::Start, outcome),
            },
            KeyCode::Up if plain => self.move_job_selection(-1),
            KeyCode::Down if plain => self.move_job_selection(1),
            KeyCode::Char('p') if ctrl => self.move_job_selection(-1),
            KeyCode::Char('n') if ctrl => self.move_job_selection(1),
            KeyCode::PageUp if plain => self.move_job_selection(-page),
            KeyCode::PageDown if plain => self.move_job_selection(page),
            KeyCode::Home if plain => self.move_job_selection(isize::MIN / 2),
            KeyCode::End if plain => self.move_job_selection(isize::MAX / 2),
            _ => match (tab, typed) {
                (_, Some('q')) => self.overlay = None,
                (_, Some('j')) => self.move_job_selection(1),
                (_, Some('k')) => self.move_job_selection(-1),
                (_, Some('g')) => self.move_job_selection(isize::MIN / 2),
                (_, Some('G')) => self.move_job_selection(isize::MAX / 2),
                (JobsTab::Jobs, Some('r')) => self.run_selected_job(outcome),
                (JobsTab::Jobs, Some('l')) => self.open_selected_job_logs(outcome),
                (JobsTab::Jobs, Some('e')) => self.toggle_selected_job(outcome),
                (JobsTab::Jobs, Some('t')) => self.start_schedule_edit(),
                (JobsTab::Decisions, Some('s')) => {
                    self.answer_selected_decision(DecisionAction::Start, outcome);
                }
                (JobsTab::Decisions, Some('z')) => {
                    self.answer_selected_decision(DecisionAction::Snooze, outcome);
                }
                (JobsTab::Decisions, Some('x')) => {
                    self.answer_selected_decision(DecisionAction::Dismiss, outcome);
                }
                (JobsTab::Decisions, Some('p')) => self.open_decision_preview(outcome),
                _ => outcome.repaint = false,
            },
        }
        true
    }

    /// Keys in the schedule prompt: the text editor, Enter to save, Esc to
    /// cancel, Ctrl+C to clear.
    fn route_schedule_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) {
        use crossterm::event::KeyModifiers;

        if key.code == KeyCode::Enter {
            self.submit_schedule_edit(outcome);
            return;
        }
        let Some(overlay) = self.jobs_overlay_mut() else {
            return;
        };
        if key.code == KeyCode::Esc {
            overlay.schedule_edit = None;
            return;
        }
        let Some(editor) = overlay.schedule_edit.as_mut() else {
            return;
        };
        let has_text = key
            .generated_text
            .as_deref()
            .is_some_and(|text| !text.is_empty());
        if !has_text
            && (key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL
                || key.code == KeyCode::Backspace && key.modifiers.contains(KeyModifiers::SUPER))
        {
            editor.input.clear();
            return;
        }
        outcome.repaint = editor.input.handle_key(key).is_some();
    }

    /// Handle a mouse event while the overlay is open. Returns false when it
    /// is not, so the event continues to the regular routing.
    pub(super) fn route_jobs_mouse(
        &mut self,
        mouse: MouseEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        use crossterm::event::{MouseButton, MouseEventKind};

        let Some(overlay) = self.jobs_overlay() else {
            return false;
        };
        let in_logs = overlay.logs.is_some() || overlay.preview.is_some();
        let point = (mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if !contains(self.hits.jobs.popup, point) {
                    if in_logs {
                        if let Some(overlay) = self.jobs_overlay_mut() {
                            overlay.logs = None;
                            overlay.preview = None;
                        }
                    } else {
                        self.overlay = None;
                    }
                } else if !in_logs {
                    let row = self
                        .hits
                        .jobs
                        .rows
                        .iter()
                        .find(|(rect, _)| contains(*rect, point))
                        .map(|(_, index)| *index);
                    if let Some(index) = row {
                        if let Some(overlay) = self.jobs_overlay_mut() {
                            match overlay.tab {
                                JobsTab::Jobs => {
                                    if let Some(job) = overlay.jobs.get(index) {
                                        overlay.selected = Some(job.job_id.clone());
                                    }
                                }
                                JobsTab::Decisions => {
                                    if let Some(decision) = overlay.decisions.get(index) {
                                        overlay.decision_selected =
                                            Some(decision.decision_id.clone());
                                    }
                                }
                            }
                        }
                    }
                }
                outcome.repaint = true;
            }
            MouseEventKind::ScrollUp => {
                if in_logs {
                    self.scroll_job_logs(-3);
                } else {
                    self.move_job_selection(-3);
                }
                outcome.repaint = true;
            }
            MouseEventKind::ScrollDown => {
                if in_logs {
                    self.scroll_job_logs(3);
                } else {
                    self.move_job_selection(3);
                }
                outcome.repaint = true;
            }
            _ => {}
        }
        true
    }

    /// Poll the list while the overlay is open (or while the palette needs
    /// fresh job names), and repaint every second so countdowns advance.
    pub(crate) fn tick_jobs(&mut self, now: Instant, outcome: &mut ClientShellInput) -> bool {
        let palette_open = matches!(self.overlay, Some(ClientShellOverlay::Palette(_)));
        if let Some(overlay) = self.jobs_overlay() {
            let repaint = overlay
                .last_paint
                .is_none_or(|at| now.saturating_duration_since(at) >= REPAINT_INTERVAL);
            let poll_due = !overlay.unsupported
                && overlay.logs.is_none()
                && overlay.preview.is_none()
                && overlay
                    .last_poll
                    .is_none_or(|at| now.saturating_duration_since(at) >= POLL_GAP)
                && !self.jobs_list_in_flight();
            let decisions_poll_due =
                poll_due && !overlay.decisions_unsupported && !self.decisions_in_flight();
            let revision = overlay.revision;
            let decisions_revision = overlay.decisions_revision;
            if let Some(overlay) = self.jobs_overlay_mut() {
                if repaint {
                    overlay.last_paint = Some(now);
                }
                if poll_due {
                    overlay.last_poll = Some(now);
                }
            }
            if poll_due {
                self.request_job_list(revision, outcome);
            }
            if decisions_poll_due {
                self.request_decision_list(decisions_revision, outcome);
            }
            return repaint;
        }
        if palette_open && !self.jobs.unsupported && !self.jobs_list_in_flight() {
            let stale = self.jobs.cache_endpoint.as_ref() != Some(&self.active_endpoint_id)
                || self
                    .jobs
                    .cache_at
                    .is_none_or(|at| now.saturating_duration_since(at) >= CACHE_MAX_AGE);
            if stale {
                // Mark first so a slow server is not asked every tick.
                self.jobs.cache_at = Some(now);
                self.jobs.cache_endpoint = Some(self.active_endpoint_id.clone());
                self.request_job_list(None, outcome);
            }
        }
        if palette_open && !self.jobs.decisions_unsupported && !self.decisions_in_flight() {
            let stale = self.jobs.decision_count_endpoint.as_ref()
                != Some(&self.active_endpoint_id)
                || self
                    .jobs
                    .decision_count_at
                    .is_none_or(|at| now.saturating_duration_since(at) >= CACHE_MAX_AGE);
            if stale {
                self.jobs.decision_count_at = Some(now);
                self.jobs.decision_count_endpoint = Some(self.active_endpoint_id.clone());
                self.request_decision_list(None, outcome);
            }
        }
        false
    }

    fn remember_job_names(&mut self, jobs: &[JobInfo]) {
        self.jobs.cache = jobs
            .iter()
            .filter(|job| job.state != JobState::Invalid)
            .map(|job| (job.job_id.clone(), job.title.clone()))
            .collect();
        self.jobs.cache_endpoint = Some(self.active_endpoint_id.clone());
        self.jobs.cache_at = Some(Instant::now());
        self.jobs.unsupported = false;
    }

    /// Handle the response to a `job.*` request. These are quiet: failures
    /// show in the overlay (or one notice for a palette run) rather than as
    /// generic endpoint notices.
    pub(super) fn complete_jobs_request(
        &mut self,
        request: JobsRequest,
        result: Result<ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let mut outcome = ClientShellInput::default();
        match (request, result) {
            (
                JobsRequest::List,
                Ok(ResponseResult::JobList {
                    revision,
                    unchanged,
                    jobs,
                    ..
                }),
            ) => {
                if !unchanged {
                    self.remember_job_names(&jobs);
                }
                let Some(overlay) = self.jobs_overlay_mut() else {
                    return (false, Vec::new());
                };
                overlay.loaded = true;
                overlay.revision = Some(revision);
                if !unchanged {
                    overlay.jobs = jobs;
                    let keep = overlay
                        .selected
                        .as_ref()
                        .is_some_and(|id| overlay.jobs.iter().any(|job| job.job_id == *id));
                    if !keep {
                        overlay.selected = overlay.jobs.first().map(|job| job.job_id.clone());
                    }
                }
                (!unchanged, Vec::new())
            }
            (
                JobsRequest::Logs { job_id },
                Ok(ResponseResult::JobLogs {
                    run,
                    stdout,
                    stderr,
                    ..
                }),
            ) => {
                let Some(overlay) = self.jobs_overlay_mut() else {
                    return (false, Vec::new());
                };
                if let Some(logs) = overlay.logs.as_mut().filter(|logs| logs.job_id == job_id) {
                    logs.header = format!(
                        "run #{} · {} · {} · {}",
                        run.run_id,
                        display::status_word(run.status),
                        run.summary,
                        display::ago(crate::jobs::now_unix_ms(), run.started_unix_ms)
                    );
                    logs.lines = build_log_lines(&stdout, &stderr);
                    logs.loading = false;
                    // Open at the end: the newest output matters most.
                    logs.scroll = usize::MAX;
                }
                (true, Vec::new())
            }
            (
                JobsRequest::Run {
                    job_id,
                    from_palette,
                },
                Ok(ResponseResult::JobRunRequested {
                    started, reason, ..
                }),
            ) => {
                let text = if started {
                    format!("started {job_id}")
                } else {
                    format!(
                        "{job_id}: {}",
                        reason.unwrap_or_else(|| "not started".into())
                    )
                };
                if let Some(overlay) = self.jobs_overlay_mut() {
                    overlay.status = Some(text);
                    overlay.last_poll = None;
                    overlay.revision = None;
                } else if from_palette && !started {
                    self.push_endpoint_notice(
                        ClientEndpointNoticeKind::Rejected,
                        format!("job.run:{job_id}"),
                        "Job not started",
                        text,
                    );
                }
                (true, Vec::new())
            }
            (JobsRequest::FeedSync { job_id, origin }, result) => {
                (self.complete_feed_sync_start(job_id, origin, result), Vec::new())
            }
            (JobsRequest::FeedSyncCheck { job_id }, result) => {
                (self.complete_feed_sync_check(&job_id, result), Vec::new())
            }
            (JobsRequest::Toggle, Ok(ResponseResult::JobUpdated { job })) => {
                let Some(overlay) = self.jobs_overlay_mut() else {
                    return (false, Vec::new());
                };
                overlay.status = Some(format!(
                    "{} {}",
                    job.job_id,
                    if job.enabled { "enabled" } else { "disabled" }
                ));
                overlay.last_poll = None;
                overlay.revision = None;
                if let Some(slot) = overlay
                    .jobs
                    .iter_mut()
                    .find(|slot| slot.job_id == job.job_id)
                {
                    *slot = job;
                }
                (true, Vec::new())
            }
            (
                JobsRequest::Decisions,
                Ok(ResponseResult::DecisionList {
                    revision,
                    unchanged,
                    pending,
                    decisions,
                }),
            ) => {
                self.jobs.decision_count = pending;
                self.jobs.decisions_unsupported = false;
                self.jobs.decision_count_endpoint = Some(self.active_endpoint_id.clone());
                self.jobs.decision_count_at = Some(Instant::now());
                let Some(overlay) = self.jobs_overlay_mut() else {
                    // The palette shows the count.
                    return (true, Vec::new());
                };
                overlay.decisions_loaded = true;
                overlay.decisions_revision = Some(revision);
                let count_changed = overlay.pending_decisions != pending;
                overlay.pending_decisions = pending;
                if !unchanged {
                    overlay.decisions = decisions;
                    let keep = overlay.decision_selected.as_ref().is_some_and(|id| {
                        overlay
                            .decisions
                            .iter()
                            .any(|decision| decision.decision_id == *id)
                    });
                    if !keep {
                        overlay.decision_selected = overlay
                            .decisions
                            .first()
                            .map(|decision| decision.decision_id.clone());
                    }
                }
                (!unchanged || count_changed, Vec::new())
            }
            (
                JobsRequest::DecisionPrompt { decision_id },
                Ok(ResponseResult::DecisionList { decisions, .. }),
            ) => {
                let Some(overlay) = self.jobs_overlay_mut() else {
                    return (false, Vec::new());
                };
                if let (Some(preview), Some(decision)) = (
                    overlay
                        .preview
                        .as_mut()
                        .filter(|preview| preview.decision_id == decision_id),
                    decisions.first(),
                ) {
                    preview.lines = build_prompt_lines(decision);
                    preview.loading = false;
                }
                (true, Vec::new())
            }
            (
                JobsRequest::DecisionAnswer {
                    decision_id,
                    action,
                },
                Ok(ResponseResult::DecisionStarted { .. }),
            ) => {
                debug_assert_eq!(action, DecisionAction::Start);
                // The new tab is focused; get out of its way.
                let _ = decision_id;
                self.overlay = None;
                (true, Vec::new())
            }
            (
                JobsRequest::DecisionAnswer { action, .. },
                Ok(ResponseResult::DecisionUpdated { decision }),
            ) => {
                let Some(overlay) = self.jobs_overlay_mut() else {
                    return (false, Vec::new());
                };
                overlay.status = Some(match action {
                    DecisionAction::Snooze => {
                        format!(
                            "{} snoozed for {}",
                            decision.decision_id,
                            display::short_span(SNOOZE_SECS * 1000)
                        )
                    }
                    DecisionAction::Dismiss => format!("{} dismissed", decision.decision_id),
                    DecisionAction::Start => format!("{} started", decision.decision_id),
                });
                overlay.last_poll = None;
                overlay.decisions_revision = None;
                (true, Vec::new())
            }
            (
                JobsRequest::Schedule,
                Ok(ResponseResult::JobScheduleUpdated {
                    job,
                    stored_in,
                    note,
                }),
            ) => {
                let Some(overlay) = self.jobs_overlay_mut() else {
                    return (false, Vec::new());
                };
                let place = match stored_in {
                    JobScheduleStore::Config => "config.toml",
                    JobScheduleStore::JobFile => "job.toml",
                    JobScheduleStore::State => "a state override",
                };
                overlay.status = Some(match note {
                    Some(note) => {
                        format!("{} now runs {} ({place}): {note}", job.job_id, job.schedule)
                    }
                    None => format!(
                        "{} now runs {} (saved in {place})",
                        job.job_id, job.schedule
                    ),
                });
                overlay.last_poll = None;
                overlay.revision = None;
                if let Some(slot) = overlay
                    .jobs
                    .iter_mut()
                    .find(|slot| slot.job_id == job.job_id)
                {
                    *slot = job;
                }
                (true, Vec::new())
            }
            (request, Err(error)) => {
                let message = error.message.clone();
                if let JobsRequest::Run {
                    job_id,
                    from_palette: true,
                } = &request
                {
                    if self.jobs_overlay().is_none() {
                        self.push_endpoint_notice(
                            ClientEndpointNoticeKind::Rejected,
                            format!("job.run:{job_id}"),
                            "Job not started",
                            message.clone(),
                        );
                        return (true, Vec::new());
                    }
                }
                if let Some(overlay) = self.jobs_overlay_mut() {
                    if matches!(request, JobsRequest::Logs { .. }) {
                        overlay.logs = None;
                    }
                    if matches!(request, JobsRequest::DecisionPrompt { .. }) {
                        overlay.preview = None;
                    }
                    if matches!(request, JobsRequest::DecisionAnswer { .. }) {
                        // A failed start records why on the decision; fetch it.
                        overlay.last_poll = None;
                        overlay.decisions_revision = None;
                    }
                    overlay.status = Some(message);
                }
                (true, Vec::new())
            }
            (_, Ok(_)) => {
                outcome.repaint = false;
                (false, outcome.actions)
            }
        }
    }

    pub(super) fn render_jobs_overlay(&self, buffer: &mut Buffer) -> Option<JobsRender> {
        let overlay = self.jobs_overlay()?;
        render_jobs(buffer, overlay, crate::jobs::now_unix_ms(), &self.config)
    }
}

struct JobColumns {
    name: u16,
    schedule: Option<u16>,
    next: Option<u16>,
}

fn choose_columns(width: u16) -> JobColumns {
    // Marker, name, schedule, next and a last-result column that takes the rest.
    for (name, schedule, next, min_last) in [
        (30u16, Some(16u16), Some(9u16), 22u16),
        (22, Some(14), Some(8), 16),
        (18, None, Some(8), 12),
    ] {
        let used = 1 + name + schedule.unwrap_or(0) + next.unwrap_or(0);
        let columns = 3 + u16::from(schedule.is_some()) + u16::from(next.is_some());
        if used + columns + min_last <= width {
            return JobColumns {
                name,
                schedule,
                next,
            };
        }
    }
    JobColumns {
        name: 14,
        schedule: None,
        next: None,
    }
}

fn dot(
    job: &JobInfo,
) -> (
    &'static str,
    fn(&crate::app::state::Palette) -> ratatui::style::Color,
) {
    match (job.state, job.last_run.as_ref().map(|run| run.status)) {
        (JobState::Running, _) => ("◉", |p| p.yellow),
        (JobState::Invalid, _) => ("✖", |p| p.red),
        (JobState::Disabled, _) => ("○", |p| p.overlay0),
        (_, Some(JobRunStatus::Success)) => ("●", |p| p.green),
        (_, Some(JobRunStatus::Skipped)) => ("●", |p| p.peach),
        (_, Some(_)) => ("●", |p| p.red),
        (_, None) => ("●", |p| p.overlay0),
    }
}

/// The tab strip in the panel title: the active tab in brackets, the
/// decisions badge on either.
fn overlay_title(overlay: &ClientJobsOverlay) -> String {
    if let Some(logs) = &overlay.logs {
        return format!("job output · {}", logs.title);
    }
    if let Some(preview) = &overlay.preview {
        return format!("decision prompt · {}", preview.title);
    }
    let decisions = if overlay.pending_decisions > 0 {
        format!("decisions ({})", overlay.pending_decisions)
    } else {
        "decisions".to_owned()
    };
    match overlay.tab {
        JobsTab::Jobs => format!("[jobs]  {decisions}"),
        JobsTab::Decisions => format!("jobs  [{decisions}]"),
    }
}

/// Pending decisions of one job, for the badge on its row.
fn job_decision_count(overlay: &ClientJobsOverlay, job_id: &str) -> usize {
    overlay
        .decisions
        .iter()
        .filter(|decision| decision.job_id == job_id && decision.status == DecisionStatus::Pending)
        .count()
}

/// A scrollable, read-only text view: a job's output or a decision's prompt.
struct DetailView<'a> {
    header: &'a str,
    lines: &'a [String],
    scroll: usize,
    loading: bool,
}

fn render_detail(
    buffer: &mut Buffer,
    inner: Rect,
    detail: &DetailView<'_>,
    palette: &crate::app::state::Palette,
) -> (usize, usize) {
    let hint_y = inner.bottom() - 1;
    let text_width = inner.width.saturating_sub(2);
    put_text(
        buffer,
        inner.x + 1,
        inner.y,
        text_width,
        &crate::ui::truncate_end(detail.header, usize::from(text_width)),
        dim_style(palette),
    );
    let body = Rect::new(
        inner.x + 1,
        inner.y + 1,
        text_width,
        inner.height.saturating_sub(3),
    );
    let viewport = usize::from(body.height).max(1);
    let max_scroll = detail.lines.len().saturating_sub(viewport);
    let offset = detail.scroll.min(max_scroll);
    if detail.loading {
        put_text(
            buffer,
            body.x,
            body.y,
            body.width,
            "loading…",
            dim_style(palette),
        );
    }
    for (row, line) in detail.lines.iter().skip(offset).take(viewport).enumerate() {
        let heading = line.starts_with("── ");
        put_text(
            buffer,
            body.x,
            body.y + row as u16,
            body.width,
            &crate::ui::truncate_end(line, usize::from(body.width)),
            if heading {
                heading_style(palette)
            } else {
                base_style(palette)
            },
        );
    }
    if detail.lines.len() > viewport {
        let position = format!(
            "{}-{} / {}",
            offset + 1,
            (offset + viewport).min(detail.lines.len()),
            detail.lines.len()
        );
        let width = display_width(&position);
        put_text(
            buffer,
            inner.right().saturating_sub(width + 1),
            hint_y,
            width,
            &position,
            dim_style(palette),
        );
    }
    let hint = if text_width >= 56 {
        "↑/↓ j/k scroll   pgup/pgdn page   g/G top/end   esc back"
    } else {
        "j/k scroll   esc back"
    };
    put_text(
        buffer,
        inner.x + 1,
        hint_y,
        text_width,
        hint,
        dim_style(palette),
    );
    (viewport, offset)
}

fn render_jobs(
    buffer: &mut Buffer,
    overlay: &ClientJobsOverlay,
    now_ms: u64,
    config: &ClientShellConfig,
) -> Option<JobsRender> {
    let palette = &config.palette;
    let area = buffer.area;
    if area.width < MIN_PANEL_WIDTH || area.height < MIN_PANEL_HEIGHT {
        return None;
    }
    let title = overlay_title(overlay);
    let inner = draw_panel(buffer, area, &title, palette)?;
    let empty = JobsRender {
        area,
        rows: Vec::new(),
        viewport_rows: 1,
        offset: 0,
        cursor: None,
    };
    if inner.height < 4 || inner.width < 20 {
        return Some(empty);
    }
    let hint_y = inner.bottom() - 1;
    let status_y = inner.bottom().saturating_sub(2);
    let text_width = inner.width.saturating_sub(2);

    let detail = match (&overlay.logs, &overlay.preview) {
        (Some(logs), _) => Some(DetailView {
            header: &logs.header,
            lines: &logs.lines,
            scroll: logs.scroll,
            loading: logs.loading,
        }),
        (None, Some(preview)) => Some(DetailView {
            header: "",
            lines: &preview.lines,
            scroll: preview.scroll,
            loading: preview.loading,
        }),
        (None, None) => None,
    };
    if let Some(detail) = detail {
        let (viewport, offset) = render_detail(buffer, inner, &detail, palette);
        return Some(JobsRender {
            viewport_rows: viewport,
            offset,
            ..empty
        });
    }

    let table_area = Rect::new(
        inner.x + 1,
        inner.y,
        text_width,
        inner.height.saturating_sub(2),
    );
    let (hit_rows, viewport_rows, offset) = match overlay.tab {
        JobsTab::Jobs => render_jobs_table(buffer, table_area, overlay, now_ms, config),
        JobsTab::Decisions => render_decisions_table(buffer, table_area, overlay, now_ms, config),
    };

    let mut cursor = None;
    if let Some(editor) = &overlay.schedule_edit {
        // The schedule prompt takes over the bottom two rows.
        let label = format!("schedule for {}: ", editor.job_id);
        let label = crate::ui::truncate_end(&label, usize::from(text_width / 2));
        let label_width = display_width(&label);
        put_text(
            buffer,
            inner.x + 1,
            status_y,
            text_width,
            &label,
            Style::default()
                .fg(palette.accent)
                .bg(palette.panel_bg)
                .add_modifier(Modifier::BOLD),
        );
        let field = Rect::new(
            inner.x + 1 + label_width,
            status_y,
            text_width.saturating_sub(label_width),
            1,
        );
        buffer.set_style(
            field,
            Style::default().fg(palette.text).bg(palette.surface0),
        );
        cursor = text_editor::render(
            buffer,
            field,
            &editor.input,
            Style::default().fg(palette.text).bg(palette.surface0),
        );
        let (verdict, style) = match parse_schedule_input(editor.input.as_str()) {
            Ok(parsed) => (
                format!("✓ runs {}   ↵ save   esc cancel", parsed.describe()),
                Style::default().fg(palette.green).bg(palette.panel_bg),
            ),
            Err(error) => (
                format!("✗ {error}"),
                Style::default().fg(palette.red).bg(palette.panel_bg),
            ),
        };
        put_text(
            buffer,
            inner.x + 1,
            hint_y,
            text_width,
            &crate::ui::truncate_end(&verdict, usize::from(text_width)),
            style,
        );
        return Some(JobsRender {
            area,
            rows: hit_rows,
            viewport_rows,
            offset,
            cursor,
        });
    }

    let selected_error = (overlay.tab == JobsTab::Decisions)
        .then(|| {
            overlay
                .decisions
                .get(ClientShellState::selected_decision_index(overlay))
                .and_then(|decision| decision.last_error.clone())
        })
        .flatten();
    if let Some(status) = overlay.status.clone().or_else(|| {
        selected_error
            .clone()
            .map(|error| format!("last start failed: {error}"))
    }) {
        let failed = overlay.status.is_none();
        put_text(
            buffer,
            inner.x + 1,
            status_y,
            text_width,
            &crate::ui::truncate_end(&status, usize::from(text_width)),
            Style::default()
                .fg(if failed { palette.red } else { palette.accent })
                .bg(palette.panel_bg)
                .add_modifier(Modifier::BOLD),
        );
    }
    let hints: [&str; 4] = match overlay.tab {
        JobsTab::Jobs => [
            "↑/↓ j/k move   r run   l logs   e on/off   t schedule   tab decisions   esc close",
            "r run   l logs   e on/off   t schedule   tab decisions   esc",
            "r run   l logs   t schedule   tab decisions",
            "esc close",
        ],
        JobsTab::Decisions => [
            "↑/↓ j/k move   ↵/s start   z snooze 1h   x dismiss   p preview   tab jobs   esc close",
            "↵/s start   z snooze   x dismiss   p preview   tab jobs   esc",
            "s start   z snooze   x dismiss   p preview",
            "esc close",
        ],
    };
    let hint = hints
        .into_iter()
        .find(|hint| hint.chars().count() <= usize::from(text_width))
        .unwrap_or("");
    put_text(
        buffer,
        inner.x + 1,
        hint_y,
        text_width,
        hint,
        dim_style(palette),
    );
    Some(JobsRender {
        area,
        rows: hit_rows,
        viewport_rows,
        offset,
        cursor,
    })
}

/// Hit rectangles, visible row count and scroll offset of a drawn table.
type TableLayout = (Vec<(Rect, usize)>, usize, usize);

fn render_jobs_table(
    buffer: &mut Buffer,
    table_area: Rect,
    overlay: &ClientJobsOverlay,
    now_ms: u64,
    config: &ClientShellConfig,
) -> TableLayout {
    let palette = &config.palette;
    let columns = choose_columns(table_area.width);
    let mut widths = vec![Constraint::Length(1), Constraint::Length(columns.name)];
    let mut header = vec!["", "job"];
    if let Some(width) = columns.schedule {
        widths.push(Constraint::Length(width));
        header.push("schedule");
    }
    if let Some(width) = columns.next {
        widths.push(Constraint::Length(width));
        header.push("next");
    }
    widths.push(Constraint::Fill(1));
    header.push("last result");
    let fixed = 1 + columns.name + columns.schedule.unwrap_or(0) + columns.next.unwrap_or(0);
    let spacing = widths.len() as u16 - 1;
    let last_width = table_area.width.saturating_sub(fixed + spacing).max(1);
    let fit = |text: &str, width: u16| crate::ui::truncate_end(text, usize::from(width));

    let selected_index = overlay
        .selected
        .as_ref()
        .and_then(|id| overlay.jobs.iter().position(|job| job.job_id == *id))
        .unwrap_or(0);
    let rows = overlay
        .jobs
        .iter()
        .enumerate()
        .map(|(index, job)| {
            let is_selected = index == selected_index;
            let plain = if job.state == JobState::Disabled {
                dim_style(palette)
            } else {
                base_style(palette)
            };
            let style_for = |style: Style| {
                if is_selected {
                    selected_style(palette)
                } else {
                    style
                }
            };
            let (marker, color) = dot(job);
            let marker_style = style_for(Style::default().fg(color(palette)).bg(palette.panel_bg));
            let next = match (job.state, job.next_run_unix_ms) {
                (JobState::Running, _) => job.running_since_unix_ms.map_or_else(
                    || "running".to_owned(),
                    |since| {
                        format!(
                            "running {}",
                            display::short_span(now_ms.saturating_sub(since))
                        )
                    },
                ),
                (JobState::Disabled, _) => "off".to_owned(),
                (_, Some(at)) => display::until(now_ms, at),
                (_, None) => "-".to_owned(),
            };
            let (last, last_style) = if job.state == JobState::Invalid {
                (
                    job.diagnostics.first().cloned().unwrap_or_default(),
                    Style::default().fg(palette.red).bg(palette.panel_bg),
                )
            } else {
                let style = match job.last_run.as_ref().map(|run| run.status) {
                    Some(JobRunStatus::Success) | None => plain,
                    Some(JobRunStatus::Skipped) => {
                        Style::default().fg(palette.peach).bg(palette.panel_bg)
                    }
                    Some(_) => Style::default().fg(palette.red).bg(palette.panel_bg),
                };
                (display::last_result(job), style)
            };
            // A badge for decisions waiting on this job, kept visible by
            // shortening the name instead.
            let waiting = job_decision_count(overlay, &job.job_id);
            let name_cell = if waiting > 0 {
                let badge = format!(" ●{waiting}");
                let room = usize::from(columns.name).saturating_sub(display_width(&badge) as usize);
                Cell::from(ratatui::text::Line::from(vec![
                    Span::styled(
                        crate::ui::truncate_end(&row_name(job), room),
                        style_for(plain),
                    ),
                    Span::styled(
                        badge,
                        style_for(Style::default().fg(palette.peach).bg(palette.panel_bg)),
                    ),
                ]))
            } else {
                Cell::from(Span::styled(
                    fit(&row_name(job), columns.name),
                    style_for(plain),
                ))
            };
            let mut cells = vec![Cell::from(Span::styled(marker, marker_style)), name_cell];
            if let Some(width) = columns.schedule {
                cells.push(Cell::from(Span::styled(
                    fit(&job.schedule, width),
                    style_for(dim_style(palette)),
                )));
            }
            if let Some(width) = columns.next {
                cells.push(Cell::from(Span::styled(
                    fit(&next, width),
                    style_for(dim_style(palette)),
                )));
            }
            cells.push(Cell::from(Span::styled(
                fit(&last, last_width),
                style_for(last_style),
            )));
            Row::new(cells).style(if is_selected {
                selected_style(palette)
            } else {
                base_style(palette)
            })
        })
        .collect::<Vec<_>>();
    let table =
        Table::new(rows, widths)
            .header(
                Row::new(header.into_iter().map(|title| {
                    Cell::from(Span::styled(title.to_owned(), heading_style(palette)))
                }))
                .style(base_style(palette)),
            )
            .column_spacing(1)
            .style(base_style(palette));
    let mut state = TableState::default()
        .with_offset(overlay.scroll)
        .with_selected((!overlay.jobs.is_empty()).then_some(selected_index));
    StatefulWidget::render(table, table_area, buffer, &mut state);
    let offset = state.offset();
    let viewport_rows = usize::from(table_area.height.saturating_sub(1)).max(1);
    let hit_rows = table_hit_rows(table_area, offset, viewport_rows, overlay.jobs.len());
    if overlay.jobs.is_empty() {
        let message = if overlay.unsupported {
            "This server does not support jobs yet. Update and restart it."
        } else if !overlay.loaded {
            "loading…"
        } else {
            "No jobs. Create ~/.config/flockr/jobs/<id>/job.toml (see the Jobs docs)."
        };
        put_text(
            buffer,
            table_area.x,
            table_area.y + 2,
            table_area.width,
            message,
            dim_style(palette),
        );
    }
    (hit_rows, viewport_rows, offset)
}

fn table_hit_rows(
    table_area: Rect,
    offset: usize,
    viewport_rows: usize,
    len: usize,
) -> Vec<(Rect, usize)> {
    (offset..len)
        .take(viewport_rows)
        .map(|index| {
            (
                Rect::new(
                    table_area.x,
                    table_area.y + 1 + (index - offset) as u16,
                    table_area.width,
                    1,
                ),
                index,
            )
        })
        .collect()
}

fn render_decisions_table(
    buffer: &mut Buffer,
    table_area: Rect,
    overlay: &ClientJobsOverlay,
    now_ms: u64,
    config: &ClientShellConfig,
) -> TableLayout {
    let palette = &config.palette;
    // Marker, job, age and an ask column that takes the rest.
    let wide = table_area.width >= 60;
    let job_width: u16 = if wide { 22 } else { 14 };
    let age_width: u16 = 5;
    let widths = vec![
        Constraint::Length(1),
        Constraint::Length(job_width),
        Constraint::Length(age_width),
        Constraint::Fill(1),
    ];
    let ask_width = table_area
        .width
        .saturating_sub(1 + job_width + age_width + 3)
        .max(1);
    let fit = |text: &str, width: u16| crate::ui::truncate_end(text, usize::from(width));
    let selected_index = overlay
        .decision_selected
        .as_ref()
        .and_then(|id| {
            overlay
                .decisions
                .iter()
                .position(|decision| decision.decision_id == *id)
        })
        .unwrap_or(0);
    let rows = overlay
        .decisions
        .iter()
        .enumerate()
        .map(|(index, decision)| {
            let is_selected = index == selected_index;
            let style_for = |style: Style| {
                if is_selected {
                    selected_style(palette)
                } else {
                    style
                }
            };
            let snoozed = decision.status == DecisionStatus::Snoozed;
            let (marker, color): (&str, ratatui::style::Color) = if decision.last_error.is_some() {
                ("●", palette.red)
            } else if snoozed {
                ("○", palette.overlay0)
            } else {
                ("●", palette.peach)
            };
            let ask = if snoozed {
                let left = decision
                    .snoozed_until_unix_ms
                    .map_or_else(String::new, |until| display::until(now_ms, until));
                format!("(snoozed {left}) {}", decision.ask)
            } else {
                decision.ask.clone()
            };
            let text_style = if snoozed {
                dim_style(palette)
            } else {
                base_style(palette)
            };
            Row::new(vec![
                Cell::from(Span::styled(
                    marker,
                    style_for(Style::default().fg(color).bg(palette.panel_bg)),
                )),
                Cell::from(Span::styled(
                    fit(&decision.job_id, job_width),
                    style_for(text_style),
                )),
                Cell::from(Span::styled(
                    fit(
                        &display::short_span(now_ms.saturating_sub(decision.created_unix_ms)),
                        age_width,
                    ),
                    style_for(dim_style(palette)),
                )),
                Cell::from(Span::styled(
                    fit(&sanitize_line(&ask), ask_width),
                    style_for(text_style),
                )),
            ])
            .style(if is_selected {
                selected_style(palette)
            } else {
                base_style(palette)
            })
        })
        .collect::<Vec<_>>();
    let table =
        Table::new(rows, widths)
            .header(
                Row::new(["", "job", "age", "ask"].map(|title| {
                    Cell::from(Span::styled(title.to_owned(), heading_style(palette)))
                }))
                .style(base_style(palette)),
            )
            .column_spacing(1)
            .style(base_style(palette));
    let mut state = TableState::default()
        .with_offset(overlay.decision_scroll)
        .with_selected((!overlay.decisions.is_empty()).then_some(selected_index));
    StatefulWidget::render(table, table_area, buffer, &mut state);
    let offset = state.offset();
    let viewport_rows = usize::from(table_area.height.saturating_sub(1)).max(1);
    let hit_rows = table_hit_rows(table_area, offset, viewport_rows, overlay.decisions.len());
    if overlay.decisions.is_empty() {
        let message = if overlay.decisions_unsupported {
            "This server does not support decisions yet. Update and restart it."
        } else if !overlay.decisions_loaded {
            "loading…"
        } else {
            "No decisions waiting. Jobs ask here when their [handoff] says so."
        };
        put_text(
            buffer,
            table_area.x,
            table_area.y + 2,
            table_area.width,
            message,
            dim_style(palette),
        );
    }
    (hit_rows, viewport_rows, offset)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{frame_rows, snapshot, surface};
    use super::*;
    use crate::api::schema::{JobEnvStatus, JobRunInfo, JobRunTrigger, JobScheduleKind, Request};
    use crate::config::Config;
    use crate::input::TerminalKey;
    use crate::raw_input::RawInputEvent;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

    fn run(status: JobRunStatus, summary: &str, duration_ms: u64) -> JobRunInfo {
        JobRunInfo {
            run_id: 3,
            trigger: JobRunTrigger::Schedule,
            status,
            started_unix_ms: crate::jobs::now_unix_ms() - 300_000,
            finished_unix_ms: crate::jobs::now_unix_ms() - 298_000,
            duration_ms,
            exit_code: Some(0),
            summary: summary.into(),
            error: None,
            skipped_count: 0,
            env_note: None,
        }
    }

    fn job(id: &str, state: JobState, last: Option<JobRunInfo>) -> JobInfo {
        JobInfo {
            job_id: id.into(),
            title: id.into(),
            source: "user".into(),
            command: vec!["echo".into()],
            schedule: "every 1h".into(),
            schedule_kind: JobScheduleKind::Interval,
            timeout_secs: 600,
            enabled: state != JobState::Disabled,
            enabled_overridden: false,
            schedule_overridden: false,
            env: "login-shell".into(),
            notify: "on_failure".into(),
            state,
            next_run_unix_ms: (state == JobState::Idle)
                .then(|| crate::jobs::now_unix_ms() + 600_000),
            running_since_unix_ms: None,
            last_run: last,
            has_feed: false,
            diagnostics: Vec::new(),
            dir: "/jobs".into(),
        }
    }

    fn sample_jobs() -> Vec<JobInfo> {
        let mut broken = job("broken", JobState::Invalid, None);
        broken.diagnostics = vec!["command: required".into()];
        let mut sync = job(
            "workbox-sync",
            JobState::Idle,
            Some(run(JobRunStatus::Success, "exit 0", 1_200)),
        );
        sync.title = "workbox sync".into();
        vec![
            sync,
            job(
                "nightly",
                JobState::Idle,
                Some(run(JobRunStatus::Failed, "exit 3", 4_100)),
            ),
            job("paused", JobState::Disabled, None),
            broken,
        ]
    }

    fn state() -> ClientShellState {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state.compose(120, 30).expect("frame");
        state
    }

    fn key(code: KeyCode) -> RawInputEvent {
        RawInputEvent::Key(TerminalKey::new(code, KeyModifiers::empty()))
    }

    fn press(state: &mut ClientShellState, ch: char) -> ClientShellInput {
        state.handle_raw_events(vec![key(KeyCode::Char(ch))])
    }

    fn open(state: &mut ClientShellState) {
        state.handle_raw_events(vec![
            RawInputEvent::Key(TerminalKey::new(KeyCode::Char('b'), KeyModifiers::CONTROL)),
            key(KeyCode::Char('t')),
        ]);
        assert!(matches!(state.overlay, Some(ClientShellOverlay::Jobs(_))));
    }

    fn requests(outcome: &ClientShellInput) -> Vec<Request> {
        outcome
            .actions
            .iter()
            .filter_map(|action| match action {
                ClientShellAction::Endpoint { request, .. } => Some((**request).clone()),
                _ => None,
            })
            .collect()
    }

    fn answer(
        state: &mut ClientShellState,
        request: &Request,
        result: ResponseResult,
    ) -> (bool, Vec<ClientShellAction>) {
        let boot = state.snapshot.as_deref().unwrap().boot_id.clone();
        state.handle_endpoint_result(&boot, &request.id, Ok(result))
    }

    fn list_result(jobs: Vec<JobInfo>, revision: u64) -> ResponseResult {
        ResponseResult::JobList {
            revision,
            unchanged: false,
            jobs,
            env: JobEnvStatus {
                source: "login-shell".into(),
                captured_unix_ms: None,
                variable_count: 3,
                refreshing: false,
                diagnostic: None,
            },
        }
    }

    fn open_with_jobs(state: &mut ClientShellState) {
        open(state);
        let mut outcome = ClientShellInput::default();
        assert!(state.tick_jobs(Instant::now(), &mut outcome));
        let request = requests(&outcome).remove(0);
        answer(state, &request, list_result(sample_jobs(), 7));
    }

    fn screen(state: &mut ClientShellState) -> String {
        frame_rows(&state.compose(120, 30).expect("frame").frame).join("\n")
    }

    fn selected(state: &ClientShellState) -> String {
        state.selected_job().unwrap().job_id
    }

    #[test]
    fn default_binding_is_free_and_opens_the_overlay() {
        let config = Config::default();
        assert!(config.collect_diagnostics().is_empty());
        assert!(matches!(
            crate::input::resolve_prefix_binding(
                &config.keybinds(),
                &TerminalKey::new(KeyCode::Char('t'), KeyModifiers::empty())
            ),
            Some(crate::input::KeybindMatch::Action(
                crate::input::KeybindAction::Jobs
            ))
        ));
        let mut state = state();
        open(&mut state);
        let outcome = state.handle_raw_events(vec![key(KeyCode::Esc)]);
        assert!(state.overlay.is_none() && outcome.repaint);
    }

    #[test]
    fn opening_asks_for_the_list_and_unchanged_answers_keep_it() {
        let mut state = state();
        open(&mut state);
        let mut outcome = ClientShellInput::default();
        state.tick_jobs(Instant::now(), &mut outcome);
        let sent = requests(&outcome);
        assert_eq!(sent.len(), 2, "the job list and the decision list");
        assert_eq!(
            sent[0].method,
            Method::JobList(JobListParams {
                since_revision: None
            })
        );
        assert_eq!(
            sent[1].method,
            Method::DecisionList(DecisionListParams::default())
        );
        // Not asked again while the answer is outstanding.
        let mut again = ClientShellInput::default();
        state.tick_jobs(Instant::now() + Duration::from_secs(5), &mut again);
        assert!(requests(&again).is_empty());

        answer(&mut state, &sent[0], list_result(sample_jobs(), 7));
        let mut next = ClientShellInput::default();
        state.tick_jobs(Instant::now() + Duration::from_secs(5), &mut next);
        let sent = requests(&next);
        assert_eq!(
            sent[0].method,
            Method::JobList(JobListParams {
                since_revision: Some(7)
            }),
            "polls with the revision it holds"
        );
        answer(
            &mut state,
            &sent[0],
            ResponseResult::JobList {
                revision: 7,
                unchanged: true,
                jobs: Vec::new(),
                env: JobEnvStatus {
                    source: "none".into(),
                    captured_unix_ms: None,
                    variable_count: 0,
                    refreshing: false,
                    diagnostic: None,
                },
            },
        );
        assert_eq!(state.jobs_overlay().unwrap().jobs.len(), 4);
    }

    #[test]
    fn table_shows_state_schedule_next_run_and_last_result() {
        let mut state = state();
        open_with_jobs(&mut state);
        let screen = screen(&mut state);
        for expected in [
            "jobs",
            "workbox-sync · workbox sync",
            "every 1h",
            "in 9m",
            "ok 1.2s",
            "nightly",
            "failed (exit 3) 4.1s",
            "paused",
            "off",
            "never run",
            "broken",
            "command: required",
            "r run   l logs",
        ] {
            assert!(
                screen.contains(expected),
                "missing {expected:?} in\n{screen}"
            );
        }
    }

    #[test]
    fn narrow_terminals_keep_the_name_and_result() {
        let mut state = state();
        open_with_jobs(&mut state);
        let rows = frame_rows(&state.compose(48, 20).expect("frame").frame).join("\n");
        assert!(rows.contains("nightly"), "{rows}");
        assert!(rows.contains("failed"), "{rows}");
    }

    #[test]
    fn selection_moves_and_survives_a_refresh() {
        let mut state = state();
        open_with_jobs(&mut state);
        assert_eq!(selected(&state), "workbox-sync");
        press(&mut state, 'j');
        assert_eq!(selected(&state), "nightly");
        state.handle_raw_events(vec![key(KeyCode::Down)]);
        assert_eq!(selected(&state), "paused");
        press(&mut state, 'G');
        assert_eq!(selected(&state), "broken");
        press(&mut state, 'g');
        assert_eq!(selected(&state), "workbox-sync");
        press(&mut state, 'j');
        // A refresh in a different order keeps the same job selected.
        let mut jobs = sample_jobs();
        jobs.reverse();
        let mut outcome = ClientShellInput::default();
        state.tick_jobs(Instant::now() + Duration::from_secs(5), &mut outcome);
        let request = requests(&outcome).remove(0);
        answer(&mut state, &request, list_result(jobs, 8));
        assert_eq!(selected(&state), "nightly");
    }

    #[test]
    fn r_runs_the_selected_job_and_reports_the_answer() {
        let mut state = state();
        open_with_jobs(&mut state);
        press(&mut state, 'j');
        let outcome = press(&mut state, 'r');
        let sent = requests(&outcome);
        assert_eq!(
            sent[0].method,
            Method::JobRun(JobTarget {
                job_id: "nightly".into()
            })
        );
        answer(
            &mut state,
            &sent[0],
            ResponseResult::JobRunRequested {
                job_id: "nightly".into(),
                started: true,
                run_id: Some(9),
                reason: None,
            },
        );
        assert!(screen(&mut state).contains("started nightly"));
        // A refresh follows at once.
        let mut next = ClientShellInput::default();
        state.tick_jobs(Instant::now(), &mut next);
        assert_eq!(requests(&next).len(), 1);
    }

    #[test]
    fn a_run_refused_because_the_job_is_running_says_so() {
        let mut state = state();
        open_with_jobs(&mut state);
        let outcome = press(&mut state, 'r');
        let sent = requests(&outcome);
        answer(
            &mut state,
            &sent[0],
            ResponseResult::JobRunRequested {
                job_id: "workbox-sync".into(),
                started: false,
                run_id: None,
                reason: Some("skipped: still running".into()),
            },
        );
        assert!(screen(&mut state).contains("workbox-sync: skipped: still running"));
    }

    #[test]
    fn e_toggles_enabled_and_invalid_jobs_explain_instead() {
        let mut state = state();
        open_with_jobs(&mut state);
        let outcome = press(&mut state, 'e');
        let sent = requests(&outcome);
        assert_eq!(
            sent[0].method,
            Method::JobSetEnabled(JobSetEnabledParams {
                job_id: "workbox-sync".into(),
                enabled: false
            })
        );
        let mut updated = sample_jobs().remove(0);
        updated.state = JobState::Disabled;
        updated.enabled = false;
        answer(
            &mut state,
            &sent[0],
            ResponseResult::JobUpdated { job: updated },
        );
        assert!(screen(&mut state).contains("workbox-sync disabled"));
        // The disabled paused job turns back on.
        press(&mut state, 'j');
        press(&mut state, 'j');
        assert_eq!(selected(&state), "paused");
        let sent = requests(&press(&mut state, 'e'));
        assert_eq!(
            sent[0].method,
            Method::JobSetEnabled(JobSetEnabledParams {
                job_id: "paused".into(),
                enabled: true
            })
        );
        press(&mut state, 'j');
        let outcome = press(&mut state, 'e');
        assert!(requests(&outcome).is_empty());
        assert!(screen(&mut state).contains("fix the job file first"));
    }

    #[test]
    fn l_opens_scrollable_output_and_esc_returns_then_closes() {
        let mut state = state();
        open_with_jobs(&mut state);
        let outcome = press(&mut state, 'l');
        let sent = requests(&outcome);
        assert!(
            matches!(&sent[0].method, Method::JobLogs(params) if params.job_id == "workbox-sync")
        );
        assert!(screen(&mut state).contains("loading"));
        let stdout = (0..100).map(|n| format!("line {n}\n")).collect::<String>();
        answer(
            &mut state,
            &sent[0],
            ResponseResult::JobLogs {
                job_id: "workbox-sync".into(),
                run: run(JobRunStatus::Success, "exit 0", 1_200),
                stdout,
                stderr: "warn \x1b[31mred\x1b[0m\ttext\n".into(),
                stdout_truncated: false,
                stderr_truncated: false,
            },
        );
        let at_end = screen(&mut state);
        assert!(at_end.contains("job output · workbox sync"), "{at_end}");
        assert!(at_end.contains("run #3 · ok · exit 0"), "{at_end}");
        assert!(at_end.contains("── stderr ──"));
        assert!(
            at_end.contains("warn red    text"),
            "escapes stripped: {at_end}"
        );
        assert!(!at_end.contains('\u{1b}'));
        assert!(
            !at_end.contains("line 0\n"),
            "opens at the end of the output"
        );
        press(&mut state, 'g');
        let top = screen(&mut state);
        assert!(
            top.contains("── stdout ──") && top.contains("line 0"),
            "{top}"
        );
        for _ in 0..3 {
            press(&mut state, 'j');
        }
        assert!(screen(&mut state).contains("line 3"));
        // No polling while reading output.
        let mut idle = ClientShellInput::default();
        state.tick_jobs(Instant::now() + Duration::from_secs(10), &mut idle);
        assert!(requests(&idle).is_empty());
        state.handle_raw_events(vec![key(KeyCode::Esc)]);
        assert!(matches!(state.overlay, Some(ClientShellOverlay::Jobs(_))));
        assert!(screen(&mut state).contains("r run   l logs"));
        state.handle_raw_events(vec![key(KeyCode::Esc)]);
        assert!(state.overlay.is_none());
    }

    #[test]
    fn jobs_that_never_ran_have_no_output_to_open() {
        let mut state = state();
        open_with_jobs(&mut state);
        press(&mut state, 'G');
        press(&mut state, 'k');
        assert_eq!(selected(&state), "paused");
        let outcome = press(&mut state, 'l');
        assert!(requests(&outcome).is_empty());
        assert!(screen(&mut state).contains("paused has not run yet"));
    }

    #[test]
    fn clicking_selects_and_clicking_outside_the_panel_closes() {
        let mut state = state();
        open_with_jobs(&mut state);
        screen(&mut state);
        let (rect, index) = state.hits.jobs.rows[1];
        assert_eq!(index, 1);
        state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x + 2,
            row: rect.y,
            modifiers: KeyModifiers::empty(),
        })]);
        assert_eq!(selected(&state), "nightly");
        state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: rect.x + 2,
            row: rect.y,
            modifiers: KeyModifiers::empty(),
        })]);
        assert_eq!(selected(&state), "broken");
    }

    #[test]
    fn a_server_without_job_methods_gets_an_explanation_not_an_error() {
        let mut state = state();
        state.set_endpoint_methods(Some(vec!["pane.read".into()]));
        open(&mut state);
        let mut outcome = ClientShellInput::default();
        state.tick_jobs(Instant::now(), &mut outcome);
        assert!(requests(&outcome).is_empty());
        let screen = screen(&mut state);
        assert!(
            screen.contains("This server does not support jobs yet"),
            "{screen}"
        );
        assert!(state.endpoint_notice_seen.is_empty());
    }

    #[test]
    fn palette_lists_run_job_entries_once_the_names_are_known() {
        let mut state = state();
        assert!(state.palette_items().iter().all(|item| item.kind != "job"));
        // Opening the palette refreshes the job names in the background.
        state.handle_raw_events(vec![
            RawInputEvent::Key(TerminalKey::new(KeyCode::Char('b'), KeyModifiers::CONTROL)),
            key(KeyCode::Char(':')),
        ]);
        let mut outcome = ClientShellInput::default();
        state.tick_jobs(Instant::now(), &mut outcome);
        let request = requests(&outcome).remove(0);
        answer(&mut state, &request, list_result(sample_jobs(), 1));
        let titles: Vec<_> = state
            .palette_items()
            .into_iter()
            .filter(|item| item.kind == "job")
            .map(|item| item.title)
            .collect();
        assert_eq!(
            titles,
            ["run job workbox-sync", "run job nightly", "run job paused"],
            "invalid jobs are not offered"
        );
        assert!(
            state
                .palette_items()
                .iter()
                .any(|item| item.title == "jobs"),
            "open-overlay entry comes from the binding table"
        );
        // Running one sends job.run with no overlay open.
        let mut outcome = ClientShellInput::default();
        state.run_palette_action(
            super::super::palette::PaletteAction::RunJob("nightly".into()),
            &mut outcome,
        );
        assert!(matches!(
            &requests(&outcome)[0].method,
            Method::JobRun(target) if target.job_id == "nightly"
        ));
    }

    #[test]
    fn output_escape_sequences_are_removed() {
        assert_eq!(sanitize_line("a\u{1b}[1;31mb\u{1b}[0mc"), "abc");
        assert_eq!(sanitize_line("x\u{1b}]0;title\u{7}y"), "xy");
        assert_eq!(sanitize_line("tab\there"), "tab    here");
        assert_eq!(sanitize_line("bell\u{7}\r"), "bell");
        assert_eq!(sanitize_line("日本語"), "日本語");
    }

    fn decision(id: &str, job: &str, ask: &str, status: DecisionStatus) -> DecisionInfo {
        DecisionInfo {
            decision_id: id.into(),
            job_id: job.into(),
            job_title: job.into(),
            run_id: 3,
            ask: ask.into(),
            tab_title: "workbox · today".into(),
            cwd: "/work".into(),
            agent_command: "claude".into(),
            status,
            created_unix_ms: crate::jobs::now_unix_ms() - 7_200_000,
            snoozed_until_unix_ms: (status == DecisionStatus::Snoozed)
                .then(|| crate::jobs::now_unix_ms() + 1_800_000),
            resolved_unix_ms: None,
            outcome: None,
            last_error: None,
            prompt_file: Some("/state/handoffs/3.md".into()),
            context: None,
            prompt: None,
        }
    }

    fn sample_decisions() -> Vec<DecisionInfo> {
        vec![
            decision(
                "d3",
                "workbox-sync",
                "3 items need you. Start Claude?",
                DecisionStatus::Pending,
            ),
            decision(
                "d2",
                "nightly",
                "Nightly failed. Look?",
                DecisionStatus::Pending,
            ),
            decision("d1", "paused", "Older question", DecisionStatus::Snoozed),
        ]
    }

    fn decision_list_result(decisions: Vec<DecisionInfo>, revision: u64) -> ResponseResult {
        let pending = decisions
            .iter()
            .filter(|decision| decision.status == DecisionStatus::Pending)
            .count() as u32;
        ResponseResult::DecisionList {
            revision,
            unchanged: false,
            pending,
            decisions,
        }
    }

    /// Open the overlay with jobs and decisions loaded, on the Jobs tab.
    fn open_loaded(state: &mut ClientShellState) {
        open(state);
        let mut outcome = ClientShellInput::default();
        state.tick_jobs(Instant::now(), &mut outcome);
        let sent = requests(&outcome);
        answer(state, &sent[0], list_result(sample_jobs(), 7));
        answer(state, &sent[1], decision_list_result(sample_decisions(), 4));
    }

    /// The same, on the Decisions tab.
    fn open_with_decisions(state: &mut ClientShellState) {
        open_loaded(state);
        press_tab(state);
        assert_eq!(state.jobs_overlay().unwrap().tab, JobsTab::Decisions);
    }

    fn press_tab(state: &mut ClientShellState) {
        state.handle_raw_events(vec![key(KeyCode::Tab)]);
    }

    fn selected_decision_id(state: &ClientShellState) -> String {
        state.selected_decision().unwrap().decision_id
    }

    #[test]
    fn the_title_carries_the_decision_badge_and_tab_switches_lists() {
        let mut state = state();
        open(&mut state);
        let before = screen(&mut state);
        assert!(before.contains("[jobs]  decisions"), "{before}");
        assert!(
            !before.contains("decisions ("),
            "no badge before the list arrives"
        );
        // The lists arrive: two decisions pending, one snoozed.
        let mut outcome = ClientShellInput::default();
        state.tick_jobs(Instant::now(), &mut outcome);
        let sent = requests(&outcome);
        answer(&mut state, &sent[0], list_result(sample_jobs(), 7));
        answer(
            &mut state,
            &sent[1],
            decision_list_result(sample_decisions(), 4),
        );
        let jobs_tab = screen(&mut state);
        assert!(jobs_tab.contains("[jobs]  decisions (2)"), "{jobs_tab}");
        assert!(
            jobs_tab.contains("workbox-sync · workbox sync ●1"),
            "per-job badge: {jobs_tab}"
        );
        assert!(jobs_tab.contains("nightly ●1"), "{jobs_tab}");
        assert!(
            !jobs_tab.contains("paused ●"),
            "snoozed decisions are not badged: {jobs_tab}"
        );
        press_tab(&mut state);
        let decisions_tab = screen(&mut state);
        assert!(
            decisions_tab.contains("jobs  [decisions (2)]"),
            "{decisions_tab}"
        );
        for expected in [
            "workbox-sync",
            "3 items need you. Start Claude?",
            "nightly",
            "Nightly failed. Look?",
            "2h",
            "(snoozed in 29m) Older question",
            "↵/s start",
            "z snooze 1h",
            "x dismiss",
            "p preview",
        ] {
            assert!(
                decisions_tab.contains(expected),
                "missing {expected:?} in\n{decisions_tab}"
            );
        }
        press_tab(&mut state);
        assert_eq!(state.jobs_overlay().unwrap().tab, JobsTab::Jobs);
        assert!(screen(&mut state).contains("r run   l logs"));
    }

    #[test]
    fn enter_and_s_start_the_selected_decision() {
        for starter in ['s', '\n'] {
            let mut state = state();
            open_with_decisions(&mut state);
            assert_eq!(selected_decision_id(&state), "d3");
            press(&mut state, 'j');
            assert_eq!(selected_decision_id(&state), "d2");
            let outcome = if starter == '\n' {
                state.handle_raw_events(vec![key(KeyCode::Enter)])
            } else {
                press(&mut state, starter)
            };
            let sent = requests(&outcome);
            assert_eq!(
                sent[0].method,
                Method::DecisionStart(DecisionTarget {
                    decision_id: "d2".into()
                })
            );
            assert!(screen(&mut state).contains("starting d2"));
            // Success leaves the overlay so the new tab is in view.
            answer(
                &mut state,
                &sent[0],
                ResponseResult::DecisionStarted {
                    decision: decision("d2", "nightly", "x", DecisionStatus::Started),
                    tab_id: "w1:t2".into(),
                    pane_id: "w1:t2:p1".into(),
                },
            );
            assert!(state.overlay.is_none());
        }
    }

    #[test]
    fn a_failed_start_keeps_the_overlay_and_says_why() {
        let mut state = state();
        open_with_decisions(&mut state);
        let outcome = press(&mut state, 's');
        let sent = requests(&outcome);
        let boot = state.snapshot.as_deref().unwrap().boot_id.clone();
        state.handle_endpoint_result(
            &boot,
            &sent[0].id,
            Err(ClientShellEndpointError {
                code: None,
                message: "the working directory /work does not exist".into(),
            }),
        );
        let text = screen(&mut state);
        assert!(
            text.contains("the working directory /work does not exist"),
            "{text}"
        );
        assert!(matches!(state.overlay, Some(ClientShellOverlay::Jobs(_))));
        // The next poll picks up the decision's recorded error.
        let mut next = ClientShellInput::default();
        state.tick_jobs(Instant::now() + Duration::from_secs(5), &mut next);
        assert!(requests(&next)
            .iter()
            .any(|request| request.method == Method::DecisionList(DecisionListParams::default())));
    }

    #[test]
    fn z_snoozes_for_an_hour_and_x_dismisses() {
        let mut state = state();
        open_with_decisions(&mut state);
        let outcome = press(&mut state, 'z');
        let sent = requests(&outcome);
        assert_eq!(
            sent[0].method,
            Method::DecisionSnooze(DecisionSnoozeParams {
                decision_id: "d3".into(),
                for_secs: Some(3600)
            })
        );
        answer(
            &mut state,
            &sent[0],
            ResponseResult::DecisionUpdated {
                decision: decision("d3", "workbox-sync", "x", DecisionStatus::Snoozed),
            },
        );
        assert!(screen(&mut state).contains("d3 snoozed for 1h"));

        let outcome = press(&mut state, 'x');
        let sent = requests(&outcome);
        assert_eq!(
            sent[0].method,
            Method::DecisionDismiss(DecisionTarget {
                decision_id: "d3".into()
            })
        );
        answer(
            &mut state,
            &sent[0],
            ResponseResult::DecisionUpdated {
                decision: decision("d3", "workbox-sync", "x", DecisionStatus::Dismissed),
            },
        );
        assert!(screen(&mut state).contains("d3 dismissed"));
        // Both asked for a fresh list right after.
        let mut next = ClientShellInput::default();
        state.tick_jobs(Instant::now(), &mut next);
        assert!(requests(&next)
            .iter()
            .any(|request| matches!(request.method, Method::DecisionList(_))));
    }

    #[test]
    fn p_previews_the_rendered_prompt_and_esc_comes_back() {
        let mut state = state();
        open_with_decisions(&mut state);
        let outcome = press(&mut state, 'p');
        let sent = requests(&outcome);
        assert_eq!(
            sent[0].method,
            Method::DecisionList(DecisionListParams {
                since_revision: None,
                include_resolved: true,
                decision_id: Some("d3".into()),
                include_prompt: true,
            })
        );
        assert!(screen(&mut state).contains("loading"));
        let mut with_prompt = decision("d3", "workbox-sync", "x", DecisionStatus::Pending);
        with_prompt.prompt = Some("Triage these\n\u{1b}[31m- item one\n- item two\n".into());
        answer(
            &mut state,
            &sent[0],
            decision_list_result(vec![with_prompt], 4),
        );
        let text = screen(&mut state);
        for expected in [
            "decision prompt · d3 · workbox-sync",
            "agent  claude",
            "cwd    /work",
            "tab    workbox · today",
            "── prompt ──",
            "Triage these",
            "- item one",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in\n{text}");
        }
        assert!(!text.contains('\u{1b}'));
        // No polling while reading, and the selected decision is unchanged.
        let mut idle = ClientShellInput::default();
        state.tick_jobs(Instant::now() + Duration::from_secs(10), &mut idle);
        assert!(requests(&idle).is_empty());
        state.handle_raw_events(vec![key(KeyCode::Esc)]);
        assert!(screen(&mut state).contains("↵/s start"));
        assert_eq!(selected_decision_id(&state), "d3");
        state.handle_raw_events(vec![key(KeyCode::Esc)]);
        assert!(state.overlay.is_none());
    }

    #[test]
    fn a_decision_without_a_prompt_previews_its_command() {
        let mut state = state();
        open_with_decisions(&mut state);
        let outcome = press(&mut state, 'p');
        let sent = requests(&outcome);
        let mut bare = decision("d3", "workbox-sync", "x", DecisionStatus::Pending);
        bare.prompt_file = None;
        answer(&mut state, &sent[0], decision_list_result(vec![bare], 4));
        assert!(screen(&mut state).contains("no prompt: Start runs the agent command as it is"));
    }

    #[test]
    fn decision_keys_do_nothing_on_the_jobs_tab_and_job_keys_not_on_decisions() {
        let mut state = state();
        open_with_decisions(&mut state);
        // `r` is a job key.
        let outcome = press(&mut state, 'r');
        assert!(requests(&outcome).is_empty());
        press_tab(&mut state);
        // `z` and `s` are decision keys.
        let outcome = press(&mut state, 'z');
        assert!(requests(&outcome).is_empty());
        let outcome = press(&mut state, 's');
        assert!(requests(&outcome).is_empty());
    }

    #[test]
    fn an_empty_decision_list_explains_itself() {
        let mut state = state();
        open(&mut state);
        let mut outcome = ClientShellInput::default();
        state.tick_jobs(Instant::now(), &mut outcome);
        let sent = requests(&outcome);
        answer(&mut state, &sent[1], decision_list_result(Vec::new(), 1));
        press_tab(&mut state);
        assert!(screen(&mut state).contains("No decisions waiting"));
        // Starting nothing sends nothing.
        assert!(requests(&press(&mut state, 's')).is_empty());
    }

    #[test]
    fn a_server_without_decision_methods_disables_only_the_decisions_tab() {
        let mut state = state();
        state.set_endpoint_methods(Some(
            [
                "job.list",
                "job.get",
                "job.run",
                "job.logs",
                "job.set_enabled",
            ]
            .map(String::from)
            .to_vec(),
        ));
        open(&mut state);
        let mut outcome = ClientShellInput::default();
        state.tick_jobs(Instant::now(), &mut outcome);
        let sent = requests(&outcome);
        assert_eq!(sent.len(), 1, "only the job list is asked: {sent:?}");
        answer(&mut state, &sent[0], list_result(sample_jobs(), 1));
        assert!(screen(&mut state).contains("workbox-sync"));
        press_tab(&mut state);
        assert!(screen(&mut state).contains("This server does not support decisions yet"));
        assert!(state.endpoint_notice_seen.is_empty());
        // And `t` says the server cannot change schedules.
        press_tab(&mut state);
        press(&mut state, 't');
        let mut outcome = ClientShellInput::default();
        let entered = state.handle_raw_events(vec![key(KeyCode::Enter)]);
        outcome.actions.extend(entered.actions);
        assert!(screen(&mut state).contains("cannot change schedules yet"));
        // The palette leaves the entry out.
        assert!(!state
            .palette_items()
            .iter()
            .any(|item| item.kind == "decision"));
    }

    #[test]
    fn clicking_a_decision_row_selects_it() {
        let mut state = state();
        open_with_decisions(&mut state);
        screen(&mut state);
        let (rect, index) = state.hits.jobs.rows[1];
        assert_eq!(index, 1);
        state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x + 2,
            row: rect.y,
            modifiers: KeyModifiers::empty(),
        })]);
        assert_eq!(selected_decision_id(&state), "d2");
    }

    #[test]
    fn the_palette_offers_decisions_with_their_count() {
        let mut state = state();
        state.handle_raw_events(vec![
            RawInputEvent::Key(TerminalKey::new(KeyCode::Char('b'), KeyModifiers::CONTROL)),
            key(KeyCode::Char(':')),
        ]);
        let mut outcome = ClientShellInput::default();
        state.tick_jobs(Instant::now(), &mut outcome);
        let sent = requests(&outcome);
        let list = sent
            .iter()
            .find(|request| matches!(request.method, Method::DecisionList(_)))
            .expect("the palette refreshes the decision count");
        answer(
            &mut state,
            list,
            decision_list_result(sample_decisions(), 4),
        );
        let item = state
            .palette_items()
            .into_iter()
            .find(|item| item.kind == "decision")
            .expect("decisions entry");
        assert_eq!(item.title, "decisions (2)");
        let mut outcome = ClientShellInput::default();
        state.run_palette_action(item.action, &mut outcome);
        assert_eq!(state.jobs_overlay().unwrap().tab, JobsTab::Decisions);
        assert_eq!(state.jobs_overlay().unwrap().pending_decisions, 2);
    }

    fn type_text(state: &mut ClientShellState, text: &str) {
        for ch in text.chars() {
            press(state, ch);
        }
    }

    #[test]
    fn t_opens_the_schedule_prompt_with_live_validation_and_saves() {
        let mut state = state();
        open_with_jobs(&mut state);
        press(&mut state, 't');
        let text = screen(&mut state);
        assert!(
            text.contains("schedule for workbox-sync: every 1h"),
            "{text}"
        );
        assert!(text.contains("✓ runs every 1h"), "{text}");
        // Typing replaces the prefilled text; an invalid value says why.
        type_text(&mut state, "every 5s");
        let bad = screen(&mut state);
        assert!(bad.contains("✗") && bad.contains("1m minimum"), "{bad}");
        let outcome = state.handle_raw_events(vec![key(KeyCode::Enter)]);
        assert!(
            requests(&outcome).is_empty(),
            "an invalid schedule is not sent"
        );
        assert!(
            state.jobs_overlay().unwrap().schedule_edit.is_some(),
            "the prompt stays open"
        );
        // Fix it.
        for _ in 0.."every 5s".len() {
            state.handle_raw_events(vec![key(KeyCode::Backspace)]);
        }
        type_text(&mut state, "every 2h");
        assert!(screen(&mut state).contains("✓ runs every 2h"));
        let outcome = state.handle_raw_events(vec![key(KeyCode::Enter)]);
        let sent = requests(&outcome);
        assert_eq!(
            sent[0].method,
            Method::JobSetSchedule(JobSetScheduleParams {
                job_id: "workbox-sync".into(),
                schedule: "every 2h".into()
            })
        );
        assert!(state.jobs_overlay().unwrap().schedule_edit.is_none());
        let mut updated = sample_jobs().remove(0);
        updated.schedule = "every 2h".into();
        answer(
            &mut state,
            &sent[0],
            ResponseResult::JobScheduleUpdated {
                job: updated,
                stored_in: JobScheduleStore::JobFile,
                note: None,
            },
        );
        let text = screen(&mut state);
        assert!(
            text.contains("workbox-sync now runs every 2h (saved in job.toml)"),
            "{text}"
        );
        assert!(text.contains("every 2h"));
    }

    #[test]
    fn the_schedule_prompt_takes_cron_and_cancels_with_esc() {
        let mut state = state();
        open_with_jobs(&mut state);
        press(&mut state, 't');
        // Ctrl+C clears the prefilled text.
        state.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        ))]);
        type_text(&mut state, "0 9 * * 1-5");
        assert!(screen(&mut state).contains("✓ runs 0 9 * * 1-5"));
        state.handle_raw_events(vec![key(KeyCode::Esc)]);
        assert!(state.jobs_overlay().unwrap().schedule_edit.is_none());
        assert!(
            matches!(state.overlay, Some(ClientShellOverlay::Jobs(_))),
            "Esc closes only the prompt"
        );
        // Keys typed in the prompt never act on the list.
        press(&mut state, 't');
        let outcome = press(&mut state, 'r');
        assert!(requests(&outcome).is_empty());
    }

    #[test]
    fn t_on_an_invalid_job_explains_instead_and_failures_show() {
        let mut state = state();
        open_with_jobs(&mut state);
        press(&mut state, 'G');
        assert_eq!(selected(&state), "broken");
        press(&mut state, 't');
        assert!(state.jobs_overlay().unwrap().schedule_edit.is_none());
        assert!(screen(&mut state).contains("fix the job file first"));
        // A refused save reports the server's message.
        press(&mut state, 'g');
        press(&mut state, 't');
        let outcome = state.handle_raw_events(vec![key(KeyCode::Enter)]);
        let sent = requests(&outcome);
        let boot = state.snapshot.as_deref().unwrap().boot_id.clone();
        state.handle_endpoint_result(
            &boot,
            &sent[0].id,
            Err(ClientShellEndpointError {
                code: None,
                message: "could not write job.toml".into(),
            }),
        );
        assert!(screen(&mut state).contains("could not write job.toml"));
    }

    #[test]
    fn the_schedule_prompt_works_on_system_jobs() {
        let mut state = state();
        open(&mut state);
        let mut outcome = ClientShellInput::default();
        state.tick_jobs(Instant::now(), &mut outcome);
        let sent = requests(&outcome);
        let mut system = job("system.upstream-herdr", JobState::Idle, None);
        system.source = "system".into();
        system.title = "upstream herdr watcher".into();
        system.schedule = "every 1d".into();
        answer(&mut state, &sent[0], list_result(vec![system], 1));
        assert!(screen(&mut state).contains("system.upstream-herdr"));
        press(&mut state, 't');
        assert!(screen(&mut state).contains("schedule for system.upstream-herdr: every 1d"));
    }
}
