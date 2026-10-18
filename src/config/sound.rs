use std::fmt;
use std::path::{Path, PathBuf};

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::detect::Agent;
use crate::sound::{BuiltInSound, Sound, SoundSelection};

use super::io::resolve_config_relative_path;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct SoundConfig {
    pub enabled: bool,
    /// Playback volume from 0.0 (silent) to 1.0 (full). Default: 1.0.
    /// Mapped to `afplay -v` on macOS; best effort with other players.
    pub volume: f32,
    /// Sound for "done" notifications: "default", a built-in name (chime, ping,
    /// pop, bell, blip, chirp), or a sound file path. Relative paths are
    /// resolved from the config file's directory.
    pub done: Option<String>,
    /// Sound for "request" notifications, in the same forms as `done`.
    pub request: Option<String>,
    /// Optional sound file path used for all sound notifications.
    /// Relative paths are resolved from the config file's directory.
    pub path: Option<PathBuf>,
    /// Optional sound file path for "done" notifications.
    /// Relative paths are resolved from the config file's directory.
    pub done_path: Option<PathBuf>,
    /// Optional sound file path for "request" notifications.
    /// Relative paths are resolved from the config file's directory.
    pub request_path: Option<PathBuf>,
    pub agents: AgentSoundOverrides,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct AgentSoundOverrides {
    pub pi: AgentSoundSetting,
    pub claude: AgentSoundSetting,
    pub codex: AgentSoundSetting,
    pub gemini: AgentSoundSetting,
    pub cursor: AgentSoundSetting,
    pub devin: AgentSoundSetting,
    pub agy: AgentSoundSetting,
    pub cline: AgentSoundSetting,
    pub open_code: AgentSoundSetting,
    pub github_copilot: AgentSoundSetting,
    pub kimi: AgentSoundSetting,
    pub kiro: AgentSoundSetting,
    pub droid: AgentSoundSetting,
    pub amp: AgentSoundSetting,
    pub grok: AgentSoundSetting,
    pub hermes: AgentSoundSetting,
    pub kilo: AgentSoundSetting,
    pub qodercli: AgentSoundSetting,
    pub qwen: AgentSoundSetting,
    pub letta: AgentSoundSetting,
    pub maki: AgentSoundSetting,
    pub muse: AgentSoundSetting,
}

/// Per-agent sound setting. Written as the string "default", "on", or "off",
/// or as a table (`[ui.sound.agents.claude]`) with its own sounds and volume.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum AgentSoundSetting {
    #[default]
    Default,
    On,
    Off,
    Custom(AgentSoundCustom),
}

/// Sounds for one agent. Unset fields fall back to the global `[ui.sound]` values.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct AgentSoundCustom {
    /// Sound for this agent's "done" notifications (same forms as `ui.sound.done`).
    pub done: Option<String>,
    /// Sound for this agent's "request" notifications.
    pub request: Option<String>,
    /// Volume for this agent, from 0.0 to 1.0.
    pub volume: Option<f32>,
}

impl<'de> Deserialize<'de> for AgentSoundSetting {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SettingVisitor;

        impl<'de> Visitor<'de> for SettingVisitor {
            type Value = AgentSoundSetting;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(
                    "\"default\", \"on\", \"off\", or a table with done, request, and volume",
                )
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                match value {
                    "default" => Ok(AgentSoundSetting::Default),
                    "on" => Ok(AgentSoundSetting::On),
                    "off" => Ok(AgentSoundSetting::Off),
                    other => Err(E::unknown_variant(other, &["default", "on", "off"])),
                }
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                AgentSoundCustom::deserialize(de::value::MapAccessDeserializer::new(map))
                    .map(AgentSoundSetting::Custom)
            }
        }

        deserializer.deserialize_any(SettingVisitor)
    }
}

/// How a configured sound value reads: a keyword, a built-in tone, or a file.
enum SoundValue<'a> {
    Default,
    BuiltIn(BuiltInSound),
    File(&'a Path),
}

fn parse_sound_value(value: &str) -> SoundValue<'_> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("default") {
        SoundValue::Default
    } else if let Some(tone) = BuiltInSound::from_name(value) {
        SoundValue::BuiltIn(tone)
    } else {
        SoundValue::File(Path::new(value))
    }
}

