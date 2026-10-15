import type { AgentEvent, PetState } from "./types";

export const petStateLabels: Record<PetState, string> = {
  idle: "En espera",
  hover: "Atento",
  dragging: "Moviendo a Alfred",
  falling: "Volviendo al escritorio",
  thinking: "Pensando",
  working: "Trabajando",
  searching_web: "Buscando en la web",
  looking_at_screen: "Mirando la pantalla",
  using_pc: "Usando el PC",
  waiting_permission: "Esperando tu permiso",
  waiting_user: "Esperando tu respuesta",
  success: "Completado",
  error: "Ha ocurrido un error",
  privacy: "Privacidad activada",
  dnd: "No molestar",
  sleeping: "En reposo",
};

export function agentPetState(event: AgentEvent): PetState {
  if (event.type === "completed") return "success";
  if (event.type === "error") return "error";
  if (event.type === "text_delta" || event.type === "response")
    return "thinking";
  if (event.type !== "tool_requested") return "working";
  // Core publishes MCP names as module_action; Claude prefixes its server name.
  const tool = (event.tool ?? "").replace(/^mcp__alfredpet__/, "");
  if (tool === "ask_user") return "waiting_user";
  if (tool === "request_permission") return "waiting_permission";
  if (
    [
      "browser_search",
      "browser_research",
      "browser_fetch",
      "browser_extract",
    ].includes(tool)
  )
    return "searching_web";
  if (
    tool.startsWith("vision_capture_") ||
    ["vision_ocr", "gui_screenshot_target", "browser_screenshot"].includes(tool)
  )
    return "looking_at_screen";
  if (
    tool.startsWith("gui_") ||
    tool.startsWith("window_") ||
    tool.startsWith("browser_")
  )
    return "using_pc";
  return "working";
}
