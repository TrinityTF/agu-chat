// Settings dialog: Accounts, Appearance/chat, Highlights, Ignores, Hotkeys, Mod buttons, Updates, About.
import { useEffect, useRef, useState, type ReactNode } from "react";
import { agu } from "../bridge";
import { useAccounts } from "../hooks";
import { PLATFORMS, type AccountState, type DevicePrompt, type IgnoredPhrase, type ModButton, type ModButtonAction, type Platform, type Settings, type UpdaterState } from "../protocol";
import { ignoreLogin, phraseError, toggleIgnoredUser } from "../chat/ignores";
import { PLATFORM_LABEL, PlatformIcon } from "../platformIcons";
import { ChevronIcon, CopyIcon, TrashIcon, XIcon } from "../icons";
import { ModButtonFace, ModIcon } from "../chat/ModButtonFace";
import {
  DEFAULT_MOD_BUTTONS,
  MAX_COMMAND,
  MAX_LABEL,
  MAX_MOD_BUTTONS,
  MOD_BUTTON_ICONS,
  TEMPLATE_HELP,
  defaultIcon,
  describeModButton,
  formatDuration,
  newModButtonId,
  normalizeModButtons,
  parseDuration,
} from "../chat/modButtons";
import { HOTKEY_ACTIONS, actionLabel, eventToBinding, findConflicts, hotkeyRecording, isUsableBinding, resolveHotkeys, toOverrides, type HotkeyAction, type HotkeyTable } from "./hotkeys";
import { Btn, Modal, Toggle, copyText, inputCls } from "./ui";
import { useAppInfo, useLiveSettings, useUpdater } from "./useBridge";
import { HighlightsSection } from "./HighlightsSection";

export const REPO_URL = "https://github.com/TrinityTF/agu-chat";

type Section = "accounts" | "appearance" | "highlights" | "ignores" | "hotkeys" | "modButtons" | "updates" | "about";
const SECTIONS: { id: Section; label: string }[] = [
  { id: "accounts", label: "Accounts" },
  { id: "appearance", label: "Appearance and chat" },
  { id: "highlights", label: "Highlights" },
  { id: "ignores", label: "Ignores" },
  { id: "hotkeys", label: "Hotkeys" },
  { id: "modButtons", label: "Mod buttons" },
  { id: "updates", label: "Updates" },
  { id: "about", label: "About" },
];

export function SettingsDialog({ onClose, initial = "accounts" }: { onClose: () => void; initial?: Section }) {
  const [section, setSection] = useState<Section>(initial);
  const [settings, setSettings] = useLiveSettings();

  return (
    <Modal title="Settings" onClose={onClose} width={780} fullOnSmall>
      {/* Narrow windows (and phones later): the section list wraps into rows above the page, so every section stays visible. */}
      <div className="flex h-[min(72vh,540px)] text-[12px] max-sm:h-full max-sm:flex-col">
        <nav className="w-40 shrink-0 overflow-y-auto border-r border-[var(--color-line)] bg-[var(--color-app)] py-2 max-sm:flex max-sm:w-full max-sm:flex-wrap max-sm:overflow-visible max-sm:border-b max-sm:border-r-0 max-sm:py-0">
          {SECTIONS.map((s) => (
            <button
              key={s.id}
              type="button"
              onClick={() => setSection(s.id)}
              aria-current={section === s.id}
              className={
                "block w-full border-l-2 px-3 py-1.5 text-left max-sm:w-auto max-sm:whitespace-nowrap max-sm:border-b-2 max-sm:border-l-0 max-sm:px-2.5 " +
                (section === s.id
                  ? "border-[var(--color-accent)] bg-[var(--color-raised)] text-[var(--color-text)]"
                  : "border-transparent text-[var(--color-muted)] hover:bg-[var(--color-raised)] hover:text-[var(--color-text)]")
              }
            >
              {s.label}
            </button>
          ))}
        </nav>
        <div className="min-h-0 min-w-0 flex-1 overflow-y-auto p-4 max-sm:p-3">
          {!settings ? (
            <div className="text-[var(--color-faint)]">Loading…</div>
          ) : section === "accounts" ? (
            <AccountsSection settings={settings} setSettings={setSettings} />
          ) : section === "appearance" ? (
            <AppearanceSection settings={settings} setSettings={setSettings} />
          ) : section === "highlights" ? (
            <HighlightsSection settings={settings} setSettings={setSettings} />
          ) : section === "ignores" ? (
            <IgnoresSection settings={settings} setSettings={setSettings} />
          ) : section === "hotkeys" ? (
            <HotkeysSection settings={settings} setSettings={setSettings} />
          ) : section === "modButtons" ? (
            <ModButtonsSection settings={settings} setSettings={setSettings} />
          ) : section === "updates" ? (
            <UpdatesSection settings={settings} setSettings={setSettings} />
          ) : (
            <AboutSection />
          )}
        </div>
      </div>
    </Modal>
  );
}

// ---- shared bits --------------------------------------------------------------

function H({ children }: { children: ReactNode }) {
  return <h3 className="mb-2 mt-1 text-[13px] font-semibold text-[var(--color-text)]">{children}</h3>;
}

function Card({ children }: { children: ReactNode }) {
  return <div className="mb-3 rounded border border-[var(--color-line)] bg-[var(--color-raised)] p-3">{children}</div>;
}

function Link({ url, children }: { url: string; children: ReactNode }) {
  return (
    <button type="button" onClick={() => agu.app.openExternal(url).catch(() => {})} className="text-[var(--color-accent-soft)] underline hover:text-[var(--color-text)]">
      {children}
    </button>
  );
}

function Err({ children }: { children?: ReactNode }) {
  return children ? <div className="mt-2 text-[11px] text-red-400">{children}</div> : null;
}

