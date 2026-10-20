# FFT tables (0x08057b40 .. 0x08057e34)
TWIDDLE        0x08090f80  1028  i16   # 257 entries of 4 halfwords: [re, im, -re, im] of exp(-2*pi*i*e/512); the code reads [re, im] only
BITREV_128     0x08095bac  113   i16   # swap list for the 128-point bit reversal (0x080578c0): count, then 56 (delta a, delta b) halfword offsets
BITREV_32      0x08095c90  25    i16   # same for 32 points: count, then 12 pairs
HEADROOM_SHIFT 0x080fd4a8  7     i8    # first-stage scaling by headroom + 1 (0x08057b40)
