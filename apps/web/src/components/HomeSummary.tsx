import { createMemo, For, Show } from "solid-js";
import type { RunnerLimits } from "../gen/RunnerLimits";
import type { Session } from "../gen/Session";
import { windowSummary } from "../lib/usage";
import { sessionPath } from "../panels/registry";
import { closestToFull } from "../state/limits";
import { countSessions, firstNeedingYou, needsYouFirst, nestSessions } from "../state/sessionsList";

type Part = { key: string; text: string; href?: string };

/**
 * The parts of the summary line, each left out when it is zero or unknown:
 * "2 need you", "1 running", "3 queued", "5-hour window 62% used". The first
 * links to the first session that needs the user, as the list orders them.
 */
export function summaryParts(
  sessions: readonly Session[],
  limits: readonly RunnerLimits[],
): Part[] {
  const counts = countSessions(sessions);
  const parts: Part[] = [];
  if (counts.needYou > 0) {
    const first = firstNeedingYou(needsYouFirst(nestSessions(sessions)));
    parts.push({
      key: "need",
      text: `${counts.needYou} ${counts.needYou === 1 ? "needs" : "need"} you`,
      href: first ? sessionPath(first.id) : undefined,
    });
  }
  if (counts.running > 0) parts.push({ key: "running", text: `${counts.running} running` });
  if (counts.queued > 0) parts.push({ key: "queued", text: `${counts.queued} queued` });
  const window = closestToFull(limits);
  if (window) parts.push({ key: "limit", text: windowSummary(window) });
  return parts;
}

/** The line above the sessions list (integrations spec, section 4); nothing when it would be empty. */
export function HomeSummary(props: {
  sessions: readonly Session[];
  limits: readonly RunnerLimits[];
}) {
  const parts = createMemo(() => summaryParts(props.sessions, props.limits));
  return (
    <Show when={parts().length > 0}>
      <p
        data-part="home-summary"
        class="flex flex-wrap items-center gap-x-1.5 border-b border-line px-3 py-1 text-sm text-muted"
      >
        <For each={parts()} keyed={(part) => part.key}>
          {(part, index) => (
            <>
              <Show when={index() > 0}>
                <span aria-hidden="true">·</span>
              </Show>
              <Show when={part().href} fallback={<span>{part().text}</span>}>
                {(href) => (
                  <a
                    href={href()}
                    class="flex min-h-11 items-center font-medium text-tone-attention underline"
                  >
                    {part().text}
                  </a>
                )}
              </Show>
            </>
          )}
        </For>
      </p>
    </Show>
  );
}
