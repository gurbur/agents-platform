# Identity Attestation

상태: frozen (2026-10-16 사용자 승인)

기준일: 2026-10-16

이 문서는 허브가 자기 agent의 신원을 제3자에게 증명하는 단기 서명 토큰(identity attestation)의 wire 계약을 정의한다. agent는 자기 `agent_api_key`로 허브에서 Ed25519로 서명한 JWT를 받고, 제3자는 허브의 공개키만으로 그 토큰을 오프라인 검증한다. 결정의 이유와 기각한 대안은 [ADR-0005](../adr/0005-identity-attestation-tokens.md)에 있다. 구현 계약(키 파일, 설정, 로그)은 `INTERFACES.md` `IF-AUTH-002`가 정한다.

첫 소비자는 같은 Pi에서 도는 별도 Go 문서 서버 garage-docs다. garage-docs는 문서별로 `agent:<id>` principal에게 읽기를 허용한다. 문서 자체, 문서 목록, ACL, 문서 proxy는 허브의 범위가 아니며 앞으로도 허브로 가져오지 않는다. 이 문서는 garage-docs를 설계하지 않는다. 검증 절에서 인용하는 garage-docs 동작은 그쪽과 합의한 상호 참조일 뿐이고 정본은 garage-docs 문서다.

## 1. 적용 범위

허브가 제공하는 것은 다음과 같다.

- agent가 자기 신원을 담은 단기 서명 토큰을 발급받는 HTTP endpoint 하나와 그 CLI 명령
- 서명 키를 만드는 운영 명령과 키 교체 절차
- 소비자가 토큰을 검증하는 순서의 권고

다음은 범위 밖이다.

- 제3자 문서의 저장·목록·전달(proxy)과 문서별 ACL
- 발급된 토큰의 폐기(revocation)와 replay 방지
- 공개키 배포 endpoint(JWKS 등). v1에서 공개키는 운영자가 손으로 옮긴다.
- 사람(브라우저) 사용자의 신원. owner token은 이 기능에 쓰이지 않는다.

identity token은 허브 자신의 API 인증에 쓰이지 않는다. 허브 API의 Bearer는 계속 owner token 또는 `agent_api_key`뿐이다.

## 2. 규범 용어

MUST는 필요한 의무, SHOULD는 특별한 이유가 없으면 따를 권고, MAY는 호환성을 깨지 않는 선택 사항이다. "소비자"는 토큰을 받아 검증하는 제3자 서비스(첫 번째는 garage-docs)다.

## 3. 구성 요소와 신뢰 경계

- **허브**는 서명 키(32바이트 Ed25519 seed)를 가진 유일한 주체다. 서명 키는 허브 밖으로 나가지 않는다.
- **agent**는 자기 `agent_api_key`로 토큰을 받아 소비자에게 `Authorization: Bearer <jwt>`로 제시한다.
- **소비자**는 운영자가 설정에 넣어 준 공개키(들)로 서명을 검증하고, 자기 ACL로 접근을 판정한다. 검증 경로에 허브 호출이 없다.
- 허브 API와 garage-docs agent listener는 모두 Pi의 Tailscale 주소에서만 listen한다. 토큰이 오가는 구간의 신뢰 경계는 지금 `agent_api_key`가 오가는 구간과 같다.
- 허브와 garage-docs가 같은 Pi에서 돌므로 두 시계는 같다. 60초 skew는 다른 머신의 소비자를 위한 여유다.

## 4. 토큰 발급 API

### 4.1 `POST /v1/identity/tokens`

`agent_api_key`로만 호출한다. 토큰의 `sub`는 호출한 agent의 `agent_id`이며 요청에서 받지 않는다.

```http
POST /v1/identity/tokens
Authorization: Bearer <agent_api_key>
Content-Type: application/json

{"aud":"garage-docs","ttl_secs":600}
```

