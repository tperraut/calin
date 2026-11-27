import { describe, expect, it } from "vite-plus/test";
import {
  parseInline,
  parseMarkdown,
  reuseBlocks,
  safeHref,
  type Block,
  type Inline,
  type ListBlock,
} from "./markdown";

/** Inlines in a compact notation: `<b>`, `<i>`, `<c>` for code, `<a href>`; text as it is. */
const inl = (nodes: readonly Inline[]): string =>
  nodes
    .map((node) => {
      switch (node.kind) {
        case "text":
          return node.text;
        case "code":
          return `<c>${node.text}</c>`;
        case "strong":
          return `<b>${inl(node.children)}</b>`;
        case "em":
          return `<i>${inl(node.children)}</i>`;
        case "link":
          return `<a ${node.href}>${inl(node.children)}</a>`;
      }
    })
    .join("");

const listOf = (list: ListBlock): string =>
  `${list.ordered ? `ol${list.start}` : "ul"}[${list.items
    .map((item) => inl(item.content) + item.lists.map(listOf).join(""))
    .join("|")}]`;

const block = (one: Block): string => {
  switch (one.kind) {
    case "paragraph":
      return `p:${inl(one.content)}`;
    case "heading":
      return `h${one.level}:${inl(one.content)}`;
    case "code":
      return `code(${one.lang})${one.closed ? "" : "…"}:${one.text}`;
    case "rule":
      return "hr";
    case "list":
      return listOf(one);
  }
};

const inline = (source: string): string => inl(parseInline(source));
const blocks = (source: string): string[] => parseMarkdown(source).map(block);

