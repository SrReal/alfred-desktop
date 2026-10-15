import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { ChatTranscript } from "../src/ui/ChatTranscript";
import { identity } from "../src/identity";
import type { Message } from "../src/types";

let height: number;
let top: number;
let resize: () => void;
const disconnect = vi.fn();
const messages: Message[] = [
  {
    id: "user",
    role: "user",
    content_text: "Hola, Alfred",
    status: "complete",
    created_at: "2026-10-11T12:00:00Z",
  },
];
const initial = {
  conversationId: "conversation",
  messages,
  stream: "",
  pendingText: "",
};
beforeEach(() => {
  height = 900;
  top = 0;
  disconnect.mockClear();
  vi.spyOn(HTMLElement.prototype, "scrollHeight", "get").mockImplementation(
    () => height,
  );
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(300);
  vi.spyOn(HTMLElement.prototype, "scrollTop", "get").mockImplementation(
    () => top,
  );
  vi.spyOn(HTMLElement.prototype, "scrollTop", "set").mockImplementation(
    (value) => {
      top = Math.max(0, Math.min(value, height - 300));
    },
  );
  vi.stubGlobal(
    "ResizeObserver",
    class {
      constructor(callback: () => void) {
        resize = callback;
      }
      observe() {}
      disconnect = disconnect;
    },
  );
});
afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

test("follows history, incoming chunks and the final persisted reply", () => {
  const view = render(<ChatTranscript {...initial} />);
  expect(top).toBe(600);
  height = 1100;
  view.rerender(<ChatTranscript {...initial} stream="Respuesta parcial" />);
  expect(top).toBe(800);
  height = 1400;
  view.rerender(
    <ChatTranscript {...initial} stream="Respuesta parcial más larga" />,
  );
  expect(top).toBe(1100);
  height = 1600;
  view.rerender(
    <ChatTranscript
      {...initial}
      messages={[
        ...messages,
        { ...messages[0], id: "reply", role: "assistant", content_text: "Fin" },
      ]}
    />,
  );
  expect(top).toBe(1300);
  expect(screen.getByText("Fin")).toBeInTheDocument();
});

test("history browsing survives unrelated renders until new content arrives", () => {
  const view = render(<ChatTranscript {...initial} />);
  const transcript = view.container.querySelector(".transcript")!;
  top = 120;
  fireEvent.scroll(transcript);
  view.rerender(<ChatTranscript {...initial} />);
  act(() => resize());
  expect(top).toBe(120);
  view.rerender(<ChatTranscript {...initial} pendingText="Pensando…" />);
  expect(top).toBe(600);
  height = 180;
  view.rerender(
    <ChatTranscript {...initial} conversationId="new" messages={[]} />,
  );
  expect(top).toBe(0);
});

test("keeps the bottom on reflow while following and cleans up its observer", () => {
  const view = render(<ChatTranscript {...initial} />);
  height = 1300;
  act(() => resize());
  expect(top).toBe(1000);
  view.unmount();
  expect(disconnect).toHaveBeenCalledOnce();
});

test("uses the edition's accessible avatars, including streaming replies", () => {
  const view = render(<ChatTranscript {...initial} stream="Sí, señor" />);
  expect(
    screen.getByRole("img", { name: identity.userLabel }),
  ).toBeInTheDocument();
  expect(screen.getByRole("img", { name: "Alfred" })).toBeInTheDocument();
  expect(view.container.querySelector(".meta")).not.toHaveTextContent("Tú");
  expect(
    view.container.querySelector(".message.assistant .meta"),
  ).not.toHaveTextContent("AlfredPet");
});

test("separates assistant responses, preserves dates and shows activity even after text", () => {
  const first = {
    ...messages[0],
    id: "first",
    role: "assistant",
    content_text: "Voy a comprobarlo.",
  };
  const second = {
    ...first,
    id: "second",
    content_text: "Hecho, señor.",
    status: "streaming",
    created_at: "2026-10-11T12:00:05Z",
  };
  const view = render(
    <ChatTranscript
      {...initial}
      messages={[...messages, first]}
      replies={[first, second]}
      pendingText="Alfred está trabajando"
    />,
  );
  expect(view.container.querySelectorAll(".message.assistant")).toHaveLength(2);
  expect(screen.getAllByRole("img", { name: "Alfred" })).toHaveLength(2);
  expect(
    view.container.querySelectorAll(".message.assistant time"),
  ).toHaveLength(2);
  expect(screen.getByText(first.content_text)).toBeInTheDocument();
  expect(screen.getByRole("status")).toHaveTextContent(
    "Alfred está trabajando",
  );
  expect(view.container.querySelectorAll(".working-dots i")).toHaveLength(3);
  view.rerender(
    <ChatTranscript
      {...initial}
      messages={[...messages, first, second]}
      replies={[first, second]}
    />,
  );
  expect(view.container.querySelectorAll(".message.assistant")).toHaveLength(2);
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
});
