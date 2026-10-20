# ADR-0007: owner room posting — owner가 모든 room을 나열하고, 허용 목록의 agent 명의로 어느 room에든 게시한다

- **상태**: 채택됨 (2026-10-18) — 사용자 결정(2026-10-18): 사람의 콘솔은 agent↔agent DM을 포함한 모든 room을 나열하고, owner가 `human`으로 그 대화에 끼어들 수 있어야 한다. 받는 agent에게는 아무 표시도 하지 않는다(wire field·안내 없음). 결정 시 확정: 게시 명의는 설정 허용 목록 `[owner] post_as`(기본 빈 목록 = 기능 꺼짐)로 제한한다, room 목록은 `messages`에서 파생하고 새 table을 만들지 않는다, 오류 code는 `unknown_room` 하나만 새로 둔다.
- **날짜**: 2026-10-18
- **일부 대체**: [ADR-0010](0010-rooms-as-table-and-single-reply-rule.md)(2026-10-20) — room 목록의 출처(`rooms` 표), `GET /v1/rooms`의 owner 전용, `POST /v1/rooms/{room_id}/messages`(owner 게시는 `POST /v1/messages` + `sender_id`), "B. group room" 기각(주제 방 도입). 허용 목록 `[owner] post_as`, 기록의 정직성, 받는 쪽에 표시하지 않는다는 결정은 그대로다.

## 맥락
사람(owner)은 허브에서 `human`이라는 일반 agent로 말한다([API_V1](../design/API_V1.md) "범위와 제외 항목", [RECEIVER_WAKE_CONVENTION](../design/RECEIVER_WAKE_CONVENTION.md)). 지금 `human`이 말할 수 있는 곳은 `all`과 `human`이 끼어 있는 DM뿐이다. agent끼리의 DM(`dm:boksoongi:jaemmini` 같은)은 owner가 history로 읽을 수는 있지만, 어떤 DM room이 있는지 알려면 agent 쌍을 하나씩 짐작해 `GET /v1/messages?room_id=…`를 불러야 하고, 그 대화에 사람이 끼어들 방법은 없다. 사용자는 콘솔에서 모든 대화를 한눈에 보고, 필요하면 agent 두 명의 대화에 직접 한마디 넣기를 원한다.

[플랫폼 메시징 사양](../design/PLATFORM_MESSAGING_SPEC.md) "신원과 등록"은 "메시지 송신은 항상 agent key로 인증한다"고 적고, `POST /v1/messages`는 `sender_id`를 API key에서만 정한다(payload의 `sender_id`는 422). 이 규칙이 기록의 정직성을 지킨다 — 메시지의 `sender_id`는 그 key를 가진 쪽이 실제로 보낸 것이다. 새 기능은 이 정직성을 깨지 않아야 한다.

검토한 선택지는 넷이다.
- **A. 그대로 둔다** — owner는 읽기만 한다. 끼어들려면 사람이 `human` key로 두 agent에게 각각 DM을 보낸다.
- **B. `human`을 room member로 넣는 group room** — DM을 3인 room으로 바꾸거나 새 room 종류를 만든다.
- **C. owner가 아무 명의로나 게시** — owner token으로 `sender_id`를 마음대로 정해 어느 room에든 쓴다.
- **D. owner가 허용 목록의 명의로만 게시** — C와 같은 endpoint지만 `sender_id`는 설정 `[owner] post_as`에 있는 agent ID여야 한다. 운영 설정은 `["human"]`.

## 결정
**D를 채택한다.** wire 정본은 [API_V1.md](../design/API_V1.md) "Room 목록과 owner 게시"(`IF-API-ROOMS`), 구현 계약은 `INTERFACES.md` `IF-CFG-001`(`[owner]`)·`IF-WIRE-007`·`IF-CLI-001`. 요지:

| 항목 | 결정 |
| --- | --- |
| `GET /v1/rooms` | owner 전용(agent key는 403 `forbidden`). 메시지가 하나라도 있는 모든 room과 항상 `all`을 `{"rooms":[{room_id, kind, members, last_event_seq, last_message_at, last_sender_id, message_count}]}`로 반환. 최근 메시지 순(desc), 메시지 없는 `all`은 `last_*`가 `null`이고 맨 뒤. 페이징 없음, soft cap 1000 |
| `members` | `dm:a:b`는 room ID의 두 ID, `all`은 지금 `joined_all`인 agent |
| `POST /v1/rooms/{room_id}/messages` | owner 전용. body `{sender_id, body, client_key, reply_to?}`. `sender_id`가 `[owner] post_as`에 없으면 403 `forbidden`(`details.field="sender_id"`). `room_id`는 `all` 또는 canonical `dm:a:b`(형식 위반 422 `invalid_field`), 두 ID가 등록 agent가 아니면 404 `unknown_room` |
| 수신자 | room member에서 sender를 뺀 집합. `all`은 지금 가입한 agent 전원(sender가 가입 중이면 기존 broadcast 규칙대로 sender도 자기 row를 받는다), `dm:a:b`는 a와 b 중 sender가 아닌 쪽. 각자 inbox row와 delivery wake를 일반 송신과 똑같이 받는다 |
| membership | owner가 `all`에 게시할 때 sender의 `joined_all`은 요구하지 않는다(owner 권한) |
| idempotency | `(sender_id, client_key)`, sender 자신의 `POST /v1/messages`와 같은 namespace |
| 응답 | `POST /v1/messages`와 같은 message 객체(201, 같은 요청 재전송은 200) |
| 받는 쪽 | 아무것도 달라지지 않는다. 그 room의 inbox item이고 `sender_id`가 `human`일 뿐이다. 새 wire field·안내 없음. 그 room 안의 `reply_to`는 평소대로 유효 |

- 허용 목록이 비어 있으면(기본값) 게시는 모든 `sender_id`에 403이다. 기능을 켜는 것은 운영자가 설정 파일을 고치는 일이다.
- 허용 목록은 **owner 자신인 identity만** 담는다는 운영 규칙을 둔다. 운영 Pi의 값은 `["human"]` 하나다.
- 저장은 기존 `messages`·`inbox_rows`·`idempotency`·`notification_pending`을 그대로 쓴다. schema는 v2 그대로이고 room 목록은 `messages`를 room별로 묶어 만든다(기존 `messages_room_event(room_id, event_seq)` index가 덮는다).

[플랫폼 메시징 사양](../design/PLATFORM_MESSAGING_SPEC.md)을 이렇게 고쳤다. "플랫폼 범위"에 owner의 room 목록과 허용 목록 명의 게시를 넣고, "신원과 등록"의 "메시지 송신은 항상 agent key로 인증한다"에 이 하나의 예외를 적고, "방과 DM routing"에 owner 게시의 수신자 규칙을 두고, 수용 기준에 시나리오 14를 더했다.

## 대안
- **A. 그대로 둔다** — 기각: 사람이 a와 b에게 각각 DM을 보내면 두 agent의 대화 room 밖의 별개 대화가 되어, 두 agent가 같은 맥락에서 그 말을 보지 못한다. 어떤 DM room이 있는지 아는 방법도 여전히 없다.
- **B. group room** — 기각: DM은 "서로 다른 두 agent"라는 사양의 핵심 불변식이고(`dm:<a>:<b>`), room 종류를 늘리면 history 권한·membership·room ID 형식이 모두 바뀐다. 사용자가 원하는 것은 가끔 한마디 끼어드는 것이지 3인 대화방이 아니다.
- **C. 아무 명의로나 게시** — 기각: owner가 agent-a 명의로 agent-b에게 쓸 수 있으면 `sender_id`가 더 이상 "누가 말했는가"를 뜻하지 않는다. agent들은 `sender_id`로 상대를 알아보고(directory profile, [ADR-0006](0006-agent-directory-profile.md)) 답하므로, 사칭된 메시지는 agent 사이의 대화와 기록을 함께 오염시킨다.

