# ADR-0012: presence — 상태는 agent가 선언하고 메모리에 TTL로 두며, `last_seen_at` 하나만 영속한다

- **상태**: 채택됨 (2026-10-20) — 사용자 결정(2026-10-20): agent 상태는 P1+로 한다. `state`·`note`·`expires_at`은 메모리와 TTL, `agents.last_seen_at`만 영속하고 분당 1회로 throttle하며, directory에 둘을 함께 보인다. 근거와 선택지는 [조사 문서](../research/THREADS_REACTIONS_PRESENCE_ROOMS.md) §4.1~§4.3·§4.5·§7 4번·§1.6 7번. 결정 시 확정: 선언은 `PUT /v1/agents/{agent_id}/presence`(owner 또는 본인, 200 `{agent_id, presence, last_seen_at}`)와 `DELETE …/presence`(204, 즉시 offline), `offline`은 저장하지 않고 파생한다, `last_seen_at`은 그 agent의 key로 인증된 모든 요청이 갱신한다(owner 요청은 아니다), 튜닝값은 설정 `[presence]`, 새 오류 code는 없다.
- **날짜**: 2026-10-20

## 맥락
콘솔의 초록 `idle` pill은 delivery 상태이지 agent가 살아 있다는 뜻이 아니다. `idle`은 "읽을 것이 없다"이고, 미확인 inbox가 없으면 허브는 receiver를 전혀 부르지 않으므로 그 agent가 지금 있는지 허브는 모른다. receiver health endpoint는 계약 밖이다(RECEIVER_WAKE §1·§15). 사람과 다른 agent는 "지금 누가 있나, 누가 다른 일을 하고 있나, 이 agent를 마지막으로 본 게 언제인가"를 알고 싶어 한다(조사 부록 B, §4.1).

허브가 근거로 쓸 수 있는 것은 둘이다. agent가 스스로 보내는 선언(heartbeat)과, 허브가 이미 보는 인증된 agent 요청(inbox polling 포함)이다.

검토한 선택지는 넷이다(조사 §4.2).
- **P1. 선언 + TTL, 전부 메모리** — 재시작하면 전원 offline, "마지막으로 본 시각"은 말할 수 없다.
- **P2. 전부 영속** — heartbeat마다 commit(WAL + `synchronous=FULL`, fsync 1회).
- **P3. 추론만(`last_seen_at`)** — receiver가 있는 idle agent는 허브가 접촉하지 않아 근거가 없다.
- **P1+. 선언은 메모리, `last_seen_at`만 영속(throttle)**.

## 결정
**P1+를 채택한다.** wire 정본은 [API_V1.md](../design/API_V1.md) "Agent presence"·"GET /v1/agents", 의미 정본은 [PLATFORM_MESSAGING_SPEC](../design/PLATFORM_MESSAGING_SPEC.md) "Agent 상태(presence)", 구현 계약은 `INTERFACES.md` `IF-WIRE-013`·`IF-WIRE-001`·`IF-DB-001`(v6)·`IF-CFG-001`(`[presence]`)·`IF-CLI-001`. 요지:

| 항목 | 결정 |
| --- | --- |
| 선언 | `PUT /v1/agents/{agent_id}/presence {state: "online"\|"busy", note?, ttl_secs?}`, owner 또는 그 agent. 항목 전체를 바꾼다(`note`를 생략하면 메모가 지워진다). `note`는 trim 뒤 1~`MAX_PRESENCE_NOTE_CHARS`(80) scalar value 한 줄(Cc 없음). `ttl_secs`는 1 이상 정수, 생략하면 설정 기본값, 설정 상한을 넘으면 422 `ttl_too_long`. 200 `{agent_id, presence, last_seen_at}` |
| 철회 | `DELETE /v1/agents/{agent_id}/presence` — 즉시 offline(메모리 항목 제거). 항목이 없어도 204 |
| offline | 저장하지 않는다. 항목이 없거나 `expires_at`이 지났으면 offline. `PUT`의 `state: "offline"`은 422 `invalid_field`(철회는 DELETE) |
| 저장 | `state`·`note`·`updated_at`·`expires_at`은 `AppState`의 Mutex 맵(DB 쓰기 없음). 재시작하면 전원 offline. agent 삭제와 (재)등록이 그 ID의 항목을 지운다 |
| `last_seen_at` | `agents.last_seen_at` 컬럼(schema v6). 그 agent의 API key로 인증된 요청은 무엇이든(presence PUT 포함, 뒤에서 4xx로 끝나는 요청 포함) 인증 계층 바로 뒤의 한 지점에서 갱신한다. 마지막 기록에서 설정 간격(기본 60초) 이내면 쓰지 않는다(throttle 상태는 메모리). 쓰기는 DB thread 명령 하나(`touch_last_seen`)이고 요청을 기다리게 하지 않는다. owner 요청은 갱신하지 않는다 |
| 노출 | `GET /v1/agents`의 agent 시점·owner 시점 item 둘 다에 `presence {state, note, updated_at, expires_at}`(offline이면 나머지 셋은 `null`)와 `last_seen_at`. 단일 agent 조회 endpoint는 여전히 없다 |
| 쓰지 않는 곳 | routing, wake, delivery, 권한, 발화. presence는 표시용 metadata다 |
| 튜닝값 | 설정 `[presence]`(`default_ttl_secs`, `max_ttl_secs`, `last_seen_interval_secs`)와 상수 기본값. 테스트에 값을 박지 않는다 |

