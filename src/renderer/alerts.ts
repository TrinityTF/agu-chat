// Highlight alerts: the sound and the taskbar flash. Shared by every split, so a message that shows in two
// splits (the same channel open twice) alerts once, and a burst of matches plays one sound, not twenty.
//
// Sounds go through Web Audio: the built-in ping is synthesized (no asset to ship), a custom file is
// read from the app's sounds folder through the bridge and decoded once.
import { agu } from "./bridge";
import type { Settings } from "./protocol";

export type AlertWants = { sound: boolean; soundFile: string; flash: boolean };

const SOUND_GAP_MS = 400;
const FLASH_GAP_MS = 1000;
const SEEN_TTL_MS = 5 * 60_000;

let audio: AudioContext | null = null;
const decoded = new Map<string, Promise<AudioBuffer | null>>();
const seen = new Map<string, number>();
let lastSound = 0;
let lastFlash = 0;

function ctx(): AudioContext | null {
  try {
    if (!audio) audio = new AudioContext();
    if (audio.state === "suspended") void audio.resume();
    return audio;
  } catch {
    return null;
  }
}

function ping(ac: AudioContext, volume: number) {
  // Two short sine blips a fifth apart: audible over stream audio without being harsh.
  const out = ac.createGain();
  out.gain.value = volume;
  out.connect(ac.destination);
  const t0 = ac.currentTime + 0.01;
  for (const [freq, at] of [
    [988, 0],
    [1480, 0.09],
  ] as const) {
    const osc = ac.createOscillator();
    const env = ac.createGain();
    osc.type = "sine";
    osc.frequency.value = freq;
    env.gain.setValueAtTime(0.0001, t0 + at);
    env.gain.exponentialRampToValueAtTime(0.5, t0 + at + 0.012);
    env.gain.exponentialRampToValueAtTime(0.0001, t0 + at + 0.32);
    osc.connect(env).connect(out);
    osc.start(t0 + at);
    osc.stop(t0 + at + 0.34);
  }
}

function load(ac: AudioContext, file: string): Promise<AudioBuffer | null> {
  let p = decoded.get(file);
  if (!p) {
    p = agu.sounds
      .read(file)
      .then((bytes) => ac.decodeAudioData(bytes.slice().buffer as ArrayBuffer))
      .catch(() => null);
    decoded.set(file, p);
    // A failed load is retried next time (the file may have been put back).
    void p.then((b) => {
      if (!b) decoded.delete(file);
    });
  }
  return p;
}

/** Play a custom sound file, or the built-in ping when `file` is "" or cannot be played. */
export async function playSound(file: string, volume0to100: number): Promise<void> {
  const ac = ctx();
  if (!ac) return;
  const volume = Math.max(0, Math.min(100, volume0to100)) / 100;
  if (volume <= 0) return;
  const buf = file ? await load(ac, file) : null;
  if (!buf) {
    ping(ac, volume);
    return;
  }
  const src = ac.createBufferSource();
  const gain = ac.createGain();
  gain.gain.value = volume;
  src.buffer = buf;
  src.connect(gain).connect(ac.destination);
  src.start();
}

/**
 * Fire the alerts a highlighted message asks for, once per message across all splits.
 * `key` identifies the message (platform, channel and message id).
 */
export function alertOnce(key: string, wants: AlertWants, rs: Settings): void {
  if (!wants.sound && !wants.flash) return;
  const now = Date.now();
  if (seen.has(key)) return;
  seen.set(key, now);
  if (seen.size > 2000) for (const [k, t] of seen) if (now - t > SEEN_TTL_MS || seen.size > 1500) seen.delete(k);

  const focused = document.hasFocus();
  if (wants.sound && (rs.highlightSoundFocused || !focused) && now - lastSound >= SOUND_GAP_MS) {
    lastSound = now;
    void playSound(wants.soundFile || rs.highlightSoundFile, rs.highlightVolume);
  }
  if (wants.flash && !focused && now - lastFlash >= FLASH_GAP_MS) {
    lastFlash = now;
    agu.app.flash().catch(() => {});
  }
}
