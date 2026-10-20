import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it, vi } from "vitest";
import { Providers } from "@/app/Providers";
import { MockBackend } from "@/backend/mock";
import type { Backend } from "@/backend/types";
import { SpectrumNavigation } from "./SpectrumNavigation";

it("moves by the RF width, accepts a new center, and displays receiver readback", async () => {
  const backend = new MockBackend({ manual: true });
  const snapshot = await backend.status();
  snapshot.center_hz = 460e6;
  snapshot.rate_hz = 25.6e6;
  snapshot.bandwidth_hz = 20e6;
  snapshot.source = { capabilities: { configure_supported: true } };
  vi.spyOn(backend, "status").mockImplementation(async () => ({ ...snapshot }));
  const configure = vi.spyOn(backend as Backend, "configureSource").mockImplementation(async (config) => {
    snapshot.center_hz = config.centerHz;
    return "Confirmed";
  });
  render(<Providers backend={backend}><SpectrumNavigation /></Providers>);
  const user = userEvent.setup();
  const next = screen.getByRole("button", { name: "Next →" });
  await waitFor(() => expect(next).toBeEnabled());
  await user.click(next);
  await waitFor(() => expect(configure).toHaveBeenLastCalledWith({ centerHz: 480e6, rateHz: 25.6e6, bandwidthHz: 20e6 }));
  await waitFor(() => expect(screen.getByRole("textbox", { name: "Center MHz" })).toHaveValue("480.00000"));
  await user.click(screen.getByRole("button", { name: "← Previous" }));
  await waitFor(() => expect(screen.getByRole("textbox", { name: "Center MHz" })).toHaveValue("460.00000"));
  const input = screen.getByRole("textbox", { name: "Center MHz" });
  await user.clear(input);
  await user.type(input, "145");
  await user.click(screen.getByRole("button", { name: "Go" }));
  await waitFor(() => expect(configure).toHaveBeenLastCalledWith({ centerHz: 145e6, rateHz: 25.6e6, bandwidthHz: 20e6 }));
  await waitFor(() => expect(input).toHaveValue("145.00000"));
});

it("reports a rejected move and keeps the actual window", async () => {
  const backend = new MockBackend({ manual: true });
  const snapshot = await backend.status();
  snapshot.source = { capabilities: { configure_supported: true } };
  vi.spyOn(backend, "status").mockResolvedValue(snapshot);
  vi.spyOn(backend, "configureSource").mockRejectedValue(new Error("RX LO readback failed"));
  render(<Providers backend={backend}><SpectrumNavigation /></Providers>);
  await waitFor(() => expect(screen.getByRole("button", { name: "Next →" })).toBeEnabled());
  await userEvent.click(screen.getByRole("button", { name: "Next →" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("RX LO readback failed");
  expect(screen.getByRole("textbox", { name: "Center MHz" })).toHaveValue((snapshot.center_hz! / 1e6).toFixed(5));
});

it("disables controls when the producer cannot retune", async () => {
  const backend = new MockBackend({ manual: true });
  const snapshot = await backend.status();
  snapshot.source = { capabilities: { configure_supported: false } };
  vi.spyOn(backend, "status").mockResolvedValue(snapshot);
  render(<Providers backend={backend}><SpectrumNavigation /></Providers>);
  await waitFor(() => expect(screen.getByRole("button", { name: "Go" })).toBeDisabled());
  expect(screen.getByRole("button", { name: "Next →" })).toBeDisabled();
});
