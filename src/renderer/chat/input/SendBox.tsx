// Ported from modtools.agu.lol web/src/app/pages/Chat.tsx (commit 1b826c2): SendBox (send, slash commands,
// colon / Tab / "@" / "/" autocomplete, emote picker, reply bar, /clear and /raid confirm) plus the
// Rumble and Kick send boxes (RumbleChat.tsx RumbleSendBox, KickChat.tsx KickSendBox). Changes:
// one box for a whole split; a textarea (Enter sends, Shift+Enter newline); a target selector (platform
// icon dropdown) instead of the site's merged-mode picker, defaulting to the first source whose conn frame
// says canSend; disabled with the conn frame's readOnlyReason when nothing can send; Up/Down recall your
// sent-message history; sends go through relayApi(source, ...).
import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type ChangeEvent, type KeyboardEvent as RKeyboardEvent } from "react";
import { relayApi } from "../../bridge";
import { PlatformIcon, PLATFORM_LABEL } from "../../platformIcons";
import type { Platform, SourceRef } from "../../protocol";
import { SmileIcon, XIcon } from "../../icons";
import type { ModCaps } from "../modCaps";
import type { ReplyTarget } from "../rows/env";
import type { Toast } from "../toast";
import { btnDanger, btnGhost, btnPrimary } from "../ui";
import { AutocompleteDropdown, handleMentionKey, MentionDropdown, rankAutoMatches, rankTabMatches, useMentions, type AutoMatch, type AutoState } from "./autocomplete";
import { EmotePicker } from "./EmotePicker";
import { mergeInventories, type Inventory } from "./inventory";
import { confirmedCommand, parseUserArg, runSlashCommand, SLASH_COMMANDS, splitArgs } from "./slash";

export type SendSource = {
  sk: string;
  ref: SourceRef;
  /** The conn frame says this account can post here. */
  canSend: boolean;
  readOnlyReason?: string;
  /** Connection still being established (no conn frame yet, or "connecting"). */
  pending: boolean;
  caps: ModCaps;
  inventory: Inventory;
};

const SENDABLE: Platform[] = ["twitch", "rumble", "kick"];
const ALL = "*";
const HISTORY_MAX = 100;

export async function postText(source: SourceRef, path: string, text: string, replyTo?: string): Promise<string | null> {
  try {
    const r = await relayApi(source, path, { method: "POST", body: JSON.stringify({ text, ...(replyTo ? { replyTo } : {}) }) });
    if (r.ok) return null;
    const t = await r.text();
    let msg = t;
    try {
      const j = JSON.parse(t);
      if (j && j.error) msg = String(j.error);
    } catch {
      /* plain text */
    }
    return r.status === 403 && msg === "relogin" ? "Log in again to grant chat permissions" : msg || "send failed";
  } catch (e) {
    return (e as Error).message || "send failed";
  }
}

const SEND_PATH: Partial<Record<Platform, string>> = { twitch: "/api/chat/send", rumble: "/api/rumble/send", kick: "/api/kick/send" };

