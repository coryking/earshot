#define _POSIX_C_SOURCE 200809L
#include "pfb.h"
#include "gate.h"
#define JSMN_PARENT_LINKS
#define JSMN_STATIC
#include "vendor/jsmn.h"
#include <iio.h>
#include <ad9361.h>
#include <arpa/inet.h>
#include <errno.h>
#include <getopt.h>
#include <inttypes.h>
#include <math.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <sys/uio.h>
#include <time.h>
#include <unistd.h>

#define BINS 64
#define RATE_LIMIT 800000
#define MAX_CONTROL 4096
#define BLOCK_SAMPLES 16384
#define INPUT_BITS 12
static volatile sig_atomic_t interrupted;
static void on_signal(int value){(void)value;interrupted=1;}
static double now(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return t.tv_sec+t.tv_nsec*1e-9;}
static void put16(unsigned char *b,uint16_t v){b[0]=v;b[1]=v>>8;}
static void put32(unsigned char *b,uint32_t v){for(unsigned j=0;j<4;++j)b[j]=v>>(8*j);}
static void put64(unsigned char *b,uint64_t v){for(unsigned j=0;j<8;++j)b[j]=v>>(8*j);}
static uint32_t get32(const unsigned char *b){uint32_t v=0;for(unsigned j=0;j<4;++j)v|=(uint32_t)b[j]<<(8*j);return v;}
typedef struct {long long rate,bandwidth,center;char gain[64];} controls;
typedef struct {
 struct iio_context *context;struct iio_device *phy,*rx;struct iio_channel *phy_ch,*tx_clock,*lo,*i,*q;struct iio_buffer *buffer;
 controls original,current;long long original_tx_rate;int fir_changed;pfb channelizer;activity_gate gate;
 int client,listener,running,restore_failed;uint64_t forward_mask;uint64_t generation,sequences[BINS+1],sent_bytes,iq_samples,input_samples;
 unsigned char activity[BINS];unsigned bits,shift;float fullscale;double started,capture_ended,processing_seconds;unsigned late_buffers;
} source;
static int read_controls(source *s,controls *c){
 if(iio_channel_attr_read_longlong(s->phy_ch,"sampling_frequency",&c->rate)<0||iio_channel_attr_read_longlong(s->phy_ch,"rf_bandwidth",&c->bandwidth)<0||iio_channel_attr_read_longlong(s->lo,"frequency",&c->center)<0)return -1;
 ssize_t n=iio_channel_attr_read(s->phy_ch,"gain_control_mode",c->gain,sizeof(c->gain));if(n<0||n>=(ssize_t)sizeof(c->gain))return -1;c->gain[n]=0;while(n>0&&(c->gain[n-1]=='\n'||c->gain[n-1]=='\r'||c->gain[n-1]==' '))c->gain[--n]=0;return 0;
}
static int write_controls(source *s,const controls *c){int failed=0;
 int rc=iio_channel_attr_write_longlong(s->phy_ch,"sampling_frequency",c->rate);if(rc<0){fprintf(stderr,"sampling_frequency write %lld failed %d\n",c->rate,rc);failed=1;}
 rc=iio_channel_attr_write_longlong(s->phy_ch,"rf_bandwidth",c->bandwidth);if(rc<0){fprintf(stderr,"rf_bandwidth write %lld failed %d\n",c->bandwidth,rc);failed=1;}
 rc=iio_channel_attr_write_longlong(s->lo,"frequency",c->center);if(rc<0){fprintf(stderr,"RX LO write failed %d\n",rc);failed=1;}
 rc=iio_channel_attr_write(s->phy_ch,"gain_control_mode",c->gain);if(rc<0){fprintf(stderr,"RX gain write failed %d\n",rc);failed=1;}
 controls actual;if(read_controls(s,&actual)||actual.rate!=c->rate||actual.bandwidth!=c->bandwidth||actual.center!=c->center||strcmp(actual.gain,c->gain))failed=1;
 return failed?-1:0;
}
static void stop_buffer(source *s){if(s->running)s->capture_ended=now();if(s->buffer){iio_buffer_destroy(s->buffer);s->buffer=NULL;}if(s->i)iio_channel_disable(s->i);if(s->q)iio_channel_disable(s->q);s->running=0;}
static int restore(source *s){stop_buffer(s);int failed=0;if(s->fir_changed){
 if(ad9361_set_trx_fir_enable(s->phy,0)<0)failed=1;
 if(iio_channel_attr_write_longlong(s->tx_clock,"sampling_frequency",s->original_tx_rate)<0)failed=1;
 bool rx=false,tx=false;if(iio_channel_attr_read_bool(s->phy_ch,"filter_fir_en",&rx)<0||iio_channel_attr_read_bool(s->tx_clock,"filter_fir_en",&tx)<0||rx||tx)failed=1;
 long long actual;if(iio_channel_attr_read_longlong(s->tx_clock,"sampling_frequency",&actual)<0||actual!=s->original_tx_rate)failed=1;
 }if(write_controls(s,&s->original))failed=1;if(failed){s->restore_failed=1;return -1;}s->current=s->original;return 0;}
