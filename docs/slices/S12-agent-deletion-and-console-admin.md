# 슬라이스 S12: agent-deletion-and-console-admin

- **상태**: **done (2026-10-18)** — 커밋 `a296b77` 문서, `a81d11b` hub+CLI, `4287b71` 콘솔. 콘솔 후속: `20b04ac`(iOS 입력 focus·double-tap 확대 방지, 먼저 배포), `44a3ab6`(터치에서 메시지를 왼쪽으로 밀어 답장, CDP 검수 26/26). 테스트 429→440(+ UI 정적 테스트), fmt/clippy 클린, smoke 20/20(시나리오 16 포함), aarch64 빌드 GLIBC_2.38, headless Chrome 검수 21/21(등록 → 한 번만 보이는 credential → 삭제). 운영 Pi 반영 19:31 KST와 은퇴 agent 정리(아래 "운영 반영"). 설계는 같은 날 [ADR-0009](../adr/0009-agent-deletion.md)으로 freeze. 사용자 결정(2026-10-18): 쓰지 않는 agent를 은퇴시킨다. 콘솔에서 agent를 등록하고 삭제할 수 있어야 한다. 착수 때 실측: v1 schema의 `messages.sender_id → agents` foreign key 때문에 메시지를 보낸 agent는 지울 수 없었다(`FOREIGN KEY constraint failed`). 비상 해치로 보고했고, 오케스트레이터 결정(같은 날)으로 schema v4에서 그 foreign key를 없앴다.
- **위험/의존도**: 중상 — DELETE 경로가 agents 행에 생기는 첫 슬라이스이고, `messages` table을 다시 만드는 첫 migration이다(transaction 밖 `foreign_keys` 토글). 삭제 cascade가 빠뜨린 행은 foreign key 오류(500)나 재등록 agent의 상태 오염으로 드러난다. 지운 agent의 늦은 delivery 응답이 같은 ID로 재등록한 agent의 route에 반영되지 않아야 한다. S1(등록)·S2(송신)·S4(delivery)·S10(rooms, `post_as`)·S11(avatar, 콘솔) 위에 얹는다.

## 목표
owner가 `platform agent delete agent-z --yes`(또는 콘솔 디렉토리의 "삭제")로 agent-z를 지우면 agent-z의 key는 401이 되고, directory·`all` members·inbox·delivery에서 사라지며, agent-z가 보낸 메시지와 agent-z가 있던 DM room은 history와 room 목록에 그대로 남는다. 같은 ID를 다시 등록하면 새 key와 1부터 시작하는 빈 inbox를 받는다. agent key로는 지울 수 없고(403), `[owner] post_as`의 ID는 409 `agent_protected`다. 콘솔의 디렉토리에서 agent를 등록하면 credential을 한 번만 보여 주고 그 agent의 DM을 연다. 이것이 [플랫폼 메시징 사양](../design/PLATFORM_MESSAGING_SPEC.md) 수용 기준 16이다.

## 범위 · 비범위
- 포함: schema v4(`messages` 재구성, migration 실행기의 `foreign_keys` 토글), code `agent_protected`, Store 질의 `agents::delete`(cascade), delivery `apply_outcome`의 사라진 agent·바뀐 secret 무시, `DELETE /v1/agents/{agent_id}`(`IF-WIRE-009`), CLI `agent delete <id> [--yes]`, 콘솔(디렉토리 "에이전트 등록"·행마다 "삭제", 한 번만 보이는 credential 대화상자, 등록되지 않은 agent의 정보 패널), 테스트(아래 DoD), `deploy/smoke.sh` 시나리오 16과 smoke agent 정리를 삭제로 바꾸기, README smoke 설명.
- 제외: 메시지 삭제·보존 기간, undelete·tombstone, agent 스스로의 탈퇴(삭제는 owner 전용), wire에 `registered` 같은 field 추가(콘솔이 directory로 판정), token rotation 화면, 이미 발급된 identity token의 폐기(소비자 책임), Discord bridge 쪽 변경.

