use crate::voice::{VoiceConfig, VoicePost};
use std::{
    collections::VecDeque,
    ffi::{c_char, c_int, c_void},
};
/// DSDcc's AMBE+2 hook (patches/dsdcc-ambe-vocoder-hook.patch): called once per voice frame
/// with the 49 error-corrected data bits, or with null bits when a new call starts.
type AmbeHookFn = unsafe extern "C" fn(*mut c_void, c_int, *const c_char, c_int, c_int, *mut f32);
unsafe extern "C" {
    fn radio_codec_new(protocol: i32, slot: i32, vocoder: Option<AmbeHookFn>, context: *mut c_void) -> *mut c_void;
    fn radio_codec_free(p: *mut c_void);
    fn radio_codec_push2(
        p: *mut c_void,
        input: *const i16,
        n: usize,
        out1: *mut i16,
        cap1: usize,
        out2: *mut i16,
        cap2: usize,
        n2: *mut usize,
        log: *mut c_char,
        log_capacity: usize,
    ) -> usize;
}
/// Which AMBE+2 voice synthesizer turns DSDcc's error-corrected 49 data bits into audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vocoder {
    /// The engine's native port of the MD-380 firmware decoder (src/ambe), the default.
    Ambe,
    /// mbelib (ISC), kept as a fallback.
    Mbelib,
}
impl Vocoder {
    pub fn name(self) -> &'static str {
        match self {
            Vocoder::Ambe => "ambe",
            Vocoder::Mbelib => "mbelib",
        }
    }
}
impl std::str::FromStr for Vocoder {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "ambe" => Ok(Vocoder::Ambe),
            "mbelib" => Ok(Vocoder::Mbelib),
            other => Err(format!("unknown vocoder {other:?}; use ambe or mbelib")),
        }
    }
}
/// Everything a decoder instance needs beyond its protocol.
#[derive(Clone, Debug)]
pub struct DecoderOptions {
    pub vocoder: Vocoder,
    pub voice: VoiceConfig,
}
impl Default for DecoderOptions {
    fn default() -> Self {
        Self {
            vocoder: Vocoder::Ambe,
            voice: VoiceConfig::default(),
        }
    }
}
/// Native AMBE+2 decoders for the two TDMA streams of one DSDcc instance.
struct AmbeHook {
    slots: [crate::ambe::Decoder; 2],
}
/// The native decoder's int16 output sits 9 to 11 dB below mbelib's float scale (int16/7) on
/// speech (three real DMR recordings: 10.9, 9.7 and 8.7 dB), so 1/7 * 10^(10/20) keeps either
/// synthesizer at the level DSDcc's 8 kHz path and the post-processing expect.
const AMBE_TO_MBELIB_FLOAT: f32 = 0.45;
/// mbelib's concealment threshold: more corrected bit errors than this marks the frame bad,
/// and the decoder repeats (then mutes) as the radio does, via its frame-flag input.
const BAD_FRAME_ERRORS: c_int = 3;
unsafe extern "C" fn ambe_hook(context: *mut c_void, stream: c_int, bits: *const c_char, _errs: c_int, errs2: c_int, pcm: *mut f32) {
    // SAFETY: `context` is the AmbeHook boxed by the owning `Decoder`, which outlives its DSDcc
    // instance and only calls into DSDcc from its own thread; DSDcc passes 49 bits and room
    // for 160 samples, or null bits for a reset.
    let hook = unsafe { &mut *context.cast::<AmbeHook>() };
    let slot = &mut hook.slots[usize::from(stream == 2)];
    if bits.is_null() {
        slot.reset();
        return;
    }
    let (bits, pcm) = unsafe { (std::slice::from_raw_parts(bits.cast::<u8>(), 49), std::slice::from_raw_parts_mut(pcm, 160)) };
    let mut frame = [0u8; 49];
    for (d, &b) in frame.iter_mut().zip(bits) {
        *d = b & 1;
    }
    let flags = if errs2 > BAD_FRAME_ERRORS { crate::ambe::FLAG_BAD_FRAME } else { 0 };
    // A panic must not unwind into C++: answer the frame with silence and start the slot over.
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| slot.decode_flags(&frame, flags))) {
        Ok(out) => {
            for (p, &o) in pcm.iter_mut().zip(&out) {
                *p = o as f32 * AMBE_TO_MBELIB_FLOAT;
            }
        }
        Err(_) => {
            pcm.fill(0.);
            slot.reset();
        }
    }
}
/// What one decoder instance heard in a block of discriminator audio.
pub struct Decoded {
    /// 8 kHz PCM for TDMA slot 1 (NXDN has only this one).
    pub slot1: Vec<i16>,
    /// 8 kHz PCM for TDMA slot 2 (DMR only).
    pub slot2: Vec<i16>,
    /// The channel carries confirmed voice (rolling gate); both slots share it.
    pub confirmed: bool,
    /// Any DMR voice/data sync or NXDN frame sync was logged in this block. The analog
    /// FM path stays muted while digital signalling is present.
    pub sync: bool,
}
pub struct Decoder {
    ptr: *mut c_void,
    protocol: String,
    events: VecDeque<(f64, String)>,
    tail: String,
    time: f64,
    confirmed: bool,
    vocoder: Vocoder,
    /// Native decoder state the DSDcc hook points at (None with mbelib). `Drop` frees the DSDcc
    /// instance before this field is dropped.
    _ambe: Option<Box<AmbeHook>>,
    /// Post-processing per output slot, and seconds each slot has been without audio.
    post: [VoicePost; 2],
    idle: [f64; 2],
}
// Each instance belongs to exactly one bounded stream worker.
unsafe impl Send for Decoder {}
impl Decoder {
    pub fn new(protocol: &str) -> anyhow::Result<Self> {
        Self::with_options(protocol, &DecoderOptions::default())
    }
    pub fn with_options(protocol: &str, options: &DecoderOptions) -> anyhow::Result<Self> {
        anyhow::ensure!(matches!(protocol, "DMR" | "NXDN48"), "unsupported protocol");
        let mut ambe = (options.vocoder == Vocoder::Ambe).then(|| {
            Box::new(AmbeHook {
                slots: [crate::ambe::Decoder::new(), crate::ambe::Decoder::new()],
            })
        });
        let (hook, context): (Option<AmbeHookFn>, *mut c_void) = match ambe.as_mut() {
            Some(h) => (Some(ambe_hook), (&mut **h as *mut AmbeHook).cast()),
            None => (None, std::ptr::null_mut()),
        };
        let ptr = unsafe { radio_codec_new(if protocol == "DMR" { 1 } else { 2 }, 1, hook, context) };
        anyhow::ensure!(!ptr.is_null(), "cannot initialize DSDcc");
        Ok(Self {
            vocoder: options.vocoder,
            _ambe: ambe,
            post: [VoicePost::new(options.voice.clone()), VoicePost::new(options.voice.clone())],
            idle: [0.; 2],
            ptr,
            protocol: protocol.into(),
            events: VecDeque::new(),
            tail: String::new(),
            time: 0.,
            confirmed: false,
        })
    }
    /// Feed 48 kHz discriminator audio. One DMR instance decodes both slots.
    pub fn push(&mut self, pcm: &[i16]) -> Decoded {
        let mut slot1: Vec<i16> = Vec::with_capacity(16000);
        let mut slot2: Vec<i16> = Vec::with_capacity(16000);
        let mut n2 = 0usize;
        let mut log = vec![0u8; 65536];
        let n = unsafe {
            radio_codec_push2(
                self.ptr,
                pcm.as_ptr(),
                pcm.len(),
                slot1.as_mut_ptr(),
                slot1.capacity(),
                slot2.as_mut_ptr(),
                slot2.capacity(),
                &mut n2,
                log.as_mut_ptr().cast(),
                log.len(),
            )
        };
        unsafe {
            slot1.set_len(n);
            slot2.set_len(n2);
        }
        self.time += pcm.len() as f64 / 48000.;
        let end = log.iter().position(|&b| b == 0).unwrap_or(log.len());
        self.tail.push_str(&String::from_utf8_lossy(&log[..end]));
        // Sign-prefixed sync prints (" +NXDN48"/" -NXDN48") are NXDN frame syncs, control
        // and data included; the bare token also appears in the start-up log line.
        let mut sync = self.protocol == "NXDN48"
            && (self.tail.contains("+NXDN48") || self.tail.contains("-NXDN48"));
        for token in ["DMRv", "DMRd", "NXDN48", "MBE:", "parity error"] {
            let count = self.tail.matches(token).count();
            // DMR voice/data sync, or NXDN decoded voice frames. (The "NXDN48" token also
            // appears in the start-up log line, so it is not evidence of signalling.)
            sync |= count > 0
                && match token {
                    "DMRv" | "DMRd" => true,
                    "MBE:" => self.protocol == "NXDN48",
                    _ => false,
                };
            for _ in 0..count {
                self.events.push_back((self.time, token.into()));
            }
        }
        // C bridge emits complete log writes; retain only incomplete token suffix.
        self.tail.clear();
        self.events.retain(|(t, _)| *t >= self.time - 1.5);
        let count = |key: &str| self.events.iter().filter(|(_, k)| k == key).count();
        let active = if self.protocol == "DMR" {
            count("DMRv") >= 2
        } else {
            let frames = count("MBE:");
            let good = count("parity error") < frames / 2;
            if count("NXDN48") > 0 && frames >= 12 && good {
                self.confirmed = true;
            }
            self.confirmed && frames >= 4 && good
        };
        if !active {
            slot1.clear();
            slot2.clear();
        }
        // A slot that has been silent for 1.5 s is a new talker next time: restart the level
        // control so it does not carry the previous talker's gain.
        let seconds = pcm.len() as f64 / 48000.;
        for (i, audio) in [&mut slot1, &mut slot2].into_iter().enumerate() {
            if audio.is_empty() {
                self.idle[i] += seconds;
                if self.idle[i] > 1.5 {
                    self.post[i].reset();
                }
            } else {
                self.idle[i] = 0.;
                self.post[i].process(audio);
            }
        }
        Decoded {
            slot1,
            slot2,
            confirmed: active,
            sync,
        }
    }
}
impl Decoder {
    /// The voice synthesizer this instance runs.
    pub fn vocoder(&self) -> Vocoder {
        self.vocoder
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { radio_codec_free(self.ptr) }
    }
}
