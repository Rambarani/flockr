//! Feed sidebar section, Feed overlay, palette entries and polling.

use std::time::{Duration, Instant};

use super::agents_worktrees_notifications::two_workspace_agent_snapshot;
use super::endpoints::state_with_remote;
use super::*;
use crate::api::schema::{
    FeedActionInfo, FeedActionResult, FeedField, FeedGetParams, FeedGroupInfo, FeedItemInfo,
    FeedItemState, FeedListParams, FeedRunActionParams, FeedSummary, FeedTarget, FeedWorkParams,
    JobInfo, JobRunInfo, JobRunStatus, JobRunTrigger, JobScheduleKind, JobState, JobTarget, Method,
    Request, ResponseResult,
};
use crate::client::shell::feed_state::FeedItems;

const COLS: u16 = 106;
const ROWS: u16 = 40;

fn summary(job_id: &str, title: &str, count: u32, unread: u32, revision: u64) -> FeedSummary {
    FeedSummary {
        job_id: job_id.into(),
        title: title.into(),
        count,
        unread,
        revision,
        stale: false,
        loaded: true,
        fetched_unix_ms: Some(crate::jobs::now_unix_ms() - 120_000),
        error: None,
        refreshing: false,
        diagnostics: Vec::new(),
        groups: Vec::new(),
        last_action: None,
    }
}

fn item(id: &str, title: &str, state: FeedItemState) -> FeedItemInfo {
    FeedItemInfo {
        id: id.into(),
        title: title.into(),
        subtitle: Some("jira · mention".into()),
        state,
        badge: Some(id.to_uppercase()),
        url: Some(format!("https://example.com/{id}")),
        updated_at: None,
        body: None,
        fields: Vec::new(),
        group: None,
        actions: vec![
            FeedActionInfo {
                key: "d".into(),
                title: "done".into(),
            },
            FeedActionInfo {
                key: "s".into(),
                title: "seen".into(),
            },
        ],
    }
}

/// Five items for the first feed: two unread.
fn today_items() -> Vec<FeedItemInfo> {
    vec![
        item("nh-1", "Fix login", FeedItemState::Unread),
        item("nh-2", "Review retry", FeedItemState::Unread),
        item("nh-3", "Reply Dana", FeedItemState::Read),
        item("nh-4", "Update runbook", FeedItemState::Done),
        item("nh-5", "Check flaky", FeedItemState::Warn),
    ]
}

fn load(state: &mut ClientShellState, feeds: Vec<(FeedSummary, Vec<FeedItemInfo>)>) {
    state.feeds.endpoint = Some(ClientEndpointId::Local);
    state.feeds.unsupported = false;
    state.feeds.feeds.clear();
    state.feeds.items.clear();
    for (summary, items) in feeds {
        state.feeds.items.insert(
            summary.job_id.clone(),
            FeedItems {
                revision: summary.revision,
                items,
            },
        );
        state.feeds.feeds.push(summary);
    }
}

fn workbox_feed() -> (FeedSummary, Vec<FeedItemInfo>) {
    (
        summary("workbox-sync", "Today (workbox)", 5, 2, 3),
        today_items(),
    )
}

fn new_state(config: &Config) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(config));
    state.set_snapshot(Box::new(two_workspace_agent_snapshot()));
    state.set_pane_surface(surface());
    state
}

fn compose(state: &mut ClientShellState) -> String {
    frame_rows(&state.compose(COLS, ROWS).expect("frame").frame).join("\n")
}

fn state_with_feeds(
    config: &Config,
    feeds: Vec<(FeedSummary, Vec<FeedItemInfo>)>,
) -> ClientShellState {
    let mut state = new_state(config);
    state.compose(COLS, ROWS).expect("frame");
    load(&mut state, feeds);
    state.compose(COLS, ROWS).expect("frame");
    state
}

fn click(state: &mut ClientShellState, kind: MouseEventKind, rect: Rect) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind,
        column: rect.x + 2,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })])
}

fn left_click(state: &mut ClientShellState, rect: Rect) -> ClientShellInput {
    click(state, MouseEventKind::Down(MouseButton::Left), rect)
}

fn right_click(state: &mut ClientShellState, rect: Rect) -> ClientShellInput {
    click(state, MouseEventKind::Down(MouseButton::Right), rect)
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

fn feed_list_result(feeds: Vec<FeedSummary>, revision: u64) -> ResponseResult {
    ResponseResult::FeedList {
        revision,
        unchanged: false,
        feeds,
    }
}

fn detail_result(feed: FeedSummary, items: Vec<FeedItemInfo>) -> ResponseResult {
    ResponseResult::FeedDetail {
        feed,
        unchanged: false,
        items,
    }
}

fn key(code: KeyCode) -> RawInputEvent {
    RawInputEvent::Key(crate::input::TerminalKey::new(code, KeyModifiers::empty()))
}

fn press(state: &mut ClientShellState, ch: char) -> ClientShellInput {
    state.handle_raw_events(vec![key(KeyCode::Char(ch))])
}

fn press_shifted(state: &mut ClientShellState, ch: char) -> ClientShellInput {
    press(state, ch)
}

fn open_overlay(state: &mut ClientShellState) {
    state.handle_raw_events(vec![
        RawInputEvent::Key(crate::input::TerminalKey::new(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
        )),
        key(KeyCode::Char('f')),
    ]);
    assert!(
        matches!(state.overlay, Some(ClientShellOverlay::Feed(_))),
        "prefix+f opens the Feed overlay"
    );
}

fn feed_rows_text(state: &ClientShellState, frame: &str) -> Vec<String> {
    let area = state.hits.feed.area;
    frame
        .lines()
        .skip(area.y as usize)
        .take(area.height as usize)
        .map(|line| {
            line.chars()
                .skip(area.x as usize)
                .take(area.width as usize)
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

// ----- sidebar section ----------------------------------------------------

#[test]
fn the_section_sits_below_the_agents_with_an_unread_count() {
    let mut state = state_with_feeds(&Config::default(), vec![workbox_feed()]);
    let frame = compose(&mut state);
    let lines = feed_rows_text(&state, &frame);
    assert!(lines[0].starts_with('─'), "separator first: {lines:?}");
    assert_eq!(
        lines[1], " ▾ Today (workbox) 2",
        "header with the unread count"
    );
    assert_eq!(lines[2], "  ● NH-1 Fix login");
    assert_eq!(lines[3], "  ● NH-2 Review retry");
    assert_eq!(lines[4], "  ○ NH-3 Reply Dana");
    assert_eq!(lines[5], "  ✓ NH-4 Update runbook");
    assert_eq!(lines[6], "  ▲ NH-5 Check flaky");
    assert_eq!(lines.len(), 7, "nothing after the last row");
    // Below the agent panel, not over it.
    let agents = state.hits.agent_body;
    assert!(state.hits.feed.area.y >= agents.bottom());
    assert!(frame.contains("agents"));
}

#[test]
fn a_feed_with_everything_read_shows_the_total_dimmed() {
    let mut feed = workbox_feed();
    feed.0.unread = 0;
    let mut state = state_with_feeds(&Config::default(), vec![feed]);
    let frame = compose(&mut state);
    assert_eq!(feed_rows_text(&state, &frame)[1], " ▾ Today (workbox) (5)");
}

#[test]
fn rows_beyond_max_rows_become_a_more_row_that_opens_the_overlay() {
    let mut config = Config::default();
    config.ui.sidebar.feeds.max_rows = 2;
    let mut state = state_with_feeds(&config, vec![workbox_feed()]);
    let frame = compose(&mut state);
    let lines = feed_rows_text(&state, &frame);
    assert_eq!(lines[2], "  ● NH-1 Fix login");
    assert_eq!(lines[3], "  ● NH-2 Review retry");
    assert_eq!(lines[4], "   +3 more");
    assert_eq!(state.hits.feed.rows.len(), 2);
    let (rect, job_id) = state.hits.feed.more[0].clone();
    assert_eq!(job_id, "workbox-sync");
    left_click(&mut state, rect);
    match &state.overlay {
        Some(ClientShellOverlay::Feed(overlay)) => {
            assert_eq!(overlay.job_id.as_deref(), Some("workbox-sync"));
        }
        other => panic!("expected the Feed overlay, got {other:?}"),
    }
}

#[test]
fn titles_are_truncated_to_the_sidebar_width() {
    let mut feed = workbox_feed();
    feed.1[0].title = "A very long item title that cannot possibly fit in the sidebar".into();
    feed.0.title = "An equally long feed title for the sidebar header".into();
    let mut state = state_with_feeds(&Config::default(), vec![feed]);
    let frame = compose(&mut state);
    let lines = feed_rows_text(&state, &frame);
    let width = state.hits.feed.area.width as usize;
    for line in &lines {
        assert!(line.chars().count() <= width, "{line:?} overflows {width}");
    }
    assert!(
        lines[1].ends_with(" 2"),
        "the count stays visible: {:?}",
        lines[1]
    );
    assert!(lines[1].contains('…'));
    assert!(lines[2].ends_with('…'));
}

#[test]
fn stale_and_refreshing_feeds_are_flagged_in_the_header() {
    let mut stale = workbox_feed();
    stale.0.stale = true;
    stale.0.error = Some("exit 1".into());
    let mut state = state_with_feeds(&Config::default(), vec![stale]);
    let frame = compose(&mut state);
    assert_eq!(feed_rows_text(&state, &frame)[1], " ▾ Today (workbox) 2 !");

    let mut refreshing = workbox_feed();
    refreshing.0.refreshing = true;
    let mut state = state_with_feeds(&Config::default(), vec![refreshing]);
    let frame = compose(&mut state);
    assert_eq!(feed_rows_text(&state, &frame)[1], " ▾ Today (workbox) 2 …");
}

#[test]
fn a_feed_that_failed_before_loading_shows_only_its_header() {
    let mut broken = summary("news", "News", 0, 0, 1);
    broken.loaded = false;
    broken.fetched_unix_ms = None;
    broken.error = Some("not valid JSON".into());
    let mut state = state_with_feeds(&Config::default(), vec![(broken, Vec::new())]);
    let frame = compose(&mut state);
    let lines = feed_rows_text(&state, &frame);
    assert_eq!(
        lines,
        ["─".repeat(lines[0].chars().count()), " ▾ News (0) !".into()]
    );
    // A feed with nothing read yet and no error stays out of the sidebar.
    let mut quiet = summary("quiet", "Quiet", 0, 0, 1);
    quiet.loaded = false;
    quiet.fetched_unix_ms = None;
    let mut state = state_with_feeds(&Config::default(), vec![(quiet, Vec::new())]);
    compose(&mut state);
    assert!(state.hits.feed.area.is_empty());
}

#[test]
fn feeds_can_be_chosen_or_turned_off_in_config() {
    let mut config = Config::default();
    config.ui.sidebar.feeds = toml::from_str("show = [\"workbox-sync\"]").expect("feeds config");
    let other = (
        summary("other", "Other feed", 1, 1, 1),
        vec![item("o-1", "Other", FeedItemState::Unread)],
    );
    let mut state = state_with_feeds(&config, vec![workbox_feed(), other]);
    let frame = compose(&mut state);
    assert!(frame.contains("Today (workbox)"));
    assert!(!frame.contains("Other feed"));
    assert_eq!(state.hits.feed.headers.len(), 1);

    let mut config = Config::default();
    config.ui.sidebar.feeds.enabled = false;
    let mut state = state_with_feeds(&config, vec![workbox_feed()]);
    let frame = compose(&mut state);
    assert!(!frame.contains("Today (workbox)"));
    assert!(state.hits.feed.area.is_empty());
}

#[test]
fn several_feeds_stack_with_their_own_headers() {
    let other = (
        summary("news", "News", 2, 1, 1),
        vec![
            item("n-1", "First story", FeedItemState::Unread),
            item("n-2", "Second story", FeedItemState::Read),
        ],
    );
    let mut state = state_with_feeds(&Config::default(), vec![workbox_feed(), other]);
    let frame = compose(&mut state);
    let lines = feed_rows_text(&state, &frame);
    let headers: Vec<_> = lines.iter().filter(|line| line.starts_with(" ▾")).collect();
    assert_eq!(headers, [&" ▾ Today (workbox) 2", &" ▾ News 1"]);
    assert_eq!(state.hits.feed.headers.len(), 2);
}

#[test]
fn the_section_never_squeezes_the_agent_panel_below_its_minimum() {
    let mut state = new_state(&Config::default());
    state.compose(COLS, 16).expect("frame");
    load(&mut state, vec![workbox_feed()]);
    state.compose(COLS, 16).expect("frame");
    // A short terminal leaves no spare rows: no feed section at all.
    assert!(state.hits.feed.area.is_empty());
    assert!(!state.hits.agent_body.is_empty());
}

// ----- agent sidebar is not disturbed -------------------------------------

#[test]
fn without_feed_data_the_agent_sidebar_hits_are_exactly_what_they_were() {
    let mut baseline = new_state(&Config::default());
    baseline.compose(COLS, ROWS).expect("frame");
    let mut with_feature = new_state(&Config::default());
    with_feature.compose(COLS, ROWS).expect("frame");
    // Feed state present but nothing loaded for this endpoint yet.
    with_feature.feeds.endpoint = Some(ClientEndpointId::Local);
    with_feature.compose(COLS, ROWS).expect("frame");
    assert_eq!(baseline.hits.agent_body, with_feature.hits.agent_body);
    assert_eq!(baseline.hits.agents, with_feature.hits.agents);
    assert_eq!(
        baseline.hits.agent_headers.len(),
        with_feature.hits.agent_headers.len()
    );
    assert!(with_feature.hits.feed.area.is_empty());

    // Disabling the feature with data loaded is identical to never having it.
    let mut config = Config::default();
    config.ui.sidebar.feeds.enabled = false;
    let mut disabled = state_with_feeds(&config, vec![workbox_feed()]);
    disabled.compose(COLS, ROWS).expect("frame");
    assert_eq!(baseline.hits.agent_body, disabled.hits.agent_body);
    assert_eq!(baseline.hits.agents, disabled.hits.agents);
}

#[test]
fn feed_rows_do_not_move_agent_rows_and_the_two_never_overlap() {
    let mut baseline = new_state(&Config::default());
    baseline.compose(COLS, ROWS).expect("frame");
    let mut with_feeds = state_with_feeds(&Config::default(), vec![workbox_feed()]);
    with_feeds.compose(COLS, ROWS).expect("frame");

    // The agent list starts where it started and keeps every row's position.
    assert_eq!(baseline.hits.agent_body.y, with_feeds.hits.agent_body.y);
    assert_eq!(baseline.hits.agent_body.x, with_feeds.hits.agent_body.x);
    assert_eq!(baseline.hits.agents, with_feeds.hits.agents);
    assert_eq!(
        baseline.hits.agent_headers.len(),
        with_feeds.hits.agent_headers.len()
    );
    for (before, after) in baseline
        .hits
        .agent_headers
        .iter()
        .zip(&with_feeds.hits.agent_headers)
    {
        assert_eq!(before.rect, after.rect);
    }
    // Every feed hit lies under the agent area, and none overlaps an agent hit.
    let agent_area = with_feeds.hits.agent_body;
    let feed_hits: Vec<Rect> = with_feeds
        .hits
        .feed
        .headers
        .iter()
        .map(|hit| hit.rect)
        .chain(with_feeds.hits.feed.rows.iter().map(|hit| hit.rect))
        .collect();
    assert!(!feed_hits.is_empty());
    for rect in feed_hits {
        assert!(rect.y >= agent_area.bottom(), "{rect:?} vs {agent_area:?}");
        for (agent_rect, _) in &with_feeds.hits.agents {
            assert!(rect.y >= agent_rect.bottom());
        }
    }
}

#[test]
fn clicking_and_scrolling_the_agent_list_still_work_with_feeds_showing() {
    let mut state = state_with_feeds(&Config::default(), vec![workbox_feed()]);
    let third = state.hits.agents[2].0;
    let input = left_click(&mut state, third);
    let [ClientShellAction::Endpoint { request, .. }] = &input.actions[..] else {
        panic!(
            "agent click should focus through the endpoint API: {:?}",
            input.actions
        );
    };
    assert!(matches!(
        &request.method,
        Method::PaneFocus(target) if target.pane_id == "pane_3"
    ));
    // A click on an agent group header still toggles that group.
    let header = state.hits.agent_headers[0].rect;
    left_click(&mut state, header);
    state.compose(COLS, ROWS).expect("frame");
    assert!(state.hits.agent_headers[0].collapsed);
    // And the feed section is untouched by it.
    assert_eq!(state.hits.feed.headers.len(), 1);
    assert!(!state.hits.feed.headers[0].collapsed);
}

#[test]
fn right_clicking_an_agent_still_opens_the_agent_menu() {
    let mut state = state_with_feeds(&Config::default(), vec![workbox_feed()]);
    let first = state.hits.agents[0].0;
    right_click(&mut state, first);
    assert!(matches!(
        &state.overlay,
        Some(ClientShellOverlay::ContextMenu(menu))
            if matches!(menu.target, ClientContextMenuTarget::Agent { .. })
    ));
}

#[test]
fn the_collapse_toggle_stays_visible_and_clickable_under_the_feed_section() {
    let mut state = state_with_feeds(&Config::default(), vec![workbox_feed()]);
    let frame = state.compose(COLS, ROWS).expect("frame");
    let toggle = state.hits.sidebar_toggle;
    let buffer = frame.frame.to_ratatui_buffer().expect("buffer");
    assert_eq!(buffer[(toggle.x, toggle.y)].symbol(), "«");
    assert!(
        state.hits.feed.area.bottom() < toggle.y + 1,
        "the section stops above the toggle row"
    );
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: toggle.x,
        row: toggle.y,
        modifiers: KeyModifiers::NONE,
    })]);
    assert!(state.sidebar_collapsed);
    // A collapsed sidebar draws no section and stops polling for feeds.
    state.compose(COLS, ROWS).expect("frame");
    assert!(state.hits.feed.area.is_empty());
    assert!(!state.feeds.sidebar_visible);
}

