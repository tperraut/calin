import { createSignal } from "solid-js";
import type { PermissionDecision } from "../gen/PermissionDecision";
import { errorMessage } from "../lib/errors";

/**
 * The answers given to permission requests on one session screen. A request
 * can be answered from the pinned bar or from its tool call's card: both ask
 * here, so an answer given in one place holds the buttons of the other.
 * Nothing is optimistic: a request is over when its `permission_resolved` event arrives.
 */
export type PermissionAnswers = {
  /** True from the tap until the request's resolution arrives: asking twice would be a 409. */
  waiting(requestId: string): boolean;
  /** True while the answer is on its way to the server. */
  sending(requestId: string): boolean;
  /** The server's reason when the last answer to this request was refused. */
  error(requestId: string): string | undefined;
  /** Sends the answer; resolves to whether the server took it. `message` goes to the agent with a denial. */
  answer(requestId: string, decision: PermissionDecision, message?: string): Promise<boolean>;
};

export function createPermissionAnswers(
  resolve: (requestId: string, decision: PermissionDecision, message?: string) => Promise<void>,
): PermissionAnswers {
  const [inFlight, setInFlight] = createSignal<ReadonlySet<string>>(new Set());
  // Answers the server accepted whose event has not arrived yet.
  const [sent, setSent] = createSignal<ReadonlySet<string>>(new Set());
  const [failures, setFailures] = createSignal<ReadonlyMap<string, string>>(new Map());
  // Signals show a tick later; two taps in one tick must not send two answers.
  const busy = new Set<string>();

  return {
    waiting: (requestId) => inFlight().has(requestId) || sent().has(requestId),
    sending: (requestId) => inFlight().has(requestId),
    error: (requestId) => failures().get(requestId),
    async answer(requestId, decision, message) {
      if (busy.has(requestId)) return false;
      busy.add(requestId);
      setInFlight((ids) => new Set(ids).add(requestId));
      setFailures((all) => {
        const next = new Map(all);
        next.delete(requestId);
        return next;
      });
      try {
        await resolve(requestId, decision, message);
        setSent((ids) => new Set(ids).add(requestId));
        return true;
      } catch (cause) {
        busy.delete(requestId);
        setFailures((all) => new Map(all).set(requestId, errorMessage(cause)));
        return false;
      } finally {
        setInFlight((ids) => {
          const next = new Set(ids);
          next.delete(requestId);
          return next;
        });
      }
    },
  };
}
