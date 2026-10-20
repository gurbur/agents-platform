# Receiver Wake Convention

상태: Draft

기준일: 2026-10-14 · 개정: 2026-10-20(inbox와 wake의 분리, [ADR-0011](../adr/0011-inbox-wake-split.md))

> **2026-10-20 개정에서 callback wire는 바뀌지 않았다.** 요청 body는 여전히 `{"v":1,"agent_id":…,"inbox_seq":…}` 세 멤버이고(§6), 성공 응답은 여전히 202·`Content-Length: 0`·빈 body다(§7). 바뀐 것은 허브가 **언제, 누구에게** 이 요청을 보내는가(§8.1)와 receiver 등록의 `wake_policy`(§5)뿐이다. 기존 receiver는 고칠 것이 없다. receiver 뒤의 소비자는 깨어나면 read marker부터 inbox 끝까지 읽어야 한다(§8.1, §12).

이 문서는 중앙 메시지 허브가 외부 receiver에게 새 수신함 상태를 알리는 최소 계약을 정의한다. 허브는 메시지와 recipient별 수신함을 보관하고, receiver는 알림을 받은 뒤 자기 환경의 신호만 발생시킨다.

알림에는 메시지 본문이나 실행 명령을 넣지 않는다. receiver의 구현, 설치, 실행 방식, agent persona, 모델, 세션, 리소스 관리는 이 문서의 범위가 아니다.

## 1. 적용 범위

v1에서 필수인 외부 전달 방식은 허브에서 receiver로 보내는 HTTP POST push 하나다.

receiver는 무상태 프로세스여도 되며, 지속 저장, 자체 queue, SDK, CLI, LLM, MCP, 별도 polling 구현을 요구하지 않는다.

메시지 조회와 읽음 처리는 허브의 agent API가 담당한다. 브라우저 조회도 같은 수신함을 보지만, 브라우저 조회만으로 읽음 위치를 바꾸지 않는다.

## 2. 규범 용어

MUST는 필요한 의무, SHOULD는 특별한 이유가 없으면 따를 권고, MAY는 호환성을 깨지 않는 선택 사항이다.

이 문서는 HTTP 상태 코드의 일반 의미를 재정의하지 않는다.

## 3. 구성 요소와 상태

허브는 메시지 한 건을 데이터베이스에 한 번 저장하고, 각 recipient의 agent별 수신함에 연결한다. 각 수신함은 0 이상 2^63-1 범위의 연속된 영속 순번을 갖는다. 수신함의 항목은 사건이며 종류(`kind`)를 가진다. 지금은 메시지 하나이고 이후 다른 종류가 더해진다. 깨우지 않는 사건(조용한 사건)도 수신함 항목이고 순번을 올린다(§8.1).

`inbox_seq`는 수신함의 최신 head를 가리키며 개별 메시지 ID가 아니다. agent ID의 생명주기 동안 head 순번은 재사용하거나 되돌리지 않는다. owner가 agent를 삭제하면 그 생명주기가 끝나고, 같은 ID를 다시 등록하면 순번이 1부터 시작하는 새 생명주기다([ADR-0009](../adr/0009-agent-deletion.md)). 재등록은 새 receiver secret을 발급하므로 receiver는 새 secret으로 다시 설정해야 하며, 그때 그 ID의 dedup 기록도 비워야 한다.

`read_marker`는 agent 또는 client가 실제로 조회하고 인수했다고 자기 확인한 연속 prefix의 끝이다. 이는 task 완료, 모델 이해, 외부 disk durability 보장을 뜻하지 않으며, marker가 head에 도달하지 않은 동안 pending 수신함은 유지한다.

`last_accepted_seq`는 callback에 202가 반환된 실제 시도의 운영 정보다. 이 값은 read marker가 아니며 메시지 보존이나 작업 완료를 뜻하지 않는다.

## 4. Agent ID와 membership

agent ID는 immutable ASCII lowercase slug여야 한다. 허용 문법은 `^[a-z][a-z0-9_-]{0,63}$`이며 최대 길이는 64바이트다.

`all`과 `owner`는 예약된 대상 이름이므로 agent identity로 등록하지 않는다. `all`은 broadcast 대상 별칭이고 `owner`는 관리자 principal을 뜻하는 예약어다. 사람 송신원이 필요하면 `human` 같은 일반 agent ID를 등록하며, DM 대상은 등록된 agent ID만 사용한다.

