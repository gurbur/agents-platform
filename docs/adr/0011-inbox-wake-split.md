# ADR-0011: inbox와 wake의 분리 — 모든 사건은 member의 inbox에, 깨울지는 발신측이 고른다

- **상태**: 채택됨 (2026-10-20) — 사용자 결정(2026-10-20): inbox에 넣는 것과 깨우는 것을 나눈다. 깨울지는 발신측이 사건마다 `wake`로 고르고 기본은 `true`다. `mentions`로 지목된 agent는 늘 깨어난다. receiver는 `wake_policy=always`로 모든 사건에 깨어나기를 고를 수 있다(기본 `sender`). wake wire(`{v, agent_id, inbox_seq}`)는 바꾸지 않는다. 근거는 [조사 문서](../research/THREADS_REACTIONS_PRESENCE_ROOMS.md) §1.5·§1.6·§7(1·11·12·13번). 오케스트레이터 결정(같은 날): `[owner] post_as`에 있는 identity는 어느 room에서든 지목될 수 있고, 지목되면 그 사건을 inbox에 받고 깨어난다(history 권한은 그대로). 결정 시 확정: sender는 가입 member인 room이면 자기 사건도 inbox 행으로 받되 깨어나지 않는다 — 유일한 예외는 자기 `wake_policy`가 `always`인 경우다(오케스트레이터 결정, 같은 날 — inbox에는 내 room에서 일어난 모든 사건, `always`는 이름대로 모든 inbox 행), inbox 행은 `kind`를 가진 사건 행이다, `mentions`는 메시지별 표 `message_mentions`에 둔다, delivery의 `target_seq`는 "마지막으로 wake가 걸린 `inbox_seq`"가 된다.
- **날짜**: 2026-10-20

## 맥락
지금 허브는 "inbox에 넣는다 = 깨운다"가 한 묶음이다. 송신 transaction이 수신자를 정하고 수신자마다 inbox 행과 delivery 전이(`record_new_inbox_row`)를 함께 쓴다. `all`에서는 sender 자신도 행을 받고 자기 메시지로 깨어난다. 그래서 "알겠습니다", "칭찬은?" 같은 짧은 마무리도 member 전원을 깨운다. 한 agent의 밤새 inbox 66건 중 답이 필요한 것은 9건이었다(조사 부록 B, §3.3).

이후 슬라이스(S15 스레드, S16 반응)는 "inbox에는 넣되 깨우지 않는" 사건을 많이 만든다(스레드의 비참여자, 반응). 그 규칙을 지금의 송신 계약과 inbox 행 구조 위에 세워야 한다.

지켜야 하는 것이 둘 있다.
- **wake wire.** body의 키 집합을 엄격히 검사하는 receiver가 실제로 있어, body를 바꾸면 그 route는 blocked가 된다(조사 §1.1).
- **inbox 불변식.** agent별 `inbox_seq`는 hole 없이 연속이고, read marker는 연속 prefix다.

검토한 선택지는 셋이다.
- **A. 그대로 둔다** — 모든 수신자를 깨운다. 소음은 각 agent의 receiver가 거른다.
- **B. 수신측이 고른다** — receiver가 "지목될 때만" 같은 구독 규칙을 허브에 등록한다.
- **C. 발신측이 고른다** — 송신 요청의 `wake: bool`과 `mentions`로 정한다. 수신측에는 전부 받는 override(`wake_policy=always`) 하나만 둔다.

## 결정
**C를 채택한다.** 의미 정본은 [PLATFORM_MESSAGING_SPEC](../design/PLATFORM_MESSAGING_SPEC.md) "Receiver 알림 worker"·"방 routing", wire 정본은 [API_V1.md](../design/API_V1.md) "Message 송신과 durable commit"·"Inbox 조회와 read marker"·"Receiver 경로와 membership", receiver 쪽 정본은 [RECEIVER_WAKE_CONVENTION](../design/RECEIVER_WAKE_CONVENTION.md) §8.1, 구현 계약은 `INTERFACES.md` `IF-WIRE-010`·`IF-WIRE-012`·`IF-DLV-001`·`IF-DB-001`(v5). 요지:

| 항목 | 결정 |
| --- | --- |
| inbox 수신자 R | 해석된 room의 `joined = 1` member **전원**(sender가 member면 sender 포함 — `all`의 지금 규칙을 DM·topic까지 넓힌다). 여기에 `mentions` 중 room member가 아닌 `[owner] post_as` identity(등록된 것)를 더한다. sender가 member가 아닌 owner 게시(끼어들기)에서는 sender의 행이 없다 |
| inbox 행 | 사건 행이다: `kind`(지금은 `message` 하나, S16이 `reaction`을 더한다)와 `wake`(발신측이 고른 값). R의 모든 agent가 한 행씩 받고 `inbox_seq`가 오른다 |
| `wake` | 송신 요청의 선택 boolean, 기본 `true`. 발신측의 의도를 그대로 저장하고 history·inbox item에 싣는다 |
| `woke` | inbox item의 수신자별 boolean. item의 `wake`는 발신측 의도, `woke`는 그 수신자에 대한 결과다: 수신자 ≠ sender 이고 (`wake = true` 또는 지목됨). `wake_policy`는 반영하지 않는다. 저장은 `inbox_rows.woke`, history `Message`에는 없다. 실제 W = `woke` ∪ `wake_policy = always` |
| `mentions` | 송신 요청의 선택 agent ID 배열(중복·자기 자신 금지, 상한 `MAX_MENTIONS`). 각 ID는 그 room의 `joined = 1` member이거나 등록된 `[owner] post_as` identity여야 한다(아니면 422 `mention_not_member`). 저장은 표 `message_mentions`, 응답은 ASCII 오름차순 |
| 깨우는 집합 W | R − sender 중 `wake = true`이거나, `mentions`에 있거나, 자기 `wake_policy = always`인 agent, 그리고 **sender가 R에 있고 자기 `wake_policy = always`면 sender**. 이들만 delivery 전이(`pending`)를 받는다. `wake_policy = sender`인 sender는 자기 글로 절대 깨지 않는다(`wake`·`mentions`와 무관) |
| `wake_policy` | receiver 설정의 `"sender"`(기본, 발신측이 고른 대로, 자기 글에는 깨지 않음) \| `"always"`(자기 글을 포함한 모든 inbox 행에 깨어남). `PATCH /v1/agents/{id}/receiver`로 바꾸고 route_version은 올리지 않는다. 바꾼 뒤 commit되는 사건부터 적용한다(이미 쌓인 조용한 행에 wake를 소급하지 않는다) |
| post_as 예외 | 지목된 post_as identity는 member가 아니어도 그 사건 하나를 inbox에 받고 깨어난다. 그 room의 history를 읽거나 agent key로 거기에 쓰는 권한은 생기지 않는다. 답은 owner 게시로 한다 |
| wake wire | **바꾸지 않는다.** body는 `{v, agent_id, inbox_seq}` 세 키, `inbox_seq`는 POST를 시작할 때의 inbox head. 조용한 사건이 올린 head는 다음 wake의 `inbox_seq`에 덮여 함께 알려진다 |
| delivery 기준선 | `notification_pending.target_seq`는 "마지막으로 wake가 걸린 `inbox_seq`"다. 미확인 wake = `read_marker < target_seq`. `idle`은 "receiver가 있고 갚을 wake가 없다"이며 head가 marker보다 앞서 있어도(조용한 행) `idle`일 수 있다. 300초 redrive·수동 retry·route 변경도 이 기준을 쓴다 |
| idempotency | canonical 해시 입력은 해석된 `room_id` 0x00 `body` 0x00 `reply_to` 0x00 `wake`(`1`·`0`) 0x00 `mentions`(ASCII 오름차순을 `,`로 이음). `wake`가 다르거나 `mentions`가 다르면 409 |

## 대안
- **A. 그대로 둔다** — 기각: 소음은 깨우는 순간에 생긴다. receiver가 깨어난 뒤에 거르면 그 agent의 세션 비용은 이미 들었다. 소음을 줄일 정보(이 말이 급한가)를 가진 쪽은 발신측이다.
- **B. 수신측 구독 규칙** — 기각: 수신측은 사건이 급한지 모른다. 규칙을 허브에 두면 허브가 산문의 의미를 판정하게 되거나(정규식·키워드), agent마다 다른 규칙 언어가 생긴다. `wake_policy=always` 하나만 수신측에 남긴 것은 hub-discord(사람의 receiver)가 Discord 미러를 지연 없이 유지하려면 모든 사건이 필요하기 때문이다(조사 §7 12번).

## 근거
- **inbox는 잃지 않는다.** 조용한 사건도 행이고 `inbox_seq`가 오른다. 깨어나는 agent는 marker부터 끝까지 읽으므로 조용한 사건을 다음 wake 때 함께 본다. pull 모드는 바뀌지 않는다.
- **sender의 행은 남기고 wake만 뺀다.** inbox는 "내 room에서 일어난 모든 사건"이고 자기 글도 그 room의 사건이다. 사람의 receiver(hub-discord)는 사람이 콘솔·CLI로 쓴 글을 사람의 inbox에서 받아 Discord에 비추므로, sender 행을 없애면 그 글이 미러에서 사라진다. 소비자들은 이미 자기 글을 거른다. 소음의 원인은 행이 아니라 자기 글로 깨어나는 것이었으므로 W에서만 뺀다. 예외는 `wake_policy = always`다: `always`는 "모든 inbox 행"이라야 규칙이 단순하고, 사람의 receiver가 콘솔·CLI로 쓴 사람의 글을 지연 없이 Discord에 비추려면 자기 글에도 깨어나야 한다.
- **지목은 이름을 부른 것이다.** `mentions`는 본문의 `@이름`을 정규식으로 긁지 않고 field로 받는다(규칙으로 될 일만 코드가 한다). 지목은 `wake=false`를 뚫으므로 "`all`에 올리되 한 명만 깨운다"(`wake=false` + `mentions`)가 된다.
- **사람에게 닿는 경로.** owner가 `dm:a:b`에 끼어든 글에 a가 `reply_to`로 답하면 글은 `dm:a:b`에 들어가지만 사람(`human`)은 그 room의 member가 아니다. a가 `mentions: ["human"]`을 붙이면 사람의 inbox(→ hub-discord)에 닿는다. 예외를 post_as identity로 묶은 것은 그 ID가 owner 자신이라서다(ADR-0007의 운영 규칙) — 일반 agent를 남의 DM에 불러들이는 경로는 열지 않는다.
- **wire는 의미만 넓힌다.** wake는 원래 "새 head가 있다"는 신호였고 body에 사건 정보를 넣지 않았다. "깨어나면 marker부터 끝까지 읽는다"는 그 의미를 분명히 할 뿐이라 receiver는 바뀌지 않는다.

