import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it, vi } from "vitest";
import { engineChannelSchema } from "@/api/engine";
import { Providers } from "@/app/Providers";
import { MockBackend } from "@/backend/mock";
import { BackendError } from "@/backend/types";
import { LiveSelection } from "./Tuning";

it("tunes an entered frequency, explains silence and reports a receive-window error", async () => {
  const channel = engineChannelSchema.parse({ id: "452675000-Unknown-1", frequency_hz: 452675000,
    protocol: "Unknown", in_range: true });
  const backend = Object.assign(new MockBackend({ manual: true }), { tune: vi.fn(async () => channel) });
  const monitor = vi.spyOn(backend, "monitor").mockResolvedValue();
  vi.spyOn(backend, "listen").mockReturnValue({ close() {} });
  render(<Providers backend={backend}><LiveSelection /></Providers>);
  const user = userEvent.setup();
  const input = screen.getByRole("textbox", { name: "Frequency (MHz)" });
  await user.type(input, "452.675");
  await user.click(screen.getByRole("button", { name: "Tune" }));
  await waitFor(() => expect(monitor).toHaveBeenCalledWith(channel.id));
  expect(backend.tune).toHaveBeenCalledWith(452675000, undefined);
  expect(screen.getByRole("status")).toHaveTextContent("Tuned · waiting for audio");
  expect(screen.getByText("452.675 MHz")).toBeVisible();
  expect(screen.getByText("Audio will play when a transmission is decoded.")).toBeVisible();
  await user.click(screen.getByRole("button", { name: "Mute" }));
  expect(screen.getByRole("status")).toHaveTextContent("Muted · unmute to hear audio");
  backend.tune.mockRejectedValueOnce(new BackendError("Frequency is outside the receive window", 422));
  await user.clear(input);
  await user.type(input, "471");
  await user.click(screen.getByRole("button", { name: "Tune" }));
  await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("Frequency is outside the receive window"));
  expect(screen.getByText("471.000 MHz")).toBeVisible();
  await user.click(screen.getByRole("button", { name: "Stop audio" }));
  expect(screen.getByText(/Choose a channel/)).toBeVisible();
});
