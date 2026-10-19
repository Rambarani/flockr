//! App-side glue for handoffs: evaluating a finished run into a decision, the
//! toast that asks the user, and the `decision.*` socket API. The evaluation
//! and templates live in `crate::jobs::handoff`, the persisted list in
//! `crate::jobs::decisions`.
//!
//! Start opens a tab through the same code as `tab.create` and types the
//! agent command into it, like `flockr upstream copy` always did, so the
//! agent owns a real terminal and the user can watch and steer it.

use std::collections::HashMap;
use std::path::Path;

use crate::api::schema::{
    DecisionInfo, DecisionListParams, DecisionSnoozeParams, DecisionTarget, ErrorResponse,
    EventData, EventEnvelope, EventKind, PaneSendInputParams, ResponseResult, SuccessResponse,
    TabCreateParams,
};
use crate::jobs::decisions::{
    parse_decision_id, AddOutcome, DecisionError, DecisionRecord, NewDecision, DEFAULT_SNOOZE_SECS,
};
use crate::jobs::handoff::{self, EvalInput, FeedSnapshot, PendingHandoff};
use crate::jobs::manager::Notice;
use crate::jobs::now_unix_ms;

use super::responses;
use crate::app::App;

/// Bytes of a rendered prompt returned for a preview.
const PROMPT_PREVIEW_BYTES: usize = 64 * 1024;
/// Variables the login-shell snapshot carries that must not be copied into a
/// new tab: the tab sets its own.
const TAB_ENV_SKIP: &[&str] = &["PWD", "OLDPWD", "SHLVL", "_"];

/// The API error codes `open_agent_tab` reports, so each caller keeps its own.
#[derive(Clone, Copy)]
pub(crate) struct AgentTabErrorCodes {
    pub(crate) tab_failed: &'static str,
    pub(crate) launch_failed: &'static str,
}

impl App {
    /// The job's feed as a handoff sees it, after the run's refresh.
    fn handoff_feed_snapshot(
        &mut self,
        job: &crate::jobs::definition::JobDef,
    ) -> Option<FeedSnapshot> {
        let feed = job.feed.as_ref()?;
        let state_dir = self.jobs.state_dir_for(&job.id);
        let summary = self
            .feeds
            .summary(&job.id, &state_dir, feed.title.as_deref(), &job.title);
        let items = self.feeds.items(&job.id, &state_dir, None);
        let json = handoff::feed_json(&summary.title, &items);
        Some(FeedSnapshot {
            unread: summary.unread,
            fingerprint: handoff::feed_fingerprint(&json),
            json,
        })
    }

    /// Decide whether a finished run asks the user something, and if so store
    /// the decision and tell them.
    pub(crate) fn evaluate_handoff(&mut self, pending: PendingHandoff) {
        let PendingHandoff {
            record,
            def,
            previous_fingerprint,
        } = pending;
        let now_ms = now_unix_ms();
        let feed = self.handoff_feed_snapshot(&def);
        let date = handoff::local_date(now_ms);
        let evaluated = handoff::evaluate(&EvalInput {
            job: &def,
            record: &record,
            feed: feed.as_ref(),
            previous_fingerprint: previous_fingerprint.as_deref(),
            date: &date,
        });
        let result = match evaluated {
            Ok(result) => result,
            Err(error) => {
                tracing::warn!(job = %def.id, %error, "handoff could not be built");
                self.notify_decision_problem(&def.title, &format!("handoff failed: {error}"));
                return;
            }
        };
        if def.handoff.is_some() {
            self.jobs
                .set_handoff_fingerprint(&def.id, result.fingerprint.clone());
        }
        let Some(offer) = result.offer else {
            return;
        };
        for warning in &offer.warnings {
            tracing::warn!(job = %def.id, %warning, "handoff");
        }
        if self
            .decisions
            .find_duplicate(&def.id, &offer.content_hash)
            .is_some()
        {
            tracing::debug!(job = %def.id, "handoff offers what was already asked");
            return;
        }
        let prompt_file = if offer.prompt.is_empty() {
            None
        } else {
            let state_dir = self.jobs.state_dir_for(&def.id);
            match handoff::write_prompt(&state_dir, record.info.run_id, &offer.prompt) {
                Ok(path) => Some(path.display().to_string()),
                Err(err) => {
                    tracing::warn!(job = %def.id, err = %err, "could not write the handoff prompt");
                    self.notify_decision_problem(
                        &def.title,
                        &format!("handoff failed: cannot write the prompt: {err}"),
                    );
                    return;
                }
            }
        };
        let outcome = self.decisions.add(
            NewDecision {
                job_id: def.id.clone(),
                job_title: def.title.clone(),
                run_id: record.info.run_id,
                ask: offer.ask,
                tab_title: offer.tab_title,
                cwd: offer.cwd,
                agent_command: offer.agent_command,
                prompt_file,
                context: offer.context,
                content_hash: offer.content_hash,
            },
            now_ms,
        );
        let AddOutcome::Created(id) = outcome else {
            return;
        };
        let Some(created) = self.decisions.get(id).cloned() else {
            return;
        };
        self.emit_event(EventEnvelope {
            event: EventKind::DecisionCreated,
            data: EventData::DecisionCreated {
                decision: created.info(),
            },
        });
        self.notify_decision(&created);
        self.deliver_job_notices();
    }

