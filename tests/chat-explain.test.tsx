import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { ChatExplain } from "../src/ui/ChatExplain";
const api = vi.hoisted(() => ({ request: vi.fn(), on: vi.fn() }));
vi.mock("../src/api", () => api);
vi.mock("../src/ui/ManagedAssets", () => ({
  ChatAssetImage: ({ id }: { id: string }) => <span>Imagen {id}</span>,
}));

test("returning from another section reloads pending context without reading while hidden", async () => {
  const props = {
    busy: false,
    conversation: vi.fn(),
    onBusy: vi.fn(),
    onDraft: vi.fn(),
  };
  const ui = render(<ChatExplain {...props} active={false} />);
  expect(api.request).not.toHaveBeenCalled();
  ui.rerender(<ChatExplain {...props} active={true} />);
  await screen.findByText("Texto seleccionado");
  ui.rerender(<ChatExplain {...props} active={false} />);
  expect(screen.queryByText("Texto seleccionado")).not.toBeInTheDocument();
  preview = { id: "after-settings", text: "Selección nueva" };
  ui.rerender(<ChatExplain {...props} active={true} />);
  await screen.findByText("Selección nueva");
});
let refresh: () => void;
let preview: {
  id: string;
  text?: string;
  asset_id?: string;
  notice?: string;
} | null;
beforeEach(() => {
  preview = { id: "preview", text: "Texto seleccionado" };
  api.on.mockImplementation(async (_event: string, cb: () => void) => {
    refresh = cb;
    return () => {};
  });
  api.request.mockReset();
  api.request.mockImplementation(async (op: string) => {
    if (op === "explain.pending") return preview;
    if (op === "explain.prepare")
      return { text: "Alfred, explícame: Texto seleccionado" };
    return {};
  });
});
test("previews without sending, then prepares in the current chat on explicit click", async () => {
  const draft = vi.fn();
  const conversation = vi.fn(async () => "chosen");
  render(
    <ChatExplain
      busy={false}
      conversation={conversation}
      onBusy={vi.fn()}
      onDraft={draft}
    />,
  );
  await screen.findByText("Texto seleccionado");
  expect(conversation).not.toHaveBeenCalled();
  expect(draft).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Usar en este chat" }));
  await waitFor(() =>
    expect(draft).toHaveBeenCalledWith("Alfred, explícame: Texto seleccionado"),
  );
  expect(api.request).toHaveBeenCalledWith("explain.prepare", {
    id: "preview",
    conversation_id: "chosen",
  });
  expect(api.request).not.toHaveBeenCalledWith("chat.send", expect.anything());
});
test("busy chat disables preparing; failed preparation keeps preview and draft untouched", async () => {
  const props = {
    conversation: vi.fn(async () => "chosen"),
    onBusy: vi.fn(),
    onDraft: vi.fn(),
  };
  const ui = render(<ChatExplain busy={true} {...props} />);
  expect(
    await screen.findByRole("button", { name: "Usar en este chat" }),
  ).toBeDisabled();
  ui.rerender(<ChatExplain busy={false} {...props} />);
  api.request.mockImplementation(async (op: string) => {
    if (op === "explain.prepare") throw new Error("CONVERSATION_BUSY");
    return preview;
  });
  fireEvent.click(screen.getByRole("button", { name: "Usar en este chat" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "CONVERSATION_BUSY",
  );
  expect(screen.getByText("Texto seleccionado")).toBeInTheDocument();
  expect(props.onDraft).not.toHaveBeenCalled();
});
test("discard never creates a conversation and privacy event clears displayed text", async () => {
  const conversation = vi.fn();
  render(
    <ChatExplain
      busy={false}
      conversation={conversation}
      onBusy={vi.fn()}
      onDraft={vi.fn()}
    />,
  );
  await screen.findByText("Texto seleccionado");
  preview = null;
  await act(async () => refresh());
  expect(screen.queryByText("Texto seleccionado")).not.toBeInTheDocument();
  preview = { id: "second", asset_id: "asset" };
  await act(async () => refresh());
  await screen.findByText("Imagen asset");
  fireEvent.click(screen.getByRole("button", { name: "Descartar selección" }));
  await waitFor(() =>
    expect(api.request).toHaveBeenCalledWith("explain.prepare", {
      id: "second",
      dismiss: true,
    }),
  );
  expect(conversation).not.toHaveBeenCalled();
});
test("missing selection offers region fallback and clears old notice before capture", async () => {
  preview = { id: "empty", notice: "EXPLAIN_NO_SELECTION" };
  render(
    <ChatExplain
      busy={false}
      conversation={vi.fn()}
      onBusy={vi.fn()}
      onDraft={vi.fn()}
    />,
  );
  expect(await screen.findByRole("status")).toHaveTextContent(
    "No se pudo leer",
  );
  expect(
    screen.queryByRole("button", { name: "Usar en este chat" }),
  ).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Seleccionar región" }));
  await waitFor(() =>
    expect(api.request).toHaveBeenCalledWith("explain.region"),
  );
  expect(api.request).toHaveBeenCalledWith("explain.prepare", {
    id: "empty",
    dismiss: true,
  });
});
