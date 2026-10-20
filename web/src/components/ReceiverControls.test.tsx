import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it, vi } from "vitest";
import { Providers } from "@/app/Providers";
import { MockBackend } from "@/backend/mock";
import { ReceiverControls } from "./ReceiverControls";

it("stops receiving, retains the stopped state, and resumes from the same visible control", async () => {
  const backend = new MockBackend({ manual: true });
  const stop = vi.spyOn(backend, "stopReceiver");
  const start = vi.spyOn(backend, "startReceiver");
  render(<Providers backend={backend}><ReceiverControls details /></Providers>);
  const user = userEvent.setup();
  await waitFor(() => expect(screen.getByRole("button", { name: "Stop receiving" })).toBeEnabled(), { timeout: 5000 });
  await user.click(screen.getByRole("button", { name: "Stop receiving" }));
  await waitFor(() => expect(screen.getByRole("button", { name: "Start receiving" })).toBeEnabled());
  expect(stop).toHaveBeenCalledOnce();
  expect((await backend.status()).state).toBe("stopped");
  await user.click(screen.getByRole("button", { name: "Start receiving" }));
  await waitFor(() => expect(screen.getByRole("button", { name: "Stop receiving" })).toBeEnabled());
  expect(start).toHaveBeenCalledOnce();
});

it("keeps the button pending until status confirms the operation, and reports a rejected request", async () => {
  const backend = new MockBackend({ manual: true });
  let release!: () => void;
  vi.spyOn(backend, "stopReceiver").mockImplementation(() => new Promise<string>((resolve) => { release = () => resolve("requested"); }));
  render(<Providers backend={backend}><ReceiverControls /></Providers>);
  const user = userEvent.setup();
  await waitFor(() => expect(screen.getByRole("button", { name: "Stop receiving" })).toBeEnabled());
  await user.click(screen.getByRole("button", { name: "Stop receiving" }));
  expect(screen.getByRole("button", { name: "Stopping receiver…" })).toBeDisabled();
  release();
  // The command was accepted but status still reports receiving: it must remain pending.
  await waitFor(() => expect(screen.getByRole("button", { name: "Stopping receiver…" })).toBeDisabled());
  const actualStatus = backend.status.bind(backend);
  vi.spyOn(backend, "status").mockImplementation(async () => ({ ...(await actualStatus()), state: "stopped",
    receiver: { ...(await actualStatus()).receiver!, state: "stopped", reason: "operator", receiving_requested: false } }));
  await waitFor(() => expect(screen.getByRole("button", { name: "Start receiving" })).toBeEnabled());
  vi.spyOn(backend, "startReceiver").mockRejectedValue(new Error("Receiver box is unavailable"));
  await user.click(screen.getByRole("button", { name: "Start receiving" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Receiver box is unavailable");
  expect(screen.getByRole("button", { name: "Start receiving" })).toBeEnabled();
});

it("does not offer operative controls for an old or stale supervisor", async () => {
  const backend = new MockBackend({ manual: true });
  const snapshot = await backend.status();
  vi.spyOn(backend, "status").mockResolvedValue({ ...snapshot, receiver: { ...snapshot.receiver!, fresh: false } });
  render(<Providers backend={backend}><ReceiverControls details /></Providers>);
  await waitFor(() => expect(screen.getByRole("button", { name: "Stop receiving" })).toBeDisabled());
  expect(screen.getByText(/require a connected supervisor/)).toBeVisible();
});

it("confirms fresh engine samples even when the browser clock is ahead", async () => {
  let engineTime = 10_000;
  const backend = new MockBackend({ manual: true, now: () => engineTime });
  await backend.stopReceiver();
  const start = backend.startReceiver.bind(backend);
  vi.spyOn(backend, "startReceiver").mockImplementation(async () => {
    engineTime += 1_000;
    return start();
  });
  render(<Providers backend={backend}><ReceiverControls /></Providers>);
  const user = userEvent.setup();
  await waitFor(() => expect(screen.getByRole("button", { name: "Start receiving" })).toBeEnabled(), { timeout: 5000 });
  await user.click(screen.getByRole("button", { name: "Start receiving" }));
  await waitFor(() => expect(screen.getByRole("button", { name: "Stop receiving" })).toBeEnabled());
  expect(engineTime / 1000).toBeLessThan(Date.now() / 1000 - 120);
});
