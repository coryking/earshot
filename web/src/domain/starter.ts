/**
 * Starter names for the 454 MHz listening area, from the 2026-10-19 wideband
 * capture and the probable users in docs/rf-environment-seattle.md § 6
 * (rf-research branch). Frequencies are as measured by this receiver (before
 * the ~10.6 kHz LO correction), so they line up with the engine's channels.
 * A licence is not proof of who is talking, so descriptions say "probably".
 *
 * Live mode uses these until the engine serves GET /api/catalog; the demo
 * mock serves them as its catalog. They match channels by frequency within
 * 3 kHz, any slot.
 */
import type { WireCatalogEntry } from "@/api/needs";

interface Starter {
  id: string;
  frequencyHz: number;
  name: string;
  description: string;
  probableUser: string | null;
  trafficKind: string | null;
  mode: WireCatalogEntry["mode"];
  encryption: WireCatalogEntry["encryption"];
}

const STARTERS: Starter[] = [
  {
    id: "columbia-shared",
    frequencyHz: 454_029_223,
    name: "Columbia Tower shared radio",
    description: "Shared business radio system run from Columbia Tower",
    probableUser: "Businesses renting time on a shared radio network (TRBOWEST / Day Wireless)",
    trafficKind: "Work chatter from many small businesses: deliveries, security, building crews",
    mode: "dmr",
    encryption: "clear",
  },
  {
    id: "ambassadors",
    frequencyHz: 452_685_504,
    name: "Downtown ambassadors",
    description: "Street ambassadors and sidewalk cleaning crews",
    probableUser: "Probably the Metropolitan Improvement District (FCC licence WPQB331)",
    trafficKind: "Cleanups, people needing help, sidewalk issues",
    mode: "dmr",
    encryption: "clear",
  },
  {
    id: "convention",
    frequencyHz: 451_360_300,
    name: "Convention Center radio",
    description: "Event, security and building staff",
    probableUser: "Probably the Washington State Convention Center (FCC licence WQJB646)",
    trafficKind: "Doors, loading docks, event setup; busiest on show days",
    mode: "dmr",
    encryption: "clear",
  },
  {
    id: "fleetnet",
    frequencyHz: 452_341_800,
    name: "FleetNet business radio",
    description: "Shared radio network for service fleets",
    probableUser: "Customers of FleetNet / Silke Communications",
    trafficKind: "Dispatch for trades and delivery fleets",
    mode: "nxdn",
    encryption: "clear",
  },
  {
    id: "amazon",
    frequencyHz: 452_160_600,
    name: "Amazon operations",
    description: "Campus operations and facilities",
    probableUser: "Probably Amazon's downtown campus (FCC licence WRFI741)",
    trafficKind: "Facilities, security and building operations",
    mode: "dmr",
    encryption: "clear",
  },
  {
    id: "stadium",
    frequencyHz: 452_985_500,
    name: "Stadium and event staff",
    description: "Event staff around the stadiums",
    probableUser: "Probably Lumen Field event operations, or a nearby venue sharing the channel",
    trafficKind: "Gates, crowds, ushers; busy only around games and concerts",
    mode: "dmr",
    encryption: "clear",
  },
  {
    id: "hotels",
    frequencyHz: 451_773_100,
    name: "Hotels and buildings",
    description: "A shared channel many downtown hotels and buildings use",
    probableUser: "One of dozens of hotels and office buildings licensed on this channel",
    trafficKind: "Housekeeping, maintenance, door staff",
    mode: "fm",
    encryption: "clear",
  },
  {
    id: "private",
    frequencyHz: 454_554_500,
    name: "Private business radio",
    description: "Private (encrypted)",
    probableUser: "A subscriber on the Day Wireless network at Columbia Tower",
    trafficKind: "Encrypted: you can see when they talk, but not hear it",
    mode: "dmr",
    encryption: "encrypted",
  },
];

export const STARTER_CATALOG: WireCatalogEntry[] = STARTERS.map((s) => ({
  id: `starter-${s.id}`,
  kind: "channel",
  channel_ids: [],
  frequency_hz: s.frequencyHz,
  slot: null,
  system: null,
  talkgroup_id: null,
  name: s.name,
  description: s.description,
  probable_user: s.probableUser,
  traffic_kind: s.trafficKind,
  mode: s.mode,
  encryption: s.encryption,
}));
