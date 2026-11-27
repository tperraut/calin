import { ComingNext } from "./ComingNext";
import type { PanelProps } from "./registry";

export default function FilesPanel(_props: PanelProps) {
  return (
    <ComingNext
      title="Files"
      detail="Browse the session's repos and edit a file next to the agent."
    />
  );
}
