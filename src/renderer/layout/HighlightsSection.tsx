// Settings > Highlights: the own-name mention alerts, the alert sound, and the user's highlight rules
// (add, edit, reorder, delete), plus a small tester to check a rule against a sample message.
import { useEffect, useMemo, useState, type ReactNode } from "react";
import { agu } from "../bridge";
import { playSound } from "../alerts";
import type { HighlightRule, Settings } from "../protocol";
import { ChevronIcon, SpeakerIcon, TrashIcon } from "../icons";
import { compileHighlights, nextRuleColor, regexError, ruleRowStyle } from "../chat/highlights";
import { Btn, Toggle, inputCls } from "./ui";
import { uid } from "./tree";

type SetSettings = (p: Partial<Settings>) => void;

const KIND_LABEL: Record<HighlightRule["kind"], string> = { phrase: "Phrase", regex: "Regex", user: "User" };
const PLACEHOLDER: Record<HighlightRule["kind"], string> = {
  phrase: "Word or phrase, e.g. giveaway",
  regex: "Regular expression, e.g. ^!(drop|claim)",
  user: "Names, separated by commas",
};

const iconBtn =
  "flex h-6 w-6 shrink-0 items-center justify-center rounded text-[var(--color-muted)] hover:bg-[var(--color-raised-2)] hover:text-[var(--color-text)] disabled:opacity-30 disabled:hover:bg-transparent";
const selectCls = "rounded border border-[var(--color-line)] bg-[var(--color-app)] px-1.5 py-1 text-[12px] outline-none focus:border-[var(--color-accent)]";

function H({ children, right }: { children: ReactNode; right?: ReactNode }) {
  return (
    <div className="mb-2 mt-1 flex items-center justify-between">
      <h3 className="text-[13px] font-semibold text-[var(--color-text)]">{children}</h3>
      {right}
    </div>
  );
}

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

function Check({ checked, onChange, children, title }: { checked: boolean; onChange: (v: boolean) => void; children: ReactNode; title?: string }) {
  return (
    <label className="inline-flex cursor-default items-center gap-1 text-[var(--color-muted)] hover:text-[var(--color-text)]" title={title}>
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} className="h-3 w-3 accent-[var(--color-accent)]" />
      {children}
    </label>
  );
}

const errMsg = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** Shown name of a stored sound file: drop the "-<hash>" suffix main adds. */
const soundLabel = (file: string) => file.replace(/-[0-9a-f]{8}(\.[a-z0-9]+)$/i, "$1");

async function pickSound(onErr: (m: string) => void): Promise<string | null> {
  try {
    const r = await agu.sounds.pick();
    return r ? r.file : null;
  } catch (e) {
    onErr(errMsg(e));
    return null;
  }
}

