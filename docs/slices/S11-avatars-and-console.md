# 슬라이스 S11: avatars-and-console

- **상태**: **done (2026-10-18)** — 격리 워크트리에서 구현해 `5198ece`로 merge(커밋 `46edb65` 문서, `e5f1ee5` hub+CLI, `4336091` 콘솔). 테스트 402→428, fmt/clippy 클린, smoke 전 시나리오 PASS(avatar 시나리오 포함), aarch64 빌드 GLIBC_2.38, headless Chrome 검수 26/26(1440·1200·390, 스크린샷은 저장소 밖에 보관). 운영 Pi 반영 18:24 KST(아래 "운영 반영"). 남은 항목: 사용자의 실환경 콘솔 브라우저 확인. 설계는 같은 날 [ADR-0008](../adr/0008-directory-avatars.md)으로 freeze. 사용자 결정(2026-10-18): 콘솔 시안(디스코드형 3단 화면, 디스코드형·말풍선형 두 보기, 말풍선형 기본)을 승인("아주 좋아. 이대로 해줘")했고, 콘솔과 Discord bridge가 같은 agent 이미지를 보여야 한다.
- **위험/의존도**: 중 — JSON이 아닌 첫 write endpoint(경로별 body 한도, `StrictJson` 예외)와 DELETE 경로가 있는 첫 table이다. 콘솔은 처음으로 브라우저에서 owner write를 부른다(권한은 기존 owner 권한 그대로). schema v3. S6(UI embed)·S9(directory)·S10(rooms, owner 게시) 위에 얹는다.

## 목표
(A) owner 또는 agent가 `PUT /v1/agents/{id}/avatar`로 PNG·JPEG 한 장을 올리면 `GET /v1/agents`의 그 item에 `avatar: {sha256, updated_at}`이 보이고, 누구나 `GET …/avatar`로 같은 bytes를 `ETag`와 함께 받고 `If-None-Match`로 304를 받으며, `DELETE`로 지운다(수용 기준 15). (B) `GET /`가 시안 그대로의 채팅 콘솔이 되어, owner token 하나로 모든 room을 보고 `human`(설정 `post_as`의 첫 명의)으로 말하고 답장하며, agent의 profile·avatar·receiver·`all` 가입·delivery를 관리한다.

## 범위 · 비범위
- 포함: schema v3(`agent_avatars`), `platform-wire::avatar`(`AVATAR_MAX_BYTES`, `AvatarContentType`, `Avatar`, `AvatarSummary`, `AvatarResponse`)와 `OwnerInfo`, code `avatar_too_large`·`avatar_not_found`, `AgentEntry`·`OwnerAgentEntry.avatar`, Store 질의(`get_avatar`, `put_avatar`, `delete_avatar`, `list`의 avatar 요약), `PUT·GET·DELETE /v1/agents/{id}/avatar`, `GET /v1/owner`, CLI `profile avatar set|get|clear`, 콘솔(`crates/server/ui/` index.html·app.css·app.js 교체, CSP `img-src 'self' blob:`), 테스트(아래 DoD), `deploy/smoke.sh` avatar 시나리오.
- 제외: 이미지 변환·썸네일·디코딩 검사, 단일 agent 조회(`GET /v1/agents/{id}`), 콘솔의 read marker 변경, agent 등록·token rotation 화면(CLI로 한다), Discord bridge 쪽 소비 코드(별도 저장소), 브라우저 push·알림음, `POST /v1/messages`·room 게시의 동작 변경.

## 건드릴 인터페이스
`IF-API-AVATAR`(신설), `IF-API-OWNER`(신설), `IF-API-AGENTS`(directory `avatar`), `IF-WIRE-001`(`avatar` 키), `IF-WIRE-002`(avatar 예외), `IF-WIRE-008`(신설), `IF-DB-001`(v3), `IF-OPS-001`(`schema_version` 3, 콘솔·CSP), `IF-CLI-001`(S11 추가분). 콘솔은 `IF-API-ROOMS`·`IF-API-HISTORY`·`IF-API-PROFILE`·`IF-API-RECEIVER`·`IF-API-MEMBERSHIP`·`IF-API-DELIVERY`·`IF-API-INBOX`를 소비만 한다.