#[test]
fn the_section_also_shows_in_the_multi_machine_sidebar_without_moving_agents() {
    let (mut state, _) = state_with_remote();
    state.compose(COLS, ROWS).expect("frame");
    let baseline_agents = state.hits.endpoint_agents.clone();
    let baseline_body = state.hits.agent_body;
    load(&mut state, vec![workbox_feed()]);
    let frame = compose(&mut state);
    assert!(frame.contains("▾ Today (workbox) 2"), "{frame}");
    assert!(!state.hits.feed.rows.is_empty());
    assert_eq!(state.hits.agent_body.y, baseline_body.y);
    assert_eq!(state.hits.endpoint_agents, baseline_agents);
    let toggle = state.hits.sidebar_toggle;
    assert!(state.hits.feed.area.bottom() <= toggle.y);

    // Clicking a row opens its link, as in the single-machine sidebar.
    let row = state.hits.feed.rows[0].rect;
    let input = left_click(&mut state, row);
    assert!(matches!(
        &input.actions[..],
        [ClientShellAction::OpenSafeWebUrl(_)]
    ));
}

// ----- clicks, menus, collapse -------------------------------------------

#[test]
fn clicking_an_item_opens_its_link_through_the_safe_url_action() {
    let mut state = state_with_feeds(&Config::default(), vec![workbox_feed()]);
    let row = state.hits.feed.rows[1].rect;
    let input = left_click(&mut state, row);
    assert!(matches!(
        &input.actions[..],
        [ClientShellAction::OpenSafeWebUrl(url)] if url == "https://example.com/nh-2"
    ));
    assert!(
        requests(&input).is_empty(),
        "no server call for opening a link"
    );
}

#[test]
fn clicking_an_item_without_a_link_opens_its_actions_menu() {
    let mut feed = workbox_feed();
    feed.1[0].url = None;
    let mut state = state_with_feeds(&Config::default(), vec![feed]);
    let row = state.hits.feed.rows[0].rect;
    let input = left_click(&mut state, row);
    assert!(input.actions.is_empty());
    assert!(matches!(
        &state.overlay,
        Some(ClientShellOverlay::ContextMenu(menu))
            if matches!(menu.target, ClientContextMenuTarget::Feed { item: Some(_), .. })
    ));
    // An item with neither link nor actions does nothing.
    let mut feed = workbox_feed();
    feed.1[0].url = None;
    feed.1[0].actions.clear();
    let mut state = state_with_feeds(&Config::default(), vec![feed]);
    let row = state.hits.feed.rows[0].rect;
    left_click(&mut state, row);
    assert!(state.overlay.is_none());
}

#[test]
fn right_clicking_an_item_lists_its_actions_and_runs_the_chosen_one() {
    let mut state = state_with_feeds(&Config::default(), vec![workbox_feed()]);
    let row = state.hits.feed.rows[0].rect;
    right_click(&mut state, row);
    let labels: Vec<String> = match &state.overlay {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .iter()
            .map(|item| item.label.to_string())
            .collect(),
        other => panic!("expected a context menu, got {other:?}"),
    };
    assert_eq!(
        labels,
        [
            "Open link",
            "done (d)",
            "seen (s)",
            "Refresh feed",
            "Sync now (run job)",
            "Open feed",
            "Collapse"
        ]
    );
    // The menu renders with the action titles.
    let frame = compose(&mut state);
    assert!(frame.contains("done (d)"), "{frame}");

    // Choose "seen (s)".
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(2, &mut outcome);
    let sent = requests(&outcome);
    assert_eq!(
        sent[0].method,
        Method::FeedRunAction(FeedRunActionParams {
            job_id: "workbox-sync".into(),
            item_id: "nh-1".into(),
            key: "s".into(),
        })
    );
    assert!(state.overlay.is_none());
}

#[test]
fn the_context_menu_can_open_the_link_refresh_or_open_the_overlay() {
    let mut state = state_with_feeds(&Config::default(), vec![workbox_feed()]);
    let row = state.hits.feed.rows[0].rect;
    right_click(&mut state, row);
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(0, &mut outcome);
    assert!(matches!(
        &outcome.actions[..],
        [ClientShellAction::OpenSafeWebUrl(url)] if url == "https://example.com/nh-1"
    ));

    right_click(&mut state, row);
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(3, &mut outcome);
    assert_eq!(
        requests(&outcome)[0].method,
        Method::FeedRefresh(FeedTarget {
            job_id: "workbox-sync".into()
        })
    );

    right_click(&mut state, row);
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(4, &mut outcome);
    assert_eq!(
        requests(&outcome)[0].method,
        Method::JobRun(JobTarget {
            job_id: "workbox-sync".into()
        })
    );

    right_click(&mut state, row);
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(5, &mut outcome);
    assert!(matches!(state.overlay, Some(ClientShellOverlay::Feed(_))));
}

#[test]
fn right_clicking_a_header_offers_refresh_open_and_collapse() {
    let mut state = state_with_feeds(&Config::default(), vec![workbox_feed()]);
    let header = state.hits.feed.headers[0].rect;
    right_click(&mut state, header);
    let labels: Vec<String> = match &state.overlay {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .iter()
            .map(|item| item.label.to_string())
            .collect(),
        other => panic!("expected a context menu, got {other:?}"),
    };
    assert_eq!(
        labels,
        [
            "Refresh feed",
            "Sync now (run job)",
            "Open feed",
            "Collapse"
        ]
    );
}

#[test]
fn clicking_the_header_collapses_and_expands_the_section() {
    let mut state = state_with_feeds(&Config::default(), vec![workbox_feed()]);
    let header = state.hits.feed.headers[0].rect;
    let input = left_click(&mut state, header);
    assert!(input.repaint);
    let frame = compose(&mut state);
    let lines = feed_rows_text(&state, &frame);
    assert_eq!(
        lines[1], " ▸ Today (workbox) 2",
        "collapsed marker, count stays"
    );
    assert_eq!(lines.len(), 2, "only the header remains");
    assert!(state.hits.feed.rows.is_empty());
    assert!(state.hits.feed.headers[0].collapsed);

    let header = state.hits.feed.headers[0].rect;
    left_click(&mut state, header);
    let frame = compose(&mut state);
    assert!(feed_rows_text(&state, &frame)[1].starts_with(" ▾"));
    assert_eq!(state.hits.feed.rows.len(), 5);
}

