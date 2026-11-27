import {
  createMemo,
  createSignal,
  createUniqueId,
  For,
  Match,
  onSettled,
  Show,
  Switch,
  useContext,
  type Accessor,
} from "solid-js";
import { HttpError } from "../api/http";
import type { PushPreferences } from "../gen/PushPreferences";
import type { PushSubscriptionInfo } from "../gen/PushSubscriptionInfo";
import { sameKey, type DeviceSubscription, type Permission } from "../lib/device";
import { errorMessage } from "../lib/errors";
import { AppContext, type AppState } from "../state/app";
import { LoadFailure, When } from "./AccessSettings";

const fieldError = "text-sm wrap-break-word text-tone-danger";
const hint = "text-xs text-muted";

export const UNAVAILABLE = {
  local: "Notifications need Calin to be served from a public URL.",
  needsInstall: "Install Calin to the home screen to enable notifications.",
  unsupported:
    "This browser cannot receive notifications from a web app. A recent Chrome, Edge, Firefox or Safari can.",
  noWorker: "Notifications need the built app: the development server runs no service worker.",
} as const;

export const PROBLEMS = {
  denied:
    "Notifications are blocked for Calin in this browser. To allow them again, open the site's settings (the icon left of the address, or the browser's settings › Site settings › Notifications), allow notifications for this site, then choose Notify this device again.",
  dismissed:
    "The browser did not get an answer to its question. Choose Notify this device again when you are ready to allow notifications.",
} as const;

const PREFERENCES: ReadonlyArray<{
  key: keyof PushPreferences;
  label: string;
  description: string;
}> = [
  {
    key: "waiting",
    label: "Waiting on you",
    description: "A session asks for a permission, asks a question or stops at a gate.",
  },
  {
    key: "review",
    label: "Ready for review",
    description: "A session finished its work and waits for your review.",
  },
  { key: "failed", label: "Failed", description: "A session stopped on an error." },
  {
    key: "merged",
    label: "All pull requests merged",
    description: "Every pull request a session opened was merged.",
  },
];

/** A 404 on removal means the device is gone already, which is what was asked. */
async function removeQuietly(app: AppState, id: string): Promise<string | undefined> {
  try {
    await app.removePushSubscription(id);
    return undefined;
  } catch (error) {
    if (error instanceof HttpError && error.status === 404) return undefined;
    return errorMessage(error);
  }
}

function DeviceRow(props: {
  device: Accessor<PushSubscriptionInfo>;
  mine: boolean;
  now: number;
  onRemove: () => Promise<string | undefined>;
}) {
  const [busy, setBusy] = createSignal(false);
  const [failure, setFailure] = createSignal<string>();

  async function remove(): Promise<void> {
    if (busy()) return;
    setBusy(true);
    setFailure(undefined);
    const problem = await props.onRemove();
    setBusy(false);
    setFailure(problem);
  }

  return (
    <li class="flex flex-col gap-1 px-3 py-2" data-mine={props.mine ? "" : undefined}>
      <div class="flex items-center gap-2">
        <div class="min-w-0 flex-1">
          <p class="flex min-w-0 flex-wrap items-center gap-x-2">
            <span class="min-w-0 truncate font-medium" title={props.device().label}>
              {props.device().label}
            </span>
            <Show when={props.mine}>
              <span class="rounded-full bg-accent-soft px-2 text-xs font-medium text-accent">
                This device
              </span>
            </Show>
          </p>
          <p class="truncate font-mono text-xs text-muted" title={props.device().service}>
            {props.device().service}
          </p>
          <p class="flex flex-wrap gap-x-2 text-xs text-muted">
            <When label="Added" ts={props.device().created_at} now={props.now} />
            <Show when={props.device().last_sent_at} fallback={<span>Last sent never</span>}>
              {(sent) => <When label="Last sent" ts={sent()} now={props.now} />}
            </Show>
          </p>
        </div>
        <button
          type="button"
          class="btn shrink-0"
          aria-label={`Remove device ${props.device().label}`}
          disabled={busy()}
          onClick={() => void remove()}
        >
          {busy() ? "Removing…" : "Remove"}
        </button>
      </div>
      <Show when={failure()}>
        {(message) => (
          <p role="alert" class={fieldError}>
            {message()}
          </p>
        )}
      </Show>
    </li>
  );
}

