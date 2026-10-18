# 인터페이스 계약 레지스트리

각 계약은 고유 id를 갖는다. slice-spec과 `CLAUDE.md`는 이 id로 계약을 참조한다.

**정본은 한 곳에만 둔다.** HTTP wire, receiver wire, identity token wire는 설계 문서가 정본이므로 여기서는 id와 링크만 등록한다(`IF-API-*`, `IF-RCV-*`, `IF-IDT-*`). 설계 문서가 "구현 세부", "후속 구현"으로 남긴 것만 이 파일이 정한다(`IF-CFG`, `IF-AUTH`, `IF-ID`, `IF-DB`, `IF-OPS`, `IF-CLI`, `IF-DLV`).

## 변경 규칙

- 계약 변경은 설계 단계 행위다. Phase 3(구현) 중에는 비상 해치(정지 → 보고 → 정정 → 재개)로만 바꾼다.
- 변경 시 소비자 목록을 갱신하고 ADR을 남긴다.
- 상태 `frozen`은 구현 시작 가능, `draft`는 아직 바뀔 수 있음. `frozen-pending`은 설계가 freeze되어 구현을 시작할 수 있지만 아직 구현되지 않은 계약(또는 개정분)이다. 해당 슬라이스가 done이 되면 `frozen`으로 바꾼다.

## A. HTTP API wire (정본: `docs/design/API_V1.md`)

| id | 계약 | 정본 절 | 상태 |
| --- | --- | --- | --- |
| `IF-API-COMMON` | media type, Bearer 인증, 상태 코드 규칙, 크기 제한(decoded body 16 KiB, wire 128 KiB, client_key ASCII 1~128), page 50/100, 오류 body `{error:{code,message,request_id,details}}` | 공통 HTTP 규칙, 오류와 권한 요약 | frozen |
| `IF-API-AGENTS` | `POST /v1/agents`(body `{agent_id, name, receiver_url?}`, `name` 필수), `GET /v1/agents`(두 principal 모두 item에 `name`·`profile`·`avatar`) | Agent 등록과 directory | frozen · S9 개정 frozen 2026-10-17 · `avatar` 요약 frozen-pending 2026-10-18(S11) |
| `IF-API-AVATAR` | `PUT /v1/agents/{id}/avatar`(owner 또는 그 agent, raw `image/png`·`image/jpeg` body ≤ 524288 B, magic 일치, 200 `{agent_id, avatar:{content_type,bytes,sha256,updated_at}}`, 413 `avatar_too_large`, 415 `unsupported_media_type`), `GET …/avatar`(모든 principal, bytes + `ETag: "<sha256>"` + `Cache-Control: private, max-age=300`, `If-None-Match` → 304, 없으면 404 `avatar_not_found`), `DELETE …/avatar`(owner 또는 그 agent, 204) | Agent 등록과 directory("Directory avatar") | frozen-pending · 2026-10-18 |
| `IF-API-OWNER` | `GET /v1/owner`(owner 전용, query 없음, 200 `{post_as:[agent_id…]}` 설정 순서) | Room 목록과 owner 게시("GET /v1/owner") | frozen-pending · 2026-10-18 |
| `IF-API-PROFILE` | `PUT /v1/agents/{id}/profile`(owner 또는 그 agent, 부분 갱신 — 생략=유지·`null`=비움, `name`은 owner만 → agent key면 403 `forbidden`, `name: null` 422, 200 `{agent_id, name, profile}`), 필드 규칙 | Agent 등록과 directory("PUT /v1/agents/{agent_id}/profile", "Directory profile 필드 규칙") | frozen · 2026-10-17 |
| `IF-API-RECEIVER` | `PATCH /v1/agents/{id}/receiver` | Receiver 경로와 membership | frozen |
| `IF-API-MEMBERSHIP` | `PUT /v1/agents/{id}/membership/all` | Receiver 경로와 membership | frozen |
| `IF-API-SEND` | `POST /v1/messages` (idempotency, fanout, DM, reply_to) | Message 송신과 durable commit | frozen |
| `IF-API-INBOX` | `GET /v1/inbox`, `GET /v1/agents/{id}/inbox` (snapshot pagination) | Inbox 조회와 read marker | frozen |
| `IF-API-READMARKER` | `PUT /v1/inbox/read-marker` | Inbox 조회와 read marker | frozen |
| `IF-API-HISTORY` | `GET /v1/messages`, `GET /v1/messages/{message_id}` | History 조회 | frozen |
| `IF-API-DELIVERY` | `GET /v1/agents/{id}/delivery`, `POST /v1/agents/{id}/delivery/retry` | Delivery 진단과 receiver 알림 | frozen |
| `IF-API-ROTATE` | `POST /v1/agents/{id}/tokens/rotate` | Token rotation | frozen |
| `IF-API-ROOMS` | `GET /v1/rooms`(owner 전용, `{rooms:[{room_id,kind,members,last_event_seq,last_message_at,last_sender_id,message_count}]}`, 최근 순, `all` 항상 포함, paging 없음·soft cap 1000), `POST /v1/rooms/{room_id}/messages`(owner 전용, body `{sender_id,body,client_key,reply_to?}`, `sender_id`는 `[owner] post_as`만 → 아니면 403 `forbidden`, 미등록 DM member 404 `unknown_room`, 수신자 = room member − sender, 응답은 `IF-API-SEND`와 같은 message 객체) | Room 목록과 owner 게시 | frozen-pending · 2026-10-18 |
| `IF-API-IDENTITY` | `POST /v1/identity/tokens` (agent 전용, 201 `{token,sub,aud}`, `owner_not_allowed`·`identity_disabled`·`unknown_audience`·`ttl_too_long`) | Identity attestation | frozen · 2026-10-16 |

## B. Receiver wire (정본: `docs/design/RECEIVER_WAKE_CONVENTION.md`)

| id | 계약 | 정본 절 | 상태 |
| --- | --- | --- | --- |
| `IF-RCV-BODY` | `{"v":1,"agent_id":..,"inbox_seq":".."}` ≤1024B, HTTP/1.1, 명시적 Content-Length, 압축·chunked 금지 | §6 | frozen |
| `IF-RCV-RESPONSE` | 202 + `Content-Length: 0` + 빈 body만 성공; 그 외 분류 | §7, §10 | frozen |
| `IF-RCV-RETRY` | connect 2s/전체 5s, 1~300s 지수 backoff+jitter, Retry-After, 300s redrive, in-flight 1, coalescing, route_version 격리 | §8, §9, §11 | frozen |

