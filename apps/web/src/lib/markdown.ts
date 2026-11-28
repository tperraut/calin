// A small Markdown reader for agent messages. It produces a tree of plain
// data; `components/Markdown.tsx` turns that into DOM nodes. Nothing in the
// source is ever treated as HTML: tags are text, like everything else.
//
// What it knows: paragraphs, ATX headings, fenced code, bullet and numbered
// lists (nested by indentation), rules, inline code, bold, italics, links and
// bare http(s) addresses. Whatever else Markdown has shows as written.
//
// It runs on every streamed chunk, over text an agent chose, so every scan is
// bounded: a search that fails once is not repeated, and nesting has a limit.

export type Inline =
  | { kind: "text"; text: string }
  | { kind: "code"; text: string }
  | { kind: "strong"; children: Inline[] }
  | { kind: "em"; children: Inline[] }
  /** `href` is always an absolute http(s) address. */
  | { kind: "link"; href: string; children: Inline[] };

export type ListItem = { content: Inline[]; lists: ListBlock[] };

export type ListBlock = {
  kind: "list";
  raw: string;
  ordered: boolean;
  /** The number the first item carries in the source. */
  start: number;
  items: ListItem[];
};

/** `raw` is the source the block was read from: two blocks with the same `raw` render the same. */
export type Block =
  | { kind: "paragraph"; raw: string; content: Inline[] }
  | { kind: "heading"; raw: string; level: number; content: Inline[] }
  /** `closed` is false while the closing fence has not arrived: the text so far is still code. */
  | { kind: "code"; raw: string; text: string; lang: string; closed: boolean }
  | { kind: "rule"; raw: string }
  | ListBlock;

const MAX_INLINE_DEPTH = 6;
const MAX_LIST_DEPTH = 6;
/** Longest link destination or bare address looked at. */
const MAX_URL = 2048;

const isSpace = (char: string | undefined): boolean =>
  char === undefined || char === " " || char === "\n" || char === "\t" || char === "\r";

const isWord = (char: string | undefined): boolean =>
  char !== undefined && /[\p{L}\p{N}]/u.test(char);