describe("inline Markdown", () => {
  it.each([
    ["plain text stays", "plain text stays"],
    ["", ""],
    // Code.
    ["run `cargo test` now", "run <c>cargo test</c> now"],
    ["``a ` b``", "<c>a ` b</c>"],
    ["`` `tick` ``", "<c>`tick`</c>"],
    ["`*not bold*`", "<c>*not bold*</c>"],
    ["`<b>&amp;</b>`", "<c><b>&amp;</b></c>"],
    ["a `line\nbreak` b", "a <c>line break</c> b"],
    // Bold and italics.
    ["**bold** and *italic*", "<b>bold</b> and <i>italic</i>"],
    ["__bold__ and _italic_", "<b>bold</b> and <i>italic</i>"],
    ["***both***", "<b><i>both</i></b>"],
    ["**bold with *italic* inside**", "<b>bold with <i>italic</i> inside</b>"],
    ["*italic with **bold** inside*", "<i>italic with <b>bold</b> inside</i>"],
    ["***a* b**", "<b><i>a</i> b</b>"],
    ["**`code` in bold**", "<b><c>code</c> in bold</b>"],
    // What is not emphasis.
    ["2 * 3 * 4", "2 * 3 * 4"],
    ["snake_case_name and __init__.py", "snake_case_name and <b>init</b>.py"],
    ["a_b_c", "a_b_c"],
    ["src/*.ts and *.md", "src/*.ts and *.md"],
    ["a ** b ** c", "a ** b ** c"],
    ["\\*not italic\\*", "*not italic*"],
    ["\\`not code\\`", "`not code`"],
    ["a \\ b", "a \\ b"],
    // Links.
    ["[docs](https://example.com/a)", "<a https://example.com/a>docs</a>"],
    ["[docs](http://example.com)", "<a http://example.com/>docs</a>"],
    ['[t](https://example.com "A title")', "<a https://example.com/>t</a>"],
    ["[t](<https://example.com/a b>)", "<a https://example.com/a%20b>t</a>"],
    ["[**bold** link](https://example.com)", "<a https://example.com/><b>bold</b> link</a>"],
    [
      "[wiki](https://en.wikipedia.org/wiki/Rust_(language))",
      "<a https://en.wikipedia.org/wiki/Rust_(language)>wiki</a>",
    ],
    ["see https://example.com/x.", "see <a https://example.com/x>https://example.com/x</a>."],
    ["(https://example.com)", "(<a https://example.com/>https://example.com</a>)"],
    ["<https://example.com/a>", "<a https://example.com/a>https://example.com/a</a>"],
    // A link's label holds no other link.
    ["[https://a.example](https://b.example)", "<a https://b.example/>https://a.example</a>"],
    // Not links: only http and https are followed.
    ["[x](javascript:alert(1))", "[x](javascript:alert(1))"],
    ["[x](JaVaScRiPt:alert(1))", "[x](JaVaScRiPt:alert(1))"],
    ["[x]( javascript:alert(1))", "[x]( javascript:alert(1))"],
    [
      "[x](data:text/html,<script>alert(1)</script>)",
      "[x](data:text/html,<script>alert(1)</script>)",
    ],
    ["[x](vbscript:msgbox)", "[x](vbscript:msgbox)"],
    ["[x](file:///etc/passwd)", "[x](file:///etc/passwd)"],
    ["[x](/relative/path)", "[x](/relative/path)"],
    ["[x](//evil.example)", "[x](//evil.example)"],
    ["[x](mailto:a@b.c)", "[x](mailto:a@b.c)"],
    ["<javascript:alert(1)>", "<javascript:alert(1)>"],
    ["xhttps://example.com", "xhttps://example.com"],
    ["[x] (https://example.com)", "[x] (<a https://example.com/>https://example.com</a>)"],
    // HTML is text.
    ["<script>alert(1)</script>", "<script>alert(1)</script>"],
    ['<img src=x onerror="alert(1)">', '<img src=x onerror="alert(1)">'],
    ["<b>bold?</b>", "<b>bold?</b>"],
    ["<!-- comment -->", "<!-- comment -->"],
    // Entities become the characters they stand for, as text.
    ["&lt;script&gt; &amp; &quot;x&quot; &apos;y&apos;", "<script> & \"x\" 'y'"],
    ["&#60;b&#62; &#x3C;i&#x3e;", "<b> <i>"],
    ["&#0; &#xD800; &#1114112;", "� � �"],
    ["&nope; &amp &; AT&T", "&nope; &amp &; AT&T"],
    ["a&nbsp;b", "a b"],
    // Unclosed constructs show as written.
    ["`unclosed code", "`unclosed code"],
    ["**unclosed bold", "**unclosed bold"],
    ["*unclosed italic", "*unclosed italic"],
    ["[unclosed link", "[unclosed link"],
    ["[label](https://example.com", "[label](<a https://example.com/>https://example.com</a>"],
    ["[label](", "[label]("],
    ["[label]", "[label]"],
    ["![image](https://example.com/a.png)", "!<a https://example.com/a.png>image</a>"],
  ])("%j", (source, expected) => {
    expect(inline(source)).toBe(expected);
  });

  it("links only what a browser would follow as http or https", () => {
    expect(safeHref("https://example.com/a?b=c#d")).toBe("https://example.com/a?b=c#d");
    expect(safeHref("  HTTP://EXAMPLE.com  ")).toBe("http://example.com/");
    for (const address of [
      "javascript:alert(1)",
      "java\nscript:alert(1)",
      "\u0001javascript:alert(1)",
      "data:text/html,x",
      "ftp://example.com",
      "https://",
      "http://exa mple.com",
      "//example.com",
      "example.com",
      "",
    ]) {
      expect(safeHref(address), address).toBeUndefined();
    }
  });

  it("stops nesting at a fixed depth instead of following the text all the way down", () => {
    const deep = `${"*a _b ".repeat(40)}x${" b_ a*".repeat(40)}`;
    const depth = (nodes: readonly Inline[]): number =>
      Math.max(0, ...nodes.map((node) => ("children" in node ? 1 + depth(node.children) : 0)));
    expect(depth(parseInline(deep))).toBeLessThanOrEqual(8);
    // Everything is still there, as text.
    expect(inline(deep).replace(/<\/?[bi]>/g, "")).toContain("x");
  });
});

