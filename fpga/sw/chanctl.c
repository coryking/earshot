// chanctl: bring-up and test tool for the Pluto FPGA channelizer.
//
// Talks to the channelizer registers and its two axi_dmac instances through
// /dev/mem. The stock firmware's device tree does not describe them, so no kernel
// driver is bound and nothing else touches these addresses. The receiver is not
// opened or configured here (no IIO, no transmit); the AD9363 keeps whatever
// settings the caller applied.
//
// DMA without a driver: `dma` captures one transfer into the no-map carve-out that
// the channelizer firmware image reserves (see dma_capture()).
//
// Build: fpga/sw/build.sh (static ARMv7 musl, so the uClibc rootfs does not matter).
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
#include <signal.h>
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
// DMA into the reserved carve-out
//
// HP-port writes bypass the CPU caches, and user-space cache maintenance on ARMv7
// cannot invalidate L1 or the PL310 L2, so DMA targets must be memory Linux never
// maps cacheable. The channelizer firmware image (make_frm.py) reserves
// DMA_BASE..+DMA_SIZE as no-map; /dev/mem with O_SYNC maps it uncached. On stock
// firmware that range is ordinary System RAM and this command refuses to run.

#define DMA_CONTROL 0x100
#define DMA_TRANSFER_ID 0x101
#define DMA_SUBMIT 0x102
#define DMA_FLAGS 0x103
#define DMA_DEST 0x104
#define DMA_XLEN 0x106
#define DMA_YLEN 0x107
#define DMA_DONE 0x10A
#define DMA_BASE 0x04000000u
#define DMA_SIZE 0x04000000u

static void dma_stop_all(void) {
    if (hist_dma) hist_dma[DMA_CONTROL] = 0;
    if (sel_dma) sel_dma[DMA_CONTROL] = 0;
}

static void on_signal(int sig) {
    dma_stop_all();
    _exit(128 + sig);
}

static int region_is_free(uint32_t base, uint32_t size) {
    // the carve-out must not be part of Linux's System RAM (it is no-map reserved)
    FILE *f = fopen("/proc/iomem", "r");
    if (!f) return 0;
    char line[256];
    int ok = 1;
    while (fgets(line, sizeof line, f)) {
        unsigned long lo, hi;
        if (line[0] == ' ') continue;
        if (sscanf(line, "%lx-%lx", &lo, &hi) == 2 && strstr(line, "System RAM") &&
            lo < (unsigned long)base + size && hi >= base) ok = 0;
    }
    fclose(f);
    return ok;
}

// One non-cyclic transfer of `bytes` (multiple of 8, <= 16 MiB) into the carve-out
// (selected stream at DMA_BASE, history at DMA_BASE + 32 MiB), then copy it out.
static int dma_capture(int fd, volatile uint32_t *dma, uint32_t offset, uint32_t bytes,
                       const char *out) {
    if (bytes == 0 || bytes > (16u << 20) || (bytes & 7)) { fprintf(stderr, "bad size\n"); return 2; }
    if (!region_is_free(DMA_BASE, DMA_SIZE)) {
        fprintf(stderr, "0x%08x is System RAM here: needs the channelizer firmware's reserved "
                "region\n", DMA_BASE);
        return 1;
    }
    volatile uint8_t *buf = mmap(NULL, bytes, PROT_READ | PROT_WRITE, MAP_SHARED, fd, DMA_BASE + offset);
    if (buf == MAP_FAILED) { perror("mmap carve-out"); return 1; }
    signal(SIGINT, on_signal);
    signal(SIGTERM, on_signal);
    signal(SIGHUP, on_signal);
    for (uint32_t i = 0; i < bytes; i += 4) *(volatile uint32_t *)(buf + i) = 0xEEEEEEEEu;
    dma[DMA_CONTROL] = 0;
    dma[DMA_CONTROL] = 1;
    dma[DMA_FLAGS] = 0;
    dma[DMA_DEST] = DMA_BASE + offset;
    dma[DMA_XLEN] = bytes - 1;
    dma[DMA_YLEN] = 0;
    uint32_t id = dma[DMA_TRANSFER_ID];
    dma[DMA_SUBMIT] = 1;
    double t0 = now();
    int done = 0;
    while (now() - t0 < 10.0) {
        if ((dma[DMA_DONE] >> (id & 3)) & 1) { done = 1; break; }
        usleep(1000);
    }
    dma[DMA_CONTROL] = 0;
    uint8_t *copy = malloc(bytes);
    if (!copy) return 1;
    for (uint32_t i = 0; i < bytes; i += 4) *(uint32_t *)(copy + i) = *(volatile uint32_t *)(buf + i);
    FILE *f = fopen(out, "wb");
    if (!f) { perror(out); return 1; }
    int ok = fwrite(copy, 1, bytes, f) == bytes;
    ok &= fclose(f) == 0;
    printf("{\"bytes\":%u,\"completed\":%s,\"seconds\":%.3f}\n", bytes, done ? "true" : "false",
           now() - t0);
    return (done && ok) ? 0 : 1;
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
    if (!buf) { wr(chan, 0x22, 0); return 1; }
    buf[0] = frame;
    for (int i = 0; i < 4096; ++i) {
        uint32_t v = rd(chan, 0x1000 + i);       // {Q[15:0] sign-ext, I[15:0] sign-ext}
        buf[1 + i] = v;
    }
    for (int k = 0; k < 512; ++k) buf[1 + 4096 + k] = rd(chan, 0x600 + k);
    wr(chan, 0x22, 0);
    FILE *f = fopen(out, "wb");
    if (!f) { perror(out); return 1; }
    int ok = fwrite(buf, 4, 1 + 4096 + 512, f) == 1 + 4096 + 512;
    ok &= fclose(f) == 0;
    printf("{\"snapshot_frame\":%u,\"out_shift\":%u}\n", frame, rd(chan, 6) & 0xF);
    return ok ? 0 : 1;
}

static void usage(void) {
    fprintf(stderr,
        "usage: chanctl info | psd | level | active | stop\n"
        "       chanctl start [--out-shift N] [--hist] [--sel] [--auto] [--bins a,b,...]\n"
        "       chanctl set-det ON OFF BIAS HANG | det-reset\n"
        "       chanctl dma hist|sel BYTES OUTFILE | snap OUTFILE\n"
        "Accesses the channelizer at 0x%08x via /dev/mem. Never opens the radio.\n", CHAN_BASE);
}

int main(int argc, char **argv) {
    if (argc < 2) { usage(); return 2; }
    // Only touch 0x7C4x0000 when the channelizer image is loaded: on the stock PL
    // those addresses are not decoded and an access can stall the bus.
    if (access("/tmp/pl_is_chan", F_OK) != 0) {
        fprintf(stderr, "channelizer PL not loaded (no runtime-load marker)\n");
        return 1;
    }
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
    else if (!strcmp(cmd, "stop")) { dma_stop_all(); wr(chan, 4, 0); wr(chan, 0x22, 0); }
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
        int hist = !strcmp(argv[2], "hist");
        return dma_capture(fd, hist ? hist_dma : sel_dma, hist ? (32u << 20) : 0,
                           (uint32_t)strtoul(argv[3], NULL, 0), argv[4]);
    } else { usage(); return 2; }
    return 0;
}
