//! The decoder's top-level routines: `ambe_decode_wav` and the frame and synthesis glue it
//! calls (0x08051248, 0x08056224, 0x08055484, 0x08055d74), plus the public [`Decoder`].

use super::frame;
use super::math::{self, Bits};
use super::ops::*;
use super::spectral;
use super::state::{PARAMS_WORDS, State, off};
use super::synth;
use super::unvoiced;
use super::voiced;

/// A 0x44-halfword parameter block (see frame.rs for the layout).
pub type Block = [i16; PARAMS_WORDS];

/// Frame-flag bit (the firmware's flags argument): the frame failed error correction. The
/// decoder repeats the previous frame's parameters, and after three bad frames in a row falls
/// back to its comfort block.
pub const FLAG_BAD_FRAME: u16 = 0x4;

/// Samples per half-frame call (the firmware clamps its count argument to 76..=84; the radio
/// and this port always use 80).
const HALF: usize = 80;

// State offsets used here (bytes from the start of the state block).
/// Previous-frame record kept by the spectral decoder (0x8c bytes).
const SPEC_PREV: usize = spectral::PREV;
/// Field-width table.
const WIDTHS: usize = frame::WIDTHS;
/// Extra flag bits OR-ed into the caller's flags.
const FLAGS_EXTRA: usize = 0x64a;
/// Tone fallback (index, level) used with flag 0x4000.
const TONE_FALLBACK: usize = 0x7a0;
const COUNTER_7C0: usize = 0x7c0;
/// Running level (0.9 * old + 0.1 * pitch step on voiced frames).
const LEVEL: usize = 0x7c2;
/// Spectral-enhancement energy memory (mantissa, exponent).
const ENHANCE_MEM: usize = 0x7c4;
/// Synthesis configuration words read by the smoothing step.
const CONFIG: usize = 0x618;
/// FIR-filtered amplitudes of the current frame (0x38 halfwords).
const AMP_FIR: usize = 0x176;
/// Half-frame length the firmware records.
const HALF_LEN: usize = 0x174;
/// Frame-rate table index (the firmware's r3 argument to ambe_decode_wav, always 0): the
/// field-width table gets 0x31 - RATE_BITS[0] = 0 extra bits.
const RATE_EXTRA_BITS: i32 = 0;

fn load(st: &State, o: usize) -> Block {
    let mut b = [0i16; PARAMS_WORDS];
    b.copy_from_slice(st.s(o, PARAMS_WORDS));
    b
}
fn store(st: &mut State, o: usize, b: &Block) {
    st.s_mut(o, PARAMS_WORDS).copy_from_slice(b);
}
fn word(p: &[i16], i: usize) -> i32 {
    (p[i] as u16 as i32) | ((p[i + 1] as i32) << 16)
}
fn set_word(p: &mut [i16], i: usize, v: i32) {
    p[i] = v as i16;
    p[i + 1] = (v >> 16) as i16;
}

