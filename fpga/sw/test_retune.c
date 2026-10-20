// Host-only control-path test; no FPGA simulation or hardware access.
// cc -O2 -Wall -Wextra -o /tmp/test-retune fpga/sw/test_retune.c -lm && /tmp/test-retune
#define main producer_main
#include "pluto_fpga_source.c"
#undef main
#include <assert.h>

static void drain_config(int peer, int ok) {
    unsigned char h[64];
    assert(recv(peer, h, sizeof h, MSG_WAITALL) == sizeof h);
    assert(!memcmp(h, "RIQ1", 4) && h[8] == 4);
    unsigned size = get32(h + 52);
    char *json = calloc(size + 1, 1);
    assert(recv(peer, json, size, MSG_WAITALL) == size);
    assert(strstr(json, ok ? "\"ok\":true" : "\"ok\":false"));
    free(json);
}

int main(void) {
    char dir[] = "/tmp/radio-retune-XXXXXX";
    assert(mkdtemp(dir));
    snprintf(phy, sizeof phy, "%s", dir);
    int sockets[2];
    assert(!socketpair(AF_UNIX, SOCK_STREAM, 0, sockets));
    client = sockets[0];
    chan = calloc(0x10000 / 4, 4);
    dma = calloc(0x1000 / 4, 4);
    chan[5] = 4; // selected FIFO empty
    center_hz = 460000000; rate_hz = 25600000; bw_hz = 20000000;
    generation = 42;
    sb[0].n = 10; mask[0] = 1; have_header = 1;
    assert(retune("move", 480000000) == 2);
    drain_config(sockets[1], 1);
    char value[64];
    assert(!attr_read("out_altvoltage0_RX_LO_frequency", value, sizeof value));
    assert(atoll(value) == 480000000 && center_hz == 480000000);
    assert(generation == 43 && !sb[0].n && !mask[0] && !have_header);
    assert(submitted == 2 && completed == 0);
    // An unsupported rate must not change the LO or generation.
    const char *json = "{\"action\":\"configure\",\"request_id\":\"bad-rate\",\"input_center_hz\":145000000,\"input_rate_hz\":1000000,\"input_bandwidth_hz\":20000000}";
    unsigned char header[64] = {0};
    memcpy(header, "RIQ1", 4); header[8] = 6; put32(header + 52, strlen(json));
    assert(send(sockets[1], header, 64, 0) == 64);
    assert(send(sockets[1], json, strlen(json), 0) == (ssize_t)strlen(json));
    assert(handle_control() == 0);
    drain_config(sockets[1], 0);
    assert(center_hz == 480000000 && generation == 43);
    char path[400]; snprintf(path, sizeof path, "%s/out_altvoltage0_RX_LO_frequency", dir);
    unlink(path); rmdir(dir);
    close(sockets[0]); close(sockets[1]);
    free((void *)chan); free((void *)dma);
    puts("retune control tests passed");
    return 0;
}
