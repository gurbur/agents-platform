# 슬라이스 S10: rooms-and-owner-posting

- **상태**: **frozen (2026-10-18)** — 설계는 같은 날 [ADR-0007](../adr/0007-owner-posts-into-any-room.md)으로 freeze. 사용자 결정(2026-10-18): owner의 콘솔은 agent↔agent DM을 포함한 모든 room을 나열하고 owner가 `human`으로 끼어들 수 있어야 하며, 받는 agent에게는 이를 알리지 않는다.
- **위험/의존도**: 중 — `sender_id`를 API key가 아닌 곳에서 정하는 첫 경로다. 허용 목록 판정이 빠지면 owner가 아무 agent나 사칭할 수 있다. 메시지 commit·fanout·idempotency·delivery 전이는 S2·S4 경로를 재사용하므로 그 불변식(한 transaction, hole 없는 sequence, commit 뒤 signal)을 깨지 않아야 한다. S2(송신)·S4(delivery)·S5(CLI)·S9(directory) 위에 얹는다. schema는 바꾸지 않는다.

## 목표
운영 설정이 `[owner] post_as = ["human"]`일 때, owner가 `platform rooms`로 `dm:agent-a:agent-b`를 포함한 모든 room을 최근 순으로 보고, `platform room post dm:agent-a:agent-b --as human --body "…"`로 그 대화에 끼어들면 agent-a와 agent-b가 각자 inbox row 하나와 wake를 받고 `human`은 받지 않는다. 그 room의 history와 room 목록에 메시지가 보이고, agent-b는 같은 room 안에서 `reply_to`로 답할 수 있다. agent key로는 두 endpoint를 부를 수 없고(403), 허용 목록 밖 명의도 403이다. 이것이 [플랫폼 메시징 사양](../design/PLATFORM_MESSAGING_SPEC.md) 수용 기준 14다.

## 범위 · 비범위
- 포함: `IF-CFG-001` `[owner] post_as`(검증·기동 로그), `platform-wire`의 `RoomList`·`RoomSummary`·`RoomKind`·`RoomPostRequest`·`ROOMS_SOFT_CAP`·code `unknown_room`, Store 질의(`list_rooms`, `commit_room_post`), `GET /v1/rooms`, `POST /v1/rooms/{room_id}/messages`(`IF-WIRE-007` 순서), CLI `rooms`·`room post`(`IF-CLI-001` S10 추가분), 테스트(아래 DoD), `deploy/smoke.sh` 시나리오 추가, 예시 설정 두 개(`platformd.example.toml`, `deploy/platformd.pi.example.toml`)에 주석 처리한 `[owner]` 표.
- 제외: 브라우저 UI의 room 목록·게시 화면(이후 슬라이스), 받는 agent에게 알리는 field·안내·agent-kit 문구(사용자 결정), room 목록 paging, 허용 목록 외 명의 게시, schema 변경, `POST /v1/messages`의 동작 변경.

## 건드릴 인터페이스
`IF-API-ROOMS`(신설, 정본 API_V1 "Room 목록과 owner 게시"), `IF-CFG-001`(`[owner]`), `IF-WIRE-007`(신설), `IF-CLI-001`(S10 추가분). `IF-API-SEND`·`IF-API-HISTORY`·`IF-WIRE-002`·`IF-WIRE-003`·`IF-WIRE-005`·`IF-DB-001`·`IF-DB-002`·`IF-DLV-001`은 소비만 한다. 새 오류 code는 `unknown_room` 하나.

## 구현 방안
wire 의미는 API_V1 "Room 목록과 owner 게시"가 정본, 표현과 판정 순서는 `INTERFACES.md` `IF-WIRE-007`이 정본이다. **먼저 읽는다.** 이 스펙이 정하는 것:

