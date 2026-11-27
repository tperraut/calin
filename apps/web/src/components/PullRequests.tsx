import { For, Show } from "solid-js";
import type { PullRequest } from "../gen/PullRequest";
import {
  checkLook,
  pullRequestHref,
  pullRequestStateLook,
  repoNames,
  reviewLook,
} from "../lib/delivery";
import type { Look, Tone } from "../lib/status";
import type { SessionStore } from "../state/sessionStore";

const toneText: Record<Tone, string> = {
  neutral: "text-muted",
  active: "text-tone-active",
  attention: "text-tone-attention",
  success: "text-tone-success",
  danger: "text-tone-danger",
};

/** State, then checks and review when something was reported: each one a word, never a color alone. */
export const pullRequestLooks = (pullRequest: PullRequest): Look[] =>
  [
    pullRequestStateLook(pullRequest.state),
    checkLook(pullRequest.checks),
    reviewLook(pullRequest.review),
  ].filter((look) => look !== undefined);

/**
 * A pull request's number, leading to it in a new tab. One that dry run made
 * up has nowhere to lead: it is the same words, not a link.
 */
export function PullRequestLink(props: {
  pullRequest: PullRequest;
  /** The repo it belongs to, for the link's name; absent when the text around says it. */
  repo?: string;
  /** "Pull request #12" instead of "#12". */
  long?: boolean;
  class?: string;
}) {
  const text = (): string =>
    props.long === true
      ? `Pull request #${props.pullRequest.number}`
      : `#${props.pullRequest.number}`;
  const name = (): string =>
    `Pull request #${props.pullRequest.number}` +
    (props.repo === undefined ? "" : ` for ${props.repo}`);
  return (
    <Show
      when={pullRequestHref(props.pullRequest.url)}
      fallback={
        <span
          data-pull-request={props.pullRequest.number}
          class={[{ "font-mono": props.long !== true }, props.class]}
          title={props.pullRequest.url}
        >
          {text()}
        </span>
      }
    >
      {(href) => (
        <a
          data-pull-request={props.pullRequest.number}
          href={href()}
          target="_blank"
          rel="noopener noreferrer"
          aria-label={`${name()} (opens in a new tab)`}
          title={props.pullRequest.title}
          class={[
            "inline-flex min-h-11 min-w-11 items-center text-accent underline decoration-line underline-offset-4 hover:decoration-current",
            { "font-mono": props.long !== true },
            props.class,
          ]}
        >
          {text()}
        </a>
      )}
    </Show>
  );
}

/** The session's pull requests in the header: one short chip each, all on a line that scrolls on a phone. */
export function PullRequestChips(props: { store: SessionStore }) {
  const nameOf = (id: string) => repoNames(props.store.repos())(id);
  return (
    <ul
      aria-label="Pull requests"
      // On a phone they share one line: the chip cut at the edge says there is more to the side.
      class="flex max-w-full min-w-0 items-center gap-1 overflow-x-auto text-xs [scrollbar-width:none] lg:flex-wrap"
    >
      <For each={props.store.delivery.pullRequests()} keyed={(one) => one.session_repo_id}>
        {(pullRequest) => (
          <li class="flex shrink-0 items-center gap-1.5 rounded border border-line px-1.5 whitespace-nowrap lg:py-0.5">
            {/* The spaces are for whoever hears the chip: laid out, the gap does the spacing. */}
            <span class="font-medium text-fg">{nameOf(pullRequest().session_repo_id)}</span>{" "}
            <PullRequestLink
              pullRequest={pullRequest()}
              repo={nameOf(pullRequest().session_repo_id)}
              class="lg:min-h-0 lg:min-w-0"
            />
            <For each={pullRequestLooks(pullRequest())}>
              {(look) => (
                <>
                  {" "}
                  <span class={["font-medium", toneText[look.tone]]}>{look.label}</span>
                </>
              )}
            </For>
          </li>
        )}
      </For>
    </ul>
  );
}