const errMsg = (e: unknown) => (e instanceof Error ? e.message : String(e));

// ---- accounts -------------------------------------------------------------------

type SetSettings = (p: Partial<Settings>) => void;

function AccountsSection({ settings, setSettings }: { settings: Settings; setSettings: SetSettings }) {
  const accounts = useAccounts();
  if (!accounts) return <div className="text-[var(--color-faint)]">Loading…</div>;
  return (
    <div>
      <H>Accounts</H>
      <TwitchAccount st={accounts.twitch} settings={settings} setSettings={setSettings} />
      <RumbleAccount st={accounts.rumble} />
      <OtherAccount platform="kick" st={accounts.kick} />
      <OtherAccount platform="youtube" st={accounts.youtube} />
      <OtherAccount platform="x" st={accounts.x} />
    </div>
  );
}

function AccountTitle({ platform, right }: { platform: Platform; right?: ReactNode }) {
  return (
    <div className="mb-2 flex items-center justify-between">
      <div className="flex items-center gap-2 text-[13px] font-semibold">
        <PlatformIcon platform={platform} className="h-4 w-4" />
        {PLATFORM_LABEL[platform]}
      </div>
      {right}
    </div>
  );
}

function ConnectedLine({ st, onLogout }: { st: AccountState; onLogout: () => void }) {
  return (
    <div className="flex items-center justify-between gap-3">
      <div className="min-w-0 truncate">
        Logged in as <span className="font-semibold">{st.user?.name || st.user?.login || "unknown"}</span>
      </div>
      <Btn danger onClick={onLogout}>
        Log out
      </Btn>
    </div>
  );
}

function useNow(active: boolean) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (!active) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [active]);
  return now;
}

