import json,sys,re,collections
D=sys.argv[1]
iss=json.load(open(D+'/user_issues.json'))
au={json.loads(l)['n']:json.loads(l) for l in open(D+'/authors.jsonl')}
CATS={
 'providers':r'供应商|提供商|provider|接入|支持.{0,12}(订阅|套餐|coding ?plan|api)|coding ?plan|codeplan|bedrock|vertex|kimi|zhipu|智谱|glm|qoder|trae|zcode|deepseek|minimax|qwen|百炼|bailian|mimo|openrouter|anyrouter|packy|fireworks|siliconflow|硅基|火山|volc|doubao|豆包|ollama|lm ?studio|omlx|opencode zen|zen free|cline|nvidia|azure|groq|cerebras|中转|relay|new ?api|one ?api|蚂蚁|longcat|sensenova|stepfun|阶跃|moonshot|copilot',
 'agents':r'agent|claude code|claude desktop|codex|gemini cli|cursor|zed|\bpi\b|opencode|kimi code|droid|factory|grok app|snow|ante|devin|workbuddy|codebuddy|vscode|vscodium|qwen code|crush|amp\b|goose|warp|roo|kilo|acp|dsh|harness|omp\b|minis',
 'routing':r'路由|route|routing|failover|故障转移|切换|fallback|intent|意图|分流|自动选|capacity|负载|轮询|round.?robin|429|smart|优先|备用|group|组',
 'usage_quota':r'用量|额度|usage|quota|余额|balance|剩余|limit|限额|计费|cost|\$|usd|费用|token ?统计|热力图|heatmap|速度|speed|tps|统计|stats',
 'subscription_auth':r'订阅|subscription|账号|account|登录|login|oauth|setup-token|keychain|token|refresh|保活|预热|warm|重置卡|reset|team ?版|多账号',
 'free_credits':r'签到|check.?in|赠送|免费额度|free|gift|领取|白嫖|公益',
 'compaction_context':r'压缩|compact|上下文|context|max_model_len|1m\b|200k|窗口大小',
 'protocol_compat':r'\b400\b|\b502\b|\b500\b|tool_call|tool call|reasoning|thinking|思考|sse|stream|invalid|schema|翻译|translate|responses|chat completions|protocol|协议|structured|function',
 'web_image_media':r'联网|搜索|search|生图|图片|图像|image|视频|video|生视频|tts|语音|embedding|embeding|rerank|向量',
 'library_mcp_skills':r'资源库|library|mcp|skill|技能|agents\.md|插件市场|指令文件',
 'sessions':r'会话|session|历史|history|resume|恢复',
 'platform_install':r'windows|win11|wsl|linux|docker|flatpak|portable|便携|安装|install|更新|update|升级|自启|autostart|托盘|tray|arm|homebrew|brew|npm',
 'remote_lan':r'远程|远端|remote|局域网|lan\b|中心化|central|server|服务器|多台|多机|sync|同步|ios|iphone|手机',
 'privacy':r'脱敏|隐私|privacy|打码|mask|redact|马赛克',
 'ui_ux':r'ui|界面|窗口|window|按钮|button|滚动|scroll|显示|display|图标|icon|菜单栏|menu ?bar|status ?bar|状态栏|浮层|弹窗|dark|主题|layout|布局|小窗',
 'caching':r'缓存|cache|prompt.?cache|命中',
 'plugins':r'插件|plugin',
 'docs_i18n':r'教程|文档|docs|tutorial|release notes|中文|english|说明|怎么|如何|how to|\?$|？$',
}
def labels(r):
    t=(r['t']+' \n'+r['body']).lower()
    tl=r['t'].lower()
    out=[c for c,p in CATS.items() if re.search(p,tl)]
    if not out: out=[c for c,p in CATS.items() if re.search(p,t)][:2]
    return out or ['unclassified']
def kind(r):
    t=r['t'].lower()
    if re.search(r'\[?(feat|feature|功能|建议|需求|request|enhancement|希望|能否|能不能|可以.{0,6}吗|支持|增加|添加|add |support)',t): return 'feature'
    if re.search(r'bug|报错|错误|失败|无法|不能|error|fail|broken|crash|panic|\b50\d\b|\b4\d\d\b|异常|不显示|丢|wrong|doesn.t|not ',t): return 'bug'
    return 'other'
agg=collections.defaultdict(lambda:{'n':0,'authors':set(),'c':0,'feature':0,'bug':0,'other':0,'open':0})
for r in iss:
    k=kind(r); r['kind']=k; r['cats']=labels(r)
    for c in r['cats']:
        a=agg[c]; a['n']+=1; a['authors'].add(au[r['n']]['u']); a['c']+=r['c']; a[k]+=1; a['open']+=r['s']=='open'
print("unclassified",agg['unclassified']['n'],"/",len(iss)); print("kinds",collections.Counter(r['kind'] for r in iss))
print(f"{'area':22}{'issues':>7}{'authors':>8}{'cmts':>6}{'feat':>6}{'bug':>5}{'open':>6}")
for c,a in sorted(agg.items(),key=lambda x:-len(x[1]['authors'])):
    print(f"{c:22}{a['n']:>7}{len(a['authors']):>8}{a['c']:>6}{a['feature']:>6}{a['bug']:>5}{a['open']:>6}")
json.dump(iss,open(D+'/classified.json','w'),ensure_ascii=False)