export function HighlightsSection({ settings, setSettings }: { settings: Settings; setSettings: SetSettings }) {
  const rules = settings.highlightRules;
  const [err, setErr] = useState("");
  const [focusId, setFocusId] = useState<string | null>(null);
  const setRules = (next: HighlightRule[]) => setSettings({ highlightRules: next });
  const update = (id: string, patch: Partial<HighlightRule>) => setRules(rules.map((r) => (r.id === id ? { ...r, ...patch } : r)));
  const move = (i: number, dir: -1 | 1) => {
    const j = i + dir;
    if (j < 0 || j >= rules.length) return;
    const next = rules.slice();
    [next[i], next[j]] = [next[j], next[i]];
    setRules(next);
  };
  const add = () => {
    const rule: HighlightRule = {
      id: uid(),
      enabled: true,
      kind: "phrase",
      pattern: "",
      caseSensitive: false,
      color: nextRuleColor(rules),
      sound: false,
      soundFile: "",
      flash: false,
    };
    setRules([...rules, rule]);
    setFocusId(rule.id);
  };

  return (
    <div>
      <H>Your name</H>
      <div className="mb-1 text-[11px] text-[var(--color-faint)]">Twitch messages that mention your name are always highlighted in red.</div>
      <div className="mb-4">
        <Row label="Play a sound">
          <Toggle checked={settings.mentionSound} onChange={(v) => setSettings({ mentionSound: v })} label="Play a sound when you are mentioned" />
        </Row>
        <Row label="Flash the taskbar" hint="Only while Agu Chat isn't the active window">
          <Toggle checked={settings.mentionFlash} onChange={(v) => setSettings({ mentionFlash: v })} label="Flash the taskbar when you are mentioned" />
        </Row>
      </div>

      <H>Alert sound</H>
      <div className="mb-4">
        <Row label="Default sound" hint="Used by rules and mentions that don't pick their own file">
          <div className="flex items-center gap-1.5">
            <span className="max-w-[160px] truncate text-[var(--color-muted)]" title={settings.highlightSoundFile || undefined}>
              {settings.highlightSoundFile ? soundLabel(settings.highlightSoundFile) : "Built-in ping"}
            </span>
            <Btn
              small
              onClick={async () => {
                setErr("");
                const f = await pickSound(setErr);
                if (f) setSettings({ highlightSoundFile: f });
              }}
            >
              Choose file…
            </Btn>
            {settings.highlightSoundFile && (
              <Btn small onClick={() => setSettings({ highlightSoundFile: "" })}>
                Use ping
              </Btn>
            )}
            <button type="button" className={iconBtn} title="Play" aria-label="Play the default sound" onClick={() => void playSound(settings.highlightSoundFile, settings.highlightVolume)}>
              <SpeakerIcon className="h-3.5 w-3.5" />
            </button>
          </div>
        </Row>
        <Row label="Volume">
          <div className="flex items-center gap-2">
            <input
              type="range"
              min={0}
              max={100}
              step={5}
              value={settings.highlightVolume}
              onChange={(e) => setSettings({ highlightVolume: Number(e.target.value) })}
              className="w-32 accent-[var(--color-accent)]"
              aria-label="Alert volume"
            />
            <span className="w-8 text-right tabular-nums text-[var(--color-muted)]">{settings.highlightVolume}%</span>
          </div>
        </Row>
        <Row label="Play sounds while Agu Chat is focused" hint="Off: sounds only play when you're in another window">
          <Toggle checked={settings.highlightSoundFocused} onChange={(v) => setSettings({ highlightSoundFocused: v })} label="Play sounds while Agu Chat is focused" />
        </Row>
        {err && <div className="mt-1 text-[11px] text-red-400">{err}</div>}
      </div>

      <H
        right={
          <Btn small primary onClick={add}>
            Add rule
          </Btn>
        }
      >
        Rules
      </H>
      <div className="mb-2 text-[11px] text-[var(--color-faint)]">
        For every platform. Checked top to bottom: the first rule that matches colours the message. Your own messages are never highlighted.
      </div>
      {rules.length === 0 ? (
        <div className="mb-3 rounded border border-dashed border-[var(--color-line)] px-3 py-4 text-center text-[var(--color-faint)]">
          No rules yet. Add one to highlight a word, a regular expression or a user.
        </div>
      ) : (
        <div className="mb-3 space-y-1.5">
          {rules.map((r, i) => (
            <RuleEditor
              key={r.id}
              rule={r}
              first={i === 0}
              last={i === rules.length - 1}
              autoFocus={focusId === r.id}
              volume={settings.highlightVolume}
              defaultSound={settings.highlightSoundFile}
              onChange={(patch) => update(r.id, patch)}
              onMove={(dir) => move(i, dir)}
              onDelete={() => setRules(rules.filter((x) => x.id !== r.id))}
            />
          ))}
        </div>
      )}
      {rules.length > 0 && <RuleTester rules={rules} />}
    </div>
  );
}

