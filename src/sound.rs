// Modified by flockr (https://github.com/Rambarani/flockr): changed from herdr v0.9.5 (Apache-2.0). See NOTICE.
//! Sound notifications for agent state changes.
//!
//! Plays the embedded default mp3s, synthesized built-in tones, or user files
//! via system audio tools. Uses afplay (macOS), Windows MediaPlayer, or
//! decoder-capable Linux audio players — no Rust audio dependencies.

mod builtin;

pub use builtin::BuiltInSound;

use std::io::Write;
#[cfg(not(any(windows, target_os = "macos")))]
use std::io::{Read, Result as IoResult};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(not(any(windows, target_os = "macos")))]
use std::time::{Duration, Instant};

use tracing::warn;

const DISABLE_SOUND_ENV: &str = "FLOCKR_DISABLE_SOUND";
#[cfg(any(windows, test))]
const WINDOWS_SOUND_PATH_ENV: &str = "FLOCKR_SOUND_PATH";
#[cfg(any(windows, test))]
const WINDOWS_SOUND_VOLUME_ENV: &str = "FLOCKR_SOUND_VOLUME";
#[cfg(not(any(windows, target_os = "macos")))]
const AUDIO_PLAYER_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(not(any(windows, target_os = "macos")))]
const AUDIO_PLAYER_POLL_INTERVAL: Duration = Duration::from_millis(25);

static SOUND_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
static SOUND_DONE: &[u8] = include_bytes!("../assets/sounds/done.mp3");
static SOUND_REQUEST: &[u8] = include_bytes!("../assets/sounds/request.mp3");

/// Which notification sound to play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    /// Agent finished work (transitioned to Idle).
    Done,
    /// Agent needs input (transitioned to Blocked).
    Request,
}

/// What a notification should sound like, resolved from config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundSelection {
    /// The embedded mp3 for this notification.
    Default,
    BuiltIn(BuiltInSound),
    /// A user file; relative paths are already resolved against the config directory.
    File(PathBuf),
}

/// A sound the settings picker can write to config: the embedded default or a built-in tone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundChoice {
    Default,
    BuiltIn(BuiltInSound),
}

impl SoundChoice {
    /// Default first, then every built-in tone in picker order.
    pub fn all() -> Vec<Self> {
        std::iter::once(Self::Default)
            .chain(BuiltInSound::ALL.into_iter().map(Self::BuiltIn))
            .collect()
    }

    /// The value written to `[ui.sound]` and accepted back by the config parser.
    pub fn config_value(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::BuiltIn(sound) => sound.name(),
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Default => "classic flockr sounds",
            Self::BuiltIn(sound) => sound.description(),
        }
    }

    pub fn selection(self) -> SoundSelection {
        match self {
            Self::Default => SoundSelection::Default,
            Self::BuiltIn(sound) => SoundSelection::BuiltIn(sound),
        }
    }

    pub fn from_selection(selection: &SoundSelection) -> Option<Self> {
        match selection {
            SoundSelection::Default => Some(Self::Default),
            SoundSelection::BuiltIn(sound) => Some(Self::BuiltIn(*sound)),
            SoundSelection::File(_) => None,
        }
    }
}

/// File extensions the platform's audio player can decode, lowercase.
pub fn supported_extensions() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &[
            "mp3", "wav", "aiff", "aif", "aifc", "m4a", "caf", "aac", "flac",
        ]
    } else if cfg!(windows) {
        &["mp3", "wav", "wma", "m4a", "aac"]
    } else {
        &[
            "mp3", "wav", "ogg", "oga", "opus", "flac", "aiff", "aif", "m4a", "aac",
        ]
    }
}

pub fn is_supported_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            supported_extensions()
                .iter()
                .any(|supported| supported.eq_ignore_ascii_case(ext))
        })
}

/// Clamp a configured volume to 0.0..=1.0; non-finite values mean full volume.
pub fn clamp_volume(volume: f32) -> f32 {
    if volume.is_finite() {
        volume.clamp(0.0, 1.0)
    } else {
        1.0
    }
}

