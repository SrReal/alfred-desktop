import React from "react";
import { createRoot } from "react-dom/client";
import { App } from "./ui/App";
import "./theme.css";
document.documentElement.dataset.surface =
  new URLSearchParams(location.search).get("surface") ?? "chat";
createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
