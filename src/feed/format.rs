//! The feed format, version 1: what a job's feed command prints.
//!
//! ```json
//! {"version":1,"title":"Today","items":[
//!   {"id":"a","title":"Fix login","subtitle":"jira · mention","state":"unread",
//!    "badge":"NH-1","url":"https://example.com","updated_at":"2026-10-18T09:00:00Z",
//!    "body":"Can you look?\nIt blocks the release.",
//!    "fields":[{"label":"Source","value":"jira"}],
//!    "group":"mentions",
//!    "actions":[{"key":"d","title":"done","command":["workbox","mark","a","done"]}]}]}
//! ```
//!
//! A feed may also declare `"groups":[{"id":"mentions","title":"Mentions"}]`
//! (optionally with a `short_title` for narrow places) and tag items with a
//! `group` id. The list order is the tab order; items whose group is missing
//! or not declared are collected in an implicit `other` group. A feed without
//! `groups` is one flat list, exactly as before.
//!
//! The parser is strict about the document shape and forgiving about
//! details: unknown fields are ignored, an unknown state becomes `read`, long
//! strings are cut, and a bad item or action is dropped with a diagnostic
//! instead of failing the whole feed.

use serde::{Deserialize, Serialize};

use crate::api::schema::FeedItemState;

pub(crate) const FEED_VERSION: u64 = 1;
/// Largest feed document a refresh accepts.
pub(crate) const MAX_FEED_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_ITEMS: usize = 500;
pub(crate) const MAX_FEED_TITLE_CHARS: usize = 120;
pub(crate) const MAX_ID_CHARS: usize = 200;
pub(crate) const MAX_TITLE_CHARS: usize = 200;
pub(crate) const MAX_SUBTITLE_CHARS: usize = 300;
pub(crate) const MAX_BADGE_CHARS: usize = 40;
pub(crate) const MAX_URL_CHARS: usize = 2048;
pub(crate) const MAX_UPDATED_AT_CHARS: usize = 40;
/// Longest `body`, in bytes of UTF-8 (cut at a character boundary).
pub(crate) const MAX_BODY_BYTES: usize = 8 * 1024;
/// Most `fields` entries on an item.
pub(crate) const MAX_FIELDS: usize = 12;
pub(crate) const MAX_FIELD_LABEL_CHARS: usize = 40;
pub(crate) const MAX_FIELD_VALUE_CHARS: usize = 500;
/// Most groups a feed may declare (the implicit `other` group is extra).
pub(crate) const MAX_GROUPS: usize = 16;
pub(crate) const MAX_GROUP_ID_CHARS: usize = 40;
pub(crate) const MAX_GROUP_TITLE_CHARS: usize = 60;
pub(crate) const MAX_GROUP_SHORT_TITLE_CHARS: usize = 20;
/// The id of the implicit group for items without a known group.
pub(crate) const OTHER_GROUP_ID: &str = "other";
pub(crate) const OTHER_GROUP_TITLE: &str = "Other";
pub(crate) const MAX_ACTIONS: usize = 8;
pub(crate) const MAX_ACTION_TITLE_CHARS: usize = 40;
pub(crate) const MAX_ARGV: usize = 64;
pub(crate) const MAX_ARG_CHARS: usize = 4096;
/// Diagnostics beyond this many are summarised in one line.
const MAX_DIAGNOSTICS: usize = 20;

/// Keys an action cannot use because the Feed overlay gives them a meaning of
/// its own (named keys are matched ignoring case).
pub(crate) const RESERVED_NAMES: &[&str] = &[
    "enter",
    "return",
    "esc",
    "escape",
    "up",
    "down",
    "left",
    "right",
    "tab",
    "backtab",
    "pageup",
    "pagedown",
    "home",
    "end",
    "backspace",
    "delete",
    "space",
];
pub(crate) const RESERVED_CHARS: &[char] = &['j', 'k', 'q', 'g', 'G', 'R', 'S', '/'];
/// Also reserved, but only in a feed that declares `groups`: the overlay
/// switches category with these, so a flat feed keeps them for actions.
pub(crate) const GROUPED_RESERVED_CHARS: &[char] =
    &['[', ']', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9'];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ActionRecord {
    pub(crate) key: String,
    pub(crate) title: String,
    pub(crate) command: Vec<String>,
}

/// A category of a feed (a tab in the Feed overlay).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GroupRecord {
    pub(crate) id: String,
    pub(crate) title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) short_title: Option<String>,
}

/// A labelled value in an item's detail view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FieldRecord {
    pub(crate) label: String,
    pub(crate) value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ItemRecord {
    pub(crate) id: String,
    pub(crate) title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) subtitle: Option<String>,
    #[serde(default)]
    pub(crate) state: FeedItemState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) badge: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) updated_at: Option<String>,
    /// Plain text for the detail view; line breaks are kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) body: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) fields: Vec<FieldRecord>,
    /// The id of the item's group; only set in a feed with `groups`, where
    /// the parser resolves it to a declared group or `other`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) group: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) actions: Vec<ActionRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedFeed {
    pub(crate) title: Option<String>,
    /// Declared groups in order, plus `other` when an item landed there.
    /// Empty for a flat feed.
    pub(crate) groups: Vec<GroupRecord>,
    pub(crate) items: Vec<ItemRecord>,
    pub(crate) diagnostics: Vec<String>,
}

/// Collects diagnostics and caps their number.
struct Diagnostics {
    lines: Vec<String>,
    dropped: usize,
}