/** The address as the browser would follow it, when it is plain http or https; nothing else is linked. */
export function safeHref(address: string): string | undefined {
  const trimmed = address.trim();
  if (!/^https?:\/\//i.test(trimmed)) return undefined;
  try {
    const url = new URL(trimmed);
    return url.protocol === "http:" || url.protocol === "https:" ? url.href : undefined;
  } catch {
    return undefined;
  }
}

const NAMED_ENTITIES: Readonly<Record<string, string>> = {
  amp: "&",
  lt: "<",
  gt: ">",
  quot: '"',
  apos: "'",
  nbsp: "\u00a0",
};

const ENTITY = /^&(?:#(\d{1,7})|#[xX]([0-9a-fA-F]{1,6})|([a-z]{2,4}));/;

/** The character an entity at the start of `text` stands for, and how long the entity is. */
function entityAt(text: string): { char: string; length: number } | undefined {
  const match = ENTITY.exec(text);
  if (!match) return undefined;
  const [whole, decimal, hex, name] = match;
  const char = name === undefined ? numericChar(decimal, hex) : NAMED_ENTITIES[name];
  return char === undefined ? undefined : { char, length: whole.length };
}

/** The character a numeric entity names; U+FFFD for a code point no text may hold. */
function numericChar(decimal: string | undefined, hex: string | undefined): string {
  const code = hex === undefined ? Number(decimal) : Number.parseInt(hex, 16);
  const valid = code > 0 && code <= 0x10ffff && (code < 0xd800 || code > 0xdfff);
  return valid ? String.fromCodePoint(code) : "\ufffd";
}

const ESCAPABLE = "\\`*_{}[]()<>#+-.!|~&\"'";
const SPECIAL = /[\\`*_[<&h]/g;
const URL_END = /[\s<>"`]/;
/** What ends the sentence after a bare address rather than the address. */
const TRAILING = ".,;:!?*_~'";

/** How many times `char` repeats from `at`. */
function runLength(text: string, at: number, char: string): number {
  let end = at;
  while (text[end] === char) end += 1;
  return end - at;
}

/** Start of the first run of `char` at or after `from` that `fits`, or -1. */
function runWhere(
  text: string,
  char: string,
  from: number,
  fits: (at: number, run: number) => boolean,
): number {
  let at = text.indexOf(char, from);
  while (at !== -1) {
    const run = runLength(text, at, char);
    if (fits(at, run)) return at;
    at = text.indexOf(char, at + run);
  }
  return -1;
}

/** Remembers the answer to "where is the next … at or after here", so a scan is not walked twice. */
type Memo = { from: number; at: number };
const recall = (memo: Memo | undefined, from: number): number | undefined =>
  memo && from >= memo.from && (memo.at === -1 || memo.at >= from) ? memo.at : undefined;

/** One `parseInline` call: its arguments, and the answers its searches found. */
type Scan = { text: string; depth: number; inLink: boolean; memos: Map<string, Memo> };

/** What a special character reads as, a node or text as written, and where reading goes on. */
type Step = { node: Inline; end: number } | { plain: string; end: number };

const plainStep = (text: string, at: number, end: number): Step => ({
  plain: text.slice(at, end),
  end,
});

/** `search(from)`, unless an earlier answer under `key` already holds for `from`. */
function remembered(
  scan: Scan,
  key: string,
  from: number,
  search: (from: number) => number,
): number {
  const known = recall(scan.memos.get(key), from);
  if (known !== undefined) return known;
  const at = search(from);
  scan.memos.set(key, { from, at });
  return at;
}

/** Where `char` next occurs at or after `from`. */
const nextOf = (scan: Scan, char: string, from: number): number =>
  remembered(scan, char, from, (start) => scan.text.indexOf(char, start));

/**
 * Reads the inline constructs of `text`. Never throws, whatever the text.
 * `inLink`: the text is a link's label, where another link cannot start.
 */
export function parseInline(text: string, depth = 0, inLink = false): Inline[] {
  if (depth > MAX_INLINE_DEPTH) return text === "" ? [] : [{ kind: "text", text }];
  const scan: Scan = { text, depth, inLink, memos: new Map() };
  const out: Inline[] = [];
  let index = 0;
  while (index < text.length) {
    SPECIAL.lastIndex = index;
    const at = SPECIAL.exec(text)?.index ?? text.length;
    appendText(out, text.slice(index, at));
    if (at === text.length) break;
    const step = stepAt(scan, at);
    if ("node" in step) out.push(step.node);
    else appendText(out, step.plain);
    index = step.end;
  }
  return out;
}

/** Adds `text` to the end of `out`, joined to the text already last there. */
function appendText(out: Inline[], text: string): void {
  if (text === "") return;
  const last = out.at(-1);
  if (last?.kind === "text") last.text += text;
  else out.push({ kind: "text", text });
}

/** Reads the construct that the special character at `at` starts. */
function stepAt(scan: Scan, at: number): Step {
  const char = scan.text[at];
  if (char === "\\") return escapeAt(scan.text, at);
  if (char === "&") return entityStepAt(scan.text, at);
  if (char === "`") return codeSpanAt(scan, at);
  if (char === "*" || char === "_") return emphasisAt(scan, at);
  const link = scan.inLink ? undefined : linkLikeAt(scan, at);
  return link ?? plainStep(scan.text, at, at + 1);
}

/** `\` keeps the punctuation after it as text; before anything else it is text itself. */
function escapeAt(text: string, at: number): Step {
  const next = text[at + 1];
  return next !== undefined && ESCAPABLE.includes(next)
    ? { plain: next, end: at + 2 }
    : plainStep(text, at, at + 1);
}

function entityStepAt(text: string, at: number): Step {
  const entity = entityAt(text.slice(at, at + 12));
  return entity ? { plain: entity.char, end: at + entity.length } : plainStep(text, at, at + 1);
}

/** Code from a run of backticks to the next run exactly as long; without one, the run is text. */
function codeSpanAt(scan: Scan, at: number): Step {
  const run = runLength(scan.text, at, "`");
  const close = closingTicks(scan, at + run, run);
  if (close === -1) return plainStep(scan.text, at, at + run);
  const text = codeText(scan.text.slice(at + run, close));
  return { node: { kind: "code", text }, end: close + run };
}

/** Start of the next run of backticks exactly `length` long, at or after `from`. */
const closingTicks = (scan: Scan, from: number, length: number): number =>
  remembered(scan, `\`${length}`, from, (start) =>
    runWhere(scan.text, "`", start, (_at, run) => run === length),
  );

function codeText(written: string): string {
  const code = written.replace(/\n/g, " ");
  // One space of padding lets a span start or end with a backtick.
  const padded =
    code.length > 2 && code.startsWith(" ") && code.endsWith(" ") && code.trim() !== "";
  return padded ? code.slice(1, -1) : code;
}

function emphasisAt(scan: Scan, at: number): Step {
  const { text } = scan;
  const char = text[at];
  const run = runLength(text, at, char);
  if (!opensEmphasis(text, at, run, char)) return plainStep(text, at, at + run);
  // `***` is bold and italic; when nothing closes all three, it is bold around a `*`.
  for (const size of run >= 3 ? [3, 2] : [run]) {
    const closer = closingRun(scan, at + run, char, size);
    if (closer) return emphasisNode(scan, at, size, closer);
  }
  return plainStep(text, at, at + run);
}

/** A run of `run` × `char` at `at` opens emphasis: not before a space, and `_` not inside a word. */
const opensEmphasis = (text: string, at: number, run: number, char: string): boolean =>
  !isSpace(text[at + run]) && (char === "*" || !isWord(text[at - 1]));

/**
 * The run of `char` that can close emphasis opened with `size` of them: not
 * after a space, and for `_` not inside a word. A lone `*` skips over `**`,
 * which belongs to bold text inside. Returns the run's start and length.
 */
function closingRun(
  scan: Scan,
  from: number,
  char: string,
  size: number,
): [number, number] | undefined {
  const { text } = scan;
  const at = remembered(scan, `${char}${size}`, from, (start) =>
    runWhere(text, char, start, (run, length) => closesEmphasis(text, run, length, char, size)),
  );
  return at === -1 ? undefined : [at, runLength(text, at, char)];
}

function closesEmphasis(
  text: string,
  at: number,
  run: number,
  char: string,
  size: number,
): boolean {
  const fits = size === 1 ? run !== 2 : run >= size;
  return fits && !isSpace(text[at - 1]) && (char === "*" || !isWord(text[at + run]));
}

function emphasisNode(scan: Scan, at: number, size: number, closer: [number, number]): Step {
  const [start, length] = closer;
  const inner = parseInline(
    scan.text.slice(at + size, start + length - size),
    scan.depth + 1,
    scan.inLink,
  );
  return { node: emphasis(size, inner), end: start + length };
}

const emphasis = (size: number, children: Inline[]): Inline =>
  size === 1
    ? { kind: "em", children }
    : size === 2
      ? { kind: "strong", children }
      : { kind: "strong", children: [{ kind: "em", children }] };

/** A link or an address starting at `at`, when one does. */
function linkLikeAt(scan: Scan, at: number): Step | undefined {
  const char = scan.text[at];
  if (char === "[") return linkAt(scan, at);
  if (char === "<") return angleAddressAt(scan, at);
  return addressAt(scan.text, at);
}

/** `[label](address "title")` at `at`, when the address may be followed. */
function linkAt(scan: Scan, at: number): Step | undefined {
  const close = nextOf(scan, "]", at + 1);
  if (close === -1 || scan.text[close + 1] !== "(") return undefined;
  const target = linkTargetAt(scan, close + 2);
  if (!target) return undefined;
  const children = parseInline(scan.text.slice(at + 1, close), scan.depth + 1, true);
  return { node: { kind: "link", href: target.href, children }, end: target.end };
}

/** `address "title")` from `start`: where the link goes and where it ends. */
function linkTargetAt(scan: Scan, start: number): { href: string; end: number } | undefined {
  const limit = Math.min(scan.text.length, start + MAX_URL);
  // Without a closing parenthesis in reach there is no link, and nothing to walk through.
  const paren = nextOf(scan, ")", start);
  if (paren === -1 || paren > limit) return undefined;
  const destination = destinationAt(scan, start, limit);
  if (!destination) return undefined;
  const end = titleEnd(scan, skipBlanks(scan.text, destination.end));
  if (end === undefined || scan.text[end] !== ")") return undefined;
  const href = safeHref(destination.address);
  return href === undefined ? undefined : { href, end: end + 1 };
}

/** `<address>` or an address up to a space or its unmatched `)`, and where it ends. */
function destinationAt(
  scan: Scan,
  start: number,
  limit: number,
): { address: string; end: number } | undefined {
  if (scan.text[start] !== "<") {
    const end = bareDestinationEnd(scan.text, start, limit);
    return { address: scan.text.slice(start, end), end };
  }
  const end = nextOf(scan, ">", start);
  if (end === -1 || end > limit) return undefined;
  return { address: scan.text.slice(start + 1, end), end: end + 1 };
}

function bareDestinationEnd(text: string, start: number, limit: number): number {
  let cursor = start;
  let open = 0;
  while (cursor < limit && !isSpace(text[cursor])) {
    if (text[cursor] === "(") open += 1;
    else if (text[cursor] === ")") {
      if (open === 0) break;
      open -= 1;
    }
    cursor += 1;
  }
  return cursor;
}

/** Past an optional quoted title at `from` and the blanks after it; undefined when it never ends. */
function titleEnd(scan: Scan, from: number): number | undefined {
  const quote = scan.text[from];
  if (quote !== '"' && quote !== "'") return from;
  const end = nextOf(scan, quote, from + 1);
  if (end === -1 || end > from + MAX_URL) return undefined;
  return skipBlanks(scan.text, end + 1);
}

function skipBlanks(text: string, from: number): number {
  let at = from;
  while (text[at] === " " || text[at] === "\t") at += 1;
  return at;
}

/** `<https://…>`; any other `<` is text, tags included. */
function angleAddressAt(scan: Scan, at: number): Step | undefined {
  const close = scan.text.startsWith("<http", at) ? nextOf(scan, ">", at) : -1;
  if (close === -1 || close > at + MAX_URL) return undefined;
  const written = scan.text.slice(at + 1, close);
  const href = /\s/.test(written) ? undefined : safeHref(written);
  return href === undefined ? undefined : addressLink(href, written, close + 1);
}

/** A bare `http://` or `https://` address at `at`, without the punctuation that ends the sentence. */
function addressAt(text: string, at: number): Step | undefined {
  if (!text.startsWith("http://", at) && !text.startsWith("https://", at)) return undefined;
  if (isWord(text[at - 1])) return undefined;
  const written = text.slice(at, addressEnd(text, at));
  const href = safeHref(written);
  return href === undefined ? undefined : addressLink(href, written, at + written.length);
}

function addressEnd(text: string, at: number): number {
  let end = at;
  const limit = Math.min(text.length, at + MAX_URL);
  while (end < limit && !URL_END.test(text[end])) end += 1;
  while (end > at && trailsAddress(text, at, end)) end -= 1;
  return end;
}

/** The character before `end` follows the address rather than belonging to it. */
function trailsAddress(text: string, at: number, end: number): boolean {
  const last = text[end - 1];
  const unmatched =
    (last === ")" && !text.slice(at, end - 1).includes("(")) ||
    (last === "]" && !text.slice(at, end - 1).includes("["));
  return unmatched || TRAILING.includes(last);
}

/** An address shown as it was written, linked to where it goes. */
const addressLink = (href: string, written: string, end: number): Step => ({
  node: { kind: "link", href, children: [{ kind: "text", text: written }] },
  end,
});

const indentOf = (line: string): number => {
  let width = 0;
  for (const char of line) {
    if (char === " ") width += 1;
    else if (char === "\t") width += 4;
    else break;
  }
  return width;
};

type Fence = { indent: number; char: string; length: number; info: string };

function fenceAt(line: string): Fence | undefined {
  const start = line.length - line.trimStart().length;
  const char = line[start];
  if (char !== "`" && char !== "~") return undefined;
  const length = runLength(line, start, char);
  if (length < 3) return undefined;
  const info = line.slice(start + length).trim();
  // A backtick after the opening run means inline code, not a fence.
  if (char === "`" && info.includes("`")) return undefined;
  return { indent: start, char, length, info };
}

const closesFence = (line: string, fence: Fence): boolean => {
  const trimmed = line.trim();
  return trimmed.length >= fence.length && runLength(trimmed, 0, fence.char) === trimmed.length;
};

const HEADING = /^ {0,3}(#{1,6})[ \t]+(?=\S)/;

function headingAt(line: string): { level: number; text: string } | undefined {
  const match = HEADING.exec(line);
  if (!match) return undefined;
  let text = line.slice(match[0].length).trimEnd();
  // A closing run of `#` set off by a space is decoration.
  let end = text.length;
  while (text[end - 1] === "#") end -= 1;
  if (end < text.length && (end === 0 || text[end - 1] === " " || text[end - 1] === "\t")) {
    text = text.slice(0, end).trimEnd();
  }
  return { level: match[1].length, text };
}

function isRule(line: string): boolean {
  if (indentOf(line) > 3) return false;
  const marks = line.replace(/[ \t]/g, "");
  const char = marks[0];
  if (marks.length < 3 || (char !== "-" && char !== "*" && char !== "_")) return false;
  return runLength(marks, 0, char) === marks.length;
}

const LIST_ITEM = /^[ \t]*(?:([-*+])|(\d{1,9})[.)])(?:[ \t]+|$)/;

type Bullet = { indent: number; ordered: boolean; start: number; text: string };

function bulletAt(line: string): Bullet | undefined {
  const match = LIST_ITEM.exec(line);
  if (!match) return undefined;
  return {
    indent: indentOf(line),
    ordered: match[2] !== undefined,
    start: match[2] === undefined ? 1 : Number(match[2]),
    text: line.slice(match[0].length),
  };
}

/** Nests the bullets of one list by their indentation. */
function buildList(bullets: readonly Bullet[], raw: string): ListBlock {
  const [first] = bullets;
  const root: ListBlock = {
    kind: "list",
    raw,
    ordered: first.ordered,
    start: first.start,
    items: [],
  };
  const open = [{ indent: first.indent, list: root }];
  for (const bullet of bullets) {
    while (open.length > 1 && bullet.indent < open[open.length - 1].indent) open.pop();
    let level = open[open.length - 1];
    const parent = level.list.items.at(-1);
    if (bullet.indent >= level.indent + 2 && parent && open.length < MAX_LIST_DEPTH) {
      const nested: ListBlock = {
        kind: "list",
        raw: "",
        ordered: bullet.ordered,
        start: bullet.start,
        items: [],
      };
      parent.lists.push(nested);
      level = { indent: bullet.indent, list: nested };
      open.push(level);
    }
    level.list.items.push({ content: parseInline(bullet.text), lists: [] });
  }
  return root;
}

/** Reads the blocks of `source`. Incomplete text reads as what it is so far; nothing throws. */
export function parseMarkdown(source: string): Block[] {
  const lines = source.replace(/\r\n?/g, "\n").split("\n");
  const blocks: Block[] = [];
  const startsBlock = (line: string): boolean =>
    fenceAt(line) !== undefined || headingAt(line) !== undefined || isRule(line);
  let index = 0;
  while (index < lines.length) {
    const line = lines[index];
    if (line.trim() === "") {
      index += 1;
      continue;
    }
    const from = index;
    const raw = (): string => lines.slice(from, index).join("\n");

    const fence = fenceAt(line);
    if (fence) {
      index += 1;
      const body: string[] = [];
      let closed = false;
      while (index < lines.length) {
        const next = lines[index];
        index += 1;
        if (closesFence(next, fence)) {
          closed = true;
          break;
        }
        // Code inside an indented fence is indented with it.
        body.push(next.slice(Math.min(fence.indent, next.length - next.trimStart().length)));
      }
      blocks.push({
        kind: "code",
        raw: raw(),
        text: body.join("\n"),
        lang: fence.info.split(/\s+/)[0] ?? "",
        closed,
      });
      continue;
    }

    const heading = headingAt(line);
    if (heading) {
      index += 1;
      blocks.push({
        kind: "heading",
        raw: raw(),
        level: heading.level,
        content: parseInline(heading.text),
      });
      continue;
    }

    if (isRule(line)) {
      index += 1;
      blocks.push({ kind: "rule", raw: raw() });
      continue;
    }

    const first = bulletAt(line);
    if (first) {
      const bullets: Bullet[] = [];
      while (index < lines.length) {
        const next = lines[index];
        if (next.trim() === "") {
          // A blank line between items is spacing; before anything else it ends the list.
          let after = index + 1;
          while (after < lines.length && lines[after].trim() === "") after += 1;
          if (after >= lines.length || isRule(lines[after]) || !bulletAt(lines[after])) break;
          index = after;
          continue;
        }
        if (bullets.length > 0 && startsBlock(next)) break;
        const bullet = bulletAt(next);
        if (!bullet) {
          // A line that is not a bullet continues the item above it.
          const last = bullets[bullets.length - 1];
          last.text += `\n${next.trim()}`;
        } else if (
          bullets.length > 0 &&
          bullet.ordered !== first.ordered &&
          bullet.indent < first.indent + 2
        ) {
          // Bullets after numbers at the same level are another list.
          break;
        } else bullets.push(bullet);
        index += 1;
      }
      blocks.push(buildList(bullets, raw()));
      continue;
    }

    index += 1;
    while (index < lines.length) {
      const next = lines[index];
      if (next.trim() === "" || startsBlock(next) || bulletAt(next)) break;
      index += 1;
    }
    const text = raw();
    blocks.push({ kind: "paragraph", raw: text, content: parseInline(text) });
  }
  return blocks;
}

/**
 * `next`, with the blocks of `previous` kept wherever they read the same.
 * While a message streams only its last block changes, so only that one is
 * drawn again.
 */
export function reuseBlocks(previous: readonly Block[] | undefined, next: Block[]): Block[] {
  if (!previous) return next;
  return next.map((block, index) => {
    const before = previous[index];
    return before && before.kind === block.kind && before.raw === block.raw ? before : block;
  });
}
