// pluto-fpga-source: RIQ1 producer for the FPGA channelizer (offload/PROTOCOL.md).
//
// Runs on the Pluto with the channelizer bitstream loaded and the channelizer
// firmware's DMA carve-out present. It configures the AD9363 receive side through
// sysfs (RX only, saved and restored), runs the channelizer's selected-bin stream
// into the carve-out with chan_sel_dma, parses the 64-bit records and forwards each
// selected bin as an RIQ1 IQ stream (stream ID = FFT bin + 1, 100 ksps at
// 25.6 Msps). PSD and activity come from the channelizer's registers.
//
//   pluto-fpga-source [--port 9010] [--center 460000000] [--rate 25600000]
//                     [--bandwidth 20000000] [--gain 40] [--forward-streams 1,201]
//                     [--seconds 0]
// Waits for one TCP client, then opens RX. Exit: 0 ok, 1 failure, 2 usage.
// Never opens a transmit path.

#define _GNU_SOURCE
#define JSMN_PARENT_LINKS
#define JSMN_STATIC
#include "../../offload/arm/vendor/jsmn.h"
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <getopt.h>
#include <inttypes.h>
#include <math.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/uio.h>
#include <time.h>
#include <unistd.h>

#define BINS 512
#define HOP 256
#define CHAN_BASE 0x7C480000u
#define SEL_DMA_BASE 0x7C470000u
#define DMA_BASE 0x04000000u          // no-map carve-out from fpga/sw/make_frm.py
#define BLOCK_BYTES 65536u
#define NBLOCKS 64u                   // 4 MiB ring of transfer blocks
#define IQ_CHUNK 1000                 // samples per RIQ1 IQ message (10 ms at 100 ksps)
#define MAX_CONTROL 4096

static volatile sig_atomic_t interrupted;
static void on_signal(int v) { (void)v; interrupted = 1; }
static double now(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec + t.tv_nsec * 1e-9; }
static void put16(unsigned char *b, uint16_t v) { b[0] = v; b[1] = v >> 8; }
static void put32(unsigned char *b, uint32_t v) { for (int j = 0; j < 4; ++j) b[j] = v >> (8 * j); }
static void put64(unsigned char *b, uint64_t v) { for (int j = 0; j < 8; ++j) b[j] = v >> (8 * j); }
static uint32_t get32(const unsigned char *b) { uint32_t v = 0; for (int j = 0; j < 4; ++j) v |= (uint32_t)b[j] << (8 * j); return v; }

// ---------------------------------------------------------------------------
// AD9363 receive controls through sysfs (RX only)

static char phy[256];
static const char *ATTRS[] = {"in_voltage0_gain_control_mode", "in_voltage_sampling_frequency",
                              "in_voltage_rf_bandwidth", "out_altvoltage0_RX_LO_frequency",
                              "in_voltage0_hardwaregain"};
#define NATTR 5
static char saved[NATTR][64];

static int find_phy(void) {
    for (int i = 0; i < 16; ++i) {
        char p[128], name[64] = {0};
        snprintf(p, sizeof p, "/sys/bus/iio/devices/iio:device%d/name", i);
        FILE *f = fopen(p, "r");
        if (!f) continue;
        if (fgets(name, sizeof name, f) && !strncmp(name, "ad9361-phy", 10)) {
            fclose(f);
            snprintf(phy, sizeof phy, "/sys/bus/iio/devices/iio:device%d", i);
            return 0;
        }
        fclose(f);
    }
    return -1;
}
static int attr_read(const char *a, char *out, size_t n) {
    char p[400]; snprintf(p, sizeof p, "%s/%s", phy, a);
    FILE *f = fopen(p, "r"); if (!f) return -1;
    if (!fgets(out, n, f)) { fclose(f); return -1; }
    fclose(f); out[strcspn(out, "\n")] = 0;
    char *sp = strchr(out, ' '); if (sp) *sp = 0;          // "40.000000 dB" -> "40.000000"
    return 0;
}
static int attr_write(const char *a, const char *v) {
    char p[400]; snprintf(p, sizeof p, "%s/%s", phy, a);
    FILE *f = fopen(p, "w"); if (!f) return -1;
    int ok = fputs(v, f) >= 0; ok &= fclose(f) == 0;
    return ok ? 0 : -1;
}
static int save_controls(void) {
    for (int i = 0; i < NATTR; ++i) if (attr_read(ATTRS[i], saved[i], sizeof saved[i])) return -1;
    return 0;
}
static int restore_controls(void) {
    int bad = 0;
    // gain mode first, then rates, LO, and the manual gain only if it was manual
    for (int i = 0; i < NATTR; ++i) {
        if (i == 4 && strcmp(saved[0], "manual")) continue;
        if (attr_write(ATTRS[i], saved[i])) bad = 1;
    }
    char now_v[64];
    for (int i = 1; i < 4; ++i) if (attr_read(ATTRS[i], now_v, sizeof now_v) || strcmp(now_v, saved[i])) bad = 1;
    if (attr_read(ATTRS[0], now_v, sizeof now_v) || strcmp(now_v, saved[0])) bad = 1;
    return bad ? -1 : 0;
}