#[test]
fn collapse_state_persists_across_client_restarts_under_a_feed_key() {
    let path = std::env::temp_dir().join(format!(
        "flockr-shell-feed-collapse-{}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let config =
        ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone());
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(two_workspace_agent_snapshot()));
    state.set_pane_surface(surface());
    state.compose(COLS, ROWS).expect("frame");
    load(&mut state, vec![workbox_feed()]);
    state.compose(COLS, ROWS).expect("frame");
    let header = state.hits.feed.headers[0].rect;
    left_click(&mut state, header);

    let saved = std::fs::read_to_string(&path).expect("preferences written");
    assert!(saved.contains("feed:workbox-sync"), "{saved}");

    let restored_config =
        ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone());
    let mut restored = ClientShellState::new(restored_config);
    restored.set_snapshot(Box::new(two_workspace_agent_snapshot()));
    restored.set_pane_surface(surface());
    restored.compose(COLS, ROWS).expect("frame");
    load(&mut restored, vec![workbox_feed()]);
    restored.compose(COLS, ROWS).expect("frame");
    assert!(restored.hits.feed.headers[0].collapsed);
    assert!(restored.hits.feed.rows.is_empty());

    // Expanding again is persisted too, and agent groups are not touched.
    let header = restored.hits.feed.headers[0].rect;
    left_click(&mut restored, header);
    let reloaded = ClientShellState::new(
        ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone()),
    );
    assert!(reloaded.collapsed_groups.is_empty());
    std::fs::remove_file(path).expect("remove preferences");
}

#[test]
fn collapsing_all_agent_groups_leaves_feed_sections_alone() {
    let mut state = state_with_feeds(&Config::default(), vec![workbox_feed()]);
    state.collapsed_groups.insert("feed:workbox-sync".into());
    let mut outcome = ClientShellInput::default();
    state.set_all_agent_groups(true, &mut outcome);
    state.set_all_agent_groups(false, &mut outcome);
    assert!(
        state.collapsed_groups.contains("feed:workbox-sync"),
        "expand-all only clears agent groups"
    );
}

// ----- polling ------------------------------------------------------------

fn tick(state: &mut ClientShellState, at: Instant) -> ClientShellInput {
    let mut outcome = ClientShellInput::default();
    state.tick_feeds(at, &mut outcome);
    outcome
}

#[test]
fn the_sidebar_polls_with_its_revision_at_a_modest_cadence() {
    let mut state = new_state(&Config::default());
    state.compose(COLS, ROWS).expect("frame");
    assert!(state.feeds.sidebar_visible);
    let start = Instant::now();
    let first = requests(&tick(&mut state, start));
    assert_eq!(first.len(), 1);
    assert_eq!(
        first[0].method,
        Method::FeedList(FeedListParams {
            since_revision: None
        })
    );
    // Not again while the answer is outstanding.
    assert!(requests(&tick(&mut state, start + Duration::from_secs(30))).is_empty());

    answer(
        &mut state,
        &first[0],
        feed_list_result(vec![workbox_feed().0], 9),
    );
    // Within five seconds of the last poll: quiet.
    assert!(requests(&tick(&mut state, start + Duration::from_secs(2))).is_empty());
    // After the gap: asks again with the revision it holds.
    let next = requests(&tick(&mut state, start + Duration::from_secs(6)));
    assert_eq!(
        next[0].method,
        Method::FeedList(FeedListParams {
            since_revision: Some(9)
        })
    );
}

#[test]
fn nothing_is_polled_unless_the_expanded_sidebar_or_an_overlay_needs_feeds() {
    let mut state = new_state(&Config::default());
    // Never composed: the sidebar has not been drawn.
    assert!(requests(&tick(&mut state, Instant::now())).is_empty());
    state.compose(COLS, ROWS).expect("frame");
    assert_eq!(requests(&tick(&mut state, Instant::now())).len(), 1);

    // A collapsed sidebar shows no section, so no polling.
    let mut state = new_state(&Config::default());
    state.sidebar_collapsed = true;
    state.compose(COLS, ROWS).expect("frame");
    assert!(!state.feeds.sidebar_visible);
    assert!(requests(&tick(&mut state, Instant::now())).is_empty());

    // Turned off in config: no polling either.
    let mut config = Config::default();
    config.ui.sidebar.feeds.enabled = false;
    let mut state = new_state(&config);
    state.compose(COLS, ROWS).expect("frame");
    assert!(requests(&tick(&mut state, Instant::now())).is_empty());
}

#[test]
fn a_server_without_feed_methods_is_left_alone_without_a_notice() {
    let mut state = new_state(&Config::default());
    state.set_endpoint_methods(Some(vec!["pane.read".into()]));
    state.compose(COLS, ROWS).expect("frame");
    assert!(requests(&tick(&mut state, Instant::now())).is_empty());
    assert!(state.feeds.unsupported);
    assert!(state.endpoint_notice_seen.is_empty());
    assert!(state.visible_endpoint_notice.is_none());
    // And never again.
    assert!(requests(&tick(&mut state, Instant::now() + Duration::from_secs(60))).is_empty());
    let frame = compose(&mut state);
    assert!(state.hits.feed.area.is_empty(), "{frame}");
}

#[test]
fn a_list_answer_fetches_items_only_for_expanded_visible_feeds() {
    let mut state = new_state(&Config::default());
    state.compose(COLS, ROWS).expect("frame");
    state.collapsed_groups.insert("feed:news".into());
    let list = requests(&tick(&mut state, Instant::now()));
    let feeds = vec![
        summary("workbox-sync", "Today (workbox)", 5, 2, 3),
        summary("news", "News", 4, 0, 5),
        summary("empty", "Empty", 0, 0, 1),
    ];
    let (_, actions) = answer(&mut state, &list[0], feed_list_result(feeds, 7));
    let sent: Vec<_> = actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some(request.method.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        sent,
        [Method::FeedGet(FeedGetParams {
            job_id: "workbox-sync".into(),
            since_revision: None,
            limit: None,
        })],
        "the collapsed and the empty feed are not fetched"
    );
}

#[test]
fn an_items_answer_fills_the_section_and_a_newer_revision_refetches() {
    let mut state = new_state(&Config::default());
    state.compose(COLS, ROWS).expect("frame");
    let list = requests(&tick(&mut state, Instant::now()));
    let (_, actions) = answer(
        &mut state,
        &list[0],
        feed_list_result(vec![workbox_feed().0], 7),
    );
    let get = actions
        .iter()
        .find_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some((**request).clone()),
            _ => None,
        })
        .expect("get request");
    let (repaint, _) = answer(
        &mut state,
        &get,
        detail_result(workbox_feed().0, today_items()),
    );
    assert!(repaint);
    let frame = compose(&mut state);
    assert!(frame.contains("Fix login"), "{frame}");

    // A list with a newer feed revision fetches again, with the held revision.
    let later = Instant::now() + Duration::from_secs(10);
    let list = requests(&tick(&mut state, later));
    let mut newer = workbox_feed().0;
    newer.revision = 4;
    let (_, actions) = answer(&mut state, &list[0], feed_list_result(vec![newer], 8));
    let method = actions
        .iter()
        .find_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some(request.method.clone()),
            _ => None,
        })
        .expect("refetch");
    assert_eq!(
        method,
        Method::FeedGet(FeedGetParams {
            job_id: "workbox-sync".into(),
            since_revision: Some(3),
            limit: None,
        })
    );
}

#[test]
fn an_unchanged_list_keeps_what_the_client_holds() {
    let mut state = state_with_feeds(&Config::default(), vec![workbox_feed()]);
    state.feeds.list_revision = Some(7);
    let list = requests(&tick(&mut state, Instant::now()));
    assert_eq!(
        list[0].method,
        Method::FeedList(FeedListParams {
            since_revision: Some(7)
        })
    );
    answer(
        &mut state,
        &list[0],
        ResponseResult::FeedList {
            revision: 7,
            unchanged: true,
            feeds: Vec::new(),
        },
    );
    assert_eq!(state.feeds.feeds.len(), 1);
    assert_eq!(state.feeds.items_of("workbox-sync").len(), 5);
}

#[test]
fn switching_machines_drops_the_other_machines_feeds() {
    let mut state = state_with_feeds(&Config::default(), vec![workbox_feed()]);
    assert_eq!(state.feeds.feeds.len(), 1);
    let other = ClientEndpointId::Ssh(crate::client::endpoint::ProfileId::generate());
    state.active_endpoint_id = other.clone();
    let mut outcome = ClientShellInput::default();
    state.tick_feeds(Instant::now(), &mut outcome);
    assert!(state.feeds.feeds.is_empty());
    assert_eq!(state.feeds.endpoint, Some(other));
}

#[test]
fn collapsing_a_section_asks_for_its_items_soon() {
    let mut state = new_state(&Config::default());
    state.compose(COLS, ROWS).expect("frame");
    load(
        &mut state,
        vec![(summary("news", "News", 3, 1, 4), Vec::new())],
    );
    state.feeds.items.clear();
    state.collapsed_groups.insert("feed:news".into());
    state.compose(COLS, ROWS).expect("frame");
    let header = state.hits.feed.headers[0].rect;
    left_click(&mut state, header);
    let out = tick(&mut state, Instant::now());
    let sent = requests(&out);
    assert_eq!(sent.len(), 1, "a list poll right after expanding");
    assert!(matches!(sent[0].method, Method::FeedList(_)));
}

// ----- action results -----------------------------------------------------

#[test]
fn a_failed_action_started_here_raises_one_notice_when_the_outcome_arrives() {
    let mut state = state_with_feeds(&Config::default(), vec![workbox_feed()]);
    let row = state.hits.feed.rows[0].rect;
    right_click(&mut state, row);
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(1, &mut outcome);
    let sent = requests(&outcome);
    answer(
        &mut state,
        &sent[0],
        ResponseResult::FeedActionRequested {
            job_id: "workbox-sync".into(),
            item_id: "nh-1".into(),
            key: "d".into(),
            started: true,
            reason: None,
        },
    );
    // The next list carries the failure.
    let mut failed = workbox_feed().0;
    failed.last_action = Some(FeedActionResult {
        item_id: "nh-1".into(),
        key: "d".into(),
        ok: false,
        summary: "exit 2: no network".into(),
        finished_unix_ms: crate::jobs::now_unix_ms(),
    });
    let list = requests(&tick(&mut state, Instant::now() + Duration::from_secs(2)));
    answer(
        &mut state,
        &list[0],
        feed_list_result(vec![failed.clone()], 11),
    );
    let notice = state.visible_endpoint_notice.as_ref().expect("notice");
    assert!(
        notice.body.contains("exit 2: no network"),
        "{}",
        notice.body
    );
    // The same outcome is not announced again.
    state.visible_endpoint_notice = None;
    let list = requests(&tick(&mut state, Instant::now() + Duration::from_secs(8)));
    answer(&mut state, &list[0], feed_list_result(vec![failed], 12));
    assert!(state.visible_endpoint_notice.is_none());
}

#[test]
fn a_busy_server_reply_to_an_action_is_reported() {
    let mut state = state_with_feeds(&Config::default(), vec![workbox_feed()]);
    let row = state.hits.feed.rows[0].rect;
    right_click(&mut state, row);
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(1, &mut outcome);
    let sent = requests(&outcome);
    answer(
        &mut state,
        &sent[0],
        ResponseResult::FeedActionRequested {
            job_id: "workbox-sync".into(),
            item_id: "nh-1".into(),
            key: "d".into(),
            started: false,
            reason: Some("the feed is already refreshing".into()),
        },
    );
    let notice = state.visible_endpoint_notice.as_ref().expect("notice");
    assert!(
        notice.body.contains("already refreshing"),
        "{}",
        notice.body
    );
}

// ----- overlay ------------------------------------------------------------

fn two_feeds() -> Vec<(FeedSummary, Vec<FeedItemInfo>)> {
    vec![
        workbox_feed(),
        (
            summary("news", "News", 3, 1, 4),
            vec![
                item("n-1", "Rust 2027 edition", FeedItemState::Unread),
                item("n-2", "Zig release", FeedItemState::Read),
                item("n-3", "Kernel update", FeedItemState::Read),
            ],
        ),
    ]
}

fn overlay_state(feeds: Vec<(FeedSummary, Vec<FeedItemInfo>)>) -> ClientShellState {
    let mut state = new_state(&Config::default());
    state.compose(COLS, ROWS).expect("frame");
    load(&mut state, feeds);
    open_overlay(&mut state);
    state
}

