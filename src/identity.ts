// Public edition identity. No private avatar resources are imported.
import userAvatar from "../assets/branding/user-avatar.png";
import assistantAvatar from "../assets/branding/assistant-avatar.png";

export const identity = {
  edition: "public",
  userAvatar,
  assistantAvatar,
  userLabel: "Tú",
  assistantLabel: "Alfred",
} as const;
