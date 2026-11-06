// Dev-only harness (not imported by index.html): renders the settings dialog on the dev mock bridge, so the
// native port's pages can be compared with it. Open /layout/dev/settings.html?s=appearance on the vite dev server.
import "../devMock";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "../../index.css";
import { SettingsDialog } from "../SettingsDialog";

const s = new URLSearchParams(location.search).get("s") ?? "accounts";
createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <SettingsDialog onClose={() => {}} initial={s as never} />
  </StrictMode>,
);
