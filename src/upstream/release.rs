//! Upstream release records, version selection and release-note bucketing.

use serde::{Deserialize, Serialize};

use crate::update::Version;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UpstreamRelease {
    pub tag: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub url: String,
}

/// Subset of the GitHub releases API object that flockr reads.
#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
}

/// Parses `v0.9.7`, `0.9.7` and `v0.9.7-rc.1` (the prerelease suffix is dropped
/// for ordering; prereleases are filtered through the `prerelease` flag).
pub(crate) fn parse_tag_version(tag: &str) -> Option<Version> {
    let core = tag
        .trim()
        .split(['-', '+'])
        .next()
        .filter(|core| !core.is_empty())?;
    Version::parse(core)
}

/// Parses the GitHub releases listing. Drafts are dropped.
pub(crate) fn parse_releases_json(bytes: &[u8]) -> Result<Vec<UpstreamRelease>, String> {
    let releases: Vec<GithubRelease> = serde_json::from_slice(bytes)
        .map_err(|e| format!("failed to parse upstream releases JSON: {e}"))?;
    Ok(releases
        .into_iter()
        .filter(|release| !release.draft)
        .map(|release| UpstreamRelease {
            name: release.name.unwrap_or_default(),
            body: release.body.unwrap_or_default(),
            prerelease: release.prerelease,
            published_at: release.published_at,
            url: release.html_url.unwrap_or_default(),
            tag: release.tag_name,
        })
        .collect())
}

/// Releases newer than `base`, not skipped, oldest first. Tags that are not
/// semver (for example `preview-2026-10-14-abc`) cannot be compared and are
/// ignored.
pub(crate) fn pending_releases(
    releases: &[UpstreamRelease],
    base: &Version,
    include_prereleases: bool,
    skipped: &[String],
) -> Vec<UpstreamRelease> {
    let mut pending: Vec<(Version, UpstreamRelease)> = releases
        .iter()
        .filter(|release| include_prereleases || !release.prerelease)
        .filter(|release| !skipped.iter().any(|tag| tag == &release.tag))
        .filter_map(|release| {
            let version = parse_tag_version(&release.tag)?;
            (version > *base).then(|| (version, release.clone()))
        })
        .collect();
    pending.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.tag.cmp(&b.1.tag)));
    pending.dedup_by(|a, b| a.1.tag == b.1.tag);
    pending.into_iter().map(|(_, release)| release).collect()
}

/// Release-note items grouped by kind. Best effort: when nothing could be
/// classified the caller shows the raw body instead.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct ReleaseBuckets {
    pub features: Vec<String>,
    pub fixes: Vec<String>,
    pub other: Vec<String>,
}

impl ReleaseBuckets {
    pub(crate) fn is_empty(&self) -> bool {
        self.features.is_empty() && self.fixes.is_empty() && self.other.is_empty()
    }

    fn merge(&mut self, other: ReleaseBuckets) {
        self.features.extend(other.features);
        self.fixes.extend(other.fixes);
        self.other.extend(other.other);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bucket {
    Feature,
    Fix,
    Other,
}

fn classify_heading(heading: &str) -> Bucket {
    let heading = heading.to_ascii_lowercase();
    if heading.contains("fix") || heading.contains("bug") {
        Bucket::Fix
    } else if heading.contains("add")
        || heading.contains("feature")
        || heading.contains("new")
        || heading.contains("enhanc")
        || heading.contains("highlight")
    {
        Bucket::Feature
    } else {
        Bucket::Other
    }
}

/// Classifies a bullet that is not under a recognizable heading by its own
/// leading words (`feat: ...`, `Fixed ...`, `Add ...`).
fn classify_item(item: &str) -> Bucket {
    let lower = item.to_ascii_lowercase();
    let first_word: String = lower
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    match first_word.as_str() {
        "feat" | "feature" | "features" | "add" | "adds" | "added" | "new" => Bucket::Feature,
        "fix" | "fixes" | "fixed" | "bugfix" => Bucket::Fix,
        _ => Bucket::Other,
    }
}

fn bullet_text(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    // Indented continuation bullets are sub-items of the previous bullet.
    if line.len() - trimmed.len() >= 2 {
        return None;
    }
    let rest = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))?;
    let rest = rest.trim();
    (!rest.is_empty()).then_some(rest)
}

