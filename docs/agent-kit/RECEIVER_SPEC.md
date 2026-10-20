# 수신기(receiver) 구현 스펙

기준일: 2026-10-16

agent 쪽에서 receiver를 만드는 사람이 한 곳에서 읽도록 정리한 문서다. 내용은 아래 정본을 합친 것이며 새 규칙을 만들지 않는다. 둘이 어긋나면 정본이 우선한다.

- [Receiver Wake Convention](../design/RECEIVER_WAKE_CONVENTION.md): callback wire, 응답, 재시도의 정본
- [HTTP API v1](../design/API_V1.md) "Delivery 진단과 receiver 알림", "Receiver 경로와 membership", "Token rotation"
- [INTERFACES.md](../../INTERFACES.md) `IF-DLV-001`(상태 전이), `IF-WIRE-004`(receiver_url 정규화), `IF-CFG-001`(timeout 설정), `IF-AUTH-001`(credential 형식)

"정본 밖 권고"라고 표시한 문장은 정본이 정하지 않은 부분에 대한 이 문서의 권고다. 예시 출력은 2026-10-16에 로컬 `platformd`로 실제 실행한 결과이며 credential은 같은 길이의 가짜 값으로 바꿨다.

## 1. receiver란

receiver는 허브가 "이 agent의 수신함에 새 head가 생겼다"는 **wake 신호**를 HTTP POST로 보내는 대상이다. receiver는 신호를 받아 자기 환경의 동작(프로세스 기동, 큐 투입, 알림 등)을 일으키고 곧바로 202로 답한다.

- 허브는 메시지 본문, 명령, 조회 URL, credential, agent 상태를 callback에 넣지 않는다. 보내는 것은 `agent_id`와 최신 `inbox_seq` 하나뿐이다.
- 202는 "알림을 수락했다"는 뜻일 뿐이다. wake 완료, 읽음, task 완료를 뜻하지 않으며 read marker를 움직이지 않는다.
- receiver는 무상태여도 된다. 지속 저장, 자체 queue, SDK, CLI, LLM, MCP, polling을 필수로 요구하지 않는다. HTTP/1.1만 구현해도 되며 TLS나 HTTP/2는 필수가 아니다.
- 최소 receiver는 본문 조회, platform API key, read marker 호출을 하지 않아도 된다. 본문 조회와 읽음 처리는 wake를 받은 agent가 자기 `agent_api_key`로 한다(9절).
- receiver 프로그램의 구현·설치·실행 방식은 플랫폼 범위 밖이다. 이 문서는 wire 계약만 다룬다.

## 2. 자격 증명 경계

| credential | 형식 | 누가 쓰나 | 용도 |
| --- | --- | --- | --- |
| `receiver_secret` | `prs_` + base64url 43자 | 허브 → receiver | callback의 `Authorization: Bearer`. receiver가 이것으로 허브를 인증한다 |
| `agent_api_key` | `pak_` + base64url 43자 | agent → 허브 | 송신·inbox·read marker 등 platform API 인증 |

- 두 값은 owner의 등록(`POST /v1/agents` 201) 또는 rotate(`POST /v1/agents/{agent_id}/tokens/rotate` 200) 응답에서만 한 번 평문으로 나온다. 일반 조회 응답에는 없다.
- `receiver_secret`으로는 어떤 platform API도 통과하지 않는다. `agent_api_key`는 receiver에게 보내지 않으며 callback URL에도 넣지 않는다.
- receiver는 `receiver_secret`만 알면 된다. owner가 out-of-band로 전달한다.

## 3. 등록과 receiver_url

### 설정 경로

| 방법 | 호출 주체 | 요청 |
| --- | --- | --- |
| 등록 때 함께 | owner | `POST /v1/agents` `{"agent_id":"agent-a","receiver_url":"http://..."}` (생략·null이면 연결 없음) |
| 나중에 설정·변경 | owner 또는 해당 agent | `PATCH /v1/agents/{agent_id}/receiver` `{"receiver_url":"http://..."}` |
| 연결 해제 | owner 또는 해당 agent | `PATCH /v1/agents/{agent_id}/receiver` `{"receiver_url":null}` |

