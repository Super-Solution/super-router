// Local, gitignored state for the example: which workspace it uses, the owner key that
// administers it, and a registry of what the console created. Server-side only.
import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { randomBytes } from "node:crypto";

export const STATE_FILE =
  process.env.SUPERCHAT_STATE_FILE ??
  join(dirname(fileURLToPath(import.meta.url)), "../../../.local/state.json");

export interface IssuedKey {
  id: string;
  customer_id: string;
  token_hint: string;
  created_at: string;
  expires_at: string;
  revoked_at?: string;
}
export interface LocalState {
  workspace: string;
  owner_key: string;
  digest_key: string;
  customers: string[];
  keys: IssuedKey[];
}

const fresh = (): LocalState => ({
  workspace: "superchat-" + randomBytes(4).toString("hex"),
  owner_key: "sr_" + randomBytes(32).toString("hex"),
  digest_key: randomBytes(32).toString("hex"),
  customers: [],
  keys: [],
});

export async function readState(): Promise<LocalState | null> {
  try {
    return JSON.parse(await readFile(STATE_FILE, "utf8")) as LocalState;
  } catch {
    return null;
  }
}

export async function writeState(state: LocalState) {
  await mkdir(dirname(STATE_FILE), { recursive: true });
  const tmp = STATE_FILE + ".tmp";
  await writeFile(tmp, JSON.stringify(state, null, 2) + "\n", { mode: 0o600 });
  await rename(tmp, STATE_FILE);
}

/** Load the state, creating a new workspace identity on first run or when asked to. */
export async function loadOrCreateState(reset = false) {
  const existing = reset ? null : await readState();
  if (existing) return { state: existing, created: false };
  const state = fresh();
  await writeState(state);
  return { state, created: true };
}

/** Serialise read-modify-write updates from the console within one process. */
let queue: Promise<unknown> = Promise.resolve();
export function updateState(fn: (s: LocalState) => void | Promise<void>) {
  const next = queue.then(async () => {
    const s = await readState();
    if (!s) throw new Error("state_missing");
    await fn(s);
    await writeState(s);
    return s;
  });
  queue = next.catch(() => undefined);
  return next;
}
