//! `flockr feed adapt workbox`: turns the JSON array printed by
//! `workbox list --json` or `workbox status --json` into a feed.
//!
//! Each item becomes a feed item whose id is workbox's item id, badge is its
//! key, state is `unread` or `read`, and subtitle is
//! `<source> · <reason>[ · <where>]`. The actions mark the item done (`d`),
//! seen (`s`), or toggle it on the today list (`t`) by running
//! `workbox mark <id> <action>`. The detail view gets `fields` (Source,
//! Reason, Where, Repo (the repository name of a GitHub or Bitbucket item's
//! `owner/repo#123`), Status, From, Key, Snoozed until, Today) and the snippet as
//! `body`, the same facts workbox's own detail pane shows.
//!
//! Items are grouped into categories (tabs in the Feed overlay) from their
//! `source` and `reason`, following workbox's own `tabs::classify`: see
//! `CATEGORIES` and `RULES`. The `tab` field is not used, because workbox
//! leaves it empty for exactly the items that fall outside its four tabs.

use serde::Deserialize;

pub(crate) const DEFAULT_TITLE: &str = "workbox";
pub(crate) const DEFAULT_BIN: &str = "workbox";

#[derive(Debug, Clone)]
pub(crate) struct AdaptOptions {
    pub(crate) title: String,
    /// The program the action commands run.
    pub(crate) bin: String,
}

impl Default for AdaptOptions {
    fn default() -> Self {
        Self {
            title: DEFAULT_TITLE.to_owned(),
            bin: DEFAULT_BIN.to_owned(),
        }
    }
}

/// The categories, in tab order: id, title, and the shorter title for narrow
/// places. Items that match no rule are left without a group and land in the
/// feed's implicit "Other".
const CATEGORIES: &[(&str, &str, &str)] = &[
    ("new-house-prs", "New House PR Requests", "New House PRs"),
    ("ticket-mentions", "JIRA Mentions", "JIRA Mentions"),
    ("jira-assigned", "New Tickets Assigned", "New Tickets"),
    ("old-house-prs", "Old House PR Requests", "Old House PRs"),
    ("my-prs", "My PRs (activity)", "My PRs"),
];

/// Which category a `source` and `reason` (workbox's `Reason::as_str`) belong
/// to. The first matching rule wins.
///
/// - GitHub `review_requested` and `mention`: someone wants me on a new-house
///   PR. Workbox shows these on its "New House" tab with its other GitHub
///   reasons; a mention on a PR is a request to look, so it stays here (the
///   `review-pr` skill treats the two the same way).
/// - GitHub `changes_requested`, `commented`, `approved` come only from
///   workbox's activity scan of my own open PRs (and `authored` is my own
///   PR), so they are "My PRs (activity)", not requests of me. Workbox shows
///   them on the New House tab, but mixing them in would hide the actual
///   requests; they stay visible in their own tab.
/// - Jira `mention` (and `commented`, which workbox never emits for Jira)
///   are mentions; Jira `assigned` is a ticket assigned to me. Workbox has no
///   Jira `status_changed` item (only GitHub notifications give that reason),
///   so it is not mapped.
/// - Bitbucket `review_requested` is the old-house PR request. Other
///   Bitbucket reasons (my own PRs) have no workbox tab; `authored`,
///   `changes_requested`, `commented` and `approved` go to My PRs.
/// - Everything else (`status_changed`, `updated`, unknown reasons, other
///   sources) is "Other".
const RULES: &[(&str, &[&str], &str)] = &[
    ("github", &["review_requested", "mention"], "new-house-prs"),
    ("jira", &["mention", "commented"], "ticket-mentions"),
    ("jira", &["assigned"], "jira-assigned"),
    ("bitbucket", &["review_requested"], "old-house-prs"),
    (
        "github",
        &["authored", "changes_requested", "commented", "approved"],
        "my-prs",
    ),
    (
        "bitbucket",
        &["authored", "changes_requested", "commented", "approved"],
        "my-prs",
    ),
];

/// The category id for an item, or `None` for "Other".
fn category(source: &str, reason: &str) -> Option<&'static str> {
    let source = source.trim().to_ascii_lowercase();
    let reason = reason.trim().to_ascii_lowercase();
    RULES
        .iter()
        .find(|(rule_source, reasons, _)| {
            *rule_source == source && reasons.contains(&reason.as_str())
        })
        .map(|(_, _, id)| *id)
}

