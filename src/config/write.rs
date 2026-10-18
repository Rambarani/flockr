#[derive(Clone, Copy)]
pub(crate) enum ConfigEdit<'a> {
    Theme(&'a str),
    StatusIndicators(super::StatusIndicatorStyle),
    Sound(bool),
    /// Choose the done and/or request sound; `None` leaves that key untouched.
    SoundTones {
        done: Option<crate::sound::SoundChoice>,
        request: Option<crate::sound::SoundChoice>,
    },
    ToastDelivery(super::ToastDelivery),
}

impl ConfigEdit<'_> {
    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::Theme(_) => "theme",
            Self::StatusIndicators(_) => "status indicators",
            Self::Sound(_) => "sound setting",
            Self::SoundTones { .. } => "notification sounds",
            Self::ToastDelivery(_) => "toast setting",
        }
    }

    pub(crate) fn apply(self, content: &str) -> String {
        match self {
            Self::Theme(name) => {
                let content =
                    super::upsert_section_value(content, "theme", "name", &format!("\"{name}\""));
                super::upsert_section_bool(&content, "theme", "auto_switch", false)
            }
            Self::StatusIndicators(style) => super::upsert_section_value(
                content,
                "ui",
                "status_indicators",
                &format!("\"{}\"", style.as_str()),
            ),
            Self::Sound(enabled) => {
                super::upsert_section_bool(content, "ui.sound", "enabled", enabled)
            }
            Self::SoundTones { done, request } => {
                let mut content = content.to_owned();
                for (key, choice) in [("done", done), ("request", request)] {
                    if let Some(choice) = choice {
                        content = super::upsert_section_value(
                            &content,
                            "ui.sound",
                            key,
                            &format!("\"{}\"", choice.config_value()),
                        );
                    }
                }
                content
            }
            Self::ToastDelivery(delivery) => {
                let value = match delivery {
                    super::ToastDelivery::Off => "\"off\"",
                    super::ToastDelivery::Herdr => "\"herdr\"",
                    super::ToastDelivery::Terminal => "\"terminal\"",
                    super::ToastDelivery::System => "\"system\"",
                };
                let content = super::upsert_section_value(content, "ui.toast", "delivery", value);
                super::remove_section_key(&content, "ui.toast", "enabled")
            }
        }
    }
}

pub(crate) fn update_file_at(
    path: &std::path::Path,
    description: &str,
    update: impl FnOnce(&str) -> String,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create config directory: {error}"))?;
    }
    let content = match super::io::read_optional_config(path) {
        Ok(Some(content)) => content,
        Ok(None) => String::new(),
        Err(error) => {
            return Err(format!(
                "failed to read config before saving {description}: {error}"
            ));
        }
    };
    std::fs::write(path, update(&content))
        .map_err(|error| format!("failed to save {description}: {error}"))
}

pub(crate) fn write_edit(edit: ConfigEdit<'_>) -> Result<(), String> {
    update_file_at(&super::config_path(), edit.description(), |content| {
        edit.apply(content)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_file_at_does_not_move_a_leading_bom_into_the_file() {
        let dir = std::env::temp_dir().join(format!("herdr-config-bom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            b"\xEF\xBB\xBF[terminal]\ndefault_shell = \"pwsh.exe\"\n",
        )
        .unwrap();

        update_file_at(&path, "onboarding setting", |content| {
            crate::config::upsert_top_level_bool(content, "onboarding", false)
        })
        .unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_dir_all(dir);

        assert!(
            !written.contains('\u{feff}'),
            "unexpected BOM in {written:?}"
        );
        assert!(
            toml::from_str::<toml::Value>(&written).is_ok(),
            "written config is not valid TOML: {written:?}"
        );
    }

    fn tones(
        done: Option<crate::sound::BuiltInSound>,
        request: Option<crate::sound::BuiltInSound>,
    ) -> ConfigEdit<'static> {
        use crate::sound::SoundChoice;
        ConfigEdit::SoundTones {
            done: done.map(SoundChoice::BuiltIn),
            request: request.map(SoundChoice::BuiltIn),
        }
    }

    #[test]
    fn sound_tones_creates_the_section_in_an_empty_config() {
        use crate::sound::BuiltInSound;

        let written = tones(Some(BuiltInSound::Chime), Some(BuiltInSound::Bell)).apply("");
        let config: crate::config::Config = toml::from_str(&written).unwrap();
        assert_eq!(config.ui.sound.done.as_deref(), Some("chime"));
        assert_eq!(config.ui.sound.request.as_deref(), Some("bell"));
    }

    #[test]
    fn sound_tones_edits_only_the_requested_keys_and_keeps_everything_else() {
        use crate::sound::{BuiltInSound, SoundChoice};

        let original = "# top comment\n[ui]\nsidebar_width = 30\n\n[ui.sound]\nenabled = false\ndone = \"pop\"\npath = \"a.mp3\"\n\n[ui.sound.agents]\nclaude = \"off\"\n\n[theme]\nname = \"nord\"\n";
        let written = tones(None, Some(BuiltInSound::Ping)).apply(original);
        let config: crate::config::Config = toml::from_str(&written).unwrap();
        // Untouched: the other key, the legacy path, the agents table, other sections.
        assert_eq!(config.ui.sound.done.as_deref(), Some("pop"));
        assert_eq!(config.ui.sound.request.as_deref(), Some("ping"));
        assert!(!config.ui.sound.enabled);
        assert_eq!(config.ui.sound.path, Some("a.mp3".into()));
        assert_eq!(
            config.ui.sound.agents.claude,
            crate::config::sound::AgentSoundSetting::Off
        );
        assert_eq!(config.ui.sidebar_width, 30);
        assert!(written.starts_with("# top comment"));
        assert!(written.contains("name = \"nord\""));

        // Replacing a value rewrites it in place rather than duplicating the key.
        let written = ConfigEdit::SoundTones {
            done: Some(SoundChoice::Default),
            request: None,
        }
        .apply(&written);
        assert_eq!(written.matches("done =").count(), 1, "{written}");
        assert!(written.contains("done = \"default\""));
    }
}
