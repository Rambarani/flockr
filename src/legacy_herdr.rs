//! Compatibility layer for the upstream `herdr` names.
//!
//! flockr is a fork of herdr (https://github.com/herdrdev/herdr) that was fully
//! renamed. Agents and scripts that already know how to talk to herdr read the
//! `HERDR_*` environment variables, send `herdr:<id>` report sources and use
//! `herdr` config values. This module is the single place that keeps those
//! legacy names working. Everything else in the code base uses `flockr` names.
//!
//! Legacy surface kept here:
//!
//! - Pane environment: every flockr pane also exports the `HERDR_*` twins of the
//!   pane runtime contract (`HERDR_ENV`, `HERDR_PANE_ID`, `HERDR_WORKSPACE_ID`,
//!   `HERDR_TAB_ID`, `HERDR_SOCKET_PATH`, `HERDR_CLIENT_SOCKET_PATH`,
//!   `HERDR_BIN_PATH`). See [`LEGACY_PANE_ENV_NAMES`] and [`apply_legacy_pane_env`].
//! - Env reads: [`var`] / [`var_os`] read `FLOCKR_X` first, then fall back to
//!   `HERDR_X`. Identity variables (socket paths, pane id, session env marker)
//!   deliberately do NOT fall back, so a flockr binary started inside a real
//!   herdr pane never talks to the herdr server. See [`is_identity_env`].
//! - Report sources: the server treats `herdr:<id>` as an alias of
//!   `flockr:<id>` ([`canonical_report_source`]), so hook scripts installed by
//!   upstream herdr keep reporting.
//! - Config values: `ui.toast.delivery = "herdr"`, `[ui.toast.herdr]` and
//!   `--right-click herdr` are accepted as deprecated aliases. These are
//!   declared with `#[serde(alias = "herdr")]` / clap hidden values at the
//!   definition sites and tested in this module.
//! - `~/.config/herdr` is never read or migrated.

use std::borrow::Cow;
use std::env::VarError;
use std::ffi::OsString;

/// Prefix of the primary environment variables.
pub(crate) const ENV_PREFIX: &str = "FLOCKR_";
/// Prefix of the legacy (upstream herdr) environment variables.
pub(crate) const LEGACY_ENV_PREFIX: &str = "HERDR_";
/// Prefix of the legacy (upstream herdr) agent report sources.
pub(crate) const LEGACY_REPORT_SOURCE_PREFIX: &str = "herdr:";
/// Prefix of the primary agent report sources.
pub(crate) const REPORT_SOURCE_PREFIX: &str = "flockr:";
/// Deprecated `--right-click` value for flockr's own pane menu (`herdr`).
pub(crate) const LEGACY_RIGHT_CLICK_VALUE: &str = "herdr";

/// Pane runtime contract that is exported under both names. Third-party agents
/// with built-in herdr support (Command Code, Crush, Kiro CLI, Muse Code, Prime
/// Agent) read the `HERDR_` spelling.
pub(crate) const LEGACY_PANE_ENV_NAMES: [&str; 7] = [
    "FLOCKR_ENV",
    "FLOCKR_PANE_ID",
    "FLOCKR_WORKSPACE_ID",
    "FLOCKR_TAB_ID",
    "FLOCKR_SOCKET_PATH",
    "FLOCKR_CLIENT_SOCKET_PATH",
    "FLOCKR_BIN_PATH",
];

/// `NAME=` record prefix of the legacy agent hint in a process environment block.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) const LEGACY_AGENT_HINT_RECORD_PREFIX: &[u8] = b"HERDR_AGENT=";

/// Maps `FLOCKR_X` to its legacy twin `HERDR_X`.
pub(crate) fn legacy_env_name(name: &str) -> Option<String> {
    name.strip_prefix(ENV_PREFIX)
        .map(|rest| format!("{LEGACY_ENV_PREFIX}{rest}"))
}

/// Variables that identify which running instance a process talks to. They are
/// never read through the legacy fallback: honoring `HERDR_SOCKET_PATH` /
/// `HERDR_PANE_ID` / `HERDR_ENV` from an upstream herdr pane would make flockr
/// control (or refuse to start inside) a different product's server.
pub(crate) fn is_identity_env(name: &str) -> bool {
    matches!(
        name,
        "FLOCKR_ENV"
            | "FLOCKR_PANE_ID"
            | "FLOCKR_WORKSPACE_ID"
            | "FLOCKR_TAB_ID"
            | "FLOCKR_SOCKET_PATH"
            | "FLOCKR_CLIENT_SOCKET_PATH"
            | "FLOCKR_SESSION"
    )
}

/// Reads `name` (a `FLOCKR_*` variable), falling back to the `HERDR_*` twin when
/// the primary is not set. Identity variables never fall back.
pub(crate) fn var(name: &str) -> Result<String, VarError> {
    match std::env::var(name) {
        Err(VarError::NotPresent) if !is_identity_env(name) => match legacy_env_name(name) {
            Some(legacy) => std::env::var(legacy),
            None => Err(VarError::NotPresent),
        },
        other => other,
    }
}

