# Voiced synthesis (0x0805df08 .. 0x0805e584)
COS512 0x08091b80 514 i16   # cos(2*pi*i/512) in Q15 for i = 0..511 (period table of 0x0805e010 and the fade window of 0x0805df08); the two entries past the end (512, 513) are whatever follows in flash, which 0x0805df08 can read at the very end of a fade
