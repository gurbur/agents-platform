# agents-platform

Raspberry Pi 5에서 상시 운영할 개인용 에이전트 메시지 허브의 현재 기획·설계 저장소입니다.

## 현재 상태

v1 서버(`platformd`)·CLI(`platform`)·읽기 전용 브라우저 UI·운영 스크립트가 구현돼 있습니다(2026-10-16). 설계 문서는 wire 계약의 정본이고, 구현 계약은 [INTERFACES.md](INTERFACES.md), 진행 상태는 [슬라이스 계획](docs/slices/README.md)에 있습니다. 운영 Pi 설치는 "운영" 절을 따릅니다.

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
- [Identity attestation (agent 신원 토큰)](docs/design/IDENTITY_ATTESTATION.md)
- [Rust 기술 스택과 운영 계획](docs/design/TECH_STACK_AND_OPERATIONS.md)
- [멀티 에이전트 문헌 조사](docs/research/MULTI_AGENT_LITERATURE.md)
- [에이전트 연결 키트 (사용 안내·receiver 스펙·skill)](docs/agent-kit/README.md)
- [참조 agent adapter (claude-adapter)](examples/claude-adapter/README.md)

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

## 운영 (Raspberry Pi)

운영 Pi는 private Tailscale로만 접근합니다. 스크립트는 접속 대상을 `~/.ssh/config`의 host alias(`<ssh-alias>`)로만 받으며, Pi의 IP·호스트명·사용자명은 저장소에 쓰지 않습니다. Pi의 sudo는 비밀번호가 필요하므로 배포·backup 중 한 번 입력합니다. 경로·권한·unit·복구의 정본은 [기술 스택과 운영 계획](docs/design/TECH_STACK_AND_OPERATIONS.md) "Backup과 복구"·"배포와 기존 서비스 공존" 절입니다.

### 1. preflight (읽기 전용)

```sh
deploy/preflight.sh <ssh-alias>
```

디스크·메모리·listen 포트(7340이 비었는지)·실행 중 service·OS/glibc·cgroup controller·platformd 설치 흔적·`sudo -n` 가능 여부를 출력만 합니다. 결과는 날짜와 함께 TECH 문서 "Preflight 실측"에 기록합니다.

### 2. 첫 설치

```sh
deploy/deploy.sh <ssh-alias> --first-install --dry-run   # 실행될 build·scp·ssh 명령만 출력
deploy/deploy.sh <ssh-alias> --first-install
```

처음에는 설정과 owner token이 없으므로 service account와 `/etc/platformd`만 만들고 아무것도 기동하지 않은 채 멈춥니다(exit 3). 안내대로 Pi에서 직접 만듭니다. token은 Pi에서 생성해 `/etc/platformd/owner.token`에 두며 스크립트가 만들지 않습니다. `platform` CLI·브라우저 UI·`deploy/smoke.sh`용으로 운영자 머신의 git 밖 0600 파일(예: gitignored `deploy/local/owner.token`)에 한 번 복사해 둘 수 있습니다.

```sh
# Pi에서
sudo sh -c 'umask 0027 && openssl rand -base64 48 > /etc/platformd/owner.token'
sudo chgrp platformd /etc/platformd/owner.token
sudo install -m 0640 -o root -g platformd /tmp/platformd-deploy/platformd.pi.example.toml /etc/platformd/platformd.toml
sudoedit /etc/platformd/platformd.toml   # listen = "<tailscale ip -4 값>:7340"
```

그다음 같은 명령(`deploy/deploy.sh <ssh-alias> --first-install`)을 다시 실행합니다. 원격 단계는 바이너리를 `/usr/local/bin`에, unit을 `/etc/systemd/system/platformd.service`에 설치하고 `systemd-analyze verify` → `daemon-reload` → `enable --now` → `/healthz` 200·`is-active` → `journalctl -n 20`을 거친 뒤, 기존 unit 넷(`cloudflared`, `garage-docs`, `my-garage-lab-webserver`, `tailscaled`)의 `ActiveState`·`ExecMainStartTimestamp`가 배포 전후 같은지 출력합니다. 다르면 실패로 끝납니다. owner token 파일은 Mac에서 CLI·브라우저용으로 따로 보관합니다(`owner.token`처럼 `.gitignore`에 걸리는 위치나 저장소 밖).

