//! Built-in notification tones.
//!
//! Each tone is a short list of notes rendered to 16-bit mono PCM and wrapped
//! in a WAV container when played. The tones are synthesized from scratch
//! (sine partials with an exponential decay), so Herdr ships no third-party
//! audio and the binary carries no extra sound files.

use std::f32::consts::TAU;

const SAMPLE_RATE: u32 = 44_100;
/// Linear fade-in that keeps the first sample at zero (no click).
const ATTACK_MS: f32 = 3.0;
/// Linear fade-out that keeps the last sample at zero (no click).
const RELEASE_MS: f32 = 10.0;
/// Peak level after normalization, as a fraction of full scale.
const PEAK_LEVEL: f32 = 0.8;

/// A tone Herdr can play without any file on disk. Select one by `name()` in
/// `[ui.sound]`, for example `done = "chime"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltInSound {
    Chime,
    Ping,
    Pop,
    Bell,
    Blip,
    Chirp,
}

impl BuiltInSound {
    pub const ALL: [Self; 6] = [
        Self::Chime,
        Self::Ping,
        Self::Pop,
        Self::Bell,
        Self::Blip,
        Self::Chirp,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Chime => "chime",
            Self::Ping => "ping",
            Self::Pop => "pop",
            Self::Bell => "bell",
            Self::Blip => "blip",
            Self::Chirp => "chirp",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Chime => "three rising soft notes",
            Self::Ping => "one short high note",
            Self::Pop => "quick falling bubble",
            Self::Bell => "long ringing bell",
            Self::Blip => "two dry beeps",
            Self::Chirp => "fast rising sweep",
        }
    }

    /// Case-insensitive lookup by the name used in config.
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim();
        Self::ALL
            .into_iter()
            .find(|sound| sound.name().eq_ignore_ascii_case(name))
    }

    /// The tone as a complete WAV file.
    pub fn wav(self) -> Vec<u8> {
        encode_wav(&render(self.notes()))
    }

    fn notes(self) -> &'static [Note] {
        match self {
            Self::Chime => CHIME,
            Self::Ping => PING,
            Self::Pop => POP,
            Self::Bell => BELL,
            Self::Blip => BLIP,
            Self::Chirp => CHIRP,
        }
    }
}

const CHIME: &[Note] = &[
    Note::new(0, 380, 1046.5, 1046.5, 0.6, Timbre::Soft, 0.3),
    Note::new(110, 380, 1318.5, 1318.5, 0.6, Timbre::Soft, 0.3),
    Note::new(220, 700, 1568.0, 1568.0, 0.7, Timbre::Soft, 0.3),
];
const PING: &[Note] = &[Note::new(0, 450, 1760.0, 1760.0, 1.0, Timbre::Soft, 0.2)];
const POP: &[Note] = &[Note::new(0, 90, 900.0, 180.0, 1.0, Timbre::Sine, 0.5)];
const BELL: &[Note] = &[Note::new(0, 1100, 880.0, 880.0, 1.0, Timbre::Bell, 0.35)];
const BLIP: &[Note] = &[
    Note::new(0, 60, 1200.0, 1200.0, 0.6, Timbre::Sine, 2.0),
    Note::new(90, 60, 1200.0, 1200.0, 0.6, Timbre::Sine, 2.0),
];
const CHIRP: &[Note] = &[Note::new(0, 140, 700.0, 2000.0, 0.8, Timbre::Sine, 1.5)];

/// Relative strength of each partial: (frequency ratio, gain).
#[derive(Clone, Copy)]
enum Timbre {
    Sine,
    Soft,
    Bell,
}

impl Timbre {
    fn partials(self) -> &'static [(f32, f32)] {
        match self {
            Self::Sine => &[(1.0, 1.0)],
            Self::Soft => &[(1.0, 1.0), (2.0, 0.25), (3.0, 0.08)],
            // Inharmonic ratios give the metallic ring of a struck bell.
            Self::Bell => &[(1.0, 1.0), (2.76, 0.45), (5.4, 0.2)],
        }
    }
}

#[derive(Clone, Copy)]
struct Note {
    start_ms: u32,
    dur_ms: u32,
    from_hz: f32,
    /// Equal to `from_hz` for a steady pitch; otherwise a linear glide.
    to_hz: f32,
    gain: f32,
    timbre: Timbre,
    /// Decay time constant as a multiple of the note length.
    decay: f32,
}

impl Note {
    const fn new(
        start_ms: u32,
        dur_ms: u32,
        from_hz: f32,
        to_hz: f32,
        gain: f32,
        timbre: Timbre,
        decay: f32,
    ) -> Self {
        Self {
            start_ms,
            dur_ms,
            from_hz,
            to_hz,
            gain,
            timbre,
            decay,
        }
    }
}

