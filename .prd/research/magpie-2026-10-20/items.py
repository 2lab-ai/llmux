import json,sys,re
iss=json.load(open(sys.argv[1]+'/classified.json'))
au={json.loads(l)['n']:json.loads(l)['u'] for l in open(sys.argv[1]+'/authors.jsonl')}
T=lambda r:(r['t']+' '+r['body']).lower()
ITEMS={
 'I1 api-key providers':r'deepseek|kimi|moonshot|zhipu|智谱|glm|minimax|qwen|百炼|ollama|lm ?studio|openrouter|中转|relay',
 'I2 cross-provider fallback':r'failover|故障转移|fallback|备用|capacity|429|回退',
 'I3 quota display':r'用量|额度|quota|余额|剩余|状态栏|menu ?bar|capsule|胶囊|小窗口',
 'I4 warm-up / reset credits':r'预热|warm.?up|prewarm|错峰|重置卡|reset card|redeem|兑换',
 'I5 docker / headless server':r'docker|\bnas\b|群晖|headless|systemd|daemon install|中心化|服务端|服务器|home ?server|\btls\b',
 'I6 windows':r'windows|win11|win10|wsl',
 'I7 web search':r'联网|web.?search|搜索',
 'I8 third harness (other agents only)':None,
}
cc=r'codex|claude code|claude-code'; oth=r'opencode|\bpi\b|workbuddy|codebuddy|cursor|\bzed\b|kimi code|droid|factory|claude desktop|cline|vscode|\bdsh\b|deepseek harness|grok app|snow|ante\b|devin|gemini cli'
for k,p in ITEMS.items():
    m=[r for r in iss if (re.search(p,T(r)) if p else (re.search(oth,T(r)) and not re.search(cc,T(r))))]
    print(f"{k}: issues={len(m)} authors={len({au[r['n']] for r in m})} feat={sum(r['kind']=='feature' for r in m)}")