function RuleEditor({
  rule,
  first,
  last,
  autoFocus,
  volume,
  defaultSound,
  onChange,
  onMove,
  onDelete,
}: {
  rule: HighlightRule;
  first: boolean;
  last: boolean;
  autoFocus: boolean;
  volume: number;
  defaultSound: string;
  onChange: (patch: Partial<HighlightRule>) => void;
  onMove: (dir: -1 | 1) => void;
  onDelete: () => void;
}) {
  // The pattern is edited locally and saved on blur / Enter, so typing doesn't write settings per keystroke.
  const [draft, setDraft] = useState(rule.pattern);
  const [err, setErr] = useState("");
  useEffect(() => setDraft(rule.pattern), [rule.pattern]);
  const commit = () => {
    if (draft !== rule.pattern) onChange({ pattern: draft });
  };
  const reErr = rule.kind === "regex" && draft.trim() ? regexError(draft, rule.caseSensitive) : null;
  const dim = !rule.enabled;

  return (
    <div className="rounded border border-[var(--color-line)] bg-[var(--color-raised)] px-2 py-1.5" style={{ borderLeft: `3px solid ${rule.color}` }}>
      <div className="flex items-center gap-1.5">
        <Toggle checked={rule.enabled} onChange={(v) => onChange({ enabled: v })} label={rule.enabled ? "Rule on" : "Rule off"} />
        <select value={rule.kind} onChange={(e) => onChange({ kind: e.target.value as HighlightRule["kind"] })} className={selectCls} aria-label="Rule type">
          {(Object.keys(KIND_LABEL) as HighlightRule["kind"][]).map((k) => (
            <option key={k} value={k}>
              {KIND_LABEL[k]}
            </option>
          ))}
        </select>
        <input
          value={draft}
          autoFocus={autoFocus}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === "Enter") commit();
          }}
          placeholder={PLACEHOLDER[rule.kind]}
          spellCheck={false}
          className={`${inputCls} min-w-0 flex-1 font-mono ${reErr ? "!border-red-500/70" : ""} ${dim ? "opacity-50" : ""}`}
          aria-label="Pattern"
        />
        <input
          type="color"
          value={rule.color}
          onChange={(e) => onChange({ color: e.target.value })}
          className="h-6 w-7 shrink-0 cursor-default rounded border border-[var(--color-line)] bg-[var(--color-app)] p-0.5"
          title="Highlight colour"
          aria-label="Highlight colour"
        />
        <button type="button" className={iconBtn} disabled={first} onClick={() => onMove(-1)} title="Move up" aria-label="Move up">
          <ChevronIcon className="h-3.5 w-3.5 rotate-180" />
        </button>
        <button type="button" className={iconBtn} disabled={last} onClick={() => onMove(1)} title="Move down" aria-label="Move down">
          <ChevronIcon className="h-3.5 w-3.5" />
        </button>
        <button type="button" className={`${iconBtn} hover:!text-red-400`} onClick={onDelete} title="Delete rule" aria-label="Delete rule">
          <TrashIcon className="h-3.5 w-3.5" />
        </button>
      </div>
      <div className={`mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 pl-9 text-[11px] ${dim ? "opacity-50" : ""}`}>
        <Check checked={rule.caseSensitive} onChange={(v) => onChange({ caseSensitive: v })} title="Match upper and lower case exactly">
          Case sensitive
        </Check>
        <Check checked={rule.flash} onChange={(v) => onChange({ flash: v })} title="Only while Agu Chat isn't the active window">
          Flash taskbar
        </Check>
        <Check checked={rule.sound} onChange={(v) => onChange({ sound: v })}>
          Sound
        </Check>
        {rule.sound && (
          <span className="inline-flex items-center gap-1 text-[var(--color-faint)]">
            <select
              value={rule.soundFile ? "file" : "default"}
              onChange={async (e) => {
                setErr("");
                if (e.target.value === "default") onChange({ soundFile: "" });
                else {
                  const f = await pickSound(setErr);
                  if (f) onChange({ soundFile: f });
                }
              }}
              className="max-w-[180px] rounded border border-[var(--color-line)] bg-[var(--color-app)] px-1 py-px text-[11px] outline-none focus:border-[var(--color-accent)]"
              aria-label="Rule sound"
            >
              <option value="default">Default sound</option>
              <option value="file">{rule.soundFile ? soundLabel(rule.soundFile) : "Choose file…"}</option>
            </select>
            {rule.soundFile && (
              <button
                type="button"
                className="underline hover:text-[var(--color-text)]"
                onClick={async () => {
                  setErr("");
                  const f = await pickSound(setErr);
                  if (f) onChange({ soundFile: f });
                }}
              >
                change
              </button>
            )}
            <button type="button" className={iconBtn} title="Play" aria-label="Play this rule's sound" onClick={() => void playSound(rule.soundFile || defaultSound, volume)}>
              <SpeakerIcon className="h-3 w-3" />
            </button>
          </span>
        )}
      </div>
      {(reErr || err) && <div className="mt-1 pl-9 text-[11px] text-red-400">{reErr ? `Invalid regular expression: ${reErr}` : err}</div>}
    </div>
  );
}

/** Type a sample message and name to see which rule (if any) would highlight it. */
function RuleTester({ rules }: { rules: HighlightRule[] }) {
  const [text, setText] = useState("");
  const [user, setUser] = useState("");
  const highlighter = useMemo(() => compileHighlights(rules), [rules]);
  const hit = text || user ? highlighter.match({ text, login: user, name: user }) : null;
  const idx = hit ? rules.indexOf(hit) : -1;
  return (
    <div className="rounded border border-[var(--color-line)] bg-[var(--color-raised)] p-2">
      <div className="mb-1.5 text-[11px] uppercase tracking-wide text-[var(--color-faint)]">Try it</div>
      <div className="flex gap-1.5">
        <input value={user} onChange={(e) => setUser(e.target.value)} placeholder="User" spellCheck={false} className={`${inputCls} !w-32 shrink-0`} aria-label="Sample user" />
        <input value={text} onChange={(e) => setText(e.target.value)} placeholder="A sample message" spellCheck={false} className={inputCls} aria-label="Sample message" />
      </div>
      {(text || user) && (
        <div className="mt-1.5 rounded px-2 py-0.5" style={hit ? ruleRowStyle(hit.color) : undefined}>
          {hit ? (
            <>
              Rule {idx + 1} ({KIND_LABEL[hit.kind].toLowerCase()} <span className="font-mono">{hit.pattern}</span>) highlights this.
            </>
          ) : (
            <span className="text-[var(--color-faint)]">No rule matches.</span>
          )}
        </div>
      )}
    </div>
  );
}
