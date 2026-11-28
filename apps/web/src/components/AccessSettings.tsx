import {
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
import type { AuthSessionInfo } from "../gen/AuthSessionInfo";
import type { CreatedDeviceToken } from "../gen/CreatedDeviceToken";
import type { DeviceTokenInfo } from "../gen/DeviceTokenInfo";
import { describeUserAgent, tokenNameProblem } from "../lib/auth";
import { errorMessage } from "../lib/errors";
import { absoluteTime, relativeTime } from "../lib/format";
import { onFormSubmit } from "../lib/submit";
import { AppContext } from "../state/app";
import { AccountControls } from "./SignIn";

const fieldError = "text-sm wrap-break-word text-tone-danger";

type Listing<T> =
  | { status: "loading" }
  | { status: "ready"; items: readonly T[] }
  | { status: "error"; message: string };

/** A list read from the server, read again on demand; what was read stays through a failed reread. */
function createListing<T>(read: () => Promise<T[]>) {
  const [listing, setListing] = createSignal<Listing<T>>({ status: "loading" });
  let known = false;
  function load(): void {
    read().then(
      (items) => {
        known = true;
        setListing({ status: "ready", items });
      },
      (error: unknown) => {
        if (!known) setListing({ status: "error", message: errorMessage(error) });
      },
    );
  }
  const items = (): readonly T[] | undefined => {
    const state = listing();
    return state.status === "ready" ? state.items : undefined;
  };
  const failure = (): string | undefined => {
    const state = listing();
    return state.status === "error" ? state.message : undefined;
  };
  return { listing, items, failure, load };
}

export function When(props: { label: string; ts: number; now: number }) {
  return (
    <span>
      {props.label}{" "}
      <time datetime={new Date(props.ts).toISOString()} title={absoluteTime(props.ts)}>
        {relativeTime(props.ts, props.now)}
      </time>
    </span>
  );
}

export function LoadFailure(props: { what: string; message: string; onRetry: () => void }) {
  return (
    <p role="alert" class="flex items-center justify-between gap-2 text-sm">
      <span class="min-w-0">
        {props.what} could not be loaded: {props.message}
      </span>
      <button type="button" class="btn" onClick={() => props.onRetry()}>
        Retry
      </button>
    </p>
  );
}

function SessionRow(props: {
  session: Accessor<AuthSessionInfo>;
  now: number;
  onSignOut: () => Promise<void>;
}) {
  const [busy, setBusy] = createSignal(false);
  const [failure, setFailure] = createSignal<string>();
  const description = (): string => describeUserAgent(props.session().user_agent);

  async function signOut(): Promise<void> {
    if (busy()) return;
    setBusy(true);
    setFailure(undefined);
    try {
      await props.onSignOut();
    } catch (error) {
      setFailure(errorMessage(error));
    } finally {
      setBusy(false);
    }
  }

  return (
    <li
      class="flex flex-col gap-1 px-3 py-2"
      data-current={props.session().current ? "" : undefined}
    >
      <div class="flex items-center gap-2">
        <div class="min-w-0 flex-1">
          <p class="flex min-w-0 flex-wrap items-center gap-x-2">
            <span
              class="min-w-0 truncate font-medium"
              title={props.session().user_agent ?? undefined}
            >
              {description()}
            </span>
            <Show when={props.session().current}>
              <span class="rounded-full bg-accent-soft px-2 text-xs font-medium text-accent">
                This browser
              </span>
            </Show>
          </p>
          <p class="flex flex-wrap gap-x-2 text-xs text-muted">
            <When label="Signed in" ts={props.session().created_at} now={props.now} />
            <When label="Last used" ts={props.session().last_used_at} now={props.now} />
          </p>
        </div>
        <button
          type="button"
          class="btn shrink-0"
          aria-label={
            props.session().current ? "Sign out this browser" : `Sign out ${description()}`
          }
          disabled={busy()}
          onClick={() => void signOut()}
        >
          Sign out
        </button>
      </div>
      <Show when={failure()}>
        {(message) => (
          <p role="alert" class={fieldError}>
            Could not sign it out: {message()}
          </p>
        )}
      </Show>
    </li>
  );
}

function SignedInBrowsers(props: { headingId: string }) {
  const app = useContext(AppContext);
  const sessions = createListing(() => app.listAuthSessions());
  onSettled(() => sessions.load());

  async function signOut(session: AuthSessionInfo): Promise<void> {
    if (session.current) {
      await app.signOut();
      return;
    }
    await app.revokeAuthSession(session.id);
    sessions.load();
  }

  return (
    <section aria-labelledby={props.headingId} class="flex flex-col gap-2">
      <h3 id={props.headingId} class="text-sm font-medium">
        Signed-in browsers
      </h3>
      <Switch>
        <Match when={sessions.failure()}>
          {(message) => (
            <LoadFailure
              what="The signed-in browsers"
              message={message()}
              onRetry={() => sessions.load()}
            />
          )}
        </Match>
        <Match when={sessions.items()}>
          {(items) => (
            <ul
              aria-label="Signed-in browsers"
              class="divide-y divide-line rounded-md border border-line"
            >
              <For each={items()} keyed={(session) => session.id}>
                {(session) => (
                  <SessionRow
                    session={session}
                    now={app.now()}
                    onSignOut={() => signOut(session())}
                  />
                )}
              </For>
            </ul>
          )}
        </Match>
        <Match when={true}>
          <p class="text-sm text-muted">Reading the signed-in browsers…</p>
        </Match>
      </Switch>
    </section>
  );
}

function TokenRow(props: {
  token: Accessor<DeviceTokenInfo>;
  now: number;
  onRevoke: () => Promise<void>;
}) {
  const [confirming, setConfirming] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [failure, setFailure] = createSignal<string>();
  let confirmButton: HTMLButtonElement | undefined;
  let revokeButton: HTMLButtonElement | undefined;

  function ask(): void {
    setFailure(undefined);
    setConfirming(true);
    queueMicrotask(() => confirmButton?.focus());
  }

  function cancel(): void {
    setConfirming(false);
    queueMicrotask(() => revokeButton?.focus());
  }

  async function revoke(): Promise<void> {
    if (busy()) return;
    setBusy(true);
    setFailure(undefined);
    try {
      await props.onRevoke();
    } catch (error) {
      setFailure(errorMessage(error));
      setConfirming(false);
    } finally {
      setBusy(false);
    }
  }

  return (
    <li class="flex flex-col gap-1 px-3 py-2">
      <div class="flex items-center gap-2">
        <div class="min-w-0 flex-1">
          <p class="truncate font-medium" title={props.token().name}>
            {props.token().name}
          </p>
          <p class="flex flex-wrap gap-x-2 text-xs text-muted">
            <When label="Created" ts={props.token().created_at} now={props.now} />
            <Show when={props.token().last_used_at} fallback={<span>Never used</span>}>
              {(used) => <When label="Last used" ts={used()} now={props.now} />}
            </Show>
          </p>
        </div>
        <Show when={!confirming()}>
          <button
            ref={(element) => (revokeButton = element)}
            type="button"
            class="btn shrink-0"
            aria-label={`Revoke ${props.token().name}`}
            onClick={ask}
          >
            Revoke
          </button>
        </Show>
      </div>
      <Show when={confirming()}>
        <div
          role="group"
          aria-label={`Revoke ${props.token().name}?`}
          class="flex flex-wrap items-center justify-end gap-2 rounded-md bg-danger-soft px-2 py-1.5"
        >
          <p class="min-w-0 flex-1 basis-40 text-sm">
            Anything still using this token loses access at once.
          </p>
          <button type="button" class="btn" disabled={busy()} onClick={cancel}>
            Cancel
          </button>
          <button
            ref={(element) => (confirmButton = element)}
            type="button"
            class="btn border-tone-danger text-tone-danger"
            aria-label={`Confirm revoking ${props.token().name}`}
            disabled={busy()}
            onClick={() => void revoke()}
          >
            {busy() ? "Revoking…" : "Revoke"}
          </button>
        </div>
      </Show>
      <Show when={failure()}>
        {(message) => (
          <p role="alert" class={fieldError}>
            Could not revoke it: {message()}
          </p>
        )}
      </Show>
    </li>
  );
}

/**
 * The secret of a token just created, the only place it is ever rendered.
 * It lives in the parent's memory until Done or until the page goes away.
 */
function OneTimeSecret(props: { created: CreatedDeviceToken; onDone: () => void }) {
  const [copied, setCopied] = createSignal<"copied" | "manual">();
  let secret: HTMLElement | undefined;
  let copyButton: HTMLButtonElement | undefined;
  onSettled(() => copyButton?.focus());

  function selectSecret(): void {
    const selection = window.getSelection();
    if (!secret || !selection) return;
    selection.selectAllChildren(secret);
  }

  function copy(): void {
    // Called straight from the click: browsers only allow clipboard writes in a user gesture.
    const clipboard = typeof navigator === "undefined" ? undefined : navigator.clipboard;
    if (!clipboard) {
      selectSecret();
      setCopied("manual");
      return;
    }
    clipboard.writeText(props.created.token).then(
      () => setCopied("copied"),
      () => {
        selectSecret();
        setCopied("manual");
      },
    );
  }

  return (
    <div
      role="region"
      aria-label="New device token"
      class="flex flex-col gap-2 rounded-md border-2 border-accent bg-surface p-3"
    >
      <p class="text-sm">
        Token for <span class="font-medium wrap-anywhere">{props.created.info.name}</span>
      </p>
      <code
        ref={(element) => (secret = element)}
        data-secret=""
        class="block rounded bg-sunken px-2 py-2 font-mono text-sm break-all select-all"
      >
        {props.created.token}
      </code>
      <p class="text-sm font-medium">This is the only time the token is shown.</p>
      <p role="status" class="text-sm text-muted empty:hidden">
        {copied() === "copied"
          ? "Copied."
          : copied() === "manual"
            ? "Copying is not available here: the token is selected, copy it yourself."
            : ""}
      </p>
      <div class="flex justify-end gap-2">
        <button
          ref={(element) => (copyButton = element)}
          type="button"
          class="btn"
          aria-label="Copy token"
          onClick={copy}
        >
          Copy
        </button>
        <button type="button" class="btn btn-primary" onClick={() => props.onDone()}>
          Done
        </button>
      </div>
    </div>
  );
}

function DeviceTokens(props: { headingId: string }) {
  const app = useContext(AppContext);
  const tokens = createListing(() => app.listDeviceTokens());
  const [name, setName] = createSignal("");
  const [problem, setProblem] = createSignal<string>();
  const [busy, setBusy] = createSignal(false);
  // The secret, in memory only and only until Done or until this section unmounts.
  const [created, setCreated] = createSignal<CreatedDeviceToken>();
  const nameId = createUniqueId();
  const problemId = createUniqueId();
  let nameField: HTMLInputElement | undefined;
  onSettled(() => tokens.load());

  async function create(): Promise<void> {
    if (busy()) return;
    const wanted = name().trim();
    const invalid = tokenNameProblem(wanted);
    setProblem(invalid);
    if (invalid !== undefined) {
      nameField?.focus();
      return;
    }
    setBusy(true);
    try {
      setCreated(await app.createDeviceToken(wanted));
      setName("");
      tokens.load();
    } catch (error) {
      setProblem(errorMessage(error));
    } finally {
      setBusy(false);
    }
  }

  function done(): void {
    setCreated(undefined);
    nameField?.focus();
  }

  async function revoke(token: DeviceTokenInfo): Promise<void> {
    await app.revokeDeviceToken(token.id);
    tokens.load();
  }

  return (
    <section aria-labelledby={props.headingId} class="flex flex-col gap-2">
      <h3 id={props.headingId} class="text-sm font-medium">
        Device tokens
      </h3>
      <p class="text-xs text-muted">
        For the desktop app, the <span class="font-mono">calin</span> command and scripts, which
        cannot sign in with a browser.
      </p>
      <Show when={created()}>{(token) => <OneTimeSecret created={token()} onDone={done} />}</Show>
      <Switch>
        <Match when={tokens.failure()}>
          {(message) => (
            <LoadFailure
              what="The device tokens"
              message={message()}
              onRetry={() => tokens.load()}
            />
          )}
        </Match>
        <Match when={tokens.items()}>
          {(items) => (
            <Show
              when={items().length > 0}
              fallback={<p class="text-sm text-muted">No device token yet.</p>}
            >
              <ul
                aria-label="Device tokens"
                class="divide-y divide-line rounded-md border border-line"
              >
                <For each={items()} keyed={(token) => token.id}>
                  {(token) => (
                    <TokenRow token={token} now={app.now()} onRevoke={() => revoke(token())} />
                  )}
                </For>
              </ul>
            </Show>
          )}
        </Match>
        <Match when={true}>
          <p class="text-sm text-muted">Reading the device tokens…</p>
        </Match>
      </Switch>
      <form
        aria-label="Create a device token"
        novalidate
        class="flex flex-col gap-1.5"
        onSubmit={onFormSubmit(create)}
      >
        <label for={nameId} class="text-sm font-medium">
          New token
        </label>
        <div class="flex gap-2">
          <input
            ref={(element) => (nameField = element)}
            id={nameId}
            type="text"
            class="field h-11 min-w-0 flex-1 px-3"
            placeholder="Laptop CLI"
            autocomplete="off"
            value={name()}
            aria-invalid={problem() === undefined ? undefined : "true"}
            aria-describedby={problem() === undefined ? undefined : problemId}
            onInput={(event) => {
              setName(event.currentTarget.value);
              setProblem(undefined);
            }}
          />
          <button type="submit" class="btn btn-primary shrink-0" disabled={busy()}>
            {busy() ? "Creating…" : "Create token"}
          </button>
        </div>
        <Show when={problem()}>
          {(message) => (
            <p id={problemId} role="alert" class={fieldError}>
              {message()}
            </p>
          )}
        </Show>
        <p class="text-xs text-muted">A name to recognize it by, up to 80 characters.</p>
      </form>
    </section>
  );
}

/** Settings › Access, in public mode only: who is signed in, where, and with which tokens. */
export function AccessSettings() {
  const headingId = createUniqueId();
  const browsersId = createUniqueId();
  const tokensId = createUniqueId();
  return (
    <section aria-labelledby={headingId} class="flex flex-col gap-4 border-t border-line pt-4">
      <h2 id={headingId} class="text-sm font-medium">
        Access
      </h2>
      {/* On a phone the header has no room for the account; it lives here instead. */}
      <AccountControls class="lg:hidden" />
      <SignedInBrowsers headingId={browsersId} />
      <DeviceTokens headingId={tokensId} />
    </section>
  );
}
