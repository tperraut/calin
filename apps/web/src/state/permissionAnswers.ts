import { createSignal } from "solid-js";
import type { PermissionDecision } from "../gen/PermissionDecision";
import { errorMessage } from "../lib/errors";

/**
 * The answers given to what an agent or a workflow asks (a permission, a
 * question) on one session screen. Nothing is optimistic: a request is over
 * when its event (`permission_resolved`, `question_answered`) arrives.
 */
export type Answers<A extends string> = {
  /** True from the tap until the request's resolution arrives: asking twice would be a 409. */
  waiting(requestId: string): boolean;
  /** True while the answer is on its way to the server. */
  sending(requestId: string): boolean;
  /** The server's reason when the last answer to this request was refused. */
  error(requestId: string): string | undefined;
  /** Sends the answer; resolves to whether the server took it. `message` goes with it. */
  answer(requestId: string, answer: A, message?: string): Promise<boolean>;
};

/**
 * A permission request can be answered from the pinned bar or from its tool
 * call's card: both ask here, so an answer given in one place holds the
 * buttons of the other. `message` goes to the agent with a denial.
 */
export type PermissionAnswers = Answers<PermissionDecision>;

export const createPermissionAnswers = (
  resolve: (requestId: string, decision: PermissionDecision, message?: string) => Promise<void>,
): PermissionAnswers => createAnswers(resolve);

/** What the answers say of one request; nothing while there is none. */
export function answerState<A extends string>(answers: Answers<A>, id: string | undefined) {
  if (id === undefined) return { waiting: false, sending: false, error: undefined };
  return { waiting: answers.waiting(id), sending: answers.sending(id), error: answers.error(id) };
}

export function createAnswers<A extends string>(
  resolve: (requestId: string, answer: A, message?: string) => Promise<void>,
): Answers<A> {
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
    async answer(requestId, answer, message) {
      if (busy.has(requestId)) return false;
      busy.add(requestId);
      setInFlight((ids) => new Set(ids).add(requestId));
      setFailures((all) => {
        const next = new Map(all);
        next.delete(requestId);
        return next;
      });
      try {
        await resolve(requestId, answer, message);
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
