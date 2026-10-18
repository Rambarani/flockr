#!/usr/bin/env rust-script
//! The feed command. Flockr runs `rust-script feed.rs` after every successful
//! run of the job, on demand (`flockr feed refresh <id>`) and when the server
//! starts.
//!
//! stdout must be ONLY one JSON object (feed format, version 1) and nothing
//! else: no log lines, no banner. Write diagnostics to stderr with eprintln!.
//! The items show in the Feed overlay and in a sidebar section under the agent
//! list.
//!
//! Each item needs a unique "id". Optional: "title", "subtitle", "state"
//! (unread, read, done, warn, error), "badge", "url" (http or https),
//! "updated_at" (RFC 3339) and "actions": up to 8, each with a one-character
//! "key" (not j k q g G R /), a "title" and a "command" argv array that runs
//! when you press the key. For real data, add serde_json with a cargo block
//! as described in job.rs.

fn main() {
    println!(
        r#"{{"version":1,"title":"Example feed","items":[{{"id":"example-1","title":"Replace this example item","subtitle":"printed by feed.rs","state":"unread","badge":"1","url":"https://example.com","actions":[{{"key":"d","title":"done","command":["echo","marked example-1 done"]}}]}}]}}"#
    );
}
