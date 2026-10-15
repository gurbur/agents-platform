# 슬라이스 S1: agents-and-credentials

- **상태**: frozen (2026-10-15)
- **위험/의존도**: 상 — 인증·권한 경계와 credential 발급은 이후 모든 endpoint가 올라타는 바닥이다.

## 목표
owner가 agent를 등록해 one-time credential을 받고, owner 또는 해당 agent가 receiver 경로·`all` membership을 바꾸며, owner가 token을 rotate한다. 모든 `/v1/*` 요청이 공통 인증 extractor와 공통 오류 body를 거친다.

## 범위 · 비범위
- 포함: `IF-API-AGENTS`, `IF-API-RECEIVER`, `IF-API-MEMBERSHIP`, `IF-API-ROTATE`, 인증 extractor(`Principal`), 공통 오류 응답·JSON 파싱 오류 매핑(`IF-WIRE-002`), `GET /v1/agents` 응답 형태(`IF-WIRE-001`), 해당 store 질의.
- 제외: 메시지·inbox·history(S2, S3), delivery worker 동작(S4 — 단 `notification_pending` 초기 행은 여기서 만든다), CLI 서브커맨드(S5), UI(S6).

## 건드릴 인터페이스
`IF-API-COMMON`, `IF-API-AGENTS`, `IF-API-RECEIVER`, `IF-API-MEMBERSHIP`, `IF-API-ROTATE`(정본 API_V1), `IF-AUTH-001`, `IF-DB-001`, `IF-DB-002`, `IF-ID-001`, `IF-WIRE-001`, `IF-WIRE-002`. 정의를 바꾸지 말고 그대로 구현한다.

## 구현 방안
wire·상태 코드·필드는 `docs/design/API_V1.md`가, credential 형식은 `INTERFACES.md`가 정본이다. **먼저 읽는다.** 이 스펙이 정하는 것:

1. **`auth` 모듈**: Axum extractor `Principal`(`Owner | Agent(AgentId)`). `IF-AUTH-001` 판정 순서. 실패 → 401 `unauthorized`. 권한 검사 헬퍼 `require_owner()`, `require_owner_or_self(agent_id)` → 403 `forbidden`.
2. **`api/error.rs`**: S0의 `ApiError`에 `details`(`serde_json::Map`)를 추가해 `IF-API-COMMON` 오류 body + 요청의 `x-request-id`(S0 task-local)로 응답한다. 생성 헬퍼(`unauthorized()`, `forbidden()`, `not_found(code)`, `unprocessable(code)` 등)를 둔다. `IF-WIRE-002`에 따라 Axum `Json` rejection을 400/422로 매핑하는 커스텀 extractor `StrictJson<T>`(`deny_unknown_fields` 타입 전용).
3. **`store/queries/agents.rs`**: 등록(agents + memberships('all', joined=1, epoch=1, effective_event_seq=현재 event_seq) + read_state(0) + notification_pending(state='idle', route_version=1, target_seq=0) 를 **한 transaction**), 목록, receiver 갱신(값이 바뀔 때만 `route_version += 1`; null↔URL 전환도 변경), membership 갱신(값이 바뀔 때만 `epoch += 1`, `effective_event_seq = 현재 event_seq`; 같은 값은 no-op 200), rotate(두 키 재발급 + `route_version += 1`), api_key_hash 조회.
4. **handlers**: API_V1 각 절의 요청·응답 필드 그대로. `GET /v1/agents`는 principal에 따라 두 형태(`IF-WIRE-001`). delivery `state` 표시는 `IF-DLV-001`의 파생 규칙(`receiver_url IS NULL → no_receiver`, 아니면 `notification_pending.state`).
5. **receiver_url 검증**: `url` crate로 파싱. scheme http/https만, userinfo·fragment 거부, host 필수 → 아니면 422 `invalid_receiver_url`. 길이 상한 2048.
6. **credential 생성**: `rand` CSPRNG 32B → base64url(no pad) → prefix. SHA-256 hex 저장. 응답에만 평문.

## 완료 정의 (DoD)
- [ ] 통합 테스트(HTTP, 임시 DB): 수용 시나리오 1(등록 201 응답에 `joined_all=true`, 두 one-time 키 포함; 재조회에는 키 없음), 시나리오 10 일부(agent-b 키로 agent-a receiver/membership 변경 → 403), 시나리오 12 일부(rotate가 새 키를 한 번만 반환하고 이전 키는 즉시 401, `route_version` 증가).
- [ ] 409 `agent_id_taken`, 422 `invalid_agent_id`(예약어·형식), 422 `invalid_receiver_url`(ftp, userinfo, fragment), 422 `unexpected_field`(허용 밖 필드), 400 `invalid_json`, 401(토큰 없음·오류·receiver secret으로 호출).
- [ ] receiver PATCH 같은 값 → `route_version` 불변, 다른 값/null 전환 → +1. membership 같은 값 → epoch 불변.
- [ ] `cargo fmt --check`, clippy `-D warnings`, `cargo test --workspace` 그린.
- [ ] 코드 미학. 로그에 키·토큰 없음(테스트에서 로그 캡처로 `pak_`/`prs_` 미출현 확인).

## 검증 방법 (증거 필수)
- 테스트 출력 전체 끝부분. 스모크: 서버 기동 → `curl` 로 등록 → 받은 키로 `GET /v1/agents` → owner 토큰으로 rotate → 이전 키 401 확인. 명령과 응답 원문.

## 핸드오프
변경 파일, 검증 로그, 비상 해치 보고. 커밋은 하지 않는다.

## 비상 해치
정본대로 불가능/모순일 때만 정지 → 보고. 우회 금지.
