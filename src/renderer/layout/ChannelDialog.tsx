// "Select channel": pick one or several sources for a split (several = merged co-chat).
import { useRef, useState } from "react";
import { PLATFORMS, type Platform, type SourceRef } from "../protocol";
import { PLATFORM_LABEL, PlatformIcon } from "../platformIcons";
import { XIcon } from "../icons";
import { Btn, Modal, inputCls } from "./ui";
import { shortName, sourceKey } from "./tree";

const HINT: Record<Platform, string> = {
  twitch: "Twitch login, e.g. a_g_u__",
  kick: "Kick channel slug, e.g. trainwreckstv",
  youtube: "@handle, channel URL or video URL",
  rumble: "Channel name or video URL",
  x: "Username or broadcast URL",
};
const PLACEHOLDER: Record<Platform, string> = {
  twitch: "channel login",
  kick: "channel slug",
  youtube: "@handle or youtube.com/...",
  rumble: "channel or rumble.com/... URL",
  x: "@username or x.com/i/broadcasts/...",
};

function lastSegment(input: string): string {
  try {
    const u = new URL(/^https?:\/\//i.test(input) ? input : `https://${input}`);
    const parts = u.pathname.split("/").filter(Boolean);
    return parts[parts.length - 1] || "";
  } catch {
    return input;
  }
}

/** Light client-side tidy-up; the platform module in main does the real normalisation. */
function clean(platform: Platform, raw: string): { channel: string } | { error: string } {
  let c = raw.trim();
  if (!c) return { error: "Enter a channel first." };
  if (platform === "twitch") {
    if (/twitch\.tv\//i.test(c)) c = lastSegment(c);
    c = c.replace(/^[@#]+/, "").toLowerCase();
    if (!/^[a-z0-9_]{1,25}$/.test(c)) return { error: "A Twitch login is letters, digits and underscores (max 25)." };
  } else if (platform === "kick") {
    if (/kick\.com\//i.test(c)) c = lastSegment(c);
    c = c.replace(/^@+/, "").toLowerCase();
    if (!/^[a-z0-9_-]{1,40}$/.test(c)) return { error: "A Kick slug is letters, digits, dashes and underscores." };
  } else if (platform === "x") {
    if (!/^https?:\/\//i.test(c) && !/(x|twitter)\.com\//i.test(c)) c = c.replace(/^@+/, "");
    if (!c) return { error: "Enter a username or broadcast URL." };
  }
  return { channel: c };
}

export function ChannelDialog({
  title,
  initial,
  submitLabel,
  onSubmit,
  onClose,
}: {
  title: string;
  initial: SourceRef[];
  submitLabel: string;
  onSubmit: (sources: SourceRef[]) => void;
  onClose: () => void;
}) {
  const [platform, setPlatform] = useState<Platform>(initial[0]?.platform ?? "twitch");
  const [text, setText] = useState("");
  const [sources, setSources] = useState<SourceRef[]>(initial);
  const [error, setError] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  /** Adds the typed channel to the list; returns the new list, or null when the input was bad. */
  const addTyped = (): SourceRef[] | null => {
    if (!text.trim()) return sources;
    const r = clean(platform, text);
    if ("error" in r) {
      setError(r.error);
      return null;
    }
    const ref: SourceRef = { platform, channel: r.channel };
    const next = sources.some((s) => sourceKey(s).toLowerCase() === sourceKey(ref).toLowerCase()) ? sources : [...sources, ref];
    setSources(next);
    setText("");
    setError("");
    return next;
  };

  const submit = () => {
    const next = addTyped();
    if (!next) return;
    if (!next.length) {
      setError("Add at least one channel.");
      return;
    }
    onSubmit(next);
  };

  return (
    <Modal
      title={title}
      onClose={onClose}
      width={460}
      footer={
        <>
          <Btn onClick={onClose}>Cancel</Btn>
          <Btn primary onClick={submit}>
            {submitLabel}
          </Btn>
        </>
      }
    >
      <form
        className="space-y-3 p-3"
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <div>
          <div className="mb-1 text-[11px] uppercase tracking-wide text-[var(--color-faint)]">Platform</div>
          <div className="flex flex-wrap gap-1">
            {PLATFORMS.map((p) => (
              <button
                key={p}
                type="button"
                onClick={() => {
                  setPlatform(p);
                  setError("");
                  inputRef.current?.focus();
                }}
                aria-pressed={platform === p}
                className={
                  "flex items-center gap-1.5 rounded border px-2 py-1 text-[12px] " +
                  (platform === p
                    ? "border-[var(--color-accent)] bg-[var(--color-mention)] text-[var(--color-text)]"
                    : "border-[var(--color-line)] text-[var(--color-muted)] hover:bg-[var(--color-raised-2)]")
                }
              >
                <PlatformIcon platform={p} />
                {PLATFORM_LABEL[p]}
              </button>
            ))}
          </div>
        </div>

        <div>
          <div className="mb-1 text-[11px] uppercase tracking-wide text-[var(--color-faint)]">Channel</div>
          <div className="flex gap-1.5">
            <input
              ref={inputRef}
              autoFocus
              value={text}
              onChange={(e) => {
                setText(e.target.value);
                setError("");
              }}
              placeholder={PLACEHOLDER[platform]}
              spellCheck={false}
              className={inputCls}
              aria-label="Channel"
            />
            <Btn onClick={() => addTyped()} disabled={!text.trim()} title="Add to this split as a merged co-chat">
              Add
            </Btn>
          </div>
          <div className="mt-1 text-[11px] text-[var(--color-faint)]">{HINT[platform]}</div>
          {error && <div className="mt-1 text-[11px] text-red-400">{error}</div>}
        </div>

        <div>
          <div className="mb-1 text-[11px] uppercase tracking-wide text-[var(--color-faint)]">
            In this split{sources.length > 1 ? " (merged co-chat)" : ""}
          </div>
          {sources.length === 0 ? (
            <div className="text-[12px] text-[var(--color-faint)]">Nothing yet. Add several channels to merge their chats into one split.</div>
          ) : (
            <div className="flex flex-wrap gap-1.5">
              {sources.map((s) => (
                <span
                  key={sourceKey(s)}
                  className="flex items-center gap-1.5 rounded border border-[var(--color-line)] bg-[var(--color-app)] py-0.5 pl-1.5 pr-1 text-[12px]"
                >
                  <PlatformIcon platform={s.platform} />
                  <span className="max-w-[180px] truncate" title={s.channel}>
                    {shortName(s)}
                  </span>
                  <button
                    type="button"
                    aria-label={`Remove ${shortName(s)}`}
                    onClick={() => setSources((l) => l.filter((x) => x !== s))}
                    className="rounded p-0.5 text-[var(--color-faint)] hover:bg-[var(--color-raised-2)] hover:text-[var(--color-text)]"
                  >
                    <XIcon className="h-3 w-3" />
                  </button>
                </span>
              ))}
            </div>
          )}
        </div>
        <button type="submit" className="hidden" />
      </form>
    </Modal>
  );
}
