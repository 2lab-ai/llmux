import json,sys,re
iss=json.load(open(sys.argv[1]+'/classified.json'))
au={json.loads(l)['n']:json.loads(l)['u'] for l in open(sys.argv[1]+'/authors.jsonl')}
T=lambda r:(r['t']+' '+r['body']).lower()
ITEMS={
 'I1 api-key providers':r'deepseek|kimi|moonshot|zhipu|智谱|glm|minimax|qwen|百炼|ollama|lm ?studio|openrouter|中转|relay',
 'I1a direct vendor keys / coding plans':r'deepseek|kimi|moonshot|zhipu|智谱|glm|minimax|qwen|百炼|dashscope|mimo|coding ?plan|codeplan|token ?plan',
 'I1b local models + relays':r'ollama|lm ?studio|omlx|llama\.cpp|vllm|本地模型|local model|中转|relay|new ?api|one ?api|anyrouter|packy',
 'I2 cross-provider fallback':r'failover|故障转移|fallback|备用|capacity|429|回退',
 'I3 quota display':r'用量|额度|quota|余额|剩余|状态栏|menu ?bar|capsule|胶囊|小窗口',
 'I4 warm-up / reset credits':r'预热|warm.?up|prewarm|错峰|重置卡|reset card|redeem|兑换',
 'I5 docker / headless server':r'docker|\bnas\b|群晖|headless|systemd|daemon install|中心化|服务端|服务器|home ?server|\btls\b',
 'I6 windows':r'windows|win11|win10|wsl',
 'I7 web search':r'联网|web.?search|搜索',
 'I8 third harness (other agents only)':None,
 'I8a Messages/Responses-capable agents only (Pi, Claude Desktop, Cursor local, DeepSeek harness, droid)':'A',
 'I8b agents magpie wires through Chat only (OpenCode, WorkBuddy/CodeBuddy, Kimi Code, Cline)':'B',
 'I9 dashboard screens named in the title (page, window, panel, tray, menu bar)':'UI',
}
cc=r'codex|claude code|claude-code'; oth=r'opencode|\bpi\b|workbuddy|codebuddy|cursor|\bzed\b|kimi code|droid|factory|claude desktop|cline|vscode|\bdsh\b|deepseek harness|grok app|snow|ante\b|devin|gemini cli'
for k,p in ITEMS.items():
    if p=='A': m=[r for r in iss if re.search(r'\bpi\b|pi coding agent|claude desktop|cursor (cli|agent|local)|cursorlocal|\bdsh\b|deepseek harness|\bdroid\b|factory droid',T(r)) and not re.search(cc,T(r))]
    elif p=='B': m=[r for r in iss if re.search(r'opencode|workbuddy|codebuddy|kimi code|kimi-code|kimicode|\bcline\b',T(r)) and not re.search(cc,T(r))]
    elif p=='UI': m=[r for r in iss if re.search(r'页面|界面|面板|主窗口|窗口|dashboard|\bgui\b|web ?ui|\bpage\b|路由页|用量页|设置页|会话页|tray|托盘|悬浮窗|小窗口|menu ?bar|菜单栏|状态栏',r['t'].lower())]
    else: m=[r for r in iss if (re.search(p,T(r)) if p else (re.search(oth,T(r)) and not re.search(cc,T(r))))]
    print(f"{k}: issues={len(m)} authors={len({au[r['n']] for r in m})} feat={sum(r['kind']=='feature' for r in m)}")
