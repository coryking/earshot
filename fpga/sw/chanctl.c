// chanctl: bring-up and test tool for the Pluto FPGA channelizer.
//
// Talks to the channelizer registers and its two axi_dmac instances through
// /dev/mem. The stock firmware's device tree does not describe them, so no kernel
// driver is bound and nothing else touches these addresses. The receiver is not
// opened or configured here (no IIO, no transmit); the AD9363 keeps whatever
// settings the caller applied.
//
// DMA without a driver: `dma` takes N pinned user pages, translates them through
// /proc/self/pagemap, and queues one 4 KiB axi_dmac transfer per page. HP-port
// writes bypass the CPU caches, so the pages are cleaned/invalidated with the ARM
// cacheflush syscall before submission and again before they are read.
//
// Build (static, so the uClibc root filesystem does not matter):
//   arm-linux-gnueabihf-gcc -O2 -static -Wall -Wextra -o chanctl chanctl.c
// Exit codes: 0 ok, 1 runtime failure, 2 usage.

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#define CHAN_BASE 0x7C480000u
#define HIST_DMA_BASE 0x7C460000u
#define SEL_DMA_BASE 0x7C470000u
#define MAP_BYTES 0x10000u

static volatile uint32_t *chan, *hist_dma, *sel_dma;

static volatile uint32_t *map_phys(int fd, uint32_t base) {
    void *p = mmap(NULL, MAP_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, base);
    if (p == MAP_FAILED) { perror("mmap /dev/mem"); exit(1); }
    return (volatile uint32_t *)p;
}

static uint32_t rd(volatile uint32_t *b, uint32_t dword) { return b[dword]; }
static void wr(volatile uint32_t *b, uint32_t dword, uint32_t v) { b[dword] = v; }
static uint64_t rd64(uint32_t lo) { uint32_t l = rd(chan, lo); return ((uint64_t)rd(chan, lo + 1) << 32) | l; }

static double now(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec + t.tv_nsec * 1e-9; }

static void info(void) {
    printf("{\"version\":\"0x%08x\",\"magic\":\"0x%08x\",\"config\":\"0x%08x\",\"det_config\":\"0x%08x\","
           "\"control\":%u,\"running\":%u,\"out_shift\":%u,\"det_on\":%u,\"det_off\":%u,\"det_bias\":%u,"
           "\"det_hang\":%u,\"samples\":%" PRIu64 ",\"frames\":%" PRIu64 ",\"adc_overflows\":%u,"
           "\"adc_clips\":%u,\"out_saturations\":%u,\"hist_frames\":%u,\"hist_drops\":%u,"
           "\"sel_frames\":%u,\"sel_drops\":%u,\"pack_overruns\":%u,\"blocks\":%u,\"psd_periods\":%u,"
           "\"hist_first_frame\":%" PRIu64 ",\"fft_events\":%u,\"fir_frames\":%u}\n",
           rd(chan, 0), rd(chan, 3), rd(chan, 7), rd(chan, 0xC), rd(chan, 4), rd(chan, 5) & 1,
           rd(chan, 6), rd(chan, 8), rd(chan, 9), rd(chan, 0xA), rd(chan, 0xB), rd64(0x10),
           rd64(0x12), rd(chan, 0x14), rd(chan, 0x15), rd(chan, 0x16), rd(chan, 0x17),
           rd(chan, 0x18), rd(chan, 0x19), rd(chan, 0x1A), rd(chan, 0x1B), rd(chan, 0x1C),
           rd(chan, 0x1D), rd64(0x1E), rd(chan, 0x20), rd(chan, 0x21));
}

static void dump_window(uint32_t base, int is_level) {
    printf("[");
    for (int k = 0; k < 512; ++k) {
        uint32_t v = rd(chan, base + k);
        if (is_level) printf("%s[%u,%u]", k ? "," : "", v & 0xFFFF, v >> 16);
        else printf("%s%u", k ? "," : "", v & 0xFFFF);
    }
    printf("]\n");
}

static void active(void) {
    printf("[");
    int first = 1;
    for (int k = 0; k < 512; ++k)
        if (rd(chan, 0x50 + k / 32) >> (k % 32) & 1) { printf("%s%d", first ? "" : ",", k); first = 0; }
    printf("]\n");
}

