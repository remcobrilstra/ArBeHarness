# Coding-agent CLI harness feature inventory (as of 2026-09-29)

Scope: capabilities that shipped products treat as harness features (tools, permissions, context, sessions, extensions, headless modes), not model quality. Facts below come from pages opened on 2026-09-29. Where a docs host redirected or a page was truncated, that is called out. This is a peer inventory, not a comparison to any local codebase.

Products opened: Claude Code (`code.claude.com`), OpenAI Codex user docs (`learn.chatgpt.com/codex/sandboxing.md`; older `developers.openai.com/codex/...` URLs redirected cross-host), Cursor (`cursor.com/docs`), Gemini CLI (`geminicli.com`), OpenCode (`opencode.ai` plus the `anomalyco/opencode` tools source doc), goose (`goose-docs.ai`), Aider (`aider.chat`), Cline (`docs.cline.bot`), Amp (`ampcode.com`), Factory Droid (`docs.factory.ai`).

## What tools does each expose (files, shell, web, browser, computer use, MCP, todos, ask-user, subagents)?

### Takeaway
Every product in this set gives the model file read/search plus some way to change files and run a shell, then diverges on whether web, browser, computer use, todos, ask-user, and subagents are first-class harness tools or left to MCP/plugins. Claude Code and Factory publish the widest named tool surfaces; Aider still edits through a chat-plus-git model rather than a general tool loop.

### Cited Findings

**Claude Code** (tools reference opened; page is long and was truncated after the Bash section, so later per-tool behavior sections were not fully read):