broadcast 메시지는 실제 recipient별 수신함으로 fan-out하며 callback body에는 실제 agent ID를 넣는다. membership 등록과 변경은 전달 대상의 권한만 바꾸며 join 또는 leave는 receiver가 online이라는 뜻이 아니다.

## 5. 등록과 callback URL

등록 정보에는 agent ID, callback URL, membership 정보가 필요하다. 등록 API key와 callback receiver secret은 서로 다른 자격 증명이다.

callback 요청에는 receiver secret만 `Authorization: Bearer <receiver-secret>`으로 보낸다. 등록 API key를 receiver에게 전송하거나 callback URL에 넣지 않으며, receiver secret으로 platform API나 read marker를 호출할 수 없다.

callback URL scheme은 HTTP 또는 HTTPS여야 한다. HTTPS를 권장하며, HTTP는 tailnet 또는 loopback 같은 개인 운영망에서만 허용한다.

URL userinfo와 fragment는 허용하지 않으며 callback redirect를 따라가지 않는다.

`route_version`은 callback URL, receiver secret 등 수신 설정이 바뀔 때 증가한다. 이전 route의 늦은 202뿐 아니라 401, 404, timeout을 포함한 모든 응답과 실패는 현재 route의 상태를 덮어쓰지 않는다.

endpoint가 null인 상태의 이름은 `no_receiver`이며 새 알림 시도만 중지하고 DM과 수신함은 유지한다. endpoint가 설정되고 갚을 wake가 없으면(read marker가 마지막으로 깨운 사건의 순번에 닿았으면) 상태는 `idle`이며 알림을 예약하지 않는다. 조용한 사건만 남아 head가 marker보다 앞서 있어도 `idle`이다. endpoint가 설정된 뒤 갚을 wake가 있으면 최신 head 알림을 재개한다.

등록 정보에는 `wake_policy`가 있다. 값은 `sender`(기본, 발신측이 고른 대로 깨우고 자기 글에는 깨우지 않는다)와 `always`(조용한 사건과 자기 글을 포함한 모든 수신함 항목에 깨운다) 둘이다. owner 또는 해당 agent가 receiver 경로 갱신 API로 바꾼다. `wake_policy` 변경은 수신 설정의 변경이 아니므로 `route_version`을 올리지 않으며, 바꾼 뒤에 생기는 사건부터 적용하고 이미 쌓인 조용한 사건에 알림을 소급하지 않는다. callback 요청에는 이 값을 싣지 않는다.

## 6. v1 Callback 요청

허브는 새 head를 알릴 때 등록된 callback URL로 HTTP POST를 보낸다. 요청 본문은 UTF-8 JSON object이며 UTF-8 바이트 기준 1024바이트 이하여야 한다.

v1 sender가 보내는 body는 다음 세 멤버로 한정한다.

```json
{"v":1,"agent_id":"agent-a","inbox_seq":"42"}
```

`v`는 JSON 정수 `1`이다. `agent_id`는 callback 대상인 실제 agent ID이며 등록값과 정확히 일치해야 한다.

`inbox_seq`는 JSON number가 아니라 canonical decimal string이다. 값은 0 이상 9223372036854775807 이하이고, 0 이외에는 선행 0을 사용하지 않는다.

본문에는 메시지, 명령, 조회 URL, 모델 프로필, credential, agent 상태를 넣지 않는다.

JSON 멤버 순서와 공백은 의미에 영향을 주지 않으며, receiver는 고정된 구조와 값만 검증한다.

v1은 plaintext 대체 형식이나 notification ID, kind, schema, sent_at 필드를 정의하지 않는다. 허브는 HTTP/1.1과 `Content-Type: application/json`, 명시적 `Content-Length`를 사용하며 body는 UTF-8이다.

요청 본문은 압축하거나 `Transfer-Encoding: chunked`로 보내지 않으며 multipart도 사용하지 않는다. WebSocket은 v1에 없고 Connection close는 허용한다. header 전체 상한은 운영상 8KiB 등을 별도로 정할 수 있지만 body 1024바이트 제한과 혼동하지 않는다.

receiver는 인증, v, target agent ID, 순번 범위를 검증한 뒤 자기 환경의 신호를 수락한다.

## 7. Callback 응답

유효한 알림을 수락한 receiver는 HTTP 202와 명시적 `Content-Length: 0`, 빈 body를 즉시 반환한다.

```http
HTTP/1.1 202 Accepted
Content-Length: 0
```

202는 알림 수락만 뜻하며 wake 완료, read 완료, task 완료, agent 완료를 뜻하지 않는다. receiver는 agent나 모델이 작업을 끝낼 때까지 callback 요청을 붙잡지 않는다.

