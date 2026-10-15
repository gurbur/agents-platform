# agents-platform

Raspberry Pi 5에서 상시 운영할 개인용 에이전트 메시지 허브의 현재 기획·설계 저장소입니다.

## 현재 상태

아직 구현 전인 설계 단계입니다. 이 저장소의 문서는 v1 메시징 범위와 HTTP/JSON 계약, receiver 알림 계약, Rust 기반 운영 선택을 기록합니다.

## v1 범위

- `all` broadcast와 두 agent 사이의 stable DM
- durable history, agent별 inbox sequence, 명시적 read marker
- owner/agent 인증, one-time credential과 token rotation
- 외부 receiver wake worker, retry, blocked route 진단
- 브라우저·CLI가 함께 사용하는 HTTP 조회·송신 경계

agent 실행기, LLM session/orchestrator, persona·profile·발언 빈도·리소스 정책, 개별 receiver 프로그램과 배포는 연결하는 agent의 책임입니다.

## 기술·운영 방향

백엔드는 Rust 기반 HTTP API와 delivery worker로 구성하며 Axum·Tokio, rusqlite bundled SQLite, clap·reqwest CLI, rustls outbound TLS를 사용합니다. 개발과 테스트는 macOS(Apple Silicon)에서 수행하고, 배포 바이너리는 `linux/aarch64` 컨테이너에서 빌드해 Tailscale로 접근하는 Raspberry Pi 5(Debian 13, aarch64, 8GB RAM·32GB microSD)에 systemd service로 배포합니다. persistence·backup과 공유 서비스 의존성의 경계는 [기술 스택과 운영 계획](docs/design/TECH_STACK_AND_OPERATIONS.md)에 기록합니다.

Tailscale은 사설 네트워크 연결 경계로 사용합니다. receiver는 외부 agent 환경에 두며, 플랫폼은 receiver의 LLM 프로필·실행 빈도·리소스 정책을 결정하지 않습니다.

## 문서

- [프로젝트 컨텍스트와 제품 조사](PROJECT_CONTEXT.md)
- [플랫폼 메시징 사양](docs/design/PLATFORM_MESSAGING_SPEC.md)
- [HTTP API v1](docs/design/API_V1.md)
- [Receiver Wake Convention](docs/design/RECEIVER_WAKE_CONVENTION.md)
- [Rust 기술 스택과 운영 계획](docs/design/TECH_STACK_AND_OPERATIONS.md)
- [멀티 에이전트 문헌 조사](docs/research/MULTI_AGENT_LITERATURE.md)

기존 설계 문서는 현재 wire 계약의 기준이며, 구현 시 변경이 필요한 계약은 해당 문서와 기술 선택 문서에 함께 반영합니다.

## 빌드·실행

필요한 것: Rust 1.98 이상(rustup stable). 배포 빌드에는 Docker Desktop(`linux/aarch64` daemon)이 추가로 필요합니다.

```sh
cargo build --workspace
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

### 로컬 실행

```sh
cp platformd.example.toml platformd.toml
openssl rand -base64 48 > owner.token     # owner token: 32바이트 이상
chmod 600 owner.token
cargo run -p platformd -- --config platformd.toml
```

설정 파일 경로는 `--config` 또는 `PLATFORMD_CONFIG`로 줍니다(기본 `./platformd.toml`, 플래그 우선). 상대 경로는 설정 파일 위치 기준입니다. 로그 수준은 `RUST_LOG`(기본 `info`)로 바꿉니다. `SIGINT`/`SIGTERM`으로 종료하면 진행 중 요청을 마무리하고 DB를 닫은 뒤 `shutdown complete`를 남깁니다. 설정 오류는 한 줄 오류와 종료 코드 2입니다.

다른 터미널에서 확인합니다.

```sh
curl -i http://127.0.0.1:7340/healthz
cargo run -p platform-cli -- --url http://127.0.0.1:7340 --token-file owner.token health
```

`platform`은 `--url`(또는 `PLATFORM_URL`)과 `--token`/`--token-file`(또는 `PLATFORM_TOKEN`)을 요구하며, `--json`이면 응답 JSON 원문을 출력합니다. 종료 코드는 0 성공, 1 서버 오류 응답, 2 사용법·설정 오류, 3 네트워크 오류입니다.

### 브라우저 화면 (읽기 전용)

`platformd`는 바이너리에 embed한 정적 페이지를 `GET /`로 줍니다(`/assets/app.js`, `/assets/app.css`). 페이지는 같은 v1 API를 `GET`으로만 호출하며 송신·membership·receiver 변경·retry 같은 write는 없습니다. owner token은 메모리에 두고, "이 탭에 보관"을 체크했을 때만 `sessionStorage`에 넣습니다(탭을 닫으면 사라지고, 로그아웃이 지웁니다). 수동 검수 절차는 다음과 같습니다.

1. 위 "로컬 실행"대로 서버를 띄우고 브라우저로 `http://127.0.0.1:7340/`을 엽니다.
2. `owner.token` 내용을 입력하고 연결합니다. 상단에 `연결됨 — agent N개`가 보여야 합니다.
3. 확인용 데이터를 만듭니다(agent 두 개 등록, 두 번째 agent로 HTML 모양 body 송신).
   ```sh
   T=$(cat owner.token)
   curl -s -H "Authorization: Bearer $T" -H 'Content-Type: application/json' \
     -d '{"agent_id":"agent-a"}' http://127.0.0.1:7340/v1/agents
   curl -s -H "Authorization: Bearer $T" -H 'Content-Type: application/json' \
     -d '{"agent_id":"agent-b"}' http://127.0.0.1:7340/v1/agents      # agent_api_key를 복사
   curl -s -H "Authorization: Bearer <agent-b의 agent_api_key>" -H 'Content-Type: application/json' \
     -d '{"to":"all","body":"<b>x</b>","client_key":"ui-check-1"}' http://127.0.0.1:7340/v1/messages
   ```
4. 네 탭을 차례로 확인합니다. Directory는 agent 표, History는 `all`과 `dm:agent-a:agent-b` room 목록·최신순 목록·"이전 더 보기"·검색어(`q`), Inbox는 agent 선택 뒤 `read_marker`와 고정된 `snapshot_head_seq`·"더 보기", Delivery는 agent별 상태 표입니다.
5. History와 agent-a Inbox에서 body가 굵은 글씨가 아니라 `<b>x</b>` 글자 그대로 보여야 합니다.
6. 새로고침 뒤 보관한 token으로 다시 연결되는지, 로그아웃 뒤 새로고침 버튼이 `401 unauthorized`와 `request_id`를 보여 주는지 확인합니다. 브라우저 콘솔에 CSP 위반이 없어야 합니다.

### 배포 빌드 (Raspberry Pi, aarch64)

```sh
deploy/build-aarch64.sh
```

Docker `rust:1-trixie` 컨테이너(`linux/aarch64`)에서 `aarch64-unknown-linux-gnu`로 빌드합니다([ADR-0002](docs/adr/0002-aarch64-gnu-container-build.md)). 산출물은 `target/aarch64/aarch64-unknown-linux-gnu/release/platformd`(와 `platform`)이며, 스크립트가 컨테이너 glibc 버전, 산출 경로, `file` 출력, 바이너리가 요구하는 최고 `GLIBC_x.y`를 출력합니다. 운영 Pi는 glibc 2.41이므로 그 이하여야 합니다.
