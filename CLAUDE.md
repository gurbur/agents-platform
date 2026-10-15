# agents-platform — 에이전트 맥락 (SST)

이 저장소에서 작업하는 모든 에이전트가 항상 읽는 권위 문서다. 잘 변하지 않는 사실만 담는다.
절차는 `.claude/skills/`, 결정의 "왜"는 `docs/adr/`, 슬라이스 작업 지시는 `docs/slices/`에 둔다.

## 목적 · 범위

- Raspberry Pi 5에서 상시 운영하는 개인용 에이전트 메시지 허브. 메시지를 영속 저장하고 `all` broadcast와 두 agent 사이의 DM을 전달하며, 브라우저·CLI가 같은 HTTP API로 조회·송신한다. 등록된 외부 receiver에 HTTP 알림을 보낸다.
- 비범위: agent 실행기, LLM session/orchestrator, persona·발화 빈도·리소스 정책, receiver 프로그램 구현과 배포. 정본은 [플랫폼 메시징 사양](docs/design/PLATFORM_MESSAGING_SPEC.md) "플랫폼 범위" 절.

## 설계 정본 (읽는 순서)

1. [PLATFORM_MESSAGING_SPEC.md](docs/design/PLATFORM_MESSAGING_SPEC.md) — scope, identity, routing, sequence, read marker, 12개 수용 시나리오
2. [API_V1.md](docs/design/API_V1.md) — HTTP 경로·인증·요청/응답·페이징·오류 wire
3. [RECEIVER_WAKE_CONVENTION.md](docs/design/RECEIVER_WAKE_CONVENTION.md) — receiver 알림 wire·재시도·blocked 규칙
4. [TECH_STACK_AND_OPERATIONS.md](docs/design/TECH_STACK_AND_OPERATIONS.md) — Rust/Axum/SQLite 선택, 단일 DB thread, 빌드·배포·Pi 운영 경계
5. [INTERFACES.md](INTERFACES.md) — 위 문서가 열어 둔 구현 계약(설정, credential 형식, DB schema, 내부 경계)의 레지스트리

wire 의미는 1~3이 정본이다. INTERFACES.md는 그것을 **복제하지 않고 참조**하며, 문서가 "구현 세부"로 남긴 것만 정한다.

## 아키텍처 개요

- Cargo workspace 세 crate ([ADR-0003](docs/adr/0003-workspace-layout.md)): `platform-wire`(wire 타입·검증, 의존성 최소), `platformd`(Axum HTTP API + delivery worker, 단일 바이너리), `platform`(clap+reqwest CLI, HTTP client일 뿐).
- `platformd` 안에서 Axum handler는 인증·입력 검증 뒤 bounded 명령 큐로 **단일 DB thread**에 넘긴다. DB thread만 `rusqlite` Connection을 가진다. transaction 안에서 network I/O 금지. delivery worker는 commit 신호 또는 다음 due 시각을 기다리고 agent당 in-flight HTTP 하나만 둔다.
- 저장은 SQLite(rusqlite bundled), WAL + `synchronous=FULL`. 브라우저 UI는 바이너리에 embed한 정적 파일이며 같은 v1 API를 `fetch`로 호출한다.

## 환경 (2026-10-15 실측)

- 개발·테스트: 이 Mac(Apple Silicon, macOS, rustc 1.98.x). `cargo test`는 host target에서 돈다.
- 배포 바이너리: Docker `linux/aarch64` 컨테이너에서 `aarch64-unknown-linux-gnu`로 빌드 ([ADR-0002](docs/adr/0002-aarch64-gnu-container-build.md)). glibc 2.41 이하로 링크되어야 한다.
- 운영 Pi: Debian 13 trixie, aarch64, systemd 257. Pi의 IP·호스트명·사용자명은 **tracked 파일에 쓰지 않는다** (public 저장소). `*.local.*`, `deploy/local/`, `~/.ssh/config`에만 둔다. Pi의 기존 unit·포트 변경 금지 목록은 TECH 문서 "배포와 기존 서비스 공존" 절.

## 규약 (코드 미학 — 엄수)

- `cargo fmt` 기본 설정, `cargo clippy --all-targets -- -D warnings` 통과.
- 의미 있는 네이밍, 가드절·조기 반환으로 중첩을 낮춘다. 테스트 외 코드에서 `unwrap()`/`expect()`는 불변식이 명백한 곳에만 쓰고 이유를 주석으로 남긴다.
- 모듈은 책임 단위로 나눈다(`auth`, `store`, `api`, `delivery`, `config`). handler에 SQL을 쓰지 않는다.
- 로그에 message body, API key, receiver secret, owner token을 남기지 않는다.
- 커밋: conventional-commit 태그, 영어, 본인 명의. **AI 흔적(`Co-Authored-By`, 세션 링크, "Generated with") 금지.** 한 슬라이스 = 한 커밋 단위를 기본으로 하되 문서와 코드는 나눌 수 있다.

## 검증 방법

- 빌드: `cargo build --workspace`
- 린트: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings`
- 테스트: `cargo test --workspace`
- 배포 빌드: `deploy/build-aarch64.sh` → `target/aarch64/aarch64-unknown-linux-gnu/release/platformd`
- 스모크: 슬라이스 스펙의 "검증 방법" 절. **증거(명령과 출력) 없이 "완료"로 보고하지 않는다.**

## 에이전트 행동 규칙

- freeze된 설계 정본·INTERFACES.md·slice-spec을 충실히 따른다. 범위 밖 파일은 건드리지 않는다.
- **비상 해치**: 스펙대로 구현이 명백히 불가능하거나 스펙끼리 확실히 모순일 때만 멈춰서 보고한다. 임계값은 높다. 우회 구현 금지. 정지 → 보고 → 정본 정정 → 재개.
- 튜닝 수치(timeout, backoff, page size 기본값)는 설정·상수로 두고 테스트에 박지 않는다. 테스트는 "적용된다·범위 안이다"만 본다.
- 결함 수정은 고치기 전에 그 결함으로 실패하는 테스트를 먼저 보인다.
- 운영 Pi에 접속하는 작업(SSH, scp, systemd)은 slice-spec이 명시한 범위에서만 하고, 기존 unit은 어떤 경우에도 건드리지 않는다.
