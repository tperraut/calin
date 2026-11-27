import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it } from "vite-plus/test";
import { find, mount, unmountAll } from "../test/render";
import { Markdown } from "./Markdown";

afterEach(unmountAll);

const render = (text: string): HTMLElement =>
  find(
    mount(() => <Markdown text={text} />),
    ".markdown",
  );

/** Every element the renderer may produce; anything else came from the text. */
const ALLOWED = new Set(["P", "PRE", "CODE", "STRONG", "EM", "A", "UL", "OL", "LI", "HR"]);

function strangers(root: Element): string[] {
  return [...root.querySelectorAll("*")]
    .filter((element) => !ALLOWED.has(element.tagName))
    .map((element) => element.tagName);
}

describe("Markdown", () => {
  it("renders paragraphs, emphasis, inline code and a heading as a bold line", () => {
    const root = render("# Summary\n\nI **changed** `main.rs` and *ran*\nthe tests.");
    const [heading, paragraph] = root.children;
    expect(heading.tagName).toBe("P");
    expect(heading.classList.contains("font-semibold")).toBe(true);
    expect(heading.textContent).toBe("Summary");
    expect(paragraph.tagName).toBe("P");
    // The line break inside the paragraph is kept, as the agent wrote it.
    expect(paragraph.textContent).toBe("I changed main.rs and ran\nthe tests.");
    expect(find(paragraph, "strong").textContent).toBe("changed");
    expect(find(paragraph, "em").textContent).toBe("ran");
    expect(find(paragraph, "code").textContent).toBe("main.rs");
    expect(root.classList.contains("whitespace-pre-wrap")).toBe(true);
  });

  it("renders a fenced block as scrollable monospace code, verbatim", () => {
    const root = render("```html\n<b>**not bold**</b> &amp;\n\tindented\n```");
    const pre = find(root, "pre");
    expect(pre.textContent).toBe("<b>**not bold**</b> &amp;\n\tindented");
    expect(pre.querySelector("b")).toBeNull();
    expect(pre.className).toContain("font-mono");
    expect(pre.className).toContain("overflow-auto");
    // A scrolling region the keyboard can reach, and that says what it is.
    expect(pre.getAttribute("tabindex")).toBe("0");
    expect(pre.getAttribute("aria-label")).toBe("Code, html");
  });

  it("renders bullet and numbered lists, nested, keeping the number a list starts at", () => {
    const root = render("- one\n  - inner\n- two\n\n3. three\n4. four");
    const [bullets, numbers] = root.children;
    expect(bullets.tagName).toBe("UL");
    expect([...bullets.children].map((item) => item.firstChild?.textContent)).toEqual([
      "one",
      "two",
    ]);
    expect(find(bullets, "li > ul > li").textContent).toBe("inner");
    expect(numbers.tagName).toBe("OL");
    expect(numbers.getAttribute("start")).toBe("3");
    expect(numbers.children).toHaveLength(2);
  });

  it("opens http and https links in a new tab without handing over the opener", () => {
    const root = render("See [the docs](https://example.com/docs) or http://example.org.");
    const links = [...root.querySelectorAll("a")];
    expect(links.map((link) => [link.textContent, link.getAttribute("href")])).toEqual([
      ["the docs", "https://example.com/docs"],
      ["http://example.org", "http://example.org/"],
    ]);
    for (const link of links) {
      expect(link.getAttribute("target")).toBe("_blank");
      expect(link.getAttribute("rel")).toBe("noopener noreferrer");
    }
  });

  it.each([
    "<script>window.pwned = true</script>",
    '<img src=x onerror="window.pwned = true">',
    "<iframe src='javascript:alert(1)'></iframe>",
    '<a href="javascript:alert(1)">x</a>',
    "[x](javascript:alert(1))",
    "[x](JAVASCRIPT:alert(1))",
    "[x](data:text/html;base64,PHNjcmlwdD4=)",
    "<javascript:alert(1)>",
    "&lt;script&gt;window.pwned = true&lt;/script&gt;",
    "&#60;img src=x onerror=alert(1)&#62;",
    "```\n</code></pre><script>window.pwned = true</script>\n```",
    "`</code><script>window.pwned = true</script>`",
    "- <script>window.pwned = true</script>\n- [x](javascript:alert(1))",
    "# <svg onload=alert(1)>",
    "**<b onmouseover=alert(1)>bold</b>**",
    "[<img src=x onerror=alert(1)>](https://example.com)",
    '[x](https://example.com" onclick="alert(1))',
    'https://example.com/"><script>alert(1)</script>',
  ])("injects nothing for %j", (source) => {
    const root = render(source);
    expect(strangers(root)).toEqual([]);
    for (const link of root.querySelectorAll("a")) {
      expect(link.getAttribute("href")).toMatch(/^https?:\/\//);
      expect(link.getAttributeNames().toSorted()).toEqual(["class", "href", "rel", "target"]);
    }
    expect("pwned" in window).toBe(false);
  });

  it("shows what it did not take as markup, word for word", () => {
    const source = '<script>alert("x")</script> and [x](javascript:alert(1))';
    expect(render(source).textContent).toBe(source);
    // An entity is the character it names, as text.
    expect(render("&lt;script&gt;").textContent).toBe("<script>");
    expect(render("&lt;script&gt;").querySelector("script")).toBeNull();
  });

  it("renders a very long line without choking or cutting it", () => {
    const line = `${"x".repeat(50_000)} <b> ${"*y ".repeat(10_000)}end`;
    const root = render(line);
    expect(root.textContent === line).toBe(true);
    expect(strangers(root)).toEqual([]);
    // Long words wrap instead of widening the page.
    expect(root.classList.contains("wrap-break-word")).toBe(true);
  });

  it("renders every prefix of a streamed message sensibly, an open fence as code so far", () => {
    const message =
      "Fixing **it** now:\n\n```rust\nfn main() {\n    run();\n}\n```\n\n- done\n- see [CI](https://example.com/ci)";
    const [text, setText] = createSignal("");
    const root = find(
      mount(() => <Markdown text={text()} streaming={true} />),
      ".markdown",
    );
    expect(root.classList.contains("markdown-streaming")).toBe(true);
    for (let length = 1; length <= message.length; length += 1) {
      setText(message.slice(0, length));
      flush();
      expect(strangers(root), String(length)).toEqual([]);
      const written = message.slice(0, length);
      const fenceOpen = written.includes("```rust\n") && !written.includes("}\n```");
      if (fenceOpen) {
        const code = written.slice(written.indexOf("```rust\n") + 8);
        // All of it is code while the fence is open, the line being written included.
        expect(find(root, "pre").textContent, String(length)).toBe(code);
        expect(root.lastElementChild?.tagName).toBe("PRE");
      }
    }
    expect([...root.children].map((child) => child.tagName)).toEqual(["P", "PRE", "UL"]);
    expect(find(root, "pre").textContent).toBe("fn main() {\n    run();\n}");
    expect(find(root, "a").getAttribute("href")).toBe("https://example.com/ci");
  });

  it("leaves the blocks already written in place while the last one grows", () => {
    const [text, setText] = createSignal("First paragraph.\n\n```\ncode\n```\n\nSecond par");
    const root = find(
      mount(() => <Markdown text={text()} streaming={true} />),
      ".markdown",
    );
    const [first, code, tail] = root.children;

    setText("First paragraph.\n\n```\ncode\n```\n\nSecond paragraph, longer.");
    flush();
    expect(root.children[0]).toBe(first);
    expect(root.children[1]).toBe(code);
    expect(root.children[2]).not.toBe(tail);
    expect(root.children[2].textContent).toBe("Second paragraph, longer.");
  });

  it("marks only a message still being written for the cursor", () => {
    const [streaming, setStreaming] = createSignal(true);
    const root = find(
      mount(() => <Markdown text="Working" streaming={streaming()} />),
      ".markdown",
    );
    expect(root.classList.contains("markdown-streaming")).toBe(true);
    setStreaming(false);
    flush();
    expect(root.classList.contains("markdown-streaming")).toBe(false);
    // The cursor is the stylesheet's: the text is the message and nothing else.
    expect(root.textContent).toBe("Working");
  });
});