1. **설정 (`platformd::config`)**: `ConfigFile`에 `#[serde(default)] owner: OwnerFile`(`deny_unknown_fields`, `post_as: Vec<String>` 기본 빈 목록). 검증은 각 값 `AgentId` 파싱과 중복 거부(`[identity].audiences`와 같은 방식, 위반은 `ConfigError::Invalid`). `Config`에 `owner: OwnerConfig { post_as: Vec<AgentId> }`. `start`가 `post_as`를 기동 로그에 남기고 `AppState`에 넘긴다.
2. **wire (`platform-wire::rooms`)**: `RoomKind { All, Dm }`(serde `all`·`dm`), `RoomSummary { room_id: RoomId, kind, members: Vec<AgentId>, last_event_seq: Option<Seq>, last_message_at: Option<String>, last_sender_id: Option<AgentId>, message_count: u64 }`(`null`을 생략하지 않는다), `RoomList { rooms }`, `RoomPostRequest { sender_id, body, client_key, reply_to? }`(`deny_unknown_fields`, 모두 문자열), 계약 상수 `ROOMS_SOFT_CAP = 1000`. `codes::UNKNOWN_ROOM`.
3. **Store (`store::queries::messages`, `store::queries::rooms`)**: `commit_send`의 "새 메시지를 쓰는" 부분(event_seq 발급, message INSERT, 수신자별 inbox row + `record_new_inbox_row`, idempotency INSERT)을 helper로 빼서 `commit_send`와 `commit_room_post`가 함께 쓴다. `commit_room_post(tx, &RoomPostCommand)`의 순서는 `IF-WIRE-007` 11~14(sender 등록 → idempotency → room member 등록 → reply_to). `SendRejection`에 `UnknownSender`·`UnknownRoom`을 더한다. `list_rooms(conn, cap)`은 `room_id <> 'all'`을 묶어 최근 순 `cap - 1`개, `all`의 통계와 가입 member를 따로 읽어 정렬 위치에 끼운다. handler에 SQL 없음.
4. **handlers (`api::rooms`)**: `GET /v1/rooms`, `POST /v1/rooms/{room_id}/messages`. owner 판정은 parts extractor(`OwnerOnly`)로 body보다 먼저. body 검사는 `POST /v1/messages`의 `client_key`·body 크기·`reply_to` 검사를 같은 함수로 재사용한다. 허용 목록 403에는 `details.field="sender_id"`. 오류 매핑은 `POST /v1/messages`와 한 함수(`rejection_error`)에서. commit 뒤 `DeliverySignal::notify`. 로그는 `IF-WIRE-007`.
5. **CLI (`platform`)**: `IF-CLI-001` S10 추가분 그대로. `commands/rooms.rs`(`rooms`, `room post`). `room post`의 출력은 `send`와 같은 `output::sent`, room 표는 `output::rooms`.
6. **예시 설정**: 두 예시 파일에 `[identity]` 앞에 주석 처리한 `#[owner]`/`#post_as = []` 블록과 설명(기본 꺼짐, 운영 Pi는 `["human"]`, 허용 목록에는 owner 자신인 identity만). `100.x.y.z` 자리표시 외의 자리표시를 만들지 않는다(`remote-install.sh` 검사 유지). 주석을 풀면 로드되는지 config 테스트로 확인한다.
7. **스모크 (`deploy/smoke.sh`)**: 새 옵션 `--post-as AGENT_ID`(서버의 `post_as`에 있고 등록된 agent, 예: `human`). 시나리오 `rooms list (14)`: owner `platform rooms --json`에 smoke DM room이 `kind=dm`·`members`=두 smoke agent·`last_sender_id`·`message_count`와 함께 있고 `all`이 있으며, agent key의 `GET /v1/rooms`는 403. 시나리오 `owner room post (14)`: agent key의 게시 403, owner가 smoke agent 명의(허용 목록 밖)로 게시 403 `details.field=sender_id`, `--post-as`가 있으면 owner가 그 명의로 smoke DM room에 `platform room post`(reply_to = smoke DM) → 두 smoke agent의 inbox에 있음, 그 room history에 있음, 같은 client_key 재전송 200 `deduplicated`, 존재하지 않는 DM member의 room 404 `unknown_room`. `--post-as`가 없으면 게시 부분만 SKIP note. 본문은 출력하지 않는다.
8. **운영 메모(오케스트레이터가 실행)**: Pi 반영 순서는 (1) 새 바이너리 배포(`deploy/deploy.sh`, 기존 절차), (2) `/etc/platformd/platformd.toml`에 아래 두 줄 추가, (3) `sudo systemctl restart platformd.service`, (4) `/healthz` 200과 smoke `--post-as human`. S9 이하 바이너리는 모르는 `[owner]` 표로 기동을 거부하므로 (2)를 (1)보다 먼저 하지 않는다. schema 변경이 없어 backup은 평소 절차대로다.
   ```toml
   [owner]
   post_as = ["human"]
   ```

