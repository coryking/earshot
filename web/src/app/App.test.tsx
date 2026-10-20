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

describe("Now", () => {
  it("leads with a plain sentence about what's happening", async () => {
    setup();
    expect(await screen.findByRole("heading", { level: 1, name: "Two channels talking." })).toBeInTheDocument();
    expect(screen.getByText(/than usual for a|About normal for a/)).toBeInTheDocument();
  });

  it("lists sources by plain name, talking first, with a private source explained", async () => {
    setup();
    const list = await screen.findByRole("list", { name: "Channels" });
    const rows = within(list).getAllByRole("listitem");
    expect(rows[0]).toHaveTextContent("Talking");
    expect(within(list).getByText("Private (encrypted)")).toBeInTheDocument();
    expect(within(list).getByText("454.348 MHz")).toBeInTheDocument();
  });

  it("listens with one tap and stops with the player's stop button", async () => {
    const { monitor, user } = setup();
    await user.click(await screen.findByRole("button", { name: /^Listen to Downtown ambassadors/ }));
    expect(monitor).toHaveBeenCalledWith("452685504-DMR-1");
    const player = await screen.findByRole("region", { name: "Player" });
    expect(player).toHaveTextContent("Downtown ambassadors");
    await user.click(within(player).getByRole("button", { name: "Stop" }));
    await waitFor(() => expect(monitor).toHaveBeenLastCalledWith(null));
    expect(screen.queryByRole("region", { name: "Player" })).not.toBeInTheDocument();
  });

  it("opens 'what is this' for a source that can't be played instead of failing", async () => {
    const { monitor, user } = setup();
    await user.click(await screen.findByRole("button", { name: /^About Private business radio/ }));
    expect(await screen.findByText(/only their own radios can understand them/)).toBeInTheDocument();
    expect(monitor).not.toHaveBeenCalled();
  });

  it("follows a source from its sheet and pins it in the list", async () => {
    const { user } = setup();
    await user.click(await screen.findByRole("button", { name: "More about Convention Center radio" }));
    await user.click(await screen.findByRole("button", { name: "Follow" }));
    expect(await screen.findByRole("button", { name: "Following" })).toHaveAttribute("aria-pressed", "true");
    expect(JSON.parse(localStorage.getItem("radio.prefs.v1")!).followed).toEqual(["starter-convention"]);
  });
});

describe("Catch up", () => {
  it("shows recorded calls with play buttons", async () => {
    location.hash = "/catch-up";
    setup();
    expect(await screen.findByRole("heading", { name: "Catch up" })).toBeInTheDocument();
    expect((await screen.findAllByRole("button", { name: /Play call from/ })).length).toBeGreaterThan(3);
  });

  it("says plainly when the engine doesn't record calls yet", async () => {
    location.hash = "/catch-up";
    const backend = new MockBackend({ manual: true });
    vi.spyOn(backend, "calls").mockResolvedValue(null as never);
    setup(backend);
    expect(await screen.findByText(/doesn't record calls yet/)).toBeInTheDocument();
  });
});

describe("Listening area", () => {
  it("explains one block at a time and switches only after a deliberate choice", async () => {
    const { backend, user } = setup();
    // The real switch is exercised in Playwright; jsdom re-renders it too slowly.
    const select = vi.spyOn(backend, "selectBlock").mockResolvedValue();
    await user.click(await screen.findByRole("button", { name: /Listening area: Downtown business radio/ }));
    const sheet = (await screen.findByText(/hears one stretch of the airwaves at a time/)).closest<HTMLElement>("[role=dialog]")!;
    await user.click(within(sheet).getByRole("radio", { name: /Upper business band/ }));
    expect(select).not.toHaveBeenCalled();
    await user.click(within(sheet).getByRole("button", { name: "Switch to Upper business band" }));
    expect(select).toHaveBeenCalledWith("upper-business");
  });
});
