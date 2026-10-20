import json,sys,re,collections
iss=json.load(open(sys.argv[1]+'/classified.json'))
T=lambda r:(r['t']+' '+r['body']).lower()
pos=r'好用|感谢|谢谢|太棒|很棒|喜欢|神器|离不开|强大|牛逼|点赞|\blove\b|\bgreat\b|\bawesome\b|\bamazing\b|\bthanks?\b|\bappreciate'
thanks=r'感谢|谢谢|\bthanks?\b|\bappreciate'
strong=r'好用|太棒|很棒|喜欢|神器|离不开|强大|牛逼|点赞|\blove\b|\bgreat\b|\bawesome\b|\bamazing\b'
m=[r for r in iss if re.search(pos,T(r))]
s=[r for r in iss if re.search(strong,T(r))]
print(f"praise-word issues={len(m)}; thanks-only={len(m)-len(s)}; non-thanks praise={len(s)}")
area=collections.Counter(c for r in iss for c in r['cats'])
for label,group in (('any praise word',m),('non-thanks praise',s)):
    c=collections.Counter(cat for r in group for cat in r['cats'])
    print(label+':',', '.join(f"{k} {v}/{area[k]}={100*v/area[k]:.1f}%" for k,v in c.most_common(8)))
# windows refined: drop time-window phrases first
W=r'(5h|5-hour|five.hour|quota|usage|reset|rolling|time|weekly|7d|7-day|context|both|two|these|those|the|limit|rate|billing|sliding)[ -]windows?'
au={json.loads(l)['n']:json.loads(l)['u'] for l in open(sys.argv[1]+'/authors.jsonl')}
w=[r for r in iss if re.search(r'windows|win1[01]|wsl',re.sub(W,' ',T(r)))]
print(f"windows refined: issues={len(w)} authors={len({au[r['n']] for r in w})}")
