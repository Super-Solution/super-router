import { randomUUID } from "node:crypto";
import {
  Service,
  RouterError,
  type Principal,
  type Offering,
  type Usage,
} from "@supersolutions/super-router-contracts";
import {
  charge,
  type BalanceRow,
  type Clock,
  type DataStore,
  type Leg,
  type RequestRecord,
} from "@supersolutions/super-router-core";

interface Account {
  id: string;
  owner: string;
  kind: string;
  asset: string;
  balance: bigint;
  held: bigint;
}
interface Bucket {
  id: string;
  seq: number;
  account: string;
  source: string;
  expiresAt: number | null;
  remaining: bigint;
  granted: bigint;
  held: bigint;
  serviceIds: string[] | null;
  sourceKind: string;
}
interface Allocation {
  bucket: string;
  account: string;
  amount: bigint;
  charged: bigint;
}
interface Attempt {
  id: string;
  offering: string;
  status: string;
  usage: Usage | null;
  cost: bigint | null;
  bound: bigint;
  upstream: string | null;
}
interface Request {
  record: RequestRecord;
  idem: string;
  digest: string;
  customer: string;
  providerSpent: bigint;
  providerHeld: bigint;
  attempts: Attempt[];
  allocations: Allocation[];
  createdAt: number;
}
export interface JournalEntry {
  key: string;
  asset: string;
  debit: string;
  credit: string;
  amount: string;
  reason: string;
}
export interface OutboxEvent {
  id: string;
  type: "billing.settled" | "billing.released" | "reconciliation.required";
  request: string;
}
/** Tenant state. A tenant is one workspace and environment. */
class Tenant {
  services = new Map<string, Service[]>();
  accounts = new Map<string, Account>();
  accountsById = new Map<string, Account>();
  buckets: Bucket[] = [];
  requests = new Map<string, Request>();
  idem = new Map<string, string>();
  disabled = new Set<string>();
  journal: JournalEntry[] = [];
  outbox: OutboxEvent[] = [];
  reconciliation = new Map<string, string>();
  active = new Map<string, number>();
  entitlements: {
    customer: string;
    serviceIds: string[];
    start: number;
    end: number;
  }[] = [];
}
const rank = (kind: string) =>
  kind === "subscription" ? 0 : kind === "promotion" ? 1 : 2;
const byExpiry = (a: Bucket, b: Bucket) =>
  (a.expiresAt ?? Infinity) - (b.expiresAt ?? Infinity) ||
  rank(a.sourceKind) - rank(b.sourceKind) ||
  a.seq - b.seq;
const same = (a: unknown, b: unknown) =>
  JSON.stringify(a) === JSON.stringify(b);

/**
 * Reference in-memory implementation of {@link DataStore}.
 *
 * It follows the PostgreSQL store's semantics exactly: idempotent admission, bucketed holds, provider
 * budget, balanced journal, append-only outbox. Every method runs to completion without awaiting, so
 * each one is atomic in a single process. State is lost on restart.
 */
export class MemoryStore implements DataStore {
  private tenants = new Map<string, Tenant>();
  private seq = 0;
  constructor(private clock: Clock = Date.now) {}

  private tenant(p: Principal): Tenant {
    const key = p.workspace + "\u0000" + p.environment;
    let t = this.tenants.get(key);
    if (!t) this.tenants.set(key, (t = new Tenant()));
    return t;
  }
  private id(prefix: string) {
    return prefix + String(++this.seq).padStart(8, "0");
  }
  private row(t: Tenant, p: Principal, id: string): Request {
    const r = t.requests.get(id);
    if (!r || r.customer !== p.customer)
      throw new RouterError("resource_not_found", 404);
    if (p.service_ids && !p.service_ids.includes(r.record.service.id))
      throw new RouterError("scope_denied", 403);
    return r;
  }
  /** What callers see: a copy, so nothing can mutate store state. */
  private view(r: Request): RequestRecord {
    return {
      ...r.record,
      legs: r.record.legs.map((l) => ({ ...l })),
      provider_spent: r.providerSpent.toString(),
    };
  }

