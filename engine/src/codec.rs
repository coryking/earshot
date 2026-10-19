use std::{
    collections::VecDeque,
    ffi::{c_char, c_void},
};
unsafe extern "C" {
    fn radio_codec_new(protocol: i32, slot: i32) -> *mut c_void;
    fn radio_codec_free(p: *mut c_void);
    fn radio_codec_push(
        p: *mut c_void,
        input: *const i16,
        n: usize,
        out: *mut i16,
        capacity: usize,
        log: *mut c_char,
        log_capacity: usize,
    ) -> usize;
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
    pub fn new(protocol: &str, slot: u8) -> anyhow::Result<Self> {
        anyhow::ensure!(
            matches!(protocol, "DMR" | "NXDN48") && matches!(slot, 1 | 2),
            "unsupported protocol/slot"
        );
        let ptr = unsafe { radio_codec_new(if protocol == "DMR" { 1 } else { 2 }, slot.into()) };
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
    pub fn push(&mut self, pcm: &[i16]) -> (Vec<i16>, bool) {
        let mut output = Vec::with_capacity(16000);
        let mut log = vec![0u8; 65536];
        let n = unsafe {
            radio_codec_push(
                self.ptr,
                pcm.as_ptr(),
                pcm.len(),
                output.as_mut_ptr(),
                output.capacity(),
                log.as_mut_ptr().cast(),
                log.len(),
            )
        };
        unsafe { output.set_len(n) };
        self.time += pcm.len() as f64 / 48000.;
        let end = log.iter().position(|&b| b == 0).unwrap_or(log.len());
        self.tail.push_str(&String::from_utf8_lossy(&log[..end]));
        for token in ["DMRv", "DMRd", "NXDN48", "MBE:", "parity error"] {
            let count = self.tail.matches(token).count();
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
            output.clear();
        }
        (output, active)
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { radio_codec_free(self.ptr) }
    }
}
