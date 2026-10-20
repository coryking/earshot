//! P25 Phase 1 receiver: complex channel IQ -> symbols -> frames.
//!
//! One demodulator serves both C4FM and CQPSK/LSM (simulcast) control channels.
//! It measures the phase change across one symbol period, arg(y[n]·conj(y[n-10]))
//! at 48 kHz. For C4FM this equals the integrate-and-dump of the FM discriminator
//! the standard's receiver uses (TIA-102.BAAA 5.x, a ±1800/±600 Hz deviation
//! integrates to ±3π/4, ±π/4 per symbol); for CQPSK it is the differential
//! detector at the symbol instant. OP25 uses the same differential decision for
//! its CQPSK path (`gardner_cc` + `costas_loop` + `diff_phasor`); SDRTrunk's LSM
//! demodulator does likewise (`P25P1DemodulatorLSM`).
//!
//! Timing and level come from the 24-symbol frame sync: each sync is located by
//! normalized correlation over the ten sample phases, the gain/offset are a least
//! squares fit to the sync symbols, and a slow decision-directed loop tracks
//! timing and DC (frequency offset) inside a frame. Frame lengths and the status
//! symbol positions follow OP25's `p25_framer.cc`.
use super::fec::{crc_ccitt_ok, nid_decode, trellis_half_decode};
use crate::dsp::Resampler;
use num_complex::Complex32;
use std::collections::VecDeque;

pub const SYMBOL_RATE: f64 = 4800.;
pub const RATE: f64 = 48000.;
const SPS: usize = 10;
/// 0x5575F5FF77FF as symbols (+3 for dibit 01, -3 for dibit 11).
pub const SYNC: [i8; 24] = [
    3, 3, 3, 3, 3, -3, 3, 3, -3, -3, 3, 3, -3, -3, -3, -3, 3, -3, 3, -3, -3, -3, -3, -3,
];