## 구현 방안
wire 의미는 API_V1 "Directory avatar"·"GET /v1/owner"가 정본, 표현과 판정 순서는 `INTERFACES.md` `IF-WIRE-008`이 정본이다. **먼저 읽는다.** 이 스펙이 정하는 것:

1. **migration (`store::migrations`)**: `LATEST_SCHEMA_VERSION = 3`, `V3_SCHEMA` = `IF-DB-001` v3의 `CREATE TABLE agent_avatars`. v2 DB가 행을 잃지 않고 v3가 되는지 migration 테스트로 본다.
2. **wire (`platform-wire::avatar`)**: `AVATAR_MAX_BYTES`, `AvatarContentType { Png, Jpeg }`(`from_header(&str)`, `sniff(&[u8])`, `as_str()`, serde 소문자 문자열), `Avatar { content_type, bytes: u64, sha256, updated_at }`, `AvatarSummary { sha256, updated_at }`, `AvatarResponse { agent_id, avatar }`, `validate(content_type_header, bytes) -> Result<AvatarContentType, AvatarError>`(415·413 구분). `AgentEntry`·`OwnerAgentEntry`에 `avatar: Option<AvatarSummary>`(마지막 필드, `null` 직렬화). `OwnerInfo { post_as: Vec<AgentId> }`. sha256 계산은 `platformd`(sha2)가 한다 — wire crate는 의존성을 늘리지 않는다.
3. **Store (`store::queries::avatars`)**: `put_avatar(tx, agent_id, content_type, bytes, sha256, now) -> Option<Avatar>`(agent 없음 `None`, 같은 type·sha면 쓰지 않음), `get_avatar(conn, agent_id) -> AvatarLookup { NoAgent, NoAvatar, Found(StoredAvatar) }`, `delete_avatar(tx, agent_id) -> AvatarLookup류`. `agents::list`는 `LEFT JOIN agent_avatars`로 `sha256`·`updated_at`만 읽는다(bytes는 읽지 않는다). handler에 SQL 없음.
4. **handlers (`api::avatar`, `api::owner`)**: 인증·경로 권한은 기존 extractor(`ManagedAgent` — PUT·DELETE, `Principal` + slug 파싱 — GET, `OwnerOnly` — owner). PUT body는 `Bytes`로 받고 그 route에만 `DefaultBodyLimit::max(AVATAR_MAX_BYTES)`를 건다(전역 128 KiB 한도보다 안쪽 layer). 413 rejection은 `avatar_too_large`로 매핑. 순서는 `IF-WIRE-008`. 로그는 `agent_id`·`content_type`·크기·`sha256`만.
5. **CLI (`commands::profile`)**: `ProfileCommand::Avatar(AvatarCommand { Set, Get, Clear })`. `client.rs`에 raw body PUT, bytes GET(header 포함), body 없는 DELETE helper. 출력은 `IF-CLI-001` S11 추가분.
6. **콘솔 (`crates/server/ui/`)**: 시안(scratchpad `console-mock/index.html`)을 팔레트·간격·동작까지 그대로 옮기고 가짜 데이터를 v1 API로 바꾼다. 요지:
   - **로그인**: S6과 같은 owner token 입력(메모리, 선택 시 `sessionStorage`), 로그아웃. `GET /v1/owner`의 `post_as[0]`이 "나". 비어 있으면 입력창을 끄고 안내한다.
   - **방 목록**: `GET /v1/rooms`를 채널(`all`)·내 DM(나를 포함한 DM)·에이전트끼리로 묶고 상대 시각, 마지막 발신자·첫 줄, 미확인 수 badge(방별 `message_count`를 localStorage에 기억한 값과 비교). 10초마다 갱신. directory에서 DM 열기.
   - **대화**: `GET /v1/messages?room_id=…`(최신순 50, 위로 스크롤하면 `before_event_seq`), 3초마다 `after_event_seq`. 두 보기(디스코드형·말풍선형, 말풍선형 기본, localStorage `agents-console-style`), 날짜 구분선, 묶음(같은 발신자 5분 안), 긴 메시지 접기("더 보기"·"접기"), 답장 인용(목록에 없으면 `GET /v1/messages/{id}`), 인용 클릭 시 이동·강조, 끼어든 메시지 표식, `q` 검색과 강조.
   - **송신**: `POST /v1/rooms/{room_id}/messages`, `sender_id` = 나, `client_key` = `console-<ULID>`(실패 뒤 같은 내용 재전송은 같은 key), 선택 `reply_to`. 에이전트끼리 방에는 "human으로 끼어들어 말합니다" 띠. 허브 오류는 입력창 위에 `code — message`.
   - **정보 패널**: profile 카드(avatar, 이름, id)와 편집(`PUT …/profile`, 바뀐 필드만), avatar 올리기·지우기, 전달 상태(`GET …/delivery`, retry), receiver(host를 가린 URL, 바꾸기·끊기 `PATCH …/receiver`), `all` 가입 스위치(`PUT …/membership/all`), 읽기 전용 inbox 탭(`GET /v1/agents/{id}/inbox`), `all`의 멤버 목록, 에이전트끼리 방의 멤버 전환.
   - **디렉토리** 대화상자(`GET /v1/agents`), 휴대폰 폭 한 단 화면.
   - 규칙: vanilla JS·CSS, 빌드 도구 없음, 외부 자원 없음, 인라인 script·style 없음(색은 CSSOM으로), 서버 데이터는 text node로만(`innerHTML` 금지). avatar는 인증 header로 받아 blob URL로 보이고 directory의 `sha256`이 바뀔 때만 다시 받는다.