/// 0x08055484: decode the frame's bits into the current block (state+0x3ec) and build the
/// block to synthesize (`out`). Returns 0 for a decoded frame, 4 when the frame is not
/// speech or tone (first six bits >= 0x3c), a tone decoder status, or 0x44 when the field
/// table is disabled.
fn decode_params(st: &mut State, out: &mut Block, bits: &mut [i16; 49], flag80: i32) -> i32 {
    if st.h(WIDTHS) == 0 {
        return PARAMS_WORDS as i32;
    }
    frame::field_widths(st.s_mut(WIDTHS, frame::WIDTHS_LEN), RATE_EXTRA_BITS);
    let mut head = 0i32;
    for &b in &bits[..6] {
        head = (b as i32).wrapping_add(head << 1);
    }
    let cur = off::PARAMS_CUR;
    let set_kind = |st: &mut State, out: &mut Block, kind: i16| {
        out[0] = kind;
        out[1] = 0xff;
        st.set_h(cur, kind);
        st.set_h(cur + 2, 0xff);
    };
    let ret = 'body: {
        if st.h(WIDTHS) == 0 {
            set_kind(st, out, 0);
            break 'body 4;
        }
        let mode = if head == 0x3f {
            set_kind(st, out, 3);
            let mut w = [0i16; frame::WIDTHS_LEN];
            w.copy_from_slice(st.s(WIDTHS, frame::WIDTHS_LEN));
            let mut blk = load(st, cur);
            let mut cursor = Bits::new(&bits[..]);
            let r = frame::tone_frame(&mut blk, out, &mut cursor, &w);
            store(st, cur, &blk);
            if r != 0 {
                break 'body r as i16 as i32;
            }
            // Tone decoded: straight to building the block to synthesize.
            let mut blend = *out;
            spectral::blend_frames(&mut blend, st);
            *out = blend;
            spectral::save_frame(st, 0);
            break 'body 0;
        } else if head == 0x3e {
            set_kind(st, out, 2);
            2
        } else {
            let voice = head <= 0x3b;
            set_kind(st, out, voice as i16);
            if !voice {
                break 'body 4;
            }
            1
        };
        let mut w = [0i16; frame::WIDTHS_LEN];
        w.copy_from_slice(st.s(WIDTHS, frame::WIDTHS_LEN));
        frame::bit_reorder(&mut bits[..], &w);
        let mut cursor = Bits::new(&bits[..]);
        let pitch = cursor.read(w[5] as usize) as i16;
        let band = cursor.read(w[4] as usize);
        let (out_bands, cur_bands) = frame::band_flags(mode, band, w[4] as i32);
        set_word(out, 4, out_bands);
        st.set_w(cur + 8, cur_bands);
        let mut blk = load(st, cur);
        if frame::pair_levels(pitch, out, &mut blk, w[5] as i32) {
            out[6] = 0x1079;
            out[2] = 0x38;
            blk[6] = 0x1079;
            blk[2] = 0x38;
        } else {
            frame::pitch_params(&mut blk, pitch as i32, w[5] as i32, flag80);
        }
        store(st, cur, &blk);
        spectral::dequant_spectrum(st, &mut cursor);
        let cfg_a = st.h(CONFIG + 0x30) as i32;
        let cfg_b = st.h(CONFIG + 0x24) as i32;
        spectral::smooth_magnitudes(st, cfg_a, cfg_b);
        spectral::blend_frames(out, st);
        spectral::save_frame(st, 0);
        0
    };
    st.set_h(SPEC_PREV + 0x88, -3);
    ret
}

