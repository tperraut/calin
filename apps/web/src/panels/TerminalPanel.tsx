import { ComingNext } from "./ComingNext";
import type { PanelProps } from "./registry";

export default function TerminalPanel(_props: PanelProps) {
  return <ComingNext title="Terminal" detail="A shell in the session's folder, on the server." />;
}
