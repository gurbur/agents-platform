# ADR-0004: credential 형식과 보관 — owner token 파일, agent key는 해시, receiver secret은 평문

- **상태**: 채택됨
- **날짜**: 2026-10-15

## 맥락
API_V1은 owner bootstrap을 "후속 구현 세부"로 두었고, "API key 검증값은 hash로 보관할 수 있고 receiver secret은 outbound callback에 필요하므로 platform이 보관한다"고만 적었다. 공개 저장소이고 운영은 사설 tailnet 안이다.

## 결정
`INTERFACES.md` `IF-AUTH-001`이 정본. 요지: owner token은 `owner_token_file`로만 주입하고 메모리 보관·상수시간 비교; `agent_api_key`는 `pak_`+base64url(32B) 평문 1회 노출, DB에는 SHA-256 hex; `receiver_secret`은 `prs_`+base64url(32B), DB 평문. Bearer 판정은 owner → agent hash 조회 → 401.

## 대안
- **agent key를 argon2/bcrypt로** — 기각: 키가 256bit CSPRNG라 사전 공격 의미가 없고, 요청마다 KDF 비용을 Pi에 지불할 이유가 없다.
- **receiver secret을 KMS/암호화 보관** — 기각(v1): 복호화 키를 같은 머신에 둬야 해 위협 모델상 이득이 없다. DB 파일 권한(service account 전용 디렉터리)으로 보호한다. 암호화는 사양의 "이후 범위".
- **owner token을 config 인라인/환경변수로** — 기각: 설정 파일이 실수로 커밋되거나 `ps`/journal에 노출될 위험. 파일 분리가 가장 단순한 경계.

## 근거
API_V1 "인증과 권한" 절의 허용 범위 안에서 가장 단순하고 검증 가능한 선택. prefix(`pak_`/`prs_`)는 로그·지원 시 어떤 종류의 비밀이 유출됐는지 식별하게 해 주고, receiver secret으로 platform API를 호출하는 실수를 코드로도 막기 쉽다.

## 결과 · 트레이드오프
- 좋아지는 것: DB 유출 시 agent key는 즉시 쓸 수 없음, owner token 비교가 타이밍 안전, 구현이 짧다.
- 감수하는 것: receiver secret은 DB 접근 = 유출. 데이터 디렉터리 권한과 backup 저장소 권한으로 막는다.
- 영향받는 인터페이스: `IF-AUTH-001`, `IF-API-AGENTS`, `IF-API-ROTATE`, `IF-DB-001`(agents 컬럼).

## 재평가 트리거
플랫폼이 tailnet 밖으로 노출되거나 다중 owner가 필요해지면 다시 본다.
