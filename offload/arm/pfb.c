#include "pfb.h"
#include <math.h>
#include <stdlib.h>
#include <string.h>
#ifndef M_PI
#define M_PI 3.14159265358979323846
#endif
int pfb_init(pfb *p, unsigned n, unsigned taps) {
    memset(p,0,sizeof(*p));
    if (n<16 || n>8192 || (n&(n-1)) || taps<4 || taps>32) return -1;
    p->n=n; p->taps=taps; p->hop=n/2; p->length=n*taps;
    p->history=calloc(p->length+n,sizeof(iqf)); p->fft=calloc(n,sizeof(iqf));
    p->roots=calloc(n/2,sizeof(iqf)); p->coeff=calloc(p->length,sizeof(float));
    p->reverse=calloc(n,sizeof(unsigned));
    if (!p->history || !p->fft || !p->roots || !p->coeff || !p->reverse) { pfb_destroy(p); return -1; }
    unsigned bits=0; for(unsigned j=n;j>1;j>>=1) ++bits;
    for(unsigned j=0;j<n;++j) { unsigned r=0,k=j; for(unsigned b=0;b<bits;++b){r=(r<<1)|(k&1);k>>=1;} p->reverse[j]=r; }
    for(unsigned j=0;j<n/2;++j){p->roots[j].re=cosf(-2*M_PI*j/n);p->roots[j].im=sinf(-2*M_PI*j/n);}
    double sum=0;
    for(unsigned j=0;j<p->length;++j){
        double x=j-(p->length-1)*0.5;
        double v=x==0?0.9/n:sin(M_PI*0.9*x/n)/(M_PI*x);
        double w=0.42-0.5*cos(2*M_PI*j/(p->length-1))+0.08*cos(4*M_PI*j/(p->length-1));
        p->coeff[j]=v*w;sum+=p->coeff[j];
    }
    for(unsigned j=0;j<p->length;++j)p->coeff[j]/=sum;
    return 0;
}
void pfb_destroy(pfb *p){free(p->history);free(p->fft);free(p->roots);free(p->coeff);free(p->reverse);memset(p,0,sizeof(*p));}
uint64_t pfb_center_times_two(const pfb *p){return 2*(p->seen-1)-(p->length-1);}
int pfb_push(pfb *p,const int16_t *samples){
    for(unsigned j=0;j<p->hop;++j){p->history[p->write].re=samples[2*j];p->history[p->write].im=samples[2*j+1];if(p->write<p->n)p->history[p->write+p->length]=p->history[p->write];if(++p->write==p->length)p->write=0;}
    p->seen+=p->hop;if(p->seen<p->length)return 0;
    memset(p->fft,0,p->n*sizeof(iqf));
    /* At a hop boundary the oldest sample is p->write. Each phase is one
       low-rate FIR. Separating wraps keeps the inner multiply-add contiguous. */
    for(unsigned t=0;t<p->taps;++t){
        unsigned src=(p->write+t*p->n)%p->length;
        const float *h=p->coeff+t*p->n;
        for(unsigned j=0;j<p->n;++j){p->fft[j].re+=p->history[src+j].re*h[j];p->fft[j].im+=p->history[src+j].im*h[j];}
    }
    for(unsigned j=0;j<p->n;++j){unsigned r=p->reverse[j];if(r>j){iqf tmp=p->fft[j];p->fft[j]=p->fft[r];p->fft[r]=tmp;}}
    for(unsigned len=2;len<=p->n;len<<=1){
        unsigned half=len/2,step=p->n/len;
        for(unsigned start=0;start<p->n;start+=len)for(unsigned j=0;j<half;++j){
            iqf a=p->fft[start+j],b=p->fft[start+j+half],w=p->roots[j*step];
            iqf v={b.re*w.re-b.im*w.im,b.re*w.im+b.im*w.re};
            p->fft[start+j]=(iqf){a.re+v.re,a.im+v.im};p->fft[start+j+half]=(iqf){a.re-v.re,a.im-v.im};
        }
    }
    /* Remove each bin's known centre-frequency rotation between overlapping
       frames. Without this, odd bins alternate phase and cease to be baseband. */
    if(((p->seen-p->length)/p->hop)&1)for(unsigned j=1;j<p->n;j+=2){p->fft[j].re=-p->fft[j].re;p->fft[j].im=-p->fft[j].im;}
    ++p->frames;return 1;
}