static int send_pair(source *s,const void *header,size_t header_size,const void *payload,size_t payload_size){
 struct iovec v[2]={{.iov_base=(void*)header,.iov_len=header_size},{.iov_base=(void*)payload,.iov_len=payload_size}};int current=0;
 while(current<2){ssize_t count=writev(s->client,v+current,2-current);if(count<0&&errno==EINTR&&!interrupted)continue;if(count<=0)return -1;s->sent_bytes+=count;size_t used=count;while(current<2&&used>=v[current].iov_len){used-=v[current].iov_len;++current;}if(current<2){v[current].iov_base=(char*)v[current].iov_base+used;v[current].iov_len-=used;}}
 return 0;
}
static long long bin_center(source *s,unsigned bin){int signed_bin=bin<BINS/2?(int)bin:(int)bin-BINS;return s->current.center+llround((double)signed_bin*s->current.rate/BINS);}
static int message(source *s,unsigned type,unsigned flags,unsigned stream,uint64_t first,unsigned format,unsigned bits,const void *payload,unsigned bytes){
 if(stream>BINS||bytes>1048576)return -1;
 unsigned char h[64]={0};memcpy(h,"RIQ1",4);put16(h+4,1);put16(h+6,64);put16(h+8,type);put16(h+10,flags);put64(h+12,s->generation);put32(h+20,stream);put64(h+24,s->sequences[stream]++);put64(h+32,first);put32(h+40,(unsigned)llround((double)s->current.rate/(BINS/2)));put64(h+44,stream?bin_center(s,stream-1):s->current.center);put32(h+52,bytes);put16(h+56,format);put16(h+58,bits);
 return send_pair(s,h,sizeof(h),payload,bytes);
}
static int emit_iq(void *opaque,unsigned bin,uint64_t first,unsigned count,unsigned flags,const iqs16 *iq){source *s=opaque;if(!(s->forward_mask&(1ULL<<bin))&&!s->gate.channels[bin].pinned)return 0;s->iq_samples+=count;return message(s,1,flags,bin+1,first,1,16,iq,count*sizeof(*iq));}
static int config(source *s,const char *request,int ok,const char *error){
 char json[32768];int used=snprintf(json,sizeof(json),"{\"input_center_hz\":%lld,\"input_rate_hz\":%lld,\"input_bandwidth_hz\":%lld,\"filter_delay_input_samples_times_two\":511,\"first_output_center_input_sample_times_two\":511,\"hardware_counter\":false,\"continuity_verified\":false,\"activity_calibrated\":false,\"activity_method\":\"experimental-global-median-energy\",\"receiver_state\":\"%s\",\"producer\":\"arm-pfb-experimental\",\"input_adc_bits\":%u,\"input_adc_shift\":%u,\"profile\":{\"max_input_rate_hz\":%d,\"bins\":%d,\"taps\":8,\"hop\":%d,\"pre_roll_ms\":200,\"hang_ms\":500,\"max_iq_packet_ms\":100},\"capabilities\":{\"controls_supported\":true,\"configure_supported\":false,\"pin_supported\":true,\"stop_supported\":true},\"psd\":{\"bins\":%d,\"center_hz\":%lld,\"span_hz\":%lld,\"unit\":\"dBFS\"},\"streams\":[",s->current.center,s->current.rate,s->current.bandwidth,s->running?"running":"stopped",s->bits,s->shift,RATE_LIMIT,BINS,BINS/2,BINS,s->current.center,s->current.rate);
 if(s->running)for(unsigned k=0;k<BINS;++k){int n=snprintf(json+used,sizeof(json)-used,"%s{\"stream_id\":%u,\"center_hz\":%lld,\"target_hz\":%lld,\"sample_rate_num_hz\":%lld,\"sample_rate_den\":%d,\"protocol\":\"Unknown\",\"pinned\":%s,\"iq_forwarded\":%s}",k?",":"",k+1,bin_center(s,k),bin_center(s,k),s->current.rate,BINS/2,s->gate.channels[k].pinned?"true":"false",((s->forward_mask&(1ULL<<k))||s->gate.channels[k].pinned)?"true":"false");if(n<0||(size_t)n>=sizeof(json)-used)return -1;used+=n;}
 int n=snprintf(json+used,sizeof(json)-used,"]%s",request?",\"control_ack\":{":"}");if(n<0||(size_t)n>=sizeof(json)-used)return -1;used+=n;
 if(request){n=snprintf(json+used,sizeof(json)-used,"\"request_id\":\"%s\",\"ok\":%s,\"error\":\"%s\"}}",request,ok?"true":"false",error?error:"");if(n<0||(size_t)n>=sizeof(json)-used)return -1;used+=n;}
 return message(s,4,0,0,0,3,0,json,used);
}
static int json_eq(const char *j,const jsmntok_t *t,const char *v){return t->type==JSMN_STRING&&(size_t)(t->end-t->start)==strlen(v)&&!memcmp(j+t->start,v,strlen(v));}
static const jsmntok_t *field(const char *j,const jsmntok_t *t,int count,const char *name){for(int k=1;k+1<count;++k)if(t[k].parent==0&&json_eq(j,t+k,name))return t+k+1;return NULL;}
static int command(source *s){
 unsigned char h[64];ssize_t ready=recv(s->client,h,sizeof(h),MSG_PEEK|MSG_DONTWAIT);if(ready<0&&(errno==EAGAIN||errno==EWOULDBLOCK))return 0;if(ready==0)return -1;if(ready<0)return -1;if(ready<64)return 0;
 unsigned bytes=get32(h+52);if(memcmp(h,"RIQ1",4)||h[4]!=1||h[5]!=0||h[6]!=64||h[7]!=0||h[8]!=6||h[9]!=0||h[56]!=3||h[57]!=0||bytes>MAX_CONTROL)return -1;
 unsigned char packet[64+MAX_CONTROL+1];ready=recv(s->client,packet,64+bytes,MSG_PEEK|MSG_DONTWAIT);if(ready<0)return -1;if((unsigned)ready<64+bytes)return 0;
 ssize_t read=recv(s->client,packet,64+bytes,0);if(read!=(ssize_t)(64+bytes))return -1;char *j=(char*)packet+64;j[bytes]=0;
 jsmn_parser parser;jsmntok_t tokens[128];jsmn_init(&parser);int count=jsmn_parse(&parser,j,bytes,tokens,128);if(count<1||tokens[0].type!=JSMN_OBJECT)return -1;
 const jsmntok_t *id=field(j,tokens,count,"request_id"),*action=field(j,tokens,count,"action");char request[65];if(!id||!action||id->type!=JSMN_STRING||id->end-id->start<1||id->end-id->start>64)return -1;
 unsigned len=id->end-id->start;for(unsigned k=0;k<len;++k){char c=j[id->start+k];if(!((c>='a'&&c<='z')||(c>='A'&&c<='Z')||(c>='0'&&c<='9')||c=='_'||c=='-'||c=='.'))return -1;}memcpy(request,j+id->start,len);request[len]=0;
 if(json_eq(j,action,"get_config"))return config(s,request,1,NULL);
 if(json_eq(j,action,"stop")){int ok=restore(s)==0;return config(s,request,ok,ok?NULL:"original receive controls failed readback");}
 if(json_eq(j,action,"pin")){
   const jsmntok_t *stream=field(j,tokens,count,"stream_id"),*pinned=field(j,tokens,count,"pinned");if(!stream||!pinned||stream->type!=JSMN_PRIMITIVE||pinned->type!=JSMN_PRIMITIVE)return config(s,request,0,"pin requires stream_id and boolean pinned");
   char number[24];len=stream->end-stream->start;if(!len||len>=sizeof(number))return -1;memcpy(number,j+stream->start,len);number[len]=0;char *end;unsigned long index=strtoul(number,&end,10);if(*end||index<1||index>BINS)return config(s,request,0,"unknown stream");
   int pin;if(pinned->end-pinned->start==4&&!memcmp(j+pinned->start,"true",4))pin=1;else if(pinned->end-pinned->start==5&&!memcmp(j+pinned->start,"false",5))pin=0;else return config(s,request,0,"pinned must be boolean");if(pin&&!s->gate.channels[index-1].pinned&&!(s->forward_mask&(1ULL<<(index-1)))){gate_channel *ch=s->gate.channels+index-1;ch->active=0;ch->pending=0;ch->last_sent=0;}s->gate.channels[index-1].pinned=pin;return config(s,request,1,NULL);
 }
 return config(s,request,0,"unsupported control for experimental ARM profile");
}
static int discovery(source *s){
 float psd[BINS];for(unsigned j=0;j<BINS;++j){unsigned bin=(j+BINS/2)%BINS;float power=s->gate.channels[bin].power;psd[j]=10*log10f(fmaxf(power/(s->fullscale*s->fullscale),1e-15f));}
 if(message(s,2,0,0,s->gate.frame,2,32,psd,sizeof(psd)))return -1;
 for(unsigned k=0;k<BINS;++k){gate_channel *c=s->gate.channels+k;char j[256];int n=snprintf(j,sizeof(j),"{\"stream_id\":%u,\"rf_active\":%s,\"level_db\":%.3f,\"sample_index\":%"PRIu64",\"forwarded\":%s}",k+1,c->rf_active?"true":"false",10*log10f(fmaxf(c->power/(s->fullscale*s->fullscale),1e-15f)),s->gate.frame,(c->active&&((s->forward_mask&(1ULL<<k))||c->pinned))?"true":"false");if(message(s,3,0,k+1,s->gate.frame,3,0,j,n))return -1;}
 return 0;
}
int main(int argc,char **argv){
 long long rate=800000,bandwidth=800000,center=454216998;unsigned port=9010;double seconds=0;unsigned pin=0;uint64_t forward_mask=0;int c;
 static struct option opts[]={{"rate",1,0,'r'},{"bandwidth",1,0,'b'},{"center",1,0,'c'},{"port",1,0,'p'},{"seconds",1,0,'s'},{"pin-stream",1,0,'P'},{"forward-streams",1,0,'F'},{"help",0,0,'h'},{0,0,0,0}};
 while((c=getopt_long(argc,argv,"",opts,NULL))!=-1){if(c=='F'){const char *arg=optarg;unsigned selected=0;while(*arg){char *end;errno=0;unsigned long id=strtoul(arg,&end,10);if(errno||end==arg||id<1||id>BINS||(*end&&*end!=',')||++selected>8){fprintf(stderr,"forward-streams requires at most8 IDs1..64\n");return 2;}forward_mask|=1ULL<<(id-1);arg=*end?end+1:end;}continue;}char *end=NULL;errno=0;double v=c=='h'?0:strtod(optarg,&end);if(c!='h'&&(errno||!optarg[0]||*end||!isfinite(v)||v<0||v>1e12)){fprintf(stderr,"Invalid numeric argument\n");return 2;}
 switch(c){case'r':rate=v;break;case'b':bandwidth=v;break;case'c':center=v;break;case'p':if(v<1||v>65535)return 2;port=v;break;case's':seconds=v;break;case'P':if(v<1||v>BINS)return 2;pin=v;break;default:fprintf(stderr,"Usage: pluto-channel-source [--rate 800000] [--bandwidth 800000] [--center 454216998] [--port 9010] [--seconds 30] [--pin-stream 1] [--forward-streams 1,50]\nIQforwardingrequiresconfiguredstreamIDsorhostpin;all64activity/PSDremainavailable.\nExperimental ARM profile: maximum800ksps;notfull20MHz. Captures RX only after one TCPclient connects; restores original controls on stop/disconnect. Exit0success,1failure,2usage.\n");return c=='h'?0:2;}}
 if(optind!=argc||rate<520834||rate>RATE_LIMIT||bandwidth<200000||bandwidth>rate||center<325000000||center>3800000000LL||seconds<0||seconds>300){fprintf(stderr,"Outside experimental ARM profile limits\n");return 2;}
 source s;memset(&s,0,sizeof(s));s.client=s.listener=-1;s.forward_mask=forward_mask;signal(SIGINT,on_signal);signal(SIGTERM,on_signal);signal(SIGPIPE,SIG_IGN);s.generation=(uint64_t)(now()*1e9);int result=1,changed=0;
 s.listener=socket(AF_INET,SOCK_STREAM,0);if(s.listener<0)goto done;int one=1;setsockopt(s.listener,SOL_SOCKET,SO_REUSEADDR,&one,sizeof(one));struct sockaddr_in address={.sin_family=AF_INET,.sin_port=htons(port),.sin_addr={.s_addr=INADDR_ANY}};
 if(bind(s.listener,(struct sockaddr*)&address,sizeof(address))||listen(s.listener,1)){perror("listen");goto done;}fprintf(stderr,"RIQ1 ARM source waiting on TCP:%u; radio unopened\n",port);
 s.client=accept(s.listener,NULL,NULL);if(s.client<0)goto done;struct timeval timeout={.tv_sec=0,.tv_usec=250000};setsockopt(s.client,SOL_SOCKET,SO_SNDTIMEO,&timeout,sizeof(timeout));int sendbuf=262144;setsockopt(s.client,SOL_SOCKET,SO_SNDBUF,&sendbuf,sizeof(sendbuf));
 s.context=iio_create_local_context();if(!s.context){perror("local IIO context");goto done;}s.phy=iio_context_find_device(s.context,"ad9361-phy");s.rx=iio_context_find_device(s.context,"cf-ad9361-lpc");if(!s.phy||!s.rx)goto done;
 s.phy_ch=iio_device_find_channel(s.phy,"voltage0",false);s.tx_clock=iio_device_find_channel(s.phy,"voltage0",true);s.lo=iio_device_find_channel(s.phy,"altvoltage0",true);s.i=iio_device_find_channel(s.rx,"voltage0",false);s.q=iio_device_find_channel(s.rx,"voltage1",false);if(!s.phy_ch||!s.lo||!s.i||!s.q||read_controls(&s,&s.original))goto done;
 bool rx_fir=false,tx_fir=false;
 if(!s.tx_clock||iio_channel_attr_read_bool(s.phy_ch,"filter_fir_en",&rx_fir)<0||iio_channel_attr_read_bool(s.tx_clock,"filter_fir_en",&tx_fir)<0||iio_channel_attr_read_longlong(s.tx_clock,"sampling_frequency",&s.original_tx_rate)<0){fprintf(stderr,"Cannot snapshot RFIC FIR/clock state\n");goto done;}
 if(rx_fir||tx_fir){fprintf(stderr,"Refusing to replace an existing active custom FIR\n");goto done;}
 changed=1;s.fir_changed=1;if(ad9361_set_bb_rate(s.phy,rate)<0){fprintf(stderr,"ADI low-rate FIR setup failed\n");goto done;}
 s.current=s.original;s.current.rate=rate;s.current.bandwidth=bandwidth;s.current.center=center;changed=1;if(write_controls(&s,&s.current)){fprintf(stderr,"RX configuration/readback failed\n");goto done;}
 const struct iio_data_format *format=iio_channel_get_data_format(s.i);if(format->length!=16||!format->is_signed||format->is_be||format->repeat>1||format->bits>16||format->bits+format->shift>16)goto done;s.bits=format->bits;s.shift=format->shift;s.fullscale=(float)(1u<<(s.bits+s.shift-1));
 if(pfb_init(&s.channelizer,BINS,8)||gate_init(&s.gate,BINS,(unsigned)(0.2*rate/(BINS/2)),(unsigned)(0.5*rate/(BINS/2)),8,32768/s.fullscale,emit_iq,&s))goto done;
 if(pin)s.gate.channels[pin-1].pinned=1;
 iio_channel_enable(s.i);iio_channel_enable(s.q);if(iio_device_set_kernel_buffers_count(s.rx,4)<0)goto done;s.buffer=iio_device_create_buffer(s.rx,BLOCK_SAMPLES,false);if(!s.buffer){perror("RX buffer");goto done;}if(iio_buffer_step(s.buffer)!=4||(char*)iio_buffer_first(s.buffer,s.q)-(char*)iio_buffer_first(s.buffer,s.i)!=2)goto done;
 s.running=1;s.started=now();if(config(&s,NULL,1,NULL))goto done;double next_discovery=0;
 while(!interrupted&&s.running&&(!seconds||now()-s.started<seconds)){
   if(command(&s))goto done;
   if(!s.running)break;
   ssize_t bytes=iio_buffer_refill(s.buffer);if(bytes<0){fprintf(stderr,"RX refill failed:%zd\n",bytes);goto done;}
   double processing=now();int16_t *raw=iio_buffer_first(s.buffer,s.i);unsigned samples=bytes/4;if(bytes%4||samples%(BINS/2))goto done;
   for(unsigned j=0;j<samples;j+=BINS/2)if(pfb_push(&s.channelizer,raw+2*j)&&gate_push(&s.gate,s.channelizer.fft))goto done;
   s.input_samples+=samples;s.processing_seconds+=now()-processing;if(now()-processing>(double)samples/s.current.rate)++s.late_buffers;
   if(now()>=next_discovery){if(discovery(&s))goto done;next_discovery=now()+0.1;}
 }
 result=0;
 done:
 if(s.running&&!result&&gate_flush(&s.gate))result=1;
 if(changed&&restore(&s)){fprintf(stderr,"FAILED restoring original RX controls\n");result=1;}
 if(s.context)iio_context_destroy(s.context);
 pfb_destroy(&s.channelizer);gate_destroy(&s.gate);if(s.client>=0)close(s.client);if(s.listener>=0)close(s.listener);
 printf("{\"status\":\"%s\",\"input_samples\":%"PRIu64",\"iq_samples_sent\":%"PRIu64",\"wire_bytes\":%"PRIu64",\"processing_seconds\":%.6f,\"capture_wall_seconds\":%.6f,\"sample_seconds\":%.6f,\"throughput_ratio\":%.6f,\"late_buffers\":%u,\"hardware_continuity_verified\":false,\"original_controls_restored\":%s}\n",result?"error":"stopped",s.input_samples,s.iq_samples,s.sent_bytes,s.processing_seconds,s.started?s.capture_ended-s.started:0,(double)s.input_samples/rate,(s.started&&s.capture_ended>s.started)?((double)s.input_samples/rate)/(s.capture_ended-s.started):0,s.late_buffers,changed&&!s.restore_failed?"true":"false");return result;
}