| 필드 | 필수 | 형식 | 의미 |
| --- | --- | --- | --- |
| `aud` | MUST | 비어 있지 않은 문자열 | 토큰을 제시할 소비자. 설정 `[identity].audiences`에 있어야 한다. 기본값이 없다 |
| `ttl_secs` | MAY | 1 이상의 JSON 정수 | 토큰 수명(초). 생략하면 `[identity].default_ttl_secs`(기본 600) |

허용하지 않은 필드(`sub` 포함)는 422 `unexpected_field`다. 요청 공통 규칙(`Content-Type`, 크기 제한, JSON 파싱)은 [API_V1](./API_V1.md) "공통 HTTP 규칙"과 `INTERFACES.md` `IF-WIRE-002`를 그대로 따른다. 이 요청은 idempotency key를 받지 않는다. 호출할 때마다 새 토큰(새 `jti`)을 만든다.

### 4.2 검사 순서

허브는 다음 순서로 판정하고 처음 실패한 단계의 오류를 반환한다. 인증·권한이 body 검사보다 먼저인 것은 `IF-WIRE-004`의 규칙과 같다.

1. Bearer가 없거나 owner token·`agent_api_key` 어느 것과도 맞지 않으면 401 `unauthorized`.
2. owner token이면 403 `owner_not_allowed`.
3. 설정에 `[identity]` 절이 없으면 503 `identity_disabled`.
4. wire body가 128 KiB를 넘으면 413 `request_too_large`, `Content-Type`이 `application/json`이 아니면 415 `unsupported_media_type`, JSON이 아니면 400 `invalid_json`.
5. 허용 밖 필드면 422 `unexpected_field`(`details.field`).
6. `aud`가 없거나 빈 문자열이거나 문자열이 아니면, 또는 `ttl_secs`가 정수가 아니거나 1보다 작으면 422 `invalid_field`(`details.field`).
7. `aud`가 `[identity].audiences`에 없으면 422 `unknown_audience`(`details.field="aud"`). 비교는 바이트 단위 완전 일치이며 trim·대소문자 정규화를 하지 않는다.
8. `ttl_secs`가 `[identity].max_ttl_secs`(기본 900)보다 크면 422 `ttl_too_long`(`details: {"field":"ttl_secs","max_ttl_secs":<설정값>}`).

### 4.3 성공 응답

성공은 201이다.

```json
{
  "token": "eyJhbGciOiJFZERTQSIsInR5cCI6IkpXVCIsImtpZCI6IjNmOWExYzJlIn0.eyJpc3MiOiJhZ2VudHMtcGxhdGZvcm0iLCJzdWIiOiJhZ2VudC1hIiwiYXVkIjoiZ2FyYWdlLWRvY3MiLCJpYXQiOjE3OTIxMjkyMDAsImV4cCI6MTc5MjEyOTgwMCwianRpIjoiMDFKQUIzQzRENUU2RjdHOEg5SjBLTU5QUVIifQ.<signature>",
  "sub": "agent-a",
  "aud": "garage-docs"
}
```

- body의 필드는 정확히 `token`, `sub`, `aud` 셋이다. `sub`, `aud`는 토큰 claims와 같은 값이다.
- 응답에는 만료 시각 필드가 없다. 만료의 유일한 출처는 JWT `exp`다. agent는 토큰을 쓰기 직전에 받고, 소비자가 만료로 거부하면 새로 받는다(12절).
- `token`은 응답에서 한 번만 보여 준다. 허브는 토큰을 저장하지 않으며 다시 조회할 방법이 없다.

### 4.4 오류 요약

오류 body는 공통 형식 `{"error":{"code","message","request_id","details"}}`다.