/// The fields of one `workbox list --json` element that the feed uses.
/// Other fields (`tab`, ...) are ignored, so new workbox fields never break
/// the adapter.
#[derive(Debug, Deserialize)]
struct WorkboxItem {
    #[serde(default)]
    id: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    key: String,
    #[serde(default)]
    title: String,
    #[serde(default, rename = "where")]
    where_: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    unread: bool,
    #[serde(default)]
    today: bool,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    actor: Option<String>,
    #[serde(default)]
    snippet: Option<String>,
    #[serde(default)]
    snoozed_until: Option<String>,
}

/// The labelled values for the detail view. `Updated` is left out: the
/// detail view already shows the item's `updated_at` in local time.
fn fields(item: &WorkboxItem) -> Vec<serde_json::Value> {
    let reason = item.reason.replace('_', " ");
    let optional = |text: &Option<String>| text.as_deref().and_then(non_empty).map(str::to_owned);
    let rows = [
        ("Source", non_empty(&item.source).map(str::to_owned)),
        ("Reason", non_empty(&reason).map(str::to_owned)),
        ("Where", non_empty(&item.where_).map(str::to_owned)),
        ("Repo", repo_name(item).map(str::to_owned)),
        ("Status", optional(&item.status)),
        ("From", optional(&item.actor)),
        ("Key", non_empty(&item.key).map(str::to_owned)),
        ("Snoozed until", optional(&item.snoozed_until)),
        (
            "Today",
            Some(if item.today { "yes" } else { "no" }.to_owned()),
        ),
    ];
    rows.into_iter()
        .filter_map(|(label, value)| {
            value.map(|value| serde_json::json!({"label": label, "value": value}))
        })
        .collect()
}

/// The repository name (`repo` of `owner/repo#123`) of a GitHub or Bitbucket
/// item, from its `where`, so a job can map it to a local checkout with
/// `{{item.field.repo}}`. Jira items have a project key, not a repository, and
/// a `where` that is not `owner/repo#<key>` gives nothing.
fn repo_name(item: &WorkboxItem) -> Option<&str> {
    let source = item.source.trim();
    if !(source.eq_ignore_ascii_case("github") || source.eq_ignore_ascii_case("bitbucket")) {
        return None;
    }
    let (repository, _key) = item.where_.trim().rsplit_once('#')?;
    let (owner, repo) = repository.split_once('/')?;
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    };
    (valid(owner) && valid(repo) && repo != "." && repo != "..").then_some(repo)
}

fn non_empty(text: &str) -> Option<&str> {
    let text = text.trim();
    (!text.is_empty()).then_some(text)
}

