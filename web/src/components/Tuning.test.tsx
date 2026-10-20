import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it, vi } from "vitest";
import { engineChannelSchema } from "@/api/engine";
import { Providers } from "@/app/Providers";
import { MockBackend } from "@/backend/mock";
import { BackendError } from "@/backend/types";
import { IqCapture, LiveSelection } from "./Tuning";

it("tunes an entered frequency, explains silence and reports a receive-window error", async () => {
  const channel = engineChannelSchema.parse({ id: "452675000-Unknown-1", frequency_hz: 452675000,
    protocol: "Unknown", in_range: true });
  const backend = Object.assign(new MockBackend({ manual: true }), { tune: vi.fn(async () => channel) });
  const monitor = vi.spyOn(backend, "monitor").mockResolvedValue();
  vi.spyOn(backend, "listen").mockReturnValue({ close() {} });
  render(<Providers backend={backend}><LiveSelection /></Providers>);
  const user = userEvent.setup();
  let input = screen.getByRole("textbox", { name: "Frequency (MHz)" });
  await user.type(input, "452.675");
  await user.click(screen.getByRole("button", { name: "Listen" }));
  await waitFor(() => expect(monitor).toHaveBeenCalledWith(channel.id));
  expect(backend.tune).toHaveBeenCalledWith(452675000, undefined, true);
  expect(screen.getByRole("status")).toHaveTextContent("waiting for receiver samples");
  expect(screen.getByText("452.675 MHz")).toBeVisible();
  expect(screen.getByText("FM signal audio · unsquelched. Sound plays as soon as receiver samples arrive, including noise and digital signals.")).toBeVisible();
  await user.click(screen.getByRole("button", { name: "Mute speaker" }));
  expect(screen.getByRole("status")).toHaveTextContent(/muted/i);
  expect(screen.getByRole("button", { name: "Unmute speaker" })).toHaveAttribute("aria-pressed", "true");
  backend.tune.mockRejectedValueOnce(new BackendError("Frequency is outside the receive window", 422));
  input = screen.getByRole("textbox", { name: "Frequency (MHz)" });
  expect(input).toHaveValue("452.67500");
  await user.clear(input);
  await user.type(input, "471");
  await user.click(screen.getByRole("button", { name: "Listen" }));
  await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("Frequency is outside the receive window"));
  expect(screen.getByText("471.000 MHz")).toBeVisible();
  await user.click(screen.getByRole("button", { name: "Stop listening" }));
  expect(screen.getByText(/Audio will not start automatically/)).toBeVisible();
});

it("does not offer an empty IQ snapshot or invent the sample range before samples arrive", async () => {
  const backend = Object.assign(new MockBackend({ manual: true }), {
    iqCaptureStatus: vi.fn(async () => ({ samples: 0, seconds: 0, sample_rate_hz: null,
      center_hz: null, stream_id: null, max_seconds: 30, last_received_at: null, collecting: false })),
    downloadIq: vi.fn(async () => new Blob()),
  });
  render(<Providers backend={backend}><IqCapture channelId="waiting" frequencyHz={452675000} /></Providers>);
  expect(await screen.findByText("0.0 s buffered / 30 s maximum")).toBeVisible();
  expect(screen.getByRole("button", { name: "Download IQ snapshot" })).toBeDisabled();
  expect(screen.queryByText(/Sampled range/)).not.toBeInTheDocument();
  expect(backend.downloadIq).not.toHaveBeenCalled();
});

it("offers original IQ separately from FM audio and preserves a stopped frequency", async () => {
  const channel = engineChannelSchema.parse({ id: "452675000-Raw-1", frequency_hz: 452675000,
    protocol: "Raw", in_range: true });
  const backend = Object.assign(new MockBackend({ manual: true }), {
    tune: vi.fn(async () => channel),
    iqCaptureStatus: vi.fn(async () => ({ samples: 100000, seconds: 1, sample_rate_hz: 100000,
      center_hz: 452666998, stream_id: 7, max_seconds: 30, last_received_at: Date.now()/1000, collecting: true })),
    downloadIq: vi.fn(async () => new Blob(["archive"], { type: "application/x-tar" })),
  });
  vi.spyOn(backend, "monitor").mockResolvedValue();
  vi.spyOn(backend, "listen").mockReturnValue({ close() {} });
  const originalCreate = URL.createObjectURL;
  const originalRevoke = URL.revokeObjectURL;
  URL.createObjectURL = vi.fn(() => "blob:test");
  URL.revokeObjectURL = vi.fn();
  const click = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});
  try {
    render(<Providers backend={backend}><LiveSelection /></Providers>);
    const user = userEvent.setup();
    expect(screen.getByRole("status")).toHaveTextContent("Listening off");
    await user.type(screen.getByRole("textbox", { name: "Frequency (MHz)" }), "452.675");
    await user.click(screen.getByRole("button", { name: "Listen" }));
    const save = await screen.findByRole("button", { name: "Download IQ snapshot" });
    await waitFor(() => expect(save).toBeEnabled());
    expect(screen.getByText(/Sampled range 452.61700–452.71700 MHz · 100.0 kHz wide/)).toBeVisible();
    expect(screen.getByText("1.0 s buffered / 30 s maximum")).toBeVisible();
    await user.click(save);
    await waitFor(() => expect(backend.downloadIq).toHaveBeenCalledWith(channel.id));
    expect(click).toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Stop listening" }));
    expect(screen.getByRole("status")).toHaveTextContent("Listening off");
    expect(screen.getByRole("textbox", { name: "Frequency (MHz)" })).toHaveValue("452.67500");
    expect(screen.getByRole("button", { name: "Resume listening" })).toBeVisible();
    // Stop is local; this mocked server reports another listener still collecting
    // the same slice. The UI must not falsely call its IQ buffer stopped.
    expect(screen.getByText("Rolling · receiving samples")).toBeVisible();
    backend.iqCaptureStatus.mockResolvedValue({ samples: 100000, seconds: 1, sample_rate_hz: 100000,
      center_hz: 452666998, stream_id: 7, max_seconds: 30, last_received_at: Date.now()/1000, collecting: false });
    await waitFor(() => expect(screen.getByText("Buffer paused · no new samples arriving")).toBeVisible(), { timeout: 2500 });
    expect(save).toBeEnabled();
  } finally {
    click.mockRestore();
    URL.createObjectURL = originalCreate;
    URL.revokeObjectURL = originalRevoke;
  }
});
