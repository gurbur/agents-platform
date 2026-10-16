# 슬라이스 S8: identity-attestation

- **상태**: frozen (2026-10-16) — 설계 사용자 승인([ADR-0005](../adr/0005-identity-attestation-tokens.md)). 구현 전.
- **위험/의존도**: 중 — 서명 키라는 새 비밀과 제3자(garage-docs)와의 wire 호환이 걸린다. DB·delivery는 건드리지 않는다. S1(인증)·S5(CLI)·S7(운영 스크립트) 위에 얹는다.

## 목표
agent가 자기 `agent_api_key`로 `POST /v1/identity/tokens`를 호출하거나 `platform identity token --aud garage-docs`를 실행하면, 허브가 Ed25519로 서명한 수명 ≤900초의 JWT를 돌려준다. 그 토큰은 `platformd identity keygen`이 출력한 공개키만으로 검증되며 garage-docs의 Go 검증기(`jwt/v5`)가 받아들인다. `[identity]` 설정이 없으면 기능은 꺼져 있고 나머지 API는 그대로다.

## 범위 · 비범위
- 포함: `platform-wire`의 요청·응답 타입과 검증, `[identity]` 설정과 기동 시 키 로드, `platformd identity keygen`, `POST /v1/identity/tokens`(정확한 검사 순서), CLI `platform identity token --aud <aud> [--ttl <secs>]`(stdout = 토큰만), 테스트(아래 DoD), 예시 설정(`platformd.example.toml`, `deploy/platformd.pi.example.toml`)에 주석 처리한 `[identity]` 블록, `deploy/remote-install.sh` 운영자 안내에 keygen을 **선택** 단계로 언급, `deploy/smoke.sh`에 "identity token" 시나리오 하나(서버가 503이면 SKIP).
- 제외: 운영 Pi 배포(사용자 확인 뒤 별도), garage-docs 쪽 구현·설정, 공개키 endpoint, 토큰 폐기·replay 방지, 만료 출력 CLI 옵션, `IF-DB-001` 변경.

## 건드릴 인터페이스
`IF-API-IDENTITY`(정본 API_V1 "Identity attestation"), `IF-IDT-JWT`·`IF-IDT-VERIFY`·`IF-IDT-KEYS`(정본 [IDENTITY_ATTESTATION.md](../design/IDENTITY_ATTESTATION.md)), `IF-AUTH-002`, `IF-CFG-001`(`[identity]`), `IF-OPS-001`(keygen), `IF-WIRE-002`·`IF-WIRE-003`(전용 code), `IF-CLI-001`(stdout 규칙), `IF-ID-001`(`jti`). `IF-AUTH-001`·`IF-DB-001`·`IF-DB-002`는 소비만 한다.

## 구현 방안
wire 의미는 IDENTITY_ATTESTATION.md가 정본. **먼저 읽는다.** 이 스펙이 정하는 것:

1. **wire 타입 (`platform-wire`)**: `IdentityTokenRequest { aud: String, ttl_secs: Option<u64> }`(`deny_unknown_fields`), `IdentityTokenResponse { token, sub, aud }`(필드 정확히 셋). 오류 code 상수 `owner_not_allowed`, `identity_disabled`, `unknown_audience`, `ttl_too_long`. wire 상한 `MAX_TOKEN_LIFETIME_SECS = 900`과 skew 60초는 `IF-IDT-JWT`의 계약 상수로 이 crate에 둔다(튜닝 값 아님). `aud` 빈 문자열·`ttl_secs < 1` 검증도 여기서.
2. **설정 (`config`)**: `[identity]` 선택 절. 있으면 `signing_key_file`·`issuer`·`audiences` 필수, `default_ttl_secs`·`max_ttl_secs`는 기본값(600·900)을 상수로. 기동 거부: `issuer` 빈 문자열, `audiences` 비었거나 빈 문자열·중복, `1 ≤ default ≤ max ≤ MAX_TOKEN_LIFETIME_SECS` 위반, 알 수 없는 키, 키 파일 읽기 실패·32바이트 아님. 상대 경로는 config 파일 위치 기준. `Debug`는 seed를 가린다(owner token과 같은 방식).
3. **키 (`platformd::identity`)**: `ed25519-dalek` 3.x를 workspace 의존성으로 추가(`SigningKey::from_bytes(&[u8; 32])`). `kid` = SHA-256(raw 공개키) 앞 4바이트 소문자 hex. 기동 때 한 번 로드해 `AppState`에 둔다. 기동 로그에 `kid`·`audiences`만.
4. **인코더**: 직접 만든 compact JWS. 서명 primitive(`signing input` 바이트 → base64url 서명)를 header 조립과 분리해 RFC 8037 vector로 따로 시험할 수 있게 한다. header는 `{"alg":"EdDSA","typ":"JWT","kid":..}`(`kid` 항상), claims는 `iss, sub, aud, iat, exp, jti` 순서·공백 없음. `iat` = 현재 시각을 초로 버림, `exp = iat + ttl`, `jti` = 새 ULID(접두사 없음). 시각은 테스트가 주입할 수 있게 한다.
5. **endpoint (`api`)**: handler에 SQL 없음, Store 명령 신설 없음. 검사 순서는 IDENTITY_ATTESTATION §4.2 그대로: 401 → 403 `owner_not_allowed` → 503 `identity_disabled` → 413/415/400 → 422 `unexpected_field` → 422 `invalid_field` → 422 `unknown_audience` → 422 `ttl_too_long`(`details.max_ttl_secs`) → 201. 발급 로그는 `sub`, `jti`, `aud`, `exp`만. 토큰 원문은 어떤 level에서도 로그·오류 message에 넣지 않는다.
6. **keygen (`platformd identity keygen --out <file>`)**: OS CSPRNG 32바이트 → 새 파일(`create_new`, mode 0640을 umask와 무관하게 명시) → fsync. stdout `kid: …`, `public_key: …`(표준 base64, padding 있음) 두 줄. 기존 파일 exit 1, 사용법 exit 2. 설정을 읽지 않는다. seed를 출력하지 않는다.
7. **CLI (`platform identity token --aud <aud> [--ttl <secs>]`)**: `--ttl`이 없으면 `ttl_secs`를 생략. 성공 시 stdout에 토큰 한 줄만(`--json`이면 201 원문). 오류는 stderr, 종료 코드 `IF-CLI-001`. `platform-wire` 타입으로 입력을 먼저 검사(S5 관례).
8. **예시 설정**: `platformd.example.toml`, `deploy/platformd.pi.example.toml`에 주석 처리한 `[identity]` 블록과 "seed는 파일로만, 0640 root:platformd" 주석.
9. **운영 스크립트**: `deploy/remote-install.sh`의 `print_operator_steps`에 keygen을 **선택** 단계로 한 블록 추가(스크립트가 키를 만들지 않는다). 설정에 `[identity]`가 있으면 `check_operator_files`가 키 파일 존재를 확인하고 owner token처럼 0640 root:platformd를 강제한다. 공존 unit을 읽는 방식은 바꾸지 않는다.
10. **스모크**: `deploy/smoke.sh`에 `scenario "identity token" check_identity_token`. smoke agent의 key로 발급을 요청해 503이면 `record … SKIP "identity disabled"`, 201이면 body 키가 정확히 `token`·`sub`·`aud`이고 `sub`가 그 agent인지, owner token이 403 `owner_not_allowed`인지 본다. 토큰은 출력하지 않는다.