## B-2. Identity token wire (정본: `docs/design/IDENTITY_ATTESTATION.md`)

| id | 계약 | 정본 절 | 상태 |
| --- | --- | --- | --- |
| `IF-IDT-JWT` | compact JWS, header `{"alg":"EdDSA","typ":"JWT","kid":..}`(`kid` 필수), claims `iss`·`sub`·`aud`(단일 문자열)·`iat`·`exp`(정수 초)·`jti`(ULID), `0 < exp-iat ≤ 900`, base64url no pad | §5 | frozen · 2026-10-16 |
| `IF-IDT-VERIFY` | 소비자 검증 순서 alg 고정 → kid → 서명 → iss/aud → exp/iat(skew 60s, 최대 수명) → sub 정규식 → 소비자 ACL, replay 방지 없음 | §6 | frozen · 2026-10-16 |
| `IF-IDT-KEYS` | keygen 출력(`kid`, 표준 base64 공개키), 교체 순서(새 키 → 소비자에 추가 → 허브 재시작 → `max_ttl_secs`+60s 뒤 옛 키 제거) | §7, §8 | frozen · 2026-10-16 |

## C. 구현 계약 (이 파일이 정본)

### `IF-ID-001 — 식별자·sequence·시각 표현`

- **종류**: 데이터 형식 · **제공자**: `platform-wire` · **소비자**: `platformd`, `platform`
- `agent_id`: `^[a-z][a-z0-9_-]{0,63}$`, 예약어 `all`, `owner` 거부 (정본 API_V1 "용어와 식별 규칙"). `platform-wire::AgentId` newtype이 유일한 검증 지점.
- `message_id`: `msg-` + ULID(26자 Crockford base32, 대문자). `request_id`: `req-` + ULID.
- identity token의 `jti`: 접두사 없는 ULID(`IF-AUTH-002`). JWT 안의 `iat`·`exp`는 RFC 3339가 아닌 정수 초다(`IF-IDT-JWT`).
- sequence(`event_seq`, `inbox_seq`, `read_marker`, `through_seq`, `after_seq`, `until_seq`, `snapshot_head_seq`, `next_*_seq`): JSON **string**. 정규식 `^(0|[1-9][0-9]*)$`, 값 `0..=i64::MAX`. 부호·공백·선행 0 거부. `platform-wire::Seq(i64)`가 serde로 이 표현을 강제한다.
- `route_version`, `membership_epoch`: JSON **number**(정수). API 예시대로.
- `created_at`, `last_attempt_at`, `profile.updated_at` 등 시각: RFC 3339, UTC, `Z` 접미, **밀리초** 정밀도 (`2026-10-15T12:34:56.789Z`). DB에도 같은 문자열로 저장한다.
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

  [identity]                          # 선택. 없으면 기능 비활성(POST /v1/identity/tokens → 503 identity_disabled).
  signing_key_file = "/etc/platformd/identity.key"   # raw seed 32바이트(IF-AUTH-002). 상대 경로는 config 파일 위치 기준.
  issuer = "agents-platform"          # JWT iss. 고정 문자열(허브는 공개 URL을 모른다).
  audiences = ["garage-docs"]         # 허용 aud. 1개 이상, 빈 문자열·중복 금지.
  default_ttl_secs = 600              # 선택. ttl_secs 생략 시.
  max_ttl_secs = 900                  # 선택. 1 ≤ default_ttl_secs ≤ max_ttl_secs ≤ 900(IF-IDT-JWT 상한) 아니면 기동 거부.

  [owner]                             # 선택(S10). 없으면 post_as = [] 와 같다.
  post_as = []                        # owner가 POST /v1/rooms/{room_id}/messages의 sender_id로 쓸 수 있는 agent_id 목록.
                                      # 기본 빈 목록 = 기능 꺼짐(모든 sender_id가 403). 운영 Pi는 ["human"].
  ```
- 알 수 없는 키는 기동 거부(`serde(deny_unknown_fields)`). 비밀값은 설정 파일에 직접 넣지 않는다(`owner_token_file`, `signing_key_file`처럼 파일 경로만 허용).
- `[identity]`가 있으면 `signing_key_file`, `issuer`, `audiences`는 필수다.
- `[owner].post_as`의 각 값은 `IF-ID-001`의 `agent_id` 형식(예약어 `all`·`owner` 거부)이어야 하고 중복이 없어야 한다. 위반은 기동 거부. 등록 여부는 기동 때 보지 않는다(요청 때 404 `agent_not_found`, `IF-WIRE-007`). 허용 목록에는 owner 자신을 나타내는 identity만 넣는다(운영 규칙, [ADR-0007](docs/adr/0007-owner-posts-into-any-room.md)). 기동 로그에 `post_as` 목록을 남긴다(ID는 비밀이 아니다).
- 테스트는 기본값 숫자를 단언하지 않고 "설정이 적용된다"만 확인한다.
- **상태**: frozen · 2026-10-15 · `[identity]` 절 frozen 2026-10-16 · `[owner]` 절 frozen-pending 2026-10-18(S10)

### `IF-AUTH-001 — credential 형식과 보관`

- **종류**: 데이터 형식/보관 규칙 · **제공자**: `platformd::auth` · **소비자**: `platformd::api`, `platformd::delivery`, `platform`(CLI)
- owner token: 운영자가 만든 임의 문자열(≥32B). `platformd`는 메모리에만 들고 `subtle` 상수시간 비교로 검증한다. 해시 보관 없음(운영자 자신의 비밀).
- `agent_api_key`: `pak_` + 32바이트 CSPRNG의 base64url(no pad, 43자). DB에는 **SHA-256 hex**만 저장(`agents.api_key_hash`). 평문은 등록 201과 rotate 200 응답에서만 한 번 반환.
- `receiver_secret`: `prs_` + 32바이트 CSPRNG base64url. outbound callback의 Bearer로 써야 하므로 DB에 **평문** 저장(`agents.receiver_secret`). 어떤 조회 응답에도 넣지 않는다.
- Bearer 판정 순서: (1) owner token과 상수시간 비교 → `Principal::Owner`; (2) SHA-256(token) → `agents.api_key_hash` 조회 → `Principal::Agent(agent_id)`; (3) 둘 다 아니면 401 `unauthorized`. receiver secret으로는 어떤 API도 통과하지 않는다.
- rotate는 두 키를 모두 새로 발급하고 `route_version += 1` (정본 API_V1 "Token rotation").
- **상태**: frozen · 2026-10-15 · 근거 [ADR-0004](docs/adr/0004-credential-format-and-storage.md)

### `IF-AUTH-002 — identity 서명 키와 토큰 발급`

- **종류**: 키 형식/발급 규칙 · **제공자**: `platformd::identity` · **소비자**: `platformd::api`, `platformd`(keygen), `platform`(CLI), 외부 소비자(garage-docs)
- 서명 키 파일: raw Ed25519 seed **정확히 32바이트**(base64·PEM·개행 없음). mode 0640 root:platformd. `platformd identity keygen --out <path>`만 만든다(기존 파일 덮어쓰기 거부).
- 공개키 표현: raw 32바이트의 **표준 base64(padding 있음, 44자)**. `kid` = SHA-256(raw 공개키) 앞 4바이트의 소문자 hex 8자.
- 로드: `[identity]`가 있으면 기동 때 한 번 읽어 메모리에 둔다(owner token과 같은 방식). 읽기 실패·길이 불일치는 기동 거부. 교체는 재시작. 기동 로그에 `kid`·`audiences`만 남긴다.
- 발급: principal이 `Principal::Agent(id)`일 때만. `Principal::Owner`는 403 `owner_not_allowed`. `sub = id`. `iat` = 현재 시각을 초로 버림, `exp = iat + ttl_secs`, `jti` = 새 ULID(접두사 없음). 응답 201은 정확히 `{token, sub, aud}`이며 만료는 JWT `exp`로만 알린다.
- 판정 순서: 401 → 403 `owner_not_allowed` → 503 `identity_disabled` → body 검사(413/415/400 → 422 `unexpected_field` → 422 `invalid_field` → 422 `unknown_audience` → 422 `ttl_too_long`). 정본 IDENTITY_ATTESTATION §4.2.
- 인코딩은 `IF-IDT-JWT`. 구현은 `ed25519-dalek`(`SigningKey::from_bytes`) + 직접 만든 compact JWS 인코더. RFC 8037 Appendix A.4 test vector와 garage-docs(`jwt/v5`) interop test로 검증한다(근거 ADR-0005).
- DB 변경 없음. Store 명령 신설 없음(인증 계층의 `api_key_hash` 조회가 agent를 이미 확인한다).
- 로그: 토큰 원문·seed 금지. 발급 1건마다 `sub`, `jti`, `aud`, `exp`만.
- identity token은 Bearer 판정(`IF-AUTH-001`)의 어느 단계도 통과하지 않는다(→ 401).
- **상태**: frozen · 2026-10-16 · 근거 [ADR-0005](docs/adr/0005-identity-attestation-tokens.md)

### `IF-DB-001 — SQLite schema (v1, v2, v3)`

- **종류**: 데이터 스키마 · **제공자**: `platformd::store::migrations` · **소비자**: `platformd::store`
- 연결 PRAGMA: `journal_mode=WAL`, `synchronous=FULL`, `foreign_keys=ON`, `busy_timeout=5000`. 기동 시 `journal_mode`가 `wal`이 아니면 기동 거부.
- 마이그레이션: `schema_migrations(version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)`. 버전은 1부터 연속. 적용은 각 버전을 하나의 transaction으로. 현재 최신 = **3**(S11, 2026-10-18. S9·S10은 2, S8까지는 1). readiness는 최신 버전이 적용된 뒤에만 성공.
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
- v2 변경(S9, [ADR-0006](docs/adr/0006-agent-directory-profile.md)): 자기소개 directory profile. v1 DB에 한 transaction으로 적용하며 기존 행은 모두 남고 새 컬럼은 `NULL`이다. 데이터 migration·backfill 없음(owner가 `name`을 손으로 채운다).
  ```sql
  ALTER TABLE agents ADD COLUMN name TEXT;                -- 표시 이름. S9 이후 등록은 항상 NOT NULL 값, v1 행은 NULL
  ALTER TABLE agents ADD COLUMN gender_form TEXT;         -- 'female' | 'male' | 'none' | NULL(선언 안 함)
  ALTER TABLE agents ADD COLUMN age_band TEXT;            -- 'child' | 'teen' | '20s' | '30s_40s' | '50s_plus' | 'none' | NULL
  ALTER TABLE agents ADD COLUMN role TEXT;                -- trim된 한 줄, NULL = 선언 안 함
  ALTER TABLE agents ADD COLUMN intro TEXT;               -- trim된 산문(LF 허용), NULL = 선언 안 함
  ALTER TABLE agents ADD COLUMN profile_updated_at TEXT;  -- RFC3339 ms. 등록 시각 또는 저장값이 실제로 바뀐 마지막 쓰기. v1 행은 NULL
  ```
  CHECK 제약·index는 두지 않는다. 값 검증 지점은 `platform-wire`(`IF-WIRE-006`) 하나다. v2를 적용한 DB는 v1 바이너리가 `SchemaTooNew`로 거부하므로 되돌림은 배포 전 backup 복원이다.
- v3 변경(S11, [ADR-0008](docs/adr/0008-directory-avatars.md)): directory avatar. v2 DB에 한 transaction으로 적용하며 기존 table과 행은 그대로다. 데이터 migration 없음.
  ```sql
  CREATE TABLE agent_avatars (
    agent_id TEXT PRIMARY KEY REFERENCES agents(agent_id),
    content_type TEXT NOT NULL,                -- 'image/png' | 'image/jpeg'
    bytes BLOB NOT NULL,                       -- 1..=AVATAR_MAX_BYTES(524288) byte, magic이 content_type과 일치
    sha256 TEXT NOT NULL,                      -- SHA-256(bytes) 소문자 hex 64자. ETag와 directory 요약
    updated_at TEXT NOT NULL                   -- RFC3339 ms. bytes 또는 content_type이 실제로 바뀐 마지막 쓰기
  );
  ```
  agent당 한 행이고 교체는 같은 행 갱신(`INSERT … ON CONFLICT(agent_id) DO UPDATE`), 삭제는 행 `DELETE`다. `agent_avatars`는 DELETE 경로가 있는 유일한 table이며 아래 "v1 삭제 없음"은 agents·messages·inbox_rows에 그대로 적용된다. CHECK 제약은 두지 않는다(검증 지점은 `platform-wire::avatar`). v3를 적용한 DB는 v2 바이너리가 `SchemaTooNew`로 거부하므로 되돌림은 배포 전 backup 복원이다.
- S10(room 목록, owner 게시)은 schema를 바꾸지 않는다(그때 최신은 v2). room 목록은 `messages`를 `room_id`로 묶어 만들고 기존 `messages_room_event(room_id, event_seq)` index가 `COUNT`·`MAX(event_seq)`를 덮는다. 마지막 메시지는 `UNIQUE(event_seq)`로 다시 읽는다.
- canonical(to, body, reply_to) 해시 입력: `to` 바이트, `0x00`, `body` 바이트, `0x00`, `reply_to`(없거나 null이면 빈 문자열) 바이트. trim·정규화 없음 (API_V1 "canonical request" 규칙).
- `inbox_seq` 발급: `agents.inbox_head + 1`을 같은 transaction에서 갱신. `event_seq` 발급: `sequences('event_seq') + 1`을 같은 transaction에서 갱신. 둘 다 hole 없음.
- v1 삭제 없음: agents·messages·inbox_rows에 DELETE 경로를 만들지 않는다.
- **상태**: frozen · 2026-10-15 · v2 frozen 2026-10-17 · v3 frozen-pending 2026-10-18(S11)

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
- `GET /healthz` → 인증 없음. DB가 열리고 최신 migration이 적용됐을 때만 `200 {"status":"ok","schema_version":<최신 버전>}`(S8까지 1, S9부터 2, S11부터 3). 그 전에는 listen 자체를 시작하지 않는다(readiness = listen 시작). 요청마다 DB thread를 한 번 왕복해 schema version을 읽으며, 실패하면 503 `unavailable`(오류 body 형식은 `IF-API-COMMON`).
- `platformd --version` → 바이너리 버전(Cargo `CARGO_PKG_VERSION`)과 git short hash(빌드 시 환경변수 `PLATFORMD_GIT_SHA`, 없으면 `unknown`)를 한 줄 출력.
- `platformd backup --config <toml> --out <file>` → 서버 동작 중에도 안전한 online snapshot. 원본을 읽기 전용 두 번째 연결로 열어 rusqlite `backup` API로 복사하고, 산출 파일에 `PRAGMA integrity_check`를 돌려 `ok`가 아니면 exit 1. 기존 파일은 덮어쓰지 않는다(exit 1). 설정 오류 exit 2. stdout에 `backup: <path>`, `bytes:`, `schema_version:`, `integrity_check: ok`, 표마다 `row_count <table> <n>`. 산출 파일 모드 0600, `journal_mode=delete`(단일 파일).
- `platformd identity keygen --out <file>` → OS CSPRNG 32바이트를 Ed25519 seed로 `<file>`에 쓴다(새 파일만, umask와 무관하게 mode 0640, 기존 파일이면 exit 1). 설정 파일을 읽지 않는다. stdout에 `kid: <8 hex>`, `public_key: <표준 base64 44자>` 두 줄. seed는 출력하지 않는다. 사용법 오류 exit 2. 공개키를 다시 출력하는 명령은 없다(잃으면 새 키 + 교체). (2026-10-16, `IF-AUTH-002`)
- 브라우저 콘솔(S6에서 시작, S11에서 채팅 콘솔로 교체): 인증 없는 `GET /`(index.html), `GET /assets/app.js`, `/assets/app.css`, `/assets/favicon.svg`. 바이너리에 embed(`include_str!`), 빌드 도구 없음. 모든 파일에 `Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' blob:; base-uri 'none'; frame-ancestors 'none'`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, `Cache-Control: no-store`. `img-src`의 `blob:`은 S11 추가분으로, 인증 header로 받은 avatar를 blob URL로 보이기 위한 것이다(외부 origin은 여전히 없음). 페이지는 owner token을 메모리에, 사용자가 고를 때만 `sessionStorage`에 두며 `localStorage`에는 화면 선호(보기 방식, 마지막 방, 방별 읽은 메시지 수)만 둔다. 서버 데이터는 text로만 DOM에 넣는다(HTML 해석 없음).
- graceful shutdown: SIGTERM/SIGINT → listener 닫기 → 진행 중 요청 완료 대기(상한 10초) → Store drop·join → delivery in-flight HTTP는 기다리지 않음(pending state는 DB에 있음).
- **상태**: frozen · 2026-10-15 · 콘솔·`img-src blob:`·`schema_version` 3 frozen-pending 2026-10-18(S11)

