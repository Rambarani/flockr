//! Which-key hint popup: once the prefix key has been pressed and no follow-up
//! arrives within `ui.which_key.delay_ms`, list every available next key.
//!
//! The popup is purely presentational. It never intercepts input, so the key
//! that follows the prefix is routed exactly as it would be without it.

use super::action_catalog::{action_specs, indexed_specs, ActionCategory};
use super::popup_frame::{base_style, dim_style, draw_panel, heading_style, key_style};
use super::render::{display_width, put_text};
use super::*;
use crate::config::Keybinds;

const MAX_POPUP_WIDTH: u16 = 110;
const COLUMN_GAP: u16 = 2;
const MAX_KEY_WIDTH: usize = 14;
const MAX_LABEL_WIDTH: usize = 30;
const MIN_LABEL_WIDTH: usize = 12;
const MAX_COLUMNS: usize = 5;
const CATEGORY_ORDER: [ActionCategory; 6] = [
    ActionCategory::Panes,
    ActionCategory::Tabs,
    ActionCategory::Workspaces,
    ActionCategory::Agents,
    ActionCategory::Session,
    ActionCategory::Custom,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WhichKeyEntry {
    pub(super) key: String,
    pub(super) label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WhichKeyGroup {
    pub(super) title: &'static str,
    pub(super) entries: Vec<WhichKeyEntry>,
}

/// Prefix-mode state: when the wait started and whether the popup is showing.
#[derive(Debug, Default)]
pub(super) struct WhichKeyState {
    prefix_since: Option<std::time::Instant>,
    visible: bool,
}

impl WhichKeyState {
    pub(super) fn visible(&self) -> bool {
        self.visible
    }
}

/// Keys that follow the prefix, grouped by category, from the live table.
/// Direct-only bindings are not follow-up keys and are left out.
pub(super) fn which_key_groups(keybinds: &Keybinds) -> Vec<WhichKeyGroup> {
    let mut buckets: Vec<(ActionCategory, Vec<WhichKeyEntry>)> = CATEGORY_ORDER
        .iter()
        .map(|category| (*category, Vec::new()))
        .collect();
    let mut push = |category: ActionCategory, key: String, label: String| {
        if let Some((_, entries)) = buckets.iter_mut().find(|(c, _)| *c == category) {
            entries.push(WhichKeyEntry { key, label });
        }
    };

    for spec in action_specs(keybinds) {
        if let Some(key) = spec.bindings.prefix_rhs_label() {
            push(spec.category, key, spec.label.to_owned());
        }
    }
    for spec in indexed_specs(keybinds) {
        let prefix_only = spec
            .bindings
            .iter()
            .filter(|binding| binding.trigger.is_prefix())
            .cloned()
            .collect::<Vec<_>>();
        if prefix_only.is_empty() {
            continue;
        }
        let key = crate::input::indexed_keybind_label(&prefix_only)
            .split(" / ")
            .map(|part| part.strip_prefix("prefix+").unwrap_or(part))
            .collect::<Vec<_>>()
            .join(" / ");
        push(spec.category, key, spec.label.to_owned());
    }
    for command in &keybinds.custom_commands {
        let Some(key) = command.bindings.prefix_rhs_label() else {
            continue;
        };
        let label = command
            .description
            .clone()
            .unwrap_or_else(|| command.command.clone());
        push(ActionCategory::Custom, key, label);
    }

    buckets
        .into_iter()
        .filter(|(_, entries)| !entries.is_empty())
        .map(|(category, entries)| WhichKeyGroup {
            title: category.label(),
            entries,
        })
        .collect()
}

#[derive(Clone, Copy)]
enum Row<'a> {
    Heading(&'a str),
    Entry(&'a WhichKeyEntry),
}

fn flatten(groups: &[WhichKeyGroup]) -> Vec<Row<'_>> {
    groups
        .iter()
        .flat_map(|group| {
            std::iter::once(Row::Heading(group.title)).chain(group.entries.iter().map(Row::Entry))
        })
        .collect()
}

/// Fill up to `columns` columns of `height` rows, column-major. A heading
/// never ends a column. Returns the columns and how many rows did not fit.
fn flow<'a>(rows: &[Row<'a>], columns: usize, height: usize) -> (Vec<Vec<Row<'a>>>, usize) {
    let mut filled: Vec<Vec<Row<'a>>> = vec![Vec::new()];
    let mut dropped = 0;
    for row in rows {
        let mut last_len = filled.last().map_or(0, Vec::len);
        let closes_column =
            last_len >= height || (matches!(row, Row::Heading(_)) && last_len + 1 >= height);
        if closes_column && last_len > 0 {
            if filled.len() == columns {
                dropped += 1;
                continue;
            }
            filled.push(Vec::new());
            last_len = 0;
        }
        if last_len >= height {
            dropped += 1;
            continue;
        }
        if let Some(column) = filled.last_mut() {
            column.push(*row);
        }
    }
    (filled, dropped)
}

