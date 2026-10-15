# 슬라이스 S0: skeleton-and-build-path

- **상태**: frozen (2026-10-15)
- **위험/의존도**: 상 — 빌드·배포 경로(ADR-0002)와 DB 경계(ADR-0001)라는 가장 틀리기 쉬운 가정을 먼저 드러낸다.

## 목표
`platformd`가 설정을 읽고 SQLite를 WAL/FULL로 열어 schema v1을 적용한 뒤 `GET /healthz`에 200을 주며, `platform health`가 그것을 호출한다. 같은 소스가 Docker linux/aarch64 컨테이너에서 `aarch64-unknown-linux-gnu` 바이너리로 빌드되고, 그 바이너리가 운영 Pi에서 기동해 `/healthz` 200을 돌려준다.

## 범위 · 비범위
- 포함: Cargo workspace(ADR-0003), `platform-wire`의 `AgentId`·`Seq`·오류 body 타입, `platformd`의 config/store(DB thread, migrations v1 전체 schema)/`/healthz`/graceful shutdown/`--version`, `platform`의 전역 옵션과 `health`, `deploy/build-aarch64.sh`, 통합 테스트.
- 제외: `/v1/*` 어떤 endpoint도 만들지 않는다(S1~). delivery worker 없음(S4). 정적 UI 없음(S6). systemd unit·Pi 영구 설치 없음(S7). **Pi 접속은 구현 에이전트가 하지 않는다** — Pi 기동 확인은 오케스트레이터가 수행한다.

## 건드릴 인터페이스
`IF-ID-001`, `IF-CFG-001`, `IF-DB-001`, `IF-DB-002`, `IF-OPS-001`, `IF-CLI-001`(전역 옵션·`health`·종료 코드만). 정의를 바꾸지 말고 그대로 구현한다. 바꿔야 하면 비상 해치.

## 구현 방안
형식·스키마·규칙은 `INTERFACES.md`가 정본이다. **먼저 읽는다.** 여기 다시 쓰지 않는다. 이 스펙이 정하는 것:

1. **workspace**: ADR-0003 레이아웃. 루트 `Cargo.toml`에 `[workspace.dependencies]`로 버전 일원화. `Cargo.lock` 커밋. `rust-version = "1.98"`.
2. **`platform-wire`**: `AgentId`(FromStr/Deserialize에서 검증, 예약어 거부, `Display`), `Seq(i64)`(serde string ↔ i64, 정규식·범위 검증), `ErrorBody { error: ErrorDetail { code, message, request_id, details } }`, 오류 code 문자열 상수 모듈. 단위 테스트: 유효/무효 agent_id(예약어, 대문자, 65자, 선두 숫자), Seq("0","42","007"거부,"-1"거부,"9223372036854775807" 허용, "9223372036854775808" 거부, number 타입 거부).
3. **`platformd`**:
   - `config.rs`: `IF-CFG-001`. `--config`/`PLATFORMD_CONFIG`, deny_unknown_fields, 상대 경로 해석, owner token 파일 읽기·길이 검증. 실패는 종료 코드 2와 사람이 읽는 한 줄 오류.
   - `store/mod.rs`: `IF-DB-002`의 `Store` — DB thread, bounded mpsc, `run`/`transaction`, shutdown join. `store/migrations.rs`: `IF-DB-001` PRAGMA 적용·검증, `schema_migrations`, v1 전체 schema. 두 번 열어도 멱등.
   - `api/mod.rs`: Router. `GET /healthz`(`IF-OPS-001`). 모든 응답에 `x-request-id`(`req-`+ULID) 헤더 — 이후 슬라이스의 오류 body `request_id`와 같은 값을 쓰기 위한 미들웨어를 지금 둔다.
   - `main.rs`: clap(`--config`, `--version`은 `IF-OPS-001` 형식), tracing 초기화(`RUST_LOG`, 기본 info), config → store 기동 → migrations → listen → SIGTERM/SIGINT graceful shutdown. 비밀값·body 로그 금지.
   - `build.rs`: `PLATFORMD_GIT_SHA` 환경변수가 없으면 `git rev-parse --short HEAD` 시도, 실패 시 `unknown`.
