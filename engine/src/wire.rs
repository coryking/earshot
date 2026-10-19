use std::io::{self, Read};
pub const MAX_PAYLOAD: usize = 1024 * 1024;
pub const START: u16 = 1;
pub const END: u16 = 2;
pub const PREROLL: u16 = 4;
pub const DISCONTINUITY: u16 = 8;
pub const OVERFLOW: u16 = 16;
pub const PINNED: u16 = 32;
#[derive(Debug, Clone)]
pub struct Frame {
    pub kind: u16,
    pub flags: u16,
    pub generation: u64,
    pub stream_id: u32,
    pub sequence: u64,
    pub first_sample: u64,
    pub rate: u32,
    pub center_hz: i64,
    pub format: u16,
    pub valid_bits: u16,
    pub payload: Vec<u8>,
}
impl Frame {
    pub fn write(&self, mut output: impl std::io::Write) -> io::Result<()> {
        if self.payload.len() > MAX_PAYLOAD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "payload exceeds 1 MiB",
            ));
        }
        let mut b = [0u8; 64];
        b[..4].copy_from_slice(b"RIQ1");
        b[4..6].copy_from_slice(&1u16.to_le_bytes());
        b[6..8].copy_from_slice(&64u16.to_le_bytes());
        b[8..10].copy_from_slice(&self.kind.to_le_bytes());
        b[10..12].copy_from_slice(&self.flags.to_le_bytes());
        b[12..20].copy_from_slice(&self.generation.to_le_bytes());
        b[20..24].copy_from_slice(&self.stream_id.to_le_bytes());
        b[24..32].copy_from_slice(&self.sequence.to_le_bytes());
        b[32..40].copy_from_slice(&self.first_sample.to_le_bytes());
        b[40..44].copy_from_slice(&self.rate.to_le_bytes());
        b[44..52].copy_from_slice(&self.center_hz.to_le_bytes());
        b[52..56].copy_from_slice(&(self.payload.len() as u32).to_le_bytes());
        b[56..58].copy_from_slice(&self.format.to_le_bytes());
        b[58..60].copy_from_slice(&self.valid_bits.to_le_bytes());
        output.write_all(&b)?;
        output.write_all(&self.payload)
    }
    pub fn read(mut input: impl Read) -> io::Result<Self> {
        let mut b = [0u8; 64];
        input.read_exact(&mut b)?;
        let u16at = |p| u16::from_le_bytes(b[p..p + 2].try_into().unwrap());
        let u32at = |p| u32::from_le_bytes(b[p..p + 4].try_into().unwrap());
        let u64at = |p| u64::from_le_bytes(b[p..p + 8].try_into().unwrap());
        if &b[..4] != b"RIQ1" || u16at(4) != 1 || u16at(6) != 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported RIQ header",
            ));
        }
        let size = u32at(52) as usize;
        if size > MAX_PAYLOAD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "RIQ payload exceeds 1 MiB",
            ));
        }
        let mut frame = Self {
            kind: u16at(8),
            flags: u16at(10),
            generation: u64at(12),
            stream_id: u32at(20),
            sequence: u64at(24),
            first_sample: u64at(32),
            rate: u32at(40),
            center_hz: i64::from_le_bytes(b[44..52].try_into().unwrap()),
            format: u16at(56),
            valid_bits: u16at(58),
            payload: vec![0; size],
        };
        if !(1..=6).contains(&frame.kind) || u32at(60) != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid RIQ type/reserved field",
            ));
        }
        if frame.kind == 1
            && (frame.format != 1
                || !matches!(frame.valid_bits, 12 | 16)
                || frame.rate == 0
                || !size.is_multiple_of(4))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid CI16 frame",
            ));
        }
        input.read_exact(&mut frame.payload)?;
        Ok(frame)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_huge_before_allocating() {
        let mut b = [0u8; 64];
        b[..4].copy_from_slice(b"RIQ1");
        b[4..6].copy_from_slice(&1u16.to_le_bytes());
        b[6..8].copy_from_slice(&64u16.to_le_bytes());
        b[52..56].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Frame::read(&b[..]).is_err());
    }
}
