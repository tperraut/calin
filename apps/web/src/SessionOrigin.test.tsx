import { flush } from "solid-js";
import { afterEach, describe, expect, it } from "vite-plus/test";
import { App } from "./App";
import type { Session } from "./gen/Session";
import { SessionList } from "./components/SessionList";
import { createAppState } from "./state/app";
import { fakeApi, FakeSocket, immediately, makeDetail, makeSession, settle } from "./test/fakes";
import { mount, unmountAll } from "./test/render";

afterEach(unmountAll);

const fromIssue = makeSession("s1", 1000, {
  title: "Add retry",
  status: "running",
  issue: {
    repo_id: "api",
    number: 12,
    title: "Add retry",
    url: "https://github.com/acme/api/issues/12",
  },
});
const automated = makeSession("s2", 900, { title: "Nightly", automation_id: "a 1" });
const plain = makeSession("s3", 800, { title: "By hand" });

function origin(root: ParentNode) {
  const issue = root.querySelector<HTMLAnchorElement>('a[data-part="issue"]');
  const automation = root.querySelector<HTMLAnchorElement>('a[data-part="automation"]');
  return {
    issue: issue && {
      text: issue.textContent,
      href: issue.getAttribute("href"),
      target: issue.getAttribute("target"),
      rel: issue.getAttribute("rel"),
    },
    automation: automation && {
      text: automation.textContent,
      href: automation.getAttribute("href"),
    },
  };
}

describe("where a session came from", () => {
  it("shows the issue chip and the automation marker in the list, beside the row's own link", () => {
    const host = mount(() => <SessionList sessions={[fromIssue, automated, plain]} now={2000} />);
    const rows = [...host.querySelectorAll(":scope > ul > li")];
    expect(origin(rows[0] as Element)).toEqual({
      issue: {
        text: "#12",
        href: "https://github.com/acme/api/issues/12",
        target: "_blank",
        rel: "noopener noreferrer",
      },
      automation: null,
    });
    expect(origin(rows[1] as Element)).toEqual({
      issue: null,
      automation: { text: "automation", href: "/automations/a%201" },
    });
    expect(origin(rows[2] as Element)).toEqual({ issue: null, automation: null });
    // Links never nest.
    expect(host.querySelector("a a")).toBeNull();
  });

  it("shows them on the session page", async () => {
    const session: Session = { ...fromIssue, automation_id: "a1" };
    history.replaceState(null, "", "/s/s1");
    const socket = new FakeSocket();
    const api = fakeApi({
      health: () => Promise.reject(new Error("no")),
      listRepos: () => Promise.resolve([]),
      getSession: () => Promise.resolve(makeDetail(session)),
      getChanges: () => Promise.resolve([]),
    });
    const state = createAppState({ api, socket, schedule: immediately });
    const host = mount(() => <App state={state} />);
    socket.setState("open");
    await settle();
    flush();
    await settle();
    flush();
    const header = host.querySelector("main header");
    expect(header).not.toBeNull();
    expect(origin(header as Element)).toEqual({
      issue: {
        text: "#12",
        href: "https://github.com/acme/api/issues/12",
        target: "_blank",
        rel: "noopener noreferrer",
      },
      automation: { text: "automation", href: "/automations/a1" },
    });
  });
});
