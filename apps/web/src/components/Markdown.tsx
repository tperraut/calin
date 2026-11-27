import { createMemo, For } from "solid-js";
import type { JSX } from "@solidjs/web";
import {
  parseMarkdown,
  reuseBlocks,
  type Block,
  type Inline,
  type ListBlock,
} from "../lib/markdown";

// A block or an inline never changes once read (see `reuseBlocks`), so these
// are plain functions of their data, not components reading reactive state.
// Everything becomes an element or a text node: no markup is ever parsed.

function inline(node: Inline): JSX.Element {
  switch (node.kind) {
    case "text":
      return node.text;
    case "code":
      return <code class="rounded bg-sunken px-1 py-0.5 font-mono text-[0.9em]">{node.text}</code>;
    case "strong":
      return <strong class="font-semibold">{inlines(node.children)}</strong>;
    case "em":
      return <em>{inlines(node.children)}</em>;
    case "link":
      return (
        <a
          href={node.href}
          target="_blank"
          rel="noopener noreferrer"
          class="text-accent underline wrap-anywhere"
        >
          {inlines(node.children)}
        </a>
      );
  }
}

const inlines = (nodes: readonly Inline[]): JSX.Element => nodes.map(inline);

function list(block: ListBlock): JSX.Element {
  const items = block.items.map((item) => (
    <li>
      {inlines(item.content)}
      {item.lists.map(list)}
    </li>
  ));
  return block.ordered ? (
    <ol start={block.start} class="list-decimal space-y-0.5 pl-6">
      {items}
    </ol>
  ) : (
    <ul class="list-disc space-y-0.5 pl-5">{items}</ul>
  );
}

function blockElement(block: Block): JSX.Element {
  switch (block.kind) {
    case "paragraph":
      return <p>{inlines(block.content)}</p>;
    case "heading":
      // A bold line: the transcript has its own outline, and a message is not part of it.
      return <p class="font-semibold">{inlines(block.content)}</p>;
    case "code":
      return (
        <pre
          tabindex="0"
          aria-label={block.lang === "" ? "Code" : `Code, ${block.lang}`}
          class="max-h-96 overflow-auto rounded bg-sunken p-2 font-mono text-xs whitespace-pre"
        >
          <code>{block.text}</code>
        </pre>
      );
    case "rule":
      return <hr class="border-line" />;
    case "list":
      return list(block);
  }
}

/**
 * An agent message, read as Markdown. `streaming` puts a cursor after the
 * last block while the message is still being written.
 */
export function Markdown(props: { text: string; streaming?: boolean }) {
  const blocks = createMemo<Block[]>((previous) =>
    reuseBlocks(previous, parseMarkdown(props.text)),
  );
  return (
    <div
      class={[
        "markdown flex flex-col gap-2 wrap-break-word whitespace-pre-wrap",
        { "markdown-streaming": props.streaming === true },
      ]}
    >
      <For each={blocks()}>{(block) => blockElement(block)}</For>
    </div>
  );
}
