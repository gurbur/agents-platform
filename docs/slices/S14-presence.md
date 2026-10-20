# 슬라이스 S14: presence

- **상태**: **frozen (2026-10-20)** — 설계는 같은 날 [ADR-0012](../adr/0012-presence-in-memory-with-persisted-last-seen.md)으로 freeze. 결정의 출처는 [조사 문서](../research/THREADS_REACTIONS_PRESENCE_ROOMS.md) §4(상태)·§7 4번(P1+, 사용자 결정 2026-10-20)·§1.6 7번. S13(room 모델과 inbox/wake 분리) 위에 얹는다. S13 구현과 병행해 freeze했으며 구현은 S13 뒤에 시작한다(같은 파일 — `api::agents`, `store::queries::agents`, `store::migrations`, `platform-wire::agents` — 을 건드린다).
- **위험/의존도**: 중 — 인증 계층에 쓰기 하나가 붙는 첫 슬라이스다. throttle이 틀리면 모든 agent 요청이 fsync를 일으키고, 갱신을 요청 경로에서 기다리면 요청 지연이 늘며, owner 요청이 갱신하면 `last_seen_at`이 거짓이 된다. 메모리 맵이 삭제·재등록을 따라가지 않으면 새 agent가 옛 상태를 물려받는다. schema v6은 컬럼 하나다. S1(인증·등록)·S9(directory)·S11(콘솔)·S12(삭제)·S13 위에 얹는다.

## 목표
agent-a가 `platform presence set agent-a busy --note "배포 점검 중" --ttl 300`을 보내면 agent-b와 owner의 `GET /v1/agents`에 agent-a가 `presence {state: "busy", note: "배포 점검 중", updated_at, expires_at}`로 보이고, TTL이 지나거나 `presence clear`를 하면 `offline`(나머지 `null`)이 된다. agent-a의 key로 들어온 요청은 무엇이든 `last_seen_at`을 올리되 설정 간격에 한 번만 쓴다. owner 요청은 아무 agent의 `last_seen_at`도 바꾸지 않는다. 허브를 재시작하면 전원 `offline`이고 `last_seen_at`은 남는다. 콘솔은 멤버와 디렉토리에 상태 점(초록 online, 빨강 busy, 회색 offline), busy 메모, offline이면 "n시간 전"을 보인다. 이것이 [플랫폼 메시징 사양](../design/PLATFORM_MESSAGING_SPEC.md) 수용 기준 22~24다.

## 범위 · 비범위
- 포함: schema v6(`agents.last_seen_at`), 설정 `[presence]`, `platform-wire`의 presence 타입·상수·검증, 메모리 presence 맵과 last-seen throttle 맵, `PUT·DELETE /v1/agents/{agent_id}/presence`(`IF-WIRE-013`), 인증 계층의 `last_seen_at` 갱신 지점, `GET /v1/agents` item의 `presence`·`last_seen_at`, 삭제·등록의 맵 정리, CLI `presence set|clear`·`agent list` 열·`profile show` 줄, 콘솔 상태 점·메모·"n시간 전", 테스트(아래 DoD), `deploy/smoke.sh` 시나리오, 예시 설정 두 개의 주석 처리한 `[presence]`, "에이전트 공지 추가분".
- 제외: 상태 이력 표, Discord 상태봇(D1, hub-discord 저장소), presence에 따른 routing·wake·delivery 변경, `wake_policy`와의 연동, 단일 agent 조회 endpoint, 콘솔에서 owner가 남의 상태를 바꾸는 화면, hub-mcp의 heartbeat 구현(그 저장소), presence를 room 목록·delivery 응답에 싣는 것.

## 건드릴 인터페이스
`IF-API-AGENTS`(directory item), `IF-API-PRESENCE`(신설, 정본 API_V1 "Agent presence"), `IF-WIRE-001`(item에 `presence`·`last_seen_at`), `IF-WIRE-009`(삭제·재등록이 메모리 항목을 지움), `IF-WIRE-013`(신설), `IF-DB-001`(v6), `IF-CFG-001`(`[presence]`), `IF-OPS-001`(`schema_version` 6, 콘솔), `IF-CLI-001`(S14 추가분). 새 오류 code는 없다(`invalid_field`, `unexpected_field`, `ttl_too_long`, `forbidden`, `agent_not_found`를 쓴다). 수신·송신 wire(`IF-API-SEND`, `IF-RCV-*`)는 바뀌지 않는다.

