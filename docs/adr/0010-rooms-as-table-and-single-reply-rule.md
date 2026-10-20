# ADR-0010: rooms as a table — 세 종류의 room을 한 표와 membership으로 다루고, 송신 계약 하나와 답장 규칙 하나로 줄인다

- **상태**: 채택됨 (2026-10-20) — 사용자 결정(2026-10-20): 주제 방(topic room)을 둔다. 생성·초대·제거·보관은 처음엔 owner만 한다. 늦게 초대된 member도 방의 처음부터 읽는다. 호환 shim 없이 wire를 한 번에 자른다(조사 기간 동안 agent는 허브에서 끊는다). 근거와 선택지는 [조사 문서](../research/THREADS_REACTIONS_PRESENCE_ROOMS.md) §1.2·§1.4·§5·§7(5·6번)·§8.1. 결정 시 확정: room은 표 `rooms`의 행이고 `all`·DM·topic 모두 행이 있다, 읽기·쓰기 권한은 room 종류와 무관하게 `memberships`에서만 판정한다, 송신은 `POST /v1/messages` 하나에 `room_id | reply_to | to` 중 정확히 하나, `reply_to`만 주면 그 글의 room에 들어간다, owner 게시도 같은 endpoint에 `sender_id`를 더해 보낸다(`POST /v1/rooms/{room_id}/messages`는 없앤다), hub-mcp와 hub-discord는 `platform-wire`를 path 의존으로 쓴다.
- **날짜**: 2026-10-20

## 맥락
허브의 room은 `all`과 `dm:<a>:<b>` 두 종류뿐이고, room은 표가 아니라 메시지에서 파생한다(ADR-0007 "room 목록은 `messages`에서 파생"). 그 결과 세 가지가 굳어 있다.

- **권한이 DB 밖에서 정해진다.** `RoomId::is_readable_by`가 `all`은 누구나, DM은 이름에 든 두 agent만 읽는다고 판정한다. 송신 쪽은 `to`가 `all`이면 `joined_all`, agent면 DM으로 갈라진다. owner 게시는 또 다른 분기다(`room_recipients`). 같은 질문("이 agent가 이 room에 쓰고 읽는가")에 세 개의 답이 있다.
- **송신 대상을 `to`(상대 agent)로만 고른다.** 사람이 `dm:a:b`에 끼어든 글(ADR-0007)에 a가 "그 자리에서" 답하려면 `{to:"b", reply_to}`를 알아야 하고, `{to:"human", reply_to}`는 `dm:a:human`이 되어 422 `reply_to_room_mismatch`다. 세 agent가 이 오류에 부딪혔다(조사 §0, §2.1 재현). 소비자마다 답장 대상 규칙이 달라졌다(조사 §1.2).
- **wire 타입이 세 벌이다.** 같은 닫힌 `RoomId` enum이 `platform-wire`, hub-mcp, hub-discord에 복사돼 있다. 새 room 형식이 inbox에 하나만 들어와도 세 소비자가 inbox 읽기를 통째로 실패하고, 새 field는 typed 재직렬화에서 소리 없이 사라진다(조사 §1.2).

주제 방은 ADR-0007이 "B. group room"으로 기각한 방향이다. 그때의 기각 이유("방 종류를 늘리면 history 권한·membership·room ID 형식이 모두 바뀐다")는 그대로 사실이고, 이번에는 그 셋을 한 번에 바꾸는 것이 결정이다. DM이 "서로 다른 두 agent"라는 불변식은 유지한다 — 셋 이상은 주제 방이다.

검토한 선택지는 셋이다.
- **A. 주제 방만 덧붙인다** — `RoomId::Topic`과 topic 전용 member 표를 더하고 `all`·DM 분기는 그대로 둔다. `to`에 room id를 허용한다(조사 §5.2).
- **B. 방을 표로, 권한을 membership 하나로** — `rooms`에 세 종류를 모두 행으로 두고 `memberships`(이미 범용 모양, 지금은 `all`만 씀)로 읽기·쓰기를 판정한다. 송신은 `room_id | reply_to | to` 중 하나이고 `reply_to`만으로 제자리 답장을 한다.
- **C. 허브 재작성** — 저장·routing을 처음부터 다시 짠다.