| 상황 | 상태 | code | 비고 |
| --- | --- | --- | --- |
| 인증 없음·틀림 | 401 | `unauthorized` | 기존 code |
| owner token으로 호출 | 403 | `owner_not_allowed` | 신설 |
| `[identity]` 설정 없음 | 503 | `identity_disabled` | 신설 |
| wire body 128 KiB 초과 | 413 | `request_too_large` | 기존 code |
| `Content-Type`이 JSON 아님 | 415 | `unsupported_media_type` | 기존 code |
| JSON 파싱 실패 | 400 | `invalid_json` | 기존 code |
| 허용 밖 필드 | 422 | `unexpected_field` | 기존 code |
| `aud` 누락·빈 문자열, `ttl_secs` 형식·하한 위반 | 422 | `invalid_field` | 기존 code |
| 설정에 없는 `aud` | 422 | `unknown_audience` | 신설 |
| `ttl_secs` > `max_ttl_secs` | 422 | `ttl_too_long` | 신설 |
| 서명 등 내부 오류 | 500 | `internal` | 기존 code |

## 5. 토큰 형식

토큰은 RFC 7519 JWT이며 RFC 7515 compact serialization, RFC 8037의 `EdDSA`(Ed25519) 서명을 쓴다.

### 5.1 Header

```json
{"alg":"EdDSA","typ":"JWT","kid":"3f9a1c2e"}
```

- `alg`는 항상 `"EdDSA"`, `typ`는 항상 `"JWT"`다.
- `kid`는 **항상 있다(MUST)**. 값은 서명 키의 raw 32바이트 공개키에 대한 SHA-256의 앞 4바이트를 소문자 hex 8자로 쓴 것이다. 소비자는 `kid`가 없는 토큰을 401로 거부한다.
- 허브는 이 세 parameter 외의 header parameter를 넣지 않는다.

### 5.2 Claims

```json
{"iss":"agents-platform","sub":"agent-a","aud":"garage-docs","iat":1792129200,"exp":1792129800,"jti":"01JAB3C4D5E6F7G8H9J0KMNPQR"}
```

| claim | 형식 | 값 |
| --- | --- | --- |
| `iss` | 문자열 | 설정 `[identity].issuer`(기본 예시 `"agents-platform"`). 허브는 자기 공개 URL을 모르므로 URL이 아닌 고정 문자열이다 |
| `sub` | 문자열 | 호출자 `agent_id`. `^[a-z][a-z0-9_-]{0,63}$` |
| `aud` | 문자열 | 요청의 `aud`. 배열이 아니라 항상 단일 문자열이다 |
| `iat` | 정수(초) | 발급 시각. 허브 시계의 현재 시각을 초 단위로 버림 |
| `exp` | 정수(초) | `iat + ttl_secs`. `0 < exp - iat ≤ 900` |
| `jti` | 문자열 | 26자 ULID(접두사 없음). 로그 상관 키일 뿐 replay 방지에 쓰지 않는다 |

`nbf` 등 다른 claim은 넣지 않는다. 900초 상한은 소비자와 합의한 wire 상한이며 설정은 이 값을 낮출 수만 있다(9절).

### 5.3 인코딩과 서명

- header와 claims는 공백 없는 UTF-8 JSON으로 직렬화하고 각각 base64url(padding 없음)로 인코딩한다.
- 서명 입력은 ASCII `<header b64>.<claims b64>`이고, 서명은 그 입력에 대한 Ed25519 서명 64바이트를 base64url(padding 없음)로 인코딩한 것이다.
- 토큰은 `<header b64>.<claims b64>.<signature b64>`다.
- JSON key 순서는 위 예시와 같게 내지만 소비자는 순서에 의존하지 않는다.

## 6. 소비자 검증 순서

소비자는 다음 순서로 검증한다(MUST). 앞 단계가 실패하면 뒤 단계를 보지 않는다. 라이브러리를 쓰더라도 허용 알고리즘을 `EdDSA` 하나로 고정하고 아래 각 항목이 실제로 검사되는지 확인한다.