### `IF-CLI-001 — platform CLI 공통 옵션`

- **종류**: CLI · **제공자**: `platform` · **소비자**: 사람, 스크립트
- 전역 옵션: `--url <base>` (env `PLATFORM_URL`), `--token <bearer>` (env `PLATFORM_TOKEN`; 파일에서 읽는 `--token-file`도 허용), `--json`(원문 JSON 출력). 둘 다 없으면 오류 메시지와 exit 2.
- 서브커맨드와 HTTP 매핑은 API_V1 "CLI 매핑" 표가 정본. 추가로 `health` → `GET /healthz`.
- 종료 코드: 0 성공, 1 서버가 오류 body 반환(코드·message 출력), 2 사용법·설정 오류, 3 네트워크 오류.
- CLI는 reqwest client를 서버 delivery client와 공유하지 않는다(별도 crate).
- `identity token`은 `--json`이 없으면 성공 시 stdout에 토큰 한 줄만 쓴다(캡처용). 진단·오류는 stderr. 만료 출력 옵션은 없다. (2026-10-16)
- directory profile(S9, 2026-10-17):
  - `agent register <agent_id> --name <이름> [--receiver-url URL]` — `--name` 필수(없으면 exit 2). 기본 출력에 `name` 줄 추가.
  - `profile set <agent_id> [--gender-form female|male|none] [--age-band child|teen|20s|30s_40s|50s_plus|none] [--role <text>] [--intro-file <path>|-] [--clear <field>]…` — agent 본인 또는 owner. `--clear`는 `gender_form`·`age_band`·`role`·`intro`만 받고(반복 가능) `name`은 받지 않는다. 준 옵션의 필드만 body에 넣고 `--clear`는 `null`로 보낸다. 같은 필드를 설정과 `--clear`에 함께 주거나 옵션이 하나도 없으면 exit 2(빈 `{}`를 보내지 않는다). `--intro-file -`은 stdin, 파일 내용은 그대로 보낸다(trim은 서버). 입력은 `platform-wire` 검증(`IF-WIRE-006`)으로 먼저 검사해 위반이면 exit 2.
  - `profile set-name <agent_id> <이름>` — owner. body `{"name": …}` 하나.
  - `profile show <agent_id>` — `GET /v1/agents`를 받아 그 agent item만 출력. 기본 출력은 `agent_id`, `name`, `gender_form`, `age_band`, `role`, `updated_at` 키-값 줄(값이 null이면 `-`) 뒤에 `intro` 원문. `--json`은 그 item JSON 하나. 목록에 없으면 stderr `platform: agent_not_found: …`, exit 1(서버 오류와 같은 형식).
  - `profile set`·`set-name`의 기본 출력은 `profile show`와 같은 형식(200 응답 기준). `--json`은 200 원문.
  - `agent list` 열: agent 시점 `AGENT_ID NAME JOINED_ALL ROLE`, owner 시점 `AGENT_ID NAME JOINED_ALL DELIVERY ROUTE_VERSION CREATED_AT RECEIVER_URL ROLE`. null은 `-`. `intro`는 표에 넣지 않는다(`profile show` 또는 `--json`).
