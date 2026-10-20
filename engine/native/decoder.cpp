// C ABI adapter to GPLv3 DSDcc + ISC mbelib. No algorithm reimplementation here.
//
// DSDcc does sync, FEC and descrambling. The AMBE+2 49 data bits -> 8 kHz PCM step is either
// mbelib's (inside DSDcc) or, when the caller passes a vocoder callback, the engine's own
// decoder (src/ambe, called back through the hook in patches/dsdcc-ambe-vocoder-hook.patch).
#include "dsd_decoder.h"
#include <unistd.h>
#include <fcntl.h>
#include <cstdio>
#include <vector>
#include <string>
#include <algorithm>
struct Codec {
    DSDcc::DSDDecoder d;
    int read_fd = -1;
    int slot;
    std::vector<short> audio;
    std::vector<short> audio_b;
    std::string diagnostics;
    Codec(int protocol, int selected_slot, DSDcc::DSDMBEDecoder::Ambe2450Vocoder vocoder, void *context): slot(selected_slot) {
        int fd[2];
        if (pipe(fd) != 0) throw 1;
        read_fd = fd[0];
        fcntl(read_fd, F_SETFL, O_NONBLOCK);
        auto path = std::string("/dev/fd/") + std::to_string(fd[1]);
        d.setLogFile(path.c_str());
        close(fd[1]);
        d.setVerbosity(2); d.setLogVerbosity(1); d.showErrorBars();
        d.setDecodeMode(DSDcc::DSDDecoder::DSDDecodeNone, true);
        d.setDecodeMode(protocol == 1 ? DSDcc::DSDDecoder::DSDDecodeDMR : DSDcc::DSDDecoder::DSDDecodeNXDN48, true);
        d.enableMbelib(true); d.enableAudioOut(true); d.setUpsampling(1);
        d.setStereo(false); d.setTDMAStereo(false);
        d.setAudioGain(1.0); d.enableCosineFiltering(true);
        if (vocoder) d.setAmbe2450Vocoder(vocoder, context);
    }
    ~Codec() { if (read_fd >= 0) close(read_fd); }
    void drain() {
        // DSDcc's public logger exposes no flush; flushing the process streams keeps
        // its pipe incremental without changing the upstream library or using disk.
        fflush(nullptr);
        char buf[4096]; ssize_t n;
        while ((n=read(read_fd,buf,sizeof(buf)))>0) if(diagnostics.size()<65536) diagnostics.append(buf,n);
    }
};
extern "C" {
// `vocoder` (nullable) replaces mbelib's AMBE+2 synthesis; `context` is passed back to it.
// Defined only by a DSDcc built with patches/dsdcc-ambe-vocoder-hook.patch: an unpatched
// library fails to link rather than run with a different DSDMBEDecoder layout.
extern "C" int dsdcc_ambe2450_vocoder_hook_v1(void);
void *radio_codec_new(int protocol, int slot, DSDcc::DSDMBEDecoder::Ambe2450Vocoder vocoder, void *context) {
    if (dsdcc_ambe2450_vocoder_hook_v1() != 1) return nullptr;
    try { return new Codec(protocol, slot, vocoder, context); } catch(...) { return nullptr; }
}
void radio_codec_free(void *p) { delete static_cast<Codec*>(p); }
// One DSDcc DMR instance decodes both TDMA slots; audio for slot 1 and slot 2 is returned
// separately. NXDN only produces slot 1.
size_t radio_codec_push2(void *p, const short *in, size_t count, short *out1, size_t cap1, short *out2, size_t cap2, size_t *n2, char *log, size_t log_capacity) {
    auto &c=*static_cast<Codec*>(p); c.audio.clear(); c.audio_b.clear(); c.diagnostics.clear();
    for(size_t i=0;i<count;i++) {
        c.d.run(in[i]);
        int n=0; short *pcm=c.d.getAudio1(n);
        if(n>0 && pcm && c.audio.size()+n<=cap1) c.audio.insert(c.audio.end(),pcm,pcm+n);
        n=0; pcm=c.d.getAudio2(n);
        if(n>0 && pcm && c.audio_b.size()+n<=cap2) c.audio_b.insert(c.audio_b.end(),pcm,pcm+n);
        c.d.resetAudio1(); c.d.resetAudio2();
        if(i%480==479) c.drain();
    }
    c.drain();
    std::copy(c.audio.begin(),c.audio.end(),out1);
    std::copy(c.audio_b.begin(),c.audio_b.end(),out2);
    *n2=c.audio_b.size();
    if(log_capacity) { size_t n=std::min(log_capacity-1,c.diagnostics.size()); std::copy_n(c.diagnostics.data(),n,log);log[n]=0; }
    return c.audio.size();
}
}
