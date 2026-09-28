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
12. [Hooks](#hooks)
13. [Sessions](#sessions)
14. [Where files are stored](#where-files-are-stored)
15. [File formats](#file-formats)
16. [Logs and troubleshooting](#logs-and-troubleshooting)
17. [Not yet active](#not-yet-active)

---

## Quick start

ArBeHarness is a terminal app. Build and run it from the repository root:

```bash
cargo run --release
```

The binary is called `arbeharness` (`target/release/arbeharness`, or `arbeharness.exe` on Windows); you can run it directly instead of going through `cargo run`.

With no configuration it talks to a local [Ollama](https://ollama.com) server at `http://localhost:11434` using model `qwen2.5-coder:3b` (or `llama3.2:3b` with the [`general` profile](#profiles)). No API key is needed to start it, but Ollama must be running and have the model pulled (`ollama pull qwen2.5-coder:3b`, and `ollama pull llama3.2:3b` for the general profile) before you get a reply.

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

### Trusted projects

A project's `.arbe/config.toml` arrives with whatever repository you open, so by default it **can't** change settings that could leak your API key, weaken approvals, or start programs. In an untrusted project these keys are ignored:

- `provider.base_url`, `provider.api_key_env`, `provider.api_key_command`, `provider.headers` (where requests, and your key, are sent, and programs that fetch it)
- everything under `[approval]`
- `[mcp.servers.*]` and `[[hooks.commands]]` (programs the harness would start)
- the same keys inside `[profiles.*]`

An `[error]` line when the session starts lists what was ignored. Everything else in a project config (model, limits, `tools`, `prompt`, skills mode, …) still applies.

To trust a project, list it in your **global** config (a project can't trust itself):

```toml
# ~/.arbe/config/config.toml
trusted_projects = ["C:/code/my-app", "/home/me/work"]   # a folder and everything inside it
```

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
allow = ["read_file", "list_dir", "glob", "grep", "todo_write", "execute(cargo test*)"]
deny = ["execute(git push*)", "write_file(.git/*)"]
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
| `trusted_projects` | `[]` | Folders whose project config may change security-sensitive settings. Global config only. See [Trusted projects](#trusted-projects). |
| `tools` | all tools | Tools the model may use, by name. An entry ending in `*` matches by prefix, e.g. `"github__*"` for every tool of the `github` [MCP server](#mcp-servers). Tools not listed are not available at all (not even through `/tool`). |
| `prompt` | per profile | System prompt template: `coding`, `general`, or a path to your own Markdown file (relative paths are relative to the config file). See [Profiles](#profiles). |
| `provider.name` | `ollama` | Same as `ARBE_PROVIDER`. |
| `provider.model` | per provider | Same as `ARBE_MODEL`. |
| `provider.base_url` | provider's endpoint | Same as `ARBE_BASE_URL`. |
| `provider.api_key_command` | none | A command that prints the API key, run once at startup — for keys kept in a password manager, e.g. `"op read op://Private/OpenAI/key"` (1Password), `"security find-generic-password -s openai -w"` (macOS Keychain), `"pass show openai"`. Takes precedence over the environment. If it fails, the app stops with its error output. |
| `provider.api_key_env` | the provider's usual variable | The **name** of the environment variable that holds the API key, e.g. `"WORK_OPENAI_KEY"`. Keys themselves never go in a config file: an `api_key` key is rejected. |
| `provider.headers` | none | Extra HTTP headers, as a table. Same as `ARBE_HTTP_HEADERS`. |
| `generation.temperature` | `1.0` openai, else `0.2` | Same as `ARBE_TEMPERATURE`. |
| `generation.max_tokens` | `4096` | Maximum length of one model response, in tokens. |
| `generation.thinking_budget_tokens` | off | Same as `ARBE_THINKING_BUDGET`. |
| `context.budget_tokens` | window − `max_tokens` | Same as `ARBE_CONTEXT_BUDGET`. |
| `context.memory_strategy` | `truncation` | What happens when history grows too large. `truncation` leaves older turns out; `compact_summary` has the model summarize them first (see [Long sessions](#sessions)). |
| `loop.max_tool_rounds` | `50` | Same as `ARBE_MAX_TOOL_ROUNDS`. |
| `loop.max_turn_tokens` | off | Same as `ARBE_MAX_TURN_TOKENS`. |
| `loop.max_tool_output_chars` | `50000` | Same as `ARBE_MAX_TOOL_OUTPUT_CHARS`. |
| `loop.max_retries` | `4` | Same as `ARBE_MAX_RETRIES`. |
| `approval.mode` | `always_prompt` | See [Approvals](#approvals). |
| `approval.allow` | `[]` | [Rules](#permission-rules) for calls that run without asking in `allowlist_auto` mode. |
| `approval.deny` | `[]` | [Rules](#permission-rules) for calls that are always refused, in every mode. |
| `approval.session_approval_covers_high_risk` | `false` | Whether pressing `a` on a high-risk call approves the whole tool rather than just that exact call. |
| `skills.mode` | `on_demand` | `on_demand` or `always`. See [Skills](#skills). |
| `hooks.timeout_ms` | `500` | Default time limit for hooks that don't set their own. |
| `[[hooks.commands]]` | none | Shell commands run at points in a turn. See [Hooks](#hooks). |
| `[mcp.servers.<name>]` | none | An [MCP server](#mcp-servers) to connect. |
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
| `ARBE_MODEL` | ollama: `qwen2.5-coder:3b`, or `llama3.2:3b` for profiles using the general prompt; openai: `gpt-5-mini`; anthropic: `claude-sonnet-5` | The model ID sent to the provider. |
| `ARBE_BASE_URL` | the provider's official endpoint | Override the API endpoint, e.g. a proxy, gateway, or remote Ollama. **Required** for `openai_compatible`. |
| `OPENAI_API_KEY` | none | API key for `openai`. |
| `ANTHROPIC_API_KEY` | none | API key for `anthropic`. |
| `ARBE_API_KEY` | none | Fallback key, used when the provider-specific variable is not set. Convenient for `openai_compatible` gateways. |
| `ARBE_HTTP_HEADERS` | none | Extra HTTP headers for every provider request, written as `Name: value; Other-Name: value`. Malformed entries are skipped. |

API keys are only ever read from the environment or from `provider.api_key_command`. They are never written to disk by the harness.

**Secrets in tool output are hidden.** If a tool prints a secret — say the model runs `env`, or reads a `.env` file — every occurrence is replaced with `[REDACTED]` before the model sees it, before it's saved in `turns.jsonl`, and before it's shown on screen. What counts as a secret:

- the provider API key, and MCP bearer tokens;
- values of configured headers whose name contains `auth`, `key`, `token`, `secret` or `cookie`;
- values (8+ characters) of environment variables whose name ends in `KEY`, `TOKEN`, `SECRET`, `PASSWORD`, `PASSWD` or `CREDENTIALS`, e.g. `GITHUB_TOKEN`, `AWS_SECRET_ACCESS_KEY`.

This is a safety net, not a guarantee: a secret the harness doesn't know about, or one that's been transformed (e.g. base64-encoded), isn't recognized.

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
| `ollama` | no | `http://localhost:11434` | Runs with an 8,192-token context window. If the model doesn't support tool calling, the harness detects that and retries without tools. The agent can then only chat and can't touch files. Some models (such as `qwen2.5-coder`) write a tool call as plain JSON text instead of using Ollama's tool-call format; when a reply consists only of such calls to offered tools, the harness treats them as real tool calls. A reply that could be one is shown once it's complete rather than word by word. |
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
| `a` | Approve for the rest of the session: this tool, or for a high-risk tool this exact call (see [approvals](#approvals)) |
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
| `/compact` | Have the model summarize everything but your latest exchange now, to free up context. Works with either memory strategy. |
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
| `remember` | `note`, optional `scope` (`project` default, or `global`) | medium | Saves a one-line note to [memory](#memory), shown at the start of future sessions. |
| `load_skill` | `name` | low | Reads a skill's full instructions (only present when [skills](#skills) load on demand). |
| `todo_write` | `todos`: list of `{content, status}` with status `pending` / `in_progress` / `completed` | low | Keeps the agent's task list. Each call replaces the whole list. At most one item can be `in_progress`, and the list holds at most 200 items. Kept in memory only. |
| `write_file` | `path`, `content` | medium | Creates or overwrites a file, creating parent directories. The write is atomic. |
| `edit_file` | `path`, `find`, `replace`, optional `replace_all` | medium | Replaces text in a file. Fails if `find` isn't found, or matches more than once without `replace_all: true`. |
| `execute` | `command`, optional `timeout_secs` (default 30, max 300) | **high** | Runs a shell command (`cmd /C` on Windows, `sh -c` elsewhere) in the workdir. On timeout, the whole process tree is killed. |

**Sandboxing.** Every file tool resolves its path inside the workdir and refuses anything outside it, including through `..`. `glob` and `grep` skip `.git`, `target`, `node_modules`, and `.venv`. `execute` is **not** sandboxed: a shell command can do anything your user account can. That is why it is marked high-risk and always asks you first.

If a tool fails (bad arguments, file not found, unknown tool) or you deny it, that result goes back to the model so it can adjust or explain. The turn itself does not fail.

**Several calls at once.** The model can ask for several tools in one step. You're asked about them one at a time, in the order the model listed them. Once all are decided, the approved read-only tools (`read_file`, `list_dir`, `glob`, `grep`) run at the same time; `write_file`, `edit_file`, `execute`, and `todo_write` always run on their own, in order. Results go back to the model in its original order.

**Long output.** A tool result longer than `ARBE_MAX_TOOL_OUTPUT_CHARS` (default 50,000 characters) is shortened before the model sees it, keeping the beginning and the end. The transcript shows at most the first 500 characters of any result.

**Stuck loops.** If the model asks for exactly the same tool calls three rounds in a row, the turn stops (the third round doesn't run). A turn also stops after `ARBE_MAX_TOOL_ROUNDS` rounds, or when `ARBE_MAX_TURN_TOKENS` is set and reached.

### MCP servers

[MCP](https://modelcontextprotocol.io) servers add tools from outside the harness: issue trackers, databases, documentation search, and so on. Add them to a [config file](#configuration-file), one table per server. The name (letters, digits, `-`, `_`) is yours to choose:

```toml
# The harness starts the server as a process (stdio).
[mcp.servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_API_URL = "https://api.github.com" }   # optional extra variables
# cwd = "..."                                          # optional working directory

# A server that's already running, reached over HTTP ("streamable HTTP").
[mcp.servers.docs]
url = "https://example.com/mcp"
bearer_token_env = "DOCS_MCP_TOKEN"   # NAME of the env var holding the token
headers = { "X-Team" = "platform" }

# Optional for either kind:
# timeout_secs = 60                   # per request
# enabled = false                     # switch off a server defined in another file
```

- Servers defined in a project's `.arbe/config.toml` only start if the project is [trusted](#trusted-projects); servers in your global config always start.
- A stdio server inherits your environment, so secrets such as `GITHUB_PERSONAL_ACCESS_TOKEN` can stay in your shell instead of the config file. On Windows, commands like `npx` and `uvx` work as-is.
- Servers are connected when a session starts, in the background. The chat is usable immediately, and an `[info]` line reports `MCP server github connected (N tools)`, or an `[error]` line says why a server is unavailable. A failed server doesn't stop the others or the session.
- Its tools appear to the model as `<server>__<tool>`, e.g. `github__search_issues`, with the descriptions and argument schemas the server provides. You can call them with [`/tool`](#chat-commands) too.
- They go through the same [approvals](#approvals) as builtin tools. Their risk level comes from the server's own hints: read-only tools are low risk, destructive ones high, everything else medium. Only read-only tools run in parallel with other calls.
- If a server announces that its tool list changed, the new list is picked up at the start of your next message.
- If a stdio server crashes, it's restarted automatically the next time one of its tools is called. Cancelling a turn (`Esc`) also cancels the MCP call in progress.
- Each stdio server's error output is written to `~/.arbe/logs/mcp/<name>.log` — the first place to look when a server won't start.
- A tool's result is shown to the model as text. Images a tool returns appear as `[image: <type>]`.
- **HTTP limitation:** the harness only hears from an HTTP server while one of its own requests is open, so an HTTP server's "tool list changed" announcements made at other times are missed until the next session.

### Approvals

How tool calls are approved is set by `approval.mode` in the [configuration file](#configuration-file):

| Mode | Behavior |
|---|---|
| `always_prompt` (default) | Every call asks you, including the read-only tools. |
| `allowlist_auto` | Calls matching an `approval.allow` rule run without asking; everything else asks. |
| `denylist_block` | Everything runs without asking, except calls matching an `approval.deny` rule. |
| `dry_run_only` | Nothing runs. Every call is refused, and the model is told so. |

`approval.deny` rules apply in **every** mode: a denied call is refused without asking, even if something else would allow it.

#### Permission rules

A rule is a tool name, optionally with a pattern for what the call acts on:

| Rule | Matches |
|---|---|
| `read_file` | every `read_file` call |
| `github__*` | every tool of the `github` MCP server |
| `execute(cargo test*)` | `execute` calls whose command starts with `cargo test` |
| `write_file(src/*)` | writes anywhere under `src/` |
| `edit_file(*.md)` | edits to any Markdown file |

`*` matches any run of characters, including `/`. What the pattern is matched against depends on the tool: the `path` argument for `read_file`, `write_file`, `edit_file`, `list_dir`, `glob` and `grep` (with forward slashes, relative to the workdir as the model wrote it, `.` when omitted), and the command line for `execute`. MCP tools and `todo_write` can only be matched by name. A malformed rule (e.g. a missing `)`) stops the app at startup.

**High-risk tools** (`execute`, and MCP tools their server marks destructive) never run without asking just because of the mode or a bare tool name. Only a rule with a pattern can let them through — `execute(cargo test*)` in `allow` does, `execute` alone doesn't. Only `dry_run_only` refuses them outright.

In the approval dialog you can:

- `y` / `n`: decide for this call only.
- `a`: approve for the rest of the session. For most tools this approves the tool, so later calls to it run without asking. For a **high-risk** tool it approves only **this exact call** — the same command line for `execute` — so `cargo test` approved once runs again without asking, but `cargo test && rm -rf target` still asks. (A high-risk MCP tool with no command line keeps asking every time, unless `approval.session_approval_covers_high_risk = true`.)
- `d`: deny this tool for the rest of the session, so later calls are refused without asking.

Session approvals and denials are forgotten when you start a new session, resume one, or quit.

---

## Instructions and skills

The model gets a system prompt made from a built-in template plus up to two instruction files you write. Both are plain Markdown and both are optional:

| File | Scope |
|---|---|
| `~/.arbe/instructions/agent.md` | Global: applies to every project. |
| `<workdir>/agent.md`, or `<workdir>/CLAUDE.md` if there's no `agent.md` | Project: applies to this workdir only. |

- Only the workdir itself is checked for these two, not parent directories.
- **Subdirectories can have their own:** an `AGENTS.md`, `agent.md` or `CLAUDE.md` in any folder inside the workdir applies to work in that folder. It's shown to the model the first time a tool reads, writes or lists something in that folder (added to that tool's result, labelled with the folder it applies to) — so in a large repository, each part's instructions only take up space once they're relevant. Each is shown once per session.
- Each file is capped at 8,000 characters. Anything longer is cut off with a `[truncated]` note.
- Both are **re-read before every message**, so edits take effect on your next message without restarting.

### Skills

A skill is a reusable block of instructions for a particular kind of work. One `.md` file per skill, directly in one of these folders (subfolders are ignored):

| Folder | Scope |
|---|---|
| `~/.arbe/skills/` | Global: every project. |
| `<workdir>/.arbe/skills/` | Project: this workdir only. A project skill replaces a global skill with the same `name`. |

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
- A file that can't be loaded (missing `name` or `description`, frontmatter not closed with a second `---`) is skipped, and an `[error]` line names it when the session starts. The other skills still load.

**How the model gets them** is set by `skills.mode` in the [configuration file](#configuration-file):

| `skills.mode` | Behavior |
|---|---|
| `on_demand` (default) | Only each skill's name and description go into the system prompt. When the model decides a skill is relevant, it reads the full instructions with the `load_skill` tool (low risk; asks for approval like any tool under `always_prompt`). Keeps the prompt small with many skills. |
| `always` | Every skill's full instructions are included in every request. |

`on_demand` needs a model that can call tools; with one that can't, skills are always included in full. `load_skill` is available whenever skills are loaded on demand, even if a profile's `tools` list doesn't mention it.

### Memory

Memory is notes that carry over between sessions — your preferences, a project's conventions, where things live. Two plain Markdown files, both included at the start of every request:

| File | Scope |
|---|---|
| `~/.arbe/memory/global/memory.md` | Every project. |
| `~/.arbe/memory/projects/<id>/memory.md` | One project. `<id>` is the workdir's folder name plus a short code derived from its full path, e.g. `my-app-3f9a12c0`. |

The model adds to them with the `remember` tool (one `- note` line per call), which asks for approval like any other write — memory shapes every future session, so it's worth a look. You can also edit the files yourself; changes apply from the next message. Each file is capped at 8,000 characters in the request.

---

---

## Hooks

A hook is a shell command the harness runs at a fixed point in every turn — to log activity, notify you, or check a tool call before it runs. Add them to a [config file](#configuration-file):

```toml
[[hooks.commands]]
phase = "before_tool_execute"
command = "python C:/tools/guard.py"   # run with cmd /C (Windows) or sh -c
timeout_ms = 5000                        # optional; default 10 s

[[hooks.commands]]
phase = "on_turn_complete"
command = "notify-send 'ArBe finished'"
```

Hooks from the global and the project config both run (global first). Project hooks only run if the project is [trusted](#trusted-projects), since they're programs. Commands run in the workdir.

**What a hook receives.** The phase's details as one JSON object on stdin, with a `phase` field added:

| `phase` | Fields |
|---|---|
| `before_context_assembly` | `turn_id` |
| `before_model_call` | `turn_id`, `round`, `message_count` |
| `after_model_call` | `turn_id`, `round`, `text_chars`, `tool_calls` |
| `before_tool_execute` | `turn_id`, `tool_name`, `arguments` |
| `after_tool_execute` | `turn_id`, `tool_name`, `is_error`, `output_chars` |
| `on_error` | `turn_id`, `error` |
| `on_turn_complete` | `turn_id` |

**What it can change.** Only `before_tool_execute` hooks affect anything; the others are notifications. A `before_tool_execute` hook can print a JSON object to stdout:

- the same object with different `arguments`: the tool call is rewritten (the approval dialog then shows the rewritten arguments);
- an object with `"veto": "reason"`: the call is refused, and the model is told `blocked by hook: reason`.

Printing nothing leaves the call as it was. A hook runs *before* the approval dialog, so it can't be used to skip approvals.

**When a hook fails** — non-zero exit, output that isn't a JSON object, or running past its time limit — it's skipped, the turn carries on, and an `[error]` line says which hook failed and why (including what it printed to stderr).

Example guard (Python) that refuses `git push`:

```python
import json, sys
call = json.load(sys.stdin)
if call["tool_name"] == "execute" and "git push" in call["arguments"].get("command", ""):
    print(json.dumps({"veto": "pushing is done by humans here"}))
```

---

## Sessions

Each launch starts a **new session**. Everything a turn produces is saved as it happens: your message, each assistant reply, every tool call and its result. If the app crashes or is killed mid-turn, the next time you resume that session the unfinished turn is recovered (marked as interrupted, with any tool calls that hadn't run yet recorded as "not executed"). At most the reply that was streaming at that moment is lost.

- **Resume**: press `Ctrl+R`, pick a session, press `Enter`. The transcript is reloaded and the conversation continues where it left off. Current environment settings apply, so you can resume a session with a different model.
- **New**: `Ctrl+N` starts a fresh session without quitting.
- **Clear view**: `Ctrl+L` only clears the screen. The session on disk is unchanged.

When you resume, the transcript shows each turn's question, how many tool calls it made, and its final answer. The model gets the full history, tool calls and results included, so it remembers what it looked at and did.

**Long sessions.** When the history no longer fits the context budget, the harness makes room in this order:

1. **Old tool output is shortened.** The oldest large tool results (a file read many turns ago, a long command output) are replaced by a one-line note, keeping every message. The same happens inside a single long turn: once its own tool results no longer fit, the older ones are shortened, while the results the model hasn't read yet are always kept whole.
2. **With `memory_strategy = "compact_summary"`: the model summarizes.** Once history passes about 80% of the budget, the oldest turns are summarized by the model (goals, decisions, facts learned, what was done, what's pending) and the summary takes their place, leaving the history at about 40%. This costs one extra model call now and then, and an `[info]` line reports it. You can also trigger it with `/compact`. If summarizing fails, step 3 is used instead.
3. **Whole turns are left out**, oldest first (never half a turn, so a tool call is never separated from its result). A turn that's too large to include in full is shortened to just your question and its final answer. This also happens in the middle of a long turn if a new tool result wouldn't otherwise fit: earlier turns make way, never the current one.

None of this deletes anything from disk: `turns.jsonl` always has everything in full.

To delete a session, remove its folder under `~/.arbe/sessions/`. There is no in-app delete.

---

## Where files are stored

Everything ArBeHarness saves goes under one directory, the **harness home**:

| OS | Default location |
|---|---|
| Linux / macOS | `~/.arbe/` (from `$HOME`) |
| Windows | `%USERPROFILE%\.arbe\`, e.g. `C:\Users\you\.arbe\` |

`--dev-home` / `ARBE_HOME` moves the whole tree. In the workdir, the harness only *reads* `agent.md`/`CLAUDE.md`, `.arbe/config.toml`, and `.arbe/skills/`. It never writes there itself. Only the tools the model calls (and you approve) change files there.

```text
~/.arbe/
├── sessions/
│   └── <session-id>/
│       ├── meta.json          session info (active)
│       ├── turns.jsonl        conversation history (active)
│       ├── in_flight.jsonl    the turn in progress (active; see below)
│       ├── compactions.jsonl  summaries of older turns (active)
│       └── events.jsonl       event log (reserved)
├── instructions/
│   └── agent.md               your global instructions (active)
├── skills/
│   └── *.md                   skill files (active)
├── memory/
│   ├── global/memory.md       notes for every project (active; see Memory)
│   └── projects/<id>/memory.md notes for one project (active)
├── mcp/                        (unused; MCP servers are configured in config.toml)
├── config/
│   └── config.toml            your global settings (active)
└── logs/
    └── mcp/<server>.log       error output of each stdio MCP server (active)
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

### `sessions/<id>/compactions.jsonl`

One line per summary made by `compact_summary` or `/compact`: `{"through_turn_index": 7, "summary": "...", "usage": {...}, "created_at": "..."}`. Only the latest line is used: it covers every turn up to and including `through_turn_index` (it builds on the previous summary). Deleting the file makes the next resume use the full history again.

### `sessions/<id>/in_flight.jsonl`

A write-ahead log for the turn in progress: one line per message, `{"turn_id": ..., "turn_index": ..., "message": {...}}`, appended as each message is produced. When the turn finishes, it's written to `turns.jsonl` and this file is deleted. If it's still there when a session is resumed, the previous run stopped mid-turn and its contents are recovered into `turns.jsonl` as an interrupted turn. Don't edit it by hand.

### `instructions/agent.md`, `<workdir>/agent.md`, `<workdir>/CLAUDE.md`

Free-form Markdown, used as-is (up to 8,000 characters). See [Instructions and skills](#instructions-and-skills).

### `skills/*.md`

Markdown with a `---` frontmatter block. See [Skills](#skills). A leading byte-order mark is tolerated.

---

## Logs and troubleshooting

**The only log files are for MCP servers:** `~/.arbe/logs/mcp/<server>.log` holds what each stdio server printed to its error output. The harness's own internal warnings, such as an unreadable instructions file or a malformed skill, are currently dropped rather than shown. Errors that stop a turn appear in the transcript as `[error] ...` lines. Startup failures are printed to the terminal after the app exits.

What you can inspect today:

- `~/.arbe/sessions/<id>/turns.jsonl` for exactly what was said, every tool call and result, and the token usage and stop reason per turn.
- `~/.arbe/sessions/<id>/meta.json` for which provider and model a session used.

Common problems:

| Symptom | Likely cause / fix |
|---|---|
| `<phase> hook <command> failed: ...` | A [hook](#hooks) exited with an error, printed something that isn't a JSON object, or timed out. The message includes its stderr. The turn continued without it. |
| `...config.toml: ignored ... (this project isn't trusted ...)` | The project's config tried to change a [security-sensitive setting](#trusted-projects). Add the project to `trusted_projects` in your global config if you trust it. |
| `invalid configuration: ...` at startup | A config file has a typo, an unknown key, or an invalid value, or an `ARBE_*` number variable isn't a number. The message names the file and line or the variable. |
| `MCP server <name> unavailable: ...` | The server couldn't be started or reached. Check the command/URL, and `~/.arbe/logs/mcp/<name>.log` for its own error output. `failed to start "..."` means the program wasn't found on your `PATH`. |
| An MCP server's tools don't appear | The server hasn't finished connecting yet (watch for the `connected` line), or a `tools` allow-list is set and doesn't include them — add `"<server>__*"`. |
| Connection error with the default setup | Ollama isn't running. Start it, or set `ARBE_PROVIDER`. |
| `... provider requires an api_key` | Set `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` (or `ARBE_API_KEY`). |
| `openai_compatible provider requires a base_url` | Set `ARBE_BASE_URL`. |
| Agent answers but never uses tools | The model doesn't support tool calling (common with small Ollama models). Pick a tool-capable model. |
| Approval prompts for tool calls that make no sense (e.g. `remember` with a plain fact) | Small models tend to call whatever tool is available. Deny it; `d` denies that tool for the rest of the session. |
| "Context length exceeded" | The model's real window is smaller than the harness assumes. Set `ARBE_CONTEXT_BUDGET` lower. |
| "… retrying in Ns (attempt N)…" status | The provider rate-limited or was overloaded, or the network hiccuped. The harness retries up to `ARBE_MAX_RETRIES` times before showing an error. |
| `[info] stopped: ...` after a turn | A loop guard ended it: too many tool rounds, the same calls repeated three times, or `ARBE_MAX_TURN_TOKENS` reached. Rephrase, or raise the limit. |
| `a turn is in progress — ...` | You pressed `Enter` while the agent was working. Your text is kept; send it once the turn ends, or press `Esc` to cancel the turn. |
| Temperature error from OpenAI | Reasoning models only accept `ARBE_TEMPERATURE=1` (the OpenAI default). |
| `skill skipped — malformed skill manifest at ...` | That file is missing `name`/`description` or its closing `---`. The other skills still work. See [Skills](#skills). |
| A skill seems ignored | With `skills.mode = "on_demand"` the model only reads a skill when it judges it relevant; make the `description` say clearly when to use it, or set `skills.mode = "always"`. |
| Garbled screen after a crash | The terminal was left in raw mode. Run `reset` (Unix) or open a new terminal window. |

---

## Not yet active

These have code in the repository but **can't be used yet**. They're listed so you don't spend time setting them up. This section shrinks as each one is connected.

| Feature | Status |
|---|---|
| Session-only skills | Skills come from the global and project folders; there's no way to add one for just the current session. |
| Harness log file | Only MCP servers get log files (`~/.arbe/logs/mcp/`); the harness's own warnings aren't written anywhere yet. |
| `events.jsonl` | Storage support exists, but the current runtime doesn't write it. |
| Showing the model's reasoning | Extended thinking is saved in `turns.jsonl`, but the chat screen only shows `thinking…` while it happens, not the text. |
| Tools returning images | Tool results are text only. |
| `--help` / `--version` | Not implemented. |
