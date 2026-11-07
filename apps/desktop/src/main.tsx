import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import App from "./App";
import { applyFont } from "./api";
import { lang } from "./i18n";
import "./styles.css";

// Kein Standard-Kontextmenü der Web-Ansicht (eigenes Menü in der Dateiansicht).
document.addEventListener("contextmenu", (e) => e.preventDefault());
document.documentElement.lang = lang;
// Native Dialoge in derselben Sprache
invoke("set_language", { code: lang }).catch(() => {});
// Eigene Schrift (falls gewählt), sonst Inconsolata
applyFont();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
