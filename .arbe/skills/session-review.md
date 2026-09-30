---
name: session-review
description: Review a saved ArBeHarness session from its on-disk trace and report whether it resolved the request and whether the path was wasteful. Use when asked to review a session, judge a session id, or look for harness waste in a saved trace. Counts from the files; does not recommend a harness change from one session.
tags: sessions, review
---

Review one saved session and the subagent sessions it started. Read only. Do not edit, move, or delete anything under the harness home, and do not write the report back into the session.

The on-disk shape is in `docs/user-guide.md` under File formats (`meta.json`, `turns.jsonl`, `in_flight.jsonl`). Sessions live in `<harness home>/sessions/<id>/`. The home is `~/.arbe` unless that session was recorded with `ARBE_HOME` or `--dev-home`.

`read_file` cannot leave the workdir, and the harness home is outside it. Read the files with `execute` and a read-only command (`Get-Content` on Windows, `cat` elsewhere). Skip image and `opaque` payloads: count that a block was there, and do not pull base64 into the report.

## What to load

1. The session's `meta.json` and `turns.jsonl`.
2. If `in_flight.jsonl` exists and is non-empty, the session closed mid-turn. Say so. Those lines are not a finished turn.
3. Every other session whose `meta.parent` is this id. Review each the same way, nested under its parent. Ignore sessions that are not this one and not its descendants. If a parent chain cycles, stop and say so.

## Facts per turn

Walk `messages` in order.

- The request is the text of the user message.
- Each assistant message that contains a `tool_use` is one tool round. Pair a `tool_result` only with a `tool_use` in the assistant message immediately before it. Some providers reuse call ids (`call_0`) every round, so a result never answers a call from an earlier round.
- A call with no paired result was not run.
- The answer is the text of the last assistant message.
- Record `stop_reason.kind`.

When you quote a message, flatten it to one line and stop at 500 characters.

## Outcome

From the last turn, plus whether an in-flight log remains. Use one of these and no other:

| Outcome | When |
|---|---|
| nothing ran | No turns. |
| answered | Last stop is `end_turn` and some turn has non-empty answer text. |
| ended without an answer | Last stop is `end_turn` and no turn has answer text. |
| stopped by a limit | Last stop is `tool_round_limit`, `turn_token_limit`, `repeated_tool_call`, or `max_tokens`. |
| cancelled | Last stop is `cancelled`. |
| interrupted | An in-flight log remains, or the last stop is `interrupted` and the turn did not end cleanly. |
| ended incomplete | Anything else. |

State the last stop reason. Whether the answer is *correct* is not in the trace; say that it would take the session's workdir.

## Process

`nothing to judge` when there are no turns. `waste visible` when any of these is true, otherwise `no waste visible`:

- a tool result with `is_error`
- the same tool name and arguments more than once in one turn
- more than one tool result whose text is over 8000 characters
- any turn stopped by a limit (`tool_round_limit`, `turn_token_limit`, `repeated_tool_call`, `max_tokens`)

## Findings

One finding per signal below. Each finding states what happened, where that behaviour lives, and when acting on it would be the wrong fix. One session is an observation. Do not recommend a harness change unless the same finding has already shown up in other sessions you were shown.

| What | Where it lives | Wrong when |
|---|---|---|
| A turn hit `tool_round_limit` (name the turn and the round count) | the loop's `max_tool_rounds` guard (`agent/turn.rs`) | the task genuinely needs many steps, such as a large refactor or a long investigation |
| A turn hit `turn_token_limit` | the loop's `max_turn_tokens` guard (`agent/turn.rs`) | the context grew because the task asked for it, such as reading several large files |
| A turn was stopped for `repeated_tool_call` | the repeated-call guard (`agent/turn.rs`) | the tool is correctly used to poll, such as a background process |
| A turn was cut off at `max_tokens` | `max_tokens` on the provider request (`agent/turn.rs`) | the request asked for a long answer |
| A turn's stop is `interrupted` | in-flight recovery (`agent/mod.rs`) | nothing to change: the turn was persisted so it could be resumed |
| One turn called the same tool with the same arguments N times (one finding per tool and arguments; N includes the first call) | the model's tool use, shaped by the system prompt and the tool's description | the result changed between calls, such as a file edited between reads or a process polled |
| N tool calls came back as errors, summed across the session | the tool's description and argument schema (`arbe-tools`) | the error is the project under work, such as a failing test or a build that does not compile |
| N tool results were over 8000 characters, and N is more than 1 | tool-result handling (`arbe-memory` pruning, and the tool's own output cap) | the task asked to read a large file in full |

## Report

Markdown, in this order:

1. `# Session \`<id>\`` then one line: title (or `(untitled)`), profile, provider, model, mode if `meta.mode` is set, turn count, `usage` tokens, and `cost_usd` if present.
2. `**Outcome: <outcome>.**` and the reason.
3. `**Process: <process>.**` and the reason. When there are findings, say how many and that each is a cost, not by itself a reason to change anything.
4. The findings, numbered, each with `Lives in` and `Would be wrong when`.
5. `Trace:` one `#### Turn <index>` per turn: the request as a quote, each call as `` `<name>` <not run | error, N chars | N chars> — `<arguments>` ``, then the stop reason and the answer. If there are no turns, say so. If a turn was still in progress, say it is not in the trace.
6. Each child after a `---`, introduced as `Subagent of \`<parent id>\`:`, reviewed the same way at a `###` heading.
