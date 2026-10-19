//! Leave a prompt in an agent's input box as a draft.
//!
//! `[feed.work]` (default) and `[handoff]` with `submit = false` start the
//! agent with no prompt argument, because an argument makes the agent submit
//! it. The prompt follows once the agent can take input, typed in as a
//! bracketed paste with no Enter, so the user adds their own words and
//! presses Enter themselves.
//!
//! The wait never blocks the server: `open_agent_tab` only queues a
//! `PendingDraft`; the server loop calls `service_pending_drafts` when
//! `next_draft_deadline` passes, and each call only reads app state.
//!
//! Readiness uses the same facts as `agent.wait`: the pane is an agent
//! terminal (detection found the agent) and its status is idle or done, held
//! for two polls so a flicker at startup does not count. A paste also needs
//! the terminal to have turned bracketed paste on, which agent TUIs do at
//! startup. An agent flockr does not know has no detection to wait for, so
//! a short fixed delay stands in.

use std::time::{Duration, Instant};

use super::App;
use crate::api::schema::AgentStatus;
use crate::jobs::definition::Prefill;

/// How often a pending draft looks at its pane.
pub(crate) const DRAFT_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// How long a known agent may take to become ready before the prompt is
/// typed anyway (with a warning in the log).
pub(crate) const DRAFT_READY_TIMEOUT: Duration = Duration::from_secs(20);
/// The wait for an agent flockr has no detection for.
pub(crate) const DRAFT_UNKNOWN_AGENT_DELAY: Duration = Duration::from_millis(1500);

/// What to type, and for which kind of agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DraftRequest {
    pub(crate) prefill: Prefill,
    /// The whole prompt, typed for `Prefill::Paste`.
    pub(crate) prompt_text: String,
    /// The one-line `@<prompt file> `, typed for `Prefill::Mention` and when
    /// the terminal cannot take a bracketed paste.
    pub(crate) mention_text: String,
    /// Flockr can detect this agent, so wait for detection instead of a delay.
    pub(crate) known_agent: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingDraft {
    pane_id: String,
    request: DraftRequest,
    queued_at: Instant,
    next_check: Instant,
    /// The previous poll found the agent ready.
    ready_before: bool,
}

/// What a poll saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DraftObservation {
    /// The pane is an agent terminal and its status is idle or done.
    pub(crate) agent_ready: bool,
    pub(crate) bracketed_paste: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DraftStep {
    Wait,
    Type {
        text: String,
        /// Set when the agent never became ready and the text goes in anyway.
        timed_out: bool,
    },
}

fn draft_text(request: &DraftRequest, bracketed_paste: bool) -> String {
    match request.prefill {
        Prefill::Paste if bracketed_paste => request.prompt_text.clone(),
        // Without bracketed paste a multi-line prompt would submit at its
        // first newline, so a single line stands in.
        Prefill::Paste | Prefill::Mention => request.mention_text.clone(),
    }
}

/// The decision of one poll, without touching any state.
pub(crate) fn draft_step(
    request: &DraftRequest,
    observed: DraftObservation,
    ready_before: bool,
    elapsed: Duration,
) -> DraftStep {
    let paste_ready = request.prefill == Prefill::Mention || observed.bracketed_paste;
    if !request.known_agent {
        return if elapsed >= DRAFT_UNKNOWN_AGENT_DELAY {
            DraftStep::Type {
                text: draft_text(request, observed.bracketed_paste),
                timed_out: false,
            }
        } else {
            DraftStep::Wait
        };
    }
    if observed.agent_ready && paste_ready && ready_before {
        return DraftStep::Type {
            text: draft_text(request, observed.bracketed_paste),
            timed_out: false,
        };
    }
    if elapsed >= DRAFT_READY_TIMEOUT {
        return DraftStep::Type {
            text: draft_text(request, observed.bracketed_paste),
            timed_out: true,
        };
    }
    DraftStep::Wait
}

impl App {
    /// Queue `request` for `pane_id`. Nothing is typed until the agent is
    /// ready; the server loop calls `service_pending_drafts`.
    pub(crate) fn queue_draft(&mut self, pane_id: String, request: DraftRequest, now: Instant) {
        self.pending_drafts.push(PendingDraft {
            pane_id,
            request,
            queued_at: now,
            next_check: now + DRAFT_POLL_INTERVAL,
            ready_before: false,
        });
    }

    /// When the server loop should next look at the pending drafts.
    pub(crate) fn next_draft_deadline(&self) -> Option<Instant> {
        self.pending_drafts
            .iter()
            .map(|pending| pending.next_check)
            .min()
    }

