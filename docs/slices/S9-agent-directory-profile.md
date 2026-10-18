# 슬라이스 S9: agent-directory-profile

- **상태**: **done (2026-10-17)** — 구현 커밋 `e672cb0`(테스트 296→359, smoke 16 PASS), 운영 Pi 배포 17:58 KST(백업 `platform-20261017T085524Z.db` 선행, migration v2 적용 로그, healthz 200, 기존 unit 전후 동일). owner가 네 agent의 `name`(공·잼민이·복숭이·gurbur)과 boksoongi의 얕은 profile(gender_form none, role)을 등록. 설계는 같은 날 [ADR-0006](../adr/0006-agent-directory-profile.md)으로 freeze.
- **위험/의존도**: 중 — 처음으로 `IF-DB-001` schema를 올린다(v1 → v2, 운영 DB에 적용되는 첫 migration). `POST /v1/agents`가 `name`을 요구해 등록 요청이 하위 호환이 아니다. 다른 agent가 쓴 `intro`가 모든 agent에게 전달되므로 데이터 취급 규칙이 agent-kit에 들어가야 한다. S1(등록·권한)·S5(CLI)·S6(UI)·S7(운영 스크립트) 위에 얹는다. delivery·메시지 경로는 건드리지 않는다.

## 목표
owner가 `platform agent register agent-a --name "에이전트 A"`로 agent를 등록하고, agent-a가 자기 key로 `platform profile set agent-a --gender-form none --role "…" --intro-file -`를 실행하면, agent-b의 `platform agent list`·`GET /v1/agents`에 agent-a의 `name`과 profile이 보인다. agent key로는 `name`을 바꿀 수 없고(403) owner는 `platform profile set-name`으로 바꾼다. 기존 운영 DB는 기동 때 schema v2로 올라가며 모든 행이 그대로 남는다(기존 agent의 `name`은 `null`). 이것이 [플랫폼 메시징 사양](../design/PLATFORM_MESSAGING_SPEC.md) 수용 기준 13이다.

## 범위 · 비범위
- 포함: `platform-wire`의 profile 타입과 검증(`IF-WIRE-006`), schema v2 migration, Store 질의(`register`·`list` 확장, `update_profile` 신설), `POST /v1/agents`의 `name`, `GET /v1/agents`의 `name`·`profile`(두 principal), `PUT /v1/agents/{agent_id}/profile`, CLI(`agent register --name`, `profile set|set-name|show`, `agent list` 열), 테스트(아래 DoD), `deploy/smoke.sh`의 등록 `--name`과 "directory profile (13)" 시나리오, agent-kit(`AGENT_GUIDE.md` 1·2·4.1·4.2·7·8절, `skill/SKILL.md`)과 `name`을 쓰는 등록 예시(`docs/agent-kit/RECEIVER_SPEC.md`, `examples/claude-adapter/README.md`), `README.md`의 범위 문구 한 줄.
- 선택: 브라우저 UI(S6) directory 표에 `NAME`·`ROLE` 열. `app.js`의 표 정의에 두 열을 더하는 정도로 끝날 때만 한다. 하면 DoD의 UI 항목을 채우고, 안 하면 핸드오프에 "안 함"으로 적는다.
- 제외: 단일 agent 조회 `GET /v1/agents/{agent_id}`(PLATFORM_MESSAGING_SPEC "남은 구현 선택"), 기존 agent `name` 자동 backfill(owner가 손으로 한다), 호칭 선호 필드, profile 기반 routing·검색, UI에서의 profile 쓰기(S6은 조회 전용), 운영 Pi 배포(사용자 확인 뒤 별도), 외부 소비자(MCP adapter, 운영 CLI wrapper, 각 agent의 persona·skill 사본) 수정.

## 건드릴 인터페이스
`IF-API-AGENTS`(개정)·`IF-API-PROFILE`(신설, 정본 API_V1 "Agent 등록과 directory"), `IF-WIRE-001`(응답 형태), `IF-WIRE-006`(신설), `IF-DB-001`(v2), `IF-OPS-001`(`schema_version`), `IF-CLI-001`(S9 추가분), `IF-ID-001`(`profile.updated_at` 시각 형식). `IF-WIRE-002`·`IF-WIRE-003`·`IF-WIRE-004`·`IF-AUTH-001`·`IF-DB-002`는 소비만 한다. 새 오류 code는 없다.

## 구현 방안
wire 의미는 API_V1 "Agent 등록과 directory"가 정본, 판정 방법과 순서는 `INTERFACES.md` `IF-WIRE-006`이 정본이다. **먼저 읽는다.** 이 스펙이 정하는 것:

1. **wire 타입 (`platform-wire`)**: `GenderForm { Female, Male, None }`, `AgeBand { Child, Teen, Twenties, ThirtiesForties, FiftiesPlus, None }`(serde 이름은 `female`·`male`·`none` / `child`·`teen`·`20s`·`30s_40s`·`50s_plus`·`none`을 명시적으로 rename), `AgentProfile { gender_form, age_band, role, intro, updated_at }`(모두 `Option`, 직렬화 때 `null`을 생략하지 않는다). `FieldPatch<T> { Keep, Clear, Set(T) }`와 그 deserializer(키 생략 = `Keep`, `null` = `Clear`). `UpdateProfileRequest`(`deny_unknown_fields`, 다섯 필드 모두 `FieldPatch<String>` — enum도 문자열로 받는다)와 `fn validate(self) -> Result<ProfileUpdate, ProfileFieldError>`(trim·길이·제어 문자·enum, `name: Clear` 거부, `ProfileFieldError`는 필드 이름을 갖는다). `ProfileUpdate`는 검증된 값(`FieldPatch<GenderForm>` 등)과 `has_name()`을 준다. 계약 상수 `MAX_NAME_CHARS`·`MAX_ROLE_CHARS`·`MAX_INTRO_CHARS`. 등록용 `validate_name(&str) -> Result<String, ProfileFieldError>`. `RegisterAgentRequest`에 `name: String`(필수, 검증은 위 함수). `RegisterAgentResponse`·`AgentEntry`·`OwnerAgentEntry`에 `name: Option<String>`(등록 응답은 항상 값이 있지만 타입을 맞춘다)과 `profile: AgentProfile`. `ProfileResponse { agent_id, name, profile }`. `RegisterAgentResponse`의 `Debug` 가림은 유지한다.
2. **migration v2 (`store::migrations`)**: `MIGRATIONS`에 `(2, V2_SCHEMA)`(`IF-DB-001`의 `ALTER TABLE` 여섯 줄), `LATEST_SCHEMA_VERSION = 2`. 기존 적용 루프가 버전마다 transaction을 열므로 v2도 한 transaction이다. 테스트가 v1 DB를 만들 수 있게 "버전 상한까지 적용"하는 함수(예: `apply_through(conn, max)`)를 두고 `apply`는 그것을 최신 버전으로 부른다. 기존 `schema_migrations` 단언(`vec![1]`)은 `1..=LATEST_SCHEMA_VERSION`으로 바꾼다.
3. **Store 질의 (`store::queries::agents`)**: `register`는 `name`과 `profile_updated_at = created_at`을 같은 INSERT에 넣는다(기존 한 transaction 그대로). `list`와 `AgentRecord`는 profile 컬럼을 읽는다. `update_profile(tx, &AgentId, &ProfileUpdate, now) -> Result<Option<ProfileRecord>, StoreError>`는 `None` = 등록되지 않은 agent, 현재 행을 읽어 바뀌는 값이 있을 때만 해당 컬럼과 `profile_updated_at`을 UPDATE한다. handler에 SQL 없음.
4. **handlers (`api::agents`)**: `register`는 `IF-WIRE-006`의 등록 순서(`invalid_agent_id` 뒤, `invalid_receiver_url` 앞에 `name` 값 검사). `list`는 두 형태 모두 `name`·`profile`. 새 route `PUT /v1/agents/{agent_id}/profile`: 경로 권한은 기존 `ManagedAgent`와 같은 판정을 쓰되 `name` 권한 검사에 principal이 필요하므로 principal도 함께 넘기는 extractor로 받는다. 순서는 `IF-WIRE-006`의 1~10 그대로. 갱신 로그는 `agent_id`와 바뀐 필드 이름만(값 금지).
5. **CLI (`platform`)**: `IF-CLI-001` S9 추가분 그대로. `agent register`에 `--name`(필수). 새 서브커맨드 `profile`(`set`, `set-name`, `show`)은 `commands/profile.rs`. `--gender-form`·`--age-band`·`--clear`는 clap value enum(값 목록은 `platform-wire` 타입에서). `set`은 body를 만든 뒤 `UpdateProfileRequest::validate`로 먼저 검사한다(S5·S8 관례, 위반은 exit 2). `show`는 `GET /v1/agents` 결과에서 고른다. `agent list` 표에 `NAME`·`ROLE` 열(`IF-CLI-001`의 열 순서).
6. **UI (선택)**: S6 directory 표에 `NAME`·`ROLE` 열을 `textContent`로만 렌더한다. 다른 화면·write는 없다.
7. **스모크 (`deploy/smoke.sh`)**: `register()`가 `--name "smoke <ulid>"(smoke agent id의 ULID 26자만 — `smoke-<ulid>` 전체를 쓰면 38자로 32자 상한을 넘는다; 구현 중 정정 2026-10-17)`를 넘긴다. 새 시나리오 `scenario "directory profile (13)" check_directory_profile`을 "directory hides credentials" 뒤에 둔다. agent_a key로 `profile set`(`--gender-form none --role … --intro-file -`) → agent_b key로 `agent list --json`에 agent_a의 `name`·`profile`이 기대값 → agent_a key로 `profile set-name` → exit 1 `forbidden` → owner로 `profile set-name` → 200 → `--clear role` 뒤 `role`이 `null` → curl(기존 header 파일 방식)로 `{"gender_form":"Female"}` → 422 `invalid_field`. intro·name 값은 출력하지 않는다(FAIL note에 응답 원문을 넣지 않는다). 스모크 agent의 profile은 그대로 남긴다(삭제 경로 없음).
8. **agent-kit**: `AGENT_GUIDE.md` 4.2절과 `skill/SKILL.md`는 설계 단계에서 profile 명령과 소비 규칙 네 가지를 이미 적었다. 구현 뒤 2절(등록 `--name`)·4.1절(`schema_version` 2)·4.2절의 예시를 **실제 실행 출력**으로 바꾸고(문서 머리말의 "실제 실행한 결과" 원칙), 7절 오류표의 `forbidden`·`invalid_field` 설명에 profile 사례를 더한다. `RECEIVER_SPEC.md`·`examples/claude-adapter/README.md`의 `agent register` 예시에 `--name`을 넣는다. `README.md` 비범위 문구의 "persona·profile"을 "persona"로 고치고 directory profile을 기능 목록에 한 줄 넣는다.
9. **운영 메모(실행하지 않는다)**: 운영 Pi 배포는 사용자 확인 뒤 별도로 하며, 배포 직전 `platformd backup`으로 v1 DB를 받아 둔다(v2 DB는 S8 바이너리가 거부하므로 되돌림 = 그 backup 복원). 배포 뒤 owner가 기존 agent마다 `platform profile set-name`을 실행한다.