fn selected_item(state: &ClientShellState) -> String {
    state.selected_feed_item().expect("selected item").id
}

#[test]
fn prefix_f_is_a_free_default_and_opens_the_overlay() {
    let config = Config::default();
    assert!(config.collect_diagnostics().is_empty());
    assert!(matches!(
        crate::input::resolve_prefix_binding(
            &config.keybinds(),
            &crate::input::TerminalKey::new(KeyCode::Char('f'), KeyModifiers::empty())
        ),
        Some(crate::input::KeybindMatch::Action(
            crate::input::KeybindAction::Feed
        ))
    ));
    let mut state = overlay_state(two_feeds());
    let outcome = state.handle_raw_events(vec![key(KeyCode::Esc)]);
    assert!(state.overlay.is_none() && outcome.repaint);
}

#[test]
fn opening_polls_at_once_and_then_every_two_seconds() {
    let mut state = new_state(&Config::default());
    state.compose(COLS, ROWS).expect("frame");
    state.sidebar_collapsed = true;
    state.compose(COLS, ROWS).expect("frame");
    open_overlay(&mut state);
    let start = Instant::now();
    let first = requests(&tick(&mut state, start));
    assert_eq!(first.len(), 1, "polls even with the sidebar collapsed");
    answer(
        &mut state,
        &first[0],
        feed_list_result(vec![workbox_feed().0], 3),
    );
    // poll_soon sets a fast window, so wait it out to see the overlay pace.
    state.feeds.fast_until = None;
    assert!(requests(&tick(&mut state, start + Duration::from_millis(1500))).is_empty());
    assert_eq!(
        requests(&tick(&mut state, start + Duration::from_millis(2100))).len(),
        1
    );
}

#[test]
fn the_overlay_shows_tabs_counts_and_the_selected_items_actions() {
    let mut state = overlay_state(two_feeds());
    let frame = compose(&mut state);
    for expected in [
        "feed",
        "Today (workbox) 2",
        "News 1",
        "5 items · 2 unread · updated 2m ago",
        "NH-1 Fix login",
        "NH-5 Check flaky",
        "d done",
        "s seen",
        "enter open",
        "R refresh",
    ] {
        assert!(frame.contains(expected), "missing {expected:?} in\n{frame}");
    }
    assert_eq!(state.hits.feed_overlay.tabs.len(), 2);
    assert_eq!(state.hits.feed_overlay.rows.len(), 5);
}

#[test]
fn tab_and_the_arrow_keys_switch_feeds_and_clicking_a_tab_does_too() {
    let mut state = overlay_state(two_feeds());
    compose(&mut state);
    assert_eq!(selected_item(&state), "nh-1");
    state.handle_raw_events(vec![key(KeyCode::Tab)]);
    let frame = compose(&mut state);
    assert!(frame.contains("Rust 2027 edition"), "{frame}");
    assert_eq!(selected_item(&state), "n-1");
    state.handle_raw_events(vec![key(KeyCode::Tab)]);
    assert!(compose(&mut state).contains("Fix login"), "wraps around");
    state.handle_raw_events(vec![key(KeyCode::Left)]);
    assert!(compose(&mut state).contains("Rust 2027 edition"));
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::BackTab,
        KeyModifiers::SHIFT,
    ))]);
    assert!(compose(&mut state).contains("Fix login"));

    let (rect, job_id) = state.hits.feed_overlay.tabs[1].clone();
    assert_eq!(job_id, "news");
    left_click(&mut state, rect);
    assert!(compose(&mut state).contains("Rust 2027 edition"));
}

#[test]
fn a_single_feed_shows_its_title_instead_of_tabs() {
    let mut state = overlay_state(vec![workbox_feed()]);
    let frame = compose(&mut state);
    assert!(frame.contains("Today (workbox)"));
    assert!(state.hits.feed_overlay.tabs.is_empty());
    // Tab does nothing with one feed.
    state.handle_raw_events(vec![key(KeyCode::Tab)]);
    assert_eq!(selected_item(&state), "nh-1");
}

#[test]
fn selection_moves_with_arrows_and_vim_keys() {
    let mut state = overlay_state(vec![workbox_feed()]);
    compose(&mut state);
    press(&mut state, 'j');
    assert_eq!(selected_item(&state), "nh-2");
    state.handle_raw_events(vec![key(KeyCode::Down)]);
    assert_eq!(selected_item(&state), "nh-3");
    press(&mut state, 'k');
    assert_eq!(selected_item(&state), "nh-2");
    press_shifted(&mut state, 'G');
    assert_eq!(selected_item(&state), "nh-5");
    press(&mut state, 'g');
    assert_eq!(selected_item(&state), "nh-1");
}

#[test]
fn enter_opens_the_link_and_an_item_without_one_says_so() {
    let mut feed = workbox_feed();
    feed.1[1].url = None;
    let mut state = overlay_state(vec![feed]);
    compose(&mut state);
    let input = state.handle_raw_events(vec![key(KeyCode::Enter)]);
    assert!(matches!(
        &input.actions[..],
        [ClientShellAction::OpenSafeWebUrl(url)] if url == "https://example.com/nh-1"
    ));
    press(&mut state, 'j');
    let input = state.handle_raw_events(vec![key(KeyCode::Enter)]);
    assert!(input.actions.is_empty());
    assert!(compose(&mut state).contains("nh-2 has no link"));
}

#[test]
fn action_keys_run_the_selected_items_actions_through_the_server() {
    let mut state = overlay_state(vec![workbox_feed()]);
    compose(&mut state);
    press(&mut state, 'j');
    let input = press(&mut state, 's');
    let sent = requests(&input);
    assert_eq!(
        sent[0].method,
        Method::FeedRunAction(FeedRunActionParams {
            job_id: "workbox-sync".into(),
            item_id: "nh-2".into(),
            key: "s".into(),
        })
    );
    // A key that is not one of the item's actions does nothing.
    let input = press(&mut state, 'z');
    assert!(requests(&input).is_empty() && !input.repaint);

    // The answer shows in the status line.
    answer(
        &mut state,
        &sent[0],
        ResponseResult::FeedActionRequested {
            job_id: "workbox-sync".into(),
            item_id: "nh-2".into(),
            key: "s".into(),
            started: true,
            reason: None,
        },
    );
    assert!(compose(&mut state).contains("running s on nh-2"));
    // The outcome arrives with the next list.
    let mut done = workbox_feed().0;
    done.last_action = Some(FeedActionResult {
        item_id: "nh-2".into(),
        key: "s".into(),
        ok: true,
        summary: "ok".into(),
        finished_unix_ms: crate::jobs::now_unix_ms() + 1,
    });
    let list = requests(&tick(&mut state, Instant::now() + Duration::from_secs(2)));
    answer(&mut state, &list[0], feed_list_result(vec![done], 12));
    assert!(compose(&mut state).contains("s on nh-2: ok"));
}

#[test]
fn shift_r_refreshes_the_current_feed() {
    let mut state = overlay_state(two_feeds());
    compose(&mut state);
    state.handle_raw_events(vec![key(KeyCode::Tab)]);
    let input = press_shifted(&mut state, 'R');
    assert_eq!(
        requests(&input)[0].method,
        Method::FeedRefresh(FeedTarget {
            job_id: "news".into()
        })
    );
    answer(
        &mut state,
        &requests(&input)[0],
        ResponseResult::FeedRefreshRequested {
            job_id: "news".into(),
            started: false,
            reason: Some("the feed is already refreshing".into()),
        },
    );
    assert!(compose(&mut state).contains("news: the feed is already refreshing"));
}

#[test]
fn slash_filters_by_title_subtitle_badge_and_id() {
    let mut state = overlay_state(vec![workbox_feed()]);
    compose(&mut state);
    press(&mut state, '/');
    for ch in "retry".chars() {
        press(&mut state, ch);
    }
    let frame = compose(&mut state);
    assert!(frame.contains("/ retry"), "{frame}");
    assert!(frame.contains("Review retry"));
    assert!(!frame.contains("Fix login"));
    assert_eq!(
        selected_item(&state),
        "nh-2",
        "selection jumps to the first match"
    );
    assert_eq!(state.hits.feed_overlay.rows.len(), 1);

    // Typing j/k/q while filtering types them; it does not move or close.
    press(&mut state, 'q');
    assert!(matches!(state.overlay, Some(ClientShellOverlay::Feed(_))));
    assert!(compose(&mut state).contains("No items match the filter."));
    state.handle_raw_events(vec![key(KeyCode::Backspace)]);
    assert!(compose(&mut state).contains("Review retry"));

    // Enter keeps the filter and returns to the list.
    state.handle_raw_events(vec![key(KeyCode::Enter)]);
    let input = press(&mut state, 'd');
    assert_eq!(requests(&input).len(), 1, "keys act on the list again");

    // The first Esc clears the filter, the second closes.
    state.handle_raw_events(vec![key(KeyCode::Esc)]);
    assert!(matches!(state.overlay, Some(ClientShellOverlay::Feed(_))));
    assert!(compose(&mut state).contains("Fix login"));
    state.handle_raw_events(vec![key(KeyCode::Esc)]);
    assert!(state.overlay.is_none());
}

#[test]
fn filtering_matches_the_badge_and_subtitle_case_insensitively() {
    let mut state = overlay_state(vec![workbox_feed()]);
    compose(&mut state);
    press(&mut state, '/');
    for ch in "NH-3".chars() {
        press_shifted_or_plain(&mut state, ch);
    }
    assert_eq!(state.visible_feed_items().len(), 1);
    state.handle_raw_events(vec![key(KeyCode::Esc)]);
    press(&mut state, '/');
    for ch in "MENTION".chars() {
        press_shifted_or_plain(&mut state, ch);
    }
    assert_eq!(
        state.visible_feed_items().len(),
        5,
        "every subtitle says mention"
    );
}

fn press_shifted_or_plain(state: &mut ClientShellState, ch: char) -> ClientShellInput {
    if ch.is_uppercase() {
        press_shifted(state, ch)
    } else {
        press(state, ch)
    }
}

#[test]
fn clicking_selects_a_second_click_opens_and_outside_closes() {
    let mut state = overlay_state(vec![workbox_feed()]);
    compose(&mut state);
    let (rect, _) = state.hits.feed_overlay.rows[2];
    let input = left_click(&mut state, rect);
    assert!(input.actions.is_empty());
    assert_eq!(selected_item(&state), "nh-3");
    let input = left_click(&mut state, rect);
    assert!(matches!(
        &input.actions[..],
        [ClientShellAction::OpenSafeWebUrl(url)] if url == "https://example.com/nh-3"
    ));
}

#[test]
fn the_wheel_moves_the_selection() {
    let mut state = overlay_state(vec![workbox_feed()]);
    compose(&mut state);
    let (rect, _) = state.hits.feed_overlay.rows[0];
    click(&mut state, MouseEventKind::ScrollDown, rect);
    assert_eq!(selected_item(&state), "nh-4");
    click(&mut state, MouseEventKind::ScrollUp, rect);
    assert_eq!(selected_item(&state), "nh-1");
}

#[test]
fn empty_states_explain_themselves() {
    let mut state = overlay_state(Vec::new());
    assert!(compose(&mut state).contains("No feeds. Add a feed table"));

    let mut state = overlay_state(vec![(summary("empty", "Empty", 0, 0, 1), Vec::new())]);
    assert!(compose(&mut state).contains("Nothing here."));

    let mut broken = summary("news", "News", 0, 0, 1);
    broken.loaded = false;
    broken.fetched_unix_ms = None;
    broken.error = Some("not valid JSON".into());
    let mut state = overlay_state(vec![(broken, Vec::new())]);
    let frame = compose(&mut state);
    assert!(frame.contains("failed: not valid JSON"), "{frame}");

    let mut state = new_state(&Config::default());
    state.set_endpoint_methods(Some(vec!["pane.read".into()]));
    state.compose(COLS, ROWS).expect("frame");
    open_overlay(&mut state);
    tick(&mut state, Instant::now());
    assert!(compose(&mut state).contains("This server does not support feeds yet"));
}