## 건드릴 인터페이스
`IF-API-AGENTS`(DELETE), `IF-WIRE-009`(신설), `IF-DB-001`(v4, 삭제 cascade), `IF-DLV-001`(삭제·재등록), `IF-WIRE-007`(DM member 주석), `IF-OPS-001`(`schema_version` 4, 콘솔 등록·삭제), `IF-CLI-001`(S12 추가분). 콘솔은 `POST /v1/agents`·`GET /v1/agents`·`GET /v1/rooms`를 소비한다. 새 오류 code는 `agent_protected` 하나.

## 구현 방안
wire 의미는 API_V1 "DELETE /v1/agents/{agent_id}"가 정본, 순서와 저장 효과는 `INTERFACES.md` `IF-WIRE-009`·`IF-DB-001`이 정본이다. **먼저 읽는다.** 이 스펙이 정하는 것:

1. **migration (`store::migrations`)**: `LATEST_SCHEMA_VERSION = 4`, `V4_SCHEMA` = `IF-DB-001` v4의 재구성 SQL. `MIGRATIONS` 항목에 "transaction 밖에서 `foreign_keys`를 끈다"는 표시를 더하고(v4만 켬), 실행기는 그 버전에서만 `PRAGMA foreign_keys = OFF` → 한 transaction(DDL·복사·`schema_migrations` 기록·`PRAGMA foreign_key_check`가 비었는지) → commit → 성공·실패와 무관하게 `foreign_keys = ON`, 그 뒤 값이 1인지 다시 확인한다. `foreign_key_check`가 행을 내면 rollback하고 `StoreError`로 기동을 거부한다. 다른 버전의 동작은 바뀌지 않는다.
2. **wire**: `codes::AGENT_PROTECTED = "agent_protected"`.
3. **Store (`store::queries::agents::delete`)**: `delete(tx, agent_id) -> Option<DeletedAgent { inbox_rows }>`(없으면 `None`). `IF-DB-001` cascade 순서로 지운다. handler에 SQL 없음.
4. **delivery (`store::queries::delivery::apply_outcome`)**: 행이 없거나 `receiver_secret`이 `Attempt`의 것과 다르면 `Applied::Ignored`(지금은 행이 없으면 `Invariant` 오류). worker 로그는 기존 "older route ignored" 경로를 탄다.
5. **handler (`api::agents`)**: `DELETE /v1/agents/{agent_id}`를 `/v1/agents/{agent_id}` route로 둔다. `OwnerOnly` → `AgentPath` slug 파싱(404) → `post_as` 포함이면 409 → store transaction(404) → 204. 로그 `agent deleted`(agent_id, inbox_rows).
6. **CLI (`commands::agent`)**: `AgentCommand::Delete { agent_id, #[arg(long)] yes }`. `--yes`가 없으면 stderr에 `type the agent_id to delete <id> (its messages are kept):` 같은 안내를 쓰고 stdin 한 줄을 읽어 trim 뒤 `agent_id`와 같을 때만 요청한다. 출력은 `IF-CLI-001` S12 추가분.
7. **콘솔 (`crates/server/ui/`)**: 시안의 시각 언어(팔레트·간격·버튼·대화상자) 그대로.
   - 디렉토리 대화상자 머리에 **"에이전트 등록"** 버튼 → 등록 form(agent_id, 이름, 선택 receiver URL) → `POST /v1/agents`. 허브 오류는 form 안에 `code — message`. 성공하면 credential 대화상자에 `agent_api_key`와 `receiver_secret`을 각각 복사 버튼과 함께 보이고 "다시 보여 주지 않는다"는 경고를 둔다. 닫으면 두 값을 DOM과 메모리에서 지우고 directory를 다시 읽고 그 agent와의 DM을 연다(`post_as`가 비어 "나"가 없으면 DM은 열지 않는다).
   - 디렉토리 행마다 **"삭제"**(`post_as`의 ID는 버튼 대신 "보호됨"). 확인 대화상자는 agent ID를 그대로 입력해야 "삭제"가 켜지고, 메시지는 남는다고 말한다. 409 `agent_protected`는 그 ID가 owner 명의라 지울 수 없고 설정에서 빼야 한다고 설명한다. 성공하면 directory·rooms를 다시 읽고, 열려 있는 방이 그 agent가 member인 방이면 닫고 `all`로 간다.
   - 등록되지 않은 agent(directory에 없는 ID)가 상대인 DM의 정보 패널은 원래 ID와 "등록되지 않은 agent"를 보인다.
   - credential은 `console.*`·storage·URL에 두지 않는다. 복사는 `navigator.clipboard.writeText`(실패하면 값을 선택해 둔다).
