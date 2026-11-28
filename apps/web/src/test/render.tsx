import { render, type JSX } from "@solidjs/web";
import { flush } from "solid-js";
import { vi } from "vite-plus/test";

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
  setViewportWidth(WIDE);
}

const WIDE = 1024;
export const PHONE = 360;

/**
 * Sizes the window, which decides between the phone and the wide layout. Call
 * it before `mount`: happy-dom does not reliably tell a mounted page.
 */
export function setViewportWidth(width: number): void {
  // happy-dom's own handle on the window; not part of the DOM typings.
  const control = (
    window as unknown as { happyDOM: { setViewport(size: { width: number }): void } }
  ).happyDOM;
  control.setViewport({ width });
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

/** Whether a button whose text contains `text` is on screen and can be pressed. */
export function offersButton(root: ParentNode, text: string): boolean {
  return [...root.querySelectorAll<HTMLButtonElement>("button")].some(
    (button) => button.textContent.includes(text) && !button.disabled,
  );
}

export function type(field: HTMLTextAreaElement | HTMLInputElement, value: string): void {
  field.value = value;
  field.dispatchEvent(new InputEvent("input", { bubbles: true }));
  flush();
}

export function choose(select: HTMLSelectElement, value: string): void {
  select.value = value;
  select.dispatchEvent(new Event("change", { bubbles: true }));
  flush();
}

/** Lets the screen catch up, including what an already answered request then does. */
async function catchUp(): Promise<void> {
  for (let turn = 0; turn < 5; turn += 1) await Promise.resolve();
  flush();
}

/** Clicks and lets the screen catch up, including what an awaited request then does. */
export async function click(element: HTMLElement): Promise<void> {
  element.click();
  await catchUp();
}

/**
 * Waits until `ready` holds, then lets the screen catch up. Code fetched on
 * demand (a lazy page or list) takes as long as the machine needs to load it,
 * so no fixed number of turns is a safe wait on a loaded machine.
 */
export async function until(ready: () => boolean): Promise<void> {
  await vi.waitFor(
    () => {
      flush();
      if (!ready()) throw new Error("still waiting");
    },
    { timeout: 4_000, interval: 5 },
  );
  await catchUp();
}

/** Whether `main` shows a page, not the fallback of one whose code is still on its way (router.ts). */
export function pageShown(host: ParentNode): boolean {
  const shown = host.querySelector("main")?.textContent;
  return shown !== undefined && shown !== "Loading…";
}

/** The form control labelled by the `<label>` containing `text`: wrapped by it, or named by `for`. */
export function control<T extends HTMLElement = HTMLInputElement>(
  root: ParentNode,
  text: string,
): T {
  const label = findByText<HTMLLabelElement>(root, "label", text);
  const field =
    label.querySelector<T>("input, select, textarea") ??
    (label.htmlFor === "" ? null : (document.getElementById(label.htmlFor) as T | null));
  if (!field) throw new Error(`the label "${text}" names no control`);
  return field;
}
