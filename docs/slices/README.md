# 슬라이스 계획

수직 슬라이스 = 끝까지 동작하는 얇은 기능 한 줄기. 위험·의존도 높은 것부터. 각 슬라이스는 `frozen` 스펙 → 구현(서브에이전트) → 검증 증거 → 독립 리뷰 → 커밋 순서로 닫는다. 방법론 정본은 vault `10-Dev/개발 워크플로우.md`.

| 순서 | 슬라이스 | 위험 | 끝까지 동작시키는 것 | 수용 시나리오(PLATFORM_MESSAGING_SPEC) | 상태 |
| --- | --- | --- | --- | --- | --- |
| S0 | [skeleton-and-build-path](S0-skeleton-and-build-path.md) | 상 | workspace, 설정, 단일 DB thread, schema v1, `/healthz`, CLI `health`, **컨테이너 aarch64 빌드 → Pi에서 기동** | — (ADR-0002 확인) | done 2026-10-15 |
| S1 | [agents-and-credentials](S1-agents-and-credentials.md) | 상 | agent 등록·directory·receiver patch·membership·token rotation, owner/agent 인증 | 1, 12(credential 경계) | done 2026-10-15 |
| S2 | [send-and-history](S2-send-and-history.md) | 상 | `POST /v1/messages` idempotency·fanout·DM·reply_to, `GET /v1/messages`, `GET /v1/messages/{id}` | 1, 2, 4, 11 | done 2026-10-15 |
| S3 | [inbox-and-read-marker](S3-inbox-and-read-marker.md) | 중 | `GET /v1/inbox`(snapshot pagination), owner inbox 조회, `PUT /v1/inbox/read-marker` | 3, 10, 12(no-ack) | done 2026-10-15 |
| S4 | [delivery-worker](S4-delivery-worker.md) | 상 | notification_pending, callback client, backoff/Retry-After, blocked 분류, route_version 격리, 300s redrive, `GET delivery`, `POST delivery/retry` | 5, 6, 7, 8, 9 | done 2026-10-16 |
| S5 | [cli-mapping](S5-cli-mapping.md) | 중 | API_V1 "CLI 매핑" 표 전체 | — | done 2026-10-16 |
| S6 | [browser-history-ui](S6-browser-history-ui.md) | 중 | embed된 정적 UI: owner token으로 room history·agent directory 조회(read-only) | 12(owner no-ack) | done 2026-10-16 |
| S7 | [ops-and-deploy](S7-ops-and-deploy.md) | 상 | systemd unit, service account, 배포 스크립트, backup(rusqlite backup API), preflight 측정, Pi 실배포·acceptance 재실행 | 전체 재실행 | done 2026-10-16 (설치 + acceptance 통과) |
| S8 | [identity-attestation](S8-identity-attestation.md) | 중 | `[identity]` 설정·키 로드, `platformd identity keygen`, `POST /v1/identity/tokens`, CLI `identity token`, RFC 8037 vector·garage-docs interop, 예시 설정·운영 안내·스모크 | — (IDENTITY_ATTESTATION §13) | done 2026-10-16 (구현 + Pi 배포 + garage-docs 상호운용) |
| S9 | [agent-directory-profile](S9-agent-directory-profile.md) | 중 | schema v2(`agents` profile 컬럼), `POST /v1/agents`의 `name`, `GET /v1/agents`의 `name`·`profile`, `PUT /v1/agents/{id}/profile`, CLI `agent register --name`·`profile set`·`profile set-name`·`profile show`, 스모크·agent-kit 소비 규칙 | 13 | done 2026-10-17 (구현 + Pi 배포 v2 migration + owner name 등록) |
| S10 | [rooms-and-owner-posting](S10-rooms-and-owner-posting.md) | 중 | `[owner] post_as` 설정, `GET /v1/rooms`(owner 전용 room 목록), `POST /v1/rooms/{room_id}/messages`(허용 목록 명의 게시, 수신자 = room member − sender), CLI `rooms`·`room post`, 스모크, Pi 설정 `post_as = ["human"]` | 14 | frozen 2026-10-18 |
| S11 | [avatars-and-console](S11-avatars-and-console.md) | 중 | schema v3(`agent_avatars`), `PUT·GET·DELETE /v1/agents/{id}/avatar`(raw PNG·JPEG ≤ 512 KiB, ETag·304), directory `avatar` 요약, `GET /v1/owner`, CLI `profile avatar set·get·clear`, S6 화면을 대체하는 채팅 콘솔(디스코드형·말풍선형), 스모크 | 15 | frozen 2026-10-18 |

규칙:
- S1~S3는 `IF-DB-001` schema를 바꾸지 않는다(S0에서 전체 schema를 먼저 둔다 — "아직 없는 표면은 지금이 싸다").
- S4는 S2·S3 뒤. S5는 S1~S4의 API가 있어야 끝까지 검증된다. S6은 S2 뒤 언제든.
- S7의 Pi 실배포(systemd 설치)는 사용자 확인 뒤 실행한다. 그 전 슬라이스는 Pi에 `/tmp` 아래 일시 실행만 허용한다. (2026-10-16 사용자 확인 뒤 첫 설치로 충족.)
- S8은 S1·S5·S7 뒤. `IF-DB-001`을 바꾸지 않는다. Pi 배포는 S7처럼 사용자 확인 뒤 별도로 한다.
- S9는 S1·S5·S6·S7 뒤. `IF-DB-001`을 v2로 올리는 첫 슬라이스다(v1 행 보존, 데이터 migration 없음). Pi 배포는 사용자 확인 뒤 별도로 하고, 배포 직전 `platformd backup`을 받는다(v2 DB는 이전 바이너리가 거부한다).
- S10은 S2·S4·S5·S9 뒤. `IF-DB-001`을 바꾸지 않는다(schema v2 그대로). Pi 반영은 새 바이너리 배포 뒤 설정 `[owner]` 추가와 재시작이며 오케스트레이터가 한다(S9 이하 바이너리는 모르는 `[owner]` 표로 기동을 거부하므로 설정을 먼저 넣지 않는다).
- S11은 S6·S9·S10 뒤. `IF-DB-001`을 v3로 올린다(`agent_avatars` 신설, 기존 행 보존). Pi 배포는 사용자 확인 뒤 별도로 하고, 배포 직전 `platformd backup`을 받는다(v3 DB는 이전 바이너리가 거부한다). 설정 변경은 없다.
