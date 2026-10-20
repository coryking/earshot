// chanctl: bring-up and test tool for the Pluto FPGA channelizer.
//
// Talks to the channelizer registers and its two axi_dmac instances through
// /dev/mem. The stock firmware's device tree does not describe them, so no kernel
// driver is bound and nothing else touches these addresses. The receiver is not
// opened or configured here (no IIO, no transmit); the AD9363 keeps whatever
// settings the caller applied.
//
// DMA without a driver: `dma` captures one transfer into the no-map carve-out that
// the channelizer firmware image reserves (see dma_capture()). `ring` runs the
// history stream as a cyclic DDR ring in the rest of the carve-out, `events` drains
// the activity event FIFO, and `preroll` waits for a bin to open and extracts that
// bin's history from before the opening (design.md section 7, PROTOCOL.md).
//
// Build: fpga/sw/build.sh (static ARMv7 musl, so the uClibc rootfs does not matter).
// Exit codes: 0 ok, 1 runtime failure, 2 usage.

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <dirent.h>
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
#define DMA_FLAG_CYCLIC 1u
// The carve-out comes from the device tree (make_frm.py's reserved-memory node), so
// this tool follows whatever size the flashed image reserves. The first SEL_BYTES
// are for selected-stream captures; the history ring takes the rest.
#define SEL_BYTES (8u << 20)
static uint32_t dma_base, dma_size;

