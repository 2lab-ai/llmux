# SSOT — email anonymous: server-owned setting + islands linkage (2026-07-02)

구술 verbatim (2026-07-02 19:4x, zbrain 세션):
> 이거 지금 llmux-islands에서 emial anonymous on/off 하는 기능 추가 햇는데 llmux에도 이
> 기능으로 추가하고 llmux server에서 현재 설정에서 email anonymous하게 보여주고 이 설정에
> 따라 llmux-islands에서 지금처럼 모자이크 처리하도록 연결해줘

## Interpretation (binding defaults)

Single source of truth = llmux 서버의 persistent 설정 `email_anonymous` (config JSON, 기본 false).

| Row | Acceptance (관측자 기준) |
|---|---|
| E1 | llmux config에 `email_anonymous` bool이 존재하고 구버전 config도 그대로 로드된다 (default false) |
| E2 | `GET /llmux/status` 응답에 `email_anonymous` 플래그가 보인다 |
| E3 | API로 설정을 켜고 끌 수 있고(POST, api-key 게이트·loopback 면제), 값이 config에 persist된다 |
| E4 | 설정 ON이면 llmux TUI의 이메일 표시 표면이 마스킹되어 보인다 (demo alias 매핑 재사용, 렌더층) |
| E5 | islands의 Email anonymous 토글이 서버 설정을 읽고/쓴다 — 토글 = 서버 설정 flip |
| E6 | 서버 설정 ON이면 islands Usage 이메일이 지금처럼 모자이크로 보인다 (OFF면 원본) |
| E7 | 구버전 데몬(플래그 없음)에 붙으면 islands는 기존 로컬-토글 동작으로 fallback |

Tensions → defaults:
- T1 API 데이터 자체를 마스킹? → **NO**: API는 실이름 유지(islands가 OFF 상태·모자이크 원본에 실데이터 필요), 마스킹은 각 표시 표면(TUI 렌더, islands 픽셀화)이 설정에 따라 수행.
- T2 LLMUX_DEMO_MODE와의 관계 → 독립, demo mode가 우선(둘 다 켜져도 마스킹 유지). demo는 load-time 치환 그대로.
- T3 islands 로컬 AppSettings 키 → 신데몬에선 서버값 미러/캐시 + 구데몬 fallback 저장소로 유지.
- T4 ship 범위 → 이번 지시는 "추가+연결" = 구현·검증·PR까지. 머지/릴리즈는 유저 지시 대기.

## Result (2026-07-02 20:1x KST)

PR #54 (feat/email-anonymous-server-setting, 4985ebc+bc1f69e). `just check` 561+37 pass,
islands xcodebuild green. E1–E4 = 단위+e2e 테스트로 닫힘 (throwaway 서버 소켓 e2e: off→POST
flip→on→persist, API 이름 실데이터 유지 T1, TUI 전 표면 마스킹 leak-test, demo-mode 우선 T2).
E6/E7 = 오프스크린 스냅샷 재생성 — pre-refactor와 sha256 byte-identical(=fallback 경로 무회귀),
ON 모자이크/OFF 원본 오케스트레이터 육안 확정. E5(토글→서버 POST 라이브 UX)만 needs-eyes
잔여 — 머지+릴리즈 후 실데몬에서 토글 한 번이 최종 확인. 머지/릴리즈 = 유저 게이트.

## Ship (2026-07-02 20:5x KST, 유저 "진행 ㄱ 배포까지 ㄱ")

- 리뷰: APPROVE-WITH-NITS, blocking 0 (nit 4건 = 기존 패턴 상속·엣지·주석 — PR 스레드에 기록).
- 머지 8c83e77. 릴리즈 v0.2.10 (bf5e301, release run 28587316296 + tap bump 28587606369 SUCCESS).
- brew formula/cask 0.2.9→0.2.10; islands 앱 cask가 재실행 — 서버-연동 토글 라이브.
- 사고+복구: 메인 체크아웃이 유저 브랜치(docs/readme-islands-update)로 바뀌어 있어 범프 커밋이
  잘못 얹힘 → reset --soft로 원상복구(유저 gif·미추적 무손상), 릴리즈는 격리 worktree에서 재실행.
  교훈: 릴리즈도 resolver처럼 항상 worktree — 메인 체크아웃 브랜치를 가정하지 마라.
- 잔여(분류기 유저-게이트, 각 1커맨드): ① 로컬 데몬 restart(`/opt/homebrew/bin/llmux restart`,
  false "not ready 5s" 무시하고 poll) ② oudwood-512 `brew upgrade llmux` + restart(활성 llmux run
  워크로드 3개 있음 — 한가할 때).
