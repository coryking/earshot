#ifndef PLUTO_GATE_H
#define PLUTO_GATE_H
#include "pfb.h"
typedef struct {int16_t re,im;} iqs16;
typedef int (*gate_emit)(void *opaque,unsigned bin,uint64_t first,unsigned count,unsigned flags,const iqs16 *iq);
typedef struct {float power;unsigned hold,active,pinned,pending,pending_flags,rf_active;uint64_t pending_first,last_sent;uint64_t starts,ends;iqs16 block[256];} gate_channel;
typedef struct {unsigned n,capacity,hang;float ratio,close_ratio,scale;uint64_t frame;unsigned filled;iqs16 *history;float *scratch;gate_channel *channels;gate_emit emit;void *opaque;} activity_gate;
int gate_init(activity_gate *g,unsigned n,unsigned pre_frames,unsigned hang_frames,float snr_db,float scale,gate_emit emit,void *opaque);
void gate_destroy(activity_gate *g);
int gate_push(activity_gate *g,const iqf *frame);
int gate_flush(activity_gate *g);
#endif
