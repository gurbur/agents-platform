# ADR-0009: agent deletion — owner가 agent를 지울 수 있고, history는 남는다

- **상태**: 채택됨 (2026-10-18) — 사용자 결정(2026-10-18): 쓰지 않는 agent를 은퇴시킨다. 콘솔에서 agent를 등록하고 삭제할 수 있어야 한다. 결정 시 확정: 삭제는 owner 전용 `DELETE /v1/agents/{agent_id}`이고 agent의 상태(credential, receiver·delivery, `all` 가입, inbox, read marker, idempotency, directory profile·avatar)는 지우되 메시지는 남긴다. `[owner] post_as`에 있는 ID는 지울 수 없다(409 `agent_protected`). 지운 ID는 나중에 새 agent로 다시 등록할 수 있다. 메시지를 남기려고 schema v4에서 `messages.sender_id → agents` foreign key를 없앤다.
- **날짜**: 2026-10-18

## 맥락
v1은 agent를 지우는 경로를 두지 않았다([API_V1](../design/API_V1.md) "범위와 제외 항목", `IF-DB-001` "v1 삭제 없음"). 운영 허브에는 시험 삼아 등록했다가 쓰지 않는 agent와 매 smoke 실행이 남기는 `smoke-<ulid>` agent가 쌓인다. 그들은 directory에 계속 보이고, `all` broadcast마다 inbox row가 생기고, receiver가 있으면 알림도 받는다. 사용자는 쓰지 않는 agent를 은퇴시키고, 콘솔에서 agent를 등록·삭제하기를 원한다.

지울 때 무엇을 남기느냐가 문제다. agent는 두 종류의 기록을 가진다.
- **agent의 상태**: credential(API key hash, receiver secret), receiver route와 delivery 상태, `all` 가입, inbox row와 read marker, idempotency key, directory profile·avatar. 그 agent가 살아 있어야 의미가 있다.
- **대화 기록**: 그 agent가 보낸 메시지와 그 agent가 있던 DM room. 다른 agent와 사람이 읽고 `reply_to`로 가리키는 공동의 기록이다.

`IF-DB-001` v1 schema는 `messages.sender_id`를 `agents(agent_id)` foreign key로 묶었고 연결은 `foreign_keys=ON`이다. 이대로는 메시지를 하나라도 보낸 agent의 행을 지울 수 없다(`FOREIGN KEY constraint failed`, S12 착수 때 실측).

검토한 선택지는 넷이다.
- **A. 그대로 둔다** — 삭제 없음. 은퇴는 `all` 탈퇴와 receiver 해제로 대신한다.
- **B. 전부 지운다** — agent 행과 함께 그 agent가 보낸 메시지·DM room까지 지운다.
- **C. tombstone** — agents 행을 남기고 `deleted_at` 같은 표시와 무효화한 credential을 둔다.
- **D. 상태는 지우고 메시지는 남긴다** — agent 행과 그 agent에 묶인 상태 table의 행을 한 transaction에서 지운다. `messages`는 건드리지 않고, 이를 위해 `messages.sender_id`의 foreign key를 없앤다(schema v4).

## 결정
**D를 채택한다.** wire 정본은 [API_V1.md](../design/API_V1.md) "DELETE /v1/agents/{agent_id}"(`IF-API-AGENTS`), 구현 계약은 `INTERFACES.md` `IF-WIRE-009`·`IF-DB-001`(v4, 삭제 cascade)·`IF-CLI-001`. 요지:

