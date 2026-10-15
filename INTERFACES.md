# 인터페이스 계약 레지스트리

각 계약은 고유 id를 갖는다. slice-spec과 `CLAUDE.md`는 이 id로 계약을 참조한다.

**정본은 한 곳에만 둔다.** HTTP wire와 receiver wire는 설계 문서가 정본이므로 여기서는 id와 링크만 등록한다(`IF-API-*`, `IF-RCV-*`). 설계 문서가 "구현 세부", "후속 구현"으로 남긴 것만 이 파일이 정한다(`IF-CFG`, `IF-AUTH`, `IF-ID`, `IF-DB`, `IF-OPS`, `IF-CLI`, `IF-DLV`).

## 변경 규칙

- 계약 변경은 설계 단계 행위다. Phase 3(구현) 중에는 비상 해치(정지 → 보고 → 정정 → 재개)로만 바꾼다.
- 변경 시 소비자 목록을 갱신하고 ADR을 남긴다.
- 상태 `frozen`은 구현 시작 가능, `draft`는 아직 바뀔 수 있음.

## A. HTTP API wire (정본: `docs/design/API_V1.md`)

| id | 계약 | 정본 절 | 상태 |
| --- | --- | --- | --- |
| `IF-API-COMMON` | media type, Bearer 인증, 상태 코드 규칙, 크기 제한(decoded body 16 KiB, wire 128 KiB, client_key ASCII 1~128), page 50/100, 오류 body `{error:{code,message,request_id,details}}` | 공통 HTTP 규칙, 오류와 권한 요약 | frozen |
| `IF-API-AGENTS` | `POST /v1/agents`, `GET /v1/agents` | Agent 등록과 directory | frozen |
| `IF-API-RECEIVER` | `PATCH /v1/agents/{id}/receiver` | Receiver 경로와 membership | frozen |
| `IF-API-MEMBERSHIP` | `PUT /v1/agents/{id}/membership/all` | Receiver 경로와 membership | frozen |
| `IF-API-SEND` | `POST /v1/messages` (idempotency, fanout, DM, reply_to) | Message 송신과 durable commit | frozen |
| `IF-API-INBOX` | `GET /v1/inbox`, `GET /v1/agents/{id}/inbox` (snapshot pagination) | Inbox 조회와 read marker | frozen |
| `IF-API-READMARKER` | `PUT /v1/inbox/read-marker` | Inbox 조회와 read marker | frozen |
| `IF-API-HISTORY` | `GET /v1/messages`, `GET /v1/messages/{message_id}` | History 조회 | frozen |
| `IF-API-DELIVERY` | `GET /v1/agents/{id}/delivery`, `POST /v1/agents/{id}/delivery/retry` | Delivery 진단과 receiver 알림 | frozen |
| `IF-API-ROTATE` | `POST /v1/agents/{id}/tokens/rotate` | Token rotation | frozen |

## B. Receiver wire (정본: `docs/design/RECEIVER_WAKE_CONVENTION.md`)

| id | 계약 | 정본 절 | 상태 |
| --- | --- | --- | --- |
| `IF-RCV-BODY` | `{"v":1,"agent_id":..,"inbox_seq":".."}` ≤1024B, HTTP/1.1, 명시적 Content-Length, 압축·chunked 금지 | §6 | frozen |
| `IF-RCV-RESPONSE` | 202 + `Content-Length: 0` + 빈 body만 성공; 그 외 분류 | §7, §10 | frozen |
| `IF-RCV-RETRY` | connect 2s/전체 5s, 1~300s 지수 backoff+jitter, Retry-After, 300s redrive, in-flight 1, coalescing, route_version 격리 | §8, §9, §11 | frozen |

## C. 구현 계약 (이 파일이 정본)

### `IF-ID-001 — 식별자·sequence·시각 표현`