// ---------------------------------------------------------------------------
// DMA test through pinned user pages

#define DMA_CONTROL 0x100
#define DMA_TRANSFER_ID 0x101
#define DMA_SUBMIT 0x102
#define DMA_FLAGS 0x103
#define DMA_DEST 0x104
#define DMA_XLEN 0x106
#define DMA_YLEN 0x107
#define DMA_DONE 0x10A

static uint64_t virt_to_phys(void *v) {
    int fd = open("/proc/self/pagemap", O_RDONLY);
    if (fd < 0) { perror("pagemap"); exit(1); }
    uint64_t e;
    off_t off = ((uintptr_t)v / 4096) * 8;
    if (pread(fd, &e, 8, off) != 8) { perror("pagemap read"); exit(1); }
    close(fd);
    if (!(e >> 63)) { fprintf(stderr, "page not present\n"); exit(1); }
    return (e & ((1ULL << 55) - 1)) * 4096;
}

static void cache_flush(void *p, size_t n) {
    // ARM private syscall: clean and invalidate the D-cache over [p, p + n)
    syscall(0xf0002, p, (char *)p + n, 0);
}

static int dma_test(volatile uint32_t *dma, int pages, const char *out) {
    uint8_t *buf = aligned_alloc(4096, (size_t)pages * 4096);
    if (!buf) return 1;
    memset(buf, 0xEE, (size_t)pages * 4096);
    if (mlock(buf, (size_t)pages * 4096)) { perror("mlock"); return 1; }
    uint64_t *phys = calloc(pages, sizeof(uint64_t));
    for (int i = 0; i < pages; ++i) {
        phys[i] = virt_to_phys(buf + (size_t)i * 4096);
        if (phys[i] >= 0x20000000ULL) { fprintf(stderr, "page above HP window\n"); return 1; }
    }
    cache_flush(buf, (size_t)pages * 4096);

    wr(dma, DMA_CONTROL, 0);
    wr(dma, DMA_CONTROL, 1);              // enable
    wr(dma, DMA_FLAGS, 0);                // not cyclic, no TLAST handling
    // At most two transfers in flight: transfer IDs are 2 bits and an ID's DONE
    // bit is cleared when that ID is resubmitted, so each one is retired (seen
    // done) well before its ID comes round again.
    int submitted = 0, done = 0;
    uint32_t id_of[4096];
    if (pages > 4096) pages = 4096;
    double t0 = now();
    while (done < pages && now() - t0 < 10.0) {
        while (done < submitted && (rd(dma, DMA_DONE) >> (id_of[done] & 3) & 1)) done++;
        if (submitted < pages && submitted - done < 2 && (rd(dma, DMA_SUBMIT) & 1) == 0) {
            wr(dma, DMA_DEST, (uint32_t)phys[submitted]);
            wr(dma, DMA_XLEN, 4096 - 1);
            wr(dma, DMA_YLEN, 0);
            id_of[submitted] = rd(dma, DMA_TRANSFER_ID);
            wr(dma, DMA_SUBMIT, 1);
            submitted++;
        }
    }
    wr(dma, DMA_CONTROL, 0);
    cache_flush(buf, (size_t)pages * 4096);
    FILE *f = fopen(out, "wb");
    if (!f) { perror(out); return 1; }
    fwrite(buf, 4096, done, f);
    fclose(f);
    printf("{\"pages\":%d,\"submitted\":%d,\"completed\":%d,\"seconds\":%.3f}\n", pages, submitted,
           done, now() - t0);
    return done == pages ? 0 : 1;
}

