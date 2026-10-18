#!/usr/bin/env rust-script
//! The job's command. Flockr runs `rust-script job.rs` in this directory on
//! the schedule set in job.toml.
//!
//! rust-script compiles this file the first time it runs and again whenever it
//! changes, so the first run can be slow. Run `flockr job build <id>` to
//! compile ahead of time. Install it once with `cargo install rust-script`.
//!
//! - Exit 0 for success and any other code for failure.
//! - stdout and stderr are stored with the run (the last 16 KiB of each) and
//!   can appear in toasts and handoff prompts. Never print secrets, tokens or
//!   anything you would not paste into a chat.
//! - The environment is your login shell's. Flockr also sets FLOCKR_JOB_ID,
//!   FLOCKR_JOB_DIR and FLOCKR_JOB_STATE_DIR.
//! - Add crates with a manifest block, for example:
//!
//! ```cargo
//! [dependencies]
//! # serde_json = "1"
//! ```
//!
//! Try it without recording anything:  flockr job run <id> --dry-run

fn main() {
    let id = std::env::var("FLOCKR_JOB_ID").unwrap_or_else(|_| "this job".to_string());
    println!("hello from {id}");

    // To ask whether to start an agent with specific text, print one line like
    // this as the last thing on stdout (it always asks, whatever [handoff] says):
    // println!(r#"{{"flockr_handoff": {{"ask": "Something needs you. Start Claude?"}}}}"#);
}