function DeviceCode({ prompt, onCancel }: { prompt: DevicePrompt; onCancel: () => void }) {
  const now = useNow(true);
  const [copied, setCopied] = useState(false);
  const left = Math.max(0, Math.round((prompt.expiresAt - now) / 1000));
  return (
    <div className="rounded border border-[var(--color-accent)]/50 bg-[var(--color-mention)] p-3">
      <div className="mb-1 text-[var(--color-muted)]">Open the page below and enter this code:</div>
      <div className="flex items-center gap-3">
        <code className="select-all rounded bg-[var(--color-app)] px-3 py-1.5 text-[26px] font-bold tracking-[0.25em] text-[var(--color-text)]">{prompt.userCode}</code>
        <Btn
          onClick={async () => {
            if (await copyText(prompt.userCode)) {
              setCopied(true);
              setTimeout(() => setCopied(false), 1500);
            }
          }}
        >
          <span className="flex items-center gap-1">
            <CopyIcon className="h-3 w-3" />
            {copied ? "Copied" : "Copy"}
          </span>
        </Btn>
      </div>
      <div className="mt-3 flex flex-wrap items-center gap-2">
        <Btn primary onClick={() => agu.app.openExternal(prompt.verificationUri).catch(() => {})}>
          Open {prompt.verificationUri.replace(/^https?:\/\//, "").replace(/[?#].*$/, "")}
        </Btn>
        <Btn onClick={onCancel}>Cancel</Btn>
        <span className="text-[11px] text-[var(--color-faint)]">
          {left > 0 ? `Waiting for you to approve… code expires in ${Math.floor(left / 60)}:${String(left % 60).padStart(2, "0")}` : "This code has expired. Cancel and try again."}
        </span>
      </div>
    </div>
  );
}

function TwitchAccount({ st, settings, setSettings }: { st: AccountState; settings: Settings; setSettings: SetSettings }) {
  const [clientId, setClientId] = useState("");
  const [changing, setChanging] = useState(false);
  const [prompt, setPrompt] = useState<DevicePrompt | null>(null);
  const [err, setErr] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (st.connected) setPrompt(null);
  }, [st.connected]);

  const pending = st.pending ?? prompt;
  const ownId = settings.twitchClientId;

  const saveId = (v: string) => {
    setSettings({ twitchClientId: v.trim() });
    setClientId("");
    setChanging(false);
  };

  const login = async () => {
    setErr("");
    setBusy(true);
    try {
      setPrompt(await agu.accounts.login("twitch"));
    } catch (e) {
      setErr(errMsg(e));
    } finally {
      setBusy(false);
    }
  };

  const cancel = async () => {
    setPrompt(null);
    try {
      await agu.accounts.cancel("twitch");
    } catch (e) {
      setErr(errMsg(e));
    }
  };

  const link = "underline hover:text-[var(--color-text)]";

  return (
    <Card>
      <AccountTitle platform="twitch" />
      {st.connected ? (
        <ConnectedLine st={st} onLogout={() => agu.accounts.logout("twitch").catch((e) => setErr(errMsg(e)))} />
      ) : pending ? (
        <DeviceCode prompt={pending} onCancel={cancel} />
      ) : (
        <div className="space-y-2">
          <div className="flex flex-wrap items-center gap-3">
            <Btn primary disabled={busy} onClick={login}>
              Log in with Twitch
            </Btn>
            <span className="text-[11px] text-[var(--color-faint)]">You approve it on twitch.tv; your password never goes through Agu Chat.</span>
          </div>
          {changing ? (
            <div className="flex flex-wrap gap-1.5">
              <input
                value={clientId}
                onChange={(e) => setClientId(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && clientId.trim() && saveId(clientId)}
                placeholder="Your app's Client ID"
                spellCheck={false}
                className={inputCls}
                aria-label="Twitch Client ID"
              />
              <Btn primary disabled={!clientId.trim()} onClick={() => saveId(clientId)}>
                Save
              </Btn>
              <Btn onClick={() => setChanging(false)}>Cancel</Btn>
            </div>
          ) : (
            <div className="text-[11px] text-[var(--color-faint)]">
              {ownId ? (
                <>
                  Using your own Twitch app ({ownId.slice(0, 6)}…).{" "}
                  <button type="button" className={link} onClick={() => saveId("")}>
                    Use Agu Chat&apos;s
                  </button>
                </>
              ) : (
                <button type="button" className={link} onClick={() => setChanging(true)}>
                  Advanced: log in through your own Twitch app
                </button>
              )}
            </div>
          )}
        </div>
      )}
      <Err>{err || st.error}</Err>
    </Card>
  );
}

function RumbleAccount({ st }: { st: AccountState }) {
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [code, setCode] = useState("");
  const [needCode, setNeedCode] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");

  if (st.unsupported) return <OtherAccount platform="rumble" st={st} />;

  const submit = async () => {
    setErr("");
    setBusy(true);
    try {
      const r = needCode ? await agu.accounts.login("rumble", { code: code.trim() }) : await agu.accounts.login("rumble", { username: username.trim(), password });
      setPassword("");
      if (r && r.twoFactor) setNeedCode(true);
      else {
        setNeedCode(false);
        setCode("");
      }
    } catch (e) {
      setErr(errMsg(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card>
      <AccountTitle platform="rumble" />
      {st.connected ? (
        <ConnectedLine st={st} onLogout={() => agu.accounts.logout("rumble").catch((e) => setErr(errMsg(e)))} />
      ) : (
        <form
          className="space-y-2"
          onSubmit={(e) => {
            e.preventDefault();
            submit();
          }}
        >
          {needCode ? (
            <>
              <div className="text-[var(--color-muted)]">Rumble sent a verification code. Enter it to finish logging in.</div>
              <input value={code} onChange={(e) => setCode(e.target.value)} placeholder="Verification code" autoFocus inputMode="numeric" className={inputCls} aria-label="Rumble verification code" />
            </>
          ) : (
            <>
              <input value={username} onChange={(e) => setUsername(e.target.value)} placeholder="Rumble username" autoComplete="username" className={inputCls} aria-label="Rumble username" />
              <input
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                type="password"
                placeholder="Password"
                autoComplete="current-password"
                className={inputCls}
                aria-label="Rumble password"
              />
            </>
          )}
          <div className="flex items-center gap-3">
            <Btn primary type="submit" disabled={busy || (needCode ? !code.trim() : !username.trim() || !password)}>
              {needCode ? "Verify" : "Log in"}
            </Btn>
            {needCode && (
              <Btn
                onClick={() => {
                  setNeedCode(false);
                  setCode("");
                }}
              >
                Back
              </Btn>
            )}
          </div>
          <div className="text-[11px] text-[var(--color-faint)]">
            Your password is sent to Rumble once to sign in and is never stored. Only the resulting session is kept, encrypted on this PC.
          </div>
        </form>
      )}
      <Err>{err || st.error}</Err>
    </Card>
  );
}

function OtherAccount({ platform, st }: { platform: Platform; st: AccountState }) {
  const [prompt, setPrompt] = useState<DevicePrompt | null>(null);
  const [err, setErr] = useState("");
  useEffect(() => {
    if (st.connected) setPrompt(null);
  }, [st.connected]);
  const pending = st.pending ?? prompt;

  return (
    <Card>
      <AccountTitle platform={platform} />
      {st.unsupported ? (
        <div className="text-[var(--color-muted)]">Read-only in this beta. You can read chat, but sending and moderating from here is not available yet.</div>
      ) : st.connected ? (
        <ConnectedLine st={st} onLogout={() => agu.accounts.logout(platform).catch((e) => setErr(errMsg(e)))} />
      ) : pending ? (
        <DeviceCode
          prompt={pending}
          onCancel={() => {
            setPrompt(null);
            agu.accounts.cancel(platform).catch((e) => setErr(errMsg(e)));
          }}
        />
      ) : (
        <div className="flex items-center gap-3">
          <span className="text-[var(--color-muted)]">Not logged in.</span>
          <Btn
            onClick={async () => {
              setErr("");
              try {
                const r = await agu.accounts.login(platform);
                if (r && r.userCode) setPrompt(r);
              } catch (e) {
                setErr(errMsg(e));
              }
            }}
          >
            Log in
          </Btn>
        </div>
      )}
      <Err>{err || st.error}</Err>
    </Card>
  );
}

// ---- appearance ---------------------------------------------------------------------

function Row({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-4 border-b border-[var(--color-line)]/60 py-1.5 last:border-0">
      <div className="min-w-0">
        <div>{label}</div>
        {hint && <div className="text-[11px] text-[var(--color-faint)]">{hint}</div>}
      </div>
      <div className="shrink-0">{children}</div>
    </div>
  );
}

type BoolKey = {
  [K in keyof Settings]: Settings[K] extends boolean ? K : never;
}[keyof Settings];

function AppearanceSection({ settings, setSettings }: { settings: Settings; setSettings: SetSettings }) {
  const bool = (key: BoolKey, label: string, hint?: string, disabled?: boolean) => (
    <Row label={label} hint={hint}>
      <Toggle checked={settings[key]} onChange={(v) => setSettings({ [key]: v })} disabled={disabled} label={label} />
    </Row>
  );
  const selectCls = "rounded border border-[var(--color-line)] bg-[var(--color-app)] px-2 py-1 text-[12px] outline-none focus:border-[var(--color-accent)]";

  return (
    <div>
      <H>Window</H>
      {bool("alwaysOnTop", "Always on top", "Keep Agu Chat above other windows (also the pin in the title bar)")}

      <H>Messages</H>
      {bool("timestamps", "Show timestamps")}
      {bool("timestampSeconds", "Timestamps with seconds", undefined, !settings.timestamps)}
      {bool("alternateRows", "Alternate row backgrounds")}
      <Row label="Font size" hint="Chat text, in pixels">
        <div className="flex items-center gap-2">
          <input
            type="range"
            min={10}
            max={22}
            step={1}
            value={settings.fontSize}
            onChange={(e) => setSettings({ fontSize: Number(e.target.value) })}
            className="w-32 accent-[var(--color-accent)]"
            aria-label="Font size"
          />
          <span className="w-8 text-right tabular-nums text-[var(--color-muted)]">{settings.fontSize}px</span>
        </div>
      </Row>
      {bool("readableColors", "Readable name colors", "Lighten dark name colors so they stay legible")}
      {bool("showPlatformMarks", "Platform marks on rows", "A small platform icon in merged co-chats")}

      <H>Emotes and badges</H>
      <Row label="Emote size">
        <select value={settings.emoteSize} onChange={(e) => setSettings({ emoteSize: e.target.value as Settings["emoteSize"] })} className={selectCls} aria-label="Emote size">
          <option value="small">Small</option>
          <option value="medium">Medium</option>
          <option value="large">Large</option>
        </select>
      </Row>
      {bool("showBadges", "Show badges")}
      {bool("sevenTv", "7TV emotes")}
      {bool("bttv", "BetterTTV emotes")}
      {bool("ffz", "FrankerFaceZ emotes")}
      {bool("sevenTvCosmetics", "7TV cosmetics", "Name paints and badges from 7TV")}

      <H>Moderation</H>
      <Row label="Deleted messages">
        <select
          value={settings.deletedMessages}
          onChange={(e) => setSettings({ deletedMessages: e.target.value as Settings["deletedMessages"] })}
          className={selectCls}
          aria-label="Deleted messages"
        >
          <option value="strike">Strike through</option>
          <option value="hide">Hide</option>
          <option value="show">Show normally</option>
        </select>
      </Row>
      {bool("showModButtons", "Show mod buttons", "Inline buttons on rows when mod mode is on")}
    </div>
  );
}

// ---- ignores -------------------------------------------------------------------------------

const smallSelect =
  "rounded border border-[var(--color-line)] bg-[var(--color-app)] px-1.5 py-1 text-[12px] outline-none focus:border-[var(--color-accent)]";

/** A two-state chip like a find bar's ".*" / "Aa" buttons. */
function Chip({ on, onChange, label, title }: { on: boolean; onChange: (v: boolean) => void; label: string; title: string }) {
  return (
    <button
      type="button"
      title={title}
      aria-label={title}
      aria-pressed={on}
      onClick={() => onChange(!on)}
      className={
        "shrink-0 rounded border px-1.5 py-1 font-mono text-[11px] leading-none " +
        (on
          ? "border-[var(--color-accent)] bg-[var(--color-mention)] text-[var(--color-text)]"
          : "border-[var(--color-line)] text-[var(--color-faint)] hover:text-[var(--color-text)]")
      }
    >
      {label}
    </button>
  );
}

function IconBtn({ title, onClick }: { title: string; onClick: () => void }) {
  return (
    <button type="button" title={title} aria-label={title} onClick={onClick} className="shrink-0 rounded p-1 text-[var(--color-faint)] hover:bg-[var(--color-raised-2)] hover:text-red-400">
      <TrashIcon className="h-3.5 w-3.5" />
    </button>
  );
}

const NEW_PHRASE: IgnoredPhrase = { pattern: "", regex: false, caseSensitive: false, action: "hide", replacement: "" };

/** One phrase rule. Text fields are edited locally and saved on blur / Enter (an empty pattern is never saved). */
function PhraseEditor({
  value,
  onSave,
  onRemove,
  adding,
}: {
  value: IgnoredPhrase;
  onSave: (p: IgnoredPhrase) => void;
  onRemove?: () => void;
  adding?: boolean;
}) {
  const [draft, setDraft] = useState(value);
  // Re-sync only when the saved rule itself changes (not on every unrelated settings broadcast).
  const saved = JSON.stringify(value);
  useEffect(() => setDraft(JSON.parse(saved)), [saved]);
  const err = draft.pattern ? phraseError(draft) : null;
  const commit = (p: IgnoredPhrase) => {
    if (adding) return;
    if (p.pattern && JSON.stringify(p) !== JSON.stringify(value)) onSave(p);
  };
  const add = () => {
    if (!draft.pattern || err) return;
    onSave(draft);
    setDraft(NEW_PHRASE);
  };
  const set = (patch: Partial<IgnoredPhrase>, save: boolean) => {
    const next = { ...draft, ...patch };
    setDraft(next);
    if (save) commit(next);
  };
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key !== "Enter") return;
    if (adding) add();
    else commit(draft);
  };

  return (
    <div className="border-b border-[var(--color-line)]/60 py-1.5 last:border-0">
      <div className="flex items-center gap-1.5">
        <input
          value={draft.pattern}
          onChange={(e) => set({ pattern: e.target.value }, false)}
          onBlur={() => commit(draft)}
          onKeyDown={onKey}
          placeholder={draft.regex ? "Regular expression" : "Phrase"}
          spellCheck={false}
          aria-label="Phrase"
          className={`${inputCls} min-w-0 flex-1 ${draft.regex ? "font-mono" : ""} ${err ? "border-red-500/60" : ""}`}
        />
        <Chip on={draft.regex} onChange={(v) => set({ regex: v }, true)} label=".*" title="Regular expression" />
        <Chip on={draft.caseSensitive} onChange={(v) => set({ caseSensitive: v }, true)} label="Aa" title="Match case" />
        <select
          value={draft.action}
          onChange={(e) => set({ action: e.target.value as IgnoredPhrase["action"] }, true)}
          className={smallSelect}
          aria-label="What to do with a match"
        >
          <option value="hide">Hide message</option>
          <option value="replace">Replace with</option>
        </select>
        {draft.action === "replace" && (
          <input
            value={draft.replacement}
            onChange={(e) => set({ replacement: e.target.value }, false)}
            onBlur={() => commit(draft)}
            onKeyDown={onKey}
            placeholder="(nothing)"
            spellCheck={false}
            aria-label="Replacement"
            className={`${inputCls} !w-28 shrink-0`}
          />
        )}
        {adding ? (
          <Btn small primary disabled={!draft.pattern || !!err} onClick={add}>
            Add
          </Btn>
        ) : (
          onRemove && <IconBtn title="Remove" onClick={onRemove} />
        )}
      </div>
      {err && <div className="mt-1 text-[11px] text-red-400">Invalid regular expression: {err}</div>}
    </div>
  );
}

function IgnoresSection({ settings, setSettings }: { settings: Settings; setSettings: SetSettings }) {
  const users = settings.ignoredUsers ?? [];
  const phrases = settings.ignoredPhrases ?? [];
  const [platform, setPlatform] = useState<Platform>("twitch");
  const [login, setLogin] = useState("");

  const addUser = () => {
    const l = ignoreLogin({ login });
    if (!l) return;
    setSettings({ ignoredUsers: toggleIgnoredUser(users, platform, l, true) });
    setLogin("");
  };
  const sortedUsers = users
    .map((u, i) => ({ u, i }))
    .sort((a, b) => PLATFORMS.indexOf(a.u.platform) - PLATFORMS.indexOf(b.u.platform) || a.u.login.localeCompare(b.u.login));

  return (
    <div>
      <H>Ignored users</H>
      <div className="mb-2 text-[11px] text-[var(--color-faint)]">
        Messages from these users are hidden in every split. You can also use "Ignore user" on a user card.
      </div>
      <div className="mb-2 flex items-center gap-1.5">
        <select value={platform} onChange={(e) => setPlatform(e.target.value as Platform)} className={smallSelect} aria-label="Platform">
          {PLATFORMS.map((p) => (
            <option key={p} value={p}>
              {PLATFORM_LABEL[p]}
            </option>
          ))}
        </select>
        <input
          value={login}
          onChange={(e) => setLogin(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && addUser()}
          placeholder={platform === "youtube" ? "Channel name" : "Username"}
          spellCheck={false}
          aria-label="Username to ignore"
          className={`${inputCls} min-w-0 flex-1`}
        />
        <Btn small primary disabled={!ignoreLogin({ login })} onClick={addUser}>
          Add
        </Btn>
      </div>
      <Card>
        {sortedUsers.length === 0 ? (
          <div className="text-[var(--color-faint)]">No ignored users.</div>
        ) : (
          sortedUsers.map(({ u, i }) => (
            <div key={`${u.platform}:${u.login}`} className="flex items-center gap-2 border-b border-[var(--color-line)]/60 py-1 last:border-0">
              <PlatformIcon platform={u.platform} className="h-3.5 w-3.5 shrink-0" />
              <span className="min-w-0 flex-1 truncate">{u.login}</span>
              <IconBtn title={`Stop ignoring ${u.login}`} onClick={() => setSettings({ ignoredUsers: users.filter((_, j) => j !== i) })} />
            </div>
          ))
        )}
      </Card>

      <H>Ignored phrases</H>
      <div className="mb-2 text-[11px] text-[var(--color-faint)]">
        A message containing the phrase is hidden, or the matching text is replaced. <span className="font-mono">.*</span> treats the phrase as a regular
        expression, <span className="font-mono">Aa</span> matches case.
      </div>
      <PhraseEditor value={NEW_PHRASE} adding onSave={(p) => setSettings({ ignoredPhrases: [...phrases, p] })} />
      <Card>
        {phrases.length === 0 ? (
          <div className="text-[var(--color-faint)]">No ignored phrases.</div>
        ) : (
          phrases.map((p, i) => (
            <PhraseEditor
              key={i}
              value={p}
              onSave={(next) => setSettings({ ignoredPhrases: phrases.map((x, j) => (j === i ? next : x)) })}
              onRemove={() => setSettings({ ignoredPhrases: phrases.filter((_, j) => j !== i) })}
            />
          ))
        )}
      </Card>
    </div>
  );
}

// ---- hotkeys ----------------------------------------------------------------------------

const chipCls = "inline-flex items-center gap-1 rounded border px-1.5 py-px text-[11px] tabular-nums";

function HotkeysSection({ settings, setSettings }: { settings: Settings; setSettings: SetSettings }) {
  const table = resolveHotkeys(settings.hotkeys);
  const conflicts = findConflicts(table);
  const [recording, setRecording] = useState<HotkeyAction | null>(null);
  const [hint, setHint] = useState("");
  const [pending, setPending] = useState<{ action: HotkeyAction; binding: string; other: HotkeyAction } | null>(null);

  const save = (next: HotkeyTable) => setSettings({ hotkeys: toOverrides(next) });
  const tableRef = useRef(table);
  tableRef.current = table;
  const saveRef = useRef(save);
  saveRef.current = save;

  // Press-to-record: while on, the app's shortcuts and Esc-closes-dialog stand aside (hotkeyRecording).
  useEffect(() => {
    if (!recording) return;
    hotkeyRecording.set(true);
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape" && !e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey) {
        setRecording(null);
        return;
      }
      const b = eventToBinding(e);
      if (!b) return; // a modifier on its own: keep waiting
      if (!isUsableBinding(b)) {
        setHint(`${b} would get in the way of typing. Hold Ctrl or Alt, or use a function key.`);
        return;
      }
      const t = tableRef.current;
      setRecording(null);
      setHint("");
      if (t[recording].includes(b)) return;
      const other = HOTKEY_ACTIONS.find((a) => a.id !== recording && t[a.id].includes(b));
      if (other) setPending({ action: recording, binding: b, other: other.id });
      else saveRef.current({ ...t, [recording]: [...t[recording], b] });
    };
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      hotkeyRecording.set(false);
    };
  }, [recording]);

  const groups = [...new Set(HOTKEY_ACTIONS.map((a) => a.group))];
  return (
    <div>
      <div className="mb-2 flex items-center justify-between">
        <H>Hotkeys</H>
        <Btn small disabled={!Object.keys(toOverrides(table)).length} onClick={() => setSettings({ hotkeys: {} })}>
          Reset all
        </Btn>
      </div>
      <div className="mb-2 text-[11px] text-[var(--color-faint)]">Click + and press the new shortcut (Esc cancels). An action can have several shortcuts.</div>
      {groups.map((g) => (
        <div key={g} className="mb-3">
          <div className="mb-1 text-[11px] uppercase tracking-wide text-[var(--color-faint)]">{g}</div>
          {HOTKEY_ACTIONS.filter((a) => a.group === g).map((a) => {
            const list = table[a.id];
            const isDefault = list.length === a.defaults.length && list.every((b, i) => b === a.defaults[i]);
            const rec = recording === a.id;
            return (
              <div key={a.id} className="border-b border-[var(--color-line)]/60 py-1 last:border-0" data-hotkey={a.id}>
                <div className="flex items-center gap-2">
                  <div className="w-40 shrink-0">{a.label}</div>
                  <div className="flex min-w-0 flex-1 flex-wrap items-center gap-1">
                    {list.map((b) => {
                      const shared = conflicts.get(b)?.filter((x) => x !== a.id);
                      return (
                        <span
                          key={b}
                          className={`${chipCls} ${shared?.length ? "border-amber-500/60 text-amber-300" : "border-[var(--color-line)] bg-[var(--color-app)]"}`}
                          title={shared?.length ? `Also bound to ${shared.map(actionLabel).join(", ")}` : undefined}
                        >
                          {b}
                          <button
                            type="button"
                            aria-label={`Remove ${b}`}
                            onClick={() => save({ ...table, [a.id]: list.filter((x) => x !== b) })}
                            className="text-[var(--color-faint)] hover:text-red-400"
                          >
                            <XIcon className="h-2.5 w-2.5" />
                          </button>
                        </span>
                      );
                    })}
                    {!list.length && !rec && <span className="text-[11px] text-[var(--color-faint)]">none</span>}
                    {rec ? (
                      <span className={`${chipCls} animate-pulse border-[var(--color-accent)] text-[var(--color-accent-soft)]`}>Press keys…</span>
                    ) : (
                      <button
                        type="button"
                        title="Add a shortcut"
                        aria-label={`Add a shortcut for ${a.label}`}
                        onClick={() => {
                          setPending(null);
                          setHint("");
                          setRecording(a.id);
                        }}
                        className={`${chipCls} border-dashed border-[var(--color-line)] text-[var(--color-muted)] hover:border-[var(--color-accent)] hover:text-[var(--color-text)]`}
                      >
                        +
                      </button>
                    )}
                  </div>
                  <Btn small disabled={isDefault} title={`Default: ${a.defaults.join(", ")}`} onClick={() => save({ ...table, [a.id]: a.defaults.slice() })}>
                    Reset
                  </Btn>
                </div>
                {rec && hint && <div className="mt-1 pl-42 text-[11px] text-amber-300">{hint}</div>}
                {pending?.action === a.id && (
                  <div className="mt-1 flex flex-wrap items-center gap-2 rounded border border-amber-500/40 bg-amber-500/10 px-2 py-1 text-[11px] text-amber-200">
                    <span className="min-w-0 flex-1">
                      {pending.binding} is already used by "{actionLabel(pending.other)}".
                    </span>
                    <Btn
                      small
                      onClick={() => {
                        save({ ...table, [pending.other]: table[pending.other].filter((x) => x !== pending.binding), [a.id]: [...list, pending.binding] });
                        setPending(null);
                      }}
                    >
                      Move it here
                    </Btn>
                    <Btn small onClick={() => setPending(null)}>
                      Cancel
                    </Btn>
                  </div>
                )}
              </div>
            );
          })}
        </div>
      ))}
    </div>
  );
}