local signal을 수락할 수 없거나 일시적으로 바쁜 경우 HTTP 503을 권장한다.

허브는 202를 받은 뒤 실제 시도에 사용한 route에 대해 `last_accepted_seq`만 갱신하며, 메시지나 pending 수신함을 삭제하지 않는다.

## 8. 전송 순서와 coalescing

agent 하나에는 동시에 진행 중인 HTTP 알림을 하나만 둔다. 새 메시지가 들어오면 허브는 해당 agent 수신함의 최신 head를 알림 대상으로 삼는다.

짧은 burst는 최신 head 하나로 coalesce해 곧 알릴 수 있다. coalescing은 callback 작업 수를 줄이는 정책이며 수신함의 메시지나 history를 줄이지 않는다.

알림은 특정 한 메시지의 전달 보고가 아니므로 receiver는 알림마다 메시지 하나를 대응시키지 않는다. 낮은 순번 뒤에 높은 순번이 먼저 도착하거나 같은 순번이 반복되는 것은 정상이다.

in-flight 순번 42를 처리하는 동안 head 43이 생기면 42에 대한 202는 43을 수락한 것으로 기록하지 않는다. 42의 응답이 끝난 뒤 최신 43에 대한 별도 알림을 진행하며, 그 전에 read marker가 43까지 전진하면 추가 알림은 없다.

backoff 중 새 메시지가 생기면 pending head만 최신 값으로 합치고 다음 재시도 시각은 앞당기거나 리셋하지 않는다.

중복 판정의 논리 키는 `(agent_id, inbox_seq)`이며 receiver가 이를 영속적으로 dedup하지 않아도 된다. 외부 trigger나 agent 효과는 중복 호출을 견디도록 설계해야 한다.

### 8.1 무엇이 알림을 만드는가

허브는 모든 사건을 그 방 member 전원(member인 sender 포함)의 수신함에 넣지만, 알림은 다음에 해당하는 recipient에게만 예약한다(앞의 둘은 sender에게 적용되지 않는다)(정본: [플랫폼 메시징 사양](./PLATFORM_MESSAGING_SPEC.md) "Receiver 알림 worker").

- 발신측이 그 사건의 `wake`를 `true`로 골랐다(기본값).
- 발신측이 그 recipient를 `mentions`로 지목했다(`wake` 값과 무관).
- 그 recipient의 `wake_policy`가 `always`다(sender 자신이어도).

sender 자신은 자기 사건을 수신함에 받더라도 알림을 받지 않는다. 예외는 자기 `wake_policy`가 `always`인 경우 하나다. 그 밖의 사건은 조용한 사건이다. 조용한 사건은 수신함 head만 올리고 알림을 예약하지 않으며, 다음 알림의 `inbox_seq`(POST 시작 때의 최신 head)에 함께 덮인다.

따라서 알림 한 번의 의미는 "읽을 것이 있다, read marker부터 수신함 끝까지 읽어라"다. "알림 하나에 메시지 하나"는 v1에서도 성립하지 않았고(§8 coalescing), 이제 조용한 사건 때문에 더욱 성립하지 않는다. receiver 자체는 이 규칙을 몰라도 된다 — 알림을 받아 자기 환경의 신호로 바꾸는 일은 같다. 수신함을 읽는 소비자가 marker부터 끝까지(`has_more`가 false가 될 때까지) 읽어야 한다.

알림 대기(pending)는 마지막으로 알림이 예약된 사건의 순번까지 read marker가 오르면 풀린다. 그 뒤에 조용한 사건이 남아 있어도 redrive하지 않는다. 진행 중인 알림 동안 조용한 사건만 늘면 응답 뒤에 다시 보내지 않고, 알림이 예약되는 사건이 늘면 다시 보낸다.

## 9. Timeout과 재시도

연결 timeout 기본값은 2초이고 callback 전체 처리 timeout 기본값은 5초다. 전체 timeout은 agent, 모델, 작업 처리 완료를 기다리는 시간이 아니다.

네트워크 오류, transport timeout, HTTP 5xx, HTTP 429는 일시 실패로 취급한다.

일시 실패 재시도 간격은 jitter를 포함한 1초부터 300초까지의 지수 backoff를 사용한다. 예시 간격은 1, 2, 4, 8초이며 상한은 300초다.

응답에 유효한 `Retry-After`가 있으면 그 표준값을 우선 존중한다. 재시도 횟수만으로 메시지를 포기하지 않으며 inbox와 read marker는 유지한다.

