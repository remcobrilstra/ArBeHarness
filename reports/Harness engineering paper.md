# Lessons from "Harness Engineering" (arXiv 2609.00006v1)

Barbaste, Darrigol, Vu, Wiltberger — *Harness Engineering: Anatomy, Architecture, and Evolution of Coding Agents — A Source-Code Study of Eleven Systems*. Read 2026-10-01 through a summarizing fetch of the arXiv abstract and HTML pages, not the PDF; system-specific details below are as that summary reported them and should be checked against the paper before being quoted.

## What the paper is

A source-code study (~4M lines, Python/TypeScript/Rust) of eleven coding harnesses — Claude Code, Codex CLI, Gemini CLI, Mistral Vibe, OpenHands, Aider, Mini-SWE-Agent, Hermes, Pi, OpenCode, OpenClaw — plus the Omnigent meta-harness. It splits a harness into seven subsystems (agent loop, LLM integration, tools, memory/context, safety, orchestration, extensibility), catalogues 29 patterns and 13 cross-cutting observations, follows eight systems across a quarter, and closes with 18 recommendations and a 90-line minimum viable harness.

Findings that matter most for us:

- **Size doesn't buy results.** Mini-SWE-Agent (~5K lines) scores like OpenHands (~100K) on SWE-Bench. Production code mass goes to safety, UX and extensibility, not to loop sophistication.
- **No frameworks, no embeddings.** None of the systems imports LangChain/LangGraph/AutoGen; all use hand-rolled async loops. None uses vector retrieval for code — ripgrep, tree-sitter, glob and markdown context files only.
- **Skills now lead MCP** (9/11 vs 8/11), converging on the `SKILL.md` format under `.agents/skills/`. Tools and skills load on demand, not upfront.
- **Threshold compaction** is standard (Codex, Gemini CLI, Mistral Vibe, Hermes, Pi, OpenCode, OpenClaw). OpenCode anchors its summaries to file paths and line numbers.
- **Verify-on-stop** (Hermes): before ending, check that edits compile/test.
- **Behaviour moved from prompt prose to configuration** over the observed quarter; hooks spread (Codex adopted Claude Code's hook vocabulary).
- **Safety is stratified by deployment:** operator-facing products stack policy rules + an LLM reviewer + an OS sandbox; self-hosted single-user harnesses are permissive with syntax-aware command rules.
- **Multi-agent converges on spawning** with context forks; parallel fan-out gains ~90% on internal benchmarks at ~15× the tokens.
- **Platformization:** six systems ship ACP servers; OpenHands hosts Claude Code/Codex/Gemini CLI as ACP backends; harnesses ship SDKs.
- **Benchmarks:** the authors dropped SWE-Bench figures from the latest edition as self-reported and not comparable; Pi asks for real session data instead.

Its anti-pattern list: a general-purpose agent framework, embedding retrieval without a deterministic fallback, unbounded context, a monolithic prompt, a single safety layer.

## Where ArBeHarness already matches

Hand-rolled tokio loop; streaming with concurrent tool batching (`parallel_safe`); grep/glob only; threshold compaction (~80% → ~40%) that keeps tool pairs and survives resume; on-demand skills (`load_skill`); MCP and command hooks; plan mode; subagents sharing a `Lineage`; turn guards (tool-round limit with a wrap-up round, turn-token cap, three identical rounds); deny rules that hold in every approval mode (the paper's "policy floor that survives YOLO"); Anthropic cache breakpoints; an embeddable `Harness` and headless JSON-RPC. None of the anti-patterns apply, except that the system prompt is one template rather than modular sections.

## What to take from it

| # | Learning | Paper source | For ArBeHarness | Size | Status |
|---|---|---|---|---|---|
| 1 | Check before the turn ends | Hermes verify-on-stop | A `before_turn_end` hook phase that can send the turn back to the model with feedback (a failing check command, or `{"continue": "..."}`), only after the turn changed something. Done as a hook rather than a dedicated feature, so it works for non-coding checks too and costs no new config surface. | S | Done (2026-10-01) |
| 2 | Interoperable skills layout | `SKILL.md` + `.agents/skills/` convergence (9/11) | Also load `<name>/SKILL.md` folders and the `.agents/skills/` project path, so skills written for other agents work unchanged. Project skills come with the repo: they are prompt text, so they need the same scrutiny as instruction files. | S | Done (2026-10-01) |
| 3 | Defer tool definitions | Deferred loading across the corpus | Above a size threshold, offer MCP tools through a search step instead of sending every definition every request (also covers `todo.md`'s "MCP schema lookup before use"). | M | Open |
| 4 | Forgiving `edit_file` matching | Aider's fuzzy-match cascade | Exact match first, then fallbacks (trailing whitespace, indentation), always requiring exactly one match. Line endings are already normalized. Aimed at small models, our known weak spot. | S–M | Done (2026-10-01) |
| 5 | Stable, cacheable prompt prefix | Claude Code cache boundary, Hermes bit-perfect prefixes | Audit that the system prompt runs static → semi-static → per-turn, and that OpenAI-compatible cached-token counts reach `usage`. Never measured. | S | Open |
| 6 | Anchored compaction summaries | OpenCode | Ask the compaction model for concrete file paths, symbols and line numbers, and show each tool call's subject (path or command) in the transcript it summarizes — clipped arguments used to cut paths out. | XS | Done (2026-10-01) |
| 7 | Session fork / rewind | Pi session tree, Claude Code and Codex forks | `/fork` or `/rewind N` into a new session with a `parent` link (`meta.json` already has one). | M | Open |
| 8 | ACP server | 6/11 systems; OpenHands hosts harnesses over it | Map the headless JSON-RPC surface to ACP so editors and orchestrators can host ArBe. | M | Open |
| 9 | Prompt per model family | OpenCode family matrix, Pi's toolset-dependent builder | Prompt fragments selected by model-name prefix via `ModelCatalog` (e.g. stricter tool-call guidance for small models) — reduces the multi-provider "abstraction tax". | S–M | Open |
| 10 | Richer loop detection | Gemini CLI hybrid detector | Catch A↔B oscillation and near-duplicate calls, not only identical rounds. | S | Open |
| 11 | Subagent cost visibility | ~15× token cost of fan-out | Roll subagent tokens and cost into the parent's totals (already on the pending list; this makes it more urgent). | S | Open |

## Deliberately not adopted

- **LLM approval reviewer and OS sandbox** — the paper ties these to operator-facing deployments; for a self-hosted harness it calls syntax-aware command rules sufficient, which we have (allow rules never stretch `*` across shell operators). The OS sandbox question is covered separately in `Agent harness feature gaps.md`. Opt-in worktree isolation for subagents would be the cheap middle step.
- **Embedding retrieval** — nobody in the corpus uses it for code.
- **Benchmark chasing** — use the `scenarios/` ladder and the `live_agent` suite instead: a fixed task set run across providers, recording success and tokens, to tell whether items 1, 4 and 9 actually help.