// ---- mod buttons --------------------------------------------------------------------------

const ACTION_LABEL: Record<ModButtonAction, string> = { ban: "Ban", timeout: "Timeout", warn: "Warn", delete: "Delete message", command: "Command" };

function ModButtonsSection({ settings, setSettings }: { settings: Settings; setSettings: SetSettings }) {
  const buttons = normalizeModButtons(settings.modButtons);
  const save = (next: ModButton[]) => setSettings({ modButtons: next });
  const patch = (id: string, p: Partial<ModButton>) => save(buttons.map((b) => (b.id === id ? { ...b, ...p } : b)));
  const move = (i: number, d: -1 | 1) => {
    const j = i + d;
    if (j < 0 || j >= buttons.length) return;
    const next = buttons.slice();
    [next[i], next[j]] = [next[j], next[i]];
    save(next);
  };
  const isDefault = JSON.stringify(buttons) === JSON.stringify(DEFAULT_MOD_BUTTONS);

  return (
    <div>
      <div className="mb-2 flex items-center justify-between">
        <H>Mod buttons</H>
        <Btn small disabled={isDefault} onClick={() => save(DEFAULT_MOD_BUTTONS)}>
          Reset to defaults
        </Btn>
      </div>
      <div className="mb-2 text-[11px] text-[var(--color-faint)]">
        Shown in this order at the start of every message when a split's mod mode is on. Ban, timeout and delete work wherever you can moderate; commands run
        in Twitch chats.
      </div>
      <Card>
        <div className="flex items-center gap-1 text-[13px]" aria-label="Preview">
          <span className="inline-flex items-center gap-px">
            {buttons.map((b) => (
              <span key={b.id} title={describeModButton(b)} className="inline-flex h-4 min-w-4 items-center justify-center rounded text-[var(--color-faint)]">
                <ModButtonFace button={b} />
              </span>
            ))}
          </span>
          <span className="font-semibold text-[var(--color-accent-soft)]">viewer</span>
          <span className="text-[var(--color-muted)]">:</span>
          <span>example message</span>
        </div>
        {!buttons.length && <div className="mt-1 text-[11px] text-[var(--color-faint)]">No buttons: rows show none in mod mode.</div>}
      </Card>
      {buttons.map((b, i) => (
        <ModButtonEditor
          key={b.id}
          b={b}
          first={i === 0}
          last={i === buttons.length - 1}
          onPatch={(p) => patch(b.id, p)}
          onMove={(d) => move(i, d)}
          onRemove={() => save(buttons.filter((x) => x.id !== b.id))}
        />
      ))}
      <Btn
        disabled={buttons.length >= MAX_MOD_BUTTONS}
        onClick={() => save([...buttons, { id: newModButtonId(), action: "timeout", icon: "clock", label: "1m", seconds: 60, command: "" }])}
        title="Adds a 1 minute timeout; change it below"
      >
        Add button
      </Btn>
    </div>
  );
}

