//! Job feeds: lists of items that a job prints in a small JSON format and
//! that flockr stores, shows in a sidebar section and a Feed overlay, and
//! lets the user act on.
//!
//! `format` parses the feed document, `manager` keeps the stored feeds and
//! their revisions, `runner` runs a feed command or an item action on a
//! worker thread, and `adapters` turn other tools' JSON into the format.

pub(crate) mod adapters;
pub(crate) mod format;
pub(crate) mod manager;
pub(crate) mod runner;

pub(crate) use manager::FeedManager;