// ---------------------------------------------------------------------------
// channelizer + DMA

static volatile uint32_t *chan, *dma;
static volatile uint8_t *ring;
static uint32_t mask[16];
// A reserved-memory DT node proves only the DMA address is safe, not which PL
// image is active. The runtime loader creates the marker after programming.
static int chan_pl_present(void) {
    return !access("/tmp/pl_is_chan", F_OK);
}
static int map_hw(void) {
    if (!chan_pl_present()) { fprintf(stderr, "channelizer PL not loaded\n"); return -1; }
    FILE *f = fopen("/proc/iomem", "r"); char line[256]; int ram_hit = 0;
    if (!f) { perror("/proc/iomem"); return -1; }
    while (fgets(line, sizeof line, f)) {
        unsigned long lo, hi;
        if (line[0] != ' ' && sscanf(line, "%lx-%lx", &lo, &hi) == 2 && strstr(line, "System RAM") &&
            lo < DMA_BASE + NBLOCKS * BLOCK_BYTES && hi >= DMA_BASE) ram_hit = 1;
    }
    fclose(f);
    if (ram_hit) { fprintf(stderr, "DMA carve-out is System RAM: needs the channelizer firmware\n"); return -1; }
    int fd = open("/dev/mem", O_RDWR | O_SYNC);
    if (fd < 0) { perror("/dev/mem"); return -1; }
    chan = mmap(NULL, 0x10000, PROT_READ | PROT_WRITE, MAP_SHARED, fd, CHAN_BASE);
    dma = mmap(NULL, 0x1000, PROT_READ | PROT_WRITE, MAP_SHARED, fd, SEL_DMA_BASE);
    ring = mmap(NULL, NBLOCKS * BLOCK_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, DMA_BASE);
    if (chan == MAP_FAILED || dma == MAP_FAILED || ring == MAP_FAILED) { perror("mmap"); return -1; }
    if (chan[3] != 0x4348414E) { fprintf(stderr, "channelizer magic missing\n"); return -1; }
    if ((chan[7] & 0xFFFF) != BINS || (chan[0] >> 16) != 1) {
        fprintf(stderr, "unexpected channelizer build (CONFIG 0x%08x VERSION 0x%08x)\n", chan[7], chan[0]);
        return -1;
    }
    return 0;
}
static void write_mask(void) { for (int w = 0; w < 16; ++w) chan[0x40 + w] = mask[w]; }
// Stop: RUN = 0 first (the packetizer finishes its record), then keep the DMA
// running until the selected-stream FIFO is empty, so nothing stale is left for the
// next session, then disable the DMA.
static void hw_stop(void) {
    if (chan) { chan[4] = 0; chan[0x22] = 0; }
    if (dma && chan) {
        for (int i = 0; i < 100 && !((chan[5] & 0xC) == 0x4); ++i) {   // sel FIFO empty, not busy
            if (dma[0x102] == 0 && (dma[0x100] & 1)) {
                dma[0x104] = DMA_BASE + (NBLOCKS - 1) * BLOCK_BYTES;   // scratch block
                dma[0x106] = BLOCK_BYTES - 1; dma[0x107] = 0; dma[0x102] = 1;
            }
            usleep(1000);
        }
    }
    if (dma) dma[0x100] = 0;
}