fn samples_for(ms: u32) -> usize {
    (u64::from(ms) * u64::from(SAMPLE_RATE) / 1000) as usize
}

fn render(notes: &[Note]) -> Vec<i16> {
    let total = notes
        .iter()
        .map(|note| samples_for(note.start_ms + note.dur_ms))
        .max()
        .unwrap_or(0);
    let mut mix = vec![0f32; total];
    let rate = SAMPLE_RATE as f32;
    let attack = ATTACK_MS / 1000.0 * rate;
    let release = RELEASE_MS / 1000.0 * rate;

    for note in notes {
        let start = samples_for(note.start_ms);
        let len = samples_for(note.dur_ms);
        let tau = (note.dur_ms as f32 / 1000.0 * note.decay).max(0.001);
        let partials = note.timbre.partials();
        let mut phases = vec![0f32; partials.len()];
        for i in 0..len {
            let progress = i as f32 / len as f32;
            let hz = note.from_hz + (note.to_hz - note.from_hz) * progress;
            let age = i as f32 / rate;
            let fade_in = (i as f32 / attack).min(1.0);
            let fade_out = ((len - i) as f32 / release).min(1.0);
            let mut sample = 0.0;
            for (index, (ratio, gain)) in partials.iter().enumerate() {
                phases[index] = (phases[index] + TAU * hz * ratio / rate) % TAU;
                // Higher partials die away faster, like a real struck tone.
                let partial_decay = (-age / (tau / (1.0 + index as f32 * 0.5))).exp();
                sample += phases[index].sin() * gain * partial_decay;
            }
            mix[start + i] += sample * note.gain * fade_in * fade_out;
        }
    }

    let peak = mix.iter().fold(0f32, |peak, sample| peak.max(sample.abs()));
    if peak <= f32::EPSILON {
        return vec![0; total];
    }
    let scale = PEAK_LEVEL * f32::from(i16::MAX) / peak;
    mix.iter()
        .map(|sample| (sample * scale).round() as i16)
        .collect()
}

fn encode_wav(samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + samples.len() * 2);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(wav: &[u8]) -> Vec<i16> {
        wav[44..]
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            .collect()
    }

    #[test]
    fn names_round_trip_and_are_unique() {
        for sound in BuiltInSound::ALL {
            assert_eq!(BuiltInSound::from_name(sound.name()), Some(sound));
            assert_eq!(
                BuiltInSound::from_name(&sound.name().to_uppercase()),
                Some(sound)
            );
        }
        let mut names = BuiltInSound::ALL.map(BuiltInSound::name).to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), BuiltInSound::ALL.len());
        assert_eq!(BuiltInSound::from_name("default"), None);
        assert_eq!(BuiltInSound::from_name("chime.wav"), None);
    }

    #[test]
    fn every_tone_is_a_valid_short_audible_wav() {
        for sound in BuiltInSound::ALL {
            let wav = sound.wav();
            assert_eq!(&wav[0..4], b"RIFF", "{}", sound.name());
            assert_eq!(&wav[8..16], b"WAVEfmt ", "{}", sound.name());
            let riff_len = u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize;
            let data_len = u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize;
            assert_eq!(riff_len, wav.len() - 8, "{}", sound.name());
            assert_eq!(data_len, wav.len() - 44, "{}", sound.name());

            let samples = pcm(&wav);
            let seconds = samples.len() as f32 / SAMPLE_RATE as f32;
            assert!(
                (0.05..1.5).contains(&seconds),
                "{} lasts {seconds}s",
                sound.name()
            );
            let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
            assert!(peak > 20_000, "{} is too quiet: {peak}", sound.name());
            assert!(
                f32::from(peak) <= f32::from(i16::MAX) * PEAK_LEVEL + 1.0,
                "{} clips: {peak}",
                sound.name()
            );
            // Starting and ending at silence means no audible click.
            assert!(samples[0].unsigned_abs() < 200, "{} start", sound.name());
            assert!(
                samples[samples.len() - 1].unsigned_abs() < 200,
                "{} end",
                sound.name()
            );
        }
    }

    #[test]
    fn tones_are_distinct_and_deterministic() {
        let wavs = BuiltInSound::ALL.map(BuiltInSound::wav);
        for (index, wav) in wavs.iter().enumerate() {
            assert_eq!(wav, &BuiltInSound::ALL[index].wav());
            for other in &wavs[index + 1..] {
                assert_ne!(wav, other);
            }
        }
    }
}
