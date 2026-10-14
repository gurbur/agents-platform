# Pi5 에이전트 메시지 허브를 위한 멀티 에이전트 시스템 문헌 조사

이 문서는 Raspberry Pi 5에서 상시 실행되는 메시지 보관·전달 허브를 중심으로, 여러 머신의 에이전트 세션이 전체방 broadcast와 안정적인 agent 간 DM을 사용할 때 필요한 설계 결정을 정리한다. 현재 구분할 역할은 Pi 허브/API·저장소, 각 머신의 상시 비LLM receiver, 필요할 때 에이전트 세션을 실행하는 runner다. Pi는 이 transport 계층과 별도로 coordinator를 호스팅할 수 있지만, coordinator를 둘지는 아직 정하지 않은 설계 선택이다.

조사 기준일은 2026-10-14이다.

문헌이 직접 다루는 것은 에이전트의 대화, 역할, 작업 분해, 스케줄링, 검증 방법이다. Tailscale을 통한 독립 머신 연결, CLI 또는 desktop runtime의 부팅, 재시작 뒤에도 남는 보관, offline 전달, 수신 확인과 중복 제거를 함께 보장하는 연구는 확인하지 않았다. 그러므로 아래의 적용 문장은 논문 결과와 프로젝트 설계 추론을 구분한다.

## 결정에 필요한 범위

- Pi 허브/API·저장소는 여러 머신의 receiver가 접근할 보관·조회·전달 경계를 제공한다. 각 머신의 상시 비LLM receiver는 해당 머신에서 메시지를 받고 식별·전달·재전달하며, runner는 필요할 때만 깨워져 에이전트 세션을 실행한다.
- 전체방 broadcast는 방의 수신자에게 같은 사건을 공개하는 전달 방식이다. 안정적인 DM은 특정 agent identity와 대화 또는 작업을 지속적으로 연결하는 전달 방식이다.
- message transport의 중앙화와 LLM coordination의 중앙화는 다르다. Pi5가 중앙 relay 서버여도 LLM 제어 흐름은 peer-to-peer, 독립, central, hybrid 중 하나일 수 있다.
- 실제 선택 대상은 저장 수명, 수신·수락·완료 상태, wake 및 scheduling, 역할·capability, 종료 조건, 모델 context에 넣을 메시지의 선별이다.
- 문헌 근거는 [논문 근거], 프로젝트에 적용할 때의 해석은 [설계 추론]으로 표시한다.

## 구조 분류

Guo 등의 분류인 layered, decentralized, centralized, shared message pool을 프로젝트의 hybrid 선택지와 함께 읽는다. 이는 LLM의 조정 구조 분류이며, 플랫폼 서버의 배치와 동일하지 않다. 여기서 hybrid는 중앙 coordinator가 작업 분해·검증을 맡으면서 agent 간 직접 협의를 결합하는 LLM coordination을 뜻한다. Pi relay가 중앙에 있다는 사실만으로 hybrid가 되지는 않는다.

| 구조 | 제어 위치와 전달 | Pi5 적용 가능성 | 확인해야 할 위험 |
| --- | --- | --- | --- |
| Layered 또는 hierarchical | 상위 역할이 하위 역할에 작업을 내리고 결과를 회수한다 | coordinator가 작업을 나누고 runner를 깨우는 SOP에 적합 | 상위 역할 병목, 하위 결과 누락, 층간 상태 불일치 |
| Decentralized peer-to-peer | agent가 서로 협의하고 다음 발화를 선택한다 | stable DM과 독립 runner, 병렬 탐색에 적합 | 종료·중복·책임 주체가 불명확해질 수 있음 |
| Centralized | central agent 또는 manager가 다음 작업과 발화를 결정한다 | 값싼 coordinator를 선택 정책으로 실험 가능 | coordinator 장애·context 병목; 논문이 항상 저가 모델을 보장하지 않음 |
| Shared message pool | 공용 게시판이나 pool에 게시하고 구독·조건으로 활성화한다 | 전체방, topic, 상태 기록을 transport와 연결하기 쉬움 | pool 보관과 LLM context는 별도 정책이 필요 |
| Hybrid | 중앙 coordinator가 작업 분해·검증을 맡고 agent 간 직접 협의를 결합한다 | Pi relay의 중앙 transport 위에서 hybrid LLM coordination을 별도 선택할 수 있음 | 중앙 relay만으로 hybrid라고 부르지 말고, coordinator의 권한·직접 협의·검증·종료를 명시해야 함 |

