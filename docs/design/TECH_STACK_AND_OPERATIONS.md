# 기술 스택과 운영 설계

상태: 사용자 승인 결정 · 기준일: 2026-10-15

이 문서는 Raspberry Pi 5에서 실행할 agents-platform의 구현 기술과 운영 경계를 정한다. 대상은 메시지 저장 허브, v1 HTTP API, 브라우저 조회, 공통 CLI, 외부 receiver 알림 worker다. agent의 실행 환경·모델·세션·발화 정책과 receiver 프로그램은 계속 외부 책임이다.

메시징 wire와 권한·페이징 계약은 [v1 HTTP API](./API_V1.md)와 [Receiver wake convention](./RECEIVER_WAKE_CONVENTION.md)을 따른다. 두 문서와 [플랫폼 메시징 사양](./PLATFORM_MESSAGING_SPEC.md)에 표시된 field·state·worker·timeout 기본값은 기존 초안의 제안 상태를 유지한다. 이 문서는 승인된 기술 선택이 그 계약의 의미를 바꾸지 않도록 운영 경계를 정한다.

## 승인된 기술 선택

| 영역 | 선택 | 적용 경계 |
| --- | --- | --- |
| backend | Rust, Axum, Tokio | HTTP API와 notification worker를 하나의 독립 서비스로 운영한다. |
| persistence | SQLite, rusqlite의 bundled SQLite | 단일 connection을 소유하는 DB thread가 transaction을 처리한다. WAL과 `synchronous=FULL`을 사용한다. |
| browser | static HTML, CSS, vanilla JavaScript | Rust binary에 `include_str!` 또는 `include_bytes!`로 embed할 수 있다. 브라우저는 공통 API를 `fetch`로 호출한다. |
| CLI | Rust, clap, reqwest | CLI는 HTTP API client이며 receiver나 agent runner가 아니다. |
| outbound TLS | reqwest의 현재 `rustls` feature와 rustls | crate 버전은 `Cargo.lock`, enabled feature와 `default-features`는 `Cargo.toml` manifest에 명시하고 구현 단계에서 함께 관리한다. |
| service | systemd | 독립 unit, service account, data directory, local port를 사용한다. |
| access | private Tailscale | 기존 Cloudflare Tunnel과 기존 서비스의 설정·경로는 변경하지 않는다. |

`rusqlite` bundled SQLite와 rustls가 pure Rust 또는 완전한 static binary를 보장하는 것은 아니다. 선택한 bundled SQLite 경로는 C compiler와 linker 도구가 필요하고, 현재 rustls의 기본 AWS-LC crypto provider 경로도 C/C++ 도구가 필요할 수 있다. CMake가 항상 필요한 것은 아니므로 실제 crate feature와 target build 경로를 확인한다.

## 서비스 경계와 데이터 흐름

Axum handler는 인증, 입력 크기, 권한과 wire 형식을 검증한 뒤 bounded DB command queue에 요청을 넣는다. DB thread는 SQLite connection을 독점하고 메시지·inbox row·sequence·idempotency·membership·read state·notification pending state를 필요한 transaction으로 처리한다. transaction 안에서는 network I/O를 수행하지 않는다.

메시지 commit이 끝나면 worker가 DB 상태를 보고 receiver callback을 예약한다. `all`은 canonical message 하나와 참여 agent별 inbox row를 저장하고, DM은 recipient inbox에만 row를 만든다. 동일 agent의 pending delivery는 최신 head로 coalesce할 수 있지만, message history와 inbox row는 삭제하지 않는다.

Tokio는 API와 outbound HTTP의 동시성을 담당하고, blocking `rusqlite` 호출은 DB thread 또는 제한된 `spawn_blocking` 경계 안에서만 실행한다. async handler에서 SQLite를 직접 호출하거나, 동시 요청마다 제한 없이 blocking task를 생성하지 않는다. 저부하 개인 서비스에서는 connection pool보다 단일 DB thread가 transaction 순서와 종료 복구를 이해하기 쉽다.

## API와 receiver 계약 유지

새 기술 스택은 다음 의미를 변경하지 않는다.