/// 0x08056224: frame-level control. Decodes the frame unless the flags mark it bad, handles
/// tone frames (flag 0x4000 or the pitch-history marker) and repeats, and leaves the block to
/// synthesize in `out`. Returns the firmware's status (0, 2, 0x20 for a repeat, 0x8002 for a
/// tone or non-speech frame).
fn frame_control(st: &mut State, bits: &mut [i16; 49], flags: i32, out: &mut Block) -> i32 {
    let fl = flags | st.h(FLAGS_EXTRA) as i32;
    let hist = (st.h(off::HISTORY) as i32) << 1;
    st.set_h(off::HISTORY, hist as i16);
    let cur = off::PARAMS_CUR;
    // r9 / r7 in the firmware: the decoder reported a bad frame / some other non-zero status.
    let (mut bad, mut other) = (false, false);
    if fl & 0xc == 0 {
        let r = decode_params(st, out, bits, fl & 0x80);
        if r != 0 {
            bad = r & 8 != 0;
            other = !bad;
        }
    }
    let mut tone = st.h(cur + 2);
    let marker = frame::pitch_history(&mut tone, st.s_mut(frame::PITCH_HISTORY, frame::PITCH_HISTORY_LEN), 0);
    let tone_level = if fl & 0x4000 != 0 {
        tone = st.h(TONE_FALLBACK);
        Some(st.h(TONE_FALLBACK + 2) as i32)
    } else if marker != 0 {
        let class = frame::tone_class(tone as i32) as u8 as i32;
        let a = st.h(cur + 2 * (frame::tone_harmonic(class, tone as i32, 0) + 8) as usize) as i32;
        let class = frame::tone_class(tone as i32) as u8 as i32;
        let b = st.h(cur + 2 * (frame::tone_harmonic(class, tone as i32, 1) + 8) as usize) as i32;
        Some((((a + b) >> 1) & 0xffff) as i16 as i32)
    } else {
        None
    };
    let status = if let Some(level) = tone_level {
        let mut blk = load(st, cur);
        frame::tone_block(&mut blk, tone, level, level, 0);
        store(st, cur, &blk);
        frame::copy_params(out, &blk);
        0x8002
    } else {
        // Which block (if any) replaces this frame's parameters.
        let repeat_src = |hist: i32| if hist & 0xe == 0xe { off::PARAMS_REPEAT } else { off::PARAMS_PREV };
        let src = if fl & 8 != 0 {
            Some(off::PARAMS_REPEAT)
        } else if fl & 4 != 0 {
            Some(repeat_src(hist))
        } else if bad {
            Some(off::PARAMS_REPEAT)
        } else if other {
            Some(repeat_src(hist))
        } else {
            match st.h(cur) {
                2 => None,
                3 | 1 => None,
                _ => Some(repeat_src(hist)),
            }
        };
        match src {
            Some(src) => {
                let from = load(st, src);
                let mut blk = load(st, cur);
                frame::copy_params(&mut blk, &from);
                store(st, cur, &blk);
                frame::copy_params(out, &blk);
                st.set_h(off::HISTORY, (hist | 1) as i16);
                frame::bump_frame_counter(st.s_mut(SPEC_PREV, PARAMS_WORDS));
                return 0x20;
            }
            None => match st.h(cur) {
                2 => 0,
                3 => 0x8002,
                _ => {
                    if st.h(WIDTHS) != 0 || st.h(cur + 2) == 0xff {
                        2
                    } else {
                        0x8002
                    }
                }
            },
        }
    };
    st.set_h(COUNTER_7C0, 0);
    frame::bump_frame_counter(st.s_mut(SPEC_PREV, PARAMS_WORDS));
    status
}

/// 0x08055d74: synthesize one half frame (`out`, 80 samples) from the block `p`. `half` is 0
/// for the first half (which also updates the running level) and 1 for the second.
fn synthesize(st: &mut State, p: &mut Block, out: &mut [i16], half: i32) {
    let n = out.len();
    if st.h(off::HISTORY) & 1 == 0 {
        let l = p[2] as usize;
        if p[0] == 1 {
            let mut fir = [0i16; synth::MAX_HARMONICS];
            synth::amp_fir(&mut fir, &p[8..8 + l]);
            st.s_mut(AMP_FIR, synth::MAX_HARMONICS).copy_from_slice(&fir);
        } else {
            st.s_mut(AMP_FIR, synth::MAX_HARMONICS).fill(0);
        }
        synth::amp_expand(&mut p[..]);
        if p[0] == 1 {
            let mut mem = [st.h(ENHANCE_MEM), st.h(ENHANCE_MEM + 2)];
            synth::enhance(&mut p[..], &mut mem);
            st.set_h(ENHANCE_MEM, mem[0]);
            st.set_h(ENHANCE_MEM + 2, mem[1]);
        }
        if p[0] == 2 {
            let mut rep = load(st, off::PARAMS_REPEAT);
            frame::copy_params(&mut rep, p);
            store(st, off::PARAMS_REPEAT, &rep);
        }
    }
    let mut voicing = [0i16; synth::MAX_HARMONICS];
    synth::voicing_array(&p[..], &mut voicing);
    // (The firmware also builds the previous block's voicing array here; nothing reads it.)
    let prev = load(st, off::PARAMS_PREV);

    if p[1] != 0xff {
        tone_adjust(st, p, &mut voicing, &prev);
    } else {
        normalize_amplitudes(st, p, half);
    }

    let mut acc = [0i32; 84];
    let acc = &mut acc[..n];
    let level = st.h(LEVEL);
    unvoiced::unvoiced(acc, st, &p[..], &voicing, level);
    voiced::synth_voiced(acc, n as i32, st, &p[..], &voicing, &prev, level);
    synth::post_filter(acc, st);
    synth::write_output(out, acc, 0xf, 0x11);
    p[0x40] = 0;
    p[0x41] = 0;
    let mut prev_out = prev;
    frame::copy_params(&mut prev_out, p);
    store(st, off::PARAMS_PREV, &prev_out);
}

