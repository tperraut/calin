import type { Agent } from "../gen/Agent";
import type { AgentMessage } from "../gen/AgentMessage";
import type { Session } from "../gen/Session";

/** Names a participant of the session tree as the transcript reads it; `agentId` is `null` for the user. */
export type Speaker = (sessionId: string, agentId: string | null) => string;

/**
 * The names the transcript of `session` gives to who sends and receives: "you"
 * for the user, an agent of this session by its role, another session by its title.
 */
export function speakerFor(
  session: Pick<Session, "id" | "parent_session_id"> | undefined,
  agents: readonly Agent[],
  others: readonly Session[],
): Speaker {
  return (sessionId, agentId) => {
    if (agentId === null) return "you";
    if (session === undefined || sessionId === session.id) {
      return agents.find((agent) => agent.id === agentId)?.role ?? "the agent";
    }
    const title = others.find((one) => one.id === sessionId)?.title;
    if (title !== undefined) return title;
    return sessionId === session.parent_session_id ? "the parent session" : "a child session";
  };
}

/**
 * What a message was, in words, from the side of the session that shows it:
 * `sent` when it left this session, `received` when it arrived here (event.rs).
 */
export function messageHeading(
  message: AgentMessage,
  direction: "sent" | "received",
  speaker: Speaker,
): string {
  const from = speaker(message.from_session_id, message.from_agent_id);
  const to = speaker(message.to_session_id, message.to_agent_id);
  switch (message.kind) {
    case "dispatch":
      // api.rs, "Child sessions": the brief that starts a child, sent by its parent.
      return direction === "sent" ? `Dispatched child session ${to}` : `Brief from ${from}`;
    case "note":
      return direction === "sent" ? `Note to ${to}` : `Note from ${from}`;
    case "report":
      return direction === "sent" ? `Report to ${to}` : `Report from ${from}`;
    default:
      // A kind from a newer server keeps its wire name.
      return direction === "sent"
        ? `${String(message.kind)} to ${to}`
        : `${String(message.kind)} from ${from}`;
  }
}

/** Above this many characters or lines a message body starts folded. */
const LONG_CHARS = 600;
const LONG_LINES = 10;

export const isLongBody = (body: string): boolean =>
  body.length > LONG_CHARS || body.split("\n").length > LONG_LINES;