function Preferences(props: { headingId: string }) {
  const app = useContext(AppContext);
  const [stored, setStored] = createSignal<PushPreferences>();
  const [loadError, setLoadError] = createSignal<string>();
  const [saving, setSaving] = createSignal<keyof PushPreferences>();
  const [failure, setFailure] = createSignal<{ key: keyof PushPreferences; message: string }>();

  function load(): void {
    setLoadError(undefined);
    app.pushPreferences().then(
      (loaded) => setStored(loaded),
      (error: unknown) => setLoadError(errorMessage(error)),
    );
  }
  onSettled(() => load());

  /** Saved at once; on a refusal the switch goes back to what the server still has. */
  async function change(key: keyof PushPreferences, value: boolean): Promise<void> {
    const before = stored();
    if (!before || saving() !== undefined) return;
    setStored({ ...before, [key]: value });
    setSaving(key);
    setFailure(undefined);
    try {
      setStored(await app.savePushPreferences({ ...before, [key]: value }));
    } catch (error) {
      setStored(before);
      setFailure({ key, message: errorMessage(error) });
    } finally {
      setSaving(undefined);
    }
  }

  return (
    <fieldset class="flex flex-col gap-1" aria-labelledby={props.headingId}>
      <legend id={props.headingId} class="mb-1 text-sm font-medium">
        Notify me when
      </legend>
      <Switch>
        <Match when={loadError()}>
          {(message) => (
            <LoadFailure what="The notification choices" message={message()} onRetry={load} />
          )}
        </Match>
        <Match when={stored()}>
          {(preferences) => (
            <ul class="divide-y divide-line rounded-md border border-line">
              <For each={PREFERENCES}>
                {(choice) => {
                  const descriptionId = createUniqueId();
                  return (
                    <li class="flex flex-col px-3">
                      <label class="flex min-h-11 cursor-pointer items-center gap-3 py-2">
                        <input
                          type="checkbox"
                          role="switch"
                          class="size-5 shrink-0 accent-[var(--accent)]"
                          checked={preferences()[choice.key]}
                          disabled={saving() !== undefined}
                          aria-describedby={descriptionId}
                          onChange={(event) => void change(choice.key, event.currentTarget.checked)}
                        />
                        <span class="flex min-w-0 flex-col">
                          <span class="text-sm">{choice.label}</span>
                          <span id={descriptionId} class={hint}>
                            {choice.description}
                          </span>
                        </span>
                      </label>
                      <Show when={failure()?.key === choice.key ? failure() : undefined}>
                        {(failed) => (
                          <p role="alert" class={["pb-2", fieldError]}>
                            Not saved: {failed().message}
                          </p>
                        )}
                      </Show>
                    </li>
                  );
                }}
              </For>
            </ul>
          )}
        </Match>
        <Match when={true}>
          <p class="text-sm text-muted">Reading the notification choices…</p>
        </Match>
      </Switch>
    </fieldset>
  );
}