struct Layout<'a> {
    columns: Vec<Vec<Row<'a>>>,
    column_width: u16,
    height: usize,
    dropped: usize,
}

fn choose_layout<'a>(
    rows: &[Row<'a>],
    content_width: u16,
    max_height: usize,
    key_width: u16,
    natural_label_width: u16,
) -> Option<Layout<'a>> {
    let natural_width = key_width + 2 + natural_label_width;
    let column_width_for = |columns: usize| -> u16 {
        let gaps = COLUMN_GAP * (columns as u16 - 1);
        (content_width.saturating_sub(gaps)) / columns as u16
    };
    let min_width = key_width + 2 + MIN_LABEL_WIDTH as u16;
    let fits = |columns: usize, width: u16| -> Option<Layout<'a>> {
        let total = rows.len();
        let start = total.div_ceil(columns).max(2);
        (start..=max_height).find_map(|height| {
            let (filled, dropped) = flow(rows, columns, height);
            (dropped == 0).then(|| Layout {
                height: filled.iter().map(Vec::len).max().unwrap_or(0),
                columns: filled,
                column_width: width,
                dropped,
            })
        })
    };
    // Prefer untruncated labels in as many columns as fit, then shrink columns.
    for columns in (1..=MAX_COLUMNS).rev() {
        let width = column_width_for(columns);
        if width >= natural_width {
            if let Some(layout) = fits(columns, natural_width) {
                return Some(layout);
            }
        }
    }
    for columns in 1..=MAX_COLUMNS {
        let width = column_width_for(columns);
        if width < min_width.min(natural_width) {
            break;
        }
        if let Some(layout) = fits(columns, width.min(natural_width)) {
            return Some(layout);
        }
    }
    // Nothing fits: show what does in the widest usable layout.
    let columns = (1..=MAX_COLUMNS)
        .rev()
        .find(|columns| column_width_for(*columns) >= min_width.min(natural_width))
        .unwrap_or(1);
    let width = column_width_for(columns).min(natural_width).max(1);
    let (filled, dropped) = flow(rows, columns, max_height.max(2));
    Some(Layout {
        height: filled.iter().map(Vec::len).max().unwrap_or(0),
        columns: filled,
        column_width: width,
        dropped,
    })
}

