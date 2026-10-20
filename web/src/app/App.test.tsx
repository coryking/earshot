import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { MockBackend } from "@/backend/mock";
import { App } from "./App";
import { Providers } from "./Providers";

function setup(backend = new MockBackend({ manual: true })) {
  const monitor = vi.spyOn(backend, "monitor");
  render(
    <Providers backend={backend}>
      <App />
    </Providers>,
  );
  return { backend, monitor, user: userEvent.setup() };
}

const wait = { timeout: 12_000 };

describe("Explore", () => {
  it("opens with the spectrum and one tuning strip, with channels folded away", async () => {
    setup();
    expect(await screen.findByRole("heading", { level: 1, name: /Downtown business radio/ })).toBeVisible();
    expect(screen.getByRole("region", { name: "Live tuning" })).toBeVisible();
    expect(document.querySelector('[data-waterfall="full"]')).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Channels" })).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByRole("list", { name: "Recent activity" })).not.toBeInTheDocument();
    expect(screen.queryByText(/Only the held channel/)).not.toBeInTheDocument();
  });

  it("searches a frequency list and tunes with one tap, then closes the list", async () => {
    const { user, monitor } = setup();
    await user.click(screen.getByRole("button", { name: "Channels" }));
    await user.type(screen.getByRole("textbox", { name: "Search channels" }), "Downtown ambassadors");
    const row = await screen.findByRole("button", { name: /^Tune to Downtown ambassadors/ }, wait);
    await user.click(row);
    await waitFor(() => expect(monitor).toHaveBeenCalledWith("452685504-DMR-1"));
    expect(screen.queryByRole("textbox", { name: "Search channels" })).not.toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "Mute speaker" })).toHaveLength(1);
    expect(screen.queryByRole("region", { name: "Player" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Signal" }));
    expect(screen.getByRole("complementary", { name: "Signal details" })).toBeVisible();
    expect(screen.getByText("Frame decoded")).toBeVisible();
    expect(screen.getByText("Speech")).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Stop listening" }));
    await waitFor(() => expect(monitor).toHaveBeenLastCalledWith(null));
    expect(screen.getByRole("textbox", { name: "Frequency (MHz)" })).toHaveValue("452.68550");
  });
});

describe("Catch up", { timeout: 40_000 }, () => {
  it("shows recorded calls with play buttons", async () => {
    location.hash = "/catch-up";
    setup();
    expect(await screen.findByRole("heading", { name: "Catch up" })).toBeInTheDocument();
    expect((await screen.findAllByRole("button", { name: /Play call from/ }, wait)).length).toBeGreaterThan(3);
  });

  it("falls back to calls heard this session without narrating it", async () => {
    location.hash = "/catch-up";
    const backend = new MockBackend({ manual: true });
    vi.spyOn(backend, "calls").mockResolvedValue(null as never);
    setup(backend);
    expect(await screen.findByRole("heading", { name: "Catch up" })).toBeInTheDocument();
    expect(screen.queryByText(/record/i)).not.toBeInTheDocument();
  });
});

describe("Listening area", { timeout: 40_000 }, () => {
  it("switches only after a deliberate choice", async () => {
    const { backend, user } = setup();
    // The real switch is exercised in Playwright; jsdom re-renders it too slowly.
    const select = vi.spyOn(backend, "selectBlock").mockResolvedValue();
    await user.click(await screen.findByRole("button", { name: /Listening area: Downtown business radio/ }, wait));
    const sheet = await screen.findByRole("dialog");
    await user.click(within(sheet).getByRole("radio", { name: /Upper business band/ }));
    expect(select).not.toHaveBeenCalled();
    await user.click(within(sheet).getByRole("button", { name: "Switch to Upper business band" }));
    expect(select).toHaveBeenCalledWith("upper-business");
  });
});