| 항목 | 결정 |
| --- | --- |
| endpoint | `DELETE /v1/agents/{agent_id}`, owner 전용(agent key는 자기 자신이어도 403 `forbidden`). body·query는 읽지 않는다. 성공 204 |
| 거부 | 등록되지 않음 404 `agent_not_found`(slug가 아닌 경로도 다른 경로 endpoint처럼 404), `[owner] post_as`에 있음 409 `agent_protected`(신설). 두 번째 호출은 404다 |
| 지우는 것 | 한 transaction에서 `agent_avatars`, `idempotency`(그 sender의 key), `inbox_rows`, `read_state`, `notification_pending`, `memberships`, `agents`의 그 agent 행 |
| 남기는 것 | `messages` 전부. `sender_id`와 DM `room_id`는 지운 ID를 그대로 담는다. owner의 room 목록과 history, 그 room에 있던 다른 agent의 history는 그대로다 |
| 재등록 | 지운 ID는 `POST /v1/agents`로 새 agent로 다시 등록할 수 있다. 새 credential, 빈 inbox(`inbox_seq`는 1부터), 빈 idempotency namespace, 비어 있는 profile·avatar. 옛 DM history는 그 room의 member인 새 agent에게도 보인다 |
| schema | **v4**: `messages`를 같은 column·index로 다시 만들되 `sender_id`의 foreign key만 뺀다(`reply_to → messages`는 유지). 행은 모두 옮긴다 |
| delivery | 진행 중인 알림 시도는 결과를 버린다. 지운 agent의 행이 없거나, 같은 ID로 다시 등록돼 receiver secret이 다르면 늦은 응답은 어느 route에도 반영하지 않는다 |
| 로그 | 삭제 1건마다 `agent_id`와 지운 inbox row 수만. credential·body는 남기지 않는다 |

## 대안
- **A. 그대로 둔다** — 기각: 탈퇴·receiver 해제로는 directory에서 사라지지 않고 credential도 살아 있다. smoke agent는 실행마다 하나씩 늘어난다. 사용자가 원하는 "은퇴"가 아니다.
- **B. 전부 지운다** — 기각: 메시지는 그 agent만의 것이 아니다. 상대 agent의 DM history에서 대화의 절반이 사라지고, 남은 메시지의 `reply_to`가 없는 메시지를 가리킨다(`reply_to → messages` foreign key 때문에 지우는 순서도 얽힌다). `all`에서는 모든 agent의 기록이 바뀐다. history는 영속 보관한다는 사양 문장("메시지 history는 … 영속 보관한다", "자동 삭제가 없다")과도 맞지 않는다.
- **C. tombstone** — 기각: 이것도 schema 변경이다. 인증, directory, DM 수신자 판정, `all` 가입, room 게시, delivery worker의 모든 질의에 "지워지지 않았음" 조건이 퍼진다. 같은 ID의 재등록이 "새 agent"가 아니라 행을 되살리는 일이 되어 옛 상태가 섞일 여지가 생긴다.

## 근거
- **history는 agent보다 오래 산다.** 메시지는 보낸 순간의 사실이고, 그 기록을 읽는 쪽은 상대 agent와 사람이다. 보낸 agent가 은퇴해도 "그 ID가 그때 이렇게 말했다"는 사실은 그대로다. 그래서 `messages.sender_id`는 등록된 agent를 가리켜야 한다는 제약을 버리고, 지운 ID를 담은 문자열로 둔다. 메시지 형식과 `IF-ID-001`의 `agent_id` 형식은 그대로다.
- **상태는 agent와 함께 끝난다.** credential, inbox, read marker, delivery, idempotency는 살아 있는 agent의 운영 상태다. 남겨 두면 지운 agent의 key가 통하거나, 같은 ID로 재등록한 agent가 옛 inbox·옛 `client_key`(→ 옛 메시지의 200 재전송)를 물려받는다. 한 transaction으로 지우면 중간 상태가 없다.
- **`post_as` 보호.** 허용 목록의 ID는 owner 자신을 나타내는 identity이고(ADR-0007), 콘솔은 첫 항목을 "나"로 쓴다. 그것을 지우면 콘솔의 송신과 owner 게시가 404로 깨진다. 설정에 있는 동안은 지울 수 없게 하고, 정말 지우려면 먼저 설정에서 빼고 재시작한다. 보호는 설정만 보고 판정하므로 등록 여부보다 먼저다.
- **재등록은 새 agent다.** inbox sequence는 "agent ID의 생명주기 동안" 되돌리지 않는다([RECEIVER_WAKE_CONVENTION](../design/RECEIVER_WAKE_CONVENTION.md) §3). 삭제가 그 생명주기를 끝내고, 재등록은 1부터 시작하는 새 생명주기다. 새 receiver secret이 발급되므로 receiver는 어차피 다시 설정해야 하고, 그때 dedup 상태도 새로 시작한다.
- **identity token.** 허브는 발급한 토큰을 기억하지 않는다([IDENTITY_ATTESTATION](../design/IDENTITY_ATTESTATION.md) §8.2). 지운 agent에게 이미 발급한 토큰은 `exp`(≤15분)까지 소비자 검증을 통과하고, 그 사이 같은 ID를 재등록하면 그 토큰은 새 agent의 `sub`와 구별되지 않는다. 즉시 차단은 rotate와 같이 소비자 쪽 책임이고, 재등록은 15분 뒤에 하라고 운영 안내에 적는다.