    /// Look at every pending draft that is due: type the ones whose agent is
    /// ready (or whose wait ran out), drop the ones whose pane is gone.
    /// Returns whether anything was typed.
    pub(crate) fn service_pending_drafts(&mut self, now: Instant) -> bool {
        if self.pending_drafts.is_empty() {
            return false;
        }
        let mut typed = false;
        let pending = std::mem::take(&mut self.pending_drafts);
        for mut draft in pending {
            if now < draft.next_check {
                self.pending_drafts.push(draft);
                continue;
            }
            let Some((ws_idx, pane_id)) = self.parse_pane_id(&draft.pane_id) else {
                tracing::debug!(pane = %draft.pane_id, "draft dropped: the pane is gone");
                continue;
            };
            let observed = self.observe_draft_pane(ws_idx, pane_id);
            let step = match observed {
                Some(observed) => draft_step(
                    &draft.request,
                    observed,
                    draft.ready_before,
                    now.saturating_duration_since(draft.queued_at),
                ),
                // The terminal has no runtime yet; only the timeout ends it.
                None if now.saturating_duration_since(draft.queued_at) >= DRAFT_READY_TIMEOUT => {
                    tracing::warn!(pane = %draft.pane_id, "draft dropped: the pane never started");
                    continue;
                }
                None => DraftStep::Wait,
            };
            match step {
                DraftStep::Wait => {
                    draft.ready_before = observed.is_some_and(|observed| observed.agent_ready);
                    draft.next_check = now + DRAFT_POLL_INTERVAL;
                    self.pending_drafts.push(draft);
                }
                DraftStep::Type { text, timed_out } => {
                    if timed_out {
                        tracing::warn!(
                            pane = %draft.pane_id,
                            "the agent was not ready for input after {}s; typing the draft anyway",
                            DRAFT_READY_TIMEOUT.as_secs()
                        );
                    }
                    typed |= self.type_draft(ws_idx, pane_id, &draft.pane_id, text);
                }
            }
        }
        typed
    }

    fn observe_draft_pane(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<DraftObservation> {
        let runtime = self.lookup_runtime_sender(ws_idx, pane_id)?;
        let agent_ready = self.agent_info(ws_idx, pane_id).is_some_and(|agent| {
            matches!(agent.agent_status, AgentStatus::Idle | AgentStatus::Done)
        });
        Some(DraftObservation {
            agent_ready,
            bracketed_paste: runtime.bracketed_paste_enabled(),
        })
    }

    /// Type `text` as a paste. Never sends Enter.
    fn type_draft(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        public_pane_id: &str,
        text: String,
    ) -> bool {
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return false;
        };
        match runtime.try_send_paste(text) {
            Ok(()) => {
                tracing::info!(pane = %public_pane_id, "typed the prompt as a draft");
                true
            }
            Err(err) => {
                tracing::warn!(pane = %public_pane_id, %err, "could not type the draft");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(prefill: Prefill, known_agent: bool) -> DraftRequest {
        DraftRequest {
            prefill,
            prompt_text: "line one\nline two\n".into(),
            mention_text: "@/state/work/1.md ".into(),
            known_agent,
        }
    }

    const READY: DraftObservation = DraftObservation {
        agent_ready: true,
        bracketed_paste: true,
    };

    #[test]
    fn a_known_agent_is_typed_once_ready_on_two_polls() {
        let req = request(Prefill::Paste, true);
        let first = draft_step(&req, READY, false, Duration::from_secs(1));
        assert_eq!(first, DraftStep::Wait, "one ready poll is not enough");
        let second = draft_step(&req, READY, true, Duration::from_secs(1));
        assert_eq!(
            second,
            DraftStep::Type {
                text: "line one\nline two\n".into(),
                timed_out: false
            }
        );
    }

    #[test]
    fn a_known_agent_that_is_working_or_not_detected_waits() {
        let req = request(Prefill::Paste, true);
        let busy = DraftObservation {
            agent_ready: false,
            bracketed_paste: true,
        };
        assert_eq!(
            draft_step(&req, busy, true, Duration::from_secs(5)),
            DraftStep::Wait
        );
    }

    #[test]
    fn a_paste_waits_for_bracketed_paste_but_a_mention_does_not() {
        let no_bracketed = DraftObservation {
            agent_ready: true,
            bracketed_paste: false,
        };
        assert_eq!(
            draft_step(
                &request(Prefill::Paste, true),
                no_bracketed,
                true,
                Duration::from_secs(1)
            ),
            DraftStep::Wait
        );
        assert_eq!(
            draft_step(
                &request(Prefill::Mention, true),
                no_bracketed,
                true,
                Duration::from_secs(1)
            ),
            DraftStep::Type {
                text: "@/state/work/1.md ".into(),
                timed_out: false
            }
        );
    }

    #[test]
    fn the_timeout_types_anyway_and_falls_back_to_one_line_without_bracketed_paste() {
        let req = request(Prefill::Paste, true);
        let stuck = DraftObservation {
            agent_ready: false,
            bracketed_paste: true,
        };
        assert_eq!(
            draft_step(&req, stuck, false, DRAFT_READY_TIMEOUT),
            DraftStep::Type {
                text: "line one\nline two\n".into(),
                timed_out: true
            }
        );
        let stuck_plain = DraftObservation {
            agent_ready: false,
            bracketed_paste: false,
        };
        assert_eq!(
            draft_step(&req, stuck_plain, false, DRAFT_READY_TIMEOUT),
            DraftStep::Type {
                text: "@/state/work/1.md ".into(),
                timed_out: true
            }
        );
        assert_eq!(
            draft_step(
                &req,
                stuck,
                false,
                DRAFT_READY_TIMEOUT - Duration::from_millis(1)
            ),
            DraftStep::Wait
        );
    }

    #[test]
    fn an_unknown_agent_waits_a_fixed_delay_and_ignores_detection() {
        let req = request(Prefill::Paste, false);
        let none = DraftObservation {
            agent_ready: false,
            bracketed_paste: true,
        };
        assert_eq!(
            draft_step(&req, none, false, Duration::from_millis(1499)),
            DraftStep::Wait
        );
        assert_eq!(
            draft_step(&req, none, false, DRAFT_UNKNOWN_AGENT_DELAY),
            DraftStep::Type {
                text: "line one\nline two\n".into(),
                timed_out: false
            }
        );
    }
}