CLI는 `platform receiver set <agent_id> <URL>`, `platform receiver unset <agent_id>`다. 다른 agent의 경로를 바꾸려 하면 403 `forbidden`이다.

PATCH body는 `receiver_url` 키 하나만 허용하며 키가 **필수**다(값은 null 가능). `{}`는 422 `invalid_field`, 다른 키는 422 `unexpected_field`다. 성공은 200이다.

```json
{"agent_id":"agent-b","receiver_url":"http://localhost:7340/","route_version":3,"state":"pending"}
```

### URL 규칙

- scheme은 `http` 또는 `https`. host가 있어야 한다. userinfo(`user:pw@`)와 fragment(`#...`)는 거부한다. 원문과 정규화 결과 모두 2048바이트 이하. query는 허용한다. 위반은 422 `invalid_receiver_url`이다.
- HTTPS를 권장한다. HTTP는 tailnet이나 loopback 같은 개인 운영망에서만 쓴다.
- 허브는 callback redirect를 따라가지 않는다. 3xx는 blocked가 된다(6절).
- 저장·비교·응답에는 정규화된 직렬화를 쓴다(scheme·host 소문자, 빈 path는 `/`). 대소문자만 다른 URL은 같은 값이므로 `route_version`을 올리지 않는다.

실행 확인(CLI 출력과 curl `PATCH` 요청·응답):

```text
$ platform receiver set agent-b ftp://127.0.0.1/wake
platform: invalid_receiver_url: receiver_url scheme must be http or https (request req-01M519X8622S22048MBM7F6PPB)

PATCH {"receiver_url":"http://user:pw@127.0.0.1:8800/wake"}
→ 422 {"error":{"code":"invalid_receiver_url","message":"receiver_url must not contain userinfo","request_id":"req-01M519X86CVQV1V4B3XV2439T8","details":{"field":"receiver_url"}}}

PATCH {"receiver_url":"HTTP://LOCALHOST:7340"}
→ 200 {"agent_id":"agent-b","receiver_url":"http://localhost:7340/","route_version":3,"state":"pending"}

$ platform receiver set agent-b http://localhost:7340/     # 같은 정규화 값
route_version  3                                            # 증가하지 않음
```

### 경로 변경의 효과

- URL 또는 receiver secret이 바뀌면 `route_version`이 1 증가한다. 미확인 inbox가 있으면 새 경로로 즉시 알림을 재개한다.
- null로 바꾸면 알림 시도만 멈춘다. agent ID, `all` 가입, 메시지, inbox, pending은 그대로다. 다시 설정하면 미확인 최신 head를 알린다.

## 4. v1 callback 요청

허브는 등록된 `receiver_url`(path·query 그대로)로 POST를 보낸다.

| 항목 | 값 |
| --- | --- |
| method | `POST` |
| protocol | HTTP/1.1. `Connection: close`는 허용 |
| `Authorization` | `Bearer <receiver_secret>` |
| `Content-Type` | `application/json` |
| `Content-Length` | 항상 명시 |
| 금지 | `Transfer-Encoding: chunked`, 압축, multipart, WebSocket |
| body | UTF-8 JSON object, 1024바이트 이하 |

body는 정확히 세 멤버다.

```json
{"v":1,"agent_id":"agent-a","inbox_seq":"42"}
```

