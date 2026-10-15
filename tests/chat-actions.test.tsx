import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { App } from "../src/ui/App";
import type { AgentEvent } from "../src/types";
const api = vi.hoisted(() => ({
  request: vi.fn(),
  show: vi.fn(),
  on: vi.fn(),
}));
vi.mock("../src/api", () => ({ ...api, desktop: false }));
vi.mock("@tauri-apps/plugin-autostart", () => ({
  enable: vi.fn(),
  disable: vi.fn(),
  isEnabled: vi.fn(),
}));
let events: Record<string, (e: unknown) => void>;
let rows: { id: string; title: string; updated_at: string }[];
beforeEach(() => {
  events = {};
  rows = [{ id: "conversation", title: "Prueba", updated_at: "v1" }];
  api.on.mockImplementation(
    async (name: string, callback: (e: unknown) => void) => {
      events[name] = callback;
      return () => {};
    },
  );
  api.request.mockReset();
  api.request.mockImplementation(async (operation: string) => {
    if (operation === "dictation.state") return null;
    if (operation === "settings.get")
      return [{ key: "claude_model", json_value: '"auto"' }];
    if (operation === "conversations.list") return [...rows];
    if (operation === "conversations.active") return "conversation";
    if (operation === "chat.state") return { running: false };
    if (operation === "chat.send") return { run_id: "run" };
    if (operation === "conversations.delete") {
      rows = [];
      return { deleted: "conversation" };
    }
    if (operation === "pet_drop.list") return { items: [], last_error: null };
    return [];
  });
  history.replaceState({}, "", "/?surface=chat");
});
async function ready() {
  render(<App />);
  await waitFor(() =>
    expect(screen.getByRole("combobox", { name: "Conversación" })).toHaveValue(
      "conversation",
    ),
  );
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Borrar chat" })).toBeEnabled(),
  );
}
test("deleting requires explicit confirmation and binds the selected conversation", async () => {
  await ready();
  fireEvent.click(screen.getByRole("button", { name: "Borrar chat" }));
  expect(screen.getByRole("alertdialog")).toHaveTextContent("Prueba");
  expect(api.request).not.toHaveBeenCalledWith(
    "conversations.delete",
    expect.anything(),
  );
  fireEvent.click(screen.getByRole("button", { name: "Cancelar" }));
  expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Borrar chat" }));
  fireEvent.click(
    screen.getByRole("button", { name: "Borrar definitivamente" }),
  );
  await waitFor(() =>
    expect(api.request).toHaveBeenCalledWith("conversations.delete", {
      id: "conversation",
      updated_at: "v1",
      confirmed: true,
    }),
  );
  await waitFor(() =>
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument(),
  );
  expect(screen.getByRole("button", { name: "Borrar chat" })).toBeDisabled();
});
test("progress remains visible across responses and tools; completion unlocks deletion", async () => {
  await ready();
  fireEvent.change(screen.getByLabelText("Mensaje"), {
    target: { value: "Hola" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Enviar" }));
  await screen.findByRole("button", { name: "Interrumpir" });
  expect(screen.getByRole("button", { name: "Borrar chat" })).toBeDisabled();
  const emit = async (event: Partial<AgentEvent>) =>
    act(async () =>
      events["alfredpet:agent"]({
        conversation_id: "conversation",
        run_id: "run",
        ...event,
      }),
    );
  const message = (id: string, text: string) => ({
    id,
    role: "assistant",
    content_text: text,
    created_at: "2026-10-12T10:00:00Z",
    status: "streaming",
  });
  await emit({ type: "response", message: message("a", "Primero") });
  await emit({ type: "tool_requested", tool: "skills.list" });
  expect(screen.getByText("Alfred está trabajando")).toBeInTheDocument();
  await emit({ type: "response", message: message("b", "Después") });
  expect(screen.getByText("Primero")).toBeInTheDocument();
  expect(screen.getByText("Después")).toBeInTheDocument();
  expect(screen.getByText("Alfred está trabajando")).toBeInTheDocument();
  await emit({ type: "completed" });
  expect(screen.queryByText("Alfred está trabajando")).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Borrar chat" })).toBeEnabled();
});
test("skills and plugins suggestions prepare an editable request without sending", async () => {
  await ready();
  fireEvent.click(screen.getByRole("button", { name: "Skills y plugins" }));
  fireEvent.click(screen.getByRole("button", { name: "Nueva skill" }));
  expect(screen.getByLabelText("Mensaje")).toHaveValue(
    "Quiero crear una skill reutilizable para: ",
  );
  expect(api.request).not.toHaveBeenCalledWith("chat.send", expect.anything());
});

test("dictation review appends to the existing draft and Enter cannot send during capture", async () => {
  const original = api.request.getMockImplementation()!;
  let state: {
    id: string;
    conversation_id: string;
    status: string;
    text: string;
    error: string;
  } | null = null;
  api.request.mockImplementation(
    async (op: string, input?: Record<string, string>) => {
      if (op === "dictation.state") return state;
      if (op === "dictation.start") {
        state = {
          id: "d",
          conversation_id: "conversation",
          status: "listening",
          text: "texto dictado",
          error: "",
        };
        return state;
      }
      if (op === "dictation.action") {
        if (input?.action === "stop") {
          state = { ...state!, status: "review" };
          events["alfredpet:dictation"]({});
          return state;
        }
        state = null;
        events["alfredpet:dictation"]({});
        return { text: input?.text };
      }
      return original(op, input);
    },
  );
  await ready();
  fireEvent.change(screen.getByLabelText("Mensaje"), {
    target: { value: "Borrador previo" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Dictar" }));
  await screen.findByText("Escuchando · máximo 2 minutos");
  expect(screen.getByRole("button", { name: "Enviar" })).toBeDisabled();
  fireEvent.keyDown(screen.getByLabelText("Mensaje"), { key: "Enter" });
  expect(api.request).not.toHaveBeenCalledWith("chat.send", expect.anything());
  fireEvent.click(screen.getByRole("button", { name: "Detener y revisar" }));
  fireEvent.change(await screen.findByLabelText("Revisar dictado"), {
    target: { value: "Texto corregido" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Añadir al mensaje" }));
  await waitFor(() =>
    expect(screen.getByLabelText("Mensaje")).toHaveValue(
      "Borrador previo\n\nTexto corregido",
    ),
  );
  expect(api.request).not.toHaveBeenCalledWith("chat.send", expect.anything());
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Enviar" })).toBeEnabled(),
  );
  fireEvent.click(screen.getByRole("button", { name: "Enviar" }));
  await waitFor(() =>
    expect(api.request).toHaveBeenCalledWith("chat.send", {
      conversation_id: "conversation",
      text: "Borrador previo\n\nTexto corregido",
    }),
  );
});

const revisionMessages = [
  {
    id: "u",
    role: "user",
    content_text: "Original",
    status: "complete",
    created_at: "2026-10-13T10:00:00Z",
  },
  {
    id: "a",
    role: "assistant",
    content_text: "Respuesta",
    status: "complete",
    created_at: "2026-10-13T10:00:01Z",
  },
];
function revisionApi(fail = false) {
  const original = api.request.getMockImplementation()!;
  api.request.mockImplementation(
    async (op: string, input?: Record<string, string>) => {
      if (op === "conversations.messages")
        return input?.id === "conversation" ? revisionMessages : [];
      if (op === "chat.draft") return null;
      if (op === "conversations.branch") {
        rows.push({ id: "variant", title: "Variante", updated_at: "v2" });
        return {
          id: "variant",
          text: input?.mode === "edit" ? input.text : "Original",
        };
      }
      if (op === "chat.send" && fail) throw new Error("Conexión no disponible");
      return original(op, input);
    },
  );
}
test("editing is reviewed, preserves the original and sends only to the new variant", async () => {
  revisionApi();
  await ready();
  fireEvent.click(
    await screen.findByRole("button", { name: "Editar y reenviar" }),
  );
  expect(api.request).not.toHaveBeenCalledWith(
    "conversations.branch",
    expect.anything(),
  );
  fireEvent.change(screen.getByLabelText("Editar mensaje"), {
    target: { value: "Corregido" },
  });
  fireEvent.keyDown(screen.getByLabelText("Editar mensaje"), { key: "Enter" });
  expect(api.request).not.toHaveBeenCalledWith("chat.send", expect.anything());
  fireEvent.click(
    screen.getByRole("button", { name: "Crear variante y enviar" }),
  );
  await waitFor(() =>
    expect(api.request).toHaveBeenCalledWith("conversations.branch", {
      conversation_id: "conversation",
      message_id: "u",
      expected_text: "Original",
      mode: "edit",
      text: "Corregido",
    }),
  );
  await waitFor(() =>
    expect(api.request).toHaveBeenCalledWith("chat.send", {
      conversation_id: "variant",
      text: "Corregido",
    }),
  );
  expect(api.request).not.toHaveBeenCalledWith(
    "conversations.delete",
    expect.anything(),
  );
  expect(screen.getByLabelText("Mensaje")).toHaveValue("");
});
test("regeneration can be cancelled and a failed send keeps a retryable draft in the variant", async () => {
  revisionApi(true);
  await ready();
  fireEvent.click(await screen.findByRole("button", { name: "Regenerar" }));
  fireEvent.click(screen.getByRole("button", { name: "Cancelar variante" }));
  expect(api.request).not.toHaveBeenCalledWith(
    "conversations.branch",
    expect.anything(),
  );
  fireEvent.click(screen.getByRole("button", { name: "Regenerar" }));
  fireEvent.click(
    screen.getByRole("button", { name: "Crear variante y enviar" }),
  );
  await waitFor(() =>
    expect(screen.getByLabelText("Mensaje")).toHaveValue("Original"),
  );
  await screen.findByText("Conexión no disponible");
  expect(screen.getByLabelText("Conversación")).toHaveValue("variant");
  fireEvent.click(screen.getByRole("button", { name: "Enviar" }));
  await waitFor(() =>
    expect(
      api.request.mock.calls.filter(([op]) => op === "chat.send"),
    ).toHaveLength(2),
  );
  expect(
    api.request.mock.calls.filter(([op]) => op === "conversations.branch"),
  ).toHaveLength(1);
});
test("an existing draft is protected and message copying preserves Markdown", async () => {
  revisionApi();
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText },
  });
  await ready();
  fireEvent.change(screen.getByLabelText("Mensaje"), {
    target: { value: "Mi borrador" },
  });
  expect(
    await screen.findByRole("button", { name: "Editar y reenviar" }),
  ).toBeDisabled();
  expect(screen.getByRole("button", { name: "Regenerar" })).toBeDisabled();
  fireEvent.click(screen.getAllByRole("button", { name: "Copiar mensaje" })[0]);
  await waitFor(() => expect(writeText).toHaveBeenCalledWith("Original"));
  expect(screen.getByLabelText("Mensaje")).toHaveValue("Mi borrador");
});

test("a late saved draft cannot reappear after a successful send", async () => {
  const original = api.request.getMockImplementation()!;
  let restore!: (value: string) => void;
  api.request.mockImplementation(async (op: string, input: unknown) => {
    if (op === "chat.draft")
      return new Promise<string>((resolve) => {
        restore = resolve;
      });
    return original(op, input);
  });
  await ready();
  fireEvent.change(screen.getByLabelText("Mensaje"), {
    target: { value: "Nuevo texto" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Enviar" }));
  await waitFor(() => expect(screen.getByLabelText("Mensaje")).toHaveValue(""));
  await act(async () => restore("Borrador viejo"));
  expect(screen.getByLabelText("Mensaje")).toHaveValue("");
});
