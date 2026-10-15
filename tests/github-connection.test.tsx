import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { App } from "../src/ui/App";
const mock = vi.hoisted(() => ({
  request: vi.fn(),
  on: vi.fn(),
  show: vi.fn(),
}));
vi.mock("../src/api", () => ({ ...mock, desktop: false }));
vi.mock("@tauri-apps/plugin-autostart", () => ({
  enable: vi.fn(),
  disable: vi.fn(),
  isEnabled: vi.fn().mockResolvedValue(false),
}));
beforeEach(() => {
  history.replaceState({}, "", "/?surface=settings");
  mock.request.mockReset();
  mock.on.mockResolvedValue(() => {});
  mock.request.mockImplementation(async (op: string) =>
    op === "connections.save"
      ? { id: "11111111-1111-4111-8111-111111111111" }
      : [],
  );
});
async function connections() {
  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Conexiones" }));
  await screen.findByLabelText("Tipo de conexión");
}
test("GitHub connection requires an explicit repository template and clears PAT when switching provider", async () => {
  await connections();
  fireEvent.change(screen.getByLabelText("Tipo de conexión"), {
    target: { value: "git" },
  });
  expect(screen.getByLabelText("Configuración de conexión")).toHaveValue(
    JSON.stringify({ auth: "prefer_gh", repos: ["owner/repository"] }, null, 2),
  );
  expect(
    screen.getByText(/GitHub: indica los repositorios permitidos/),
  ).toBeInTheDocument();
  fireEvent.change(screen.getByLabelText("Nombre de conexión"), {
    target: { value: "Owned GitHub" },
  });
  fireEvent.change(screen.getByLabelText("Credencial de conexión"), {
    target: { value: "owned_fixture_pat" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Guardar conexión" }));
  await waitFor(() =>
    expect(mock.request).toHaveBeenCalledWith("connections.save", {
      id: null,
      module_id: "git",
      name: "Owned GitHub",
      config: { auth: "prefer_gh", repos: ["owner/repository"] },
      secret: "owned_fixture_pat",
    }),
  );
  await waitFor(() =>
    expect(screen.getByLabelText("Credencial de conexión")).toHaveValue(""),
  );
  fireEvent.change(screen.getByLabelText("Credencial de conexión"), {
    target: { value: "another_owned_fixture" },
  });
  fireEvent.change(screen.getByLabelText("Tipo de conexión"), {
    target: { value: "notion" },
  });
  expect(screen.getByLabelText("Credencial de conexión")).toHaveValue("");
});
test("a stale GitHub save cannot assign its connection id to a newly selected provider", async () => {
  let resolveSave!: (value: { id: string }) => void;
  let once = true;
  mock.request.mockImplementation(async (op: string) => {
    if (op === "connections.save" && once) {
      once = false;
      return new Promise<{ id: string }>((resolve) => {
        resolveSave = resolve;
      });
    }
    return op === "connections.save"
      ? { id: "22222222-2222-4222-8222-222222222222" }
      : [];
  });
  await connections();
  fireEvent.change(screen.getByLabelText("Tipo de conexión"), {
    target: { value: "git" },
  });
  fireEvent.change(screen.getByLabelText("Nombre de conexión"), {
    target: { value: "Owned" },
  });
  const saveButton = screen.getByRole("button", { name: "Guardar conexión" });
  fireEvent.click(saveButton);
  await waitFor(() => expect(saveButton).toBeDisabled());
  fireEvent.change(screen.getByLabelText("Tipo de conexión"), {
    target: { value: "notion" },
  });
  resolveSave({ id: "11111111-1111-4111-8111-111111111111" });
  await waitFor(() => {
    expect(saveButton).toBeInTheDocument();
    expect(saveButton).toBeEnabled();
  });
  fireEvent.click(saveButton);
  await waitFor(() =>
    expect(
      mock.request.mock.calls.filter((call) => call[0] === "connections.save"),
    ).toHaveLength(2),
  );
  const saves = mock.request.mock.calls.filter(
    (call) => call[0] === "connections.save",
  );
  expect(saves[1][1]).toMatchObject({
    id: null,
    module_id: "notion",
    secret: "",
  });
});