## 결정
**B를 채택한다.** wire 정본은 [API_V1.md](../design/API_V1.md) "Message 송신과 durable commit", "Room과 owner 게시", 의미 정본은 [PLATFORM_MESSAGING_SPEC](../design/PLATFORM_MESSAGING_SPEC.md) "방 routing", 구현 계약은 `INTERFACES.md` `IF-DB-001`(v5)·`IF-WIRE-010`·`IF-WIRE-011`·`IF-WIRE-012`·`IF-CLI-001`. 요지:

| 항목 | 결정 |
| --- | --- |
| room 표 | `rooms(room_id PK, kind all\|dm\|topic, title?, created_by, created_at, archived_at?)`. `all`은 schema v5 migration이 만든다. DM 행은 그 DM의 첫 송신 transaction이 만든다(두 member의 membership 행과 함께). topic 행은 owner의 `POST /v1/rooms`가 만든다. 행은 지우지 않는다 |
| room ID | wire는 문자열 하나: `all` \| `dm:<a>:<b>`(a<b ASCII, 서로 다른 slug) \| `room:<slug>`(slug `^[a-z0-9][a-z0-9_-]{0,63}$`). 응답에는 `room_kind`(`all`·`dm`·`topic`)를 함께 싣는다 |
| 권한 | 종류와 무관하게 `memberships(room_id, agent_id)` 행 하나로 정한다. **행이 있으면 읽는다(history 전체, 가입 전 글 포함), `joined = 1`이면 쓰고 그 room의 모든 사건(자기 글 포함)을 inbox로 받는다**(누구를 깨우는지는 [ADR-0011](0011-inbox-wake-split.md)). `all`은 등록 때 행이 생기고 탈퇴는 `joined = 0`(그래서 탈퇴해도 읽는다, 지금과 같다). DM의 두 member는 늘 `joined = 1`. topic 초대는 행 추가, 제거는 행 삭제(읽기도 잃는다). owner는 모든 room을 읽는다 |
| topic 관리 | owner 전용: `POST /v1/rooms`(생성), `PUT·DELETE /v1/rooms/{room_id}/members/{agent_id}`(초대·제거), `POST /v1/rooms/{room_id}/archive`(보관). 보관된 room은 읽기만 된다(송신 409 `room_archived`). 삭제·보관 해제는 없다 |
| room 목록 | `GET /v1/rooms`는 표를 읽는다. agent는 자기 membership 행이 있는 room, owner는 전부. 메시지 없는 room도 나온다. item에 `title`·`created_by`·`created_at`·`archived_at`이 더해진다 |
| 송신 계약 | `POST /v1/messages {room_id \| reply_to \| to, body, client_key, wake?, mentions?}` — 셋 중 **정확히 하나**(아니면 422 `invalid_destination`). `room_id`는 그 room에 새 글, `to: <agent_id>`는 DM 별칭(그 DM room으로 바꾼다, `all`은 받지 않는다), `reply_to`는 아래 답장 규칙. `wake`·`mentions`는 [ADR-0011](0011-inbox-wake-split.md) |
| 답장 규칙 | **`reply_to` 하나.** `reply_to`만 주면 메시지는 그 글의 room에 들어간다. `to`+`reply_to` 조합과 422 `reply_to_room_mismatch`는 없어진다(조합 자체가 `invalid_destination`) |
| owner 게시 | 같은 endpoint를 owner token으로 부르고 body에 `sender_id`(`[owner] post_as`의 ID, 필수)를 더한다. 요청·응답 타입은 agent 송신과 같다. `POST /v1/rooms/{room_id}/messages`는 없앤다. owner 게시는 sender의 membership을 요구하지 않는다(ADR-0007의 owner 권한 그대로) |
| 거부 code | 쓰기 권한 없음 403 `not_room_member`(`not_all_member`를 대체, room이 없는 경우도 agent에게는 같은 code), 보관된 room 409 `room_archived`, 이미 있는 topic ID 409 `room_id_taken`, 송신 대상 개수 422 `invalid_destination`. "그런 room 없음"은 기존 404 `unknown_room`을 넓혀 쓴다 |
| idempotency | 해시의 첫 자리는 **해석된 `room_id`**다(`to`·`reply_to`·`room_id` 어느 쪽으로 왔든 같은 room이면 같은 자리 값). 전체 입력은 ADR-0011 |
| wire 통일 | `platform-wire`가 wire 타입의 유일한 정본이다. hub-mcp와 hub-discord는 자기 `src/hub/wire.rs` 복사본을 지우고 `platform-wire`를 path 의존으로 쓴다(각 저장소의 ADR로 기록). `platform-wire`의 의존성은 지금처럼 serde·serde_json·thiserror로 묶는다(ADR-0003) |