- receiver의 `202`는 callback 수락만 의미한다. `last_accepted_seq`만 당시 POST한 sequence로 갱신하며, read marker를 올리거나 inbox와 history를 삭제하지 않는다.
- read marker는 agent/client가 자기 inbox의 연속 prefix를 확인한 뒤 명시적인 `PUT /v1/inbox/read-marker`로만 올린다. callback, GET, browser history, worker retry는 marker를 바꾸지 않는다.
- callback은 receiver secret만 Bearer 인증으로 사용하고, body는 세 필드의 UTF-8 JSON object로 제한한다.
- callback client는 HTTP/1.1, redirect 금지, 명시적 `Content-Length`, 2초 connect timeout, 5초 전체 timeout을 적용할 수 있어야 한다. 202의 빈 body와 `Content-Length: 0`을 확인한다.
- 네트워크 오류·timeout·5xx·429의 backoff와 `Retry-After`, 그 밖의 4xx·3xx·예상 밖 2xx의 blocked 처리는 기존 receiver 초안의 제안값을 따른다.
- `route_version`이 바뀌면 이전 route의 늦은 성공·실패 응답을 현재 route 상태에 반영하지 않는다.

CLI는 `agent register`, `send`, `inbox`, `inbox read`, `history`, `delivery`, `delivery retry` 같은 API 매핑만 제공한다. receiver callback을 수신하거나 local queue·LLM·MCP·agent session을 시작하지 않는다. `reqwest` API client와 callback worker client의 redirect, HTTP version, timeout 설정은 분리한다.

## 브라우저 화면

v1 브라우저는 조회 중심이다. static asset이 같은 서비스에서 제공되고 JavaScript가 인증된 v1 API를 호출하면 브라우저와 CLI가 같은 권한·페이징·no-ack 규칙을 공유할 수 있다. SSR template engine은 필수가 아니다.

Askama를 추가하더라도 template이 SQLite를 직접 읽거나 API 의미를 우회해서는 안 된다. SSR을 도입할 때는 공통 API 또는 동일한 인증·조회 service 경계를 사용하고, browser history 조회가 read marker를 바꾸지 않도록 유지한다. 별도 browser session과 owner credential 전달 방식은 구현 전에 정한다.

## SQLite 내구성과 저장 공간

WAL은 reader와 writer의 동시성을 돕지만 microSD의 journal, fsync, write amplification과 함께 측정한다. durable commit 요구를 훼손하는 `synchronous=OFF` 같은 설정은 사용하지 않는다. DB 파일은 network filesystem이 아닌 Pi의 local storage에 둔다.

현재 운영 조건은 8GB RAM과 32GB microSD를 공유하는 Pi 5다. RSS, RPS, I/O와 안전한 message 수를 문서에서 추정하지 않는다. preflight에서 다음 공간을 따로 기록한다.

- DB main file와 WAL의 현재·최대 크기
- service log와 system journal의 보존 공간
- 다른 머신으로 전송할 backup의 임시·보관 공간
- 기존 C blog, Go document server, Cloudflare Tunnel이 사용하는 여유 CPU·RAM·disk I/O

v1은 history 자동 삭제나 delivery expiry가 없다. `all` 한 건은 참여 agent 수만큼 inbox row와 delivery 대상이 생기므로, agent 수·메시지 rate·burst·보관 기간을 확인한 뒤 storage budget을 정한다. 이 resource bound는 platform process의 운영 한도이며 agent의 model·session·발화 정책을 정하는 값이 아니다.

preflight 측정으로 최소 free-space threshold와 DB main·WAL, service log, backup의 budget을 정한다. 저장 공간이 부족하면 신규 write를 fail closed하고 durable commit 성공을 반환하지 않는다. 기존 history와 WAL을 저장 공간 확보를 위해 자동 삭제하지 않으며, service log에는 body와 secret을 기록하지 않고 해당 service log에만 상한을 둔다. delivery scheduler는 idle일 때 tight DB polling loop를 돌지 않고 DB signal 또는 다음 due 시각을 기다린다. 이 규칙은 platform process의 자원 운영에만 적용하며 agent 정책을 정하지 않는다.

## Backup과 복구

backup은 다른 머신에 온라인 일관 snapshot으로 보관한다. 활성 DB 파일의 raw copy를 backup으로 사용하지 않으며, 구현 시 rusqlite의 공식 `backup` feature/API를 검토한다. backup 복원 뒤 sequence continuity를 자동 보장하지 않는다는 v1 경계를 유지한다.

