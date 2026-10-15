# 슬라이스 S3: inbox-and-read-marker

- **상태**: done (2026-10-15) — 162 tests. owner 경로는 agent 본인도 403, `until_seq > head`는 그대로 echo, 409 details는 `{}`.
- **위험/의존도**: 중 — snapshot pagination과 marker 단조성. 저장 모델은 S0·S2에 이미 있다.

## 목표
agent가 자기 inbox를 `snapshot_head_seq` 고정 상한으로 누락·중복 없이 순회하고, 확인한 연속 prefix만 `PUT /v1/inbox/read-marker`로 올린다. owner는 임의 agent의 inbox를 부작용 없이 조회한다.

## 범위 · 비범위
- 포함: `IF-API-INBOX`(두 endpoint), `IF-API-READMARKER`, 해당 store 질의.
- 제외: marker 변화에 따른 delivery state 전이(S4), CLI(S5), UI(S6).

## 건드릴 인터페이스
`IF-API-COMMON`, `IF-API-INBOX`, `IF-API-READMARKER`(정본 API_V1 "Inbox 조회와 read marker"; snapshot 규칙은 PLATFORM_MESSAGING_SPEC "Inbox pagination snapshot"도 참조), `IF-DB-001`, `IF-DB-002`, `IF-ID-001`, `IF-WIRE-002`, `IF-WIRE-003`.

## 구현 방안
정본을 **먼저 읽는다.** 이 스펙이 정하는 것:

1. **inbox 질의**: 입력 `after_seq`(기본 = 그 agent의 `read_marker`), `until_seq`(선택), `limit`(1..100, 기본 50). `snapshot_head_seq` = `until_seq`가 있으면 그 값 그대로, 없으면 현재 `agents.inbox_head`. 반환 범위 `after_seq < inbox_seq <= snapshot_head_seq`, `inbox_seq` 오름차순, `limit+1`로 `has_more`. `next_after_seq` = 마지막 item의 `inbox_seq`, item 없으면 요청 `after_seq`. item은 inbox_rows ⋈ messages (`inbox_seq, message_id, event_seq, room_id, sender_id, body, created_at`). `read_marker` 필드 포함.
2. **owner inbox**: `GET /v1/agents/{id}/inbox` 같은 질의, agent 없으면 404 `agent_not_found`. agent principal이 이 경로를 쓰면 403.
3. **read marker**: agent만(owner 403). `through_seq > inbox_head` → 409 `read_marker_ahead`; `through_seq <= read_marker` → no-op; 아니면 갱신. 응답 `{agent_id, read_marker}` 200. 하나의 transaction.
4. GET 경로에는 어떤 write도 없다(테스트가 DB 변화 없음을 확인).

## 완료 정의 (DoD)
- [ ] 수용 시나리오 3(offline agent도 inbox에 있고 뒤에 조회 가능), 7·10(API_V1 시나리오 7: head 44에서 `after_seq=41&limit=1` → 42, `next_after_seq=42`; 다음 `after_seq=42&until_seq=44` → 43부터; 순회 중 45가 생겨도 제외), 8(GET은 marker 불변, PUT만 전진, 초과 409, 낮은 값 no-op 200), 9(agent-b가 agent-a inbox → 403; owner 조회 성공·marker 불변).
- [ ] `limit` 범위 밖 422, `after_seq`/`until_seq` 형식 오류 422(`Seq` 규칙). query parameter 엄격성은 `IF-WIRE-005`를 따른다(S2의 history 파서를 재사용).
- [ ] fmt/clippy/test 그린. 코드 미학.

## 검증 방법 (증거 필수)
테스트 출력 끝부분. 스모크: 송신 3건 → inbox page 순회 curl 원문 → read-marker PUT → 재조회.

## 핸드오프
변경 파일, 검증 로그, 비상 해치 보고. 커밋하지 않는다.

## 비상 해치
정본대로 불가능/모순일 때만 정지 → 보고.