- **종류**: 데이터 형식 · **제공자**: `platform-wire` · **소비자**: `platformd`, `platform`
- `agent_id`: `^[a-z][a-z0-9_-]{0,63}$`, 예약어 `all`, `owner` 거부 (정본 API_V1 "용어와 식별 규칙"). `platform-wire::AgentId` newtype이 유일한 검증 지점.
- `message_id`: `msg-` + ULID(26자 Crockford base32, 대문자). `request_id`: `req-` + ULID.
- sequence(`event_seq`, `inbox_seq`, `read_marker`, `through_seq`, `after_seq`, `until_seq`, `snapshot_head_seq`, `next_*_seq`): JSON **string**. 정규식 `^(0|[1-9][0-9]*)$`, 값 `0..=i64::MAX`. 부호·공백·선행 0 거부. `platform-wire::Seq(i64)`가 serde로 이 표현을 강제한다.
- `route_version`, `membership_epoch`: JSON **number**(정수). API 예시대로.
- `created_at`, `last_attempt_at` 등 시각: RFC 3339, UTC, `Z` 접미, **밀리초** 정밀도 (`2026-10-15T12:34:56.789Z`). DB에도 같은 문자열로 저장한다.
- **상태**: frozen · 2026-10-15

### `IF-CFG-001 — platformd 설정`

- **종류**: 파일(TOML) · **제공자**: 운영자 · **소비자**: `platformd::config`
- 경로: `platformd --config <path>` (기본 `./platformd.toml`), 환경변수 `PLATFORMD_CONFIG`로도 지정 가능. 플래그가 우선.
- 스키마 v1:
  ```toml
  listen = "127.0.0.1:7340"          # SocketAddr. 기본값 그대로.
  db_path = "data/platform.db"        # 상대 경로는 config 파일 위치 기준. 부모 디렉터리는 없으면 생성.
  owner_token_file = "owner.token"    # 파일 내용(trim) = owner token. 32바이트 이상 아니면 기동 거부.

  [delivery]                          # 전부 선택. 기본값은 RECEIVER 문서 제안값.
  connect_timeout_ms = 2000
  request_timeout_ms = 5000
  backoff_min_secs = 1
  backoff_max_secs = 300
  redrive_after_secs = 300
  ```
- 알 수 없는 키는 기동 거부(`serde(deny_unknown_fields)`). 비밀값은 설정 파일에 직접 넣지 않는다(`owner_token_file`만 허용).
- 테스트는 기본값 숫자를 단언하지 않고 "설정이 적용된다"만 확인한다.
- **상태**: frozen · 2026-10-15

### `IF-AUTH-001 — credential 형식과 보관`

- **종류**: 데이터 형식/보관 규칙 · **제공자**: `platformd::auth` · **소비자**: `platformd::api`, `platformd::delivery`, `platform`(CLI)
- owner token: 운영자가 만든 임의 문자열(≥32B). `platformd`는 메모리에만 들고 `subtle` 상수시간 비교로 검증한다. 해시 보관 없음(운영자 자신의 비밀).
- `agent_api_key`: `pak_` + 32바이트 CSPRNG의 base64url(no pad, 43자). DB에는 **SHA-256 hex**만 저장(`agents.api_key_hash`). 평문은 등록 201과 rotate 200 응답에서만 한 번 반환.
- `receiver_secret`: `prs_` + 32바이트 CSPRNG base64url. outbound callback의 Bearer로 써야 하므로 DB에 **평문** 저장(`agents.receiver_secret`). 어떤 조회 응답에도 넣지 않는다.
- Bearer 판정 순서: (1) owner token과 상수시간 비교 → `Principal::Owner`; (2) SHA-256(token) → `agents.api_key_hash` 조회 → `Principal::Agent(agent_id)`; (3) 둘 다 아니면 401 `unauthorized`. receiver secret으로는 어떤 API도 통과하지 않는다.
- rotate는 두 키를 모두 새로 발급하고 `route_version += 1` (정본 API_V1 "Token rotation").
- **상태**: frozen · 2026-10-15 · 근거 [ADR-0004](docs/adr/0004-credential-format-and-storage.md)

### `IF-DB-001 — SQLite schema v1`