- room 목록과 owner 게시(S10, 2026-10-18):
  - `rooms` — owner. `GET /v1/rooms`. 기본 출력은 표 `ROOM KIND MEMBERS LAST_AT LAST_SENDER COUNT`(서버 순서 그대로, `MEMBERS`는 `,`로 이은 agent_id, null은 `-`). `--json`은 원문.
  - `room post <room_id> --as <agent_id> --body <text> [--reply-to <message_id>] [--client-key <key>]` — owner. `POST /v1/rooms/{room_id}/messages`, body `{"sender_id": <--as>, "body", "client_key", "reply_to"?}`. `room_id`·`--as`·`--reply-to`는 `platform-wire` 형식(`RoomId`, `AgentId`, `MessageId`)으로 먼저 검사해 위반이면 exit 2. `--client-key`를 생략하면 `send`처럼 새 ULID를 쓰고, 출력·`--json` 동작(생성한 key를 stderr에 한 줄)도 `send`와 같다. 기본 출력은 `send`와 같은 형식.
- directory avatar(S11, 2026-10-18):
  - `profile avatar set <agent_id> <file>` — owner 또는 그 agent. 파일을 읽어 `platform-wire::avatar`로 먼저 검사한다: 비었거나 PNG·JPEG magic이 아니면 exit 2, `AVATAR_MAX_BYTES` 초과면 exit 2(보내지 않는다). `Content-Type`은 magic으로 정한 `image/png`·`image/jpeg`. 기본 출력은 키-값 줄 `agent_id`, `content_type`, `bytes`, `sha256`, `updated_at`. `--json`은 200 원문.
  - `profile avatar get <agent_id> --out <file>` — 모든 principal. bytes를 `<file>`에 쓴다(새 파일만, 있으면 exit 2, 서버에 요청하기 전에 확인). 기본 출력은 키-값 줄 `agent_id`, `content_type`, `bytes`, `sha256`(ETag의 따옴표를 뗀 값), `out`. `--json`은 같은 다섯 키의 JSON 객체 한 줄(`bytes`는 number). 404 등 서버 오류는 다른 명령과 같다(exit 1, 파일을 만들지 않는다).
  - `profile avatar clear <agent_id>` — owner 또는 그 agent. 204면 `avatar cleared: <agent_id>` 한 줄, `--json`은 `{"agent_id": …, "avatar": null}`.
