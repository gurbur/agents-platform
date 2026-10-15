# 슬라이스 S4: delivery-worker

- **상태**: done (2026-10-16) — 217 tests. Retry-After 하한 clamp, marker-at-head 우선, 미확인 없는 route 변경 → idle 은 `IF-DLV-001`에 확정 반영.
- **위험/의존도**: 상 — 외부 HTTP, 재시도, 상태 기계, 재시작 복구가 겹친다. 수용 시나리오 5개가 걸려 있다.

## 목표
inbox head가 올라간 agent의 receiver에 `IF-RCV-BODY` POST를 보내고, 응답을 `IF-RCV-RESPONSE`·`IF-RCV-RETRY` 규칙대로 분류해 `notification_pending`을 갱신한다. 재시작 뒤에도 pending·backoff가 DB에서 이어진다. owner/agent가 `GET delivery`로 상태를 보고 `POST delivery/retry`로 blocked를 푼다.

## 범위 · 비범위
- 포함: `delivery` 모듈(worker loop, callback client, 응답 분류, 상태 전이), `IF-API-DELIVERY` 두 endpoint, S1 receiver PATCH·rotate와 S3 read-marker PUT 경로에 상태 전이 hook 추가, `IF-DLV-001` 전이표 확정.
- 제외: CLI(S5), UI(S6), systemd(S7).

## 건드릴 인터페이스
`IF-RCV-BODY`, `IF-RCV-RESPONSE`, `IF-RCV-RETRY`(정본 RECEIVER_WAKE_CONVENTION), `IF-API-DELIVERY`(정본 API_V1 "Delivery 진단과 receiver 알림"), `IF-DLV-001`, `IF-DB-001`(`notification_pending`), `IF-DB-002`(`DeliverySignal`), `IF-CFG-001`(`[delivery]`).

## 구현 방안
외부 의미는 정본 두 문서가 정한다. **먼저 읽는다.** 이 스펙이 정하는 것(= `IF-DLV-001` 전이표):

### 저장 표현
- `state ∈ {idle, pending, blocked}`. 표시용 `no_receiver`는 `receiver_url IS NULL`에서 파생.
- `target_seq`: 다음 POST에 넣을 seq = 평가 시점의 `inbox_head`. `pending_seq`(API 표시) = `read_marker < inbox_head`이면 `inbox_head`, 아니면 `null`.
- in-flight 여부는 메모리(worker의 `HashSet<AgentId>`)에만 둔다. 재시작하면 in-flight는 없다.

### 전이 (모두 하나의 `transaction` 안에서)
| 사건 | 조건 | 전이 |
| --- | --- | --- |
| 새 inbox row (S2 commit 뒤 `DeliverySignal`) | `state != blocked` | `state=pending`, `target_seq=head`. `next_due_at`이 NULL이면 `now`(즉시). 이미 미래 시각이면 **유지**(backoff deadline 리셋 금지). |
| 새 inbox row | `state == blocked` | `target_seq=head`만 갱신. 시도 없음. |
| POST 시작 | 큐에서 due인 agent, URL non-null, in-flight 아님 | 메모리 in-flight 추가. snapshot `(route_version, url, secret, posted_seq=target_seq)`. `last_attempt_at=now`. |
| 응답 도착, `row.route_version != snapshot.route_version` | — | **무시**(로그만). in-flight 해제 뒤 재평가. |
| 202 + `Content-Length: 0` + 빈 body | — | `last_accepted_seq=posted_seq`, `attempt=0`, `last_error=NULL`, `redrive_due_at=now+redrive_after`. 그 뒤 `read_marker >= head`면 `state=idle`, `next_due_at=NULL`, `redrive_due_at=NULL`; `head > posted_seq`면 `target_seq=head`, `next_due_at=now`; 아니면 `next_due_at=NULL`(redrive 대기). |
| network·connect/전체 timeout·5xx·429 | — | `attempt+=1`, `next_due_at=now+backoff(attempt)` 단 유효 `Retry-After`(초 또는 HTTP-date)가 있으면 `min(그 값, backoff_max)`, `last_error` 코드(`connect`, `timeout`, `http_503`, `http_429` …), `state=pending`. |
| 그 외 4xx·3xx·202 아닌 2xx·non-empty 202·Content-Length 없는 202 | — | `state=blocked`, `next_due_at=NULL`, `redrive_due_at=NULL`, `last_error`(`http_404`, `http_301`, `http_200`, `protocol_mismatch`). |
| `redrive_due_at` 도달 | `state=pending`, `read_marker<head`, `next_due_at IS NULL` | `target_seq=head`, `next_due_at=now`. |
| read marker PUT (S3 hook, `store/queries/inbox.rs::advance_read_marker` 같은 transaction) | `read_marker >= head` | `state=idle`, `next_due_at=NULL`, `redrive_due_at=NULL`, `attempt=0`. (blocked여도 idle로 — 미확인이 없다.) |
| receiver PATCH로 URL 변경 / rotate (S1 hook) | `route_version` 증가함 | `notification_pending.route_version = agents.route_version`(S1 코드는 `agents`만 올리고 이 표는 건드리지 않았다 — `store/queries/agents.rs::update_receiver`·`rotate_credentials`에 같은 transaction으로 추가), `attempt=0`, `last_error=NULL`. URL non-null이고 `read_marker<head`면 `state=pending`, `target_seq=head`, `next_due_at=now`; URL null이면 `next_due_at=NULL`(state 유지). blocked는 해제된다. |
| `POST delivery/retry` | URL null | 변경 없음, 응답 `state=no_receiver`. |
| `POST delivery/retry` | `read_marker >= head` | `state=idle` 보장, 응답 `idle`. |
| `POST delivery/retry` | blocked | `state=pending`, `attempt=0`, `next_due_at=now`, `target_seq=head`. |
| `POST delivery/retry` | pending(backoff 중 또는 in-flight) | **변경 없음**(기존 due 유지, 병렬 금지). `target_seq=head`만 갱신. |

