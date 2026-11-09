// Settings shared by the router, console and chat apps of this example.
// Everything binds to loopback; nothing here is meant for a public deployment.

// Browser bundles import this module too, so `process` may not exist.
const env: Record<string, string | undefined> =
  typeof process === "undefined" ? {} : process.env;

export const HOST = "127.0.0.1";
export const PORTS = {
  router: Number(env.SUPERCHAT_ROUTER_PORT ?? 4400),
  chat: Number(env.SUPERCHAT_CHAT_PORT ?? 4410),
  console: Number(env.SUPERCHAT_CONSOLE_PORT ?? 4420),
};
export const ROUTER_URL = `http://${HOST}:${PORTS.router}`;

export const ENVIRONMENT = "test";
export const SERVICE_ID = "chat";
export const CREDIT_ASSET = "credit:superchat";
export const APP_MEMBER = "superchat-app";
/** The scopes a chat app key gets: call the Service and read its own credits. */
export const APP_SCOPES = ["invoke", "credits:read"];
export const MAX_OUTPUT_TOKENS = 256;

/** Customers created on first run, with the credits they start with. */
export const SEED_CUSTOMERS = [
  { id: "alice", credits: "5" },
  { id: "bob", credits: "0.12" },
];

export const SCENARIOS = [
  { id: "normal", label: "Normal", note: "The primary provider answers." },
  {
    id: "fallback",
    label: "Fallback",
    note: "The primary refuses before running; the backup answers. Charged once.",
  },
  {
    id: "all-fail",
    label: "All providers fail",
    note: "Both refuse before running. Nothing is charged; the hold is released.",
  },
  {
    id: "stream-cut",
    label: "Stream cut",
    note: "The primary stops mid-answer without final usage. The hold stays pending for reconciliation.",
  },
  {
    id: "unknown",
    label: "Outcome unknown",
    note: "The primary times out after the request was sent. Not retried elsewhere; the hold stays pending.",
  },
] as const;
export type ScenarioId = (typeof SCENARIOS)[number]["id"];
export const isScenario = (s: unknown): s is ScenarioId =>
  SCENARIOS.some((x) => x.id === s);

const ATOMS = 1_000_000_000n;

/** Credits as a decimal string (e.g. "0.05") to ledger atoms (scale 9). */
export function creditsToAtoms(credits: string): string {
  if (!/^\d{1,12}(\.\d{1,9})?$/.test(credits))
    throw new Error("invalid_credits");
  const [whole, frac = ""] = credits.split(".");
  return (BigInt(whole!) * ATOMS + BigInt(frac.padEnd(9, "0"))).toString();
}

/** Ledger atoms to a readable credit amount, trimmed to `digits` decimals. */
export function formatCredits(
  atoms: string | bigint | null | undefined,
  digits = 4,
) {
  if (atoms === null || atoms === undefined || atoms === "") return "—";
  const n = BigInt(atoms);
  const sign = n < 0n ? "-" : "";
  const abs = n < 0n ? -n : n;
  const whole = abs / ATOMS;
  const frac = (abs % ATOMS).toString().padStart(9, "0").slice(0, digits);
  return `${sign}${whole.toLocaleString("en-US")}.${frac}`;
}
