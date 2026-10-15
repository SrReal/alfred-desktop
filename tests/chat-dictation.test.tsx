import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import { ChatDictation } from "../src/ui/ChatDictation";
const api = vi.hoisted(() => ({ request: vi.fn(), on: vi.fn() }));
vi.mock("../src/api", () => api);
type Session = {
  id: string;
  conversation_id: string;
  status: string;
  text: string;
  error: string;
};
let session: Session | null;
let refresh: () => void;
const make = (status = "listening"): Session => ({
  id: "one",
  conversation_id: "chat",
  status,
  text: "Hola alfred",
  error: "",
});
beforeEach(() => {
  session = null;
  api.request.mockReset();
  api.on.mockImplementation(async (_name: string, cb: () => void) => {
    refresh = cb;
    return () => {};
  });
  api.request.mockImplementation(
    async (op: string, input: Record<string, string>) => {
      if (op === "dictation.state") return session;
      if (op === "dictation.start") {
        session = make();
        return session;
      }
      if (op === "dictation.action") {
        if (input.action === "stop") {
          session = make("review");
          refresh();
          return session;
        }
        session = null;
        refresh();
        return input.action === "use" ? { text: input.text } : null;
      }
      throw Error(op);
    },
  );
});
function props() {
  return {
    active: true,
    conversationId: "chat",
    busy: false,
    onBusy: vi.fn(),
    onDraft: vi.fn(),
  };
}
test("explicit start, stop, edit and add; no message is sent and partial text is never a draft", async () => {
  const p = props();
  render(<ChatDictation {...p} />);
  await waitFor(() =>
    expect(api.request).toHaveBeenCalledWith("dictation.state"),
  );
  expect(api.request).not.toHaveBeenCalledWith(
    "dictation.start",
    expect.anything(),
  );
  fireEvent.click(screen.getByRole("button", { name: "Dictar" }));
  await screen.findByText("Escuchando · máximo 2 minutos");
  expect(p.onDraft).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Detener y revisar" }));
  const review = await screen.findByLabelText("Revisar dictado");
  fireEvent.change(review, {
    target: { value: "Hola, Alfred. Texto corregido." },
  });
  fireEvent.click(screen.getByRole("button", { name: "Añadir al mensaje" }));
  await waitFor(() =>
    expect(p.onDraft).toHaveBeenCalledWith("Hola, Alfred. Texto corregido."),
  );
  expect(
    api.request.mock.calls.every(([op]) => String(op).startsWith("dictation.")),
  ).toBe(true);
});
test("discard and privacy-cleared state remove preview without changing draft", async () => {
  const p = props();
  session = make("review");
  render(<ChatDictation {...p} />);
  await screen.findByLabelText("Revisar dictado");
  fireEvent.click(screen.getByRole("button", { name: "Descartar dictado" }));
  await waitFor(() =>
    expect(screen.queryByLabelText("Revisar dictado")).not.toBeInTheDocument(),
  );
  session = make();
  await act(async () => refresh());
  await screen.findByText("Escuchando · máximo 2 minutos");
  session = null;
  await act(async () => refresh());
  expect(
    screen.queryByText("Escuchando · máximo 2 minutos"),
  ).not.toBeInTheDocument();
  expect(p.onDraft).not.toHaveBeenCalled();
});
test("leaving chat or changing conversation discards and does not carry text", async () => {
  const p = props();
  session = make("review");
  const ui = render(<ChatDictation {...p} />);
  await screen.findByLabelText("Revisar dictado");
  ui.rerender(<ChatDictation {...p} active={false} />);
  await waitFor(() =>
    expect(api.request).toHaveBeenCalledWith("dictation.action", {
      id: "one",
      action: "discard",
    }),
  );
  expect(p.onDraft).not.toHaveBeenCalled();
  expect(screen.queryByLabelText("Revisar dictado")).not.toBeInTheDocument();
  session = make("review");
  ui.rerender(<ChatDictation {...p} conversationId="different" />);
  await waitFor(() => expect(session).toBeNull());
  expect(p.onDraft).not.toHaveBeenCalled();
});
test("late start after navigation is cancelled and never appears in another chat", async () => {
  let resolve!: (s: Session) => void;
  api.request.mockImplementation(async (op: string) => {
    if (op === "dictation.start")
      return new Promise<Session>((r) => {
        resolve = r;
      });
    return null;
  });
  const p = props();
  const ui = render(<ChatDictation {...p} />);
  fireEvent.click(screen.getByRole("button", { name: "Dictar" }));
  ui.rerender(<ChatDictation {...p} conversationId="other" />);
  await act(async () => resolve(make()));
  expect(api.request).toHaveBeenCalledWith("dictation.action", {
    id: "one",
    action: "discard",
  });
  expect(p.onDraft).not.toHaveBeenCalled();
});
test("missing recognizer and microphone errors explain recovery; busy cannot start", async () => {
  const p = props();
  const ui = render(<ChatDictation {...p} busy />);
  expect(screen.getByRole("button", { name: "Dictar" })).toBeDisabled();
  session = { ...make("error"), error: "DICTATION_MICROPHONE_UNAVAILABLE" };
  ui.rerender(<ChatDictation {...p} />);
  await act(async () => refresh());
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "permiso de micrófono",
  );
  session = { ...make("error"), error: "DICTATION_SPANISH_REQUIRED" };
  await act(async () => refresh());
  expect(screen.getByRole("alert")).toHaveTextContent(
    "Falta un reconocedor local de español",
  );
});