static int carveout(void) {
    const char *dir = "/proc/device-tree/reserved-memory";
    DIR *d = opendir(dir);
    if (!d) return 0;
    struct dirent *e;
    char path[320] = {0};
    while ((e = readdir(d)))
        if (!strncmp(e->d_name, "chan_dma@", 9)) {
            snprintf(path, sizeof path, "%s/%s/reg", dir, e->d_name);
            break;
        }
    closedir(d);
    FILE *f = path[0] ? fopen(path, "rb") : NULL;
    if (!f) return 0;
    uint8_t b[8];
    int ok = fread(b, 1, 8, f) == 8;
    fclose(f);
    if (!ok) return 0;
    dma_base = (uint32_t)b[0] << 24 | b[1] << 16 | b[2] << 8 | b[3];
    dma_size = (uint32_t)b[4] << 24 | b[5] << 16 | b[6] << 8 | b[7];
    return dma_size > SEL_BYTES;
}

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
// (selected stream at its base, history at base + SEL_BYTES), then copy it out.
static int dma_capture(int fd, volatile uint32_t *dma, uint32_t offset, uint32_t bytes,
                       const char *out) {
    if (!carveout() || !region_is_free(dma_base, dma_size)) {
        fprintf(stderr, "no reserved chan_dma region: needs the channelizer firmware image\n");
        return 1;
    }
    if (bytes == 0 || bytes > dma_size - offset || bytes > (16u << 20) || (bytes & 7)) {
        fprintf(stderr, "bad size\n");
        return 2;
    }
    volatile uint8_t *buf = mmap(NULL, bytes, PROT_READ | PROT_WRITE, MAP_SHARED, fd, dma_base + offset);
    if (buf == MAP_FAILED) { perror("mmap carve-out"); return 1; }
    signal(SIGINT, on_signal);
    signal(SIGTERM, on_signal);
    signal(SIGHUP, on_signal);
    for (uint32_t i = 0; i < bytes; i += 4) *(volatile uint32_t *)(buf + i) = 0xEEEEEEEEu;
    dma[DMA_CONTROL] = 0;
    dma[DMA_CONTROL] = 1;
    dma[DMA_FLAGS] = 0;
    dma[DMA_DEST] = dma_base + offset;
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

// ---------------------------------------------------------------------------
// History ring (cyclic axi_dmac over the carve-out above SEL_BYTES)
//
// Registers (DWORD): HIST_WINDOW 0x028 {pairs[24:16], first[8:0]}, HIST_RECORDS
// 0x029 (reading latches HIST_LAST_FRAME 0x02A), HIST_DONE 0x02B (records handed to
// the DMA since arming), HIST_WINDOW_ACTIVE 0x02C. Record r (1-based, r <= DONE)
// sits at slot (r - 1) mod N, N = ring bytes / record bytes; its header holds its
// frame number. Records older than DONE - N + RING_GUARD may be mid-overwrite.

#define RING_GUARD 16u
#define RING_STATE "/tmp/chan_ring"

struct ring { uint32_t base, rec_bytes, n, first, pairs; volatile uint8_t *mem; };

static int ring_load(int fd, struct ring *r) {
    FILE *f = fopen(RING_STATE, "r");
    if (!f) { fprintf(stderr, "ring not started (no %s)\n", RING_STATE); return 0; }
    int ok = fscanf(f, "%" SCNu32 " %" SCNu32 " %" SCNu32 " %" SCNu32 " %" SCNu32,
                    &r->base, &r->rec_bytes, &r->n, &r->first, &r->pairs) == 5;
    fclose(f);
    if (!ok || !r->n || !r->rec_bytes) { fprintf(stderr, "bad %s\n", RING_STATE); return 0; }
    void *p = mmap(NULL, r->n * r->rec_bytes, PROT_READ, MAP_SHARED, fd, r->base);
    if (p == MAP_FAILED) { perror("mmap ring"); return 0; }
    r->mem = p;
    return 1;
}

static uint64_t rec_header(const struct ring *r, uint32_t rec) {
    volatile const uint32_t *w = (volatile const uint32_t *)(r->mem + ((rec - 1) % r->n) * r->rec_bytes);
    uint32_t lo = w[0], hi = w[1];
    return (uint64_t)hi << 32 | lo;
}

// Bin `bin`'s complex sample in record `rec`, or -1 if the bin is outside the window.
static int rec_sample(const struct ring *r, uint32_t rec, uint32_t bin, uint32_t *iq) {
    uint32_t pos = (bin - r->first) & 511;
    if (pos >= 2 * r->pairs) return -1;
    volatile const uint32_t *w = (volatile const uint32_t *)(r->mem + ((rec - 1) % r->n) * r->rec_bytes);
    *iq = w[2 + pos];          // header is words 0-1; window bin p at 32-bit word 2 + p
    return 0;
}

static int ring_start(uint32_t first, uint32_t pairs) {
    if (!carveout() || !region_is_free(dma_base, dma_size)) {
        fprintf(stderr, "no reserved chan_dma region: needs the channelizer firmware image\n");
        return 1;
    }
    if (pairs == 0 || pairs > 256 || first > 511) { fprintf(stderr, "bad window\n"); return 2; }
    uint32_t rec_bytes = 8 * (pairs + 1), ring_bytes = dma_size - SEL_BYTES;
    uint32_t n = ring_bytes / rec_bytes;
    // disarm, let the history FIFO drain, then restart the DMA and re-arm
    wr(chan, 4, rd(chan, 4) & ~2u);
    double t0 = now();
    while (!(rd(chan, 5) & 2)) {
        if (now() - t0 > 1.0) { fprintf(stderr, "history FIFO did not drain\n"); return 1; }
        usleep(1000);
    }
    hist_dma[DMA_CONTROL] = 0;
    hist_dma[DMA_CONTROL] = 1;
    hist_dma[DMA_FLAGS] = DMA_FLAG_CYCLIC;
    hist_dma[DMA_DEST] = dma_base + SEL_BYTES;
    hist_dma[DMA_XLEN] = n * rec_bytes - 1;   // whole records, so none straddles the wrap
    hist_dma[DMA_YLEN] = 0;
    hist_dma[DMA_SUBMIT] = 1;
    wr(chan, 0x28, pairs << 16 | first);
    wr(chan, 4, rd(chan, 4) | 2u);
    FILE *f = fopen(RING_STATE, "w");
    if (!f) { perror(RING_STATE); return 1; }
    fprintf(f, "%u %u %u %u %u\n", dma_base + SEL_BYTES, rec_bytes, n, first, pairs);
    fclose(f);
    printf("{\"ring_base\":\"0x%08x\",\"record_bytes\":%u,\"records\":%u,\"seconds\":%.3f,"
           "\"first_bin\":%u,\"bins\":%u}\n", dma_base + SEL_BYTES, rec_bytes, n, n / 100000.0,
           first, 2 * pairs);
    return 0;
}

static void ring_status(void) {
    uint32_t recs = rd(chan, 0x29), last = rd(chan, 0x2A), done = rd(chan, 0x2B), win = rd(chan, 0x2C);
    printf("{\"hist_en\":%u,\"records\":%u,\"last_frame\":%u,\"done\":%u,\"first_frame\":%" PRIu64
           ",\"drops\":%u,\"first_bin\":%u,\"pairs\":%u,\"fifo_empty\":%u}\n",
           (rd(chan, 4) >> 1) & 1, recs, last, done, rd64(0x1E), rd(chan, 0x18), win & 0x1FF,
           (win >> 16) & 0x1FF, (rd(chan, 5) >> 1) & 1);
}

// Write bin `bin`'s samples for frames [f0, f1] from the ring as ci16 pairs, oldest
// first. Frames missing from the ring (dropped, overwritten, not yet written) are
// skipped and counted. Returns the number written, or -1.
static long ring_extract(const struct ring *r, uint32_t bin, uint64_t f0, uint64_t f1, FILE *out,
                         uint64_t *first_out, uint32_t *gaps) {
    uint32_t done = rd(chan, 0x2B);
    if (!done) return 0;
    uint32_t oldest = done > r->n - RING_GUARD ? done - (r->n - RING_GUARD) : 1;
    // newest record and its frame; without drops frame(rec) = frame(done) - (done - rec)
    uint64_t fd_ = rec_header(r, done) & 0xFFFFFFFFFFFFull;
    long written = 0;
    *gaps = 0;
    *first_out = 0;
    uint32_t rec = 0;
    if (f0 > fd_) return 0;
    if (fd_ - f0 <= done - oldest) rec = done - (uint32_t)(fd_ - f0);
    else rec = oldest;
    // step back over drops: headers must not be newer than f0
    while (rec > oldest && (rec_header(r, rec) & 0xFFFFFFFFFFFFull) > f0) --rec;
    uint64_t expect = f0;
    for (; rec <= done; ++rec) {
        uint64_t h = rec_header(r, rec);
        uint64_t f = h & 0xFFFFFFFFFFFFull;
        if ((h >> 56) != 0xA1) { (*gaps)++; continue; }
        if (f < f0) continue;
        if (f > f1) break;
        uint32_t iq;
        if (rec_sample(r, rec, bin, &iq) < 0) return -1;
        // re-read the header: the DMA may have overwritten the record meanwhile
        if ((rec_header(r, rec) & 0xFFFFFFFFFFFFull) != f) { (*gaps)++; continue; }
        if (f != expect) *gaps += (uint32_t)(f - expect);
        if (!written) *first_out = f;
        fwrite(&iq, 4, 1, out);
        written++;
        expect = f + 1;
    }
    return written;
}

static int events(double wait_s) {
    double t0 = now();
    int n = 0;
    do {
        uint32_t st;
        while ((st = rd(chan, 0x25)) >> 31) {
            uint32_t fr = rd(chan, 0x26);
            wr(chan, 0x27, 1);
            printf("{\"open\":%u,\"bin\":%u,\"frame\":%u}\n", (st >> 30) & 1, st & 0x1FF, fr);
            n++;
        }
        fflush(stdout);
        if (wait_s > 0) usleep(10000);
    } while (now() - t0 < wait_s);
    uint32_t st = rd(chan, 0x24);
    fprintf(stderr, "{\"events\":%d,\"lost_total\":%u}\n", n, st >> 16);
    return 0;
}

// Wait for bin `bin` (or any bin, -1) to open, then write `pre_ms` of that bin's
// history before the opening frame plus `post_ms` after it.
static int preroll(int fd, int bin, double pre_ms, double post_ms, double timeout_s, const char *out) {
    struct ring r;
    if (!ring_load(fd, &r)) return 1;
    double t0 = now();
    uint32_t ev_bin = 0, ev_frame = 0;
    int found = 0;
    while (!found && now() - t0 < timeout_s) {
        uint32_t st;
        while ((st = rd(chan, 0x25)) >> 31) {
            uint32_t fr = rd(chan, 0x26);
            wr(chan, 0x27, 1);
            if (((st >> 30) & 1) && (bin < 0 || (st & 0x1FF) == (uint32_t)bin)) {
                ev_bin = st & 0x1FF; ev_frame = fr; found = 1; break;
            }
        }
        if (!found) usleep(5000);
    }
    if (!found) { fprintf(stderr, "no open event within %.1f s\n", timeout_s); return 1; }
    // event frames are 32-bit; extend them with the newest drained record's frame
    uint32_t done = rd(chan, 0x2B);
    if (!done) { fprintf(stderr, "history ring is empty (HIST_EN off?)\n"); return 1; }
    uint64_t newest = rec_header(&r, done) & 0xFFFFFFFFFFFFull;
    uint64_t evf = (newest & ~0xFFFFFFFFull) | ev_frame;
    if (evf > newest + (1u << 20)) evf -= 1ull << 32;
    uint64_t pre = (uint64_t)(pre_ms * 100.0), post = (uint64_t)(post_ms * 100.0);
    uint64_t f0 = evf > pre ? evf - pre : 0, f1 = evf + post;
    double t1 = now();
    while ((rec_header(&r, rd(chan, 0x2B)) & 0xFFFFFFFFFFFFull) < f1 && now() - t1 < post_ms / 1000 + 1)
        usleep(2000);
    FILE *f = fopen(out, "wb");
    if (!f) { perror(out); return 1; }
    uint64_t first = 0;
    uint32_t gaps = 0;
    long nw = ring_extract(&r, ev_bin, f0, f1, f, &first, &gaps);
    int ok = fclose(f) == 0 && nw > 0;
    printf("{\"bin\":%u,\"open_frame\":%" PRIu64 ",\"first_frame\":%" PRIu64 ",\"samples\":%ld,"
           "\"gaps\":%u,\"preroll_samples\":%" PRId64 ",\"latency_s\":%.3f}\n", ev_bin, evf, first, nw,
           gaps, (int64_t)evf - (int64_t)first, now() - t0);
    return ok ? 0 : 1;
}

static void usage(void) {
    fprintf(stderr,
        "usage: chanctl info | psd | level | active | stop\n"
        "       chanctl start [--out-shift N] [--hist] [--sel] [--auto] [--bins a,b,...]\n"
        "       chanctl set-det ON OFF BIAS HANG | det-reset\n"
        "       chanctl dma hist|sel BYTES OUTFILE | snap OUTFILE\n"
        "       chanctl ring start [--first B] [--pairs P] | ring status\n"
        "       chanctl ring read BIN FRAME0 FRAME1 OUTFILE\n"
        "       chanctl events [SECONDS] | preroll BIN|-1 PRE_MS POST_MS TIMEOUT_S OUTFILE\n"
        "Accesses the channelizer at 0x%08x via /dev/mem. Never opens the radio.\n", CHAN_BASE);
}

int main(int argc, char **argv) {
    if (argc < 2) { usage(); return 2; }
    // Only touch 0x7C4x0000 when the channelizer image is loaded: on the stock PL
    // those addresses are not decoded and an access can stall the bus.
    if (access("/tmp/pl_is_chan", F_OK) != 0) {
        fprintf(stderr, "channelizer PL not loaded (no /tmp/pl_is_chan); see pluto_runtime_load.sh\n");
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
        return dma_capture(fd, hist ? hist_dma : sel_dma, hist ? SEL_BYTES : 0,
                           (uint32_t)strtoul(argv[3], NULL, 0), argv[4]);
    } else if (!strcmp(cmd, "ring") && argc >= 3 && !strcmp(argv[2], "start")) {
        uint32_t first = 0, pairs = 256;
        for (int i = 3; i + 1 < argc; i += 2) {
            if (!strcmp(argv[i], "--first")) first = strtoul(argv[i + 1], NULL, 0);
            else if (!strcmp(argv[i], "--pairs")) pairs = strtoul(argv[i + 1], NULL, 0);
            else { usage(); return 2; }
        }
        signal(SIGINT, on_signal);
        return ring_start(first, pairs);
    } else if (!strcmp(cmd, "ring") && argc == 3 && !strcmp(argv[2], "status")) {
        ring_status();
    } else if (!strcmp(cmd, "ring") && argc == 7 && !strcmp(argv[2], "read")) {
        struct ring r;
        if (!ring_load(fd, &r)) return 1;
        FILE *f = fopen(argv[6], "wb");
        if (!f) { perror(argv[6]); return 1; }
        uint64_t first = 0;
        uint32_t gaps = 0;
        long n = ring_extract(&r, strtoul(argv[3], NULL, 0), strtoull(argv[4], NULL, 0),
                              strtoull(argv[5], NULL, 0), f, &first, &gaps);
        int ok = fclose(f) == 0 && n >= 0;
        printf("{\"samples\":%ld,\"first_frame\":%" PRIu64 ",\"gaps\":%u}\n", n, first, gaps);
        return ok ? 0 : 1;
    } else if (!strcmp(cmd, "events") && argc <= 3) {
        return events(argc == 3 ? atof(argv[2]) : 0);
    } else if (!strcmp(cmd, "preroll") && argc == 7) {
        return preroll(fd, atoi(argv[2]), atof(argv[3]), atof(argv[4]), atof(argv[5]), argv[6]);
    } else { usage(); return 2; }
    return 0;
}