    /// Tell the user a decision is waiting: an actionable attention toast
    /// carrying the ask text.
    fn notify_decision(&mut self, record: &DecisionRecord) {
        let notice = Notice {
            title: record.job_title.clone(),
            body: record.ask.clone(),
            failed: true,
            actionable: true,
        };
        if self.state.toast_config.delivery != crate::config::ToastDelivery::Off {
            self.job_client_notices.push(notice.clone());
        }
        self.queue_job_notice(notice);
    }

    fn notify_decision_problem(&mut self, title: &str, body: &str) {
        let notice = Notice {
            title: title.to_owned(),
            body: body.to_owned(),
            failed: true,
            actionable: false,
        };
        if self.state.toast_config.delivery != crate::config::ToastDelivery::Off {
            self.job_client_notices.push(notice.clone());
        }
        self.queue_job_notice(notice);
        self.deliver_job_notices();
    }

    /// Snoozes that ended become pending again and ask once more.
    pub(crate) fn wake_snoozed_decisions(&mut self, now_ms: u64) {
        for record in self.decisions.expire_snoozes(now_ms) {
            self.notify_decision(&record);
        }
    }

    fn decision_info(record: &DecisionRecord, include_prompt: bool) -> DecisionInfo {
        let mut info = record.info();
        if include_prompt {
            info.prompt = record
                .prompt_file
                .as_deref()
                .map(|path| read_prompt_preview(Path::new(path)));
        }
        info
    }

    fn decision_error(id: String, decision_id: &str, error: DecisionError) -> String {
        match error {
            DecisionError::NotFound => responses::encode_error(
                id,
                "decision_not_found",
                format!("no decision named {decision_id}"),
            ),
            DecisionError::Resolved(_) => responses::encode_error(
                id,
                "decision_resolved",
                format!("decision {decision_id}: {error}"),
            ),
        }
    }

    pub(super) fn handle_decision_list(
        &mut self,
        id: String,
        params: DecisionListParams,
    ) -> String {
        let now_ms = now_unix_ms();
        self.wake_snoozed_decisions(now_ms);
        self.deliver_job_notices();
        let revision = self.decisions.revision();
        let unchanged = params.since_revision == Some(revision)
            && params.decision_id.is_none()
            && !params.include_prompt;
        let pending = self.decisions.pending_count();
        let decisions = if unchanged {
            Vec::new()
        } else if let Some(wanted) = &params.decision_id {
            let Some(numeric) = parse_decision_id(wanted) else {
                return Self::decision_error(id, wanted, DecisionError::NotFound);
            };
            match self.decisions.get(numeric) {
                Some(record) => vec![Self::decision_info(record, params.include_prompt)],
                None => return Self::decision_error(id, wanted, DecisionError::NotFound),
            }
        } else {
            self.decisions
                .list(params.include_resolved)
                .into_iter()
                .map(|record| Self::decision_info(record, params.include_prompt))
                .collect()
        };
        responses::encode_success(
            id,
            ResponseResult::DecisionList {
                revision,
                unchanged,
                pending,
                decisions,
            },
        )
    }

    pub(super) fn handle_decision_snooze(
        &mut self,
        id: String,
        params: DecisionSnoozeParams,
    ) -> String {
        let Some(numeric) = parse_decision_id(&params.decision_id) else {
            return Self::decision_error(id, &params.decision_id, DecisionError::NotFound);
        };
        match self.decisions.snooze(
            numeric,
            params.for_secs.unwrap_or(DEFAULT_SNOOZE_SECS),
            now_unix_ms(),
        ) {
            Ok(record) => {
                self.next_jobs_tick = Some(std::time::Instant::now());
                responses::encode_success(
                    id,
                    ResponseResult::DecisionUpdated {
                        decision: record.info(),
                    },
                )
            }
            Err(error) => Self::decision_error(id, &params.decision_id, error),
        }
    }

    pub(super) fn handle_decision_dismiss(&mut self, id: String, target: DecisionTarget) -> String {
        let Some(numeric) = parse_decision_id(&target.decision_id) else {
            return Self::decision_error(id, &target.decision_id, DecisionError::NotFound);
        };
        match self.decisions.dismiss(numeric, now_unix_ms()) {
            Ok(record) => responses::encode_success(
                id,
                ResponseResult::DecisionUpdated {
                    decision: record.info(),
                },
            ),
            Err(error) => Self::decision_error(id, &target.decision_id, error),
        }
    }

