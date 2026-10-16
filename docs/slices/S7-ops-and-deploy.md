# 슬라이스 S7: ops-and-deploy

- **상태**: done (2026-10-16) — 산출물: 245 tests, smoke 14 PASS(로컬), Docker 가짜 Pi에서 remote-install 경로 검증. 실배포: 사용자 확인 뒤 운영 Pi에 첫 설치, 실배포본 대상 acceptance 스모크 14/14 PASS([실배포 기록](#실배포-기록-2026-10-16)). 구현 판단: `sudo bash -s < script`는 `-t`와 양립하지 않아 staging에 올린 스크립트를 `ssh -t … sudo bash <path>`로 실행; unit에 `StartLimitIntervalSec=0`·`RestartPreventExitStatus=2`·`UMask=0027` 추가.
- **위험/의존도**: 상 — 운영 머신에 영구 상태(service account, unit, 데이터 디렉터리)를 만든다. 기존 서비스 공존 조건이 걸려 있다.

## 목표
`deploy/deploy.sh <ssh-alias>` 한 번으로 Mac에서 빌드한 바이너리가 운영 Pi에 systemd 서비스로 설치·기동되고, Tailscale 안의 Mac 브라우저·CLI에서 접근된다. backup·복구 절차와 preflight 측정이 저장소에 남는다. 12개 수용 시나리오를 실배포본에 대해 재실행한다.

## 범위 · 비범위
- 포함: preflight 스크립트, systemd unit·하드닝·resource control, service account·디렉터리·권한, 설정·owner token 배치 절차, 배포 스크립트, `platformd backup` 서브커맨드(rusqlite online backup API), backup 전송·복구 절차 문서, acceptance 스모크 스크립트, TECH 문서의 "구현 단계에서 정한다" 항목 갱신.
- 제외: Cloudflare Tunnel·기존 unit·포트 변경(절대 금지), public exposure, 자동 backup 스케줄러(절차와 명령만 제공; 스케줄은 사용자가 Mac launchd 등으로).

## 건드릴 인터페이스
`IF-CFG-001`, `IF-OPS-001`(`platformd backup` 추가 — 아래 표기), TECH 문서 "배포와 기존 서비스 공존"·"Backup과 복구"·"Migration과 운영 변경".

## Preflight 실측 (2026-10-15T15:36Z, 오케스트레이터가 읽기 전용 SSH로 수행)
- Debian 13 trixie aarch64, kernel 6.18.50+rpt-rpi-2712, 4 CPU, RAM 8062 MiB(사용 325, 가용 7737), swap 2047 MiB, load 0.00.
- `/` 29G 중 4.2G 사용·24G 여유(microSD). `/tmp`는 tmpfs 4.0G(스테이징에 사용 가능). journald 사용량 8M.
- listen 포트: 22, 8080(0.0.0.0), 8787·20241(127.0.0.1), tailscaled 2개(tailscale0). **7340 비어 있음.**
- 실행 중 변경 금지 unit: `cloudflared.service`, `garage-docs.service`, `my-garage-lab-webserver.service`, `tailscaled.service`.
- `platformd` 사용자·`/etc/platformd`·`/var/lib/platformd`·`/usr/local/bin/platformd`·unit 모두 **없음**(깨끗한 설치 대상).
- **`sudo -n true` 실패 — sudo에 비밀번호가 필요하다.** 따라서 `deploy.sh`는 `ssh -t`로 TTY를 확보하고 sudo 비밀번호 입력을 사람이 하도록 설계한다. 비대화형 자동 배포는 v1 범위 밖. `sudoers` 변경은 하지 않는다.
- 도구: `systemd-analyze`, `curl`, `install` 있음. `useradd`는 `/usr/sbin`(sudo 경로).
- Tailscale 인터페이스 IPv4 /32 존재(값은 저장소에 적지 않음).

## 구현 방안
1. **preflight (`deploy/preflight.sh <alias>`)**: SSH로 `df -h`, `free -h`, `ss -ltn`, 실행 중 unit, `nproc`, OS/glibc를 읽어 출력만 한다(변경 없음). 결과를 TECH 문서 preflight 절에 날짜와 함께 기록.
2. **unit (`deploy/platformd.service`)**: `Type=simple`, `User=platformd`, `Group=platformd`, `ExecStart=/usr/local/bin/platformd --config /etc/platformd/platformd.toml`, `Restart=on-failure`, `RestartSec=2`, `KillSignal=SIGTERM`, `TimeoutStopSec=20`(DRAIN 10s + store join 5s + 여유). 하드닝: `NoNewPrivileges=yes`, `ProtectSystem=strict`, `ProtectHome=yes`, `PrivateTmp=yes`, `ReadWritePaths=/var/lib/platformd`, `ProtectKernelTunables=yes`, `RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX`. resource control은 preflight 결과로 `MemoryMax`(초안 512M), `CPUQuota`(초안 100%), `TasksMax`(초안 256)를 두되 수치는 unit 파일에만 두고 테스트에 박지 않는다. 로그는 journald(stderr), `LogRateLimit`는 기본.
3. **배치**: `/usr/local/bin/platformd`(0755 root), `/etc/platformd/platformd.toml`(0640 root:platformd), `/etc/platformd/owner.token`(0640 root:platformd — 사용자가 Pi에서 `openssl rand -base64 48`로 생성), `/var/lib/platformd/`(0750 platformd:platformd, `db_path = "/var/lib/platformd/platform.db"`). `listen`은 Pi의 **Tailscale 인터페이스 IP:7340**으로 설정 파일에만 적는다(저장소에는 예시 `100.x.y.z:7340` 자리표시). 포트 7340은 2026-10-15 실측 사용 중 목록(22, 8080, 8787, 20241)과 겹치지 않는다.
4. **deploy.sh**: `build-aarch64.sh` → `scp` 바이너리·unit을 `/tmp` → `ssh -t <alias> sudo …`(비밀번호는 사람이 입력; 원격 단계는 하나의 `sudo bash -s` 스크립트로 묶어 입력을 한 번만 받는다) `install …`, `systemctl daemon-reload`, `systemctl enable --now platformd` (처음) 또는 `restart`, `systemctl is-active`, `curl http://<listen>/healthz` 200, `journalctl -u platformd -n 20`. 다른 unit에는 어떤 systemctl도 호출하지 않는다(스크립트가 unit 이름을 상수 `platformd.service`로만 쓴다). alias·주소는 인자/로컬 설정으로만.
5. **backup**: `platformd backup --config <toml> --out <file>` — rusqlite `backup::Backup`으로 온라인 일관 snapshot을 쓴다(서비스 동작 중 가능, WAL). `deploy/backup.sh <alias> <local-dir>`가 Pi에서 그 명령을 실행하고 `scp`로 Mac에 가져온다(파일명에 UTC 시각). 복구 절차(문서): 서비스 중지 → 기존 DB·WAL·SHM 보관 → snapshot 복사 → 기동 → `/healthz`·`GET /v1/agents`·delivery 상태 확인 → "sequence continuity는 보장하지 않음" 경고.
6. **acceptance (`deploy/smoke.sh <base-url> <owner-token-file>`)**: 12개 시나리오 중 외부에서 재현 가능한 것(등록, all·DM 송신, idempotency, inbox paging, read marker, owner no-ack, rotate 뒤 401, delivery 상태)을 curl 또는 CLI로 실행하고 결과를 표로 출력. receiver 알림(5~9)은 Mac에서 임시 mock receiver를 띄워 Pi가 Tailscale로 호출하게 한다.
7. **문서 갱신**: TECH 문서 "SQLite 내구성과 저장 공간" preflight 실측, "Backup과 복구" 절차, "배포와 기존 서비스 공존"의 실제 unit·경로. README에 운영 절 추가.

## 완료 정의 (DoD)
- [x] preflight 출력이 문서에 기록됨.
- [x] unit 파일이 `systemd-analyze verify`(Pi에서) 경고 없음. — 2026-10-16 첫 설치에서 `no messages`.
- [x] **사용자 확인 뒤** `deploy.sh`로 설치·기동, `is-active`, `/healthz` 200, Mac 브라우저에서 UI 접근, CLI `health` 성공. — 2026-10-16: `is-active: active`, `NRestarts=0`, `/healthz` 200 `{"status":"ok","schema_version":1}`(Pi에서, Mac에서 Tailscale로 모두), 설치 바이너리 sha256이 로컬 aarch64 빌드와 일치, Mac 브라우저에서 Tailscale로 `http://<tailscale-ip>:7340/`을 열어 owner token 입력 뒤 "연결됨 — agent 3개", Directory 탭에 smoke agent 3개(delivery.state `no_receiver`), CLI `health`는 스모크 첫 시나리오로 PASS.
- [x] 기존 unit 네 개의 `ActiveState`·`ExecMainStartTimestamp`가 배포 전후 동일(배포가 건드리지 않았다는 증거). — 2026-10-16 첫 설치에서 `unchanged`.
- [x] `platformd backup` 산출 파일을 Mac에서 열어 `PRAGMA integrity_check`=ok, 테이블·행 수 일치. — 2026-10-16 13:50 KST `deploy/backup.sh <ssh-alias> deploy/local/backups`: `platform-20261016T045010Z.db`(86016 bytes, gitignored), 로컬 sqlite3 `integrity_check` = ok, 9개 테이블 행 수가 Pi 출력과 일치.
- [x] acceptance 스모크 전체 통과 로그. — 2026-10-16 실배포본 대상 14/14 PASS(receiver 시나리오 포함).
- [x] 재배포(두 번째 `deploy.sh`)가 데이터를 보존하고 무중단에 가깝게 재시작. — 2026-10-16 backup 직후 `deploy/deploy.sh <ssh-alias>`(같은 바이너리·checksum): graceful shutdown 뒤 새 PID로 기동, `/healthz` 200, `NRestarts=0`, 공존 unit 불변, `platform agent list`에 smoke agent 3개 그대로.

## 실배포 기록 (2026-10-16)

사용자 확인 뒤 운영 Pi에 첫 설치했다. 로그는 IP·호스트명·사용자명을 가린 사본에서 옮겼다.

- **배포본**: `platformd 0.1.0 (ceae705)`. Pi의 `/usr/local/bin/platformd` sha256 = 로컬 aarch64 빌드(`7453dfc1…a875d86`), `/usr/local/bin/platform` = `21925e0e…b7dc8b6f`. 바이너리 요구 glibc `GLIBC_2.38` ≤ Pi glibc 2.41.
- **흐름**: `deploy/deploy.sh <ssh-alias> --first-install` 첫 실행이 설계대로 service account와 `/etc/platformd`만 만들고 exit 3으로 멈춤 → 운영자 파일(`owner.token`은 `openssl rand -base64 48`, `platformd.toml`은 예시에서 복사해 `listen = <tailscale-ip>:7340`)을 만들고 곧바로 `remote-install.sh --first-install`을 다시 실행. owner token은 CLI·브라우저 UI·스모크용으로 운영자 Mac의 gitignored `deploy/local/`에 한 번 복사(0600). 복사에 쓴 Pi 쪽 일회용 사본은 scp 직후 운영자 script·staging 디렉터리와 함께 삭제했다(부재 확인).
- **편차**: 운영자 단계를 README의 `sudo …`·`sudoedit` 수작업 대신 작은 운영자 script로 **sudo 세션 하나**에서 실행했다. 결과 파일과 권한(0640 root:platformd)은 같고, 원격 script가 그 권한을 다시 강제했다.

설치 로그 발췌:

```text
== [pi] installed: platformd 0.1.0 (ceae705)
== [pi] systemd-analyze verify: no messages
== [pi] systemctl enable --now platformd.service
== [pi] healthz 200: {"status":"ok","schema_version":1}
== [pi] is-active: active
INFO platformd::store::migrations: applied schema migration version=1
INFO platformd: database ready db_path=/var/lib/platformd/platform.db schema_version=1
INFO platformd::delivery::worker: delivery worker started
INFO platformd: listening addr=<tailscale-ip>:7340
== [pi] coexisting units: unchanged (same ActiveState and ExecMainStartTimestamp as before)
```

acceptance 스모크: Mac에서 `deploy/smoke.sh http://<tailscale-ip>:7340 <owner-token-file> --receiver-url …`, receiver는 Mac의 Tailscale 주소에서 듣는 `deploy/mock-receiver.py`. 결과 `smoke: all PASS`.

| 시나리오 | 결과 | 비고 |
| --- | --- | --- |
| healthz / platform health | PASS | |
| register (one-time credentials) | PASS | |
| directory hides credentials | PASS | |
| all fanout (1) | PASS | |
| DM recipient-only (2) | PASS | |
| unknown 404 / self DM 422 (2) | PASS | |
| idempotency 200 / 409 (4) | PASS | |
| inbox paging snapshot (10) | PASS | 5 rows in pages of 2 |
| read marker ahead/no-op (10) | PASS | |
| owner reads do not ack (12) | PASS | |
| delivery no_receiver | PASS | |
| rotate: old key 401 (12) | PASS | |
| receiver 202 keeps marker (5) | PASS | last_accepted_seq 1, then idle at marker=head |
| cleanup (leave all, unset receiver) | PASS | smoke agents left all |

backup (13:50 KST, `deploy/backup.sh <ssh-alias> deploy/local/backups`): Pi `/tmp/platformd-backup/platform-20261016T045010Z.db` → Mac `deploy/local/backups/`(86016 bytes, gitignored). 로컬 `PRAGMA integrity_check` = ok, 행 수가 Pi 출력과 일치: agents 3, idempotency 7, inbox_rows 8, memberships 3, messages 7, notification_pending 3, read_state 3, schema_migrations 1, sequences 1.

재배포 (backup 직후, `deploy/deploy.sh <ssh-alias>`, 같은 바이너리·checksum): journal에 `shutdown signal received` … `database closed` … `shutdown complete` 뒤 새 PID로 `listening addr=<tailscale-ip>:7340`, `/healthz` 200, `NRestarts=0`, 공존 unit 불변. 이후 `platform agent list`가 같은 smoke agent 3개(route_version 2/1/2, created 04:45:52–04:45:55Z)를 보여 데이터 보존을 확인했다.

스모크가 만든 `smoke-<ulid>` agent 3개는 v1에 삭제가 없어 directory에 남는다(`all`에서는 탈퇴).

## 검증 방법 (증거 필수)
각 DoD 항목의 명령·출력 원문. 배포본 확인은 "했다"가 아니라 Pi에서 `platformd --version`과 `/healthz`를 읽은 결과로.

## 핸드오프 / 비상 해치
변경 파일, 검증 로그, 비상 해치 보고. **Pi 설치 단계는 오케스트레이터가 사용자 확인을 받은 뒤 직접 수행하거나 명시 위임한다.** 정본대로 불가능/모순일 때만 정지 → 보고.