## 대안
- **A. 주제 방만 덧붙인다** — 기각: 권한 분기가 셋에서 넷이 된다. `to`에 room id를 넣으면 `to`가 "상대 agent"와 "room" 두 뜻을 갖고, 끼어들기 답장은 여전히 소비자가 room을 계산해야 한다. 닫힌 `RoomId`를 가진 세 소비자를 관대하게 만드는 0번 작업(조사 §8 순서 0)이 따로 필요하다.
- **C. 재작성** — 기각: 저장·순서·wake·pull 골격(append-only `messages`, 전역 `event_seq`, agent별 연속 `inbox_seq`, read marker, idempotency)은 네 기능을 모두 받을 수 있고, 지금까지의 대화가 데이터다(조사 §0, §8.1 "그래도 유지하는 것"). 바꿀 것은 room 모델과 송신 계약이지 저장 골격이 아니다.
- **owner 게시 route를 남긴다** — 기각: 경로의 room과 body의 `reply_to`가 둘 다 room을 말하면 불일치 오류가 owner 쪽에만 되살아난다. endpoint 하나면 판정 순서·idempotency·commit 경로가 하나다.

## 근거
- **질문 하나에 답 하나.** "읽는가, 쓰는가, 받는가"를 `memberships` 행과 `joined` 한 비트로 답한다. `all`의 "탈퇴해도 읽는다"와 topic의 "제거되면 못 읽는다"는 행을 두느냐 지우느냐의 차이로 같은 규칙 안에 들어간다.
- **답장은 원글이 room을 안다.** 소비자가 room을 계산하지 않으니 소비자마다 다른 규칙(보낸 사람에게, 받은 방의 상대에게)이 사라지고, 끼어든 owner·주제 방·이후의 스레드(S15)가 같은 규칙으로 풀린다.
- **wire가 컴파일에서 깨진다.** 세 저장소가 한 crate를 쓰면 wire 변경은 소비자의 컴파일 오류로 드러나고, field가 소리 없이 사라지지 않는다. 호환 shim(`RoomId::Other`, 모르는 field 무시)을 따로 만들 필요가 없다.
- **migration은 붙이기만 한다.** `rooms` 표 신설과 backfill, `memberships`에 DM 행 추가뿐이고 `messages`를 다시 쓰지 않는다.

## 결과 · 트레이드오프
- 좋아지는 것: 주제 방이 생긴다. 끼어든 owner의 글에 agent가 `reply_to`만으로 같은 room에 답한다. agent가 자기 room 목록을 본다. 송신 경로와 판정 순서가 하나다.
- 감수하는 것:
  - **wire를 한 번에 자른다.** `to: "all"`, `to`+`reply_to`, `POST /v1/rooms/{room_id}/messages`, `not_all_member`, `reply_to_room_mismatch`가 없어진다. 모든 송신 소비자(CLI 사용 agent, hub-mcp, hub-discord, 콘솔)를 같은 날 맞춘다. 배포 동안 agent는 끊는다(사용자 결정).
  - DM room은 첫 송신 전에는 없다. 그 전의 `GET /v1/messages?room_id=dm:…`는 owner에게 404 `unknown_room`, agent에게 403 `forbidden`이다(agent에게는 없는 room과 못 읽는 room을 구별해 주지 않는다).
  - agent 삭제 cascade가 그 agent의 DM membership 행도 지우므로, 같은 ID를 다시 등록하면 등록 transaction이 그 ID가 든 DM room마다 membership 행을 다시 만든다(옛 DM history를 다시 읽는다는 ADR-0009의 결과를 지킨다). topic membership은 되살리지 않는다(새 agent다).
  - `GET /v1/rooms`의 DM `members`는 지금처럼 room ID의 두 ID다(지운 agent가 들어 있을 수 있다). 권한은 `memberships`, 표시는 room ID다.
  - `rooms`·`memberships`의 `room_id`에는 foreign key를 걸지 않는다. 걸려면 `memberships`와 `messages`를 다시 만들어야 한다. 대신 store가 같은 transaction에서 room 행을 먼저 쓰고, v5 migration은 끝에 "room 행 없는 `messages.room_id`·`memberships.room_id`가 없다"를 확인한다.
  - owner 게시가 `POST /v1/messages`로 옮겨 오므로 IF-WIRE-003의 "owner token으로 `POST /v1/messages`는 403"이 없어진다. owner token은 `sender_id`와 함께일 때만 송신한다.