pub(crate) fn bucket_release_body(body: &str) -> ReleaseBuckets {
    let mut buckets = ReleaseBuckets::default();
    let mut section: Option<Bucket> = None;
    let mut in_fence = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if trimmed.starts_with('#') {
            let heading = trimmed.trim_start_matches('#').trim();
            // A release title such as "## [0.9.5] - 2026-10-14" resets context.
            let is_version_title = heading.starts_with('[')
                || heading
                    .strip_prefix('v')
                    .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()));
            section = if heading.is_empty() || is_version_title {
                None
            } else {
                Some(classify_heading(heading))
            };
            continue;
        }
        let Some(item) = bullet_text(line) else {
            continue;
        };
        let bucket = section.unwrap_or_else(|| classify_item(item));
        let target = match bucket {
            Bucket::Feature => &mut buckets.features,
            Bucket::Fix => &mut buckets.fixes,
            Bucket::Other => &mut buckets.other,
        };
        target.push(item.to_string());
    }
    buckets
}

pub(crate) fn bucket_releases(releases: &[UpstreamRelease]) -> ReleaseBuckets {
    let mut all = ReleaseBuckets::default();
    for release in releases {
        all.merge(bucket_release_body(&release.body));
    }
    all
}

fn count_label(count: usize, singular: &str, plural: &str) -> Option<String> {
    match count {
        0 => None,
        1 => Some(format!("1 {singular}")),
        n => Some(format!("{n} {plural}")),
    }
}

