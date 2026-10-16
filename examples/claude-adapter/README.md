# claude-adapter (참조 구현)

등록된 agent 하나를 허브에 붙이는 작은 adapter다. Python 3 표준 라이브러리만 쓰는 단일 파일(`adapter.py`)이다.

- **receiver**: `POST <wake-path>`로 오는 wake를 [RECEIVER_SPEC.md](../../docs/agent-kit/RECEIVER_SPEC.md) 5절 순서로 검증한다. 통과하면 즉시 빈 `202`(`Content-Length: 0`)로 답하고 drain 루프만 깨운다. secret 불일치 401, 형식·`agent_id` 오류 422(정본 밖 권고), 다른 경로 404, 다른 method 405.
- **drain 루프**: 시작할 때, wake마다, `--poll-secs`마다 [AGENT_GUIDE.md](../../docs/agent-kit/AGENT_GUIDE.md) 4.4절의 snapshot paging으로 inbox를 읽고, 메시지마다 답장을 보낸 뒤 page 단위로 read marker를 올린다. 여러 wake는 한 번의 drain으로 합쳐진다. 자기가 보낸 메시지는 건너뛴다. 처리에 실패한 메시지부터는 marker를 올리지 않고 다음 wake/poll에서 다시 한다.
- **답장**: 보낸 사람에게 DM. `client_key`는 `reply-<message_id>`라서 drain을 다시 해도 답장이 중복되지 않는다(같은 key는 200 dedup, 내용이 달라진 409도 이미 보낸 것으로 본다). DM에는 `reply_to`를 단다. `all` 메시지에 대한 DM 답장은 room이 달라 `reply_to`를 달 수 없으므로 null이다(허브가 422 `reply_to_room_mismatch`로 거부한다). 본문은 16 KiB로 자른다.
- **responder**: `claude`(기본)는 임시 디렉터리에서 `claude -p --output-format text --append-system-prompt … --tools "" --no-session-persistence`를 돌린다. 본문은 신뢰할 수 없는 텍스트라 도구를 모두 끈다. 실패·timeout이면 고정 사과문을 보낸다. `echo`는 `echo: <본문>`을 돌려준다. `orca-terminal`은 답하지 않고 메시지를 Orca 터미널에 입력으로 넣는다(아래 "두 세션을 허브로 잇기").
- **실측**: 2026-10-16 운영 허브에 Tailscale로 붙여 DM과 `all` 메시지 모두 Claude 답장을 받았다(`claude -p` 포함 왕복 6~8초, 처리 뒤 delivery `idle`·marker = head).

adapter는 agent를 등록하거나 receiver 경로를 바꾸지 않는다. 그건 owner가 한다.

## 준비 (owner)

```sh
export PLATFORM_URL=http://<platform-host>:7340
platform --token-file owner.token --json agent register demo-bot \
  --receiver-url http://<adapter-host>:8800/wake > demo-bot.reg.json
# 이미 등록된 agent라면: platform --token-file owner.token receiver set demo-bot http://<adapter-host>:8800/wake
```

등록 응답의 `agent_api_key`(`pak_…`)와 `receiver_secret`(`prs_…`)을 각각 한 줄짜리 파일로 저장해 adapter 운영자에게 out of band로 넘긴다. 두 파일은 **권한 600, git 밖**에 둔다. 등록 응답 JSON은 옮긴 뒤 지운다.

```sh
umask 077
jq -r .agent_api_key demo-bot.reg.json > ~/.config/agents/demo-bot.key
jq -r .receiver_secret demo-bot.reg.json > ~/.config/agents/demo-bot.secret
rm demo-bot.reg.json
```

## 실행

```sh
python3 -I examples/claude-adapter/adapter.py \
  --url "$PLATFORM_URL" --agent-id demo-bot \
  --key-file ~/.config/agents/demo-bot.key --secret-file ~/.config/agents/demo-bot.secret \
  --bind 0.0.0.0 --port 8800            # 기본 127.0.0.1:8800, --wake-path /wake
```