#[test]
fn a_stale_feed_says_so_in_the_overlay() {
    let mut stale = workbox_feed();
    stale.0.stale = true;
    stale.0.error = Some("feed command failed: exit 1".into());
    let mut state = overlay_state(vec![stale]);
    let frame = compose(&mut state);
    assert!(
        frame.contains("stale: feed command failed: exit 1"),
        "{frame}"
    );
    assert!(frame.contains("Fix login"), "last good items stay");
}

#[test]
fn the_selection_survives_a_refresh_that_reorders_items() {
    let mut state = overlay_state(vec![workbox_feed()]);
    compose(&mut state);
    press(&mut state, 'j');
    press(&mut state, 'j');
    assert_eq!(selected_item(&state), "nh-3");
    let mut items = today_items();
    items.reverse();
    let mut fresh = workbox_feed().0;
    fresh.revision = 4;
    state
        .feeds
        .items
        .insert("workbox-sync".into(), FeedItems { revision: 4, items });
    state.feeds.feeds[0] = fresh;
    assert_eq!(selected_item(&state), "nh-3");
}

// ----- palette ------------------------------------------------------------

#[test]
fn the_palette_lists_feed_entries_once_the_feeds_are_known() {
    let mut state = new_state(&Config::default());
    assert!(state.palette_items().iter().all(|item| item.kind != "feed"));
    load(&mut state, two_feeds());
    let titles: Vec<_> = state
        .palette_items()
        .into_iter()
        .filter(|item| item.kind == "feed")
        .map(|item| item.title)
        .collect();
    assert_eq!(
        titles,
        [
            "feed: Today (workbox)",
            "refresh feed workbox-sync",
            "feed: News",
            "refresh feed news"
        ]
    );
    assert!(
        state
            .palette_items()
            .iter()
            .any(|item| item.title == "feed"),
        "the open-overlay entry comes from the binding table"
    );

    let mut outcome = ClientShellInput::default();
    state.run_palette_action(
        super::super::palette::PaletteAction::RefreshFeed("news".into()),
        &mut outcome,
    );
    assert_eq!(
        requests(&outcome)[0].method,
        Method::FeedRefresh(FeedTarget {
            job_id: "news".into()
        })
    );
    let mut outcome = ClientShellInput::default();
    state.run_palette_action(
        super::super::palette::PaletteAction::OpenFeed("news".into()),
        &mut outcome,
    );
    match &state.overlay {
        Some(ClientShellOverlay::Feed(overlay)) => {
            assert_eq!(overlay.job_id.as_deref(), Some("news"));
        }
        other => panic!("expected the Feed overlay, got {other:?}"),
    }
}

#[test]
fn opening_the_palette_refreshes_the_cached_feed_list() {
    let mut state = new_state(&Config::default());
    state.sidebar_collapsed = true;
    state.compose(COLS, ROWS).expect("frame");
    state.handle_raw_events(vec![
        RawInputEvent::Key(crate::input::TerminalKey::new(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
        )),
        key(KeyCode::Char(':')),
    ]);
    let sent = requests(&tick(&mut state, Instant::now()));
    assert_eq!(sent.len(), 1);
    assert!(matches!(sent[0].method, Method::FeedList(_)));
}

#[test]
fn which_key_lists_the_feed_binding() {
    let config = Config::default();
    let groups = super::super::which_key::which_key_groups(&config.keybinds());
    let entry = groups
        .iter()
        .flat_map(|group| group.entries.iter())
        .find(|entry| entry.label == "feed")
        .expect("which-key lists feed");
    assert_eq!(entry.key, "f");
}

// ----- master-detail ------------------------------------------------------

fn detailed_feed() -> (FeedSummary, Vec<FeedItemInfo>) {
    let mut first = item("nh-1", "Fix login redirect", FeedItemState::Unread);
    first.updated_at = Some("2026-10-18T08:30:00Z".into());
    first.fields = vec![
        FeedField {
            label: "Source".into(),
            value: "jira".into(),
        },
        FeedField {
            label: "Status".into(),
            value: "In Progress".into(),
        },
    ];
    first.body = Some("can you look at the redirect?".into());
    let mut second = item("nh-2", "Review retry", FeedItemState::Read);
    second.fields = vec![FeedField {
        label: "Source".into(),
        value: "github".into(),
    }];
    second.body = Some(
        (1..=60)
            .map(|n| format!("body line {n}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    (
        summary("workbox-sync", "Today (workbox)", 2, 1, 3),
        vec![first, second],
    )
}

fn compose_at(state: &mut ClientShellState, cols: u16, rows: u16) -> String {
    frame_rows(&state.compose(cols, rows).expect("frame").frame).join("\n")
}

fn ctrl(state: &mut ClientShellState, ch: char) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char(ch),
        KeyModifiers::CONTROL,
    ))])
}

#[test]
fn a_wide_overlay_shows_the_list_left_and_the_detail_right() {
    let mut state = overlay_state(vec![detailed_feed()]);
    let frame = compose_at(&mut state, 200, 50);
    let hits = &state.hits.feed_overlay;
    let list_right = hits.rows.iter().map(|(r, _)| r.right()).max().unwrap();
    assert!(hits.detail.x > list_right, "{:?} {list_right}", hits.detail);
    assert!(hits.detail.y <= hits.rows[0].0.y);
    for expected in [
        "Fix login redirect",
        "jira · mention",
        "Source   jira",
        "Status   In Progress",
        "can you look at the redirect?",
        "Link     https://example.com/nh-1",
        "d done",
    ] {
        assert!(frame.contains(expected), "missing {expected:?}\n{frame}");
    }
    // The list rows are compact: the subtitle is only in the detail.
    let list_line = frame
        .lines()
        .find(|line| line.contains("NH-1") && line.contains("Fix login"))
        .unwrap();
    assert!(!list_line
        .split('│')
        .nth(1)
        .unwrap()
        .contains("jira · mention"));
}

#[test]
fn moving_the_selection_changes_the_detail() {
    let mut state = overlay_state(vec![detailed_feed()]);
    let before = compose_at(&mut state, 200, 50);
    assert!(before.contains("can you look") && !before.contains("body line 1"));
    press(&mut state, 'j');
    let after = compose_at(&mut state, 200, 50);
    assert!(after.contains("body line 1") && !after.contains("can you look"));
    assert!(after.contains("Source  github"));
    // Arrow keys and the mouse follow too.
    state.handle_raw_events(vec![key(KeyCode::Up)]);
    assert!(compose_at(&mut state, 200, 50).contains("can you look"));
    let second = state.hits.feed_overlay.rows[1].0;
    left_click(&mut state, second);
    assert!(compose_at(&mut state, 200, 50).contains("body line 1"));
}

#[test]
fn a_narrow_overlay_stacks_the_detail_under_the_list() {
    let mut state = overlay_state(vec![detailed_feed()]);
    let frame = compose_at(&mut state, 90, 30);
    let hits = &state.hits.feed_overlay;
    let list_bottom = hits.rows.iter().map(|(r, _)| r.bottom()).max().unwrap();
    assert!(
        hits.detail.y > list_bottom,
        "{:?} {list_bottom}",
        hits.detail
    );
    assert_eq!(hits.detail.x, hits.rows[0].0.x);
    assert!(
        frame.contains("Source   jira") && frame.contains("can you look"),
        "{frame}"
    );
    press(&mut state, 'j');
    assert!(compose_at(&mut state, 90, 30).contains("body line 1"));
}

#[test]
fn a_tiny_overlay_falls_back_to_the_list_alone_and_space_zooms_the_detail() {
    let mut state = overlay_state(vec![detailed_feed()]);
    let frame = compose_at(&mut state, 90, 16);
    assert_eq!(state.hits.feed_overlay.detail, Rect::default());
    assert!(!frame.contains("can you look"));
    press(&mut state, ' ');
    let frame = compose_at(&mut state, 90, 16);
    assert!(frame.contains("Source   jira"), "{frame}");
    assert!(state.hits.feed_overlay.rows.is_empty());
    // j still moves the selection while zoomed; esc leaves the zoom first.
    press(&mut state, 'j');
    assert!(compose_at(&mut state, 90, 16).contains("Source  github"));
    state.handle_raw_events(vec![key(KeyCode::Esc)]);
    assert!(matches!(state.overlay, Some(ClientShellOverlay::Feed(_))));
    compose_at(&mut state, 90, 16);
    assert!(!state.hits.feed_overlay.rows.is_empty());
}

#[test]
fn the_detail_scrolls_on_its_own_and_resets_for_a_new_selection() {
    let mut state = overlay_state(vec![detailed_feed()]);
    press(&mut state, 'j');
    let top = compose_at(&mut state, 200, 50);
    assert!(top.contains("body line 1") && state.hits.feed_overlay.detail_max_scroll > 0);
    let selected = selected_item(&state);
    ctrl(&mut state, 'd');
    let scrolled = compose_at(&mut state, 200, 50);
    assert!(
        !scrolled.contains("Source  github"),
        "title scrolled away\n{scrolled}"
    );
    assert_eq!(selected_item(&state), selected, "the list did not move");
    ctrl(&mut state, 'u');
    assert!(compose_at(&mut state, 200, 50).contains("Source  github"));
    // The wheel over the detail scrolls it; over the list it moves the selection.
    let detail = state.hits.feed_overlay.detail;
    for _ in 0..3 {
        click(&mut state, MouseEventKind::ScrollDown, detail);
    }
    assert!(!compose_at(&mut state, 200, 50).contains("Source  github"));
    assert_eq!(selected_item(&state), selected);
    // The end is clamped.
    for _ in 0..40 {
        ctrl(&mut state, 'd');
    }
    let end = compose_at(&mut state, 200, 50);
    assert!(end.contains("body line 60"), "{end}");
    // A new selection shows from the top.
    state.handle_raw_events(vec![key(KeyCode::Up)]);
    compose_at(&mut state, 200, 50);
    press(&mut state, 'j');
    assert!(compose_at(&mut state, 200, 50).contains("Source  github"));
}

#[test]
fn existing_keys_still_work_beside_the_detail() {
    let mut state = overlay_state(vec![detailed_feed()]);
    compose_at(&mut state, 200, 50);
    let input = press(&mut state, 'd');
    assert!(matches!(
        requests(&input)[0].method,
        Method::FeedRunAction(_)
    ));
    let input = state.handle_raw_events(vec![key(KeyCode::Enter)]);
    assert!(matches!(
        &input.actions[..],
        [ClientShellAction::OpenSafeWebUrl(_)]
    ));
    let input = press(&mut state, 'R');
    assert!(matches!(requests(&input)[0].method, Method::FeedRefresh(_)));
}

fn sync_job(state: JobState, run_id: u64, status: JobRunStatus, has_feed: bool) -> JobInfo {
    JobInfo {
        job_id: "workbox-sync".into(),
        title: "workbox-sync".into(),
        source: "user".into(),
        command: vec!["workbox".into(), "sync".into()],
        schedule: "every 1h".into(),
        schedule_kind: JobScheduleKind::Interval,
        timeout_secs: 600,
        enabled: true,
        enabled_overridden: false,
        schedule_overridden: false,
        env: "login-shell".into(),
        notify: "on_failure".into(),
        state,
        next_run_unix_ms: None,
        running_since_unix_ms: None,
        last_run: Some(JobRunInfo {
            run_id,
            trigger: JobRunTrigger::Manual,
            status,
            started_unix_ms: 1,
            finished_unix_ms: 2,
            duration_ms: 1,
            exit_code: Some(0),
            summary: if status == JobRunStatus::Success {
                "exit 0".into()
            } else {
                "exit 3".into()
            },
            error: None,
            skipped_count: 0,
            env_note: None,
        }),
        has_feed,
        diagnostics: Vec::new(),
        dir: "/jobs".into(),
    }
}

fn job_list(job: JobInfo) -> ResponseResult {
    ResponseResult::JobList {
        revision: 1,
        unchanged: false,
        jobs: vec![job],
        env: crate::api::schema::JobEnvStatus {
            source: "none".into(),
            captured_unix_ms: None,
            variable_count: 0,
            refreshing: false,
            diagnostic: None,
        },
    }
}

