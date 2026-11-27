---
name: Calin
description: A calm, dense workbench for watching and steering coding agents, on a desk or a phone.
colors:
  bg: "#f7f7f5"
  surface: "#ffffff"
  sunken: "#efefec"
  line: "#dcdcd7"
  line-strong: "#8d8d86"
  fg: "#1b1b19"
  muted: "#5c5c56"
  accent: "#2a56c6"
  accent-fill: "#2a56c6"
  accent-fg: "#ffffff"
  accent-soft: "#e6ecfb"
  tone-attention: "#8a5a00"
  tone-success: "#1f7a3d"
  tone-danger: "#b3261e"
  attention-soft: "#fbf3df"
  danger-soft: "#fbeceb"
  bg-dark: "#141513"
  surface-dark: "#1c1d1b"
  sunken-dark: "#101110"
  line-dark: "#33352f"
  line-strong-dark: "#73756d"
  fg-dark: "#e8e8e4"
  muted-dark: "#a5a59e"
  accent-dark: "#8fb0ff"
  accent-fill-dark: "#3a5fd0"
  accent-fg-dark: "#ffffff"
  accent-soft-dark: "#1d2740"
  tone-attention-dark: "#e2b24a"
  tone-success-dark: "#6fcf8e"
  tone-danger-dark: "#ff8a80"
  attention-soft-dark: "#33290f"
  danger-soft-dark: "#3a1c1a"
typography:
  title:
    fontFamily: "system-ui, -apple-system, Segoe UI, Roboto, Helvetica Neue, sans-serif"
    fontSize: "1.25rem"
    fontWeight: 600
    lineHeight: 1.45
  body:
    fontFamily: "system-ui, -apple-system, Segoe UI, Roboto, Helvetica Neue, sans-serif"
    fontSize: "0.9375rem"
    fontWeight: 400
    lineHeight: 1.45
  body-small:
    fontFamily: "system-ui, -apple-system, Segoe UI, Roboto, Helvetica Neue, sans-serif"
    fontSize: "0.875rem"
    fontWeight: 400
    lineHeight: 1.45
  label:
    fontFamily: "system-ui, -apple-system, Segoe UI, Roboto, Helvetica Neue, sans-serif"
    fontSize: "0.75rem"
    fontWeight: 500
    lineHeight: 1.45
  code:
    fontFamily: "ui-monospace, SF Mono, Cascadia Mono, Menlo, Consolas, monospace"
    fontSize: "0.8125rem"
    fontWeight: 400
    lineHeight: 1.5
rounded:
  control: "6px"
  pill: "9999px"
spacing:
  gutter: "12px"
  gutter-wide: "16px"
  touch: "44px"
components:
  button:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.fg}"
    rounded: "{rounded.control}"
    padding: "0 0.9rem"
    height: "44px"
  button-primary:
    backgroundColor: "{colors.accent-fill}"
    textColor: "{colors.accent-fg}"
    rounded: "{rounded.control}"
    padding: "0 0.9rem"
    height: "44px"
  field:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.fg}"
    rounded: "{rounded.control}"
    height: "44px"
  choice-checked:
    backgroundColor: "{colors.accent-soft}"
    textColor: "{colors.fg}"
    rounded: "{rounded.control}"
    height: "44px"
  badge:
    backgroundColor: "{colors.surface}"
    rounded: "{rounded.pill}"
    padding: "2px 8px"
    typography: "{typography.label}"
---

# Design System: Calin

## Overview

**Creative North Star: "The Lab Bench"**

Calin is a bench a developer comes back to all day: sessions laid out side by side, a transcript
next to its diff, a terminal one tab away. The interface is quiet so that the work is loud. Content
(agent text, code, diffs, terminal output, the graph) fills the space; chrome is hairlines, small
labels and the system font. Color is spent on one thing at a time: what needs the user.

The same system runs at 360 px and at 1440 px. On a phone one thing is shown at a time and the
sessions list is the home screen; from 1024 px the sessions list becomes a sidebar and a session
shows its transcript beside a side panel. Nothing is decorative: no gradients, no illustrations,
no shadows at rest, no motion without a state change behind it.