/// Draw the popup bottom-centered above `bottom` inside `bounds`. Returns the
/// occupied rectangle, or `None` when the terminal is too small.
pub(super) fn render_which_key(
    buffer: &mut Buffer,
    bounds: Rect,
    bottom: u16,
    groups: &[WhichKeyGroup],
    footer: &str,
    palette: &Palette,
) -> Option<Rect> {
    let bounds = bounds.intersection(buffer.area);
    let bottom = bottom.min(bounds.bottom());
    // Border rows (2) plus the footer row.
    let max_height = usize::from(bottom.saturating_sub(bounds.y)).checked_sub(3)?;
    let outer_width = bounds.width.saturating_sub(2).min(MAX_POPUP_WIDTH);
    // Border (2) and one cell of padding on each side.
    let content_width = outer_width.checked_sub(4)?;
    if max_height < 2 || content_width < 14 || groups.is_empty() {
        return None;
    }

    let rows = flatten(groups);
    let key_width = groups
        .iter()
        .flat_map(|group| &group.entries)
        .map(|entry| display_width(&entry.key))
        .max()
        .unwrap_or(1)
        .min(MAX_KEY_WIDTH as u16);
    let label_width = groups
        .iter()
        .flat_map(|group| &group.entries)
        .map(|entry| display_width(&entry.label))
        .max()
        .unwrap_or(1)
        .min(MAX_LABEL_WIDTH as u16);
    let layout = choose_layout(&rows, content_width, max_height, key_width, label_width)?;
    let columns = layout.columns.len().max(1) as u16;
    let used = columns * layout.column_width + COLUMN_GAP * (columns - 1);
    let width = (used + 4).min(outer_width).max(14);
    let height = (layout.height as u16 + 3).min(bottom.saturating_sub(bounds.y));
    let rect = Rect::new(
        bounds.x + (bounds.width - width) / 2,
        bottom - height,
        width,
        height,
    );
    let inner = draw_panel(buffer, rect, "which key", palette)?;
    let content = Rect::new(
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(2),
        inner.height.saturating_sub(1),
    );
    let label_max = usize::from(layout.column_width)
        .saturating_sub(usize::from(key_width) + 2)
        .max(1);
    let last_column = layout.columns.len().saturating_sub(1);
    for (index, column) in layout.columns.iter().enumerate() {
        let x = content.x + index as u16 * (layout.column_width + COLUMN_GAP);
        let width = layout.column_width.min(content.right().saturating_sub(x));
        for (offset, row) in column.iter().enumerate() {
            let y = content.y + offset as u16;
            if y >= content.bottom() {
                break;
            }
            let is_last_cell = layout.dropped > 0
                && index == last_column
                && offset + 1 == column.len().min(usize::from(content.height));
            if is_last_cell {
                let text = format!("… {} more", layout.dropped + 1);
                put_text(buffer, x, y, width, &text, dim_style(palette));
                continue;
            }
            match row {
                Row::Heading(title) => {
                    put_text(buffer, x, y, width, title, heading_style(palette));
                }
                Row::Entry(entry) => {
                    let key = crate::ui::truncate_end(&entry.key, usize::from(key_width));
                    put_text(buffer, x, y, width, &key, key_style(palette));
                    let label = crate::ui::truncate_end(&entry.label, label_max);
                    put_text(
                        buffer,
                        x + key_width + 2,
                        y,
                        width.saturating_sub(key_width + 2),
                        &label,
                        base_style(palette),
                    );
                }
            }
        }
    }
    let footer = crate::ui::truncate_end(footer, usize::from(content.width));
    put_text(
        buffer,
        content.x,
        inner.bottom().saturating_sub(1),
        content.width,
        &footer,
        dim_style(palette),
    );
    Some(rect)
}

impl ClientShellState {
    fn which_key_wanted(&self) -> bool {
        self.config.which_key.enabled
            && self.mode == ClientShellMode::Prefix
            && self.overlay.is_none()
    }

    /// Track prefix mode after input was handled. Returns true when the popup
    /// was visible and must be repainted away. The wait starts the moment the
    /// input handler leaves the client in prefix mode.
    pub(super) fn sync_which_key(&mut self, now: std::time::Instant) -> bool {
        if self.which_key_wanted() {
            self.which_key.prefix_since.get_or_insert(now);
            return false;
        }
        let was_visible = self.which_key.visible;
        self.which_key = WhichKeyState::default();
        was_visible
    }

