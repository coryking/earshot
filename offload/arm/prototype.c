#define _POSIX_C_SOURCE 200809L
#include "pfb.h"
#include "gate.h"
#include <errno.h>
#include <getopt.h>
#include <inttypes.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#ifdef WITH_IIO
#include <iio.h>
#endif
static double now(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return t.tv_sec+t.tv_nsec*1e-9;}
static int count_emit(void *p,unsigned bin,uint64_t first,unsigned count,unsigned flags,const iqs16 *iq){(void)bin;(void)first;(void)flags;(void)iq;*(uint64_t*)p+=count;return 0;}
static void usage(void){
    fprintf(stderr,"Usage: pluto-pfb --rate HZ [--bins 1024] [--taps 8] [--samples N]\n"
    "  --benchmark             synthetic compute-only benchmark; never opens radio\n"
    "  --input PATH|-          raw ci16le file/pipe for offline validation\n"
    "  --output PATH|-         write ungated cf32le, one FFT-bin vector per frame\n"
    "  --json                  emit final measurement as JSON (default)\n"
    "No capture, configuration, or transmit operations in this prototype.\n"
    "Exit codes:0 success,1 operational failure,2 usage error.\n");
}
int main(int argc,char **argv){
    unsigned bins=1024,taps=8;uint64_t rate=25600000,samples=25600000;
    int bench=0,gating=0;const char *in=NULL,*out=NULL;
    static struct option opts[]={{"rate",1,0,'r'},{"bins",1,0,'b'},{"taps",1,0,'t'},
    {"samples",1,0,'s'},{"input",1,0,'i'},{"output",1,0,'o'},{"benchmark",0,0,'B'},
    {"gate-benchmark",0,0,'G'},{"json",0,0,'j'},{"help",0,0,'h'},{0,0,0,0}};
    int c;while((c=getopt_long(argc,argv,"",opts,NULL))!=-1){char *end=NULL;uint64_t v=0;
      if(c=='r'||c=='b'||c=='t'||c=='s'){errno=0;v=strtoull(optarg,&end,10);if(errno||!*optarg||*end||optarg[0]=='-'){usage();return 2;}}
      switch(c){case'r':rate=v;break;case'b':if(v>8192){usage();return 2;}bins=v;break;case't':if(v>32){usage();return 2;}taps=v;break;case's':samples=v;break;case'i':in=optarg;break;case'o':out=optarg;break;case'B':bench=1;break;case'G':bench=1;gating=1;break;case'j':break;case'h':usage();return 0;default:usage();return 2;}}
    if(optind!=argc||rate==0||samples==0||bench==!!in){usage();return 2;}
    pfb p;if(pfb_init(&p,bins,taps)){fprintf(stderr,"Invalid PFB shape or allocation failed\n");return 2;}
    FILE *input=NULL,*output=NULL;
    if(in){input=!strcmp(in,"-")?stdin:fopen(in,"rb");if(!input){perror("input");pfb_destroy(&p);return 1;}}
    if(out){output=!strcmp(out,"-")?stdout:fopen(out,"wbx");if(!output){perror("output (must not exist)");pfb_destroy(&p);return 1;}}
    int16_t *block=calloc(p.hop*2,sizeof(int16_t));if(!block){pfb_destroy(&p);return 1;}
    if(bench)for(unsigned j=0;j<p.hop;++j){block[2*j]=(int16_t)(700*cos(2*3.141592653589793*8*j/bins));block[2*j+1]=(int16_t)(700*sin(2*3.141592653589793*8*j/bins));}
    activity_gate gate={0};uint64_t gated_samples=0;if(gating&&gate_init(&gate,bins,(unsigned)(0.2*rate/p.hop),(unsigned)(0.5*rate/p.hop),8,16,count_emit,&gated_samples))return 1;
    double start=now();uint64_t total=0;int error=0;
    while(total+p.hop<=samples){
       if(input){size_t count=fread(block,sizeof(int16_t)*2,p.hop,input);if(count!=p.hop){if(ferror(input)){perror("read");error=1;}break;}}
       if(pfb_push(&p,block)){if(gating&&gate_push(&gate,p.fft)){error=1;break;}if(output&&fwrite(p.fft,sizeof(iqf),p.n,output)!=p.n){perror("write");error=1;break;}}
       total+=p.hop;
    }
    if(gating&&gate_flush(&gate))error=1;
    double elapsed=now()-start;
    if(output&&fflush(output)){perror("flush");error=1;}
    FILE *stats=output==stdout?stderr:stdout;
    fprintf(stats,"{\"input_samples\":%"PRIu64",\"input_rate_hz\":%"PRIu64",\"bins\":%u,\"taps\":%u,\"hop\":%u,\"output_frames\":%"PRIu64",\"output_channel_rate_hz\":%.9g,\"wall_seconds\":%.6f,\"samples_per_second\":%.9g,\"realtime_ratio\":%.6f,\"gate_enabled\":%s,\"gated_samples\":%"PRIu64",\"radio_opened\":false,\"continuity_verified\":false}\n",total,rate,p.n,p.taps,p.hop,p.frames,(double)rate/p.hop,elapsed,total/elapsed,total/elapsed/rate,gating?"true":"false",gated_samples);
    if(input&&input!=stdin)fclose(input);
    if(output&&output!=stdout)fclose(output);
    free(block);gate_destroy(&gate);pfb_destroy(&p);return error;
}
