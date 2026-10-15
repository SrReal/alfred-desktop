import {
  act,
  render,
  screen,
  fireEvent,
  waitFor,
} from "@testing-library/react";
import { vi, describe, test, expect, beforeEach } from "vitest";
import { App } from "../src/ui/App";
import {
  gravityFrame,
  audioAllowed,
  AssetRegistry,
  petStates,
} from "../src/assets";
const mock = vi.hoisted(() => ({
  request: vi.fn(),
  show: vi.fn(),
  on: vi.fn(),
}));
vi.mock("../src/api", () => ({ ...mock, desktop: false }));
vi.mock("@tauri-apps/plugin-autostart", () => ({
  enable: vi.fn(),
  disable: vi.fn(),
  isEnabled: vi.fn().mockResolvedValue(false),
}));
beforeEach(() => {
  mock.request.mockReset();
  mock.on.mockResolvedValue(() => {});
  mock.request.mockImplementation(async (op: string) =>
    op === "pet_drop.list"
      ? { items: [], last_error: null }
      : op === "claude.health"
        ? {
            approved: false,
            version: "fixture",
            diagnostic: "Subscription blocked",
          }
        : [],
  );
  history.replaceState({}, "", "/?surface=chat");
});
describe("AlfredPet UI", () => {
  test("audio selection saves the chosen WAV and displays its name without a path field", async () => {
    history.replaceState({}, "", "/?surface=settings");
    let audio: string | null = "C:\\Audios\\anterior.wav";
    mock.request.mockImplementation(
      async (op: string, input?: { value: string | null }) => {
        if (op === "settings.get")
          return [
            { key: "attention_audio", json_value: JSON.stringify(audio) },
          ];
        if (op === "settings.pick_audio") return "C:\\Mis audios\\señor.wav";
        if (op === "settings.set") audio = input!.value;
        return [];
      },
    );
    render(<App />);
    await screen.findByText("anterior.wav");
    expect(
      screen.queryByPlaceholderText("Ruta absoluta de un archivo WAV"),
    ).not.toBeInTheDocument();
    fireEvent.click(
      screen.getByRole("button", { name: "Seleccionar archivo de audio" }),
    );
    await screen.findByText("señor.wav");
    expect(mock.request).toHaveBeenCalledWith("settings.set", {
      key: "attention_audio",
      value: "C:\\Mis audios\\señor.wav",
    });
    fireEvent.click(screen.getByRole("button", { name: "Quitar audio" }));
    await screen.findByText("Ningún archivo seleccionado");
    expect(audio).toBeNull();
  });
  test.each([null, new Error("No se pudo abrir el selector")])(
    "cancelling or failing the audio picker preserves the saved audio (%s)",
    async (result) => {
      history.replaceState({}, "", "/?surface=settings");
      mock.request.mockImplementation(async (op: string) => {
        if (op === "settings.get")
          return [
            {
              key: "attention_audio",
              json_value: JSON.stringify("C:\\Audios\\anterior.wav"),
            },
          ];
        if (op === "settings.pick_audio") {
          if (result instanceof Error) throw result;
          return result;
        }
        return [];
      });
      render(<App />);
      await screen.findByText("anterior.wav");
      fireEvent.click(
        screen.getByRole("button", { name: "Seleccionar archivo de audio" }),
      );
      await waitFor(() =>
        expect(
          screen.getByRole("button", { name: "Seleccionar archivo de audio" }),
        ).toBeEnabled(),
      );
      expect(screen.getByText("anterior.wav")).toBeInTheDocument();
      expect(
        mock.request.mock.calls.some(([op]) => op === "settings.set"),
      ).toBe(false);
      if (result instanceof Error)
        expect(screen.getByText(result.message)).toBeInTheDocument();
    },
  );
  test("memory filters origin, opens source and exports or rebuilds its index", async () => {
    history.replaceState({}, "", "/?surface=settings");
    mock.request.mockImplementation(async (operation: string) =>
      operation === "memory.list"
        ? [
            {
              id: "inferred",
              title: "Inferido fixture",
              content: "Decisión inferida",
              type: "decision",
              explicit: 0,
              pinned: 0,
              disabled: 0,
              confidence: 0.7,
              use_count: 3,
              source_conversation_id: "origin-1",
            },
            {
              id: "explicit",
              title: "Explícito fixture",
              content: "Preferencia confirmada",
              type: "preference",
              explicit: 1,
              pinned: 0,
              disabled: 0,
            },
          ]
        : operation === "memory.export"
          ? { path: "C:/fixtures/memories.json" }
          : [],
    );
    render(<App />);
    fireEvent.click(screen.getByRole("button", { name: "Memoria" }));
    await screen.findByText("Inferido fixture");
    expect(screen.getByText(/Confianza: 70 % · Usos: 3/)).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Filtrar origen de memoria"), {
      target: { value: "inferred" },
    });
    expect(screen.queryByText("Explícito fixture")).not.toBeInTheDocument();
    fireEvent.click(
      screen.getByRole("button", { name: "Ver conversación de origen" }),
    );
    await waitFor(() =>
      expect(mock.request).toHaveBeenCalledWith("conversations.open", {
        id: "origin-1",
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Exportar memoria" }));
    expect(
      await screen.findByText("Memoria exportada: C:/fixtures/memories.json"),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Reconstruir índice" }));
    await waitFor(() =>
      expect(mock.request).toHaveBeenCalledWith("memory.rebuild_index"),
    );
  });
  test("organizing requires a reviewed preview and applying sends its saved plan", async () => {
    history.replaceState({}, "", "/?surface=task_center");
    mock.request.mockImplementation(
      async (
        op: string,
        input: { tool?: string; input?: { body?: { plan_id?: string } } },
      ) =>
        op === "tools.execute" && input.tool === "files.organize"
          ? {
              plan_id: "preview-1",
              applied: !!input.input?.body?.plan_id,
              moves: [
                {
                  source: "C:/fixtures/a.txt",
                  destination: "C:/fixtures/Documents/a.txt",
                },
              ],
            }
          : [],
    );
    render(<App />);
    expect(
      screen.queryByRole("button", { name: "Aplicar movimientos" }),
    ).not.toBeInTheDocument();
    mock.request.mockResolvedValueOnce(["C:/fixtures"]);
    fireEvent.click(screen.getByLabelText("Carpeta que organizar"));
    await waitFor(() =>
      expect(screen.getByLabelText("Carpeta que organizar")).toHaveAttribute(
        "title",
        "C:/fixtures",
      ),
    );
    fireEvent.change(screen.getByLabelText("Extensiones que organizar"), {
      target: { value: "txt, md" },
    });
    fireEvent.change(screen.getByLabelText("Subcarpeta de destino"), {
      target: { value: "Documents" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Vista previa" }));
    const apply = await screen.findByRole("button", {
      name: "Aplicar movimientos",
    });
    expect(
      screen.getByText("Destino: C:/fixtures/Documents/a.txt"),
    ).toBeInTheDocument();
    expect(mock.request).toHaveBeenCalledWith("tools.execute", {
      tool: "files.organize",
      input: {
        path: "C:/fixtures",
        body: { rules: [{ extensions: ["txt", "md"], folder: "Documents" }] },
      },
    });
    fireEvent.click(apply);
    await waitFor(() =>
      expect(mock.request).toHaveBeenCalledWith("tools.execute", {
        tool: "files.organize",
        input: { path: "C:/fixtures", body: { plan_id: "preview-1" } },
      }),
    );
    expect(
      await screen.findByText("Movimientos aplicados: 1"),
    ).toBeInTheDocument();
  });
  test("download watches are created through policy and paused from the task center", async () => {
    history.replaceState({}, "", "/?surface=task_center");
    const watch = {
      id: "watch-1",
      owner_module: "downloads",
      kind: "download",
      enabled: 1,
      effective_enabled: true,
      status: "active",
      config: {
        path: "C:/fixtures/downloads",
        condition: "ready",
        recursive: false,
      },
    };
    mock.request.mockImplementation(async (op: string) =>
      op === "watches.list" ? [watch] : op === "tools.execute" ? [] : {},
    );
    render(<App />);
    fireEvent.change(screen.getByLabelText("Tipo de vigilancia"), {
      target: { value: "download" },
    });
    mock.request.mockResolvedValueOnce(["C:/fixtures/downloads"]);
    fireEvent.click(screen.getByLabelText("Destino de vigilancia"));
    await waitFor(() =>
      expect(screen.getByLabelText("Destino de vigilancia")).toHaveAttribute(
        "title",
        "C:/fixtures/downloads",
      ),
    );
    fireEvent.click(screen.getByRole("button", { name: "Crear vigilancia" }));
    await waitFor(() =>
      expect(mock.request).toHaveBeenCalledWith("tools.execute", {
        tool: "downloads.watch",
        input: {
          path: "C:/fixtures/downloads",
          kind: "download",
          recursive: false,
        },
      }),
    );
    fireEvent.click(await screen.findByRole("button", { name: "Pausar" }));
    await waitFor(() =>
      expect(mock.request).toHaveBeenCalledWith("tools.execute", {
        tool: "downloads.watch_update",
        input: { id: "watch-1", body: { enabled: false } },
      }),
    );
    expect(
      screen.getByText(/nunca se ejecutan automáticamente/),
    ).toBeInTheDocument();
  });
  test("watch errors remain visible and deleting uses the mandatory backend action", async () => {
    history.replaceState({}, "", "/?surface=task_center");
    mock.request.mockImplementation(async (op: string) =>
      op === "watches.list"
        ? [
            {
              id: "blocked",
              owner_module: "files",
              kind: "folder",
              enabled: 1,
              effective_enabled: true,
              status: "error",
              error_code: "PATH_SCOPE",
              config: {
                path: "C:/fixtures/revoked",
                condition: "changed",
                recursive: false,
              },
            },
          ]
        : op === "tools.execute"
          ? []
          : {},
    );
    render(<App />);
    expect(await screen.findByText("PATH_SCOPE")).toBeInTheDocument();
    fireEvent.click(
      screen.getByRole("button", { name: "Eliminar vigilancia" }),
    );
    await waitFor(() =>
      expect(mock.request).toHaveBeenCalledWith("tools.execute", {
        tool: "files.unwatch",
        input: { id: "blocked" },
      }),
    );
  });
  test("undo uses the policy checked tool and hides consumed checkpoints", async () => {
    history.replaceState({}, "", "/?surface=activity");
    mock.request.mockImplementation(async (op: string) =>
      op === "activity.list"
        ? [
            {
              id: "a",
              timestamp: "2026-10-02T09:00:00Z",
              tool_id: "files.write",
              risk: "Contextual",
              status: "succeeded",
              duration_ms: 1,
              input_summary: "{}",
              output_summary: "{}",
              undo_token: "checkpoint",
              undo_state: "available",
            },
            {
              id: "b",
              timestamp: "2026-10-02T09:00:00Z",
              tool_id: "files.copy",
              risk: "Contextual",
              status: "succeeded",
              duration_ms: 1,
              input_summary: "{}",
              output_summary: "{}",
              undo_token: "old",
              undo_state: "undone",
            },
          ]
        : {},
    );
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Deshacer" }));
    await waitFor(() =>
      expect(mock.request).toHaveBeenCalledWith("tools.execute", {
        tool: "files.undo",
        input: { id: "checkpoint" },
      }),
    );
    expect(screen.getByText("Deshecho")).toBeInTheDocument();
  });
  test("renders chat and prevents empty send", () => {
    render(<App />);
    expect(screen.getByRole("button", { name: "Enviar" })).toBeDisabled();
    expect(
      screen.getByText(/AlfredPet está listo para conversar/),
    ).toBeInTheDocument();
  });
  test("chat receives valid native events, streams and reloads the completed turn", async () => {
    const events = new Map<string, (value: unknown) => void>();
    mock.on.mockClear();
    mock.on.mockImplementation(
      async (name: string, callback: (value: unknown) => void) => {
        expect(name).toMatch(/^[\p{L}\p{N}_:/-]+$/u);
        events.set(name, callback);
        return () => events.delete(name);
      },
    );
    let complete = false;
    mock.request.mockImplementation(async (operation: string) => {
      if (operation === "conversations.active") return "fixture-chat";
      if (operation === "conversations.list")
        return [{ id: "fixture-chat", title: "Fixture" }];
      if (operation === "conversations.messages")
        return complete
          ? [
              {
                id: "fixture-response",
                role: "assistant",
                content_text: "Fixture respuesta completa",
                status: "complete",
                created_at: "2026-10-05T00:00:00Z",
              },
            ]
          : [];
      if (operation === "chat.send") return { run_id: "fixture-run" };
      return [];
    });
    render(<App />);
    await waitFor(() =>
      expect(screen.getByLabelText("Conversación")).toHaveValue("fixture-chat"),
    );
    await waitFor(() => expect(events.has("alfredpet:agent")).toBe(true));
    fireEvent.change(screen.getByLabelText("Mensaje"), {
      target: { value: "Fixture pregunta" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Enviar" }));
    await screen.findByRole("button", { name: "Interrumpir" });
    await act(async () =>
      events.get("alfredpet:agent")?.({
        type: "text_delta",
        conversation_id: "fixture-chat",
        run_id: "fixture-run",
        text: "Fixture parcial",
      }),
    );
    expect(screen.getByText("Fixture parcial")).toBeInTheDocument();
    complete = true;
    await act(async () =>
      events.get("alfredpet:agent")?.({
        type: "completed",
        conversation_id: "fixture-chat",
        run_id: "fixture-run",
      }),
    );
    await screen.findByText("Fixture respuesta completa");
    expect(screen.queryByText("Fixture parcial")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Enviar" })).toBeInTheDocument();
  });
  test("activity routes a window checkpoint to window undo", async () => {
    history.replaceState({}, "", "/?surface=activity");
    mock.request.mockImplementation(async (op: string) =>
      op === "activity.list"
        ? [
            {
              id: "w",
              timestamp: "2026-10-03T09:00:00Z",
              tool_id: "window.move",
              risk: "Contextual",
              status: "succeeded",
              duration_ms: 1,
              input_summary: "{}",
              output_summary: "{}",
              undo_token: "window-checkpoint",
              undo_state: "available",
            },
          ]
        : {},
    );
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Deshacer" }));
    await waitFor(() =>
      expect(mock.request).toHaveBeenCalledWith("tools.execute", {
        tool: "window.undo",
        input: { id: "window-checkpoint" },
      }),
    );
  });
  test("eligible exact L3 action can be remembered only by explicit choice", async () => {
    history.replaceState({}, "", "/?surface=permission");
    mock.request.mockImplementation(async (op: string) =>
      op === "permissions.list"
        ? [
            {
              id: "p",
              tool_id: "terminal.command",
              risk: "Mandatory",
              rememberable: true,
              input_json_redacted: '{"command":"Get-Service"}',
            },
          ]
        : [],
    );
    render(<App />);
    const button = await screen.findByRole("button", {
      name: "Permitir siempre esta acción exacta",
    });
    expect(mock.request).not.toHaveBeenCalledWith(
      "permissions.resolve",
      expect.anything(),
    );
    fireEvent.click(button);
    await waitFor(() =>
      expect(mock.request).toHaveBeenCalledWith("permissions.resolve", {
        id: "p",
        decision: "always",
      }),
    );
    expect(
      screen.queryByRole("button", {
        name: "Permitir este alcance en la sesión",
      }),
    ).not.toBeInTheDocument();
  });
  test("L3 permission never offers session allow", async () => {
    history.replaceState({}, "", "/?surface=permission");
    mock.request.mockImplementation(async (op: string) =>
      op === "permissions.list"
        ? [
            {
              id: "p",
              tool_id: "files.delete",
              risk: "Mandatory",
              input_json_redacted: '{"path":"fixture"}',
            },
          ]
        : [],
    );
    render(<App />);
    await screen.findByText("files.delete");
    expect(
      screen.getByText("L3 · confirmación obligatoria"),
    ).toBeInTheDocument();
    expect(
      screen.queryByText("Permitir este alcance en la sesión"),
    ).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Denegar" }));
    await waitFor(() =>
      expect(mock.request).toHaveBeenCalledWith("permissions.resolve", {
        id: "p",
        decision: "deny",
      }),
    );
  });
  test("module toggles go to backend", async () => {
    history.replaceState({}, "", "/?surface=settings");
    mock.request.mockImplementation(async (op: string) =>
      op === "modules.list"
        ? [
            {
              id: "browser",
              name: "Navegador",
              kind: "native",
              enabled: false,
              version: "0.1.0",
              tools: 10,
              live: "LIVE_NOT_RUN",
            },
          ]
        : op === "claude.health"
          ? {}
          : [],
    );
    render(<App />);
    fireEvent.click(screen.getByText("Módulos"));
    fireEvent.click(await screen.findByRole("checkbox", { name: "Navegador" }));
    await waitFor(() =>
      expect(mock.request).toHaveBeenCalledWith("modules.toggle", {
        id: "browser",
        enabled: true,
      }),
    );
  });
  test("memory correction links original", async () => {
    history.replaceState({}, "", "/?surface=settings");
    mock.request.mockImplementation(async (op: string) =>
      op === "memory.list"
        ? [
            {
              id: "m",
              title: "Proyecto",
              content: "Versión anterior",
              type: "fact",
              explicit: 1,
              pinned: 0,
              disabled: 0,
              source_conversation_id: null,
            },
          ]
        : [],
    );
    render(<App />);
    fireEvent.click(screen.getByText("Memoria"));
    fireEvent.click(await screen.findByRole("button", { name: "Corregir" }));
    fireEvent.change(screen.getByLabelText("Nueva memoria"), {
      target: { value: "Versión actual" },
    });
    fireEvent.click(screen.getByText("Guardar corrección"));
    await waitFor(() =>
      expect(mock.request).toHaveBeenCalledWith(
        "memory.save",
        expect.objectContaining({ supersedes: "m", content: "Versión actual" }),
      ),
    );
  });
  test("error remains visible when send fails", async () => {
    mock.request.mockImplementation(async (op: string) => {
      if (op === "pet_drop.list") return { items: [], last_error: null };
      if (op === "conversations.create") return { id: "c" };
      if (op === "chat.send") throw new Error("CLAUDE_SUBSCRIPTION_REQUIRED");
      return [];
    });
    render(<App />);
    fireEvent.change(screen.getByLabelText("Mensaje"), {
      target: { value: "Hola" },
    });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Enviar" })).toBeEnabled(),
    );
    fireEvent.click(screen.getByRole("button", { name: "Enviar" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "CLAUDE_SUBSCRIPTION_REQUIRED",
    );
  });
});
describe("physics/assets/audio", () => {
  test("fall monotonically reaches exact target and snaps above work area", () => {
    let previous = 20;
    for (let t = 0; t <= 1500; t += 16) {
      const y = gravityFrame(20, 800, t);
      expect(y).toBeGreaterThanOrEqual(previous);
      expect(y).toBeLessThanOrEqual(800);
      previous = y;
    }
    expect(gravityFrame(900, 800, 1)).toBe(800);
  });
  test("approved PixelLab states keep their logical canvas, baseline and bounded hit regions", () => {
    expect(AssetRegistry.pixellabEnabled).toBe(true);
    expect(AssetRegistry.animatedStates).toEqual(petStates);
    for (const state of petStates) {
      const asset = AssetRegistry.pet[state];
      expect(asset.state).toBe(state);
      expect([asset.width, asset.height, asset.baseline]).toEqual([
        144, 192, 188,
      ]);
      expect(asset.frames).toBeGreaterThan(1);
      expect(asset.durations).toHaveLength(asset.frames);
      expect(asset.durations.every((ms) => ms >= 100)).toBe(true);
      expect(asset.hitRegion.x).toBeGreaterThanOrEqual(0);
      expect(asset.hitRegion.y).toBeGreaterThanOrEqual(0);
      expect(asset.hitRegion.x + asset.hitRegion.width).toBeLessThanOrEqual(
        asset.width,
      );
      expect(asset.hitRegion.y + asset.hitRegion.height).toBeLessThanOrEqual(
        asset.height,
      );
    }
  });
  test("normal chat never plays attention sound; DND retains blocking events", () => {
    expect(audioAllowed("simple_chat_response", false)).toBe(false);
    expect(audioAllowed("completed_requires_attention", true)).toBe(false);
    expect(audioAllowed("waiting_permission", true)).toBe(true);
  });
});
