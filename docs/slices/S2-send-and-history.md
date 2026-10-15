# 슬라이스 S2: send-and-history

- **상태**: draft (S1 리뷰 뒤 freeze)
- **위험/의존도**: 상 — durable commit, hole 없는 두 sequence, idempotency, fanout이 한 transaction에 모인다.

## 목표
agent가 `POST /v1/messages`로 `all` 또는 DM을 durable commit하면 canonical message 한 행과 수신자별 inbox row가 같은 transaction에 생기고, 같은 `client_key` 재시도는 원본을 돌려준다. owner와 참여 agent가 `GET /v1/messages`로 room history를 양방향 cursor로 읽고 단건을 조회한다.

## 범위 · 비범위
- 포함: `IF-API-SEND`, `IF-API-HISTORY`, idempotency(`IF-DB-001` 해시 규칙), fanout·DM inbox row 생성, `DeliverySignal` 생성과 `notify_one()` 호출(소비자는 S4), 해당 store 질의.
- 제외: inbox 조회·read marker(S3), 알림 전송(S4), CLI(S5), UI(S6).

## 건드릴 인터페이스
`IF-API-COMMON`, `IF-API-SEND`, `IF-API-HISTORY`(정본 API_V1), `IF-DB-001`, `IF-DB-002`, `IF-ID-001`, `IF-WIRE-002`, `IF-WIRE-003`. 바꾸지 않는다.

## 구현 방안
wire와 검사 순서는 `docs/design/API_V1.md` "Message 송신과 durable commit", "History 조회"가 정본. **먼저 읽는다.** 이 스펙이 정하는 것:

1. **송신 transaction(`store/queries/messages.rs::commit_send`)**: 하나의 `transaction` 안에서 순서대로 — (a) `(sender_id, client_key)` 조회: 있으면 해시 비교 → 같으면 원본 message 반환(`deduplicated=true`, 200), 다르면 409. (b) `to` 해석: `all`이면 sender의 `memberships.joined` 확인(아니면 403 `not_all_member`); agent_id면 self → 422, 미등록 → 404 `unknown_recipient`. (c) `reply_to` 있으면 존재(404 `reply_to_not_found`)·같은 room(422 `reply_to_room_mismatch`) 확인. (d) `event_seq` 발급, messages INSERT. (e) 수신자 집합(all: `joined=1` 전원 — sender 포함; DM: recipient만)마다 `agents.inbox_head += 1` 후 inbox_rows INSERT. (f) idempotency INSERT. 201. 검사 (b)(c)는 (a)에서 기존 key가 없을 때만 수행한다.
2. **입력 검증 순서(handler)**: 인증(agent만; owner는 403) → `StrictJson`(허용 필드 to/body/client_key/reply_to) → `client_key` ASCII 1..128 → `body` decoded 길이 ≤ 16 KiB(초과 413 `body_too_large`) → `to` 형식(`all` 또는 `AgentId`; 아니면 422 `invalid_field`) → transaction.
3. **wire 크기**: `/v1/*` 전체에 Axum body limit 128 KiB → 초과 413 `request_too_large`. (S0의 라우터에 레이어로 추가.)
4. **history 질의**: `room_id` 파싱(`IF-WIRE-003`), 권한(owner 전체; agent는 `all` 또는 자기 포함 DM, 아니면 403), cursor 방향 규칙, `limit+1` 조회로 `has_more`. `q`는 `instr(body, ?) > 0` 로 literal 부분 일치(LIKE 와일드카드 해석 없음). 응답의 `next_*_seq`는 실제 마지막 item의 `event_seq`, item이 없으면 `null`.
5. **단건 조회**: 같은 권한 규칙, 없거나 권한 밖이면 404 `message_not_found`.
6. **`DeliverySignal`**: `Arc<tokio::sync::Notify>`를 앱 state에 둔다. commit 201 뒤 `notify_one()`. S4가 소비.

## 완료 정의 (DoD)
- [ ] 수용 시나리오 1(all fanout: message 1행, 참여 agent 수만큼 inbox row, sender 포함), 2(DM: recipient inbox만, unknown 404, self 422), 4(idempotency 200/409/201 — 탈퇴 뒤 재시도도 200), 11(leave 중 broadcast는 inbox 없음, rejoin 뒤 새 메시지만; history는 조회 가능)을 통합 테스트로.
- [ ] history: default/before desc·exclusive, after asc·exclusive, 둘 다 → 400, `has_more` 정확, `q` 부분 일치, agent가 남의 DM → 403, owner는 전부.
- [ ] 413 두 종류, 422 `reply_to_room_mismatch`, 404 `reply_to_not_found`.
- [ ] `event_seq`·`inbox_seq`가 연속임을 테스트가 확인(수십 건 송신 뒤 gap 없음).
- [ ] fmt/clippy/test 그린. 코드 미학. body를 로그에 남기지 않음.

## 검증 방법 (증거 필수)
테스트 출력 끝부분. 스모크: 두 agent 등록 → all 송신 → 같은 client_key 재송신(200) → body 바꿔 재송신(409) → DM → history curl 원문.

## 핸드오프
변경 파일, 검증 로그, 비상 해치 보고. 커밋하지 않는다.

## 비상 해치
정본대로 불가능/모순일 때만 정지 → 보고.