fn selection_for_value(value: &str) -> SoundSelection {
    match parse_sound_value(value) {
        SoundValue::Default => SoundSelection::Default,
        SoundValue::BuiltIn(tone) => SoundSelection::BuiltIn(tone),
        SoundValue::File(path) => SoundSelection::File(resolve_config_relative_path(path)),
    }
}

/// A bare word with no extension or separator reads as a mistyped built-in
/// name rather than a file.
fn looks_like_tone_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn available_sound_names() -> String {
    std::iter::once("default")
        .chain(BuiltInSound::ALL.into_iter().map(BuiltInSound::name))
        .collect::<Vec<_>>()
        .join(", ")
}

impl SoundConfig {
    pub fn allows(&self, agent: Option<Agent>) -> bool {
        if !self.enabled {
            return false;
        }

        !matches!(self.agents.for_agent(agent), AgentSoundSetting::Off)
    }

    /// What `sound` should play for `agent`. Precedence, most specific first:
    /// the agent's own sound, `done`/`request`, `done_path`/`request_path`,
    /// `path`, then the embedded default.
    pub fn selection_for(&self, sound: Sound, agent: Option<Agent>) -> SoundSelection {
        let pick = |done: &Option<String>, request: &Option<String>| match sound {
            Sound::Done => done.clone(),
            Sound::Request => request.clone(),
        };
        let agent_value = match self.agents.for_agent(agent) {
            AgentSoundSetting::Custom(custom) => pick(&custom.done, &custom.request),
            _ => None,
        };
        if let Some(value) = agent_value.or_else(|| pick(&self.done, &self.request)) {
            return selection_for_value(&value);
        }

        let legacy = match sound {
            Sound::Done => self.done_path.as_ref().or(self.path.as_ref()),
            Sound::Request => self.request_path.as_ref().or(self.path.as_ref()),
        };
        match legacy {
            Some(path) => SoundSelection::File(resolve_config_relative_path(path)),
            None => SoundSelection::Default,
        }
    }

    /// Playback volume for `agent`, clamped to 0.0..=1.0.
    pub fn volume_for(&self, agent: Option<Agent>) -> f32 {
        let volume = match self.agents.for_agent(agent) {
            AgentSoundSetting::Custom(AgentSoundCustom {
                volume: Some(volume),
                ..
            }) => *volume,
            _ => self.volume,
        };
        crate::sound::clamp_volume(volume)
    }

    pub fn diagnostics(&self) -> Vec<String> {
        let mut diagnostics = Vec::new();
        check_volume("ui.sound.volume", Some(self.volume), &mut diagnostics);
        for (field, value) in [
            ("ui.sound.done", self.done.as_deref()),
            ("ui.sound.request", self.request.as_deref()),
        ] {
            if let Some(value) = value {
                check_sound_value(field, value, &mut diagnostics);
            }
        }
        for (field, path) in [
            ("ui.sound.path", self.path.as_ref()),
            ("ui.sound.done_path", self.done_path.as_ref()),
            ("ui.sound.request_path", self.request_path.as_ref()),
        ] {
            if let Some(path) = path {
                check_sound_file(field, path, &mut diagnostics);
            }
        }
        for (agent, setting) in self.agents.entries() {
            let AgentSoundSetting::Custom(custom) = setting else {
                continue;
            };
            for (key, value) in [
                ("done", custom.done.as_deref()),
                ("request", custom.request.as_deref()),
            ] {
                if let Some(value) = value {
                    check_sound_value(
                        &format!("ui.sound.agents.{agent}.{key}"),
                        value,
                        &mut diagnostics,
                    );
                }
            }
            check_volume(
                &format!("ui.sound.agents.{agent}.volume"),
                custom.volume,
                &mut diagnostics,
            );
        }
        diagnostics
    }
}

fn check_volume(field: &str, volume: Option<f32>, diagnostics: &mut Vec<String>) {
    let Some(volume) = volume else {
        return;
    };
    if !volume.is_finite() || !(0.0..=1.0).contains(&volume) {
        diagnostics.push(format!(
            "invalid volume: {field} = {volume} is outside 0.0 to 1.0; using {}",
            crate::sound::clamp_volume(volume)
        ));
    }
}