`backoff(attempt) = min(backoff_max, backoff_min · 2^(attempt-1))` 에 ±50% jitter(결과는 `[backoff_min, backoff_max]`로 clamp).

### Worker loop (`delivery/worker.rs`)
- 기동 시 DB에서 `state=pending`인 행을 읽어 due 시각을 계산한다(재시작 복구 — 시나리오 8).
- `loop { select!(signal.notified(), sleep_until(next_wakeup)) }`. 깨어나면 due 행(`next_due_at <= now` 또는 `redrive_due_at <= now`)을 뽑아 in-flight 아닌 agent마다 attempt task를 spawn. 동시 attempt 상한은 상수 16. idle이면 신호·다음 due까지 잔다(tight loop 금지).
- attempt task: POST → 분류 → `transaction`으로 전이 적용 → in-flight 해제 → `signal.notify_one()`(재평가).

### Callback client (`delivery/client.rs`)
- 전용 `reqwest::Client`: `redirect(Policy::none())`, `http1_only()`, `connect_timeout`, `timeout`(전체), gzip 등 압축 feature 없음, `User-Agent: platformd/<version>`. CLI client와 공유하지 않는다.
- 요청: `POST url`, `Authorization: Bearer <receiver_secret>`, `Content-Type: application/json`, body `{"v":1,"agent_id":"..","inbox_seq":".."}` (serde로 `Seq` 문자열), ≤1024B(검증, 초과면 내부 오류로 blocked `protocol_mismatch`가 아니라 `internal` 로그 — agent_id 64자 상한에서 일어날 수 없음).
- 202 판정: status 202 **그리고** `Content-Length` 헤더가 정확히 `0` **그리고** 본문 0바이트. 셋 중 하나라도 아니면 `protocol_mismatch`(blocked).
- 로그에 URL host까지만, secret·body 금지.

### API
- `GET /v1/agents/{id}/delivery`: owner 또는 본인. 필드는 API_V1 그대로(`read_marker`, `inbox_head_seq`, `pending_seq`, `last_accepted_seq`, `route_version`, `receiver_url_present`, `state`, `last_attempt_at`, `last_error`). seq류는 `Seq` 문자열, 없으면 `null`.
- `POST /v1/agents/{id}/delivery/retry`: 빈 body(또는 `{}`), owner 또는 본인. 응답 200 `{state, route_version, pending_seq, last_accepted_seq}`. 전이는 위 표. 실행 뒤 `signal.notify_one()`.

## 완료 정의 (DoD)
- [ ] 테스트용 mock receiver(axum, 프로그래머블 응답·수신 기록) 로 수용 시나리오 5(URL null→commit 유지→URL 설정 뒤 새 route_version으로 알림), 6(중복·out-of-order 허용, agent당 in-flight 1 — 느린 receiver에 연속 commit 시 동시 요청 1개), 7(5xx/429/timeout backoff·Retry-After, backoff 중 새 head가 deadline을 바꾸지 않음, manual retry가 blocked 해제·즉시 예약, pending 중 retry는 due 유지, null→`no_receiver`, marker=head→`idle`), 8(202 뒤 marker 안 올라가면 redrive; 서버 재시작 뒤 pending·backoff 유지), 9(URL/secret 변경 뒤 이전 route의 늦은 202·401이 현재 상태를 바꾸지 않음) 통과.
- [ ] 202 판정: body 있는 202, `Content-Length` 없는 202(chunked), 200 → 모두 blocked `protocol_mismatch`/`http_200`. 404 → blocked. 503 → pending backoff.
- [ ] 수신 body가 정확히 세 필드·`inbox_seq` 문자열·≤1024B, `Authorization`이 receiver secret(agent api key 아님).
- [ ] S1·S3 hook 테스트: read marker가 head에 닿으면 idle; rotate 뒤 blocked 해제.
- [ ] 튜닝 수치를 테스트에 박지 않는다(테스트는 config로 1~2초 값을 주입하고 "그 범위 안에 재시도된다"만 본다).
- [ ] fmt/clippy/test 그린. 코드 미학.

## 검증 방법 (증거 필수)
테스트 출력 끝부분. 스모크: 로컬 mock receiver(예: `python3 -m http.server` 아닌, 202+CL:0을 주는 작은 서버 — 테스트 crate의 mock을 바이너리로 띄우거나 `nc`) 에 실제 POST가 오는 것과 `GET delivery` 응답 원문.

## 핸드오프
변경 파일, 검증 로그, 전이표와 구현의 대응(표 행마다 테스트 이름), 비상 해치 보고. 커밋하지 않는다.

## 비상 해치
정본대로 불가능/모순일 때만 정지 → 보고.
