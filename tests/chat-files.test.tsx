import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, test, vi } from "vitest";
import { ChatFiles } from "../src/ui/ChatFiles";
const request = vi.hoisted(() => vi.fn());
vi.mock("../src/api", () => ({ request }));
const conversation = vi.fn(async () => "conversation-owned");
const onBusy = vi.fn();
const onError = vi.fn();
function setup(busy = false) {
  return render(
    <ChatFiles
      busy={busy}
      conversation={conversation}
      onBusy={onBusy}
      onError={onError}
    >
      <textarea aria-label="Mensaje" />
    </ChatFiles>,
  );
}
beforeEach(() => {
  vi.clearAllMocks();
  request.mockResolvedValue({});
});
describe("chat file gestures", () => {
  test("pasting a screenshot imports bytes without a source path", async () => {
    setup();
    fireEvent.paste(screen.getByLabelText("Mensaje"), {
      clipboardData: {
        files: [
          new File(["owned-image"], "captura.png", { type: "image/png" }),
        ],
      },
    });
    await waitFor(() =>
      expect(request).toHaveBeenCalledWith("attachments.import", {
        conversation_id: "conversation-owned",
        name: "captura.png",
        data_base64: btoa("owned-image"),
      }),
    );
    expect(onBusy.mock.calls).toEqual([[true], [false]]);
  });
  test("ordinary text paste remains available", () => {
    setup();
    expect(
      fireEvent.paste(screen.getByLabelText("Mensaje"), {
        clipboardData: { files: [] },
      }),
    ).toBe(true);
    expect(request).not.toHaveBeenCalled();
  });
  test("drop imports every selected file into the same conversation", async () => {
    setup();
    fireEvent.drop(screen.getByLabelText("Mensaje"), {
      dataTransfer: {
        types: ["Files"],
        files: [new File(["a"], "a.txt"), new File(["b"], "b.pdf")],
      },
    });
    await waitFor(() => expect(request).toHaveBeenCalledTimes(2));
    expect(conversation).toHaveBeenCalledOnce();
    expect(request.mock.calls.map(([, input]) => input.name)).toEqual([
      "a.txt",
      "b.pdf",
    ]);
  });
  test("file selection imports without displaying an absolute path input", async () => {
    setup();
    fireEvent.change(screen.getByLabelText("Seleccionar archivos"), {
      target: { files: [new File(["text"], "notas.md")] },
    });
    await waitFor(() => expect(request).toHaveBeenCalledOnce());
    expect(screen.queryByLabelText("Ruta del adjunto")).toBeNull();
  });
  test("oversized batches and busy chat never import or create a conversation", async () => {
    const view = setup();
    fireEvent.paste(screen.getByLabelText("Mensaje"), {
      clipboardData: {
        files: Array.from({ length: 6 }, () => new File(["a"], "a.txt")),
      },
    });
    expect(onError).toHaveBeenCalledWith(expect.stringContaining("5 archivos"));
    view.unmount();
    setup(true);
    fireEvent.paste(screen.getByLabelText("Mensaje"), {
      clipboardData: { files: [new File(["a"], "a.txt")] },
    });
    expect(onError).toHaveBeenCalledWith(
      expect.stringContaining("operación actual"),
    );
    expect(request).not.toHaveBeenCalled();
    expect(conversation).not.toHaveBeenCalled();
  });
  test("duplicate gestures cannot overlap imports and errors release the busy state", async () => {
    let reject!: (error: Error) => void;
    request.mockImplementation(
      () =>
        new Promise((_, r) => {
          reject = r;
        }),
    );
    setup();
    const clipboardData = { files: [new File(["a"], "a.txt")] };
    fireEvent.paste(screen.getByLabelText("Mensaje"), { clipboardData });
    await waitFor(() => expect(request).toHaveBeenCalledOnce());
    fireEvent.paste(screen.getByLabelText("Mensaje"), { clipboardData });
    expect(conversation).toHaveBeenCalledOnce();
    reject(new Error("Owned fixture failure"));
    await waitFor(() => expect(onBusy).toHaveBeenLastCalledWith(false));
    expect(onError).toHaveBeenCalledWith("Owned fixture failure");
  });
});
