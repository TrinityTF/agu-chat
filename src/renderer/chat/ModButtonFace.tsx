// What one mod button looks like (its icon or short label) and its hover tone. Shared by the chat rows and
// the "Mod buttons" settings page so the preview there matches the rows.
import type { ComponentType } from "react";
import type { ModButton, ModButtonIcon } from "../protocol";
import { ChatIcon, CheckIcon, ClockIcon, EyeOffIcon, FlagIcon, MinusCircleIcon, PinIcon, ShieldIcon, TrashIcon, WarningIcon } from "../icons";

const ICONS: Record<ModButtonIcon, ComponentType<{ className?: string }>> = {
  ban: MinusCircleIcon,
  clock: ClockIcon,
  trash: TrashIcon,
  shield: ShieldIcon,
  warning: WarningIcon,
  flag: FlagIcon,
  eyeoff: EyeOffIcon,
  chat: ChatIcon,
  pin: PinIcon,
  check: CheckIcon,
};

export function ModIcon({ icon, className = "h-3 w-3" }: { icon: ModButtonIcon; className?: string }) {
  const I = ICONS[icon] ?? ChatIcon;
  return <I className={className} />;
}

export function ModButtonFace({ button, iconClass = "h-3 w-3" }: { button: ModButton; iconClass?: string }) {
  if (button.label) return <span className="px-px text-[0.72em] font-bold leading-none">{button.label}</span>;
  return <ModIcon icon={button.icon || "chat"} className={iconClass} />;
}

export function modButtonTone(b: ModButton): string {
  return b.action === "timeout" ? "hover:text-amber-400" : b.action === "command" ? "hover:text-accent-soft" : "hover:text-red-400";
}