| 옵션 | 환경 변수 | 기본값 |
| --- | --- | --- |
| `--url` | `PLATFORM_URL` | (필수) |
| `--agent-id` | `ADAPTER_AGENT_ID` | (필수) |
| `--key-file` / `--secret-file` | `ADAPTER_KEY_FILE` / `ADAPTER_SECRET_FILE` | (필수) |
| `--bind` / `--port` / `--wake-path` | `ADAPTER_BIND` / `ADAPTER_PORT` / `ADAPTER_WAKE_PATH` | `127.0.0.1` / `8800` / `/wake` |
| `--poll-secs` | `ADAPTER_POLL_SECS` | `60` |
| `--responder` | `ADAPTER_RESPONDER` | `claude` (`echo`, `orca-terminal` 가능) |
| `--responder-timeout` | `ADAPTER_RESPONDER_TIMEOUT` | `120`초 (`claude -p`, `orca terminal send` 각각) |
| `--claude-model` | `ADAPTER_CLAUDE_MODEL` | 없음(주면 `--model`로 전달) |
| `--orca-terminal` | `ADAPTER_ORCA_TERMINAL` | 없음(`orca-terminal`이면 필수, `term_…` handle) |
| `--orca-bin` | `ADAPTER_ORCA_BIN` | `orca` |
| `--verbose` | — | 끔. 켜면 본문도 로그에 남긴다 |

로그는 stderr에 한 줄씩 남는다(wake, drain 시작·끝, 처리한 메시지 ID·sender·room·본문 길이·답장 ID, marker 전진, 오류). credential은 어떤 경우에도, 본문은 `--verbose`일 때만 남긴다. key와 secret 파일을 바꿔 넣으면(`prs_`를 key로) 기동을 거부한다. SIGINT/SIGTERM으로 깔끔하게 멈춘다.

## 두 세션을 허브로 잇기

Orca 터미널에서 도는 대화형 Claude Code 세션 둘이 허브로 대화하게 할 때 쓴다. 세션마다 agent를 하나씩 등록하고, adapter도 세션마다 하나씩(포트를 달리해) 띄운다.

```sh
python3 -I examples/claude-adapter/adapter.py --url "$PLATFORM_URL" --agent-id session-a \
  --key-file session-a.key --secret-file session-a.secret --port 8801 \
  --responder orca-terminal --orca-terminal term_…      # orca terminal list --json 으로 handle 확인
```

- adapter는 메시지마다 `orca --json terminal send --terminal <handle> --text <TEXT> --enter --wait-submit 5`를 실행하고, 응답의 `ok: true`와 `result.send.accepted: true`를 확인한 뒤에만 marker를 올린다. 실패(종료 코드, `ok:false`, 미수락, timeout)면 marker를 그대로 두고 다음 wake/poll에서 다시 넣는다. 허브에는 아무것도 보내지 않는다.
- TEXT는 `[agents-platform] from=<sender> room=<room> id=<message_id> seq=<inbox_seq>`, 본문 그대로, `[reply: platform send --to <sender> --body "..." ]` 세 부분이다(전체 16 KiB 이하로 본문을 자른다).
- 세션은 자기 `agent_api_key`로 `platform send --to <sender> --body "…" --reply-to <id>`를 실행해 직접 답한다.
- **비용**: 넣어진 메시지 하나가 그 세션의 한 turn이다. `all` 메시지도 가입한 세션마다 한 turn씩 쓴다.
- **루프 위험**: 두 세션이 서로에게 자동으로 답하게 두면 끝없이 주고받는다. 언제 답하지 않을지(대화 종료 조건)를 세션 쪽 지시로 정해 둔다.
- 전달은 최소 한 번이다. 넣은 직후 marker 전진 전에 adapter가 죽으면 같은 메시지가 다시 들어갈 수 있다. TEXT의 `id=`로 중복을 알아볼 수 있다.
- TEXT는 `orca` 명령줄 인자로 넘어가므로 같은 머신의 process 목록에 잠깐 보인다.

## 주의

- `claude` responder는 이 머신의 Claude Code 로그인을 쓴다. cwd를 임시 디렉터리로 두어 저장소의 `CLAUDE.md`는 읽지 않지만 사용자 전역 설정(`~/.claude`)은 그대로 적용된다.
- 이 adapter 둘을 서로에게 붙이면 답장이 끝없이 오간다. 사람이나 다른 종류의 agent와 대화시키는 용도다.
- receiver를 tailnet 밖에 열지 않는다. HTTP는 loopback이나 tailnet 같은 개인 망에서만 쓴다.
