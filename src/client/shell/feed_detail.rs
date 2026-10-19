//! The Feed overlay's detail pane: everything one item carries, laid out for
//! a given width, plus the drawing of a scrolled window of it.
//!
//! Building the lines is a pure function of the item, the width and the
//! clock, so the layout is tested without a terminal. The pane draws the
//! item's own facts (title, badge, state, subtitle, updated time, link, the
//! `fields` table and the wrapped `body`); the action key hints are pinned to
//! the pane's last row by the overlay and do not scroll.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthChar;

use super::popup_frame::{base_style, dim_style, heading_style};
use super::render::{display_width, put_text};
use crate::api::schema::{FeedItemInfo, FeedItemState};
use crate::app::state::Palette;
use crate::jobs::display;

/// Widest label column of the fields table.
const MAX_LABEL_WIDTH: usize = 16;

/// One styled piece of a detail line.
pub(super) type Span = (String, Style);
pub(super) type DetailLine = Vec<Span>;

fn text_width(text: &str) -> usize {
    usize::from(display_width(text))
}

/// Break `text` into lines of at most `width` columns. Line breaks in the
/// text stay; words wrap at spaces; a word wider than a line is cut.
pub(super) fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut line_width = 0;
        for word in paragraph.split(' ').filter(|word| !word.is_empty()) {
            let word_width = text_width(word);
            if !line.is_empty() && line_width + 1 + word_width > width {
                lines.push(std::mem::take(&mut line));
                line_width = 0;
            }
            if word_width > width {
                // A long word (a URL, a path) is cut where the line ends.
                if !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                }
                let mut piece = String::new();
                let mut piece_width = 0;
                for ch in word.chars() {
                    let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
                    if piece_width + ch_width > width && !piece.is_empty() {
                        lines.push(std::mem::take(&mut piece));
                        piece_width = 0;
                    }
                    piece.push(ch);
                    piece_width += ch_width;
                }
                line = piece;
                line_width = piece_width;
                continue;
            }
            if !line.is_empty() {
                line.push(' ');
                line_width += 1;
            }
            line.push_str(word);
            line_width += word_width;
        }
        lines.push(line);
    }
    lines
}

fn state_word(state: FeedItemState) -> &'static str {
    match state {
        FeedItemState::Unread => "unread",
        FeedItemState::Read => "read",
        FeedItemState::Done => "done",
        FeedItemState::Warn => "warning",
        FeedItemState::Error => "error",
    }
}

/// `2026-10-18 14:02 (4h ago)` in the local time zone, from an RFC 3339
/// timestamp. Falls back to the text as given when it does not parse.
pub(super) fn updated_text(updated_at: &str, now_ms: u64) -> String {
    let Some(secs) = crate::feed::format::parse_rfc3339_secs(updated_at) else {
        return updated_at.to_owned();
    };
    let absolute = local_clock(secs);
    let at_ms = u64::try_from(secs)
        .ok()
        .map(|secs| secs.saturating_mul(1000));
    let relative = match at_ms {
        Some(at_ms) if at_ms > now_ms => "in the future".to_owned(),
        Some(at_ms) => display::ago(now_ms, at_ms),
        None => String::new(),
    };
    if relative.is_empty() {
        absolute
    } else {
        format!("{absolute} ({relative})")
    }
}

fn local_clock(secs: i64) -> String {
    if let Some((year, month, day, hour, minute, _)) = crate::platform::local_civil_at(secs) {
        return format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}");
    }
    let (year, month, day) = crate::jobs::cron::civil_from_days(secs.div_euclid(86_400));
    let minutes = secs.rem_euclid(86_400) / 60;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        minutes / 60,
        minutes % 60
    )
}

/// The detail pane's lines for `item` at `width` columns.
pub(super) fn build_detail(
    item: &FeedItemInfo,
    width: usize,
    now_ms: u64,
    palette: &Palette,
) -> Vec<DetailLine> {
    let width = width.max(8);
    let base = base_style(palette);
    let dim = dim_style(palette);
    let mut lines: Vec<DetailLine> = Vec::new();

    for line in wrap_text(&item.title, width) {
        lines.push(vec![(line, heading_style(palette))]);
    }
    let mut meta: DetailLine = Vec::new();
    if let Some(badge) = &item.badge {
        meta.push((format!("{badge}  "), base.add_modifier(Modifier::BOLD)));
    }
    meta.push((state_word(item.state).to_owned(), dim));
    lines.push(meta);
    if let Some(subtitle) = &item.subtitle {
        for line in wrap_text(subtitle, width) {
            lines.push(vec![(line, dim)]);
        }
    }

    // The table: when, link, then the item's own fields.
    let mut rows: Vec<(String, String)> = Vec::new();
    if let Some(updated_at) = &item.updated_at {
        rows.push(("Updated".to_owned(), updated_text(updated_at, now_ms)));
    }
    if let Some(url) = &item.url {
        rows.push(("Link".to_owned(), url.clone()));
    }
    rows.extend(
        item.fields
            .iter()
            .map(|field| (field.label.clone(), field.value.clone())),
    );
    if !rows.is_empty() {
        lines.push(Vec::new());
        let label_width = rows
            .iter()
            .map(|(label, _)| text_width(label))
            .max()
            .unwrap_or(0)
            .min(MAX_LABEL_WIDTH)
            .min(width / 3);
        let value_width = width.saturating_sub(label_width + 2).max(4);
        for (label, value) in &rows {
            let label = crate::ui::truncate_end(label, label_width);
            let pad = label_width.saturating_sub(text_width(&label));
            let label_cell = format!("{label}{}  ", " ".repeat(pad));
            let blank = " ".repeat(label_width + 2);
            for (index, piece) in wrap_text(value, value_width).into_iter().enumerate() {
                let head = if index == 0 {
                    label_cell.clone()
                } else {
                    blank.clone()
                };
                lines.push(vec![(head, dim), (piece, base)]);
            }
        }
    }

    if let Some(body) = &item.body {
        lines.push(Vec::new());
        for line in wrap_text(body, width) {
            lines.push(vec![(line, base)]);
        }
    }
    lines
}