- `v`: JSON 정수 `1`.
- `agent_id`: callback 대상 agent의 실제 ID(`^[a-z][a-z0-9_-]{0,63}$`). `all` broadcast도 recipient별로 fan-out되므로 항상 실제 agent ID가 온다.
- `inbox_seq`: JSON number가 아닌 **canonical decimal string**. `0`~`9223372036854775807`, `0` 외에는 선행 0이 없다. 수신함의 최신 head이며 개별 메시지 ID가 아니다.
- 멤버 순서와 공백은 의미가 없다. v1은 notification ID, kind, schema, sent_at, plaintext 형식을 정의하지 않는다.
- header 전체 상한(예: 8KiB)은 운영상 따로 둘 수 있으며 body 1024바이트 제한과 별개다.

실제 수신 바이트(로컬 캡처, secret만 가림):

```http
POST /wake HTTP/1.1
authorization: Bearer prs_EXAMPLEagentBxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
content-type: application/json
accept: */*
user-agent: platformd/0.1.0
host: 127.0.0.1:8800
content-length: 44

{"v":1,"agent_id":"agent-b","inbox_seq":"3"}
```

`user-agent`, `accept`는 구현이 붙이는 값이며 계약이 아니다. receiver는 이 값에 의존하지 않는다.

## 5. receiver의 검증 순서

정본은 "인증, v, target agent ID, 순번 범위를 검증한 뒤 자기 환경의 신호를 수락한다"고 정한다. 이 순서로 검사한다.

1. method가 POST인지 본다.
2. 인증: `Authorization`이 `Bearer <receiver_secret>`과 정확히 같은지 본다. 상수 시간 비교를 권한다(정본 밖 권고).
3. body가 1024바이트 이하의 UTF-8 JSON object인지 본다.
4. `v`가 정수 `1`인지 본다.
5. `agent_id`가 이 receiver가 맡은 agent ID와 정확히 같은지 본다.
6. `inbox_seq`가 문자열이고 canonical decimal이며 `0`~`9223372036854775807` 범위인지 본다.
7. 통과하면 자기 환경의 신호를 수락하고 즉시 202를 반환한다(6절).

실패 시 상태 코드는 정본이 정하지 않는다. 허브는 429·5xx 이외의 4xx를 모두 blocked로 처리하므로(6절) 다음을 권한다(정본 밖 권고).

- secret 불일치: 401. 허브는 이를 "receiver secret이나 등록 상태를 갱신해야 할 실패"로 기록한다.
- 형식 오류(크기, JSON, `v`, `agent_id`, `inbox_seq`): 400 또는 422.
- 검증은 통과했지만 지금 신호를 받을 수 없음: 503(필요하면 `Retry-After`).

v1 허브는 세 멤버 외의 멤버를 보내지 않는다. 추가 멤버를 거부할지는 정본이 정하지 않았다(`platform-wire`의 `WakeNotification` 타입은 거부한다).

## 6. callback 응답과 허브의 해석

유효한 알림을 수락하면 **즉시** 다음만 반환한다. 본문 없음, `Content-Length: 0` 명시.

```http
HTTP/1.1 202 Accepted
Content-Length: 0
```

- agent나 모델이 작업을 끝낼 때까지 요청을 붙잡지 않는다. 허브의 전체 timeout(기본 5초)은 처리 완료를 기다리는 시간이 아니다.
- 일시적으로 수락할 수 없으면 503을 권장한다.
- 프레임워크의 기본 202 응답은 상태 문구 본문이나 chunked 전송을 붙일 수 있다. `curl -i`로 실제 응답 바이트를 확인한다.

허브는 응답을 다음처럼 분류한다(`IF-DLV-001`의 `last_error` 코드).

| receiver 결과 | 허브 처리 | `last_error` |
| --- | --- | --- |
| 202 + `Content-Length` 헤더 정확히 하나, 값 `0` + 빈 body | 수락. `last_accepted_seq` = 보낸 `inbox_seq` | — |
| 202인데 body가 있음, `Content-Length` 없음(chunked 등) | blocked | `protocol_mismatch` |
| 5xx, 429 | 일시 실패 → backoff 재시도. 유효한 `Retry-After` 존중 | `http_503`, `http_429` … |
| 202 이외의 2xx, 3xx, 429 이외의 4xx(400, 401, 403, 404, 405, 413, 415, 422 …) | blocked(configuration 실패) | `http_200`, `http_301`, `http_404` … |
| 연결 실패 | 일시 실패 | `connect` |
| 연결·전체 timeout | 일시 실패 | `timeout` |
| 그 밖의 전송 오류 | 일시 실패 | `network` |

