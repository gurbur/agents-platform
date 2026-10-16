# ADR-0005: identity attestation — 허브가 단기 Ed25519 서명 JWT로 agent 신원을 제3자에게 증명한다

- **상태**: 채택됨 (2026-10-16) — 승인 시 결정: 403 `owner_not_allowed`는 `forbidden`과 공존하는 새 code, 응답은 `{token,sub,aud}`뿐이고 만료는 JWT `exp`만, 503 `identity_disabled`는 401·403 뒤·body 검사 앞, `ed25519-dalek` + 직접 만든 인코더를 RFC 8037 vector와 garage-docs(`jwt/v5`) interop test로 검증, 옛 키 제거 전 `max_ttl_secs` + 60초 대기·공개키 재출력 없음.
- **날짜**: 2026-10-16

## 맥락
사용자는 허브에 등록된 agent 신원을 같은 Pi에서 도는 별도 Go 문서 서버 garage-docs의 문서 읽기 권한까지 넓히려 한다. garage-docs는 문서별로 `agent:<id>` principal에게 읽기를 허용한다. 지금 agent가 가진 credential은 `agent_api_key` 하나뿐이고(`IF-AUTH-001`), 이는 허브 API에만 통한다. [플랫폼 메시징 사양](../design/PLATFORM_MESSAGING_SPEC.md) "플랫폼 범위"는 허브를 transport와 보관 경계로 한정하므로, 문서 자체나 문서 ACL을 허브로 가져올 수는 없다.

검토한 선택지는 넷이다.
- **A. Cloudflare Access service token** — garage-docs 앞단에서 agent마다 service token을 발급한다.
- **B. 허브가 발급하는 단기 서명 토큰** — agent가 자기 `agent_api_key`로 허브에서 짧은 수명의 서명 토큰을 받고, garage-docs가 허브의 공개키로 오프라인 검증한다.
- **C. garage-docs 자체 agent key** — garage-docs가 agent별 key를 따로 발급·보관한다.
- **D. 허브 proxy** — 허브가 agent 대신 garage-docs 문서를 받아 전달한다.

## 결정
**B를 채택한다.** wire 정본은 [IDENTITY_ATTESTATION.md](../design/IDENTITY_ATTESTATION.md), 구현 계약은 `INTERFACES.md` `IF-AUTH-002`. 요지:
- `POST /v1/identity/tokens`, `agent_api_key` 전용, `sub`는 호출자 agent_id. 응답 201 `{"token","sub","aud"}`. 만료의 유일한 출처는 JWT `exp`다.
- 토큰은 compact JWS(JWT), header `{"alg":"EdDSA","typ":"JWT","kid":"<sha256(raw public key) 앞 8 hex>"}`(`kid`는 항상 있다), claims `iss`·`sub`·`aud`·`iat`·`exp`·`jti`. `iat`·`exp`는 정수 초, `exp - iat ≤ 900`, 검증 측 skew 60초, replay 방지 없음(`jti`는 로그 상관 키).
- 서명 키는 `platformd identity keygen`이 만드는 32바이트 seed 파일. 공개키는 운영자가 garage-docs 설정 `agent_access.public_keys`에 붙여 넣는다. v1에는 공개키 endpoint가 없다. 키 교체는 허브 새 키 → garage-docs `public_keys`에 추가(kid 공존) → 허브 재시작 → 옛 키 제거.
- 설정 `[identity]`가 없으면 기능이 꺼지고 503 `identity_disabled`.

[플랫폼 메시징 사양](../design/PLATFORM_MESSAGING_SPEC.md) "플랫폼 범위"에 다음 한 줄을 추가했다:

> 허브는 자기 agent의 신원을 제3자에게 단기 서명 토큰으로 증명한다(identity attestation). 문서 자체·ACL은 허브 밖.

문서 proxy(D)는 이번에 기각하는 데 그치지 않고 **영구히 범위 밖**으로 둔다.

## 대안
- **A. Cloudflare Access service token** — 기각: agent 신원이 허브와 Cloudflare 두 곳으로 갈라지고, 발급·폐기가 저장소 밖 외부 계정 설정으로 나가며, tailnet 안에서 끝나는 경로에 외부 의존을 끼운다.
- **C. garage-docs 자체 agent key** — 기각: agent마다 두 번째 장기 credential을 배포·회전해야 하고, 허브와 garage-docs의 agent 목록이 따로 논다.
- **D. 허브 proxy** — 기각(영구): 허브가 문서 내용과 ACL을 다루게 되어 범위 경계를 깨고, garage-docs 장애·부하가 허브에 묶이며, 허브가 멈추면 문서 읽기도 멈춘다.
- **B 변형: HS256 공유 secret** — 기각: 서명 secret이 허브 밖(garage-docs)에도 있어야 하고, 그쪽이 유출되면 아무 agent 신원이나 위조된다.