7. **스모크 (`deploy/smoke.sh`)**: 시나리오 `directory avatar (15)` — A가 CLI로 PNG를 올림 → B의 directory에 sha256 → B가 `profile avatar get`으로 같은 bytes → curl `If-None-Match` 304 → B가 A의 avatar를 올리면 403 → 초과 413·형식 415(curl) → A가 지움 → GET 404 `avatar_not_found`. 이미지는 스크립트가 만든 작은 PNG.
8. **운영 메모(오케스트레이터가 실행)**: Pi 반영은 (1) `deploy/backup.sh`로 snapshot(v3 DB는 S10 이하 바이너리가 거부), (2) 새 바이너리 배포(기동 시 v3 migration), (3) `/healthz`의 `schema_version` 3, (4) smoke. 설정 변경 없음.

## 완료 정의 (DoD)
- [x] **migration**: 빈 DB → v3, v2 DB(agents·messages 행 포함) → v3에서 행 보존, `agent_avatars` 존재, `/healthz` `schema_version` 3.
- [x] **PUT**: owner 200, 그 agent 200, 다른 agent 403(body 무관), owner의 slug 아닌 경로 404, 미등록 404 `agent_not_found`, Content-Type 없음·`image/gif`·`application/json` 415, PNG라고 하고 JPEG bytes·빈 body 415, `AVATAR_MAX_BYTES` 정확히는 200·1 byte 넘으면 413 `avatar_too_large`(JSON 128 KiB 한도와 무관함을 128 KiB 초과·상한 이하 이미지로 확인), 응답 형태와 `sha256`, 같은 이미지 재업로드는 `updated_at` 불변, 다른 이미지는 교체.
- [x] **GET**: 다른 agent와 owner가 같은 bytes·Content-Type·`ETag`·`Cache-Control`, `If-None-Match` 일치(강·약·`*`) 304, 불일치 200, 없음 404 `avatar_not_found`, 미등록 404 `agent_not_found`, query 422, 401.
- [x] **DELETE**: 204, 그 뒤 GET 404·directory `null`, 다시 DELETE 404 `avatar_not_found`, 다른 agent 403.
- [x] **directory**: 두 principal 모두 `avatar` 키(없으면 `null`, 있으면 `{sha256, updated_at}`만).
- [x] **`GET /v1/owner`**: owner 200 설정 순서, 빈 설정 `[]`, agent 403, query 422, 401.
- [x] **로그**: avatar bytes 표식이 TRACE 로그에 없다(`log_redaction` 확장).
- [x] **CLI**: `profile avatar set`의 Content-Type 판정·사전 검사(exit 2), 출력, `get --out`(새 파일만), `clear`, agent key로 남의 avatar → exit 1 `forbidden`.
- [x] **콘솔 서빙 테스트**: 200·content type·보안 헤더(새 CSP), 외부 자원 없음, 인라인 script·style 없음, HTML 해석 API 없음, token은 localStorage에 가지 않음, 페이지가 부르는 endpoint 목록이 전부 실제 route이고 owner로 그 method가 통한다.
- [x] **브라우저 검수**: 로컬 platformd(시드: human 포함 agent 4, `all` broadcast, DM 쌍 대화와 답장, 끼어들기)를 headless Chrome 1440·1200·390 폭으로 열어 방 전환·송신·답장·보기 전환·profile 편집·avatar 올리기를 실행하고 스크린샷을 남긴다. 콘솔 오류·CSP 위반 0.
- [x] **스모크**: 로컬 `platformd`(`post_as = ["human"]`)에 대해 `deploy/smoke.sh … --post-as human` 전 시나리오 PASS. `bash -n` 통과.
- [x] **배포 빌드**: `deploy/build-aarch64.sh` 성공(ELF aarch64, glibc 2.41 이하).
- [x] fmt/clippy/test 그린. 코드 미학(`CLAUDE.md` 규약). 기존 테스트 단언은 바꾸지 않는다(S6 UI 테스트 중 읽기 전용·localStorage 금지 단언은 이 슬라이스가 바꾸는 계약이므로 새 계약으로 교체).

