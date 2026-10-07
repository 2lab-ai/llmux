# Codex frontend — fixed target
Status: in-progress
Date: 2026-10-07

## User request (verbatim)

```text
llmux 개선 작업

현재 llmux run으로 실행하면 클로드를 llmux가 제공하는 엔드포인트로 실행하고 있음

codex호환(openai 호환) 엔드포인트를 제공


목표: claude에서 claude 모델이랑 코덱스 모델을 사용하는 것처럼
"llmux run --codex" 명령으로 codex를 llmux 백엔드로 실행하는 피쳐
  
프리릴리즈까지 배포 목표 

명확하지 않은 기능은 모두 claude code 용 피쳐랑 똑같이 openai용으로 구현
claude 모델 사용은 claude agent sdk로 구현해서 처리(이거 이렇게 구현해야 codex에서 클로드 모델 사용 가능함)

.claude 폴더 스킬에 using-dotprd 스킬 읽고 .prd 추가해서 작업 진행

)
```

## Additional request (verbatim)

```text
(추가로) activity에 배경색으로 claude 엔드포인트 요청인지, codex 요청인지 처리 (openai endpoint 요청은 배경을 어두운 회색으로 표시)
```

## Fixed acceptance

Actual `llmux run --codex` launches Codex using the same local/remote daemon,
account scheduler, refresh/retry semantics and model catalog as Claude Code.
Codex can converse and execute tools using Codex and Claude models; Claude requests
use the actual Claude Agent SDK with isolated leased credentials. OpenAI endpoint
activity has a dark gray background independent of the served provider. Preview
release publication and installed-binary verification close the work.
