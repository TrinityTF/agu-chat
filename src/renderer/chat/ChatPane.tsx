// STUB: replaced by the chat view. The props are the contract with layout/ (see docs/ARCHITECTURE.md).
import type { ConnFrame, SourceRef } from "../protocol";

export type ChatPaneProps = {
  /** One source = a single channel; several = a merged co-chat. */
  sources: SourceRef[];
  /** Chatterino's "mod mode": inline mod buttons on every row. */
  modMode: boolean;
  /** This split has keyboard focus (focus its input when true). */
  focused: boolean;
  /** Called whenever a source's latest conn frame changes, keyed `${platform}:${channel}` as given in `sources`. */
  onStatus?: (status: Record<string, ConnFrame>) => void;
};

export function ChatPane({ sources }: ChatPaneProps) {
  return (
    <div className="flex h-full items-center justify-center text-xs text-[var(--color-faint)]">
      {sources.map((s) => `${s.platform}:${s.channel}`).join(", ")}
    </div>
  );
}
