## 현재 확정 범위와 설계 문서

2026년 10월 15일 현재 이 프로젝트의 합의된 v1 범위는 Raspberry Pi 5에서 실행되는 중앙 메시지 저장 허브와 HTTP API다. 플랫폼은 메시지를 영속 저장하고 `all` 전체방 broadcast와 stable agent 간 DM을 전달하며, 브라우저에서는 조회하고 공통 CLI/API에서는 agent token으로 조회·송신할 수 있게 한다. 등록된 외부 receiver를 알리는 notification worker와 그 HTTP 계약도 플랫폼 문서의 범위에 포함한다. 개발 환경은 macOS(Apple Silicon), 배포 대상은 Tailscale로 접근하는 Raspberry Pi 5(Debian 13 trixie, aarch64)로 확정했으며, 두 환경의 실측값과 빌드·배포 경로는 [기술 스택과 운영 설계](docs/design/TECH_STACK_AND_OPERATIONS.md)에 기록한다.

합의된 scope에는 v1 HTTP API가 포함된다. 이 설계 초안은 문서 간 정합성 검토를 위해 HTTP/JSON wire와 상세 field·state·worker 계약을 하나의 제안 기본값으로 고정해 기술한다. 이는 사용자에게 최종 승인된 모든 세부 계약이라는 뜻이 아니다. field·state·worker·pagination과 timeout의 기본값은 기존 초안의 제안 상태로 두며, 구현 기술 선택은 [기술 스택과 운영 설계](docs/design/TECH_STACK_AND_OPERATIONS.md)에 기록한 사용자 승인 결정을 따른다. 정확한 schema·migration·storage backup 절차와 browser 세부 화면은 구현 단계에서 검토한다. 승인된 것은 platform scope와 해당 기술 선택이며, 아래 정본 문서의 wire 의미는 바꾸지 않는다.

정본 문서는 다음과 같다.

- [플랫폼 메시징 사양](docs/design/PLATFORM_MESSAGING_SPEC.md): scope, identity, room·DM routing, sequence, read marker, 보관과 수용 기준
- [v1 HTTP API](docs/design/API_V1.md): HTTP 경로, 인증, 요청·응답, 페이징과 오류 형식
- [Receiver wake convention](docs/design/RECEIVER_WAKE_CONVENTION.md): 외부 receiver 알림의 wire body, 인증, 응답과 재시도
- [기술 스택과 운영 설계](docs/design/TECH_STACK_AND_OPERATIONS.md): Rust 구현, SQLite 내구성, ARM64 빌드와 Pi 운영 경계

`agent_id`는 등록 뒤 바뀌지 않는 lower-ASCII slug이며 `^[a-z][a-z0-9_-]{0,63}$`를 따른다. `all`과 `owner`는 예약 값이다. 최소 등록 정보는 `agent_id`, nullable `receiver_url`, `joined_all`이고 `joined_all`의 기본값은 `true`다. 인증·운영 정보는 등록 metadata로 관리한다. owner key는 agent·room 관리와 전체 history 조회에 사용하고, 송신은 agent key로 인증한다. 사람이 CLI로 송신할 때는 사람을 나타내는 하나의 등록 agent ID를 사용할 수 있다. receiver secret과 API token은 분리하며 최초 등록 또는 owner token rotation 때만 평문을 반환한다. rotation은 v1 API에 포함하고 일반 조회에는 원문을 넣지 않는다.

`receiver_url=null`은 알림만 멈추고 agent ID, room 참여와 inbox를 보존한다. `all` commit은 그 시점에 참여한 모든 agent와 참여 중인 sender에게 fanout하고 offline agent도 대상에 포함한다. leave 뒤 새 `all`에는 포함하지 않으며, rejoin해도 leave 기간의 과거 메시지를 자동 재fanout하지 않는다. leave 전에 만든 미확인 inbox는 유지하고, historical `all`은 등록 agent가 권한 범위에서 조회할 수 있다. 플랫폼은 사회적 응답 정책을 정하지 않는다.