1. **형식**: `Authorization: Bearer <jwt>` 하나. 세 조각, 각 조각이 base64url로 디코딩되고 header·claims가 JSON object여야 한다.
2. **alg 고정**: `alg`가 정확히 `"EdDSA"`, `typ`가 `"JWT"`. `none`, `HS*` 등은 거부한다. `crit`가 있으면 거부한다. 키를 header의 `jwk`·`jku`·`x5u`·`x5c`에서 가져오지 않는다.
3. **kid**: `kid`가 있어야 하고, 설정된 공개키 목록에서 그 `kid`의 키를 찾는다. 없으면 거부한다.
4. **서명**: 찾은 32바이트 공개키로 Ed25519 서명을 검증한다.
5. **iss / aud**: `iss`가 설정된 issuer와, `aud`가 자기 audience와 정확히 같은 문자열이어야 한다.
6. **시각**: `iat`, `exp`가 정수여야 한다. 현재 시각 `now`(초)와 skew 60초로 `iat ≤ now + 60`, `now < exp + 60`, `0 < exp - iat ≤ max_ttl_secs`(기본 900)를 모두 만족해야 한다.
7. **sub**: `^[a-z][a-z0-9_-]{0,63}$`에 맞아야 한다. principal은 `agent:<sub>`다.
8. **소비자 ACL**: 여기서부터는 소비자 자신의 권한 판정이다.

replay 방지는 하지 않는다. `jti`는 기록용 상관 키로만 쓴다.

### 6.1 garage-docs의 합의된 동작 (참조, 정본은 garage-docs)

- 1~7단계 실패(토큰 없음·형식 오류·검증 실패·만료)는 **401**과 한 단어 body(`missing`, `invalid`, `expired`)다. 어떤 키가 있는지 같은 정보는 내지 않는다.
- 토큰은 유효하지만 grant가 없거나 문서가 없으면 **404**다(사람 경로와 같다). 권한 없는 문서의 존재를 드러내지 않는다.
- agent listener는 Pi의 Tailscale 주소 port 8788에서 `GET`/`HEAD`만 받는다(그 밖은 405). 경로는 `/index.json`(그 agent에게 허용된 문서 목록, 항목 `{slug,title,url}`), `/{slug}/…`(HTML 문서), `/healthz`이고 `/admin`과 `/`는 404다.
- 사람 listener는 `Authorization`을 무시하고, agent listener는 `Cf-Access-Jwt-Assertion`을 무시한다. 두 경로의 신원이 섞이지 않는다.
- 문서 grant는 `allowed_agents` 필드에 `agent:<id>` principal로 둔다.
- 설정 객체는 `agent_access {listen, base_url, issuer, audience, max_ttl_secs (기본 900), public_keys:[{kid,key}]}`다. `listen`은 loopback 또는 `100.64.0.0/10` 리터럴이어야 하고, `key`가 정확히 32바이트로 디코딩되지 않으면 serve가 기동을 거부한다.

### 6.2 양쪽 설정 맞추기

운영자는 다음이 맞는지 확인한다(SHOULD).

| 허브 `[identity]` | garage-docs `agent_access` |
| --- | --- |
| `issuer` | `issuer`와 같은 문자열 |
| `audiences`에 포함된 값 | `audience` |
| `max_ttl_secs` | `max_ttl_secs` 이하 (그렇지 않으면 긴 토큰이 거부된다) |
| keygen 출력 `kid`, `public_key` | `public_keys` 항목 `{kid, key}` |

## 7. 키 관리

### 7.1 키 생성

```sh
sudo platformd identity keygen --out /etc/platformd/identity.key
sudo chgrp platformd /etc/platformd/identity.key
```

- OS CSPRNG에서 32바이트를 받아 Ed25519 seed로 쓴다.
- `--out` 파일을 새로 만들고(이미 있으면 덮어쓰지 않고 exit 1) umask와 무관하게 mode 0640으로 둔다. 소유 group은 owner token과 같은 방식으로 운영자가 `platformd`로 바꾼다. 최종 권한은 0640 root:platformd다.
- 파일 내용은 raw seed 32바이트뿐이다. base64·PEM·개행이 없다.
- 설정 파일을 읽지 않는다(`--config` 불필요).
- stdout에 두 줄을 낸다. seed는 어디에도 출력하지 않는다.