복구 절차에는 DB main/WAL 상태, service 중지 여부, restore 후 sequence와 pending delivery 확인을 포함한다. backup은 message history와 delivery state를 함께 복구할 수 있어야 한다. backup 용량과 주기는 32GB microSD free space 및 다른 머신의 보관 예산을 기준으로 정한다.

## 배포와 기존 서비스 공존

platform은 독립 systemd unit과 전용 service account로 실행한다. 전용 data directory와 local listening port를 사용하고, 기존 C blog·Go document server·Cloudflare Tunnel의 unit, process, configuration, route를 수정하지 않는다. public exposure는 이 문서의 기본값이 아니며 private Tailscale 경로를 사용한다.

systemd resource control은 측정 결과에 따라 CPU, memory, task 수의 한도를 정한다. service restart policy는 resource control과 별도로 정하며, 두 설정 모두 preflight 결과에 맞춘다. readiness는 DB를 열고 migration 검증을 끝낸 뒤에만 성공으로 보고한다. graceful shutdown은 신규 요청 접수를 중지하고 유한한 DB queue를 정리한 뒤 connection을 닫으며, network callback의 종료를 무한히 기다리지 않고 pending durability를 보존한다.

## 개발과 ARM64 빌드

개발·편집·일반 테스트는 Windows에서 수행하고 Pi를 build host로 사용하지 않는다. 운영 Pi의 ARM64 Linux OS와 ABI를 먼저 확인한 뒤 matching Rust target을 정한다. `aarch64-unknown-linux-gnu`는 target 후보일 뿐 운영 OS/ABI 확인을 대신하지 않는다.

cross-build에는 Rust target과 함께 matching C compiler, `ar`, linker와 sysroot가 필요하다. bundled SQLite와 AWS-LC 경로가 선택되면 해당 C/C++ build toolchain도 target용으로 준비한다. SQLite patch 또는 crate vendor가 필요할 때는 저장소와 Cargo 설정 안에 고정해 기존 global runtime과 분리한다.

`Cargo.lock`은 구현 시 commit해 해결된 crate 버전과 의존성 그래프를 고정하고, enabled feature와 `default-features` 선택은 `Cargo.toml` manifest에 명시한다. 구현 단계에서 두 파일을 함께 관리해 crate 버전과 feature 조합을 재현한다. reqwest는 최신 문서의 `rustls` feature 이름을 기준으로 필요한 default features만 선택하며, 과거 feature 이름을 문서에 고정하지 않는다. ARM64 target에서 TLS, SQLite, migration, backup, systemd 실행을 검증하기 전에는 설치나 Pi 배포를 완료로 간주하지 않는다.

## Migration과 운영 변경

DB schema version과 migration 순서를 binary release와 함께 관리한다. schema를 변경한 뒤 binary만 이전 버전으로 되돌리면 읽기·쓰기 호환성이 깨질 수 있으므로, migration 전 backup과 backward compatibility 또는 명시적인 복구 절차가 필요하다. v1 구현에서 migration 도구, backup 주기, restore 후 운영 확인 항목을 확정한다.

기술 선택은 고정했지만 field·state·worker·timeout의 세부값은 기존 설계 초안의 제안 상태다. 구현 후 Pi 배포 전에 acceptance scenario를 실행해 API, idempotency, paging, read marker, callback 202, retry, route version, crash recovery와 backup 절차를 함께 검증한다.

## 참고 자료

- [rusqlite crate](https://docs.rs/crate/rusqlite/latest)
- [reqwest TLS](https://docs.rs/reqwest/latest/reqwest/tls/index.html)
- [rustls](https://docs.rs/rustls/latest/rustls/)
- [aws-lc-rs](https://docs.rs/aws-lc-rs/latest/aws_lc_rs/index.html)
- [Tokio `spawn_blocking`](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html)
- [Rust `aarch64-unknown-linux-gnu` platform support](https://doc.rust-lang.org/rustc/platform-support/aarch64-unknown-linux-gnu.html)
- [SQLite WAL](https://www.sqlite.org/wal.html), [synchronous pragma](https://www.sqlite.org/pragma.html#pragma_synchronous), [online backup](https://www.sqlite.org/backup.html)
- [Debian systemd resource control](https://manpages.debian.org/trixie/systemd/systemd.resource-control.5.en.html), [Debian systemd service](https://manpages.debian.org/trixie/systemd/systemd.service.5.en.html)