    /// `decision.start`: open a tab in the decision's directory, titled as the
    /// handoff asked, and type the agent command into it. A failure (missing
    /// directory, no workspace, a prompt file that is gone) leaves the
    /// decision pending with the reason recorded.
    pub(super) fn handle_decision_start(&mut self, id: String, target: DecisionTarget) -> String {
        let Some(numeric) = parse_decision_id(&target.decision_id) else {
            return Self::decision_error(id, &target.decision_id, DecisionError::NotFound);
        };
        let Some(record) = self.decisions.get(numeric).cloned() else {
            return Self::decision_error(id, &target.decision_id, DecisionError::NotFound);
        };
        if !matches!(
            record.status,
            crate::api::schema::DecisionStatus::Pending
                | crate::api::schema::DecisionStatus::Snoozed
        ) {
            return Self::decision_error(
                id,
                &target.decision_id,
                DecisionError::Resolved(record.status),
            );
        }
        match self.start_decision_tab(&record) {
            Ok((tab_id, pane_id)) => {
                let outcome = format!("started in tab {tab_id}");
                match self.decisions.mark_started(numeric, outcome, now_unix_ms()) {
                    Ok(started) => responses::encode_success(
                        id,
                        ResponseResult::DecisionStarted {
                            decision: started.info(),
                            tab_id,
                            pane_id,
                        },
                    ),
                    Err(error) => Self::decision_error(id, &target.decision_id, error),
                }
            }
            Err((code, message)) => {
                let _ = self.decisions.record_error(numeric, message.clone());
                responses::encode_error(id, code, message)
            }
        }
    }

    /// The tab half of Start. `Err` is an API error code and message.
    fn start_decision_tab(
        &mut self,
        record: &DecisionRecord,
    ) -> Result<(String, String), (&'static str, String)> {
        let cwd = handoff::expand_cwd(&record.cwd);
        if !cwd.is_dir() {
            return Err((
                "decision_cwd_missing",
                format!(
                    "the working directory {} does not exist; create it or dismiss the decision",
                    cwd.display()
                ),
            ));
        }
        let prompt_path = match record.prompt_file.as_deref() {
            Some(path) => {
                let path = Path::new(path);
                if !path.is_file() {
                    return Err((
                        "decision_prompt_missing",
                        format!(
                            "the prompt file {} is gone; dismiss the decision",
                            path.display()
                        ),
                    ));
                }
                Some(path.to_path_buf())
            }
            None => None,
        };
        let command = handoff::launch_command(&record.agent_command, prompt_path.as_deref())
            .map_err(|message| ("decision_command_invalid", message))?;
        self.open_agent_tab(
            &record.job_id,
            &cwd,
            command,
            (!record.tab_title.is_empty()).then(|| record.tab_title.clone()),
            AgentTabErrorCodes {
                tab_failed: "decision_tab_failed",
                launch_failed: "decision_launch_failed",
            },
        )
    }

    /// Open a focused tab in `cwd` with the job's environment rules, titled
    /// `label`, and type `command` into it. The one code path under both a
    /// decision's Start and `feed.work`, so the agent owns a real terminal
    /// and the user can watch and steer it. `Err` is an API error code and
    /// message.
    pub(crate) fn open_agent_tab(
        &mut self,
        job_id: &str,
        cwd: &Path,
        command: String,
        label: Option<String>,
        codes: AgentTabErrorCodes,
    ) -> Result<(String, String), (&'static str, String)> {
        let env = self.decision_tab_env(job_id);

        let created = self.handle_tab_create(
            "decision:start:tab".into(),
            TabCreateParams {
                workspace_id: None,
                cwd: Some(cwd.display().to_string()),
                focus: true,
                label,
                env,
            },
        );
        let (tab_id, pane_id) = match serde_json::from_str::<SuccessResponse>(&created) {
            Ok(SuccessResponse {
                result: ResponseResult::TabCreated { tab, root_pane },
                ..
            }) => (tab.tab_id, root_pane.pane_id),
            _ => {
                let message = serde_json::from_str::<ErrorResponse>(&created)
                    .map(|error| error.error.message)
                    .unwrap_or_else(|_| "the new tab did not report its pane".into());
                return Err((codes.tab_failed, format!("could not open a tab: {message}")));
            }
        };
        let sent = self.handle_pane_send_input(
            "decision:start:input".into(),
            PaneSendInputParams {
                pane_id: pane_id.clone(),
                text: command,
                keys: vec!["Enter".into()],
            },
        );
        if serde_json::from_str::<ErrorResponse>(&sent).is_ok() {
            return Err((
                codes.launch_failed,
                format!("opened tab {tab_id} but could not start the agent in it: {sent}"),
            ));
        }
        Ok((tab_id, pane_id))
    }