/// Tone frames (block halfword 1 != 0xff), part of 0x08055d74.
fn tone_adjust(st: &mut State, p: &mut Block, voicing: &mut [i16], prev: &Block) {
    let code = p[1] as i32;
    if st.h(WIDTHS) == 1 {
        if frame::tone_class(code) != 0 || prev[0] != 3 || frame::tone_class(prev[1] as i32) != 0 {
            return;
        }
        let pk = prev[3] as i32;
        let near = (p[3] as i32 - pk).abs() <= 1;
        let new = if near && code < (pk + 1) << 4 {
            pk
        } else {
            let q = math::div_int(code * (pk + 1) + (prev[1] as i32) / 2, prev[1] as i32);
            let mut r = q & 0xffff;
            let mut t = (r as i16 as i32) << 4;
            if t <= code {
                loop {
                    t += 0x10;
                    r = (r + 1) & 0xffff;
                    if code < t {
                        break;
                    }
                }
            }
            (r - 1) as i16 as i32
        };
        synth::move_peak(&mut p[..], voicing, new);
        return;
    }
    let class = frame::tone_class(code) as u8 as i32;
    let h0 = frame::tone_harmonic(class, code, 0);
    let h1 = frame::tone_harmonic(class, code, 1);
    for i in 0..p[2].max(0) as i32 {
        if i != h0 && i != h1 {
            let v = &mut p[8 + i as usize];
            *v = (smulbb(*v as i32, 0x51e) >> 15) as i16;
        }
    }
    let code16 = p[1] as u16 as i32;
    if ((code16 - 0x80) & 0xffff) <= 0x1f {
        let (a, b) = (8 + h1 as usize, 8 + h0 as usize);
        let e = smlabb(p[b] as i32, p[b] as i32, smulbb(p[a] as i32, p[a] as i32));
        let r = math::sqrt_scaled(e, 8, 4) as i16;
        p[a] = r;
        p[b] = r;
    }
    let code16 = p[1] as u16 as i32;
    if ((code16 - 0x14) & 0xffff) <= 2 {
        let den = (((p[3] as i32 + 1) << 8) as i16) as i32;
        p[6] = math::div_half(code16 << 20, den) as i16;
    }
}

