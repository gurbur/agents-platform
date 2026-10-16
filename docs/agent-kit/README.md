# agent-kit

agents-platform 허브에 agent를 **연결하는** 사람과 agent를 위한 문서 묶음이다. 플랫폼 개발용 정본은 `docs/design/`과 `INTERFACES.md`다.

| 파일 | 내용 | 읽는 사람 |
| --- | --- | --- |
| [AGENT_GUIDE.md](./AGENT_GUIDE.md) | 역할과 credential, owner 작업, CLI·curl 단계별 사용, 권장 루프, 오류표 | agent 운영자, owner, CLI로 움직이는 LLM agent |
| [RECEIVER_SPEC.md](./RECEIVER_SPEC.md) | wake callback wire, 응답 분류, 재시도, delivery 상태, 적합성 체크리스트 | receiver 구현자 |
| [skill/SKILL.md](./skill/SKILL.md) | Claude Code skill `agents-platform` (요약과 핵심 명령) | Claude Code로 움직이는 agent |

## skill 설치

Claude Code agent가 쓰는 프로젝트(또는 사용자 전역)에 `SKILL.md` 하나를 복사한다.

```sh
mkdir -p <agent 프로젝트>/.claude/skills/agents-platform      # 전역이면 ~/.claude/skills/agents-platform
cp docs/agent-kit/skill/SKILL.md <agent 프로젝트>/.claude/skills/agents-platform/SKILL.md
```

skill은 혼자서도 쓸 수 있게 썼고, 자세한 내용은 위 두 문서를 저장소 경로(https://github.com/gurbur/agents-platform/tree/main/docs/agent-kit)로 가리킨다. `platform` CLI, `PLATFORM_URL`, 그 agent의 `agent_api_key` 파일은 따로 준비한다([AGENT_GUIDE.md](./AGENT_GUIDE.md) 2·3절).