- Built-in tools named in the reference include `Agent` (subagent; with agent teams, a call that carries a `name` can launch a teammate), `AskUserQuestion`, `Bash`, `Edit`, `Write`, `Read`, `Glob`, `Grep` (Glob and Grep “absent by default on macOS, Linux, and WSL”), `NotebookEdit`, `PowerShell`, `WebFetch`, `WebSearch`, `Skill`, `TodoWrite` (disabled by default in favor of `TaskCreate` / `TaskGet` / `TaskList` / `TaskUpdate`, which are themselves default only on listed models), `EnterPlanMode` / `ExitPlanMode`, `EnterWorktree` / `ExitWorktree`, `Monitor` (background command or WebSocket, each output line fed back), `LSP`, `CronCreate` / `CronDelete` / `CronList`, `ScheduleWakeup`, `Workflow` (script that orchestrates many background subagents), `ListAgents` / `SendMessage` (cross-session and team messaging; `ListAgents` requires v2.1.224+ and only appears when cross-session messaging is enabled), MCP helpers `ToolSearch`, `WaitForMcpServers`, `ListMcpResourcesTool`, `ReadMcpResourceTool`. — [Tools reference](https://code.claude.com/docs/en/tools-reference)
- `Agent` runs a subagent in a separate context window; the parent sees only the final result, not intermediate tool calls. Background subagents are the default except listed foreground cases. Launching the subagent does not itself prompt; each of the subagent’s tool calls is checked against permission rules. — [Tools reference](https://code.claude.com/docs/en/tools-reference)
- `AskUserQuestion` asks multiple-choice questions and stays open until answered unless `askUserQuestionTimeout` is set (`60s`, `5m`, or `10m`). The timeout does not apply to permission prompts. — [Tools reference](https://code.claude.com/docs/en/tools-reference)
- Bash: each command is a separate process; `cd` inside the project (or added dirs) persists in the main session but not in subagents; env vars do not persist across commands; default timeout 2 minutes, ceiling 10 minutes; output streamed to a file, killed past 5 GB; valid results inline up to ~30,000 characters. Background via `run_in_background: true`; list/stop with `/tasks`. In `-p` mode, background shells are killed about five seconds after the final result. — [Tools reference](https://code.claude.com/docs/en/tools-reference); [Headless](https://code.claude.com/docs/en/headless)
- Custom tools are MCP servers. Reusable workflows are skills invoked through the existing `Skill` tool, not new tool entries. — [Tools reference](https://code.claude.com/docs/en/tools-reference)
- Version notes on the same page: `EndConversation` requires v2.1.213+; `ReportFindings` v2.1.196+; `SubagentHandback` v2.1.271+ and only in auto mode for local non-fork subagents; `SendFeedback` v2.1.238+; `bashOutputMaxChars` v2.1.261+; Linux/WSL memory cgroup cap `CLAUDE_CODE_TOOL_MEMORY_LIMIT` v2.1.233+. — [Tools reference](https://code.claude.com/docs/en/tools-reference)

**OpenAI Codex** (sandbox page opened; tool catalog page was not opened):

- Local commands in the ChatGPT desktop app, Codex CLI, and IDE extension run inside the sandbox by default. The sandbox applies to spawned commands (`git`, package managers, test runners), not only built-in file operations. — [Sandbox](https://learn.chatgpt.com/codex/sandboxing.md)
- The Codex docs index opened via redirect lists separate pages for Browser, Computer use, Web search, Image generation, Image inputs, MCP, Skills, Subagents, Hooks, Git worktrees, and AGENTS.md. Those bodies were not fetched, so tool names and behavior are not restated here. — [Codex docs nav as rendered from the sandbox fetch’s site chrome](https://learn.chatgpt.com/codex/sandboxing.md)

**Cursor (editor agent, CLI, cloud agents)**:

- Editor Agent tools described at a capability level (not as stable tool IDs): search files/folders, web search, Fetch Rules, read files including images (png/jpg/gif/webp/svg) for vision models, edit files, run shell commands, browser control (navigate, interact, screenshots), image generation (saved under the project `assets/` folder by default), and ask-questions. While waiting on a question, the agent continues reading, editing, or running commands. “There is no limit on the number of tool calls Agent can make during a task.” — [Cursor Agent](https://cursor.com/docs/agent/overview)
- A Project is a coordinator agent that plans and delegates to other agents. — [Cursor Agent](https://cursor.com/docs/agent/overview)
- Cloud agents run in isolated VMs, can build/test/use the changed software, “can also use computers to control the desktop and browser,” support team MCP (HTTP and stdio, OAuth), and can run multi-repo environments and open PRs in the repos they change. They include a built-in Cursor Cloud MCP for transcripts, run events, environment details, and setup logs. — [Cloud Agents](https://cursor.com/docs/cloud-agent)
- CLI binary is `agent`. Modes: Agent (default, full tools), Plan (`Shift+Tab`, `/plan`, `--plan`), Ask (read-only, `/ask`, `--mode=ask`). — [Cursor CLI](https://cursor.com/docs/cli/overview)

**Gemini CLI** (tools reference last updated Sep 1, 2026; banner says unpaid-tier and Google One users were moved to Antigravity CLI on June 18, 2026):

- Execution: `run_shell_command` (interactive sessions and background processes; manual confirmation). Files: `glob`, `grep_search` (legacy alias `search_file_content`), `list_directory`, `read_file` (text, images, audio, PDF), `read_many_files` (also `@` in the prompt), `replace`, `write_file`. Interaction: `ask_user` (questions with `question`/`header`/`type`/`options`), `write_todos`. Web: `google_web_search`, `web_fetch` (validated against private/reserved IPs; in Plan Mode requires explicit confirmation). Memory/planning: `activate_skill`, `get_internal_docs`, `enter_plan_mode`, `exit_plan_mode`. MCP: `list_mcp_resources`, `read_mcp_resource`. Subagents: `complete_task` “is not available to the user.” Experimental task tracker (`experimental.taskTracker`): `tracker_create_task`, `tracker_update_task`, `tracker_get_task`, `tracker_list_tasks`, `tracker_add_dependency`, `tracker_visualize`, plus `update_topic`. Manual shell via `!`. Custom tools via `tools.discoveryCommand` or MCP. — [Tools reference](https://geminicli.com/docs/reference/tools/)
- The same page lists the tracker tools twice, once under “Task Tracker (Experimental)” and again under “Task Tracking” with kind `Think`. — [Tools reference](https://geminicli.com/docs/reference/tools/)

**OpenCode** (tools doc from the `dev` branch source, plus the live agents page which announces “New OpenCode v2”):

- Built-ins documented: `bash`, `edit` (exact string replace), `write`, `read` (line ranges), `grep`, `glob`, `apply_patch`, `skill`, `todowrite`, `webfetch`, `websearch`, `question`. `lsp` is experimental and only present when `OPENCODE_EXPERIMENTAL_LSP_TOOL=true` or `OPENCODE_EXPERIMENTAL=true` (goToDefinition, findReferences, hover, documentSymbol, workspaceSymbol, goToImplementation, call hierarchy). `write` and `apply_patch` are gated by the `edit` permission. `websearch` only with the OpenCode / OpenCode Go provider, or `OPENCODE_ENABLE_EXA` / `OPENCODE_ENABLE_PARALLEL`. `grep`/`glob` use ripgrep and respect `.gitignore`; a project `.ignore` can re-include paths. Custom tools and MCP servers are the extension path. — [OpenCode tools source](https://github.com/anomalyco/opencode/blob/dev/packages/web/src/content/docs/tools.mdx)
- Agents page also names `list`, `todoread`, and a `task` tool. Built-in primary agents: Build (all tools) and Plan (edits and bash default to `ask`). Built-in subagents: General (full tools except todo), Explore (read-only), Scout (read-only external docs/dependency research; can clone a dependency into a managed cache). Hidden system agents: compaction, title, summary. Users invoke subagents with `@`; the model uses Task. Child sessions have keybinds to enter, cycle, and return to the parent. — [OpenCode agents](https://opencode.ai/docs/agents/)

**goose**:

- Tools arrive as extensions (packages such as Developer). The permissions page says goose “performs best with fewer than 25 total tools” was **not** on the page opened; the opened permissions page only describes modes and says read/write classification is a best-effort interpreted by the LLM provider, with examples of “text editor write,” “text editor edit,” and “bash - rm, cp, mv.” A full built-in tool list was not opened. — [goose permission modes](https://goose-docs.ai/docs/guides/managing-tools/goose-permissions)
- When goose uses a CLI provider such as Claude Code, Claude Code’s own permission prompts are routed through goose’s confirmation UI in approve mode. — [goose permission modes](https://goose-docs.ai/docs/guides/managing-tools/goose-permissions)

**Aider**:

- Not a general tool-calling harness in the docs that were opened. It pair-programs in a local git repo: the user adds files, Aider edits them, and git records the edits. In-chat commands include `/add`, `/model`, `/diff`, `/undo`, `/commit`, `/git`. Docs also index chat modes (code, architect, ask, help), images and web pages, voice, lint/test auto-fix, conventions, and scripting. A dedicated tools/MCP/subagent page was not in the docs index that was opened. — [Aider documentation](https://aider.chat/docs/); [Git integration](https://aider.chat/docs/git.html)

**Cline**:

- Plan mode can read the codebase and run searches but cannot modify files or execute commands. Act mode can modify files and run commands; conversation history carries across the switch. `/deep-planning` explores, lists affected files, writes an implementation plan, and asks clarifying questions. Docs recommend asking Cline to create a todo list during planning. A named built-in tool table was not opened. — [Plan & Act](https://docs.cline.bot/core-workflows/plan-and-act)
- Checkpoints snapshot project files after each tool use (file edits, commands, etc.). — [Checkpoints](https://docs.cline.bot/features/checkpoints)

**Amp**:

- Docs say to list built-ins with `amp tools list` and do not enumerate file/shell tools on the tools page. Named capabilities that were opened: `oracle` (second-opinion model; routing depends on mode; in `high` mode the main agent is described as GPT-6 Astra with medium reasoning and the oracle as Claude Fable 5.1; `low`/`ultra` oracle is GPT-6 Astra; `medium` is GPT-5.6 Sol; “these mappings can change”), Librarian subagent (search/read public GitHub and opted-in private repos; default branch only), Painter (image generate/edit, GPT Image 2.5 Sunburst, up to 3 reference images, transparent PNG via a `background` tool option). — [Amp tools](https://ampcode.com/docs/tools)
- Plugins can `amp.registerTool`, `registerSkill`, `registerCommand`, and `amp.createAgent` / `registerAgentMode`, and can run a subagent with `parentThreadID`. Built-in agent mode keys mentioned: `low`, `medium`, `high`, `ultra` (deprecated `smart`/`deep`/`rush` still accepted and remap). — [Amp plugins](https://ampcode.com/docs/customize/plugins)

**Factory Droid**:

- Overview: interactive CLI, project context, approvals, MCP (`droid mcp add` or `/mcp`), Missions (orchestrator plus worker droids; `/missions` or `droid exec --mission`), custom droids (`/droids`), skills (`/skills`, `/create-skill`), hooks (`/hooks`), plugins (`droid plugin install` or `/plugins`), and `droid exec` for scripts/CI. `!` toggles a direct bash mode. — [Droid CLI overview](https://docs.factory.ai/cli/getting-started/overview)
- Hook matchers name tools `Execute`, `Read`, `Edit`, `Create`, `ApplyPatch`, `LS`, `Glob`, `Grep`, `Task`, `FetchUrl`, `WebSearch`, and MCP tools as `mcp__<server>__<tool>`. `SubagentStop` fires when a Task-launched sub-droid finishes. — [Droid hooks](https://docs.factory.ai/docs/harness/hooks)

### Inferences
- Ask-user, todos, plan mode, and subagents are now common harness primitives (Claude, Gemini CLI, OpenCode, Cursor, Cline, Factory, Amp), not differentiators by themselves. The split is whether they are named tools with permission rules (Claude, OpenCode, Gemini, Factory) or product behaviors described without stable tool IDs (Cursor, Cline, Aider).
- Computer use and a real browser are documented as first-class for Cursor cloud agents (desktop and browser) and are indexed, but not read in full, for Codex. They were not found on the Claude Code tools table that was read, nor on the Gemini CLI tools table.

### Gaps
- Codex tool names (shell, apply_patch, web, browser, computer use, MCP) were not read from a tools page; only the docs nav and the sandbox page were opened.
- goose’s built-in extension/tool inventory, Aider’s repo-map page, Cline’s MCP and tool-approval pages, and Amp’s `amp tools list` output were not opened.
- Claude Code’s tools page was truncated; computer-use was not in the tool table that was read. A third-party guide claimed a built-in Computer Use MCP server; that page was not used as a source.
- Whether Gemini CLI’s tool list still applies to Antigravity CLI after the June 18, 2026 replacement was not checked. The tools page itself still documents Gemini CLI as of Sep 1, 2026.

## How do permissions, sandboxing, and auto-approval work?

### Takeaway
The shared pattern is a mode (ask / auto-approve edits / plan / full bypass) plus path or command rules, with deny usually winning. Sandboxing is OS-native for Codex (Seatbelt, bubblewrap, Windows sandbox) and optional/containerized for Gemini CLI; Claude Code documents a Bash sandbox and a separate classifier “auto” mode; Amp ships with tools auto-running unless a plugin adds policy.

### Cited Findings

**Claude Code**:

- Modes and what runs without asking: `default` (Manual; reads only), `acceptEdits` (reads, file edits, and common filesystem commands `mkdir`, `touch`, `rm`, `rmdir`, `mv`, `cp`, `sed`, plus some PowerShell content cmdlets on in-scope paths), `plan` (reads, plus classifier-approved commands when auto mode is available), `auto` (everything, with a background classifier), `dontAsk` (reads and pre-approved tools; anything that would prompt is denied), `bypassPermissions` (everything; “isolated containers and VMs only”). Deny rules block in every mode, including `bypassPermissions`. Allow rules have no effect in `bypassPermissions`. — [Permission modes](https://code.claude.com/docs/en/permission-modes)
- Nothing is auto-approved, including in `bypassPermissions`, for: explicit ask rules, org connector tools set to `ask`, `AskUserQuestion` and MCP tools marked `requiresUserInteraction`, `rm`/`rmdir` of a critical path, cross-session messaging safeguards, and (v2.1.257+) reads outside working directories when `permissions.blockReadsOutsideWorkingDirectories` is on. — [Permission modes](https://code.claude.com/docs/en/permission-modes)
- Auto mode uses a separate classifier that blocks actions that escalate beyond the request, target unrecognized infrastructure, or look driven by hostile content. Explicit ask rules still prompt. With v2.1.283+, auto is the built-in start mode for interactive terminal and VS Code sessions on every plan; `claude -p` and the Agent SDK still start in `default`. Orgs can set `permissions.disableAutoMode` to `"disable"`. Auto mode requires a supported model (page truncated at the model list: on the Anthropic API, Opus 4.6+, Sonnet 4.6+, or a Fable model). — [Permission modes](https://code.claude.com/docs/en/permission-modes)
- Project `.claude/settings.json` cannot start a session in `auto` or `bypassPermissions`. Shift+Tab cycles modes; `dontAsk` is not in the cycle. Plan mode blocks source edits until the plan is approved (`Yes, and use auto mode` / `Yes, manually approve edits` / `No, keep planning`). `Ctrl+G` opens the plan in an editor. — [Permission modes](https://code.claude.com/docs/en/permission-modes)
- Rule syntax from the tools page: `Bash(npm run *)`, `PowerShell(Get-ChildItem *)`, `Read(~/secrets/**)` (also Grep, Glob, LSP), `Edit(/src/**)` (also Write, NotebookEdit), `Skill(deploy *)`, `Agent(Explore)`, `WebFetch(domain:example.com)`. An `Edit` allow also grants read; a `Read` deny also blocks Edit (v2.1.208+) and Write (v2.1.228+). CLI flags `--allowedTools` / `--disallowedTools` and the Agent SDK use the same rules. — [Tools reference](https://code.claude.com/docs/en/tools-reference)
- The same page says Bash sandbox (macOS, Linux, WSL2) and outer isolation are separate from the permission mode; `/sandbox` can select auto-allow. The sandboxing doc itself was not opened. `--dangerously-skip-permissions` is documented for unattended runs inside a container, VM, or sandbox runtime. Cloud sessions ignore `dontAsk` and bypass from settings files. — [Permission modes](https://code.claude.com/docs/en/permission-modes)
- Non-interactive: `--permission-mode`, and v2.1.259+ `--permission-prompts none` (deny anything that would prompt; do not retry; drop `AskUserQuestion`). `dontAsk` denies `AskUserQuestion`, org `ask` connectors, and MCP tools marked `requiresUserInteraction` even if an allow rule matches. — [Headless](https://code.claude.com/docs/en/headless)
- Without `--bare`, a `-p` session runs project hooks and connects `.mcp.json` servers even in a folder that was never trusted, and shows no workspace trust dialog. — [Headless](https://code.claude.com/docs/en/headless)

**Codex**:

- Sandbox and approvals are separate. Sandbox modes: `read-only` (inspect only; edits and commands need approval), `workspace-write` (read, edit in the workspace, routine local commands; “the default low-friction mode”), `danger-full-access` (no filesystem or network sandbox). Approval policies: `on-request` (ask when leaving the sandbox), `never` (no approval prompts). `untrusted` is retired and no longer selectable. `approvals_reviewer` is `user` (default) or `auto_review` (reviewer agent; does not widen the sandbox). Full access is `danger-full-access` plus `approval_policy = "never"`. Lower-risk automation is `--sandbox workspace-write --ask-for-approval on-request`. Config keys named: `sandbox_mode`, `approval_policy`, `approvals_reviewer`, `sandbox_workspace_write.writable_roots`. CLI picker is `/permissions`. — [Sandbox](https://learn.chatgpt.com/codex/sandboxing.md)
- Enforcement: macOS Seatbelt; native Windows sandbox in PowerShell; Linux/WSL2 bubblewrap (`bwrap`), with a bundled helper fallback that needs unprivileged user namespaces, plus Ubuntu AppArmor notes (24.04 vs 25.04). — [Sandbox](https://learn.chatgpt.com/codex/sandboxing.md)
- Command-prefix exceptions are “rules” that allow, prompt, or forbid prefixes outside the sandbox. The rules page was not opened. — [Sandbox](https://learn.chatgpt.com/codex/sandboxing.md)
- ChatGPT web does not expose the local sandbox or approval-mode selector. Work network access is a separate settings control. — [Sandbox](https://learn.chatgpt.com/codex/sandboxing.md)

**Cursor**:

- CLI sandbox is on/off: `/sandbox` or `--sandbox enabled|disabled`, plus a menu for network access. Settings persist across sessions. Sudo prompts go to `sudo` over IPC; “the AI model never sees” the password. — [Cursor CLI](https://cursor.com/docs/cli/overview)
- Cloud agents: Cursor manages VM isolation; users can add secrets, restrict outbound domains, use Tailscale or similar, and private connectivity. Secrets are injected at agent start and are not picked up by agents already running. Self-hosted machines are a separate doc that was not opened. Snapshots can include `.env.local` if it was present at snapshot time; the Secrets tab is the recommended path. — [Cloud Agents](https://cursor.com/docs/cloud-agent)
- Editor-agent approval rules were not on the Agent overview page that was opened.

**Gemini CLI**:

- Mutating tools and shell require manual confirmation; the CLI shows a diff or the exact command. Sandboxing is optional and containerized (see sandbox guide; not opened beyond the tools page’s pointer). Trusted folders gate which directories may use system tools. `web_fetch` checks private and reserved IPs. A policy engine accepts `argsPattern` rules, e.g. deny `write_file` when `file_path` matches `.*\.env`. — [Tools reference](https://geminicli.com/docs/reference/tools/)
- Skill activation shows a confirmation naming the skill, its purpose, and the directory it will be allowed to read. — [Agent Skills](https://geminicli.com/docs/cli/skills)

**OpenCode**:

- Default: all tools enabled and they do not need permission to run. Permissions are `allow` / `ask` / `deny`, keyed by tool, with glob objects for paths or command patterns. Last matching rule wins. Keys include `read`, `edit` (write, edit, apply_patch), `glob`, `grep`, `list`, `bash`, `task`, `external_directory` (any tool touching files outside the worktree), `todowrite`, `webfetch`, `websearch`, `lsp`, `skill`, `question`, `doom_loop`. Wildcard keys match MCP and custom tools (`mymcp_*`). Plan agent defaults file edits and bash to `ask`. `permission.task` can hide subagents from the Task tool description; the user can still `@`-mention them. As of the permissions source excerpt (not the full page), legacy boolean `tools` config was deprecated and merged into `permission` as of v1.1.1; the agents page still documents `tools` as deprecated. — [Tools source](https://github.com/anomalyco/opencode/blob/dev/packages/web/src/content/docs/tools.mdx); [Agents](https://opencode.ai/docs/agents/)

**goose**:

- Modes: Completely Autonomous (default; modify, use extensions, and delete without approval), Manual Approval (confirm before any tools/extensions; granular tool permissions), Smart Approval (auto-approve low-risk, flag the rest), Chat Only (no extension use or file modifications). CLI: `/mode auto|smart_approve|approve|chat`, or `goose configure`. In manual and smart modes it only asks for tools it classifies as write. Read/write classification “makes best effort” and “is interpreted by your LLM provider.” — [goose permission modes](https://goose-docs.ai/docs/guides/managing-tools/goose-permissions)

**Aider**:

- Safety net is git, not a sandbox. Each edit is committed. Dirty files are committed first unless `--no-dirty-commits`. `--no-auto-commits` and `--no-git` exist. Commits use `--no-verify` unless `--git-commit-verify`. `/undo` discards the last change. Author/committer names get “(aider)” unless attribution flags disable it. — [Git integration](https://aider.chat/docs/git.html)

**Cline**:

- Plan mode is the hard read-only gate (no file mods, no commands). Checkpoints are the rollback for Act mode and are described as what makes auto-approve practical: enable auto-approve for edits and commands, review at the end, restore a checkpoint if needed. The auto-approve settings page itself was not opened. — [Plan & Act](https://docs.cline.bot/core-workflows/plan-and-act); [Checkpoints](https://docs.cline.bot/features/checkpoints)

**Amp**:

- “By default, Amp does not ask for approval before running tools.” Untrusted repos, MCP servers, and other inputs can influence actions. Control is a custom policy plugin (`tool.call` can `allow`, `reject-and-continue`, `modify`, or `synthesize`) or an isolated environment. Workspace admins can publish a global workspace plugin. The docs’ example plugin classifies git commands with `amp.ai.ask` and confirms destructive ones. `requireHuman: true` on plugin dialogs requires a human (owner, or human contributors while multiplayer is on; secret inputs stay owner-only). — [Amp tools](https://ampcode.com/docs/tools); [Amp plugins](https://ampcode.com/docs/customize/plugins)

**Factory Droid**:

- Hook input includes `permission_mode`: `"off" | "spec" | "auto-low" | "auto-medium" | "auto-high"`. `PreToolUse` can return `permissionDecision` `allow` (can bypass the normal prompt), `deny`, or `ask`, and `updatedInput` to rewrite parameters. Exit code 2 on `PreToolUse` blocks the call. The autonomy-levels page linked from hooks was not opened, so what each mode auto-approves was not verified. — [Droid hooks](https://docs.factory.ai/docs/harness/hooks)

### Inferences
- Classifier or reviewer auto-approval (Claude auto mode, Codex `approvals_reviewer = auto_review`, goose Smart Approval, Amp’s optional `amp.ai.ask` plugin) is a 2026 harness feature distinct from static allowlists.
- Aider is the outlier: no permission prompt model in the pages opened; undo is a git commit rewind.

### Gaps
- Codex permission profiles (`:read-only`, `:workspace`, `:danger-full-access`) appeared in a search excerpt of `developers.openai.com/codex/permissions` but that URL redirected and the body was not re-read. Execution-policy `.rules` files were the same situation.
- Cursor editor allow/deny rules, Gemini sandbox modes (`docker`/`podman`/`sandbox-exec`), Cline auto-approve toggles, and Factory autonomy levels were not opened.
- Claude permission-modes page was truncated mid auto-mode availability list.

## How do they manage context (compaction, memory files, AGENTS.md / CLAUDE.md, skills, rules)?

### Takeaway
Persistent instruction files plus on-demand skills are the common context design. Claude Code, Gemini CLI, and OpenCode document automatic compaction or summarization as a harness step. Path-scoped rules exist so not every instruction is loaded every turn.

### Cited Findings

**Claude Code**:

- Each session starts with a fresh context window. `CLAUDE.md` (user-written) and auto memory (model-written) are both loaded every session. Auto memory is per repository, shared across worktrees, and only the first 200 lines or 25 KB load. CLAUDE.md is context, not enforcement; hard blocks belong in PreToolUse hooks. — [Memory](https://code.claude.com/docs/en/memory)
- Load order: managed policy file (macOS `/Library/Application Support/ClaudeCode/CLAUDE.md`, Linux/WSL `/etc/claude-code/CLAUDE.md`, Windows `C:\Program Files\ClaudeCode\CLAUDE.md`, or a `claudeMd` key in managed settings) → `~/.claude/CLAUDE.md` → project `./CLAUDE.md` or `./.claude/CLAUDE.md` → `./CLAUDE.local.md`. Ancestor files load at launch and are concatenated root-down, not overridden. Subdirectory files load when Claude reads a file there. HTML comments in CLAUDE.md are stripped before injection. `@path` imports (max depth four) expand at launch; the first external import in a project needs approval. Target size: under 200 lines per file. — [Memory](https://code.claude.com/docs/en/memory)
- `.claude/rules/*.md` load like project CLAUDE.md, or only when matching files are read if `paths` frontmatter globs are set. `paths` is the only frontmatter field read. Brace expansion is capped at 1,000 patterns and 4 MiB per rule (v2.1.217+ behavior for overflow). User rules live in `~/.claude/rules/`. — [Memory](https://code.claude.com/docs/en/memory)
- `AGENTS.md` direct read requires v2.1.277+. Default `claude-md-or-agents-md`: if any `CLAUDE.md` / `.claude/CLAUDE.md` / `CLAUDE.local.md` exists in the working directory or above, AGENTS.md is not read; user and managed CLAUDE.md do not count for that check. Other settings: `claude-md-and-agents-md`, `claude-md`, `managed-only`. Not read: `AGENTS.local.md`, `AGENTS.override.md`, anything under `.agents/`. `/init` can draft a CLAUDE.md; `CLAUDE_CODE_NEW_INIT=1` makes `/init` also offer skills and hooks. `/doctor prompt-audit` (v2.1.283+) reports conflicting or stale instruction files without editing them until asked. — [Memory](https://code.claude.com/docs/en/memory)
- Skills are the on-demand alternative to always-loaded rules (skills page not opened). `--bare` skips auto memory, CLAUDE.md, skills, hooks, plugins, MCP, and custom subagents. A `PreCompact` hook exists in the Factory docs, not confirmed here for Claude beyond the hooks event name in a search excerpt that was not re-fetched. — [Headless](https://code.claude.com/docs/en/headless)

**Gemini CLI**:

- `GEMINI.md` is persistent workspace-wide background. Skills are on-demand: at session start only name and description enter the system prompt; `activate_skill` loads the body after a confirmation and adds the skill directory to allowed file paths. Discovery precedence (low to high): built-in, extension skills, user (`~/.gemini/skills/` or `~/.agents/skills/`), workspace (`.gemini/skills/` or `.agents/skills/`). Within a tier, `.agents/skills/` wins over `.gemini/skills/`. Commands: `/skills list|disable|enable|reload`, `gemini skills install|uninstall`. Skills page last updated Apr 30, 2026. — [Agent Skills](https://geminicli.com/docs/cli/skills)
- Plan mode is a tool pair (`enter_plan_mode` / `exit_plan_mode`), not only a UI flag. — [Tools reference](https://geminicli.com/docs/reference/tools/)

**OpenCode**:

- Skills are `SKILL.md` files loaded by the `skill` tool; the tools page says the agent sees them and loads full content when needed (skills page not opened in full). A hidden compaction agent “compacts long context into a smaller summary” and “runs automatically when needed.” Separate hidden agents generate titles and session summaries. Agent prompts can be inline or `{file:./prompts/...}`. `steps` caps agentic iterations; at the limit the agent is told to summarize and recommend remaining work. — [Tools source](https://github.com/anomalyco/opencode/blob/dev/packages/web/src/content/docs/tools.mdx); [Agents](https://opencode.ai/docs/agents/)

**Cursor**:

- Agent is described as instructions (system prompt plus rules) + tools + model. Fetch Rules retrieves rules by type and description. `/goal` sets a long-lived objective until complete and is “rolling out.” Pairing `/goal` with a built-in `/loop` skill runs a prompt or skill on an interval the agent can choose. Side chats (`/side`, `/btw`) are separate transcripts that use the parent thread as hidden context. — [Cursor Agent](https://cursor.com/docs/agent/overview)
- Compaction behavior was not on the pages opened (a workshop blurb mentioned `/summarize` and automatic compaction; that was a video description, not docs, and is not treated as product fact here).

**Factory**:

- `PreCompact` runs before manual or automatic compaction, with `trigger` `manual` or `auto`, `custom_instructions`, `message_count`, `estimated_tokens`. `SessionStart` sources include `compact`. Overview points at an AGENTS.md guide and a skills guide; those pages were not opened. — [Droid hooks](https://docs.factory.ai/docs/harness/hooks); [Overview](https://docs.factory.ai/cli/getting-started/overview)

**Aider**:

- Docs index a “Repository map” page and a “Specifying coding conventions” page. Neither body was opened, so map token budget and convention file names are not stated here. Chat modes include code, architect, ask, and help. — [Aider documentation](https://aider.chat/docs/)

**Amp**:

- Plugins bundle skills and can keep tools listed in a skill’s `builtin-tools` frontmatter hidden until the skill loads. `agent.start` can append hidden instructions to the user message. No AGENTS.md behavior was on the pages opened. — [Amp plugins](https://ampcode.com/docs/customize/plugins)

**Cline / goose**:

- No memory-file or compaction page was opened. Cline’s plan/act switch keeps the same conversation history. — [Plan & Act](https://docs.cline.bot/core-workflows/plan-and-act)

### Inferences
- `AGENTS.md` and `.agents/skills/` are the cross-tool portability layer Claude (v2.1.277+) and Gemini CLI explicitly implement. Claude’s default is still “CLAUDE.md wins if present.”
- Automatic compaction is documented as a real harness step for OpenCode (hidden agent) and Factory (`PreCompact`, including auto trigger). Claude’s compact command exists in the product (resume-after-compaction and hook names appear in adjacent docs) but the compaction page itself was not opened.

### Gaps
- Claude compaction/`/compact` page, Claude skills page, Codex AGENTS.md and memories pages, Cursor rules/skills pages, Aider repo map and conventions pages, goose memory, Cline rules (`.clinerules` or similar), and Amp instruction files were not opened.
- Auto-memory write path (where files land, who can turn it off) was past the truncation point of the Claude memory page.

## What session, resume, rewind/checkpoint, worktree, and multi-session features exist?

### Takeaway
Resume-by-id is documented for Claude Code and Cursor CLI. Checkpoints that revert files without wiping the transcript exist for Cursor and Cline (Cline uses a shadow git repo). Claude Code’s worktree tools and cross-session messaging are the most explicit multi-session harness; Cursor cloud agents are the parallel remote-session model.

### Cited Findings

**Claude Code**:

- `EnterWorktree` creates an isolated git worktree and switches into it, or switches to an existing one via `path`. Paths outside `.claude/worktrees/` prompt (behavior changed by v2.1.206; nested-repo worktrees accepted as of v2.1.203). `ExitWorktree` returns to the original directory and is not available to subagents that already have their own working directory (`isolation: worktree`). — [Tools reference](https://code.claude.com/docs/en/tools-reference)
- Headless resume: `--continue` continues the most recent conversation; `--resume` takes a session id or, alternatively, the absolute path of a session `.jsonl`. As of v2.1.223, resume by id works from any project on the machine, not only the current directory and its worktrees. As of v2.1.257, `--continue` opens a finished background session but not one still running. JSON output includes `session_id`. SIGTERM exits 143, leaves the turn unfinished, kills running Bash process trees, and runs `SessionEnd` hooks; `CLAUDE_CODE_RESUME_INTERRUPTED_TURN=1` continues the interrupted turn on resume. — [Headless](https://code.claude.com/docs/en/headless)
- `ListAgents` / `SendMessage` can address subagents, teammates, other local sessions, and, with Remote Control, cloud sessions and Remote Control sessions on other machines (v2.1.224+). — [Tools reference](https://code.claude.com/docs/en/tools-reference)
- `CronCreate` tasks are session-scoped and restored on `--resume` or `--continue` if unexpired. — [Tools reference](https://code.claude.com/docs/en/tools-reference)
- Accepting a plan can generate a session title. Permission mode on resume is documented on a sessions page that was not opened. Auto memory is shared across worktrees of a repo; a gitignored `CLAUDE.local.md` is not. — [Permission modes](https://code.claude.com/docs/en/permission-modes); [Memory](https://code.claude.com/docs/en/memory)

**Cursor**:

- Editor checkpoints: automatic snapshots of all modified files before significant changes, stored locally and separate from Git. Restore reverts files only and does not delete messages. — [Cursor Agent](https://cursor.com/docs/agent/overview)
- While an agent runs: Enter queues a follow-up; Cmd+Enter sends immediately; “Send now” / double-Enter steers at the next tool call. CLI: Enter steers at a safe boundary; Enter again interrupts. Side chats are durable and do not interrupt the main thread. Agents Window can search past transcripts via a local index (Cmd/Ctrl+K). — [Cursor Agent](https://cursor.com/docs/agent/overview)
- CLI sessions: `agent ls`, `agent resume`, `agent --continue`, `agent --resume="chat-id"`. Prefix `&` to hand the conversation to a Cloud Agent. — [Cursor CLI](https://cursor.com/docs/cli/overview)
- Cloud agents: many in parallel, laptop can disconnect, each on its own VM, separate branch, push for handoff. Start from iOS app, cursor.com/agents, desktop Cloud dropdown, Slack `@cursor`, GitHub/Bitbucket `@cursor`, Linear, or API. Formerly called Background Agents. Teammates can open a run read-only if they are on the same Cursor team and have repo access; follow-up by teammates needs an admin “team follow-ups” setting. Paid plans only. Artifacts include screenshots, videos, and logs; user can take remote desktop control and hand it back. — [Cloud Agents](https://cursor.com/docs/cloud-agent)

**OpenCode**:

- Subagent runs are child sessions with navigation keybinds (`session_child_first`, cycle, `session_parent`). No rewind/worktree page was opened. — [Agents](https://opencode.ai/docs/agents/)

**Cline**:

- Shadow git repo, separate from project history, committed after each tool use, including untracked files. Persists across editor sessions. Restore Files (code only), Restore Task Only (messages only), or Restore Files & Task. Compare opens a diff. Editing a previous message with “Restore All” restores files to that checkpoint before resubmitting. Enabled by default; large repos may get slow. — [Checkpoints](https://docs.cline.bot/features/checkpoints)

**Aider**:

- `/undo` discards the last AI change. History is the real git branch, not a shadow repo. `/diff` shows changes since the user’s last message. — [Git integration](https://aider.chat/docs/git.html)

**Factory**:

- `SessionStart` sources: `startup`, `resume`, `clear`, `compact`. `SessionEnd` reasons include `clear`, `logout`, `prompt_input_exit`, `other`. Transcript path is in hook input. Worktree behavior was not opened. — [Droid hooks](https://docs.factory.ai/docs/harness/hooks)

**Amp**:

- Multiple threads can run at once in one CLI. Plugins: no `session.end` event. `agent.end` can `continue` with a follow-up; Amp stops chaining plugin continues after five unless `maxContinuations` is raised. Orbs / `executor: 'orb'` can start a background thread; `multiplayerTTLSeconds` is 5 minutes through 7 days for workspace orb threads. — [Amp plugins](https://ampcode.com/docs/customize/plugins)

**Gemini CLI / goose / Codex**:

- Session resume, rewind, and worktree behavior were not on the pages opened. Codex docs nav lists a Git worktrees page that was not fetched. goose’s old `block.github.io` session-management URL 404s and points at goose-docs.ai. — [Sandbox nav](https://learn.chatgpt.com/codex/sandboxing.md); [goose move](https://block.github.io/goose/docs/guides/sessions/session-management)

### Inferences
- Two rewind designs are documented: shadow snapshots that leave conversation intact (Cursor checkpoints, Cline shadow git) versus real git commits you undo (Aider). Claude’s documented rewind primitive in the pages read is worktree isolation plus git, not a shadow checkpoint tool.

### Gaps
- Claude `/rewind` or checkpoint commands, Codex session resume and worktrees, Gemini CLI session storage, goose sessions, OpenCode session export, and Amp thread resume were not opened.

## What hooks, plugins, or extension points exist?

### Takeaway
Claude Code, Factory, Cursor cloud agents, and Amp all document lifecycle hooks that can block or rewrite a tool call. OpenCode and Gemini CLI document custom tools and MCP more clearly than a hook event list in the pages that were opened. goose’s extension is the plugin unit.

### Cited Findings

**Claude Code**:

- Hooks are an official control surface: tool names are hook matchers; `PreToolUse` can decide in place of the permission system (stated on the tools page, which links the hooks guide). The full event list was not re-fetched. A search excerpt of a third-party guide claimed 30+ events and an `mcp_tool` hook type added in v2.1.118 (2026-04-23); that guide is not treated as primary. — [Tools reference](https://code.claude.com/docs/en/tools-reference)
- Confirmed from primary pages that were opened: `SessionEnd` runs on SIGTERM of `claude -p`; `PermissionRequest` hooks can allow a call under `--permission-prompts none`; `Elicitation` hooks can answer MCP elicitation; `--bare` skips hook discovery; project `.claude/settings.json` hooks run in untrusted folders during `-p` unless `--bare` is set. Plugins load unless skipped; `system/init` in stream-json reports `plugins`, `plugin_errors`, `mcp_servers`, `mcp_server_errors`. — [Headless](https://code.claude.com/docs/en/headless)
- Managed settings and managed CLAUDE.md are org extension points separate from hooks. — [Memory](https://code.claude.com/docs/en/memory)

**Factory**:

- Events: `PreToolUse`, `PostToolUse`, `UserPromptSubmit`, `Notification`, `Stop`, `SubagentStop`, `PreCompact`, `SessionStart`, `SessionEnd`. Type is only `"command"`. Exit 0 success; exit 2 blocks (tool, prompt, or stop); other non-zero is non-blocking. JSON `permissionDecision` allow/deny/ask and `updatedInput`. Timeout default 60s. Scopes: `~/.factory/hooks.json`, `.factory/hooks.json`, enterprise managed hooks, legacy `.factory/hooks/hooks.json`. Plugins ship `hooks/hooks.json`. `allowManagedHooksOnly` drops user and project hooks. Hooks are snapshotted at startup. Absolute paths required; `$FACTORY_PROJECT_DIR` and `$DROID_PLUGIN_ROOT` (also `$CLAUDE_PLUGIN_ROOT`) expand. — [Droid hooks](https://docs.factory.ai/docs/harness/hooks)
- Plugins bundle commands, droids, skills, and hooks. — [Overview](https://docs.factory.ai/cli/getting-started/overview)

**Cursor**:

- Cloud agents run command hooks from repo `.cursor/hooks.json`. Enterprise also runs team and enterprise-managed hooks. User `~/.cursor/hooks.json` is not available on cloud VMs. Hooks do not run during early read-only exploratory turns. Supported names listed: `preToolUse`, `beforeShellExecution`, `afterFileEdit`, `beforeSubmitPrompt`, `subagentStart` / `subagentStop`, `preCompact`, `afterAgentResponse` / `afterAgentThought`, `stop`. Tab hooks and `workspaceOpen` are IDE-specific and not listed as cloud-supported. — [Cloud Agents](https://cursor.com/docs/cloud-agent)

**Amp**:

- Plugins are TS/JS modules. Locations and precedence when names collide: project (`.amp/plugins/`), then system (`~/.config/amp/plugins/` or `$XDG_CONFIG_HOME/amp/plugins/`), then personal, then workspace. Events opened: `session.start`, `tool.call`, `tool.result`, `agent.start`, `agent.end`, `changes.prompt`. APIs: `registerTool`, `registerSkill`, `registerCommand`, `registerLinkPattern`, `createAgent`, `registerAgentMode`, `getBuiltinAgent`, `amp.ai.ask`, `amp.$` shell, UI confirm/input/select/notify. `amp plugins add <url>`, `amp --execute` still applies plugin activation settings. — [Amp plugins](https://ampcode.com/docs/customize/plugins)

**OpenCode**:

- Custom tools in config, MCP servers, markdown agents in `~/.config/opencode/agents/` and `.opencode/agents/`, `opencode agent create`. The tools source mentions `tool.execute.before` / `tool.execute.after` and says `apply_patch` must be matched as `"apply_patch"`, not `"patch"`. A hooks reference page was not opened. — [Tools source](https://github.com/anomalyco/opencode/blob/dev/packages/web/src/content/docs/tools.mdx); [Agents](https://opencode.ai/docs/agents/)

**Gemini CLI**:

- MCP servers, `tools.discoveryCommand`, extensions that can bundle skills, and a policy engine. A hooks page was not opened. — [Tools reference](https://geminicli.com/docs/reference/tools/); [Agent Skills](https://geminicli.com/docs/cli/skills)

**goose**:

- Extensions are the add-on unit. `goose configure` can add, toggle, and remove extensions and set tool permissions. No hook event list was opened. Docs banner: goose moved to the Agentic AI Foundation (AAIF); blog link dated 2026-04-07. — [goose permission modes](https://goose-docs.ai/docs/guides/managing-tools/goose-permissions)

**Aider / Cline**:

- No hooks page was opened. Cline documents an MCP config at `~/.cline/mcp.json` for CLI and a servers UI for IDE extensions, including stdio and remote streamable HTTP or legacy SSE, plus `cline mcp` wizard and per-server `autoApprove` arrays. That MCP page was seen as a search extract, not a full fetch, so it is not repeated as a detailed finding here.

### Inferences
- Factory’s hook schema is visibly close to Claude Code’s (same event names, exit code 2, `hookSpecificOutput`, and even `$CLAUDE_PLUGIN_ROOT` as an alias). Cursor cloud hooks use a similar but not identical event vocabulary (`preToolUse`, `beforeShellExecution`).

### Gaps
- Claude hooks reference (full event list, `mcp_tool` handler) was not opened. OpenCode plugin/hooks docs, Gemini hooks, goose extensions catalog, and Cline hooks were not opened as full pages.

## What headless / SDK / CI modes exist?

### Takeaway
Claude Code, Cursor CLI, and Factory document a non-interactive print/exec mode aimed at CI. Claude Code’s Agent SDK is the same loop as the CLI. Codex’s docs nav lists SDK, app server, GitHub Action, and non-interactive mode, but those pages were not read. Aider documents scripting. Amp has `amp --execute`.

### Cited Findings

**Claude Code**:

- `claude -p` / `--print` runs non-interactively and exits 0 on success. `--output-format` is `text` (default), `json` (result, session id, metadata, `total_cost_usd`), or `stream-json` (with `--verbose` and `--include-partial-messages`). `--json-schema` puts structured output in `structured_output` (invalid schema is a hard error as of v2.1.205; `format` is not enforced). Stdin is capped at 10 MB. `--continue` / `--resume` work with `-p`. `--bare` is recommended for CI and “will become the default for `-p` in a future release”; it does not read OAuth or the keychain, so `ANTHROPIC_API_KEY` or `apiKeyHelper` is required for the Anthropic API. `--bare` still gives Bash and file read/edit tools. Background subagents and workflows keep the process open, default idle cap 10 minutes (`CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS`, `0` to wait forever). Monitor watches default to a 5-minute timeout. Python and TypeScript Agent SDK packages exist for callbacks and native messages; `canUseTool` is a permission host. `claude setup-token` mints a one-year `CLAUDE_CODE_OAUTH_TOKEN` for CI; it can only make model requests (no Remote Control, no claude.ai connectors). Bare mode does not read that OAuth token. GitHub Actions are a linked doc that was not opened. `--bg` is rejected with `-p`. — [Headless](https://code.claude.com/docs/en/headless); [Authentication](https://code.claude.com/docs/en/iam)

**Cursor**:

- `agent -p` is print mode for scripts and CI, with `--model` and `--output-format text` shown. `--mode=plan` and `--mode=ask` exist. Cloud handoff is `&` inside an interactive session, plus an API for cloud agents (API page not opened). — [Cursor CLI](https://cursor.com/docs/cli/overview); [Cloud Agents](https://cursor.com/docs/cloud-agent)

**Factory**:

- `droid exec` is the headless path for scripts and CI, with structured input and output. `droid exec --mission` runs a mission headlessly. — [Overview](https://docs.factory.ai/cli/getting-started/overview)

**Amp**:

- Plugin activation applies to `amp --execute` and `amp --no-tui`. Plugin agents can create threads from those runners. — [Amp plugins](https://ampcode.com/docs/customize/plugins)

**Aider**:

- Docs index “Scripting aider” via command line or Python. The scripting page was not opened, so flags are not listed here. — [Aider documentation](https://aider.chat/docs/)

**OpenCode / Gemini CLI / goose / Cline**:

- Gemini CLI accepts a non-interactive prompt via `-p` in the sandbox quickstart that was quoted from the tools page’s sandbox link; the sandbox page itself was not opened, so CI/SDK claims are not made. OpenCode, goose, and Cline headless/SDK pages were not opened. Cline has a CLI (`cline mcp` was in a search extract only).

**Codex**:

- Docs nav lists Codex SDK, App Server, GitHub Action, and Non-interactive mode under “Build with Codex.” Bodies were not fetched. — [Sandbox page site index](https://learn.chatgpt.com/codex/sandboxing.md)

### Inferences
- Structured streaming JSON plus a session id for resume is the CI contract Claude documents in detail. Cursor and Factory advertise the same job (print/exec in CI) with less protocol detail on the pages that were opened.

### Gaps
- Codex non-interactive flags, OpenCode `opencode run` or server mode, goose CLI one-shot, Cline CLI `cline -y` style headless, and Gemini CLI `--output-format` were not opened.

## What is notably absent or called out as a limitation in their own docs?

### Takeaway
Vendors document limits more often as safety boundaries and startup skips than as missing features. The sharpest self-stated limits: Amp runs tools with no approval by default; goose’s autonomous mode is the default and read/write classification is best-effort; Claude’s instruction files are not enforcement; cloud agents cannot see your home-directory hooks; Glob/Grep are absent by default on some OSes in Claude Code; Gemini CLI’s consumer SKU was replaced.

### Cited Findings

- Claude: CLAUDE.md and auto memory “are context, not enforced configuration.” Deny rules are the hard layer. Auto mode “does not guarantee safety.” `bypassPermissions` is for containers/VMs. Glob and Grep are absent by default on macOS, Linux, and WSL. Several tools are unavailable on Bedrock, Google Cloud’s Agent Platform, or Microsoft Foundry because they need Anthropic-hosted delivery (`PushNotification`, `RemoteTrigger`, `SendUserFile` on some of those). Background subagents before v2.1.186 auto-denied any tool call that would have prompted. Piped stdin to `-p` is capped at 10 MB. `--bare` drops CLAUDE.md, memory, hooks, skills, and MCP on purpose. Cloud sessions ignore `dontAsk` and bypass-from-settings. — [Memory](https://code.claude.com/docs/en/memory); [Permission modes](https://code.claude.com/docs/en/permission-modes); [Tools reference](https://code.claude.com/docs/en/tools-reference); [Headless](https://code.claude.com/docs/en/headless)
- Codex: `danger-full-access` removes filesystem and network boundaries and should be intentional. `untrusted` approval policy is retired. Linux sandbox needs bubblewrap or working user namespaces; missing `bwrap` yields a startup warning. ChatGPT web has no local sandbox selector. Web search, plugins, and the remote browser are controlled separately from the code/shell sandbox. — [Sandbox](https://learn.chatgpt.com/codex/sandboxing.md)
- Cursor cloud: user-level hooks are unavailable; hooks skip early read-only turns; secrets do not update running agents; viewing a teammate’s agent is read-only unless team follow-ups are enabled; team membership alone does not grant repo access; paid plan required; spend limit is requested on first use. Checkpoints are not a substitute for Git. `/goal` “is rolling out.” — [Cloud Agents](https://cursor.com/docs/cloud-agent); [Cursor Agent](https://cursor.com/docs/agent/overview)
- Gemini CLI: banner states unpaid tier and Google One users were moved to Antigravity CLI on June 18, 2026. Task tracker is experimental. Mutators always need confirmation unless the user changes policy (policy engine exists; default confirmation is stated on the tools page). Skill consent is a prompt. `web_fetch` blocks private/reserved IPs. — [Tools reference](https://geminicli.com/docs/reference/tools/); [Agent Skills](https://geminicli.com/docs/cli/skills)
- OpenCode: `websearch` is unavailable unless the hosted provider or an enable flag is set. `lsp` is off unless an experimental env flag is set. `todowrite` is disabled for subagents by default. Plan mode does not hard-deny edits; it sets them to `ask` (a config can change plan to `deny`). No step limit unless `steps` is set. OpenCode v2 is announced on the agents page; what v2 removes was not read. — [Tools source](https://github.com/anomalyco/opencode/blob/dev/packages/web/src/content/docs/tools.mdx); [Agents](https://opencode.ai/docs/agents/)
- goose: autonomous mode is the default. Smart/manual read-vs-write detection is best-effort and provider-interpreted. Chat mode cannot use extensions or modify files. — [goose permission modes](https://goose-docs.ai/docs/guides/managing-tools/goose-permissions)
- Aider: git integration can be turned off, but the docs say that is not recommended and the user should keep backups. Pre-commit hooks are skipped unless opted in. No sandbox is described on the git page. — [Git integration](https://aider.chat/docs/git.html)
- Cline: Plan mode cannot modify files or run commands. Checkpoints can be slow and large; they are on by default. The shadow repo is not the user’s git history. — [Plan & Act](https://docs.cline.bot/core-workflows/plan-and-act); [Checkpoints](https://docs.cline.bot/features/checkpoints)
- Amp: no approval before tools, by default. Librarian searches only the default branch. Painter transparency is unreliable if requested only in the prompt; the tool `background` option must be set. Plugin `continue` chains stop after five unless raised. Image uploads for tool results are limited to 4.9 MB decoded and 8000 px per side. Deprecated mode names still work but remap. — [Amp tools](https://ampcode.com/docs/tools); [Amp plugins](https://ampcode.com/docs/customize/plugins)
- Factory: hooks run with the user’s credentials; only command hooks exist; invalid matcher regexes are skipped; org hooks cannot be removed by lower scopes; SessionEnd cannot be blocked; a user cancel emits `Notification` rather than `Stop`, so a hook cannot override cancel. — [Droid hooks](https://docs.factory.ai/docs/harness/hooks)

### Inferences
- “Absent” in 2026 docs is often a default-off or platform-gap (Claude Glob/Grep on Unix, OpenCode web search and LSP, Gemini consumer CLI), not a missing file editor.
- The products that most clearly tell users the harness will not save them are Amp (auto-run), goose (autonomous default, fuzzy read/write), and Claude (instructions are not policy).

### Gaps
- No primary page was opened that catalogs missing features for Codex, Cursor’s local agent, or OpenCode v2. Antigravity CLI’s feature delta versus Gemini CLI was not read (only the replacement banner and its blog URL).
- Aider HISTORY / current release version was not opened, so “as of version X” is not stated for Aider.
- Factory tool list beyond hook matcher names, and whether Droid has a browser or computer-use tool, was not verified.
