import { randomUUID } from "node:crypto";
import type { PoolClient } from "pg";
import {
  RouterError,
  WEEK_HOURS,
  WindowLimits,
  type Principal,
} from "@supersolutions/super-router-contracts";
export type WindowKind = "session" | "weekly";
export interface WindowSpec {
  kind: WindowKind;
  hours: number;
  limit: bigint;
}
export interface WindowBlock {
  kind: WindowKind;
  limit: bigint;
  used: bigint;
  /** When the open window closes; null when no window is open (the draw alone exceeds the limit). */
  resetsAt: Date | null;
  /** False when even an empty window could not admit the draw, so waiting does not help. */
  retryable: boolean;
}
/** Which of two blocking windows to report: one that waiting cannot fix, else the one that resets last. */
const worse = (a: WindowBlock, b?: WindowBlock) =>
  !b ||
  (!a.retryable && b.retryable) ||
  (a.retryable === b.retryable &&
    (a.resetsAt?.getTime() ?? 0) > (b.resetsAt?.getTime() ?? 0));
const scope = (p: Pick<Principal, "workspace" | "environment">) => [
  p.workspace,
  p.environment,
];
/** The enforced windows of an entitlement snapshot; empty when the option is off or absent. */
export function windowSpecs(raw: unknown): WindowSpec[] {
  if (!raw) return [];
  const w = WindowLimits.parse(raw);
  if (!w.enabled) return [];
  const out: WindowSpec[] = [];
  if (w.session)
    out.push({
      kind: "session",
      hours: w.session.hours,
      limit: BigInt(w.session.credits),
    });
  if (w.weekly)
    out.push({
      kind: "weekly",
      hours: WEEK_HOURS,
      limit: BigInt(w.weekly.credits),
    });
  return out;
}
interface Current {
  id: string;
  window_end: Date;
  spent: string;
  held: string;
}
async function current(
  c: PoolClient,
  p: Pick<Principal, "workspace" | "environment">,
  subscription: string,
  kind: WindowKind,
): Promise<Current | undefined> {
  const r = await c.query(
    "SELECT id,window_end,spent::text,held::text FROM sr_usage_windows WHERE workspace=$1 AND environment=$2 AND subscription=$3 AND kind=$4 AND window_end>now() ORDER BY window_start DESC LIMIT 1 FOR UPDATE",
    [...scope(p), subscription, kind],
  );
  return r.rows[0];
}
/**
 * Admit up to `want` of the subscription's credits against every enforced window. Returns how
 * much may be drawn (the smallest headroom), the windows that now hold it, and, when a window
 * held the draw below `want`, the one that resets last (the wait before the full draw is possible).
 * Windows are opened lazily and only when something is actually drawn. Runs inside the tenant
 * transaction, so concurrent reservations of one tenant cannot overshoot a limit.
 */
export async function admit(
  c: PoolClient,
  p: Pick<Principal, "workspace" | "environment">,
  subscription: string,
  specs: WindowSpec[],
  want: bigint,
) {
  const rows = new Map<WindowKind, Current | undefined>();
  let take = want;
  let blocked: WindowBlock | undefined;
  for (const s of specs) {
    const row = await current(c, p, subscription, s.kind);
    rows.set(s.kind, row);
    const used = row ? BigInt(row.spent) + BigInt(row.held) : 0n;
    const room = s.limit > used ? s.limit - used : 0n;
    if (room < want) {
      const b: WindowBlock = {
        kind: s.kind,
        limit: s.limit,
        used,
        resetsAt: row?.window_end ?? null,
        retryable: want <= s.limit,
      };
      if (worse(b, blocked)) blocked = b;
      if (room < take) take = room;
    }
  }
  if (take <= 0n) return { take: 0n, ids: [] as string[], blocked };
  const ids: string[] = [];
  for (const s of specs) {
    let row = rows.get(s.kind);
    if (!row) {
      const id = randomUUID();
      await c.query(
        "INSERT INTO sr_usage_windows(workspace,environment,id,subscription,kind,window_start,window_end) VALUES($1,$2,$3,$4,$5,now(),now()+make_interval(hours=>$6))",
        [...scope(p), id, subscription, s.kind, s.hours],
      );
      row = { id, window_end: new Date(), spent: "0", held: "0" };
    }
    await c.query(
      "UPDATE sr_usage_windows SET held=held+$4 WHERE workspace=$1 AND environment=$2 AND id=$3",
      [...scope(p), row.id, take.toString()],
    );
    ids.push(row.id);
  }
  return { take, ids, blocked };
}
/**
 * An allocation settled: its hold leaves the windows and what was actually charged enters them.
 * A window that ends up with nothing spent and nothing held (every request in it was released)
 * is dropped, so a request that cost nothing does not start anyone's session clock.
 */
export async function settle(
  c: PoolClient,
  p: Pick<Principal, "workspace" | "environment">,
  ids: string[],
  held: string,
  charged: string,
) {
  if (!ids.length) return;
  await c.query(
    "UPDATE sr_usage_windows SET held=held-$4,spent=spent+$5 WHERE workspace=$1 AND environment=$2 AND id=ANY($3)",
    [...scope(p), ids, held, charged],
  );
  await c.query(
    "DELETE FROM sr_usage_windows WHERE workspace=$1 AND environment=$2 AND id=ANY($3) AND spent=0 AND held=0",
    [...scope(p), ids],
  );
}
/** A refund gives the refunded credits back to the windows the original charge counted against. */
export async function refund(
  c: PoolClient,
  p: Pick<Principal, "workspace" | "environment">,
  ids: string[],
  amount: string,
) {
  if (!ids.length) return;
  await c.query(
    "UPDATE sr_usage_windows SET spent=spent-$4 WHERE workspace=$1 AND environment=$2 AND id=ANY($3)",
    [...scope(p), ids, amount],
  );
}
/** The customer-facing refusal: which window, how full, when it resets. */
export function windowError(b: WindowBlock) {
  return new RouterError("window_limit_reached", 429, b.retryable, {
    window: b.kind,
    limit: b.limit.toString(),
    used: b.used.toString(),
    resets_at: b.resetsAt?.toISOString() ?? null,
  });
}