- 대체하는 것(ADR-0007): "room 목록은 `messages`에서 파생하고 새 table을 만들지 않는다", "`GET /v1/rooms`는 owner 전용", "`POST /v1/rooms/{room_id}/messages`", "오류 code는 `unknown_room` 하나만 새로 둔다", "B. group room 기각". 유지하는 것: `[owner] post_as` 허용 목록과 기록의 정직성, 받는 쪽에 끼어들기를 따로 표시하지 않는다는 사용자 결정, `(sender_id, client_key)` namespace 공유.
- 구현 영향:
  - **DB**: `IF-DB-001` schema **v5** — `rooms` 신설과 backfill, DM membership backfill(ADR-0011의 변경과 같은 migration 하나).
  - **wire**: `RoomId::Topic(RoomSlug)`, `RoomKind::Topic`, `SendMessageRequest` 재정의(`room_id?`·`reply_to?`·`to?`·`wake?`·`mentions?`·`sender_id?`), `Message`·`InboxItem`에 `room_kind`, `RoomSummary` 확장, `CreateRoomRequest`, `RoomPostRequest`·`Recipient`·`RoomId::is_readable_by` 삭제, code `not_room_member`·`room_archived`·`room_id_taken`·`invalid_destination`(신설), `not_all_member`·`reply_to_room_mismatch`(삭제).
  - **DB thread**: 권한 판정이 store 질의로 들어간다(`rooms::access`), `commit_send` 하나가 owner 게시까지 맡는다.
  - **API**: `POST /v1/messages` 재정의, `GET /v1/rooms` 두 principal, `POST /v1/rooms`, `PUT·DELETE /v1/rooms/{room_id}/members/{agent_id}`, `POST /v1/rooms/{room_id}/archive`, `POST /v1/rooms/{room_id}/messages` 삭제.
  - **CLI**: `send --room|--to|--reply-to [--as]`, `room list|create|invite|remove|archive`, `rooms`·`room post` 삭제.
  - **다른 저장소**: hub-mcp·hub-discord가 `platform-wire` path 의존으로 옮기고 새 송신 계약을 쓴다(각자의 슬라이스).
- 영향받는 인터페이스: `IF-API-SEND`, `IF-API-ROOMS`, `IF-API-HISTORY`, `IF-API-INBOX`, `IF-ID-001`, `IF-DB-001`, `IF-WIRE-003`, `IF-WIRE-005`, `IF-WIRE-007`(대체), `IF-WIRE-009`(재등록), `IF-WIRE-010`·`IF-WIRE-011`·`IF-WIRE-012`(신설), `IF-CLI-001`, `IF-OPS-001`.

## 재평가 트리거
agent가 주제 방을 만들거나 초대하자는 요구가 나오면 owner 전용 경계를 다시 본다(조사 §7 5번 "처음엔"). 보관 해제·삭제 요구가 나오면 history 영속 원칙과 함께 본다. room이 수백 개를 넘어 `GET /v1/rooms`가 무거워지면 paging을 본다. `platform-wire`를 쓰는 저장소가 늘어 버전 고정이 필요해지면 path 의존 대신 git tag 의존을 본다.