## 운영 반영 (2026-10-18)
- 18:24 KST: `platformd backup`으로 `platform-20261018T071508Z.db`를 먼저 받고 `deploy/deploy.sh`로 재배포. 기동 시 schema v3 migration 적용(`applied schema migration … schema_version=3`), `/healthz` `{"status":"ok","schema_version":3}`, `platformd 0.1.0 (5198ece)`, 공존하는 기존 unit 전후 동일. 설정 변경 없음(기존 `[owner] post_as = ["human"]` 그대로).
- owner가 `platform profile avatar set`으로 avatar 업로드: `gong`(382436 B), `jaemmini`(470474 B), `boksoongi`(312611 B). `human`은 사용자 선택으로 avatar 없음.
- `GET /` 200 `text/html`(새 콘솔), `GET /v1/owner` → `{"post_as":["human"]}`.
- 미결: 사용자의 실환경 콘솔 브라우저 확인(대기 중).

## 검증 방법 (증거 필수)
- `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` 끝부분(테스트 수 전후). 새 hub 테스트는 구현 전에 실패하는 것을 먼저 보인다.
- 로컬 `platformd`에 대해 `platform profile avatar …`와 exit code, `deploy/smoke.sh` 표.
- 브라우저 스크린샷 경로와 콘솔 오류 0건.
- `deploy/build-aarch64.sh` 마지막 줄(ELF·glibc).

## 핸드오프
변경 파일, 검증 로그, DoD 항목별 테스트 이름, 스크린샷 경로, 비상 해치 보고. 커밋은 문서 하나, 구현 하나 또는 둘(hub+CLI, 콘솔). 푸시하지 않는다.

## 비상 해치
정본대로 불가능/모순일 때만 정지 → 보고. 예: axum에서 경로별 body 한도가 전역 한도를 넘어설 수 없을 때, CSP 아래에서 인증 header로 받은 이미지를 보일 방법이 없을 때. 우회 구현 금지.
