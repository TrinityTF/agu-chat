// devMock must come first: it installs a stand-in window.agu in a plain browser (dev only),
// and bridge.ts reads window.agu when its module is first evaluated.
import "./layout/devMock";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./index.css";
import { App } from "./layout/App";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