## 완료 정의 (DoD)
- [ ] **RFC 8037 vector**: [RFC 8037](https://www.rfc-editor.org/rfc/rfc8037) Appendix A.1의 키(`d`)로 A.4의 signing input(`{"alg":"EdDSA"}` / `Example of Ed25519 signing`)을 서명한 결과가 A.4의 JWS와 바이트 단위로 같다. A.1 `d`에서 만든 공개키가 A.1 `x`와 같다. 값은 RFC 원문에서 옮기고 출처 절을 테스트 주석에 적는다.
- [ ] **interop (garage-docs `jwt/v5`)**: 고정 seed·고정 시각으로 만든 토큰, 공개키(표준 base64), `kid`를 테스트 fixture로 남기고, garage-docs의 Go 검증 코드(`jwt/v5`, `agent_access` 설정)가 그 토큰을 받아들이며 서명 1바이트 변조·`kid` 제거·`exp` 경과(skew 밖)는 거부하는 것을 실행 기록으로 보인다. garage-docs 쪽 코드가 아직 없으면 `github.com/golang-jwt/jwt/v5`만 쓴 임시 Go 검증기(저장소에 커밋하지 않음)로 같은 검사를 돌리고, 이 항목은 garage-docs 실코드로 다시 돌릴 때까지 열어 둔다.
- [ ] **endpoint**: 401(Bearer 없음), 403 `owner_not_allowed`(owner token), 503 `identity_disabled`(`[identity]` 없는 서버, owner·agent 모두 같은 순서), 422 `invalid_field`(`aud` 누락·빈 문자열), 422 `unknown_audience`, 422 `ttl_too_long`(`details.max_ttl_secs`가 설정값), 422 `unexpected_field`(`sub` 필드), 201(body 키가 정확히 셋). 검사 순서가 섞인 요청(예: owner + 잘못된 body → 403, `[identity]` 없음 + 잘못된 body → 503)으로 순서를 확인한다.
- [ ] **검증 가능성**: 201 토큰을 keygen이 출력한 공개키(base64 디코딩)로 검증하면 성공. header의 `kid`가 keygen 출력과 같고, claims의 `sub`·`aud`·`iss`가 기대값, `0 < exp - iat ≤` 설정 `max_ttl_secs`, `--ttl` 생략 시 `exp - iat`가 설정 `default_ttl_secs`.
- [ ] **토큰이 로그에 없다**: `crates/server/tests/log_redaction.rs`(TRACE 전역 capture)에 identity 경로를 더해, 발급한 토큰 원문·seed(hex·base64 둘 다)·`agent_api_key`가 로그에 없고 발급 로그에 `jti`가 있음을 확인한다.
- [ ] **설정 경계**: `default > max`, `max > MAX_TOKEN_LIFETIME_SECS`, 빈 `audiences`, 중복 aud, 31·33바이트 키 파일, 알 수 없는 키 → 기동 거부. `[identity]` 없음 → 기동 성공·기능 비활성. 테스트는 기본값 숫자(600·900)를 단언하지 않고 주입한 값이 적용되는지만 본다. 상한 시험은 리터럴이 아니라 `MAX_TOKEN_LIFETIME_SECS` 상수를 쓴다.
- [ ] **keygen**: 새 파일이 32바이트·mode 0640, stdout 두 줄 형식, 출력 공개키가 파일 seed에서 유도한 값과 같다. 기존 파일이면 exit 1이고 원래 내용이 그대로다.
- [ ] **CLI**: `identity token` stdout이 토큰 한 줄뿐(개행 하나), `--json`은 201 원문, 서버 오류는 stderr + exit 1, `--aud` 누락은 exit 2.
- [ ] 예시 설정 두 파일의 주석 블록, `remote-install.sh` 안내(`bash -n` 통과), `smoke.sh` 시나리오가 로컬 서버(`[identity]` 있음 → PASS, 없음 → SKIP) 양쪽에서 기대대로.
- [ ] fmt/clippy/test 그린. 코드 미학(`CLAUDE.md` 규약).

## 검증 방법 (증거 필수)
- `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` 출력 끝부분.
- RFC 8037 vector 테스트 이름과 통과 줄.
- interop: Go 검증기 실행 명령과 출력 원문(통과·거부 사례 각각). 어떤 검증기(garage-docs 실코드 / 임시 `jwt/v5`)였는지 명시.
- 로컬 `platformd`(임시 디렉터리 설정, `[identity]` 있음)에 대해 `platformd identity keygen --out …` 출력, `platform identity token --aud garage-docs` 실행과 exit code, `curl`로 401/403/503/422/201 응답 원문(토큰은 앞 10자만 남기고 가린다).
- `deploy/smoke.sh` 로컬 실행 표(identity 시나리오 PASS, `[identity]` 없는 서버에서 SKIP).

## 핸드오프
변경 파일, 검증 로그, 새 의존성(`ed25519-dalek` 버전과 활성 feature, `Cargo.lock` diff 요약), DoD 항목별 테스트 이름, 비상 해치 보고. 커밋하지 않는다.

## 비상 해치
정본대로 불가능/모순일 때만 정지 → 보고. 예: garage-docs 검증기가 정본 header·claims를 거부하는데 원인이 정본 쪽에 있을 때. 우회 구현 금지.