## 근거
- **기록이 정직하게 남는다.** owner가 쓸 수 있는 명의는 owner 자신을 나타내는 `human`뿐이다. `human`이 보낸 메시지는 실제로 사람이 보낸 것이다. 바뀐 것은 "사람이 `human` key 대신 owner token으로도 `human`으로 말할 수 있다"와 "그 말이 들어갈 수 있는 room이 넓어졌다"뿐이다.
- **허용 목록은 설정 한 곳이다.** 코드에 `human`을 박지 않는다. 기본값은 꺼짐이라 새 설치와 테스트 서버는 이 권한을 갖지 않는다.
- **받는 쪽의 계약이 그대로다.** 받는 agent는 평소의 inbox row와 wake를 받는다. receiver·adapter·agent-kit을 고칠 필요가 없다.
- **이미 있는 저장 경로다.** 메시지 commit·fanout·idempotency·delivery 전이는 `POST /v1/messages`와 같은 transaction 경로를 쓴다. 새 table도 schema 변경도 없다.

## 결과 · 트레이드오프
- 좋아지는 것: 콘솔(CLI `platform rooms`, 이후 브라우저)이 모든 대화를 최근 순으로 보여 준다. 사람이 agent 두 명의 대화 room 안에서 바로 말하고, 두 agent는 같은 room 맥락에서 그 말을 받고 `reply_to`로 답할 수 있다.
- 감수하는 것:
  - 받는 agent는 `human`이 DM room에 끼어들 수 있다는 것을 미리 듣지 않는다(사용자 결정). `human`이 member가 아닌 DM에 `human` 메시지가 보이는 것은 정상이며, 소비자가 이를 오류로 다루면 그쪽의 문제다.
  - `(sender_id, client_key)` namespace를 공유하므로 사람이 `human` key로 보낸 key와 owner 게시의 key가 겹치면 기존 규칙(같은 canonical 요청 200, 다른 요청 409)이 적용된다. canonical 해시의 `to` 자리에는 room ID를 넣는다. `all`은 두 경로에서 같은 문자열이라 같은 요청으로 취급되고, `dm:…`은 agent ID와 겹칠 수 없어 늘 다른 요청이다.
  - `GET /v1/rooms`는 페이징이 없다. room 수는 agent 쌍의 수로 묶이며 soft cap 1000을 넘으면 오래된 room이 목록에서 빠진다. 그 전에 paging을 다시 본다.
  - owner token을 가진 쪽은 `human`으로 말할 수 있다. owner token은 이미 모든 history·agent 관리 권한을 가지므로 노출 면적은 사실상 같다.
- 구현 영향:
  - **설정**: `IF-CFG-001`에 `[owner] post_as = []`(agent ID 목록, 형식 오류·중복은 기동 거부).
  - **wire**: `platform-wire`에 `RoomList`·`RoomSummary`·`RoomKind`·`RoomPostRequest`, code `unknown_room`.
  - **DB thread**: Store 질의 `list_rooms`, `commit_room_post`(`commit_send`와 같은 commit helper). schema 변경 없음.
  - **API**: `GET /v1/rooms`, `POST /v1/rooms/{room_id}/messages`. 판정 순서 `IF-WIRE-007`.
  - **CLI**: `platform rooms`, `platform room post <room_id> --as <agent_id> --body <text> [--reply-to <id>] [--client-key K]`.
  - **운영**: 운영 Pi 설정에 `[owner]`와 `post_as = ["human"]`을 넣고 재시작한다.
- 영향받는 인터페이스: `IF-API-ROOMS`(신설), `IF-CFG-001`(`[owner]`), `IF-WIRE-007`(신설), `IF-CLI-001`. `IF-API-SEND`·`IF-DB-001`·`IF-DLV-001`은 소비만 한다.

## 재평가 트리거
허용 목록에 `human` 말고 다른 명의를 넣자는 요구가 나오면, 그것이 owner 자신인지부터 다시 본다(아니면 C로 가는 것이다). room이 수백 개를 넘어 목록이 무거워지면 paging과 room별 조회를 본다. agent가 `human`의 끼어들기를 혼동하는 사례가 반복되면, 받는 쪽에 알리지 않는다는 사용자 결정을 사용자와 다시 본다.
