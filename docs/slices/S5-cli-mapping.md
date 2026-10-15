# 슬라이스 S5: cli-mapping

- **상태**: frozen (2026-10-16)
- **위험/의존도**: 중 — 서버 API가 전부 있어야 끝까지 검증된다. 로직은 얇다.

## 목표
`platform` CLI가 API_V1 "CLI 매핑" 표의 모든 명령을 제공해, 사람이 터미널에서 agent 등록·송신·inbox 순회·read marker·history·delivery 진단을 끝까지 할 수 있다.

## 범위 · 비범위
- 포함: 매핑 표 전체 + `health`(S0). 사람이 읽는 기본 출력과 `--json` 원문 출력. 종료 코드 `IF-CLI-001`.
- 제외: receiver 수신, local queue, 대화형 모드, 셸 completion, UI(S6).

## 건드릴 인터페이스
`IF-CLI-001`, `IF-API-*`(소비만), `IF-ID-001`. `platform-wire`의 DTO를 재사용한다(ADR-0003).

## 구현 방안
매핑은 API_V1 "CLI 매핑" 표가 정본. **먼저 읽는다.** 이 스펙이 정하는 것:

1. **명령 트리**(clap derive, kebab-case):
   `agent register <agent_id> [--receiver-url URL]` · `agent list` · `agent inbox <agent_id> [--after-seq N] [--until-seq N] [--limit N]` · `agent tokens rotate <agent_id>` · `receiver set <agent_id> <url>` · `receiver unset <agent_id>` · `all join <agent_id>` · `all leave <agent_id>` · `send --to <all|agent_id> (--body TEXT | --body-file PATH | stdin) [--client-key KEY] [--reply-to MESSAGE_ID]` · `inbox [--after-seq N] [--until-seq N] [--limit N] [--all-pages]` · `inbox read --through-seq N` · `history --room ROOM [--after N | --before N] [--limit N] [--q TEXT]` · `message show <message_id>` · `delivery <agent_id>` · `delivery retry <agent_id>` · `health`.
2. **`send`의 `client_key`**: 생략하면 ULID를 생성해 사용하고 결과 출력에 `client_key`를 함께 보인다(재시도에 쓸 수 있게). `--body-file -`는 stdin.
3. **`inbox --all-pages`**: 첫 응답의 `snapshot_head_seq`를 `until_seq`로 고정해 `has_more=false`까지 순회(PLATFORM_MESSAGING_SPEC "Inbox pagination snapshot" 규칙의 참조 구현). 조회만 하고 marker는 올리지 않는다.
4. **출력**: 기본은 사람이 읽는 표/줄(비밀값은 등록·rotate 결과에만 그대로 출력하고 "한 번만 표시된다" 경고 한 줄을 stderr로). `--json`은 서버 응답 원문 그대로(`inbox --all-pages`는 items를 합친 하나의 JSON).
5. **오류**: 서버 오류 body → stderr `platform: <code>: <message> (request <request_id>)`, exit 1. 네트워크 exit 3. 사용법 exit 2.
6. 서버 `reqwest` client와 분리(이미 crate가 다름). 타임아웃 기본 30초(상수).

## 완료 정의 (DoD)
- [ ] 표의 모든 명령이 in-process 서버에 대해 통합 테스트로 한 번씩 실행되고 HTTP 매핑(method·path·body)이 맞다.
- [ ] `inbox --all-pages`가 순회 중 새 메시지를 제외하고 누락·중복 없이 끝난다.
- [ ] `send` 재시도(같은 `--client-key`)가 200/`deduplicated=true`를 표시한다.
- [ ] 종료 코드 네 종류 테스트. `--json` 출력이 유효 JSON.
- [ ] fmt/clippy/test 그린. 코드 미학.

## 검증 방법 (증거 필수)
테스트 출력. 스모크: 서버 기동 → CLI로 등록 두 개 → send → inbox → inbox read → history → delivery. 명령과 출력 원문.

## 핸드오프 / 비상 해치
변경 파일, 검증 로그, 비상 해치 보고. 커밋하지 않는다. 정본대로 불가능/모순일 때만 정지 → 보고.
