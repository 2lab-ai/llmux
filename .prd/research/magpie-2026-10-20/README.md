# magpie issue analysis — reproduction (2026-10-20)

Inputs are fetched, not committed: issue bodies are third-party user content, so the outputs keep only counts and issue numbers.

```sh
D=$(mktemp -d)
gh api --paginate 'repos/yetone/magpie/issues?state=all&per_page=100' \
  --jq '.[] | {n:.number, t:.title, s:.state, pr:(.pull_request!=null), c:.comments, r:.reactions.total_count, up:.reactions["+1"], heart:.reactions.heart, rocket:.reactions.rocket, l:[.labels[].name], at:.created_at, closed:.closed_at, sr:.state_reason, body:(.body // "" | .[0:600])}' > "$D/issues.jsonl"
gh api --paginate 'repos/yetone/magpie/issues?state=all&per_page=100' \
  --jq '.[] | select(.pull_request==null) | {n:.number, u:.user.login, ut:.user.type, a:.author_association}' > "$D/authors.jsonl"
python3 -I - "$D" <<'PY'
import json,sys; D=sys.argv[1]
au={json.loads(l)['n']:json.loads(l) for l in open(D+'/authors.jsonl')}
rows=[json.loads(l) for l in open(D+'/issues.jsonl')]
json.dump([r for r in rows if not r['pr'] and au[r['n']]['a']!='OWNER'],open(D+'/user_issues.json','w'),ensure_ascii=False)
PY
python3 -I classify.py "$D" > areas.txt     # writes $D/classified.json
python3 -I mentions.py "$D" > mentions.txt
python3 -I items.py "$D" > items.txt
```

Snapshot taken 2026-10-20 07:41Z (last issue #3038): 3,036 items = 2,464 issues + 572 PRs, 2,138 user (non-owner) issues from 1,014 authors.