## 근거
- **신원이 하나다.** agent는 이미 가진 `agent_api_key`만으로 토큰을 받는다. 등록·rotate·directory는 지금 허브 그대로다.
- **검증이 오프라인이다.** garage-docs는 공개키만으로 검증하므로 검증 경로에 허브 호출이 없다. 이미 받은 토큰은 허브가 멈춰도 만료 전까지 통한다.
- **tailnet 경계를 이미 신뢰한다.** 허브 API와 garage-docs agent listener 모두 Tailscale 주소에서만 listen한다. bearer 토큰이 오가는 구간은 지금의 `agent_api_key`와 같다.
- **서명 secret이 허브 밖에 없다.** 비대칭 서명이라 garage-docs가 가진 것은 공개키뿐이다. 소비자가 유출돼도 토큰 위조는 불가능하다.

## 결과 · 트레이드오프
- 좋아지는 것: agent 하나에 credential 하나, garage-docs 쪽 ACL은 `agent:<id>` 문자열만 다루면 된다, 허브 장애가 이미 발급된 읽기에 번지지 않는다.
- 감수하는 것: 발급된 토큰은 bearer라 유출되면 만료(≤15분) 전까지 쓸 수 있다. 허브는 발급 토큰을 폐기하지 않는다. 허브의 agent key rotate는 새 토큰 발급을 막고 이미 발급된 토큰은 exp(≤15분)까지 유효하다. 즉시 차단은 garage-docs `revoke SLUG agent:<id>`가 맡는다. 서명 키 파일을 가진 사람은 모든 agent 신원을 위조할 수 있으므로 owner token과 같은 수준(0640 root:platformd)으로 보호한다. 키 교체에는 허브 재시작이 필요하다.
- 구현 영향:
  - **crate 의존성**: `ed25519-dalek` 3.0.0 + 직접 만든 JWT 인코더로 정했다. 허브는 서명만 하고 신뢰할 수 없는 JWT를 파싱하지 않으므로, JWT 라이브러리가 값을 하는 검증 쪽 기능이 필요 없다. header가 고정이라 인코딩은 base64url 세 조각과 서명 한 번이다. `ed25519-dalek`의 `SigningKey::from_bytes(&[u8; 32])`가 키 파일 형식(raw seed)과 그대로 맞는다. `jsonwebtoken` 11.1.0은 EdDSA 서명 키를 PKCS#8 DER(`EncodingKey::from_ed_der`/`from_ed_pem`)로만 받아 seed를 감싸는 코드가 따로 필요하고, crypto provider feature(`aws_lc_rs`/`rust_crypto`)를 골라야 한다(2026-10-16 crates.io와 crate 소스로 확인). base64·sha2·ulid·rand는 이미 workspace 의존성이다. 직접 만든 인코더는 RFC 8037 Appendix A.4의 Ed25519 JWS test vector(A.1 키)로 서명·base64url을 확인하고, garage-docs의 검증 코드(Go `jwt/v5`)가 허브 토큰을 받아들이는 interop test로 확인한다([S8](../slices/S8-identity-attestation.md) DoD).
  - **DB**: schema 변경 없음. `IF-DB-001` migration 최신 버전은 1 그대로.
  - **DB thread**: 새 Store 명령 없음. 인증 계층의 기존 `api_key_hash` 조회가 agent 존재를 이미 확인한다.
  - **기동**: 서명 키는 owner token처럼 기동 때 한 번 읽어 메모리에 둔다. 읽기 실패나 형식 오류면 기동을 거부한다.
  - **운영**: README "운영 (Raspberry Pi)" 절에 선택 단계로 keygen을 적었다. `deploy/remote-install.sh`의 운영자 안내는 S8에서 keygen을 선택 단계로 언급한다. 스크립트는 키를 만들지 않는다.
  - **로그**: 토큰 원문과 seed는 어떤 로그에도 남기지 않는다. 발급 로그는 `sub`·`jti`·`aud`·`exp`만 남긴다.
- 영향받는 인터페이스: `IF-API-IDENTITY`·`IF-IDT-*`·`IF-AUTH-002`(신설), `IF-CFG-001`(`[identity]`), `IF-OPS-001`(`platformd identity keygen`), `IF-WIRE-002`·`IF-WIRE-003`(전용 code), `IF-CLI-001`(stdout 규칙).

## 재평가 트리거
garage-docs 외의 소비자가 생겨 공개키 배포를 손으로 하기 어려워지면 공개키 endpoint(JWKS)를 다시 본다. 허브나 소비자가 tailnet 밖으로 노출되거나, 15분 bearer 창이 받아들이기 어려운 유출 사례가 나오면 수명 단축·sender-constrained 토큰을 다시 본다. 허브와 소비자가 다른 머신으로 갈라지면 시계 동기화와 skew 60초를 다시 본다.