    /// Show the popup once the configured delay has passed. Returns true when
    /// the frame must be repainted.
    pub(crate) fn tick_which_key(&mut self, now: std::time::Instant) -> bool {
        let mut repaint = self.sync_which_key(now);
        if let Some(since) = self.which_key.prefix_since {
            if !self.which_key.visible && now >= since + self.config.which_key.delay() {
                self.which_key.visible = true;
                repaint = true;
            }
        }
        repaint
    }

    pub(super) fn which_key_deadline(&self) -> Option<std::time::Instant> {
        self.which_key
            .prefix_since
            .filter(|_| !self.which_key.visible)
            .map(|since| since + self.config.which_key.delay())
    }

    /// Draw the popup above the mode bar when it is due. Returns its area.
    pub(super) fn render_which_key_popup(
        &self,
        buffer: &mut Buffer,
        bounds: Rect,
        mode_bar: Option<Rect>,
    ) -> Option<Rect> {
        if !self.which_key.visible || !self.which_key_wanted() {
            return None;
        }
        let groups = which_key_groups(&self.config.keybinds.keybinds);
        let footer = format!(
            "esc cancel   {} send prefix",
            self.config.keybinds.primary_prefix_label()
        );
        render_which_key(
            buffer,
            bounds,
            mode_bar.map_or(bounds.bottom(), |bar| bar.y),
            &groups,
            &footer,
            &self.config.palette,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::popup_frame::buffer_row;
    use super::super::tests::{frame_rows, snapshot, surface};
    use super::*;
    use crate::config::Config;
    use crate::input::TerminalKey;
    use crate::raw_input::RawInputEvent;
    use crossterm::event::KeyModifiers;

    fn live_keybinds(toml: &str) -> Keybinds {
        let config: Config = toml::from_str(toml).unwrap();
        config
            .live_keybinds_with_diagnostics()
            .expect("keybindings resolve")
            .0
            .keybinds
    }

    fn entry<'a>(groups: &'a [WhichKeyGroup], title: &str, label: &str) -> Option<&'a str> {
        groups
            .iter()
            .find(|group| group.title == title)?
            .entries
            .iter()
            .find(|entry| entry.label == label)
            .map(|entry| entry.key.as_str())
    }

    fn rows(buffer: &Buffer) -> Vec<String> {
        (buffer.area.y..buffer.area.bottom())
            .map(|y| buffer_row(buffer, y))
            .collect()
    }

    fn render(width: u16, height: u16, groups: &[WhichKeyGroup]) -> (Buffer, Option<Rect>) {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        let rect = render_which_key(
            &mut buffer,
            area,
            height - 1,
            groups,
            "esc cancel   ctrl+b send prefix",
            &Palette::catppuccin(),
        );
        (buffer, rect)
    }

    fn prefix_state() -> ClientShellState {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state
    }