## 문헌 기록

### 1. 조사 체계를 제공하는 두 연구

#### Guo et al., Large Language Model Based Multi-agents: A Survey of Progress and Challenges

- 서지: Taicheng Guo, Xiuying Chen, Yaqi Wang, Ruidi Chang, Shichao Pei, Nitesh V. Chawla, Olaf Wiest, Xiangliang Zhang. IJCAI 2024 Survey Track, pp. 8048–8057. DOI 10.24963/ijcai.2024/890. arXiv 초판과 출판본을 구분해 출판본을 기준으로 기록한다.
- 주 링크: [IJCAI 논문 페이지](https://www.ijcai.org/proceedings/2024/890), [공식 PDF](https://www.ijcai.org/proceedings/2024/0890.pdf).
- 읽은 본문: 3.3 Agents Communication, 6.4 Scaling Up LLM-MA, 6.5 Evaluation and Benchmarks. 관련 절은 IJCAI 공식 PDF에서 확인했다.
- 핵심 방법 [논문 근거]: cooperative, debate, competitive 상호작용과 layered, decentralized, centralized, shared message pool 구조를 분류한다. agent 수 증가는 자원, 통신, 조정 복잡도를 함께 높이며 평가 공백이 남는다고 정리한다.
- 프로젝트 적용 [설계 추론]: Pi5 relay는 shared pool 또는 중앙 transport로 둘 수 있고, 그 위의 LLM 조정은 별도 선택한다. 전체방 보관과 모델에 실제로 주입할 context를 분리한다.
- 한계: survey의 분류는 설계 언어이지 Pi5의 저장·재전달·Tailscale·offline 성질을 검증한 결과가 아니다. 구조별 보편적 우열이나 안정적인 agent 수를 보장하지 않는다.

#### Kim et al., Towards a Science of Scaling Agent Systems

- 서지: Yubin Kim, Ken Gu, Chanwoo Park, Chunjong Park, Samuel Schmidgall, A. Ali Heydari, Yao Yan, Zhihan Zhang, Yuchen Zhuang, Yun Liu, Mark Malhotra, Paul Pu Liang, Hae Won Park, Yuzhe Yang, Xuhai Xu, Yilun Du, Shwetak Patel, Tim Althoff, Daniel McDuff, Xin Liu. arXiv preprint, v1 2025; v3 2026-04-08. 출판 venue는 이 문서에서 주장하지 않는다.
- 주 링크: [arXiv v3 HTML](https://arxiv.org/html/2512.08296v3), [arXiv v3 기록](https://arxiv.org/abs/2512.08296v3).
- 읽은 본문: 3.1 system definition, 4.2 main results, 4.3 scaling principles, 4.4 coordination efficiency and error dynamics, 5 limitations. v3 판본만 사용하며 v1의 구성·벤치마크 수치는 혼용하지 않는다.
- 핵심 방법 [논문 근거]: 동일한 compute 조건에서 single-agent, independent, centralized, decentralized, hybrid 구조를 여러 task와 비교한다. 병렬 분해가 가능한 task와 순차 계획 task에서 multi-agent의 비용·이득 방향이 다르고 architecture-task alignment가 중요하다고 보고한다.
- 프로젝트 적용 [설계 추론]: 전체방·DM·optional coordinator를 한 정책으로 고정하기보다 task 유형별로 single baseline, independent runner, central coordinator, 직접 협의를 결합한 hybrid LLM coordination을 비교한다. cheap coordinator를 항상 두는 정책은 검증할 가설이지 논문 보장이 아니다.
- 한계: 결과를 모든 task, model, runtime, 네트워크로 일반화할 수 없다. 특정 agent 수의 최적값이나 특정 capability 임계값을 프로젝트 규칙으로 옮기지 않는다.

### 2. 대화·역할·스케줄링 프레임워크

#### Wu et al., AutoGen: Enabling Next-Gen LLM Applications via Multi-Agent Conversation

- 서지: Qingyun Wu, Gagan Bansal, Chi Wang et al. arXiv v1 2023-08-16, v2 2023-10-03; COLM 2024.
- 주 링크: [arXiv](https://arxiv.org/abs/2308.08155), [COLM paper PDF](https://openreview.net/pdf?id=BAakY1hNKS).
- 읽은 본문: conversable agent abstraction, auto-reply, pair conversation, GroupChat와 GroupChatManager, termination 및 example sections. 전문 열람.
- 핵심 방법 [논문 근거]: agent는 send, receive, generate_reply를 통해 LLM·사람·도구와 대화한다. GroupChatManager가 다음 speaker를 선택하고 응답을 전체 group에 broadcast하며, pair conversation은 특정 두 agent 간 상호작용을 구성한다. auto-reply 횟수, TERMINATE, 코드 기반 종료를 둔다.
- 프로젝트 적용 [설계 추론]: broadcast와 DM의 의미를 runner API와 분리해 모델링하는 출발점이 된다. receiver가 대화를 보관하고, runner가 회신을 생성하도록 나누면 프레임워크의 발화 기능을 transport에 얹을 수 있다.
- 한계: conversation framework의 message dispatch는 durable queue, ack, reconnect, exactly-once, offline 저장 또는 Tailscale을 보장하지 않는다. GroupChatManager를 Pi5 전체 시스템의 필수 central coordinator로 해석하지 않는다.

#### Hong et al., MetaGPT: Meta Programming for A Multi-Agent Collaborative Framework

- 서지: Sirui Hong, Mingchen Zhuge, Jürgen Schmidhuber et al. arXiv v1 2023-08-01; ICLR 2024.
- 주 링크: [arXiv](https://arxiv.org/abs/2308.00352), [ICLR 2024 paper](https://proceedings.iclr.cc/paper_files/paper/2024/hash/6507b115562bb0a305f1958ccc87355a-Abstract-Conference.html).
- 읽은 본문: role profiles, SOP workflow, global message pool, publish-subscribe, dependency activation, executable feedback와 retry sections. 전문 열람.
- 핵심 방법 [논문 근거]: Product Manager, Architect, Project Manager, Engineer, QA Engineer 등의 역할과 정해진 소프트웨어 개발 SOP를 사용한다. global pool에 메시지를 게시하고 역할 profile과 선행 조건에 맞는 agent가 활성화되며 실행 피드백을 제한된 횟수로 반영한다.
- 프로젝트 적용 [설계 추론]: 역할·capability·topic 구독과 “조건 충족 시 wake”를 설계할 근거가 된다. 전체방은 공개 사건, DM은 역할 대상의 후속 작업, shared state는 별도 기록으로 둘 수 있다.
- 한계: pool은 연구 시스템 내부의 협업 상태이지 Pi5에서 재시작 후 보존되는 메시지 broker가 아니다. 소프트웨어 개발 SOP를 일반 작업과 독립 머신의 runtime 보장으로 확장하지 않는다.

#### Qian et al., ChatDev: Communicative Agents for Software Development

- 서지: Chen Qian, Wei Liu et al. arXiv v1 2023-07-16; ACL 2024. ACL 출판본의 대표 저자 표기를 따른다.
- 주 링크: [arXiv](https://arxiv.org/abs/2307.07924), [ACL 2024](https://aclanthology.org/2024.acl-long.810/).
- 읽은 본문: chat chain, role-playing, communicative dehallucination, memory, termination와 software development evaluation sections. 전문 열람.
- 핵심 방법 [논문 근거]: 설계·코딩·테스트의 waterfall chat chain에서 각 subtask를 instructor와 assistant의 대화로 처리한다. 역할을 교대해 설명과 검토를 유도하고, 짧은 memory와 이전 산출물을 전달한다. 반복이 정체되거나 통신 라운드 제한에 도달하면 종료한다.
- 프로젝트 적용 [설계 추론]: runner가 수행할 SOP, bounded retry, turn limit, 검토 역할을 명시하는 예로 쓸 수 있다. receiver의 전체방 transport 자체를 정의하는 연구로 사용하지 않는다.
- 한계: 소프트웨어 개발의 고정 chain과 평가에 맞춘 구조다. 일반적인 topic routing, durable storage, 독립 머신 wake, offline 복구를 검증하지 않는다.

#### Fourney et al., Magentic-One: A Generalist Multi-Agent System for Solving Complex Tasks

- 서지: Adam Fourney, Gagan Bansal, Saleema Amershi et al. arXiv v1 2024-11-07; Microsoft Research Technical Report MSR-TR-2024-47, 2024.
- 주 링크: [arXiv](https://arxiv.org/abs/2411.04468), [Microsoft Research](https://www.microsoft.com/en-us/research/publication/magentic-one-a-generalist-multi-agent-system-for-solving-complex-tasks/).
- 읽은 본문: Orchestrator, outer task ledger, inner progress ledger, stall detection, replanning, termination와 agent composition sections. 전문 열람.
- 핵심 방법 [논문 근거]: central Orchestrator가 task ledger와 progress ledger로 계획·진행·다음 발화자를 관리한다. 정체가 설정한 한도를 넘으면 reflection과 replan을 수행하고, task complete·최대 시도·시간 제한으로 종료한다. WebSurfer, FileSurfer, Coder, ComputerTerminal처럼 기능별 agent와 서로 다른 모델을 조합한다.
- 프로젝트 적용 [설계 추론]: Pi5의 coordinator가 runner를 깨우고, 현재 계획·진행·다음 작업을 별도 상태로 유지하는 설계 후보가 된다. 중앙 조정은 stable DM과 전체방을 대체하지 않고 task 실행 계층에서 선택한다.
- 한계: 실험의 team composition은 실제로 고정된 부분이 있고, plan 갱신 때 context가 reset되며 cross-task long-term memory를 보장하지 않는다. model heterogeneity를 network heterogeneity나 offline 독립성을 의미하는 것으로 읽지 않는다.

#### Gao et al., AgentScope: A Flexible yet Robust Multi-Agent Platform

- 서지: Dawei Gao, Zitao Li, Bolin Ding, Jingren Zhou et al. arXiv v1 2024-02-21, v2 2024-05-20. 별도 출판 venue는 이 문서에서 주장하지 않는다.
- 주 링크: [arXiv](https://arxiv.org/abs/2402.14034).
- 읽은 본문: message abstraction, sequential·conditional·while·for pipeline, msghub broadcast, mention/speak_list, actor-based distributed mode, timeout·exit와 discussion limit examples. 전문 열람.
- 핵심 방법 [논문 근거]: 메시지에 name, content, url, UUID, timestamp를 두고, pipeline과 msghub로 순차·조건·반복·broadcast를 구성한다. actor는 필요한 메시지가 준비될 때 계산하며 URL·port를 이용한 단일·다중 머신 예제를 제공한다.
- 프로젝트 적용 [설계 추론]: 다중 머신 runner와 Pi5 relay 사이의 envelope identity, timestamp, broadcast, condition wake를 비교할 때 가장 직접적인 개념적 선례다. 다만 transport의 보관 수명과 runner의 실행 상태는 별도 계층으로 둔다.
- 한계: URL·port RPC는 Tailscale 연결, 재시작 뒤 durable queue, ack·dedup·reconnect, 항상 켜진 receiver를 보장하지 않는다. 프레임워크의 actor를 운영 인프라의 내구성으로 일반화하지 않는다.

### 3. 고전적 협력 프로토콜과 상태 모델

#### Smith, The Contract Net Protocol: High-Level Communication and Control in a Distributed Problem Solver

- 서지: Reid G. Smith. IEEE Transactions on Computers C-29(12), 1980, pp. 1104–1113. DOI 10.1109/TC.1980.1675516.
- 주 링크: [저자 PDF](https://www.reidgsmith.com/The_Contract_Net_Protocol_Dec-1980.pdf), [DOI](https://doi.org/10.1109/TC.1980.1675516).
- 읽은 본문: task announcement, bid, award, execution, report와 manager·contractor 역할 sections. 원문 전문 열람.
- 핵심 방법 [논문 근거]: manager가 작업을 broadcast하고 contractor가 bid를 내며, manager가 award한 뒤 실행과 report를 받는 동적 작업 할당이다. 역할은 작업마다 바뀔 수 있다.
- 프로젝트 적용 [설계 추론]: 전체방 announcement와 특정 runner의 안정적인 DM을 연결하는 task allocation protocol 후보가 된다. bid, acceptance, completion을 하나의 “전달됨” 상태로 합치지 않는다.
- 한계: lower-level reliable communication을 가정한다. OS wake, durable inbox, offline 저장, 재시작 복구를 정의한 문헌이 아니므로 Pi5 transport가 별도로 채워야 한다.

#### FIPA, FIPA ACL Message Structure Specification

- 서지: Foundation for Intelligent Physical Agents. SC00061G, 2002. 표준 문서이며 논문 venue로 부르지 않는다.
- 주 링크: [FIPA 공식 PDF](https://www.fipa.org/specs/fipa00061/SC00061G.pdf), [대학 mirror](https://ppgia.pucpr.br/~fabricio/ftp/Aulas/Mestrado/AS/Aula3/SC00061G_FIPA_ACL.pdf).
- 읽은 본문: message structure와 sender, receiver, performative, content, conversation-id, reply-with, in-reply-to, reply-by fields. 표준 전문 열람; 공식 서버 접근은 mirror로 보완했다.
- 핵심 방법 [논문 근거]: 전달 envelope에서 누가 누구에게 어떤 행위를 요청하는지, 어떤 대화와 응답을 상관시키는지, 다음 응답을 원하는 최종 시각을 어떻게 표시하는지 분리한다. reply-by는 다음 응답의 희망 시각이며 전체 workflow 종료 시각이나 실행 timeout이 아니다.
- 프로젝트 적용 [설계 추론]: message id, sender·recipient, intent, conversation-id, correlation, reply-by를 Pi5 envelope에 차용할 수 있다. task TTL과 runner 실행 timeout은 별도 필드와 규칙으로 둔다. 표준 전체를 그대로 구현하기보다 목적과 상관관계 필드만 선택한다.
- 한계: ACL 필드는 저장, 재전달, 실행 성공을 보장하지 않는다. performative가 실제 task completion을 뜻한다고 간주하지 않는다.

#### Corkill, Blackboard Systems

- 서지: Daniel D. Corkill. AI Expert 6(9), 1991, pp. 40–47.
- 주 링크: [저자 PDF](https://dancorkill.com/pubs/ai-expert.pdf).
- 읽은 본문: blackboard, independent knowledge sources, control component, posting·trigger·execution separation. 저자 제공 원문을 전문 열람했다.
- 핵심 방법 [논문 근거]: 공유 blackboard에 상태와 부분 해답을 게시하고 독립적인 knowledge source가 조건에 따라 반응하며, 별도 control이 선택 순서를 조정한다.
- 프로젝트 적용 [설계 추론]: 전체방 기록과 task state를 같은 transport 위에서 유지하되, 게시·trigger·실행을 분리하는 모델을 제공한다. shared board의 항목을 모두 각 모델 context에 넣을 필요는 없다.
- 한계: 고전 blackboard는 현재의 LLM, Pi5, Tailscale, message retention을 실험하지 않는다. 게시판 접근 정책과 충돌 해결은 프로젝트가 정해야 한다.

#### Finin et al., KQML as an Agent Communication Language

- 서지: Tim Finin, Richard Fritzson, Don McKay, Robin McEntire. CIKM 1994, pp. 456–463. DOI 10.1145/191246.191322.
- 주 링크: [저자 기관 PDF](https://research.cs.umbc.edu/KQML/papers/kqmlacl.pdf), [DOI](https://doi.org/10.1145/191246.191322).
- 읽은 본문: performative와 content 분리, router·facilitator, registration, broadcast와 brokering. 원문 전문 열람.
- 핵심 방법 [논문 근거]: 메시지의 의도와 내용 표현을 분리하고 facilitator가 agent 등록·발견·중개를 지원한다. 중개는 전달 경로를 제공하지만 모든 전달 성공을 의미하지 않는다.
- 프로젝트 적용 [설계 추론]: role·capability와 message intent를 envelope에 분리하고, 어떤 agent가 현재 online인지 찾는 discovery 계층을 둘 때 참고한다. 전체방과 DM을 intent 필드로 혼합하지 않는다.
- 한계: KQML의 facilitator는 Pi5의 영속 inbox, acknowledgement, exactly-once, offline 보관을 보장하는 broker가 아니다. 오래된 언어 표준을 현재 API의 완성된 사양으로 옮기지 않는다.

### 4. 상호작용과 실패를 측정한 경험 연구

#### Park et al., Generative Agents: Interactive Simulacra of Human Behavior

- 서지: Joon Sung Park, Joseph C. O’Brien, Carrie J. Cai, Meredith Ringel Morris, Percy Liang, Michael S. Bernstein. UIST 2023. DOI 10.1145/3586183.3606763.
- 주 링크: [arXiv](https://arxiv.org/abs/2304.03442), [DOI](https://doi.org/10.1145/3586183.3606763).
- 읽은 본문: memory stream, retrieval, reflection, planning, Smallville 25-agent simulation과 evaluation sections. 전문 열람.
- 핵심 방법 [논문 근거]: 관찰을 memory stream에 저장하고 중요도·최근성·관련성으로 회수하며, reflection과 계획으로 사회적 행동을 생성한다.
- 프로젝트 적용 [설계 추론]: agent별 장기 기록, 회수 규칙, reflection을 transport 보관과 구분하는 근거가 된다. DM의 대화 history를 곧바로 모두 context에 넣지 않는다.
- 한계: 사회 시뮬레이션의 행동 일관성을 평가한 연구이지 업무 정확도, CLI workflow, 메시지 내구성의 비교 연구가 아니다. memory 오류와 과잉 추론 가능성을 운영 규칙으로 해결했다고 볼 수 없다.

#### Du et al., Improving Factuality and Reasoning in Language Models through Multiagent Debate

- 서지: Yilun Du, Shuang Li, Antonio Torralba, Joshua B. Tenenbaum, Igor Mordatch. arXiv preprint, 2023. 출판 venue는 이 문서에서 주장하지 않는다.
- 주 링크: [arXiv](https://arxiv.org/abs/2305.14325).
- 읽은 본문: independent answer, debate rounds, answer revision, factuality·reasoning experiments와 analysis. 전문 열람.
- 핵심 방법 [논문 근거]: 여러 agent가 독립 답을 낸 뒤 서로의 추론을 검토하고 제한된 라운드에서 수정한다.
- 프로젝트 적용 [설계 추론]: 전체방 broadcast를 모든 task의 기본 대화로 만들기보다, 결과 검토가 필요한 task에 선택적 debate runner를 붙인다. 종료 라운드와 판정자를 별도로 기록한다.
- 한계: 수학·사실성 benchmark에서의 개선을 장기 CLI workflow, agent 간 durable DM, 운영상 합의의 정답성으로 일반화하지 않는다. 여러 모델의 합의가 정답을 보장하지 않는다.

#### Liu et al., A Dynamic LLM-Powered Agent Network for Task-Oriented Agent Collaboration

- 서지: Zijun Liu, Yanzhe Zhang, Peng Li, Yang Liu, Diyi Yang. arXiv preprint 2023; COLM 2024.
- 주 링크: [arXiv](https://arxiv.org/abs/2310.02170), [COLM 2024 PDF](https://openreview.net/pdf?id=XII0Wp1XA9).
- 읽은 본문: dynamic agent network, agent importance ranker, 참여 선택, early stopping, task-oriented experiments와 ablation. 전문 열람.
- 핵심 방법 [논문 근거]: 매 task에서 필요한 agent의 중요도를 추정하고 network 참여자를 동적으로 선택하며, 필요 이상으로 계속 대화하지 않도록 early stopping을 사용한다.
- 프로젝트 적용 [설계 추론]: 전체방 수신자 전부를 매번 LLM context에 포함하지 않고, capability·importance에 따라 runner를 깨우는 선택 계층의 근거가 된다.
- 한계: ranker의 계산과 오판 비용이 있다. 동적 참여는 Pi5의 message retention이나 오프라인 수신 복구를 제공하지 않으며, 전체방 broadcast를 없애야 한다는 증거도 아니다.

#### Li et al., Improving Multi-Agent Debate with Sparse Communication Topology

- 서지: Yunxuan Li, Yibing Du, Jiageng Zhang et al. Findings of EMNLP 2024.
- 주 링크: [ACL Anthology](https://aclanthology.org/2024.findings-emnlp.427/), [PDF](https://aclanthology.org/2024.findings-emnlp.427.pdf).
- 읽은 본문: full-connected와 sparse debate topology, 토큰 비용, 성능 비교와 ablation. 전문 열람.
- 핵심 방법 [논문 근거]: 모든 agent가 서로 연결된 토론과 일부 연결만 둔 토론을 비교해, 통신량을 줄이면서 일부 성능을 유지할 수 있음을 관찰한다.
- 프로젝트 적용 [설계 추론]: 전체방을 보관하되 모델별로 필요한 DM·검토자·요약만 context에 넣는 선택 정책을 검증한다. transport broadcast와 model-visible topology를 별도로 둔다.
- 한계: 정적 debate graph와 해당 benchmark의 결과다. Pi5에서 모든 메시지를 저장하지 말아야 한다거나, sparse topology가 모든 업무에 최적이라는 근거는 아니다.

#### Cemri et al., Why Do Multi-Agent LLM Systems Fail?

- 서지: Mert Cemri et al. NeurIPS 2025 Datasets and Benchmarks Track으로 기록된 MAST 연구, arXiv v3 2025.
- 주 링크: [arXiv v3](https://arxiv.org/abs/2503.13657v3), [HTML v3](https://arxiv.org/html/2503.13657v3).
- 읽은 본문: MAST taxonomy, system-design·inter-agent-misalignment·task-verification 세 범주, 14 failure modes, analysis와 implications. v3 전문 열람.
- 핵심 방법 [논문 근거]: 역할 혼동, 반복, context loss, 종료 무시, 정보 누락, 불완전 검증 등 multi-agent 실패를 세 범주와 세부 모드로 체계화한다.
- 프로젝트 적용 [설계 추론]: 메시지 metadata, stop rule, 수락·완료 분리, 결과 검증, 중복·context 누락 관찰을 설계 검증 항목으로 만든다.
- 한계: taxonomy가 Pi5용 protocol로 구현되었다는 뜻은 아니다. 모든 실패 모드를 저장소 필드나 자동 검출 규칙으로 바로 번역하지 않고, 실제 task trace에서 확인한다.

## 프로젝트 설계 비교

| 프로젝트 기능 | 직접 참고할 문헌 | 적용할 설계 추론 | 보류할 주장 |
| --- | --- | --- | --- |
| 전체방 broadcast | Guo 3.3, AutoGen GroupChat, AgentScope msghub, KQML facilitator | relay는 모든 구독자에게 사건을 보관·전달하고, 각 runner는 필요한 항목만 context로 선택한다 | 모든 agent가 모든 메시지를 읽거나 모든 모델에 주입해야 한다 |
| 안정적인 agent 간 DM | AutoGen pair conversation, FIPA ACL, KQML, Contract Net report | sender·recipient·conversation-id·in-reply-to를 고정하고 DM을 재개 가능한 task/session에 연결한다 | pair conversation이 재시작·offline·exactly-once를 보장한다 |
| 작업 할당 | Contract Net, MetaGPT SOP, Magentic-One ledger | announce → bid 또는 accept → start → progress → result/failure 상태를 분리한다 | broadcast만으로 누가 작업을 맡았는지 알 수 있다 |
| shared board와 상태 | Blackboard, MetaGPT global pool, AgentScope pipeline | 보관된 사건, 현재 task state, 모델 context를 서로 다른 view로 둔다 | shared pool이 영속 broker와 동일하다 |
| 중앙 coordinator | Magentic-One, Guo centralized, Kim architecture comparison | cheap coordinator 상시 정책을 single·independent·decentralized와 같은 task에서 비교한다 | 중앙 coordinator가 항상 최선이거나 논문이 저가 coordinator를 보장한다 |
| 독립·분산 runner | AutoGen 기본 대화, Kim independent/decentralized, AgentScope actor | 병렬 가능한 task에는 independent 또는 peer를, 순차 task에는 coordinator 비용을 포함해 평가한다 | model heterogeneity가 network 또는 machine independence를 보장한다 |
| 선택적 검토·debate | Du et al., sparse topology, DyLAN | 오류 위험이 높은 결과만 검토자로 보내고 라운드·비용·종료를 기록한다 | debate 합의가 정답이나 안정적인 장기 workflow를 보장한다 |

## 운영 가설과 검증 기준

1. Pi 허브/API·저장소, 각 머신의 상시 비LLM receiver, 에이전트 세션을 실행하는 runner를 분리한다. runner와 coordinator는 메시지·조건·예약에 따라 깨울 수 있다. 이는 [설계 추론]이며 문헌의 runtime 보장이 아니다.
2. envelope에는 message id, sender, recipient 또는 room, role·capability, intent 또는 performative, conversation-id, in-reply-to, created-at, reply-by, state를 후보로 둔다. 필드가 있다고 수신 성공·실행 성공을 뜻하지 않는다.
3. receipt, acceptance, start, progress, completion, failure를 구분한다. Contract Net과 FIPA에서 차용하는 개념이며, 실제 ack·재전달·중복 제거 규칙은 별도 설계다.
4. durable record와 model-visible context를 분리한다. 전체방 보관은 감사·재개를 위해 필요할 수 있지만, 각 모델에는 task와 capability에 맞는 메시지만 넣는다.
5. single-agent, independent runners, central coordinator, decentralized peer, 중앙 coordinator와 agent 간 직접 협의를 결합한 hybrid LLM coordination을 동일 task·비슷한 compute 조건에서 비교한다. 성공률과 함께 token·시간, message 수, coordinator 비용, retry, 종료 실패, 재시작 후 복구를 기록한다.
6. broadcast와 DM을 처음부터 하나의 LLM groupchat으로 합치지 않는다. relay routing, wake policy, runner session, LLM coordination을 각각 교체 가능한 경계로 검증한다.

## 읽기 우선순위

1. Guo et al. 3.3, 6.4, 6.5: 구조 분류와 평가 공백.
2. Kim et al. v3 3.1, 4.2–4.4, 5: task에 따른 scaling과 architecture 비교.
3. AgentScope의 message·msghub·pipeline·distributed actor: transport와 runner의 경계.
4. Contract Net과 FIPA ACL: announcement, assignment, correlation, completion 표현.
5. AutoGen과 Magentic-One: pair/group 대화와 중앙 계획·stall·종료.
6. MetaGPT와 ChatDev: 역할·SOP·bounded retry·turn termination.
7. Blackboard, KQML, DyLAN, sparse debate, MAST, Generative Agents, multiagent debate: 상태·선택적 context·실패 검증을 보완.

## 근거의 한계

이 문헌 집합은 multi-agent 상호작용과 조정 방식을 설명하지만, 모든 Desktop/CLI runtime을 자동 부팅하거나, 독립 머신이 offline 상태에서도 메시지를 보존·전달하거나, Tailscale 위에서 reconnect·ack·exactly-once를 제공한다고 말하지 않는다. 이런 성질은 receiver의 저장소, queue, wake, identity, retry와 운영 환경을 별도로 검증해야 한다.

문헌에서 가져오는 것은 역할·메시지 의도·작업 할당·공유 상태·조정·종료를 표현하는 개념이다. Pi5를 중앙 transport hub로 두는 것과, 중앙 coordinator의 작업 분해·검증에 agent 간 직접 협의를 결합한 hybrid LLM coordination을 선택하는 것은 별도 결정이다. cheap coordinator를 항상 둘지, 전체방을 어떤 context에 노출할지는 프로젝트의 측정 가능한 가설로 남긴다.

## 부록: 근거가 제한된 FIPA Request 기록

#### FIPA, FIPA Request Interaction Protocol Specification

- 서지: Foundation for Intelligent Physical Agents. SC00026H, 2002.
- 주 링크: [FIPA 공식 PDF](https://www.fipa.org/specs/fipa00026/SC00026H.pdf).
- 확인 범위: 공식 서버는 403으로 본문을 열지 못했고 검색 색인 excerpt만 확인했다. 따라서 본문 전문을 읽은 것처럼 인용하지 않는다.
- 사용할 수 있는 제한적 요지: request 뒤에 agree 또는 refuse가 오고, 이후 inform-done, inform-result 또는 failure로 결과를 알리는 상호작용 이름을 제공한다.
- 프로젝트 적용 [설계 추론]: receipt·acceptance·completion·failure를 분리하는 용어 후보로만 보관하고, 이 문헌을 구현 규격이나 전달 보장 근거로 사용하지 않는다. 상세 protocol 선택은 원문을 직접 확인한 뒤 다시 검토한다.
