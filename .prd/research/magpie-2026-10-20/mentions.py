import json,sys,re,collections
iss=json.load(open(sys.argv[1]+'/classified.json'))
def cnt(table):
    out=[]
    for name,p in table.items():
        m=[r for r in iss if re.search(p,(r['t']+' '+r['body']).lower())]
        f=sum(1 for r in m if r['kind']=='feature')
        out.append((len(m),f,sum(r['c'] for r in m),name))
    for x in sorted(out,reverse=True): print(f"  {x[3]:22} issues={x[0]:4} feat={x[1]:3} cmts={x[2]}")
print("CLIENT AGENTS"); cnt({'Codex':r'codex','Claude Code':r'claude code|claude-code|\bcc\b','Claude Desktop':r'claude desktop','Gemini CLI':r'gemini cli|gemini-cli','Cursor':r'cursor','Zed':r'\bzed\b','OpenCode':r'opencode','Pi':r'\bpi\b','Kimi Code':r'kimi code|kimi-code|kimicode','Factory droid':r'droid|factory','DeepSeek harness(dsh)':r'\bdsh\b|deepseek harness','Copilot/VSCode':r'vscode|vscodium|copilot chat','Cline':r'\bcline\b','Grok':r'grok','WorkBuddy/CodeBuddy':r'workbuddy|codebuddy','Antigravity':r'antigravity','Qwen Code':r'qwen code|qwen-code'})
print("PROVIDERS/BACKENDS"); cnt({'DeepSeek':r'deepseek','Kimi/Moonshot':r'kimi|moonshot','Zhipu/GLM':r'zhipu|智谱|glm','Claude subscription':r'claude.{0,6}(订阅|subscription|max|pro)|订阅.{0,4}claude','ChatGPT/Codex subscription':r'chatgpt|codex.{0,6}(订阅|subscription|plus|pro|额度)','GitHub Copilot':r'copilot','Antigravity/Gemini':r'antigravity|gemini','MiniMax':r'minimax','Qwen/Bailian':r'qwen|百炼|bailian|dashscope','Qoder':r'qoder','Trae':r'trae','ZCode':r'zcode','OpenRouter':r'openrouter','OpenCode Zen':r'zen','AWS Bedrock':r'bedrock','Ollama/local':r'ollama|lm ?studio|omlx|本地模型|local model','MiMo (Xiaomi)':r'mimo','Grok/xAI':r'grok|xai','Relay/中转':r'中转|new ?api|one ?api|relay|anyrouter|packy'})
print("LOVE / DEPENDENCY SIGNALS")
pos=r'好用|感谢|谢谢|太棒|很棒|喜欢|神器|离不开|强大|牛|赞|love|great|awesome|amazing|thank|appreciate|best'
m=[r for r in iss if re.search(pos,(r['t']+' '+r['body']).lower())]
print("  issues with praise words:",len(m))
c=collections.Counter(cat for r in m for cat in r['cats']); print(" ",c.most_common(12))
print('  most-discussed praise issues:',[r['n'] for r in sorted(m,key=lambda r:-r['c'])[:12]])
