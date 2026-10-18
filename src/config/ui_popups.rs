//! Settings for the client-side hint and preview popups under `[ui]`.

use serde::Deserialize;

/// Longest popup delay accepted. Larger values are clamped with a diagnostic so
/// a typo cannot make a popup effectively unreachable.
pub const MAX_POPUP_DELAY_MS: u64 = 10_000;

/// Hint popup shown when a prefix key is pending and no follow-up arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct WhichKeyConfig {
    /// Show the which-key hint popup while prefix mode waits. Default: true.
    pub enabled: bool,
    /// Milliseconds to wait after the prefix key before showing the popup. Default: 400.
    pub delay_ms: u64,
}

impl Default for WhichKeyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            delay_ms: 400,
        }
    }
}

impl WhichKeyConfig {
    pub fn delay(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.delay_ms.min(MAX_POPUP_DELAY_MS))
    }

    pub fn diagnostics(&self) -> Vec<String> {
        let mut diagnostics = Vec::new();
        check_delay("ui.which_key.delay_ms", self.delay_ms, &mut diagnostics);
        diagnostics
    }
}

/// Popup showing the recent screen of an agent, opened by hovering its sidebar
/// row or with `keys.preview_agent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct AgentPreviewConfig {
    /// Allow the agent preview popup. Default: true.
    pub enabled: bool,
    /// Milliseconds the mouse must rest on an agent row before the preview opens. Default: 600.
    pub hover_delay_ms: u64,
    /// Screen lines shown in the preview, between 1 and 40. Default: 15.
    pub lines: u16,
}

pub const MAX_AGENT_PREVIEW_LINES: u16 = 40;

impl Default for AgentPreviewConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            hover_delay_ms: 600,
            lines: 15,
        }
    }
}

impl AgentPreviewConfig {
    pub fn hover_delay(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.hover_delay_ms.min(MAX_POPUP_DELAY_MS))
    }

    pub fn line_count(&self) -> usize {
        usize::from(self.lines.clamp(1, MAX_AGENT_PREVIEW_LINES))
    }

    pub fn diagnostics(&self) -> Vec<String> {
        let mut diagnostics = Vec::new();
        check_delay(
            "ui.agent_preview.hover_delay_ms",
            self.hover_delay_ms,
            &mut diagnostics,
        );
        if !(1..=MAX_AGENT_PREVIEW_LINES).contains(&self.lines) {
            diagnostics.push(format!(
                "ui.agent_preview.lines must be between 1 and {MAX_AGENT_PREVIEW_LINES}; using {}",
                self.line_count()
            ));
        }
        diagnostics
    }
}

fn check_delay(field: &str, value: u64, diagnostics: &mut Vec<String>) {
    if value > MAX_POPUP_DELAY_MS {
        diagnostics.push(format!(
            "{field} must be between 0 and {MAX_POPUP_DELAY_MS}; using {MAX_POPUP_DELAY_MS}"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_key_defaults_to_enabled_with_a_short_delay() {
        let config = WhichKeyConfig::default();
        assert!(config.enabled);
        assert_eq!(config.delay(), std::time::Duration::from_millis(400));
        assert!(config.diagnostics().is_empty());
    }

    #[test]
    fn which_key_delay_is_clamped_with_a_diagnostic() {
        let config = WhichKeyConfig {
            enabled: true,
            delay_ms: 60_000,
        };
        assert_eq!(
            config.delay(),
            std::time::Duration::from_millis(MAX_POPUP_DELAY_MS)
        );
        assert_eq!(
            config.diagnostics(),
            ["ui.which_key.delay_ms must be between 0 and 10000; using 10000"]
        );
    }

    #[test]
    fn agent_preview_defaults_and_clamping() {
        let config = AgentPreviewConfig::default();
        assert!(config.enabled);
        assert_eq!(config.hover_delay(), std::time::Duration::from_millis(600));
        assert_eq!(config.line_count(), 15);
        assert!(config.diagnostics().is_empty());

        let config = AgentPreviewConfig {
            enabled: true,
            hover_delay_ms: 99_999,
            lines: 0,
        };
        assert_eq!(config.line_count(), 1);
        assert_eq!(config.diagnostics().len(), 2);
        let config = AgentPreviewConfig {
            lines: 500,
            ..AgentPreviewConfig::default()
        };
        assert_eq!(config.line_count(), 40);
        assert_eq!(
            config.diagnostics(),
            ["ui.agent_preview.lines must be between 1 and 40; using 40"]
        );
    }

    #[test]
    fn agent_preview_parses_from_the_ui_section() {
        let config: crate::config::Config = toml::from_str(
            "[ui.agent_preview]\nenabled = false\nhover_delay_ms = 250\nlines = 8\n",
        )
        .unwrap();
        assert!(!config.ui.agent_preview.enabled);
        assert_eq!(config.ui.agent_preview.hover_delay_ms, 250);
        assert_eq!(config.ui.agent_preview.lines, 8);
        assert!(config.collect_diagnostics().is_empty());
    }

    #[test]
    fn which_key_parses_from_the_ui_section() {
        let config: crate::config::Config =
            toml::from_str("[ui.which_key]\nenabled = false\ndelay_ms = 150\n").unwrap();
        assert!(!config.ui.which_key.enabled);
        assert_eq!(config.ui.which_key.delay_ms, 150);
        assert!(config.collect_diagnostics().is_empty());
    }
}