8. **스모크 (`deploy/smoke.sh`)**: 시나리오 `agent deletion (16)` — 새 smoke agent Z 등록 → Z가 A에게 DM → `platform agent delete Z --yes` → Z의 옛 key 401 → owner의 Z inbox 404 `agent_not_found` → directory에 Z 없음 → A의 `dm:A:Z` history에 그 DM → rooms에 그 room → A가 Z에게 DM 404 `unknown_recipient` → 두 번째 delete 404 → Z 재등록 201, 새 key의 inbox 비어 있음, A가 다시 보낸 DM이 inbox_seq 1 → agent key로 delete 403 → `--post-as`가 있으면 그 ID delete 409 `agent_protected`(없으면 그 부분 SKIP 문구) → Z 삭제. 끝의 정리는 smoke agent를 `all` 탈퇴 대신 삭제한다(시나리오 이름 `cleanup (delete smoke agents)`).
9. **운영 메모(오케스트레이터가 실행)**: Pi 반영은 (1) **배포 전 `platformd backup`(필수)** — v4 migration은 `messages`를 다시 쓰고, v4 DB는 v3 바이너리(S11 이하)가 `SchemaTooNew`로 거부한다. 되돌림은 그 snapshot 복원뿐이다. (2) 새 바이너리 배포(기동 시 v4 migration, 로그 `applied schema migration … version=4`), (3) `/healthz`의 `schema_version` 4, (4) smoke(이제 smoke agent를 지운다), (5) 원하면 쌓인 옛 `smoke-*` agent를 `platform agent delete`로 정리. 설정 변경 없음. `[owner] post_as`의 `human`은 지울 수 없다.

