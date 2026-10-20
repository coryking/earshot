/**
 * Starts a private engine (HTTP 8830, ingest 8831) and loops the DMR fixture
 * through engine/examples/fixture_ingress when the binaries and the private
 * fixture exist on this box. Otherwise fixture specs skip. Receive-only; never
 * touches the Pluto or the production service.
 */
import { spawn, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const repo = process.env.RADIO_REPO ?? join(homedir(), "projects/amateur-radio");
const engine = process.env.RADIO_ENGINE_BIN ?? join(repo, "engine/target/release/radio-engine");
const ingress = process.env.RADIO_FIXTURE_INGRESS ?? join(repo, "engine/target/release/examples/fixture_ingress");
const fixture = process.env.RADIO_FIXTURE ?? join(repo, "output/native-handoff/dmr-slot2-25ksps-ci16le.raw");

export default async function globalSetup() {
  if (![engine, ingress, fixture].every(existsSync)) {
    console.warn("fixture: engine binaries or fixture missing; fixture specs will skip");
    return;
  }
  const children: ChildProcess[] = [];
  const server = spawn(engine, ["serve", "--port", "8830", "--ingest-port", "8831", "--max-decoders", "8"], {
    cwd: join(repo, "engine"),
    stdio: "ignore",
  });
  children.push(server);
  await new Promise((r) => setTimeout(r, 1000));
  let stopped = false;
  const loop = () => {
    if (stopped) return;
    const feed = spawn(ingress, [fixture, "127.0.0.1:8831"], { stdio: "ignore" });
    children.push(feed);
    feed.on("exit", () => setTimeout(loop, 1500));
  };
  loop();
  process.env.FIXTURE_ENGINE = "1";
  const stop = () => {
    stopped = true;
    children.forEach((c) => c.kill());
  };
  // Also when the run is interrupted, so no private engine is left on 8830.
  process.once("exit", stop);
  process.once("SIGINT", stop);
  process.once("SIGTERM", stop);
  return async () => stop();
}