- **종류**: 데이터 스키마 · **제공자**: `platformd::store::migrations` · **소비자**: `platformd::store`
- 연결 PRAGMA: `journal_mode=WAL`, `synchronous=FULL`, `foreign_keys=ON`, `busy_timeout=5000`. 기동 시 `journal_mode`가 `wal`이 아니면 기동 거부.
- 마이그레이션: `schema_migrations(version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)`. 버전은 1부터 연속. 적용은 각 버전을 하나의 transaction으로. 현재 최신 = **1**. readiness는 최신 버전이 적용된 뒤에만 성공.
- v1 테이블(API_V1 "저장 모델과 경계"의 논리 모델을 물리화):
  ```sql
  agents(
    agent_id TEXT PRIMARY KEY,
    api_key_hash TEXT NOT NULL UNIQUE,
    receiver_secret TEXT NOT NULL,
    receiver_url TEXT,                         -- NULL = no_receiver
    route_version INTEGER NOT NULL DEFAULT 1,
    inbox_head INTEGER NOT NULL DEFAULT 0,     -- 마지막 발급 inbox_seq
    created_at TEXT NOT NULL
  );
  memberships(
    room_id TEXT NOT NULL,                     -- v1에서는 'all'만
    agent_id TEXT NOT NULL REFERENCES agents(agent_id),
    joined INTEGER NOT NULL,                   -- 0/1
    epoch INTEGER NOT NULL DEFAULT 1,          -- join/leave마다 +1
    effective_event_seq INTEGER NOT NULL,      -- 이 상태가 유효해진 시점의 event_seq
    PRIMARY KEY (room_id, agent_id)
  );
  sequences(name TEXT PRIMARY KEY, value INTEGER NOT NULL);   -- v1 migration이 ('event_seq', 0) 행을 미리 넣는다. 발급은 UPDATE만.
  messages(
    message_id TEXT PRIMARY KEY,
    event_seq INTEGER NOT NULL UNIQUE,
    room_id TEXT NOT NULL,
    sender_id TEXT NOT NULL REFERENCES agents(agent_id),
    body TEXT NOT NULL,
    reply_to TEXT REFERENCES messages(message_id),
    created_at TEXT NOT NULL
  );
  CREATE INDEX messages_room_event ON messages(room_id, event_seq);
  inbox_rows(
    agent_id TEXT NOT NULL REFERENCES agents(agent_id),
    inbox_seq INTEGER NOT NULL,
    message_id TEXT NOT NULL REFERENCES messages(message_id),
    PRIMARY KEY (agent_id, inbox_seq)
  );
  read_state(
    agent_id TEXT PRIMARY KEY REFERENCES agents(agent_id),
    read_marker INTEGER NOT NULL DEFAULT 0
  );
  idempotency(
    sender_id TEXT NOT NULL REFERENCES agents(agent_id),
    client_key TEXT NOT NULL,
    request_hash TEXT NOT NULL,                -- SHA-256 hex of canonical(to, body, reply_to)
    message_id TEXT NOT NULL REFERENCES messages(message_id),
    PRIMARY KEY (sender_id, client_key)
  );
  notification_pending(
    agent_id TEXT PRIMARY KEY REFERENCES agents(agent_id),
    route_version INTEGER NOT NULL,
    state TEXT NOT NULL,                       -- 'idle' | 'pending' | 'blocked'  (no_receiver는 receiver_url IS NULL로 파생)
    target_seq INTEGER NOT NULL,               -- 다음 POST에 넣을 inbox_seq (coalesce된 최신 head)
    last_accepted_seq INTEGER,                 -- 202를 받은 실제 posted_seq
    attempt INTEGER NOT NULL DEFAULT 0,        -- 연속 실패 횟수 (backoff 지수)
    next_due_at TEXT,                          -- 다음 시도 시각 (RFC3339 ms). NULL = 즉시 또는 없음
    redrive_due_at TEXT,                       -- 202 뒤 300초 redrive 예정 시각
    last_attempt_at TEXT,
    last_error TEXT                            -- code 수준 요약. body·secret 금지
  );
  ```
- canonical(to, body, reply_to) 해시 입력: `to` 바이트, `0x00`, `body` 바이트, `0x00`, `reply_to`(없거나 null이면 빈 문자열) 바이트. trim·정규화 없음 (API_V1 "canonical request" 규칙).
- `inbox_seq` 발급: `agents.inbox_head + 1`을 같은 transaction에서 갱신. `event_seq` 발급: `sequences('event_seq') + 1`을 같은 transaction에서 갱신. 둘 다 hole 없음.
- v1 삭제 없음: agents·messages·inbox_rows에 DELETE 경로를 만들지 않는다.
- **상태**: frozen · 2026-10-15

### `IF-DB-002 — Store 명령 큐 (단일 DB thread)`