function ModButtonEditor({
  b,
  first,
  last,
  onPatch,
  onMove,
  onRemove,
}: {
  b: ModButton;
  first: boolean;
  last: boolean;
  onPatch: (p: Partial<ModButton>) => void;
  onMove: (d: -1 | 1) => void;
  onRemove: () => void;
}) {
  const selectCls = "rounded border border-[var(--color-line)] bg-[var(--color-app)] px-1.5 py-1 text-[12px] outline-none focus:border-[var(--color-accent)]";
  const iconBtn = "rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-raised-2)] hover:text-[var(--color-text)] disabled:opacity-30";
  const setAction = (action: ModButtonAction) => {
    // A new action gets that action's look (a "1m" label would mislabel a ban).
    const p: Partial<ModButton> = { action, icon: defaultIcon(action), label: "" };
    if (action === "timeout" && !b.seconds) p.seconds = 600;
    if (action === "command" && !b.command) p.command = "/warn {user} Please follow the chat rules";
    onPatch(p);
  };
  return (
    <div className="mb-1.5 rounded border border-[var(--color-line)] bg-[var(--color-raised)] px-2 py-1.5" data-mod-button={b.id}>
      <div className="flex items-center gap-1.5">
        <span className="flex h-6 w-8 shrink-0 items-center justify-center rounded border border-[var(--color-line)] bg-[var(--color-app)] text-[13px] text-[var(--color-muted)]">
          <ModButtonFace button={b} iconClass="h-3.5 w-3.5" />
        </span>
        <select value={b.action} onChange={(e) => setAction(e.target.value as ModButtonAction)} className={selectCls} aria-label="Action">
          {(Object.keys(ACTION_LABEL) as ModButtonAction[]).map((a) => (
            <option key={a} value={a}>
              {ACTION_LABEL[a]}
            </option>
          ))}
        </select>
        <div className="min-w-0 flex-1">
          {b.action === "timeout" ? (
            <div className="flex items-center gap-2">
              <div className="w-20 shrink-0">
                <CommitInput
                  key={`t${b.seconds}`}
                  initial={formatDuration(b.seconds)}
                  parse={(v) => parseDuration(v)}
                  onCommit={(secs) => onPatch({ seconds: secs })}
                  placeholder="10m"
                  label="Timeout length"
                  hint="30s, 10m, 1h, 1d, 1w"
                />
              </div>
              <span className="truncate text-[11px] text-[var(--color-faint)]">30s, 10m, 1h, 1d, up to 2w</span>
            </div>
          ) : b.action === "command" ? (
            <CommitInput
              key={`c${b.command}`}
              initial={b.command}
              parse={(v) => (v.trim() ? v.trim().slice(0, MAX_COMMAND) : null)}
              onCommit={(command) => onPatch({ command })}
              placeholder="/timeout {user} 600"
              label="Command"
              hint={TEMPLATE_HELP}
            />
          ) : null}
        </div>
        <button type="button" className={iconBtn} disabled={first} onClick={() => onMove(-1)} aria-label="Move up" title="Move up">
          <ChevronIcon className="h-3 w-3 rotate-180" />
        </button>
        <button type="button" className={iconBtn} disabled={last} onClick={() => onMove(1)} aria-label="Move down" title="Move down">
          <ChevronIcon className="h-3 w-3" />
        </button>
        <button type="button" className={`${iconBtn} hover:!text-red-400`} onClick={onRemove} aria-label="Remove button" title="Remove">
          <XIcon className="h-3 w-3" />
        </button>
      </div>
      <div className="mt-1 flex flex-wrap items-center gap-1 text-[11px] text-[var(--color-faint)]">
        <span className="mr-0.5">Icon</span>
        {MOD_BUTTON_ICONS.map((ic) => {
          const on = !b.label && b.icon === ic.id;
          return (
            <button
              key={ic.id}
              type="button"
              title={ic.label}
              aria-label={`Icon ${ic.label}`}
              aria-pressed={on}
              onClick={() => onPatch({ icon: ic.id, label: "" })}
              className={
                "flex h-5 w-5 items-center justify-center rounded border " +
                (on ? "border-[var(--color-accent)] bg-[var(--color-mention)] text-[var(--color-text)]" : "border-transparent text-[var(--color-muted)] hover:bg-[var(--color-raised-2)]")
              }
            >
              <ModIcon icon={ic.id} />
            </button>
          );
        })}
        <span className="ml-2 mr-0.5">or text</span>
        <input
          value={b.label}
          maxLength={MAX_LABEL}
          onChange={(e) => onPatch({ label: e.target.value.slice(0, MAX_LABEL), ...(e.target.value.trim() ? {} : { icon: b.icon || defaultIcon(b.action) }) })}
          placeholder="1d"
          spellCheck={false}
          aria-label="Button text"
          className="w-16 rounded border border-[var(--color-line)] bg-[var(--color-app)] px-1.5 py-0.5 text-[11px] text-[var(--color-text)] outline-none focus:border-[var(--color-accent)]"
        />
      </div>
      {b.action === "command" && (
        <div className="mt-1 text-[11px] text-[var(--color-faint)]">
          A slash command or a chat message. Placeholders: {TEMPLATE_HELP}.
        </div>
      )}
    </div>
  );
}

