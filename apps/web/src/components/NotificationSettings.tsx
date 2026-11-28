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
  type Setter,
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

/** What to tell the user when the browser's answer is not yes. */
function permissionProblem(answer: Permission): string | undefined {
  if (answer === "denied") return PROBLEMS.denied;
  return answer === "granted" ? undefined : PROBLEMS.dismissed;
}

/** Stopping has two halves, the browser's and the server's: says which one failed. */
function stopProblem(unsubscribed: boolean, serverError: string | undefined): string | undefined {
  if (!unsubscribed && serverError !== undefined) {
    return `Nothing changed: this browser could not unsubscribe, and the server refused to remove this device: ${serverError}`;
  }
  if (!unsubscribed) {
    return "The server no longer notifies this device, but this browser could not unsubscribe. Block notifications for this site in the browser's settings to be sure.";
  }
  if (serverError !== undefined) {
    return `This browser unsubscribed, but the server still lists this device: ${serverError}`;
  }
  return undefined;
}

/** The server's entry for `current`, matched by the hash of its endpoint. */
function entryFor(
  items: readonly PushSubscriptionInfo[] | undefined,
  current: DeviceSubscription | null | undefined,
): PushSubscriptionInfo | undefined {
  if (!current) return undefined;
  return items?.find((one) => one.endpoint_sha256 === current.endpointSha256);
}

/** This browser's subscription and the server's list of notified devices, kept in step. */
class PushControls {
  readonly devices: Accessor<Devices>;
  /** `undefined` until the browser answered; `null` when it holds no subscription. */
  readonly mine: Accessor<DeviceSubscription | null | undefined>;
  readonly permission: Accessor<Permission>;
  readonly busy: Accessor<boolean>;
  readonly problem: Accessor<string | undefined>;
  readonly notice: Accessor<string | undefined>;
  /** The server's entry for this browser. */
  readonly thisDevice: Accessor<PushSubscriptionInfo | undefined>;
  private readonly setDevices: Setter<Devices>;
  private readonly setMine: Setter<DeviceSubscription | null | undefined>;
  private readonly setPermission: Setter<Permission>;
  private readonly setBusy: Setter<boolean>;
  private readonly setProblem: Setter<string | undefined>;
  private readonly setNotice: Setter<string | undefined>;
  private readonly app: AppState;

  constructor(app: AppState) {
    this.app = app;
    [this.devices, this.setDevices] = createSignal<Devices>({ status: "loading" });
    [this.mine, this.setMine] = createSignal<DeviceSubscription | null>();
    [this.permission, this.setPermission] = createSignal<Permission>(app.device.permission());
    [this.busy, this.setBusy] = createSignal(false);
    [this.problem, this.setProblem] = createSignal<string>();
    [this.notice, this.setNotice] = createSignal<string>();
    this.thisDevice = createMemo(() => entryFor(this.listed(), this.mine()));
  }

  listed(): readonly PushSubscriptionInfo[] | undefined {
    const state = this.devices();
    return state.status === "ready" ? state.items : undefined;
  }

  devicesError(): string | undefined {
    const state = this.devices();
    return state.status === "error" ? state.message : undefined;
  }

  known(): boolean {
    return this.mine() !== undefined && this.listed() !== undefined;
  }

  isThisDevice(device: PushSubscriptionInfo): boolean {
    return device.id === this.thisDevice()?.id;
  }

  async load(): Promise<void> {
    const [items, current] = await Promise.all([this.loadDevices(), this.readSubscription()]);
    this.setMine(current);
    await this.renewIfKeyChanged(current, items);
  }

  async loadDevices(): Promise<readonly PushSubscriptionInfo[] | undefined> {
    try {
      const items = await this.app.listPushSubscriptions();
      this.setDevices({ status: "ready", items });
      return items;
    } catch (error) {
      if (this.listed() === undefined) {
        this.setDevices({ status: "error", message: errorMessage(error) });
      }
      return undefined;
    }
  }

  async notify(): Promise<void> {
    if (this.busy()) return;
    this.begin();
    try {
      const problem = await this.enable();
      if (problem !== undefined) {
        this.setProblem(problem);
        return;
      }
      await this.loadDevices();
      this.setNotice("This device will be notified.");
    } finally {
      this.setBusy(false);
    }
  }

  /** Both halves: the browser's subscription and the server's entry. */
  async stop(): Promise<string | undefined> {
    if (this.busy()) return undefined;
    this.begin();
    const current = this.mine();
    const entry = this.thisDevice();
    try {
      const unsubscribed = current ? await current.unsubscribe() : true;
      const serverError = entry ? await removeQuietly(this.app, entry.id) : undefined;
      if (unsubscribed) this.setMine(null);
      await this.loadDevices();
      const problem = stopProblem(unsubscribed, serverError);
      if (problem === undefined) this.setNotice("This device is no longer notified.");
      else this.setProblem(problem);
      return undefined;
    } finally {
      this.setBusy(false);
    }
  }

  async removeDevice(device: PushSubscriptionInfo): Promise<string | undefined> {
    if (this.isThisDevice(device)) return this.stop();
    const problem = await removeQuietly(this.app, device.id);
    if (problem !== undefined) return `Could not remove it: ${problem}`;
    await this.loadDevices();
    return undefined;
  }

  private begin(): void {
    this.setBusy(true);
    this.setProblem(undefined);
    this.setNotice(undefined);
  }