/// Draw the window of `lines` that starts at `scroll` into `area`.
pub(super) fn draw_lines(buffer: &mut Buffer, area: Rect, lines: &[DetailLine], scroll: usize) {
    for (row, line) in lines
        .iter()
        .skip(scroll)
        .take(usize::from(area.height))
        .enumerate()
    {
        let y = area.y + row as u16;
        let mut x = area.x;
        for (text, style) in line {
            let room = area.right().saturating_sub(x);
            if room == 0 {
                break;
            }
            put_text(buffer, x, y, room, text, *style);
            x = x.saturating_add(display_width(text).min(room));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::FeedField;

    fn palette() -> Palette {
        Palette::catppuccin()
    }

    fn plain(lines: &[DetailLine]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.iter().map(|(text, _)| text.as_str()).collect())
            .collect()
    }

    #[test]
    fn wrapping_keeps_line_breaks_and_cuts_long_words() {
        assert_eq!(wrap_text("aa bb cc", 5), ["aa bb", "cc"]);
        assert_eq!(wrap_text("one\n\ntwo", 10), ["one", "", "two"]);
        assert_eq!(wrap_text("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap_text("hi abcdefghij", 4), ["hi", "abcd", "efgh", "ij"]);
        assert_eq!(wrap_text("", 4), [""]);
        assert!(wrap_text("日本語のテキスト", 6)
            .iter()
            .all(|line| text_width(line) <= 6));
    }

    #[test]
    fn the_detail_lists_title_state_table_and_body() {
        let item = FeedItemInfo {
            id: "a".into(),
            title: "Remove sourceEnv-based QR response routing".into(),
            subtitle: Some("bitbucket · review requested".into()),
            state: FeedItemState::Unread,
            badge: Some("#339".into()),
            url: Some("https://example.com/pr/339".into()),
            updated_at: Some("2020-01-02T03:04:05Z".into()),
            body: Some("first line\nsecond line".into()),
            fields: vec![
                FeedField {
                    label: "Source".into(),
                    value: "bitbucket".into(),
                },
                FeedField {
                    label: "Today".into(),
                    value: "no".into(),
                },
            ],
            group: None,
            also_groups: Vec::new(),
            actions: Vec::new(),
        };
        let lines = plain(&build_detail(&item, 30, 1_700_000_000_000 * 2, &palette()));
        assert_eq!(lines[0], "Remove sourceEnv-based QR");
        assert_eq!(lines[1], "response routing");
        assert_eq!(lines[2], "#339  unread");
        assert_eq!(lines[3], "bitbucket · review requested");
        assert_eq!(lines[4], "");
        assert!(lines[5].starts_with("Updated  2020-01-0"), "{:?}", lines[5]);
        assert!(lines[6].contains("ago)"), "{:?}", lines[6]);
        assert!(lines
            .iter()
            .any(|line| line.starts_with("Link     https://")));
        assert!(lines.iter().any(|line| line == "Source   bitbucket"));
        assert!(lines.iter().any(|line| line == "Today    no"));
        assert_eq!(&lines[lines.len() - 2..], ["first line", "second line"]);
    }

    #[test]
    fn a_bare_item_has_no_table_or_body() {
        let item = FeedItemInfo {
            id: "a".into(),
            title: "Bare".into(),
            subtitle: None,
            state: FeedItemState::Read,
            badge: None,
            url: None,
            updated_at: None,
            body: None,
            fields: Vec::new(),
            group: None,
            also_groups: Vec::new(),
            actions: Vec::new(),
        };
        assert_eq!(
            plain(&build_detail(&item, 30, 0, &palette())),
            ["Bare", "read"]
        );
    }

    #[test]
    fn long_values_wrap_under_their_label() {
        let item = FeedItemInfo {
            id: "a".into(),
            title: "T".into(),
            subtitle: None,
            state: FeedItemState::Read,
            badge: None,
            url: None,
            updated_at: None,
            body: None,
            fields: vec![FeedField {
                label: "Where".into(),
                value: "alpha beta gamma delta".into(),
            }],
            group: None,
            also_groups: Vec::new(),
            actions: Vec::new(),
        };
        let lines = plain(&build_detail(&item, 17, 0, &palette()));
        assert_eq!(
            &lines[3..],
            ["Where  alpha beta", "       gamma", "       delta"]
        );
    }
}