- **종류**: Rust 내부 API · **제공자**: `platformd::store` · **소비자**: `platformd::api`, `platformd::delivery`
- 단일 OS thread가 `rusqlite::Connection` 하나를 소유한다. `Store`(Clone, Send+Sync)는 bounded `tokio::sync::mpsc`(용량은 설정 아닌 상수, 초기값 256)로 명령을 보낸다.
  ```rust
  pub struct Store { /* mpsc::Sender<Command> */ }
  impl Store {
      /// 클로저를 DB thread에서 실행하고 결과를 돌려준다. 큐가 가득 차면 await로 대기한다.
      pub async fn run<T, F>(&self, f: F) -> Result<T, StoreError>
      where F: FnOnce(&mut rusqlite::Connection) -> Result<T, StoreError> + Send + 'static,
            T: Send + 'static;
      /// `run`과 같되 BEGIN IMMEDIATE … COMMIT 으로 감싼다. 에러면 ROLLBACK.
      pub async fn transaction<T, F>(&self, f: F) -> Result<T, StoreError>
      where F: FnOnce(&rusqlite::Transaction) -> Result<T, StoreError> + Send + 'static,
            T: Send + 'static;
  }
  ```
- 클로저 안에서 network I/O·await 금지. 도메인 질의는 `store::queries` 모듈의 함수로 두고 handler는 그것을 `run`/`transaction`에 넘긴다.
- commit 뒤 inbox row가 생기면 호출자가 `DeliverySignal`(`tokio::sync::Notify`)을 `notify_one()`한다. worker는 이 신호 또는 `next_due_at`까지의 sleep 중 먼저 오는 것을 기다린다. idle tight loop 금지.
- 종료: shutdown 시 Sender를 drop → thread가 남은 명령을 처리하고 Connection을 닫는다. 서버는 thread join을 유한 시간 기다린다.
- **상태**: frozen · 2026-10-15

### `IF-OPS-001 — 운영 endpoint`

- **종류**: REST · **제공자**: `platformd::api` · **소비자**: systemd/운영자, `platform health`, 배포 스크립트
- `GET /healthz` → 인증 없음. DB가 열리고 최신 migration이 적용됐을 때만 `200 {"status":"ok","schema_version":1}`. 그 전에는 listen 자체를 시작하지 않는다(readiness = listen 시작). 요청마다 DB thread를 한 번 왕복해 schema version을 읽으며, 실패하면 503 `unavailable`(오류 body 형식은 `IF-API-COMMON`).
- `platformd --version` → 바이너리 버전(Cargo `CARGO_PKG_VERSION`)과 git short hash(빌드 시 환경변수 `PLATFORMD_GIT_SHA`, 없으면 `unknown`)를 한 줄 출력.
- `platformd backup --config <toml> --out <file>` → 서버 동작 중에도 안전한 online snapshot. 원본을 읽기 전용 두 번째 연결로 열어 rusqlite `backup` API로 복사하고, 산출 파일에 `PRAGMA integrity_check`를 돌려 `ok`가 아니면 exit 1. 기존 파일은 덮어쓰지 않는다(exit 1). 설정 오류 exit 2. stdout에 `backup: <path>`, `bytes:`, `schema_version:`, `integrity_check: ok`, 표마다 `row_count <table> <n>`. 산출 파일 모드 0600, `journal_mode=delete`(단일 파일).
- graceful shutdown: SIGTERM/SIGINT → listener 닫기 → 진행 중 요청 완료 대기(상한 10초) → Store drop·join → delivery in-flight HTTP는 기다리지 않음(pending state는 DB에 있음).
- **상태**: frozen · 2026-10-15

### `IF-CLI-001 — platform CLI 공통 옵션`

- **종류**: CLI · **제공자**: `platform` · **소비자**: 사람, 스크립트
- 전역 옵션: `--url <base>` (env `PLATFORM_URL`), `--token <bearer>` (env `PLATFORM_TOKEN`; 파일에서 읽는 `--token-file`도 허용), `--json`(원문 JSON 출력). 둘 다 없으면 오류 메시지와 exit 2.
- 서브커맨드와 HTTP 매핑은 API_V1 "CLI 매핑" 표가 정본. 추가로 `health` → `GET /healthz`.
- 종료 코드: 0 성공, 1 서버가 오류 body 반환(코드·message 출력), 2 사용법·설정 오류, 3 네트워크 오류.
- CLI는 reqwest client를 서버 delivery client와 공유하지 않는다(별도 crate).
- **상태**: frozen · 2026-10-15

### `IF-DLV-001 — delivery worker 상태 전이`