/** A text box that saves on Enter / blur, and only a value `parse` accepts (red border otherwise). */
function CommitInput<T>({
  initial,
  parse,
  onCommit,
  placeholder,
  label,
  hint,
}: {
  initial: string;
  parse: (v: string) => T | null;
  onCommit: (v: T) => void;
  placeholder: string;
  label: string;
  hint?: string;
}) {
  const [v, setV] = useState(initial);
  const bad = parse(v) === null;
  const commit = () => {
    const p = parse(v);
    if (p === null) setV(initial);
    else if (v !== initial) onCommit(p);
  };
  return (
    <input
      value={v}
      onChange={(e) => setV(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === "Enter") commit();
      }}
      placeholder={placeholder}
      title={hint}
      spellCheck={false}
      aria-label={label}
      aria-invalid={bad}
      className={`${inputCls} ${bad ? "!border-red-500/70" : ""}`}
    />
  );
}

// ---- updates ----------------------------------------------------------------------------

export function updaterText(u: UpdaterState | null): string {
  if (!u) return "Loading…";
  switch (u.status) {
    case "idle":
      return "Not checked yet.";
    case "dev":
      return "Updates are off in development builds.";
    case "checking":
      return "Checking for updates…";
    case "none":
      return "You are on the latest version.";
    case "available":
      return `Version ${u.version ?? "?"} is available.`;
    case "downloading":
      return `Downloading ${u.version ?? "the update"}… ${Math.round(u.progress)}%`;
    case "ready":
      return `Version ${u.version ?? "?"} is ready to install.`;
    case "error":
      return `Update check failed${u.error ? `: ${u.error}` : "."}`;
  }
}