// transfer bookkeeping: at most two outstanding (IDs are 2 bits)
static uint32_t submitted, completed, ids[NBLOCKS];
static void submit_next(void) {
    uint32_t b = submitted % NBLOCKS;
    dma[0x104] = DMA_BASE + b * BLOCK_BYTES;
    dma[0x106] = BLOCK_BYTES - 1;
    dma[0x107] = 0;
    ids[b] = dma[0x101];
    dma[0x102] = 1;
    submitted++;
}

// ---------------------------------------------------------------------------
// RIQ1 output

static int client = -1;
static uint64_t generation, seq[BINS + 1], sent_bytes;
static long long center_hz, rate_hz, bw_hz;
static long long bin_center(unsigned k) {
    long long off = (k < BINS / 2) ? (long long)k : (long long)k - BINS;
    return center_hz + off * (rate_hz / BINS);
}
static int send_all(const void *h, size_t hn, const void *p, size_t pn) {
    struct iovec v[2] = {{(void *)h, hn}, {(void *)p, pn}};
    size_t total = hn + pn, done = 0;
    while (done < total) {
        ssize_t n = writev(client, v, 2);
        if (n <= 0) { if (n < 0 && errno == EINTR) continue; return -1; }
        done += n;
        for (int i = 0; i < 2; ++i) {
            size_t take = (size_t)n < v[i].iov_len ? (size_t)n : v[i].iov_len;
            v[i].iov_base = (char *)v[i].iov_base + take; v[i].iov_len -= take; n -= take;
        }
    }
    sent_bytes += total;
    return 0;
}
static int message(unsigned type, unsigned flags, unsigned stream, uint64_t first, unsigned format,
                   unsigned bits, const void *payload, unsigned bytes) {
    unsigned char h[64] = {0};
    memcpy(h, "RIQ1", 4); put16(h + 4, 1); put16(h + 6, 64); put16(h + 8, type); put16(h + 10, flags);
    put64(h + 12, generation); put32(h + 20, stream); put64(h + 24, seq[stream]++); put64(h + 32, first);
    put32(h + 40, (uint32_t)(rate_hz / HOP)); put64(h + 44, stream ? bin_center(stream - 1) : center_hz);
    put32(h + 52, bytes); put16(h + 56, format); put16(h + 58, bits);
    return send_all(h, 64, payload, bytes);
}
static int is_selected(unsigned k) { return mask[k / 32] >> (k % 32) & 1; }
static int any_selected(void) {
    for (unsigned i = 0; i < 16; ++i) if (mask[i]) return 1;
    return 0;
}

static int send_config(const char *request, int ok, const char *error, int running) {
    static char json[200000];
    int u = snprintf(json, sizeof json,
        "{\"input_center_hz\":%lld,\"input_rate_hz\":%lld,\"input_bandwidth_hz\":%lld,"
        "\"filter_delay_input_samples_times_two\":4095,\"first_output_center_input_sample_times_two\":4095,"
        "\"hardware_counter\":true,\"continuity_verified\":false,\"activity_calibrated\":false,"
        "\"activity_method\":\"fpga-welch-minstat-cfar\",\"receiver_state\":\"%s\",\"producer\":\"fpga-pfb\","
        "\"input_adc_bits\":12,\"input_adc_shift\":0,"
        "\"profile\":{\"max_input_rate_hz\":%lld,\"bins\":%d,\"taps\":8,\"hop\":%d,\"pre_roll_ms\":0,\"hang_ms\":100,\"max_iq_packet_ms\":10},"
        "\"capabilities\":{\"controls_supported\":true,\"configure_supported\":true,\"pin_supported\":true,\"stop_supported\":true},"
        "\"psd\":{\"bins\":%d,\"center_hz\":%lld,\"span_hz\":%lld,\"unit\":\"dBFS\"},\"streams\":[",
        center_hz, rate_hz, bw_hz, running ? "running" : "stopped", rate_hz, BINS, HOP, BINS,
        center_hz - rate_hz / (2 * BINS), rate_hz);
    if (running)
        for (unsigned k = 0; k < BINS; ++k) {
            int n = snprintf(json + u, sizeof json - u,
                "%s{\"stream_id\":%u,\"center_hz\":%lld,\"target_hz\":%lld,\"sample_rate_num_hz\":%lld,"
                "\"sample_rate_den\":%d,\"protocol\":\"Unknown\",\"pinned\":%s,\"iq_forwarded\":%s}",
                k ? "," : "", k + 1, bin_center(k), bin_center(k), rate_hz, HOP,
                is_selected(k) ? "true" : "false", is_selected(k) ? "true" : "false");
            if (n < 0 || (size_t)n >= sizeof json - u) return -1;
            u += n;
        }
    int n = snprintf(json + u, sizeof json - u, "]%s", request ? ",\"control_ack\":{" : "}");
    u += n;
    if (request) u += snprintf(json + u, sizeof json - u, "\"request_id\":\"%s\",\"ok\":%s,\"error\":\"%s\"}}",
                               request, ok ? "true" : "false", error ? error : "");
    return message(4, 0, 0, 0, 3, 0, json, u);
}