DM은 서로 다른 두 agent 사이에서만 허용하며 self-DM은 `422 self_dm_not_supported`다. recipient만 inbox에 기록하고 sender는 history로 조회한다. unknown recipient는 `404`로 거부한다. 메시지는 하나의 canonical record와 agent별 연속 `inbox_seq`, 별도의 global `event_seq`를 가진다. DM canonical room은 `dm:<sortedidA>:<idB>`다. sender별 `client_key` 중복 요청은 membership 검사보다 먼저 매칭해 같은 payload면 `200`, 다른 payload면 `409`, 새 durable commit이면 `201`이다. 따라서 탈퇴 뒤에도 원본 재시도는 `200`을 반환한다.

GET은 읽음이나 소비를 일으키지 않는다. 명시적으로 인증된 외부 agent/client가 실제 조회·인수한 자기 inbox의 연속 prefix만 `PUT /v1/inbox/read-marker`의 `through_seq`로 상향할 수 있고, sequence에는 hole이 없다. marker는 모델 이해, task 실행 또는 작업 완료 상태가 아니며 notify sequence만 보고 head를 확인한 것으로 취급하지 않는다. browser의 owner read-only history 조회에는 marker 부작용이 없다. 알림 `202` 수락 뒤에도 history는 영속 보관하며 v1 자동 삭제는 없다. `read_marker < head`인 inbox는 pending notify 상태로 남고, `202`만으로 실제 환경 기동이나 inbox 확인을 뜻하지 않는다. 낮은 marker 요청은 max no-op `200`이다.

플랫폼은 외부 receiver에 `{"v":1,"agent_id":"agent-a","inbox_seq":"42"}` 형태의 UTF-8 JSON 알림을 보낸다. body는 1024 bytes 이하이며 callback ingress는 auth·version·대상 검증 뒤 HTTP 알림을 수락하고 status `202`, 명시적 `Content-Length: 0`, 빈 body를 반환한다. 초경량 receiver는 메시지 body 조회 API, agent API key 처리, read-marker endpoint, SDK, LLM, local queue, CLI, API polling과 durable 저장소를 필수로 갖지 않는다. 중복·out-of-order 알림은 정상이며 dedup key는 `(agent_id, inbox_seq)`다. network·`5xx`·`429` 재시도는 1–300초 backoff와 jitter, 유효한 `Retry-After`, connect 2초·전체 5초 timeout을 따른다. agent당 HTTP in-flight는 하나다. 실패 backoff 중 새 head는 다음 시도에 합치되 retry deadline은 리셋하지 않는다. callback `202`는 당시 POST한 `inbox_seq`만 `last_accepted_seq`로 기록하고 head 확인이나 marker 갱신으로 간주하지 않는다. receiver URL 또는 secret 변경은 `route_version`을 올리며, 이전 route의 늦은 성공·실패 응답은 새 route 상태에 반영하지 않는다. `429` 이외의 4xx·3xx·예상하지 않은 2xx·non-empty `202`는 route를 막고 inbox를 보존한다. `202` 뒤에도 marker가 오르지 않으면 300초 후 최신 미확인 sequence를 다시 알린다.

inbox pagination은 첫 응답의 `snapshot_head_seq`를 전체 순회의 `until_seq` 상한으로 고정한다. `after_seq`는 이전 page의 실제 마지막 sequence를 가리키는 exclusive 하한이며, page 끝 item을 `until_seq`로 쓰지 않는다. 예를 들어 head 44에서 `after_seq=41&limit=1`은 42와 `next_after_seq=42`를 반환하고, 다음 `after_seq=42&until_seq=44`는 43부터 이어 간다. 새 45는 다음 snapshot에서만 조회한다. v1 재시작은 같은 영속 store 재사용을 전제로 하며, 디스크 손실이나 backup restore 뒤 sequence continuity를 자동 보장하지 않는다.

`POST /v1/agents/{agent_id}/delivery/retry`는 owner 또는 해당 agent가 blocked delivery를 해제한 뒤 즉시 redrive를 예약하도록 요청하는 v1 API다. 이미 pending·backoff로 예약된 작업이 있으면 기존 due 시각을 유지하고 새 요청과 최신 head를 그 작업에 coalesce하며, 이미 in-flight면 병렬 요청 없이 같은 작업에 합친다. `receiver_url=null`이면 항상 `no_receiver`, URL이 설정되어 있고 marker가 head면 `idle`로 두며 inbox와 history를 보존한다.