fn check_sound_value(field: &str, value: &str, diagnostics: &mut Vec<String>) {
    match parse_sound_value(value) {
        SoundValue::Default | SoundValue::BuiltIn(_) => {}
        SoundValue::File(_) if looks_like_tone_name(value.trim()) => {
            diagnostics.push(format!(
                "unknown built-in sound: {field} = {value}; available: {}; using default sound",
                available_sound_names()
            ));
        }
        SoundValue::File(path) => check_sound_file(field, path, diagnostics),
    }
}

fn check_sound_file(field: &str, path: &Path, diagnostics: &mut Vec<String>) {
    let resolved = resolve_config_relative_path(path);
    if !crate::sound::is_supported_file(&resolved) {
        diagnostics.push(format!(
            "unsupported sound file format: {field} = {} resolves to {}; supported formats here: {}; using default sound",
            path.display(),
            resolved.display(),
            crate::sound::supported_extensions().join(", ")
        ));
        return;
    }

    if !resolved.exists() {
        diagnostics.push(format!(
            "missing sound file: {field} = {} resolves to {}; using default sound",
            path.display(),
            resolved.display()
        ));
    } else if !resolved.is_file() {
        diagnostics.push(format!(
            "invalid sound file: {field} = {} resolves to {}; using default sound",
            path.display(),
            resolved.display()
        ));
    }
}

impl AgentSoundOverrides {
    pub fn for_agent(&self, agent: Option<Agent>) -> &AgentSoundSetting {
        static DEFAULT: AgentSoundSetting = AgentSoundSetting::Default;
        match agent {
            Some(Agent::Pi) => &self.pi,
            Some(Agent::Claude) => &self.claude,
            Some(Agent::Codex) => &self.codex,
            Some(Agent::Gemini) => &self.gemini,
            Some(Agent::Cursor) => &self.cursor,
            Some(Agent::Devin) => &self.devin,
            Some(Agent::Antigravity) => &self.agy,
            Some(Agent::Cline) => &self.cline,
            Some(Agent::Omp) => &DEFAULT,
            Some(Agent::Mastracode) => &DEFAULT,
            Some(Agent::OpenCode) => &self.open_code,
            Some(Agent::GithubCopilot) => &self.github_copilot,
            Some(Agent::Kimi) => &self.kimi,
            Some(Agent::Kiro) => &self.kiro,
            Some(Agent::Droid) => &self.droid,
            Some(Agent::Amp) => &self.amp,
            Some(Agent::Grok) => &self.grok,
            Some(Agent::Hermes) => &self.hermes,
            Some(Agent::Kilo) => &self.kilo,
            Some(Agent::Qodercli) => &self.qodercli,
            Some(Agent::Qwen) => &self.qwen,
            Some(Agent::Letta) => &self.letta,
            Some(Agent::Maki) => &self.maki,
            Some(Agent::Muse) => &self.muse,
            None => &DEFAULT,
        }
    }

    /// Every configurable agent with its config key, for validation.
    fn entries(&self) -> [(&'static str, &AgentSoundSetting); 22] {
        [
            ("pi", &self.pi),
            ("claude", &self.claude),
            ("codex", &self.codex),
            ("gemini", &self.gemini),
            ("cursor", &self.cursor),
            ("devin", &self.devin),
            ("agy", &self.agy),
            ("cline", &self.cline),
            ("open_code", &self.open_code),
            ("github_copilot", &self.github_copilot),
            ("kimi", &self.kimi),
            ("kiro", &self.kiro),
            ("droid", &self.droid),
            ("amp", &self.amp),
            ("grok", &self.grok),
            ("hermes", &self.hermes),
            ("kilo", &self.kilo),
            ("qodercli", &self.qodercli),
            ("qwen", &self.qwen),
            ("letta", &self.letta),
            ("maki", &self.maki),
            ("muse", &self.muse),
        ]
    }
}

impl Default for SoundConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            volume: 1.0,
            done: None,
            request: None,
            path: None,
            done_path: None,
            request_path: None,
            agents: AgentSoundOverrides::default(),
        }
    }
}