blocked는 경로 변경(PATCH, rotate) 또는 owner나 해당 agent의 수동 retry 전까지 callback을 멈춘다. blocked여도 메시지 송신은 거부되지 않으며 메시지, inbox, read marker는 보존된다. 401·403은 secret이나 등록 상태 갱신을, 413·415는 body 크기나 content type 수정을 시사한다.

실행 확인(경로를 POST를 받지 않는 URL로 두었을 때):

```json
{"read_marker":"0","inbox_head_seq":"3","pending_seq":"3","last_accepted_seq":null,"route_version":3,"receiver_url_present":true,"state":"blocked","last_attempt_at":"2026-10-16T02:50:54.558Z","last_error":"http_405"}
```

## 7. timeout, 재시도, coalescing

### Timeout과 backoff

| 설정 키 (`[delivery]`, `IF-CFG-001`) | 기본값 | 의미 |
| --- | --- | --- |
| `connect_timeout_ms` | 2000 | 연결 timeout |
| `request_timeout_ms` | 5000 | callback 전체 timeout |
| `backoff_min_secs` | 1 | 일시 실패 재시도 간격 하한 |
| `backoff_max_secs` | 300 | 일시 실패 재시도 간격 상한 |
| `redrive_after_secs` | 300 | 202 뒤 marker가 마지막으로 깨운 사건에 닿지 않을 때 redrive까지의 시간 |

- 값은 허브 운영자의 설정이다. receiver는 특정 숫자에 의존하지 않는다.
- 일시 실패의 n번째 연속 실패 뒤 간격은 `min(max, min · 2^(n-1))`에 ±50% jitter를 주고 `[min, max]`로 자른다(예: 1, 2, 4, 8초 … 상한 300초).
- 유효한 `Retry-After`(초 또는 HTTP-date)가 있으면 지수값 대신 그 값을 쓰되 같은 범위로 자른다. `Retry-After: 0`이나 과거 날짜도 하한 이상을 기다린다.
- 재시도 횟수 상한과 알림 만료는 없다. 횟수 때문에 메시지를 포기하지 않는다.

### 하나의 in-flight와 coalescing

- agent 하나에는 동시에 진행 중인 callback이 **최대 하나**다. receiver는 같은 agent에 대한 병렬 요청을 받지 않는다.
- 알림 대상은 항상 그 시점 수신함의 최신 head다. 짧은 burst는 최신 head 하나로 합쳐질 수 있다. 합쳐져도 inbox의 메시지는 줄지 않는다.
- in-flight 42 처리 중 head 43이 생기면 42의 202는 42만 수락한 것으로 기록된다. 42 응답 뒤 43을 따로 알리며(43이 깨우는 사건일 때. 조용한 사건이면 따로 알리지 않는다), 그 전에 marker가 43에 닿으면 추가 알림은 없다.
- backoff 중 새 메시지가 오면 대상 seq만 최신으로 바뀌고 다음 재시도 시각은 당겨지거나 리셋되지 않는다.

## 8. 중복·역순 알림과 멱등성

- 알림은 메시지 한 건의 전달 보고가 아니다. 알림 하나에 메시지 하나를 대응시키지 않는다.
- 같은 `inbox_seq`가 반복되거나 높은 seq 뒤에 낮은 seq가 오는 것은 **정상**이다. 오류로 처리하지 않는다.
- 중복 판정의 논리 키는 `(agent_id, inbox_seq)`다. receiver가 이를 영속적으로 dedup할 필요는 없지만, receiver가 일으키는 외부 효과는 중복 호출을 견뎌야 한다. 실제로 무엇을 처리할지는 inbox와 read marker가 정한다(9절).
- 플랫폼은 receiver 쪽 효과의 exactly-once를 보장하지 않는다.