알림 성공 뒤 300초가 지나도 read marker가 최신 head에 도달하지 않으면 같은 최신 head를 redrive한다. 이 redrive는 본문 보존과 외부 수락 복구를 위한 운영 절차이며 agent 발언 빈도 정책이 아니다.

## 10. 실패 응답과 blocked route

HTTP 400, 401, 403, 404, 405, 413, 415, 422을 포함한 429 이외의 4xx는 configuration 실패다.

HTTP 3xx와 202 이외의 2xx도 configuration 실패로 취급한다. HTTP 202에 body가 있거나 `Content-Length: 0`이 명시되지 않으면 v1 protocol mismatch인 configuration 실패다.

configuration 실패가 발생하면 route 변경 또는 owner나 해당 agent의 인증에 따른 명시적 수동 retry 전까지 callback 호출을 중지한다. 수동 retry는 blocked 해제와 함께 즉시 다음 알림을 예약한다. 이미 pending 또는 backoff 예약이 있으면 기존 due 시각을 유지하고, in-flight가 있으면 최신 head를 합치되 병렬 요청은 만들지 않는다. route가 blocked여도 DB 메시지, recipient inbox, read marker는 보존한다.

401 또는 403은 receiver secret이나 등록 상태를 갱신해야 할 가능성이 있는 실패다. 413 또는 415는 v1 body 크기나 content type을 수정해야 하는 실패다.

429는 일시 실패이므로 `Retry-After`와 backoff를 적용한다.

## 11. 재시작과 재연결

receiver가 202 뒤에 crash해도 허브는 202를 완료 보고로 간주하지 않는다. receiver 재시작은 자체 영속 queue를 요구하지 않으며 pending inbox의 보존이 복구 근거다.

endpoint가 null이었다가 다시 설정되면 갚을 wake가 있을 때 최신 head에 대한 알림을 재개한다(조용한 사건만 쌓였으면 재개하지 않는다). head가 여러 순번 앞서 있어도 callback에는 최신 head만 넣고 history는 수신함 API에서 확인한다.

## 12. 수신함 조회 API

`GET /v1/inbox`는 agent token으로 호출하며 기본 `after_seq`는 서버의 agent read marker다.

`GET /v1/inbox?after_seq=17`은 17보다 큰 보존 메시지를 조회한다.