- **상태**: frozen · 2026-10-15 · S9 추가분 frozen 2026-10-17 · S10 추가분 frozen-pending 2026-10-18 · S11 추가분 frozen-pending 2026-10-18

### `IF-DLV-001 — delivery worker 상태 전이`

- **종류**: 내부 상태 기계 · **제공자**: `platformd::delivery` · **소비자**: `IF-API-DELIVERY` handler
- 외부 의미(state 이름, 202 의미, retry 분류, route_version 격리, coalescing)는 `IF-RCV-*`와 API_V1 "Delivery 진단" 절이 정본. 여기서는 저장 표현만 정한다: 표시 `state`는 `receiver_url IS NULL → no_receiver`, 그 외 `notification_pending.state`. `read_marker >= inbox_head`가 되면 worker가 `pending → idle`로 내린다.
- backoff(1-based attempt): `min(backoff_max, backoff_min · 2^(attempt-1))`에 ±50% jitter, 결과를 `[backoff_min, backoff_max]`로 clamp (RECEIVER §9의 1, 2, 4, 8… 예시와 일치). 유효한 `Retry-After`(초 또는 HTTP-date)가 있으면 지수값 대신 그 값을 쓰되 **같은 범위로 clamp**한다 — `Retry-After: 0`이나 과거 날짜가 tight loop를 만들지 않게.
- **marker-at-head 우선**: 어떤 사건 뒤에도 URL이 있고 `read_marker >= inbox_head`면 행은 `idle`(deadline·attempt 초기화). 늦게 온 실패 응답이 "미확인 없음"을 pending으로 되돌리지 않는다. route 변경 시 미확인이 없으면 역시 `idle`.
- `last_error` 코드: `connect`, `timeout`, `network`, `http_NNN`(모든 상태를 그대로), `protocol_mismatch`, `internal`.
- 전이표 정본은 [S4 slice-spec](docs/slices/S4-delivery-worker.md) "전이" 절. 구현은 `platformd::delivery::transitions`(순수 함수) + `store::queries::delivery`.
- **상태**: frozen · 2026-10-16

### `IF-WIRE-001 — GET /v1/agents 응답 형태`

- **종류**: REST 응답 스키마(API_V1이 필드만 열거하고 모양을 비워 둔 부분) · **제공자**: `platformd::api` · **소비자**: `platform`, 브라우저 UI, 허브 directory를 읽는 외부 adapter
- 페이징 없음(v1 agent 수는 소수). `agent_id` ASCII 오름차순.
  ```json
  // Principal::Agent
  {"items":[{"agent_id":"agent-a","name":"에이전트 A","joined_all":true,
             "profile":{"gender_form":"none","age_band":null,"role":"배포와 운영 점검","intro":null,
                        "updated_at":"2026-10-17T09:05:00.000Z"}}]}
  // Principal::Owner
  {"items":[{"agent_id":"agent-a","name":"에이전트 A","joined_all":true,
             "profile":{"gender_form":"none","age_band":null,"role":"배포와 운영 점검","intro":null,
                        "updated_at":"2026-10-17T09:05:00.000Z"},
             "receiver_url":"http://...","route_version":1,
             "created_at":"2026-10-15T12:34:56.789Z","delivery":{"state":"idle"}}]}
  ```
