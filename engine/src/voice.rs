//! Light post-processing for decoded 8 kHz voice. Parameters and their basis are in
//! `DESIGN-VOCODER.md`; the chain is high-pass, presence peak, optional gate, level
//! normalization, soft limiter. Everything is causal with no look-ahead: it adds only the
//! filters' group delay (under 1 ms above 300 Hz) and the 20 ms frame the vocoder already
//! produces.

/// Sample rate of decoded voice.
pub const RATE: f64 = 8000.;
/// Level-control block: one vocoder frame.
const BLOCK: usize = 160;
const SILENCE_DB: f64 = -90.;

#[derive(Clone, Debug, PartialEq)]
pub struct VoiceConfig {
    /// Master switch. Off passes decoded audio through untouched. The default is the shipped
    /// profile: on, gate off.
    pub enabled: bool,
    /// 2nd-order Butterworth high-pass corner.
    pub highpass_hz: f64,
    /// Peaking filter centre, gain and Q ("presence"). Zero gain disables it.
    pub presence_hz: f64,
    pub presence_db: f64,
    pub presence_q: f64,
    /// Target active-speech RMS, dB relative to full scale.
    pub target_dbfs: f64,
    /// Gain applied before the level control has heard anything (start of a call). Decoded
    /// DMR speech from this engine sits near -40 dBFS, so starting at the typical boost keeps
    /// the first syllables from being swallowed while the control converges.
    pub initial_gain_db: f64,
    /// Largest boost / cut the level control may apply.
    pub max_boost_db: f64,
    pub max_cut_db: f64,
    /// Expander that attenuates between words: `Some(threshold)` is dB below the tracked
    /// active-speech level; `None` is off.
    pub gate_below_db: Option<f64>,
    /// Attenuation the closed gate applies (positive dB).
    pub gate_floor_db: f64,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            highpass_hz: 280.,
            presence_hz: 2400.,
            presence_db: 2.5,
            presence_q: 0.9,
            target_dbfs: -21.,
            initial_gain_db: 18.,
            max_boost_db: 36.,
            max_cut_db: 12.,
            gate_below_db: None,
            gate_floor_db: 15.,
        }
    }
}

impl VoiceConfig {
    /// Pass decoded audio through untouched.
    pub fn off() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        let nyquist = RATE / 2.;
        if !(50. ..=600.).contains(&self.highpass_hz) {
            return Err("voice high-pass must be 50..600 Hz".into());
        }
        if !(500. ..nyquist - 100.).contains(&self.presence_hz) || !(0.3..=4.).contains(&self.presence_q) {
            return Err("voice presence must be 500..3900 Hz with Q 0.3..4".into());
        }
        if self.presence_db.abs() > 9. {
            return Err("voice presence gain must be within +-9 dB".into());
        }
        if !(-60. ..=-6.).contains(&self.target_dbfs) {
            return Err("voice AGC target must be -60..-6 dBFS".into());
        }
        if !(0. ..=self.max_boost_db).contains(&self.initial_gain_db) {
            return Err("voice initial gain must be between 0 and the maximum boost".into());
        }
        if self.gate_below_db.is_some_and(|d| !(10. ..=60.).contains(&d)) {
            return Err("voice gate threshold must be 10..60 dB below speech level".into());
        }
        if !(3. ..=40.).contains(&self.gate_floor_db) {
            return Err("voice gate floor must be 3..40 dB".into());
        }
        Ok(())
    }
}

