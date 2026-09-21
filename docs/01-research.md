# 01 — Research: reference apps and what to take from each

The redesign goal: feel like being **at the computer** — full capability —
while reading the agent's state semantically, not as raw CLI scrollback.
Nobody in this space has nailed both halves; each app got a piece right.

## Competitive landscape

### Omnara (YC) — agent command center

Wraps Claude Code / Codex sessions, streams parsed session files over SSE,
desktop + web + iOS/Android + Apple Watch. Users praise monitoring and
handoff; the #1 complaint in reviews: **no real Android notifications** —
you have to open the app to see if an agent needs you.

**Take:**
- Session-level semantic feed as the default view (they parse
  `~/.claude/projects` JSONL — Lerdr already does this via
  `internal/conversation`, for 7+ agent kinds).
- Approvals/diffs as first-class mobile cards.
- Their gap is our moat: Lerdr's push pipeline already works — keep it
  front and center.

**Avoid:** their architecture needs a cloud account; sessions die with the
laptop unless migrated to their cloud. Lerdr's local-first + E2EE is
strictly better — don't lose it.

### Happy Coder (slopus/happy, MIT) — Claude Code on the phone

CLI wrapper + relay server + React Native/Expo app. E2EE with TweetNaCl,
QR pairing, voice via LiveKit, slash commands. The closest ideological
peer: self-hostable, encrypted, runs on your machine.

**Take:**
- Proof that a JSONL-driven chat-style session view is the right primary
  UX on a phone.
- Their sync engine (per-session encrypted envelope, sequence numbers)
  validates Lerdr's watch+ack design.

**Avoid:** React Native chat ergonomics — message-list thinking applied to
terminals. Their terminal parity story is weak; ours must be strong.

### VibeTunnel — terminal to browser

Turns any terminal into a shareable live stream. Excellent at exactly one
thing: **fidelity of the live terminal** (ttyd/asciinema-grade rendering).

**Take:**
- Terminal mode is a real mode, not a gimmick: keyboard input, control
  keys, scrollback, resize.
- Their rendering insight applies to us: render rows, not the whole frame.

**Avoid:** it's read-mostly monitoring. Input latency and special keys on a
phone keyboard are unsolved there — we solve it with the special-keys bar +
semantic actions.

### Termius / Blink Shell — mobile SSH/terminal

The benchmark for **mobile terminal input**: special-keys toolbar (Esc,
Tab, Ctrl, arrows), Ctrl-combos, key repeat, IME handling, font sizing,
pinch zoom, session switching.

**Take:**
- The special-keys row + Ctrl modifier latch pattern (Blink's is the best).
- Snippet shortcuts (reusable commands) — maps to our slash commands.
- Hardware keyboard support path.

### ChatGPT / Claude / Gemini apps — agent chat UX

**Take:**
- Composer: autosize, draft persistence, attach, voice, send-state.
- Streaming text with markdown, code blocks with copy, tool-call cards.
- Pull-up "details" affordance for collapsible tool payloads — exactly how
  we should render `ConversationTool.input/output`.

### GitHub Mobile — inbox/triage

**Take:**
- Notification inbox as a first-class tab with swipe-to-done.
- Grouped, skimmable lists with strong empty/loading states.
- Deep-link discipline: every notification lands on exactly the right pane.

### PagerDuty / Opsgenie — attention semantics

**Take:**
- Attention states have **urgency and acknowledgment**, not just badges:
  agent needs input → persistent card, swipe to open, snooze.
- Map `attention_kind: approval|question|chat` to distinct visual urgencies.

### Telegram — polish and gesture vocabulary

**Take:**
- Swipe gestures on rows (swipe agent → quick approve / open / stop).
- Per-conversation drafts (we already have `prompt-drafts`).
- Smooth shared-element transitions list→detail.

## Material 3 Expressive — status and fit

M3 Expressive is the 2025 design system refresh; in Compose it lives in
`androidx.compose.material3` **1.5.x** (currently alpha; use
`compose-bom-alpha`, e.g. 2026.06.00+ / material3 1.5.0-alpha22+).
Components and APIs relevant to this app:

| Component | Use |
|---|---|
| `ButtonGroup` (stable in 1.5.0-a22) | Approval/answer actions on question cards — connected buttons with press-expand animation |
| `SplitButtonLayout` | Agent row actions (primary: open; secondary: overflow) |
| `FloatingActionButtonMenu` | Home FAB → new agent / new workspace / paste pairing link |
| `Carousel` | Multi-relay strip on home; agent screenshots/thumbnails |
| `WavyProgressIndicator`, `LoadingIndicator` (morphing shapes) | Agent working/thinking states — the signature M3E look |
| `FlexibleBottomAppBar` / `ShortNavigationBar` | Main nav; composer-adjacent actions |
| `MaterialExpressiveTheme` + `motionScheme()` | Spring-based motion everywhere; dynamic color from wallpaper |
| Shape library (rounded, "cookie", clover…) | Status chips and agent avatars get distinct expressive shapes |

Caveats: APIs marked `@ExperimentalMaterial3ExpressiveApi` still shift —
pin the version, isolate expressive components behind thin wrappers, and
keep a `material3` stable fallback theme file.

## Terminal rendering on native Android — the pivotal decision

Pane content arrives as **text lines** (post-ANSI from `read_pane`), not a
PTY byte stream. So we do NOT need a VT100 emulator — we need:

1. An **ANSI→AnnotatedString parser** (SGR colors/attrs; strip the rest) —
   port of `frontend/src/lib/terminal.ts` parsing, with the existing Go/TS
   test vectors as golden fixtures.
2. A **line-diff applier** for `pane_delta` ops (copy/insert/delete) — a
   direct port of `applyPaneDelta` semantics.
3. A **virtualized row renderer**: `LazyColumn` keyed by row index +
   generation, monospace font (bundle Geist Mono / JetBrains Mono),
   hardware-accelerated — trivially 60 fps, no WebView tax.
4. **Input path**: visible IME for text (`send_text`), intercept
   `KeyEvent` for hardware keys, special-keys toolbar, Ctrl latch →
   `send_keys`. Size lease (`lease_pane_size`) driven by the composable's
   measured cell grid — this is where "same capability as the computer"
   is won or lost.

The semantic feed and the terminal are **two renderers over one watch
stream** — feed consumes conversation pages + attention state; terminal
consumes pane frames/deltas. Both coexist in the agent screen.

## Synthesis — the product thesis

> **Omnara's feed + Happy's privacy + VibeTunnel's terminal fidelity +
> Termius's input + GitHub's inbox + PagerDuty's urgency + M3E polish —
> over a wire protocol that never leaves your machines.**

Lerdr already beats every one of them on plumbing (multi-agent coverage,
E2EE, push, gateway+tailscale transports, structured questions). The new
app wins on the two axes nobody combined: **semantic clarity** (what the
agent is doing, needs, finished) and **full terminal parity** when you want
the raw machine.