// Live self-test: freeze at the next frame boundary, then read that frame's exact
// 4096-sample input window and its 512 output bins. Output file: u32 frame number,
// 4096 x {I16,Q16} input, 512 x {I16,Q16} bins (little-endian).
static int snapshot(const char *out) {
    wr(chan, 0x22, 1);
    double t0 = now();
    while ((rd(chan, 0x22) & 6) != 6) {
        if (now() - t0 > 2.0) { fprintf(stderr, "snapshot not ready (0x%x)\n", rd(chan, 0x22)); wr(chan, 0x22, 0); return 1; }
        usleep(1000);
    }
    uint32_t frame = rd(chan, 0x23);
    uint32_t *buf = malloc((1 + 4096 + 512) * 4);
    buf[0] = frame;
    for (int i = 0; i < 4096; ++i) {
        uint32_t v = rd(chan, 0x1000 + i);       // {Q[15:0] sign-ext, I[15:0] sign-ext}
        buf[1 + i] = v;
    }
    for (int k = 0; k < 512; ++k) buf[1 + 4096 + k] = rd(chan, 0x600 + k);
    wr(chan, 0x22, 0);
    FILE *f = fopen(out, "wb");
    if (!f) { perror(out); return 1; }
    fwrite(buf, 4, 1 + 4096 + 512, f);
    fclose(f);
    printf("{\"snapshot_frame\":%u}\n", frame);
    return 0;
}

static void usage(void) {
    fprintf(stderr,
        "usage: chanctl info | psd | level | active | stop\n"
        "       chanctl start [--out-shift N] [--hist] [--sel] [--auto] [--bins a,b,...]\n"
        "       chanctl set-det ON OFF BIAS HANG | det-reset\n"
        "       chanctl dma hist|sel PAGES OUTFILE | snap OUTFILE\n"
        "Accesses the channelizer at 0x%08x via /dev/mem. Never opens the radio.\n", CHAN_BASE);
}

int main(int argc, char **argv) {
    if (argc < 2) { usage(); return 2; }
    int fd = open("/dev/mem", O_RDWR | O_SYNC);
    if (fd < 0) { perror("/dev/mem"); return 1; }
    chan = map_phys(fd, CHAN_BASE);
    hist_dma = map_phys(fd, HIST_DMA_BASE);
    sel_dma = map_phys(fd, SEL_DMA_BASE);
    if (rd(chan, 3) != 0x4348414E) { fprintf(stderr, "channelizer not present (magic 0x%08x)\n", rd(chan, 3)); return 1; }

    const char *cmd = argv[1];
    if (!strcmp(cmd, "info")) info();
    else if (!strcmp(cmd, "psd")) dump_window(0x400, 0);
    else if (!strcmp(cmd, "level")) dump_window(0x200, 1);
    else if (!strcmp(cmd, "active")) active();
    else if (!strcmp(cmd, "stop")) wr(chan, 4, 0);
    else if (!strcmp(cmd, "det-reset")) wr(chan, 4, rd(chan, 4) | 0x10);
    else if (!strcmp(cmd, "set-det") && argc == 6) {
        wr(chan, 8, atoi(argv[2])); wr(chan, 9, atoi(argv[3]));
        wr(chan, 0xA, atoi(argv[4])); wr(chan, 0xB, atoi(argv[5]));
    } else if (!strcmp(cmd, "start")) {
        uint32_t ctl = 1, mask[16] = {0};
        for (int i = 2; i < argc; ++i) {
            if (!strcmp(argv[i], "--hist")) ctl |= 2;
            else if (!strcmp(argv[i], "--sel")) ctl |= 4;
            else if (!strcmp(argv[i], "--auto")) ctl |= 8;
            else if (!strcmp(argv[i], "--out-shift") && i + 1 < argc) wr(chan, 6, atoi(argv[++i]));
            else if (!strcmp(argv[i], "--bins") && i + 1 < argc) {
                char *s = argv[++i], *end;
                while (*s) {
                    long b = strtol(s, &end, 10);
                    if (end == s || b < 0 || b > 511) { usage(); return 2; }
                    mask[b / 32] |= 1u << (b % 32);
                    s = *end == ',' ? end + 1 : end;
                }
            } else { usage(); return 2; }
        }
        for (int w = 0; w < 16; ++w) wr(chan, 0x40 + w, mask[w]);
        wr(chan, 4, 0);
        usleep(1000);
        wr(chan, 4, ctl);
    } else if (!strcmp(cmd, "snap") && argc == 3) {
        return snapshot(argv[2]);
    } else if (!strcmp(cmd, "dma") && argc == 5) {
        volatile uint32_t *dma = !strcmp(argv[2], "hist") ? hist_dma : sel_dma;
        return dma_test(dma, atoi(argv[3]), argv[4]);
    } else { usage(); return 2; }
    return 0;
}
