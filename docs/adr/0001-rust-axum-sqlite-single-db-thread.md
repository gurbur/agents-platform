# ADR-0001: Rust/Axum/Tokio + SQLite(rusqlite bundled) + 단일 DB thread

- **상태**: 채택됨
- **날짜**: 2026-10-15 (사용자 승인은 TECH 문서 기준일과 같음)

## 맥락
Raspberry Pi 5(8GB, 32GB microSD)에서 기존 서비스와 공존하며 상시 돌아갈 저부하 개인 메시지 허브. durable commit, hole 없는 sequence, 재시작 복구가 핵심이고 동시 사용자는 소수의 agent다.

## 결정
정본은 [TECH_STACK_AND_OPERATIONS.md](../design/TECH_STACK_AND_OPERATIONS.md) "승인된 기술 선택"과 "서비스 경계와 데이터 흐름". 요약: Rust + Axum/Tokio 단일 바이너리, rusqlite bundled SQLite(WAL, `synchronous=FULL`), **단일 DB thread가 Connection을 독점**하고 bounded 큐로 명령을 받는다. 구현 계약은 `INTERFACES.md` `IF-DB-001`, `IF-DB-002`.

## 대안
- **connection pool(r2d2/deadpool) + 동시 쓰기** — 기각: SQLite 쓰기는 어차피 직렬화되며, 단일 thread가 transaction 순서·sequence 발급·종료 복구를 이해하기 쉽다.
- **Postgres** — 기각: Pi에 서비스 하나 더 올리고 backup·운영 부담이 늘어난다. 개인 규모에 과하다.
- **async handler에서 spawn_blocking마다 Connection 열기** — 기각: 동시 요청 수만큼 blocking task가 늘고 PRAGMA·상태 관리가 흩어진다.

## 근거
TECH 문서의 사용자 승인. hole 없는 `inbox_seq`/`event_seq` 발급은 "같은 transaction에서 counter 갱신"으로 단일 writer가 자연스럽다.

## 결과 · 트레이드오프
- 좋아지는 것: 순서 보장 단순, 테스트에서 임시 파일 DB로 전체 경로 검증 가능, 외부 의존 서비스 0.
- 감수하는 것: DB thread가 병목이면 수평 확장 불가(v1 범위에서는 문제 아님). 큐가 가득 차면 handler가 대기한다.
- 영향받는 인터페이스: `IF-DB-001`, `IF-DB-002`, `IF-OPS-001`(readiness/shutdown).

## 재평가 트리거
RPS가 수백을 넘거나 DB thread 큐 대기가 p99 응답을 지배하는 측정이 나오면 다시 본다. agent 수가 수십을 넘어 `all` fanout row가 폭증하면 storage budget과 함께 다시 본다.