Light and dark are equal citizens. Each color token is defined once per theme; the reader picks
System, Light or Dark in Settings, and the choice is applied before the first paint.

**Key Characteristics:**

- Warm-gray tinted neutrals, one blue accent, four status tones that always come with a word.
- Hairline borders and tonal steps (bg, surface, sunken) instead of shadows.
- System sans for prose and UI, system mono for anything the machine wrote.
- 44 px touch targets everywhere, 16 px form text so iOS does not zoom.
- Dense type: 15 px body, 14 px secondary, 12 px labels; one 20 px page title.

## Colors

A warm, slightly yellow-gray neutral ramp with a single cobalt accent; status tones are muted
enough to sit in dense lists without shouting.

### Primary

- **Cobalt** (`accent`, dark `accent-dark`): links, the selected tab, the focus ring, checked
  choices, the user's own messages (on `accent-soft`).
- **Cobalt fill** (`accent-fill`, with `accent-fg` text): the primary button, one per view. In
  light it is the accent itself; in dark it is a deeper cobalt with white text, because the pale
  dark accent as a fill outshone every other control.

### Neutral

- **Paper** (`bg` / `bg-dark`): the page and the app frame.
- **Sheet** (`surface` / `surface-dark`): cards, inputs, buttons, panels, the editor and terminal.
- **Well** (`sunken` / `sunken-dark`): inset areas, disabled fields, code chips, the graph's repo
  nodes.
- **Hairline** (`line` / `line-dark`): every border and divider.
- **Edge** (`line-strong` / `line-strong-dark`): the border of fields and the search box, and
  scrollbar thumbs; 3:1 against Sheet so a field's boundary can be seen (WCAG 1.4.11).
- **Ink** (`fg` / `fg-dark`) and **Pencil** (`muted` / `muted-dark`): text. Pencil is secondary
  text and keeps at least 4.5:1 on every surface it sits on.

### Status

- **Attention** (`tone-attention`, on `attention-soft`): waiting for you, review, permission and
  question cards, the dry-run banner.
- **Success** (`tone-success`): done, passed, added lines.
- **Danger** (`tone-danger`, on `danger-soft`): failed, denied, deleted lines, destructive actions.
- **Active** is the accent; **Neutral** is Pencil.

Panel-scoped tokens extend these: editor and diff colors and the syntax palette
(`panels/editor/editor.css`), the ANSI palette (`panels/terminal/terminal.css`) and the timeline's
series colors (`panels/graph/graph.css`). Each has a light and a dark value.

### Themes

Every color token is defined twice in the stylesheets: once on `:root` (light) and once on
`:root[data-theme="dark"]`. Nothing else decides the theme:

- `public/theme.js`, a blocking classic script in the page head, sets `data-theme` before the
  first paint from the stored choice (`calin.theme.v1` in local storage) or, without one, from
  `prefers-color-scheme`. The CSP allows no inline script, so it is a file.
- `src/lib/theme.ts` keeps it current: the choice made in Settings → Theme (System, Light, Dark)
  and system changes while the choice is System. It also sets the `theme-color` meta tags to
  `bg`, so the browser's bar matches.
- `color-scheme` follows `data-theme`, so native controls and scrollbars switch too.
- What paints with concrete colors reads the tokens and listens for `data-theme` changes
  (`watchTheme`): the xterm theme and CodeMirror's dark flag. The graph, the timeline, the usage
  chart, diffs and syntax highlighting use the tokens directly.

Dark is not an inversion: surfaces step up in lightness from `bg` to `surface` (nearer the
reader), `sunken` steps down, text is off-white, and each tone is lifted until it keeps 4.5:1 on
dark surfaces.

### Named Rules

**The Two Values Rule.** A color token without a dark value is a bug.

**The One Signal Rule.** Color means state. The accent marks where the user acts; the tones mark
status. Nothing is colored for decoration.

**The Word Beside The Dot Rule.** A status is a dot and a word, never color alone.

## Typography