    /// The environment of the job's rules for the new tab: the cached
    /// login-shell variables (when captured), `env_file`, `[env]` overrides and
    /// the `FLOCKR_*` job variables. Never waits for a capture.
    fn decision_tab_env(&self, job_id: &str) -> HashMap<String, String> {
        let Some(job) = self.jobs.job_def(job_id) else {
            return HashMap::new();
        };
        let state_dir = self.jobs.state_dir_for(job_id);
        let vars = crate::jobs::env::flockr_job_vars(job, &state_dir);
        match crate::jobs::env::resolve_job_env_cached(job, &self.jobs.cache(), vars) {
            Ok(resolved) => resolved
                .vars
                .into_iter()
                .filter(|(name, _)| !TAB_ENV_SKIP.contains(&name.as_str()))
                .collect(),
            Err(err) => {
                tracing::warn!(job = %job_id, %err, "decision tab environment skipped");
                HashMap::new()
            }
        }
    }
}

fn read_prompt_preview(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => {
            let mut end = bytes.len().min(PROMPT_PREVIEW_BYTES);
            let mut text = String::from_utf8_lossy(&bytes[..end]).into_owned();
            if bytes.len() > end {
                // The cut may split a character; lossy decoding turned that
                // into one replacement mark at the end.
                while end > 0 && text.ends_with('\u{fffd}') {
                    text.pop();
                    end -= 1;
                }
                text.push_str("\n… (preview truncated)");
            }
            text
        }
        Err(err) => format!("(the prompt file {} cannot be read: {err})", path.display()),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::sync::Arc;

    use super::super::test_support::{exiting_test_command, shutdown_test_runtimes};
    use super::*;
    use crate::api::schema::{DecisionStatus, ErrorResponse, Method, Request};
    use crate::jobs::decisions::DecisionStore;
    use crate::jobs::env::LoginEnvCache;
    use crate::jobs::{JobsManager, JobsPaths};

    struct Fixture {
        root: std::path::PathBuf,
        app: App,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let root = std::env::temp_dir().join(format!(
                "flockr-app-decisions-{name}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(root.join("config")).unwrap();
            let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
            let mut app = App::new(
                &crate::config::Config::default(),
                crate::app::AppPolicy::TEST,
                None,
                api_rx,
                crate::api::EventHub::default(),
            );
            app.jobs = JobsManager::new(
                JobsPaths {
                    config_dir: root.join("config"),
                    state_root: root.join("state"),
                },
                Arc::new(LoginEnvCache::new(Box::new(|| {
                    Ok(vec![("PATH".into(), "/usr/bin:/bin".into())])
                }))),
                true,
            );
            app.decisions = DecisionStore::open(root.join("decisions.json"));
            app.state.toast_config.delivery = crate::config::ToastDelivery::Flockr;
            app.state.default_shell = exiting_test_command().into();
            app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;
            app.state.workspaces = vec![crate::workspace::Workspace::test_new("spaces")];
            app.state.active = Some(0);
            app.state.selected = 0;
            app.state.ensure_test_terminals();
            Self { root, app }
        }

        fn write_job(&self, id: &str, body: &str) {
            let dir = self.root.join("config").join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("job.toml"), body).unwrap();
        }

        fn call(&mut self, method: Method) -> String {
            self.app.handle_api_request(Request {
                id: "test".into(),
                method,
            })
        }

        fn ok(&mut self, method: Method) -> ResponseResult {
            let response = self.call(method);
            serde_json::from_str::<SuccessResponse>(&response)
                .unwrap_or_else(|err| panic!("not a success: {response}: {err}"))
                .result
        }

        fn err(&mut self, method: Method) -> (String, String) {
            let response = self.call(method);
            let error = serde_json::from_str::<ErrorResponse>(&response)
                .unwrap_or_else(|err| panic!("not an error: {response}: {err}"))
                .error;
            (error.code, error.message)
        }

        /// Run the job and pump events until its handoff is evaluated.
        fn run_job(&mut self, job_id: &str) {
            self.ok(Method::JobRun(crate::api::schema::JobTarget {
                job_id: job_id.into(),
            }));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            loop {
                self.app.drain_all_internal_events();
                if !self.app.jobs.is_running(job_id)
                    && !self.app.feeds.is_busy(job_id)
                    && self.app.pending_handoffs.is_empty()
                {
                    return;
                }
                assert!(std::time::Instant::now() < deadline, "job did not finish");
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }

        fn list(&mut self) -> Vec<DecisionInfo> {
            match self.ok(Method::DecisionList(DecisionListParams::default())) {
                ResponseResult::DecisionList { decisions, .. } => decisions,
                other => panic!("expected a decision list, got {other:?}"),
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn target(id: &str) -> DecisionTarget {
        DecisionTarget {
            decision_id: id.into(),
        }
    }

    fn handoff_job(cwd: &Path, extra: &str) -> String {
        format!(
            "command = [\"sh\", \"-c\", \"echo hello\"]\nevery = \"1h\"\nenv = \"server\"\ntitle = \"Demo\"\n[handoff]\nwhen = \"always\"\nask = \"{{{{job.id}}}} said {{{{stdout}}}} - start?\"\ncwd = \"{}\"\ntab_title = \"demo tab\"\nprompt = \"Please look at {{{{stdout}}}}\"\n{extra}",
            cwd.display()
        )
    }

    #[test]
    fn a_run_creates_one_pending_decision_with_a_prompt_file_and_a_toast() {
        let mut fx = Fixture::new("create");
        let work = fx.root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        fx.write_job("demo", &handoff_job(&work, ""));
        fx.run_job("demo");

        let decisions = fx.list();
        assert_eq!(decisions.len(), 1, "{decisions:?}");
        let decision = &decisions[0];
        assert_eq!(decision.decision_id, "d1");
        assert_eq!(decision.job_id, "demo");
        assert_eq!(decision.job_title, "Demo");
        assert_eq!(decision.ask, "demo said hello - start?");
        assert_eq!(decision.tab_title, "demo tab");
        assert_eq!(decision.cwd, work.display().to_string());
        assert_eq!(decision.status, DecisionStatus::Pending);
        let prompt_file = decision.prompt_file.clone().expect("prompt file");
        assert!(prompt_file.ends_with("handoffs/1.md"), "{prompt_file}");
        assert_eq!(
            std::fs::read_to_string(&prompt_file).unwrap(),
            "Please look at hello\n"
        );
        // The user is asked through an attention toast with the ask text.
        let toast = fx.app.state.toast.as_ref().expect("decision toast");
        assert_eq!(toast.kind, crate::app::state::ToastKind::NeedsAttention);
        assert_eq!(toast.title, "Demo");
        assert_eq!(toast.context, "demo said hello - start?");
        // And the event reached subscribers.
        assert_eq!(fx.app.decisions.pending_count(), 1);
    }

    #[test]
    fn the_same_content_does_not_ask_twice() {
        let mut fx = Fixture::new("dedupe");
        let work = fx.root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        fx.write_job("demo", &handoff_job(&work, ""));
        fx.run_job("demo");
        fx.app.state.toast = None;
        fx.run_job("demo");
        assert_eq!(fx.list().len(), 1, "same job and same content");
        assert!(fx.app.state.toast.is_none(), "no second toast");
    }

    #[test]
    fn on_change_asks_again_only_when_the_output_changes() {
        let mut fx = Fixture::new("onchange");
        let work = fx.root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let counter = fx.root.join("counter");
        std::fs::write(&counter, "1").unwrap();
        fx.write_job(
            "demo",
            &format!(
                "command = [\"sh\", \"-c\", \"cat {}\"]\nevery = \"1h\"\nenv = \"server\"\n[handoff]\nwhen = \"on_change\"\ncwd = \"{}\"\nask = \"changed {{{{stdout}}}}\"\n",
                counter.display(),
                work.display()
            ),
        );
        fx.run_job("demo");
        assert_eq!(fx.list().len(), 1, "the first run counts as a change");
        fx.run_job("demo");
        assert_eq!(fx.list().len(), 1, "same output, no new question");
        std::fs::write(&counter, "2").unwrap();
        fx.run_job("demo");
        let decisions = fx.list();
        assert_eq!(decisions.len(), 2);
        assert_eq!(decisions[0].ask, "changed 2");
    }

    #[test]
    fn a_job_without_a_matching_when_asks_nothing() {
        let mut fx = Fixture::new("nomatch");
        let work = fx.root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        fx.write_job(
            "demo",
            &handoff_job(&work, "").replace("always", "on_failure"),
        );
        fx.run_job("demo");
        assert!(fx.list().is_empty());
        assert!(fx.app.state.toast.is_none());
    }

    #[test]
    fn a_dynamic_line_creates_a_decision_without_a_handoff_table() {
        let mut fx = Fixture::new("dynamic");
        let work = fx.root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let line = format!(
            "{{\\\"flockr_handoff\\\":{{\\\"ask\\\":\\\"{{{{context.tag}}}} is out\\\",\\\"cwd\\\":\\\"{}\\\",\\\"agent_command\\\":\\\"echo {{{{context.tag}}}}\\\",\\\"context\\\":{{\\\"tag\\\":\\\"v1\\\"}}}}}}",
            work.display()
        );
        fx.write_job(
            "dyn",
            &format!(
                "command = [\"sh\", \"-c\", \"echo '{line}'\"]\nevery = \"1h\"\nenv = \"server\"\n"
            ),
        );
        fx.run_job("dyn");
        let decisions = fx.list();
        assert_eq!(decisions.len(), 1, "{decisions:?}");
        assert_eq!(decisions[0].ask, "v1 is out");
        assert_eq!(decisions[0].agent_command, "echo v1");
        assert_eq!(decisions[0].context.as_ref().unwrap()["tag"], "v1");
        assert!(decisions[0].prompt_file.is_none());
    }

    #[test]
    fn the_prompt_waits_for_the_feed_refresh() {
        let mut fx = Fixture::new("feedwait");
        let work = fx.root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        fx.write_job(
            "demo",
            &format!(
                "command = [\"true\"]\nevery = \"1h\"\nenv = \"server\"\nfeed = {{ command = [\"sh\", \"-c\", \"echo '{{\\\"version\\\":1,\\\"items\\\":[{{\\\"id\\\":\\\"a\\\",\\\"title\\\":\\\"Fix login\\\",\\\"state\\\":\\\"unread\\\"}},{{\\\"id\\\":\\\"b\\\",\\\"title\\\":\\\"Review PR\\\",\\\"state\\\":\\\"unread\\\"}}]}}'\"] }}\n[handoff]\nwhen = \"on_change\"\ncwd = \"{}\"\nask = \"{{{{count}}}} items\"\nprompt = \"{{{{feed_json}}}}\"\n",
                work.display()
            ),
        );
        fx.run_job("demo");
        let decisions = fx.list();
        assert_eq!(decisions.len(), 1, "{decisions:?}");
        assert_eq!(decisions[0].ask, "2 items");
        let prompt = std::fs::read_to_string(decisions[0].prompt_file.as_ref().unwrap()).unwrap();
        assert!(
            prompt.contains("Fix login") && prompt.contains("Review PR"),
            "{prompt}"
        );
    }

    #[tokio::test]
    async fn start_opens_a_tab_in_the_cwd_titled_and_types_the_command() {
        let mut fx = Fixture::new("start");
        let work = fx.root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        // A shell that stays alive, so the typed command has somewhere to go.
        fx.app.state.default_shell = "/bin/cat".into();
        fx.write_job(
            "demo",
            &handoff_job(&work, "agent_command = \"cat {prompt_file}\"\n"),
        );
        fx.run_job("demo");
        let before = fx.app.state.workspaces[0].tabs.len();

        let ResponseResult::DecisionStarted {
            decision,
            tab_id,
            pane_id,
        } = fx.ok(Method::DecisionStart(target("d1")))
        else {
            panic!("expected a started decision");
        };
        assert_eq!(decision.status, DecisionStatus::Started);
        assert!(decision
            .outcome
            .as_deref()
            .unwrap()
            .starts_with("started in tab"));
        assert_eq!(fx.app.state.workspaces[0].tabs.len(), before + 1);
        let tab = fx.app.state.workspaces[0].tabs.last().unwrap();
        assert_eq!(tab.custom_name.as_deref(), Some("demo tab"));
        assert!(!tab_id.is_empty() && !pane_id.is_empty());
        // The new pane runs in the handoff's directory.
        let root_pane = tab.root_pane;
        let terminal_cwd = fx.app.state.workspaces[0]
            .terminal_id(root_pane)
            .and_then(|terminal_id| fx.app.state.terminals.get(terminal_id))
            .map(|terminal| crate::worktree::canonical_or_original(&terminal.cwd));
        assert_eq!(
            terminal_cwd,
            Some(crate::worktree::canonical_or_original(&work))
        );
        // It left the pending list.
        assert!(fx.list().is_empty());
        assert_eq!(
            fx.err(Method::DecisionStart(target("d1"))).0,
            "decision_resolved"
        );
        shutdown_test_runtimes(&mut fx.app);
    }

    #[tokio::test]
    async fn start_with_a_missing_cwd_fails_and_the_decision_stays_pending() {
        let mut fx = Fixture::new("nocwd");
        let missing = fx.root.join("does-not-exist");
        fx.write_job("demo", &handoff_job(&missing, ""));
        fx.run_job("demo");
        let before = fx.app.state.workspaces[0].tabs.len();
        let (code, message) = fx.err(Method::DecisionStart(target("d1")));
        assert_eq!(code, "decision_cwd_missing");
        assert!(message.contains("does not exist"), "{message}");
        assert_eq!(
            fx.app.state.workspaces[0].tabs.len(),
            before,
            "no tab opened"
        );
        let decisions = fx.list();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].status, DecisionStatus::Pending);
        assert!(decisions[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("does not exist"));
        // Once the directory exists the same decision starts.
        fx.app.state.default_shell = "/bin/cat".into();
        std::fs::create_dir_all(&missing).unwrap();
        assert!(matches!(
            fx.ok(Method::DecisionStart(target("d1"))),
            ResponseResult::DecisionStarted { .. }
        ));
        shutdown_test_runtimes(&mut fx.app);
    }

    #[test]
    fn snooze_dismiss_and_unknown_ids() {
        let mut fx = Fixture::new("answers");
        let work = fx.root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        fx.write_job("demo", &handoff_job(&work, ""));
        fx.run_job("demo");

        let ResponseResult::DecisionUpdated { decision } =
            fx.ok(Method::DecisionSnooze(DecisionSnoozeParams {
                decision_id: "d1".into(),
                for_secs: Some(600),
            }))
        else {
            panic!("expected an updated decision");
        };
        assert_eq!(decision.status, DecisionStatus::Snoozed);
        assert!(decision.snoozed_until_unix_ms.is_some());
        // Snoozed decisions stay in the list but are not pending.
        match fx.ok(Method::DecisionList(DecisionListParams::default())) {
            ResponseResult::DecisionList {
                pending, decisions, ..
            } => {
                assert_eq!(pending, 0);
                assert_eq!(decisions.len(), 1);
            }
            other => panic!("{other:?}"),
        }

        let ResponseResult::DecisionUpdated { decision } =
            fx.ok(Method::DecisionDismiss(target("d1")))
        else {
            panic!("expected an updated decision");
        };
        assert_eq!(decision.status, DecisionStatus::Dismissed);
        assert!(fx.list().is_empty());
        match fx.ok(Method::DecisionList(DecisionListParams {
            include_resolved: true,
            ..DecisionListParams::default()
        })) {
            ResponseResult::DecisionList { decisions, .. } => assert_eq!(decisions.len(), 1),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            fx.err(Method::DecisionDismiss(target("d1"))).0,
            "decision_resolved"
        );
        assert_eq!(
            fx.err(Method::DecisionDismiss(target("d99"))).0,
            "decision_not_found"
        );
        assert_eq!(
            fx.err(Method::DecisionStart(target("zzz"))).0,
            "decision_not_found"
        );
    }

    #[test]
    fn an_expired_snooze_asks_again() {
        let mut fx = Fixture::new("wake");
        let work = fx.root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        fx.write_job("demo", &handoff_job(&work, ""));
        fx.run_job("demo");
        fx.ok(Method::DecisionSnooze(DecisionSnoozeParams {
            decision_id: "d1".into(),
            for_secs: Some(1),
        }));
        fx.app.state.toast = None;
        fx.app.pending_job_notices.clear();
        // Not yet.
        fx.app.wake_snoozed_decisions(now_unix_ms());
        assert_eq!(fx.app.decisions.pending_count(), 0);
        fx.app.wake_snoozed_decisions(now_unix_ms() + 2_000);
        assert_eq!(fx.app.decisions.pending_count(), 1);
        assert!(
            !fx.app.pending_job_notices.is_empty(),
            "it reminds the user"
        );
    }

    #[test]
    fn list_can_return_one_decision_with_its_prompt() {
        let mut fx = Fixture::new("preview");
        let work = fx.root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        fx.write_job("demo", &handoff_job(&work, ""));
        fx.run_job("demo");
        let ResponseResult::DecisionList { decisions, .. } =
            fx.ok(Method::DecisionList(DecisionListParams {
                decision_id: Some("d1".into()),
                include_prompt: true,
                ..DecisionListParams::default()
            }))
        else {
            panic!("expected a list");
        };
        assert_eq!(
            decisions[0].prompt.as_deref(),
            Some("Please look at hello\n")
        );
        // The plain list leaves the prompt out.
        assert!(fx.list()[0].prompt.is_none());
        assert_eq!(
            fx.err(Method::DecisionList(DecisionListParams {
                decision_id: Some("d9".into()),
                ..DecisionListParams::default()
            }))
            .0,
            "decision_not_found"
        );
    }

    #[test]
    fn the_list_answers_unchanged_for_the_current_revision() {
        let mut fx = Fixture::new("revision");
        let ResponseResult::DecisionList { revision, .. } =
            fx.ok(Method::DecisionList(DecisionListParams::default()))
        else {
            panic!("expected a list");
        };
        let ResponseResult::DecisionList {
            unchanged,
            decisions,
            ..
        } = fx.ok(Method::DecisionList(DecisionListParams {
            since_revision: Some(revision),
            ..DecisionListParams::default()
        }))
        else {
            panic!("expected a list");
        };
        assert!(unchanged && decisions.is_empty());
    }

    #[test]
    fn a_prompt_file_that_cannot_be_read_stops_the_handoff_with_a_toast() {
        let mut fx = Fixture::new("badprompt");
        let work = fx.root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        fx.write_job(
            "demo",
            &handoff_job(&work, "").replace(
                "prompt = \"Please look at {{stdout}}\"",
                "prompt_file = \"missing.md\"",
            ),
        );
        fx.run_job("demo");
        assert!(fx.list().is_empty());
        let toast = fx.app.state.toast.as_ref().expect("problem toast");
        assert!(
            toast.context.contains("handoff failed"),
            "{}",
            toast.context
        );
    }

    #[test]
    fn decision_toasts_show_even_when_toasts_are_off_and_last_longer() {
        let mut fx = Fixture::new("offtoast");
        fx.app.state.toast_config.delivery = crate::config::ToastDelivery::Off;
        let work = fx.root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        fx.write_job("demo", &handoff_job(&work, ""));
        fx.write_job(
            "plain",
            "command = [\"true\"]\nevery = \"1h\"\nenv = \"server\"\nnotify = \"always\"\n",
        );
        let started = std::time::Instant::now();
        fx.run_job("demo");
        assert!(
            fx.app.state.toast.is_some(),
            "the question shows with toasts off"
        );
        let deadline = fx.app.toast_deadline.expect("deadline");
        assert!(deadline >= started + std::time::Duration::from_secs(10));
        // A plain job notice still obeys `off`.
        fx.app.state.toast = None;
        fx.app.toast_deadline = None;
        fx.run_job("plain");
        assert!(fx.app.state.toast.is_none());
    }

    #[test]
    fn system_delivery_forwards_the_question_to_clients() {
        let mut fx = Fixture::new("forward");
        fx.app.state.toast_config.delivery = crate::config::ToastDelivery::System;
        let work = fx.root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        fx.write_job("demo", &handoff_job(&work, ""));
        fx.run_job("demo");
        let notices = fx.app.take_job_client_notices();
        assert_eq!(notices.len(), 1);
        assert!(notices[0].failed && notices[0].actionable);
        assert_eq!(notices[0].body, "demo said hello - start?");
        assert!(fx.app.state.toast.is_none(), "no in-frame toast");
    }

    /// Finish a run of the system upstream job whose stdout is `stdout`.
    fn finish_upstream_run(fx: &mut Fixture, repo: &Path, stdout: &str) -> bool {
        use crate::api::schema::{JobRunInfo, JobRunStatus, JobRunTrigger};
        use crate::jobs::runner::JobRunResult;
        use crate::jobs::state::RunRecord;
        let _ = repo;
        fx.app.apply_system_jobs();
        fx.app.reload_jobs_now();
        let now = now_unix_ms();
        let plan = fx
            .app
            .jobs
            .begin_run("system.upstream-herdr", JobRunTrigger::Manual, now)
            .expect("begin");
        let record = RunRecord {
            info: JobRunInfo {
                run_id: plan.run_id,
                trigger: JobRunTrigger::Manual,
                status: JobRunStatus::Success,
                started_unix_ms: now,
                finished_unix_ms: now,
                duration_ms: 5,
                exit_code: Some(0),
                summary: "exit 0".into(),
                error: None,
                skipped_count: 0,
                env_note: None,
            },
            stdout_tail: stdout.into(),
            stderr_tail: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            handoff: crate::jobs::state::find_handoff_line(stdout),
        };
        fx.app.finish_job_run(JobRunResult {
            job_id: "system.upstream-herdr".into(),
            record,
        })
    }

    #[test]
    fn an_upstream_release_raises_one_decision_and_one_toast_with_the_tag_context() {
        let mut fx = Fixture::new("upstream");
        let repo = fx.root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let value = crate::upstream::check::handoff_value(
            &[crate::upstream::release::UpstreamRelease {
                tag: "v0.9.7".into(),
                name: "v0.9.7".into(),
                body: "### Added\n- a\n### Fixed\n- b".into(),
                prerelease: false,
                published_at: None,
                url: "https://example.invalid/v0.9.7".into(),
            }],
            &crate::upstream::check::UpstreamNotification {
                tag: "v0.9.7".into(),
                title: "herdr v0.9.7 available upstream".into(),
            },
            "/opt/flockr/flockr",
            Some(&repo.display().to_string()),
        );
        let stdout = format!(
            "synced to herdr v0.9.5; 1 new upstream release\n{}\n",
            crate::upstream::check::handoff_line(&value)
        );
        assert!(finish_upstream_run(&mut fx, &repo, &stdout));

        let decisions = fx.list();
        assert_eq!(decisions.len(), 1, "{decisions:?}");
        let decision = &decisions[0];
        assert_eq!(decision.job_id, "system.upstream-herdr");
        assert_eq!(
            decision.ask,
            "herdr v0.9.7 is out (1 feature, 1 fix). Copy updates?"
        );
        assert_eq!(
            decision.agent_command,
            "/opt/flockr/flockr upstream sync v0.9.7"
        );
        assert_eq!(decision.tab_title, "upstream-sync v0.9.7");
        assert_eq!(decision.cwd, repo.display().to_string());
        assert_eq!(decision.context.as_ref().unwrap()["tag"], "v0.9.7");
        assert!(
            decision.prompt_file.is_none(),
            "the sync writes its own prompt"
        );

        // Exactly one toast, from the decision; no separate upstream toast.
        assert_eq!(
            fx.app.state.toast.as_ref().unwrap().kind,
            crate::app::state::ToastKind::NeedsAttention
        );
        assert_eq!(fx.app.pending_job_notices.len(), 0);
        let events = fx.app.event_hub.events_after(0);
        let created = events
            .iter()
            .filter(|(_, envelope)| envelope.event == EventKind::DecisionCreated)
            .count();
        assert_eq!(created, 1);

        // The next run prints nothing new (the watcher marked the tag
        // notified), and a repeat of the same line is a duplicate.
        fx.app.state.toast = None;
        assert!(finish_upstream_run(&mut fx, &repo, &stdout));
        assert_eq!(fx.list().len(), 1);
        assert!(fx.app.state.toast.is_none(), "no double toast");
        assert!(finish_upstream_run(
            &mut fx,
            &repo,
            "synced to herdr v0.9.5; 0 new upstream releases\n"
        ));
        assert_eq!(fx.list().len(), 1);
        assert!(fx.app.state.toast.is_none());
    }
}
