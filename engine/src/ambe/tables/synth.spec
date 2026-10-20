# Constant tables used by the synthesis helpers (src/ambe/synth.rs).
SYNTH_FIR10 0x080f9e74 10 i16   # 0x0805ec20 filter coefficients (10 taps)
COS_TAB 0x08091b80 1800 i16     # 0x0805ed14 cosine table 32767*cos(2*pi*k/256) for k = 0..511, then other firmware data (index = hw6*i/1024 can reach 1791)
POPCNT8 0x080962c4 256 u8       # 0x0805d85c bit count of a byte