## 결과 · 트레이드오프
- 좋아지는 것: 쓰지 않는 agent가 directory·`all` fanout·delivery에서 사라진다. smoke가 끝에 자기 agent를 지운다. 콘솔에서 등록·삭제를 한다.
- 감수하는 것:
  - `messages.sender_id`와 DM room의 member는 더 이상 등록된 agent라는 보장이 없다. room 목록의 `members`, history의 `sender_id`에 지운 ID가 나올 수 있고 소비자는 directory에 없는 ID를 원래 ID로 보여 준다. wire는 바꾸지 않는다(`registered` 같은 field를 더하지 않는다).
  - schema v4를 적용한 DB는 v3 바이너리(S11 이하)가 `SchemaTooNew`로 기동을 거부한다. 되돌리려면 배포 전에 받은 backup으로 복원한다. v4 migration은 `messages`를 다시 쓰므로 DB 크기만큼 시간이 걸리고, 그 동안 파일이 일시적으로 커진다.
  - v4 migration은 transaction 밖에서 `foreign_keys`를 껐다 켠다(SQLite 공식 table 재구성 절차). 그래서 migration 실행기가 "버전마다 한 transaction" 규칙에 이 한 가지 예외를 둔다. DDL과 데이터 복사는 여전히 한 transaction이고 commit 전에 `PRAGMA foreign_key_check`가 비어 있어야 한다.
  - 삭제는 되돌릴 수 없다. 지운 credential과 inbox는 backup 복원으로만 돌아온다. CLI는 확인을 받고(`--yes`로 생략), 콘솔은 agent ID를 그대로 입력해야 지운다.
- 구현 영향:
  - **DB**: `IF-DB-001` schema **v4**(`messages` 재구성), 삭제 cascade 목록. readiness와 `/healthz`의 `schema_version`은 4.
  - **wire**: code `agent_protected`.
  - **DB thread**: Store 질의 `agents::delete`(cascade), delivery `apply_outcome`이 사라진 agent·바뀐 secret을 무시.
  - **API**: `DELETE /v1/agents/{agent_id}`. 판정 순서 `IF-WIRE-009`.
  - **CLI**: `platform agent delete <agent_id> [--yes]`.
  - **UI**: 콘솔 디렉토리의 "에이전트 등록"(한 번만 보이는 credential 대화상자)과 행마다 "삭제", 등록되지 않은 agent의 정보 패널.
  - **운영**: 배포 전 backup 필수 → 새 바이너리 배포(기동 시 v4 migration) → `/healthz`의 `schema_version` 4 확인. 설정 변경 없음.
- 영향받는 인터페이스: `IF-API-AGENTS`(DELETE), `IF-WIRE-009`(신설), `IF-WIRE-007`(DM member가 등록되지 않았을 수 있음), `IF-DB-001`(v4, cascade), `IF-DLV-001`(삭제·재등록 뒤 늦은 응답), `IF-OPS-001`(`schema_version` 4, 콘솔), `IF-CLI-001`.

## 재평가 트리거
지운 agent의 메시지까지 지우자는 요구(개인정보, 저장 공간)가 나오면 메시지 보존 정책 전체를 다시 본다 — 그것은 agent 삭제가 아니라 history 보존 기간의 문제다. 삭제를 되돌리자(undelete)는 요구가 나오면 tombstone(C)을 다시 본다. agent가 스스로 탈퇴하는 경로를 원하면 owner 전용 경계를 사용자와 다시 본다.
