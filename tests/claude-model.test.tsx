import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { App } from "../src/ui/App";

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
let selected = "sonnet";
let save: (value: string) => Promise<void>;
let start: () => Promise<{ run_id: string }>;
beforeEach(() => {
  selected = "sonnet";
  save = async (value) => {
    selected = value;
  };
  start = async () => ({ run_id: "run" });
  api.request.mockReset();
  api.on.mockResolvedValue(() => {});
  api.request.mockImplementation(
    async (operation: string, input?: { value: string }) => {
      if (operation === "settings.get")
        return [{ key: "claude_model", json_value: JSON.stringify(selected) }];
      if (operation === "settings.set") return save(input!.value);
      if (operation === "conversations.list")
        return [{ id: "conversation", title: "Prueba" }];
      if (operation === "conversations.active") return "conversation";
      if (operation === "pet_drop.list") return { items: [], last_error: null };
      if (operation === "chat.send") return start();
      if (operation === "claude.health")
        return { approved: false, diagnostic: "Sin conexión" };
      return [];
    },
  );
  history.replaceState({}, "", "/?surface=chat");
});

test("model change persists and sending waits until it is saved", async () => {
  let resolveSave!: () => void;
  save = async (value) => {
    await new Promise<void>((resolve) => {
      resolveSave = resolve;
    });
    selected = value;
  };
  const view = render(<App />);
  const picker = screen.getByRole("combobox", { name: "Modelo de Claude" });
  await waitFor(() => expect(picker).toHaveValue("sonnet"));
  fireEvent.change(screen.getByLabelText("Mensaje"), {
    target: { value: "Hola" },
  });
  fireEvent.change(picker, { target: { value: "haiku" } });
  expect(picker).toBeDisabled();
  expect(screen.getByRole("button", { name: "Enviar" })).toBeDisabled();
  fireEvent.keyDown(screen.getByLabelText("Mensaje"), { key: "Enter" });
  expect(api.request).not.toHaveBeenCalledWith("chat.send", expect.anything());
  await act(async () => resolveSave());
  await waitFor(() => expect(picker).toHaveValue("haiku"));
  expect(api.request).toHaveBeenCalledWith("settings.set", {
    key: "claude_model",
    value: "haiku",
  });
  view.unmount();
  render(<App />);
  await waitFor(() =>
    expect(
      screen.getByRole("combobox", { name: "Modelo de Claude" }),
    ).toHaveValue("haiku"),
  );
});

test("failed save keeps the prior selection and exposes the error", async () => {
  save = async () => {
    throw new Error("No se pudo guardar el modelo");
  };
  render(<App />);
  const picker = screen.getByRole("combobox", { name: "Modelo de Claude" });
  await waitFor(() => expect(picker).toHaveValue("sonnet"));
  fireEvent.change(picker, { target: { value: "opus" } });
  await screen.findByText("No se pudo guardar el modelo");
  expect(picker).toHaveValue("sonnet");
  expect(picker).toBeEnabled();
});

test("model is locked while a turn starts and while it is running", async () => {
  let resolveStart!: (value: { run_id: string }) => void;
  start = () =>
    new Promise((resolve) => {
      resolveStart = resolve;
    });
  render(<App />);
  const picker = screen.getByRole("combobox", { name: "Modelo de Claude" });
  await waitFor(() => expect(picker).toHaveValue("sonnet"));
  fireEvent.change(screen.getByLabelText("Mensaje"), {
    target: { value: "Hola" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Enviar" }));
  await waitFor(() =>
    expect(api.request).toHaveBeenCalledWith("chat.send", expect.anything()),
  );
  expect(picker).toBeDisabled();
  await act(async () => resolveStart({ run_id: "run" }));
  await screen.findByRole("button", { name: "Interrumpir" });
  expect(picker).toBeDisabled();
});

test("Claude settings explains relative consumption and saves the same preference", async () => {
  history.replaceState({}, "", "/?surface=settings");
  render(<App />);
  fireEvent.click(screen.getByRole("button", { name: "Claude" }));
  const picker = await screen.findByRole("combobox", {
    name: "Modelo predeterminado de Claude",
  });
  await waitFor(() => expect(picker).toHaveValue("sonnet"));
  fireEvent.change(picker, { target: { value: "haiku" } });
  await waitFor(() => expect(picker).toHaveValue("haiku"));
  expect(
    screen.getByText(/No indica el porcentaje restante/),
  ).toBeInTheDocument();
  expect(
    screen.getByText(/análisis automáticos de memoria/),
  ).toBeInTheDocument();
  expect(
    screen.getByRole("option", { name: "Haiku · menor consumo" }),
  ).toBeInTheDocument();
});