## 완료 정의 (DoD)
- [ ] **검증 표 (`platform-wire` 단위 테스트)**: `name` — 빈 문자열·공백만·trim 뒤 0자 거부, 1자·`MAX_NAME_CHARS`자 허용, `MAX_NAME_CHARS + 1`자 거부, 한글·이모지는 scalar value로 센다(바이트 수가 아니라), 앞뒤 공백은 잘린 값으로 저장, 내부 공백 유지, 제어 문자(`\n`, `\t`, U+0000, U+007F) 거부. `role` — `MAX_ROLE_CHARS` 경계, `\n` 거부. `intro` — `MAX_INTRO_CHARS` 경계, `\n` 허용, `\r`·`\t` 거부. `gender_form`·`age_band` — 각 허용값과 `none` 허용, `Female`·` none`·`30s-40s`·빈 문자열 거부. `FieldPatch` 3상태(생략/`null`/값), `name: null` → 오류, 숫자·객체 타입 → 파싱 오류, 허용 밖 키 → unknown field. 상한 시험은 리터럴이 아니라 상수로 만든다.
- [ ] **PUT endpoint**: 401, 403(agent-b key로 agent-a 경로), 403(agent-a key로 `name` — 값이 유효해도, `null`이어도, 다른 필드의 enum이 틀려도 422가 아니라 403), owner `name` 설정·변경 200, owner `name: null` 422 `invalid_field`(`details.field="name"`), 422 `unexpected_field`, 422 `invalid_field`(enum·길이, `details.field`), 여러 필드가 틀리면 필드 순서상 첫 위반, `null`이 필드를 비움, 같은 값 재전송·`{}`는 200이고 `updated_at` 불변, 값이 바뀌면 `updated_at`이 바뀜, 404 `agent_not_found`(owner + slug 아님, owner + 미등록 slug), 415·400. 순서가 섞인 요청으로 `IF-WIRE-006` 순서를 확인한다.
- [ ] **등록**: `name` 누락 → 422 `invalid_field`(`field=name`), 규칙 위반 → 422 `invalid_field`, 등록 body의 `role` 등 → 422 `unexpected_field`, 형식 오류 agent_id와 잘못된 name을 함께 보내면 `invalid_agent_id`, 201에 `name`과 네 필드가 `null`인 `profile`, `updated_at`이 owner 목록의 `created_at`과 같다.
- [ ] **목록 형태**: agent principal과 owner principal 모두 item에 `name`·`profile`(다섯 키 모두, 미선언은 `null`)이 있고, agent 형태에는 owner 전용 필드가 없으며, 어느 쪽에도 `pak_`·`prs_` 값이 없다(기존 `directory_never_contains_credentials` 확장). v1에서 올라온 행은 `name: null`.
- [ ] **migration v1 → v2**: v1까지만 적용한 DB에 agent 둘·메시지·inbox row·read marker·idempotency 행을 넣고 닫은 뒤 현재 코드로 열면 `schema_migrations`가 `1..=LATEST_SCHEMA_VERSION`, 기존 행과 값이 모두 그대로, 새 컬럼은 `NULL`, `GET /v1/agents`가 그 agent들을 `name: null`로 보여 주고 메시지·inbox 조회도 그대로 된다. 다시 열어도 변화 없음(멱등). `/healthz`·`platformd backup`의 `schema_version`이 `LATEST_SCHEMA_VERSION`. 미래 버전 DB 거부 테스트는 그대로 통과.
- [ ] **로그**: `log_redaction` 테스트에 profile 경로를 더해 `name`·`role`·`intro`로 넣은 표식 문자열이 TRACE 로그에 없음을 확인한다.
- [ ] **CLI**: `agent register` `--name` 없으면 exit 2, 있으면 body에 `name`; `profile set`이 준 옵션만 body에 넣고 `--clear`는 `null`; 같은 필드 설정+clear·옵션 없음·로컬 규칙 위반·`--clear name`은 exit 2; `--intro-file -`이 stdin을 읽음; `set-name`의 body가 `name` 하나; agent key `set-name`은 exit 1 `forbidden`; `show` 기본 출력·`--json` item 하나·목록에 없으면 exit 1 `agent_not_found`; `agent list` 두 시점의 열.
- [ ] **스모크**: 로컬 서버에서 `deploy/smoke.sh` 전 시나리오 PASS(새 "directory profile (13)" 포함). `bash -n` 통과.
- [ ] **UI (선택)**: 했다면 directory 표에 `NAME`·`ROLE`이 보이고 `<b>x</b>` 같은 이름이 글자 그대로 보이는 것을 브라우저로 확인(스크린샷), 콘솔 오류 0건. 안 했다면 핸드오프에 "안 함".
- [ ] **문서**: 구현 방안 8의 파일들이 실제 실행 출력과 함께 갱신됨.
- [ ] fmt/clippy/test 그린. 코드 미학(`CLAUDE.md` 규약). 기존 테스트는 `name`이 필수가 되어 바뀌는 등록 helper와 schema 버전 단언만 고치고, 다른 단언은 바꾸지 않는다.