/// `OsString` flavor of [`var`].
pub(crate) fn var_os(name: &str) -> Option<OsString> {
    std::env::var_os(name).or_else(|| {
        if is_identity_env(name) {
            return None;
        }
        legacy_env_name(name).and_then(std::env::var_os)
    })
}

/// Mirrors the pane runtime contract onto the legacy `HERDR_*` names: a twin is
/// set when its `FLOCKR_*` primary is present on `cmd` and removed otherwise, so
/// an identity inherited from an upstream herdr pane never leaks into a flockr
/// pane.
pub(crate) fn apply_legacy_pane_env(cmd: &mut portable_pty::CommandBuilder) {
    for name in LEGACY_PANE_ENV_NAMES {
        let Some(legacy) = legacy_env_name(name) else {
            continue;
        };
        match cmd.get_env(name).map(|value| value.to_os_string()) {
            Some(value) => cmd.env(legacy, value),
            None => cmd.env_remove(legacy),
        }
    }
}

/// Log filter directives from `FLOCKR_LOG`, or from the legacy `HERDR_LOG` with
/// its `herdr` tracing targets rewritten to `flockr` (the crate name the
/// tracing targets are keyed on).
pub(crate) fn log_filter_directives() -> Option<String> {
    if let Ok(value) = std::env::var("FLOCKR_LOG") {
        return Some(value);
    }
    let legacy = std::env::var(legacy_env_name("FLOCKR_LOG")?).ok()?;
    Some(translate_legacy_log_target(&legacy))
}

fn translate_legacy_log_target(directives: &str) -> String {
    directives
        .split(',')
        .map(|directive| {
            let (target, level) = match directive.split_once('=') {
                Some((target, level)) => (target.trim(), Some(level)),
                None => (directive.trim(), None),
            };
            let target = if target == "herdr" {
                "flockr".to_string()
            } else if let Some(rest) = target.strip_prefix("herdr::") {
                format!("flockr::{rest}")
            } else {
                target.to_string()
            };
            match level {
                Some(level) => format!("{target}={level}"),
                None => target,
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Maps a legacy `herdr:<id>` report source to its `flockr:<id>` alias.
pub(crate) fn canonical_report_source(source: &str) -> Cow<'_, str> {
    match source.strip_prefix(LEGACY_REPORT_SOURCE_PREFIX) {
        Some(rest) => Cow::Owned(format!("{REPORT_SOURCE_PREFIX}{rest}")),
        None => Cow::Borrowed(source),
    }
}

/// In-place [`canonical_report_source`].
pub(crate) fn canonicalize_report_source(source: &mut String) {
    if let Cow::Owned(canonical) = canonical_report_source(source) {
        *source = canonical;
    }
}

/// In-place [`canonical_report_source`] for optional sources.
pub(crate) fn canonicalize_optional_report_source(source: &mut Option<String>) {
    if let Some(source) = source {
        canonicalize_report_source(source);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_names_map_one_to_one() {
        assert_eq!(
            legacy_env_name("FLOCKR_SOCKET_PATH").as_deref(),
            Some("HERDR_SOCKET_PATH")
        );
        assert_eq!(legacy_env_name("PATH"), None);
    }

    #[test]
    fn report_sources_alias_legacy_prefix() {
        assert_eq!(canonical_report_source("herdr:claude"), "flockr:claude");
        assert_eq!(canonical_report_source("flockr:claude"), "flockr:claude");
        assert_eq!(canonical_report_source("custom:agent"), "custom:agent");
        let mut source = Some("herdr:pi".to_string());
        canonicalize_optional_report_source(&mut source);
        assert_eq!(source.as_deref(), Some("flockr:pi"));
    }

    #[test]
    fn legacy_log_targets_are_rewritten() {
        assert_eq!(
            translate_legacy_log_target("herdr=debug,herdr::pane_graphics_files=trace,warn"),
            "flockr=debug,flockr::pane_graphics_files=trace,warn"
        );
    }

    #[test]
    fn identity_vars_never_fall_back() {
        assert!(is_identity_env("FLOCKR_SOCKET_PATH"));
        assert!(is_identity_env("FLOCKR_ENV"));
        assert!(!is_identity_env("FLOCKR_CONFIG_PATH"));
    }

    #[test]
    fn legacy_pane_contract_has_expected_twins() {
        let twins: Vec<_> = LEGACY_PANE_ENV_NAMES
            .iter()
            .filter_map(|name| legacy_env_name(name))
            .collect();
        for expected in [
            "HERDR_ENV",
            "HERDR_PANE_ID",
            "HERDR_WORKSPACE_ID",
            "HERDR_TAB_ID",
            "HERDR_SOCKET_PATH",
            "HERDR_CLIENT_SOCKET_PATH",
            "HERDR_BIN_PATH",
        ] {
            assert!(twins.iter().any(|name| name == expected), "{expected}");
        }
        assert_eq!(LEGACY_RIGHT_CLICK_VALUE, "herdr");
    }
}
