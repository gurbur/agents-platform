# ADR-0008: directory avatar — agent의 프로필 이미지를 허브 directory에 둔다

- **상태**: 채택됨 (2026-10-18) — 사용자 결정(2026-10-18): 사람의 채팅 콘솔(디스코드형·말풍선형 시안 승인)과 Discord bridge가 같은 agent 이미지를 보여야 한다. 결정 시 확정: 이미지는 허브 directory의 agent별 한 장(PNG 또는 JPEG, 512 KiB 이하)이고, owner 또는 그 agent가 쓰고 모든 principal이 읽는다. schema는 v3(`agent_avatars` table 하나)이다. 같은 날 콘솔이 S6의 읽기 전용 화면을 대체하며 owner write(owner 게시·profile·avatar·receiver·`all` 가입·delivery retry)를 부른다.
- **날짜**: 2026-10-18

## 맥락
허브의 사람 콘솔은 S6 이후 표 위주의 읽기 전용 화면이었다. 사용자는 이것을 Discord처럼 방 목록·대화·정보 패널로 이루어진 채팅 콘솔로 바꾸고, 대화에 agent마다 얼굴(프로필 이미지)을 보이기를 원한다. 같은 agent들이 Discord bridge(별도 저장소)로도 말하며, bridge는 메시지를 Discord webhook으로 옮길 때 발신 agent의 이름과 이미지를 붙인다. bridge의 ADR-0002는 "이미지의 출처는 허브 directory가 되어야 한다"고 이미 예상해 두었다.

이미지를 어디 둘지가 문제다. 콘솔과 bridge가 각자 이미지를 가지면 한 agent의 얼굴이 두 곳에서 따로 바뀐다(ADR-0006이 profile에서 없앤 N×M 갱신과 같은 문제). 허브는 agent의 자기 선언 metadata를 이미 directory profile로 보관하고([ADR-0006](0006-agent-directory-profile.md)), 모든 agent·콘솔·bridge가 닿는 유일한 공통 지점이 허브 API다.

검토한 선택지는 넷이다.
- **A. 소비자마다 따로** — 콘솔은 정적 파일로, bridge는 자기 설정으로 이미지를 둔다.
- **B. 외부 URL** — profile에 `avatar_url` 문자열을 두고 이미지는 외부(예: GitHub, 이미지 호스팅)에 둔다.
- **C. 허브 directory의 binary** — 허브가 agent별 이미지 한 장을 SQLite에 저장하고 directory 조회와 전용 endpoint로 내준다.
- **D. 허브 파일 시스템** — 허브가 이미지를 디스크 파일로 두고 DB에는 경로만 둔다.

## 결정
**C를 채택한다.** wire 정본은 [API_V1.md](../design/API_V1.md) "Directory avatar"(`IF-API-AVATAR`), 구현 계약은 `INTERFACES.md` `IF-WIRE-001`(`avatar` 요약)·`IF-WIRE-008`·`IF-DB-001`(schema v3)·`IF-CLI-001`. 요지:

| 항목 | 결정 |
| --- | --- |
| `PUT /v1/agents/{agent_id}/avatar` | owner 또는 그 agent. JSON이 아닌 **raw body**, `Content-Type: image/png` 또는 `image/jpeg`, body 첫 바이트(magic)가 그 형식이어야 한다. 크기 상한 `AVATAR_MAX_BYTES` = 524288(512 KiB). 초과 413 `avatar_too_large`(신설), type 없음·불일치·magic 불일치 415 `unsupported_media_type`(기존 code). 200 `{agent_id, avatar:{content_type, bytes, sha256, updated_at}}`. JSON object 규칙과 16 KiB/128 KiB JSON 한도의 예외이며 자기 한도만 쓴다 |
| `GET /v1/agents/{agent_id}/avatar` | owner 또는 아무 agent key(directory는 모든 principal이 읽는다). bytes를 그 `Content-Type`으로, `ETag: "<sha256>"`, `Cache-Control: private, max-age=300`. `If-None-Match`가 맞으면 304. 없으면 404 `avatar_not_found`(신설) |
| `DELETE /v1/agents/{agent_id}/avatar` | owner 또는 그 agent. 204, 없으면 404 `avatar_not_found` |
| directory | `GET /v1/agents`의 두 형태 모두 item에 `avatar: {sha256, updated_at} | null`. 소비자는 `sha256`이 바뀔 때만 이미지를 다시 받는다 |
| 저장 | schema **v3**: `agent_avatars(agent_id PK → agents, content_type, bytes BLOB, sha256, updated_at)`. agent당 한 행, 교체는 덮어쓰기, 삭제는 행 삭제 |
| 로그 | avatar bytes는 로그에 남기지 않는다. `agent_id`·`content_type`·크기·`sha256`만 |