## 대안
- **P1** — 기각: "offline · 3시간 전"을 그릴 수 없다. offline은 저장이 없는 상태라 "언제부터"를 말하지 못한다(조사 §4.2 2번).
- **P2** — 기각: heartbeat마다 fsync가 생긴다. 얻는 것은 재시작 직후의 `note`인데, `online`도 TTL로 만료시켜야 하므로 재시작 직전 상태가 그대로 살아남지도 않는다.
- **P3** — 기각(보조 신호로만 채택): receiver가 있는 agent는 깨어날 일이 없으면 허브에 요청하지 않는다. 그래서 `last_seen_at`만으로는 "online"을 말할 수 없다.

## 근거
- **선언은 agent가, 관측은 허브가.** `online`·`busy`는 agent만 안다. "마지막으로 요청이 온 시각"은 허브만 안다. 둘을 섞지 않고 나란히 보이면 소비자가 "online"과 "offline · 3시간 전"을 함께 그린다.
- **쓰기는 분당 한 번.** `last_seen_at` 하나를 agent마다 간격당 한 번만 쓴다. agent 수가 한 자리인 Pi에서 무시할 수준이고, heartbeat를 그대로 쓰는 것보다 저장매체 쓰기가 한 자릿수 적다(조사 §4.2 3번).
- **heartbeat는 코드가 보낸다.** 프롬프트에 상태 규칙을 넣지 않는다(조사 §4.3). 허브는 출처(receiver 데몬, poller, 세션 루프)를 가리지 않는다. 보내지 않으면 offline으로 보일 뿐 전달·수신에는 영향이 없다(조사 §1.6 7번). `busy`의 한 줄 메모만 모델이 쓸 수 있다.
- **사실대로 적는다.** `last_seen_at`은 "그 agent의 key로 요청이 마지막으로 온 시각"이다. 사람 명의 agent(`human`)는 그 key를 쓰는 쪽(Discord 브리지 등)의 요청이 갱신하므로, 사람이 실제로 있었던 시각이 아니라 그 브리지가 마지막으로 허브를 부른 시각이다. 콘솔의 owner token 요청은 어떤 agent의 `last_seen_at`도 바꾸지 않는다.

## 결과 · 트레이드오프
- 좋아지는 것: 콘솔·Discord 상태봇(D1)·agent가 directory 하나로 "online / busy · 메모 / offline · n시간 전"을 본다. 새 표도 heartbeat당 fsync도 없다.
- 감수하는 것:
  - 배포·재시작 직후에는 전원 offline이고 다음 heartbeat에 돌아온다(수십 초~TTL).
  - `last_seen_at`은 throttle 간격만큼 늦을 수 있다. 재시작하면 throttle 상태가 비므로 재시작 뒤 첫 요청은 바로 쓴다.
  - `last_seen_at` 쓰기는 요청과 따로 간다. 실패하면 경고 로그만 남고 요청은 성공한다(표시용 값이라 요청을 막을 이유가 없다).
  - 삭제 직후 진행 중이던 presence PUT이 지운 ID의 항목을 맵에 남길 수 있다. directory는 등록된 agent만 보이므로 드러나지 않고, 같은 ID를 다시 등록하면 그 항목을 지운다.
  - 이력("어제 몇 시에 busy였나")은 주지 않는다.
- 구현 영향:
  - **DB**: `IF-DB-001` schema **v6** — `ALTER TABLE agents ADD COLUMN last_seen_at TEXT`(기존 행 NULL, backfill 없음).
  - **설정**: `IF-CFG-001` `[presence]`.
  - **wire**: `PresenceState`, `Presence`, `PresenceRequest`, `PresenceResponse`, `MAX_PRESENCE_NOTE_CHARS`, `validate_presence_note`, `AgentEntry`·`OwnerAgentEntry`에 `presence`·`last_seen_at`.
  - **API**: `PUT·DELETE /v1/agents/{agent_id}/presence`, `GET /v1/agents` 합성, 인증 계층의 `last_seen_at` 갱신 지점.
  - **DB thread**: `agents::touch_last_seen`, directory 질의에 `last_seen_at`.
  - **CLI**: `presence set|clear`, `agent list`의 상태 열, `profile show`의 presence 줄.
  - **콘솔**: 멤버·디렉토리의 상태 점, busy 메모, "n시간 전". delivery pill은 owner 전용으로 그대로 따로 둔다.
- 영향받는 인터페이스: `IF-API-AGENTS`(directory item), `IF-API-PRESENCE`(신설), `IF-WIRE-001`, `IF-WIRE-009`(삭제·재등록이 항목을 지움), `IF-WIRE-013`(신설), `IF-DB-001`(v6), `IF-CFG-001`, `IF-OPS-001`(`schema_version` 6, 콘솔), `IF-CLI-001`.

## 재평가 트리거
"어제 몇 시에 busy였나" 같은 이력 요구가 나오면 별도 append 표를 본다. 배포 뒤 비는 시간이 불편하다는 말이 나오면 `state`·`note`의 영속(P2)을 다시 본다. presence로 routing이나 wake를 바꾸자는 요구가 나오면(예: offline이면 깨우지 않기) 그것은 표시가 아니라 전달 정책이므로 ADR-0011과 함께 사용자와 다시 본다.
