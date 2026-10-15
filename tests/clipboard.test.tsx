import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { ClipboardPanel } from "../src/ui/ClipboardPanel";
const mock = vi.hoisted(() => ({ request: vi.fn(), on: vi.fn() }));
vi.mock("../src/api", () => mock);
vi.mock("../src/ui/ManagedAssets", () => ({
  CaptureAsset: ({ id }: { id: string }) => <div>Private asset {id}</div>,
}));
let status: {
  enabled: boolean;
  module_enabled: boolean;
  privacy: boolean;
  scope_generation: number;
  entries: number;
  maximum_entries: number;
  retention_days: number;
  last_error: null | string;
};
let callbacks: Map<string, () => void>;
const expected = { sequence: 42, fingerprint: "a".repeat(64) };
const observed = {
  expected,
  kind: "text",
  text: "  Fixture 😀  ",
  has_image: false,
  sensitive: false,
  formats: [{ id: 13, name: "Unicode text" }],
};
const entry = {
  id: "own-history-entry",
  text: "<img src=x onerror=alert(1)>",
  updated_at: "2026-10-03T00:00:00Z",
  expires_at: "2026-10-10T00:00:00Z",
};
beforeEach(() => {
  status = {
    enabled: false,
    module_enabled: true,
    privacy: false,
    scope_generation: 1,
    entries: 1,
    maximum_entries: 100,
    retention_days: 7,
    last_error: null,
  };
  callbacks = new Map();
  mock.request.mockReset();
  mock.on.mockReset();
  mock.on.mockImplementation(async (event, callback) => {
    callbacks.set(event, callback);
    return () => callbacks.delete(event);
  });
  mock.request.mockImplementation(async (operation, input) => {
    if (operation === "clipboard.status") return structuredClone(status);
    if (operation === "settings.set") {
      status.enabled = input.value;
      status.scope_generation++;
      return {};
    }
    if (operation === "tools.execute") {
      if (input.tool === "clipboard.get" || input.tool === "clipboard.formats")
        return structuredClone(observed);
      if (input.tool === "clipboard.history") return { items: [entry] };
      if (input.tool === "clipboard.transform")
        return {
          text: "Fixture 😀",
          expected,
          preview_only: !input.input.apply,
        };
      if (input.tool === "clipboard.image") return { id: "own-private-asset" };
      return { written: true };
    }
    return {};
  });
});
async function show() {
  render(<ClipboardPanel />);
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Inspeccionar portapapeles" }),
    ).toBeEnabled(),
  );
}
function calls(tool: string) {
  return mock.request.mock.calls.filter(
    ([operation, input]) =>
      operation === "tools.execute" && input.tool === tool,
  );
}
test("opening settings reads safe status without native reads or history, and optin is an explicit gesture", async () => {
  await show();
  expect(mock.request).toHaveBeenCalledTimes(1);
  expect(mock.request).toHaveBeenCalledWith("clipboard.status");
  expect(
    screen.getByRole("checkbox", { name: "Guardar historial de texto" }),
  ).not.toBeChecked();
  expect(
    screen.getByRole("button", { name: "Mostrar historial" }),
  ).toBeDisabled();
  fireEvent.click(
    screen.getByRole("checkbox", { name: "Guardar historial de texto" }),
  );
  await waitFor(() => expect(screen.getByRole("checkbox")).toBeChecked());
  expect(mock.request).toHaveBeenCalledWith("settings.set", {
    key: "clipboard_history",
    value: true,
  });
  expect(calls("clipboard.get")).toHaveLength(0);
  expect(calls("clipboard.history")).toHaveLength(0);
});
test("history is explicitly shown as inert React text and copy uses a fresh observation through policy", async () => {
  status.enabled = true;
  await show();
  fireEvent.click(screen.getByRole("button", { name: "Mostrar historial" }));
  await screen.findByText(entry.text);
  expect(document.querySelector("img")).toBeNull();
  fireEvent.click(
    screen.getByRole("button", { name: "Copiar esta entrada con revisión" }),
  );
  await waitFor(() => expect(calls("clipboard.set")).toHaveLength(1));
  expect(mock.request).toHaveBeenCalledWith("tools.execute", {
    tool: "clipboard.formats",
    input: {},
  });
  expect(mock.request).toHaveBeenCalledWith("tools.execute", {
    tool: "clipboard.set",
    input: { text: entry.text, expected },
  });
});
test("transformation preview cannot write and application uses the same observed fingerprint", async () => {
  await show();
  fireEvent.click(
    screen.getByRole("button", { name: "Inspeccionar portapapeles" }),
  );
  await screen.findByLabelText("Texto observado");
  fireEvent.click(
    screen.getByRole("button", { name: "Previsualizar transformación" }),
  );
  await screen.findByLabelText("Vista previa de transformación");
  expect(calls("clipboard.set")).toHaveLength(0);
  expect(mock.request).toHaveBeenCalledWith("tools.execute", {
    tool: "clipboard.transform",
    input: { operation: "trim", expected, apply: false },
  });
  fireEvent.click(
    screen.getByRole("button", { name: "Aplicar transformación con revisión" }),
  );
  await screen.findByText("Transformación aplicada y comprobada.");
  expect(mock.request).toHaveBeenCalledWith("tools.execute", {
    tool: "clipboard.transform",
    input: { operation: "trim", expected, apply: true },
  });
  expect(screen.queryByLabelText("Texto observado")).toBeNull();
});
test("a changed clipboard rejects replacement visibly and clears the stale preview without retrying", async () => {
  await show();
  fireEvent.click(
    screen.getByRole("button", { name: "Inspeccionar portapapeles" }),
  );
  await screen.findByLabelText("Texto observado");
  mock.request.mockImplementation(async (operation, input) => {
    if (operation === "tools.execute" && input.tool === "clipboard.set")
      throw new Error("CLIPBOARD_CHANGED_REINSPECT");
    return status;
  });
  fireEvent.change(screen.getByLabelText("Texto de reemplazo"), {
    target: { value: "Explicit replacement" },
  });
  fireEvent.click(
    screen.getByRole("button", { name: "Escribir texto con revisión" }),
  );
  await screen.findByRole("alert");
  expect(screen.getByRole("alert")).toHaveTextContent(
    "CLIPBOARD_CHANGED_REINSPECT",
  );
  expect(screen.queryByLabelText("Texto observado")).toBeNull();
  expect(calls("clipboard.set")).toHaveLength(1);
});
test("privacy removes shown text and discards a history response that finishes after revocation", async () => {
  status.enabled = true;
  await show();
  let resolve: (value: unknown) => void = () => {};
  mock.request.mockImplementation(async (operation, input) => {
    if (operation === "clipboard.status") return structuredClone(status);
    if (operation === "tools.execute" && input.tool === "clipboard.history")
      return new Promise((done) => {
        resolve = done;
      });
    return observed;
  });
  fireEvent.click(screen.getByRole("button", { name: "Mostrar historial" }));
  await waitFor(() => expect(calls("clipboard.history")).toHaveLength(1));
  status.privacy = true;
  status.scope_generation++;
  await act(async () => callbacks.get("alfredpet:settings")?.());
  await screen.findByText(/pausado por privacidad/);
  await act(async () => resolve({ items: [entry] }));
  expect(screen.queryByText(entry.text)).toBeNull();
  expect(
    screen.getByRole("button", { name: "Inspeccionar portapapeles" }),
  ).toBeDisabled();
});
test("disabling history preserves stored entries but clears displayed text and never rereads", async () => {
  status.enabled = true;
  await show();
  fireEvent.click(screen.getByRole("button", { name: "Mostrar historial" }));
  await screen.findByText(entry.text);
  fireEvent.click(screen.getByRole("checkbox"));
  await waitFor(() => expect(screen.getByRole("checkbox")).not.toBeChecked());
  expect(screen.queryByText(entry.text)).toBeNull();
  expect(screen.getByText(/1 entradas guardadas/)).toBeInTheDocument();
  expect(calls("clipboard.history_clear")).toHaveLength(0);
});
test("an image is saved only by its explicit button and displayed using the private asset component", async () => {
  mock.request.mockImplementation(async (operation, input) => {
    if (operation === "clipboard.status") return status;
    if (input.tool === "clipboard.get")
      return { ...observed, kind: "image", has_image: true, text: "" };
    if (input.tool === "clipboard.image") return { id: "own-private-asset" };
    return {};
  });
  await show();
  expect(calls("clipboard.image")).toHaveLength(0);
  fireEvent.click(
    screen.getByRole("button", { name: "Inspeccionar portapapeles" }),
  );
  await screen.findByRole("button", {
    name: "Guardar imagen como activo privado",
  });
  expect(calls("clipboard.image")).toHaveLength(0);
  fireEvent.click(
    screen.getByRole("button", { name: "Guardar imagen como activo privado" }),
  );
  await screen.findByText("Private asset own-private-asset");
  expect(mock.request).toHaveBeenCalledWith("tools.execute", {
    tool: "clipboard.image",
    input: { expected },
  });
});
test("clear uses the mandatory tool and remains available during privacy with an enabled module", async () => {
  status.privacy = true;
  render(<ClipboardPanel />);
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Borrar historial con confirmación" }),
    ).toBeEnabled(),
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Borrar historial con confirmación" }),
  );
  await screen.findByText("Historial borrado.");
  expect(mock.request).toHaveBeenCalledWith("tools.execute", {
    tool: "clipboard.history_clear",
    input: {},
  });
  expect(calls("clipboard.get")).toHaveLength(0);
});