impl Default for AgentSoundOverrides {
    fn default() -> Self {
        Self {
            pi: AgentSoundSetting::Default,
            claude: AgentSoundSetting::Default,
            codex: AgentSoundSetting::Default,
            gemini: AgentSoundSetting::Default,
            cursor: AgentSoundSetting::Default,
            devin: AgentSoundSetting::Default,
            agy: AgentSoundSetting::Default,
            cline: AgentSoundSetting::Default,
            open_code: AgentSoundSetting::Default,
            github_copilot: AgentSoundSetting::Default,
            kimi: AgentSoundSetting::Default,
            kiro: AgentSoundSetting::Default,
            droid: AgentSoundSetting::Off,
            amp: AgentSoundSetting::Default,
            grok: AgentSoundSetting::Default,
            hermes: AgentSoundSetting::Default,
            kilo: AgentSoundSetting::Default,
            qodercli: AgentSoundSetting::Default,
            qwen: AgentSoundSetting::Default,
            letta: AgentSoundSetting::Default,
            maki: AgentSoundSetting::Default,
            muse: AgentSoundSetting::Default,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::{config_path, Config};

    #[test]
    fn sound_table_config_parses() {
        let toml = r#"
[ui.sound]
enabled = true
path = "sounds/all.mp3"
done_path = "sounds/done.mp3"
request_path = "/tmp/request.mp3"

[ui.sound.agents]
droid = "off"
claude = "on"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.ui.sound.enabled);
        assert_eq!(config.ui.sound.path, Some(PathBuf::from("sounds/all.mp3")));
        assert_eq!(
            config.ui.sound.done_path,
            Some(PathBuf::from("sounds/done.mp3"))
        );
        assert_eq!(
            config.ui.sound.request_path,
            Some(PathBuf::from("/tmp/request.mp3"))
        );
        assert_eq!(config.ui.sound.agents.droid, AgentSoundSetting::Off);
        assert_eq!(config.ui.sound.agents.claude, AgentSoundSetting::On);
        assert_eq!(config.ui.sound.agents.pi, AgentSoundSetting::Default);
        assert_eq!(config.ui.sound.agents.letta, AgentSoundSetting::Default);
    }

    #[test]
    fn letta_sound_override_is_used() {
        let overrides: AgentSoundOverrides = toml::from_str("letta = \"on\"").unwrap();
        assert_eq!(overrides.letta, AgentSoundSetting::On);
        assert_eq!(
            overrides.for_agent(Some(Agent::Letta)),
            &AgentSoundSetting::On
        );
    }

    #[test]
    fn sound_path_resolution_prefers_specific_over_global() {
        let config: Config = toml::from_str(
            r#"
[ui.sound]
path = "sounds/all.mp3"
done_path = "sounds/done.mp3"
"#,
        )
        .unwrap();

        let config_root = config_path().parent().unwrap().to_path_buf();
        assert_eq!(
            config
                .ui
                .sound
                .selection_for(crate::sound::Sound::Done, None),
            SoundSelection::File(config_root.join("sounds/done.mp3"))
        );
        assert_eq!(
            config
                .ui
                .sound
                .selection_for(crate::sound::Sound::Request, None),
            SoundSelection::File(config_root.join("sounds/all.mp3"))
        );
    }

    #[test]
    fn missing_sound_file_produces_diagnostic() {
        let config: Config = toml::from_str(
            r#"
[ui.sound]
done_path = "sounds/missing.mp3"
"#,
        )
        .unwrap();

        let diagnostics = config.collect_diagnostics();
        assert!(diagnostics.iter().any(
            |diag| diag.contains("ui.sound.done_path") && diag.contains("using default sound")
        ));
    }

    #[test]
    fn unsupported_sound_file_format_produces_diagnostic() {
        let config: Config = toml::from_str(
            r#"
[ui.sound]
path = "sounds/notification.txt"
"#,
        )
        .unwrap();

        let diagnostics = config.collect_diagnostics();
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("ui.sound.path")
                && diag.contains("unsupported sound file format")
                && diag.contains("wav")
                && diag.contains("using default sound")
        }));
    }

    #[test]
    fn wav_sound_file_is_a_supported_format() {
        let config: Config = toml::from_str(
            r#"
[ui.sound]
path = "sounds/notification.wav"
"#,
        )
        .unwrap();

        // Only the missing file is reported, not the format.
        let diagnostics = config.collect_diagnostics();
        assert!(diagnostics
            .iter()
            .all(|diag| !diag.contains("unsupported sound file format")));
        assert!(diagnostics
            .iter()
            .any(|diag| diag.contains("missing sound file")));
    }

    #[test]
    fn built_in_names_and_files_select_sounds() {
        let config: Config = toml::from_str(
            r#"
[ui.sound]
done = "chime"
request = "sounds/ping.wav"
"#,
        )
        .unwrap();
        let sound = &config.ui.sound;
        let config_root = config_path().parent().unwrap().to_path_buf();

        assert_eq!(
            sound.selection_for(Sound::Done, None),
            SoundSelection::BuiltIn(BuiltInSound::Chime)
        );
        assert_eq!(
            sound.selection_for(Sound::Request, None),
            SoundSelection::File(config_root.join("sounds/ping.wav"))
        );
        assert!(sound
            .diagnostics()
            .iter()
            .all(|diag| !diag.contains("ui.sound.done")));
    }

    #[test]
    fn built_in_names_are_case_insensitive_and_default_is_a_keyword() {
        let config: Config = toml::from_str(
            r#"
[ui.sound]
done = "Bell"
request = "default"
path = "sounds/all.mp3"
"#,
        )
        .unwrap();
        let sound = &config.ui.sound;

        assert_eq!(
            sound.selection_for(Sound::Done, None),
            SoundSelection::BuiltIn(BuiltInSound::Bell)
        );
        // "default" wins over the legacy path: it is an explicit choice.
        assert_eq!(
            sound.selection_for(Sound::Request, None),
            SoundSelection::Default
        );
    }

    #[test]
    fn mistyped_built_in_name_gets_a_helpful_diagnostic() {
        let config: Config = toml::from_str(
            r#"
[ui.sound]
done = "chimee"
"#,
        )
        .unwrap();

        let diagnostics = config.collect_diagnostics();
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("unknown built-in sound")
                && diag.contains("ui.sound.done")
                && diag.contains("chime")
                && diag.contains("using default sound")
        }));
    }

    #[test]
    fn name_and_new_keys_take_precedence_over_legacy_paths() {
        let config: Config = toml::from_str(
            r#"
[ui.sound]
path = "all.mp3"
done_path = "done.mp3"
done = "ping"
"#,
        )
        .unwrap();
        let config_root = config_path().parent().unwrap().to_path_buf();
        let sound = &config.ui.sound;

        assert_eq!(
            sound.selection_for(Sound::Done, None),
            SoundSelection::BuiltIn(BuiltInSound::Ping)
        );
        // Request has no new-style key, so it still follows the legacy path.
        assert_eq!(
            sound.selection_for(Sound::Request, None),
            SoundSelection::File(config_root.join("all.mp3"))
        );
        assert_eq!(
            SoundConfig::default().selection_for(Sound::Done, None),
            SoundSelection::Default
        );
    }

    #[test]
    fn agent_setting_accepts_strings_and_tables() {
        let config: Config = toml::from_str(
            r#"
[ui.sound.agents]
droid = "on"
codex = "off"
pi = "default"

[ui.sound.agents.claude]
done = "chime"
request = "bell"
volume = 0.5
"#,
        )
        .unwrap();
        let agents = &config.ui.sound.agents;

        assert_eq!(agents.droid, AgentSoundSetting::On);
        assert_eq!(agents.codex, AgentSoundSetting::Off);
        assert_eq!(agents.pi, AgentSoundSetting::Default);
        assert_eq!(
            agents.claude,
            AgentSoundSetting::Custom(AgentSoundCustom {
                done: Some("chime".into()),
                request: Some("bell".into()),
                volume: Some(0.5),
            })
        );
        assert_eq!(agents.gemini, AgentSoundSetting::Default);
    }

    #[test]
    fn invalid_agent_setting_names_the_accepted_forms() {
        let error = toml::from_str::<Config>(
            "[ui.sound.agents]
claude = \"loud\"\n",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("default"), "{error}");
        assert!(error.contains("off"), "{error}");

        let error = toml::from_str::<Config>(
            "[ui.sound.agents]
claude = 3\n",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("table"), "{error}");
    }

    #[test]
    fn per_agent_sounds_override_global_and_other_agents_keep_global() {
        let config: Config = toml::from_str(
            r#"
[ui.sound]
done = "ping"
volume = 0.8

[ui.sound.agents.claude]
done = "chime"
volume = 0.3
"#,
        )
        .unwrap();
        let sound = &config.ui.sound;

        assert_eq!(
            sound.selection_for(Sound::Done, Some(Agent::Claude)),
            SoundSelection::BuiltIn(BuiltInSound::Chime)
        );
        // Claude sets no request sound, so it falls back to the global one (default).
        assert_eq!(
            sound.selection_for(Sound::Request, Some(Agent::Claude)),
            SoundSelection::Default
        );
        assert_eq!(
            sound.selection_for(Sound::Done, Some(Agent::Codex)),
            SoundSelection::BuiltIn(BuiltInSound::Ping)
        );
        assert_eq!(
            sound.selection_for(Sound::Done, None),
            SoundSelection::BuiltIn(BuiltInSound::Ping)
        );
        assert_eq!(sound.volume_for(Some(Agent::Claude)), 0.3);
        assert_eq!(sound.volume_for(Some(Agent::Codex)), 0.8);
        assert_eq!(sound.volume_for(None), 0.8);
    }

    #[test]
    fn custom_agent_table_does_not_mute_and_off_string_still_does() {
        let config: Config = toml::from_str(
            r#"
[ui.sound.agents]
codex = "off"

[ui.sound.agents.claude]
done = "chime"
"#,
        )
        .unwrap();
        let sound = &config.ui.sound;

        assert!(sound.allows(Some(Agent::Claude)));
        assert!(!sound.allows(Some(Agent::Codex)));
        // Droid stays muted by default.
        assert!(!SoundConfig::default().allows(Some(Agent::Droid)));
    }

    #[test]
    fn per_agent_values_are_validated_with_their_full_key() {
        let config: Config = toml::from_str(
            r#"
[ui.sound.agents.claude]
done = "chimee"
request = "sounds/missing.wav"
volume = 2.0
"#,
        )
        .unwrap();

        let diagnostics = config.collect_diagnostics();
        for needle in [
            "ui.sound.agents.claude.done",
            "ui.sound.agents.claude.request",
            "ui.sound.agents.claude.volume",
        ] {
            assert!(
                diagnostics.iter().any(|diag| diag.contains(needle)),
                "{needle}: {diagnostics:?}"
            );
        }
    }

    #[test]
    fn volume_defaults_to_full_accepts_integers_and_clamps() {
        assert_eq!(SoundConfig::default().volume, 1.0);
        let config: Config = toml::from_str("[ui.sound]\nvolume = 0\n").unwrap();
        assert_eq!(config.ui.sound.volume_for(None), 0.0);
        let config: Config = toml::from_str("[ui.sound]\nvolume = 1\n").unwrap();
        assert_eq!(config.ui.sound.volume_for(None), 1.0);

        let config: Config = toml::from_str("[ui.sound]\nvolume = 1.5\n").unwrap();
        assert_eq!(config.ui.sound.volume_for(None), 1.0);
        assert!(config
            .collect_diagnostics()
            .iter()
            .any(|diag| diag.contains("ui.sound.volume") && diag.contains("0.0 to 1.0")));

        let config: Config = toml::from_str("[ui.sound]\nvolume = -0.5\n").unwrap();
        assert_eq!(config.ui.sound.volume_for(None), 0.0);
        assert!(!toml::from_str::<Config>("[ui.sound]\nvolume = 0.25\n")
            .unwrap()
            .collect_diagnostics()
            .iter()
            .any(|diag| diag.contains("volume")));
    }

    #[test]
    fn existing_string_form_config_keeps_working_unchanged() {
        let config: Config = toml::from_str(
            r#"
[ui.sound]
enabled = true
path = "sounds/all.mp3"

[ui.sound.agents]
droid = "off"
claude = "on"
"#,
        )
        .unwrap();
        let config_root = config_path().parent().unwrap().to_path_buf();

        assert!(config.ui.sound.allows(Some(Agent::Claude)));
        assert!(!config.ui.sound.allows(Some(Agent::Droid)));
        assert_eq!(
            config
                .ui
                .sound
                .selection_for(Sound::Done, Some(Agent::Claude)),
            SoundSelection::File(config_root.join("sounds/all.mp3"))
        );
        assert_eq!(config.ui.sound.volume_for(Some(Agent::Claude)), 1.0);
    }
}