impl Diagnostics {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            dropped: 0,
        }
    }

    fn push(&mut self, line: String) {
        if self.lines.len() < MAX_DIAGNOSTICS {
            self.lines.push(line);
        } else {
            self.dropped += 1;
        }
    }

    fn finish(mut self) -> Vec<String> {
        if self.dropped > 0 {
            self.lines.push(format!("... and {} more", self.dropped));
        }
        self.lines
    }
}

/// Cut `text` to `max` characters; the flag says whether it was cut.
fn clip(text: &str, max: usize) -> (String, bool) {
    if text.chars().count() <= max {
        return (text.to_owned(), false);
    }
    (text.chars().take(max).collect(), true)
}

/// Remove control characters (they could recolor or move the cursor in the
/// terminal) and collapse line breaks and tabs to spaces.
pub(crate) fn clean_text(text: &str) -> String {
    text.chars()
        .filter_map(|ch| match ch {
            '\n' | '\r' | '\t' => Some(' '),
            ch if ch.is_control() => None,
            ch => Some(ch),
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Like `clean_text` for multi-line text: line breaks stay, tabs become
/// spaces, other control characters go, and surrounding blank space is cut.
pub(crate) fn clean_multiline(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .filter_map(|ch| match ch {
            '\n' => Some('\n'),
            '\r' => Some('\n'),
            '\t' => Some(' '),
            ch if ch.is_control() => None,
            ch => Some(ch),
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Cut `text` to at most `max` bytes at a character boundary.
fn clip_bytes(text: &str, max: usize) -> (&str, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

/// Read the optional `body`: multi-line plain text cut to `MAX_BODY_BYTES`.
fn parse_body(
    object: &serde_json::Map<String, serde_json::Value>,
    context: &str,
    diagnostics: &mut Diagnostics,
) -> Option<String> {
    match string_field(object, "body") {
        StringField::Absent => None,
        StringField::WrongType => {
            diagnostics.push(format!("{context}: body must be a string; ignored"));
            None
        }
        StringField::Text(text) => {
            let cleaned = clean_multiline(text);
            if cleaned.is_empty() {
                return None;
            }
            let (cut, was_cut) = clip_bytes(&cleaned, MAX_BODY_BYTES);
            if was_cut {
                diagnostics.push(format!(
                    "{context}: body is longer than {} KiB; cut",
                    MAX_BODY_BYTES / 1024
                ));
            }
            Some(cut.trim_end().to_owned())
        }
    }
}

/// Read the optional `fields` array of `{label, value}` strings.
fn parse_fields(
    object: &serde_json::Map<String, serde_json::Value>,
    context: &str,
    diagnostics: &mut Diagnostics,
) -> Vec<FieldRecord> {
    let list = match object.get("fields") {
        None | Some(serde_json::Value::Null) => return Vec::new(),
        Some(serde_json::Value::Array(list)) => list,
        Some(_) => {
            diagnostics.push(format!("{context}: fields must be an array; ignored"));
            return Vec::new();
        }
    };
    let mut fields = Vec::new();
    for entry in list {
        if fields.len() >= MAX_FIELDS {
            diagnostics.push(format!(
                "{context}: more than {MAX_FIELDS} fields; the rest are dropped"
            ));
            break;
        }
        let Some(entry) = entry.as_object() else {
            diagnostics.push(format!("{context}: a field must be an object; dropped"));
            continue;
        };
        let label = optional_text(entry, "label", MAX_FIELD_LABEL_CHARS, context, diagnostics);
        let value = optional_text(entry, "value", MAX_FIELD_VALUE_CHARS, context, diagnostics);
        match (label, value) {
            (Some(label), Some(value)) => fields.push(FieldRecord { label, value }),
            _ => diagnostics.push(format!(
                "{context}: a field needs a non-empty string label and value; dropped"
            )),
        }
    }
    fields
}

enum StringField<'a> {
    Absent,
    Text(&'a str),
    WrongType,
}

fn string_field<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> StringField<'a> {
    match object.get(name) {
        None | Some(serde_json::Value::Null) => StringField::Absent,
        Some(serde_json::Value::String(text)) => StringField::Text(text),
        Some(_) => StringField::WrongType,
    }
}

/// Read an optional text field: cleaned, cut to `max`, empty becomes `None`.
fn optional_text(
    object: &serde_json::Map<String, serde_json::Value>,
    name: &str,
    max: usize,
    context: &str,
    diagnostics: &mut Diagnostics,
) -> Option<String> {
    match string_field(object, name) {
        StringField::Absent => None,
        StringField::WrongType => {
            diagnostics.push(format!("{context}: {name} must be a string; ignored"));
            None
        }
        StringField::Text(text) => {
            let cleaned = clean_text(text);
            if cleaned.is_empty() {
                return None;
            }
            let (cut, was_cut) = clip(&cleaned, max);
            if was_cut {
                diagnostics.push(format!(
                    "{context}: {name} is longer than {max} characters; cut"
                ));
            }
            Some(cut)
        }
    }
}

/// Why `key` cannot be an action key, if it cannot.
#[cfg(test)]
pub(crate) fn action_key_problem(key: &str) -> Option<String> {
    action_key_problem_in(key, false)
}

/// Like `action_key_problem`; `grouped` adds the keys the overlay uses to
/// switch category in a feed with groups.
pub(crate) fn action_key_problem_in(key: &str, grouped: bool) -> Option<String> {
    let mut chars = key.chars();
    let (Some(first), None) = (chars.next(), chars.next()) else {
        if RESERVED_NAMES.contains(&key.to_ascii_lowercase().as_str()) {
            return Some(format!("{key:?} is reserved for the overlay"));
        }
        return Some(format!("{key:?} must be a single character"));
    };
    if first.is_control() || first.is_whitespace() {
        return Some("must be a visible character".to_owned());
    }
    if RESERVED_CHARS.contains(&first) {
        return Some(format!("{first:?} is reserved for the overlay"));
    }
    if grouped && GROUPED_RESERVED_CHARS.contains(&first) {
        return Some(format!(
            "{first:?} is reserved for the overlay in a feed with groups"
        ));
    }
    None
}

fn parse_action(
    value: &serde_json::Value,
    context: &str,
    grouped: bool,
    seen_keys: &mut Vec<String>,
    diagnostics: &mut Diagnostics,
) -> Option<ActionRecord> {
    let Some(object) = value.as_object() else {
        diagnostics.push(format!("{context}: an action must be an object; dropped"));
        return None;
    };
    let Some(key) = object.get("key").and_then(serde_json::Value::as_str) else {
        diagnostics.push(format!("{context}: an action needs a string key; dropped"));
        return None;
    };
    if let Some(problem) = action_key_problem_in(key, grouped) {
        diagnostics.push(format!("{context}: action key {problem}; dropped"));
        return None;
    }
    if seen_keys.iter().any(|seen| seen == key) {
        diagnostics.push(format!(
            "{context}: action key {key:?} is used twice; kept the first"
        ));
        return None;
    }
    let Some(argv) = object.get("command").and_then(serde_json::Value::as_array) else {
        diagnostics.push(format!(
            "{context}: action {key:?} needs a command array of strings; dropped"
        ));
        return None;
    };
    let mut command = Vec::with_capacity(argv.len());
    for arg in argv {
        let Some(arg) = arg.as_str() else {
            diagnostics.push(format!(
                "{context}: action {key:?} command must contain only strings; dropped"
            ));
            return None;
        };
        command.push(arg.to_owned());
    }
    if command.is_empty() || command[0].trim().is_empty() {
        diagnostics.push(format!(
            "{context}: action {key:?} has an empty command; dropped"
        ));
        return None;
    }
    if command.len() > MAX_ARGV
        || command
            .iter()
            .any(|arg| arg.chars().count() > MAX_ARG_CHARS)
    {
        diagnostics.push(format!(
            "{context}: action {key:?} command is longer than {MAX_ARGV} arguments or {MAX_ARG_CHARS} characters each; dropped"
        ));
        return None;
    }
    let title = optional_text(
        object,
        "title",
        MAX_ACTION_TITLE_CHARS,
        context,
        diagnostics,
    )
    .unwrap_or_else(|| key.to_owned());
    seen_keys.push(key.to_owned());
    Some(ActionRecord {
        key: key.to_owned(),
        title,
        command,
    })
}

fn parse_item(
    value: &serde_json::Value,
    position: usize,
    grouped: bool,
    diagnostics: &mut Diagnostics,
) -> Option<ItemRecord> {
    let context_fallback = format!("items[{position}]");
    let Some(object) = value.as_object() else {
        diagnostics.push(format!(
            "{context_fallback}: an item must be an object; dropped"
        ));
        return None;
    };
    let id = match string_field(object, "id") {
        StringField::Text(text) => clean_text(text),
        _ => String::new(),
    };
    if id.is_empty() {
        diagnostics.push(format!(
            "{context_fallback}: an item needs a non-empty string id; dropped"
        ));
        return None;
    }
    let (id, id_cut) = clip(&id, MAX_ID_CHARS);
    let context = format!("item {id:?}");
    if id_cut {
        diagnostics.push(format!(
            "{context}: id is longer than {MAX_ID_CHARS} characters; cut"
        ));
    }
    let title = optional_text(object, "title", MAX_TITLE_CHARS, &context, diagnostics)
        .unwrap_or_else(|| {
            diagnostics.push(format!("{context}: no title; showing the id"));
            id.clone()
        });
    let state = match string_field(object, "state") {
        StringField::Absent => FeedItemState::Read,
        StringField::WrongType => {
            diagnostics.push(format!("{context}: state must be a string; using read"));
            FeedItemState::Read
        }
        StringField::Text(text) => match text.trim().to_ascii_lowercase().as_str() {
            "unread" => FeedItemState::Unread,
            "read" => FeedItemState::Read,
            "done" => FeedItemState::Done,
            "warn" => FeedItemState::Warn,
            "error" => FeedItemState::Error,
            other => {
                diagnostics.push(format!("{context}: unknown state {other:?}; using read"));
                FeedItemState::Read
            }
        },
    };
    let url = optional_text(object, "url", MAX_URL_CHARS, &context, diagnostics).and_then(|url| {
        if url.starts_with("http://") || url.starts_with("https://") {
            Some(url)
        } else {
            diagnostics.push(format!(
                "{context}: url must start with http:// or https://; ignored"
            ));
            None
        }
    });
    let mut actions = Vec::new();
    match object.get("actions") {
        None | Some(serde_json::Value::Null) => {}
        Some(serde_json::Value::Array(list)) => {
            let mut seen_keys = Vec::new();
            for value in list {
                if actions.len() >= MAX_ACTIONS {
                    diagnostics.push(format!(
                        "{context}: more than {MAX_ACTIONS} actions; the rest are dropped"
                    ));
                    break;
                }
                if let Some(action) =
                    parse_action(value, &context, grouped, &mut seen_keys, diagnostics)
                {
                    actions.push(action);
                }
            }
        }
        Some(_) => diagnostics.push(format!("{context}: actions must be an array; ignored")),
    }
    let body = parse_body(object, &context, diagnostics);
    let fields = parse_fields(object, &context, diagnostics);
    let group = optional_text(object, "group", MAX_GROUP_ID_CHARS, &context, diagnostics);
    Some(ItemRecord {
        id,
        title,
        subtitle: optional_text(
            object,
            "subtitle",
            MAX_SUBTITLE_CHARS,
            &context,
            diagnostics,
        ),
        state,
        badge: optional_text(object, "badge", MAX_BADGE_CHARS, &context, diagnostics),
        url,
        updated_at: optional_text(
            object,
            "updated_at",
            MAX_UPDATED_AT_CHARS,
            &context,
            diagnostics,
        ),
        body,
        fields,
        group,
        actions,
    })
}

/// Read the optional feed-level `groups` array: `{id, title, short_title?}`.
fn parse_groups(
    root: &serde_json::Map<String, serde_json::Value>,
    diagnostics: &mut Diagnostics,
) -> Vec<GroupRecord> {
    let list = match root.get("groups") {
        None | Some(serde_json::Value::Null) => return Vec::new(),
        Some(serde_json::Value::Array(list)) => list,
        Some(_) => {
            diagnostics.push("feed: groups must be an array; ignored".to_owned());
            return Vec::new();
        }
    };
    let mut groups: Vec<GroupRecord> = Vec::new();
    for (position, entry) in list.iter().enumerate() {
        let context = format!("groups[{position}]");
        if groups.len() >= MAX_GROUPS {
            diagnostics.push(format!(
                "more than {MAX_GROUPS} groups; the rest are dropped"
            ));
            break;
        }
        let Some(entry) = entry.as_object() else {
            diagnostics.push(format!("{context}: a group must be an object; dropped"));
            continue;
        };
        let Some(id) = optional_text(entry, "id", MAX_GROUP_ID_CHARS, &context, diagnostics) else {
            diagnostics.push(format!(
                "{context}: a group needs a non-empty string id; dropped"
            ));
            continue;
        };
        if groups.iter().any(|group| group.id == id) {
            diagnostics.push(format!(
                "{context}: group {id:?} appears twice; kept the first"
            ));
            continue;
        }
        let title = optional_text(entry, "title", MAX_GROUP_TITLE_CHARS, &context, diagnostics)
            .unwrap_or_else(|| id.clone());
        let short_title = optional_text(
            entry,
            "short_title",
            MAX_GROUP_SHORT_TITLE_CHARS,
            &context,
            diagnostics,
        );
        groups.push(GroupRecord {
            id,
            title,
            short_title,
        });
    }
    groups
}

/// Point every item at a declared group, or at `other` (added to `groups`
/// when needed). A flat feed (no groups) leaves items alone.
fn resolve_groups(
    groups: &mut Vec<GroupRecord>,
    items: &mut [ItemRecord],
    diagnostics: &mut Diagnostics,
) {
    if groups.is_empty() {
        for item in items.iter_mut() {
            item.group = None;
        }
        return;
    }
    let mut needs_other = false;
    for item in items.iter_mut() {
        match item.group.take() {
            Some(id) if groups.iter().any(|group| group.id == id) => item.group = Some(id),
            other => {
                if let Some(id) = other {
                    diagnostics.push(format!(
                        "item {:?}: unknown group {id:?}; shown in {OTHER_GROUP_TITLE}",
                        item.id
                    ));
                }
                item.group = Some(OTHER_GROUP_ID.to_owned());
                needs_other = true;
            }
        }
    }
    if needs_other && !groups.iter().any(|group| group.id == OTHER_GROUP_ID) {
        groups.push(GroupRecord {
            id: OTHER_GROUP_ID.to_owned(),
            title: OTHER_GROUP_TITLE.to_owned(),
            short_title: None,
        });
    }
}

/// Parse a feed document. `Err` carries the reasons the document as a whole
/// is unusable; `Ok` may still carry diagnostics for dropped or fixed parts.
pub(crate) fn parse_feed(text: &str) -> Result<ParsedFeed, Vec<String>> {
    if text.len() > MAX_FEED_BYTES {
        return Err(vec![format!(
            "feed output is larger than {} KiB",
            MAX_FEED_BYTES / 1024
        )]);
    }
    let text = text.trim();
    if text.is_empty() {
        return Err(vec!["the feed command printed nothing".to_owned()]);
    }
    let value: serde_json::Value = serde_json::from_str(text).map_err(|err| {
        vec![format!(
            "not valid JSON: {err} (the feed command must print one JSON object on stdout)"
        )]
    })?;
    let Some(root) = value.as_object() else {
        return Err(vec![
            "the feed must be a JSON object with an items array".to_owned()
        ]);
    };
    let mut diagnostics = Diagnostics::new();
    match root.get("version") {
        None => diagnostics.push("version is missing; reading it as version 1".to_owned()),
        Some(version) if version.as_u64() == Some(FEED_VERSION) => {}
        Some(version) => {
            return Err(vec![format!(
                "unsupported feed version {version}; this flockr reads version {FEED_VERSION}"
            )])
        }
    }
    let Some(items) = root.get("items").and_then(serde_json::Value::as_array) else {
        return Err(vec!["the feed needs an items array".to_owned()]);
    };
    let title = optional_text(
        root,
        "title",
        MAX_FEED_TITLE_CHARS,
        "feed",
        &mut diagnostics,
    );
    let mut groups = parse_groups(root, &mut diagnostics);
    let grouped = !groups.is_empty();
    let mut records: Vec<ItemRecord> = Vec::new();
    for (position, value) in items.iter().enumerate() {
        if records.len() >= MAX_ITEMS {
            diagnostics.push(format!("more than {MAX_ITEMS} items; the rest are dropped"));
            break;
        }
        let Some(item) = parse_item(value, position, grouped, &mut diagnostics) else {
            continue;
        };
        if records.iter().any(|existing| existing.id == item.id) {
            diagnostics.push(format!("item {:?} appears twice; kept the first", item.id));
            continue;
        }
        records.push(item);
    }
    resolve_groups(&mut groups, &mut records, &mut diagnostics);
    Ok(ParsedFeed {
        title,
        groups,
        items: records,
        diagnostics: diagnostics.finish(),
    })
}

/// Seconds since the epoch for an RFC 3339 timestamp, or `None` when it does
/// not parse. Fractions of a second are ignored.
pub(crate) fn parse_rfc3339_secs(text: &str) -> Option<i64> {
    let bytes = text.trim().as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let slice = bytes.get(range)?;
        if !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(slice).ok()?.parse().ok()
    };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    if bytes[4] != b'-' || bytes[7] != b'-' || !matches!(bytes[10], b'T' | b't' | b' ') {
        return None;
    }
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut index = 19;
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
    }
    let offset_secs = match bytes.get(index)? {
        b'Z' | b'z' if index + 1 == bytes.len() => 0,
        sign @ (b'+' | b'-') if bytes.len() == index + 6 && bytes[index + 3] == b':' => {
            let hours = number(index + 1..index + 3)?;
            let minutes = number(index + 4..index + 6)?;
            let total = hours * 3600 + minutes * 60;
            if *sign == b'+' {
                total
            } else {
                -total
            }
        }
        _ => return None,
    };
    // Days from the civil date (Howard Hinnant's algorithm).
    let year_adjusted = if month <= 2 { year - 1 } else { year };
    let era = year_adjusted.div_euclid(400);
    let year_of_era = year_adjusted - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> ParsedFeed {
        parse_feed(text).unwrap_or_else(|err| panic!("expected a feed: {err:?}"))
    }

    #[test]
    fn a_full_feed_parses_every_field() {
        let feed = parse(
            r#"{"version":1,"title":"Today (workbox)","items":[
              {"id":"jira:NH-1","title":"Fix login","subtitle":"jira · mention","state":"unread",
               "badge":"NH-1","url":"https://example.com/NH-1","updated_at":"2026-10-18T09:00:00Z",
               "actions":[{"key":"d","title":"done","command":["workbox","mark","jira:NH-1","done"]}]}]}"#,
        );
        assert_eq!(feed.title.as_deref(), Some("Today (workbox)"));
        assert!(feed.diagnostics.is_empty(), "{:?}", feed.diagnostics);
        let item = &feed.items[0];
        assert_eq!(item.id, "jira:NH-1");
        assert_eq!(item.state, FeedItemState::Unread);
        assert_eq!(item.badge.as_deref(), Some("NH-1"));
        assert_eq!(item.actions[0].key, "d");
        assert_eq!(
            item.actions[0].command,
            ["workbox", "mark", "jira:NH-1", "done"]
        );
    }

    #[test]
    fn body_keeps_line_breaks_and_fields_parse_in_order() {
        let feed = parse(
            r#"{"version":1,"items":[{"id":"a","title":"A",
              "body":"line one\r\nline two\u0007\n\ttabbed\n\n",
              "fields":[{"label":"Source","value":"jira"},{"label":"Today","value":"no"}]}]}"#,
        );
        assert!(feed.diagnostics.is_empty(), "{:?}", feed.diagnostics);
        let item = &feed.items[0];
        assert_eq!(item.body.as_deref(), Some("line one\nline two\n tabbed"));
        assert_eq!(item.fields.len(), 2);
        assert_eq!(item.fields[0].label, "Source");
        assert_eq!(item.fields[1].value, "no");
    }

    #[test]
    fn an_old_feed_without_body_or_fields_still_parses_clean() {
        let feed = parse(r#"{"version":1,"items":[{"id":"a","title":"A","state":"unread"}]}"#);
        assert!(feed.diagnostics.is_empty());
        assert!(feed.items[0].body.is_none());
        assert!(feed.items[0].fields.is_empty());
    }

    #[test]
    fn a_long_body_is_cut_at_a_character_boundary() {
        let text = "é".repeat(MAX_BODY_BYTES);
        let feed = parse(&format!(
            r#"{{"version":1,"items":[{{"id":"a","body":"{text}"}}]}}"#
        ));
        let body = feed.items[0].body.as_deref().unwrap();
        assert!(body.len() <= MAX_BODY_BYTES);
        assert_eq!(
            body.len(),
            MAX_BODY_BYTES,
            "é is two bytes, so it fits exactly"
        );
        assert!(feed
            .diagnostics
            .iter()
            .any(|d| d.contains("body is longer")));
    }

    #[test]
    fn fields_are_capped_and_bad_entries_dropped() {
        let many: Vec<String> = (0..MAX_FIELDS + 3)
            .map(|n| format!(r#"{{"label":"L{n}","value":"v"}}"#))
            .collect();
        let long_label = "l".repeat(MAX_FIELD_LABEL_CHARS + 5);
        let long_value = "v".repeat(MAX_FIELD_VALUE_CHARS + 5);
        let feed = parse(&format!(
            r#"{{"version":1,"items":[
              {{"id":"a","fields":[{}]}},
              {{"id":"b","fields":[{{"label":"{long_label}","value":"{long_value}"}},
                 {{"label":"only label"}},{{"value":"only value"}},7,{{"label":"n","value":3}}]}},
              {{"id":"c","fields":"nope","body":5}}]}}"#,
            many.join(",")
        ));
        assert_eq!(feed.items[0].fields.len(), MAX_FIELDS);
        assert!(feed
            .diagnostics
            .iter()
            .any(|d| d.contains("more than 12 fields")));
        let b = &feed.items[1].fields;
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].label.chars().count(), MAX_FIELD_LABEL_CHARS);
        assert_eq!(b[0].value.chars().count(), MAX_FIELD_VALUE_CHARS);
        let dropped = feed
            .diagnostics
            .iter()
            .filter(|d| d.contains("a field needs") || d.contains("a field must be"))
            .count();
        assert_eq!(dropped, 4, "{:?}", feed.diagnostics);
        assert!(feed.items[2].fields.is_empty() && feed.items[2].body.is_none());
        assert!(feed
            .diagnostics
            .iter()
            .any(|d| d.contains("fields must be an array")));
        assert!(feed
            .diagnostics
            .iter()
            .any(|d| d.contains("body must be a string")));
    }

    #[test]
    fn s_is_reserved_for_sync_now() {
        assert!(action_key_problem("S").is_some());
        assert!(action_key_problem("s").is_none());
    }

    #[test]
    fn unknown_fields_are_ignored_and_optional_fields_default() {
        let feed = parse(
            r#"{"version":1,"items":[{"id":"a","title":"A","future":{"x":1}}],"extra":true}"#,
        );
        assert!(feed.title.is_none());
        assert_eq!(feed.items[0].state, FeedItemState::Read);
        assert!(feed.items[0].actions.is_empty());
        assert!(feed.diagnostics.is_empty());
    }

    #[test]
    fn a_missing_version_is_read_as_one_with_a_note() {
        let feed = parse(r#"{"items":[]}"#);
        assert_eq!(
            feed.diagnostics,
            ["version is missing; reading it as version 1"]
        );
        assert!(feed.items.is_empty());
    }

    #[test]
    fn documents_that_are_not_usable_report_why() {
        for (text, expected) in [
            ("", "printed nothing"),
            ("not json", "not valid JSON"),
            ("[]", "must be a JSON object"),
            (r#"{"version":2,"items":[]}"#, "unsupported feed version 2"),
            (r#"{"version":1}"#, "needs an items array"),
            (r#"{"version":1,"items":{}}"#, "needs an items array"),
        ] {
            let errors = parse_feed(text).unwrap_err();
            assert!(errors[0].contains(expected), "{text:?}: {errors:?}");
        }
    }

    #[test]
    fn bad_items_are_dropped_with_a_diagnostic_and_the_rest_survive() {
        let feed = parse(
            r#"{"version":1,"items":[
              "text",
              {"title":"no id"},
              {"id":"","title":"empty id"},
              {"id":"ok","title":"fine"},
              {"id":"ok","title":"duplicate"},
              {"id":"untitled"}]}"#,
        );
        let ids: Vec<_> = feed.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["ok", "untitled"]);
        assert_eq!(feed.items[1].title, "untitled");
        let joined = feed.diagnostics.join("\n");
        for expected in [
            "items[0]: an item must be an object",
            "items[1]: an item needs a non-empty string id",
            "items[2]: an item needs a non-empty string id",
            "item \"ok\" appears twice",
            "item \"untitled\": no title; showing the id",
        ] {
            assert!(
                joined.contains(expected),
                "missing {expected:?} in\n{joined}"
            );
        }
    }

    #[test]
    fn unknown_states_fall_back_to_read_and_urls_must_be_web_links() {
        let feed = parse(
            r#"{"version":1,"items":[
              {"id":"a","title":"A","state":"snoozed","url":"file:///etc/passwd"},
              {"id":"b","title":"B","state":"WARN","url":"http://example.com"}]}"#,
        );
        assert_eq!(feed.items[0].state, FeedItemState::Read);
        assert!(feed.items[0].url.is_none());
        assert_eq!(feed.items[1].state, FeedItemState::Warn);
        assert_eq!(feed.items[1].url.as_deref(), Some("http://example.com"));
        let joined = feed.diagnostics.join("\n");
        assert!(joined.contains("unknown state \"snoozed\""), "{joined}");
        assert!(joined.contains("url must start with http"), "{joined}");
    }

    #[test]
    fn strings_are_cleaned_and_cut_to_their_caps() {
        let long = "x".repeat(MAX_TITLE_CHARS + 50);
        let text = format!(
            r#"{{"version":1,"items":[{{"id":"a","title":"{long}","subtitle":"line one\nline\ttwo\u001b[31m"}}]}}"#
        );
        let feed = parse(&text);
        assert_eq!(feed.items[0].title.chars().count(), MAX_TITLE_CHARS);
        assert_eq!(
            feed.items[0].subtitle.as_deref(),
            Some("line one line two[31m")
        );
        assert!(feed
            .diagnostics
            .iter()
            .any(|line| line.contains("title is longer than 200")));
    }

    #[test]
    fn the_item_count_is_capped() {
        let items = (0..MAX_ITEMS + 5)
            .map(|n| format!(r#"{{"id":"i{n}","title":"t"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let feed = parse(&format!(r#"{{"version":1,"items":[{items}]}}"#));
        assert_eq!(feed.items.len(), MAX_ITEMS);
        assert!(feed
            .diagnostics
            .iter()
            .any(|line| line.contains("more than 500 items")));
    }

    #[test]
    fn oversized_documents_are_rejected() {
        let text = format!(
            r#"{{"version":1,"items":[],"pad":"{}"}}"#,
            "x".repeat(MAX_FEED_BYTES)
        );
        assert!(parse_feed(&text).unwrap_err()[0].contains("larger than"));
    }

    #[test]
    fn action_keys_must_be_one_visible_unreserved_character() {
        for key in ["d", "s", "t", "x", "1", "?", "D", "é"] {
            assert!(action_key_problem(key).is_none(), "{key}");
        }
        for key in [
            "Enter", "enter", "esc", "Esc", "Escape", "Up", "down", "Left", "right", "Tab",
            "PageDown", "Home", "space",
        ] {
            assert!(
                action_key_problem(key).is_some_and(|text| text.contains("reserved")),
                "{key}"
            );
        }
        for key in ["j", "k", "q", "g", "G", "R", "/"] {
            assert!(
                action_key_problem(key).is_some_and(|text| text.contains("reserved")),
                "{key}"
            );
        }
        for key in ["", "ab", " ", "\t"] {
            assert!(action_key_problem(key).is_some(), "{key:?}");
        }
    }

    #[test]
    fn bad_actions_are_dropped_and_good_ones_kept() {
        let feed = parse(
            r#"{"version":1,"items":[{"id":"a","title":"A","actions":[
              {"key":"d","title":"done","command":["x","done"]},
              {"key":"Enter","command":["x"]},
              {"key":"j","command":["x"]},
              {"key":"d","command":["y"]},
              {"key":"s","command":[]},
              {"key":"t","command":["x",5]},
              {"key":"u"},
              {"command":["x"]},
              "text",
              {"key":"w","command":["x"]}]}]}"#,
        );
        let keys: Vec<_> = feed.items[0]
            .actions
            .iter()
            .map(|a| a.key.as_str())
            .collect();
        assert_eq!(keys, ["d", "w"]);
        assert_eq!(
            feed.items[0].actions[1].title, "w",
            "title defaults to the key"
        );
        let joined = feed.diagnostics.join("\n");
        for expected in [
            "action key \"Enter\" is reserved",
            "action key 'j' is reserved",
            "action key \"d\" is used twice",
            "action \"s\" has an empty command",
            "action \"t\" command must contain only strings",
            "action \"u\" needs a command array",
            "an action needs a string key",
            "an action must be an object",
        ] {
            assert!(
                joined.contains(expected),
                "missing {expected:?} in\n{joined}"
            );
        }
    }

    #[test]
    fn at_most_eight_actions_per_item_survive() {
        let actions = (0..10)
            .map(|n| format!(r#"{{"key":"{n}","command":["x"]}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let feed = parse(&format!(
            r#"{{"version":1,"items":[{{"id":"a","title":"A","actions":[{actions}]}}]}}"#
        ));
        assert_eq!(feed.items[0].actions.len(), MAX_ACTIONS);
        assert!(feed
            .diagnostics
            .iter()
            .any(|line| line.contains("more than 8 actions")));
    }

    #[test]
    fn diagnostics_are_capped() {
        let items = (0..60)
            .map(|_| r#"{"title":"no id"}"#)
            .collect::<Vec<_>>()
            .join(",");
        let feed = parse(&format!(r#"{{"version":1,"items":[{items}]}}"#));
        assert_eq!(feed.diagnostics.len(), MAX_DIAGNOSTICS + 1);
        assert_eq!(feed.diagnostics.last().unwrap(), "... and 40 more");
    }

    #[test]
    fn groups_define_order_titles_and_resolve_items() {
        let feed = parse(
            r#"{"version":1,"groups":[
                 {"id":"b","title":"Bee","short_title":"B"},
                 {"id":"a","title":"Ay"},
                 {"id":"empty"}],
               "items":[
                 {"id":"1","title":"t","group":"a"},{"id":"2","title":"t","group":"b"},
                 {"id":"3","title":"t"},{"id":"4","title":"t","group":"nope"}]}"#,
        );
        let ids: Vec<_> = feed.groups.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["b", "a", "empty", "other"]);
        assert_eq!(feed.groups[0].short_title.as_deref(), Some("B"));
        assert_eq!(feed.groups[2].title, "empty", "title defaults to the id");
        assert_eq!(feed.groups[3].title, OTHER_GROUP_TITLE);
        let of: Vec<_> = feed.items.iter().map(|i| i.group.as_deref()).collect();
        assert_eq!(of, [Some("a"), Some("b"), Some("other"), Some("other")]);
        let joined = feed.diagnostics.join("\n");
        assert!(joined.contains("unknown group \"nope\""), "{joined}");
        assert_eq!(
            feed.diagnostics.len(),
            1,
            "a missing group is not a problem"
        );
    }

    #[test]
    fn no_implicit_other_group_when_every_item_has_a_declared_group() {
        let feed = parse(
            r#"{"version":1,"groups":[{"id":"a"}],"items":[{"id":"1","title":"t","group":"a"}]}"#,
        );
        assert_eq!(feed.groups.len(), 1);
        assert!(feed.diagnostics.is_empty());
    }

    #[test]
    fn a_declared_other_group_takes_the_ungrouped_items() {
        let feed = parse(
            r#"{"version":1,"groups":[{"id":"other","title":"Misc"}],"items":[{"id":"1","title":"t"}]}"#,
        );
        assert_eq!(feed.groups.len(), 1);
        assert_eq!(feed.groups[0].title, "Misc");
        assert_eq!(feed.items[0].group.as_deref(), Some("other"));
    }

    #[test]
    fn a_feed_without_groups_ignores_item_groups_and_stays_flat() {
        let feed = parse(
            r#"{"version":1,"items":[{"id":"1","title":"t","group":"a"},{"id":"2","title":"t"}]}"#,
        );
        assert!(feed.groups.is_empty());
        assert!(feed.items.iter().all(|item| item.group.is_none()));
        assert!(feed.diagnostics.is_empty());
    }

    #[test]
    fn groups_are_capped_and_bad_entries_dropped() {
        let many = (0..MAX_GROUPS + 3)
            .map(|n| format!(r#"{{"id":"g{n}"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let feed = parse(&format!(r#"{{"version":1,"groups":[{many}],"items":[]}}"#));
        assert_eq!(feed.groups.len(), MAX_GROUPS);
        assert!(feed
            .diagnostics
            .iter()
            .any(|d| d.contains("more than 16 groups")));

        let long_id = "i".repeat(MAX_GROUP_ID_CHARS + 5);
        let long_title = "t".repeat(MAX_GROUP_TITLE_CHARS + 5);
        let long_short = "s".repeat(MAX_GROUP_SHORT_TITLE_CHARS + 5);
        let feed = parse(&format!(
            r#"{{"version":1,"groups":[
              {{"id":"{long_id}","title":"{long_title}","short_title":"{long_short}"}},
              {{"id":"dup"}},{{"id":"dup"}},{{"title":"no id"}},{{"id":""}},7,{{"id":3}}],
              "items":[{{"id":"1","group":"{long_id}"}}]}}"#
        ));
        let ids: Vec<_> = feed.groups.iter().map(|g| g.id.chars().count()).collect();
        assert_eq!(ids, [MAX_GROUP_ID_CHARS, 3]);
        assert_eq!(feed.groups[0].title.chars().count(), MAX_GROUP_TITLE_CHARS);
        assert_eq!(
            feed.groups[0]
                .short_title
                .as_deref()
                .map(|t| t.chars().count()),
            Some(MAX_GROUP_SHORT_TITLE_CHARS)
        );
        assert_eq!(
            feed.items[0].group.as_deref().map(|g| g.chars().count()),
            Some(MAX_GROUP_ID_CHARS),
            "an over-long item group is cut like the group id, so they still match"
        );
        let joined = feed.diagnostics.join("\n");
        for expected in [
            "group \"dup\" appears twice",
            "a group needs a non-empty string id",
            "a group must be an object",
            "id must be a string",
        ] {
            assert!(
                joined.contains(expected),
                "missing {expected:?} in\n{joined}"
            );
        }
    }

    #[test]
    fn a_non_array_groups_value_is_ignored_with_a_diagnostic() {
        let feed = parse(r#"{"version":1,"groups":"x","items":[{"id":"1","group":"a"}]}"#);
        assert!(feed.groups.is_empty());
        assert!(feed.items[0].group.is_none());
        assert!(feed
            .diagnostics
            .iter()
            .any(|d| d.contains("groups must be an array")));
    }

    #[test]
    fn category_keys_are_reserved_only_in_a_feed_with_groups() {
        for key in ["[", "]", "0", "1", "9"] {
            assert!(action_key_problem(key).is_none(), "{key}");
            assert!(action_key_problem_in(key, true).is_some(), "{key}");
        }
        let actions = r#""actions":[{"key":"1","command":["x"]},{"key":"d","command":["x"]}]"#;
        let flat = parse(&format!(
            r#"{{"version":1,"items":[{{"id":"a",{actions}}}]}}"#
        ));
        assert_eq!(flat.items[0].actions.len(), 2, "flat feeds are unchanged");
        let grouped = parse(&format!(
            r#"{{"version":1,"groups":[{{"id":"g"}}],"items":[{{"id":"a","group":"g",{actions}}}]}}"#
        ));
        assert_eq!(grouped.items[0].actions.len(), 1);
        assert!(grouped
            .diagnostics
            .iter()
            .any(|d| d.contains("reserved for the overlay in a feed with groups")));
    }

    #[test]
    fn rfc3339_timestamps_convert_to_epoch_seconds() {
        assert_eq!(parse_rfc3339_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_secs("2026-10-18T09:00:00Z"),
            Some(1_792_314_000)
        );
        assert_eq!(
            parse_rfc3339_secs("2026-10-18T14:30:00+05:30"),
            Some(1_792_314_000)
        );
        assert_eq!(
            parse_rfc3339_secs("2026-10-18T09:00:00.123Z"),
            Some(1_792_314_000)
        );
        assert_eq!(
            parse_rfc3339_secs("2024-02-29T00:00:00Z"),
            Some(1_709_164_800)
        );
        for bad in [
            "",
            "yesterday",
            "2026-13-01T00:00:00Z",
            "2026-10-18T09:00:00",
            "2026-10-18",
        ] {
            assert_eq!(parse_rfc3339_secs(bad), None, "{bad:?}");
        }
    }
}