```text
kid: 3f9a1c2e
public_key: <raw 32바이트 공개키의 base64 (표준 alphabet, padding 있음, 44자)>
```

- `public_key`는 JWT 조각과 달리 **표준 base64(padding 있음)**다. 운영자는 두 값을 garage-docs `agent_access.public_keys`에 `{kid, key}`로 붙여 넣는다.
- 종료 코드: 0 성공, 1 파일 존재·쓰기 실패, 2 사용법 오류.
- v1에는 공개키를 다시 출력하는 명령이 없다. keygen 출력을 잃었으면 새 키를 만들고 8절 교체 절차를 밟는다.

### 7.2 기동 시 로드

- `[identity]`가 있으면 허브는 owner token처럼 기동 때 `signing_key_file`을 한 번 읽어 메모리에 둔다. 파일을 읽지 못하거나 크기가 정확히 32바이트가 아니면 기동을 거부한다(설정 오류).
- 기동 로그에 identity 활성 여부, `kid`, `audiences`를 남긴다. `kid`와 공개키는 비밀이 아니다. seed는 남기지 않는다.
- 키를 바꾸려면 허브를 재시작한다. hot reload는 없다.
- v1에는 공개키 endpoint가 없다.

## 8. 키 교체와 폐기

### 8.1 서명 키 교체

순서는 **허브 새 키 → garage-docs `public_keys`에 추가(kid 공존) → 허브 재시작 → 옛 키 제거**다.

1. 새 키 파일을 만든다: `sudo platformd identity keygen --out /etc/platformd/identity.next.key`, `chgrp platformd`. 출력된 `kid`, `public_key`를 적어 둔다.
2. garage-docs `agent_access.public_keys`에 새 `{kid, key}`를 **추가**한다. 옛 키는 남긴다. `kid`가 달라 두 키가 공존한다.
3. 허브 `[identity].signing_key_file`을 새 파일로 바꾸고 `platformd.service`만 재시작한다. 기동 로그의 `kid`가 새 값인지 확인한다.
4. 옛 키로 서명된 토큰이 모두 만료된 뒤(마지막 옛 키 발급 뒤 `max_ttl_secs` + 60초, 기본 16분) garage-docs에서 옛 키를 제거하고 허브의 옛 키 파일을 지운다.

서명 키 유출이 의심되면 4단계에서 기다리지 않고 바로 옛 키를 제거한다. 그동안 옛 키 토큰을 가진 agent는 401을 받고 새 토큰을 받으면 된다.

### 8.2 agent key rotate와 토큰 차단

허브의 agent key rotate는 새 토큰 발급을 막고 이미 발급된 토큰은 exp(≤15분)까지 유효하다. 즉시 차단은 garage-docs `revoke SLUG agent:<id>`가 맡는다.

허브는 발급한 토큰을 기억하지 않으므로 폐기 목록을 두지 않는다.

## 9. 설정

`INTERFACES.md` `IF-CFG-001`의 선택 절 `[identity]`다.

```toml
[identity]
signing_key_file = "/etc/platformd/identity.key"
issuer = "agents-platform"
audiences = ["garage-docs"]
default_ttl_secs = 600
max_ttl_secs = 900
```