/// `"3 features, 5 fixes"`, or `None` when nothing was classified.
pub(crate) fn bucket_counts_label(buckets: &ReleaseBuckets) -> Option<String> {
    let parts: Vec<String> = [
        count_label(buckets.features.len(), "feature", "features"),
        count_label(buckets.fixes.len(), "fix", "fixes"),
        count_label(buckets.other.len(), "other change", "other changes"),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// Toast title for the pending releases, for example
/// `herdr v0.9.7 available upstream — 3 features, 5 fixes`.
pub(crate) fn notification_title(pending: &[UpstreamRelease]) -> Option<String> {
    let latest = pending.last()?;
    let mut title = format!("herdr {} available upstream", latest.tag);
    if pending.len() > 1 {
        title.push_str(&format!(" ({} new releases)", pending.len()));
    }
    if let Some(counts) = bucket_counts_label(&bucket_releases(pending)) {
        title.push_str(" — ");
        title.push_str(&counts);
    }
    Some(title)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, prerelease: bool, body: &str) -> UpstreamRelease {
        UpstreamRelease {
            tag: tag.to_string(),
            name: tag.to_string(),
            body: body.to_string(),
            prerelease,
            published_at: Some("2026-10-14T16:07:28Z".to_string()),
            url: format!("https://github.com/herdrdev/herdr/releases/tag/{tag}"),
        }
    }

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn tag_versions_compare_numerically_not_lexically() {
        assert_eq!(parse_tag_version("v0.9.10"), Some(v("0.9.10")));
        assert!(parse_tag_version("v0.9.10").unwrap() > parse_tag_version("v0.9.9").unwrap());
        assert!(parse_tag_version("v0.10.0").unwrap() > parse_tag_version("v0.9.99").unwrap());
        assert_eq!(parse_tag_version("0.9.7"), Some(v("0.9.7")));
        assert_eq!(parse_tag_version("v0.9.7-rc.1"), Some(v("0.9.7")));
        assert_eq!(parse_tag_version("preview-2026-10-14-fbd2ae23927f"), None);
        assert_eq!(parse_tag_version("v1.2"), None);
        assert_eq!(parse_tag_version(""), None);
        assert_eq!(parse_tag_version("-rc1"), None);
    }

    #[test]
    fn pending_releases_are_newer_than_base_sorted_and_not_skipped() {
        let releases = vec![
            release("v0.9.10", false, ""),
            release("v0.9.5", false, ""),
            release("v0.9.7", false, ""),
            release("v0.9.4", false, ""),
            release("preview-2026-10-14-fbd2ae23927f", true, ""),
            release("v0.9.6", false, ""),
        ];
        let pending = pending_releases(&releases, &v("0.9.5"), false, &["v0.9.6".to_string()]);
        let tags: Vec<_> = pending.iter().map(|r| r.tag.as_str()).collect();
        assert_eq!(tags, ["v0.9.7", "v0.9.10"]);
    }

    #[test]
    fn pending_releases_include_prereleases_only_when_asked() {
        let releases = vec![
            release("v0.9.6", false, ""),
            release("v0.9.7-rc.1", true, ""),
        ];
        let stable = pending_releases(&releases, &v("0.9.5"), false, &[]);
        assert_eq!(stable.len(), 1);
        let all = pending_releases(&releases, &v("0.9.5"), true, &[]);
        let tags: Vec<_> = all.iter().map(|r| r.tag.as_str()).collect();
        assert_eq!(tags, ["v0.9.6", "v0.9.7-rc.1"]);
    }

    #[test]
    fn pending_releases_empty_when_up_to_date() {
        let releases = vec![release("v0.9.5", false, ""), release("v0.9.4", false, "")];
        assert!(pending_releases(&releases, &v("0.9.5"), true, &[]).is_empty());
    }

    #[test]
    fn github_json_drops_drafts_and_maps_fields() {
        let json = r#"[
            {"tag_name":"v0.9.7","name":"v0.9.7","body":"- x\n- y","draft":false,"prerelease":false,
             "published_at":"2026-10-20T00:00:00Z","html_url":"https://github.com/herdrdev/herdr/releases/tag/v0.9.7","id":1},
            {"tag_name":"v0.9.8","draft":true,"prerelease":false},
            {"tag_name":"preview-1","body":null,"draft":false,"prerelease":true}
        ]"#;
        let releases = parse_releases_json(json.as_bytes()).unwrap();
        assert_eq!(releases.len(), 2);
        assert_eq!(releases[0].tag, "v0.9.7");
        assert_eq!(releases[0].body, "- x\n- y");
        assert_eq!(
            releases[0].url,
            "https://github.com/herdrdev/herdr/releases/tag/v0.9.7"
        );
        assert!(releases[1].prerelease);
        assert!(releases[1].body.is_empty());
        assert!(parse_releases_json(b"{}").is_err());
    }

    #[test]
    fn bucketing_uses_keep_a_changelog_headings() {
        let body = "\
Intro paragraph that is not an item.

### Added
- `herdr plugin update` updates plugins. (#4245)
- Windows notifications stay in Notification Center.

### Changed
- An elevated Windows server only accepts administrator clients.

### Fixed
- Restarting a Mac keeps your sessions.
- Session saves are written before they replace the old file.
  - nested detail is not a separate item
- The server keeps running when processes run out.
";
        let buckets = bucket_release_body(body);
        assert_eq!(buckets.features.len(), 2);
        assert_eq!(buckets.fixes.len(), 3);
        assert_eq!(buckets.other.len(), 1);
        assert!(buckets.features[0].starts_with("`herdr plugin update`"));
        assert_eq!(
            bucket_counts_label(&buckets).as_deref(),
            Some("2 features, 3 fixes, 1 other change")
        );
    }

    #[test]
    fn bucketing_classifies_headingless_bullets_by_leading_word() {
        let body = "- feat: add sound picker\n- Fixed a crash on resize\n* Docs tweak\n+ Add thing";
        let buckets = bucket_release_body(body);
        assert_eq!(buckets.features, ["feat: add sound picker", "Add thing"]);
        assert_eq!(buckets.fixes, ["Fixed a crash on resize"]);
        assert_eq!(buckets.other, ["Docs tweak"]);
    }

    #[test]
    fn bucketing_ignores_code_fences_and_returns_empty_for_prose() {
        let body = "Just prose.\n```\n- not an item\n```\nMore prose.";
        let buckets = bucket_release_body(body);
        assert!(buckets.is_empty());
        assert_eq!(bucket_counts_label(&buckets), None);
    }

    #[test]
    fn release_title_heading_resets_section_context() {
        let body = "### Fixed\n- a\n## [0.9.4] - 2026-10-13\n- feat: b";
        let buckets = bucket_release_body(body);
        assert_eq!(buckets.fixes, ["a"]);
        assert_eq!(buckets.features, ["feat: b"]);
    }

    #[test]
    fn notification_title_summarizes_counts_and_release_count() {
        let single = vec![release(
            "v0.9.7",
            false,
            "### Added\n- a\n- b\n- c\n### Fixed\n- 1\n- 2\n- 3\n- 4\n- 5",
        )];
        assert_eq!(
            notification_title(&single).as_deref(),
            Some("herdr v0.9.7 available upstream — 3 features, 5 fixes")
        );

        let two = vec![
            release("v0.9.6", false, "### Fixed\n- 1"),
            release("v0.9.7", false, "### Added\n- a"),
        ];
        assert_eq!(
            notification_title(&two).as_deref(),
            Some("herdr v0.9.7 available upstream (2 new releases) — 1 feature, 1 fix")
        );

        let prose = vec![release("v0.9.8", false, "Only prose")];
        assert_eq!(
            notification_title(&prose).as_deref(),
            Some("herdr v0.9.8 available upstream")
        );
        assert_eq!(notification_title(&[]), None);
    }
}
