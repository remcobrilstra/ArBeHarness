# ArBeHarness User Guide

This guide is for people **using** ArBeHarness: how to start it, how to configure it, what you can do inside a chat, and where it keeps its files. For architecture and development docs, see the other files in `docs/` and `CLAUDE.md`.

> **Keeping this current:** this document describes the harness as it actually behaves today. Any change to CLI flags, environment variables, keybindings, chat commands, tools, file locations, or file formats must update this guide in the same change. Features that exist in the code but are not yet active are listed under [Not yet active](#not-yet-active) rather than described as working.

---

## Contents

1. [Quick start](#quick-start)
2. [Command-line arguments](#command-line-arguments)
3. [Configuration file](#configuration-file)
4. [Profiles](#profiles)
5. [Environment variables](#environment-variables)
6. [Providers and models](#providers-and-models)
7. [The chat screen](#the-chat-screen)
8. [Keybindings](#keybindings)
9. [Chat commands](#chat-commands)
10. [Tools and approvals](#tools-and-approvals)
11. [Instructions and skills](#instructions-and-skills)
12. [Sessions](#sessions)
13. [Where files are stored](#where-files-are-stored)
14. [File formats](#file-formats)
15. [Logs and troubleshooting](#logs-and-troubleshooting)
16. [Not yet active](#not-yet-active)

---

## Quick start

ArBeHarness is a terminal app. Build and run it from the repository root:

```bash
cargo run --release
```

The binary is called `arbeharness` (`target/release/arbeharness`, or `arbeharness.exe` on Windows); you can run it directly instead of going through `cargo run`.

With no configuration it talks to a local [Ollama](https://ollama.com) server at `http://localhost:11434` using model `llama3.1`. No API key is needed to start it, but Ollama must be running and have the model pulled (`ollama pull llama3.1`) before you get a reply.

To use a hosted provider, set environment variables first:

```powershell
# PowerShell
$env:ARBE_PROVIDER = "anthropic"
$env:ANTHROPIC_API_KEY = "sk-ant-..."
arbeharness
```

```bash
# bash / zsh
ARBE_PROVIDER=openai OPENAI_API_KEY=sk-... arbeharness
```

The agent works on the directory you start it in. To point it somewhere else, use `--workdir`.

---

## Command-line arguments

| Argument | Equivalent env var | Description |
|---|---|---|
| `--workdir <path>` | `ARBE_WORKDIR` | The project/repository the agent works on. File tools are sandboxed to it and shell commands run in it. Defaults to the current directory. |
| `--profile <name>` | `ARBE_PROFILE` | Which [profile](#profiles) to use: `coding` (default), `general`, or one defined in a config file. |
| `--dev-home <path>` | `ARBE_HOME` | **For development and testing only.** Moves the harness's own storage (normally `~/.arbe/`) to another directory so experiments don't touch your real sessions and settings. |

Both accept `--flag value` or `--flag=value`. If you pass both a flag and its env var, the flag wins.

When running through Cargo, put the arguments after `--`:

```bash
cargo run --release -- --workdir ../my-project
```

There is no `--help`, `--version`, or other flag right now. Unknown arguments are silently ignored. Everything else is configured with a [configuration file](#configuration-file) or [environment variables](#environment-variables).

### Workdir vs. harness home

These two are easy to mix up:

- **Workdir** (`--workdir`): the code the agent reads, edits, and runs commands in. The TUI header shows it as `workdir: ...`.
- **Harness home** (`~/.arbe/`, overridable with `--dev-home`): where ArBeHarness keeps *its own* data, such as sessions, skills, and global instructions. Normal use never needs to move it.

---

## Configuration file

Settings can live in TOML files instead of (or as well as) environment variables. Both files are optional:

| File | Scope |
|---|---|
| `~/.arbe/config/config.toml` | Global: every project. |
| `<workdir>/.arbe/config.toml` | Project: only when working in this directory. |

**Precedence**, lowest to highest: built-in defaults → the built-in profile → global file → project file → the selected profile's section in the global file, then in the project file → environment variables → command-line flags. Each layer only changes what it sets.

Files are read once at startup; restart after editing. A mistake stops the app before the chat opens, with a message naming the file and line, for example:

```text
invalid configuration: C:\Users\you\.arbe\config\config.toml: TOML parse error at line 2, column 1 ... unknown field `temprature`
```

Unknown keys are errors on purpose, so a typo can't silently do nothing.

### Example

```toml
profile = "coding"                 # default profile (see Profiles)

[provider]
name = "anthropic"                 # ollama | openai | anthropic | openai_compatible
model = "claude-sonnet-5"
# base_url = "https://..."
api_key_env = "ANTHROPIC_API_KEY"  # NAME of the env var holding the key
headers = { "X-Title" = "ArBe" }

[generation]
temperature = 0.2
max_tokens = 4096
# thinking_budget_tokens = 8000

[context]
# budget_tokens = 100000
memory_strategy = "truncation"     # truncation | compact_summary

[loop]
max_tool_rounds = 50
# max_turn_tokens = 500000
max_tool_output_chars = 50000
max_retries = 4

[approval]
mode = "allowlist_auto"            # always_prompt | allowlist_auto | denylist_block | dry_run_only
allow = ["read_file", "list_dir", "glob", "grep", "todo_write"]
deny = []
session_approval_covers_high_risk = false

[hooks]
timeout_ms = 500

[[models]]                         # correct the built-in model table
provider = "ollama"
name = "qwen3:14b"
context_window = 32768
# tool_calls = true, vision = false, thinking = false
```

### Keys

| Key | Default | Description |
|---|---|---|
| `profile` | `coding` | Profile to use. Top level only. |
| `tools` | all tools | Tools the model may use, by name. Tools not listed are not available at all (not even through `/tool`). |
| `prompt` | per profile | System prompt template: `coding`, `general`, or a path to your own Markdown file (relative paths are relative to the config file). See [Profiles](#profiles). |
| `provider.name` | `ollama` | Same as `ARBE_PROVIDER`. |
| `provider.model` | per provider | Same as `ARBE_MODEL`. |
| `provider.base_url` | provider's endpoint | Same as `ARBE_BASE_URL`. |
| `provider.api_key_env` | the provider's usual variable | The **name** of the environment variable that holds the API key, e.g. `"WORK_OPENAI_KEY"`. Keys themselves never go in a config file: an `api_key` key is rejected. |
| `provider.headers` | none | Extra HTTP headers, as a table. Same as `ARBE_HTTP_HEADERS`. |
| `generation.temperature` | `1.0` openai, else `0.2` | Same as `ARBE_TEMPERATURE`. |
| `generation.max_tokens` | `4096` | Maximum length of one model response, in tokens. |
| `generation.thinking_budget_tokens` | off | Same as `ARBE_THINKING_BUDGET`. |
| `context.budget_tokens` | window − `max_tokens` | Same as `ARBE_CONTEXT_BUDGET`. |
| `context.memory_strategy` | `truncation` | What happens to older history that no longer fits. `truncation` leaves it out silently; `compact_summary` leaves it out and adds a one-line note saying how many messages (and roughly how many tokens) were left out. Neither summarizes the content yet. |
| `loop.max_tool_rounds` | `50` | Same as `ARBE_MAX_TOOL_ROUNDS`. |
| `loop.max_turn_tokens` | off | Same as `ARBE_MAX_TURN_TOKENS`. |
| `loop.max_tool_output_chars` | `50000` | Same as `ARBE_MAX_TOOL_OUTPUT_CHARS`. |
| `loop.max_retries` | `4` | Same as `ARBE_MAX_RETRIES`. |
| `approval.mode` | `always_prompt` | See [Approvals](#approvals). |
| `approval.allow` | `[]` | Tools that run without asking in `allowlist_auto` mode. |
| `approval.deny` | `[]` | Tools that are always refused in `denylist_block` mode. |
| `approval.session_approval_covers_high_risk` | `false` | Whether pressing `a` also covers high-risk tools (`execute`). |
| `hooks.timeout_ms` | `500` | Time limit for each hook. (You can't register hooks yet; see [Not yet active](#not-yet-active).) |
| `[[models]]` | none | Corrects the [context-window table](#providers-and-models) for one model: `provider`, exact `name`, `context_window`, and optionally `tool_calls`, `vision`, `thinking`. For Ollama this also sets the context size the server is asked to allocate. |

---

## Profiles

A profile is a named set of settings: which tools the agent has, which system prompt it uses, and anything else from the config file. Pick one with `--profile`, `ARBE_PROFILE`, or `profile = "..."` in a config file. The header shows the active profile.

Two profiles are built in:

| Profile | Tools | System prompt |
|---|---|---|
| `coding` (default) | all builtin tools | A software-engineering agent working in the workdir. |
| `general` | only `todo_write` (no file access, no shell) | A general-purpose assistant. |

Define your own in a config file as `[profiles.<name>]`, using any of the keys above except `profile` and `[[models]]`. A profile's settings override the file's top-level settings. You can also redefine `coding` or `general` this way.

```toml
[profiles.review]                  # a read-only code reviewer
tools = ["read_file", "list_dir", "glob", "grep"]
prompt = "prompts/review.md"      # your own template

[profiles.review.approval]
mode = "allowlist_auto"
allow = ["read_file", "list_dir", "glob", "grep"]
```

**Custom prompt templates** are Markdown files. Put `{global_instructions}` and `{project_instructions}` where your [instruction files](#instructions-and-skills) should be inserted; either can be left out. The file is re-read before every message. If it can't be read, the `coding` template is used instead.

An unknown profile name stops the app at startup with a list of the known ones.

---

## Environment variables

Environment variables override the [configuration file](#configuration-file). They're read at startup; changing one requires restarting the app. An invalid number (e.g. `ARBE_MAX_TOOL_ROUNDS=lots`) stops the app with an error naming the variable.

### Provider and model

| Variable | Default | Description |
|---|---|---|
| `ARBE_PROFILE` | `coding` | Same as `--profile`. See [Profiles](#profiles). |
| `ARBE_PROVIDER` | `ollama` | One of `ollama`, `openai`, `anthropic`, `openai_compatible`. See [Providers and models](#providers-and-models). |
| `ARBE_MODEL` | per provider: `llama3.1` (ollama), `gpt-5-mini` (openai), `claude-sonnet-5` (anthropic) | The model ID sent to the provider. |
| `ARBE_BASE_URL` | the provider's official endpoint | Override the API endpoint, e.g. a proxy, gateway, or remote Ollama. **Required** for `openai_compatible`. |
| `OPENAI_API_KEY` | none | API key for `openai`. |
| `ANTHROPIC_API_KEY` | none | API key for `anthropic`. |
| `ARBE_API_KEY` | none | Fallback key, used when the provider-specific variable is not set. Convenient for `openai_compatible` gateways. |
| `ARBE_HTTP_HEADERS` | none | Extra HTTP headers for every provider request, written as `Name: value; Other-Name: value`. Malformed entries are skipped. |

API keys are only ever read from the environment. They are never written to disk by the harness.

### Generation and context

| Variable | Default | Description |
|---|---|---|
| `ARBE_TEMPERATURE` | `1.0` for `openai`, `0.2` otherwise | Sampling temperature. OpenAI's reasoning models (o-series, gpt-5) reject any value other than 1. |
| `ARBE_THINKING_BUDGET` | off | Token budget for extended thinking on models that support it (Anthropic Claude). Unset means no extended thinking. |
| `ARBE_CONTEXT_BUDGET` | model's context window minus 4096 | Maximum tokens of conversation sent per request. When history exceeds this, older turns are dropped (see [Sessions](#sessions)). |
| `ARBE_MAX_TOOL_ROUNDS` | `50` | Maximum model↔tool round trips in one turn before the turn stops. This is a runaway guard, not a cost limit. |
| `ARBE_MAX_TURN_TOKENS` | off | Stops a turn once it has used this many tokens in total (input + output, across all its rounds). Unset means no limit. |
| `ARBE_MAX_TOOL_OUTPUT_CHARS` | `50000` | Longest tool result sent back to the model. Longer output keeps its beginning and end, with a `[... N characters omitted ...]` marker in between. |
| `ARBE_MAX_RETRIES` | `4` | How many times a failed provider request is retried (rate limits, overloads, transient network errors). `0` disables retrying. Backoff starts at 1s and caps at 60s, and honors the provider's `Retry-After`. |

The maximum response length is fixed at 4096 output tokens.

### Locations

| Variable | Default | Description |
|---|---|---|
| `ARBE_WORKDIR` | current directory | Same as `--workdir`. |
| `ARBE_HOME` | `~/.arbe` | Same as `--dev-home`. Development/testing only. |

---

## Providers and models

| `ARBE_PROVIDER` | Needs key | Default endpoint | Notes |
|---|---|---|---|
| `ollama` | no | `http://localhost:11434` | Runs with an 8,192-token context window. If the model doesn't support tool calling, the harness detects that and retries without tools. The agent can then only chat and can't touch files. |
| `openai` | `OPENAI_API_KEY` | `https://api.openai.com/v1` | Chat Completions API. |
| `anthropic` | `ANTHROPIC_API_KEY` | `https://api.anthropic.com` | Messages API. Supports extended thinking (`ARBE_THINKING_BUDGET`). |
| `openai_compatible` | optional | none, so `ARBE_BASE_URL` is required | Any server that speaks the OpenAI Chat Completions format (vLLM, LM Studio, LiteLLM, OpenRouter, …). Set the base URL including the `/v1` part, e.g. `http://localhost:8000/v1`. |

**Context windows.** The harness sizes the context budget from a built-in table of known models, matched by name prefix:

| Model prefix | Context window |
|---|---|
| `gpt-5*` | 400,000 |
| `gpt-4.1*` | 1,047,576 |
| `gpt-4o*` | 128,000 |
| `o1*`, `o3*`, `o4*` | 200,000 |
| `claude-*` | 200,000 |
| other OpenAI / compatible models | 128,000 |
| Ollama models | 8,192 |

If your model's real window is smaller than this, set `ARBE_CONTEXT_BUDGET` to avoid "context length exceeded" errors.

**Token counts** in the header are estimates (about 4 characters per token). After the first reply, the harness calibrates the estimate against the provider's reported usage.

---

## The chat screen

The screen has three parts:

1. **Header**: `workdir`, then `profile | provider | model | session <id> | phase | context | used`. `phase` shows what the agent is doing right now (e.g. `calling model`, `thinking…`, `running tool: grep…`, `rate limited — retrying in 4s`), or `idle`. `context` is the estimated size of the last request sent. `used` is the total tokens the provider has reported for this session.
2. **Transcript**: your messages, the assistant's replies (basic Markdown formatting), tool activity, and `[error]` / `[info]` status lines. Replies stream in as they are generated, including any text the model writes between tool calls. If you scroll up, new output doesn't pull you back down. Scroll to the bottom to follow it again.
3. **Input box**: what you are typing. Its title shows the available keys.

Pop-up dialogs appear over the transcript when a tool needs approval or when you open the session picker.

When a turn ends for a reason other than a normal answer, an `[info]` line says why, for example `stopped: too many tool rounds in one turn`, `stopped: the model kept repeating the same tool call`, `stopped: the turn's token budget ran out`, or `turn cancelled`.

---

## Keybindings

### While typing

| Key | Action |
|---|---|
| `Enter` | Send the message |
| `Shift+Enter` or `Alt+Enter` | Insert a newline (some terminals only deliver one of the two) |
| `←` / `→`, `Home` / `End` | Move the cursor |
| `Backspace` / `Delete` | Delete before / after the cursor |
| `↑` / `↓` | Scroll the transcript one line |
| `PgUp` / `PgDn` | Scroll one page |
| `Ctrl+U` / `Ctrl+D` | Scroll half a page up / down |
| `Ctrl+L` | Clear the transcript view (nothing is deleted from disk) |
| `Ctrl+N` | Start a new session |
| `Ctrl+R` | Open the session picker to resume an earlier session |
| `Esc` | While a reply is in progress: cancel the turn |
| `Ctrl+C` | Quit. A turn in progress is cancelled and saved first (if saving takes more than about 2 seconds, it's recovered the next time you resume the session) |

### Tool-approval dialog

| Key | Decision |
|---|---|
| `y` | Approve this one call |
| `n` | Deny this one call |
| `a` | Approve this tool for the rest of the session (see [approvals](#approvals)) |
| `d` | Deny this tool for the rest of the session |
| `Esc` | Cancel the whole turn (not just this call) |

If you don't answer within **30 seconds**, the call is denied automatically. The dialog shows a countdown.

### Session picker

| Key | Action |
|---|---|
| `↑` / `↓` | Select a session |
| `Enter` | Resume the selected session |
| `Esc` | Close the picker |

**Cancelling** stops the model mid-reply, tells running tools to stop (a running `execute` command is killed, including anything it started), and closes any open approval dialog. What the turn produced up to that point is kept: the partial reply and the results of tools that already finished stay in the session. Tool calls that never ran are recorded as "not executed". You can send a new message right away.

While a turn is running you can keep typing, but `Enter` doesn't send: it leaves your text in the box and shows `a turn is in progress — wait for it, or press Esc to cancel`. The same applies to `/tool` commands. The input box title shows `Esc: cancel turn` while a turn runs.

---

## Chat commands

Anything you type is sent to the model, except lines starting with a recognized command:

| Command | Description |
|---|---|
| `/tool <name> <json-args>` | Run one of the [builtin tools](#builtin-tools) yourself. It goes through exactly the same path as a call from the model: the same approval dialog, the same output limit, the same transcript lines. Handy for checking that a tool works. Invalid JSON is treated as `{}`. Not available while a turn is running. |

Examples:

```text
/tool list_dir {}
/tool read_file {"path":"Cargo.toml","start_line":1,"end_line":20}
/tool grep {"pattern":"fn main","path":"src"}
/tool execute {"command":"git status"}
```

Other text starting with `/` is sent to the model as a normal message.

---

## Tools and approvals

### Builtin tools

Every session gets these tools. When the model supports tool calling, it decides on its own when to use them.

| Tool | Arguments | Risk | What it does |
|---|---|---|---|
| `read_file` | `path`, optional `start_line`, `end_line` (1-indexed, inclusive) | low | Reads a UTF-8 text file. |
| `list_dir` | optional `path` (defaults to workdir root) | low | Lists a directory (max 1,000 entries). |
| `glob` | `pattern` (e.g. `**/*.rs`), optional `path` | low | Finds files by pattern (max 2,000 matches). |
| `grep` | `pattern` (regex), optional `path`, `case_insensitive` | low | Searches file contents (max 500 matches). |
| `todo_write` | `todos`: list of `{content, status}` with status `pending` / `in_progress` / `completed` | low | Keeps the agent's task list. Each call replaces the whole list. At most one item can be `in_progress`, and the list holds at most 200 items. Kept in memory only. |
| `write_file` | `path`, `content` | medium | Creates or overwrites a file, creating parent directories. The write is atomic. |
| `edit_file` | `path`, `find`, `replace`, optional `replace_all` | medium | Replaces text in a file. Fails if `find` isn't found, or matches more than once without `replace_all: true`. |
| `execute` | `command`, optional `timeout_secs` (default 30, max 300) | **high** | Runs a shell command (`cmd /C` on Windows, `sh -c` elsewhere) in the workdir. On timeout, the whole process tree is killed. |

**Sandboxing.** Every file tool resolves its path inside the workdir and refuses anything outside it, including through `..`. `glob` and `grep` skip `.git`, `target`, `node_modules`, and `.venv`. `execute` is **not** sandboxed: a shell command can do anything your user account can. That is why it is marked high-risk and always asks you first.

If a tool fails (bad arguments, file not found, unknown tool) or you deny it, that result goes back to the model so it can adjust or explain. The turn itself does not fail.

**Several calls at once.** The model can ask for several tools in one step. You're asked about them one at a time, in the order the model listed them. Once all are decided, the approved read-only tools (`read_file`, `list_dir`, `glob`, `grep`) run at the same time; `write_file`, `edit_file`, `execute`, and `todo_write` always run on their own, in order. Results go back to the model in its original order.

**Long output.** A tool result longer than `ARBE_MAX_TOOL_OUTPUT_CHARS` (default 50,000 characters) is shortened before the model sees it, keeping the beginning and the end. The transcript shows at most the first 500 characters of any result.

**Stuck loops.** If the model asks for exactly the same tool calls three rounds in a row, the turn stops (the third round doesn't run). A turn also stops after `ARBE_MAX_TOOL_ROUNDS` rounds, or when `ARBE_MAX_TURN_TOKENS` is set and reached.

### Approvals

How tool calls are approved is set by `approval.mode` in the [configuration file](#configuration-file):

| Mode | Behavior |
|---|---|
| `always_prompt` (default) | Every call asks you, including the read-only tools. |
| `allowlist_auto` | Tools listed in `approval.allow` run without asking; everything else asks. |
| `denylist_block` | Tools listed in `approval.deny` are refused; everything else runs without asking. |
| `dry_run_only` | Nothing runs. Every call is refused, and the model is told so. |

**High-risk tools always ask.** `execute` (and any other high-risk tool) prompts you even if a mode or list would let it run on its own. Only `dry_run_only` refuses it outright.

In the approval dialog you can:

- `y` / `n`: decide for this call only.
- `a`: approve this tool for the rest of the session, so later calls to it run without asking. **Exception:** `a` does not cover high-risk tools. `execute` asks every time, even after you press `a`.
- `d`: deny this tool for the rest of the session, so later calls are rejected without asking.

Session approvals and denials are forgotten when you start a new session, resume one, or quit.

---

## Instructions and skills

The model gets a system prompt made from a built-in template plus up to two instruction files you write. Both are plain Markdown and both are optional:

| File | Scope |
|---|---|
| `~/.arbe/instructions/agent.md` | Global: applies to every project. |
| `<workdir>/agent.md`, or `<workdir>/CLAUDE.md` if there's no `agent.md` | Project: applies to this workdir only. |

- Only the workdir itself is checked, not parent directories or subdirectories.
- Each file is capped at 8,000 characters. Anything longer is cut off with a `[truncated]` note.
- Both are **re-read before every message**, so edits take effect on your next message without restarting.

### Skills

A skill is a reusable block of instructions. Put skill files in `~/.arbe/skills/`: one `.md` file per skill, directly in that folder (subfolders are ignored). Every skill found there is added to the context of every request.

```markdown
---
name: rust-style
description: House style for Rust code
tags: rust, style
---
Always run `cargo fmt` after editing Rust files.
Prefer `thiserror` for error types.
```

- `name` and `description` are required. `tags` is an optional comma-separated list.
- Everything after the second `---` is the instruction text given to the model.
- Skills load once, when a session starts. Start a new session (`Ctrl+N`) after changing them.
- **One malformed skill file (missing `name`/`description`, unclosed frontmatter) disables all skills for that session**, and no message is shown. If your skills seem to be ignored, check every file in the folder.

---

## Sessions

Each launch starts a **new session**. Everything a turn produces is saved as it happens: your message, each assistant reply, every tool call and its result. If the app crashes or is killed mid-turn, the next time you resume that session the unfinished turn is recovered (marked as interrupted, with any tool calls that hadn't run yet recorded as "not executed"). At most the reply that was streaming at that moment is lost.

- **Resume**: press `Ctrl+R`, pick a session, press `Enter`. The transcript is reloaded and the conversation continues where it left off. Current environment settings apply, so you can resume a session with a different model.
- **New**: `Ctrl+N` starts a fresh session without quitting.
- **Clear view**: `Ctrl+L` only clears the screen. The session on disk is unchanged.

When you resume, the transcript shows each turn's question, how many tool calls it made, and its final answer. The model gets the full history, tool calls and results included, so it remembers what it looked at and did.

**Long sessions.** When the history no longer fits the context budget, older turns are left out of what's sent to the model, whole turns at a time (never half a turn, so a tool call is never separated from its result). A turn that's too large to include in full is shortened to just your question and its final answer. Nothing is deleted from disk.

To delete a session, remove its folder under `~/.arbe/sessions/`. There is no in-app delete.

---

## Where files are stored

Everything ArBeHarness saves goes under one directory, the **harness home**:

| OS | Default location |
|---|---|
| Linux / macOS | `~/.arbe/` (from `$HOME`) |
| Windows | `%USERPROFILE%\.arbe\`, e.g. `C:\Users\you\.arbe\` |

`--dev-home` / `ARBE_HOME` moves the whole tree. In the workdir, the harness only *reads* `agent.md`/`CLAUDE.md` and `.arbe/config.toml`. It never writes there itself. Only the tools the model calls (and you approve) change files there.

```text
~/.arbe/
├── sessions/
│   └── <session-id>/
│       ├── meta.json          session info (active)
│       ├── turns.jsonl        conversation history (active)
│       ├── in_flight.jsonl    the turn in progress (active; see below)
│       └── events.jsonl       event log (reserved)
├── instructions/
│   └── agent.md               your global instructions (active)
├── skills/
│   └── *.md                   skill files (active)
├── memory/
│   ├── global/memory.md       (reserved, not read yet)
│   └── projects/<id>/memory.md (reserved, not read yet)
├── mcp/                        (reserved, not read yet)
├── config/
│   └── config.toml            your global settings (active)
└── logs/                       (reserved, nothing written yet)
```

**Active** entries are read or written today. **Reserved** entries belong to features that exist in the code but aren't connected yet. Files you put there are ignored for now. See [Not yet active](#not-yet-active).

Folders are created as needed, so a fresh install may only have `sessions/`.

---

## File formats

All files are UTF-8. JSON Lines (`.jsonl`) files hold one JSON object per line and are only ever appended to.

### `sessions/<id>/meta.json`

Rewritten atomically (temp file + rename) whenever the session changes.

```json
{
  "id": "0c0c6c0e-2a2b-4c4d-8e8f-909192939495",
  "status": "active",
  "profile": "default",
  "provider": "anthropic",
  "model": "claude-sonnet-5",
  "created_at": "2026-09-28T10:00:00Z",
  "updated_at": "2026-09-28T10:05:12Z",
  "title": "optional, may be absent",
  "usage": { "input_tokens": 5120, "output_tokens": 830, "cache_read_tokens": 0, "cache_write_tokens": 0 }
}
```

`status` is one of `created`, `active`, `closed`, `failed`. `usage` is the running total for the session.

### `sessions/<id>/turns.jsonl`

One line per completed turn (schema version 2):

```json
{
  "schema_version": 2,
  "id": "<turn-uuid>",
  "session_id": "<session-uuid>",
  "index": 0,
  "messages": [
    { "role": "user", "content": [ { "type": "text", "text": "What does main.rs do?" } ], "timestamp": "2026-09-28T10:00:00Z" },
    { "role": "assistant", "content": [ { "type": "text", "text": "It starts the TUI..." } ], "timestamp": "2026-09-28T10:00:04Z" }
  ],
  "tool_calls": [],
  "tool_results": [],
  "usage": { "input_tokens": 2400, "output_tokens": 310, "cache_read_tokens": 0, "cache_write_tokens": 0 },
  "stop_reason": { "kind": "end_turn" },
  "created_at": "2026-09-28T10:00:00Z"
}
```

- `messages` holds the whole turn in order: your message, then each assistant message (text and/or `tool_use` blocks) followed by a `tool` message with the matching `tool_result` blocks, ending with the final answer.
- `role`: `user`, `assistant`, `system`, or `tool`.
- `content`: a list of blocks, each tagged by `type`: `text`, `image`, `tool_use`, `tool_result`, `thinking`, or `opaque` (provider-specific data passed back unchanged).
- `stop_reason.kind`: `end_turn`, `tool_use`, `max_tokens`, `stop_sequence`, `refusal`, `cancelled`, `interrupted`, `tool_round_limit`, `turn_token_limit`, `repeated_tool_call`, or `other` (with a `detail` string).
- Older (v1) records have no `schema_version`, and store `user_message` / `assistant_message` fields with plain-string `content`. They still load and are converted when read.
- If the app is killed mid-write, a half-written last line is ignored on load. A damaged line anywhere else makes the session fail to load.

### `sessions/<id>/in_flight.jsonl`

A write-ahead log for the turn in progress: one line per message, `{"turn_id": ..., "turn_index": ..., "message": {...}}`, appended as each message is produced. When the turn finishes, it's written to `turns.jsonl` and this file is deleted. If it's still there when a session is resumed, the previous run stopped mid-turn and its contents are recovered into `turns.jsonl` as an interrupted turn. Don't edit it by hand.

### `instructions/agent.md`, `<workdir>/agent.md`, `<workdir>/CLAUDE.md`

Free-form Markdown, used as-is (up to 8,000 characters). See [Instructions and skills](#instructions-and-skills).

### `skills/*.md`

Markdown with a `---` frontmatter block. See [Skills](#skills). A leading byte-order mark is tolerated.

---

## Logs and troubleshooting

**There are no log files yet.** The `logs/` folder is reserved but nothing writes to it. Internal warnings, such as an unreadable instructions file or a malformed skill, are currently dropped rather than shown. Errors that stop a turn appear in the transcript as `[error] ...` lines. Startup failures are printed to the terminal after the app exits.

What you can inspect today:

- `~/.arbe/sessions/<id>/turns.jsonl` for exactly what was said, every tool call and result, and the token usage and stop reason per turn.
- `~/.arbe/sessions/<id>/meta.json` for which provider and model a session used.

Common problems:

| Symptom | Likely cause / fix |
|---|---|
| `invalid configuration: ...` at startup | A config file has a typo, an unknown key, or an invalid value, or an `ARBE_*` number variable isn't a number. The message names the file and line or the variable. |
| Connection error with the default setup | Ollama isn't running. Start it, or set `ARBE_PROVIDER`. |
| `... provider requires an api_key` | Set `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` (or `ARBE_API_KEY`). |
| `openai_compatible provider requires a base_url` | Set `ARBE_BASE_URL`. |
| Agent answers but never uses tools | The model doesn't support tool calling (common with small Ollama models). Pick a tool-capable model. |
| "Context length exceeded" | The model's real window is smaller than the harness assumes. Set `ARBE_CONTEXT_BUDGET` lower. |
| "… retrying in Ns (attempt N)…" status | The provider rate-limited or was overloaded, or the network hiccuped. The harness retries up to `ARBE_MAX_RETRIES` times before showing an error. |
| `[info] stopped: ...` after a turn | A loop guard ended it: too many tool rounds, the same calls repeated three times, or `ARBE_MAX_TURN_TOKENS` reached. Rephrase, or raise the limit. |
| `a turn is in progress — ...` | You pressed `Enter` while the agent was working. Your text is kept; send it once the turn ends, or press `Esc` to cancel the turn. |
| Temperature error from OpenAI | Reasoning models only accept `ARBE_TEMPERATURE=1` (the OpenAI default). |
| Skills ignored | One skill file is malformed, which disables all of them. See [Skills](#skills). |
| Garbled screen after a crash | The terminal was left in raw mode. Run `reset` (Unix) or open a new terminal window. |

---

## Not yet active

These have code in the repository but **can't be used yet**. They're listed so you don't spend time setting them up. This section shrinks as each one is connected.

| Feature | Status |
|---|---|
| MCP servers (`~/.arbe/mcp/servers.toml`) | The client and the file parser exist, but servers are not started or offered to the model. |
| Memory notes (`~/.arbe/memory/...`) | The files can be read, but their content isn't added to the context. |
| Real summarization of old history | `compact_summary` only notes how much was left out; it doesn't summarize it. |
| Project-local / session-local skills | Only global skills (`~/.arbe/skills/`) are loaded. |
| Hooks | The hook system exists, but you can't register your own hooks. |
| Log files (`~/.arbe/logs/`) | Nothing is written. |
| `events.jsonl` | Storage support exists, but the current runtime doesn't write it. |
| Showing the model's reasoning | Extended thinking is saved in `turns.jsonl`, but the chat screen only shows `thinking…` while it happens, not the text. |
| Tools returning images | Tool results are text only. |
| `--help` / `--version` | Not implemented. |
