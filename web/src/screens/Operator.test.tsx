import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it, vi } from "vitest";
import { engineChannelSchema } from "@/api/engine";
import { Providers } from "@/app/Providers";
import { MockBackend } from "@/backend/mock";
import { OperatorScreen } from "./Operator";

it("keeps repeated Listen armed, disables unsupported window changes, and stops its player before RX", async () => {
  const channel = engineChannelSchema.parse({ id: "454018750-Unknown-1", frequency_hz: 454018750,
    protocol: "Unknown", slot: 1, in_range: true });
  const backend = Object.assign(new MockBackend({ manual: true }), { tune: vi.fn(async () => channel) });
  const snapshot = await backend.status();
  vi.spyOn(backend, "status").mockResolvedValue({ ...snapshot,
    source: { ...snapshot.source, capabilities: { configure_supported: false } } });
  const monitor = vi.spyOn(backend, "monitor").mockResolvedValue();
  vi.spyOn(backend, "listen").mockReturnValue({ close() {} });
  const stop = vi.spyOn(backend, "stopReceiver").mockResolvedValue("Receive session stopped.");
  render(<Providers backend={backend}><OperatorScreen /></Providers>);
  const user = userEvent.setup();
  await waitFor(() => expect(screen.getByRole("textbox", { name: "Center MHz" })).toBeDisabled());
  expect(screen.getByRole("button", { name: /Apply/ })).toBeDisabled();
  expect(screen.getByText(/cannot change the receive window from the browser/)).toBeVisible();
  await user.type(screen.getByRole("textbox", { name: "Listen at MHz" }), "454.01875");
  await user.click(screen.getByRole("button", { name: "Listen" }));
  await waitFor(() => expect(monitor).toHaveBeenCalledWith(channel.id));
  await waitFor(() => expect(screen.getByRole("button", { name: "Listen" })).toBeEnabled());
  await user.click(screen.getByRole("button", { name: "Listen" }));
  await waitFor(() => expect(backend.tune).toHaveBeenCalledTimes(2));
  expect(monitor).not.toHaveBeenCalledWith(null);
  await user.click(screen.getByRole("button", { name: "Stop receiver…" }));
  expect(await screen.findByText(/A supervisor may start receiving again automatically/)).toBeVisible();
  await user.click(screen.getByRole("button", { name: "Stop receiver" }));
  await waitFor(() => expect(stop).toHaveBeenCalledOnce());
  expect(monitor).toHaveBeenLastCalledWith(null);
  expect(monitor.mock.invocationCallOrder.at(-1)).toBeLessThan(stop.mock.invocationCallOrder[0]);
});