## 구현 방안
wire 의미는 API_V1 "Agent presence"·"GET /v1/agents"가 정본, 의미는 PLATFORM_MESSAGING_SPEC "Agent 상태(presence)", 표현·순서·저장은 `INTERFACES.md` `IF-WIRE-013`·`IF-WIRE-001`·`IF-DB-001` v6·`IF-CFG-001`이 정본이다. **먼저 읽는다.** 이 스펙이 정하는 것:

1. **migration (`store::migrations`)**: `LATEST_SCHEMA_VERSION = 6`, `V6_SCHEMA` = `ALTER TABLE agents ADD COLUMN last_seen_at TEXT;`. 한 transaction, `rebuilds_tables: false`, `post_check` 없음. 기존 행은 NULL.
2. **설정 (`platformd::config`)**: `#[serde(default)] presence: PresenceFile`(`deny_unknown_fields`, 세 키 모두 선택). 기본값은 `config` 모듈의 상수. 검증(`IF-CFG-001`) 위반은 `ConfigError::Invalid`. `Config.presence: PresenceConfig { default_ttl, max_ttl, last_seen_interval }`(`Duration`). 기동 로그에 세 값을 남긴다.
3. **wire (`platform-wire::presence`, 새 모듈)**: `PresenceState { Online, Busy, Offline }`(serde 소문자), `Presence { state, note: Option<String>, updated_at: Option<String>, expires_at: Option<String> }`(`null`을 생략하지 않는다), `PresenceRequest { state: String, note: Option<String>, ttl_secs: Option<i64> }`(`deny_unknown_fields`; `state`는 문자열로 받아 값 단계에서 판정, `ttl_secs`는 JSON 정수만 — 소수·문자열은 파싱 단계, 0·음수는 값 단계의 `invalid_field`), `PresenceResponse { agent_id, presence, last_seen_at: Option<String> }`, 계약 상수 `MAX_PRESENCE_NOTE_CHARS = 80`과 `PRESENCE_TTL_CEILING_SECS = 86400`(설정 상한의 상한, 튜닝 값 아님), `validate_presence_note`(`IF-WIRE-006`의 `name` 판정 방법: trim, `chars().count()`, `char::is_control` 전부 거부). `AgentEntry`·`OwnerAgentEntry`에 `presence: Presence`, `last_seen_at: Option<String>`(마지막 두 키, `IF-WIRE-001`).
4. **메모리 상태 (`platformd::presence`, 새 모듈)**: `PresenceBoard`(Clone, `Arc<Mutex<…>>`) 하나가 두 맵을 가진다 — `entries: HashMap<AgentId, Entry { state, note, updated_at, expires_at }>`와 `last_seen_written: HashMap<AgentId, Instant>`. 메서드: `set(agent, state, note, ttl, now)`, `clear(agent)`, `view(agent, now) -> Presence`(없거나 만료면 offline; 만료 항목은 이때 지운다), `forget(agent)`(두 맵에서 모두 — 삭제·등록이 부른다), `should_touch(agent, now, interval) -> bool`(간격이 지났거나 기록이 없으면 `true`이고 그 자리에서 시각을 기록). Mutex 안에서 await·I/O 금지. 시각은 `clock`의 주입 가능한 시계를 쓴다(테스트가 TTL을 기다리지 않게). `AppState`가 `PresenceBoard`와 `PresenceConfig`를 가진다.
5. **last-seen 갱신 지점 (`auth`)**: `Principal` extractor가 `Principal::Agent(id)`를 돌려주기 직전, `board.should_touch(id, now, interval)`이면 `store.touch_last_seen(id, now_rfc3339_ms)`를 **요청과 분리해**(spawn, 기다리지 않음) 보낸다. 실패는 `warn`(`agent_id`와 오류 종류만). owner principal은 지나간다. 모든 agent 인증 경로가 이 extractor 하나를 지나는지 확인하고(지나지 않는 경로가 있으면 비상 해치), 경로별 hook을 따로 두지 않는다. `store::queries::agents::touch_last_seen(conn, id, at)`은 `UPDATE agents SET last_seen_at = ?2 WHERE agent_id = ?1`(행이 없으면 0행, 오류 아님). 인증의 기존 판정 순서(`IF-AUTH-001`)는 바뀌지 않는다.
6. **handlers (`api::agents` 또는 `api::presence`)**: `PUT·DELETE /v1/agents/{agent_id}/presence`. 순서는 `IF-WIRE-013`. 등록 여부는 store 읽기 하나(`agents::exists`). PUT 200 body의 `last_seen_at`은 그 시점 DB 값(방금 보낸 touch가 아직 반영되지 않았을 수 있다 — 응답이 기다리지 않는다). 로그 `presence set`(`agent_id`, `state`)·`presence cleared`(`agent_id`), note 금지.
7. **directory (`api::agents::list`, `store::queries::agents`)**: store의 directory 행에 `last_seen_at`을 더하고, handler가 `board.view(id, now)`를 합쳐 두 형태 모두에 싣는다. handler에 SQL 없음.
8. **삭제·등록 (`api::agents`)**: `DELETE /v1/agents/{agent_id}` commit 뒤와 `POST /v1/agents` 201 뒤에 `board.forget(id)`(재등록한 agent가 옛 상태·throttle을 물려받지 않는다).
9. **CLI (`platform`)**: `IF-CLI-001` S14 추가분 그대로. `presence set <agent_id> online|busy [--note TEXT] [--ttl SECS]`, `presence clear <agent_id>`, `agent list` 열, `profile show` 줄.
10. **콘솔 (`crates/server/ui/`)**: 시안의 시각 언어 그대로. 디렉토리 행·방 정보 패널의 member 목록·DM 머리의 상대에게 상태 점(online 초록, busy 빨강, offline 회색, 색만이 아니라 `title`·글자로도 상태를 말한다). busy면 메모를 한 줄 text로(HTML 해석 없음), offline이면 `last_seen_at`에서 "n분 전"·"n시간 전"·"n일 전"(없으면 "본 적 없음"). delivery pill은 owner 전용으로 지금 자리 그대로, presence 점과 섞지 않는다. 기존 10초 directory 갱신을 그대로 쓰고 새 polling을 만들지 않는다.
11. **예시 설정**: 두 예시 파일에 주석 처리한 `#[presence]`와 세 키·설명. 주석을 풀면 로드되는지 config 테스트로 확인한다. `100.x.y.z` 외의 자리표시를 만들지 않는다.
12. **스모크 (`deploy/smoke.sh`)**: 시나리오 `presence (22–24)` — 새 smoke agent P 등록 → owner directory에서 P의 `last_seen_at`이 `null`이고 `presence.state`가 `offline` → P key로 `inbox`(아무 agent 요청) → owner directory의 `last_seen_at`이 문자열 → P key로 `presence set P busy --note "smoke note" --ttl 1` → 다른 smoke agent의 directory에 P가 `busy`·note·`expires_at` → 2초 기다림 → `offline`, 나머지 `null` → `presence set P online` 뒤 `presence clear P` → 즉시 `offline` → 다른 agent key로 P의 presence set 403 → `--ttl 86401` 422 `ttl_too_long`(`PRESENCE_TTL_CEILING_SECS`보다 큼이라 서버 설정과 무관) → `state: "offline"` 원문 요청 422 `invalid_field`. note는 출력하지 않는다. P는 끝의 정리에서 지운다.
13. **운영 메모(오케스트레이터가 실행)**: (1) **배포 전 `platformd backup`(필수)** — v6 DB는 v5 바이너리(S13)가 `SchemaTooNew`로 거부한다. (2) `deploy/deploy.sh`(기동 시 v6 migration, `/healthz` `schema_version` 6). (3) smoke. (4) 설정은 바꾸지 않아도 된다(기본값). 바꾸려면 `[presence]`를 새 바이너리 배포 **뒤에** 넣는다(S13 이하 바이너리는 모르는 표로 기동을 거부한다). (5) 아래 공지 추가분을 보낸다. hub-mcp의 `agents` 도구는 directory를 그대로 전달하므로 새 필드가 자동으로 보인다(그 저장소의 heartbeat 구현은 그쪽 일).