**Body Font:** the system UI font (`system-ui`, then the platform's own).
**Mono Font:** the system monospace (`ui-monospace`, SF Mono, Cascadia Mono, Menlo, Consolas).

**Character:** native and unremarkable on purpose: the app reads like the platform it runs on, and
mono marks everything that came from a machine (paths, branches, commands, costs, ids).

### Hierarchy

- **Title** (600, 20 px): one per page: Settings, Usage, Automations, an automation, the
  error and not-found screens. A session's title in its header stays at body size (600, 15 px):
  the header is dense and the transcript is the page.
- **Section** (500–600, 14 px): headings inside a page and panel, labels of fields.
- **Body** (400, 15 px, line height 1.45): transcript prose, list rows.
- **Secondary** (400, 14 px): descriptions, table cells, panel text.
- **Label** (500, 12 px): badges, hints, metadata, timestamps.
- **Code** (13 px mono, line height 1.5): editor, diff, terminal (terminal size is its own).

### Named Rules

**The Machine Wrote It Rule.** Paths, branch names, commands, ids and amounts are mono; prose is
never mono.

## Layout

- Phone first: one column, 12 px gutters, the sessions list on top of the composer at home, a
  session full screen with its panels as tabs.
- From 1024 px (`lg`): a 320 px sessions sidebar; a session shows its transcript and one side panel
  side by side, the pane resizable. Forms and settings sit in a centered column of at most 672 px.
- Every interactive target is at least 44 px tall and wide (`min-h-11`, `.btn`).
- The safe areas of a phone are honored at the frame (`pt-safe`, `pb-safe`, `px-safe`).

## Elevation & Depth

Flat by default. Depth comes from the three tonal steps (bg, surface, sunken) and hairlines.
Shadows appear only on things that float over content: the graph's legend and details sheet, the
"jump to latest" button, the connection indicator.

### Named Rules

**The Flat-At-Rest Rule.** Nothing at rest casts a shadow; a shadow means "this floats above".

## Shapes

Gently rounded controls (6 px) for buttons, fields, cards and choice boxes; full pills for badges
and counts; square edges for panes and the frame. Borders are 1 px hairlines; a 2 px accent
outline with a 2 px offset is the one focus style.

## Components

### Buttons

- **Shape:** 6 px radius, at least 44 × 44 px, 1 px hairline border.
- **Default:** Sheet background, Ink text, 500 weight. Hover darkens the border to Pencil.
- **Primary:** Cobalt fill, white text, in both themes. One per view.
- **Pressed toggle:** `aria-pressed="true"` turns border and text Cobalt.
- **Disabled:** half opacity, not-allowed cursor. A disabled primary drops its fill: Well
  background, hairline border, Pencil text, so it never reads as live.

### Choices

A radio group drawn as a row or stack of boxes (`.choice`): 44 px tall, hairline border, 6 px
radius; the checked one gets a Cobalt border on `accent-soft`. Used for permission modes,
automation triggers and the theme.

### Badges

A pill with a hairline border on Sheet: a 6 px dot and a 12 px word in the status tone. Running,
preparing and waiting dots pulse unless reduced motion is asked for.

### Inputs / Fields

Sheet background, Edge border (3:1), 6 px radius, 16 px text, 44 px tall; Pencil placeholders;
disabled fields sink to Well. Errors are a sentence in Danger under the field, tied by
`aria-describedby`.

### Navigation

Text links in Cobalt in the sidebar header; panel tabs are a row of text tabs with a 2 px Cobalt
underline on the selected one and a count pill.

### Transcript cards

Tool calls are bordered Sheet rows with a mono kind chip, a mono summary and a status badge;
permission requests and questions are Attention cards with their actions on the right; the user's
messages sit on `accent-soft`.

## Do's and Don'ts

### Do:

- **Do** take every color from a token, with a light and a dark value.
- **Do** keep text at 4.5:1 or more on its surface in both themes.
- **Do** pair every status color with its word.
- **Do** keep targets at 44 px and form text at 16 px.
- **Do** use mono for machine-made strings and the system sans for everything else.

### Don't:

- **Don't** hard-code a color in a component, an SVG attribute or a JavaScript theme object.
- **Don't** add a second accent, gradients, or shadows on things at rest.
- **Don't** fetch fonts or scripts from a third party: the CSP allows the app's origin only.
- **Don't** animate without a state change behind it, and honor reduced motion.
