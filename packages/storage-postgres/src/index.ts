import { revalidate } from "./access.js";
import pg, { type PoolClient } from "pg";
import { readFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import {
  Service,
  Invocation,
  RouterError,
  type Principal,
  type Offering,
  type Usage,
  EventSchema,
} from "@superrouter/contracts";
import {
  route,
  charge,
  type Store,
  type Leg,
  type RequestRecord,
} from "@superrouter/core";
export const LOCAL_DATABASE_URL =
  "postgresql://superrouter:local-development-only@127.0.0.1:25432/superrouter";
const scope = (p: Principal) => [p.workspace, p.environment];
export class PostgresStore implements Store {
  readonly pool: pg.Pool;
  constructor(url: string) {
    this.pool = new pg.Pool({ connectionString: url, max: 10 });
  }
  async close() {
    await this.pool.end();
  }
  async migrate() {
    const c = await this.pool.connect();
    try {
      await c.query("BEGIN");
      await c.query(
        "SELECT pg_advisory_xact_lock(hashtextextended('superrouter:migrate',0))",
      );
      await c.query(
        "CREATE TABLE IF NOT EXISTS sr_migrations(version integer PRIMARY KEY)",
      );
      const r = await c.query("SELECT 1 FROM sr_migrations WHERE version=1");
      if (!r.rowCount) {
        await c.query(
          await readFile(
            new URL("../migrations/001_initial.sql", import.meta.url),
            "utf8",
          ),
        );
        await c.query("INSERT INTO sr_migrations VALUES(1)");
      }
      for (const [version, file] of [
        [2, "002_integrity.sql"],
        [3, "003_membership_subscriptions.sql"],
        [4, "004_integration_bindings.sql"],
        [5, "005_checkouts.sql"],
      ] as const) {
        const done = await c.query(
          "SELECT 1 FROM sr_migrations WHERE version=$1",
          [version],
        );
        if (!done.rowCount) {
          await c.query(
            await readFile(
              new URL("../migrations/" + file, import.meta.url),
              "utf8",
            ),
          );
          await c.query("INSERT INTO sr_migrations VALUES($1)", [version]);
        }
      }
      await c.query("COMMIT");
    } catch (e) {
      await c.query("ROLLBACK");
      throw e;
    } finally {
      c.release();
    }
  }
  private async tx<T>(
    p: Principal,
    fn: (c: PoolClient) => Promise<T>,
  ): Promise<T> {
    const c = await this.pool.connect();
    try {
      await c.query("BEGIN");
      await c.query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))", [
        JSON.stringify(scope(p)),
      ]);
      const out = await fn(c);
      await c.query("COMMIT");
      return out;
    } catch (e) {
      await c.query("ROLLBACK");
      throw e;
    } finally {
      c.release();
    }
  }
  private async row(c: pg.Pool | PoolClient, p: Principal, id: string) {
    const r = await c.query(
      "SELECT * FROM sr_requests WHERE workspace=$1 AND environment=$2 AND id=$3 AND customer=$4",
      [...scope(p), id, p.customer],
    );
    if (!r.rows[0]) throw new RouterError("resource_not_found", 404);
    if (p.service_ids && !p.service_ids.includes(r.rows[0].service.id))
      throw new RouterError("scope_denied", 403);
    return {
      ...r.rows[0],
      service: Service.parse(r.rows[0].service),
    } as RequestRecord;
  }
  async replay(p: Principal, key: string, digest: string) {
    const r = await this.pool.query(
      "SELECT * FROM sr_requests WHERE workspace=$1 AND environment=$2 AND idem=$3",
      [...scope(p), key],
    );
    if (!r.rows[0]) return null;
    if (r.rows[0].digest !== digest || r.rows[0].customer !== p.customer)
      throw new RouterError("idempotency_conflict", 409);
    return r.rows[0] as RequestRecord;
  }
  async get(p: Principal, id: string) {
    return this.row(this.pool, p, id);
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
    await this.tx(p, async (c) => {
      await revalidate(c, p, "service:write");
      const latest = await c.query(
        "SELECT config,version FROM sr_services WHERE workspace=$1 AND environment=$2 AND id=$3 ORDER BY version DESC LIMIT 1",
        [...scope(p), s.id],
      );
      const previous = latest.rows[0]
        ? Service.parse(latest.rows[0].config)
        : undefined;
      if (
        expectedVersion !== undefined &&
        previous?.version !== expectedVersion
      )
        throw new RouterError("service_version_conflict", 409);
      if (mode === "cloud" && p.auth_key_id) {
        // Database-authenticated developers cannot provision or rewrite platform supply economics.
        if (
          !previous ||
          JSON.stringify(previous.platform_price) !==
            JSON.stringify(s.platform_price) ||
          JSON.stringify(previous.offerings) !== JSON.stringify(s.offerings)
        )
          throw new RouterError("platform_pricing_protected", 403);
      }
      const r = await c.query(
        "SELECT config FROM sr_services WHERE workspace=$1 AND environment=$2 AND id=$3 AND version=$4",
        [...scope(p), s.id, s.version],
      );
      if (r.rowCount) {
        if (
          JSON.stringify(Service.parse(r.rows[0].config)) !== JSON.stringify(s)
        )
          throw new RouterError("version_exists", 409);
        return;
      }
      if (previous && s.version <= previous.version)
        throw new RouterError("service_version_conflict", 409);
      await c.query(
        "UPDATE sr_services SET active=false WHERE workspace=$1 AND environment=$2 AND id=$3",
        [...scope(p), s.id],
      );
      await c.query(
        "INSERT INTO sr_services(workspace,environment,id,version,config) VALUES($1,$2,$3,$4,$5)",
        [...scope(p), s.id, s.version, s],
      );
      await c.query(
        "INSERT INTO sr_audit(id,workspace,environment,actor,action,resource,details) VALUES($1,$2,$3,$4,$5,$6,$7)",
        [
          randomUUID(),
          ...scope(p),
          p.member_id ?? "trusted-host",
          "service.published",
          s.id,
          { version: s.version },
        ],
      );
    });
    return s;
  }
  async getService(p: Principal, id: string, version?: number) {
    if (p.service_ids && !p.service_ids.includes(id))
      throw new RouterError("scope_denied", 403);
    const r = await this.pool.query(
      `SELECT config FROM sr_services WHERE workspace=$1 AND environment=$2 AND id=$3 AND ${version === undefined ? "active=true" : "version=$4"}`,
      [...scope(p), id, ...(version === undefined ? [] : [version])],
    );
    if (!r.rows[0]) throw new RouterError("resource_not_found", 404);
    return Service.parse(r.rows[0].config);
  }
  async list(p: Principal) {
    const r = await this.pool.query(
      "SELECT config FROM sr_services WHERE workspace=$1 AND environment=$2 AND active=true ORDER BY id LIMIT 100",
      scope(p),
    );
    return r.rows
      .map((x) => Service.parse(x.config))
      .filter((s) => !p.service_ids || p.service_ids.includes(s.id));
  }
  async grant(
    p: Principal,
    kind: "customer_sale" | "platform_sale",
    asset: string,
    amount: string,
    source: string,
    expires?: Date,
  ) {
    if (!p.scopes.includes("billing:write"))
      throw new RouterError("scope_denied", 403);
    if (!/^[1-9][0-9]{0,29}$/.test(amount))
      throw new RouterError("invalid_amount");
    return this.tx(p, async (c) => {
      await revalidate(c, p, "billing:write");
      return this.grantTx(c, p, kind, asset, amount, source, expires);
    });
  }
  /** Internal trusted composition: caller owns the tenant transaction and authorization. */
  async grantTx(
    c: PoolClient,
    p: Principal,
    kind: "customer_sale" | "platform_sale",
    asset: string,
    amount: string,
    source: string,
    expires?: Date,
    serviceIds?: string[],
    sourceKind = "purchased",
  ) {
    const owner = kind === "platform_sale" ? p.workspace : p.customer;
    const prior = await c.query(
      "SELECT b.*,a.asset,a.owner,a.kind FROM sr_buckets b JOIN sr_accounts a ON (a.workspace,a.environment,a.id)=(b.workspace,b.environment,b.account) WHERE b.workspace=$1 AND b.environment=$2 AND b.source=$3",
      [...scope(p), source],
    );
    if (prior.rows[0]) {
      const b = prior.rows[0];
      if (
        b.granted !== amount ||
        b.asset !== asset ||
        b.owner !== owner ||
        b.kind !== kind ||
        (b.expires_at?.toISOString() ?? null) !==
          (expires?.toISOString() ?? null)
      )
        throw new RouterError("idempotency_conflict", 409);
      if (
        JSON.stringify(b.service_ids ?? null) !==
          JSON.stringify(serviceIds ?? null) ||
        b.source_kind !== sourceKind
      )
        throw new RouterError("idempotency_conflict", 409);
      return b.account as string;
    }
    await c.query(
      "INSERT INTO sr_accounts(workspace,environment,id,owner,kind,asset) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(workspace,environment,owner,kind,asset) DO NOTHING",
      [...scope(p), randomUUID(), owner, kind, asset],
    );
    const r = await c.query(
      "SELECT id FROM sr_accounts WHERE workspace=$1 AND environment=$2 AND owner=$3 AND kind=$4 AND asset=$5",
      [...scope(p), owner, kind, asset],
    );
    const account = r.rows[0].id;
    await c.query(
      "UPDATE sr_accounts SET balance=balance+$4 WHERE workspace=$1 AND environment=$2 AND id=$3",
      [...scope(p), account, amount],
    );
    await c.query(
      "INSERT INTO sr_buckets(workspace,environment,id,account,source,expires_at,remaining,granted,service_ids,source_kind) VALUES($1,$2,$3,$4,$5,$6,$7,$7,$8,$9)",
      [
        ...scope(p),
        randomUUID(),
        account,
        source,
        expires ?? null,
        amount,
        serviceIds ?? null,
        sourceKind,
      ],
    );
    await c.query(
      "INSERT INTO sr_journal(workspace,environment,business_key,asset,debit,credit,amount,reason) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",
      [
        ...scope(p),
        "grant:" + source,
        asset,
        "issuer:" + asset,
        account,
        amount,
        "grant",
      ],
    );
    return account;
  }
  async accounts(p: Principal, s: Service) {
    const legs: Leg[] = [];
    for (const [kind, price] of [
      ["customer_sale", s.price],
      ["platform_sale", s.platform_price],
    ] as const) {
      if (!price) continue;
      const owner = kind === "platform_sale" ? p.workspace : p.customer;
      const r = await this.pool.query(
        "SELECT id FROM sr_accounts WHERE workspace=$1 AND environment=$2 AND owner=$3 AND kind=$4 AND asset=$5",
        [...scope(p), owner, kind, price.asset],
      );
      if (!r.rows[0]) throw new RouterError("insufficient_balance", 402);
      legs.push({ kind, account: r.rows[0].id, price, reserved: "0" });
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
    return this.tx(p, async (c) => {
      await revalidate(c, p, "invoke");
      if (p.service_ids && !p.service_ids.includes(service.id))
        throw new RouterError("scope_denied", 403);
      if (service.requires_subscription) {
        const entitlement = await c.query(
          "SELECT 1 FROM sr_entitlements WHERE workspace=$1 AND environment=$2 AND customer=$3 AND $4=ANY(service_ids) AND period_start<=now() AND period_end>now()",
          [...scope(p), p.customer, service.id],
        );
        if (!entitlement.rowCount)
          throw new RouterError("subscription_required", 403);
      }
      const old = await c.query(
        "SELECT * FROM sr_requests WHERE workspace=$1 AND environment=$2 AND idem=$3",
        [...scope(p), key],
      );
      if (old.rows[0]) {
        if (
          old.rows[0].digest !== digest ||
          old.rows[0].customer !== p.customer
        )
          throw new RouterError("idempotency_conflict", 409);
        return { request: old.rows[0] as RequestRecord, fresh: false };
      }
      // Admission and service publication share the tenant transaction lock.
      // Replays keep their admitted snapshot; fresh work must reserve the active price.
      const active = await c.query(
        "SELECT 1 FROM sr_services WHERE workspace=$1 AND environment=$2 AND id=$3 AND version=$4 AND active=true",
        [...scope(p), service.id, service.version],
      );
      if (!active.rowCount)
        throw new RouterError("inactive_service_version", 409);
      const id = randomUUID();
      await c.query(
        "INSERT INTO sr_requests(workspace,environment,id,customer,idem,digest,service,legs,deadline,route_decision) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
        [
          ...scope(p),
          id,
          p.customer,
          key,
          digest,
          service,
          JSON.stringify(legs),
          deadline,
          decision ?? null,
        ],
      );
      for (const leg of [...legs].sort((a, b) =>
        a.account.localeCompare(b.account),
      )) {
        let remaining = BigInt(leg.reserved);
        const buckets = await c.query(
          "SELECT * FROM sr_buckets WHERE workspace=$1 AND environment=$2 AND account=$3 AND (expires_at IS NULL OR expires_at>now()) AND (service_ids IS NULL OR $4=ANY(service_ids)) ORDER BY expires_at ASC NULLS LAST, CASE source_kind WHEN 'subscription' THEN 0 WHEN 'promotion' THEN 1 ELSE 2 END,id FOR UPDATE",
          [...scope(p), leg.account, service.id],
        );
        for (const b of buckets.rows) {
          const available = BigInt(b.remaining) - BigInt(b.held);
          const take = available < remaining ? available : remaining;
          if (take <= 0n) continue;
          await c.query(
            "UPDATE sr_buckets SET held=held+$4 WHERE workspace=$1 AND environment=$2 AND id=$3",
            [...scope(p), b.id, take.toString()],
          );
          await c.query(
            "INSERT INTO sr_allocations(workspace,environment,request,bucket,account,amount) VALUES($1,$2,$3,$4,$5,$6)",
            [...scope(p), id, b.id, leg.account, take.toString()],
          );
          remaining -= take;
          if (!remaining) break;
        }
        if (remaining > 0n) throw new RouterError("insufficient_balance", 402);
        await c.query(
          "UPDATE sr_accounts SET held=held+$4 WHERE workspace=$1 AND environment=$2 AND id=$3",
          [...scope(p), leg.account, leg.reserved],
        );
      }
      return { request: await this.row(c, p, id), fresh: true };
    });
  }
  async begin(p: Principal, id: string, o: Offering, bound: string) {
    return this.tx(p, async (c) => {
      await revalidate(c, p, "invoke");
      const r = await this.row(c, p, id);
      if (r.cancel_requested || Date.now() > new Date(r.deadline).getTime())
        throw new RouterError("cancelled", 409);
      if (r.billing !== "reserved")
        throw new RouterError("state_conflict", 409);
      const disabled = await c.query(
        "SELECT 1 FROM sr_disabled_connections WHERE workspace=$1 AND environment=$2 AND offering=$3",
        [...scope(p), o.id],
      );
      if (disabled.rowCount) throw new RouterError("connection_disabled", 409);
      const snapshot = r.service.offerings.find((x) => x.id === o.id);
      if (!snapshot || JSON.stringify(snapshot) !== JSON.stringify(o))
        throw new RouterError("state_conflict", 409);
      const sum = await c.query(
        "SELECT provider_spent+provider_held+$4::numeric AS total FROM sr_requests WHERE workspace=$1 AND environment=$2 AND id=$3",
        [...scope(p), id, bound],
      );
      if (BigInt(sum.rows[0].total) > BigInt(r.service.max_provider_cost))
        throw new RouterError("budget_exceeded", 402);
      const attempt = randomUUID();
      await c.query(
        "INSERT INTO sr_attempts(workspace,environment,id,request,offering,cost_bound) VALUES($1,$2,$3,$4,$5,$6)",
        [...scope(p), attempt, id, o.id, bound],
      );
      await c.query(
        "UPDATE sr_requests SET execution='running',provider_held=provider_held+$4 WHERE workspace=$1 AND environment=$2 AND id=$3",
        [...scope(p), id, bound],
      );
      return attempt;
    });
  }
  async output(p: Principal, id: string) {
    await this.tx(p, async (c) => {
      await this.row(c, p, id);
      await c.query(
        "UPDATE sr_requests SET output_started=true WHERE workspace=$1 AND environment=$2 AND id=$3",
        [...scope(p), id],
      );
    });
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
    await this.tx(p, async (c) => {
      const r = await this.row(c, p, id);
      if (!["reserved", "pending"].includes(r.billing))
        throw new RouterError("state_conflict", 409);
      const a = await c.query(
        "SELECT * FROM sr_attempts WHERE workspace=$1 AND environment=$2 AND id=$3 AND request=$4 FOR UPDATE",
        [...scope(p), attempt, id],
      );
      const row = a.rows[0];
      if (!row) throw new RouterError("resource_not_found", 404);
      if (row.status !== "dispatched") return;
      await c.query(
        "UPDATE sr_attempts SET status=$4,usage=$5,cost=$6,upstream_id=$7 WHERE workspace=$1 AND environment=$2 AND id=$3",
        [...scope(p), attempt, status, usage, cost, upstream ?? null],
      );
      if (cost !== null)
        await c.query(
          "UPDATE sr_requests SET provider_spent=provider_spent+$4,provider_held=provider_held-$5 WHERE workspace=$1 AND environment=$2 AND id=$3",
          [...scope(p), id, cost, row.cost_bound],
        );
    });
  }
  private async event(
    c: PoolClient,
    p: Principal,
    id: string,
    type: "billing.settled" | "billing.released" | "reconciliation.required",
  ) {
    const payload = EventSchema.parse({
      id: randomUUID(),
      type,
      schema_version: 1,
      workspace_id: p.workspace,
      environment_id: p.environment,
      request_id: id,
      occurred_at: new Date().toISOString(),
    });
    await c.query(
      "INSERT INTO sr_outbox(id,workspace,environment,request,payload) VALUES($1,$2,$3,$4,$5)",
      [payload.id, ...scope(p), id, payload],
    );
  }
  async finish(
    p: Principal,
    id: string,
    execution: string,
    usage: Usage | null,
    pending: boolean,
  ) {
    await this.tx(p, (c) => this.finishTx(c, p, id, execution, usage, pending));
  }
  private async finishTx(
    c: PoolClient,
    p: Principal,
    id: string,
    execution: string,
    usage: Usage | null,
    pending: boolean,
  ) {
    const r = await this.row(c, p, id);
    if (["settled", "released"].includes(r.billing)) return;
    if (pending && !r.output_started) {
      const attempts = await c.query(
        "SELECT 1 FROM sr_attempts WHERE workspace=$1 AND environment=$2 AND request=$3",
        [...scope(p), id],
      );
      if (!attempts.rowCount) {
        pending = false;
        execution = "cancelled";
      }
    }
    if (usage) {
      const over = await c.query(
        "SELECT 1 FROM sr_attempts WHERE workspace=$1 AND environment=$2 AND request=$3 AND cost>cost_bound",
        [...scope(p), id],
      );
      if (over.rowCount) pending = true;
    }
    if (
      pending ||
      (usage && r.legs.some((l) => charge(usage, l.price) > BigInt(l.reserved)))
    ) {
      if (r.billing !== "pending")
        await this.event(c, p, id, "reconciliation.required");
      await c.query(
        "UPDATE sr_requests SET execution=$4,billing='pending' WHERE workspace=$1 AND environment=$2 AND id=$3",
        [...scope(p), id, execution],
      );
      await c.query(
        "INSERT INTO sr_reconciliation(workspace,environment,request,reason) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING",
        [...scope(p), id, pending ? "unknown_outcome" : "reservation_exceeded"],
      );
      return;
    }
    for (const leg of r.legs) {
      let due = usage ? charge(usage, leg.price) : 0n;
      const amount = due;
      const allocations = await c.query(
        "SELECT a.*,b.expires_at FROM sr_allocations a JOIN sr_buckets b ON (a.workspace,a.environment,a.bucket)=(b.workspace,b.environment,b.id) WHERE a.workspace=$1 AND a.environment=$2 AND a.request=$3 AND a.account=$4 ORDER BY b.expires_at ASC NULLS LAST, CASE b.source_kind WHEN 'subscription' THEN 0 WHEN 'promotion' THEN 1 ELSE 2 END,b.id",
        [...scope(p), id, leg.account],
      );
      for (const a of allocations.rows) {
        const take = due < BigInt(a.amount) ? due : BigInt(a.amount);
        due -= take;
        await c.query(
          "UPDATE sr_allocations SET charged=$5 WHERE workspace=$1 AND environment=$2 AND request=$3 AND bucket=$4",
          [...scope(p), id, a.bucket, take.toString()],
        );
        await c.query(
          "UPDATE sr_buckets SET remaining=remaining-$4,held=held-$5 WHERE workspace=$1 AND environment=$2 AND id=$3",
          [...scope(p), a.bucket, take.toString(), a.amount],
        );
      }
      await c.query(
        "UPDATE sr_accounts SET balance=balance-$4,held=held-$5 WHERE workspace=$1 AND environment=$2 AND id=$3",
        [...scope(p), leg.account, amount.toString(), leg.reserved],
      );
      await c.query("INSERT INTO sr_settlements VALUES($1,$2,$3,$4,$5,$6)", [
        ...scope(p),
        id,
        leg.kind,
        leg.account,
        amount.toString(),
      ]);
      await c.query(
        "INSERT INTO sr_journal(workspace,environment,business_key,asset,debit,credit,amount,reason) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",
        [
          ...scope(p),
          "settle:" + id + ":" + leg.kind,
          leg.price.asset,
          leg.account,
          "consumed:" + leg.price.asset,
          amount.toString(),
          "settlement",
        ],
      );
    }
    await c.query(
      "UPDATE sr_requests SET execution=$4,billing=$5 WHERE workspace=$1 AND environment=$2 AND id=$3",
      [...scope(p), id, execution, usage ? "settled" : "released"],
    );
    await this.expire(c, p);
    await this.event(c, p, id, usage ? "billing.settled" : "billing.released");
  }
  private async expire(c: PoolClient, p: Principal) {
    const rows = await c.query(
      "SELECT * FROM sr_buckets WHERE workspace=$1 AND environment=$2 AND expires_at<=now() AND remaining>held FOR UPDATE",
      scope(p),
    );
    for (const b of rows.rows) {
      const n = BigInt(b.remaining) - BigInt(b.held);
      await c.query(
        "UPDATE sr_buckets SET remaining=held WHERE workspace=$1 AND environment=$2 AND id=$3",
        [...scope(p), b.id],
      );
      await c.query(
        "UPDATE sr_accounts SET balance=balance-$4 WHERE workspace=$1 AND environment=$2 AND id=$3",
        [...scope(p), b.account, n.toString()],
      );
      const asset = await c.query(
        "SELECT asset FROM sr_accounts WHERE workspace=$1 AND environment=$2 AND id=$3",
        [...scope(p), b.account],
      );
      await c.query(
        "INSERT INTO sr_journal(workspace,environment,business_key,asset,debit,credit,amount,reason) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",
        [
          ...scope(p),
          "expire:" + randomUUID(),
          asset.rows[0].asset,
          b.account,
          "expired:" + asset.rows[0].asset,
          n.toString(),
          "expiry",
        ],
      );
    }
  }
  async balance(p: Principal) {
    return this.tx(p, async (c) => {
      await this.expire(c, p);
      const r = await c.query(
        "SELECT id,kind,asset,balance::text,held::text,(balance-held)::text AS available FROM sr_accounts WHERE workspace=$1 AND environment=$2 AND (owner=$3 OR (owner=$1 AND kind='platform_sale')) ORDER BY kind",
        [...scope(p), p.customer],
      );
      return r.rows;
    });
  }
  async cancel(p: Principal, id: string) {
    await this.tx(p, async (c) => {
      await this.row(c, p, id);
      await c.query(
        "UPDATE sr_requests SET cancel_requested=true WHERE workspace=$1 AND environment=$2 AND id=$3",
        [...scope(p), id],
      );
    });
  }
  async disable(p: Principal, offering: string) {
    if (!p.scopes.includes("service:write"))
      throw new RouterError("scope_denied", 403);
    await this.pool.query(
      "INSERT INTO sr_disabled_connections VALUES($1,$2,$3) ON CONFLICT DO NOTHING",
      [...scope(p), offering],
    );
  }
  async trace(p: Principal, id: string) {
    if (!p.scopes.includes("operations:read"))
      throw new RouterError("scope_denied", 403);
    const request = await this.get(p, id);
    const attempts = await this.pool.query(
      "SELECT id,offering,status,cost,usage FROM sr_attempts WHERE workspace=$1 AND environment=$2 AND request=$3 ORDER BY created_at",
      [...scope(p), id],
    );
    const settlements = await this.pool.query(
      "SELECT kind,amount FROM sr_settlements WHERE workspace=$1 AND environment=$2 AND request=$3",
      [...scope(p), id],
    );
    return { request, attempts: attempts.rows, settlements: settlements.rows };
  }
  /** Never redispatch an expired uncertain attempt. Operator resolution requires evidence. */
  async recover() {
    const rows = await this.pool.query(
      "SELECT * FROM sr_requests WHERE deadline<now() AND billing='reserved'",
    );
    for (const r of rows.rows) {
      const p: Principal = {
        workspace: r.workspace,
        environment: r.environment,
        customer: r.customer,
        scopes: [],
      };
      await this.tx(p, async (c) => {
        const current = await this.row(c, p, r.id);
        if (current.billing !== "reserved") return;
        const a = await c.query(
          "SELECT * FROM sr_attempts WHERE workspace=$1 AND environment=$2 AND request=$3 ORDER BY created_at",
          [...scope(p), r.id],
        );
        const last = a.rows.at(-1);
        const knownSuccess =
          ["succeeded", "partial"].includes(last?.status) && last?.usage;
        await this.finishTx(
          c,
          p,
          r.id,
          knownSuccess ? last.status : a.rowCount ? "unknown" : "cancelled",
          knownSuccess ? last.usage : null,
          Boolean(a.rowCount) && !knownSuccess,
        );
      });
    }
    return rows.rowCount;
  }
  async resolveWaiver(p: Principal, id: string, evidence: string) {
    if (!p.scopes.includes("billing:write") || evidence.length < 10)
      throw new RouterError("scope_denied", 403);
    await this.tx(p, async (c) => {
      const r = await this.row(c, p, id);
      if (r.billing !== "pending") throw new RouterError("state_conflict", 409);
      await this.finishTx(c, p, id, "failed", null, false);
      await c.query(
        "UPDATE sr_reconciliation SET state='open',reason='provider_cost_pending_after_customer_waiver',resolution=$4,resolved_at=NULL WHERE workspace=$1 AND environment=$2 AND request=$3",
        [...scope(p), id, evidence],
      );
    });
  }
  async refund(
    p: Principal,
    id: string,
    kind: string,
    amount: string,
    key: string,
    reason: string,
  ) {
    if (!p.scopes.includes("billing:write"))
      throw new RouterError("scope_denied", 403);
    if (!/^[1-9][0-9]{0,29}$/.test(amount) || reason.length < 5 || !key)
      throw new RouterError("invalid_refund");
    return this.tx(p, async (c) => {
      const r = await this.row(c, p, id);
      const leg = r.legs.find((l) => l.kind === kind);
      if (!leg) throw new RouterError("resource_not_found", 404);
      const old = await c.query(
        "SELECT * FROM sr_refunds WHERE workspace=$1 AND environment=$2 AND business_key=$3",
        [...scope(p), key],
      );
      if (old.rows[0]) {
        const o = old.rows[0];
        if (
          o.request !== id ||
          o.kind !== kind ||
          o.amount !== amount ||
          o.reason !== reason
        )
          throw new RouterError("idempotency_conflict", 409);
        return;
      }
      const result = await c.query(
        "SELECT * FROM sr_settlements WHERE workspace=$1 AND environment=$2 AND request=$3 AND kind=$4",
        [...scope(p), id, kind],
      );
      if (!result.rows[0]) throw new RouterError("state_conflict", 409);
      const refunded = await c.query(
        "SELECT COALESCE(sum(amount),0)::text AS total FROM sr_refunds WHERE workspace=$1 AND environment=$2 AND request=$3 AND kind=$4",
        [...scope(p), id, kind],
      );
      if (
        BigInt(amount) + BigInt(refunded.rows[0].total) >
        BigInt(result.rows[0].amount)
      )
        throw new RouterError("refund_exceeded", 409);
      let remaining = BigInt(amount);
      const allocations = await c.query(
        "SELECT * FROM sr_allocations WHERE workspace=$1 AND environment=$2 AND request=$3 AND account=$4 ORDER BY bucket",
        [...scope(p), id, leg.account],
      );
      for (const a of allocations.rows) {
        const available = BigInt(a.charged) - BigInt(a.refunded);
        const n = available < remaining ? available : remaining;
        if (n <= 0n) continue;
        await c.query(
          "UPDATE sr_allocations SET refunded=refunded+$5 WHERE workspace=$1 AND environment=$2 AND request=$3 AND bucket=$4",
          [...scope(p), id, a.bucket, n.toString()],
        );
        await c.query(
          "UPDATE sr_buckets SET remaining=remaining+$4 WHERE workspace=$1 AND environment=$2 AND id=$3",
          [...scope(p), a.bucket, n.toString()],
        );
        remaining -= n;
      }
      if (remaining !== 0n) throw new RouterError("allocation_invariant", 500);
      await c.query(
        "UPDATE sr_accounts SET balance=balance+$4 WHERE workspace=$1 AND environment=$2 AND id=$3",
        [...scope(p), leg.account, amount],
      );
      await c.query(
        "INSERT INTO sr_refunds(workspace,environment,request,kind,business_key,amount,reason) VALUES($1,$2,$3,$4,$5,$6,$7)",
        [...scope(p), id, kind, key, amount, reason],
      );
      await c.query(
        "INSERT INTO sr_journal(workspace,environment,business_key,asset,debit,credit,amount,reason) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",
        [
          ...scope(p),
          "refund:" + key,
          leg.price.asset,
          "consumed:" + leg.price.asset,
          leg.account,
          amount,
          reason,
        ],
      );
      await this.expire(c, p);
    });
  }
  async claimEvents(
    limit = 20,
    tenant?: Pick<Principal, "workspace" | "environment">,
  ) {
    if (!Number.isInteger(limit) || limit < 1 || limit > 1000)
      throw new RouterError("invalid_limit");
    return this.tx(
      {
        workspace: "system",
        environment: "outbox",
        customer: "worker",
        scopes: [],
      },
      async (c) => {
        const rows = await c.query(
          `SELECT id FROM sr_outbox WHERE (state='pending' OR (state='delivering' AND lease_until<now())) ${tenant ? "AND workspace=$2 AND environment=$3" : ""} ORDER BY created_at,id LIMIT $1 FOR UPDATE SKIP LOCKED`,
          [limit, ...(tenant ? [tenant.workspace, tenant.environment] : [])],
        );
        if (!rows.rowCount) return [];
        const claimed = await c.query(
          "UPDATE sr_outbox SET state='delivering',generation=generation+1,lease_until=now()+interval '30 seconds' WHERE id=ANY($1) RETURNING id,generation,payload",
          [rows.rows.map((x) => x.id)],
        );
        return claimed.rows as {
          id: string;
          generation: number;
          payload: unknown;
        }[];
      },
    );
  }
  async ackEvent(id: string, generation: number) {
    const r = await this.pool.query(
      "UPDATE sr_outbox SET state='delivered',lease_until=NULL WHERE id=$1 AND generation=$2 AND state='delivering' AND lease_until>now()",
      [id, generation],
    );
    return r.rowCount === 1;
  }
}

export { ControlPlane } from "./control.js";
export type { PaymentIntegration } from "./control.js";
