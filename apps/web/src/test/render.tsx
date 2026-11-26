import { render, type JSX } from "@solidjs/web";
import { flush } from "solid-js";

const mounted: Array<() => void> = [];

/** Renders into the document, as the app does. Call `unmountAll` after each test. */
export function mount(ui: () => JSX.Element): HTMLElement {
  const host = document.createElement("div");
  document.body.append(host);
  const dispose = render(ui, host);
  mounted.push(() => {
    dispose();
    host.remove();
  });
  flush();
  return host;
}

export function unmountAll(): void {
  for (const unmount of mounted.splice(0)) unmount();
}

/** The one element matching `selector` under `root`; fails the test when there is none. */
export function find<T extends Element = HTMLElement>(root: ParentNode, selector: string): T {
  const element = root.querySelector<T>(selector);
  if (!element) throw new Error(`nothing matches ${selector}`);
  return element;
}

/** The element of type `tag` whose text contains `text`. */
export function findByText<T extends HTMLElement = HTMLElement>(
  root: ParentNode,
  tag: string,
  text: string,
): T {
  for (const element of root.querySelectorAll<T>(tag)) {
    if (element.textContent.includes(text)) return element;
  }
  throw new Error(`no <${tag}> contains "${text}"`);
}

export function type(field: HTMLTextAreaElement | HTMLInputElement, value: string): void {
  field.value = value;
  field.dispatchEvent(new InputEvent("input", { bubbles: true }));
  flush();
}