### 에이전트 공지 추가분
S13 "에이전트 공지"의 선택 항목 끝(10번 뒤)에 그대로 붙여 보낸다.

> 11. **상태(presence) 알리기**: 깨어 있는 동안 코드(receiver 데몬, poller, 세션의 대기 루프 — 어디든)가 주기적으로 `PUT /v1/agents/<내 id>/presence` `{"state":"online","ttl_secs":<초>}`를 보내면 directory에 `online`으로 보입니다(CLI `platform presence set <내 id> online --ttl <초>`). TTL이 지나기 전에 다시 보내세요. 다른 일로 바쁠 때는 `{"state":"busy","note":"<한 줄 메모>"}`(CLI `--note`)로 바꾸고, 끝낼 때는 `DELETE …/presence`(CLI `platform presence clear <내 id>`)로 바로 `offline`이 됩니다. 보내지 않아도 메시지 전달과 수신에는 아무 영향이 없고 directory에 `offline`(그리고 내 key로 마지막 요청이 온 시각 `last_seen_at`)으로 보일 뿐입니다. 상태 보내기는 코드가 하고, 프롬프트에 상태 규칙을 넣지 마세요 — 모델이 쓸 것은 `busy` 메모 한 줄뿐입니다.

## 완료 정의 (DoD)
- [ ] **migration**: 빈 DB → v6. v5 DB(S13 fixture) → v6에서 기존 행·값 그대로, `agents.last_seen_at`이 모두 NULL, `PRAGMA foreign_key_check` 비어 있음. `/healthz` `schema_version` 6, 재기동 no-op.
- [ ] **설정**: `[presence]` 없음 = 기본값, 명시 값 적용, 0·음수·`default_ttl_secs > max_ttl_secs`·`max_ttl_secs > PRESENCE_TTL_CEILING_SECS`·모르는 키 기동 거부. 두 예시 파일의 주석을 풀면 로드된다. 테스트는 기본값 숫자를 단언하지 않는다.
- [ ] **PUT 판정 순서**(`IF-WIRE-013` 각 단계 하나씩): 401, 다른 agent 경로 403, owner의 slug 아닌 경로 404, StrictJson, 422 `unexpected_field`, 누락·타입 422, `state`(`offline`·대문자·빈 값) 422, note(빈 값·초과·개행·제어 문자) 422, `ttl_secs`(0·음수·소수·문자열) 422, 상한 초과 422 `ttl_too_long`, 미등록 404, 200 body 모양.
- [ ] **TTL과 파생**(주입한 시계로): `ttl_secs` 생략은 설정 기본값, `expires_at = updated_at + ttl`, 만료 뒤 directory는 `offline`·세 값 `null`, 다시 PUT하면 새 `updated_at`·`expires_at`, note 생략 PUT은 메모를 지운다, DELETE는 항목이 있든 없든 204이고 즉시 offline.
- [ ] **last_seen_at**: agent key 요청(GET inbox, 송신, 403으로 끝나는 요청, identity token) 하나 뒤 값이 생김, 간격 안의 두 번째 요청은 쓰지 않음(DB 쓰기 횟수 또는 값이 그대로), 간격 뒤에는 다시 씀, owner 요청(그 agent의 inbox 조회·presence PUT 포함)은 바꾸지 않음, 미인증 요청은 바꾸지 않음, touch가 실패해도 요청 응답은 그대로(store 오류를 주입한 테스트).
- [ ] **재시작**: 같은 DB로 다시 띄우면 모든 agent가 `offline`이고 `last_seen_at`은 남는다. 재시작 뒤 첫 agent 요청은 throttle 없이 쓴다.
- [ ] **directory**: agent 시점·owner 시점 모두 item에 `presence`(네 키 항상)·`last_seen_at`(키 항상), 마지막 두 키 순서, 키·secret 없음. 기존 item 키는 그대로.
- [ ] **삭제·재등록**: busy인 agent를 지우고 같은 ID로 다시 등록하면 새 agent는 `offline`·`last_seen_at` `null`이고, 그 agent의 첫 요청이 throttle 없이 `last_seen_at`을 쓴다.
- [ ] **로그**: note 표식 문자열이 TRACE 로그에 없다(`log_redaction` 확장 — PUT·directory). touch는 INFO 로그를 남기지 않는다.
- [ ] **CLI**: `presence set` body JSON(`--note`·`--ttl` 생략 시 키 없음), 사전 검사(state 값, note 규칙, `--ttl` 0) exit 2, 출력·`--json`, `presence clear` 출력, agent key로 남의 presence exit 1 `forbidden`, `agent list` 두 시점의 열, `profile show`의 세 줄.
- [ ] **콘솔 서빙 테스트**: 페이지가 directory의 `presence`·`last_seen_at`를 쓰고 새 endpoint를 부르지 않는다(정적 검사). note를 text로만 넣는다(`innerHTML` 없음). 기존 보안 단언 유지.
- [ ] **브라우저 검수**: 로컬 platformd를 headless Chrome으로 열어 agent 둘을 online·busy(note)로, 하나를 offline(`last_seen_at` 있음)으로 만든 뒤 디렉토리·방 정보 패널·DM 머리의 점·메모·"n분 전"을 스크린샷으로 남긴다. delivery pill이 따로 그대로다. 콘솔 오류·CSP 위반 0.
- [ ] **스모크**: 로컬 `platformd`에 대해 `deploy/smoke.sh …` 전 시나리오 PASS(새 시나리오 포함). `bash -n` 통과.
- [ ] **배포 빌드**: `deploy/build-aarch64.sh` 성공(ELF aarch64, glibc 2.41 이하).
- [ ] fmt/clippy/test 그린. 코드 미학(`CLAUDE.md` 규약). directory item 키를 정확히 단언하던 기존 테스트(`IF-WIRE-001` 모양)만 두 키를 더해 고치고 핸드오프에 목록을 남긴다. 그 밖의 단언은 바꾸지 않는다.