#[test]
fn shift_s_runs_the_feeds_job_and_the_feed_refreshes_when_it_finishes() {
    let mut state = overlay_state(vec![detailed_feed()]);
    compose_at(&mut state, 200, 50);
    let input = press(&mut state, 'S');
    let sent = requests(&input);
    assert_eq!(
        sent[0].method,
        Method::JobRun(JobTarget {
            job_id: "workbox-sync".into()
        })
    );
    let frame = compose_at(&mut state, 200, 50);
    assert!(frame.contains("syncing workbox-sync…"), "{frame}");
    assert!(frame.contains("syncing…"), "header says so too\n{frame}");
    answer(
        &mut state,
        &sent[0],
        ResponseResult::JobRunRequested {
            job_id: "workbox-sync".into(),
            started: true,
            run_id: Some(7),
            reason: None,
        },
    );
    // The client asks job.list until run 7 is done.
    let now = Instant::now() + Duration::from_secs(1);
    let input = tick(&mut state, now);
    let check = requests(&input)
        .into_iter()
        .find(|request| matches!(request.method, Method::JobList(_)))
        .expect("a job.list check");
    // Still running: nothing changes.
    answer(
        &mut state,
        &check,
        job_list(sync_job(JobState::Running, 6, JobRunStatus::Success, true)),
    );
    assert!(compose_at(&mut state, 200, 50).contains("syncing workbox-sync"));
    // The run finished but the feed has not been refreshed yet.
    let input = tick(&mut state, now + Duration::from_secs(2));
    let check = requests(&input)
        .into_iter()
        .find(|request| matches!(request.method, Method::JobList(_)))
        .expect("a second check");
    answer(
        &mut state,
        &check,
        job_list(sync_job(JobState::Idle, 7, JobRunStatus::Success, true)),
    );
    assert!(compose_at(&mut state, 200, 50).contains("refreshing the feed"));
    // The refreshed feed lands.
    let mut fresh = detailed_feed().0;
    fresh.fetched_unix_ms = Some(crate::jobs::now_unix_ms());
    state.feeds.feeds[0] = fresh;
    let input = tick(&mut state, now + Duration::from_secs(4));
    let check = requests(&input)
        .into_iter()
        .find(|request| matches!(request.method, Method::JobList(_)))
        .expect("a third check");
    answer(
        &mut state,
        &check,
        job_list(sync_job(JobState::Idle, 7, JobRunStatus::Success, true)),
    );
    let frame = compose_at(&mut state, 200, 50);
    assert!(frame.contains("synced workbox-sync: 2 items"), "{frame}");
    assert!(!state.feeds.is_syncing("workbox-sync"));
}

#[test]
fn a_failed_sync_run_is_reported_and_a_refused_one_too() {
    let mut state = overlay_state(vec![detailed_feed()]);
    compose_at(&mut state, 200, 50);
    let sent = requests(&press(&mut state, 'S'));
    answer(
        &mut state,
        &sent[0],
        ResponseResult::JobRunRequested {
            job_id: "workbox-sync".into(),
            started: true,
            run_id: Some(8),
            reason: None,
        },
    );
    let input = tick(&mut state, Instant::now() + Duration::from_secs(1));
    let check = requests(&input)
        .into_iter()
        .find(|request| matches!(request.method, Method::JobList(_)))
        .unwrap();
    answer(
        &mut state,
        &check,
        job_list(sync_job(JobState::Idle, 8, JobRunStatus::Failed, true)),
    );
    let frame = compose_at(&mut state, 200, 50);
    assert!(frame.contains("sync failed: failed: exit 3"), "{frame}");
    assert!(!state.feeds.is_syncing("workbox-sync"));

    let sent = requests(&press(&mut state, 'S'));
    answer(
        &mut state,
        &sent[0],
        ResponseResult::JobRunRequested {
            job_id: "workbox-sync".into(),
            started: false,
            run_id: None,
            reason: Some("already running".into()),
        },
    );
    assert!(compose_at(&mut state, 200, 50).contains("workbox-sync: already running"));
    assert!(!state.feeds.is_syncing("workbox-sync"));
}

#[test]
fn s_is_not_an_action_key_any_more() {
    assert!(crate::feed::format::action_key_problem("S").is_some());
}

// ----- groups (category tabs) -------------------------------------------

fn group_info(
    id: &str,
    title: &str,
    short: Option<&str>,
    count: u32,
    unread: u32,
) -> FeedGroupInfo {
    FeedGroupInfo {
        id: id.into(),
        title: title.into(),
        short_title: short.map(str::to_owned),
        count,
        unread,
    }
}

/// The five `today_items` spread over four groups, the last one implicit.
fn grouped_feed() -> (FeedSummary, Vec<FeedItemInfo>) {
    let mut items = today_items();
    for (item, group) in items
        .iter_mut()
        .zip(["reviews", "reviews", "mentions", "tickets", "other"])
    {
        item.group = Some(group.into());
    }
    let mut feed = summary("workbox-sync", "Today (workbox)", 5, 2, 3);
    feed.groups = vec![
        group_info("reviews", "Review Requests", Some("Reviews"), 2, 2),
        group_info("mentions", "Mentions", None, 1, 0),
        group_info("tickets", "Tickets", None, 1, 0),
        group_info("empty", "Nothing Here", None, 0, 0),
        group_info("other", "Other", None, 1, 0),
    ];
    (feed, items)
}

fn titles(state: &ClientShellState) -> Vec<String> {
    state
        .visible_feed_items()
        .iter()
        .map(|item| item.id.clone())
        .collect()
}

fn selected_group(state: &ClientShellState) -> Option<String> {
    match &state.overlay {
        Some(ClientShellOverlay::Feed(overlay)) => overlay.group.clone(),
        other => panic!("expected the Feed overlay, got {other:?}"),
    }
}

fn tab_labels(state: &ClientShellState, frame: &str) -> String {
    let row = state
        .hits
        .feed_overlay
        .groups
        .first()
        .expect("category tabs")
        .0
        .y;
    frame.lines().nth(usize::from(row)).unwrap().to_owned()
}

#[test]
fn a_wide_overlay_shows_the_category_tab_bar_with_counts_and_unread() {
    let mut state = overlay_state(vec![grouped_feed()]);
    let frame = compose_at(&mut state, 200, 50);
    let bar = tab_labels(&state, &frame);
    for expected in [
        "All 5 ●2",
        "Reviews 2 ●2",
        "Mentions 1",
        "Tickets 1",
        "Nothing Here 0",
        "Other 1",
    ] {
        assert!(bar.contains(expected), "missing {expected:?} in {bar:?}");
    }
    let hits = &state.hits.feed_overlay;
    assert_eq!(hits.groups.len(), 6);
    assert_eq!(hits.groups[0].1, None, "the first tab is All");
    assert_eq!(hits.groups[1].1.as_deref(), Some("reviews"));
    let info_row = frame
        .lines()
        .position(|line| line.contains("5 items · 2 unread"))
        .unwrap();
    assert_eq!(
        usize::from(hits.groups[0].0.y),
        info_row + 1,
        "under the info line"
    );
    assert!(hits.rows[0].0.y > hits.groups[0].0.y);
    assert_eq!(hits.rows.len(), 5, "All shows every item");
}

#[test]
fn a_feed_without_groups_has_no_tab_bar_and_keeps_its_layout() {
    let mut state = overlay_state(vec![workbox_feed()]);
    let frame = compose_at(&mut state, 200, 50);
    assert!(state.hits.feed_overlay.groups.is_empty());
    assert!(!frame.contains("All 5"));
    // Digits and brackets are not category keys here, so they stay free.
    let input = press(&mut state, '1');
    assert!(requests(&input).is_empty());
    press(&mut state, ']');
    assert_eq!(selected_item(&state), "nh-1");
}

#[test]
fn a_narrow_overlay_shortens_the_labels_and_then_scrolls_the_tab_bar() {
    let mut state = overlay_state(vec![grouped_feed()]);
    // Narrower than the full labels: the unread marks go first.
    let frame = compose_at(&mut state, 70, 30);
    let bar = tab_labels(&state, &frame);
    assert!(
        bar.contains("All 5") && !bar.contains('●') || bar.contains("Other 1"),
        "{bar:?}"
    );
    assert_eq!(state.hits.feed_overlay.groups.len(), 6, "{bar:?}");
    // Very narrow: not every tab fits, so the bar scrolls and marks the edge.
    let frame = compose_at(&mut state, 40, 30);
    let bar = tab_labels(&state, &frame);
    assert!(bar.contains('›'), "{bar:?}");
    assert!(state.hits.feed_overlay.groups.len() < 6);
    for line in frame.lines() {
        assert!(line.chars().count() <= 40, "{line:?}");
    }
    // Selecting the last tab scrolls it into view with a marker on the left.
    press(&mut state, '5');
    let frame = compose_at(&mut state, 40, 30);
    let bar = tab_labels(&state, &frame);
    assert!(bar.contains('‹') && bar.contains("Other"), "{bar:?}");
    assert!(
        state
            .hits
            .feed_overlay
            .groups
            .iter()
            .any(|(_, group)| group.as_deref() == Some("other")),
        "the selected tab is always drawn"
    );
}

#[test]
fn brackets_and_digits_switch_category_and_zero_is_all() {
    let mut state = overlay_state(vec![grouped_feed()]);
    compose(&mut state);
    assert_eq!(selected_group(&state), None);
    press(&mut state, ']');
    assert_eq!(selected_group(&state).as_deref(), Some("reviews"));
    assert_eq!(titles(&state), ["nh-1", "nh-2"]);
    press(&mut state, ']');
    assert_eq!(titles(&state), ["nh-3"]);
    press(&mut state, '[');
    press(&mut state, '[');
    assert_eq!(selected_group(&state), None, "[ steps back to All");
    press(&mut state, '[');
    assert_eq!(
        selected_group(&state).as_deref(),
        Some("other"),
        "and wraps to the last tab"
    );
    press(&mut state, ']');
    assert_eq!(selected_group(&state), None, "] wraps to All");

    press(&mut state, '3');
    assert_eq!(selected_group(&state).as_deref(), Some("tickets"));
    assert_eq!(titles(&state), ["nh-4"]);
    press(&mut state, '4');
    assert!(titles(&state).is_empty(), "a declared but empty category");
    assert!(compose(&mut state).contains("Nothing in this category."));
    press(&mut state, '0');
    assert_eq!(selected_group(&state), None);
    assert_eq!(titles(&state).len(), 5);
    // A digit past the last tab does nothing.
    press(&mut state, '2');
    let input = press(&mut state, '9');
    assert_eq!(selected_group(&state).as_deref(), Some("mentions"));
    assert!(!input.repaint && requests(&input).is_empty());
}

#[test]
fn the_selection_restarts_at_the_top_of_the_new_category() {
    let mut state = overlay_state(vec![grouped_feed()]);
    compose(&mut state);
    press(&mut state, 'j');
    press(&mut state, 'j');
    assert_eq!(selected_item(&state), "nh-3");
    press(&mut state, '1');
    assert_eq!(selected_item(&state), "nh-1");
    press(&mut state, 'j');
    assert_eq!(selected_item(&state), "nh-2");
    press(&mut state, '0');
    assert_eq!(selected_item(&state), "nh-1");
}

#[test]
fn clicking_a_category_tab_selects_it() {
    let mut state = overlay_state(vec![grouped_feed()]);
    compose_at(&mut state, 200, 50);
    let (rect, group) = state.hits.feed_overlay.groups[2].clone();
    assert_eq!(group.as_deref(), Some("mentions"));
    left_click(&mut state, rect);
    assert_eq!(selected_group(&state).as_deref(), Some("mentions"));
    assert_eq!(titles(&state), ["nh-3"]);
    let frame = compose_at(&mut state, 200, 50);
    assert!(frame.contains("Reply Dana"));
    let (all, _) = state.hits.feed_overlay.groups[0].clone();
    left_click(&mut state, all);
    assert_eq!(selected_group(&state), None);
    assert_eq!(titles(&state).len(), 5);
}