// per-bin IQ accumulation
typedef struct { int16_t iq[2 * IQ_CHUNK]; unsigned n; uint64_t first; unsigned flags; } stream_buf;
static stream_buf sb[BINS];
static int flush_bin(unsigned k) {
    stream_buf *b = &sb[k];
    if (!b->n) return 0;
    int r = message(1, b->flags, k + 1, b->first, 1, 16, b->iq, b->n * 4);
    b->n = 0; b->flags = 0;
    return r;
}

// record parser state (records can straddle transfer blocks)
static uint64_t cur_frame;
static unsigned cur_flags, have_header;
static uint64_t expect_frame;
static int parse_word(uint64_t w) {
    unsigned tag = w >> 56;
    if (tag == 0xA2) {
        uint64_t f = w & ((1ULL << 48) - 1);
        unsigned fl = (w >> 48) & 0xFF;
        unsigned riq = 0;
        if (have_header && f != expect_frame) riq |= 8;     // dropped frames (also flag bit0)
        if (fl & 1) riq |= 8;
        if (fl & 2) riq |= 8 | 16;                         // window may span lost ADC samples
        cur_frame = f; cur_flags = riq; have_header = 1; expect_frame = f + 1;
        return 0;
    }
    if ((w >> 60) != 0x5 || !have_header) return 0;
    unsigned k = (w >> 48) & 0x1FF;
    if (((w >> 32) & 0xFFFF) != (cur_frame & 0xFFFF)) return 0;   // torn/foreign word
    stream_buf *b = &sb[k];
    if (b->n && b->first + b->n != cur_frame) { if (flush_bin(k)) return -1; }
    if (!b->n) { b->first = cur_frame; b->flags = cur_flags; }
    else b->flags |= cur_flags;  // a loss in the middle marks the whole IQ chunk
    b->iq[2 * b->n] = (int16_t)(w & 0xFFFF);
    b->iq[2 * b->n + 1] = (int16_t)((w >> 16) & 0xFFFF);
    if (++b->n == IQ_CHUNK) return flush_bin(k);
    return 0;
}

static int send_psd_activity(void) {
    float psd[BINS];
    for (unsigned m = 0; m < BINS; ++m) {           // low to high frequency
        unsigned k = (m + BINS / 2) % BINS;
        unsigned l = chan[0x400 + k] & 0xFFFF;
        psd[m] = 3.0103f * l / 256.0f - 96.33f - 39.13f;     // -10 log10(2^13 frames)
    }
    if (message(2, 0, 0, (uint64_t)chan[0x12], 2, 32, psd, sizeof psd)) return -1;
    // The detector is not calibrated on live RF yet. A noisy window can mark
    // nearly every bin active; sending 512 individual activity frames at the
    // 10 Hz PSD cadence overwhelms the host with unchanged state. Spread one
    // sweep over ten PSD ticks instead of blocking DMA service with 512 writes.
    static unsigned activity_bin;
    for (unsigned sent = 0; sent < (BINS + 9) / 10; ++sent) {
        unsigned k = activity_bin;
        activity_bin = (activity_bin + 1) % BINS;
        int active = chan[0x50 + k / 32] >> (k % 32) & 1;
        uint32_t lv = chan[0x200 + k];
        char j[200];
        int n = snprintf(j, sizeof j, "{\"stream_id\":%u,\"rf_active\":%s,\"level_db\":%.2f,\"floor_db\":%.2f,"
                         "\"sample_index\":%u,\"forwarded\":%s}", k + 1, active ? "true" : "false",
                         3.0103 * (lv & 0xFFFF) / 256.0 - 96.33 - 24.08, 3.0103 * (lv >> 16) / 256.0 - 96.33 - 24.08,
                         chan[0x12], is_selected(k) ? "true" : "false");
        if (message(3, 0, k + 1, chan[0x12], 3, 0, j, n)) return -1;
    }
    return 0;
}