## 완료 정의 (DoD)
- [ ] **설정**: `[owner]` 없음 = 빈 목록, 명시 값 적용, 형식 오류(`Human`, `owner`, 빈 문자열)·중복 기동 거부, 모르는 키 거부. 두 예시 파일의 `[owner]` 주석을 풀면 로드된다.
- [ ] **room 목록**: 401, agent key 403, query parameter 422 `unexpected_field`, 메시지 없는 허브는 `all` 하나(`last_*` null, count 0, members = 가입 agent), DM·`all`이 최근 메시지 순, 메시지 없는 `all`은 맨 뒤, `members`(DM은 두 ID, `all`은 현재 가입자 — 탈퇴 반영), `last_event_seq`·`last_message_at`·`last_sender_id`가 마지막 메시지와 같고 `message_count`가 맞다. GET이 marker를 바꾸지 않는다.
- [ ] **시나리오 14 (barge-in)**: `post_as=["human"]`에서 owner가 `human`으로 `dm:a:b`에 게시 → 201, `room_id`·`sender_id`, a·b 각 inbox row 하나, `human` inbox row 없음, a·b의 `notification_pending`이 pending이고 receiver mock이 a·b 각각의 wake를 받음, owner history와 room 목록에 보임, b가 `reply_to`로 같은 room에 답장 201.
- [ ] **권한**: agent key(허용 목록의 `human` key 포함) 게시 403(body를 보지 않음 — 잘못된 body여도 403), 허용 목록 밖 `sender_id` 403 `details.field=sender_id`, 기본 설정(빈 목록)에서 `human`도 403, 허용 목록에 있지만 미등록 404 `agent_not_found`. 거부된 요청은 아무것도 저장하지 않는다.
- [ ] **room 검사**: 비canonical·형식 오류 room 422 `invalid_field`(`field=room_id`), 미등록 member 404 `unknown_room`, 순서(형식 422가 허용 목록 403보다 먼저, 허용 목록 403이 404보다 먼저).
- [ ] **reply_to**: 다른 room 422 `reply_to_room_mismatch`, 없는 id 404 `reply_to_not_found`, 형식 오류 422 `invalid_field`.
- [ ] **idempotency**: 같은 요청 재전송 200 `deduplicated=true`·같은 `message_id`·메시지 하나, 같은 key 다른 body 409, `all` 게시와 sender 자신의 `to=all` 송신이 같은 namespace(같은 body면 200), DM room 게시 key로 sender 자신의 DM 송신은 409.
- [ ] **`all` 게시**: sender가 `all`을 떠나 있어도 201이고 수신자는 가입자 전원(sender 없음), sender가 가입 중이면 sender도 자기 row.
- [ ] **body 형식**: `to` 등 허용 밖 필드 422 `unexpected_field`, 필수 누락 422 `invalid_field`, 비객체 422, client_key 위반 422, body 16 KiB 초과 413 `body_too_large`.
- [ ] **로그**: room 게시 body 표식 문자열이 TRACE 로그에 없다(`log_redaction` 확장).
- [ ] **CLI**: `rooms` 표 머리와 행, `--json`; `room post`의 body JSON(`--as` → `sender_id`, `--reply-to`, `--client-key`), 생략 시 생성 key, 출력 형식; agent key → exit 1 `forbidden`; 잘못된 room·`--as` 형식 → exit 2.
- [ ] **스모크**: 로컬 `platformd`(`post_as = ["human"]`, `human` 등록)에 대해 `deploy/smoke.sh … --post-as human` 전 시나리오 PASS. `bash -n` 통과.
- [ ] **배포 빌드**: `deploy/build-aarch64.sh` 성공(ELF aarch64, glibc 2.41 이하).
- [ ] fmt/clippy/test 그린. 코드 미학(`CLAUDE.md` 규약). 기존 테스트의 단언은 바꾸지 않는다(`commit_send` helper 추출은 동작 불변).

## 검증 방법 (증거 필수)
- `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` 출력 끝부분(테스트 수 전후). 새 테스트는 구현 전에 실패하는 것을 먼저 보인다.
- 로컬 `platformd`(임시 설정, `[owner] post_as = ["human"]`)에 대해 `platform rooms`, `platform room post …`와 exit code, `deploy/smoke.sh` 실행 표.
- `deploy/build-aarch64.sh` 마지막 줄(ELF·glibc).

## 핸드오프
변경 파일, 검증 로그, DoD 항목별 테스트 이름, 비상 해치 보고. 커밋은 오케스트레이터 지시대로(문서 커밋과 구현 커밋 둘). 푸시하지 않는다.

## 비상 해치
정본대로 불가능/모순일 때만 정지 → 보고. 예: axum extractor 구조상 owner 403을 body 검사 앞에 둘 수 없을 때, `commit_send` helper 추출이 기존 테스트의 단언을 바꿔야만 할 때. 우회 구현 금지.