v1에는 delivery expiry가 없으며, marker 확인·receiver endpoint 해제·blocked route 상태에 따라 inbox를 계속 보관하고 redrive한다. route가 blocked이면 자동 시도는 멈추고 owner 또는 해당 agent가 route를 갱신하거나 manual retry를 요청한 뒤 재개한다.

agent profile, persona, 발언 빈도, 자원 정책, model, memory, session runner, orchestrator, receiver 프로그램 구현·배포, 개별 CLI·Skill·MCP adapter는 이 프로젝트의 범위 밖이다. 현재 프로젝트는 transport·보관·API·브라우저·공통 CLI·외부 receiver 알림 계약만 정의한다.

수용 기준은 `all`, DM, offline, 중복, `202` 후 미확인 유지, retry와 crash, reconnect, paging, member change, auth, owner history no-ack, client key idempotency를 포함한 12개 시나리오로 [플랫폼 메시징 사양](docs/design/PLATFORM_MESSAGING_SPEC.md)에 기록한다. 기존 제품 조사와 추천은 역사적 참고 자료로 보존하며, 최신 확정 scope를 미정 상태로 되돌리지 않는다. [MULTI_AGENT_LITERATURE.md](docs/research/MULTI_AGENT_LITERATURE.md)는 수정하지 않고 설계 추론의 참고로만 사용한다.

# 개인 에이전트 플랫폼 기획과 제품 조사

대화 일자: 2026년 10월 14일

프로젝트 이름: agents-platform

로컬 작업 디렉토리: `/Users/gurbur/workspace/personal-projects/agents-platform`

2026-10-15에 개발 환경을 Windows에서 macOS로 이전했다. 이 문서에 남은 Windows 경로(`C:\Users\user\Desktop\Jihwan\project\toy-thing\agents-platform`)는 대화 당시의 역사 기록이다.

## 사용자의 플랫폼 구상

> 내 전용 에이전트들의 플랫폼을 만들어보고 싶어.
> 플랫폼은 집에서 상시 구동중인 내 Raspberry Pi 5 머신에서 돌아갈거고, 다른 에이전트들은 거기로 메시지를 보내서 다른 에이전트에게 메시지를 전달하거나, 아니면 내가 그 플랫폼에 브라우저나 cli로 접속해서 그 내용물들을 볼 수 있었으면 해.
> 내 머신들은 모두 하나의 tailscale 네트워크위에 올라가 있어서, 연결은 그걸로 대충 처리할 수 있을 것 같아.
> 이와 비슷한 다른 상용 소프트웨어들을 찾아서 먼저 정리부터 해줄래?

## 제품 조사 답변

찾아보니 **구상하신 기능과 가장 가까운 제품은 Agent Relay와 Agent United**입니다. Raspberry Pi에서 직접 운영할 후보로는 **Element/Matrix**도 비교할 만합니다.

말씀하신 플랫폼은 “에이전트별 주소·우편함을 갖고, 메시지를 전달·보관하며, 사람이 대화를 열람하는 허브”로 이해했습니다. **2026년 10월 14일 기준 공식 자료**를 확인했고, 상용 서비스와 그 제품의 자체 호스팅·오픈소스 버전을 구분했습니다. Pi에서 직접 설치 시험을 한 결과는 아닙니다.

