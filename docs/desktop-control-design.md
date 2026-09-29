# Desktop Control ("Computer Use") — Design

**Status:** proposal, not implemented · **Written:** 2026-09-29 · **Scope:** after the v2.0 release bar (it isn't in `v2-implementation-plan.md` yet)

## Summary

Let the agent see the user's desktop (screenshots) and operate it (mouse, scroll, keyboard) — opt-in, off by default, and treated as the most dangerous capability the harness has.

Recommendations:

1. **Seven builtin tools with a `desktop_` prefix** (`desktop_screenshot`, `desktop_click`, `desktop_move`, `desktop_drag`, `desktop_scroll`, `desktop_type`, `desktop_key`) rather than one `computer` tool with an `action` enum. In this harness, risk and permission rules are **per tool name**. Separate tools let a screenshot be Medium risk while a click is High, and let rules name individual actions. Flat per-action schemas are also easier for small and medium models to call correctly.
2. **Coordinates are pixels of the last screenshot**, which is downscaled to fit 1280×800 by default. The harness maps them back to physical pixels itself. The process must be per-monitor DPI-aware on Windows; we measured this below.
3. **Backends:** `xcap` for capture on Windows/macOS. Our own small X11 capture on Linux via `x11rb`, which avoids xcap's pipewire/bindgen stack. `enigo` for input on all three OSes. Both sit behind our own `DesktopBackend` trait, with a fake for CI. Wayland is **unsupported in v1** and detected with a clear error. A portal and libei backend comes later.
4. **Builtin, not an MCP server**, in a new `arbe-desktop` crate behind a cargo feature. The safety design (per-app approval subjects, kill switch into `cancel_turn`, screenshot retention and persistence, protecting the harness's own terminal) needs deep runtime integration. The MCP client also currently turns images into `[image: …]` placeholders.
5. **Safety:** `[desktop]` config lives in the global config only, and a dedicated `desktop` profile is required. Approvals are per app: the approval subject of a click, type or key action is *the application under the point*, so pressing `a` approves "clicks in Calculator" for the session, reusing today's exact-rule mechanism. A pointer-takeover kill switch applies (the user moves the mouse and the turn is aborted), along with rate limits, blocked apps (never touched, masked in screenshots), secrets and password-field checks on typing, and a hard refusal to touch the harness's own terminal. Desktop tools are never available in `--print` or to subagents, and are available in `--headless` only by opt-in.

---

## 1. Fit with the current architecture

What we build on (all verified in the code):

- `ToolExecutor` (`crates/arbe-tools/src/lib.rs`): `execute(invocation, &ToolContext)`, `description()` (schema derived from the `Args` type via schemars), `default_risk()`, `subject()`, `parallel_safe()`. A `ToolContext` can only be issued by the gate (`Authorized::execute`), so desktop tools get approval enforcement for free.
- **Risk is looked up per tool name** (`ToolRegistry::risk_of(name)` in `agent/tools.rs`), not per call. This is the deciding fact for the tool surface (§2).
- `StandardApprovalPolicy`: a High-risk call never auto-approves on a bare tool name. `a` on a High-risk call records an **exact, literal** rule `tool(subject)`. Rules support tool-name wildcards (`desktop_*(…)`).
- Images already flow: `ToolResult.attachments` → `ContentBlock::Image` → sent only when `provider.capabilities(model).vision`, otherwise replaced by a note (`agent/tools.rs`). `read_file` caps images at 3.75 MB raw, and we keep that cap.
- `arbe-memory` counts an image as a flat `IMAGE_TOKEN_ESTIMATE = 1600` tokens. Pruning is budget-driven and doesn't limit image count.
- Tool output goes through `Redactor`, which only works on text. It can't redact pixels.
- Profiles carry a tool allow-set (`tools = [...]`, `prefix*` supported). **`tools` absent means every registered tool**, so registration alone must never expose desktop tools to the `coding` profile.
- `Layer::strip_sensitive` strips risky settings from untrusted project configs.
- `--print --approve all` approves everything, so desktop tools must not even be registered there.
- `arbe-mcp` flattens MCP image content to `"[image: image/png]"` (`client.rs`), which means an MCP screenshot server would currently be blind.

## 2. Tool surface

### Decision: seven `desktop_*` tools

| Tool | Risk | Subject (for rules) | Notes |
|---|---|---|---|
| `desktop_screenshot` | Medium | none | The only way to set the coordinate frame |
| `desktop_move` | Medium | app under point | Hover menus and tooltips |
| `desktop_scroll` | Medium | app under point | |
| `desktop_click` | **High** | app under point | |
| `desktop_drag` | **High** | app under the start point | |
| `desktop_type` | **High** | focused app | |
| `desktop_key` | **High** | focused app | |

All are `parallel_safe() == false` (one pointer, one keyboard).

Why not one `computer` tool, the shape Anthropic's older `computer_2025*` tool and OpenAI's `computer_use` action union use:

- **Risk granularity.** One tool name means one risk level. A screenshot would have to be High (a prompt every time, and `a` approves only that exact call), or a click would be Medium. Neither is acceptable.
- **Rules.** `deny = ["desktop_key"]` or `allow = ["desktop_scroll"]` just work. A single tool would need rule syntax for arguments.
- **Model reliability.** An `action` enum plus a bag of optional fields is where small and medium models produce invalid combinations (`click` without coordinates, `type` with `coordinate`). With per-action schemas every field is required or has an obvious default. Anthropic's newest toolset (`computer_toolset_20260801`) also splits actions into separate member tools.
- **Cost:** about 1k prompt tokens for seven specs, paid only in the `desktop` profile.

We don't use Anthropic's server-defined computer tool type in v1. Our tools work as ordinary function tools on every provider. Mapping to the native toolset for Claude models is a later option (§9, D6).

### Argument schemas

Written as the Rust `Args` types. Doc comments become the model-facing descriptions via `ToolDescription::from_args`. Coordinates are flat integers, not `[x, y]` arrays, because small models get flat fields right more often.

```rust
/// desktop_screenshot — "Capture the screen. Coordinates in every other desktop_* tool
/// are pixels of the most recent screenshot (origin top-left)."
struct ScreenshotArgs {
    /// Display index from a previous screenshot's `displays` list. Default: the display
    /// of the last screenshot, else the primary display.
    display: Option<u32>,
    /// Wait this long before capturing (0–10000 ms), e.g. for a page to load.
    wait_ms: Option<u32>,
}

struct ClickArgs {
    x: u32, y: u32,
    /// "left" (default), "right" or "middle".
    button: Option<Button>,
    /// 1 (default), 2 = double-click, 3 = triple-click.
    clicks: Option<u8>,
    /// Held during the click: any of "ctrl", "shift", "alt", "meta" (Windows key / Cmd).
    modifiers: Option<Vec<Modifier>>,
}

struct MoveArgs { x: u32, y: u32 }

struct DragArgs { from_x: u32, from_y: u32, to_x: u32, to_y: u32, button: Option<Button> }

struct ScrollArgs {
    x: u32, y: u32,
    /// "up", "down", "left" or "right".
    direction: Direction,
    /// Wheel notches, 1–20 (default 3).
    amount: Option<u8>,
}

struct TypeArgs {
    /// Literal text to type into the focused element (max `desktop.max_type_chars`,
    /// default 500). Use desktop_key for Enter, Tab and shortcuts.
    text: String,
}

struct KeyArgs {
    /// A key or combination joined with "+": "enter", "ctrl+s", "ctrl+shift+t",
    /// "alt+tab", "f5". Names: a–z, 0–9, f1–f24, enter, tab, escape, backspace,
    /// delete, space, up, down, left, right, home, end, pageup, pagedown;
    /// modifiers ctrl, shift, alt, meta (aliases: cmd, win, super, return, esc).
    keys: String,
    /// Press it this many times (1–20, default 1).
    repeat: Option<u8>,
}
```

**Results.** `desktop_screenshot` returns a text part plus the image:

```json
{"display":0,"width":1280,"height":800,"cursor":[611,402],"focused_app":"firefox",
 "displays":[{"index":0,"primary":true,"width":2880,"height":1800},{"index":1,"primary":false,"width":1920,"height":1080}]}
```

Every input tool returns `"ok"` plus, when `desktop.screenshot_after_action = true` (the default), a fresh screenshot taken `settle_ms` (default 300) after the action. This is OpenAI's pattern. It halves model round trips, and small models often forget to look again. Invalid coordinates are an error that restates the bounds: `x=1400 is outside the 1280×800 screenshot`. An input action with no screenshot yet fails with "take a screenshot first".

## 3. Screenshots and coordinates

**Frame state.** Each screenshot records a `Frame { display_id, origin_phys, size_phys, model_size, layout_hash }`. Input actions map through the *last* frame. If the monitor layout changed since then (a hash of every display's origin, size and scale), the action fails with "the screen layout changed; take a new screenshot".

**Scaling (pure function, unit-tested).**
`s = min(1, max_w / Pw, max_h / Ph, sqrt(max_pixels / (Pw·Ph)))`, with model size `(round(Pw·s), round(Ph·s))`.
Mapping back: `px = Ox + min(Pw-1, floor((mx + 0.5) · Pw / w))`, and the same for y. It uses the exact ratio to avoid drift, and clamps to the display.
Defaults are `max_w = 1280`, `max_h = 800`, `max_pixels = 1_150_000`. These stay inside the older Claude limits (1568 px long edge, about 1.15 MP) and inside Anthropic's recommended 1024–1366 range for computer use. Newer Claude models accept up to 2576 px, so these are config (`desktop.max_width` etc.).

**Physical vs. logical pixels.** The internal space is **physical pixels in virtual-desktop coordinates** (origins may be negative, e.g. a monitor left of the primary). Each backend converts to its injection unit:

- **Windows:** the process must call `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)` before any capture or input. We measured this on the dev machine (2880×1800 panel at 175 %). Without it, xcap reported 2880×1800 while enigo's `main_display()` reported **1646×1029**: DPI virtualization, so every click lands at the wrong place. With the call, both report 2880×1800. The setting is process-wide. That's fine for us (the terminal is a different process) and it's a hidden global side effect we have to document.
- **macOS:** capture is in pixels (2× on Retina), while `CGEvent` works in points. Divide by the display's backing scale.
- **X11:** physical pixels throughout. Monitors come from RandR.

**Format and size.** PNG by default (text stays sharp), falling back to JPEG q80 if PNG exceeds the 3.75 MB cap. Measured with xcap on the dev machine: capture 53–83 ms at 2880×1800, resize to 1280×720 about 30 ms, **PNG 365 KB / JPEG 110 KB**; at 1024×576 PNG is 262 KB. Size is never a problem at the target resolution.

**Token cost** at 1280×800: about 1,365 tokens on Anthropic (w·h/750), and about 1,100 on OpenAI high-detail (6 tiles × 170 + 85). `IMAGE_TOKEN_ESTIMATE = 1600` is conservatively fine.

**Retention.** A 50-action task with screenshots after every action would be about 70k tokens of images. We add a **count-based image limit** to `arbe-memory`: keep the newest `desktop.screenshots_in_context` images (default 3) and replace older ones with a stub like `[screenshot 1280×800 omitted]`. Anthropic's guidance is to prune in batches for prompt caching; we can quantize like the existing 4-turn truncation. This is generic: any tool's images count.

**Persistence.** Screenshots can contain anything on screen, and `turns.jsonl` currently stores images as inline base64. Default is `desktop.persist_screenshots = false`: the persisted trace (`in_flight.jsonl` and `turns.jsonl`) stores the stub, while the in-memory history keeps the image for the current context. This needs a small core addition, an "ephemeral" marker on attachments (e.g. `ToolExecutor::persist_attachments() -> bool`, default `true`), which the turn persister honors.

**Coordinate convention.** Pixels by default. Some vision models (Qwen-VL, Gemini) are trained on a normalized 0–1000 grid, so `desktop.coordinates = "pixels" | "normalized_1000"` changes only the pure mapping function and the tool descriptions.

## 4. Backends and crates

### Evaluation (versions from crates.io, 2026-09-29)

| Crate | Version / last release | License | Verdict |
|---|---|---|---|
| `xcap` (capture) | 0.9.8, 2026-08 | Apache-2.0 | **Use on Windows/macOS.** Active, monitor and window enumeration (app name, title, rect, z-order), reportedly moved to ScreenCaptureKit on macOS (verify in D0). On Linux it **unconditionally** pulls `pipewire` (bindgen + libclang at build time, libpipewire at run time), `xcb` and `libwayshot-xcap` (**BSD-2-Clause, rejected by our `deny.toml`**, confirmed by running `cargo deny check` against it) |
| `enigo` (input) | 0.6.1, 2025-08 | MIT | **Use for input.** Default Linux backend is pure-Rust `x11rb` (no libxdo). Wayland/libei backends are experimental and feature-gated. Handles Unicode typing and keymaps, which is the hard part to write ourselves. On Linux it links system `libxkbcommon` (build: `libxkbcommon-dev`). Release cadence is slowish, so we wrap it in our trait |
| `x11rb` | 0.14, 2026-07 | MIT/Apache | **Use for Linux capture** (GetImage + RandR, about 200 lines) and window enumeration (`_NET_CLIENT_LIST`, `WM_CLASS`, `_NET_WM_PID`). Pure Rust, already under enigo |
| `global-hotkey` | 0.8.0, 2026-05 | MIT/Apache | Later, for a kill-switch hotkey (Win/macOS/X11) |
| `rdev` | 0.5.3, 2023-06 | MIT | Reject: unmaintained. `cargo deny` runs with `unmaintained = "all"` |
| `screenshots` | 0.8.10, 2024-03 | Apache-2.0 | Reject: superseded by xcap |
| `autopilot` | 0.4.1, 2025-04 | MIT/Apache | Reject: low usage, older capture paths |
| `windows` / `objc2-core-graphics` | current | MIT/Apache | Direct calls where needed (DPI awareness, UI Automation password check, macOS AX/permission preflight) |
| `ashpd` + `reis` | 0.13 / 0.7 | MIT | Later, for the Wayland backend (portal + libei) |

A throwaway build on Windows of xcap + enigo + `image` (png/jpeg) produced a **531 KB release binary**. Size is a non-issue on Windows.

Because Linux uses `x11rb` for capture, xcap becomes a `cfg(any(windows, target_os = "macos"))` dependency, and **Linux builds never compile pipewire**. cargo-deny still resolves the whole graph (`all-features = true`, all targets), so `libwayshot-xcap` still has to be allowed: **add `BSD-2-Clause` to `deny.toml`** (OSI-approved permissive). This is a maintainer decision.

### Per-OS feasibility

- **Windows:** GDI/DXGI capture via xcap, `SendInput` via enigo. No permissions needed. **UIPI:** injected input can't reach elevated (admin) windows from a non-elevated harness. We report that as an error and don't try to work around it.
- **macOS:** needs **Screen Recording** and **Accessibility**, granted to the *responsible* app, which is the terminal emulator (Terminal/iTerm/VS Code) when run from a shell. That means granting it lets *every* program run in that terminal do the same. Preflight at startup (`CGPreflightScreenCaptureAccess`, `AXIsProcessTrusted`) and fail with instructions. Isolating these permissions would need a signed helper `.app` (D6).
- **Linux X11:** XTest via enigo, GetImage via x11rb. Works without prompts.
- **Linux Wayland:** unsupported in v1. With `XDG_SESSION_TYPE=wayland`, XTest through XWayland reaches only X clients and root-window capture shows only X windows, which is silently wrong. So we refuse with a clear message. The feasible path later is xdg-desktop-portal **ScreenCast + RemoteDesktop with `ConnectToEIS`** (libei) via `ashpd` + `reis`: supported by GNOME and KDE Plasma 6, **not** by wlroots/Sway. The user gets a consent dialog per session (restore tokens can persist it).

### Structure

New crate **`crates/arbe-desktop`** (depends on `arbe-core` and `arbe-tools`). A new crate is justified: it keeps system-library dependencies out of `arbe-tools`.

```rust
pub trait DesktopBackend: Send + Sync {          // physical pixels, virtual-desktop coords
    fn displays(&self) -> Result<Vec<DisplayInfo>, DesktopError>;
    fn capture(&self, display: DisplayId) -> Result<RgbaImage, DesktopError>;
    fn windows(&self) -> Result<Vec<WindowInfo>, DesktopError>;   // app, title, rect, z, pid
    fn focused_window(&self) -> Result<Option<WindowInfo>, DesktopError>;
    fn focused_is_password(&self) -> Option<bool>;                 // best effort
    fn cursor(&self) -> Result<PhysPoint, DesktopError>;
    fn move_to(&self, p: PhysPoint) -> Result<(), DesktopError>;
    fn button(&self, b: Button, dir: Press) -> Result<(), DesktopError>;
    fn scroll(&self, dx: i32, dy: i32) -> Result<(), DesktopError>;
    fn key(&self, k: Key, dir: Press) -> Result<(), DesktopError>;
    fn text(&self, s: &str) -> Result<(), DesktopError>;
}
```

- Always compiled, with no system dependencies: geometry and scaling, frame state, key-combo parser, safety checks (blocked apps, harness window, secrets, rate limiter, drift detection), the tools themselves, and `FakeBackend`.
- `#[cfg(feature = "native")]`: `NativeBackend`, run as a **dedicated actor thread** that owns the enigo instance. That serializes all input, sidesteps enigo's non-`Send` platform state, and keeps a list of pressed keys and buttons so every exit path (error, cancel, panic, drop) releases them.
- `arbe-runtime` gets a `desktop` feature → `arbe-desktop/native`. The `arbeharness` binary enables it for Windows/macOS release builds. For Linux, a desktop-enabled binary links `libxkbcommon` and fails to start on a machine without it, so it ships as a separate artifact or behind the feature (open question).

## 5. Builtin vs. MCP server

| | Builtin (`arbe-desktop`, feature-gated) | Separate MCP server |
|---|---|---|
| Approval integration | Per-tool risk, per-app subjects, exact-rule session grants, harness-window protection | Risk from MCP annotations only; no subjects; one rule for the whole server tool |
| Kill switch / cancel | Can call `cancel_turn` and release held keys | Only `notifications/cancelled`; the harness can't see a takeover |
| Images | Works today | Needs MCP image → `ContentBlock::Image` support first (currently a placeholder) |
| Screenshot persistence / retention | Ephemeral marker, count limit | Same core work *plus* MCP plumbing |
| Headless / subagent gating | Registration-time control | Possible via allow-sets, but easy to misconfigure |
| Isolation | In-process; process-wide DPI flag | Separate process: crash and DPI isolation |
| Optionality / packaging | Cargo feature; Linux needs libxkbcommon | Fully optional, own release cadence, reusable by other agents |
| macOS permissions | Terminal's | Still the terminal's (the child's responsible process), no gain |

**Recommendation: builtin.** Every safety property in §6 depends on the harness understanding desktop actions. An MCP server would get the weakest approval model for the highest-risk capability. The `DesktopBackend` trait keeps the door open: moving injection out of process later (e.g. a signed macOS helper) is a backend swap, not a redesign. Independently, **teaching `arbe-mcp` to pass image content through** is worth doing (small task, listed in D6).

## 6. Safety design

**Opt-in, in three independent places:**

1. The build has the `desktop` feature.
2. `[desktop] enabled = true` in the **global** config (or `--config`). `[desktop]` is **global-only**, like `trusted_projects`: a project config that sets it is rejected with an error, even when trusted. Nobody opens a repo expecting it to take over their mouse.
3. The active profile names the tools explicitly. New rule: `desktop_*` tools are offered **only when the allow-set lists them by name or prefix**. `tools` absent ("everything") does *not* include them. We ship a built-in **`desktop` profile**:
   - `tools = ["desktop_*", "todo_write", "remember"]`. No `execute` or file writes, which limits what a prompt-injected page can do.
   - A `desktop` prompt template.
   - Config load fails if the model lacks vision ("the desktop profile needs a vision model; see [[models]] to declare one").

**Approval.**

- Input tools are High risk, so nothing auto-approves on a tool name, and `approval.mode = "denylist_block"` still prompts.
- The subject is the target application (process or bundle name, e.g. `CalculatorApp.exe`, `com.apple.TextEdit`, `firefox`). Pressing `a` records the existing exact rule `desktop_click(firefox)`, so later clicks in Firefox run without asking, and a click in any other app asks again. That covers "confirm before acting outside the allowed app" with no new policy code.
- Config can pre-authorize: `allow = ["desktop_*(CalculatorApp.exe)"]`.
- TOCTOU: the executor re-resolves the target app at execution and fails if it differs from the approved subject ("the window under (x, y) changed since approval").
- Open question: one `a` could cover all `desktop_*` tools for that app, instead of one prompt per tool per app (small change to `SessionApprovals`).

**Hard refusals** apply whatever the approval says, enforced inside the executor:

- **The harness's own terminal.** Record the foreground window and our process ancestry at session start. Never click, type or send keys into it: that would let the model approve its own calls by typing `y`. As a second layer, the TUI discards key events that arrive while an injection is in flight (plus 300 ms), which is feasible because both run in one process.
- **`desktop.blocked_apps`** (defaults include common password managers): never a target. Their window rectangles are **filled black in screenshots**, using window rectangles and z-order, before encoding. That's the practical form of screenshot "redaction". OCR-based secret redaction is out of scope. Only `Redactor`'s text scrubbing applies to the result text.
- **Typing secrets:** refuse `desktop_type` if `Redactor` would change the text (it contains a known API key or token).
- **Password fields:** refuse `desktop_type` when the focused element is a password field. Windows uses UI Automation `IsPassword`, macOS uses AX role `AXSecureTextField`, X11 has no check (AT-SPI later). This is best effort and documented as such.
- **Dangerous combos:** `desktop.confirm_keys` (default: `meta+r`, `meta+space`, `alt+f4`, `meta+q`, `ctrl+alt+t`, `ctrl+alt+delete`) always prompt, even under a session grant.

**Kill switch.**

- **Pointer takeover.** While a desktop turn runs, a watchdog polls the cursor every 50 ms. If it moved more than 8 px from where we last put it, or entered a screen corner (pyautogui's fail-safe), the watchdog releases held keys and buttons, cancels the turn through an injected `cancel_turn` callback, and publishes a new `RuntimeEvent::DesktopAborted { reason }`. Typing is chunked (about 20 characters) so an abort takes effect mid-string.
- **Hotkey.** A global hotkey (`global-hotkey`, e.g. Ctrl+Alt+Esc) comes in D6. It doesn't work on Wayland, where pointer takeover is enough.
- `Esc` in the TUI still cancels, but the terminal usually isn't focused.

**Rate limits and guards.**

- `min_action_interval_ms = 150`, `max_actions_per_turn = 150` (ends the turn with a new `StopReason::DesktopActionLimit`), `max_type_chars = 500`.
- The existing `RepeatedToolCall` guard stays: three identical rounds is a stuck loop.
- One controller per machine: `~/.arbe/desktop.lock` (pid, session id) taken on the first desktop action and released at turn end or close.

**Visibility.**

- The TUI header shows a red `DESKTOP` badge while a desktop turn runs, plus the approved apps. The status bar rings the terminal bell on an approval request, since the terminal is probably behind other windows. Every action is already a `ToolCallProposed` / `ToolCallCompleted` event and persisted in `turns.jsonl`, so there is an audit trail.
- OS-level overlays or notifications are out of scope for v1.

**Prompt injection.** On-screen text is attacker-controllable (web pages, emails). Mitigations:

- the `desktop` template states that screen content is data, never instructions, and tells the model to stop and ask before logins, payments, sending messages, accepting terms or deleting data (Anthropic's guidance);
- per-app approval limits blast radius;
- no `execute` or file tools in the profile;
- blocked apps.

We have no screenshot classifier like Anthropic's hosted one. That residual risk goes in the user guide.

**Non-interactive modes.**

- `--print`: desktop tools are **never registered**, even with `--approve all`.
- `--headless` (JSON-RPC): not registered unless `desktop.headless = true`, since an embedding editor may have a human answering `approval/decide`.
- **Subagents:** the `task` child config always removes `desktop_*`. There is one pointer, and concurrent children would fight over it.

**Config sketch** (global only):

```toml
[desktop]
enabled = false
max_width = 1280
max_height = 800
coordinates = "pixels"            # or "normalized_1000"
screenshot_after_action = true
settle_ms = 300
screenshots_in_context = 3
persist_screenshots = false
min_action_interval_ms = 150
max_actions_per_turn = 150
max_type_chars = 500
abort_on_pointer_takeover = true
blocked_apps = ["1Password*", "KeePass*", "Bitwarden*"]
confirm_keys = ["meta+r", "meta+space", "alt+f4", "meta+q", "ctrl+alt+t"]
headless = false
```

## 7. Testing

**CI (no display anywhere).** Everything runs against `FakeBackend`, which provides configurable displays (incl. negative origins and 1.75/2.0 scales), windows with z-order, a scriptable cursor, and an operation log.

- **Pure unit tests:** scale factor for many resolutions, round trip model → physical (every corner and edge maps inside the display; exact ratio, no drift), `normalized_1000` mapping, layout-change detection, key-combo parser and aliases, rate limiter, drift detector, blocked-window masking (pixels inside the rect are black, outside untouched), PNG→JPEG fallback over the cap.
- **Tool tests via the fake:** "take a screenshot first", out-of-bounds errors, subject = app under point, TOCTOU mismatch, harness-window refusal, password-field and secret refusal, held keys released on cancel and on error, pointer takeover → abort callback fired.
- **Runtime tests:** desktop tools absent from `coding`/`general` with `tools` unset, absent under `--print` and in subagents, `[desktop]` in project config rejected, vision check, `a` → exact per-app rule, ephemeral screenshots stubbed in `turns.jsonl`, image count limit.
- **Compile coverage:** a CI step builds and clippies `arbe-desktop --features native` on all three OSes. Linux needs `apt-get install libxkbcommon-dev`; verify nothing else is required. `cargo deny` passes with BSD-2-Clause allowed.

**Live (`#[ignore]`, `ARBE_LIVE_DESKTOP=1`, a human present).**

- Automated: capture primary, then move to 4 points via the model-space mapping and read back `cursor()`. This catches DPI mistakes automatically; run it at 100 %, 150 % and 175 % on Windows and on Retina.
- End to end with a real vision model: open Notepad / TextEdit / xterm, type a sentence, save to a temp file, verify the file.

**Manual checklist:**

- two monitors including one left of the primary;
- moving the mouse mid-task aborts within about 100 ms and leaves no stuck modifier keys;
- a click in a second app prompts again;
- a password manager window is black in the screenshot;
- a browser password field refuses typing;
- clicking the TUI terminal is refused;
- a macOS missing-permission message;
- a Wayland session refusal message;
- an elevated window on Windows gives a clear error.

## 8. Implementation plan

Sizes: S < 1 day, M 1–3 days, L 3–5 days.

| # | Task | Size |
|---|---|---|
| **D0** | **Spike.** xcap + enigo on real macOS (SCK path, permissions, Retina points) and X11. Confirm Linux CI build deps. Decide BSD-2-Clause. Confirm xcap `Window` gives pid/app name on each OS | S |
| **D1** | **`arbe-desktop` core (no system deps).** `DesktopBackend` trait, `FakeBackend`, geometry and scaling, `Frame`, key parser, the seven tools with schemas, subjects, results, error texts. Unit tests | M |
| **D2** | **Native backends.** Actor thread. Windows (DPI awareness, UIPI error), macOS (preflight, point conversion), X11 (x11rb capture/RandR/windows, enigo input). Wayland detection. Held-key release. Live `#[ignore]` round-trip test | L |
| **D3** | **Runtime integration.** `[desktop]` config (global-only, validation), `desktop` profile and prompt template, explicit-listing rule for `desktop_*`, vision check, registration gating (`--print`, `--headless`, subagents), ephemeral attachments in persistence, count-based image retention in `arbe-memory`, `desktop.lock`, CI job for `--features native` | M |
| **D4** | **Safety.** Pointer-takeover watchdog → `cancel_turn` + `DesktopAborted`, corners, rate limits + `StopReason::DesktopActionLimit`, harness-window protection + TUI key discard, blocked-app masking, secret and password-field checks (Win UIA, macOS AX), `confirm_keys`, TUI badge + bell | M–L |
| **D5** | **Docs and verification.** User guide (tools, config, profile, permissions per OS, residual risks, "Not yet active" for Wayland), v2 plan entry, live runs on Windows and macOS with a frontier vision model | S–M |
| **D6** | **Later (optional).** Global kill-switch hotkey; Wayland portal + libei backend; `zoom` (region at full resolution); map to Anthropic's native computer toolset for Claude models; MCP image pass-through in `arbe-mcp`; group session grant (`a` covers all `desktop_*` for one app); signed macOS helper for isolated permissions; AT-SPI password detection | — |

The critical path is D0 → D1 → D2/D3 in parallel → D4 → D5, about 2–3 weeks of focused work.

## 9. Open questions for the maintainer

1. Accept adding **`BSD-2-Clause`** to `deny.toml` (needed for xcap's `libwayshot-xcap`)? The alternative is writing our own Windows/macOS capture, about +1 week.
2. **Linux packaging:** ship one Linux binary with `desktop` (hard dependency on `libxkbcommon.so`, fine on desktops, may break minimal servers) or two artifacts?
3. Should `[desktop]` really be global-only, or allowed in *trusted* project configs?
4. Should `a` grant **all** desktop tools for an app (one prompt per app) or stay per tool (up to five prompts per app)?
5. Default `persist_screenshots = false` makes resumed sessions lose old screenshots (they'd be stale anyway). Is that acceptable?
6. Is `screenshot_after_action = true` the right default, given about 1.3k tokens per action?
7. Should the `desktop` profile ship with a default model, or require the user to pick a vision model?
8. Is Wayland (GNOME/KDE via portal + libei) needed before release, or is "X11, Windows, macOS" acceptable for v1?

## Sources

- Anthropic computer-use tool docs: https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool
- OpenAI computer-use guide: https://developers.openai.com/api/docs/guides/tools-computer-use
- xcap: https://github.com/nashaofu/xcap · enigo: https://github.com/enigo-rs/enigo (Cargo.toml features) · xkbcommon-rs Cargo.toml
- XDG RemoteDesktop portal (`ConnectToEIS`): https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html · libei portal integrations: http://who-t.blogspot.com/2026/07/libei-integrations-in-xdg-remotedesktop.html · wlroots lacks RemoteDesktop: https://github.com/NousResearch/hermes-agent/issues/127860
- macOS 15 `CGWindowListCreateImage` obsoleted: https://trac.macports.org/ticket/71136
- Measurements: throwaway crate in the session scratchpad (xcap 0.9.8 + enigo 0.6.1 + image 0.25, Windows 11, 2880×1800 @ 175 %), `cargo deny check` with the repo's `deny.toml`.