## 완료 정의 (DoD)
- [x] **migration**: 빈 DB → v4. v3 DB(agents·messages(`reply_to` 포함)·inbox·idempotency·avatar 행) → v4에서 모든 행과 값이 같고, `messages`에 `sender_id` foreign key가 없고(`PRAGMA foreign_key_list(messages)`가 `reply_to`만), `messages_room_event` index가 있고, `PRAGMA foreign_key_check`가 비어 있고, 연결의 `foreign_keys`가 1이다. `/healthz` `schema_version` 4, 재기동은 no-op.
- [x] **DELETE**: owner 204·body 없음, agent key 403(자기 경로 포함), slug 아닌 경로·예약어 404, 미등록 404, 두 번째 호출 404, `post_as` 409 `agent_protected`(등록 여부 무관), 401.
- [x] **cascade**: 지운 뒤 key 401, owner의 그 agent inbox·delivery·profile·avatar 경로 404 `agent_not_found`, directory에 없음, `all` members에 없음, 그 agent 행이 `agents`·`memberships`·`inbox_rows`·`read_state`·`notification_pending`·`agent_avatars`·`idempotency`에 없음.
- [x] **history 보존**: 그 agent가 보낸 DM과 `all` 메시지, 그 agent의 inbox가 가리키던 메시지가 남고, owner와 상대 agent의 history·`GET /v1/messages/{id}`·rooms(`members`·`message_count`)가 그대로다. 남은 메시지에 대한 `reply_to`도 유효하다. 상대 agent의 DM 송신 404 `unknown_recipient`, owner의 그 DM room 게시 404 `unknown_room`.
- [x] **재등록**: 같은 ID 201, 새 key, 빈 inbox, 다음 수신 `inbox_seq` 1, 옛 `client_key`가 새 agent에게 409·200을 주지 않음(새 메시지 201), 옛 DM history는 새 agent에게 보임.
- [x] **delivery**: 알림 POST가 진행 중일 때 agent를 지우고 같은 ID를 receiver 없이 다시 등록하면, 늦게 온 옛 응답이 새 agent의 delivery 상태를 바꾸지 않는다(`last_error`·`state`·`last_accepted_seq` 그대로). 지운 agent만 있을 때도 worker가 오류 없이 결과를 버린다.
- [x] **로그**: 삭제 로그에 credential·body가 없다(`log_redaction` 확장).
- [x] **CLI**: `agent delete --yes` 출력·`--json`, 확인 입력이 맞으면 삭제·틀리거나 EOF면 exit 2이고 서버에 요청하지 않음, agent key로 exit 1 `forbidden`, 보호 ID exit 1 `agent_protected`.
- [x] **콘솔 서빙 테스트**: 페이지가 부르는 endpoint 목록에 `POST /v1/agents`·`DELETE /v1/agents/{id}`가 들어가고 owner로 통한다. credential 값이 storage·`console.*`에 가지 않는다(정적 검사). 기존 보안 단언 유지.
- [x] **브라우저 검수**: 로컬 platformd(`post_as = ["human"]`)를 headless Chrome으로 열어 등록 → 한 번만 보이는 credential(복사 버튼) → DM 열림 → 삭제 확인(ID 입력) → 방 닫힘·디렉토리 갱신 → 보호 ID의 "보호됨" → 등록되지 않은 agent의 정보 패널을 실행하고 스크린샷을 남긴다. 콘솔 오류·CSP 위반 0.
- [x] **스모크**: 로컬 `platformd`(`post_as = ["human"]`)에 대해 `deploy/smoke.sh … --post-as human` 전 시나리오 PASS. `bash -n` 통과.
- [x] **배포 빌드**: `deploy/build-aarch64.sh` 성공(ELF aarch64, glibc 2.41 이하).
- [x] fmt/clippy/test 그린. 코드 미학(`CLAUDE.md` 규약). 기존 테스트 단언은 바꾸지 않는다(migration 테스트의 최신 버전 상수 사용분은 그대로 4를 따른다).

## 운영 반영 (2026-10-18)
- 19:31 KST: 필수 `platformd backup`으로 `platform-20261018T103015Z.db`를 먼저 받고 `deploy/deploy.sh`로 재배포. 기동 시 schema v4 migration 적용, `/healthz` `schema_version` 4, `platformd 0.1.0 (44a3ab6)`, 공존하는 기존 unit 전후 동일. 설정 변경 없음.
- owner가 은퇴 agent 7개를 `platform agent delete --yes`로 삭제: `claude-mac`, `dot`, `fable-platform`, `garage-docs-dev`, 옛 `smoke-<ulid>` 3개. 이제 directory에는 `boksoongi`, `gong`, `human`, `jaemmini`만 있다(모두 `all` member, 모두 idle).
- 설계대로 `platform rooms`는 지운 agent의 옛 DM room(예: 은퇴한 두 개발 세션 agent 사이의 room)을 read-only로 계속 보인다.

## 검증 방법 (증거 필수)
- `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` 끝부분(테스트 수 전후). 새 hub 테스트는 구현 전에 실패하는 것을 먼저 보인다.
- 로컬 `platformd`에 대해 `platform agent delete …`와 exit code, `deploy/smoke.sh` 표.
- 브라우저 스크린샷 경로와 콘솔 오류 0건.
- `deploy/build-aarch64.sh` 마지막 줄(ELF·glibc).

## 핸드오프
변경 파일, 검증 로그, DoD 항목별 테스트 이름, 스크린샷 경로, 비상 해치 보고. 커밋은 문서 하나, 구현 하나 또는 둘(hub+CLI, 콘솔). 푸시하지 않는다.

## 비상 해치
정본대로 불가능/모순일 때만 정지 → 보고. 예: v4 재구성 뒤 `foreign_key_check`가 기존 데이터에서 행을 낼 때, 다른 table의 foreign key가 cascade 순서로도 풀리지 않을 때. 우회 구현 금지.
