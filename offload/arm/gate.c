#include "gate.h"
#include <math.h>
#include <stdlib.h>
#include <string.h>
static float median(float *a,unsigned n){unsigned lo=0,hi=n-1,k=n/2;while(lo<hi){float pivot=a[k];unsigned i=lo,j=hi;for(;;){while(a[i]<pivot)++i;while(a[j]>pivot)--j;if(i>j)break;float t=a[i];a[i]=a[j];a[j]=t;++i;if(j==0)break;--j;}if(j<k)lo=i;if(k<i)hi=j;}return a[k];}
static int16_t quantize(float value){if(value>32767)return 32767;if(value< -32768)return -32768;return (int16_t)(value>=0?value+0.5f:value-0.5f);}
int gate_init(activity_gate *g,unsigned n,unsigned pre_frames,unsigned hang_frames,float snr_db,float scale,gate_emit emit,void *opaque){
 memset(g,0,sizeof(*g));if(!n||!pre_frames||!hang_frames||!emit)return -1;
 g->n=n;g->capacity=pre_frames;g->hang=hang_frames;g->ratio=powf(10,snr_db/10);g->close_ratio=g->ratio*0.5f;g->scale=scale;g->emit=emit;g->opaque=opaque;
 g->history=calloc((size_t)n*pre_frames,sizeof(iqs16));g->scratch=calloc(n,sizeof(float));g->channels=calloc(n,sizeof(gate_channel));
 if(!g->history||!g->scratch||!g->channels){gate_destroy(g);return -1;}return 0;
}
void gate_destroy(activity_gate *g){free(g->history);free(g->scratch);free(g->channels);memset(g,0,sizeof(*g));}
static int flush_one(activity_gate *g,unsigned bin,unsigned flags){gate_channel *c=g->channels+bin;if(!c->pending)return 0;if(g->emit(g->opaque,bin,c->pending_first,c->pending,flags|c->pending_flags|(c->pinned?32:0),c->block))return -1;c->last_sent=c->pending_first+c->pending;c->pending=0;c->pending_flags=0;return 0;}
int gate_flush(activity_gate *g){for(unsigned k=0;k<g->n;++k)if(flush_one(g,k,2))return -1;return 0;}
int gate_push(activity_gate *g,const iqf *frame){
 unsigned slot=g->frame%g->capacity;
 for(unsigned k=0;k<g->n;++k){gate_channel *c=g->channels+k;float power=frame[k].re*frame[k].re+frame[k].im*frame[k].im;c->power+=0.1f*(power-c->power);g->scratch[k]=c->power;g->history[(size_t)slot*g->n+k]=(iqs16){quantize(frame[k].re*g->scale),quantize(frame[k].im*g->scale)};}
 if(g->filled<g->capacity)++g->filled;
 float floor=median(g->scratch,g->n);if(floor<1e-10f)floor=1e-10f;
 for(unsigned k=0;k<g->n;++k){
   gate_channel *c=g->channels+k;int carrier=c->power>floor*(c->active?g->close_ratio:g->ratio);c->rf_active=carrier;
   if(!c->active&&(carrier||c->pinned)){
      c->active=1;c->hold=g->hang;c->pending_flags=1;++c->starts;
      uint64_t first=g->frame+1-g->filled;if(first<c->last_sent)first=c->last_sent;
      /* Previously emitted samples never reappear in a later pre-roll. */
      while(first<g->frame){unsigned count=(unsigned)(g->frame-first);if(count>256)count=256;
        iqs16 old[256];for(unsigned j=0;j<count;++j)old[j]=g->history[((first+j)%g->capacity)*g->n+k];
        unsigned flags=4|c->pending_flags|(c->pinned?32:0);
        if(g->emit(g->opaque,k,first,count,flags,old))return -1;c->last_sent=first+count;c->pending_flags=0;first+=count;
      }
   }
   if(c->active){
      if(!c->pending)c->pending_first=g->frame;
      c->block[c->pending++]=g->history[(size_t)slot*g->n+k];
      if(carrier||c->pinned)c->hold=g->hang;else if(c->hold)--c->hold;
      if(!c->hold&&!c->pinned){if(flush_one(g,k,2))return -1;c->active=0;++c->ends;}
      else if(c->pending==256&&flush_one(g,k,0))return -1;
   }
 }
 ++g->frame;return 0;
}
