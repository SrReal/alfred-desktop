// Public edition identity. No private avatar resources are imported.
import userAvatar from "../assets/branding/user-avatar.png";
import assistantAvatar from "../src-tauri/icons/tray.png";

export const identity = {
  edition: "public",
  userAvatar,
  assistantAvatar,
  userLabel: "Tú",
  assistantLabel: "Alfred",
} as const;