function TestNotification() {
  const app = useContext(AppContext);
  const [busy, setBusy] = createSignal(false);
  const [sent, setSent] = createSignal(false);
  const [failure, setFailure] = createSignal<string>();

  async function send(): Promise<void> {
    if (busy()) return;
    setBusy(true);
    setSent(false);
    setFailure(undefined);
    try {
      await app.sendTestPush();
      setSent(true);
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="flex flex-col gap-1.5">
      <div class="flex flex-wrap items-center gap-3">
        <button type="button" class="btn" disabled={busy()} onClick={() => void send()}>
          {busy() ? "Sending…" : "Send a test notification"}
        </button>
        <p role="status" class="text-sm text-tone-success empty:hidden">
          {sent() ? "Sent to every notified device." : ""}
        </p>
      </div>
      <Show when={failure()}>
        {(message) => (
          <p role="alert" class={fieldError}>
            Could not send it: {message()}
          </p>
        )}
      </Show>
      <p class={hint}>It can take a few seconds to arrive.</p>
    </div>
  );
}

type Devices =
  | { status: "loading" }
  | { status: "ready"; items: readonly PushSubscriptionInfo[] }
  | { status: "error"; message: string };

/** Everything that needs this browser to support push, in public mode. */
function NotificationControls() {
  const app = useContext(AppContext);
  const devicesId = createUniqueId();
  const preferencesId = createUniqueId();
  const [devices, setDevices] = createSignal<Devices>({ status: "loading" });
  // `undefined` until the browser answered; `null` when it holds no subscription.
  const [mine, setMine] = createSignal<DeviceSubscription | null>();
  const [permission, setPermission] = createSignal<Permission>(app.device.permission());
  const [busy, setBusy] = createSignal(false);
  const [problem, setProblem] = createSignal<string>();
  const [notice, setNotice] = createSignal<string>();

  const listed = (): readonly PushSubscriptionInfo[] | undefined => {
    const state = devices();
    return state.status === "ready" ? state.items : undefined;
  };
  const devicesError = (): string | undefined => {
    const state = devices();
    return state.status === "error" ? state.message : undefined;
  };
  /** The server's entry for this browser, matched by the hash of its endpoint. */
  const thisDevice = createMemo((): PushSubscriptionInfo | undefined => {
    const current = mine();
    if (!current) return undefined;
    return listed()?.find((one) => one.endpoint_sha256 === current.endpointSha256);
  });
  const known = (): boolean => mine() !== undefined && listed() !== undefined;

  async function loadDevices(): Promise<readonly PushSubscriptionInfo[] | undefined> {
    try {
      const items = await app.listPushSubscriptions();
      setDevices({ status: "ready", items });
      return items;
    } catch (error) {
      if (listed() === undefined) setDevices({ status: "error", message: errorMessage(error) });
      return undefined;
    }
  }

  async function readSubscription(): Promise<DeviceSubscription | null> {
    try {
      return await app.device.subscription();
    } catch {
      return null;
    }
  }

  /**
   * The server's key changed since this browser subscribed: the old
   * subscription can no longer be used, so it is replaced without asking.
   */
  async function renewIfKeyChanged(
    current: DeviceSubscription | null,
    items: readonly PushSubscriptionInfo[] | undefined,
  ): Promise<void> {
    if (!current || current.publicKey === null || permission() !== "granted" || !items) return;
    const old = items.find((one) => one.endpoint_sha256 === current.endpointSha256);
    // Removed from the server (from another device, say): not this page's to bring back.
    if (!old) return;
    try {
      const key = await app.pushKey();
      if (sameKey(current.publicKey, key)) return;
      await current.unsubscribe();
      const renewed = await app.device.subscribe(key);
      await app.registerPushSubscription(renewed.request);
      await removeQuietly(app, old.id);
      setMine(renewed);
      await loadDevices();
    } catch (error) {
      setMine(await readSubscription());
      setProblem(
        `Could not renew this device after the server's key changed: ${errorMessage(error)}`,
      );
    }
  }

  async function load(): Promise<void> {
    const [items, current] = await Promise.all([loadDevices(), readSubscription()]);
    setMine(current);
    await renewIfKeyChanged(current, items);
  }
  onSettled(() => void load());

  async function notify(): Promise<void> {
    if (busy()) return;
    setBusy(true);
    setProblem(undefined);
    setNotice(undefined);
    try {
      // First, while the click still counts as the user's: browsers only ask then.
      let answer: Permission;
      try {
        answer = await app.device.requestPermission();
      } catch {
        answer = app.device.permission();
      }
      setPermission(answer);
      if (answer === "denied") {
        setProblem(PROBLEMS.denied);
        return;
      }
      if (answer !== "granted") {
        setProblem(PROBLEMS.dismissed);
        return;
      }
      let key: string;
      try {
        key = await app.pushKey();
      } catch (error) {
        setProblem(`The server did not give its notification key: ${errorMessage(error)}`);
        return;
      }
      let subscription: DeviceSubscription;
      try {
        // A subscription made with another key cannot be reused for this one.
        const existing = mine();
        if (existing?.publicKey && !sameKey(existing.publicKey, key)) await existing.unsubscribe();
        subscription = await app.device.subscribe(key);
      } catch (error) {
        setProblem(`This browser could not subscribe to notifications: ${errorMessage(error)}`);
        return;
      }
      setMine(subscription);
      try {
        await app.registerPushSubscription(subscription.request);
      } catch (error) {
        setProblem(`The server did not accept this device: ${errorMessage(error)}`);
        return;
      }
      await loadDevices();
      setNotice("This device will be notified.");
    } finally {
      setBusy(false);
    }
  }

  /** Both halves: the browser's subscription and the server's entry. Says which one failed. */
  async function stop(): Promise<string | undefined> {
    if (busy()) return undefined;
    setBusy(true);
    setProblem(undefined);
    setNotice(undefined);
    const current = mine();
    const entry = thisDevice();
    try {
      const unsubscribed = current ? await current.unsubscribe() : true;
      const serverError = entry ? await removeQuietly(app, entry.id) : undefined;
      if (unsubscribed) setMine(null);
      await loadDevices();
      let message: string | undefined;
      if (!unsubscribed && serverError !== undefined) {
        message = `Nothing changed: this browser could not unsubscribe, and the server refused to remove this device: ${serverError}`;
      } else if (!unsubscribed) {
        message =
          "The server no longer notifies this device, but this browser could not unsubscribe. Block notifications for this site in the browser's settings to be sure.";
      } else if (serverError !== undefined) {
        message = `This browser unsubscribed, but the server still lists this device: ${serverError}`;
      }
      if (message === undefined) setNotice("This device is no longer notified.");
      else setProblem(message);
      return undefined;
    } finally {
      setBusy(false);
    }
  }

  async function removeDevice(device: PushSubscriptionInfo): Promise<string | undefined> {
    if (device.id === thisDevice()?.id) return stop();
    const problem = await removeQuietly(app, device.id);
    if (problem !== undefined) return `Could not remove it: ${problem}`;
    await loadDevices();
    return undefined;
  }

  return (
    <>
      <div class="flex flex-col gap-2">
        <Switch>
          <Match when={!known()}>
            <p class="text-sm text-muted">Checking this device…</p>
          </Match>
          <Match when={thisDevice()}>
            <div class="flex flex-wrap items-center justify-between gap-2">
              <p class="text-sm font-medium text-tone-success">This device is notified</p>
              <button type="button" class="btn" disabled={busy()} onClick={() => void stop()}>
                {busy() ? "Stopping…" : "Stop notifying this device"}
              </button>
            </div>
          </Match>
          <Match when={true}>
            <p class="text-sm">
              Get a notification on this device when a session needs you, even with Calin closed.
            </p>
            <div>
              <button
                type="button"
                class="btn btn-primary"
                disabled={busy()}
                onClick={() => void notify()}
              >
                {busy() ? "Asking…" : "Notify this device"}
              </button>
            </div>
            <Show when={permission() === "denied" && problem() === undefined}>
              <p class={hint}>{PROBLEMS.denied}</p>
            </Show>
          </Match>
        </Switch>
        <Show when={problem()}>
          {(message) => (
            <p role="alert" class={fieldError}>
              {message()}
            </p>
          )}
        </Show>
        <p role="status" class="text-sm text-tone-success empty:hidden">
          {notice() ?? ""}
        </p>
      </div>

      <section aria-labelledby={devicesId} class="flex flex-col gap-2">
        <h3 id={devicesId} class="text-sm font-medium">
          Notified devices
        </h3>
        <Switch>
          <Match when={devicesError()}>
            {(message) => (
              <LoadFailure
                what="The notified devices"
                message={message()}
                onRetry={() => void loadDevices()}
              />
            )}
          </Match>
          <Match when={listed()}>
            {(items) => (
              <Show
                when={items().length > 0}
                fallback={<p class="text-sm text-muted">No device is notified yet.</p>}
              >
                <ul
                  aria-label="Notified devices"
                  class="divide-y divide-line rounded-md border border-line"
                >
                  <For each={items()} keyed={(device) => device.id}>
                    {(device) => (
                      <DeviceRow
                        device={device}
                        mine={device().id === thisDevice()?.id}
                        now={app.now()}
                        onRemove={() => removeDevice(device())}
                      />
                    )}
                  </For>
                </ul>
              </Show>
            )}
          </Match>
          <Match when={true}>
            <p class="text-sm text-muted">Reading the notified devices…</p>
          </Match>
        </Switch>
      </section>

      <Preferences headingId={preferencesId} />
      <TestNotification />
    </>
  );
}

/** Settings › Notifications: Web Push to this and the identity's other devices (spec section 6). */
export function NotificationSettings() {
  const app = useContext(AppContext);
  const headingId = createUniqueId();
  const unavailable = (): string | undefined => {
    if (app.auth().status !== "signed_in") return UNAVAILABLE.local;
    switch (app.device.pushSupport()) {
      case "needs-install":
        return UNAVAILABLE.needsInstall;
      case "unsupported":
        return UNAVAILABLE.unsupported;
      case "no-worker":
        return UNAVAILABLE.noWorker;
      default:
        return undefined;
    }
  };
  return (
    <section aria-labelledby={headingId} class="flex flex-col gap-4 border-t border-line pt-4">
      <h2 id={headingId} class="text-sm font-medium">
        Notifications
      </h2>
      <Show when={unavailable()} fallback={<NotificationControls />}>
        {(why) => <p class="text-sm text-muted">{why()}</p>}
      </Show>
    </section>
  );
}

/** How to add the app to the home screen; not shown once it runs from there. */
export function InstallHint() {
  const app = useContext(AppContext);
  const headingId = createUniqueId();
  return (
    <Show when={!app.device.standalone()}>
      <section aria-labelledby={headingId} class="flex flex-col gap-2 border-t border-line pt-4">
        <h2 id={headingId} class="text-sm font-medium">
          Install
        </h2>
        <p class="text-sm">
          Add Calin to your phone's home screen to open it full screen, like an app.
        </p>
        <ul class="list-disc pl-5 text-sm">
          <li>
            Android: open the browser's menu and choose <em>Install app</em> or{" "}
            <em>Add to Home screen</em>.
          </li>
          <li>
            iPhone and iPad: tap <em>Share</em>, then <em>Add to Home Screen</em>. On iOS,
            notifications only work once Calin is installed this way.
          </li>
        </ul>
      </section>
    </Show>
  );
}
