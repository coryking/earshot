#ifndef PLUTO_PFB_H
#define PLUTO_PFB_H
#include <stdint.h>
#include <stddef.h>
typedef struct { float re, im; } iqf;
typedef struct {
    unsigned n, taps, hop, length, write;
    uint64_t seen, frames;
    iqf *history, *fft, *roots;
    float *coeff;
    unsigned *reverse;
} pfb;
int pfb_init(pfb *p, unsigned n, unsigned taps);
void pfb_destroy(pfb *p);
/* Supply exactly hop signed I,Q pairs. Returns 1 with a new complete frame.
 * The complex output retains phase; bins are not magnitude-only FFT pixels.
 * Before filter history is full, returns 0. */
int pfb_push(pfb *p, const int16_t *samples);
uint64_t pfb_center_times_two(const pfb *p);
#endif