/// Transposed direct form II biquad, f64 for low corners at an 8 kHz rate.
#[derive(Clone, Copy, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    /// Robert Bristow-Johnson cookbook high-pass.
    fn highpass(fc: f64, q: f64) -> Self {
        let w = 2. * std::f64::consts::PI * fc / RATE;
        let (s, c) = w.sin_cos();
        let alpha = s / (2. * q);
        let a0 = 1. + alpha;
        Self {
            b: [(1. + c) / 2. / a0, -(1. + c) / a0, (1. + c) / 2. / a0],
            a: [-2. * c / a0, (1. - alpha) / a0],
            z: [0.; 2],
        }
    }
    /// Cookbook peaking EQ.
    fn peaking(fc: f64, q: f64, gain_db: f64) -> Self {
        let a = 10f64.powf(gain_db / 40.);
        let w = 2. * std::f64::consts::PI * fc / RATE;
        let (s, c) = w.sin_cos();
        let alpha = s / (2. * q);
        let a0 = 1. + alpha / a;
        Self {
            b: [(1. + alpha * a) / a0, -2. * c / a0, (1. - alpha * a) / a0],
            a: [-2. * c / a0, (1. - alpha / a) / a0],
            z: [0.; 2],
        }
    }
    fn step(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
    fn clear(&mut self) {
        self.z = [0.; 2];
    }
    /// Group delay in milliseconds at `hz`, from the numerical phase slope. For tests and docs.
    #[cfg(test)]
    fn group_delay_ms(&self, hz: f64) -> f64 {
        let phase = |f: f64| {
            let w = 2. * std::f64::consts::PI * f / RATE;
            let z1 = num_complex::Complex::from_polar(1., -w);
            let z2 = z1 * z1;
            let num = self.b[0] + self.b[1] * z1 + self.b[2] * z2;
            let den = 1. + self.a[0] * z1 + self.a[1] * z2;
            (num / den).arg()
        };
        let df = 1.;
        let dphi = phase(hz + df) - phase(hz - df);
        -dphi / (2. * std::f64::consts::PI * 2. * df) * 1000.
    }
    /// Magnitude response at `hz`, for tests and documentation.
    #[cfg(test)]
    fn gain_db(&self, hz: f64) -> f64 {
        let w = 2. * std::f64::consts::PI * hz / RATE;
        let z1 = num_complex::Complex::from_polar(1., -w);
        let z2 = z1 * z1;
        let num = self.b[0] + self.b[1] * z1 + self.b[2] * z2;
        let den = 1. + self.a[0] * z1 + self.a[1] * z2;
        20. * (num / den).norm().log10()
    }
}

fn db_to_lin(db: f64) -> f64 {
    10f64.powf(db / 20.)
}

/// Stateful processor for one voice stream (one TDMA slot).
pub struct VoicePost {
    cfg: VoiceConfig,
    highpass: Biquad,
    presence: Option<Biquad>,
    /// Filtered samples of the current 20 ms block.
    block: Vec<f64>,
    /// Running active-speech power (linear, full scale = 1) and how much speech it has seen.
    speech_power: f64,
    speech_blocks: f64,
    gain_db: f64,
    gate_open: bool,
    gate_hold: usize,
    gate_gain_db: f64,
}

impl VoicePost {
    pub fn new(cfg: VoiceConfig) -> Self {
        let mut p = Self {
            highpass: Biquad::highpass(cfg.highpass_hz, std::f64::consts::FRAC_1_SQRT_2),
            presence: (cfg.presence_db != 0.).then(|| Biquad::peaking(cfg.presence_hz, cfg.presence_q, cfg.presence_db)),
            cfg,
            block: Vec::with_capacity(BLOCK),
            speech_power: 0.,
            speech_blocks: 0.,
            gain_db: 0.,
            gate_open: true,
            gate_hold: 0,
            gate_gain_db: 0.,
        };
        p.reset();
        p
    }

    /// Forget talker-specific state (new call). Filters are cleared too.
    pub fn reset(&mut self) {
        self.highpass.clear();
        if let Some(p) = &mut self.presence {
            p.clear();
        }
        self.block.clear();
        self.speech_power = 0.;
        self.speech_blocks = 0.;
        self.gain_db = self.cfg.initial_gain_db;
        self.gate_open = true;
        self.gate_hold = 0;
        self.gate_gain_db = 0.;
    }

    /// Process decoded 8 kHz PCM in place. The output has the same length as the input;
    /// level control works in 20 ms blocks and passes the tail through at the current gain,
    /// so no block boundary is ever delayed.
    pub fn process(&mut self, pcm: &mut [i16]) {
        if !self.cfg.enabled {
            return;
        }
        for sample in pcm.iter_mut() {
            let mut x = *sample as f64 / 32768.;
            x = self.highpass.step(x);
            if let Some(p) = &mut self.presence {
                x = p.step(x);
            }
            // Level and gate decisions are made on whole blocks, but every sample is
            // emitted immediately at the gain decided so far; the decision for a block takes
            // effect from its first sample on the next block (<= 20 ms, matching the
            // vocoder frame; no look-ahead).
            self.block.push(x);
            let y = x * self.current_linear_gain();
            *sample = soft_limit(y);
            if self.block.len() == BLOCK {
                self.end_block();
            }
        }
    }

