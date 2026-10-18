//! `flockr feed adapt workbox`: turns the JSON array printed by
//! `workbox list --json` or `workbox status --json` into a feed.
//!
//! Each item becomes a feed item whose id is workbox's item id, badge is its
//! key, state is `unread` or `read`, and subtitle is
//! `<source> · <reason>[ · <where>]`. The actions mark the item done (`d`),
//! seen (`s`), or toggle it on the today list (`t`) by running
//! `workbox mark <id> <action>`.

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

/// The fields of one `workbox list --json` element that the feed uses.
/// Other fields (`tab`, `actor`, `snippet`, `snoozed_until`, ...) are
/// ignored, so new workbox fields never break the adapter.
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
}
