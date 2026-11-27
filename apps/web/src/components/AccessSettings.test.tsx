import { flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import { HttpError, type Api } from "../api/http";
import type { AuthSessionInfo } from "../gen/AuthSessionInfo";
import type { CreatedDeviceToken } from "../gen/CreatedDeviceToken";
import type { DeviceTokenInfo } from "../gen/DeviceTokenInfo";
import type { Health } from "../gen/Health";
import type { Me } from "../gen/Me";
import type { Settings } from "../gen/Settings";
import SettingsPage from "../pages/SettingsPage";
import { AppContext, createAppState } from "../state/app";
import { badRequest, conflict, fakeApi, FakeSocket, immediately, settle } from "../test/fakes";
import { click, control, find, findByText, mount, type, unmountAll } from "../test/render";

const health: Health = {
  name: "calin",
  version: "0.1.0",
  dry_run: false,
  runners: ["mock"],
  github_problem: null,
};
const settings: Settings = {
  home_repo: null,
  scan_roots: [],
  clone_root: null,
  max_parallel: null,
};
const publicMe: Me = { mode: "public", identity: "ada@example.org", device_token: false };
const NOW = Date.now();
const MINUTE = 60_000;

const FIREFOX = "Mozilla/5.0 (X11; Linux x86_64; rv:133.0) Gecko/20100101 Firefox/133.0";
const IPHONE =
  "Mozilla/5.0 (iPhone; CPU iPhone OS 18_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Mobile/15E148 Safari/604.1";

const browsers: AuthSessionInfo[] = [
  {
    id: "b1",
    user_agent: FIREFOX,
    created_at: NOW - 3 * 60 * MINUTE,
    last_used_at: NOW,
    current: true,
  },
  {
    id: "b2",
    user_agent: IPHONE,
    created_at: NOW - 2 * 24 * 60 * MINUTE,
    last_used_at: NOW - 5 * MINUTE,
    current: false,
  },
  { id: "b3", user_agent: "calin-desktop/0.1", created_at: NOW, last_used_at: NOW, current: false },
];
const tokens: DeviceTokenInfo[] = [
  { id: "t1", name: "Laptop CLI", created_at: NOW - 10 * MINUTE, last_used_at: NOW - 2 * MINUTE },
  { id: "t2", name: "Backup script", created_at: NOW - 60 * MINUTE, last_used_at: null },
];
const SECRET = "calin_s3cr3t-0123456789abcdef";
const created: CreatedDeviceToken = {
  info: { id: "t3", name: "Phone shortcut", created_at: NOW, last_used_at: null },
  token: SECRET,
};

const stops: Array<() => void> = [];

/** Shadows the browser's clipboard on `navigator` itself, until the test ends. */
function setClipboard(clipboard: { writeText: (text: string) => Promise<void> } | undefined): void {
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: clipboard });
}

afterEach(() => {
  for (const stop of stops.splice(0)) stop();
  unmountAll();
  // The own property goes; the prototype's getter shows through again.
  Reflect.deleteProperty(navigator, "clipboard");
});

async function open(routes: Partial<Api> = {}, me: Me = publicMe) {
  const api = fakeApi({
    me: () => Promise.resolve(me),
    health: () => Promise.resolve(health),
    getSettings: () => Promise.resolve(settings),
    listRepos: () => Promise.resolve([]),
    listAuthSessions: vi.fn<Api["listAuthSessions"]>().mockResolvedValue(browsers),
    listDeviceTokens: vi.fn<Api["listDeviceTokens"]>().mockResolvedValue(tokens),
    ...routes,
  });
  const state = createAppState({ api, socket: new FakeSocket(), schedule: immediately });
  stops.push(state.start());
  await settle();
  const host = mount(() => (
    <AppContext value={state}>
      <SettingsPage />
    </AppContext>
  ));
  await act();
  const browsersList = () => find(host, 'ul[aria-label="Signed-in browsers"]');
  const tokensList = () => find(host, 'ul[aria-label="Device tokens"]');
  return { host, state, api, browsersList, tokensList };
}

async function act(): Promise<void> {
  await settle();
  flush();
  await settle();
  flush();
}

/** Each row's words, one text node after another, as a screen reader reads them. */
function words(node: Node): string {
  const parts: string[] = [];
  const walker = document.createTreeWalker(node, NodeFilter.SHOW_TEXT);
  for (let text = walker.nextNode(); text; text = walker.nextNode())
    parts.push(text.nodeValue ?? "");
  return parts.join(" ").replace(/\s+/g, " ").trim();
}