    fn current_linear_gain(&self) -> f64 {
        db_to_lin(self.gain_db - self.gate_gain_db)
    }

    fn end_block(&mut self) {
        let power = self.block.iter().map(|v| v * v).sum::<f64>() / BLOCK as f64;
        self.block.clear();
        let level_db = if power > 1e-12 { 10. * power.log10() } else { SILENCE_DB };
        // Speech is anything within 30 dB of the running speech level (or, before any level
        // is known, anything above -55 dBFS): pauses and hiss must not drag the estimate
        // down and make the gain pump up the noise.
        let speech_db = if self.speech_blocks > 0. {
            10. * self.speech_power.max(1e-12).log10()
        } else {
            SILENCE_DB
        };
        let floor = if self.speech_blocks > 0. { speech_db - 30. } else { -55. };
        if level_db > floor {
            // Count-weighted at first so a new talker converges in about a second, then a
            // 2.5 s time constant (125 blocks) so syllables are not compressed.
            self.speech_blocks = (self.speech_blocks + 1.).min(125.);
            let alpha = 1. / self.speech_blocks;
            self.speech_power += alpha * (power - self.speech_power);
            let level = 10. * self.speech_power.max(1e-12).log10();
            let want = (self.cfg.target_dbfs - level).clamp(-self.cfg.max_cut_db, self.cfg.max_boost_db);
            // Slew-limit: up 6 dB/s, down 12 dB/s (0.12 / 0.24 dB per block), faster at the
            // very start of a call.
            let (up, down) = if self.speech_blocks < 25. { (1.5, 3.) } else { (0.12, 0.24) };
            let step = (want - self.gain_db).clamp(-down, up);
            self.gain_db += step;
        }
        if let Some(below) = self.cfg.gate_below_db {
            let threshold = if self.speech_blocks >= 10. { speech_db_now(self.speech_power) - below } else { -80. };
            // 4 dB hysteresis and a 6-block (120 ms) hold keep word endings and breaths.
            if level_db > threshold {
                self.gate_open = true;
                self.gate_hold = 6;
            } else if level_db < threshold - 4. {
                if self.gate_hold > 0 {
                    self.gate_hold -= 1;
                } else {
                    self.gate_open = false;
                }
            }
            let goal = if self.gate_open { 0. } else { self.cfg.gate_floor_db };
            // Open in one block (fast attack), close at 3 dB per block (~150 dB/s).
            self.gate_gain_db = if goal < self.gate_gain_db { goal } else { (self.gate_gain_db + 3.0).min(goal) };
        } else {
            self.gate_gain_db = 0.;
        }
    }
}

fn speech_db_now(power: f64) -> f64 {
    10. * power.max(1e-12).log10()
}