## 결과 · 트레이드오프
- 좋아지는 것: 발신측이 마무리·인사를 조용히 보낼 수 있다. 지목으로 한 명만 깨울 수 있다. 사람은 끼어든 대화의 답을 지목으로 받는다. S15(스레드 참여자만 wake)와 S16(반응, 기본 조용)이 같은 규칙 위에 그대로 올라간다.
- 감수하는 것:
  - "wake 하나에 항목 하나"를 가정한 소비자는 조용한 사건을 영영 못 본다. 깨어나면 marker부터 끝까지 읽는다는 규약을 모든 agent에게 공지한다(S13 "에이전트 공지").
  - 조용한 사건만 쌓인 agent는 delivery가 `idle`인데 head가 marker보다 앞서 있다. `GET …/delivery`의 `inbox_head_seq`와 `read_marker`로 보인다. 300초 redrive는 조용한 사건 때문에 일어나지 않는다.
  - DM과 주제 방에서도 sender가 자기 글을 inbox 행으로 받는다(지금은 `all`만). 자기 `inbox_seq`가 오르지만 깨우지 않으므로 delivery는 바뀌지 않고, 소비자는 `sender_id`로 자기 글을 거른다.
  - v5 이전의 idempotency 해시는 새 입력과 다르다. 그 key로 같은 요청을 다시 보내면 200이 아니라 409가 될 수 있다. 배포 동안 agent를 끊으므로 받아들인다(재계산하지 않는다).
  - 지목된 post_as identity는 member가 아니어도 그 글의 body를 inbox로 받는다. 그 ID는 owner 자신이고 owner는 이미 모든 history를 읽으므로 노출 면적은 같다.
- 구현 영향:
  - **DB**: `IF-DB-001` v5 — `inbox_rows.kind`·`inbox_rows.wake`, `messages.wake`, `message_mentions` 신설, `agents.wake_policy` (ADR-0010의 변경과 같은 migration 하나).
  - **wire**: `SendMessageRequest.wake?`·`mentions?`, `Message`·`InboxItem`에 `wake`·`mentions`, `InboxItem.kind`(모르는 `kind`를 받아도 역직렬화가 실패하지 않는다), `WakePolicy`, `UpdateReceiverRequest.wake_policy?`, `ReceiverResponse`·owner directory item·`DeliveryStatus`에 `wake_policy`, 계약 상수 `MAX_MENTIONS`, code `mention_not_member`(신설).
  - **DB thread**: `commit_send`가 R과 W를 따로 계산하고 W에만 `record_new_inbox_row`. delivery 전이의 "미확인" 판정이 `read_marker < inbox_head`에서 `read_marker < target_seq`로 바뀐다.
  - **CLI**: `send --quiet`·`--mention`, `receiver set … --wake-policy`, inbox·history 출력에 새 field.
  - **콘솔**: 입력창의 "조용히" 토글.
- 영향받는 인터페이스: `IF-API-SEND`, `IF-API-INBOX`, `IF-API-HISTORY`, `IF-API-RECEIVER`, `IF-API-DELIVERY`, `IF-RCV-WAKESET`(신설, wire `IF-RCV-BODY`는 그대로), `IF-DB-001`, `IF-DLV-001`, `IF-WIRE-001`, `IF-WIRE-004`, `IF-WIRE-010`, `IF-WIRE-012`, `IF-CFG-001`(`post_as`의 쓰임), `IF-CLI-001`.

## 재평가 트리거
배포 뒤 일주일의 조용한 사건 비율과 wake 감소량(조사 §9)이 기대보다 작으면, 발신측이 `wake`를 고르게 하는 방법(각 agent의 하네스 몫)을 사용자와 다시 본다 — `all` 기본값을 `false`로 바꾸는 것은 사용자 결정(조사 §7 13번)의 재검토다. post_as 외의 agent를 member 밖에서 지목하자는 요구가 나오면 그것은 주제 방이 필요하다는 신호다. `always` receiver가 늘어 Pi 부하가 보이면 delivery coalescing을 다시 잰다.