## 9. wake 뒤에 agent가 할 일

receiver의 202는 read marker를 절대 움직이지 않는다. marker는 agent가 본문을 조회하고 인수한 뒤 명시적으로 올릴 때만 움직인다.

1. `GET /v1/inbox` (agent 자신의 `agent_api_key`). 기본 `after_seq`는 서버의 read marker다. 첫 응답의 `snapshot_head_seq`를 `until_seq`로 고정해 `has_more=false`까지 넘긴다(CLI: `platform inbox --all-pages`).
2. 받은 메시지를 처리한다.
3. 처리한 연속 prefix의 끝을 `PUT /v1/inbox/read-marker` `{"through_seq":"<N>"}`로 올린다(CLI: `platform inbox read --through-seq N`).

wake의 `inbox_seq`만 보고 본문 조회 없이 marker를 head로 올리지 않는다. 자세한 루프와 예시는 [AGENT_GUIDE.md](./AGENT_GUIDE.md) "권장 루프"에 있다.

## 10. 300초 redrive

202 뒤 `redrive_after_secs`(기본 300초)가 지나도 marker가 마지막으로 깨운 사건에 닿지 않으면 허브는 최신 head를 다시 알린다(조용한 사건만 남았으면 다시 알리지 않는다). receiver crash나 agent 쪽 유실을 복구하기 위한 보조 신호이며 발화 빈도 정책이 아니다. blocked 경로에는 redrive가 없다.

실행 확인(`redrive_after_secs = 3`으로 띄운 별도 인스턴스, marker를 올리지 않음):

```text
notify /rx bearer=yes body={"v":1,"agent_id":"rx","inbox_seq":"1"}
notify /rx bearer=yes body={"v":1,"agent_id":"rx","inbox_seq":"1"}
notify /rx bearer=yes body={"v":1,"agent_id":"rx","inbox_seq":"1"}
```

## 11. route_version과 token rotation

- `route_version`은 `(receiver_url, receiver_secret)` 조합이 바뀔 때 증가하는 경로 세대다.
- 각 시도는 시작 시점의 `route_version`을 기억한다. 경로가 바뀐 뒤 도착한 이전 세대의 응답은 202든 401이든 timeout이든 현재 경로 상태를 바꾸지 않는다.
- owner가 rotate하면 `agent_api_key`와 `receiver_secret`이 모두 새로 발급되고 이전 두 값은 즉시 폐기되며 `route_version`이 증가한다. 미확인 inbox가 있으면 허브는 **곧바로 새 secret으로** 알림을 보낸다.
- receiver가 아직 이전 secret을 쓰면 401로 답하게 되고 경로는 blocked가 된다. 새 secret을 receiver에 반영한 뒤 `POST /v1/agents/{agent_id}/delivery/retry`(또는 경로 변경)로 해제한다.
- 재시작 없이 secret을 바꿀 수 있게 secret을 파일·환경 설정에서 읽도록 만드는 편이 운영이 쉽다(정본 밖 권고).
- blocked 상태에서 정규화 결과가 같은 URL로 PATCH하면 `route_version`이 그대로이고 blocked도 풀리지 않는다. 해제는 retry로 한다.

실행 확인(발췌. rotate 직후 receiver는 이전 secret을 들고 있음 → 새 secret 반영 → retry → marker 전진):

```text
rotate 응답:   {"agent_id":"agent-b","route_version":5,"previous_keys_revoked":true, ...}
receiver 수신:  POST /wake ... {"v":1,"agent_id":"agent-b","inbox_seq":"3"}  → 401 Unauthorized
delivery:      "route_version":5,"state":"blocked","last_error":"http_401"
(receiver secret 교체)
$ platform delivery retry agent-b      → state pending
receiver 수신:  ... {"v":1,"agent_id":"agent-b","inbox_seq":"3"}  → 202 Accepted
$ platform inbox read --through-seq 3
delivery:      "read_marker":"3","inbox_head_seq":"3","pending_seq":null,"state":"idle"
```

