//! Adapters turn another tool's JSON into the feed format. They are pure
//! functions over text so a job's feed command can pipe a tool through
//! `flockr feed adapt <name>`.

pub(crate) mod workbox;
