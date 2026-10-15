import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { beforeEach, afterEach, describe, expect, test, vi } from "vitest";
import { App } from "../src/ui/App";
const mock = vi.hoisted(() => ({
  request: vi.fn(),
  show: vi.fn(),
  on: vi.fn(),
  startDragging: vi.fn(),
  setPosition: vi.fn(),
}));
vi.mock("../src/api", () => ({ ...mock, desktop: false }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => mock,
  PhysicalPosition: class {},
}));
vi.mock("@tauri-apps/plugin-autostart", () => ({
  enable: vi.fn(),
  disable: vi.fn(),
  isEnabled: vi.fn(),
}));
beforeEach(() => {
  vi.clearAllMocks();
  mock.request.mockResolvedValue([]);
  mock.show.mockResolvedValue({});
  mock.on.mockResolvedValue(() => {});
  mock.startDragging.mockResolvedValue(undefined);
  vi.stubGlobal("PointerEvent", MouseEvent);
  history.replaceState({}, "", "/?surface=pet");
});
afterEach(() => vi.unstubAllGlobals());
describe("pet window interactions", () => {
  test("real MCP tool events select their visual states without opening a window", async () => {
    render(<App />);
    await waitFor(() =>
      expect(mock.on).toHaveBeenCalledWith(
        "alfredpet:agent",
        expect.any(Function),
      ),
    );
    const handler = mock.on.mock.calls.find(
      ([name]) => name === "alfredpet:agent",
    )![1];
    const pet = screen.getByRole("button", { name: "AlfredPet mascota" });
    for (const [tool, state] of [
      ["mcp__alfredpet__browser_search", "searching_web"],
      ["mcp__alfredpet__vision_capture_region", "looking_at_screen"],
      ["mcp__alfredpet__gui_click", "using_pc"],
      ["mcp__alfredpet__files_read", "working"],
      ["mcp__alfredpet__ask_user", "waiting_user"],
    ]) {
      await act(async () => handler({ type: "tool_requested", tool }));
      expect(pet).toHaveAttribute("data-state", state);
    }
    expect(mock.show).not.toHaveBeenCalled();
  });
  test("each ordinary left click toggles chat, including keyboard activation", async () => {
    render(<App />);
    const pet = screen.getByRole("button", { name: "AlfredPet mascota" });
    fireEvent.pointerDown(pet, { button: 0 });
    fireEvent.pointerUp(pet, { button: 0 });
    fireEvent.click(pet, { button: 0, detail: 1 });
    fireEvent.click(pet, { button: 0, detail: 1 });
    fireEvent.click(pet, { button: 0, detail: 0 });
    await waitFor(() =>
      expect(
        mock.request.mock.calls.filter(([op]) => op === "window.toggle"),
      ).toHaveLength(3),
    );
    expect(mock.request).toHaveBeenCalledWith("window.toggle", {
      surface: "chat",
    });
    expect(mock.show).not.toHaveBeenCalled();
  });
  test("right click opens only the native menu", async () => {
    render(<App />);
    const pet = screen.getByRole("button", { name: "AlfredPet mascota" });
    fireEvent.pointerDown(pet, { button: 2 });
    fireEvent.pointerUp(pet, { button: 2 });
    fireEvent.contextMenu(pet, { button: 2 });
    await waitFor(() => expect(mock.request).toHaveBeenCalledWith("pet.menu"));
    expect(mock.request).not.toHaveBeenCalledWith(
      "window.toggle",
      expect.anything(),
    );
    expect(mock.show).not.toHaveBeenCalled();
    expect(mock.startDragging).not.toHaveBeenCalled();
  });
  test("dragging and cancelled pointers do not activate chat", async () => {
    render(<App />);
    const pet = screen.getByRole("button", { name: "AlfredPet mascota" });
    fireEvent.pointerDown(pet, { button: 0, screenX: 20, screenY: 20 });
    await act(async () =>
      fireEvent.pointerMove(pet, { screenX: 40, screenY: 40 }),
    );
    fireEvent.pointerUp(pet, { button: 0 });
    fireEvent.click(pet, { button: 0, detail: 1 });
    expect(mock.request).toHaveBeenCalledWith("pet.drag");
    expect(mock.request).not.toHaveBeenCalledWith(
      "window.toggle",
      expect.anything(),
    );
    fireEvent.pointerDown(pet, { button: 0 });
    fireEvent.pointerCancel(pet);
    fireEvent.click(pet, { button: 0, detail: 1 });
    expect(mock.request).not.toHaveBeenCalledWith(
      "window.toggle",
      expect.anything(),
    );
  });
  test("double click keeps the command palette shortcut without a second chat toggle", async () => {
    render(<App />);
    const pet = screen.getByRole("button", { name: "AlfredPet mascota" });
    fireEvent.click(pet, { button: 0, detail: 1 });
    fireEvent.click(pet, { button: 0, detail: 2 });
    fireEvent.doubleClick(pet);
    await waitFor(() =>
      expect(mock.show).toHaveBeenCalledWith("command_palette"),
    );
    expect(
      mock.request.mock.calls.filter(([op]) => op === "window.toggle"),
    ).toHaveLength(1);
  });
  test("settings does not launch Claude health until its tab is opened", async () => {
    history.replaceState({}, "", "/?surface=settings");
    render(<App />);
    await waitFor(() =>
      expect(mock.request).toHaveBeenCalledWith("settings.get", {}),
    );
    expect(mock.request).not.toHaveBeenCalledWith("claude.health", {});
    fireEvent.click(screen.getByRole("button", { name: "Claude" }));
    await waitFor(() =>
      expect(mock.request).toHaveBeenCalledWith("claude.health", {}),
    );
  });
  test("main navigation reuses the shell and preserves the chat draft", async () => {
    history.replaceState({}, "", "/?surface=chat");
    render(<App />);
    fireEvent.change(screen.getByLabelText("Mensaje"), {
      target: { value: "Borrador conservado" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Configuración" }));
    await screen.findByRole("heading", { name: "Configuración" });
    fireEvent.click(screen.getByRole("button", { name: "Chat" }));
    expect(screen.getByLabelText("Mensaje")).toHaveValue("Borrador conservado");
    expect(mock.show).not.toHaveBeenCalled();
    expect(screen.queryByLabelText("Ruta del adjunto")).toBeNull();
  });
  test("auxiliary windows do not subscribe to main navigation", () => {
    for (const surface of ["pet", "permission", "bubble", "capture_overlay"]) {
      history.replaceState({}, "", `/?surface=${surface}`);
      const view = render(<App />);
      expect(mock.on).not.toHaveBeenCalledWith(
        "alfredpet:navigate",
        expect.anything(),
      );
      view.unmount();
    }
  });
  test("dropping files outside chat cannot navigate the privileged webview", async () => {
    history.replaceState({}, "", "/?surface=settings");
    render(<App />);
    const heading = screen.getByRole("heading", { name: "Configuración" });
    expect(
      fireEvent.drop(heading, {
        dataTransfer: { types: ["Files"], files: [new File(["a"], "a.txt")] },
      }),
    ).toBe(false);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Abre Chat para adjuntar",
    );
    expect(mock.request).not.toHaveBeenCalledWith(
      "attachments.import",
      expect.anything(),
    );
  });
});