pub fn dibit_to_symbol(dibit: u8) -> i8 {
    [1, 3, -1, -3][dibit as usize & 3]
}
fn symbol_to_dibit(value: f32) -> u8 {
    if value >= 2. {
        1
    } else if value >= 0. {
        0
    } else if value >= -2. {
        2
    } else {
        3
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Duid {
    Hdu,
    Tdu,
    Ldu1,
    Tsdu,
    Ldu2,
    Pdu,
    TduLc,
    Other(u8),
}
impl Duid {
    pub fn from(v: u8) -> Self {
        match v {
            0 => Duid::Hdu,
            3 => Duid::Tdu,
            5 => Duid::Ldu1,
            7 => Duid::Tsdu,
            10 => Duid::Ldu2,
            12 => Duid::Pdu,
            15 => Duid::TduLc,
            o => Duid::Other(o),
        }
    }
    /// Total dibits including sync, NID and status symbols (OP25 max_frame_lengths / 2).
    fn fixed_dibits(&self) -> Option<usize> {
        match self {
            Duid::Hdu => Some(396),
            Duid::Tdu => Some(72),
            Duid::Ldu1 | Duid::Ldu2 => Some(864),
            Duid::TduLc => Some(216),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub nac: u16,
    pub duid: Duid,
    pub nid_errors: u32,
    /// Bits following the NID with status symbols removed (0/1 per byte).
    pub bits: Vec<u8>,
    /// TSDU / PDU blocks: decoded 12-byte block, trellis errors, CRC result.
    pub blocks: Vec<([u8; 12], u32, bool)>,
    /// Sample index (48 kHz receiver clock) of the last sync symbol.
    pub sample: u64,
    pub sync_correlation: f32,
    /// RMS distance of the decided symbols from ideal levels, in symbol units.
    pub symbol_error_rms: f32,
}

/// Mixer + resampler to a 48 kHz complex channel, with slow AFC fed back from
/// the demodulator's measured DC (frequency offset).
pub struct Frontend {
    resample: Resampler,
    phase: Complex32,
    input_rate: f64,
    offset_hz: f64,
    afc_hz: f64,
    count: u64,
}
impl Frontend {
    pub fn new(input_rate: f64, offset_hz: f64) -> Self {
        Self {
            resample: Resampler::new(input_rate, RATE),
            phase: Complex32::new(1., 0.),
            input_rate,
            offset_hz,
            afc_hz: 0.,
            count: 0,
        }
    }
    pub fn push(&mut self, iq: &[Complex32]) -> Vec<Complex32> {
        let step = Complex32::from_polar(
            1.,
            (-2. * std::f64::consts::PI * (self.offset_hz + self.afc_hz) / self.input_rate) as f32,
        );
        let mixed: Vec<_> = iq
            .iter()
            .map(|x| {
                let y = *x * self.phase;
                self.phase *= step;
                self.count += 1;
                if self.count.is_multiple_of(1024) {
                    self.phase /= self.phase.norm();
                }
                y
            })
            .collect();
        self.resample.push(&mixed)
    }
    pub fn afc_hz(&self) -> f64 {
        self.afc_hz
    }
    /// Apply a measured residual offset. Bounded to ±3 kHz of the commanded tune.
    pub fn nudge(&mut self, measured_hz: f64) {
        self.afc_hz = (self.afc_hz + 0.3 * measured_hz).clamp(-3000., 3000.);
    }
}

enum State {
    Hunting,
    Frame(FrameRx),
}
struct FrameRx {
    /// Absolute sample index of the last sync symbol.
    anchor: u64,
    /// Fractional timing correction in samples.
    mu: f32,
    gain: f32,
    offset: f32,
    correlation: f32,
    dibits: Vec<u8>,
    soft: Vec<f32>,
    duid: Option<(u16, Duid, u32)>,
    target: usize,
    blocks: Vec<([u8; 12], u32, bool)>,
}

/// Differential-phase symbol demodulator and P25 Phase 1 framer at 48 kHz.
pub struct Receiver {
    previous: VecDeque<Complex32>,
    d: VecDeque<f32>,
    /// Absolute index of `d[0]`.
    base: u64,
    /// Next absolute index to examine for a sync.
    cursor: u64,
    state: State,
    pending_peak: Option<(u64, f32)>,
    pub threshold: f32,
    pub frames: u64,
    pub nid_failures: u64,
    /// Running estimate of the DC term (radians per symbol) from recent frames.
    pub last_offset_rad: f32,
    expected_nac: Option<u16>,
}

impl Default for Receiver {
    fn default() -> Self {
        Self::new()
    }
}

impl Receiver {
    pub fn new() -> Self {
        Self {
            previous: VecDeque::with_capacity(SPS + 1),
            d: VecDeque::new(),
            base: 0,
            cursor: (SPS * 24) as u64,
            state: State::Hunting,
            pending_peak: None,
            threshold: 0.82,
            frames: 0,
            nid_failures: 0,
            last_offset_rad: 0.,
            expected_nac: None,
        }
    }
    /// Feed 48 kHz complex samples; returns completed frames.
    pub fn push(&mut self, samples: &[Complex32]) -> Vec<Frame> {
        for &x in samples {
            let delta = if self.previous.len() == SPS {
                let old = self.previous.pop_front().unwrap();
                (x * old.conj()).arg()
            } else {
                0.
            };
            self.previous.push_back(x);
            self.d.push_back(delta);
        }
        let mut frames = Vec::new();
        self.run(&mut frames);
        // Keep enough history for one sync window behind the cursor.
        let keep_from = self
            .cursor
            .saturating_sub((SPS * 30) as u64)
            .min(match &self.state {
                State::Frame(f) => f.anchor.saturating_sub((SPS * 30) as u64),
                State::Hunting => u64::MAX,
            });
        if keep_from > self.base {
            let n = ((keep_from - self.base) as usize).min(self.d.len());
            self.d.drain(..n);
            self.base += n as u64;
        }
        frames
    }
    fn end(&self) -> u64 {
        self.base + self.d.len() as u64
    }
    fn at(&self, index: f32) -> Option<f32> {
        if index < self.base as f32 {
            return None;
        }
        let i = index.floor();
        let frac = index - i;
        let i = i as u64;
        if i + 1 >= self.end() {
            return None;
        }
        let a = self.d[(i - self.base) as usize];
        let b = self.d[(i + 1 - self.base) as usize];
        Some(a + (b - a) * frac)
    }
    fn correlation(&self, n: u64) -> f32 {
        let mut values = [0f32; 24];
        for (k, v) in values.iter_mut().enumerate() {
            *v = self.d[(n - ((23 - k) * SPS) as u64 - self.base) as usize];
        }
        let mean = values.iter().sum::<f32>() / 24.;
        let sync_mean = SYNC.iter().map(|s| *s as f32).sum::<f32>() / 24.;
        let (mut num, mut a, mut b) = (0., 0., 0.);
        for (v, s) in values.iter().zip(SYNC) {
            let x = v - mean;
            let y = s as f32 - sync_mean;
            num += x * y;
            a += x * x;
            b += y * y;
        }
        if a <= 1e-12 {
            0.
        } else {
            num / (a * b).sqrt()
        }
    }
    fn run(&mut self, frames: &mut Vec<Frame>) {
        loop {
            match &mut self.state {
                State::Hunting => {
                    if self.cursor < self.base + (SPS * 23) as u64 {
                        self.cursor = self.base + (SPS * 23) as u64;
                    }
                    if self.cursor >= self.end() {
                        return;
                    }
                    let n = self.cursor;
                    let c = self.correlation(n);
                    self.cursor += 1;
                    match self.pending_peak {
                        Some((at, best)) => {
                            if c.abs() > best.abs() {
                                self.pending_peak = Some((n, c));
                            } else if n >= at + SPS as u64 / 2 {
                                self.pending_peak = None;
                                self.lock(at, best);
                            }
                        }
                        None if c.abs() >= self.threshold => self.pending_peak = Some((n, c)),
                        None => {}
                    }
                }
                State::Frame(_) => {
                    if !self.advance_frame(frames) {
                        return;
                    }
                }
            }
        }
    }
    fn lock(&mut self, anchor: u64, correlation: f32) {
        // Least squares d = gain * sync + offset.
        let values: Vec<f32> = (0..24)
            .map(|k| self.d[(anchor - ((23 - k) * SPS) as u64 - self.base) as usize])
            .collect();
        let n = 24.;
        let sx: f32 = SYNC.iter().map(|s| *s as f32).sum();
        let sy: f32 = values.iter().sum();
        let sxx: f32 = SYNC.iter().map(|s| (*s as f32).powi(2)).sum();
        let sxy: f32 = SYNC.iter().zip(&values).map(|(s, v)| *s as f32 * v).sum();
        let gain = (n * sxy - sx * sy) / (n * sxx - sx * sx);
        let offset = (sy - gain * sx) / n;
        if gain.abs() < 0.05 {
            return;
        }
        let mut dibits = Vec::with_capacity(900);
        let mut soft = Vec::with_capacity(900);
        for s in SYNC {
            dibits.push(match s {
                3 => 1,
                _ => 3,
            });
            soft.push(s as f32);
        }
        self.state = State::Frame(FrameRx {
            anchor,
            mu: 0.,
            gain,
            offset,
            correlation,
            dibits,
            soft,
            duid: None,
            target: 57,
            blocks: Vec::new(),
        });
    }
    /// Slice symbols for the current frame. Returns false when more input is needed.
    fn advance_frame(&mut self, frames: &mut Vec<Frame>) -> bool {
        let State::Frame(f) = &mut self.state else {
            return true;
        };
        while f.dibits.len() < f.target {
            let i = f.dibits.len();
            let t = f.anchor as f32 + ((i as f32) - 23.) * SPS as f32 + f.mu;
            let (Some(v), Some(early), Some(late)) = (
                Self::interp(&self.d, self.base, t),
                Self::interp(&self.d, self.base, t - 1.),
                Self::interp(&self.d, self.base, t + 1.),
            ) else {
                return false;
            };
            let value = (v - f.offset) / f.gain;
            let dibit = symbol_to_dibit(value);
            let ideal = dibit_to_symbol(dibit) as f32;
            // Decision-directed tracking: move toward the eye peak, follow DC.
            let slope = ((late - early) / f.gain) * ideal.signum();
            f.mu = (f.mu + 0.04 * slope.clamp(-2., 2.)).clamp(-4., 4.);
            f.offset += 0.01 * (v - (ideal * f.gain + f.offset));
            f.dibits.push(dibit);
            f.soft.push(value);
            if f.dibits.len() == f.target {
                let more = Self::frame_step(f, self.expected_nac);
                match more {
                    Step::Continue => {}
                    Step::Done | Step::Abort => {
                        let ok = matches!(more, Step::Done);
                        let f = match std::mem::replace(&mut self.state, State::Hunting) {
                            State::Frame(f) => f,
                            State::Hunting => unreachable!(),
                        };
                        if ok {
                            let frame = Self::finish(f);
                            self.last_offset_rad = frame_offset(&frame, self.last_offset_rad);
                            self.expected_nac.get_or_insert(frame.nac);
                            self.cursor = frame.sample
                                + (SPS * (frame_len_dibits(&frame) - 23 + 23)) as u64
                                - (SPS * 2) as u64;
                            self.frames += 1;
                            frames.push(frame);
                        } else {
                            self.nid_failures += 1;
                        }
                        return true;
                    }
                }
            }
        }
        true
    }
    fn interp(d: &VecDeque<f32>, base: u64, index: f32) -> Option<f32> {
        if index < base as f32 {
            return None;
        }
        let i = index.floor();
        let frac = index - i;
        let i = i as u64;
        if i + 1 >= base + d.len() as u64 {
            return None;
        }
        let a = d[(i - base) as usize];
        let b = d[(i + 1 - base) as usize];
        Some(a + (b - a) * frac)
    }
    fn frame_step(f: &mut FrameRx, expected_nac: Option<u16>) -> Step {
        let data = data_bits(&f.dibits);
        if f.duid.is_none() {
            // 48 sync bits then 64 NID bits.
            let nid = data[48..112].iter().fold(0u64, |a, b| a << 1 | *b as u64);
            let Some((nac, duid, errors)) = nid_decode(nid, 9) else {
                return Step::Abort;
            };
            // Tolerate fewer errors when the NAC disagrees with the established one.
            if expected_nac.is_some_and(|n| n != nac) && errors > 4 {
                return Step::Abort;
            }
            let duid = Duid::from(duid);
            f.duid = Some((nac, duid, errors));
            f.target = match duid.fixed_dibits() {
                Some(n) => n,
                None => match duid {
                    Duid::Tsdu | Duid::Pdu => total_dibits(112 + 196),
                    _ => return Step::Abort,
                },
            };
            return Step::Continue;
        }
        let (_, duid, _) = f.duid.unwrap();
        match duid {
            Duid::Tsdu | Duid::Pdu => {
                let index = f.blocks.len();
                let start = 112 + 196 * index;
                let Some((block, errors)) = trellis_half_decode(&data[start..start + 196]) else {
                    return Step::Done;
                };
                let crc = crc_ccitt_ok(&block);
                f.blocks.push((block, errors, crc));
                let more = match duid {
                    Duid::Tsdu => crc && block[0] & 0x80 == 0 && f.blocks.len() < 3,
                    _ => {
                        // PDU header: blocks to follow, bits 49..55 (SDRTrunk PDUHeader).
                        let follow = (f.blocks[0].0[6] & 0x7f) as usize;
                        f.blocks[0].2 && f.blocks.len() <= follow.min(4)
                    }
                };
                if more {
                    f.target = total_dibits(112 + 196 * (f.blocks.len() + 1));
                    Step::Continue
                } else {
                    Step::Done
                }
            }
            _ => Step::Done,
        }
    }
    fn finish(f: FrameRx) -> Frame {
        let (nac, duid, nid_errors) = f.duid.unwrap();
        let data = data_bits(&f.dibits);
        let rms = (f
            .soft
            .iter()
            .skip(24)
            .map(|v| {
                let ideal = dibit_to_symbol(symbol_to_dibit(*v)) as f32;
                (v - ideal).powi(2)
            })
            .sum::<f32>()
            / (f.soft.len().saturating_sub(24)).max(1) as f32)
            .sqrt();
        Frame {
            nac,
            duid,
            nid_errors,
            bits: data[112..].to_vec(),
            blocks: f.blocks,
            sample: f.anchor,
            sync_correlation: f.correlation,
            symbol_error_rms: rms,
        }
        .with_offset(f.offset / f.gain.abs() * f.gain.signum())
    }
}

enum Step {
    Continue,
    Done,
    Abort,
}

trait WithOffset {
    fn with_offset(self, offset: f32) -> Self;
}
impl WithOffset for Frame {
    fn with_offset(mut self, offset: f32) -> Self {
        // Stash the DC (radians/symbol, normalized by gain sign) in the error field's
        // companion; tracked by Receiver::last_offset_rad via frame_offset().
        self.blocks.shrink_to_fit();
        OFFSETS.with(|o| o.set(offset));
        self
    }
}
thread_local! {
    static OFFSETS: std::cell::Cell<f32> = const { std::cell::Cell::new(0.) };
}
fn frame_offset(_frame: &Frame, _previous: f32) -> f32 {
    OFFSETS.with(|o| o.get())
}

fn frame_len_dibits(frame: &Frame) -> usize {
    total_dibits(112 + frame.bits.len())
}

/// Dibits on air (including status symbols) needed to carry `bits` frame bits.
fn total_dibits(bits: usize) -> usize {
    let data = bits / 2;
    // A status dibit follows every 35 data dibits.
    data + data / 35
}

/// Remove status symbols (every 36th dibit) and expand to bits.
fn data_bits(dibits: &[u8]) -> Vec<u8> {
    let mut bits = Vec::with_capacity(dibits.len() * 2);
    for (i, d) in dibits.iter().enumerate() {
        if (i + 1) % 36 == 0 {
            continue;
        }
        bits.push(d >> 1 & 1);
        bits.push(d & 1);
    }
    bits
}

/// Convenience: complex IQ at any rate -> frames (front end + receiver).
pub struct Channel {
    pub frontend: Frontend,
    pub receiver: Receiver,
}
impl Channel {
    pub fn new(input_rate: f64, offset_hz: f64) -> Self {
        Self {
            frontend: Frontend::new(input_rate, offset_hz),
            receiver: Receiver::new(),
        }
    }
    pub fn push(&mut self, iq: &[Complex32]) -> Vec<Frame> {
        let baseband = self.frontend.push(iq);
        let frames = self.receiver.push(&baseband);
        if !frames.is_empty() {
            // DC in radians per symbol -> Hz: f = φ * Rs / 2π. Positive DC means the
            // carrier sits above the commanded frequency.
            let hz = self.receiver.last_offset_rad as f64 * SYMBOL_RATE
                / (2. * std::f64::consts::PI);
            let _ = hz;
        }
        frames
    }
}