4. **`platform` CLI**: `IF-CLI-001` 전역 옵션(`--url`, `--token`, `--token-file`, `--json`), 서브커맨드 `health`. 종료 코드 규칙. reqwest `rustls` feature, 실제 feature 이름은 reqwest 최신 문서로 확정해 manifest에 명시.
5. **`deploy/build-aarch64.sh`**: ADR-0002. `docker run --rm --platform linux/aarch64 -v "$PWD":/work -w /work -v platformd-cargo-registry:/usr/local/cargo/registry rust:1-trixie bash -c 'cargo build --release --workspace --target-dir target/aarch64'` 형태. 스크립트가 끝나면 (a) 컨테이너 안 `ldd --version` 첫 줄, (b) 산출물 경로, (c) `file`과 `readelf`/`objdump`로 읽은 **요구 glibc symbol 최고 버전**을 stdout에 찍는다. `set -euo pipefail`. 호스트 식별 정보 없음.
6. **README**: "빌드·실행" 절 추가(로컬 실행 예: config 예시 파일 `platformd.example.toml`, owner token 파일 만들기 `openssl rand -base64 48 > owner.token`).

## 완료 정의 (DoD)
- [ ] `cargo build --workspace`, `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` 모두 통과(Mac host).
- [ ] 통합 테스트: 임시 디렉터리에 config·token 파일을 만들어 앱을 in-process 기동 → `GET /healthz` 200 + 명시된 body + `x-request-id` 헤더. 같은 DB를 두 번 열어도 성공(멱등 migration). 연결의 `journal_mode`=`wal`, `synchronous`=2(FULL)인지 PRAGMA로 확인. `schema_migrations`에 version 1 한 행. v1 테이블 9개가 `sqlite_master`에 존재.
- [ ] config 오류(알 수 없는 키, 토큰 32B 미만, 파일 없음) 각각 비정상 종료 + 코드 2 (프로세스 테스트 또는 함수 단위 테스트).
- [ ] `platform --url http://127.0.0.1:PORT --token x health` → `/healthz` 결과 출력, exit 0. 서버 없으면 exit 3.
- [ ] `deploy/build-aarch64.sh` 가 Mac에서 성공하고 `target/aarch64/aarch64-unknown-linux-gnu/release/platformd`가 생긴다. `file` 출력이 `ELF 64-bit LSB ... ARM aarch64 ... dynamically linked`, 요구 glibc 최고 버전 ≤ 2.41.
- [ ] 코드 미학: 모듈 분리, 가드절, `unwrap` 사유 주석, 로그 비밀 없음.
- [ ] (오케스트레이터 수행) 산출 바이너리를 Pi `/tmp` 아래 복사해 `--version` 출력과 임시 config로 기동 → `curl /healthz` 200 → 종료·삭제. 이 결과로 ADR-0002 상태를 "채택됨"으로 바꾸고 TECH 문서 "개발과 ARM64 빌드" 절의 "구현 단계에서 확인" 문구를 실측으로 갱신한다.

## 검증 방법 (증거 필수)
- 빌드/린트/테스트: 위 네 명령의 실제 출력(요약 아닌 마지막 수십 줄).
- 스모크(Mac): `cargo run -p platformd -- --config /tmp/.../platformd.toml` 기동 로그 → `curl -i http://127.0.0.1:7340/healthz` 출력 → `platform health` 출력 → SIGINT 후 "shutdown complete" 로그.
- 컨테이너 빌드: 스크립트 전체 stdout(ldd 버전, 산출 경로, glibc 요구 버전).
- **위 증거 없이 "완료" 보고 불가.**

## 핸드오프 (구현 에이전트가 돌려줄 것)
- 변경 파일 목록, 위 검증 로그, 선택한 crate 버전·feature 목록(특히 reqwest·rusqlite·axum·tokio), 비상 해치 보고(있으면). **커밋은 하지 않는다**(오케스트레이터 리뷰 뒤 별도 위임).

## 비상 해치
스펙·INTERFACES대로 구현이 명백히 불가능하거나 서로 모순일 때만: 정지 → 보고 → 정본 정정 → 재개. 우회 금지. 예: rust:1-trixie 이미지에 aws-lc-rs 빌드 도구가 없어 실패하면 이미지를 바꾸지 말고 보고한다(어떤 패키지가 없는지 포함).