export const SendBox = memo(function SendBox({
  sources,
  focused,
  replyTarget,
  onClearReply,
  toast,
  getChatters,
  getLastMessageId,
  getPinnedId,
}: {
  sources: SendSource[];
  focused: boolean;
  replyTarget: ReplyTarget | null;
  onClearReply: () => void;
  toast: Toast;
  /** Display names newest-first for the given source keys (read lazily: only when "@" is typed). */
  getChatters: (sks: string[]) => string[];
  getLastMessageId: (sk: string) => string | null;
  getPinnedId: (sk: string) => string | null;
}) {
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [autoState, setAutoState] = useState<AutoState | null>(null);
  const [confirm, setConfirm] = useState<{ kind: "clear" } | { kind: "raid"; login: string } | null>(null);
  const [target, setTarget] = useState<string>("");
  const [targetOpen, setTargetOpen] = useState(false);
  const taRef = useRef<HTMLTextAreaElement | null>(null);
  const pickerRef = useRef<HTMLDivElement | null>(null);
  const pickerBtnRef = useRef<HTMLButtonElement | null>(null);
  const targetRef = useRef<HTMLDivElement | null>(null);
  // Sent-message history (per split): Up/Down recall, like a shell.
  const history = useRef<string[]>([]);
  const histIdx = useRef(-1);
  const draft = useRef("");

  const sendable = useMemo(() => sources.filter((s) => SENDABLE.includes(s.ref.platform) && s.canSend), [sources]);
  const byKey = useMemo(() => new Map(sources.map((s) => [s.sk, s])), [sources]);

  // A reply pins the target: its id only means something in the chat it came from.
  const replySrc = replyTarget ? byKey.get(replyTarget.sk) : undefined;
  const effective = replySrc && replySrc.canSend ? replySrc.sk : target === ALL && sendable.length > 1 ? ALL : sendable.find((s) => s.sk === target)?.sk || sendable[0]?.sk || "";
  const targets = effective === ALL ? sendable : sendable.filter((s) => s.sk === effective);
  const single = targets.length === 1 ? targets[0] : null;

  // Keep the selected target valid as connections come and go.
  useEffect(() => {
    if (effective !== target && !replyTarget) setTarget(effective);
  }, [effective, target, replyTarget]);

  useEffect(() => {
    if (focused) taRef.current?.focus();
  }, [focused]);

  // Picking Reply puts the caret in the box.
  useEffect(() => {
    if (replyTarget) taRef.current?.focus();
  }, [replyTarget]);

  useEffect(() => {
    if (!pickerOpen) return;
    const onDoc = (e: MouseEvent) => {
      const t = e.target as Node;
      if (pickerBtnRef.current?.contains(t)) return;
      if (pickerRef.current && !pickerRef.current.contains(t)) setPickerOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [pickerOpen]);

  useEffect(() => {
    if (!targetOpen) return;
    const onDoc = (e: MouseEvent) => {
      if (targetRef.current && !targetRef.current.contains(e.target as Node)) setTargetOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [targetOpen]);

  // Auto-grow up to ~5 lines.
  useLayoutEffect(() => {
    const el = taRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = Math.min(el.scrollHeight, 96) + "px";
  }, [text]);

  const inventory = useMemo(() => mergeInventories(targets.length ? targets.map((t) => t.inventory) : [{ sections: [], inventory: [] }]), [targets]);
  const getNames = useCallback(() => getChatters(targets.map((t) => t.sk)), [getChatters, targets]);
  const mentions = useMentions(getNames);
  // The slash-command dropdown uses the Twitch caps of a single Twitch target.
  const twitchCaps = single && single.ref.platform === "twitch" ? single.caps : null;
  const slashOk = !!single && single.ref.platform === "twitch";

  const placeholder = !sendable.length
    ? sources.find((s) => s.readOnlyReason)?.readOnlyReason || (sources.some((s) => s.pending) ? "Connecting..." : "Chat is read-only")
    : replyTarget
      ? `Reply to @${replyTarget.user}`
      : effective === ALL
        ? `Send to ${sendable.map((s) => PLATFORM_LABEL[s.ref.platform]).join(" + ")}`
        : `Message ${single ? (single.ref.platform === "twitch" ? "#" : "") + single.ref.channel : ""}`;

  const clear = () => {
    setText("");
    setAutoState(null);
    mentions.close();
    onClearReply();
  };

  const remember = (t: string) => {
    const h = history.current;
    if (h[h.length - 1] !== t) h.push(t);
    if (h.length > HISTORY_MAX) h.splice(0, h.length - HISTORY_MAX);
    histIdx.current = -1;
    draft.current = "";
  };

  const send = async () => {
    const t = text.trim();
    if (!t || sending || !targets.length) return;
    const body = t.slice(0, 500);

    // Slash commands (everything but "/me") run against one Twitch source.
    if (t.startsWith("/") && !/^\/me(\s|$)/i.test(t) && single?.ref.platform === "twitch") {
      const m = /^\/(\S+)\s*([\s\S]*)$/.exec(t);
      const cmd = m ? m[1].toLowerCase() : "";
      const rest = m ? m[2].trim() : "";
      if (cmd === "clear") return setConfirm({ kind: "clear" });
      if (cmd === "raid") {
        const login = parseUserArg(splitArgs(rest)[0] || "");
        if (!login) return toast("Usage: /raid <user>", "warn");
        return setConfirm({ kind: "raid", login });
      }
      setSending(true);
      try {
        const ok = await runSlashCommand(t, { source: single.ref, toast, lastMessageId: () => getLastMessageId(single.sk), pinnedId: () => getPinnedId(single.sk) });
        if (ok) {
          remember(t);
          clear();
        }
      } catch (e) {
        toast((e as Error).message, "warn");
      } finally {
        setSending(false);
      }
      return;
    }
    if (t.startsWith("/") && !/^\/me(\s|$)/i.test(t) && effective === ALL) {
      toast("Commands run on a single Twitch chat: pick it in the target menu", "warn");
      return;
    }

    setSending(true);
    try {
      // Sequential, not parallel: rate limits are per platform, and one failing must not lose the message
      // that already reached another.
      const failures: string[] = [];
      for (const s of targets) {
        const path = SEND_PATH[s.ref.platform];
        if (!path) continue;
        // A reply id only means something in the chat it came from.
        const replyTo = replyTarget && replyTarget.sk === s.sk ? replyTarget.id : undefined;
        const err = await postText(s.ref, path, body, replyTo);
        if (err) failures.push(targets.length > 1 ? `${PLATFORM_LABEL[s.ref.platform]}: ${err}` : err);
      }
      if (failures.length) toast(failures.join(" - "), "warn");
      else {
        remember(t);
        clear();
      }
    } finally {
      setSending(false);
    }
  };

  const executeConfirm = async () => {
    if (!confirm || !single) return;
    setSending(true);
    try {
      await confirmedCommand(single.ref, confirm, toast);
      clear();
    } catch (e) {
      toast((e as Error).message, "warn");
    } finally {
      setSending(false);
      setConfirm(null);
    }
  };

  const setCaret = (pos: number) =>
    requestAnimationFrame(() => {
      taRef.current?.focus();
      taRef.current?.setSelectionRange(pos, pos);
    });

  const insertEmote = (name: string) => {
    const el = taRef.current;
    const insert = ` ${name} `;
    if (!el) return setText((t) => t + insert);
    const start = el.selectionStart ?? text.length;
    const end = el.selectionEnd ?? text.length;
    setText(text.slice(0, start) + insert + text.slice(end));
    setCaret(start + insert.length);
  };

  // Replaces the tracked [start,end) range with the pick + a trailing space (Enter / Tab / a click).
  // Command mode always replaces from 0 with "/name " and does NOT send.
  const commitAuto = (auto: AutoState, choice?: AutoMatch) => {
    const pick = choice || auto.matches[auto.idx] || auto.matches[0];
    setAutoState(null);
    if (!pick) return;
    if (auto.mode === "cmd") {
      setText("/" + pick.name + " " + text.slice(auto.end));
      return setCaret(pick.name.length + 2);
    }
    const ins = pick.insert ?? pick.name;
    setText(text.slice(0, auto.start) + ins + " " + text.slice(auto.end));
    setCaret(auto.start + ins.length + 1);
  };

  // Cycling with the arrows (emote modes only) inline-writes the highlighted match's name into the box
  // so the user can flip between candidates seeing each name, then press space to accept.
  const previewAuto = (auto: AutoState, idx: number) => {
    const pick = auto.matches[idx];
    if (!pick) return;
    const ins = pick.insert ?? pick.name;
    setText(text.slice(0, auto.start) + ins + text.slice(auto.end));
    const pos = auto.start + ins.length;
    setAutoState({ mode: auto.mode, start: auto.start, end: pos, matches: auto.matches, idx });
    setCaret(pos);
  };

  const applyMention = (choice?: string) => {
    const applied = mentions.apply(text, choice);
    if (!applied) return;
    setText(applied.text);
    setCaret(applied.caret);
  };

  const handleChange = (e: ChangeEvent<HTMLTextAreaElement>) => {
    const val = e.target.value;
    const pos = e.target.selectionStart ?? val.length;
    setText(val);
    histIdx.current = -1;
    const before = val.slice(0, pos);

    // Mentions first: "@" can't also start a command or a :emote:.
    mentions.refresh(val, pos);
    if (/(^|\s)@[^\s@]*$/.test(before)) return setAutoState(null);

    const cmdMatch = /^\/(\S*)$/.exec(before);
    if (cmdMatch && slashOk) {
      const query = cmdMatch[1].toLowerCase();
      const matches: AutoMatch[] = SLASH_COMMANDS.filter((c) => c.name.startsWith(query)).map((c) => ({
        name: c.name,
        img: "",
        label: `/${c.name} ${c.usage}`,
        dim: c.capKey && twitchCaps ? !twitchCaps[c.capKey] : false,
      }));
      return setAutoState(matches.length ? { mode: "cmd", start: 0, end: pos, matches, idx: 0 } : null);
    }
    const colonMatch = /(^|\s)(:[^\s:]{2,})$/.exec(before);
    if (colonMatch) {
      const query = colonMatch[2].slice(1);
      const start = pos - colonMatch[2].length;
      const matches = rankAutoMatches(inventory.inventory, query, "colon");
      return setAutoState(matches.length ? { mode: "colon", start, end: pos, matches, idx: 0 } : null);
    }
    setAutoState((prev) => {
      if (!prev || prev.mode !== "prefix") return null;
      const wordMatch = /(\S+)$/.exec(before);
      if (!wordMatch) return null;
      const word = wordMatch[1];
      const matches = rankTabMatches(inventory.inventory, getNames(), word);
      return matches.length ? { mode: "prefix", start: pos - word.length, end: pos, matches, idx: 0 } : null;
    });
  };

  const handleKeyDown = (e: RKeyboardEvent<HTMLTextAreaElement>) => {
    // While the mention list is open it owns Tab/arrows/Enter/Escape.
    if (handleMentionKey(e, mentions, () => applyMention())) return;
    if (autoState) {
      if (e.key === "Tab" || e.key === "Enter") {
        e.preventDefault();
        return commitAuto(autoState);
      }
      if (e.key === "ArrowUp" || e.key === "ArrowDown") {
        e.preventDefault();
        const n = autoState.matches.length;
        const nextIdx = (autoState.idx + (e.key === "ArrowUp" ? -1 : 1) + n) % n;
        if (autoState.mode === "cmd") setAutoState((s) => (s ? { ...s, idx: nextIdx } : s));
        else previewAuto(autoState, nextIdx);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        return setAutoState(null);
      }
      return; // any other key types normally; handleChange re-evaluates
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      return void send();
    }
    if (e.key === "Escape") {
      if (replyTarget) {
        e.preventDefault();
        onClearReply();
      }
      return;
    }
    if (e.key === "Tab") {
      e.preventDefault();
      const el = e.currentTarget;
      const pos = el.selectionStart ?? text.length;
      const wordMatch = /(\S+)$/.exec(text.slice(0, pos));
      if (!wordMatch) return;
      const word = wordMatch[1];
      const matches = rankTabMatches(inventory.inventory, getNames(), word);
      if (matches.length) setAutoState({ mode: "prefix", start: pos - word.length, end: pos, matches, idx: 0 });
      return;
    }
    // Up/Down recall: only from the first / last line so multi-line editing still moves the caret.
    if ((e.key === "ArrowUp" || e.key === "ArrowDown") && !e.shiftKey && !e.altKey && !e.ctrlKey) {
      const el = e.currentTarget;
      const pos = el.selectionStart ?? 0;
      const h = history.current;
      if (e.key === "ArrowUp" && !text.slice(0, pos).includes("\n") && h.length) {
        e.preventDefault();
        if (histIdx.current === -1) {
          draft.current = text;
          histIdx.current = h.length - 1;
        } else if (histIdx.current > 0) histIdx.current--;
        const v = h[histIdx.current];
        setText(v);
        setCaret(v.length);
      } else if (e.key === "ArrowDown" && histIdx.current !== -1 && !text.slice(pos).includes("\n")) {
        e.preventDefault();
        if (histIdx.current < h.length - 1) histIdx.current++;
        else histIdx.current = -1;
        const v = histIdx.current === -1 ? draft.current : h[histIdx.current];
        setText(v);
        setCaret(v.length);
      }
    }
  };

  const disabled = !sendable.length;
  const showTarget = sources.filter((s) => SENDABLE.includes(s.ref.platform)).length > 1 || sendable.length > 1;

  return (
    <div className="shrink-0 border-t border-line bg-raised">
      {confirm && (
        <div className="flex items-center gap-2 border-b border-red-500/30 bg-red-500/10 px-2 py-1 text-xs text-red-200">
          <span className="min-w-0 flex-1 truncate">{confirm.kind === "clear" ? "Clear the entire chat? This can't be undone." : `Start raiding @${confirm.login}?`}</span>
          <button type="button" disabled={sending} onClick={executeConfirm} className={btnDanger}>
            Confirm
          </button>
          <button type="button" onClick={() => setConfirm(null)} className={btnGhost}>
            Cancel
          </button>
        </div>
      )}
      {replyTarget && (
        <div className="flex items-center gap-2 border-b border-line px-2 py-1 text-xs text-muted">
          <span className="min-w-0 flex-1 truncate">
            ↩ Replying to @{replyTarget.user}: {replyTarget.text}
          </span>
          <button type="button" onClick={onClearReply} className="shrink-0 rounded p-0.5 hover:text-text" title="Cancel reply (Esc)">
            <XIcon className="h-3.5 w-3.5" />
          </button>
        </div>
      )}
      <div className="relative flex items-end gap-1.5 p-1.5">
        {autoState && !disabled && <AutocompleteDropdown state={autoState} onPick={(m) => commitAuto(autoState, m)} />}
        {mentions.state && !disabled && <MentionDropdown state={mentions.state} onPick={(name) => applyMention(name)} />}
        {pickerOpen && !disabled && (
          <div ref={pickerRef} className="absolute bottom-full left-1.5 z-30 mb-1">
            <EmotePicker sections={inventory.sections} onPick={insertEmote} />
          </div>
        )}
        <button
          ref={pickerBtnRef}
          type="button"
          disabled={disabled}
          onClick={() => {
            setPickerOpen((v) => !v);
            setAutoState(null);
          }}
          className="shrink-0 rounded border border-line p-1.5 text-muted hover:border-white/25 hover:text-text disabled:opacity-40"
          title="Emotes"
        >
          <SmileIcon className="h-4 w-4" />
        </button>
        {showTarget && (
          <div ref={targetRef} className="relative shrink-0">
            {targetOpen && (
              <div className="absolute bottom-full left-0 z-30 mb-1 w-44 overflow-hidden rounded border border-line bg-raised py-1 shadow-2xl shadow-black/60">
                {sources
                  .filter((s) => SENDABLE.includes(s.ref.platform))
                  .map((s) => (
                    <button
                      key={s.sk}
                      type="button"
                      disabled={!s.canSend}
                      onClick={() => {
                        setTarget(s.sk);
                        setTargetOpen(false);
                        taRef.current?.focus();
                      }}
                      className={`flex w-full items-center gap-2 px-2.5 py-1 text-left text-xs hover:bg-raised-2 disabled:cursor-not-allowed disabled:opacity-50 ${effective === s.sk ? "text-text" : "text-muted"}`}
                      title={s.canSend ? "" : s.readOnlyReason || "Can't send here"}
                    >
                      <PlatformIcon platform={s.ref.platform} className="h-3.5 w-3.5" />
                      <span className="min-w-0 flex-1 truncate">{s.ref.channel}</span>
                      {!s.canSend && <span className="text-[10px] text-faint">read-only</span>}
                    </button>
                  ))}
                {sendable.length > 1 && (
                  <button
                    type="button"
                    onClick={() => {
                      setTarget(ALL);
                      setTargetOpen(false);
                      taRef.current?.focus();
                    }}
                    className={`flex w-full items-center gap-2 px-2.5 py-1 text-left text-xs hover:bg-raised-2 ${effective === ALL ? "text-text" : "text-muted"}`}
                  >
                    <span className="w-3.5 text-center text-[10px] font-bold">All</span>
                    <span className="flex-1">All chats</span>
                  </button>
                )}
              </div>
            )}
            <button
              type="button"
              disabled={disabled}
              onClick={() => setTargetOpen((v) => !v)}
              title={effective === ALL ? "Sending to all chats" : single ? `Sending to ${PLATFORM_LABEL[single.ref.platform]}: ${single.ref.channel}` : "Send target"}
              className="flex h-full items-center rounded border border-line px-1.5 py-1.5 hover:border-white/25 disabled:opacity-40"
            >
              {effective === ALL ? <span className="text-xs font-bold text-muted">All</span> : <PlatformIcon platform={single?.ref.platform || "twitch"} className="h-4 w-4" />}
            </button>
          </div>
        )}
        <textarea
          ref={taRef}
          rows={1}
          className="min-w-0 flex-1 resize-none rounded border border-line bg-panel px-2 py-1.5 text-[13px] leading-[1.35] text-text outline-none placeholder:text-faint focus:border-accent/60 disabled:cursor-not-allowed disabled:opacity-60"
          placeholder={placeholder}
          maxLength={500}
          disabled={disabled}
          value={text}
          onChange={handleChange}
          onKeyDown={handleKeyDown}
          spellCheck
        />
        <button className={`${btnPrimary} self-stretch`} disabled={disabled || sending || !text.trim()} onClick={() => void send()}>
          {sending ? "..." : "Send"}
        </button>
      </div>
    </div>
  );
});