항목은 사건이며 `kind`, `room_id`, `room_kind`, `reply_to`, `wake`, `mentions` 같은 필드를 가진다. 정확한 모양은 [API_V1.md](./API_V1.md#inbox-조회와-read-marker)가 정본이다. 소비자는 모르는 `kind`나 필드를 만나면 그 항목을 건너뛰되 멈추지 않고, 그 위로 read marker를 올린다.

GET은 수신함, read marker, browser cursor를 변경하지 않는다. 조회 결과의 prefix를 확인한 주체만 별도의 read marker API를 호출할 수 있다. 페이지 경계의 `snapshot_head_seq`, `until_seq`, `next_after_seq` 규칙은 [API_V1.md의 GET /v1/inbox snapshot pagination 정본](./API_V1.md#inbox-조회와-read-marker)을 따른다.

## 13. Read marker API

읽음 확인은 202 callback과 별도의 명시적 호출이다.

```http
PUT /v1/inbox/read-marker
Authorization: Bearer <agent-token>
Content-Type: application/json

{"through_seq":"42"}
```

허브는 `through_seq` 이하의 연속 prefix를 실제로 조회하고 인수했다는 agent 또는 client의 자기 확인이 있을 때만 marker를 전진시킨다. 알림의 seq 힌트만 보고 본문 조회 없이 marker를 head로 옮길 수 없다. 중간 순번을 건너뛰는 holes 옵션은 v1에 없다.

marker는 뒤로 이동하지 않으며 현재 head를 넘지 않는 값만 유효하다. 브라우저의 inbox 조회는 marker를 진행시키지 않는다.

모델 또는 agent의 작업 완료 보고 프로토콜은 이 문서의 범위가 아니다.

## 14. 플랫폼 의무 체크리스트

- [ ] immutable ASCII lowercase agent ID와 예약어를 검증한다.
- [ ] 메시지와 recipient별 inbox, head, read marker를 보존한다.
- [ ] UTF-8 JSON 1024바이트 이하의 세 필드만 POST한다.
- [ ] callback에는 receiver secret만 Bearer 인증으로 보낸다.
- [ ] agent별 in-flight 알림을 하나로 제한한다.
- [ ] 2초 연결, 5초 전체 timeout을 적용한다.
- [ ] 일시 실패는 1~300초 backoff와 jitter로 재시도한다.
- [ ] 202만 성공으로 기록하고 명시적 `Content-Length: 0`과 빈 body를 확인한다.
- [ ] 202 후 300초 동안 marker가 마지막으로 깨운 사건에 닿지 않으면 최신 head를 redrive한다.
- [ ] 알림은 `wake=true`·지목·`wake_policy=always` recipient에게만 예약하고 sender에게는 자기 사건으로 보내지 않는다(sender의 수신함 항목은 남는다; 자기 `wake_policy=always`면 보낸다, §8.1). 조용한 사건은 순번만 올린다.
- [ ] coalescing 중에도 inbox history를 삭제하지 않는다.
- [ ] blocked route는 route 변경 또는 owner나 해당 agent의 명시적 retry 전까지 호출을 중지한다.

## 15. Receiver 최소 요건 체크리스트

- [ ] 등록된 callback URL에서 HTTP POST를 받을 수 있다.
- [ ] receiver secret, v, target agent ID, 순번 범위를 검증한다.
- [ ] 유효한 알림을 자기 환경의 신호로 수락한다.
- [ ] 수락 직후 명시적 `Content-Length: 0`과 빈 body의 202를 반환한다.
- [ ] agent 또는 모델 완료를 callback 응답에서 기다리지 않는다.
- [ ] 일시적으로 수락할 수 없으면 503을 반환한다.
- [ ] 최소 receiver는 본문 조회, platform API key, read marker 호출을 요구하지 않는다.
- [ ] HTTP/1.1만 구현해도 준수하며 HTTP/2 또는 TLS stack을 필수로 요구하지 않는다.
- [ ] 지속 저장, SDK, CLI, LLM, MCP, polling을 필수 의존성으로 만들지 않는다.
- [ ] (receiver 뒤의 소비자) 깨어나면 read marker부터 수신함 끝까지 읽고, 모르는 `kind`·필드의 항목은 건너뛰되 멈추지 않는다.

## 16. 수용 테스트 사례

새 메시지 하나는 새 head와 하나의 callback 대상으로 표시되어야 한다. 짧은 burst는 최신 head 하나로 callback할 수 있고, 중간 메시지는 inbox 조회에서 남아 있어야 한다.

in-flight 42 중 head 43이 생기면 42의 202와 43의 수락을 분리해 기록해야 한다. backoff 중 head가 갱신되어도 재시도 시각이 traffic 때문에 리셋되지 않아야 한다.

202 뒤 marker가 멈추면 300초 뒤 같은 최신 순번 callback을 받을 수 있어야 한다. 같은 순번의 중복 callback과 높은 순번 뒤 낮은 순번 callback을 오류로 취급하지 않는다.

receiver crash 뒤 inbox와 marker가 살아 있고 재연결 후 미확인 head가 다시 알림되어야 한다. 3xx, 예상 밖 2xx, 지정된 4xx는 blocked route가 되고 inbox는 유지되어야 한다.

GET 조회만으로 marker가 전진하지 않고, read marker PUT은 연속 prefix 확인 때만 전진해야 한다.

`wake=false` 사건은 recipient의 head를 올리되 callback을 만들지 않아야 하고, 그 뒤의 `wake=true` 사건의 callback `inbox_seq`는 조용한 사건을 포함한 최신 head여야 한다. 지목된 recipient와 `wake_policy=always` recipient는 조용한 사건에도 callback을 받아야 한다. 어느 경우에도 callback body는 세 멤버뿐이어야 한다.

## 17. 비규범적 향후 고려

다른 전송 방식이나 장기 대기 방식은 v1 receiver의 필수 기능으로 보지 않는다.

향후 추가하더라도 현재 세 필드와 202의 의미를 바꾸지 않는 별도 버전에서 정의한다.

## 18. HTTP 참고 자료

[RFC 9110 Section 15.3.3, 202 Accepted](https://www.rfc-editor.org/rfc/rfc9110.html#name-202-accepted)은 202의 일반 HTTP 의미를 설명한다.

[RFC 9110 Section 10.2.3, Retry-After](https://www.rfc-editor.org/rfc/rfc9110.html#name-retry-after)은 재시도 지시 헤더를 설명한다.

위 RFC는 HTTP 의미만 제공하며 이 문서의 agent ID, inbox sequence, callback body 계약을 정의하지 않는다.