function UpdatesSection({ settings, setSettings }: { settings: Settings; setSettings: SetSettings }) {
  const u = useUpdater();
  const info = useAppInfo();
  const busy = u?.status === "checking" || u?.status === "downloading";
  const [err, setErr] = useState("");

  return (
    <div>
      <H>Updates</H>
      <Card>
        <div className="mb-2 text-[11px] uppercase tracking-wide text-[var(--color-faint)]">Update channel</div>
        <div className="grid gap-2 sm:grid-cols-2">
          {(["beta", "stable"] as const).map((c) => (
            <button
              key={c}
              type="button"
              onClick={() => setSettings({ updateChannel: c })}
              aria-pressed={settings.updateChannel === c}
              className={
                "rounded border p-2 text-left " +
                (settings.updateChannel === c ? "border-[var(--color-accent)] bg-[var(--color-mention)]" : "border-[var(--color-line)] hover:bg-[var(--color-raised-2)]")
              }
            >
              <div className="font-semibold">{c === "beta" ? "Beta" : "Stable"}</div>
              <div className="text-[11px] text-[var(--color-muted)]">
                {c === "beta" ? "Prereleases: the newest features, with rough edges." : "Full releases only: fewer updates, better tested."}
              </div>
            </button>
          ))}
        </div>
      </Card>

      <Card>
        <div className="flex items-center justify-between gap-3">
          <div>
            <div>
              Current version <span className="font-semibold">{info?.version ?? "…"}</span>
              {info && !info.packaged && <span className="text-[var(--color-faint)]"> (development build)</span>}
            </div>
            <div className="mt-0.5 text-[var(--color-muted)]" data-updater-status={u?.status}>
              {updaterText(u)}
            </div>
          </div>
          <div className="flex shrink-0 gap-2">
            <Btn disabled={busy} onClick={() => agu.updater.check().catch((e) => setErr(errMsg(e)))}>
              Check now
            </Btn>
            {(u?.status === "available" || u?.status === "ready") && (
              <Btn primary onClick={() => agu.updater.install().catch((e) => setErr(errMsg(e)))}>
                Restart and update
              </Btn>
            )}
          </div>
        </div>
        {u?.status === "downloading" && (
          <div className="mt-2 h-1.5 overflow-hidden rounded bg-[var(--color-app)]">
            <div className="h-full bg-[var(--color-accent)]" style={{ width: `${Math.max(2, Math.min(100, u.progress))}%` }} />
          </div>
        )}
        {u && u.notes.length > 0 && (
          <ul className="mt-2 list-disc space-y-0.5 pl-5 text-[11px] text-[var(--color-muted)]">
            {u.notes.map((n, i) => (
              <li key={i}>{n}</li>
            ))}
          </ul>
        )}
        <Err>{err}</Err>
      </Card>
    </div>
  );
}

// ---- about -------------------------------------------------------------------------------

function AboutSection() {
  const info = useAppInfo();
  return (
    <div>
      <H>About</H>
      <Card>
        <div className="text-[16px] font-bold">Agu Chat</div>
        <div className="text-[var(--color-muted)]">Version {info?.version ?? "…"}</div>
        <div className="mt-2 text-[var(--color-muted)]">Live chat for Twitch, Kick, YouTube, Rumble and X in one window.</div>
        <div className="mt-3">
          <Link url={REPO_URL}>{REPO_URL}</Link>
        </div>
      </Card>
    </div>
  );
}