const rows = (list: HTMLElement): string[] => [...list.querySelectorAll(":scope > li")].map(words);

const button = (root: ParentNode, name: string): HTMLButtonElement =>
  find<HTMLButtonElement>(root, `button[aria-label="${name}"]`);

describe("Access settings", () => {
  it("is absent in local mode, and asks the server for none of it", async () => {
    const listAuthSessions = vi.fn<Api["listAuthSessions"]>();
    const listDeviceTokens = vi.fn<Api["listDeviceTokens"]>();
    const { host } = await open(
      { listAuthSessions, listDeviceTokens },
      { mode: "local", identity: null, device_token: false },
    );
    expect(host.textContent).toContain("Home repo");
    expect(host.textContent).not.toContain("Access");
    expect(host.textContent).not.toContain("Sign out");
    expect(listAuthSessions).not.toHaveBeenCalled();
    expect(listDeviceTokens).not.toHaveBeenCalled();
  });

  it("shows the account, for phones where the header has no room for it", async () => {
    const { host } = await open();
    const access = findByText(host, "section", "Access");
    expect(find(access, '[title="ada@example.org"]').textContent).toBe(
      "Signed in as ada@example.org",
    );
  });

  describe("signed-in browsers", () => {
    it("lists them by browser and system, with times, and marks this one", async () => {
      const { browsersList } = await open();
      expect(rows(browsersList())).toEqual([
        "Firefox on Linux This browser Signed in 3 h ago Last used just now Sign out",
        "Safari on iOS Signed in 2 d ago Last used 5 min ago Sign out",
        "calin-desktop/0.1 Signed in just now Last used just now Sign out",
      ]);
      expect(button(browsersList(), "Sign out this browser")).toBeTruthy();
      expect(button(browsersList(), "Sign out Safari on iOS")).toBeTruthy();
    });

    it("signs another browser out and reads the list again", async () => {
      const revokeAuthSession = vi.fn<Api["revokeAuthSession"]>().mockResolvedValue();
      const listAuthSessions = vi
        .fn<Api["listAuthSessions"]>()
        .mockResolvedValueOnce(browsers)
        .mockResolvedValue([browsers[0], browsers[2]]);
      const { browsersList } = await open({ revokeAuthSession, listAuthSessions });

      await click(button(browsersList(), "Sign out Safari on iOS"));
      await act();

      expect(revokeAuthSession).toHaveBeenCalledWith("b2");
      expect(rows(browsersList())).toHaveLength(2);
      expect(browsersList().textContent).not.toContain("Safari");
    });

    it("takes one that is already gone as signed out, without an error", async () => {
      const revokeAuthSession = vi
        .fn<Api["revokeAuthSession"]>()
        .mockRejectedValue(new HttpError(404, "unknown session", true));
      const listAuthSessions = vi
        .fn<Api["listAuthSessions"]>()
        .mockResolvedValueOnce(browsers)
        .mockResolvedValue([browsers[0]]);
      const { host, browsersList } = await open({ revokeAuthSession, listAuthSessions });

      await click(button(browsersList(), "Sign out Safari on iOS"));
      await act();

      expect(listAuthSessions).toHaveBeenCalledTimes(2);
      expect(rows(browsersList())).toHaveLength(1);
      expect(host.querySelector('[role="alert"]')).toBeNull();
    });

    it("shows the server's reason when it refuses", async () => {
      const revokeAuthSession = vi
        .fn<Api["revokeAuthSession"]>()
        .mockRejectedValue(new HttpError(403, "origin not allowed", true));
      const { browsersList } = await open({ revokeAuthSession });
      await click(button(browsersList(), "Sign out Safari on iOS"));
      expect(browsersList().textContent).toContain("Could not sign it out: origin not allowed");
    });

    it("signs this browser out like the account's own Sign out", async () => {
      const logout = vi.fn<Api["logout"]>().mockResolvedValue();
      const revokeAuthSession = vi.fn<Api["revokeAuthSession"]>();
      const { browsersList, state } = await open({ logout, revokeAuthSession });

      await click(button(browsersList(), "Sign out this browser"));
      await act();

      expect(logout).toHaveBeenCalledTimes(1);
      expect(revokeAuthSession).not.toHaveBeenCalled();
      expect(state.auth()).toEqual({ status: "signed_out", ended: false });
    });
  });

  describe("device tokens", () => {
    it("lists them with when they were created and last used", async () => {
      const { tokensList } = await open();
      expect(rows(tokensList())).toEqual([
        "Laptop CLI Created 10 min ago Last used 2 min ago Revoke",
        "Backup script Created 1 h ago Never used Revoke",
      ]);
    });

    it("checks the name before asking the server", async () => {
      const createDeviceToken = vi.fn<Api["createDeviceToken"]>();
      const { host } = await open({ createDeviceToken });
      const create = findByText<HTMLButtonElement>(host, "button", "Create token");

      await click(create);
      expect(host.textContent).toContain("Give the token a name.");
      expect(control(host, "New token").getAttribute("aria-invalid")).toBe("true");

      type(control(host, "New token"), "x".repeat(81));
      await click(create);
      expect(host.textContent).toContain("Keep the name to 80 characters.");
      expect(createDeviceToken).not.toHaveBeenCalled();
    });

    it("shows the server's reason when it refuses the name", async () => {
      const createDeviceToken = vi
        .fn<Api["createDeviceToken"]>()
        .mockRejectedValue(badRequest("a token named Laptop CLI exists"));
      const { host } = await open({ createDeviceToken });
      type(control(host, "New token"), "Laptop CLI");
      await click(findByText(host, "button", "Create token"));
      expect(host.textContent).toContain("a token named Laptop CLI exists");
      expect(host.querySelector('[aria-label="New device token"]')).toBeNull();
    });

    it("shows a new token's secret once, in its box, until Done", async () => {
      const createDeviceToken = vi.fn<Api["createDeviceToken"]>().mockResolvedValue(created);
      const listDeviceTokens = vi
        .fn<Api["listDeviceTokens"]>()
        .mockResolvedValueOnce(tokens)
        .mockResolvedValue([created.info, ...tokens]);
      const { host, tokensList } = await open({ createDeviceToken, listDeviceTokens });

      type(control(host, "New token"), "  Phone shortcut ");
      await click(findByText(host, "button", "Create token"));
      await act();

      expect(createDeviceToken).toHaveBeenCalledWith({ name: "Phone shortcut" });
      const box = find(host, '[role="region"][aria-label="New device token"]');
      expect(find(box, "code").textContent).toBe(SECRET);
      expect(box.textContent).toContain("This is the only time the token is shown.");
      expect(document.activeElement).toBe(button(box, "Copy token"));
      // The list shows its name, never its secret.
      expect(rows(tokensList())[0]).toContain("Phone shortcut");
      expect(tokensList().textContent).not.toContain(SECRET);
      expect(control(host, "New token").value).toBe("");

      await click(findByText(box, "button", "Done"));
      expect(host.querySelector('[aria-label="New device token"]')).toBeNull();
      expect(document.body.innerHTML).not.toContain(SECRET);
    });

    it("forgets the secret when the page goes away", async () => {
      const createDeviceToken = vi.fn<Api["createDeviceToken"]>().mockResolvedValue(created);
      const { host, state } = await open({ createDeviceToken });
      type(control(host, "New token"), "Phone shortcut");
      await click(findByText(host, "button", "Create token"));
      expect(document.body.innerHTML).toContain(SECRET);

      unmountAll();
      expect(document.body.innerHTML).not.toContain(SECRET);
      // Coming back shows the page afresh, with no box.
      const again = mount(() => (
        <AppContext value={state}>
          <SettingsPage />
        </AppContext>
      ));
      await act();
      expect(again.textContent).toContain("Device tokens");
      expect(again.querySelector('[aria-label="New device token"]')).toBeNull();
      expect(document.body.innerHTML).not.toContain(SECRET);
    });

    it("never keeps the secret in storage, nor anywhere on the page outside its box", async () => {
      const setItem = vi.spyOn(Storage.prototype, "setItem");
      const createDeviceToken = vi.fn<Api["createDeviceToken"]>().mockResolvedValue(created);
      const { host } = await open({ createDeviceToken });
      type(control(host, "New token"), "Phone shortcut");
      await click(findByText(host, "button", "Create token"));
      await click(button(host, "Copy token"));

      for (const [key, value] of setItem.mock.calls) {
        expect(key).not.toContain(SECRET);
        expect(value).not.toContain(SECRET);
      }
      for (const storage of [localStorage, sessionStorage]) {
        for (let index = 0; index < storage.length; index += 1) {
          const key = storage.key(index) ?? "";
          expect(key + (storage.getItem(key) ?? "")).not.toContain(SECRET);
        }
      }
      const box = find(host, '[aria-label="New device token"]');
      const outside = document.body.cloneNode(true) as HTMLElement;
      outside.querySelector('[aria-label="New device token"]')?.remove();
      expect(box.innerHTML).toContain(SECRET);
      expect(outside.innerHTML).not.toContain(SECRET);
      expect(document.title).not.toContain(SECRET);
      expect(location.href).not.toContain(SECRET);
    });

    it("copies the secret to the clipboard", async () => {
      const writeText = vi.fn<(text: string) => Promise<void>>().mockResolvedValue();
      setClipboard({ writeText });
      const createDeviceToken = vi.fn<Api["createDeviceToken"]>().mockResolvedValue(created);
      const { host } = await open({ createDeviceToken });
      type(control(host, "New token"), "Phone shortcut");
      await click(findByText(host, "button", "Create token"));

      await click(button(host, "Copy token"));
      expect(writeText).toHaveBeenCalledWith(SECRET);
      expect(find(host, '[aria-label="New device token"]').textContent).toContain("Copied.");
    });

    it("selects the secret for copying by hand when there is no clipboard", async () => {
      setClipboard(undefined);
      const createDeviceToken = vi.fn<Api["createDeviceToken"]>().mockResolvedValue(created);
      const { host } = await open({ createDeviceToken });
      type(control(host, "New token"), "Phone shortcut");
      await click(findByText(host, "button", "Create token"));

      await click(button(host, "Copy token"));
      const box = find(host, '[aria-label="New device token"]');
      expect(box.textContent).toContain("the token is selected, copy it yourself");
      expect(window.getSelection()?.toString()).toBe(SECRET);
    });

    it("selects the secret when the clipboard refuses", async () => {
      const writeText = vi
        .fn<(text: string) => Promise<void>>()
        .mockRejectedValue(new Error("denied"));
      setClipboard({ writeText });
      const createDeviceToken = vi.fn<Api["createDeviceToken"]>().mockResolvedValue(created);
      const { host } = await open({ createDeviceToken });
      type(control(host, "New token"), "Phone shortcut");
      await click(findByText(host, "button", "Create token"));

      await click(button(host, "Copy token"));
      expect(host.textContent).toContain("the token is selected, copy it yourself");
    });

    it("revokes only after a confirmation, and can be called off", async () => {
      const revokeDeviceToken = vi.fn<Api["revokeDeviceToken"]>().mockResolvedValue();
      const listDeviceTokens = vi
        .fn<Api["listDeviceTokens"]>()
        .mockResolvedValueOnce(tokens)
        .mockResolvedValue([tokens[1]]);
      const { host, tokensList } = await open({ revokeDeviceToken, listDeviceTokens });

      await click(button(tokensList(), "Revoke Laptop CLI"));
      const confirm = find(host, '[role="group"][aria-label="Revoke Laptop CLI?"]');
      expect(confirm.textContent).toContain("loses access at once");
      expect(revokeDeviceToken).not.toHaveBeenCalled();
      await click(findByText(confirm, "button", "Cancel"));
      expect(host.querySelector('[role="group"]')).toBeNull();

      await click(button(tokensList(), "Revoke Laptop CLI"));
      await click(button(tokensList(), "Confirm revoking Laptop CLI"));
      await act();
      expect(revokeDeviceToken).toHaveBeenCalledWith("t1");
      expect(rows(tokensList())).toEqual(["Backup script Created 1 h ago Never used Revoke"]);
    });

    it("shows the server's reason when a revocation fails", async () => {
      const revokeDeviceToken = vi
        .fn<Api["revokeDeviceToken"]>()
        .mockRejectedValue(conflict("not possible in local mode"));
      const { tokensList } = await open({ revokeDeviceToken });
      await click(button(tokensList(), "Revoke Backup script"));
      await click(button(tokensList(), "Confirm revoking Backup script"));
      expect(tokensList().textContent).toContain("Could not revoke it: not possible in local mode");
    });

    it("shows the server's reason when the list cannot be read, and retries", async () => {
      const listDeviceTokens = vi
        .fn<Api["listDeviceTokens"]>()
        .mockRejectedValueOnce(new HttpError(500, "the token store is unreadable", true))
        .mockResolvedValue(tokens);
      const { host } = await open({ listDeviceTokens });
      const alert = findByText(host, '[role="alert"]', "could not be loaded");
      expect(alert.textContent).toContain("the token store is unreadable");
      await click(findByText(alert, "button", "Retry"));
      await act();
      expect(rows(find(host, 'ul[aria-label="Device tokens"]'))).toHaveLength(2);
    });
  });
});