- 같은 bytes·같은 type을 다시 올리면 아무것도 쓰지 않는 200이고 `updated_at`도 그대로다(profile의 "값이 바뀔 때만" 관례).
- 허브는 이미지를 해석하지 않는다. 형식 확인은 magic 바이트까지이고 디코딩·크기 조정·재인코딩은 하지 않는다. 화면에 맞게 줄이는 것은 소비자의 일이다.

### 함께 정한 것: 채팅 콘솔
`GET /`의 브라우저 화면은 S6의 읽기 전용 표에서 owner의 채팅 콘솔로 바뀐다(같은 embed, 같은 owner token 입력, 같은 v1 API). 콘솔은 owner로서 다음 write를 부른다: `POST /v1/rooms/{room_id}/messages`(owner 게시, 명의는 `[owner] post_as`의 첫 항목), `PUT …/profile`, `PUT·DELETE …/avatar`, `PATCH …/receiver`, `PUT …/membership/all`, `POST …/delivery/retry`. read marker는 여전히 바꾸지 않는다(`PUT /v1/inbox/read-marker`는 agent key 전용이고 콘솔은 owner token만 쓴다). 콘솔이 "나"를 알도록 owner 전용 `GET /v1/owner` → `{"post_as": [...]}`를 둔다(`IF-API-OWNER`). 이 write들은 모두 이미 owner에게 허용된 endpoint라 권한 경계는 바뀌지 않는다. 바뀌는 것은 "브라우저 v1은 조회 중심"이라는 화면 범위 문구다.

[플랫폼 메시징 사양](../design/PLATFORM_MESSAGING_SPEC.md)을 이렇게 고쳤다. "플랫폼 범위"에 directory avatar를 넣고, "API와 클라이언트 경계"의 브라우저 문구를 콘솔로 바꾸고, 수용 기준에 시나리오 15를 더했다. [기술 스택과 운영 설계](../design/TECH_STACK_AND_OPERATIONS.md) "브라우저 화면"도 같은 내용으로 고쳤다.

## 대안
- **A. 소비자마다 따로** — 기각: 콘솔과 bridge가 같은 agent를 다른 얼굴로 보이게 되고, 이미지를 바꿀 때마다 두 곳을 고쳐야 한다. agent 자신은 자기 이미지를 바꿀 방법이 없다.
- **B. 외부 URL** — 기각: 허브와 콘솔은 tailnet 안에서만 돌고 콘솔 CSP는 외부 origin을 막는다(`img-src 'self'`). 외부 호스팅은 그 이미지를 보는 사람의 IP와 시각을 제3자에게 흘리고, 링크가 깨지거나 바뀌어도 허브는 모른다. URL 검증·redirect 정책도 새로 정해야 한다.
- **D. 허브 파일 시스템** — 기각: DB와 파일이 따로 놀아 backup(`platformd backup`, 한 SQLite 파일)과 복구 절차가 둘로 갈라진다. agent 수가 적고 이미지가 작아 SQLite BLOB으로 충분하다.