  private async readSubscription(): Promise<DeviceSubscription | null> {
    try {
      return await this.app.device.subscription();
    } catch {
      return null;
    }
  }

  /**
   * The server's key changed since this browser subscribed: the old
   * subscription can no longer be used, so it is replaced without asking.
   */
  private async renewIfKeyChanged(
    current: DeviceSubscription | null,
    items: readonly PushSubscriptionInfo[] | undefined,
  ): Promise<void> {
    if (!current || current.publicKey === null || this.permission() !== "granted") return;
    const old = entryFor(items, current);
    // Removed from the server (from another device, say): not this page's to bring back.
    if (!old) return;
    try {
      const key = await this.app.pushKey();
      if (sameKey(current.publicKey, key)) return;
      await current.unsubscribe();
      const renewed = await this.app.device.subscribe(key);
      await this.app.registerPushSubscription(renewed.request);
      await removeQuietly(this.app, old.id);
      this.setMine(renewed);
      await this.loadDevices();
    } catch (error) {
      this.setMine(await this.readSubscription());
      this.setProblem(
        `Could not renew this device after the server's key changed: ${errorMessage(error)}`,
      );
    }
  }

  /** Asks, subscribes and registers; what went wrong, if anything. */
  private async enable(): Promise<string | undefined> {
    // First, while the click still counts as the user's: browsers only ask then.
    const answer = await this.askPermission();
    this.setPermission(answer);
    const refused = permissionProblem(answer);
    if (refused !== undefined) return refused;
    let key: string;
    try {
      key = await this.app.pushKey();
    } catch (error) {
      return `The server did not give its notification key: ${errorMessage(error)}`;
    }
    let subscription: DeviceSubscription;
    try {
      subscription = await this.subscribeWith(key);
    } catch (error) {
      return `This browser could not subscribe to notifications: ${errorMessage(error)}`;
    }
    this.setMine(subscription);
    try {
      await this.app.registerPushSubscription(subscription.request);
    } catch (error) {
      return `The server did not accept this device: ${errorMessage(error)}`;
    }
    return undefined;
  }

  private async askPermission(): Promise<Permission> {
    try {
      return await this.app.device.requestPermission();
    } catch {
      return this.app.device.permission();
    }
  }

  private async subscribeWith(key: string): Promise<DeviceSubscription> {
    // A subscription made with another key cannot be reused for this one.
    const existing = this.mine();
    if (existing?.publicKey && !sameKey(existing.publicKey, key)) await existing.unsubscribe();
    return this.app.device.subscribe(key);
  }
}

function NotifyThisDevice(props: { controls: PushControls }) {
  return (
    <>
      <p class="text-sm">
        Get a notification on this device when a session needs you, even with Calin closed.
      </p>
      <div>
        <button
          type="button"
          class="btn btn-primary"
          disabled={props.controls.busy()}
          onClick={() => void props.controls.notify()}
        >
          {props.controls.busy() ? "Asking…" : "Notify this device"}
        </button>
      </div>
      <Show
        when={props.controls.permission() === "denied" && props.controls.problem() === undefined}
      >
        <p class={hint}>{PROBLEMS.denied}</p>
      </Show>
    </>
  );
}

function ThisDevice(props: { controls: PushControls }) {
  return (
    <div class="flex flex-col gap-2">
      <Switch>
        <Match when={!props.controls.known()}>
          <p class="text-sm text-muted">Checking this device…</p>
        </Match>
        <Match when={props.controls.thisDevice()}>
          <div class="flex flex-wrap items-center justify-between gap-2">
            <p class="text-sm font-medium text-tone-success">This device is notified</p>
            <button
              type="button"
              class="btn"
              disabled={props.controls.busy()}
              onClick={() => void props.controls.stop()}
            >
              {props.controls.busy() ? "Stopping…" : "Stop notifying this device"}
            </button>
          </div>
        </Match>
        <Match when={true}>
          <NotifyThisDevice controls={props.controls} />
        </Match>
      </Switch>
      <Show when={props.controls.problem()}>
        {(message) => (
          <p role="alert" class={fieldError}>
            {message()}
          </p>
        )}
      </Show>
      <p role="status" class="text-sm text-tone-success empty:hidden">
        {props.controls.notice() ?? ""}
      </p>
    </div>
  );
}

function NotifiedDevices(props: { controls: PushControls }) {
  const app = useContext(AppContext);
  const headingId = createUniqueId();
  return (
    <section aria-labelledby={headingId} class="flex flex-col gap-2">
      <h3 id={headingId} class="text-sm font-medium">
        Notified devices
      </h3>
      <Switch>
        <Match when={props.controls.devicesError()}>
          {(message) => (
            <LoadFailure
              what="The notified devices"
              message={message()}
              onRetry={() => void props.controls.loadDevices()}
            />
          )}
        </Match>
        <Match when={props.controls.listed()}>
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
                      mine={props.controls.isThisDevice(device())}
                      now={app.now()}
                      onRemove={() => props.controls.removeDevice(device())}
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
  );
}

/** Everything that needs this browser to support push, in public mode. */
function NotificationControls() {
  const controls = new PushControls(useContext(AppContext));
  const preferencesId = createUniqueId();
  onSettled(() => void controls.load());
  return (
    <>
      <ThisDevice controls={controls} />
      <NotifiedDevices controls={controls} />
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
