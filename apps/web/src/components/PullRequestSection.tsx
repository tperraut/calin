import { createMemo, For, Show } from "solid-js";
import {
  needsAttention,
  pullRequestHref,
  repoNames,
  rollUp,
  sendBackMessage,
} from "../lib/delivery";
import { absoluteTime, relativeTime } from "../lib/format";
import { createRequest } from "../lib/request";
import type { SessionStore } from "../state/sessionStore";
import { Badge } from "./Badge";
import { PullRequestLink, pullRequestLooks } from "./PullRequests";

/**
 * The session's pull requests in full, for the Changes panel: what stands out
 * in one line, then each one with its state, checks and review. "Send back to
 * the agent" writes the message and leaves the sending to the reader.
 */
export function PullRequestSection(props: {
  store: SessionStore;
  /** Puts a draft in the message box; absent where there is none to fill. */
  onSendBack?: (text: string) => void;
}) {
  const delivery = () => props.store.delivery;
  const pullRequests = createMemo(() => delivery().pullRequests());
  const nameOf = (id: string) => repoNames(props.store.repos())(id);
  const refresh = createRequest(() => delivery().refresh());
  const newest = (): number =>
    pullRequests().reduce((latest, one) => Math.max(latest, one.updated_at), 0);
  // api.rs, "While `shipping`": a message starts a turn then, so there is someone to send it to.
  const sendBack = (): string =>
    props.store.status() === "shipping" && pullRequests().some(needsAttention)
      ? sendBackMessage(pullRequests(), nameOf)
      : "";

  return (
    <div role="region" aria-label="Pull requests" class="border-b border-line">
      <div class="flex items-center gap-2 bg-surface pl-3">
        <div class="min-w-0 flex-1 py-1.5">
          <h3 class="text-xs font-medium text-muted">Pull requests</h3>
          <p data-part="roll-up" class="text-sm font-medium wrap-break-word">
            {rollUp(pullRequests(), nameOf)}
          </p>
          <p class="text-xs text-muted">
            Updated{" "}
            <time datetime={new Date(newest()).toISOString()} title={absoluteTime(newest())}>
              {relativeTime(newest(), delivery().now())}
            </time>
          </p>
        </div>
        <button
          type="button"
          class="btn mr-1 px-2.5 text-sm"
          aria-label="Refresh pull requests"
          title="Ask GitHub for the state of the pull requests now"
          disabled={refresh.busy()}
          onClick={() => void refresh.run()}
        >
          {refresh.busy() ? "Refreshing…" : "Refresh"}
        </button>
      </div>
      <Show when={refresh.error()}>
        {(message) => (
          <p role="alert" class="border-t border-line bg-danger-soft px-3 py-2 text-sm">
            The pull requests could not be refreshed: {message()}
          </p>
        )}
      </Show>
      <ul class="divide-y divide-line border-t border-line">
        <For each={pullRequests()} keyed={(one) => one.session_repo_id}>
          {(pullRequest) => (
            <li class="flex min-h-11 flex-wrap items-center gap-x-2 gap-y-1 px-3 py-1 text-sm">
              <span class="max-w-[50%] truncate font-semibold">
                {nameOf(pullRequest().session_repo_id)}
              </span>{" "}
              <PullRequestLink
                pullRequest={pullRequest()}
                repo={nameOf(pullRequest().session_repo_id)}
              />
              <span class="ml-auto flex flex-wrap justify-end gap-1">
                <For each={pullRequestLooks(pullRequest())}>
                  {(look) => (
                    <>
                      {" "}
                      <Badge look={look} />
                    </>
                  )}
                </For>
              </span>
              <Show when={pullRequestHref(pullRequest().url) === undefined}>
                <span class="basis-full pb-1 font-mono text-xs wrap-anywhere text-muted">
                  {pullRequest().url}
                </span>
              </Show>
            </li>
          )}
        </For>
      </ul>
      <Show when={sendBack() !== "" && props.onSendBack}>
        {(onSendBack) => (
          <div class="flex flex-wrap items-center justify-between gap-2 border-t border-line bg-attention-soft px-3 py-2">
            <p class="min-w-0 flex-1 basis-40 text-sm">
              Something needs fixing before these can merge.
            </p>
            <button type="button" class="btn" onClick={() => onSendBack()(sendBack())}>
              Send back to the agent
            </button>
          </div>
        )}
      </Show>
    </div>
  );
}
