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
  if (name !== undefined) {
    const char = NAMED_ENTITIES[name];
    return char === undefined ? undefined : { char, length: whole.length };
  }
  const code = decimal !== undefined ? Number(decimal) : Number.parseInt(hex ?? "", 16);
  const valid = code > 0 && code <= 0x10ffff && (code < 0xd800 || code > 0xdfff);
  return { char: valid ? String.fromCodePoint(code) : "\ufffd", length: whole.length };
}

const ESCAPABLE = "\\`*_{}[]()<>#+-.!|~&\"'";
const SPECIAL = /[\\`*_[<&h]/g;
const URL_END = /[\s<>"`]/;

/** How many times `char` repeats from `at`. */
function runLength(text: string, at: number, char: string): number {
  let end = at;
  while (text[end] === char) end += 1;
  return end - at;
}

/** Remembers the answer to "where is the next … at or after here", so a scan is not walked twice. */
type Memo = { from: number; at: number };
const recall = (memo: Memo | undefined, from: number): number | undefined =>
  memo && from >= memo.from && (memo.at === -1 || memo.at >= from) ? memo.at : undefined;

/**
 * Reads the inline constructs of `text`. Never throws, whatever the text.
 * `inLink`: the text is a link's label, where another link cannot start.
 */
export function parseInline(text: string, depth = 0, inLink = false): Inline[] {
  const out: Inline[] = [];
  if (depth > MAX_INLINE_DEPTH) return text === "" ? out : [{ kind: "text", text }];
  let plain = "";
  const flush = (): void => {
    if (plain === "") return;
    out.push({ kind: "text", text: plain });
    plain = "";
  };
  const memos = new Map<string, Memo>();

  /** Start of the next run of backticks exactly `length` long, at or after `from`. */
  function closingTicks(from: number, length: number): number {
    const key = `\`${length}`;
    const known = recall(memos.get(key), from);
    if (known !== undefined) return known;
    let at = text.indexOf("`", from);
    while (at !== -1) {
      const run = runLength(text, at, "`");
      if (run === length) break;
      at = text.indexOf("`", at + run);
    }
    memos.set(key, { from, at });
    return at;
  }

  /**
   * The run of `char` that can close emphasis opened with `size` of them: not
   * after a space, and for `_` not inside a word. A lone `*` skips over `**`,
   * which belongs to bold text inside. Returns the run's start and length.
   */
  function closingRun(from: number, char: string, size: number): [number, number] | undefined {
    const key = `${char}${size}`;
    const known = recall(memos.get(key), from);
    if (known === -1) return undefined;
    let at = known ?? text.indexOf(char, from);
    while (at !== -1) {
      const run = runLength(text, at, char);
      const fits = size === 1 ? run !== 2 : run >= size;
      const closes = !isSpace(text[at - 1]) && (char === "*" || !isWord(text[at + run]));
      if (fits && closes) break;
      at = text.indexOf(char, at + run);
    }
    memos.set(key, { from, at });
    return at === -1 ? undefined : [at, runLength(text, at, char)];
  }

  /** Where `char` next occurs at or after `from`. */
  function nextOf(char: string, from: number): number {
    const known = recall(memos.get(char), from);
    if (known !== undefined) return known;
    const at = text.indexOf(char, from);
    memos.set(char, { from, at });
    return at;
  }

  /** `[label](address "title")` at `at`: the link and where it ends, when the address may be followed. */
  function linkAt(at: number): { node: Inline; end: number } | undefined {
    const close = nextOf("]", at + 1);
    if (close === -1 || text[close + 1] !== "(") return undefined;
    let cursor = close + 2;
    const limit = Math.min(text.length, cursor + MAX_URL);
    // Without a closing parenthesis in reach there is no link, and nothing to walk through.
    const paren = nextOf(")", cursor);
    if (paren === -1 || paren > limit) return undefined;
    let address: string;
    if (text[cursor] === "<") {
      const end = nextOf(">", cursor);
      if (end === -1 || end > limit) return undefined;
      address = text.slice(cursor + 1, end);
      cursor = end + 1;
    } else {
      const begin = cursor;
      let open = 0;
      while (cursor < limit && !isSpace(text[cursor])) {
        if (text[cursor] === "(") open += 1;
        else if (text[cursor] === ")") {
          if (open === 0) break;
          open -= 1;
        }
        cursor += 1;
      }
      address = text.slice(begin, cursor);
    }
    while (text[cursor] === " " || text[cursor] === "\t") cursor += 1;
    const quote = text[cursor];
    if (quote === '"' || quote === "'") {
      const end = nextOf(quote, cursor + 1);
      if (end === -1 || end > cursor + MAX_URL) return undefined;
      cursor = end + 1;
      while (text[cursor] === " " || text[cursor] === "\t") cursor += 1;
    }
    if (text[cursor] !== ")") return undefined;
    const href = safeHref(address);
    if (href === undefined) return undefined;
    const children = parseInline(text.slice(at + 1, close), depth + 1, true);
    return { node: { kind: "link", href, children }, end: cursor + 1 };
  }

  /** A bare `http://` or `https://` address at `at`, without the punctuation that ends the sentence. */
  function addressAt(at: number): { node: Inline; end: number } | undefined {
    if (!text.startsWith("http://", at) && !text.startsWith("https://", at)) return undefined;
    if (isWord(text[at - 1])) return undefined;
    let end = at;
    const limit = Math.min(text.length, at + MAX_URL);
    while (end < limit && !URL_END.test(text[end])) end += 1;
    while (end > at) {
      const last = text[end - 1];
      const unmatched =
        (last === ")" && !text.slice(at, end - 1).includes("(")) ||
        (last === "]" && !text.slice(at, end - 1).includes("["));
      if (!unmatched && !".,;:!?*_~'".includes(last)) break;
      end -= 1;
    }
    const written = text.slice(at, end);
    const href = safeHref(written);
    if (href === undefined) return undefined;
    return { node: { kind: "link", href, children: [{ kind: "text", text: written }] }, end };
  }

  let index = 0;
  while (index < text.length) {
    SPECIAL.lastIndex = index;
    const found = SPECIAL.exec(text);
    if (!found) {
      plain += text.slice(index);
      break;
    }
    const at = found.index;
    plain += text.slice(index, at);
    const char = text[at];
    index = at + 1;

    if (char === "\\") {
      const next = text[at + 1];
      if (next !== undefined && ESCAPABLE.includes(next)) {
        plain += next;
        index = at + 2;
      } else plain += char;
    } else if (char === "&") {
      const entity = entityAt(text.slice(at, at + 12));
      plain += entity ? entity.char : char;
      if (entity) index = at + entity.length;
    } else if (char === "`") {
      const run = runLength(text, at, char);
      const close = closingTicks(at + run, run);
      if (close === -1) {
        plain += text.slice(at, at + run);
        index = at + run;
      } else {
        let code = text.slice(at + run, close).replace(/\n/g, " ");
        // One space of padding lets a span start or end with a backtick.
        if (code.length > 2 && code.startsWith(" ") && code.endsWith(" ") && code.trim() !== "") {
          code = code.slice(1, -1);
        }
        flush();
        out.push({ kind: "code", text: code });
        index = close + run;
      }
    } else if (char === "*" || char === "_") {
      const run = runLength(text, at, char);
      const opens = !isSpace(text[at + run]) && (char === "*" || !isWord(text[at - 1]));
      // `***` is bold and italic; when nothing closes all three, it is bold around a `*`.
      const sizes = run >= 3 ? [3, 2] : [run];
      let made = false;
      for (const size of opens ? sizes : []) {
        const closer = closingRun(at + run, char, size);
        if (!closer) continue;
        const [start, length] = closer;
        const inner = parseInline(text.slice(at + size, start + length - size), depth + 1, inLink);
        const node: Inline =
          size === 1
            ? { kind: "em", children: inner }
            : size === 2
              ? { kind: "strong", children: inner }
              : { kind: "strong", children: [{ kind: "em", children: inner }] };
        flush();
        out.push(node);
        index = start + length;
        made = true;
        break;
      }
      if (!made) {
        plain += text.slice(at, at + run);
        index = at + run;
      }
    } else if (inLink) {
      plain += char;
    } else if (char === "[") {
      const link = linkAt(at);
      if (link) {
        flush();
        out.push(link.node);
        index = link.end;
      } else plain += char;
    } else if (char === "<") {
      // `<https://…>`; any other `<` is text, tags included.
      const close = text.startsWith("<http", at) ? nextOf(">", at) : -1;
      const written = close === -1 || close > at + MAX_URL ? "" : text.slice(at + 1, close);
      const href = /\s/.test(written) ? undefined : safeHref(written);
      if (href !== undefined) {
        flush();
        out.push({ kind: "link", href, children: [{ kind: "text", text: written }] });
        index = close + 1;
      } else plain += char;
    } else {
      const address = addressAt(at);
      if (address) {
        flush();
        out.push(address.node);
        index = address.end;
      } else plain += char;
    }
  }
  flush();
  return out;
}

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