static int json_eq(const char *j, const jsmntok_t *t, const char *v) {
    return t->type == JSMN_STRING && (size_t)(t->end - t->start) == strlen(v) && !memcmp(j + t->start, v, strlen(v));
}
static const jsmntok_t *field(const char *j, const jsmntok_t *t, int count, const char *name) {
    for (int k = 1; k + 1 < count; ++k) if (t[k].parent == 0 && json_eq(j, t + k, name)) return t + k + 1;
    return NULL;
}
// returns 1 on stop request (its request_id is left in stop_request)
static char stop_request[80];
static long long json_integer(const char *j, const jsmntok_t *t) {
    if (!t || t->type != JSMN_PRIMITIVE) return -1;
    char *end;
    errno = 0;
    long long value = strtoll(j + t->start, &end, 10);
    return errno || end != j + t->end ? -1 : value;
}

// Stop and drain before changing LO so old samples cannot acquire a new label.
// Rate and RF bandwidth stay fixed for this FPGA image.
static int retune(const char *request, long long target) {
    char value[64], readback[64];
    long long previous = center_hz;
    hw_stop();
    snprintf(value, sizeof value, "%lld", target);
    int wrote = !attr_write("out_altvoltage0_RX_LO_frequency", value);
    int read = !attr_read("out_altvoltage0_RX_LO_frequency", readback, sizeof readback);
    long long actual = read ? atoll(readback) : 0;
    // The driver aligns the request to an achievable synthesizer setting.
    // Its successful write and valid readback are authoritative, not equality
    // with the requested number (nor an arbitrary frequency-error tolerance).
    int ok = wrote && read && actual >= 70000000 && actual <= 6000000000LL;
    fprintf(stderr, "RX LO request=%lld readback=%lld write_ok=%d read_ok=%d accepted=%d\n",
            target, actual, wrote, read, ok);
    if (!ok) {
        snprintf(value, sizeof value, "%lld", previous);
        if (attr_write("out_altvoltage0_RX_LO_frequency", value)
            || attr_read("out_altvoltage0_RX_LO_frequency", readback, sizeof readback)) return -1;
        previous = atoll(readback);
        if (previous < 70000000 || previous > 6000000000LL) return -1;
    }
    center_hz = ok ? actual : previous;
    usleep(20000); // LO settling; acquisition remains stopped.
    memset(sb, 0, sizeof sb);
    memset(seq, 0, sizeof seq);
    memset(mask, 0, sizeof mask);
    have_header = 0;
    submitted = completed = 0;
    generation++;
    write_mask();
    dma[0x100] = 0; dma[0x100] = 1; dma[0x103] = 0;
    submit_next(); submit_next();
    chan[4] = 0x15; // RUN, SEL_EN, detector reset
    char error[180];
    snprintf(error, sizeof error,
             "RX LO change failed: requested %lld Hz, read %lld Hz, write_ok=%d, read_ok=%d; previous frequency restored",
             target, actual, wrote, read);
    if (send_config(request, ok, ok ? NULL : error, 1)) return -1;
    return 2;
}

