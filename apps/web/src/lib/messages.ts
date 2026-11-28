import type { Agent } from "../gen/Agent";
import type { AgentMessage } from "../gen/AgentMessage";
import type { MessageKind } from "../gen/MessageKind";
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

const sentWords: Record<MessageKind, string> = {
  // api.rs, "Child sessions": the brief that starts a child, sent by its parent.
  dispatch: "Dispatched child session",
  note: "Note to",
  report: "Report to",
};

const receivedWords: Record<MessageKind, string> = {
  dispatch: "Brief from",
  note: "Note from",
  report: "Report from",
};

/**
 * What a message was, in words, from the side of the session that shows it:
 * `sent` when it left this session, `received` when it arrived here (event.rs).
 */
export function messageHeading(
  message: AgentMessage,
  direction: "sent" | "received",
  speaker: Speaker,
): string {
  // A kind from a newer server is missing from the tables and keeps its wire name.
  if (direction === "sent") {
    const to = speaker(message.to_session_id, message.to_agent_id);
    return `${sentWords[message.kind] ?? `${String(message.kind)} to`} ${to}`;
  }
  const from = speaker(message.from_session_id, message.from_agent_id);
  return `${receivedWords[message.kind] ?? `${String(message.kind)} from`} ${from}`;
}

/** Above this many characters or lines a message body starts folded. */
const LONG_CHARS = 600;
const LONG_LINES = 10;

export const isLongBody = (body: string): boolean =>
  body.length > LONG_CHARS || body.split("\n").length > LONG_LINES;