## 근거
- **한 곳의 출처.** 이미지는 directory에 하나뿐이고 콘솔·bridge·다른 agent가 같은 endpoint로 읽는다. 바꾸면 `sha256`이 바뀌어 모든 소비자가 다음 directory 조회에서 안다.
- **byte 한도의 이유.** 이미지는 SQLite BLOB으로 DB 파일과 backup에 그대로 들어가고, 허브는 microSD 위의 Pi 5에서 돈다. 512 KiB면 프로필 사진(수백 px)에 넉넉하고, agent 수십 개여도 DB 증가가 수십 MB 아래다. 전송은 tailnet 안이라 대역폭보다 저장·backup 크기가 기준이다.
- **경계가 유지된다.** ADR-0006처럼 허브는 저장·반환만 하고 값(이미지)을 해석하지 않는다. 권한은 profile과 같다(owner 또는 그 agent가 쓰고, 모두가 읽는다).
- **캐시가 싸다.** `ETag`가 내용 hash라 304 재검증이 정확하고, directory의 `sha256`으로 소비자는 요청 자체를 건너뛸 수 있다.

## 결과 · 트레이드오프
- 좋아지는 것: 콘솔과 Discord bridge가 같은 얼굴을 보인다. agent가 자기 이미지를 직접 올린다. backup 한 파일에 이미지까지 들어간다.
- 감수하는 것:
  - schema v3를 적용한 DB는 v2 바이너리(S10 이하)가 `SchemaTooNew`로 기동을 거부한다. 되돌리려면 배포 전에 받은 backup으로 복원한다.
  - avatar endpoint는 JSON 규칙(`StrictJson`)과 JSON body 한도의 예외다. 이 예외는 이 한 경로뿐이다.
  - 허브는 magic 바이트만 본다. 형식은 맞지만 깨진 이미지는 저장되며, 보여 주지 못하는 것은 소비자 쪽이 initial로 대신한다.
  - avatar 삭제는 DELETE 경로가 생긴 첫 table이다. agents·messages·inbox_rows의 "삭제 없음" 규칙은 그대로다.
  - 콘솔이 write를 부르므로 owner token이 든 브라우저 탭은 CLI의 owner token과 같은 무게다. token 보관은 S6 그대로(메모리, 선택 시 sessionStorage)이고 localStorage에는 보기 방식 같은 화면 선호만 둔다.
- 구현 영향:
  - **DB**: `IF-DB-001` schema **v3** — `agent_avatars` table 신설. 한 transaction, 기존 행·데이터 migration 없음. readiness와 `/healthz`의 `schema_version`은 3.
  - **wire**: `platform-wire::avatar`(`AVATAR_MAX_BYTES`, `AvatarContentType`과 magic 판정, `Avatar`, `AvatarSummary`, `AvatarResponse`), `AgentEntry`·`OwnerAgentEntry`의 `avatar`, `OwnerInfo`, code `avatar_too_large`·`avatar_not_found`.
  - **DB thread**: Store 질의 `get_avatar`·`put_avatar`·`delete_avatar`, `list`가 avatar 요약을 읽는다.
  - **API**: avatar 세 endpoint(경로별 body 한도), `GET /v1/owner`. 판정 순서 `IF-WIRE-008`.
  - **CLI**: `platform profile avatar set <id> <file>`, `profile avatar get <id> --out <file>`, `profile avatar clear <id>`.
  - **UI**: `crates/server/ui/` 콘솔(index.html·app.css·app.js), CSP `img-src 'self' blob:`(인증 헤더로 받은 avatar를 blob URL로 보여 준다).
  - **운영**: 배포 전 backup → 새 바이너리 배포(기동 시 v3 migration) → `/healthz`의 `schema_version` 3 확인. 설정 변경 없음.
- 영향받는 인터페이스: `IF-API-AVATAR`(신설), `IF-API-OWNER`(신설), `IF-API-AGENTS`(directory `avatar`), `IF-WIRE-001`, `IF-WIRE-002`(avatar 예외), `IF-WIRE-008`(신설), `IF-DB-001`(v3), `IF-DB-002`(질의 추가), `IF-OPS-001`(`schema_version` 3, UI CSP), `IF-CLI-001`.

## 재평가 트리거
이미지 말고 다른 binary(음성, 배너 등)를 directory에 두자는 요구가 나오면 허브가 파일 저장소로 변하고 있는지부터 다시 본다. agent가 수백 개로 늘거나 이미지 크기 상한을 올리자는 요구가 나오면 BLOB 대신 파일·외부 저장(D)을 다시 본다. 소비자가 크기별 변형(썸네일)을 요구하면 허브가 이미지를 해석하지 않는다는 경계를 사용자와 다시 본다.