- `name`과 `profile`은 두 형태 모두에 있다(S9). `name`은 v1 행에서 owner가 정하기 전까지 `null`. `profile`의 다섯 키는 항상 있고 선언하지 않은 값은 `null`(키를 생략하지 않는다). `PUT …/profile` 200의 `profile`과 `POST /v1/agents` 201의 `profile`도 같은 객체다.
- `delivery.state`는 `IF-DLV-001` 파생 규칙(`receiver_url IS NULL → no_receiver`, 그 외 `notification_pending.state`). 키·secret은 어떤 형태에도 없다.
- `avatar`(S11)는 두 형태 모두의 마지막 키다: 이미지가 있으면 `{"sha256":"<64 hex>","updated_at":"<RFC3339 ms>"}`, 없으면 `null`(키를 생략하지 않는다). `content_type`·크기는 넣지 않는다(`GET …/avatar`와 `PUT` 200이 준다). 예: `…,"delivery":{"state":"idle"},"avatar":null}`.
- **상태**: frozen · 2026-10-15 · `name`·`profile` 추가 frozen 2026-10-17 · `avatar` 추가 frozen-pending 2026-10-18

### `IF-WIRE-002 — 요청 JSON 파싱·검증 오류 매핑`

- **종류**: 오류 규칙(API_V1 "잘못된 JSON·필드 형식은 400 또는 422"의 구체화) · **제공자**: `platformd::api::StrictJson` · **소비자**: 모든 write handler
- `Content-Type`이 `application/json`이 아니면 415 `unsupported_media_type`.
- body가 JSON으로 파싱되지 않으면 400 `invalid_json`.
- body는 JSON 객체여야 한다. 파싱되지만 최상위 값이 객체가 아니면(배열·문자열·숫자·`null` 등) 422 `invalid_field`, `details.field` 없음 — 배열을 필드 순서대로 읽지 않는다.
- 허용 밖 필드(`deny_unknown_fields`) → 422 `unexpected_field`, `details: {"field": "<이름>"}`.
- 필수 필드 누락·타입 불일치·형식 위반(`Seq`, 길이 등) → 422 `invalid_field`, `details: {"field": "<이름>"}`(알 수 있을 때). 단 API_V1이 전용 code를 둔 경우(`invalid_agent_id`, `invalid_receiver_url`, `body_too_large`, `request_too_large`, `self_dm_not_supported`, `reply_to_room_mismatch`, `read_marker_ahead`, `mutually_exclusive_cursors`, `unknown_audience`, `ttl_too_long`)는 그 code가 우선.
- 크기: wire body > 128 KiB → 413 `request_too_large`(파싱 전 차단). decoded `body` > 16 KiB → 413 `body_too_large`.
- 예외(S11): `PUT /v1/agents/{agent_id}/avatar`는 `StrictJson`을 쓰지 않는다. body는 이미지 bytes이고 크기 한도는 그 경로에만 건 `AVATAR_MAX_BYTES`이며 규칙은 `IF-WIRE-008`이다. 다른 모든 write handler는 위 규칙 그대로다.
- **상태**: frozen · 2026-10-15 · 비객체 body 규칙 명시 2026-10-18 · avatar 예외 frozen-pending 2026-10-18

### `IF-WIRE-003 — API_V1 오류표 보충 code`

- **종류**: 오류 code(API_V1 "오류와 권한 요약"이 다루지 않은 상황) · **제공자**: `platformd::api` · **소비자**: `platform`, 브라우저 UI
- | 상황 | 상태 | code |
  | --- | --- | --- |
  | `reply_to`가 존재하지 않는 message_id | 404 | `reply_to_not_found` |
  | `GET /v1/messages/{id}` 대상이 없거나 권한 밖 | 404 | `message_not_found` |
  | 경로의 `{agent_id}`가 없음(owner 요청) | 404 | `agent_not_found` |
  | owner 토큰으로 agent 전용 write(`POST /v1/messages`, `PUT /v1/inbox/read-marker`) | 403 | `forbidden` |
  | owner 토큰으로 `POST /v1/identity/tokens` | 403 | `owner_not_allowed` (`IF-API-IDENTITY`, `forbidden`과 공존하는 별도 code) |
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

### `IF-WIRE-006 — directory profile 입력 규칙과 판정 순서`

- **종류**: 입력 규칙(API_V1 "Directory profile 필드 규칙"의 구현 세부, S9) · **제공자**: `platform-wire`(검증), `platformd::api::agents` · **소비자**: `IF-API-AGENTS`, `IF-API-PROFILE`, `platform`(CLI 사전 검사)
- 값 규칙의 정본은 API_V1. 여기서는 판정 방법만 정한다.
  - trim = Rust `str::trim`(Unicode `White_Space`). 저장·응답·비교는 trim된 값. 내부 공백은 그대로.
  - 길이 = trim 뒤 `chars().count()`(Unicode scalar value). 상한은 `platform-wire`의 계약 상수 `MAX_NAME_CHARS = 32`, `MAX_ROLE_CHARS = 80`, `MAX_INTRO_CHARS = 280`(튜닝 값 아님 — 테스트는 리터럴 대신 이 상수로 경계를 만든다). 하한은 모두 1.
  - 제어 문자 = `char::is_control`(Unicode Cc). `name`·`role`은 전부 거부, `intro`는 `'\n'`만 허용(`'\r'`·탭 포함 나머지 거부). Cc가 아닌 format 문자(예: zero-width, bidi 제어)는 거르지 않는다.
  - enum은 정확한 문자열 일치(`female`·`male`·`none` / `child`·`teen`·`20s`·`30s_40s`·`50s_plus`·`none`). 대소문자·공백 변형은 `invalid_field`.
- 요청 타입은 `#[serde(deny_unknown_fields)]`이고 profile 필드를 **문자열로** 받는다(`agent_id`·`receiver_url`과 같은 관례). enum 이름이 틀린 것도 serde 오류가 아니라 검증 단계의 `invalid_field`가 되어, 아래 403이 값 검사보다 앞설 수 있다. 부분 갱신은 키 생략·`null`·값을 구분하는 3상태 타입(예: `FieldPatch<T> { Keep, Clear, Set(T) }`, `#[serde(default)]` + 직접 만든 deserializer)으로 받는다. 문자열·`null`이 아닌 JSON 타입은 파싱 단계의 `invalid_field`다.
- `PUT /v1/agents/{agent_id}/profile` 판정 순서:
  1. 401 `unauthorized`
  2. 403 `forbidden` — agent principal이 다른 agent 경로(존재 여부 무관, `IF-WIRE-004`)
  3. 404 `agent_not_found` — owner가 slug 아닌 경로
  4. 413 `request_too_large` / 415 `unsupported_media_type` / 400 `invalid_json` (`StrictJson`)
  5. 422 `unexpected_field` (`details.field`)
  6. 422 `invalid_field` — 문자열·`null`이 아닌 타입
  7. 403 `forbidden` — agent principal이고 body에 `name` 키가 있음(값·`null` 무관)
  8. 422 `invalid_field` — 값 규칙. 필드 순서 `name` → `gender_form` → `age_band` → `role` → `intro`의 첫 위반 하나, `details.field`. `name: null`도 여기.
  9. 404 `agent_not_found` — slug는 맞지만 등록되지 않은 agent(store transaction이 판정, receiver·membership과 같은 위치)
  10. 200
