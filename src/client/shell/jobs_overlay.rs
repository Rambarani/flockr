//! Jobs overlay: a full-screen table of the server's scheduled jobs (state,
//! schedule, next run, last result), with `r` run now, `l` or Enter for the
//! last run's output, `e` enable or disable, and Esc to close.
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
use super::*;
use crate::api::schema::{
    JobInfo, JobListParams, JobLogsParams, JobRunStatus, JobSetEnabledParams, JobState, JobTarget,
    Method, ResponseResult,
};
use crate::jobs::display;

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
    Logs { job_id: String },
    Run { job_id: String, from_palette: bool },
    Toggle,
}

/// Client-side memory of the jobs the active server reported, for the
/// palette's "run job" entries.
#[derive(Debug, Default)]
pub(super) struct JobsClientState {
    pub(super) cache: Vec<(String, String)>,
    pub(super) cache_endpoint: Option<ClientEndpointId>,
    cache_at: Option<Instant>,
    /// The server rejected or does not know `job.list`.
    unsupported: bool,
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

fn row_name(job: &JobInfo) -> String {
    if job.title == job.job_id || job.title.is_empty() {
        job.job_id.clone()
    } else {
        format!("{} · {}", job.job_id, job.title)
    }
}

impl ClientShellState {
    pub(super) fn open_jobs_overlay(&mut self) {
        self.overlay = Some(ClientShellOverlay::Jobs(ClientJobsOverlay {
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
        }));
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

    fn move_job_selection(&mut self, delta: isize) {
        let viewport = self.hits.jobs.viewport_rows.max(1);
        let offset = self.hits.jobs.offset;
        let Some(overlay) = self.jobs_overlay_mut() else {
            return;
        };
        if overlay.jobs.is_empty() {
            return;
        }
        let current = Self::selected_job_index(overlay);
        let next = (current as isize + delta).clamp(0, overlay.jobs.len() as isize - 1) as usize;
        overlay.selected = Some(overlay.jobs[next].job_id.clone());
        overlay.scroll = if next < offset {
            next
        } else if next >= offset + viewport {
            next + 1 - viewport
        } else {
            offset
        };
    }

    fn scroll_job_logs(&mut self, delta: isize) {
        let viewport = self.hits.jobs.viewport_rows.max(1);
        let Some(logs) = self
            .jobs_overlay_mut()
            .and_then(|overlay| overlay.logs.as_mut())
        else {
            return;
        };
        let max = logs.lines.len().saturating_sub(viewport) as isize;
        logs.scroll = (logs.scroll as isize + delta).clamp(0, max.max(0)) as usize;
    }

    /// Ask the server for something; a server without the method turns the
    /// overlay into an explanation instead of raising a notice.
    fn push_jobs_request(
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
        let in_logs = overlay.logs.is_some();
        let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
        let plain = modifiers.is_empty();
        let ctrl = modifiers == KeyModifiers::CONTROL;
        let page = self.hits.jobs.viewport_rows.max(1) as isize;
        outcome.repaint = true;
        let typed = {
            let shifted_without_evidence = key.modifiers.contains(KeyModifiers::SHIFT)
                && key.generated_text.is_none()
                && key.shifted_codepoint.is_none();
            (!shifted_without_evidence)
                .then(|| crate::input::keybind_help_text_char(key))
                .flatten()
        };
        if in_logs {
            match code {
                KeyCode::Esc => {
                    if let Some(overlay) = self.jobs_overlay_mut() {
                        overlay.logs = None;
                    }
                }
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
                    Some('q' | 'l' | 'h') => {
                        if let Some(overlay) = self.jobs_overlay_mut() {
                            overlay.logs = None;
                        }
                    }
                    _ => outcome.repaint = false,
                },
            }
            return true;
        }
        match code {
            KeyCode::Esc => self.overlay = None,
            KeyCode::Enter if plain => self.open_selected_job_logs(outcome),
            KeyCode::Up if plain => self.move_job_selection(-1),
            KeyCode::Down if plain => self.move_job_selection(1),
            KeyCode::Char('p') if ctrl => self.move_job_selection(-1),
            KeyCode::Char('n') if ctrl => self.move_job_selection(1),
            KeyCode::PageUp if plain => self.move_job_selection(-page),
            KeyCode::PageDown if plain => self.move_job_selection(page),
            KeyCode::Home if plain => self.move_job_selection(isize::MIN / 2),
            KeyCode::End if plain => self.move_job_selection(isize::MAX / 2),
            _ => match typed {
                Some('q') => self.overlay = None,
                Some('r') => self.run_selected_job(outcome),
                Some('l') => self.open_selected_job_logs(outcome),
                Some('e') => self.toggle_selected_job(outcome),
                Some('j') => self.move_job_selection(1),
                Some('k') => self.move_job_selection(-1),
                Some('g') => self.move_job_selection(isize::MIN / 2),
                Some('G') => self.move_job_selection(isize::MAX / 2),
                _ => outcome.repaint = false,
            },
        }
        true
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
        let in_logs = overlay.logs.is_some();
        let point = (mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if !contains(self.hits.jobs.popup, point) {
                    if in_logs {
                        if let Some(overlay) = self.jobs_overlay_mut() {
                            overlay.logs = None;
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
                            if let Some(job) = overlay.jobs.get(index) {
                                overlay.selected = Some(job.job_id.clone());
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
                && overlay
                    .last_poll
                    .is_none_or(|at| now.saturating_duration_since(at) >= POLL_GAP)
                && !self.jobs_list_in_flight();
            let revision = overlay.revision;
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
    let title = match &overlay.logs {
        Some(logs) => format!("job output · {}", logs.title),
        None => "jobs".to_owned(),
    };
    let inner = draw_panel(buffer, area, &title, palette)?;
    let empty = JobsRender {
        area,
        rows: Vec::new(),
        viewport_rows: 1,
        offset: 0,
    };
    if inner.height < 4 || inner.width < 20 {
        return Some(empty);
    }
    let hint_y = inner.bottom() - 1;
    let status_y = inner.bottom().saturating_sub(2);
    let text_width = inner.width.saturating_sub(2);

    if let Some(logs) = &overlay.logs {
        put_text(
            buffer,
            inner.x + 1,
            inner.y,
            text_width,
            &crate::ui::truncate_end(&logs.header, usize::from(text_width)),
            dim_style(palette),
        );
        let body = Rect::new(
            inner.x + 1,
            inner.y + 1,
            text_width,
            inner.height.saturating_sub(3),
        );
        let viewport = usize::from(body.height).max(1);
        let max_scroll = logs.lines.len().saturating_sub(viewport);
        let offset = logs.scroll.min(max_scroll);
        if logs.loading {
            put_text(
                buffer,
                body.x,
                body.y,
                body.width,
                "loading…",
                dim_style(palette),
            );
        }
        for (row, line) in logs.lines.iter().skip(offset).take(viewport).enumerate() {
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
        if logs.lines.len() > viewport {
            let position = format!(
                "{}-{} / {}",
                offset + 1,
                (offset + viewport).min(logs.lines.len()),
                logs.lines.len()
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
            let mut cells = vec![
                Cell::from(Span::styled(marker, marker_style)),
                Cell::from(Span::styled(
                    fit(&row_name(job), columns.name),
                    style_for(plain),
                )),
            ];
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
    let hit_rows = (offset..overlay.jobs.len())
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
        .collect::<Vec<_>>();
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
    if let Some(status) = &overlay.status {
        put_text(
            buffer,
            inner.x + 1,
            status_y,
            text_width,
            &crate::ui::truncate_end(status, usize::from(text_width)),
            Style::default()
                .fg(palette.accent)
                .bg(palette.panel_bg)
                .add_modifier(Modifier::BOLD),
        );
    }
    let hint = [
        "↑/↓ j/k move   r run now   l logs   e enable/disable   esc close",
        "r run   l logs   e on/off   esc close",
        "r run   l logs   esc close",
        "esc close",
    ]
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
    })
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
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0].method,
            Method::JobList(JobListParams {
                since_revision: None
            })
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
            "r run now",
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
        assert!(screen(&mut state).contains("r run now"));
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
}