describe("Markdown blocks", () => {
  it.each([
    ["", []],
    ["\n\n  \n", []],
    ["One line.", ["p:One line."]],
    ["Two lines\nof one paragraph.", ["p:Two lines\nof one paragraph."]],
    ["First.\n\nSecond.\n\n\nThird.", ["p:First.", "p:Second.", "p:Third."]],
    ["Windows\r\nline ends.\r\n\r\nNext.", ["p:Windows\nline ends.", "p:Next."]],
    // Headings.
    ["# Title", ["h1:Title"]],
    ["### Third level ###", ["h3:Third level"]],
    ["###### Six", ["h6:Six"]],
    ["####### Seven is text", ["p:####### Seven is text"]],
    ["#hashtag", ["p:#hashtag"]],
    ["#", ["p:#"]],
    ["# C# and F#", ["h1:C# and F#"]],
    ["## With `code` and **bold**", ["h2:With <c>code</c> and <b>bold</b>"]],
    ["Text\n## Heading\nMore", ["p:Text", "h2:Heading", "p:More"]],
    // Code.
    ["```\nlet a = 1;\n```", ["code():let a = 1;"]],
    ["```rust\nfn main() {}\n```", ["code(rust):fn main() {}"]],
    ["```ts title=x\n1\n```", ["code(ts):1"]],
    ["~~~\ntilde\n~~~", ["code():tilde"]],
    ["````\n```\ninner\n```\n````", ["code():```\ninner\n```"]],
    ["```\n**not bold** <b>&amp;</b>\n```", ["code():**not bold** <b>&amp;</b>"]],
    ["```\n\n  indented\n\n```", ["code():\n  indented\n"]],
    ["Before\n```\ncode\n```\nAfter", ["p:Before", "code():code", "p:After"]],
    ["  ```\n  in\n    deeper\n  ```", ["code():in\n  deeper"]],
    ["```inline``` is not a fence", ["p:<c>inline</c> is not a fence"]],
    // Lists.
    ["- one\n- two", ["ul[one|two]"]],
    ["* one\n+ two", ["ul[one|two]"]],
    ["1. one\n2. two", ["ol1[one|two]"]],
    ["3) three\n4) four", ["ol3[three|four]"]],
    ["- one\n\n- two\n\n- three", ["ul[one|two|three]"]],
    ["- one\n  continued\n- two", ["ul[one\ncontinued|two]"]],
    ["- a\n  - b\n  - c\n- d", ["ul[aul[b|c]|d]"]],
    ["1. a\n   - b\n2. c", ["ol1[aul[b]|c]"]],
    ["- a\n    1. b\n        - c", ["ul[aol1[bul[c]]]"]],
    ["- **bold** and `code`", ["ul[<b>bold</b> and <c>code</c>]"]],
    ["Intro:\n- one\n- two\n\nOutro.", ["p:Intro:", "ul[one|two]", "p:Outro."]],
    ["- a\n1. b", ["ul[a]", "ol1[b]"]],
    ["1. a\n\n```\ncode\n```\n\n2. b", ["ol1[a]", "code():code", "ol2[b]"]],
    ["-not a list", ["p:-not a list"]],
    ["1.not a list", ["p:1.not a list"]],
    ["**bold** start", ["p:<b>bold</b> start"]],
    ["*italic* start", ["p:<i>italic</i> start"]],
    // Rules.
    ["---", ["hr"]],
    ["***", ["hr"]],
    ["_ _ _", ["hr"]],
    ["above\n\n---\n\nbelow", ["p:above", "hr", "p:below"]],
    ["--", ["p:--"]],
    // What this reader does not know shows as written.
    ["> quoted", ["p:> quoted"]],
    ["| a | b |\n|---|---|\n| 1 | 2 |", ["p:| a | b |\n|---|---|\n| 1 | 2 |"]],
    ["<div>\n<script>alert(1)</script>\n</div>", ["p:<div>\n<script>alert(1)</script>\n</div>"]],
  ])("%j", (source, expected) => {
    expect(blocks(source)).toEqual(expected);
  });

  it("reads an unclosed fence as code so far, at every length of a streamed message", () => {
    const message = "Here is the fix:\n\n```rust\nfn main() {\n    run();\n}\n```\n\nDone.";
    expect(blocks("Here is the fix:\n\n```rust\nfn main() {")).toEqual([
      "p:Here is the fix:",
      "code(rust)…:fn main() {",
    ]);
    expect(blocks("Text\n\n```")).toEqual(["p:Text", "code()…:"]);
    expect(blocks(message)).toEqual([
      "p:Here is the fix:",
      "code(rust):fn main() {\n    run();\n}",
      "p:Done.",
    ]);
    for (let length = 0; length <= message.length; length += 1) {
      const read = blocks(message.slice(0, length));
      // Once the fence opened, nothing after it reads as prose until it closes.
      const code = read.find((one) => one.startsWith("code"));
      if (length >= message.indexOf("fn main")) expect(code, String(length)).toBeDefined();
      if (code?.includes("…")) expect(read.at(-1)).toBe(code);
    }
  });

  it("reads every prefix of a message that uses everything, without throwing", () => {
    const message = [
      "# Plan",
      "",
      'I\'ll **change** `main.rs` and _then_ run the [tests](https://example.com/ci "CI").',
      "",
      "1. Edit <https://example.com/a>",
      "   - nested *item*",
      "2. Run",
      "",
      "```sh",
      "cargo test -- --nocapture",
      "```",
      "",
      "---",
      "",
      "Done &amp; dusted: https://example.com/done.",
    ].join("\n");
    for (let length = 0; length <= message.length; length += 1) {
      expect(() => parseMarkdown(message.slice(0, length))).not.toThrow();
    }
    expect(blocks(message)).toEqual([
      "h1:Plan",
      "p:I'll <b>change</b> <c>main.rs</c> and <i>then</i> run the <a https://example.com/ci>tests</a>.",
      "ol1[Edit <a https://example.com/a>https://example.com/a</a>ul[nested <i>item</i>]|Run]",
      "code(sh):cargo test -- --nocapture",
      "hr",
      "p:Done & dusted: <a https://example.com/done>https://example.com/done</a>.",
    ]);
  });

  it("keeps the blocks that read the same, so a streamed message redraws only its end", () => {
    const first = parseMarkdown("Intro.\n\n- one\n\nTail so");
    const second = reuseBlocks(first, parseMarkdown("Intro.\n\n- one\n\nTail so far"));
    expect(second[0]).toBe(first[0]);
    expect(second[1]).toBe(first[1]);
    expect(second[2]).not.toBe(first[2]);
    expect(block(second[2])).toBe("p:Tail so far");
    // Same text at another place is another block.
    const moved = reuseBlocks(first, parseMarkdown("New.\n\nIntro."));
    expect(moved[1]).not.toBe(first[0]);
    expect(reuseBlocks(undefined, first)).toBe(first);
  });

  it("limits how deep lists nest", () => {
    const stairs = Array.from({ length: 40 }, (_, step) => `${"  ".repeat(step)}- step ${step}`);
    const depth = (list: ListBlock): number =>
      1 + Math.max(0, ...list.items.flatMap((item) => item.lists.map(depth)));
    const [list] = parseMarkdown(stairs.join("\n"));
    expect(list.kind === "list" && depth(list)).toBe(6);
    expect(block(list)).toContain("step 39");
  });
});