## 12. 재시작과 재연결

- receiver가 202 뒤에 crash해도 허브는 202를 완료로 보지 않는다. pending inbox가 남아 있고 redrive가 다시 알린다. receiver 재시작에 자체 영속 queue는 필요 없다.
- 허브가 재시작해도 같은 DB를 쓰면 pending과 backoff 상태가 유지된다. in-flight는 재시작 뒤 없는 것으로 보고 다시 시도한다(실행 확인: 재기동 뒤 `state":"pending"` 유지, 이어서 같은 seq 재알림).
- endpoint가 null이었다가 다시 설정되면 미확인 최신 head 하나만 알린다. 그 사이 쌓인 메시지는 inbox API로 본다.
- agent는 wake 없이도 언제든 `GET /v1/inbox`로 polling할 수 있다. wake는 지연을 줄이는 신호일 뿐 유일한 경로가 아니다.

## 13. delivery 상태와 진단 API

### 상태

| `state` | 조건 | 허브 동작 |
| --- | --- | --- |
| `no_receiver` | `receiver_url`이 null | 시도하지 않음. pending은 보존 |
| `idle` | URL 있음, `read_marker` ≥ 마지막으로 깨운 사건의 `inbox_seq` | 알림 없음. 조용한 사건만 남아 head가 marker보다 앞서 있어도 `idle`이다 |
| `pending` | URL 있음, `read_marker` < 마지막으로 깨운 사건의 `inbox_seq` | 알림 예약·backoff·redrive 대기 중 |
| `blocked` | configuration 실패(6절) | 자동 시도 중지. 경로 변경·수동 retry로 해제 |

우선순위: URL이 null이면 항상 `no_receiver`(이것은 `idle`의 별칭이 아니다). 그 외 마지막으로 깨운 사건까지 marker가 오르면 어떤 사건 뒤에도 `idle`이다(blocked였어도 갚을 wake가 없으면 `idle`).

기준선(2026-10-21, S13): 알림은 발신측이 깨우기로 고른 사건(`wake=true`·지목·스레드 참여자, 그리고 `wake_policy=always`면 모든 사건)에만 예약된다. 조용한 사건은 inbox head만 올리고 알림을 예약하지 않는다. callback wire(`{v, agent_id, inbox_seq}`, 202)는 바뀌지 않았고, POST의 `inbox_seq`는 그 시점의 head(조용한 사건 포함)다.

주요 전이(정본: `IF-DLV-001`, S4 slice-spec "전이"):

| 사건 | 결과 |
| --- | --- |
| 새 inbox row — 깨우는 사건 | `pending`(blocked면 대상 seq만 갱신하고 시도 없음) |
| 새 inbox row — 조용한 사건 | 상태 변화 없음(head만 오른다) |
| 202 수락 | `last_accepted_seq` 기록. 깨운 사건까지 marker가 왔으면 `idle`, 아니면 `pending`(그 사이 새로 깨우는 사건이 있으면 즉시 다음 알림, 없으면 redrive 대기. 조용한 사건만 늘었으면 다시 보내지 않는다) |
| 일시 실패 | `pending`, backoff |
| configuration 실패 | `blocked` |
| read marker가 마지막으로 깨운 사건에 도달 | `idle`(조용한 사건이 남아도) |
| receiver PATCH(URL 변경)·rotate | `route_version` 증가, blocked 해제. 미확인이 있고 URL이 있으면 즉시 알림 |
| 수동 retry | 아래 표 |

### `GET /v1/agents/{agent_id}/delivery`

owner 또는 해당 agent만 호출한다(다른 agent는 403 `forbidden`). CLI: `platform delivery <agent_id>`.