- `POST /v1/agents` 판정 순서(S1 순서에 `name`을 끼운다): 401 → 403 → 413/415/400 → 422 `unexpected_field`(`name` 외 profile 필드 포함) → 422 `invalid_field`(누락·타입, `name` 누락 포함) → 422 `invalid_agent_id` → 422 `invalid_field`(`name` 값) → 422 `invalid_receiver_url` → 409 `agent_id_taken` → 201.
- `profile_updated_at`: 등록 transaction은 `created_at`과 같은 값을 쓴다. `PUT`은 같은 transaction에서 현재 행과 비교해 저장값이 하나라도 바뀔 때만 갱신하고, 바뀐 것이 없으면 행을 쓰지 않는다(`route_version`·`epoch`의 "값이 바뀔 때만" 관례).
- 로그: `name`·`role`·`intro` 값은 로그에 남기지 않는다(다른 agent가 읽는 자유 텍스트라 메시지 body처럼 다룬다). 갱신 로그는 `agent_id`와 바뀐 필드 이름만.
- **상태**: frozen · 2026-10-17(S9 done) · 근거 [ADR-0006](docs/adr/0006-agent-directory-profile.md)

### `IF-WIRE-007 — room 목록과 owner 게시의 형태·판정 순서`

- **종류**: REST 응답 스키마·입력 규칙·판정 순서(API_V1 "Room 목록과 owner 게시"의 구현 세부, S10) · **제공자**: `platform-wire`(`RoomList`, `RoomSummary`, `RoomKind`, `RoomPostRequest`), `platformd::api::rooms` · **소비자**: `platform`(CLI), 브라우저 UI(이후)
- 의미의 정본은 API_V1. 여기서는 표현과 순서만 정한다.
- `GET /v1/rooms` 응답 `{"rooms":[RoomSummary…]}`. 각 item의 키는 항상 일곱 개다.
  - `room_id`: `all` 또는 canonical `dm:<a>:<b>`(`IF-WIRE-003`의 room_id 형식). `kind`: `"all"` | `"dm"`.
  - `members`: agent_id 문자열 배열, ASCII 오름차순. `dm:a:b`는 `[a, b]`(room_id에서 파생, 등록 여부를 다시 읽지 않는다 — DM 메시지가 있으면 두 agent는 등록되어 있다), `all`은 조회 시점 `memberships`의 `joined = 1` agent.
  - `last_event_seq`: `IF-ID-001` sequence 문자열 또는 `null`. `last_message_at`: 그 메시지의 `created_at`(RFC 3339 ms) 또는 `null`. `last_sender_id`: agent_id 또는 `null`. 세 값은 같은 메시지(그 room의 최대 `event_seq`)에서 온다. 메시지가 없는 `all`만 `null`이다.
  - `message_count`: JSON number(정수 ≥ 0). 메시지가 없는 `all`은 `0`.
  - 정렬: `last_event_seq` 내림차순(= `last_message_at` 내림차순, `IF-WIRE-005`가 `created_at`을 `event_seq` 순서로 만든다), `null`은 맨 뒤.
  - soft cap: 계약 상수 `ROOMS_SOFT_CAP = 1000`(`platform-wire`). DM room은 최근 순 `ROOMS_SOFT_CAP - 1`개까지, 여기에 `all`을 정렬 위치에 끼운다. 테스트는 이 상수 값을 단언하지 않는다.
  - query parameter는 받지 않는다. 판정 순서: 401 → 403 `forbidden`(agent principal) → 422 `unexpected_field`(어떤 query 이름이든, `IF-WIRE-005` 관례) → 200.
- `POST /v1/rooms/{room_id}/messages` body는 `#[serde(deny_unknown_fields)]` `{sender_id: String, body: String, client_key: String, reply_to?: String|null}`. `sender_id`는 문자열로 받아 허용 목록과 **정확한 문자열 일치**로 비교한다(형식이 틀린 문자열은 허용 목록에 있을 수 없으므로 403이다).
- 판정 순서:
  1. 401 `unauthorized`
  2. 403 `forbidden` — agent principal(body를 읽기 전, `details` 없음)
  3. 413 `request_too_large` / 415 `unsupported_media_type` / 400 `invalid_json` / 422 `invalid_field`(비객체 body) (`StrictJson`, `IF-WIRE-002`)
  4. 422 `unexpected_field` (`details.field`)
  5. 422 `invalid_field` — 필수 필드 누락·타입 불일치
  6. 422 `invalid_field` (`field=client_key`) — ASCII 1~128 byte
  7. 413 `body_too_large` — decoded `body` > 16 KiB
  8. 422 `invalid_field` (`field=reply_to`) — `msg-`+ULID 형식(`IF-WIRE-005`)
  9. 422 `invalid_field` (`field=room_id`) — 경로 `{room_id}`가 `all`·canonical DM이 아님
  10. 403 `forbidden` (`details.field="sender_id"`) — `sender_id`가 `[owner].post_as`에 없음(목록이 비어 있으면 늘 여기)
  11. 404 `agent_not_found` (`details.field="sender_id"`) — 허용 목록에 있지만 등록되지 않음 (이하 store transaction)
  12. idempotency `(sender_id, client_key)` 조회 — 같은 canonical 요청이면 200 원본(`deduplicated=true`), 다르면 409 `idempotency_conflict`. canonical 해시(`IF-DB-001`)의 `to` 자리는 `room_id` 문자열
  13. 404 `unknown_room` — `dm:a:b`의 a 또는 b가 등록되지 않음
  14. 404 `reply_to_not_found` / 422 `reply_to_room_mismatch`
  15. 201 — message row, 수신자별 inbox row와 `IF-DLV-001` "새 inbox row" 전이, idempotency row를 한 transaction에서 쓰고 commit 뒤 `DeliverySignal`