/// Play a notification sound in a background thread.
/// Silently does nothing if no audio player is available.
pub fn play(
    sound: Sound,
    config: &crate::config::SoundConfig,
    agent: Option<crate::detect::Agent>,
) {
    preview(
        sound,
        config.selection_for(sound, agent),
        config.volume_for(agent),
    );
}

/// Play `selection` in a background thread regardless of the `enabled` setting.
/// `sound` only decides which embedded default a failed selection falls back to.
pub fn preview(sound: Sound, selection: SoundSelection, volume: f32) {
    if sound_playback_disabled_by_env() {
        return;
    }

    let volume = clamp_volume(volume);
    std::thread::spawn(move || play_selection(sound, &selection, volume));
}

fn play_selection(sound: Sound, selection: &SoundSelection, volume: f32) {
    match selection {
        SoundSelection::Default => {}
        SoundSelection::BuiltIn(tone) => match play_bytes(&tone.wav(), "wav", volume) {
            Ok(()) => return,
            Err(err) => {
                warn!(tone = tone.name(), sound = ?sound, err = %err, "built-in sound playback failed, falling back to default sound")
            }
        },
        SoundSelection::File(path) if !is_supported_file(path) => {
            warn!(path = %path.display(), sound = ?sound, "unsupported sound file format, falling back to default sound")
        }
        SoundSelection::File(path) => match play_file(path, volume) {
            Ok(()) => return,
            Err(err) => {
                warn!(path = %path.display(), sound = ?sound, err = %err, "custom sound playback failed, falling back to default sound")
            }
        },
    }

    let data = match sound {
        Sound::Done => SOUND_DONE,
        Sound::Request => SOUND_REQUEST,
    };

    if let Err(err) = play_bytes(data, "mp3", volume) {
        warn!(sound = ?sound, err = %err, "sound playback failed");
    }
}

fn sound_playback_disabled_by_env() -> bool {
    crate::legacy_herdr::var_os(DISABLE_SOUND_ENV).is_some()
        || std::env::var_os("NEXTEST").is_some()
}

fn play_file(path: &Path, volume: f32) -> Result<(), String> {
    match run_player(path, volume) {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(playback_error(&output)),
        Err(err) => Err(err),
    }
}

fn play_bytes(data: &[u8], extension: &str, volume: f32) -> Result<(), String> {
    // Write to a temp file because the supported audio players need a file path.
    // The extension matters: players pick their decoder from it.
    let tmp = temp_sound_path().with_extension(extension);
    let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    file.write_all(data).map_err(|e| e.to_string())?;
    drop(file);

    let result = run_player(&tmp, volume);

    let _ = std::fs::remove_file(&tmp);

    match result {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(playback_error(&output)),
        Err(e) => Err(e),
    }
}

fn playback_error(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        format!("player exited with {}", output.status)
    } else {
        format!("player exited with {}: {stderr}", output.status)
    }
}

fn temp_sound_path() -> PathBuf {
    let id = SOUND_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("flockr-sound-{}-{id}.mp3", std::process::id()))
}

#[cfg(windows)]
fn run_player(path: &Path, volume: f32) -> Result<Output, String> {
    run_windows_player(path, volume)
}