static int handle_control(void) {
    unsigned char h[64];
    ssize_t n = recv(client, h, 64, MSG_DONTWAIT | MSG_PEEK);
    if (n < 64) return (n == 0 || (n < 0 && errno != EAGAIN && errno != EWOULDBLOCK)) ? -1 : 0;
    unsigned bytes = get32(h + 52);
    if (memcmp(h, "RIQ1", 4) || h[8] != 6 || bytes > MAX_CONTROL) return -1;
    char buf[64 + MAX_CONTROL + 1];
    size_t total = 64 + bytes;
    n = recv(client, buf, total, MSG_DONTWAIT | MSG_PEEK);
    if (n < (ssize_t)total) return (n == 0 || (n < 0 && errno != EAGAIN && errno != EWOULDBLOCK)) ? -1 : 0;
    // The full frame is queued. Consume it without ever waiting on the client:
    // the same thread must keep servicing selected DMA blocks.
    n = recv(client, buf, total, MSG_DONTWAIT | MSG_WAITALL);
    if (n != (ssize_t)total) return -1;
    char *j = buf + 64; j[bytes] = 0;
    jsmn_parser p; jsmntok_t t[64]; jsmn_init(&p);
    int count = jsmn_parse(&p, j, bytes, t, 64);
    if (count < 1) return -1;
    const jsmntok_t *rid = field(j, t, count, "request_id"), *act = field(j, t, count, "action");
    char request[80] = "?";
    if (rid && rid->end - rid->start < 79) { memcpy(request, j + rid->start, rid->end - rid->start); request[rid->end - rid->start] = 0; }
    if (!act) return send_config(request, 0, "missing action", 1);
    if (json_eq(j, act, "get_config")) return send_config(request, 1, NULL, 1);
    if (json_eq(j, act, "stop")) { strcpy(stop_request, request); return 1; }
    if (json_eq(j, act, "configure")) {
        long long center = json_integer(j, field(j, t, count, "input_center_hz"));
        long long rate = json_integer(j, field(j, t, count, "input_rate_hz"));
        long long bandwidth = json_integer(j, field(j, t, count, "input_bandwidth_hz"));
        if (center < 70000000 || center > 6000000000LL)
            return send_config(request, 0, "center must be between 70 and 6000 MHz", 1);
        if (rate != rate_hz || bandwidth != bw_hz)
            return send_config(request, 0, "FPGA rate and bandwidth are fixed; change center only", 1);
        return retune(request, center);
    }
    if (json_eq(j, act, "pin")) {
        const jsmntok_t *st = field(j, t, count, "stream_id"), *pin = field(j, t, count, "pinned");
        if (!st || !pin) return send_config(request, 0, "pin requires stream_id and pinned", 1);
        long id = strtol(j + st->start, NULL, 10);
        if (id < 1 || id > BINS) return send_config(request, 0, "stream_id out of range", 1);
        unsigned k = id - 1;
        if (j[pin->start] == 't') mask[k / 32] |= 1u << (k % 32);
        else { mask[k / 32] &= ~(1u << (k % 32)); if (flush_bin(k)) return -1; }
        write_mask();
        return send_config(request, 1, NULL, 1);
    }
    return send_config(request, 0, "unsupported control", 1);
}

