# Tables for frame.rs (frame-level parameter decode).
PITCH_PATTERNS   0x0809ac34 0x2c u8   # 0x0805573c: two 17-entry pitch-index patterns (offsets 0 and 0x14), then four bytes (0x28..0x2b) forming a bit mask word
TONE_INDEX       0x080966a0 72  u8   # 0x080556fc: per tone index (0x80..0xa3) two harmonic indices + 1
TONE_FLAGS       0x080966e8 36  u16  # 0x08055808: per tone index (0x80..0xa3), stored in the block at halfword 6
TONE_LEVEL       0x08096658 36  i16  # 0x08055808: per tone index (0x80..0xa3), level offset used with the tone's two amplitudes
POPCOUNT8        0x080962c4 256 u8   # 0x0805d618: bit count of a byte
SPREAD_BITS      0x08094d74 128 u32  # 0x0805d0ec: 7-bit index -> 16+16 bits that 0x08060f4c spreads into two 32-bit words