#[cfg(target_os = "macos")]
fn run_player(path: &Path, volume: f32) -> Result<Output, String> {
    Command::new("afplay")
        .args(afplay_args(path, volume))
        .output()
        .map_err(|e| format!("no audio player available: {e}"))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn run_player(path: &Path, volume: f32) -> Result<Output, String> {
    run_linux_player(path, volume)
}

/// afplay takes its volume as `-v` with 1.0 as full scale; it handles mp3, wav,
/// aiff, m4a, caf and more.
#[cfg(any(target_os = "macos", test))]
fn afplay_args(path: &Path, volume: f32) -> Vec<std::ffi::OsString> {
    vec![
        "-v".into(),
        format!("{:.2}", clamp_volume(volume)).into(),
        path.into(),
    ]
}

#[cfg(any(windows, test))]
fn windows_media_player_script() -> &'static str {
    r#"
$ErrorActionPreference = 'Stop'
$Path = [Environment]::GetEnvironmentVariable('FLOCKR_SOUND_PATH', 'Process')
if ([string]::IsNullOrWhiteSpace($Path)) { throw 'FLOCKR_SOUND_PATH is not set' }
Add-Type -AssemblyName PresentationCore
Add-Type -AssemblyName WindowsBase
$resolved = (Resolve-Path -LiteralPath $Path).ProviderPath
$volume = 1.0
$volumeText = [Environment]::GetEnvironmentVariable('FLOCKR_SOUND_VOLUME', 'Process')
if (-not [string]::IsNullOrWhiteSpace($volumeText)) {
    $volume = [double]::Parse($volumeText, [System.Globalization.CultureInfo]::InvariantCulture)
}
$script:player = [System.Windows.Media.MediaPlayer]::new()
$script:player.Volume = [Math]::Min(1.0, [Math]::Max(0.0, $volume))
$script:frame = [System.Windows.Threading.DispatcherFrame]::new()
$script:timer = [System.Windows.Threading.DispatcherTimer]::new()
$script:timer.Interval = [TimeSpan]::FromSeconds(15)
$script:failed = $null
$script:timedOut = $false
$script:player.add_MediaOpened({ $script:player.Play() })
$script:player.add_MediaEnded({ $script:frame.Continue = $false })
$script:player.add_MediaFailed({
    param($sender, $eventArgs)
    $script:failed = $eventArgs.ErrorException
    $script:frame.Continue = $false
})
$script:timer.add_Tick({
    $script:timedOut = $true
    $script:frame.Continue = $false
})
try {
    $script:player.Open([Uri]::new($resolved))
    $script:timer.Start()
    [System.Windows.Threading.Dispatcher]::PushFrame($script:frame)
} finally {
    $script:timer.Stop()
    $script:player.Close()
}
if ($script:failed) { throw "sound media failed: $($script:failed.Message)" }
if ($script:timedOut) { throw 'sound playback timed out' }
"#
}

/// Builds the same player script with a shorter timer for tests. Production
/// keeps the fixed 15 second bound in `windows_media_player_script`.
#[cfg(test)]
fn windows_media_player_script_with_timeout(timeout_seconds: u64) -> String {
    windows_media_player_script().replace(
        "FromSeconds(15)",
        &format!("FromSeconds({timeout_seconds})"),
    )
}

#[cfg(any(windows, test))]
fn windows_player_command(path: &Path, script: &str, volume: f32) -> Command {
    let mut command = crate::noninteractive_process::command("powershell.exe");
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .env(WINDOWS_SOUND_PATH_ENV, path)
        .env(
            WINDOWS_SOUND_VOLUME_ENV,
            format!("{:.2}", clamp_volume(volume)),
        );
    command
}

#[cfg(windows)]
fn run_windows_player(path: &Path, volume: f32) -> Result<Output, String> {
    windows_player_command(path, windows_media_player_script(), volume)
        .output()
        .map_err(|e| format!("Windows MediaPlayer playback failed: {e}"))
}

/// How a Linux player takes a volume, when it does.
#[cfg(any(not(any(windows, target_os = "macos")), test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VolumeFlag {
    /// Player has no volume flag worth relying on; play at the system level.
    /// Only the timeout tests spawn players without one.
    #[cfg(test)]
    None,
    /// `--volume=N` with N in 0..=65536 (paplay).
    Paplay,
    /// `--volume=F` with F as a linear factor (pw-play).
    PwPlay,
    /// `-volume N` with N in 0..=100 (ffplay).
    Ffplay,
    /// `-f N` with N as a scale factor where 32768 is unity (mpg123).
    Mpg123,
    /// `--volume=N` with N in 0..=100 (mpv).
    Mpv,
}