- **종류**: 내부 상태 기계 · **제공자**: `platformd::delivery` · **소비자**: `IF-API-DELIVERY` handler
- 외부 의미(state 이름, 202 의미, retry 분류, route_version 격리, coalescing)는 `IF-RCV-*`와 API_V1 "Delivery 진단" 절이 정본. 여기서는 저장 표현만 정한다: 표시 `state`는 `receiver_url IS NULL → no_receiver`, 그 외 `notification_pending.state`. `read_marker >= inbox_head`가 되면 worker가 `pending → idle`로 내린다.
- backoff(1-based attempt): `min(backoff_max, backoff_min · 2^(attempt-1))`에 ±50% jitter, 결과를 `[backoff_min, backoff_max]`로 clamp (RECEIVER §9의 1, 2, 4, 8… 예시와 일치). 유효한 `Retry-After`(초 또는 HTTP-date)가 있으면 지수값 대신 그 값을 쓰되 **같은 범위로 clamp**한다 — `Retry-After: 0`이나 과거 날짜가 tight loop를 만들지 않게.
- **marker-at-head 우선**: 어떤 사건 뒤에도 URL이 있고 `read_marker >= inbox_head`면 행은 `idle`(deadline·attempt 초기화). 늦게 온 실패 응답이 "미확인 없음"을 pending으로 되돌리지 않는다. route 변경 시 미확인이 없으면 역시 `idle`.
- `last_error` 코드: `connect`, `timeout`, `network`, `http_NNN`(모든 상태를 그대로), `protocol_mismatch`, `internal`.
- 전이표 정본은 [S4 slice-spec](docs/slices/S4-delivery-worker.md) "전이" 절. 구현은 `platformd::delivery::transitions`(순수 함수) + `store::queries::delivery`.
- **상태**: frozen · 2026-10-16

### `IF-WIRE-001 — GET /v1/agents 응답 형태`

- **종류**: REST 응답 스키마(API_V1이 필드만 열거하고 모양을 비워 둔 부분) · **제공자**: `platformd::api` · **소비자**: `platform`, 브라우저 UI
- 페이징 없음(v1 agent 수는 소수). `agent_id` ASCII 오름차순.
  ```json
  // Principal::Agent
  {"items":[{"agent_id":"agent-a","joined_all":true}]}
  // Principal::Owner
  {"items":[{"agent_id":"agent-a","joined_all":true,"receiver_url":"http://...","route_version":1,
             "created_at":"2026-10-15T12:34:56.789Z","delivery":{"state":"idle"}}]}
  ```
- `delivery.state`는 `IF-DLV-001` 파생 규칙(`receiver_url IS NULL → no_receiver`, 그 외 `notification_pending.state`). 키·secret은 어떤 형태에도 없다.
- **상태**: frozen · 2026-10-15

### `IF-WIRE-002 — 요청 JSON 파싱·검증 오류 매핑`

- **종류**: 오류 규칙(API_V1 "잘못된 JSON·필드 형식은 400 또는 422"의 구체화) · **제공자**: `platformd::api::StrictJson` · **소비자**: 모든 write handler
- `Content-Type`이 `application/json`이 아니면 415 `unsupported_media_type`.
- body가 JSON으로 파싱되지 않으면 400 `invalid_json`.
- 허용 밖 필드(`deny_unknown_fields`) → 422 `unexpected_field`, `details: {"field": "<이름>"}`.
- 필수 필드 누락·타입 불일치·형식 위반(`Seq`, 길이 등) → 422 `invalid_field`, `details: {"field": "<이름>"}`(알 수 있을 때). 단 API_V1이 전용 code를 둔 경우(`invalid_agent_id`, `invalid_receiver_url`, `body_too_large`, `request_too_large`, `self_dm_not_supported`, `reply_to_room_mismatch`, `read_marker_ahead`, `mutually_exclusive_cursors`)는 그 code가 우선.
- 크기: wire body > 128 KiB → 413 `request_too_large`(파싱 전 차단). decoded `body` > 16 KiB → 413 `body_too_large`.
- **상태**: frozen · 2026-10-15

### `IF-WIRE-003 — API_V1 오류표 보충 code`