fn subtitle(item: &WorkboxItem) -> String {
    let reason = item.reason.replace('_', " ");
    [
        non_empty(&item.source),
        non_empty(&reason),
        non_empty(&item.where_),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

fn action(bin: &str, id: &str, key: &str, title: &str, verb: &str) -> serde_json::Value {
    serde_json::json!({
        "key": key,
        "title": title,
        "command": [bin, "mark", id, verb],
    })
}

/// Convert workbox's JSON array into a feed document. `Err` is a message for
/// stderr; the warnings name items that were skipped.
pub(crate) fn adapt(
    input: &str,
    options: &AdaptOptions,
) -> Result<(serde_json::Value, Vec<String>), String> {
    let items: Vec<serde_json::Value> = serde_json::from_str(input.trim()).map_err(|err| {
        format!("expected the JSON array printed by `workbox list --json`: {err}")
    })?;
    let mut warnings = Vec::new();
    let mut feed_items = Vec::with_capacity(items.len());
    for (position, value) in items.into_iter().enumerate() {
        let item: WorkboxItem = match serde_json::from_value(value) {
            Ok(item) => item,
            Err(err) => {
                warnings.push(format!("item {position}: {err}; skipped"));
                continue;
            }
        };
        let Some(id) = non_empty(&item.id) else {
            warnings.push(format!("item {position}: no id; skipped"));
            continue;
        };
        let mut entry = serde_json::Map::new();
        entry.insert("id".into(), id.into());
        entry.insert("title".into(), non_empty(&item.title).unwrap_or(id).into());
        let subtitle = subtitle(&item);
        if !subtitle.is_empty() {
            entry.insert("subtitle".into(), subtitle.into());
        }
        entry.insert(
            "state".into(),
            if item.unread { "unread" } else { "read" }.into(),
        );
        if let Some(key) = non_empty(&item.key) {
            entry.insert("badge".into(), key.into());
        }
        if let Some(url) = non_empty(&item.url) {
            entry.insert("url".into(), url.into());
        }
        if let Some(updated_at) = non_empty(&item.updated_at) {
            entry.insert("updated_at".into(), updated_at.into());
        }
        if let Some(group) = category(&item.source, &item.reason) {
            entry.insert("group".into(), group.into());
        }
        entry.insert("fields".into(), serde_json::Value::Array(fields(&item)));
        if let Some(snippet) = item.snippet.as_deref().and_then(non_empty) {
            entry.insert("body".into(), snippet.into());
        }
        let bin = options.bin.as_str();
        entry.insert(
            "actions".into(),
            serde_json::Value::Array(vec![
                action(bin, id, "d", "done", "done"),
                action(bin, id, "s", "seen", "seen"),
                if item.today {
                    action(bin, id, "t", "untoday", "untoday")
                } else {
                    action(bin, id, "t", "today", "today")
                },
            ]),
        );
        feed_items.push(serde_json::Value::Object(entry));
    }
    Ok((
        serde_json::json!({
            "version": 1,
            "title": options.title,
            "groups": CATEGORIES
                .iter()
                .map(|(id, title, short_title)| {
                    serde_json::json!({"id": id, "title": title, "short_title": short_title})
                })
                .collect::<Vec<_>>(),
            "items": feed_items,
        }),
        warnings,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::FeedItemState;
    use crate::feed::format::parse_feed;

    /// A sample of `workbox list --json`, field for field.
    const SAMPLE: &str = r#"[
      {"id":"jira:NH-1412","source":"jira","key":"NH-1412","title":"Fix login redirect",
       "where":"NH board","reason":"mention","status":"In Progress","tab":"jira","actor":"alice",
       "url":"https://example.atlassian.net/browse/NH-1412","updated_at":"2026-10-18T08:30:00Z",
       "snippet":"can you look?","unread":true,"today":false,"snoozed_until":null},
      {"id":"gh:o/r#12","source":"github","key":"o/r#12","title":"Add retry to uploader",
       "where":"","reason":"review_requested","status":null,"tab":"new-house","actor":null,
       "url":"https://github.com/o/r/pull/12","updated_at":"2026-10-17T17:00:00Z",
       "snippet":null,"unread":false,"today":true,"snoozed_until":null}
    ]"#;

    fn adapted(options: &AdaptOptions) -> (serde_json::Value, Vec<String>) {
        adapt(SAMPLE, options).expect("adapts")
    }

    #[test]
    fn the_sample_array_becomes_a_feed_with_the_documented_fields() {
        let (feed, warnings) = adapted(&AdaptOptions::default());
        assert!(warnings.is_empty());
        assert_eq!(feed["version"], 1);
        assert_eq!(feed["title"], "workbox");
        let first = &feed["items"][0];
        assert_eq!(first["id"], "jira:NH-1412");
        assert_eq!(first["title"], "Fix login redirect");
        assert_eq!(first["subtitle"], "jira · mention · NH board");
        assert_eq!(first["state"], "unread");
        assert_eq!(first["badge"], "NH-1412");
        assert_eq!(first["url"], "https://example.atlassian.net/browse/NH-1412");
        assert_eq!(first["updated_at"], "2026-10-18T08:30:00Z");
        let keys: Vec<_> = first["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|action| action["key"].as_str().unwrap())
            .collect();
        assert_eq!(keys, ["d", "s", "t"]);
        assert_eq!(
            first["actions"][0]["command"],
            serde_json::json!(["workbox", "mark", "jira:NH-1412", "done"])
        );
        assert_eq!(
            first["actions"][1]["command"],
            serde_json::json!(["workbox", "mark", "jira:NH-1412", "seen"])
        );
        assert_eq!(
            first["actions"][2]["command"],
            serde_json::json!(["workbox", "mark", "jira:NH-1412", "today"])
        );
    }

    #[test]
    fn read_items_without_a_where_and_today_items_toggle_the_other_way() {
        let (feed, _) = adapted(&AdaptOptions::default());
        let second = &feed["items"][1];
        assert_eq!(second["state"], "read");
        assert_eq!(second["subtitle"], "github · review requested");
        assert_eq!(second["actions"][2]["title"], "untoday");
        assert_eq!(
            second["actions"][2]["command"],
            serde_json::json!(["workbox", "mark", "gh:o/r#12", "untoday"])
        );
    }

    #[test]
    fn detail_fields_and_the_snippet_body_mirror_the_workbox_pane() {
        let (feed, _) = adapted(&AdaptOptions::default());
        let pairs = |item: &serde_json::Value| -> Vec<(String, String)> {
            item["fields"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| {
                    (
                        f["label"].as_str().unwrap().to_owned(),
                        f["value"].as_str().unwrap().to_owned(),
                    )
                })
                .collect()
        };
        let want = |rows: &[(&str, &str)]| -> Vec<(String, String)> {
            rows.iter()
                .map(|(l, v)| ((*l).to_owned(), (*v).to_owned()))
                .collect()
        };
        let first = &feed["items"][0];
        assert_eq!(
            pairs(first),
            want(&[
                ("Source", "jira"),
                ("Reason", "mention"),
                ("Where", "NH board"),
                ("Status", "In Progress"),
                ("From", "alice"),
                ("Key", "NH-1412"),
                ("Today", "no"),
            ])
        );
        assert_eq!(first["body"], "can you look?");
        let second = &feed["items"][1];
        assert_eq!(
            pairs(second),
            want(&[
                ("Source", "github"),
                ("Reason", "review requested"),
                ("Key", "o/r#12"),
                ("Today", "yes"),
            ])
        );
        assert!(second.get("body").is_none());
    }

    #[test]
    fn a_snoozed_item_lists_when_it_wakes() {
        let (feed, _) = adapt(
            r#"[{"id":"a","title":"A","snoozed_until":"2026-10-19T09:00:00Z"}]"#,
            &AdaptOptions::default(),
        )
        .unwrap();
        let rows = feed["items"][0]["fields"].as_array().unwrap();
        assert!(rows
            .iter()
            .any(|f| f["label"] == "Snoozed until" && f["value"] == "2026-10-19T09:00:00Z"));
    }

    #[test]
    fn the_title_and_program_are_options() {
        let (feed, _) = adapted(&AdaptOptions {
            title: "Today (workbox)".into(),
            bin: "/opt/bin/workbox".into(),
        });
        assert_eq!(feed["title"], "Today (workbox)");
        assert_eq!(
            feed["items"][0]["actions"][0]["command"][0],
            "/opt/bin/workbox"
        );
    }

    #[test]
    fn the_output_is_a_valid_feed_without_diagnostics() {
        let (feed, _) = adapted(&AdaptOptions::default());
        let parsed = parse_feed(&feed.to_string()).expect("valid feed");
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert_eq!(parsed.items.len(), 2);
        assert_eq!(parsed.items[0].fields.len(), 7);
        assert_eq!(parsed.items[0].body.as_deref(), Some("can you look?"));
        assert_eq!(parsed.items[0].state, FeedItemState::Unread);
        assert_eq!(
            parsed.items[0].actions.len(),
            3,
            "no action key is reserved"
        );
    }

    #[test]
    fn bad_input_is_an_error_and_bad_items_are_skipped() {
        assert!(adapt("", &AdaptOptions::default()).is_err());
        assert!(adapt("{\"items\":[]}", &AdaptOptions::default()).is_err());
        let (feed, warnings) = adapt(
            r#"[{"title":"no id"},{"id":"ok","unread":"maybe"},{"id":"fine","title":"Fine"},5]"#,
            &AdaptOptions::default(),
        )
        .unwrap();
        assert_eq!(feed["items"].as_array().unwrap().len(), 1);
        assert_eq!(feed["items"][0]["id"], "fine");
        assert_eq!(warnings.len(), 3, "{warnings:?}");
    }

    #[test]
    fn an_empty_array_is_an_empty_feed() {
        let (feed, warnings) = adapt("[]\n", &AdaptOptions::default()).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(feed["items"], serde_json::json!([]));
    }

    /// One item per source and reason that workbox can produce, plus
    /// combinations it cannot, with the category each must land in.
    const MATRIX: &[(&str, &str, Option<&str>)] = &[
        ("github", "review_requested", Some("new-house-prs")),
        ("github", "mention", Some("new-house-prs")),
        ("github", "changes_requested", Some("my-prs")),
        ("github", "commented", Some("my-prs")),
        ("github", "approved", Some("my-prs")),
        ("github", "authored", Some("my-prs")),
        ("github", "assigned", None),
        ("github", "status_changed", None),
        ("github", "updated", None),
        ("jira", "mention", Some("ticket-mentions")),
        ("jira", "commented", Some("ticket-mentions")),
        ("jira", "assigned", Some("jira-assigned")),
        ("jira", "status_changed", None),
        ("jira", "review_requested", None),
        ("jira", "authored", None),
        ("jira", "updated", None),
        ("jira", "changes_requested", None),
        ("jira", "approved", None),
        ("bitbucket", "review_requested", Some("old-house-prs")),
        ("bitbucket", "authored", Some("my-prs")),
        ("bitbucket", "changes_requested", Some("my-prs")),
        ("bitbucket", "commented", Some("my-prs")),
        ("bitbucket", "approved", Some("my-prs")),
        ("bitbucket", "mention", None),
        ("bitbucket", "assigned", None),
        ("bitbucket", "status_changed", None),
        ("bitbucket", "updated", None),
        ("slack", "mention", None),
        ("", "", None),
    ];

    #[test]
    fn every_source_and_reason_maps_to_its_category() {
        for (source, reason, want) in MATRIX {
            assert_eq!(category(source, reason), *want, "{source} / {reason}");
        }
        assert_eq!(
            category(" GitHub ", "Review_Requested"),
            Some("new-house-prs")
        );
    }

    #[test]
    fn the_matrix_covers_every_workbox_reason_for_every_source() {
        for source in ["github", "jira", "bitbucket"] {
            for reason in [
                "mention",
                "review_requested",
                "assigned",
                "commented",
                "status_changed",
                "authored",
                "updated",
                "changes_requested",
                "approved",
            ] {
                assert!(
                    MATRIX.iter().any(|(s, r, _)| *s == source && *r == reason),
                    "{source} / {reason} is not in the matrix"
                );
            }
        }
    }

    #[test]
    fn rules_only_name_declared_categories() {
        for (_, _, id) in RULES {
            assert!(
                CATEGORIES.iter().any(|(declared, _, _)| declared == id),
                "{id}"
            );
        }
    }

    #[test]
    fn a_feed_built_from_the_matrix_groups_every_item_as_mapped() {
        let input = serde_json::Value::Array(
            MATRIX
                .iter()
                .enumerate()
                .map(|(n, (source, reason, _))| {
                    serde_json::json!({"id": format!("i{n}"), "title": format!("t{n}"),
                        "source": source, "reason": reason, "unread": n % 2 == 0})
                })
                .collect(),
        )
        .to_string();
        let (feed, warnings) = adapt(&input, &AdaptOptions::default()).unwrap();
        assert!(warnings.is_empty());
        let parsed = parse_feed(&feed.to_string()).expect("valid feed");
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let order: Vec<_> = parsed.groups.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(
            order,
            [
                "new-house-prs",
                "ticket-mentions",
                "jira-assigned",
                "old-house-prs",
                "my-prs",
                "other"
            ]
        );
        let titles: Vec<_> = parsed.groups.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(
            titles[..4],
            [
                "New House PR Requests",
                "JIRA Mentions",
                "New Tickets Assigned",
                "Old House PR Requests"
            ]
        );
        for (item, (source, reason, want)) in parsed.items.iter().zip(MATRIX) {
            assert_eq!(
                item.group.as_deref(),
                Some(want.unwrap_or("other")),
                "{source} / {reason}"
            );
        }
    }

    #[test]
    fn there_is_no_other_group_when_every_item_has_a_category() {
        let (feed, _) = adapt(
            r#"[{"id":"a","title":"A","source":"jira","reason":"assigned"}]"#,
            &AdaptOptions::default(),
        )
        .unwrap();
        let parsed = parse_feed(&feed.to_string()).unwrap();
        assert_eq!(parsed.groups.len(), 5);
        assert!(parsed.groups.iter().all(|g| g.id != "other"));
    }

    #[test]
    fn github_and_bitbucket_items_get_a_repo_field_from_where() {
        let input = r##"[
          {"id":"a","source":"github","key":"#915","title":"t","where":"acme/web-app#915","reason":"review_requested"},
          {"id":"b","source":"github","key":"#571","title":"t","where":"acme/platform-ui#571","reason":"mention"},
          {"id":"c","source":"bitbucket","key":"#7","title":"t","where":"ws/old.repo_x#7","reason":"review_requested"},
          {"id":"d","source":"jira","key":"NH-1","title":"t","where":"NH-1","reason":"mention"},
          {"id":"e","source":"github","key":"x","title":"t","where":"no-hash/repo","reason":"mention"},
          {"id":"f","source":"github","key":"#1","title":"t","where":"a/../b#1","reason":"mention"},
          {"id":"g","source":"github","key":"#1","title":"t","where":"","reason":"mention"}
        ]"##;
        let feed = adapt(input, &AdaptOptions::default()).unwrap().0;
        let repo = |index: usize| {
            feed["items"][index]["fields"]
                .as_array()
                .unwrap()
                .iter()
                .find(|field| field["label"] == "Repo")
                .map(|field| field["value"].as_str().unwrap().to_owned())
        };
        assert_eq!(repo(0).as_deref(), Some("web-app"));
        assert_eq!(repo(1).as_deref(), Some("platform-ui"));
        assert_eq!(repo(2).as_deref(), Some("old.repo_x"));
        for none in 3..7 {
            assert_eq!(repo(none), None, "item {none} has no repository");
        }
    }
}
