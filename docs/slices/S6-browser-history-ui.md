# 슬라이스 S6: browser-history-ui

- **상태**: done (2026-10-16) — Orca 브라우저로 수동 검수: 네 탭 동작, `<b>x</b> & <script>` 본문이 글자 그대로(td 자식 요소 0), reload 뒤 sessionStorage 복원, 로그아웃 뒤 새로고침은 `연결 실패 — unauthorized`, 콘솔 메시지 0건, owner 조회 뒤 read_marker 불변. `HEAD /`는 200(RFC 9110).
- **위험/의존도**: 중 — 보안 경계(owner token 취급, CSP)와 "조회만, marker 부작용 없음"이 핵심. 사람이 브라우저로 직접 검수한다.

## 목표
`GET /`가 바이너리에 embed된 정적 페이지를 주고, 사용자가 owner token을 넣으면 agent directory·room history·agent inbox·delivery 상태를 **읽기 전용**으로 본다. 모든 데이터는 같은 v1 API를 `fetch`로 호출한다.

## 범위 · 비범위
- 포함: `GET /`, `GET /assets/*`(embed, `include_str!`/`include_bytes!`), 보안 헤더, 화면 네 개(directory, history, inbox, delivery), 페이징·검색 UI, token 입력·세션 보관.
- 제외: 송신, membership·receiver 변경, retry 버튼 등 **모든 write**(v1 브라우저는 조회 중심 — TECH 문서). SSR/Askama 없음. 빌드 도구(npm) 없음 — vanilla JS·CSS 파일 그대로.

## 건드릴 인터페이스
`IF-OPS-001`(정적 경로 추가 — 아래 표기 추가), `IF-API-AGENTS`, `IF-API-INBOX`, `IF-API-HISTORY`, `IF-API-DELIVERY`(소비만), `IF-WIRE-001`.

## 구현 방안
1. **정적 서빙**: `crates/server/ui/{index.html, app.js, app.css}`를 embed. `GET /` → index.html, `GET /assets/app.js|app.css`. `Cache-Control: no-store`. 보안 헤더: `Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; frame-ancestors 'none'`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`. 인라인 스크립트·스타일 없음.
2. **token 취급**: 입력 필드로 받아 메모리에 두고 선택적으로 `sessionStorage`에만 저장(탭 닫히면 소멸). `localStorage`·cookie·URL에 넣지 않는다. 로그아웃 버튼으로 지운다.
3. **화면**: (a) Directory — `GET /v1/agents` owner 형태 표. (b) History — room 선택(`all` + directory에서 만든 DM room 목록 `dm:<a>:<b>`), 기본 최신순, "이전 더 보기"(`before_event_seq`), `q` 검색. body는 **textContent로만** 렌더(HTML 해석 금지 — API_V1). (c) Inbox — agent 선택 → `GET /v1/agents/{id}/inbox` snapshot 순회, read_marker 표시. (d) Delivery — agent별 `GET /v1/agents/{id}/delivery` 표.
4. **부작용 없음**: UI 코드에 GET 외 메서드가 없다(테스트가 소스를 grep하는 것이 아니라, 통합 테스트에서 UI가 쓰는 endpoint 목록이 전부 GET임을 라우터 수준에서 확인하고, 수동 검수 체크리스트에 넣는다).
5. **접근성·가독성**: 시스템 폰트, 다크/라이트 `prefers-color-scheme`, 모바일 폭에서도 표가 가로 스크롤.

## 완료 정의 (DoD)
- [ ] 통합 테스트: `GET /` 200 text/html + CSP 등 헤더, `/assets/*` 200 + 올바른 content-type, 404는 공통 오류 body.
- [ ] **수동 검수(사용자 또는 오케스트레이터가 브라우저로)**: token 입력 → 네 화면 동작, history 페이징, 검색, body에 `<b>x</b>`를 넣어도 글자 그대로 보임, 새로고침 뒤 sessionStorage 복원, 로그아웃 뒤 요청 401.
- [ ] 콘솔에 CSP 위반·오류 0건(Orca 브라우저 `orca console`로 확인).
- [ ] fmt/clippy/test 그린. JS는 함수 단위로 나누고 전역 상태 하나.

## 검증 방법 (증거 필수)
테스트 출력 + 브라우저 스크린샷(Orca `orca screenshot`) 네 화면 + 콘솔 로그 0건 캡처.

## 핸드오프 / 비상 해치
변경 파일, 검증 로그, 스크린샷 경로. 커밋하지 않는다. 정본대로 불가능/모순일 때만 정지 → 보고.