#[cfg(any(not(any(windows, target_os = "macos")), test))]
impl VolumeFlag {
    /// Arguments that scale playback to `volume`. Empty at full volume so the
    /// default invocation stays exactly what it was before volume existed.
    fn args(self, volume: f32) -> Vec<String> {
        let volume = clamp_volume(volume);
        if volume >= 1.0 {
            return Vec::new();
        }
        match self {
            #[cfg(test)]
            Self::None => Vec::new(),
            Self::Paplay => vec![format!("--volume={}", (volume * 65536.0).round() as u32)],
            Self::PwPlay => vec![format!("--volume={volume:.2}")],
            Self::Ffplay => vec!["-volume".into(), format!("{}", (volume * 100.0).round())],
            Self::Mpg123 => vec!["-f".into(), format!("{}", (volume * 32768.0).round())],
            Self::Mpv => vec![format!("--volume={}", (volume * 100.0).round())],
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
#[derive(Debug, Clone, Copy)]
struct AudioPlayer {
    program: &'static str,
    args: &'static [&'static str],
    volume: VolumeFlag,
}

#[cfg(not(any(windows, target_os = "macos")))]
impl AudioPlayer {
    fn output(self, path: &Path, volume: f32) -> std::io::Result<Output> {
        self.output_with_timeout(path, volume, AUDIO_PLAYER_TIMEOUT)
    }

    fn output_with_timeout(
        self,
        path: &Path,
        volume: f32,
        timeout: Duration,
    ) -> std::io::Result<Output> {
        let mut child = Command::new(self.program)
            .args(self.args)
            .args(self.volume.args(volume))
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let Some(stdout) = child.stdout.take() else {
            terminate_and_reap(&mut child)?;
            return Err(std::io::Error::other("audio player stdout was not piped"));
        };
        let Some(stderr) = child.stderr.take() else {
            terminate_and_reap(&mut child)?;
            return Err(std::io::Error::other("audio player stderr was not piped"));
        };
        let stdout_reader = read_output(stdout);
        let stderr_reader = read_output(stderr);
        let deadline = Instant::now() + timeout;

        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let (stdout, stderr) = finish_output(stdout_reader, stderr_reader)?;
                    return Ok(Output {
                        status,
                        stdout,
                        stderr,
                    });
                }
                Ok(None) => {}
                Err(wait_err) => {
                    let cleanup_result = terminate_and_reap(&mut child);
                    let _ = finish_output(stdout_reader, stderr_reader);
                    cleanup_result?;
                    return Err(wait_err);
                }
            }

            let now = Instant::now();
            if now >= deadline {
                let cleanup_result = terminate_and_reap(&mut child);
                let _ = finish_output(stdout_reader, stderr_reader);
                cleanup_result?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("{} playback timed out after {timeout:?}", self.program),
                ));
            }

            std::thread::sleep((deadline - now).min(AUDIO_PLAYER_POLL_INTERVAL));
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn read_output<R>(mut reader: R) -> std::thread::JoinHandle<IoResult<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut output = Vec::new();
        reader.read_to_end(&mut output)?;
        Ok(output)
    })
}

