//! Fine channel filtering for complex IQ slices.
//!
//! Design (see `DESIGN-DSP.md` for sources and measured results): a polyphase
//! rational resampler whose prototype is a Kaiser-windowed-sinc lowpass
//! (Kaiser 1974; Oppenheim & Schafer; Crochiere & Rabiner, *Multirate Digital
//! Signal Processing*, 1983, for the polyphase structure). Passband/stopband
//! edges come from the land-mobile channel plan, not from tuning by ear.
use num_complex::Complex32;
/// Channel filter specification, derived from the channel plan.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChannelSpec {
    /// Highest frequency (Hz from channel center) that must pass flat.
    pub pass_hz: f64,
    /// Lowest frequency (Hz from channel center) where a neighbor's emission begins.
    pub stop_hz: f64,
    /// Required stopband attenuation, dB.
    pub atten_db: f64,
    /// Authorized emission bandwidth of the channel class, Hz (activity metering).
    pub band_hz: f64,
}
impl ChannelSpec {
    /// 12.5 kHz channels (DMR 4FSK, narrowband analog FM). Passband edge is half of
    /// the FCC 47 CFR 90.209 authorized bandwidth for 12.5 kHz equipment (11.25 kHz;
    /// ITU-R M.478-5 class A necessary bandwidth is 11 kHz) and is also the edge of
    /// the 0 dB region of 47 CFR 90.210 Mask D. A neighbor 12.5 kHz away emits from
    /// 12.5 - 5.625 = 6.875 kHz inward. ITU-R M.478-5 §3.2.2 requires receiver
    /// adjacent channel selectivity of at least 60 dB for 12.5 kHz spacing; the
    /// design target is 70 dB for 10 dB of margin.
    pub const CHANNEL_12_5: Self = Self {
        pass_hz: 5625.,
        stop_hz: 6875.,
        atten_db: 70.,
        band_hz: 11250.,
    };
    /// 6.25 kHz channels (NXDN48). FCC authorizes 6 kHz (Mask E 0 dB region +-3 kHz),
    /// so a neighbor 6.25 kHz away emits from 3.25 kHz inward. The NXDN Forum quotes
    /// about 4 kHz occupied bandwidth, so the passband stops at 2.5 kHz. The 60 dB
    /// stopband is a design choice: no standard in the sources consulted gives a
    /// 6.25 kHz receiver selectivity figure.
    pub const CHANNEL_6_25: Self = Self {
        pass_hz: 2500.,
        stop_hz: 3250.,
        atten_db: 60.,
        band_hz: 6000.,
    };
}
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1., 1.);
    for k in 1..60 {
        term *= (x / (2. * k as f64)).powi(2);
        sum += term;
        if term < sum * 1e-16 {
            break;
        }
    }
    sum
}
/// Kaiser's empirical beta for stopband attenuation `a` dB.
pub fn kaiser_beta(a: f64) -> f64 {
    if a > 50. {
        0.1102 * (a - 8.7)
    } else if a >= 21. {
        0.5842 * (a - 21.).powf(0.4) + 0.07886 * (a - 21.)
    } else {
        0.
    }
}
/// Kaiser's order estimate M = (A - 8) / (2.285 * dw), dw in rad/sample.
pub fn kaiser_order(a: f64, transition_cycles_per_sample: f64) -> f64 {
    (a - 8.) / (2.285 * 2. * std::f64::consts::PI * transition_cycles_per_sample)
}
/// Causal windowed sinc resampler. Output clock and history persist across packets.
pub struct Resampler {
    input_rate: f64,
    output_rate: f64,
    next: f64,
    total: u64,
    history: Vec<Complex32>,
    radius: i64,
    coefficients: Vec<Vec<f32>>,
}
impl Resampler {
    pub fn new(input: f64, output: f64, spec: ChannelSpec) -> Self {
        assert!(input.is_finite() && output.is_finite() && input > 0. && output > 0.);
        assert!(spec.stop_hz > spec.pass_hz && spec.stop_hz < input / 2.);
        // Cutoff halfway through the transition band: -6 dB at the channel edge.
        let fc = (spec.pass_hz + spec.stop_hz) / 2. / input;
        let transition = (spec.stop_hz - spec.pass_hz) / input;
        let radius = (kaiser_order(spec.atten_db, transition) / 2.).ceil() as i64;
        let beta = kaiser_beta(spec.atten_db);
        let i0_beta = bessel_i0(beta);
        let ratio = input / output;
        let phases = (1..=1024)
            .find(|n| {
                let x = ratio * (*n as f64);
                (x - x.round()).abs() < 1e-8
            })
            .unwrap_or(1024);
        let coefficients = (0..phases)
            .map(|phase| {
                let fraction = phase as f64 / phases as f64;
                let mut row: Vec<f32> = (-radius..=radius)
                    .map(|index| {
                        let x = index as f64 - fraction;
                        let t = 2. * fc * x;
                        let sinc = if t.abs() < 1e-10 {
                            1.
                        } else {
                            (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t)
                        };
                        let u = x / radius as f64;
                        let window = if u.abs() <= 1. {
                            bessel_i0(beta * (1. - u * u).sqrt()) / i0_beta
                        } else {
                            0.
                        };
                        (sinc * window * 2. * fc) as f32
                    })
                    .collect();
                let sum: f32 = row.iter().sum();
                for v in &mut row {
                    *v /= sum;
                }
                row
            })
            .collect();
        Self {
            input_rate: input,
            output_rate: output,
            next: 0.,
            total: 0,
            history: vec![],
            radius,
            coefficients,
        }
    }
    pub fn push(&mut self, input: &[Complex32]) -> Vec<Complex32> {
        let start = self.total as i64 - self.history.len() as i64;
        self.history.extend_from_slice(input);
        self.total += input.len() as u64;
        let mut output = Vec::with_capacity(
            (input.len() as f64 * self.output_rate / self.input_rate).ceil() as usize,
        );
        while self.next + (self.radius as f64) + 1. < (self.total as f64) {
            let mut center = self.next.floor() as i64;
            let phases = self.coefficients.len();
            let mut phase = ((self.next - center as f64) * phases as f64).round() as usize;
            if phase == phases {
                phase = 0;
                center += 1;
            }
            let first = (center - self.radius).max(start).max(0);
            let last = (center + self.radius + 1).min(self.total as i64);
            let row = &self.coefficients[phase];
            let offset = (first - (center - self.radius)) as usize;
            let mut acc = Complex32::new(0., 0.);
            for (sample, weight) in self.history[(first - start) as usize..(last - start) as usize]
                .iter()
                .zip(&row[offset..offset + (last - first) as usize])
            {
                acc += *sample * *weight;
            }
            output.push(acc);
            self.next += self.input_rate / self.output_rate;
        }
        let keep_from = (self.next.floor() as i64 - self.radius - 2)
            .max(start)
            .max(0);
        self.history.drain(..(keep_from - start) as usize);
        output
    }
}
pub struct ChannelDsp {
    resample: Resampler,
    phase: Complex32,
    step: Complex32,
    previous: Option<Complex32>,
    dc_x: f32,
    dc_y: f32,
    count: u64,
}
impl ChannelDsp {
    pub fn new(rate: f64, offset: f64, spec: ChannelSpec) -> Self {
        Self {
            resample: Resampler::new(rate, 48000., spec),
            phase: Complex32::new(1., 0.),
            step: Complex32::from_polar(1., (-2. * std::f64::consts::PI * offset / rate) as f32),
            previous: None,
            dc_x: 0.,
            dc_y: 0.,
            count: 0,
        }
    }
    /// Mix the target to baseband and band-limit/resample to 48 kHz complex.
    pub fn filter(&mut self, iq: &[Complex32]) -> Vec<Complex32> {
        let mixed: Vec<_> = iq
            .iter()
            .map(|x| {
                let y = *x * self.phase;
                self.phase *= self.step;
                self.count += 1;
                if self.count.is_multiple_of(4096) {
                    self.phase /= self.phase.norm();
                }
                y
            })
            .collect();
        self.resample.push(&mixed)
    }
    /// FM discriminator with DC removal, scaled for the 5 kHz deviation DSDcc expects.
    pub fn demod(&mut self, filtered: &[Complex32]) -> Vec<i16> {
        filtered
            .iter()
            .map(|&x| {
                let p = self.previous.unwrap_or(x);
                self.previous = Some(x);
                let fm = (x * p.conj()).arg();
                let dc = fm - self.dc_x + 0.999 * self.dc_y;
                self.dc_x = fm;
                self.dc_y = dc;
                (dc * 48000. / (2. * std::f32::consts::PI * 5000.) * 16000.).clamp(-32768., 32767.)
                    as i16
            })
            .collect()
    }
    pub fn push(&mut self, iq: &[Complex32]) -> Vec<i16> {
        let filtered = self.filter(iq);
        self.demod(&filtered)
    }
}
/// In-place radix-2 FFT; `data.len()` must be a power of two.
fn fft(data: &mut [Complex32]) {
    let n = data.len();
    assert!(n.is_power_of_two());
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            data.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let step = Complex32::from_polar(1., -2. * std::f32::consts::PI / len as f32);
        for block in data.chunks_exact_mut(len) {
            let mut w = Complex32::new(1., 0.);
            let (lo, hi) = block.split_at_mut(len / 2);
            for (a, b) in lo.iter_mut().zip(hi.iter_mut()) {
                let t = *b * w;
                *b = *a - t;
                *a += t;
                w *= step;
            }
        }
        len <<= 1;
    }
}
/// A logical target inside one shared physical IQ slice.
pub struct TargetBand {
    pub logical: u64,
    pub offset_hz: f64,
    pub bandwidth_hz: f64,
    pub active: bool,
    pub level_dbfs: f64,
    /// Power-weighted spectral centroid of this packet's occupied bins, Hz from the slice
    /// center; `None` when the band holds no carrier.
    pub centroid_hz: Option<f64>,
    /// Running estimate of the carrier's offset from the slice center (Hz), smoothed over
    /// packets whose in-band power was well above noise.
    pub carrier_hz: Option<f64>,
    /// Packets that contributed to `carrier_hz`.
    pub carrier_updates: u32,
    /// In-band power over the slice noise reference, dB, from the latest packet.
    pub snr_db: f64,
}
impl TargetBand {
    pub fn new(logical: u64, offset_hz: f64, bandwidth_hz: f64) -> Self {
        Self {
            logical,
            offset_hz,
            bandwidth_hz,
            active: false,
            level_dbfs: f64::NEG_INFINITY,
            centroid_hz: None,
            carrier_hz: None,
            carrier_updates: 0,
            snr_db: 0.,
        }
    }
}
/// Per-target RF activity from the shared slice's spectrum. It does not depend on
/// codec admission, and it is separate from the physical slice's activity: a
/// neighbor transmitting elsewhere in the slice does not make a quiet target active.
/// The noise reference is the slice's median bin power, so it needs no history.
pub struct SliceMeter {
    window: Vec<f32>,
    window_power: f32,
    /// Samples of an incomplete window, carried to the next packet so producers that
    /// emit short blocks (the ARM gate sends at most 256 samples) are still measured.
    pending: Vec<Complex32>,
    pub targets: Vec<TargetBand>,
}
impl SliceMeter {
    const FFT: usize = 1024;
    /// DB above the slice noise to open a target, and to keep it open.
    const OPEN_DB: f64 = 8.;
    const CLOSE_DB: f64 = 5.;
    /// Carrier estimation window around the nominal frequency: half the 12.5 kHz raster, so
    /// a neighboring channel's emissions (which begin 6.875 kHz out) stay outside.
    const CARRIER_WINDOW_HZ: f64 = 6250.;
    /// Bins at least 4 dB over the noise estimate count toward the centroid.
    const CARRIER_BIN_RATIO: f64 = 2.512;
    /// Minimum in-band excess for any centroid, and for it to move the tracked estimate.
    const CARRIER_MIN_DB: f64 = 6.;
    const CARRIER_TRACK_DB: f64 = 10.;
    const CARRIER_SMOOTH: f64 = 0.25;
    pub fn new(targets: Vec<TargetBand>) -> Self {
        let window: Vec<f32> = (0..Self::FFT)
            .map(|i| {
                0.5 - 0.5 * (2. * std::f32::consts::PI * i as f32 / Self::FFT as f32).cos()
            })
            .collect();
        let window_power = window.iter().map(|w| w * w).sum();
        Self {
            window,
            window_power,
            pending: Vec::new(),
            targets,
        }
    }
    /// Drop the carried partial window (new burst, gap or discontinuity).
    pub fn reset_window(&mut self) {
        self.pending.clear();
    }
    /// Update every target from one live IQ packet. `full_scale` is the amplitude of a
    /// full-scale sample. Returns false until enough samples have accumulated for one
    /// FFT window (the remainder of a packet is kept for the next call).
    pub fn measure(&mut self, iq: &[Complex32], rate: f64, full_scale: f64) -> bool {
        self.pending.extend_from_slice(iq);
        let windows = self.pending.len() / Self::FFT;
        if windows == 0 {
            return false;
        }
        let iq = std::mem::take(&mut self.pending);
        self.pending = iq[windows * Self::FFT..].to_vec();
        let mut power = vec![0f64; Self::FFT];
        let mut buffer = vec![Complex32::new(0., 0.); Self::FFT];
        for chunk in iq[..windows * Self::FFT].chunks_exact(Self::FFT) {
            for ((b, x), w) in buffer.iter_mut().zip(chunk).zip(&self.window) {
                *b = *x * *w;
            }
            fft(&mut buffer);
            for (p, b) in power.iter_mut().zip(&buffer) {
                *p += b.norm_sqr() as f64;
            }
        }
        // Mean-square amplitude carried by each bin (Parseval).
        let scale = 1. / (windows as f64 * Self::FFT as f64 * self.window_power as f64);
        for p in &mut power {
            *p *= scale;
        }
        let mut sorted = power.clone();
        sorted.sort_by(f64::total_cmp);
        // The median of a mean of m exponential bin powers (a Gamma(m) variate) is about
        // (m - 1/3)/m of its mean, so scale it up to estimate the mean noise power.
        let m = windows as f64;
        let noise_bin = (sorted[Self::FFT / 2] * m / (m - 1. / 3.)).max(1e-12);
        let bin_hz = rate / Self::FFT as f64;
        for target in &mut self.targets {
            let mut total = 0.;
            let mut bins = 0;
            for (k, p) in power.iter().enumerate() {
                let f = if k < Self::FFT / 2 {
                    k as f64 * bin_hz
                } else {
                    (k as f64 - Self::FFT as f64) * bin_hz
                };
                if (f - target.offset_hz).abs() <= target.bandwidth_hz / 2. {
                    total += p;
                    bins += 1;
                }
            }
            let noise = noise_bin * bins.max(1) as f64;
            let ratio_db = 10. * (total.max(1e-18) / noise).log10();
            // Carrier estimate: power-weighted centroid over bins that stand 4 dB above noise.
            let (mut weight, mut moment) = (0., 0.);
            for (k, p) in power.iter().enumerate() {
                let f = if k < Self::FFT / 2 {
                    k as f64 * bin_hz
                } else {
                    (k as f64 - Self::FFT as f64) * bin_hz
                };
                if (f - target.offset_hz).abs() <= Self::CARRIER_WINDOW_HZ
                    && *p > noise_bin * Self::CARRIER_BIN_RATIO
                {
                    weight += p - noise_bin;
                    moment += f * (p - noise_bin);
                }
            }
            target.snr_db = ratio_db;
            target.centroid_hz = (weight > 0. && ratio_db > Self::CARRIER_MIN_DB).then(|| moment / weight);
            if let Some(c) = target.centroid_hz
                && ratio_db >= Self::CARRIER_TRACK_DB
            {
                target.carrier_hz = Some(match target.carrier_hz {
                    None => c,
                    Some(old) => old + Self::CARRIER_SMOOTH * (c - old),
                });
                target.carrier_updates += 1;
            }
            target.active = if target.active {
                ratio_db > Self::CLOSE_DB
            } else {
                ratio_db > Self::OPEN_DB
            };
            target.level_dbfs = 10. * (total.max(1e-18) / (full_scale * full_scale)).log10();
        }
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packet_boundaries_do_not_change_dsp() {
        let data: Vec<_> = (0..25000)
            .map(|i| Complex32::from_polar(1000., i as f32 * 0.04))
            .collect();
        let mut a = ChannelDsp::new(25000., 0., ChannelSpec::CHANNEL_12_5);
        let all = a.push(&data);
        let mut b = ChannelDsp::new(25000., 0., ChannelSpec::CHANNEL_12_5);
        let chunks: Vec<_> = data.chunks(127).flat_map(|c| b.push(c)).collect();
        assert_eq!(all, chunks);
    }
    use std::f64::consts::PI;
    /// Deterministic complex white noise with the given per-sample mean-square power.
    fn noise(n: usize, power: f64, seed: u64) -> Vec<Complex32> {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut uniform = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ((state >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        };
        (0..n)
            .map(|_| {
                let r = (-2. * uniform().ln()).sqrt() * (power / 2.).sqrt();
                let t = 2. * PI * uniform();
                Complex32::new((r * t.cos()) as f32, (r * t.sin()) as f32)
            })
            .collect()
    }
    /// FM carrier at `offset_hz` with a sinusoidal message.
    fn fm(n: usize, rate: f64, offset_hz: f64, amplitude: f64, tone_hz: f64, deviation_hz: f64) -> Vec<Complex32> {
        let index = deviation_hz / tone_hz;
        (0..n)
            .map(|i| {
                let t = i as f64 / rate;
                let phase = 2. * PI * offset_hz * t + index * (2. * PI * tone_hz * t).sin();
                Complex32::new((amplitude * phase.cos()) as f32, (amplitude * phase.sin()) as f32)
            })
            .collect()
    }
    fn power(x: &[Complex32]) -> f64 {
        x.iter().map(|v| v.norm_sqr() as f64).sum::<f64>() / x.len().max(1) as f64
    }
    fn db(ratio: f64) -> f64 {
        10. * ratio.max(1e-30).log10()
    }
    /// Steady-state channel filter output power for a unit-amplitude tone, in dB.
    fn tone_gain_db(rate: f64, spec: ChannelSpec, tone_hz: f64) -> f64 {
        let mut dsp = ChannelDsp::new(rate, 0., spec);
        let n = (rate * 0.4) as usize;
        let iq: Vec<_> = (0..n)
            .map(|i| {
                let ph = 2. * PI * tone_hz * i as f64 / rate;
                Complex32::new(ph.cos() as f32, ph.sin() as f32)
            })
            .collect();
        let out = dsp.filter(&iq);
        db(power(&out[out.len() / 2..]))
    }
    /// Measured response against the channel-plan spec, for every stream rate the
    /// producer may send (16..192 ksps; 25 ksps PFB slices and 100 ksps coarse slices).
    #[test]
    fn channel_filter_meets_channel_plan_specification() {
        for spec in [ChannelSpec::CHANNEL_12_5, ChannelSpec::CHANNEL_6_25] {
            for rate in [16000., 25000., 50000., 100000., 192000.] {
                let mut ripple: f64 = 0.;
                let mut f = 0.;
                while f <= spec.pass_hz {
                    ripple = ripple.max(tone_gain_db(rate, spec, f).abs());
                    f += spec.pass_hz / 11.;
                }
                assert!(ripple < 0.1, "{spec:?} @ {rate}: passband ripple {ripple} dB");
                let edge = tone_gain_db(rate, spec, (spec.pass_hz + spec.stop_hz) / 2.);
                assert!((edge + 6.).abs() < 0.6, "{spec:?} @ {rate}: channel-edge gain {edge} dB");
                let mut worst = f64::MIN;
                let mut f = spec.stop_hz;
                while f < rate / 2. * 0.98 {
                    worst = worst.max(tone_gain_db(rate, spec, f));
                    f += 331.;
                }
                println!("{spec:?} @ {rate}: ripple {ripple:.4} dB, edge {edge:.2} dB, stopband worst {worst:.1} dB");
                assert!(
                    worst < -(spec.atten_db - 1.),
                    "{spec:?} @ {rate}: stopband {worst} dB, need -{}",
                    spec.atten_db
                );
            }
        }
    }
    /// ITU-R M.478-5 §3.2.2 requires 60 dB adjacent channel selectivity at 12.5 kHz
    /// spacing. An FM interferer one channel away (same modulation limit, 2.5 kHz
    /// deviation) 60 dB above the wanted signal must be at least 10 dB below the
    /// wanted signal after filtering, i.e. the filter alone supplies at least 70 dB.
    #[test]
    fn adjacent_channel_selectivity_at_100_ksps_exceeds_60_db() {
        let rate = 100000.;
        let n = 100000;
        for (target, neighbor) in [(-25000., -12500.), (-25000., -37500.), (0., 12500.), (37500., 25000.)] {
            let spec = ChannelSpec::CHANNEL_12_5;
            let wanted = fm(n, rate, target, 10., 1000., 2500.);
            let interferer = fm(n, rate, neighbor, 10000., 700., 2500.);
            let mut a = ChannelDsp::new(rate, target, spec);
            let mut b = ChannelDsp::new(rate, target, spec);
            let w = a.filter(&wanted);
            let i = b.filter(&interferer);
            let margin = db(power(&w[w.len() / 2..]) / power(&i[i.len() / 2..]));
            println!("target {target} neighbor {neighbor}: wanted/residual {margin:.1} dB with interferer +60 dB");
            assert!(margin >= 10., "selectivity short: {margin} dB");
        }
    }
    /// NXDN48 has 6.25 kHz neighbors; the narrow class must hold 60 dB there at 100 ksps.
    #[test]
    fn narrow_class_rejects_a_6_25_khz_neighbor() {
        let rate = 100000.;
        let n = 100000;
        for neighbor in [-6250., 6250.] {
            let spec = ChannelSpec::CHANNEL_6_25;
            let mut a = ChannelDsp::new(rate, 0., spec);
            let mut b = ChannelDsp::new(rate, 0., spec);
            let w = a.filter(&fm(n, rate, 0., 10., 700., 1050.));
            let i = b.filter(&fm(n, rate, neighbor, 10000., 500., 1050.));
            let margin = db(power(&w[w.len() / 2..]) / power(&i[i.len() / 2..]));
            println!("narrow neighbor {neighbor}: wanted/residual {margin:.1} dB at +60 dB");
            assert!(margin >= 0., "narrow class selectivity short: {margin} dB");
        }
        // The 12.5 kHz class cannot do this: its stopband starts at 6.875 kHz.
        let mut wide = ChannelDsp::new(rate, 0., ChannelSpec::CHANNEL_12_5);
        let leak = wide.filter(&fm(n, rate, 6250., 10000., 500., 1050.));
        assert!(power(&leak[leak.len() / 2..]) > 1e5, "wide filter unexpectedly rejects 6.25 kHz");
    }
    /// The carrier estimate must land within 150 Hz of the truth wherever in the +-3 kHz
    /// range the carrier sits inside its channel, for voice-modulated FM at 2.5 kHz deviation.
    #[test]
    fn carrier_estimate_locates_an_off_center_fm_carrier() {
        let rate = 100000.;
        for truth in [-3000., -1200., 0., 800., 3000.] {
            let mut meter = SliceMeter::new(vec![band(0., 11250.)]);
            let mut iq = noise(40000, 100., 21);
            for (s, c) in iq.iter_mut().zip(fm(40000, rate, truth, 3000., 1000., 2500.)) {
                *s += c;
            }
            for packet in iq.chunks(10000) {
                assert!(meter.measure(packet, rate, 32768.));
            }
            let est = meter.targets[0].carrier_hz.expect("carrier");
            println!("carrier truth {truth} est {est:.0}");
            assert!((est - truth).abs() < 150., "truth {truth}, estimate {est}");
            assert!(meter.targets[0].carrier_updates >= 3);
        }
        // Noise alone never produces a tracked carrier.
        let mut meter = SliceMeter::new(vec![band(0., 11250.)]);
        for seed in 0..10 {
            meter.measure(&noise(10000, 100., 50 + seed), rate, 32768.);
        }
        assert!(meter.targets[0].carrier_hz.is_none());
    }
    /// The ARM gate emits blocks of at most 256 samples; the meter must still work on them.
    #[test]
    fn slice_meter_accumulates_short_packets() {
        let rate = 100000.;
        let mut meter = SliceMeter::new(vec![band(-25000., 11250.), band(12500., 11250.)]);
        let mut iq = noise(20480, 100., 11);
        for (s, c) in iq.iter_mut().zip(fm(20480, rate, 12500., 3000., 1000., 2500.)) {
            *s += c;
        }
        let measured: Vec<bool> = iq.chunks(256).map(|p| meter.measure(p, rate, 32768.)).collect();
        assert!(!measured[0] && measured.iter().filter(|m| **m).count() >= 15);
        assert_eq!(
            meter.targets.iter().map(|t| t.active).collect::<Vec<_>>(),
            vec![false, true]
        );
        meter.reset_window();
        assert!(!meter.measure(&iq[..256], rate, 32768.), "reset must drop the carried window");
    }
    /// Several logical targets share one physical 100 ksps slice; each worker must
    /// recover its own FM message and reject the others (including the adjacent one).
    #[test]
    fn targets_sharing_one_slice_recover_their_own_audio() {
        let rate = 100000.;
        let n = 100000;
        let carriers = [(-25000., 500.), (-12500., 1000.), (25000., 1500.)];
        let mut slice = vec![Complex32::new(0., 0.); n];
        for &(offset, tone) in &carriers {
            for (s, c) in slice.iter_mut().zip(fm(n, rate, offset, 1000., tone, 2500.)) {
                *s += c;
            }
        }
        let goertzel = |x: &[i16], f: f64| {
            let (mut re, mut im) = (0., 0.);
            for (i, v) in x.iter().enumerate() {
                let ph = 2. * PI * f * i as f64 / 48000.;
                re += *v as f64 * ph.cos();
                im += *v as f64 * ph.sin();
            }
            (re * re + im * im) / (x.len() * x.len()) as f64
        };
        for &(offset, tone) in &carriers {
            let mut dsp = ChannelDsp::new(rate, offset, ChannelSpec::CHANNEL_12_5);
            let pcm = dsp.push(&slice);
            let pcm = &pcm[pcm.len() / 2..];
            let own = goertzel(pcm, tone);
            for &(_, other) in carriers.iter().filter(|(o, _)| *o != offset) {
                let leak = goertzel(pcm, other);
                println!("target {offset}: own {tone} Hz vs {other} Hz: {:.1} dB", db(own / leak));
                assert!(db(own / leak) > 40., "target {offset} hears {other} Hz at {} dB", db(own / leak));
            }
        }
    }
    fn band(offset_hz: f64, bandwidth_hz: f64) -> TargetBand {
        TargetBand {
            logical: offset_hz as u64,
            offset_hz,
            bandwidth_hz,
            active: false,
            level_dbfs: f64::NEG_INFINITY,
            centroid_hz: None,
            carrier_hz: None,
            carrier_updates: 0,
            snr_db: 0.,
        }
    }
    #[test]
    fn fft_matches_direct_dft() {
        let x = noise(64, 1., 3);
        let mut y = x.clone();
        fft(&mut y);
        for k in 0..64 {
            let dft: Complex32 = x
                .iter()
                .enumerate()
                .map(|(n, v)| *v * Complex32::from_polar(1., -2. * PI as f32 * (k * n) as f32 / 64.))
                .sum();
            assert!((dft - y[k]).norm() < 1e-3, "bin {k}");
        }
    }
    /// A carrier in one target's band must light only that target, however strong it is,
    /// and the slice's other targets (including the adjacent channel) stay quiet.
    #[test]
    fn slice_meter_keeps_target_activity_separate_from_slice_activity() {
        let rate = 100000.;
        let mut meter = SliceMeter::new(vec![band(-25000., 11250.), band(0., 11250.), band(12500., 11250.), band(25000., 11250.)]);
        let n = 10000;
        // The slice has RF (a strong carrier at +12.5 kHz) but the others hold only noise.
        let mut iq = noise(n, 100., 7);
        for (s, c) in iq.iter_mut().zip(fm(n, rate, 12500., 3000., 1000., 2500.)) {
            *s += c;
        }
        assert!(meter.measure(&iq, rate, 32768.));
        let active: Vec<bool> = meter.targets.iter().map(|t| t.active).collect();
        assert_eq!(active, vec![false, false, true, false], "levels {:?}", meter.targets.iter().map(|t| t.level_dbfs).collect::<Vec<_>>());
        // Level is calibrated: a 3000-amplitude carrier is 20log10(3000/32768) dBFS.
        let level = meter.targets[2].level_dbfs;
        assert!((level - 20. * (3000f64 / 32768.).log10()).abs() < 0.5, "level {level}");
    }
    #[test]
    fn slice_meter_does_not_false_alarm_on_noise_and_follows_hysteresis() {
        let rate = 100000.;
        let mut meter = SliceMeter::new(vec![band(-25000., 11250.), band(0., 11250.), band(25000., 11250.)]);
        for seed in 0..300 {
            assert!(meter.measure(&noise(10000, 400., seed), rate, 32768.));
            assert!(meter.targets.iter().all(|t| !t.active), "false alarm on packet {seed}");
        }
        // Hysteresis: 6.5 dB above noise stays off when idle but holds once open.
        let level = |db_above: f64| {
            let mut iq = noise(10000, 400., 99);
            // Mean-square noise in the 11.25 kHz band is 400 * 11250/100000.
            let amp = (400. * 0.1125 * 10f64.powf(db_above / 10.)).sqrt();
            for (s, c) in iq.iter_mut().zip(fm(10000, rate, 0., amp, 1000., 2500.)) {
                *s += c;
            }
            iq
        };
        meter.measure(&level(6.5), rate, 32768.);
        assert!(!meter.targets[1].active, "opened below the open threshold");
        meter.measure(&level(12.), rate, 32768.);
        assert!(meter.targets[1].active);
        meter.measure(&level(6.5), rate, 32768.);
        assert!(meter.targets[1].active, "closed above the close threshold");
        meter.measure(&level(2.), rate, 32768.);
        assert!(!meter.targets[1].active);
    }
}