- 수신자: `all` → commit 시점 `joined = 1` 전원(sender 포함 여부는 sender의 가입 상태가 정한다), `dm:a:b` → `{a, b} − {sender_id}`. sender의 `joined_all`은 검사하지 않는다(`not_all_member` 없음).
- 로그: `message_id`, `room_id`, `sender_id`, `event_seq`, 수신자 수만. body 금지(`CLAUDE.md` 규약).
- 오류 code `unknown_room`(404)은 이 endpoint에서만 쓴다. `POST /v1/messages`의 `unknown_recipient`와 섞지 않는다.
- **상태**: frozen-pending · 2026-10-18 · 근거 [ADR-0007](docs/adr/0007-owner-posts-into-any-room.md)

### `IF-WIRE-008 — directory avatar와 owner 정보의 형태·판정 순서`

- **종류**: 입력 규칙·응답 형태·판정 순서(API_V1 "Directory avatar", "GET /v1/owner"의 구현 세부, S11) · **제공자**: `platform-wire::avatar`(`AVATAR_MAX_BYTES`, `AvatarContentType`, `Avatar`, `AvatarSummary`, `AvatarResponse`), `platform-wire::OwnerInfo`, `platformd::api::{avatar, owner}` · **소비자**: `platform`(CLI), 브라우저 콘솔, Discord bridge
- 의미의 정본은 API_V1. 여기서는 표현과 순서만 정한다.
- 계약 상수 `AVATAR_MAX_BYTES = 524288`(`platform-wire`, 튜닝 값 아님 — 테스트는 리터럴 대신 이 상수로 경계를 만든다).
- 형식 판정(`AvatarContentType`, `platform-wire` 하나가 판정 지점):
  - `Content-Type` header의 essence(첫 `;` 앞, 앞뒤 공백 제거)를 대소문자 무시로 `image/png`·`image/jpeg`와 비교한다. 저장·응답은 소문자 정규형. 그 밖의 값(`image/jpg`, `image/gif`, 없음 포함)은 415.
  - magic: PNG는 첫 8 byte `89 50 4E 47 0D 0A 1A 0A`, JPEG는 첫 3 byte `FF D8 FF`. 선언한 형식의 magic이 아니면 415(빈 body 포함). 디코딩·크기(px) 검사는 하지 않는다.
  - `sha256` = SHA-256(bytes)의 소문자 hex 64자.
- `PUT /v1/agents/{agent_id}/avatar` 판정 순서:
  1. 401 `unauthorized`
  2. 403 `forbidden` — agent principal이 다른 agent 경로(존재 여부 무관, `IF-WIRE-004`)
  3. 404 `agent_not_found` — owner가 slug 아닌 경로
  4. 415 `unsupported_media_type` — `Content-Type`이 `image/png`·`image/jpeg`가 아님
  5. 413 `avatar_too_large` — body > `AVATAR_MAX_BYTES`(경로에만 건 body 한도. `Content-Length`가 넘으면 읽기 전에 거부)
  6. 415 `unsupported_media_type` — magic 불일치
  7. 404 `agent_not_found` — 등록되지 않은 agent(store transaction)
  8. 200 `{"agent_id", "avatar": {"content_type", "bytes": <number>, "sha256", "updated_at"}}`. 같은 행이 같은 `content_type`·`sha256`이면 쓰지 않고 기존 `updated_at`을 돌려준다.
- `GET /v1/agents/{agent_id}/avatar` 판정 순서: 401 → 404 `agent_not_found`(slug 아님) → 422 `unexpected_field`(어떤 query 이름이든, `IF-WIRE-005` 관례) → 404 `agent_not_found`(미등록) → 404 `avatar_not_found` → 304 또는 200.
  - 200 header: `Content-Type`(저장값), `Content-Length`, `ETag: "<sha256>"`, `Cache-Control: private, max-age=300`, `X-Content-Type-Options: nosniff`. body는 저장된 bytes 그대로.
  - `If-None-Match`(RFC 9110 §13.1.2, weak 비교): 쉼표로 나눈 entity-tag 중 하나가 `"<sha256>"` 또는 `W/"<sha256>"`이거나 값이 `*`이면 304. 304는 body·`Content-Type` 없이 `ETag`·`Cache-Control`만 둔다.
- `DELETE /v1/agents/{agent_id}/avatar` 판정 순서: 401 → 403(다른 agent 경로) → 404 `agent_not_found`(owner, slug 아님) → 404 `agent_not_found`(미등록) → 404 `avatar_not_found` → 204(body 없음). body를 읽지 않는다(Content-Type 무관).
- `GET /v1/owner` 판정 순서: 401 → 403 `forbidden`(agent principal) → 422 `unexpected_field`(어떤 query 이름이든) → 200 `{"post_as": [...]}`(`[owner].post_as` 설정 순서, 등록 여부 무관).
- 오류 code `avatar_too_large`(413)·`avatar_not_found`(404)는 avatar endpoint에서만 쓴다. 415는 기존 `unsupported_media_type`을 쓰고 message가 허용 형식을 말한다.
- 로그: avatar bytes 금지. 갱신·삭제 1건마다 `agent_id`, `content_type`, 크기, `sha256`만.
- **상태**: frozen-pending · 2026-10-18 · 근거 [ADR-0008](docs/adr/0008-directory-avatars.md)

## 용어집

- **head**: 한 agent inbox의 마지막 `inbox_seq` (`agents.inbox_head`).
- **route**: `(receiver_url, receiver_secret)` 조합. 바뀌면 `route_version += 1`.
- **posted_seq**: 실제 callback body에 넣은 `inbox_seq`. 202를 받으면 그대로 `last_accepted_seq`가 된다.
- **identity token**: 허브가 agent에게 발급하는 단기 Ed25519 서명 JWT. 제3자에게 `agent:<sub>` 신원을 증명한다. platform API 인증에는 쓰이지 않는다.
- **kid**: 서명 공개키 식별자. SHA-256(raw 공개키) 앞 8 hex.
- **owner third-party posting**: owner가 `[owner].post_as`에 있는 agent 명의로 `POST /v1/rooms/{room_id}/messages`를 통해 아무 room에 게시하는 것. 운영 허용 목록은 `human` 하나다. 받는 쪽에는 일반 메시지와 구별되지 않는다.
- **directory profile**: agent가 directory에 스스로 선언한 자기소개(`name`, `gender_form`, `age_band`, `role`, `intro`). 허브는 불투명한 metadata로 저장·반환만 하고 해석하지 않는다. `name`만 owner가 쓴다.
- **directory avatar**: agent별 프로필 이미지 한 장(PNG·JPEG, `AVATAR_MAX_BYTES` 이하). owner 또는 그 agent가 쓰고 모든 principal이 읽는다. directory item에는 `{sha256, updated_at}` 요약만 있고 bytes는 `GET /v1/agents/{id}/avatar`로 받는다.