- 절이 없으면 기능이 꺼지고 `POST /v1/identity/tokens`는 503 `identity_disabled`다. 나머지 API는 영향이 없다.
- 절이 있으면 `signing_key_file`, `issuer`, `audiences`가 필수다. `default_ttl_secs`(기본 600), `max_ttl_secs`(기본 900)는 선택이다.
- 기동 거부 조건: `issuer`가 빈 문자열, `audiences`가 비었거나 빈 문자열·중복이 있음, `1 ≤ default_ttl_secs ≤ max_ttl_secs ≤ 900`이 아님, 알 수 없는 키(`deny_unknown_fields`), 키 파일 오류(7.2).
- 상대 경로는 `IF-CFG-001` 규칙대로 config 파일 위치 기준이다. seed를 설정 파일에 직접 넣지 않는다.
- 600·900은 튜닝 값이라 설정에 둔다. 테스트는 기본값 숫자를 단언하지 않고 "설정이 적용된다·범위 안이다"만 본다. 900은 소비자와 합의한 wire 상한이기도 해서 설정이 넘을 수 없다.

## 10. CLI

```sh
platform --token-file agent.key identity token --aud garage-docs [--ttl 600]
```

- `POST /v1/identity/tokens`에 `{"aud":<--aud>}`를, `--ttl`이 있으면 `"ttl_secs"`도 넣어 보낸다. `--ttl`이 없으면 필드를 생략해 서버 기본값을 쓴다.
- 성공하면 stdout에 **토큰 한 줄만** 쓴다(끝에 개행 하나). `token=$(platform … identity token …)`로 그대로 받을 수 있게 다른 출력은 없다.
- 전역 `--json`을 주면 201 응답 JSON 원문을 stdout에 쓴다(`IF-CLI-001`과 같음).
- 오류는 stderr에 쓰고 종료 코드는 `IF-CLI-001`과 같다: 0 성공, 1 서버 오류 응답, 2 사용법·설정(`--aud` 누락 포함), 3 네트워크.
- CLI는 토큰을 파일·로그에 쓰지 않는다.

## 11. 보안 메모

- **bearer 토큰이다.** 가진 사람이 곧 그 agent다. 만료 전까지 누구나 쓸 수 있다.
- **읽기 전용 용도다.** 첫 소비자 garage-docs는 agent listener에서 `GET`/`HEAD`만 받는다. 토큰은 허브 API에 통하지 않는다.
- **15분 상한.** `exp - iat ≤ 900`이고 소비자도 같은 상한을 검사하므로, 유출된 토큰의 피해 창은 길어야 발급 뒤 15분(+skew 60초)이다.
- **로그에 남기지 않는다.** 허브는 발급 로그에 `sub`, `jti`, `aud`, `exp`만 남긴다. 토큰 원문, seed, `agent_api_key`는 허브·CLI·소비자 어느 로그에도 남기지 않는다. 요청·응답 body를 통째로 남기는 tracing을 켜지 않는다.
- **서명 키 = 모든 agent 신원.** seed 파일을 가진 사람은 아무 `sub`나 서명할 수 있다. owner token과 같은 권한(0640 root:platformd)으로 다룬다. `platformd backup`은 DB만 복사하므로 owner token처럼 서명 키도 backup에 들어가지 않는다. 공개 저장소·`deploy/local/`·운영자 머신에 복사하지 않는다.
- **폐기·replay 방지 없음.** 차단은 소비자 쪽(`revoke`)과 짧은 수명이 맡는다.
- **URL·메시지 본문에 넣지 않는다.** 토큰은 `Authorization` header로만 보낸다. query string, 허브 메시지 본문, 프롬프트에 넣지 않는다.

## 12. agent가 쓰는 법

garage-docs 문서 URL은 agent 리스너의 `index.json`이 주는 `url`(`http://<tailscale-ip>:8788/<slug>/`)을 그대로 쓴다. `docs.gurbur.dev`는 사람용(Cloudflare Access 뒤)이라 토큰으로 열 수 없다.

```sh
export PLATFORM_URL=http://<platform-host>:7340
umask 077
token=$(platform --token-file agent.key identity token --aud garage-docs) || exit 1
printf 'Authorization: Bearer %s\n' "$token" > headers       # 명령줄·process 목록에 토큰을 펼치지 않는다
curl -sS -H @headers http://<tailscale-ip>:8788/index.json   # 허용된 문서 목록 {slug,title,url}
curl -sS -H @headers http://<tailscale-ip>:8788/<slug>/      # index.json의 url을 그대로
rm -f headers
```