    fn press(
        state: &mut ClientShellState,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> ClientShellInput {
        state.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(code, modifiers))])
    }

    #[test]
    fn groups_list_default_prefix_keys_by_category() {
        let groups = which_key_groups(&live_keybinds(""));
        assert_eq!(entry(&groups, "tabs", "new tab"), Some("c"));
        assert_eq!(entry(&groups, "tabs", "switch tab"), Some("1..9"));
        assert_eq!(entry(&groups, "agents", "rename agent"), Some("shift+a"));
        assert_eq!(
            entry(&groups, "agents", "collapse / expand agent group"),
            Some("shift+e")
        );
        assert_eq!(entry(&groups, "panes", "zoom pane"), Some("z"));
        assert_eq!(entry(&groups, "session", "detach"), Some("q"));
        assert_eq!(
            entry(&groups, "custom", "Review upstream herdr updates"),
            Some("u")
        );
        // Unbound actions have nothing to hint.
        assert_eq!(entry(&groups, "tabs", "move tab left"), None);
    }

    #[test]
    fn groups_follow_overrides_custom_commands_and_skip_direct_only_bindings() {
        let groups = which_key_groups(&live_keybinds(
            r#"
[keys]
new_tab = "prefix+t"
zoom = "ctrl+alt+z"
rename_agent = ["prefix+shift+a", "prefix+f2"]

[[keys.command]]
key = "prefix+y"
command = "echo hi"
description = "say hi"

[[keys.command]]
key = "ctrl+alt+y"
command = "echo direct"
description = "direct only"
"#,
        ));
        assert_eq!(entry(&groups, "tabs", "new tab"), Some("t"));
        assert_eq!(entry(&groups, "panes", "zoom pane"), None);
        assert_eq!(
            entry(&groups, "agents", "rename agent"),
            Some("shift+a / f2")
        );
        assert_eq!(entry(&groups, "custom", "say hi"), Some("y"));
        assert_eq!(entry(&groups, "custom", "direct only"), None);
        assert_eq!(
            entry(&groups, "custom", "Review upstream herdr updates"),
            Some("u")
        );
    }

    #[test]
    fn popup_fits_large_and_small_terminals() {
        let groups = which_key_groups(&live_keybinds(""));
        for (width, height) in [(200, 50), (100, 30), (80, 24), (60, 16), (40, 12)] {
            let (buffer, rect) = render(width, height, &groups);
            let rect = rect.unwrap_or_else(|| panic!("{width}x{height} renders"));
            assert!(
                buffer
                    .area
                    .contains(ratatui::layout::Position::new(rect.x, rect.y))
                    && rect.right() <= width
                    && rect.bottom() < height,
                "{width}x{height}: {rect:?}"
            );
            let text = rows(&buffer).join("\n");
            assert!(text.contains("which key"), "{width}x{height}\n{text}");
            assert!(text.contains("esc cancel"), "{width}x{height}\n{text}");
        }
        // Large terminals show every entry untruncated; small ones say how
        // many were left out.
        let (buffer, _) = render(200, 50, &groups);
        let text = rows(&buffer).join("\n");
        assert!(text.contains("collapse / expand agent group"), "{text}");
        assert!(text.contains("Review upstream herdr updates"), "{text}");
        assert!(!text.contains(" more"), "{text}");
        let (buffer, _) = render(40, 12, &groups);
        assert!(rows(&buffer).join("\n").contains("more"));
    }

    #[test]
    fn popup_uses_more_columns_on_wide_terminals() {
        let groups = which_key_groups(&live_keybinds(""));
        let height = |width: u16, rows: u16| {
            render(width, rows, &groups)
                .1
                .map(|rect| rect.height)
                .unwrap_or_default()
        };
        assert!(height(160, 50) < height(60, 50));
    }

    #[test]
    fn popup_is_skipped_when_there_is_no_room() {
        let groups = which_key_groups(&live_keybinds(""));
        assert_eq!(render(12, 20, &groups).1, None);
        assert_eq!(render(80, 4, &groups).1, None);
        assert_eq!(render(80, 24, &[]).1, None);
    }

    #[test]
    fn popup_appears_after_the_delay_and_never_before() {
        let mut state = prefix_state();
        let outcome = press(&mut state, KeyCode::Char('b'), KeyModifiers::CONTROL);
        assert_eq!(state.mode, ClientShellMode::Prefix);
        assert!(outcome.requests.is_empty() && outcome.actions.is_empty());
        let since = state.which_key.prefix_since.expect("wait started");
        assert!(!state.which_key.visible());
        assert_eq!(
            state.which_key_deadline(),
            Some(since + std::time::Duration::from_millis(400))
        );

        assert!(!state.tick_which_key(since + std::time::Duration::from_millis(399)));
        assert!(!state.which_key.visible());
        assert!(state.tick_which_key(since + std::time::Duration::from_millis(400)));
        assert!(state.which_key.visible());
        // Already showing: no further repaints.
        assert!(!state.tick_which_key(since + std::time::Duration::from_millis(900)));
        assert_eq!(state.which_key_deadline(), None);

        let frame = state.compose(100, 30).expect("frame");
        let text = frame_rows(&frame.frame).join("\n");
        assert!(text.contains("which key"), "{text}");
        assert!(text.contains("rename agent"), "{text}");
    }

    #[test]
    fn the_follow_up_key_is_handled_normally_and_hides_the_popup() {
        let mut state = prefix_state();
        press(&mut state, KeyCode::Char('b'), KeyModifiers::CONTROL);
        let since = state.which_key.prefix_since.expect("wait started");
        assert!(state.tick_which_key(since + std::time::Duration::from_secs(1)));
        assert!(state.which_key.visible());

        // `prefix+v` splits the pane exactly as it does without the popup.
        let outcome = press(&mut state, KeyCode::Char('v'), KeyModifiers::empty());
        assert!(outcome.repaint, "the popup must be painted away");
        assert!(!state.which_key.visible());
        assert_eq!(state.mode, ClientShellMode::Terminal);
        assert!(
            outcome.actions.iter().any(|action| matches!(
                action,
                ClientShellAction::Endpoint { request, .. }
                    if matches!(request.method, crate::api::schema::Method::PaneSplit(_))
            )),
            "{:?}",
            outcome.actions
        );
    }

    #[test]
    fn escape_hides_the_popup_without_forwarding_anything() {
        let mut state = prefix_state();
        press(&mut state, KeyCode::Char('b'), KeyModifiers::CONTROL);
        let since = state.which_key.prefix_since.expect("wait started");
        state.tick_which_key(since + std::time::Duration::from_secs(1));
        let outcome = press(&mut state, KeyCode::Esc, KeyModifiers::empty());
        assert!(outcome.repaint);
        assert!(!state.which_key.visible());
        assert!(outcome.requests.is_empty() && outcome.actions.is_empty());
    }

    #[test]
    fn a_key_before_the_delay_never_shows_the_popup() {
        let mut state = prefix_state();
        press(&mut state, KeyCode::Char('b'), KeyModifiers::CONTROL);
        press(&mut state, KeyCode::Char('z'), KeyModifiers::empty());
        assert_eq!(state.which_key.prefix_since, None);
        assert!(
            !state.tick_which_key(std::time::Instant::now() + std::time::Duration::from_secs(5))
        );
        assert!(!state.which_key.visible());
    }

    #[test]
    fn disabled_popup_never_shows() {
        let mut config = Config::default();
        config.ui.which_key.enabled = false;
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        press(&mut state, KeyCode::Char('b'), KeyModifiers::CONTROL);
        assert_eq!(state.mode, ClientShellMode::Prefix);
        assert!(
            !state.tick_which_key(std::time::Instant::now() + std::time::Duration::from_secs(5))
        );
        assert!(!state.which_key.visible());
        assert_eq!(state.which_key_deadline(), None);
    }

    #[test]
    fn delay_follows_the_config() {
        let mut config = Config::default();
        config.ui.which_key.delay_ms = 50;
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        press(&mut state, KeyCode::Char('b'), KeyModifiers::CONTROL);
        let since = state.which_key.prefix_since.expect("wait started");
        assert!(!state.tick_which_key(since + std::time::Duration::from_millis(49)));
        assert!(state.tick_which_key(since + std::time::Duration::from_millis(50)));
    }

    #[test]
    fn popup_reflects_endpoint_keybinding_overrides_live() {
        let mut state = prefix_state();
        let config: Config = toml::from_str("[keys]\nnew_tab = \"prefix+t\"\n").unwrap();
        state.config.keybinds = config.live_keybinds_with_diagnostics().unwrap().0;
        press(&mut state, KeyCode::Char('b'), KeyModifiers::CONTROL);
        let since = state.which_key.prefix_since.expect("wait started");
        state.tick_which_key(since + std::time::Duration::from_secs(1));
        let text = frame_rows(&state.compose(120, 40).expect("frame").frame).join("\n");
        let line = text
            .lines()
            .find(|line| line.contains("new tab"))
            .expect("new tab listed");
        let before = line[..line.find("new tab").unwrap()].trim_end();
        assert!(before.ends_with(" t"), "{line}");
    }
}