  async replay(p: Principal, key: string, digest: string) {
    const t = this.tenant(p);
    const id = t.idem.get(key);
    if (!id) return null;
    const r = t.requests.get(id)!;
    if (r.digest !== digest || r.customer !== p.customer)
      throw new RouterError("idempotency_conflict", 409);
    return this.view(r);
  }
  async get(p: Principal, id: string) {
    return this.view(this.row(this.tenant(p), p, id));
  }
  async publish(
    p: Principal,
    raw: unknown,
    mode: "cloud" | "self-host" = "self-host",
    expectedVersion?: number,
  ) {
    if (!p.scopes.includes("service:write"))
      throw new RouterError("scope_denied", 403);
    const s = Service.parse(raw);
    if (p.service_ids && !p.service_ids.includes(s.id))
      throw new RouterError("scope_denied", 403);
    if (mode === "cloud" && s.offerings.some((o) => o.source === "byok"))
      throw new RouterError("byok_not_allowed", 403);
    const t = this.tenant(p);
    const versions = t.services.get(s.id) ?? [];
    const previous = versions.at(-1);
    if (expectedVersion !== undefined && previous?.version !== expectedVersion)
      throw new RouterError("service_version_conflict", 409);
    if (mode === "cloud" && p.auth_key_id) {
      if (
        !previous ||
        !same(previous.platform_price, s.platform_price) ||
        !same(previous.offerings, s.offerings)
      )
        throw new RouterError("platform_pricing_protected", 403);
    }
    const existing = versions.find((v) => v.version === s.version);
    if (existing) {
      if (!same(existing, s)) throw new RouterError("version_exists", 409);
      return s;
    }
    if (previous && s.version <= previous.version)
      throw new RouterError("service_version_conflict", 409);
    versions.push(structuredClone(s));
    t.services.set(s.id, versions);
    t.active.set(s.id, s.version);
    return s;
  }
  async getService(p: Principal, id: string, version?: number) {
    if (p.service_ids && !p.service_ids.includes(id))
      throw new RouterError("scope_denied", 403);
    const t = this.tenant(p);
    const wanted = version ?? t.active.get(id);
    const s = (t.services.get(id) ?? []).find((v) => v.version === wanted);
    if (!s) throw new RouterError("resource_not_found", 404);
    return structuredClone(s);
  }
  async list(p: Principal) {
    const t = this.tenant(p);
    return [...t.active.entries()]
      .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
      .slice(0, 100)
      .map(([id, version]) =>
        structuredClone(
          t.services.get(id)!.find((v) => v.version === version)!,
        ),
      )
      .filter((s) => !p.service_ids || p.service_ids.includes(s.id));
  }
  async grant(
    p: Principal,
    kind: "customer_sale" | "platform_sale",
    asset: string,
    amount: string,
    source: string,
    expires?: Date,
    serviceIds?: string[],
    sourceKind = "purchased",
  ) {
    if (!p.scopes.includes("billing:write"))
      throw new RouterError("scope_denied", 403);
    if (!/^[1-9][0-9]{0,29}$/.test(amount))
      throw new RouterError("invalid_amount");
    const t = this.tenant(p);
    const owner = kind === "platform_sale" ? p.workspace : p.customer;
    const prior = t.buckets.find((b) => b.source === source);
    if (prior) {
      const a = t.accountsById.get(prior.account)!;
      if (
        prior.granted.toString() !== amount ||
        a.asset !== asset ||
        a.owner !== owner ||
        a.kind !== kind ||
        prior.expiresAt !== (expires?.getTime() ?? null) ||
        !same(prior.serviceIds, serviceIds ?? null) ||
        prior.sourceKind !== sourceKind
      )
        throw new RouterError("idempotency_conflict", 409);
      return a.id;
    }
    const key = [owner, kind, asset].join("\u0000");
    let account = t.accounts.get(key);
    if (!account)
      t.accounts.set(
        key,
        (account = {
          id: this.id("acct_"),
          owner,
          kind,
          asset,
          balance: 0n,
          held: 0n,
        }),
      );
    t.accountsById.set(account.id, account);
    account.balance += BigInt(amount);
    t.buckets.push({
      id: this.id("bkt_"),
      seq: ++this.seq,
      account: account.id,
      source,
      expiresAt: expires?.getTime() ?? null,
      remaining: BigInt(amount),
      granted: BigInt(amount),
      held: 0n,
      serviceIds: serviceIds ?? null,
      sourceKind,
    });
    t.journal.push({
      key: "grant:" + source,
      asset,
      debit: "issuer:" + asset,
      credit: account.id,
      amount,
      reason: "grant",
    });
    return account.id;
  }
  async accounts(p: Principal, s: Service) {
    const t = this.tenant(p);
    const legs: Leg[] = [];
    for (const [kind, price] of [
      ["customer_sale", s.price],
      ["platform_sale", s.platform_price],
    ] as const) {
      if (!price) continue;
      const owner = kind === "platform_sale" ? p.workspace : p.customer;
      const account = t.accounts.get([owner, kind, price.asset].join("\u0000"));
      if (!account) throw new RouterError("insufficient_balance", 402);
      legs.push({ kind, account: account.id, price, reserved: "0" });
    }
    return legs;
  }
  async reserve(
    p: Principal,
    key: string,
    digest: string,
    service: Service,
    legs: Leg[],
    deadline: string,
    decision?: unknown,
  ) {
    void decision;
    if (p.service_ids && !p.service_ids.includes(service.id))
      throw new RouterError("scope_denied", 403);
    const t = this.tenant(p);
    if (
      service.requires_subscription &&
      !t.entitlements.some(
        (e) =>
          e.customer === p.customer &&
          e.serviceIds.includes(service.id) &&
          e.start <= this.clock() &&
          this.clock() < e.end,
      )
    )
      throw new RouterError("subscription_required", 403);
    const oldId = t.idem.get(key);
    if (oldId) {
      const old = t.requests.get(oldId)!;
      if (old.digest !== digest || old.customer !== p.customer)
        throw new RouterError("idempotency_conflict", 409);
      return { request: this.view(old), fresh: false };
    }
    if (t.active.get(service.id) !== service.version)
      throw new RouterError("inactive_service_version", 409);
    // Compute every allocation first, then apply: a failure leaves no partial hold.
    const now = this.clock();
    const taken: { bucket: Bucket; amount: bigint; account: Account }[] = [];
    const held = new Map<Bucket, bigint>();
    for (const leg of [...legs].sort((a, b) =>
      a.account < b.account ? -1 : a.account > b.account ? 1 : 0,
    )) {
      let remaining = BigInt(leg.reserved);
      const account = t.accountsById.get(leg.account);
      if (!account) throw new RouterError("insufficient_balance", 402);
      const buckets = t.buckets
        .filter(
          (b) =>
            b.account === leg.account &&
            (b.expiresAt === null || b.expiresAt > now) &&
            (b.serviceIds === null || b.serviceIds.includes(service.id)),
        )
        .sort(byExpiry);
      for (const b of buckets) {
        const available = b.remaining - b.held - (held.get(b) ?? 0n);
        const take = available < remaining ? available : remaining;
        if (take <= 0n) continue;
        held.set(b, (held.get(b) ?? 0n) + take);
        taken.push({ bucket: b, amount: take, account });
        remaining -= take;
        if (!remaining) break;
      }
      if (remaining > 0n) throw new RouterError("insufficient_balance", 402);
    }
    const id = randomUUID();
    const record: RequestRecord = {
      id,
      workspace: p.workspace,
      environment: p.environment,
      customer: p.customer,
      service: structuredClone(service),
      execution: "accepted",
      billing: "reserved",
      cancel_requested: false,
      output_started: false,
      deadline,
      legs: legs.map((l) => ({ ...l })),
      provider_spent: "0",
    };
    const request: Request = {
      record,
      idem: key,
      digest,
      customer: p.customer,
      providerSpent: 0n,
      providerHeld: 0n,
      attempts: [],
      allocations: [],
      createdAt: now,
    };
    for (const { bucket, amount } of taken) {
      bucket.held += amount;
      request.allocations.push({
        bucket: bucket.id,
        account: bucket.account,
        amount,
        charged: 0n,
      });
    }
    for (const leg of legs) {
      const account = t.accountsById.get(leg.account)!;
      account.held += BigInt(leg.reserved);
    }
    t.requests.set(id, request);
    t.idem.set(key, id);
    return { request: this.view(request), fresh: true };
  }
  async begin(p: Principal, id: string, o: Offering, bound: string) {
    const t = this.tenant(p);
    const r = this.row(t, p, id);
    if (
      r.record.cancel_requested ||
      this.clock() > new Date(r.record.deadline).getTime()
    )
      throw new RouterError("cancelled", 409);
    if (r.record.billing !== "reserved")
      throw new RouterError("state_conflict", 409);
    if (t.disabled.has(o.id)) throw new RouterError("connection_disabled", 409);
    const snapshot = r.record.service.offerings.find((x) => x.id === o.id);
    if (!snapshot || !same(snapshot, o))
      throw new RouterError("state_conflict", 409);
    if (
      r.providerSpent + r.providerHeld + BigInt(bound) >
      BigInt(r.record.service.max_provider_cost)
    )
      throw new RouterError("budget_exceeded", 402);
    const attempt = randomUUID();
    r.attempts.push({
      id: attempt,
      offering: o.id,
      status: "dispatched",
      usage: null,
      cost: null,
      bound: BigInt(bound),
      upstream: null,
    });
    r.record.execution = "running";
    r.providerHeld += BigInt(bound);
    return attempt;
  }
  async output(p: Principal, id: string) {
    this.row(this.tenant(p), p, id).record.output_started = true;
  }
  async attemptResult(
    p: Principal,
    id: string,
    attempt: string,
    status: string,
    usage: Usage | null,
    cost: string | null,
    upstream?: string,
  ) {
    const r = this.row(this.tenant(p), p, id);
    if (!["reserved", "pending"].includes(r.record.billing))
      throw new RouterError("state_conflict", 409);
    const a = r.attempts.find((x) => x.id === attempt);
    if (!a) throw new RouterError("resource_not_found", 404);
    if (a.status !== "dispatched") return;
    a.status = status;
    a.usage = usage;
    a.cost = cost === null ? null : BigInt(cost);
    a.upstream = upstream ?? null;
    if (a.cost !== null) {
      r.providerSpent += a.cost;
      r.providerHeld -= a.bound;
    }
  }
  async finish(
    p: Principal,
    id: string,
    execution: string,
    usage: Usage | null,
    pending: boolean,
  ) {
    const t = this.tenant(p);
    this.finishTx(t, p, this.row(t, p, id), execution, usage, pending);
  }
  private event(t: Tenant, id: string, type: OutboxEvent["type"]) {
    t.outbox.push({ id: randomUUID(), type, request: id });
  }
  private finishTx(
    t: Tenant,
    p: Principal,
    r: Request,
    execution: string,
    usage: Usage | null,
    pending: boolean,
  ) {
    const rec = r.record;
    if (["settled", "released"].includes(rec.billing)) return;
    if (pending && !rec.output_started && !r.attempts.length) {
      pending = false;
      execution = "cancelled";
    }
    if (usage && r.attempts.some((a) => a.cost !== null && a.cost > a.bound))
      pending = true;
    if (
      pending ||
      (usage &&
        rec.legs.some((l) => charge(usage, l.price) > BigInt(l.reserved)))
    ) {
      if (rec.billing !== "pending")
        this.event(t, rec.id, "reconciliation.required");
      rec.execution = execution;
      rec.billing = "pending";
      if (!t.reconciliation.has(rec.id))
        t.reconciliation.set(
          rec.id,
          pending ? "unknown_outcome" : "reservation_exceeded",
        );
      return;
    }
    for (const leg of rec.legs) {
      let due = usage ? charge(usage, leg.price) : 0n;
      const amount = due;
      const mine = r.allocations
        .filter((a) => a.account === leg.account)
        .map((a) => ({ a, b: t.buckets.find((b) => b.id === a.bucket)! }))
        .sort((x, y) => byExpiry(x.b, y.b));
      for (const { a, b } of mine) {
        const take = due < a.amount ? due : a.amount;
        due -= take;
        a.charged = take;
        b.remaining -= take;
        b.held -= a.amount;
      }
      const account = t.accountsById.get(leg.account)!;
      account.balance -= amount;
      account.held -= BigInt(leg.reserved);
      t.journal.push({
        key: "settle:" + rec.id + ":" + leg.kind,
        asset: leg.price.asset,
        debit: leg.account,
        credit: "consumed:" + leg.price.asset,
        amount: amount.toString(),
        reason: "settlement",
      });
    }
    rec.execution = execution;
    rec.billing = usage ? "settled" : "released";
    this.expire(t);
    this.event(t, rec.id, usage ? "billing.settled" : "billing.released");
  }
  /** Move unheld remainder of expired buckets out of the balance. */
  private expire(t: Tenant) {
    const now = this.clock();
    for (const b of t.buckets) {
      if (b.expiresAt === null || b.expiresAt > now || b.remaining <= b.held)
        continue;
      const n = b.remaining - b.held;
      b.remaining = b.held;
      const account = t.accountsById.get(b.account)!;
      account.balance -= n;
      t.journal.push({
        key: "expire:" + this.id("x_"),
        asset: account.asset,
        debit: b.account,
        credit: "expired:" + account.asset,
        amount: n.toString(),
        reason: "expiry",
      });
    }
  }
  async balance(p: Principal): Promise<BalanceRow[]> {
    const t = this.tenant(p);
    this.expire(t);
    return [...t.accounts.values()]
      .filter(
        (a) =>
          a.owner === p.customer ||
          (a.owner === p.workspace && a.kind === "platform_sale"),
      )
      .sort((a, b) => (a.kind < b.kind ? -1 : a.kind > b.kind ? 1 : 0))
      .map((a) => ({
        id: a.id,
        kind: a.kind,
        asset: a.asset,
        balance: a.balance.toString(),
        held: a.held.toString(),
        available: (a.balance - a.held).toString(),
      }));
  }
  async cancel(p: Principal, id: string) {
    this.row(this.tenant(p), p, id).record.cancel_requested = true;
  }
  async disable(p: Principal, offering: string) {
    if (!p.scopes.includes("service:write"))
      throw new RouterError("scope_denied", 403);
    this.tenant(p).disabled.add(offering);
  }
  /** Never redispatch an expired uncertain attempt. */
  async recover() {
    let n = 0;
    const now = this.clock();
    for (const [key, t] of this.tenants) {
      const [workspace, environment] = key.split("\u0000") as [string, string];
      for (const r of [...t.requests.values()]) {
        if (
          r.record.billing !== "reserved" ||
          new Date(r.record.deadline).getTime() >= now
        )
          continue;
        const last = r.attempts.at(-1);
        const knownSuccess =
          !!last &&
          ["succeeded", "partial"].includes(last.status) &&
          !!last.usage;
        const p: Principal = {
          workspace,
          environment,
          customer: r.customer,
          scopes: [],
        };
        this.finishTx(
          t,
          p,
          r,
          knownSuccess
            ? last!.status
            : r.attempts.length
              ? "unknown"
              : "cancelled",
          knownSuccess ? last!.usage : null,
          r.attempts.length > 0 && !knownSuccess,
        );
        n++;
      }
    }
    return n;
  }
  /** Entitle a customer to subscription-only services for a period. The control plane does this in the SQL store. */
  entitle(p: Principal, serviceIds: string[], start: Date, end: Date) {
    this.tenant(p).entitlements.push({
      customer: p.customer,
      serviceIds,
      start: start.getTime(),
      end: end.getTime(),
    });
  }
  /** Read-only views for tests and diagnostics. */
  inspect(p: Principal) {
    const t = this.tenant(p);
    return {
      journal: t.journal.map((e) => ({ ...e })),
      outbox: t.outbox.map((e) => ({ ...e })),
      reconciliation: Object.fromEntries(t.reconciliation),
      buckets: t.buckets.map((b) => ({
        source: b.source,
        remaining: b.remaining.toString(),
        held: b.held.toString(),
      })),
    };
  }
}