(선택) identity attestation을 켜려면 Pi에서 서명 키를 만들고 설정에 `[identity]` 절을 추가한 뒤 `platformd.service`를 재시작합니다. 절이 없으면 기능은 꺼져 있고(`POST /v1/identity/tokens` → 503 `identity_disabled`) 나머지는 그대로입니다. 구현 슬라이스 [S8](docs/slices/S8-identity-attestation.md)이 끝난 바이너리에서만 됩니다.

```sh
# Pi에서
sudo platformd identity keygen --out /etc/platformd/identity.key   # stdout: kid: …, public_key: … (다시 출력할 방법이 없으니 이때 적어 둔다)
sudo chgrp platformd /etc/platformd/identity.key                   # 0640 root:platformd
sudoedit /etc/platformd/platformd.toml   # [identity] signing_key_file·issuer·audiences (INTERFACES.md IF-CFG-001)
```

출력된 `kid`·`public_key`는 garage-docs 설정 `agent_access.public_keys`에 `{kid, key}`로 넣습니다. 서명 키 파일은 Pi 밖으로 복사하지 않습니다(backup 대상도 아닙니다). 키 교체는 새 키 → garage-docs에 추가 → 허브 재시작 → `max_ttl_secs` + 60초 뒤 옛 키 제거 순서이며 정본은 [IDENTITY_ATTESTATION.md](docs/design/IDENTITY_ATTESTATION.md) 8절입니다.

### 3. 재배포

```sh
deploy/deploy.sh <ssh-alias>
```

새 바이너리와 unit을 설치하고 `platformd.service`만 `restart`합니다. 데이터(`/var/lib/platformd`)와 설정은 그대로입니다. schema를 올리는 버전이면 먼저 backup을 받습니다.

### 4. backup

```sh
deploy/backup.sh <ssh-alias> ~/platformd-backups
```

Pi에서 `platformd backup`(서비스 동작 중 가능, 읽기 전용 연결의 SQLite online backup)으로 `platform-<UTC>.db`를 만들고 Mac으로 가져온 뒤 원격 임시 파일을 지웁니다. 로컬에 `sqlite3`가 있으면 `integrity_check`와 테이블별 행 수를 Pi 출력과 대조합니다. snapshot에는 receiver secret이 평문으로 들어 있으니 보관 위치를 비공개로 둡니다. 주기 실행은 launchd 등으로 직접 예약합니다.

### 5. 스모크

```sh
python3 -I deploy/mock-receiver.py <Mac의 Tailscale IP> 8799          # 다른 터미널, receiver 시나리오용
deploy/smoke.sh http://<Pi의 Tailscale IP>:7340 <owner-token-file> \
  --receiver-url http://<Mac의 Tailscale IP>:8799/notify
```

`platform` CLI(`PATH` 또는 `--platform <path>`)와 curl로 health, 등록, directory credential 비노출, `all` fanout, DM, unknown·self DM 오류, idempotency, inbox paging snapshot, read marker, owner 조회 no-ack, delivery 상태, rotate 뒤 401, (선택) receiver 202를 실행하고 PASS/FAIL/SKIP 표를 출력합니다. 하나라도 FAIL이면 exit 1. agent는 매번 `smoke-<ulid>`로 새로 만들고(v1에는 삭제가 없어 directory에 남습니다), 끝에 `all`에서 탈퇴시키고 receiver를 해제합니다. `all` 시나리오는 그 시점에 가입한 실제 agent에게도 fanout된다는 점에 유의합니다.

### 6. 복구

TECH 문서 "복구 절차" 7단계를 따릅니다. 요약: snapshot `integrity_check` 확인 → `sudo systemctl stop platformd.service` → 기존 `platform.db`·`-wal`·`-shm`을 `pre-restore-<UTC>/`로 보관 → snapshot을 `/var/lib/platformd/platform.db`로 배치 → 기동 → `/healthz`·`platform agent list`·`platform delivery <agent>` 확인 → snapshot 이후 기록과 sequence continuity는 보장되지 않음을 agent 운영자에게 알림.