- 토큰은 쓰기 직전에 받는다. garage-docs가 401 `expired`를 주면 새로 한 번 받아 다시 시도한다.
- 401 `invalid`가 계속되면 허브와 garage-docs의 키·issuer·audience 설정이 어긋난 것이다(6.2). 토큰을 바꿔 가며 반복하지 말고 운영자에게 알린다.
- 404는 grant가 없거나 문서가 없다는 뜻이다. 토큰 문제가 아니다.

## 13. 수용 시나리오

구현 뒤 검증할 기준이며 현재 구현 완료를 뜻하지 않는다.

1. agent-a key로 `{"aud":"garage-docs"}`를 보내면 201이고, 토큰의 `sub`는 `agent-a`, `exp - iat`는 설정 `default_ttl_secs`이며, 응답 body의 필드는 `token`·`sub`·`aud` 셋뿐이다.
2. 토큰 header에 `kid`가 있고, 그 값은 keygen이 출력한 `kid`와 같으며, keygen이 출력한 공개키로 서명이 검증된다.
3. owner token으로 호출하면 403 `owner_not_allowed`, 인증이 없으면 401 `unauthorized`다.
4. `[identity]` 없이 기동한 허브는 503 `identity_disabled`를 반환하고 나머지 API는 그대로 동작한다.
5. `aud` 누락·빈 문자열은 422 `invalid_field`, 설정에 없는 `aud`는 422 `unknown_audience`, `ttl_secs`가 `max_ttl_secs`를 넘으면 422 `ttl_too_long`, `sub` 필드를 보내면 422 `unexpected_field`다.
6. 발급된 토큰을 허브 API의 Bearer로 쓰면 401이다.
7. 허브 로그 어디에도 토큰 원문과 seed가 없고, 발급 로그에는 `sub`·`jti`·`aud`·`exp`가 있다.
8. 키 파일이 32바이트가 아니거나 `max_ttl_secs`가 900을 넘거나 `default_ttl_secs > max_ttl_secs`이면 허브가 기동을 거부한다.
9. `platform identity token --aud garage-docs`의 stdout은 토큰 한 줄뿐이고, 서버 오류는 stderr와 exit 1이다.
10. agent key를 rotate하면 옛 key로는 새 토큰을 받지 못하고(401), 이미 받은 토큰은 그대로 디코딩·검증된다.

## 14. 결정 기록 (2026-10-16)

사용자가 초안을 승인하며 정한 것이다. 근거는 [ADR-0005](../adr/0005-identity-attestation-tokens.md).

- 403 `owner_not_allowed`는 새 code다. owner token으로 agent 전용 write를 할 때의 403 `forbidden`(`IF-WIRE-003`)과 공존한다.
- 201 body는 정확히 `{"token","sub","aud"}`다. 응답에 만료 시각 필드를 두지 않으며 만료의 유일한 출처는 JWT `exp`다. agent는 쓰기 직전에 토큰을 받고, 소비자가 401 `expired`를 주면 새로 받는다. CLI에 만료 출력 옵션은 두지 않는다.
- 503 `identity_disabled`는 401·403 뒤, body 검사 앞에 둔다(4.2).
- 구현은 `ed25519-dalek` + 직접 만든 JWT 인코더다. RFC 8037 Appendix A.4의 Ed25519 JWS test vector와 garage-docs(`jwt/v5`) interop test로 검증한다.
- 키 교체는 옛 키 제거 전에 `max_ttl_secs` + 60초를 기다린다(8.1). keygen은 공개키를 다시 출력하지 않는다(7.1).
- `ttl_too_long`의 `details.max_ttl_secs`, keygen의 덮어쓰기 거부와 출력 형식, 설정 범위 `1 ≤ default_ttl_secs ≤ max_ttl_secs ≤ 900`은 초안대로 확정했다.
