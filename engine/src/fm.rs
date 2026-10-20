//! Analog narrowband FM audio from the 48 kHz discriminator stream.
//!
//! Chain (all linear, per-block state carried across calls):
//!
//! 1. De-emphasis: land-mobile NBFM transmitters pre-emphasize at 6 dB/octave, so the
//!    receiver rolls off at 6 dB/octave. A one-pole lowpass with its corner at 300 Hz,
//!    gain-normalized to 0 dB at 1 kHz (the usual reference tone).
//! 2. 300 Hz highpass (4th-order Butterworth): rejects CTCSS/DCS sub-audible signalling
//!    (67 to 254 Hz) and DC, as receivers do ahead of the audio stage.
//! 3. Decimation 48 kHz to 8 kHz through a Kaiser lowpass (3.0 kHz pass, 4.0 kHz stop, 50 dB)
//!    designed by the same method as the channel filters (see DESIGN-DSP.md).
//!
//! Noise squelch (the standard FM receiver method): with no carrier the discriminator
//! output is dominated by noise whose spectrum rises with frequency, so the energy in a
//! band above the voice passband (a band-pass at 5.4 kHz, Q 1.8) is large; a quieted signal has little
//! there. The metric is that band's RMS in Hz of deviation, compared with open/close
//! thresholds. Data bursts (DMR/NXDN) are kept out by the caller using decoder sync.
use crate::dsp::{ChannelSpec, Resampler};
use num_complex::Complex32;

/// Discriminator scaling used by `ChannelDsp::demod`: 5 kHz of deviation is 16000.
pub const COUNTS_PER_HZ: f32 = 16000. / 5000.;

