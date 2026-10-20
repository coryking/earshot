use std::{
    collections::VecDeque,
    ffi::{c_char, c_void},
};
unsafe extern "C" {
    fn radio_codec_new(protocol: i32, slot: i32) -> *mut c_void;
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
}
// Each instance belongs to exactly one bounded stream worker.
unsafe impl Send for Decoder {}
impl Decoder {
    pub fn new(protocol: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(matches!(protocol, "DMR" | "NXDN48"), "unsupported protocol");
        let ptr = unsafe { radio_codec_new(if protocol == "DMR" { 1 } else { 2 }, 1) };
        anyhow::ensure!(!ptr.is_null(), "cannot initialize DSDcc");
        Ok(Self {
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
        Decoded {
            slot1,
            slot2,
            confirmed: active,
            sync,
        }
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { radio_codec_free(self.ptr) }
    }
}
