import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import type { OpenQuestion } from "../state/sessionReducer";
import { conflict, deferred } from "../test/fakes";
import { click, control, find, findByText, mount, type, unmountAll } from "../test/render";
import { QuestionBar } from "./QuestionBar";

afterEach(unmountAll);

const asked = (
  id: string,
  seq: number,
  options: string[] = ["Header", "Footer"],
): OpenQuestion => ({
  question: {
    id,
    text: `The badge can go in the **header** or the footer (${id}). Which one?`,
    options,
  },
  agentId: "a1",
  ts: 1000 + seq,
  seq,
});

type Answer = (questionId: string, answer: string) => Promise<void>;

function setup(
  initial: OpenQuestion[],
  onAnswer: Answer = () => Promise.resolve(),
  compact = false,
) {
  const [questions, setQuestions] = createSignal<readonly OpenQuestion[]>(initial);
  const answer = vi.fn(onAnswer);
  const host = mount(() => (
    <QuestionBar questions={questions()} onAnswer={answer} compact={compact} />
  ));
  return {
    host,
    answer,
    bar: () => host.querySelector('section[aria-label="Question from the agent"]'),
    button: (label: string) => findByText<HTMLButtonElement>(host, "button", label),
    /** What the events do once the server has recorded an answer. */
    arrive: (next: OpenQuestion[]) => {
      setQuestions(next);
      flush();
    },
  };
}

describe("QuestionBar", () => {
  it("shows nothing but its live region while no question is open", () => {
    const { host, bar } = setup([]);
    expect(bar()).toBeNull();
    expect(find(host, '[role="status"]').textContent).toBe("");
  });

  it("shows the question as Markdown, its options as buttons, and a field for any other answer", () => {
    const { host, bar, button } = setup([asked("q1", 3)]);
    expect(find(bar() as HTMLElement, "strong").textContent).toBe("header");
    const options = find(host, '[role="group"][aria-label="Suggested answers"]');
    expect([...options.querySelectorAll("button")].map((one) => one.textContent)).toEqual([
      "Header",
      "Footer",
    ]);
    expect(control(host, "Your answer").value).toBe("");
    // Nothing to send yet.
    expect(button("Send answer").disabled).toBe(true);
    expect(find(host, '[role="status"]').textContent).toContain("The agent asks: The badge can go");
  });

  it("answers with an option, holds every control until the event arrives, then leaves", async () => {
    const request = deferred();
    const { host, answer, bar, button, arrive } = setup([asked("q1", 1)], () => request.promise);
    await click(button("Footer"));
    expect(answer.mock.calls).toEqual([["q1", "Footer"]]);
    expect(button("Header").disabled).toBe(true);
    expect(button("Footer").disabled).toBe(true);
    expect(control(host, "Your answer").disabled).toBe(true);
    expect(bar()?.textContent).toContain("Sending…");

    request.resolve();
    await click(button("Header"));
    // The server said yes, but the bar only moves with the session's events.
    expect(bar()).not.toBeNull();
    expect(bar()?.textContent).toContain("Sent, waiting for the agent…");
    expect(button("Header").disabled).toBe(true);
    expect(answer).toHaveBeenCalledTimes(1);

    arrive([]);
    expect(bar()).toBeNull();
  });

  it("sends a free-text answer, trimmed, with the button or with Enter", async () => {
    const { host, answer, button } = setup([asked("q1", 1, [])]);
    expect(host.querySelector('[aria-label="Suggested answers"]')).toBeNull();
    type(control(host, "Your answer"), "  In the sidebar.  ");
    expect(button("Send answer").disabled).toBe(false);
    find<HTMLFormElement>(host, "form").dispatchEvent(
      new Event("submit", { bubbles: true, cancelable: true }),
    );
    await click(host);
    expect(answer.mock.calls).toEqual([["q1", "In the sidebar."]]);
  });

  it("does not send a blank answer, nor two answers for two quick taps", async () => {
    const request = deferred();
    const { host, answer, button } = setup([asked("q1", 1)], () => request.promise);
    type(control(host, "Your answer"), "   ");
    expect(button("Send answer").disabled).toBe(true);
    button("Header").click();
    button("Footer").click();
    await click(host);
    expect(answer.mock.calls).toEqual([["q1", "Header"]]);
  });

  it("shows the server's message when the answer is refused, and lets the reader try again", async () => {
    let refuse = true;
    const { host, answer, bar, button } = setup([asked("q1", 1)], () =>
      refuse ? Promise.reject(conflict("question was already answered")) : Promise.resolve(),
    );
    type(control(host, "Your answer"), "Header, please");
    await click(button("Send answer"));
    expect(find(bar() as HTMLElement, '[role="alert"]').textContent).toBe(
      "question was already answered",
    );
    // What was typed is still there, and nothing is held.
    expect(control(host, "Your answer").value).toBe("Header, please");
    expect(button("Header").disabled).toBe(false);
    refuse = false;
    await click(button("Send answer"));
    expect(answer).toHaveBeenCalledTimes(2);
    expect(bar()?.querySelector('[role="alert"]')).toBeNull();
    expect(control(host, "Your answer").value).toBe("");
  });

  it("shows the oldest question first and the next one once it is answered", async () => {
    const { bar, button, arrive } = setup([asked("q1", 1), asked("q2", 2, ["Blue"])]);
    expect(bar()?.textContent).toContain("(q1)");
    expect(bar()?.textContent).toContain("1 more waiting");
    await click(button("Header"));
    arrive([asked("q2", 2, ["Blue"])]);
    expect(bar()?.textContent).toContain("(q2)");
    // The next question is not held by the answer to the one before.
    expect(button("Blue").disabled).toBe(false);
  });

  it("stays one line under a panel on a phone until the reader opens it", async () => {
    const { host, bar, button } = setup([asked("q1", 1)], undefined, true);
    expect(bar()?.textContent).toContain("The agent asks");
    expect(host.querySelector("form")).toBeNull();
    expect(button("Answer").getAttribute("aria-expanded")).toBe("false");
    await click(button("Answer"));
    expect(host.querySelector("form")).not.toBeNull();
    expect(button("Hide").getAttribute("aria-expanded")).toBe("true");
    await click(button("Footer"));
    expect(bar()?.textContent).toContain("Sent, waiting for the agent…");
  });
});
