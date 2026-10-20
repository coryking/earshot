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

// The home view samples the engine once a second. Poll with plain timers: under
// waitFor the first full render of the list stalls for many seconds in jsdom.
const wait = { timeout: 12_000 };
async function rowsReady() {
  for (let i = 0; i < 80; i++) {
    if (document.querySelectorAll("[data-source-list] [data-source-row]").length > 3) return;
    await new Promise((r) => setTimeout(r, 250));
  }
  throw new Error("the channel list never filled");
}

describe("Now", { timeout: 40_000 }, () => {

  it("titles the page with the listening area and pins who's talking", async () => {
    setup();
    await rowsReady();
    expect(screen.getByRole("heading", { level: 1, name: /Downtown business radio/ })).toBeInTheDocument();
    const talking = screen.getByRole("list", { name: "Talking now" });
    expect(within(talking).getAllByRole("listitem")).toHaveLength(2);
    expect(screen.getByText(/than usual$|^About normal$/)).toBeInTheDocument();
    expect(screen.queryByText(/Still learning|Quiet right now/)).not.toBeInTheDocument();
  });

  it("keeps noise out of the channel list and tells same-named rows apart", async () => {
    const { user } = setup();
    await rowsReady();
    const list = screen.getByRole("list", { name: "Channels" });
    expect(within(list).getByText("Downtown ambassadors")).toBeInTheDocument();
    expect(within(list).queryByText(/Unidentified|451\.806 MHz/)).not.toBeInTheDocument();
    expect(within(list).getAllByText("Shared business network")).toHaveLength(2);
    expect(within(list).getByText("Line 1")).toBeInTheDocument();
    expect(within(list).getByText("Line 2")).toBeInTheDocument();
    const other = screen.getByRole("button", { name: /^Other activity \(\d+\)$/ });
    expect(other).toHaveAttribute("aria-expanded", "false");
    await user.click(other);
    expect(within(screen.getByRole("list", { name: "Other activity" })).getByText("451.806 MHz")).toBeInTheDocument();
  });

  it("listens with one tap and stops with the player's stop button", async () => {
    const { monitor, user } = setup();
    await rowsReady();
    const [row] = screen.getAllByRole("button", { name: /^Listen to Downtown ambassadors/ });
    await user.click(row);
    expect(monitor).toHaveBeenCalledWith("452685504-DMR-1");
    const player = await screen.findByRole("region", { name: "Player" });
    expect(player).toHaveTextContent("Downtown ambassadors");
    await user.click(within(player).getByRole("button", { name: "Stop" }));
    await waitFor(() => expect(monitor).toHaveBeenLastCalledWith(null));
    expect(screen.queryByRole("region", { name: "Player" })).not.toBeInTheDocument();
  });

  it("never opens the info sheet from a row tap; About is its own button", async () => {
    const { user } = setup();
    await rowsReady();
    const [row] = screen.getAllByRole("button", { name: /^Listen to Private business radio/ });
    await user.click(row);
    // Can't be heard: the player says so in a few words; no sheet.
    expect(await screen.findByRole("region", { name: "Player" })).toHaveTextContent("Encrypted");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    await user.click(screen.getAllByRole("button", { name: "About Private business radio" })[0]);
    expect(await screen.findByRole("dialog")).toHaveTextContent("Encrypted");
  });

  it("follows a source from its sheet", async () => {
    const { user } = setup();
    await rowsReady();
    await user.click(screen.getAllByRole("button", { name: "About Convention Center radio" })[0]);
    await user.click(await screen.findByRole("button", { name: "Follow" }));
    expect(await screen.findByRole("button", { name: "Following" })).toHaveAttribute("aria-pressed", "true");
    expect(JSON.parse(localStorage.getItem("radio.prefs.v1")!).followed).toEqual(["starter-convention"]);
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
