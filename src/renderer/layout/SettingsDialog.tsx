// Settings dialog: Accounts, Appearance/chat, Ignores, Updates, About.
import { useEffect, useState, type ReactNode } from "react";
import { agu } from "../bridge";
import { useAccounts } from "../hooks";
import { PLATFORMS, type AccountState, type DevicePrompt, type IgnoredPhrase, type Platform, type Settings, type UpdaterState } from "../protocol";
import { ignoreLogin, phraseError, toggleIgnoredUser } from "../chat/ignores";
import { PLATFORM_LABEL, PlatformIcon } from "../platformIcons";
import { CopyIcon, TrashIcon } from "../icons";
import { Btn, Modal, Toggle, copyText, inputCls } from "./ui";
import { useAppInfo, useLiveSettings, useUpdater } from "./useBridge";

export const REPO_URL = "https://github.com/TrinityTF/agu-chat";

type Section = "accounts" | "appearance" | "ignores" | "updates" | "about";
const SECTIONS: { id: Section; label: string }[] = [
  { id: "accounts", label: "Accounts" },
  { id: "appearance", label: "Appearance and chat" },
  { id: "ignores", label: "Ignores" },
  { id: "updates", label: "Updates" },
  { id: "about", label: "About" },
];

export function SettingsDialog({ onClose, initial = "accounts" }: { onClose: () => void; initial?: Section }) {
  const [section, setSection] = useState<Section>(initial);
  const [settings, setSettings] = useLiveSettings();

  return (
    <Modal title="Settings" onClose={onClose} width={780}>
      <div className="flex h-[min(72vh,540px)] text-[12px]">
        <nav className="w-40 shrink-0 border-r border-[var(--color-line)] bg-[var(--color-app)] py-2">
          {SECTIONS.map((s) => (
            <button
              key={s.id}
              type="button"
              onClick={() => setSection(s.id)}
              aria-current={section === s.id}
              className={
                "block w-full border-l-2 px-3 py-1.5 text-left " +
                (section === s.id
                  ? "border-[var(--color-accent)] bg-[var(--color-raised)] text-[var(--color-text)]"
                  : "border-transparent text-[var(--color-muted)] hover:bg-[var(--color-raised)] hover:text-[var(--color-text)]")
              }
            >
              {s.label}
            </button>
          ))}
        </nav>
        <div className="min-w-0 flex-1 overflow-y-auto p-4">
          {!settings ? (
            <div className="text-[var(--color-faint)]">Loading…</div>
          ) : section === "accounts" ? (
            <AccountsSection settings={settings} setSettings={setSettings} />
          ) : section === "appearance" ? (
            <AppearanceSection settings={settings} setSettings={setSettings} />
          ) : section === "ignores" ? (
            <IgnoresSection settings={settings} setSettings={setSettings} />
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
      <div>
        <div>
          Logged in as <span className="font-semibold">{st.user?.name || st.user?.login || "unknown"}</span>
        </div>
        {st.scopes && st.scopes.length > 0 && (
          <div className="mt-0.5 max-w-[420px] truncate text-[11px] text-[var(--color-faint)]" title={st.scopes.join(" ")}>
            {st.scopes.length} permissions: {st.scopes.join(", ")}
          </div>
        )}
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

const TWITCH_STEPS: ReactNode[] = [
  <>
    Go to <Link url="https://dev.twitch.tv/console/apps">https://dev.twitch.tv/console/apps</Link>
  </>,
  <>
    Click <b>Register Your Application</b>
  </>,
  <>
    Name: <code className="text-[var(--color-text)]">Agu Chat (&lt;your name&gt;)</code>, it must be unique on Twitch
  </>,
  <>
    OAuth Redirect URL: <code className="text-[var(--color-text)]">http://localhost</code> (the form requires one, it is not used)
  </>,
  <>
    Category: <b>Chat Bot</b>
  </>,
  <>
    Client Type: <b>Public</b>
  </>,
  <>
    Click <b>Create</b>, then <b>Manage</b> on the new app
  </>,
  <>Copy the Client ID and paste it below</>,
];

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
  const hasId = !!settings.twitchClientId;

  const saveId = () => {
    const v = clientId.trim();
    if (!v) return;
    setSettings({ twitchClientId: v });
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

  return (
    <Card>
      <AccountTitle platform="twitch" />
      {!hasId || changing ? (
        <div>
          {!hasId && (
            <>
              <div className="mb-2 text-[var(--color-muted)]">
                Agu Chat logs in with your own Twitch application, so nothing is shared with anyone else. One-time setup:
              </div>
              <ol className="mb-3 list-decimal space-y-1 pl-5 text-[var(--color-muted)]">
                {TWITCH_STEPS.map((s, i) => (
                  <li key={i}>{s}</li>
                ))}
              </ol>
            </>
          )}
          <div className="flex gap-1.5">
            <input
              value={clientId}
              onChange={(e) => setClientId(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && saveId()}
              placeholder="Client ID"
              spellCheck={false}
              className={inputCls}
              aria-label="Twitch Client ID"
            />
            <Btn primary disabled={!clientId.trim()} onClick={saveId}>
              Save
            </Btn>
            {hasId && <Btn onClick={() => setChanging(false)}>Cancel</Btn>}
          </div>
        </div>
      ) : st.connected ? (
        <ConnectedLine st={st} onLogout={() => agu.accounts.logout("twitch").catch((e) => setErr(errMsg(e)))} />
      ) : pending ? (
        <DeviceCode prompt={pending} onCancel={cancel} />
      ) : (
        <div className="flex items-center gap-3">
          <Btn primary disabled={busy} onClick={login}>
            Log in with Twitch
          </Btn>
          <span className="text-[11px] text-[var(--color-faint)]">
            Client ID {settings.twitchClientId.slice(0, 6)}… <button type="button" className="underline hover:text-[var(--color-text)]" onClick={() => setChanging(true)}>change</button>
          </span>
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
