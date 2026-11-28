import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import { deferred } from "../test/fakes";
import { click, find, mount, type, unmountAll } from "../test/render";
import { MessageBox } from "./MessageBox";

afterEach(unmountAll);

function setup(
  options: {
    enabled?: boolean;
    resumes?: boolean;
    onSend?: (text: string) => Promise<void>;
  } = {},
) {
  const [enabled, setEnabled] = createSignal(options.enabled ?? true);
  const [prefill, setPrefill] = createSignal<{ text: string; seq: number }>();
  const [shown, setShown] = createSignal<boolean | undefined>(undefined);
  const onSend = vi.fn(options.onSend ?? (() => Promise.resolve()));
  const host = mount(() => (
    <MessageBox
      enabled={enabled()}
      closedHint="The session is closed"
      resumes={options.resumes}
      prefill={prefill()}
      shown={shown()}
      onSend={onSend}
    />
  ));
  const field = () => find<HTMLTextAreaElement>(host, "textarea");
  const button = () => find<HTMLButtonElement>(host, 'button[type="submit"]');
  return { host, field, button, onSend, setEnabled, setPrefill, setShown };
}

describe("MessageBox", () => {
  it("sends the trimmed text, empties the field, and sends nothing blank", async () => {
    const { field, button, onSend } = setup();
    expect(field().placeholder).toBe("Message the agent");
    expect([button().textContent, button().title]).toEqual(["Send", "Send (Ctrl+Enter)"]);

    type(field(), "   ");
    await click(button());
    expect(onSend).not.toHaveBeenCalled();

    type(field(), "  Add a test  ");
    await click(button());
    expect(onSend).toHaveBeenCalledWith("Add a test");
    expect(field().value).toBe("");
  });

  it("says Continue when the message restarts the agent", () => {
    const { field, button } = setup({ resumes: true });
    expect(field().placeholder).toBe("Message the agent to continue");
    expect([button().textContent, button().title]).toEqual(["Continue", "Continue (Ctrl+Enter)"]);
  });

  it("shows the closed hint and takes nothing while disabled", async () => {
    const { field, button, onSend, setEnabled } = setup({ enabled: false });
    expect([field().disabled, field().placeholder]).toEqual([true, "The session is closed"]);
    expect(button().disabled).toBe(true);
    setEnabled(true);
    flush();
    type(field(), "Go on");
    setEnabled(false);
    flush();
    field().form?.requestSubmit();
    await click(field());
    expect(onSend).not.toHaveBeenCalled();
  });

  it("disables Send while a message is in flight, and keeps the text when it is refused", async () => {
    const pending = deferred();
    const { host, field, button, onSend } = setup({ onSend: () => pending.promise });
    type(field(), "Ship it");
    await click(button());
    expect([button().disabled, button().textContent]).toEqual([true, "Sending…"]);
    field().form?.requestSubmit();
    await click(field());
    expect(onSend).toHaveBeenCalledTimes(1);

    pending.reject(new Error("The agent is not running"));
    await click(host);
    const alert = find(host, '[role="alert"]');
    expect(alert.textContent).toBe("The agent is not running");
    expect(field().getAttribute("aria-describedby")).toBe(alert.id);
    expect(field().value).toBe("Ship it");
    expect(button().disabled).toBe(false);
  });

  it("sends on Ctrl+Enter or Cmd+Enter, and not on Enter alone", async () => {
    const { host, field, onSend } = setup();
    const press = async (init: KeyboardEventInit): Promise<boolean> => {
      const event = new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init });
      field().dispatchEvent(event);
      await click(host);
      return event.defaultPrevented;
    };
    type(field(), "One");
    expect(await press({ key: "Enter" })).toBe(false);
    expect(onSend).not.toHaveBeenCalled();
    expect(await press({ key: "Enter", ctrlKey: true })).toBe(true);
    type(field(), "Two");
    expect(await press({ key: "Enter", metaKey: true })).toBe(true);
    expect(onSend.mock.calls).toEqual([["One"], ["Two"]]);
  });

  it("grows with a longer draft, up to six rows", () => {
    const { field } = setup();
    expect(field().getAttribute("rows")).toBe("2");
    type(field(), "a\nb");
    expect(field().getAttribute("rows")).toBe("2");
    type(field(), "a\nb\nc");
    expect(field().getAttribute("rows")).toBe("5");
    type(field(), "a\nb\nc\nd\ne\nf");
    expect(field().getAttribute("rows")).toBe("6");
  });

  it("takes a prefill once per request, and gives the focus once the box shows", () => {
    const { field, setPrefill, setShown } = setup();
    setShown(false);
    setPrefill({ text: "Review this", seq: 1 });
    flush();
    expect(field().value).toBe("Review this");
    expect(document.activeElement).not.toBe(field());

    setShown(true);
    flush();
    expect(document.activeElement).toBe(field());

    type(field(), "Edited");
    field().blur();
    setPrefill({ text: "Review this", seq: 1 });
    flush();
    expect(field().value).toBe("Edited");
    expect(document.activeElement).not.toBe(field());

    setPrefill({ text: "Review this", seq: 2 });
    flush();
    expect(field().value).toBe("Review this");
    expect(document.activeElement).toBe(field());
  });
});