describe("hostile and very long input", () => {
  const LONG = 100_000;
  it.each([
    ["one long word", "a".repeat(LONG)],
    ["openers that never close", "*a ".repeat(LONG / 3)],
    ["underscores that never close", "_a ".repeat(LONG / 3)],
    ["bold that never closes", "**a ".repeat(LONG / 4)],
    ["stars only", "*".repeat(LONG)],
    ["backticks only", "`".repeat(LONG)],
    [
      "backtick runs of every length",
      Array.from({ length: 400 }, (_, n) => "`".repeat(n + 1)).join("a"),
    ],
    ["brackets that never close", "[".repeat(LONG)],
    ["brackets closed at the very end", `${"[".repeat(LONG)}]`],
    ["links without an end", "[a](".repeat(LONG / 4)],
    ["links without an end, then one", `${"[a](".repeat(LONG / 4)})`],
    ["links with nested parentheses", "[a]((".repeat(LONG / 5)],
    ["angle brackets", "<http".repeat(LONG / 5)],
    ["addresses end to end", "http://".repeat(LONG / 7)],
    ["entities", "&#x26;&amp;&".repeat(LONG / 12)],
    ["a heading of spaces", `# ${" ".repeat(LONG)}x`],
    ["a rule that is not one", `${"- ".repeat(LONG / 2)}x`],
    ["a list item of spaces", `-${" ".repeat(LONG)}x`],
    ["nested emphasis", `${"*a _b ".repeat(LONG / 12)}${" b_ a*".repeat(LONG / 12)}`],
    ["escapes", "\\*".repeat(LONG / 2)],
    ["many short lines", "- a\n".repeat(LONG / 4)],
    ["many fences", "```\n".repeat(LONG / 4)],
    ["a script on one line", `<script>${"alert(1);".repeat(LONG / 9)}</script>`],
  ])("reads %s quickly and loses nothing", (_name, source) => {
    const started = performance.now();
    const read = parseMarkdown(source);
    const elapsed = performance.now() - started;
    // Linear work takes a few milliseconds here; a quadratic scan would take seconds.
    expect(elapsed).toBeLessThan(1000);
    expect(read.length).toBeGreaterThan(0);
    expect(() => JSON.stringify(read)).not.toThrow();
  });
});