#[cfg(not(any(windows, target_os = "macos")))]
fn finish_output(
    stdout_reader: std::thread::JoinHandle<IoResult<Vec<u8>>>,
    stderr_reader: std::thread::JoinHandle<IoResult<Vec<u8>>>,
) -> IoResult<(Vec<u8>, Vec<u8>)> {
    let stdout = stdout_reader
        .join()
        .map_err(|_| std::io::Error::other("audio player stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| std::io::Error::other("audio player stderr reader panicked"))??;
    Ok((stdout, stderr))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn terminate_and_reap(child: &mut std::process::Child) -> std::io::Result<()> {
    if let Err(kill_err) = child.kill() {
        if child.try_wait()?.is_none() {
            return Err(kill_err);
        }
    }
    child.wait().map(|_| ())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn linux_audio_players() -> &'static [AudioPlayer] {
    // Do not add bare aplay here. It does not decode MP3 and plays MP3 bytes as raw PCM.
    // Players that cannot decode a file's format fail and the next one is tried
    // (mpg123 only reads mp3, so wav and ogg fall through to the others).
    &[
        AudioPlayer {
            program: "paplay",
            args: &[],
            volume: VolumeFlag::Paplay,
        },
        AudioPlayer {
            program: "pw-play",
            args: &[],
            volume: VolumeFlag::PwPlay,
        },
        AudioPlayer {
            program: "ffplay",
            args: &["-nodisp", "-autoexit", "-loglevel", "quiet"],
            volume: VolumeFlag::Ffplay,
        },
        AudioPlayer {
            program: "mpg123",
            args: &["-q"],
            volume: VolumeFlag::Mpg123,
        },
        AudioPlayer {
            program: "mpv",
            args: &["--no-video", "--really-quiet"],
            volume: VolumeFlag::Mpv,
        },
    ]
}

#[cfg(not(any(windows, target_os = "macos")))]
fn run_linux_player(path: &Path, volume: f32) -> Result<Output, String> {
    let mut errors = Vec::new();

    for player in linux_audio_players() {
        match player.output(path, volume) {
            Ok(output) if output.status.success() => return Ok(output),
            Ok(output) => errors.push(player_error(*player, &output)),
            Err(err) => errors.push(format!("{} failed: {err}", player.program)),
        }
    }

    Err(format!(
        "no audio player could play this file: {}",
        errors.join("; ")
    ))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn player_error(player: AudioPlayer, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();

    if stderr.is_empty() {
        format!("{} exited with {}", player.program, output.status)
    } else {
        format!("{} exited with {}: {stderr}", player.program, output.status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_sound_paths_are_unique() {
        assert_ne!(temp_sound_path(), temp_sound_path());
    }

    #[test]
    fn afplay_gets_volume_before_the_file() {
        let args = afplay_args(Path::new("/tmp/a.wav"), 0.5);
        assert_eq!(args, ["-v", "0.50", "/tmp/a.wav"]);
        // Out-of-range and non-finite values never reach the player.
        assert_eq!(afplay_args(Path::new("a.wav"), 7.0)[1], "1.00");
        assert_eq!(afplay_args(Path::new("a.wav"), -1.0)[1], "0.00");
        assert_eq!(afplay_args(Path::new("a.wav"), f32::NAN)[1], "1.00");
    }

    #[test]
    fn linux_volume_flags_scale_per_player_and_stay_silent_at_full_volume() {
        for flag in [
            VolumeFlag::None,
            VolumeFlag::Paplay,
            VolumeFlag::PwPlay,
            VolumeFlag::Ffplay,
            VolumeFlag::Mpg123,
            VolumeFlag::Mpv,
        ] {
            assert!(flag.args(1.0).is_empty(), "{flag:?}");
        }
        assert!(VolumeFlag::None.args(0.5).is_empty());
        assert_eq!(VolumeFlag::Paplay.args(0.5), ["--volume=32768"]);
        assert_eq!(VolumeFlag::PwPlay.args(0.25), ["--volume=0.25"]);
        assert_eq!(VolumeFlag::Ffplay.args(0.5), ["-volume", "50"]);
        assert_eq!(VolumeFlag::Mpg123.args(0.5), ["-f", "16384"]);
        assert_eq!(VolumeFlag::Mpv.args(0.5), ["--volume=50"]);
        assert_eq!(VolumeFlag::Mpv.args(0.0), ["--volume=0"]);
    }

    #[test]
    fn supported_formats_cover_mp3_and_wav_on_every_platform() {
        assert!(is_supported_file(Path::new("a.mp3")));
        assert!(is_supported_file(Path::new("dir/A.WAV")));
        assert!(!is_supported_file(Path::new("a.txt")));
        assert!(!is_supported_file(Path::new("noext")));
        #[cfg(target_os = "macos")]
        for ext in ["aiff", "m4a", "caf"] {
            assert!(is_supported_file(Path::new(&format!("a.{ext}"))), "{ext}");
        }
    }

    #[test]
    fn picker_choices_round_trip_through_config_values() {
        let choices = SoundChoice::all();
        assert_eq!(choices[0], SoundChoice::Default);
        assert_eq!(choices.len(), BuiltInSound::ALL.len() + 1);
        for choice in choices {
            assert_eq!(
                SoundChoice::from_selection(&choice.selection()),
                Some(choice)
            );
        }
        assert_eq!(
            SoundChoice::from_selection(&SoundSelection::File("a.wav".into())),
            None
        );
    }

    #[test]
    fn clamp_volume_bounds_and_defaults_to_full() {
        assert_eq!(clamp_volume(0.4), 0.4);
        assert_eq!(clamp_volume(-3.0), 0.0);
        assert_eq!(clamp_volume(3.0), 1.0);
        assert_eq!(clamp_volume(f32::INFINITY), 1.0);
        assert_eq!(clamp_volume(f32::NAN), 1.0);
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn linux_audio_players_are_mp3_capable() {
        let programs: Vec<&str> = linux_audio_players()
            .iter()
            .map(|player| player.program)
            .collect();

        assert_eq!(programs, ["paplay", "pw-play", "ffplay", "mpg123", "mpv"]);
        assert!(!programs.contains(&"aplay"));
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn linux_audio_player_does_not_wait_forever() {
        let pid_path = temp_sound_path().with_extension("pid");
        let player = AudioPlayer {
            program: "sh",
            args: &[
                "-c",
                "printf '%s' \"$$\" > \"$1\"; exec sleep 2",
                "flockr-sound-timeout-test",
            ],
            volume: VolumeFlag::None,
        };
        let result = player.output_with_timeout(&pid_path, 1.0, Duration::from_millis(100));
        let pid = std::fs::read_to_string(&pid_path)
            .expect("hanging test player should record its process ID");
        let _ = std::fs::remove_file(pid_path);

        let err = result.expect_err("hanging audio player should time out");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        let status = Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(std::process::Stdio::null())
            .status()
            .expect("test should inspect the timed-out player PID");
        assert!(
            !status.success(),
            "timed-out audio player should be terminated and reaped"
        );
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn linux_audio_player_preserves_completed_output() {
        let player = AudioPlayer {
            program: "sh",
            args: &[
                "-c",
                "i=0; while [ \"$i\" -lt 8192 ]; do printf 0123456789abcdef; i=$((i + 1)); done; i=0; while [ \"$i\" -lt 8192 ]; do printf fedcba9876543210; i=$((i + 1)); done >&2; exit 7",
                "flockr-sound-output-test",
            ],
            volume: VolumeFlag::None,
        };

        let output = player
            .output_with_timeout(Path::new("unused.mp3"), 1.0, Duration::from_secs(5))
            .expect("completed audio player should return its output");

        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout.len(), 131_072);
        assert_eq!(output.stderr.len(), 131_072);
        assert!(output.stdout.starts_with(b"0123456789abcdef"));
        assert!(output.stderr.starts_with(b"fedcba9876543210"));
    }

    #[test]
    fn windows_media_player_uses_process_environment_and_dispatcher() {
        let script = windows_media_player_script();
        let path = Path::new(r"C:\sound dir\döne.mp3");
        let command = windows_player_command(path, script, 0.5);
        let env_path = command.get_envs().find_map(|(key, value)| {
            (key == std::ffi::OsStr::new(WINDOWS_SOUND_PATH_ENV))
                .then_some(value)
                .flatten()
        });

        assert!(script.contains("GetEnvironmentVariable('FLOCKR_SOUND_PATH', 'Process')"));
        assert!(!script.contains("param([string]$Path)"));
        assert!(script.contains("Resolve-Path -LiteralPath $Path"));
        assert!(script.contains("Dispatcher]::PushFrame"));
        assert!(script.contains("add_MediaEnded"));
        assert!(script.contains("add_MediaFailed"));
        assert!(script.contains("FromSeconds(15)"));
        assert!(windows_media_player_script_with_timeout(2).contains("FromSeconds(2)"));
        assert_eq!(env_path, Some(path.as_os_str()));
        let env_volume = command.get_envs().find_map(|(key, value)| {
            (key == std::ffi::OsStr::new(WINDOWS_SOUND_VOLUME_ENV))
                .then_some(value)
                .flatten()
        });
        assert_eq!(env_volume, Some(std::ffi::OsStr::new("0.50")));
        assert!(script.contains("$script:player.Volume"));
        assert!(!command.get_args().any(|arg| arg == path.as_os_str()));
    }

    #[cfg(windows)]
    #[test]
    fn windows_media_player_reports_invalid_media() {
        let _lock = crate::integration::integration_env_lock();
        let path = temp_sound_path();
        std::fs::write(&path, b"not an mp3").unwrap();
        let script = windows_media_player_script_with_timeout(2);
        // Whether the host media stack raises MediaFailed before the playback
        // timeout is environment-dependent, so use a short test-only timer and
        // accept either terminal error instead of the production 15 second wait.
        let output = windows_player_command(&path, &script, 1.0)
            .output()
            .unwrap();
        let _ = std::fs::remove_file(path);

        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("sound media failed") || stderr.contains("sound playback timed out"),
            "stderr should report why playback stopped: {stderr}"
        );
    }
}
