# Product

<!-- impeccable:product-schema 1 -->

Written without an interview: the session that wrote it had no way to ask the owner. Every fact
below comes from `README.md`, `AGENTS.md`, `docs/guide.md`, `docs/status.md` and
`docs/specs/2026-11-26-calin-design.md`. Lines marked *(inferred)* are readings of those documents
that the owner has not confirmed.

## Platform

web

A single-page web app served by the Calin binary, installable as a PWA on a phone. A Tauri
desktop shell is planned and does not exist yet.

## Users

One developer, who runs Calin for themself. Calin is single-user by design; teams and permissions
are a non-goal.

They use it in two situations:

- **At a laptop or desktop**, beside their editor: starting sessions, reading a transcript next to
  a diff, the graph or the terminal, and reviewing what agents changed across several repos.
- **On a phone, away from the desk**: checking what needs them, answering a permission request or
  a question, reading a transcript, glancing at a diff, approving a workflow gate. The phone is a
  first-class client, not a fallback (spec, section 1 goals; AGENTS.md "Phone first").

The user is fluent in git, worktrees, pull requests, shells and coding agents. *(inferred)* They
read dense technical content well and want facts, not guidance.

## Product Purpose

Calin is a self-hosted orchestrator for coding agents. One Rust server runs agent CLIs (Claude
Code today) in git worktrees with the user's own login, records everything as an append-only event
log, and shows it live in the web app. Agents keep working when every client is closed.

Success: the user can start, watch, steer and review agent work across several repositories from
any device, and nothing merges without them.

## Positioning

Three things set it apart from cezar, the tool that inspired it, and are the reason it exists:

1. **A session spans several repositories.** Start from a home repo, attach the repos the work
   touches, and review every change in one place.
2. **Orchestration is a graph.** Who spawned whom and who said what to whom is recorded as events
   and drawn live: session graph, timeline, workflow run.
3. **The workbench is built in.** Editor, diff, terminal and search are part of the app, so work
   can be reviewed and corrected from a phone without another tool.

## Operating Context

- Screens: the sessions list (home, with a summary of what needs the user), the new-session
  composer (repo picker, modes, workflow, issues), a session (transcript, permission and question
  cards, plan, header actions) with side panels Changes, Files (tree and editor), Terminal and
  Graph (graph, timeline, workflow), Settings (home repo, repos, workflows, access, notifications),
  Automations (list and editor), Usage (cost, tokens, plan limits), sign-in, and error and empty
  states.
- Wide layout from 1024 px: sessions list as a sidebar beside the open page, session transcript
  beside a side panel. Narrow layout: one column, the list is the home screen.
- Sessions run for minutes to hours; the user returns to them, often from a notification that a
  session waits for them, reached review, failed or had its pull requests merged.
- Long live transcripts, code, diffs and terminal output are the main content.
- Dry run (`CALIN_DRY_RUN=1`) simulates every agent; it is how the app is developed and tested.
- Light and dark themes follow the system setting (AGENTS.md "Web app").

## Capabilities and Constraints

- SolidJS 2, Vite+, Bun, Tailwind v4, CodeMirror 6, xterm.js, a hand-written SVG graph renderer.
  No ELK for the session graph; no Monaco.
- Every screen works at 360 px wide; touch targets are at least 44 px (AGENTS.md).
- Initial web bundle under 250 KB compressed, with editor, terminal and graph loaded on demand
  (spec, section 2 resource targets).
- Content Security Policy allows scripts from the app's own origin only: no inline scripts, no
  third-party scripts or fonts at runtime.
- All server state arrives over REST and one event WebSocket, opened only from `src/api/`.
- No telemetry of any kind (spec, section 1 non-goals).
- Terms the UI and docs use consistently: session, repo, home repo, worktree, attach mode
  (Worktree, Read only, Edit in place), permission mode (Ask, Accept edits, Autonomous), workflow,
  gate, automation, review, shipping, pull request, dry run, device token.
- Session states shown to the user: queued, preparing, running, waiting, review, shipping, done,
  failed, cancelled.

## Brand Commitments

- Name: Calin. No logo beyond the existing icons in `apps/web/public/`.
- Voice: plain, factual, short sentences; says what happened and what the user can do. Errors name
  the cause. *(inferred from the existing copy and the docs' style)*
- Honest about state: the UI says "Dry run: agents are simulated" and that cost on a subscription
  is an estimate. Nothing pretends to be more finished than it is.

## Evidence on Hand

- Real flows run end to end in `e2e/run.mjs` and `e2e/signin.mjs` against the real binary.
- The mock runner's demo directives (`demo: orchestrate`, `demo: note`, `demo: ask`) produce
  realistic sessions for screenshots.
- No user research, customers, testimonials or metrics exist. None may be invented.

## Product Principles

1. **The user decides.** Nothing merges on its own; every session ends at a review step, and what
   waits for the user is always the most visible thing on screen.
2. **The log is the truth.** Every view is a replay of a session's events; the UI shows what was
   recorded, attributed to who did it.
3. **Phone and desk are equals.** Every flow works on a phone; the wide layout adds side-by-side
   views, not features.
4. **Dense, calm, legible.** A tool read for hours: information density over decoration, status
   through words plus color, nothing that moves without a reason. *(inferred)*
5. **Zero configuration.** Every setting has a working default; the UI never blocks on setup.

## Accessibility & Inclusion

- Touch targets of at least 44 px; inputs at 16 px so iOS does not zoom (AGENTS.md, `index.css`).
- Text tokens keep at least 4.5:1 contrast against their backgrounds in both themes (`index.css`).
- One visible focus style everywhere; reduced motion is honored by the graph views and pulses.
- Status is never color alone: every state has a word.