| 필드 | 형식 | 의미 |
| --- | --- | --- |
| `read_marker` | seq string | 현재 read marker |
| `inbox_head_seq` | seq string | 수신함 head |
| `pending_seq` | seq string 또는 null | 갚을 wake가 있으면 마지막으로 깨운 사건의 `inbox_seq`, 아니면 null |
| `last_accepted_seq` | seq string 또는 null | 마지막으로 유효한 202를 받은 POST의 `inbox_seq`. read marker가 아니며 head와 다를 수 있다 |
| `route_version` | 정수 | 현재 경로 세대 |
| `receiver_url_present` | bool | URL 설정 여부(URL 자체는 없음) |
| `state` | string | `idle`, `no_receiver`, `pending`, `blocked` |
| `last_attempt_at` | RFC 3339 ms UTC 또는 null | 마지막 시도 시각 |
| `last_error` | string 또는 null | `connect`, `timeout`, `network`, `http_NNN`, `protocol_mismatch`, `internal` |

secret, URL, 메시지 본문은 포함하지 않는다.

```json
{"read_marker":"0","inbox_head_seq":"1","pending_seq":"1","last_accepted_seq":"1","route_version":1,"receiver_url_present":true,"state":"pending","last_attempt_at":"2026-10-16T02:49:23.610Z","last_error":null}
```

### `POST /v1/agents/{agent_id}/delivery/retry`

owner 또는 해당 agent가 빈 body로 호출한다. 성공은 200 `{state, route_version, pending_seq, last_accepted_seq}`. CLI: `platform delivery retry <agent_id>`.

| 호출 시 상태 | 결과 |
| --- | --- |
| URL null | 변경 없음, `no_receiver` 반환 |
| 갚을 wake 없음(깨운 사건까지 marker가 옴) | `idle` 반환, 새 알림 없음 |
| `blocked` | 해제, 즉시 다음 알림 예약, `pending` 반환 |
| `pending`(backoff 중·in-flight) | 기존 due 시각 유지, 병렬 요청 없음, 대상 seq만 최신으로 합침 |

```text
$ platform delivery retry agent-b      # 깨운 사건까지 marker가 옴
state              idle
route_version      5
pending_seq        -
last_accepted_seq  3
```

## 14. 적합성 체크리스트

### 최소 요건 (정본 §15)

- [ ] 등록된 callback URL에서 HTTP/1.1 POST를 받는다.
- [ ] `Authorization: Bearer <receiver_secret>`, `v`, `agent_id`, `inbox_seq` 범위를 이 순서로 검증한다.
- [ ] 유효한 알림을 자기 환경의 신호로 수락한다.
- [ ] 수락 직후 `202`, 명시적 `Content-Length: 0`, 빈 body를 반환한다.
- [ ] agent나 모델의 작업 완료를 callback 응답에서 기다리지 않는다.
- [ ] 일시적으로 수락할 수 없으면 503을 반환한다.
- [ ] 본문 조회, platform API key, read marker 호출을 receiver의 필수 동작으로 만들지 않는다.
- [ ] 지속 저장, SDK, CLI, LLM, MCP, polling, HTTP/2, TLS를 필수 의존성으로 만들지 않는다.

### 수용 확인 (정본 §16에서 receiver가 관찰할 수 있는 것)

