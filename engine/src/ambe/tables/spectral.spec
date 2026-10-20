# Spectral-magnitude dequantisation tables (src/ambe/spectral.rs).
GAIN      0x0809984c   32 i16   # gain codebook (0x08061340), widths[6] = 5 bits
CNT       0x08090ea4   48 i16   # per harmonic count L = 9..56: four 4-bit block lengths minus 2 (0x0805c7f8)
PRBA24    0x080902a4 1536 i16   # 9-bit, 3-vector codebook (PRBA terms 1..3)
PRBA58    0x0808fea4  512 i16   # 7-bit, 4-vector codebook (PRBA terms 4..7)
HOC0      0x0808fda4  128 i16   # block 0 HOC codebook: 5 bits x 4
HOC1      0x0808fd24   64 i16   # block 1 HOC codebook: 4 bits x 4
HOC2      0x0808fca4   64 i16   # block 2 HOC codebook: 4 bits x 4
HOC3      0x0808fc64   32 i16   # block 3 HOC codebook: 3 bits x 4
DCT_STEP  0x080979c0   24 i16   # 32768 / n phase step of the n-point inverse DCT (0x080612ac), n <= 17
COS       0x08091b80  512 i16   # cosine table, 9-bit phase (0x080612ac)