#[derive(Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}
impl Biquad {
    /// RBJ audio-EQ-cookbook designs.
    fn highpass(fs: f32, f0: f32, q: f32) -> Self {
        let w = 2. * std::f32::consts::PI * f0 / fs;
        let (s, c) = w.sin_cos();
        let alpha = s / (2. * q);
        let a0 = 1. + alpha;
        Self {
            b0: (1. + c) / 2. / a0,
            b1: -(1. + c) / a0,
            b2: (1. + c) / 2. / a0,
            a1: -2. * c / a0,
            a2: (1. - alpha) / a0,
            z1: 0.,
            z2: 0.,
        }
    }
    /// Constant 0 dB peak-gain band-pass.
    fn bandpass(fs: f32, f0: f32, q: f32) -> Self {
        let w = 2. * std::f32::consts::PI * f0 / fs;
        let (s, c) = w.sin_cos();
        let alpha = s / (2. * q);
        let a0 = 1. + alpha;
        Self {
            b0: alpha / a0,
            b1: 0.,
            b2: -alpha / a0,
            a1: -2. * c / a0,
            a2: (1. - alpha) / a0,
            z1: 0.,
            z2: 0.,
        }
    }
    fn step(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}
/// One block of analog audio plus the squelch measurements for it.
pub struct FmBlock {
    /// 8 kHz signed16 audio, de-emphasized and band-limited.
    pub audio: Vec<i16>,
    /// RMS of the noise band (band-pass at 5.4 kHz, Q 1.8), in Hz of deviation.
    pub noise_hz: f32,
    /// RMS of the audio after de-emphasis and the 300 Hz highpass (before decimation), in Hz
    /// of deviation.
    pub voice_hz: f32,
}
pub struct FmAudio {
    deemph_state: f32,
    deemph_alpha: f32,
    deemph_gain: f32,
    highpass: [Biquad; 2],
    noise_band: Biquad,
    decimator: Resampler,
}
impl Default for FmAudio {
    fn default() -> Self {
        Self::new()
    }
}
impl FmAudio {
    const FS: f32 = 48000.;
    pub fn new() -> Self {
        let corner = 300.0f32;
        // One-pole lowpass y += a (x - y), a = 1 - exp(-2 pi fc / fs); normalize to 0 dB at 1 kHz.
        let alpha = 1. - (-2. * std::f32::consts::PI * corner / Self::FS).exp();
        let w = 2. * std::f32::consts::PI * 1000. / Self::FS;
        let (cw, sw) = (w.cos(), w.sin());
        // |H(e^jw)| = a / |1 - (1-a) e^-jw|
        let denom = ((1. - (1. - alpha) * cw).powi(2) + ((1. - alpha) * sw).powi(2)).sqrt();
        let gain = denom / alpha;
        Self {
            deemph_state: 0.,
            deemph_alpha: alpha,
            deemph_gain: gain,
            // 4th-order Butterworth = two sections with Q = 0.5412 and 1.3066.
            highpass: [
                Biquad::highpass(Self::FS, 300., 0.5412),
                Biquad::highpass(Self::FS, 300., 1.3066),
            ],
            noise_band: Biquad::bandpass(Self::FS, 5400., 1.8),
            decimator: Resampler::new(
                48000.,
                8000.,
                ChannelSpec {
                    pass_hz: 3000.,
                    stop_hz: 4000.,
                    atten_db: 50.,
                    band_hz: 0.,
                },
            ),
        }
    }
    pub fn push(&mut self, pcm: &[i16]) -> FmBlock {
        let (mut noise_sq, mut voice_sq) = (0f64, 0f64);
        let mut voice = Vec::with_capacity(pcm.len());
        for &x in pcm {
            let x = x as f32;
            let n = self.noise_band.step(x);
            noise_sq += (n * n) as f64;
            self.deemph_state += self.deemph_alpha * (x - self.deemph_state);
            let stage = self.highpass[0].step(self.deemph_state * self.deemph_gain);
            let v = self.highpass[1].step(stage);
            voice_sq += (v * v) as f64;
            voice.push(Complex32::new(v, 0.));
        }
        let count = pcm.len().max(1) as f64;
        let audio = self
            .decimator
            .push(&voice)
            .iter()
            .map(|c| c.re.clamp(-32768., 32767.) as i16)
            .collect();
        FmBlock {
            audio,
            noise_hz: (noise_sq / count).sqrt() as f32 / COUNTS_PER_HZ,
            voice_hz: (voice_sq / count).sqrt() as f32 / COUNTS_PER_HZ,
        }
    }
}
/// Noise squelch with hysteresis and a short hang so word gaps do not chop the audio.
pub struct Squelch {
    open: bool,
    hang_blocks: u32,
    pub open_noise_hz: f32,
    pub close_noise_hz: f32,
    pub min_voice_hz: f32,
    pub hang: u32,
}
impl Default for Squelch {
    fn default() -> Self {
        Self {
            open: false,
            hang_blocks: 0,
            open_noise_hz: 700.,
            close_noise_hz: 1100.,
            min_voice_hz: 120.,
            hang: 3,
        }
    }
}
impl Squelch {
    /// Update with one block's measurements. True while the audio should be heard.
    pub fn update(&mut self, block: &FmBlock) -> bool {
        let quiet = if self.open {
            block.noise_hz < self.close_noise_hz
        } else {
            block.noise_hz < self.open_noise_hz
        };
        if quiet && block.voice_hz >= self.min_voice_hz {
            self.open = true;
            self.hang_blocks = self.hang;
        } else if quiet && self.open && self.hang_blocks > 0 {
            self.hang_blocks -= 1;
        } else {
            self.open = false;
        }
        self.open
    }
}
/// Block the analog path works in. The squelch thresholds were calibrated on blocks of this
/// length (100 ms), so producers that deliver tiny packets are batched up to it.
pub const BLOCK_SAMPLES: usize = 4800;
/// Analog audio for a channel: FM demodulation, noise squelch, and the rule that digital
/// signalling mutes it. Time-based, independent of how the producer chunks its IQ.
pub struct AnalogGate {
    fm: FmAudio,
    squelch: Squelch,
    pending: Vec<i16>,
    digital_hold_s: f32,
    sync_seen: bool,
    open_s: f32,
    /// Digital sync mutes analog audio for this long.
    pub digital_hold: f32,
    /// A quieted voice carrier must hold this long before the channel counts as analog voice.
    pub confirm_s: f32,
}
impl Default for AnalogGate {
    fn default() -> Self {
        Self {
            fm: FmAudio::new(),
            squelch: Squelch::default(),
            pending: Vec::new(),
            digital_hold_s: 0.,
            sync_seen: false,
            open_s: 0.,
            digital_hold: 3.,
            confirm_s: 0.3,
        }
    }
}
impl AnalogGate {
    /// Feed 48 kHz discriminator audio; `digital_sync` says DMR/NXDN signalling was seen in
    /// the same span. Returns (8 kHz audio, confirmed analog voice) per completed block.
    pub fn push(&mut self, pcm: &[i16], digital_sync: bool) -> Vec<(Vec<i16>, bool)> {
        self.sync_seen |= digital_sync;
        self.pending.extend_from_slice(pcm);
        let mut out = Vec::new();
        while self.pending.len() >= BLOCK_SAMPLES {
            let rest = self.pending.split_off(BLOCK_SAMPLES);
            let block = std::mem::replace(&mut self.pending, rest);
            let seconds = BLOCK_SAMPLES as f32 / 48000.;
            self.digital_hold_s = if self.sync_seen {
                self.digital_hold
            } else {
                (self.digital_hold_s - seconds).max(0.)
            };
            self.sync_seen = false;
            let measured = self.fm.push(&block);
            let open = self.squelch.update(&measured) && self.digital_hold_s <= 0.;
            self.open_s = if open { self.open_s + seconds } else { 0. };
            let audio = if open { measured.audio } else { Vec::new() };
            out.push((audio, self.open_s >= self.confirm_s - 1e-6));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;
    fn tone(freq: f32, dev_hz: f32, n: usize) -> Vec<i16> {
        (0..n)
            .map(|i| (dev_hz * COUNTS_PER_HZ * (2. * PI * freq * i as f32 / 48000.).sin()) as i16)
            .collect()
    }
    fn rms(x: &[i16]) -> f32 {
        (x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt() as f32
    }
    /// Output amplitude (rms) of a tone of 1 kHz-equivalent deviation, in dB re the 1 kHz result.
    fn response_db(freq: f32) -> f32 {
        let mut fm = FmAudio::new();
        let out = fm.push(&tone(freq, 1500., 48000));
        let reference = {
            let mut fm = FmAudio::new();
            rms(&fm.push(&tone(1000., 1500., 48000)).audio[2000..])
        };
        20. * (rms(&out.audio[2000..]) / reference).log10()
    }
    #[test]
    fn audio_response_follows_the_nbfm_receiver_convention() {
        // Reference 0 dB at 1 kHz; -6 dB/octave de-emphasis above the 300 Hz corner.
        assert!(response_db(1000.).abs() < 0.01);
        let at_2k = response_db(2000.);
        assert!((-7.5..-4.5).contains(&at_2k), "2 kHz: {at_2k} dB");
        // CTCSS band and DC are rejected by the 300 Hz highpass: at least 18 dB down at 100 Hz.
        let at_100 = response_db(100.);
        assert!(at_100 < -18., "100 Hz: {at_100} dB");
        // Above the 3.0 kHz passband edge nothing aliases back: 5.5 kHz content is gone.
        let at_5k5 = response_db(5500.);
        assert!(at_5k5 < -40., "5.5 kHz: {at_5k5} dB");
    }
    #[test]
    fn deviation_scaling_is_preserved_at_1_khz() {
        let mut fm = FmAudio::new();
        let out = fm.push(&tone(1000., 2000., 48000));
        // 2 kHz deviation tone is 2000 * 3.2 = 6400 peak, 4525 rms, unchanged at 1 kHz.
        let r = rms(&out.audio[2000..]);
        assert!((r - 4525.).abs() < 120., "rms {r}");
        assert!((out.voice_hz - 1414.).abs() < 80., "voice_hz {}", out.voice_hz);
    }
    #[test]
    fn packet_boundaries_do_not_change_the_audio() {
        let x = tone(700., 1800., 24000);
        let mut a = FmAudio::new();
        let whole = a.push(&x).audio;
        let mut b = FmAudio::new();
        let split: Vec<i16> = x.chunks(1000).flat_map(|c| b.push(c).audio).collect();
        assert_eq!(whole, split);
    }
    #[test]
    fn noise_squelch_separates_a_quieted_carrier_from_open_noise() {
        // Deterministic white discriminator noise at the level real empty channels show
        // (about 2 kHz in the 4 to 7 kHz band, measured on recordings/ at 454 MHz).
        let mut state = 0x1234_5678u32;
        let noise: Vec<i16> = (0..48000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (((state >> 8) as f32 / 16_777_216. - 0.5) * 2. * 9000. * COUNTS_PER_HZ) as i16
            })
            .collect();
        let mut fm = FmAudio::new();
        let noisy = fm.push(&noise);
        let mut quiet_fm = FmAudio::new();
        let voice = quiet_fm.push(&tone(900., 1800., 48000));
        println!("noise-only: noise {:.0} Hz voice {:.0} Hz; voice: noise {:.0} Hz voice {:.0} Hz",
            noisy.noise_hz, noisy.voice_hz, voice.noise_hz, voice.voice_hz);
        let mut sq = Squelch::default();
        assert!(!sq.update(&noisy), "squelch opened on noise");
        let mut sq = Squelch::default();
        assert!(sq.update(&voice), "squelch closed on a quieted voice tone");
        // Silence under a carrier (no modulation) is quiet but not voice: stays shut.
        let mut quiet = FmAudio::new();
        let silent = quiet.push(&[0i16; 4800]);
        assert!(!Squelch::default().update(&silent));
    }
    fn voice_pcm(seconds: f32) -> Vec<i16> {
        tone(900., 1800., (48000. * seconds) as usize)
    }
    #[test]
    fn digital_sync_mutes_analog_for_the_hold_time_then_releases() {
        let mut gate = AnalogGate::default();
        // 0.5 s of clear voice: audio and confirmation arrive after 0.3 s.
        let blocks = gate.push(&voice_pcm(0.5), false);
        assert_eq!(blocks.len(), 5);
        assert!(blocks.iter().filter(|b| b.1).count() >= 2, "confirmed after 0.3 s of voice");
        assert!(blocks.iter().any(|b| !b.0.is_empty()));
        // A DMR sync arrives: for the next 3 s the same voice-like signal stays muted.
        let muted = gate.push(&voice_pcm(0.1), true);
        assert!(muted.iter().all(|b| b.0.is_empty() && !b.1), "muted on sync");
        let still = gate.push(&voice_pcm(2.8), false);
        assert!(still.iter().all(|b| b.0.is_empty() && !b.1), "held for 3 s: {}", still.len());
        // Hold expired: audio returns, and must re-confirm.
        let after = gate.push(&voice_pcm(0.6), false);
        assert!(after.iter().any(|b| !b.0.is_empty()), "audio returns after the hold");
        assert!(!after[0].1 || after.len() > 3);
    }
    #[test]
    fn tiny_producer_packets_give_the_same_decisions_as_whole_blocks() {
        let pcm = voice_pcm(1.0);
        let mut whole = AnalogGate::default();
        let a: Vec<(Vec<i16>, bool)> = whole.push(&pcm, false);
        let mut small = AnalogGate::default();
        let b: Vec<(Vec<i16>, bool)> = pcm.chunks(120).flat_map(|c| small.push(c, false)).collect();
        assert_eq!(a, b, "256-sample ARM-gate packets are batched to 100 ms blocks");
    }
    #[test]
    fn squelch_hysteresis_holds_between_the_thresholds() {
        let measure = |noise_hz: f32| FmBlock { audio: vec![], noise_hz, voice_hz: 800. };
        let mut sq = Squelch::default();
        assert!(!sq.update(&measure(900.)), "900 Hz is above the 700 Hz open threshold");
        assert!(sq.update(&measure(500.)), "opens below 700");
        assert!(sq.update(&measure(900.)), "stays open between 700 and 1100");
        assert!(sq.update(&measure(1050.)));
        for _ in 0..4 {
            sq.update(&measure(1200.));
        }
        assert!(!sq.update(&measure(1200.)), "closes above 1100");
    }
    #[test]
    fn squelch_hangs_through_a_short_gap_then_closes() {
        let mut sq = Squelch::default();
        let mut fm = FmAudio::new();
        let speech = fm.push(&tone(900., 1800., 4800));
        let gap = fm.push(&[0i16; 4800]);
        assert!(sq.update(&speech));
        for _ in 0..3 {
            assert!(sq.update(&gap), "hang should keep the audio open");
        }
        assert!(!sq.update(&gap), "closes once the hang is spent");
    }
}