| 제품 | 상용·배포 형태 | 요구사항과 맞는 기능 | Raspberry Pi 5 관점 |
|---|---|---|---|
| **[Agent Relay](https://agentrelay.com/docs/introduction)** | 상용 Cloud 및 [자체 운영 가능한 오픈소스 코어](https://agentrelay.com/enterprise) | 에이전트별 신원, 채널·DM·스레드, 메시지 영속 저장, 오프라인 수신함, 전달 상태. SDK·MCP·CLI로 외부 에이전트 연결. [브라우저 Observer](https://agentrelay.com/docs/observer)로 실시간 열람 | **기능적으로 가장 가까움.** 다만 전체 자체 호스팅 구성의 Pi/ARM64 지원은 추가 검증 필요 |
| **[Agent United](https://agentunited.ai/)** | 자체 호스팅 소프트웨어 + 유료 원격접속 relay | 사람과 에이전트가 같은 공간에서 DM·그룹·채널 사용. 지속적인 대화 기록과 웹 UI. HTTP API로 기존 에이전트 연결, Docker Compose 배포 | **개인용 에이전트 채팅 공간에 가까움.** ARM64 지원 보장은 확인하지 못함. Tailscale 환경에서는 유료 터널의 필요성이 낮아짐 |
| **[AgentDM](https://agentdm.ai/)** | 호스팅 서비스, 현재 Early Access 무료 | 에이전트마다 `@alias` 부여. MCP·A2A로 DM·채널 메시지 전달, 읽음·전달 상태 확인. Slack과 양방향 연동 | **에이전트 연결 방식의 좋은 참고 사례.** 서버 자체 호스팅 옵션은 확인되지 않아 Pi 중심 운영에는 제약 |
| **[Element / Matrix](https://element.io/en/server-suite/community)** | 상용 Element 제품 + 무료 ESS Community | 에이전트를 사용자 계정으로 등록하고 방·DM으로 통신. 메시지 기록과 웹 클라이언트 제공. API 및 별도 터미널 클라이언트로 접근 | **자체 호스팅 후보로 유력.** [ARM64 구성 이미지 지원](https://matrix.org/blog/2025/11/14/this-week-in-matrix-2025-11-14/) 확인. 에이전트 연동·실행 제어는 별도로 붙여야 함 |
| **[Mattermost](https://mattermost.com/pricing/)** | 상용 자체 호스팅 제품 + OSS Team Edition | bot 계정·REST API, 채널·DM·검색·웹 UI. `mmctl`로 CLI에서 메시지 조회 가능 | 기능은 잘 맞지만 **[ARM64 서버의 운영 환경은 공식 지원하지 않음](https://support.mattermost.com/hc/en-us/articles/53131125053204-ARM64-support-for-Mattermost-Server)**. ARM64 설치본은 개발·테스트용 |
| **[Slack](https://api.slack.com/messaging/overview)** | 상용 클라우드 SaaS | bot/API, 채널·스레드·검색, 익숙한 브라우저 UI | 사용자 경험의 참고 사례. 중앙 서버·기록을 Pi에 둘 수 없음. 무료판은 [최근 90일 조회 및 1년 초과 데이터 삭제](https://slack.com/help/articles/27204752526611-Feature-limitations-on-the-free-version-of-Slack) 제한 |

특히 앞의 두 제품은 다음처럼 구분하면 좋겠습니다.

- **Agent Relay:** 메시지를 실제 에이전트 세션에 전달하고, 전달 성공·대기·실패를 추적하는 기능이 두드러집니다. 여러 머신의 기존 에이전트를 연결하는 설계 참고에 적합합니다.
- **Agent United:** 사람이 브라우저에서 여러 에이전트와 대화하고 기록을 보는 경험이 중심입니다. 직접 만들 플랫폼의 화면과 사용 흐름을 참고하기 좋습니다.
- **Element/Matrix:** 방·사용자·메시지 기록이라는 기반이 이미 갖춰져 있습니다. 이를 활용하면 직접 구현해야 할 메시징 기능을 줄일 수 있지만, 에이전트를 깨우고 작업을 실행하는 기능은 연동해야 합니다.

상용 에이전트 운영 플랫폼인 **LangSmith Deployment와 CrewAI AMP**도 조사했습니다. 원격 에이전트 연동 기능이 있지만, 에이전트 코드·워크플로를 배포하고 실행·관찰하는 범위까지 포함합니다. LangSmith 자체 설치에는 PostgreSQL·Redis·라이선스 등이 필요하고, CrewAI는 SaaS인 AMP와 자체 호스팅 제품인 Factory를 구분합니다. 현재 말씀하신 규모에서는 후순위 비교 대상으로 보겠습니다. [LangSmith 설치 문서](https://docs.langchain.com/langsmith/deploy-standalone-server), [CrewAI 배포 옵션](https://docs.crewai.com/en/installation).

상용 제품 외에 **OpenClaw**도 참고 가치가 큽니다. 공식적으로 [Pi 5 설치](https://docs.openclaw.ai/install/raspberry-pi), [Tailscale 접근](https://docs.openclaw.ai/gateway/tailscale), 브라우저·터미널 UI와 에이전트 간 통신을 안내합니다. 다만 자체 에이전트 실행 환경까지 포함하므로, 기존 외부 에이전트를 연결하려면 연동 범위를 확인해야 합니다.

현재 구상에서는 **Agent Relay의 전달·수신함 구조와 Agent United의 대화 UI를 먼저 살펴보고, Element/Matrix를 재사용할지 비교하는 순서**를 권합니다. 제품을 평가할 때는 **꺼진 에이전트의 메시지 보존, 재접속 후 수신, 자동 실행 여부, 장기 기록 검색**을 핵심 기준으로 두면 됩니다. Tailscale은 Pi의 웹·API를 사설 네트워크 안에 노출하는 역할을 맡길 수 있습니다. [Tailscale Serve 문서](https://tailscale.com/docs/features/tailscale-serve?q=serve).

## 사용자의 프로젝트 등록 요청

> 이거 아예 프로젝트로 올려야겠다. 이 대화 내용을 ChatGPT 앱의 프로젝트로 만들어서 올려줄 수도 있어? 작업 디렉토리는 'C:\Users\user\Desktop\Jihwan\project\toy-thing\agents-platform' 여기로 하고 싶은데.

## 미정 사항

다음 항목은 구현 전에 정해야 하며 아직 결정된 요구사항은 아니다.

- 연결할 에이전트 종류와 HTTP, MCP, A2A, CLI 등의 연동 방식
- 메시지 중계·보관 외에 플랫폼이 에이전트 실행까지 맡을지 여부
- 메시지 보관 기간, 전달 확인, 재시도 및 중복 처리 정책
- 에이전트 상태의 정의와 브라우저·CLI의 최소 기능
- 정확한 schema·migration·storage backup 관리와 복구 절차

## 사용자가 제공한 작업 규칙

이 절은 2026-10-14 Codex 세션 당시 사용자가 제공한 규칙의 기록이다. 현재 오케스트레이션 규칙의 정본은 사용자 전역 지침이다. 아래 Git Bash 항목은 Windows 환경을 전제로 하므로 2026-10-15 macOS 이전 뒤에는 적용하지 않는다.

GitHub에 접근할 때는 Git Bash를 사용한다. 사용자는 해당 환경에 접근 키가 있다고 안내했다.

코드 변경이 필요한 작업에서는 Sol(Codex의 경우)이 오케스트레이터를 맡는다. Sol은 요구사항을 분석하고 작업 범위와 검증 기준을 정하며 구현·탐색·커밋을 직접 주도하지 않는다.

재귀적인 파일·코드베이스 탐색과 조사(관련 파일, 기존 변경, 참조 관계, 영향 범위 확인 포함), 실제 코드 작성·수정, `git status`·`git diff` 검토와 커밋 작업은 우선 **GPT-5.6 Luna(Max 추론) 서브 에이전트**에게 맡긴다.

상위 모델은 하위 모델의 결과, 실제 변경 범위, 코드 품질, 테스트 결과, 커밋 구성을 독립적으로 검수한다. 누락·과잉·오류·범위 불일치가 있으면 문제와 완료 조건을 구체적으로 정리해 수정을 지시하고, 재작업 후 다시 검수한다.

검증 기준을 충족하면 결과를 사용자에게 보고하고, 사용자 요청과 권한 범위 안에서 커밋 정리와 푸시 흐름을 감독한다.

실행 환경에서 정해진 하위 모델을 사용할 수 없으면 다른 모델로 임의 대체하지 않는다. 사용 가능한 선택지와 제약을 사용자에게 알리고 지시를 받는다.

**Astra 모델은 모든 경우에서 예외이다.** 필요한 경우 자율적으로 서브 에이전트를 호출하며, 적절한 토큰 사용량 억제 방법을 제안하고 사용할 수 있다.
