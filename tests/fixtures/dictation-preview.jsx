import React from "react";
import { createRoot } from "react-dom/client";
import { App } from "../../src/ui/App";
import "../../src/theme.css";
document.documentElement.dataset.surface = "chat";
createRoot(document.getElementById("root")).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
