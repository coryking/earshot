#include "gate.h"
#include <assert.h>
#include <stdio.h>
static unsigned seen[16][400];static unsigned preroll[16];static unsigned starts[16];static unsigned ends[16];
static int emit(void *unused,unsigned bin,uint64_t first,unsigned count,unsigned flags,const iqs16 *iq){
 (void)unused;assert(bin<16);assert(first+count<=400);
 if(flags&1)++starts[bin];
 if(flags&2)++ends[bin];
 if(flags&4)preroll[bin]+=count;
 for(unsigned j=0;j<count;++j){assert(!seen[bin][first+j]);seen[bin][first+j]=1;assert(iq[j].re>=0);}
 return 0;
}
int main(void){
 activity_gate g;assert(gate_init(&g,16,4,3,8,16,emit,NULL)==0);g.channels[0].pinned=1;
 for(unsigned frame=0;frame<300;++frame){iqf v[16];for(unsigned k=0;k<16;++k)v[k]=(iqf){1,0};if((frame>=50&&frame<60)||(frame>=200&&frame<210))v[3].re=10;assert(gate_push(&g,v)==0);}
 assert(gate_flush(&g)==0);
 for(unsigned j=0;j<300;++j){assert(seen[0][j]);assert(!seen[1][j]);}
 for(unsigned j=50;j<60;++j)assert(seen[3][j]);
 for(unsigned j=200;j<210;++j)assert(seen[3][j]);
 assert(preroll[3]==6);assert(starts[3]==2);assert(ends[3]==2);assert(starts[0]==1);
 gate_destroy(&g);puts("gate: pinned continuous; first bursts intact; two pre-rolls; no duplicates; idle suppressed");return 0;
}