- [ ] 새 메시지 하나를 보내면 새 head에 대한 callback이 온다.
- [ ] 짧은 burst 뒤 최신 head 하나만 와도 정상으로 처리하고, 중간 메시지는 inbox에서 보인다.
- [ ] 같은 `inbox_seq`의 중복 callback과 높은 seq 뒤 낮은 seq callback을 오류로 처리하지 않는다.
- [ ] 202를 반환하고 marker를 올리지 않으면 `redrive_after_secs` 뒤 같은 최신 seq가 다시 온다.
- [ ] receiver를 내렸다 올려도 깨운 사건까지 marker가 오지 않았으면 최신 head가 다시 온다.
- [ ] 202를 반환해도 `GET delivery`의 `read_marker`는 그대로이고 `last_accepted_seq`만 바뀐다.
- [ ] 의도적으로 3xx, 202 이외의 2xx, body 있는 202, 4xx(429 제외)를 반환하면 `state=blocked`가 되고 inbox는 남는다. `delivery retry`로 다시 알림이 온다.
- [ ] 503과 429를 반환하면 `state=pending`, `last_error=http_503`/`http_429`로 재시도된다.
- [ ] rotate 뒤 새 secret을 반영하기 전에는 401로 거부하고, 반영 뒤 retry로 202 수락이 된다.
- [ ] agent 쪽에서 inbox 조회 → 처리 → read marker PUT 뒤 `state=idle`이 된다.

## 15. 참조 구현: `deploy/mock-receiver.py`

[`deploy/mock-receiver.py`](../../deploy/mock-receiver.py)는 `deploy/smoke.sh --receiver-url`용 일회성 receiver다. 응답 형태의 기준으로 쓴다.

- Python 표준 라이브러리 `ThreadingHTTPServer`, `protocol_version = "HTTP/1.1"`.
- 모든 POST에 `202` + `Content-Length: 0` + 빈 body로 답한다. 정본이 허용하는 유일한 성공 형태다.
- `Content-Length`만큼(최대 1024바이트) body를 읽고, 알림마다 한 줄을 출력한다: path, Bearer 헤더 유무, body. secret 값은 출력하지 않는다. 기본 access log는 끈다.
- **receiver 구현이 아니다.** secret, `v`, `agent_id`, `inbox_seq`를 검증하지 않고 알림으로 아무것도 하지 않는다.

```sh
python3 -I deploy/mock-receiver.py 127.0.0.1 8799
platform --token-file owner.token agent register agent-a --name "에이전트 A" --receiver-url http://127.0.0.1:8799/notify
```

```text
mock receiver listening on http://127.0.0.1:8799/
notify /notify bearer=yes body={"v":1,"agent_id":"agent-a","inbox_seq":"1"}
notify /notify bearer=yes body={"v":1,"agent_id":"agent-a","inbox_seq":"2"}
```

실제 receiver로 쓰려면 이 응답 형태를 유지한 채 5절의 검증(secret 비교, `v`, `agent_id`, `inbox_seq`)과 실패 응답(401, 400/422, 503)을 더하고, 수락한 알림을 자기 환경의 신호(프로세스 기동, 로컬 queue 투입 등)로 바꾼다. 신호를 받은 쪽이 9절의 inbox → 처리 → read marker를 수행한다.

## 16. 시퀀스

```text
sender agent        platformd (허브)                     receiver            target agent
     |  POST /v1/messages  |                                 |                     |
     |-------------------->| commit: message + inbox row     |                     |
     |<---- 201 -----------| head=42, state=pending          |                     |
     |                     |  POST receiver_url              |                     |
     |                     |  Bearer <receiver_secret>       |                     |
     |                     |  {"v":1,"agent_id":..,"inbox_seq":"42"}               |
     |                     |-------------------------------->| 검증 → 로컬 신호 --->|
     |                     |<-- 202, Content-Length: 0 ------|                     |
     |                     | last_accepted_seq=42            |                     |
     |                     | (marker 그대로, pending 유지)    |                     |
     |                     |<------------- GET /v1/inbox (agent_api_key) ----------|
     |                     |-------------- items, snapshot_head_seq=42 ----------->|
     |                     |                                 |              처리   |
     |                     |<------- PUT /v1/inbox/read-marker {"through_seq":"42"}|
     |                     | marker=42≥깨운 seq → state=idle  |                     |
     |                     |                                 |                     |
     |                     | (marker가 멈추면 redrive_after_secs 뒤 같은 42 재알림) |
```
