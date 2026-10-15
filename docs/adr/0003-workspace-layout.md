# ADR-0003: Cargo workspace 세 crate — `platform-wire`, `platformd`, `platform`

- **상태**: 채택됨
- **날짜**: 2026-10-15

## 맥락
서버(HTTP API + worker)와 CLI가 같은 wire 타입(요청·응답·오류·sequence 문자열·agent_id 검증)을 써야 한다. CLI는 axum·rusqlite에 의존하지 않아야 가볍다.

## 결정
```
Cargo.toml                 # [workspace] members = ["crates/*"], resolver = "2", workspace.dependencies로 버전 일원화
crates/wire/   → package platform-wire  (lib)  serde 타입, AgentId/Seq 검증, 오류 code 상수. 의존성: serde, serde_json, thiserror 정도.
crates/server/ → package platformd      (bin platformd)  config, auth, store, api, delivery, 정적 UI embed
crates/cli/    → package platform-cli   (bin platform)   clap + reqwest(rustls), platform-wire
```
- `Cargo.lock` 커밋. 각 crate `rust-version = "1.98"`.
- reqwest는 `default-features = false`, features `["rustls-tls", "json", "http1"]`를 기준으로 하되 실제 feature 이름은 구현 시 reqwest 최신 문서로 확정하고 manifest에 명시한다(TECH 문서 규칙).
- 통합 테스트는 `crates/server/tests/`에서 임시 디렉터리 DB로 앱을 in-process 기동해 HTTP로 검증한다.

## 대안
- **단일 crate, lib + 두 bin** — 기각: CLI 바이너리 빌드에 서버 의존성이 끌려온다.
- **서버/CLI 완전 분리 저장소** — 기각: wire 타입이 갈라진다.

## 근거
wire 타입을 한 crate에 두면 API 문서 ↔ 서버 ↔ CLI 사이의 표현 불일치(예: sequence를 number로 보내는 실수)가 컴파일 단계에서 걸린다.

## 결과 · 트레이드오프
- 좋아지는 것: 의존성 경계가 명확, CLI가 가볍다.
- 감수하는 것: crate 셋 관리, 변경 시 workspace 전체 clippy.
- 영향받는 인터페이스: `IF-ID-001`(wire crate가 유일한 검증 지점), `IF-CLI-001`.

## 재평가 트리거
브라우저 UI가 TypeScript 빌드를 요구하게 되면 정적 자산 crate 또는 빌드 단계 추가를 검토한다.
