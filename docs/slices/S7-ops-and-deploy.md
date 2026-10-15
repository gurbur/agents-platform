# 슬라이스 S7: ops-and-deploy

- **상태**: frozen (2026-10-16) — **Pi 영구 설치 단계는 사용자 확인 뒤 오케스트레이터가 실행**
- **위험/의존도**: 상 — 운영 머신에 영구 상태(service account, unit, 데이터 디렉터리)를 만든다. 기존 서비스 공존 조건이 걸려 있다.

## 목표
`deploy/deploy.sh <ssh-alias>` 한 번으로 Mac에서 빌드한 바이너리가 운영 Pi에 systemd 서비스로 설치·기동되고, Tailscale 안의 Mac 브라우저·CLI에서 접근된다. backup·복구 절차와 preflight 측정이 저장소에 남는다. 12개 수용 시나리오를 실배포본에 대해 재실행한다.

## 범위 · 비범위
- 포함: preflight 스크립트, systemd unit·하드닝·resource control, service account·디렉터리·권한, 설정·owner token 배치 절차, 배포 스크립트, `platformd backup` 서브커맨드(rusqlite online backup API), backup 전송·복구 절차 문서, acceptance 스모크 스크립트, TECH 문서의 "구현 단계에서 정한다" 항목 갱신.
- 제외: Cloudflare Tunnel·기존 unit·포트 변경(절대 금지), public exposure, 자동 backup 스케줄러(절차와 명령만 제공; 스케줄은 사용자가 Mac launchd 등으로).

## 건드릴 인터페이스
`IF-CFG-001`, `IF-OPS-001`(`platformd backup` 추가 — 아래 표기), TECH 문서 "배포와 기존 서비스 공존"·"Backup과 복구"·"Migration과 운영 변경".

## 구현 방안
1. **preflight (`deploy/preflight.sh <alias>`)**: SSH로 `df -h`, `free -h`, `ss -ltn`, 실행 중 unit, `nproc`, OS/glibc를 읽어 출력만 한다(변경 없음). 결과를 TECH 문서 preflight 절에 날짜와 함께 기록.
2. **unit (`deploy/platformd.service`)**: `Type=simple`, `User=platformd`, `Group=platformd`, `ExecStart=/usr/local/bin/platformd --config /etc/platformd/platformd.toml`, `Restart=on-failure`, `RestartSec=2`, `KillSignal=SIGTERM`, `TimeoutStopSec=20`(DRAIN 10s + store join 5s + 여유). 하드닝: `NoNewPrivileges=yes`, `ProtectSystem=strict`, `ProtectHome=yes`, `PrivateTmp=yes`, `ReadWritePaths=/var/lib/platformd`, `ProtectKernelTunables=yes`, `RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX`. resource control은 preflight 결과로 `MemoryMax`(초안 512M), `CPUQuota`(초안 100%), `TasksMax`(초안 256)를 두되 수치는 unit 파일에만 두고 테스트에 박지 않는다. 로그는 journald(stderr), `LogRateLimit`는 기본.
3. **배치**: `/usr/local/bin/platformd`(0755 root), `/etc/platformd/platformd.toml`(0640 root:platformd), `/etc/platformd/owner.token`(0640 root:platformd — 사용자가 Pi에서 `openssl rand -base64 48`로 생성), `/var/lib/platformd/`(0750 platformd:platformd, `db_path = "/var/lib/platformd/platform.db"`). `listen`은 Pi의 **Tailscale 인터페이스 IP:7340**으로 설정 파일에만 적는다(저장소에는 예시 `100.x.y.z:7340` 자리표시). 포트 7340은 2026-10-15 실측 사용 중 목록(22, 8080, 8787, 20241)과 겹치지 않는다.
4. **deploy.sh**: `build-aarch64.sh` → `scp` 바이너리·unit을 `/tmp` → `ssh sudo install …`, `systemctl daemon-reload`, `systemctl enable --now platformd` (처음) 또는 `restart`, `systemctl is-active`, `curl http://<listen>/healthz` 200, `journalctl -u platformd -n 20`. 다른 unit에는 어떤 systemctl도 호출하지 않는다(스크립트가 unit 이름을 상수 `platformd.service`로만 쓴다). alias·주소는 인자/로컬 설정으로만.
5. **backup**: `platformd backup --config <toml> --out <file>` — rusqlite `backup::Backup`으로 온라인 일관 snapshot을 쓴다(서비스 동작 중 가능, WAL). `deploy/backup.sh <alias> <local-dir>`가 Pi에서 그 명령을 실행하고 `scp`로 Mac에 가져온다(파일명에 UTC 시각). 복구 절차(문서): 서비스 중지 → 기존 DB·WAL·SHM 보관 → snapshot 복사 → 기동 → `/healthz`·`GET /v1/agents`·delivery 상태 확인 → "sequence continuity는 보장하지 않음" 경고.
6. **acceptance (`deploy/smoke.sh <base-url> <owner-token-file>`)**: 12개 시나리오 중 외부에서 재현 가능한 것(등록, all·DM 송신, idempotency, inbox paging, read marker, owner no-ack, rotate 뒤 401, delivery 상태)을 curl 또는 CLI로 실행하고 결과를 표로 출력. receiver 알림(5~9)은 Mac에서 임시 mock receiver를 띄워 Pi가 Tailscale로 호출하게 한다.
7. **문서 갱신**: TECH 문서 "SQLite 내구성과 저장 공간" preflight 실측, "Backup과 복구" 절차, "배포와 기존 서비스 공존"의 실제 unit·경로. README에 운영 절 추가.

## 완료 정의 (DoD)
- [ ] preflight 출력이 문서에 기록됨.
- [ ] unit 파일이 `systemd-analyze verify`(Pi에서) 경고 없음.
- [ ] **사용자 확인 뒤** `deploy.sh`로 설치·기동, `is-active`, `/healthz` 200, Mac 브라우저에서 UI 접근, CLI `health` 성공.
- [ ] 기존 unit 네 개의 `ActiveState`·`ExecMainStartTimestamp`가 배포 전후 동일(배포가 건드리지 않았다는 증거).
- [ ] `platformd backup` 산출 파일을 Mac에서 열어 `PRAGMA integrity_check`=ok, 테이블·행 수 일치.
- [ ] acceptance 스모크 전체 통과 로그.
- [ ] 재배포(두 번째 `deploy.sh`)가 데이터를 보존하고 무중단에 가깝게 재시작.

## 검증 방법 (증거 필수)
각 DoD 항목의 명령·출력 원문. 배포본 확인은 "했다"가 아니라 Pi에서 `platformd --version`과 `/healthz`를 읽은 결과로.

## 핸드오프 / 비상 해치
변경 파일, 검증 로그, 비상 해치 보고. **Pi 설치 단계는 오케스트레이터가 사용자 확인을 받은 뒤 직접 수행하거나 명시 위임한다.** 정본대로 불가능/모순일 때만 정지 → 보고.