/// Speech frames, part of 0x08055d74: scale the amplitudes above the level-dependent
/// harmonic cut-off and bring the block to one common exponent.
fn normalize_amplitudes(st: &mut State, p: &mut Block, half: i32) {
    let level = if half == 0 {
        let v = synth::level_update(st.h(LEVEL) as i32, p[6] as i32, word(&p[..], 4) as u32) as i16;
        st.set_h(LEVEL, v);
        v as i32
    } else {
        st.h(LEVEL) as i32
    };
    let limit = level.min(0x3333);
    let x = limit << 16;
    let s = norm32(x) as i16 as i32;
    let (m, e) = math::div_norm(0x4000_0000, 1, lsl(x, s) >> 16, -4 - s);
    let step = p[6] as i32;
    if step < limit {
        let m = m & 0xffff;
        let k = e - 4;
        let inc = step << 16;
        let mut idx = step;
        let mut x = p[6] as u16 as i32;
        let mut i = 8;
        while idx < limit && i < 0x40 {
            let mut g = smulbb(x, m) << 1;
            let s = norm32(g) as i16 as i32;
            g = lsl(g, s);
            g = smultt(g, g) << 1;
            let d = k - s;
            g = if d >= 0 { lsl(g, 2 * d) } else { asr(g, -2 * d) };
            p[i] = (smultb(g, p[i] as i32) >> 15) as i16;
            i += 1;
            let r = qadd(idx << 16, inc);
            x = lsr(r, 16);
            idx = x as i16 as i32;
        }
    }
    let l = p[2] as i32;
    let nl = l.max(0) as usize;
    let mut amp = [0i16; synth::MAX_HARMONICS];
    let mut exp = [0i16; synth::MAX_HARMONICS];
    if l > 0 {
        amp[..nl].copy_from_slice(&p[8..8 + nl]);
        exp[..nl].fill(p[0x42]);
    }
    let mut thr = ((step.wrapping_mul(l)) >> 3) & 0xffff;
    if thr as i16 as i32 > 0x4ccd {
        let e0 = (p[0x42] as u16 as i32 + 1) & 0xffff;
        let dec = step << 13;
        let mut k = nl;
        while thr as i16 as i32 > 0x4ccd && k > 0 {
            let g = (smulbb(thr, 0x6aab) >> 15) & 0xffff;
            let v = smulbb(g, amp[k - 1] as i32) << 1;
            let s = norm32(v) as i16 as i32;
            let next = lsr(((thr as i16 as i32) << 16).wrapping_sub(dec), 16);
            amp[k - 1] = (lsl(v, s) >> 16) as i16;
            exp[k - 1] = (e0 - s) as i16;
            thr = next;
            k -= 1;
        }
    }
    let mut max = exp[0] as i32;
    for &e in &exp[1..nl.max(1)] {
        max = max.max(e as i32);
    }
    if l > 0 {
        for j in 0..nl {
            let v = (amp[j] as i32) << 16;
            let d = exp[j] as i32 - max;
            p[8 + j] = if d >= 0 { lsl(v, d) >> 16 } else { asr(v, max - exp[j] as i32) >> 16 } as i16;
        }
    }
    set_word(&mut p[..], 0x42, max);
}

/// 0x08051248 `ambe_decode_wav`: one half-frame call. `half` 0 decodes `bits` (which the
/// firmware reorders in place) and synthesizes samples 0-79; `half` 1 synthesizes 80-159 from
/// the block saved by the first call. Returns the frame status for half 0, else 0.
fn decode_wav(st: &mut State, out: &mut [i16], bits: &mut [i16; 49], flags: i16, half: i16) -> u16 {
    st.set_h(HALF_LEN, 0x50);
    if half != 0 {
        let mut p = load(st, off::PARAMS_CUR);
        synthesize(st, &mut p, out, half as i32);
        store(st, off::PARAMS_CUR, &p);
        return 0;
    }
    let mut p = [0i16; PARAMS_WORDS];
    let status = frame_control(st, bits, flags as i32, &mut p);
    synthesize(st, &mut p, out, 0);
    status as u16
}

/// Native AMBE+2 (3600x2450) decoder for one voice stream.
#[derive(Clone, Default)]
pub struct Decoder {
    st: State,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }
    /// Forget the previous call (the state the firmware starts from).
    pub fn reset(&mut self) {
        self.st = State::default();
    }
    /// Decode one 20 ms frame: 49 error-corrected data bits (one per byte, 0/1, mbelib
    /// `ambe_d` order) to 160 samples of 8 kHz audio.
    pub fn decode(&mut self, bits: &[u8; 49]) -> [i16; 160] {
        self.decode_flags(bits, 0)
    }
    /// As [`decode`](Self::decode), also passing the firmware's frame flags (e.g.
    /// [`FLAG_BAD_FRAME`]).
    pub fn decode_flags(&mut self, bits: &[u8; 49], flags: u16) -> [i16; 160] {
        let mut b = [0i16; 49];
        for (d, &s) in b.iter_mut().zip(bits) {
            *d = (s & 1) as i16;
        }
        let mut pcm = [0i16; 160];
        let (first, second) = pcm.split_at_mut(HALF);
        decode_wav(&mut self.st, first, &mut b, flags as i16, 0);
        decode_wav(&mut self.st, second, &mut b, 0, 1);
        pcm
    }
}