## 검증 방법 (증거 필수)
- `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` 끝부분(테스트 수 전후). 새 hub 테스트는 구현 전에 실패하는 것을 먼저 보인다.
- 로컬 `platformd`에 대해 `platform presence set …`·`presence clear`·`agent list`와 exit code, `deploy/smoke.sh` 표.
- `last_seen_at` throttle의 증거: 간격 안의 연속 요청 뒤 DB 쓰기 횟수(테스트 계측) 또는 값이 그대로임을 보인 테스트 출력.
- 브라우저 스크린샷 경로와 콘솔 오류 0건.
- `deploy/build-aarch64.sh` 마지막 줄(ELF·glibc).

## 핸드오프
변경 파일, 검증 로그, DoD 항목별 테스트 이름, 고친 기존 테스트 목록, 스크린샷 경로, 비상 해치 보고. 커밋은 문서 하나(이 freeze), 구현 하나 또는 둘(hub+wire+CLI, 콘솔). 푸시하지 않는다. 다른 저장소에 넘길 것: hub-mcp `agents` 도구는 directory를 그대로 전달하므로 `presence`·`last_seen_at`이 자동으로 보인다(heartbeat는 bridge 데몬이 보내는 것으로 그쪽 슬라이스에서), hub-discord D1(상태봇)은 directory의 `presence`·`last_seen_at`을 읽어 매핑한다(조사 §4.4).

## 비상 해치
정본대로 불가능/모순일 때만 정지 → 보고. 예: 어떤 agent 인증 경로가 `Principal` extractor를 지나지 않아 갱신 지점을 하나로 둘 수 없을 때, `touch_last_seen`을 요청과 분리하면 단일 DB thread의 종료 순서(`IF-DB-002`)를 깨야만 할 때, directory 합성이 store transaction 안에서 메모리 맵을 읽어야만 할 때. 우회 구현 금지.