/// Transparent below 0.7 of full scale, then a tanh knee that approaches 0.98.
fn soft_limit(y: f64) -> i16 {
    let a = y.abs();
    let out = if a <= 0.7 { a } else { 0.7 + 0.28 * ((a - 0.7) / 0.28).tanh() };
    (out.copysign(y) * 32767.).round() as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f64, dbfs: f64, seconds: f64) -> Vec<i16> {
        let n = (seconds * RATE) as usize;
        let amp = db_to_lin(dbfs) * 2f64.sqrt() * 32768.;
        (0..n)
            .map(|i| (amp * (2. * std::f64::consts::PI * hz * i as f64 / RATE).sin()).round() as i16)
            .collect()
    }
    fn rms_db(pcm: &[i16]) -> f64 {
        let p = pcm.iter().map(|&v| (v as f64 / 32768.).powi(2)).sum::<f64>() / pcm.len() as f64;
        10. * p.max(1e-14).log10()
    }
    /// Deterministic speech-like test signal: a 120 Hz-rich pulse train through a vowel-ish
    /// formant resonator, amplitude-modulated at a syllable rate with pauses.
    fn speechlike(dbfs: f64, seconds: f64) -> Vec<i16> {
        let n = (seconds * RATE) as usize;
        let mut res = [Biquad::peaking(700., 4., 18.), Biquad::peaking(1200., 5., 14.), Biquad::peaking(2600., 6., 10.)];
        let mut out = vec![0f64; n];
        for (i, o) in out.iter_mut().enumerate() {
            let t = i as f64 / RATE;
            let pulse = if i % 67 == 0 { 1. } else { 0. };
            let mut v = pulse;
            for r in &mut res {
                v = r.step(v);
            }
            // 4 Hz syllables, off for the last quarter of every second.
            let env = if t.fract() > 0.75 { 0. } else { 0.5 + 0.5 * (2. * std::f64::consts::PI * 4. * t).sin() };
            *o = v * env;
        }
        let p = out.iter().map(|v| v * v).sum::<f64>() / n as f64;
        let k = db_to_lin(dbfs) / p.sqrt() * 32768.;
        out.iter().map(|v| (v * k).round().clamp(-32768., 32767.) as i16).collect()
    }
    fn run(cfg: VoiceConfig, input: &[i16]) -> Vec<i16> {
        let mut p = VoicePost::new(cfg);
        let mut out = input.to_vec();
        // Odd chunk sizes: results must not depend on how the audio is sliced.
        for chunk in out.chunks_mut(173) {
            p.process(chunk);
        }
        out
    }

    #[test]
    fn disabled_is_bit_exact_passthrough() {
        let x = speechlike(-30., 2.);
        assert_eq!(run(VoiceConfig::off(), &x), x);
    }

    #[test]
    fn highpass_and_presence_responses_match_the_design() {
        let cfg = VoiceConfig::default();
        let hp = Biquad::highpass(cfg.highpass_hz, std::f64::consts::FRAC_1_SQRT_2);
        assert!((hp.gain_db(cfg.highpass_hz) + 3.01).abs() < 0.05, "corner is -3 dB");
        assert!(hp.gain_db(100.) < -15., "100 Hz rumble is cut, got {}", hp.gain_db(100.));
        assert!(hp.gain_db(1000.).abs() < 0.2, "1 kHz is flat");
        let pk = Biquad::peaking(cfg.presence_hz, cfg.presence_q, cfg.presence_db);
        assert!((pk.gain_db(cfg.presence_hz) - cfg.presence_db).abs() < 0.01);
        assert!(pk.gain_db(500.).abs() < 0.6 && pk.gain_db(3900.) < 1.5, "gentle, not a spike");
    }

    #[test]
    fn rumble_is_removed_and_midband_is_kept() {
        let mut cfg = VoiceConfig::default();
        cfg.max_boost_db = 0.;
        cfg.max_cut_db = 0.;
        let low = run(cfg.clone(), &tone(80., -20., 2.));
        let mid = run(cfg, &tone(1000., -20., 2.));
        assert!(rms_db(&low[4000..]) < -20. - 20., "80 Hz down >20 dB: {}", rms_db(&low[4000..]));
        assert!((rms_db(&mid[4000..]) + 20.).abs() < 0.6, "1 kHz unchanged: {}", rms_db(&mid[4000..]));
    }

    #[test]
    fn talkers_of_different_loudness_land_within_two_db() {
        let cfg = VoiceConfig::default();
        let measure = |dbfs: f64| {
            let out = run(cfg.clone(), &speechlike(dbfs, 12.));
            rms_db(&out[8 * 8000..])
        };
        let levels: Vec<f64> = [-48., -36., -24., -14.].iter().map(|&d| measure(d)).collect();
        let spread = levels.iter().cloned().fold(f64::MIN, f64::max) - levels.iter().cloned().fold(f64::MAX, f64::min);
        assert!(spread < 2.0, "levels {levels:?}");
    }

    #[test]
    fn output_never_clips_and_syllable_dynamics_survive() {
        let cfg = VoiceConfig::default();
        let out = run(cfg, &speechlike(-45., 12.));
        assert!(out.iter().all(|&v| v.unsigned_abs() < 32767));
        // Per-block levels still vary by well over 10 dB: it is a leveler, not a compressor.
        let blocks: Vec<f64> = out[8 * 8000..].chunks(160).map(rms_db).filter(|&d| d > -70.).collect();
        let spread = blocks.iter().cloned().fold(f64::MIN, f64::max) - blocks.iter().cloned().fold(f64::MAX, f64::min);
        assert!(spread > 10., "spread {spread}");
    }

    #[test]
    fn silence_stays_silent_and_noise_is_not_pumped_up() {
        let out = run(VoiceConfig::default(), &vec![0i16; 16000]);
        assert!(out.iter().all(|&v| v == 0));
        // Speech then a long -70 dBFS hiss: the gain freezes instead of boosting the hiss.
        let mut x = speechlike(-30., 8.);
        let mut state = 12345u32;
        x.extend((0..8 * 8000).map(|_| {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            (((state >> 16) as f64 / 65536. - 0.5) * 2. * db_to_lin(-70.) * 32768. * 1.7) as i16
        }));
        let out = run(VoiceConfig::default(), &x);
        let tail = rms_db(&out[12 * 8000..]);
        assert!(tail < -70. + 40., "hiss boosted to {tail} dBFS");
    }

    #[test]
    fn gate_attenuates_long_pauses_but_not_speech() {
        // 13 s of speech-like audio, then 3 s of nothing but low hiss, hiss under everything.
        let mut x = speechlike(-30., 13.);
        x.extend(vec![0i16; 3 * 8000]);
        let mut state = 99u32;
        let noisy: Vec<i16> = x
            .iter()
            .map(|&v| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                v.saturating_add((((state >> 16) as f64 / 65536. - 0.5) * 40.) as i16)
            })
            .collect();
        let mut gated_cfg = VoiceConfig::default();
        gated_cfg.gate_below_db = Some(30.);
        let plain = run(VoiceConfig::default(), &noisy);
        let gated = run(gated_cfg, &noisy);
        let pause = |v: &[i16]| rms_db(&v[14 * 8000..15 * 8000]);
        let drop = pause(&plain) - pause(&gated);
        assert!(drop > 12., "gate floor is 15 dB, only dropped {drop}");
        let speech = |v: &[i16]| rms_db(&v[12 * 8000..12 * 8000 + 4000]);
        assert!((speech(&gated) - speech(&plain)).abs() < 1.0, "speech untouched");
    }

    #[test]
    fn filters_add_under_one_millisecond_in_the_speech_band() {
        let cfg = VoiceConfig::default();
        let hp = Biquad::highpass(cfg.highpass_hz, std::f64::consts::FRAC_1_SQRT_2);
        let pk = Biquad::peaking(cfg.presence_hz, cfg.presence_q, cfg.presence_db);
        for hz in [300., 500., 1000., 2000., 3000.] {
            let delay = hp.group_delay_ms(hz) + pk.group_delay_ms(hz);
            assert!(delay.abs() < 1.0, "group delay at {hz} Hz is {delay:.3} ms");
        }
    }

    #[test]
    fn a_quiet_talker_is_audible_from_the_first_syllable() {
        // Decoded speech sits near -40 dBFS; the first half second must already be close to
        // the level the control settles at, not ramping up from silence.
        let out = run(VoiceConfig::default(), &speechlike(-40., 6.));
        let first = rms_db(&out[..4000]);
        let settled = rms_db(&out[4 * 8000..]);
        assert!((first - settled).abs() < 6., "first half second {first:.1} dB, settled {settled:.1} dB");
    }

    #[test]
    fn reset_restores_the_initial_state() {
        let x = speechlike(-30., 4.);
        let mut p = VoicePost::new(VoiceConfig::default());
        let mut a = x.clone();
        p.process(&mut a);
        p.reset();
        let mut b = x.clone();
        p.process(&mut b);
        assert_eq!(a, b);
    }

    #[test]
    fn config_validation_rejects_nonsense() {
        assert!(VoiceConfig::default().validate().is_ok());
        let mut c = VoiceConfig::default();
        c.highpass_hz = 5.;
        assert!(c.validate().is_err());
        let mut c = VoiceConfig::default();
        c.gate_below_db = Some(2.);
        assert!(c.validate().is_err());
    }
}
