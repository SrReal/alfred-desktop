import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { request } from "../src/api";
import { MediaPanel } from "../src/ui/MediaPanel";
vi.mock("../src/api", () => ({
  request: vi.fn(),
  on: vi.fn(async () => () => {}),
}));
vi.mock("../src/ui/ManagedAssets", () => ({
  CaptureAsset: ({ id }: { id: string }) => <div>Managed preview {id}</div>,
}));
const call = vi.mocked(request);
beforeEach(() => call.mockReset());
describe("Reviewed document/image work", () => {
  it("requires a new destination and sends bounded PDF split parameters through Core", async () => {
    render(<MediaPanel />);
    fireEvent.change(screen.getByLabelText("Operación de medios"), {
      target: { value: "documents.split" },
    });
    call.mockResolvedValueOnce(["D:\\Owned\\source.pdf"]);
    fireEvent.click(screen.getByLabelText("Archivos de medios"));
    await waitFor(() =>
      expect(screen.getByLabelText("Archivos de medios")).toHaveAttribute(
        "title",
        "D:\\Owned\\source.pdf",
      ),
    );
    expect(screen.getByText("Revisar y ejecutar")).toBeDisabled();
    call.mockResolvedValueOnce(["D:\\Owned\\split.pdf"]);
    fireEvent.click(screen.getByLabelText("Destino de medios"));
    await waitFor(() =>
      expect(screen.getByLabelText("Destino de medios")).toHaveAttribute(
        "title",
        "D:\\Owned\\split.pdf",
      ),
    );
    call.mockResolvedValueOnce({
      path: "D:\\Owned\\split.pdf",
      originals_preserved: true,
    });
    fireEvent.click(screen.getByText("Revisar y ejecutar"));
    await waitFor(() =>
      expect(call).toHaveBeenCalledWith("tools.execute", {
        tool: "documents.split",
        input: {
          path: "D:\\Owned\\source.pdf",
          destination: "D:\\Owned\\split.pdf",
          page: 1,
        },
      }),
    );
    expect(call).toHaveBeenCalledTimes(3);
  });
  it("uses explicit review before analysis and opens its returned conversation without a second send", async () => {
    render(<MediaPanel />);
    fireEvent.change(screen.getByLabelText("Operación de medios"), {
      target: { value: "images.analyze" },
    });
    call.mockResolvedValueOnce(["D:\\Owned\\image.png"]);
    fireEvent.click(screen.getByLabelText("Archivos de medios"));
    await waitFor(() =>
      expect(screen.getByLabelText("Archivos de medios")).toHaveAttribute(
        "title",
        "D:\\Owned\\image.png",
      ),
    );
    expect(
      call.mock.calls.every(([operation]) => operation === "paths.pick"),
    ).toBe(true);
    call
      .mockResolvedValueOnce({
        conversation_id: "owned-conversation",
        run_id: "run",
      })
      .mockResolvedValueOnce({});
    fireEvent.click(screen.getByText("Revisar y enviar a Claude"));
    fireEvent.click(await screen.findByText("Abrir análisis en Chat"));
    await waitFor(() =>
      expect(call).toHaveBeenCalledWith("conversations.open", {
        id: "owned-conversation",
      }),
    );
    expect(call).toHaveBeenCalledTimes(3);
  });
  it("shows failure once and rejects options that override reviewed file fields", async () => {
    render(<MediaPanel />);
    call.mockResolvedValueOnce(["D:\\Owned\\document.pdf"]);
    fireEvent.click(screen.getByLabelText("Archivos de medios"));
    await waitFor(() =>
      expect(screen.getByLabelText("Archivos de medios")).toHaveAttribute(
        "title",
        "D:\\Owned\\document.pdf",
      ),
    );
    call.mockRejectedValueOnce(new Error("MEDIA_SCOPE_REVOKED"));
    fireEvent.click(screen.getByText("Revisar y ejecutar"));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "MEDIA_SCOPE_REVOKED",
    );
    expect(call).toHaveBeenCalledTimes(2);
    fireEvent.change(screen.getByLabelText("Opciones de medios"), {
      target: { value: '{"path":"replacement"}' },
    });
    fireEvent.click(screen.getByText("Revisar y ejecutar"));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "campos correspondientes",
    );
    expect(call).toHaveBeenCalledTimes(2);
  });
});