- **종류**: 오류 code(API_V1 "오류와 권한 요약"이 다루지 않은 상황) · **제공자**: `platformd::api` · **소비자**: `platform`, 브라우저 UI
- | 상황 | 상태 | code |
  | --- | --- | --- |
  | `reply_to`가 존재하지 않는 message_id | 404 | `reply_to_not_found` |
  | `GET /v1/messages/{id}` 대상이 없거나 권한 밖 | 404 | `message_not_found` |
  | 경로의 `{agent_id}`가 없음(owner 요청) | 404 | `agent_not_found` |
  | owner 토큰으로 agent 전용 write(`POST /v1/messages`, `PUT /v1/inbox/read-marker`) | 403 | `forbidden` |
  | `room_id` 형식 오류(`all` 또는 `dm:<a>:<b>`, a<b ASCII, 유효 slug, a≠b 아님) | 422 | `invalid_field` (`details.field="room_id"`) |
  | `limit`이 1..100 밖 또는 정수 아님 | 422 | `invalid_field` (`details.field="limit"`) |
  | 알 수 없는 경로 | 404 | `not_found` |
  | 허용되지 않은 method | 405 | `method_not_allowed` |
  | 내부 오류(DB 등) | 500 | `internal` (message는 일반 문구, 상세는 로그에만) |
  | `/healthz`에서 DB 왕복 실패 또는 schema 불일치 | 503 | `unavailable` |
- `403 또는 404 forbidden`로 적힌 "다른 agent의 DM·inbox·delivery 조회"는 **403 `forbidden`**으로 통일한다(수용 시나리오 10의 403과 일관).
- **상태**: frozen · 2026-10-15

### `IF-WIRE-004 — receiver_url 정규화와 PATCH body`

- **종류**: 입력 규칙(S1 구현에서 확정) · **제공자**: `platformd::api::receiver_url` · **소비자**: `IF-API-AGENTS`, `IF-API-RECEIVER`, delivery client
- `receiver_url`은 `url` crate 파싱 뒤 **정규화된 직렬화**(scheme·host 소문자, 빈 path → `/`)로 저장·비교·응답한다. 따라서 대소문자만 다른 URL은 같은 값이며 `route_version`을 올리지 않는다. 원문과 정규화 결과 모두 2048바이트 이하.
- `PATCH /v1/agents/{id}/receiver` body는 `receiver_url` 키가 **필수**(값은 null 허용). `{}`는 422 `invalid_field`.
- `POST /v1/agents/{id}/tokens/rotate`는 body를 읽지 않는다(Content-Type 무관).
- 인증·권한(401/403)은 body 검사(415/400/422)보다 먼저다. agent principal이 다른 agent 경로를 지정하면 존재 여부와 무관하게 403, owner가 slug가 아닌 경로를 지정하면 404 `agent_not_found`.
- **상태**: frozen · 2026-10-15

### `IF-WIRE-005 — 송신·history 입력 규칙 보충`

- **종류**: 입력 규칙(S2 구현에서 확정) · **제공자**: `platformd::api::{messages,history}` · **소비자**: `platform`, 브라우저 UI
- `reply_to`는 `msg-`+ULID 형식이어야 하며 형식 위반은 422 `invalid_field`(`field=reply_to`), `to` 검사 직후에 판정한다. 형식은 맞지만 없는 id는 404 `reply_to_not_found`. (빈 문자열이 null과 같은 해시가 되는 것을 막는다.)
- 검사 순서는 S2 스펙 그대로: idempotency 조회가 recipient·self-DM·membership보다 먼저다. 따라서 이미 다른 payload에 묶인 key로 self-DM을 보내면 422가 아니라 409다.
- `GET /v1/messages`·`GET /v1/inbox`류의 **query parameter는 엄격**하다: 알 수 없는 이름 → 422 `unexpected_field`, 같은 이름 반복 → 422 `invalid_field`. 확인 순서: 이름 → `room_id`(422) → 권한(403) → 커서 동시 사용(400) → seq·limit 값(422). 브라우저 UI는 캐시버스터 등 추가 파라미터를 붙이지 않는다.
- `limit`은 ASCII 숫자만(`+5` 거부). `q`는 form-urlencoded 디코딩(`+`는 공백). `client_key`는 `is_ascii()`만 검사(1..128바이트).
- `message_id`·`created_at`은 DB transaction 안에서 생성해 `event_seq` 순서와 시각 순서를 맞춘다.
- **상태**: frozen · 2026-10-15

## 용어집

- **head**: 한 agent inbox의 마지막 `inbox_seq` (`agents.inbox_head`).
- **route**: `(receiver_url, receiver_secret)` 조합. 바뀌면 `route_version += 1`.
- **posted_seq**: 실제 callback body에 넣은 `inbox_seq`. 202를 받으면 그대로 `last_accepted_seq`가 된다.