## 구현 중 발견 (2026-10-17)
- smoke의 `--name "smoke $agent"`는 38자라 상한(32)을 넘어 첫 실행에서 실패했다. 위처럼 정정.
- serde 파생 역직렬화는 JSON 배열을 위치 필드로 읽어 `[]`가 빈 profile 갱신(200), `["x"]`가 `name` 설정으로 통했다. 테스트로 잡아 `UpdateProfileRequest`는 객체만 받게 고쳤다. **같은 구멍이 기존 요청 타입(`POST /v1/agents` 등)에도 있다** — 범위 밖이라 두었고, 별도 결함 수정(실패 테스트 먼저)으로 처리한다. → 2026-10-18 수정: `StrictJson`이 typed 역직렬화 전에 최상위 값이 객체인지 보고, 객체가 아닌 유효 JSON은 422 `invalid_field`(`details.field` 없음)로 거부한다. JSON body를 받는 7개 endpoint 모두에 적용되며, 고치기 전 6개 endpoint의 배열 body가 201/200으로 통하는 실패를 `crates/server/tests/non_object_body.rs`로 먼저 보였다(profile은 이미 거부).
- CLI 표는 문자 수로 패딩해 한글 이름이 섞인 `NAME` 열이 시각적으로 어긋난다(기존 표 코드 한계, AGENT_GUIDE §4.2에 명시).

## 검증 방법 (증거 필수)
- `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` 출력 끝부분(테스트 수 전후).
- migration: v1 DB를 만든 테스트 이름과 통과 줄. 추가로 S8 바이너리로 만든 실제 v1 DB 파일(임시 디렉터리)을 S9 `platformd`로 기동해 `/healthz` `schema_version`과 `sqlite3 … 'PRAGMA table_info(agents)'`, 기동 전후 `platformd backup`의 `row_count` 줄 비교.
- 로컬 `platformd`(임시 설정)에 대해 `platform agent register … --name …`, `profile set`·`set-name`·`show`, `agent list`(agent·owner) 실행과 exit code, `curl`로 401/403(경로)/403(`name`)/404/422(`unexpected_field`·`invalid_field`)/200 응답 원문(key는 가린다).
- `deploy/smoke.sh` 로컬 실행 표.

## 핸드오프
변경 파일, 검증 로그, DoD 항목별 테스트 이름, UI 선택 항목을 했는지, 기존 테스트에서 고친 단언 목록, 비상 해치 보고. 커밋하지 않는다.

## 비상 해치
정본대로 불가능/모순일 때만 정지 → 보고. 예: serde 구조상 `IF-WIRE-006`의 403(`name`)을 값 검사 앞에 둘 수 없을 때, SQLite `ALTER TABLE ADD COLUMN`이 v1 행을 보존하지 못할 때. 우회 구현 금지.