#[test]
fn the_filter_applies_within_the_selected_category() {
    let mut state = overlay_state(vec![grouped_feed()]);
    compose(&mut state);
    press(&mut state, '1');
    press(&mut state, '/');
    for ch in "re".chars() {
        press(&mut state, ch);
    }
    // "Review retry" is in Reviews; "Reply Dana" and "Update runbook" also
    // contain "re" but belong to other categories.
    assert_eq!(titles(&state), ["nh-2"]);
    state.handle_raw_events(vec![key(KeyCode::Enter)]);
    // Digits are not typed into the filter once it is accepted: they switch.
    press(&mut state, '0');
    assert_eq!(selected_group(&state), None);
    assert!(titles(&state).contains(&"nh-2".to_owned()) && titles(&state).len() > 1);
    // While typing, digits and brackets belong to the filter.
    press(&mut state, '/');
    press(&mut state, '1');
    assert_eq!(selected_group(&state), None);
    state.handle_raw_events(vec![key(KeyCode::Esc)]);
    press(&mut state, '3');
    press(&mut state, '/');
    press(&mut state, 'x');
    assert!(titles(&state).is_empty());
    assert!(compose(&mut state).contains("No items match the filter."));
    state.handle_raw_events(vec![key(KeyCode::Esc)]);
    assert_eq!(
        selected_group(&state).as_deref(),
        Some("tickets"),
        "esc clears only the filter"
    );
    assert_eq!(titles(&state), ["nh-4"]);
}

#[test]
fn the_detail_pane_follows_the_category_and_its_filtered_list() {
    let mut feed = detailed_feed();
    feed.0.groups = vec![
        group_info("a", "A", None, 1, 1),
        group_info("b", "B", None, 1, 0),
    ];
    feed.1[0].group = Some("a".into());
    feed.1[1].group = Some("b".into());
    let mut state = overlay_state(vec![feed]);
    let frame = compose_at(&mut state, 200, 50);
    assert!(frame.contains("can you look at the redirect?"));
    press(&mut state, '2');
    let frame = compose_at(&mut state, 200, 50);
    assert!(
        frame.contains("body line 1") && !frame.contains("can you look"),
        "{frame}"
    );
    assert_eq!(selected_item(&state), "nh-2");
    press(&mut state, '1');
    let frame = compose_at(&mut state, 200, 50);
    assert!(frame.contains("can you look") && !frame.contains("body line 1"));
    // Actions run on the item the category shows.
    press(&mut state, '2');
    compose_at(&mut state, 200, 50);
    let sent = requests(&press(&mut state, 'd'));
    assert_eq!(
        sent[0].method,
        Method::FeedRunAction(FeedRunActionParams {
            job_id: "workbox-sync".into(),
            item_id: "nh-2".into(),
            key: "d".into(),
        })
    );
}

#[test]
fn the_category_is_remembered_per_feed_and_across_reopening() {
    let mut other = summary("news", "News", 3, 1, 4);
    other.groups = vec![group_info("x", "X", None, 3, 1)];
    let mut news_items = vec![
        item("n-1", "Rust 2027 edition", FeedItemState::Unread),
        item("n-2", "Zig release", FeedItemState::Read),
        item("n-3", "Kernel update", FeedItemState::Read),
    ];
    for item in &mut news_items {
        item.group = Some("x".into());
    }
    let mut state = overlay_state(vec![grouped_feed(), (other, news_items)]);
    compose(&mut state);
    press(&mut state, '2');
    assert_eq!(selected_group(&state).as_deref(), Some("mentions"));
    state.handle_raw_events(vec![key(KeyCode::Tab)]);
    assert_eq!(selected_group(&state), None, "news has its own choice");
    press(&mut state, '1');
    assert_eq!(selected_group(&state).as_deref(), Some("x"));
    state.handle_raw_events(vec![key(KeyCode::Tab)]);
    assert_eq!(
        selected_group(&state).as_deref(),
        Some("mentions"),
        "coming back restores the first feed's category"
    );
    state.handle_raw_events(vec![key(KeyCode::Esc)]);
    assert!(state.overlay.is_none());
    open_overlay(&mut state);
    assert_eq!(selected_group(&state).as_deref(), Some("mentions"));
    assert_eq!(titles(&state), ["nh-3"]);
    press(&mut state, '0');
    state.handle_raw_events(vec![key(KeyCode::Esc)]);
    open_overlay(&mut state);
    assert_eq!(selected_group(&state), None, "All is remembered too");
}

#[test]
fn the_category_is_saved_in_the_preferences_under_a_feedgroup_key() {
    let path = std::env::temp_dir().join(format!(
        "flockr-shell-feed-group-{}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let build = || {
        let config =
            ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone());
        let mut state = ClientShellState::new(config);
        state.set_snapshot(Box::new(two_workspace_agent_snapshot()));
        state.set_pane_surface(surface());
        state.compose(COLS, ROWS).expect("frame");
        load(&mut state, vec![grouped_feed()]);
        state
    };
    let mut state = build();
    open_overlay(&mut state);
    press(&mut state, '2');
    let saved = std::fs::read_to_string(&path).expect("preferences written");
    assert!(saved.contains("feedgroup:workbox-sync:mentions"), "{saved}");
    press(&mut state, '3');
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(
        saved.contains("feedgroup:workbox-sync:tickets")
            && !saved.contains("feedgroup:workbox-sync:mentions"),
        "one entry per feed: {saved}"
    );

    let mut restored = build();
    open_overlay(&mut restored);
    assert_eq!(selected_group(&restored).as_deref(), Some("tickets"));
    press(&mut restored, '0');
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(!saved.contains("feedgroup:"), "All clears it: {saved}");
    std::fs::remove_file(path).expect("remove preferences");
}

#[test]
fn a_remembered_category_that_no_longer_exists_shows_all() {
    let mut state = overlay_state(vec![grouped_feed()]);
    compose(&mut state);
    press(&mut state, '2');
    let mut changed = grouped_feed();
    changed.0.groups.retain(|group| group.id != "mentions");
    changed
        .1
        .retain(|item| item.group.as_deref() != Some("mentions"));
    load_keeping_overlay(&mut state, changed);
    assert_eq!(titles(&state).len(), 4, "falls back to All");
    compose(&mut state);
}

fn load_keeping_overlay(state: &mut ClientShellState, feed: (FeedSummary, Vec<FeedItemInfo>)) {
    state.feeds.feeds.clear();
    state.feeds.items.clear();
    state.feeds.items.insert(
        feed.0.job_id.clone(),
        FeedItems {
            revision: feed.0.revision,
            items: feed.1,
        },
    );
    state.feeds.feeds.push(feed.0);
}

#[test]
fn category_keys_do_not_break_existing_overlay_keys() {
    let mut state = overlay_state(vec![grouped_feed()]);
    compose(&mut state);
    for ch in ['j', 'k', 'g', 'G'] {
        press(&mut state, ch);
    }
    assert_eq!(selected_group(&state), None);
    let sent = requests(&press(&mut state, 's'));
    assert!(matches!(sent[0].method, Method::FeedRunAction(_)));
    state.handle_raw_events(vec![key(KeyCode::Tab)]);
    assert!(state.overlay.is_some(), "tab with one feed is a no-op");
    press(&mut state, 'q');
    assert!(state.overlay.is_none());
}

// ----- groups in the sidebar -----------------------------------------------

#[test]
fn a_grouped_feed_shows_group_rows_instead_of_items() {
    let mut state = state_with_feeds(&Config::default(), vec![grouped_feed()]);
    let frame = compose(&mut state);
    let lines = feed_rows_text(&state, &frame);
    assert_eq!(lines[1], " ▾ Today (workbox) 2");
    assert_eq!(lines[2], "   Reviews 2 ●2");
    assert_eq!(lines[3], "   Mentions 1");
    assert_eq!(lines[4], "   Tickets 1");
    assert_eq!(lines[5], "   Other 1", "empty groups are left out");
    assert_eq!(lines.len(), 6, "{lines:?}");
    assert!(state.hits.feed.rows.is_empty(), "no item rows");
    assert_eq!(state.hits.feed.groups.len(), 4);
    assert_eq!(state.hits.feed.groups[0].group_id, "reviews");
}

#[test]
fn clicking_a_group_row_opens_the_overlay_on_that_category() {
    let mut state = state_with_feeds(&Config::default(), vec![grouped_feed()]);
    compose(&mut state);
    let rect = state.hits.feed.groups[1].rect;
    assert_eq!(state.hits.feed.groups[1].group_id, "mentions");
    left_click(&mut state, rect);
    assert_eq!(selected_group(&state).as_deref(), Some("mentions"));
    match &state.overlay {
        Some(ClientShellOverlay::Feed(overlay)) => {
            assert_eq!(overlay.job_id.as_deref(), Some("workbox-sync"));
        }
        other => panic!("expected the Feed overlay, got {other:?}"),
    }
    let frame = compose_at(&mut state, 200, 50);
    assert!(frame.contains("Reply Dana"));
    assert_eq!(titles(&state), ["nh-3"]);
}

#[test]
fn a_collapsed_grouped_feed_hides_its_group_rows_and_right_click_offers_the_menu() {
    let mut state = state_with_feeds(&Config::default(), vec![grouped_feed()]);
    compose(&mut state);
    let group_row = state.hits.feed.groups[0].rect;
    right_click(&mut state, group_row);
    assert!(
        matches!(state.overlay, Some(ClientShellOverlay::ContextMenu(_))),
        "the feed's menu opens on a group row"
    );
    state.overlay = None;
    let header = state.hits.feed.headers[0].rect;
    left_click(&mut state, header);
    compose(&mut state);
    assert!(state.hits.feed.groups.is_empty());
}