int main(int argc, char **argv) {
    int port = 9010; long long seconds = 0; double gain = 40;
    center_hz = 460000000; rate_hz = 25600000; bw_hz = 20000000;
    static struct option opts[] = {{"port", 1, 0, 'p'}, {"center", 1, 0, 'c'}, {"rate", 1, 0, 'r'},
        {"bandwidth", 1, 0, 'b'}, {"gain", 1, 0, 'g'}, {"forward-streams", 1, 0, 'f'}, {"seconds", 1, 0, 's'}, {0, 0, 0, 0}};
    int c;
    while ((c = getopt_long(argc, argv, "", opts, NULL)) != -1) {
        switch (c) {
        case 'p': port = atoi(optarg); break;
        case 'c': center_hz = atoll(optarg); break;
        case 'r': rate_hz = atoll(optarg); break;
        case 'b': bw_hz = atoll(optarg); break;
        case 'g': gain = atof(optarg); break;
        case 's': seconds = atoll(optarg); break;
        case 'f': for (char *s = optarg; *s;) { long id = strtol(s, &s, 10); if (id < 1 || id > BINS) return 2;
                      mask[(id - 1) / 32] |= 1u << ((id - 1) % 32); if (*s == ',') s++; } break;
        default: fprintf(stderr, "usage: see header comment\n"); return 2;
        }
    }
    if (rate_hz % BINS || port < 1 || port > 65535) return 2;
    signal(SIGINT, on_signal); signal(SIGTERM, on_signal); signal(SIGPIPE, SIG_IGN);
    if (find_phy() || map_hw()) return 1;
    dma[0x100] = 0; dma[0x100] = 1; dma[0x103] = 0;   // drain anything a previous session left
    hw_stop();

    int ls = socket(AF_INET, SOCK_STREAM, 0), one = 1;
    setsockopt(ls, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    struct sockaddr_in a = {.sin_family = AF_INET, .sin_port = htons(port), .sin_addr.s_addr = htonl(INADDR_ANY)};
    if (bind(ls, (struct sockaddr *)&a, sizeof a) || listen(ls, 1)) { perror("listen"); return 1; }
    fprintf(stderr, "pluto-fpga-source waiting on TCP:%d; radio unopened\n", port);
    client = accept(ls, NULL, NULL);
    if (client < 0) return 1;
    struct timeval tv = {.tv_sec = 0, .tv_usec = 250000};
    setsockopt(client, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof tv);
    setsockopt(client, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);

    int result = 0, changed = 0;
    if (save_controls()) { fprintf(stderr, "cannot read RX controls\n"); return 1; }
    char v[64];
    changed = 1;
    attr_write("in_voltage0_gain_control_mode", "manual");
    snprintf(v, sizeof v, "%lld", rate_hz); attr_write("in_voltage_sampling_frequency", v);
    snprintf(v, sizeof v, "%lld", bw_hz); attr_write("in_voltage_rf_bandwidth", v);
    snprintf(v, sizeof v, "%lld", center_hz); attr_write("out_altvoltage0_RX_LO_frequency", v);
    snprintf(v, sizeof v, "%.0f", gain); attr_write("in_voltage0_hardwaregain", v);
    if (attr_read("in_voltage_sampling_frequency", v, sizeof v) || atoll(v) != rate_hz) {
        fprintf(stderr, "sample rate readback %s != %lld\n", v, rate_hz); result = 1; goto out;
    }
    if (!attr_read("out_altvoltage0_RX_LO_frequency", v, sizeof v)) center_hz = atoll(v);
    if (!attr_read("in_voltage_rf_bandwidth", v, sizeof v)) bw_hz = atoll(v);
    generation = (uint64_t)time(NULL);

    // start: channelizer RUN + SEL_EN (history ring off), DMA ring of transfer blocks
    chan[6] = 7;                      // OUT_SHIFT: x16 ADC units
    write_mask();
    chan[4] = 0x5;
    dma[0x100] = 0; dma[0x100] = 1; dma[0x103] = 0;
    submit_next(); submit_next();
    if (send_config(NULL, 1, NULL, 1)) { result = 1; goto out; }

    double t0 = now(), last_psd = 0;
    double last_frame_progress = t0, last_dma_progress = t0;
    uint32_t previous_frame = chan[0x12];
    while (!interrupted && (seconds == 0 || now() - t0 < seconds)) {
        uint32_t b = completed % NBLOCKS;
        if (completed < submitted && (dma[0x10A] >> (ids[b] & 3) & 1)) {
            // Refill the hardware queue before parsing and socket writes can
            // block. The next destination is a different ring block.
            completed++;
            submit_next();
            const volatile uint64_t *w = (const volatile uint64_t *)(ring + b * BLOCK_BYTES);
            for (unsigned i = 0; i < BLOCK_BYTES / 8; ++i) if (parse_word(w[i])) { result = 1; goto out; }
            last_dma_progress = now();
            continue;
        }
        double t = now();
        if (t - last_psd > 0.1) {
            uint32_t frame = chan[0x12];
            if (frame != previous_frame) { previous_frame = frame; last_frame_progress = t; }
            if (!any_selected()) last_dma_progress = t;
            if (t - last_frame_progress > 2.0 || t - last_dma_progress > 2.0) {
                fprintf(stderr, "channelizer frame or selected DMA stream stalled\n");
                result = 1; goto out;
            }
            if (send_psd_activity()) { result = 1; goto out; }
            last_psd = t;
        }
        int r = handle_control();
        if (r < 0) break;                 // client gone
        if (r == 2) {
            last_frame_progress = last_dma_progress = now();
            previous_frame = chan[0x12];
            last_psd = now();
        }
        if (r == 1) {                     // stop: restore before acknowledging
            hw_stop();
            int ok = restore_controls() == 0;
            changed = 0;
            send_config(stop_request, ok, ok ? NULL : "receive controls failed readback", 0);
            goto done;
        }
        usleep(2000);
    }
out:
    hw_stop();
done:
    if (changed && restore_controls()) { fprintf(stderr, "FAILED restoring RX controls\n"); result = 1; }
    fprintf(stderr, "{\"status\":\"%s\",\"wire_bytes\":%" PRIu64 ",\"frames\":%u,\"adc_losses\":%u,\"sel_drops\":%u}\n",
            result ? "error" : "stopped", sent_bytes, chan ? chan[0x12] : 0, chan ? chan[0x14] : 0, chan ? chan[0x1A] : 0);
    close(client);
    return result;
}
