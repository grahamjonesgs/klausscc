import re,collections
W={}
for l in open('run/run.err',errors='ignore'):
    m=re.match(r'CACHESIM w(\d+) I (.*?)\s+(?:\(board\) )?misses\s+(\d+) accesses\s+(\d+) per1k\s+([\d.]+)',l)
    if m: W.setdefault(int(m[1]),collections.OrderedDict())[m[2].strip()]=int(m[3]); continue
    m=re.match(r'CACHESIM w(\d+) uITLB (\d+) entries, (cleared|kept) .*?misses\s+(\d+)',l)
    if m: W.setdefault(int(m[1]),collections.OrderedDict())['uITLB %s %s'%(m[2],m[3])]=int(m[4]); continue
    m=re.match(r'CACHESIM window (\d+): (\d+) instr',l)
    if m: W.setdefault(int(m[1]),collections.OrderedDict())['instr']=int(m[2])
# board: cycles, instr, ic_miss, ic_fill_cycles
B={'py0':(3,229.0e6,49.2e6,10.68e6,.225,.28,.098),'pyloop':(2,482.2e6,143.0e6,27.89e6,.149,.24,.145),'sqlite':(5,881.3e6,265.2e6,34.98e6,.23,.28,.14)}
shapes=[k for k in W[3] if not k.startswith('uITLB') and k!='instr']
print('shape',*['%s per1k | saved Mcyc | %%'%n for n in B],sep=' | ')
for s in shapes:
    row=[s]
    for n,(w,cyc,ins,icm,fillf,ifm,_) in B.items():
        d=W[w]; mb=d[shapes[0]]; ms=d[s]
        per1k=1000*ms/d['instr']
        fill=fillf*cyc
        saved=(mb-ms)*fill/mb
        row.append('%.1f | %.1f | %.1f%%'%(per1k,saved/1e6,100*saved/cyc))
    print(*row,sep=' | ')
print()
print('validation')
for n,(w,cyc,ins,icm,fillf,ifm,_) in B.items():
    d=W[w]; mb=d[shapes[0]]; u=d['uITLB 1 cleared']
    print(n,'board ic_miss %.2fM (%.0f/1k)'%(icm/1e6,1000*icm/ins),'model %.2fM (%.1f/1k)'%(mb/1e6,1000*mb/d['instr']),'ratio %.2f'%(icm/mb),'uITLB1 %.2fM'%(u/1e6),'k=(B-m)/u=%.2f'%((icm-mb)/u),'pred(k=2.5) %.2fM %+.1f%%'%((mb+2.5*u)/1e6,100*((mb+2.5*u)/icm-1)), 'cyc/fill=%.1f'%(fillf*cyc/mb), 'if_miss-fill cyc per uITLB miss %.1f'%((ifm-fillf)*cyc/u))
print()
print('uITLB what-if (misses per 1k instr; saved Mcyc at 4 cyc/miss)')
for n,(w,cyc,ins,icm,fillf,ifm,_) in B.items():
    d=W[w]
    print(n,'; '.join('%s: %.1f/1k saved %.1fM (%.1f%%)'%(k[6:],1000*d[k]/d['instr'],4*(d['uITLB 1 cleared']-d[k])/1e6,100*4*(d['uITLB 1 cleared']-d[k])/cyc) for k in d if k.startswith('uITLB')))
