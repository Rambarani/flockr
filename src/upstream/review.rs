//! Rendering and input parsing for the upstream review popup
//! (`flockr upstream review`). The popup is a plain terminal screen opened in
//! flockr's popup pane, so it needs no new client/server protocol surface.

use super::release::{bucket_counts_label, bucket_release_body, bucket_releases, UpstreamRelease};

/// Items shown per bucket before the list is cut off with a "more" line.
const MAX_ITEMS_PER_BUCKET: usize = 3;
/// Raw note lines shown for a release whose notes could not be bucketed.
const MAX_RAW_LINES: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReviewChoice {
    CopyUpdates,
    SkipVersion,
    Later,
    ViewFull,
    Unknown,
}

/// Maps a typed line to an action. An empty line (just Enter) means "later" so
/// closing the popup never changes anything by accident.
pub(crate) fn parse_choice(input: &str) -> ReviewChoice {
    match input.trim().to_ascii_lowercase().as_str() {
        "c" | "copy" | "copy updates" => ReviewChoice::CopyUpdates,
        "s" | "skip" => ReviewChoice::SkipVersion,
        "" | "l" | "later" | "q" | "quit" | "esc" => ReviewChoice::Later,
        "v" | "view" | "full" => ReviewChoice::ViewFull,
        _ => ReviewChoice::Unknown,
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{kept}…")
}

fn push_bucket(out: &mut String, label: &str, items: &[String], width: usize) {
    if items.is_empty() {
        return;
    }
    out.push_str(&format!("  {label} ({})\n", items.len()));
    for item in items.iter().take(MAX_ITEMS_PER_BUCKET) {
        out.push_str(&format!(
            "    - {}\n",
            truncate(item, width.saturating_sub(6))
        ));
    }
    if items.len() > MAX_ITEMS_PER_BUCKET {
        out.push_str(&format!(
            "    ... and {} more (v shows the full notes)\n",
            items.len() - MAX_ITEMS_PER_BUCKET
        ));
    }
}

/// The summary screen: one block per pending release, newest last.
pub(crate) fn render_summary(base_tag: &str, pending: &[UpstreamRelease], width: usize) -> String {
    let width = width.clamp(40, 200);
    let mut out = String::new();
    out.push_str("flockr: upstream herdr updates\n");
    if pending.is_empty() {
        out.push_str(&format!(
            "\nflockr is synced to herdr {base_tag}; no new upstream releases.\n"
        ));
        return out;
    }
    let all = bucket_releases(pending);
    let counts = bucket_counts_label(&all).unwrap_or_else(|| "see notes".to_string());
    out.push_str(&format!(
        "synced to herdr {base_tag}; {} new release{} ({counts})\n",
        pending.len(),
        if pending.len() == 1 { "" } else { "s" }
    ));
    for release in pending {
        out.push('\n');
        let date = release
            .published_at
            .as_deref()
            .and_then(|stamp| stamp.get(..10))
            .unwrap_or("");
        out.push_str(&format!("{} {date}\n", release.tag));
        if !release.url.is_empty() {
            out.push_str(&format!("  {}\n", release.url));
        }
        let buckets = bucket_release_body(&release.body);
        if buckets.is_empty() {
            let lines: Vec<&str> = release
                .body
                .lines()
                .map(str::trim_end)
                .filter(|line| !line.trim().is_empty())
                .collect();
            if lines.is_empty() {
                out.push_str("  (no release notes)\n");
            }
            for line in lines.iter().take(MAX_RAW_LINES) {
                out.push_str(&format!("  {}\n", truncate(line, width.saturating_sub(2))));
            }
            if lines.len() > MAX_RAW_LINES {
                out.push_str("  ... (v shows the full notes)\n");
            }
        } else {
            push_bucket(&mut out, "Features", &buckets.features, width);
            push_bucket(&mut out, "Fixes", &buckets.fixes, width);
            push_bucket(&mut out, "Other", &buckets.other, width);
        }
    }
    out
}

/// Full, unabridged notes of every pending release.
pub(crate) fn render_full(pending: &[UpstreamRelease]) -> String {
    let mut out = String::new();
    for release in pending {
        out.push_str(&format!("===== {} =====\n", release.tag));
        if release.body.trim().is_empty() {
            out.push_str("(no release notes)\n");
        } else {
            out.push_str(release.body.trim_end());
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

pub(crate) const PROMPT_LINE: &str =
    "[c] Copy updates   [s] Skip this version   [l] Later (default)   [v] View full notes";

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, body: &str) -> UpstreamRelease {
        UpstreamRelease {
            tag: tag.to_string(),
            name: tag.to_string(),
            body: body.to_string(),
            prerelease: false,
            published_at: Some("2026-10-20T10:00:00Z".to_string()),
            url: format!("https://github.com/herdrdev/herdr/releases/tag/{tag}"),
        }
    }

    #[test]
    fn choices_parse_with_safe_default() {
        assert_eq!(parse_choice("c"), ReviewChoice::CopyUpdates);
        assert_eq!(parse_choice(" Copy "), ReviewChoice::CopyUpdates);
        assert_eq!(parse_choice("S"), ReviewChoice::SkipVersion);
        assert_eq!(parse_choice("skip"), ReviewChoice::SkipVersion);
        assert_eq!(parse_choice(""), ReviewChoice::Later);
        assert_eq!(parse_choice("\n"), ReviewChoice::Later);
        assert_eq!(parse_choice("l"), ReviewChoice::Later);
        assert_eq!(parse_choice("q"), ReviewChoice::Later);
        assert_eq!(parse_choice("v"), ReviewChoice::ViewFull);
        assert_eq!(parse_choice("delete everything"), ReviewChoice::Unknown);
    }

    #[test]
    fn summary_lists_buckets_with_counts_dates_and_links() {
        let pending = vec![release(
            "v0.9.7",
            "### Added\n- a\n- b\n### Fixed\n- c\n### Changed\n- d",
        )];
        let text = render_summary("v0.9.5", &pending, 80);
        assert!(text
            .contains("synced to herdr v0.9.5; 1 new release (2 features, 1 fix, 1 other change)"));
        assert!(text.contains("v0.9.7 2026-10-20"));
        assert!(text.contains("https://github.com/herdrdev/herdr/releases/tag/v0.9.7"));
        assert!(text.contains("Features (2)"));
        assert!(text.contains("    - a"));
        assert!(text.contains("Fixes (1)"));
        assert!(text.contains("Other (1)"));
    }

    #[test]
    fn summary_truncates_long_lists_and_lines() {
        let items: String = (0..9).map(|i| format!("- item {i}\n")).collect();
        let long = "x".repeat(300);
        let body = format!("### Fixed\n{items}- {long}\n");
        let text = render_summary("v0.9.5", &[release("v0.9.6", &body)], 60);
        assert!(text.contains("... and 7 more"));
        assert!(text.lines().all(|line| line.chars().count() <= 60));
    }

    #[test]
    fn summary_shows_raw_notes_when_nothing_could_be_bucketed() {
        let body = "This is a hotfix release.\nSee the v0.9.4 notes.";
        let text = render_summary("v0.9.5", &[release("v0.9.6", body)], 80);
        assert!(text.contains("  This is a hotfix release."));
        assert!(text.contains("  See the v0.9.4 notes."));
        assert!(text.contains("(see notes)"));
    }

    #[test]
    fn summary_handles_empty_and_missing_notes() {
        let text = render_summary("v0.9.5", &[], 80);
        assert!(text.contains("no new upstream releases"));
        let text = render_summary("v0.9.5", &[release("v0.9.6", "")], 80);
        assert!(text.contains("(no release notes)"));
    }

    #[test]
    fn full_view_prints_every_release_body() {
        let text = render_full(&[release("v0.9.6", "### Fixed\n- a"), release("v0.9.7", "")]);
        assert!(text.contains("===== v0.9.6 ====="));
        assert!(text.contains("- a"));
        assert!(text.contains("===== v0.9.7 =====\n(no release notes)"));
    }
}
