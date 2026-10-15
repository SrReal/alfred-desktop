// Browser-only visual fixture. Never imported by the production entry point.
export const desktop = false;
const listeners = new Map();
const emit = (name, data = {}) =>
  listeners.get(name)?.forEach((fn) => fn(data));
let session = null;
let conversations = [];
export async function request(operation, input = {}) {
  if (operation === "dictation.state") return session;
  if (operation === "dictation.start") {
    session = {
      id: "synthetic",
      conversation_id: input.conversation_id,
      status: "listening",
      text: "Alfred prepara las tareas de mañana",
      error: "",
    };
    emit("alfredpet:dictation");
    return session;
  }
  if (operation === "dictation.action") {
    if (input.action === "stop") {
      session = { ...session, status: "review" };
      emit("alfredpet:dictation");
      return session;
    }
    session = null;
    emit("alfredpet:dictation");
    return input.action === "use" ? { text: input.text } : null;
  }
  if (operation === "conversations.create") {
    conversations = [
      {
        id: "fixture-chat",
        title: "Dictado · prueba simulada",
        updated_at: "now",
      },
    ];
    emit("alfredpet:conversations");
    return conversations[0];
  }
  if (operation === "conversations.list") return conversations;
  if (operation === "conversations.active") return conversations[0]?.id ?? null;
  if (operation === "conversations.messages") return [];
  if (operation === "settings.get")
    return [{ key: "claude_model", json_value: '"auto"' }];
  if (operation === "explain.pending") return null;
  if (operation === "chat.state") return { running: false };
  if (operation === "pet_drop.list") return { items: [], last_error: null };
  if (operation === "window.show") {
    emit("alfredpet:navigate", { surface: input.surface });
    return {};
  }
  if (operation === "chat.send")
    throw new Error("UNEXPECTED_SEND_IN_DICTATION_PREVIEW");
  return [];
}
export async function on(name, callback) {
  if (!listeners.has(name)) listeners.set(name, new Set());
  listeners.get(name).add(callback);
  return () => listeners.get(name)?.delete(callback);
}
export const show = (surface) => request("window.show", { surface });
window.__DICTATION_FIXTURE__ = { request };