#[test]
fn a_grouped_feed_is_not_fetched_for_the_sidebar() {
    let mut state = new_state(&Config::default());
    state.compose(COLS, ROWS).expect("frame");
    let list = requests(&tick(&mut state, Instant::now()));
    let (grouped, _) = grouped_feed();
    let flat = summary("news", "News", 3, 1, 4);
    let (_, actions) = answer(
        &mut state,
        &list[0],
        feed_list_result(vec![grouped, flat], 7),
    );
    let fetched: Vec<_> = actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => match &request.method {
                Method::FeedGet(params) => Some(params.job_id.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(fetched, ["news"], "only the flat feed needs its items");
}

#[test]
fn group_rows_do_not_move_or_overlap_agent_rows() {
    let mut baseline = new_state(&Config::default());
    baseline.compose(COLS, ROWS).expect("frame");
    let mut grouped = state_with_feeds(&Config::default(), vec![grouped_feed()]);
    grouped.compose(COLS, ROWS).expect("frame");
    assert_eq!(baseline.hits.agent_body.y, grouped.hits.agent_body.y);
    assert_eq!(baseline.hits.agent_body.x, grouped.hits.agent_body.x);
    assert_eq!(baseline.hits.agents, grouped.hits.agents);
    assert_eq!(
        baseline.hits.agent_headers.len(),
        grouped.hits.agent_headers.len()
    );
    let agent_area = grouped.hits.agent_body;
    for hit in &grouped.hits.feed.groups {
        assert!(hit.rect.y >= agent_area.bottom(), "{:?}", hit.rect);
    }
    // A click on an agent row still selects that agent, not a feed row.
    let (agent_rect, _) = grouped.hits.agents[0].clone();
    left_click(&mut grouped, agent_rect);
    assert!(grouped.overlay.is_none());
}

#[test]
fn group_rows_are_cut_to_the_sidebar_width() {
    let mut feed = grouped_feed();
    feed.0.groups[0] = group_info(
        "reviews",
        "An extremely long category title that cannot fit",
        None,
        2,
        2,
    );
    let mut state = state_with_feeds(&Config::default(), vec![feed]);
    let frame = compose(&mut state);
    let lines = feed_rows_text(&state, &frame);
    let width = usize::from(state.hits.feed.area.width);
    for line in &lines {
        assert!(line.chars().count() <= width, "{line:?} overflows {width}");
    }
    assert!(
        lines[2].contains('…') && lines[2].ends_with("2 ●2"),
        "{:?}",
        lines[2]
    );
}

#[test]
fn the_overlay_fetches_items_of_the_first_feed_even_when_only_its_groups_show_in_the_sidebar() {
    let mut state = new_state(&Config::default());
    state.compose(COLS, ROWS).expect("frame");
    open_overlay(&mut state);
    let list = requests(&tick(&mut state, Instant::now()));
    let (grouped, _) = grouped_feed();
    let (_, actions) = answer(&mut state, &list[0], feed_list_result(vec![grouped], 7));
    let fetched: Vec<_> = actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => match &request.method {
                Method::FeedGet(params) => Some(params.job_id.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(fetched, ["workbox-sync"]);
}

// ----- selection and `w` (work) -----------------------------------------------

fn marked(state: &ClientShellState) -> Vec<String> {
    match &state.overlay {
        Some(ClientShellOverlay::Feed(overlay)) => overlay.marked.iter().cloned().collect(),
        other => panic!("expected the Feed overlay, got {other:?}"),
    }
}

fn work_params(ids: &[&str]) -> Method {
    Method::FeedWork(FeedWorkParams {
        job_id: "workbox-sync".into(),
        item_ids: ids.iter().map(|id| (*id).to_owned()).collect(),
    })
}

fn started_result(count: u32) -> ResponseResult {
    ResponseResult::FeedWorkStarted {
        job_id: "workbox-sync".into(),
        count,
        agent: "claude".into(),
        tab_id: "w1:t2".into(),
        pane_id: "w1:t2:p1".into(),
        tab_title: "workbox · 3 items".into(),
        prompt_file: "/state/work/1.md".into(),
        skipped: Vec::new(),
        warnings: Vec::new(),
    }
}

fn status_line(state: &ClientShellState) -> Option<String> {
    match &state.overlay {
        Some(ClientShellOverlay::Feed(overlay)) => overlay.status.clone(),
        _ => None,
    }
}

#[test]
fn x_marks_the_current_item_with_a_checkmark_and_the_header_counts() {
    let mut state = overlay_state(vec![workbox_feed()]);
    let frame = compose(&mut state);
    assert!(!frame.contains("selected"), "nothing marked yet");
    press(&mut state, 'x');
    assert_eq!(marked(&state), ["nh-1"]);
    press(&mut state, 'j');
    press(&mut state, 'j');
    press(&mut state, 'x');
    assert_eq!(marked(&state), ["nh-1", "nh-3"]);
    let frame = compose(&mut state);
    assert!(frame.contains("5 items · 2 unread · 2 selected"), "{frame}");
    let rows: Vec<&str> = frame.lines().filter(|line| line.contains("NH-")).collect();
    let checked: Vec<&&str> = rows.iter().filter(|line| line.contains('✓')).collect();
    // nh-4 is `done` and has its own check dot; the marks are in column 0 of the row.
    assert!(
        rows.iter().any(|row| row.contains("✓ ● NH-1")),
        "marked unread row: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.contains("✓ ○ NH-3")),
        "marked read row: {rows:?}"
    );
    assert!(
        rows.iter().all(|row| !row.contains("✓ ● NH-2")),
        "{checked:?}"
    );
    // x again unmarks.
    press(&mut state, 'x');
    assert_eq!(marked(&state), ["nh-1"]);
    assert!(compose(&mut state).contains("1 selected"));
}

#[test]
fn capital_x_clears_the_selection() {
    let mut state = overlay_state(vec![workbox_feed()]);
    compose(&mut state);
    press(&mut state, 'x');
    press(&mut state, 'j');
    press(&mut state, 'x');
    assert_eq!(marked(&state).len(), 2);
    let input = press_shifted(&mut state, 'X');
    assert!(input.repaint);
    assert!(marked(&state).is_empty());
    assert!(!compose(&mut state).contains("selected"));
    // Nothing to clear: no repaint.
    let input = press_shifted(&mut state, 'X');
    assert!(!input.repaint);
}

#[test]
fn the_selection_survives_category_tabs_and_filtering() {
    let mut state = overlay_state(vec![grouped_feed()]);
    compose_at(&mut state, 200, 50);
    press(&mut state, 'x'); // nh-1, in Reviews
    press(&mut state, '2'); // Mentions
    assert_eq!(titles(&state), ["nh-3"]);
    press(&mut state, 'x');
    assert_eq!(marked(&state), ["nh-1", "nh-3"]);
    // The header counts items the current feed has, shown or not.
    let frame = compose_at(&mut state, 200, 50);
    assert!(frame.contains("2 selected"), "{frame}");
    // A filter that hides nh-1 keeps it marked.
    press(&mut state, '0');
    press(&mut state, '/');
    for ch in "reply".chars() {
        press(&mut state, ch);
    }
    assert_eq!(titles(&state), ["nh-3"]);
    assert_eq!(marked(&state), ["nh-1", "nh-3"]);
    // While typing a filter, x is text.
    press(&mut state, 'x');
    assert_eq!(marked(&state), ["nh-1", "nh-3"]);
    state.handle_raw_events(vec![key(KeyCode::Esc)]);
    // And `w` sends both, in feed order.
    compose_at(&mut state, 200, 50);
    let input = press(&mut state, 'w');
    let sent = requests(&input);
    assert_eq!(sent[0].method, work_params(&["nh-1", "nh-3"]));
}

#[test]
fn switching_feeds_clears_the_selection() {
    let mut state = overlay_state(two_feeds());
    compose(&mut state);
    press(&mut state, 'x');
    assert_eq!(marked(&state).len(), 1);
    state.handle_raw_events(vec![key(KeyCode::Tab)]);
    assert!(marked(&state).is_empty());
}

#[test]
fn esc_clears_the_selection_before_the_filter_before_closing() {
    let mut state = overlay_state(vec![workbox_feed()]);
    compose(&mut state);
    press(&mut state, 'x');
    press(&mut state, '/');
    for ch in "retry".chars() {
        press(&mut state, ch);
    }
    state.handle_raw_events(vec![key(KeyCode::Enter)]);
    let esc = |state: &mut ClientShellState| {
        state.handle_raw_events(vec![key(KeyCode::Esc)]);
    };
    esc(&mut state);
    assert!(marked(&state).is_empty(), "first esc clears the selection");
    match &state.overlay {
        Some(ClientShellOverlay::Feed(overlay)) => assert_eq!(overlay.filter, "retry"),
        other => panic!("{other:?}"),
    }
    esc(&mut state);
    match &state.overlay {
        Some(ClientShellOverlay::Feed(overlay)) => assert!(overlay.filter.is_empty()),
        other => panic!("{other:?}"),
    }
    esc(&mut state);
    assert!(state.overlay.is_none(), "third esc closes");
}

#[test]
fn w_works_on_the_current_item_when_nothing_is_selected() {
    let mut state = overlay_state(vec![workbox_feed()]);
    compose(&mut state);
    press(&mut state, 'j');
    let input = press(&mut state, 'w');
    let sent = requests(&input);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].method, work_params(&["nh-2"]));
    assert!(compose(&mut state).contains("starting an agent with 1 item…"));
}

#[test]
fn w_sends_every_selected_id_in_feed_order() {
    let mut state = overlay_state(vec![workbox_feed()]);
    compose(&mut state);
    // Select out of order: nh-4, nh-1, nh-2.
    press(&mut state, 'G');
    press(&mut state, 'k');
    press(&mut state, 'x');
    press(&mut state, 'g');
    press(&mut state, 'x');
    press(&mut state, 'j');
    press(&mut state, 'x');
    let input = press(&mut state, 'w');
    let sent = requests(&input);
    assert_eq!(sent[0].method, work_params(&["nh-1", "nh-2", "nh-4"]));
    assert!(compose(&mut state).contains("starting an agent with 3 items…"));
}

#[test]
fn a_started_work_request_closes_the_overlay_and_says_what_happened() {
    let mut state = overlay_state(vec![workbox_feed()]);
    compose(&mut state);
    press(&mut state, 'x');
    press(&mut state, 'j');
    press(&mut state, 'x');
    let input = press(&mut state, 'w');
    let sent = requests(&input);
    answer(&mut state, &sent[0], started_result(2));
    // The new tab is focused on the server; the overlay gets out of its way.
    assert!(state.overlay.is_none());
    let notice = state.visible_endpoint_notice.as_ref().expect("notice");
    assert_eq!(
        notice.body,
        "started claude with 2 items in tab workbox · 3 items"
    );
}

#[test]
fn a_failed_work_request_keeps_the_overlay_and_the_selection() {
    let mut state = overlay_state(vec![workbox_feed()]);
    compose(&mut state);
    press(&mut state, 'x');
    let input = press(&mut state, 'w');
    let sent = requests(&input);
    let boot = state.snapshot.as_deref().unwrap().boot_id.clone();
    state.handle_endpoint_result(
        &boot,
        &sent[0].id,
        Err(ClientShellEndpointError {
            code: Some("feed_work_cwd_missing".into()),
            message: "the working directory /nope does not exist".into(),
        }),
    );
    assert_eq!(marked(&state), ["nh-1"]);
    assert_eq!(
        status_line(&state).as_deref(),
        Some("the working directory /nope does not exist")
    );
}

#[test]
fn a_server_without_feed_work_disables_only_w() {
    let mut state = overlay_state(vec![workbox_feed()]);
    state.set_endpoint_methods(Some(vec![
        "feed.list".into(),
        "feed.get".into(),
        "feed.run_action".into(),
    ]));
    compose(&mut state);
    let input = press(&mut state, 'w');
    assert!(requests(&input).is_empty());
    assert!(status_line(&state)
        .unwrap()
        .contains("does not support starting work from a feed"));
    assert!(!state.feeds.unsupported, "feeds keep working");
    // Selecting and the other keys still work.
    press(&mut state, 'x');
    assert_eq!(marked(&state), ["nh-1"]);
}

#[test]
fn w_with_an_empty_list_has_nothing_to_work_on() {
    let mut feed = workbox_feed();
    feed.1.clear();
    feed.0.count = 0;
    let mut state = overlay_state(vec![feed]);
    compose(&mut state);
    let input = press(&mut state, 'w');
    assert!(requests(&input).is_empty());
    assert_eq!(status_line(&state).as_deref(), Some("nothing to work on"));
}

#[test]
fn a_click_on_the_checkmark_column_or_ctrl_click_marks_the_row() {
    let mut state = overlay_state(vec![workbox_feed()]);
    compose(&mut state);
    let (row, _) = state.hits.feed_overlay.rows[2];
    let at = |column: u16, modifiers: KeyModifiers| {
        RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row: row.y,
            modifiers,
        })
    };
    // The checkmark column marks, and selects the row too.
    state.handle_raw_events(vec![at(row.x, KeyModifiers::empty())]);
    assert_eq!(marked(&state), ["nh-3"]);
    assert_eq!(selected_item(&state), "nh-3");
    // Clicking it again unmarks.
    compose(&mut state);
    state.handle_raw_events(vec![at(row.x + 1, KeyModifiers::empty())]);
    assert!(marked(&state).is_empty());
    // Ctrl-click anywhere on another row marks it without opening.
    compose(&mut state);
    let (other, _) = state.hits.feed_overlay.rows[0];
    let input = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: other.x + 10,
        row: other.y,
        modifiers: KeyModifiers::CONTROL,
    })]);
    assert_eq!(marked(&state), ["nh-1"]);
    assert!(
        input.actions.is_empty(),
        "ctrl-click does not open the link"
    );
    // A plain click elsewhere only selects.
    compose(&mut state);
    let (third, _) = state.hits.feed_overlay.rows[1];
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: third.x + 10,
        row: third.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert_eq!(marked(&state), ["nh-1"]);
    assert_eq!(selected_item(&state), "nh-2");
}

#[test]
fn the_footer_and_the_detail_row_hint_at_select_and_work() {
    let mut state = overlay_state(vec![workbox_feed()]);
    let frame = compose_at(&mut state, 200, 50);
    assert!(frame.contains("x select   w work"), "{frame}");
    press(&mut state, 'x');
    let frame = compose_at(&mut state, 200, 50);
    assert!(
        frame.contains("X clear   w work 1") && frame.contains("esc clear selection"),
        "{frame}"
    );
    // Grouped feeds keep the category hint and add the new one.
    let mut state = overlay_state(vec![grouped_feed()]);
    let frame = compose_at(&mut state, 200, 50);
    assert!(
        frame.contains("[/] or 0-9 category   x select   w work"),
        "{frame}"
    );
}

#[test]
fn x_x_and_w_are_reserved_action_keys() {
    for key in ["x", "X", "w"] {
        assert!(
            crate::feed::format::action_key_problem(key)
                .is_some_and(|text| text.contains("reserved")),
            "{key}"
        );
    }
    // The keys workbox uses stay free.
    for key in ["d", "s", "t"] {
        assert!(
            crate::feed::format::action_key_problem(key).is_none(),
            "{key}"
        );
    }
}
