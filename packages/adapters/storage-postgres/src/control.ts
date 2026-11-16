import type {
  PaymentAdapter,
  Receipt,
} from "@supersolutions/super-router-payments";
import {
  createHash,
  createHmac,
  randomBytes,
  randomUUID,
  timingSafeEqual,
} from "node:crypto";
import type { PoolClient } from "pg";
import {
  RouterError,
  roleScopes,
  MemberInput,
  CustomerInput,
  KeyInput,
  PlanInput,
  SubscriptionInput,
  PlanChangeInput,
  PaymentInput,
  CreditGrantInput,
  type Principal,
  type Role,
} from "@supersolutions/super-router-contracts";
import type { PostgresStore } from "./index.js";
import { keyPrincipal, revalidate } from "./access.js";
import { windowSpecs } from "./windows.js";
export interface SandboxPaymentIntegration {
  id: string;
  workspace: string;
  environment: string;
  secret: string;
  mode: "sandbox";
}
export interface HostedPaymentIntegration {
  id: string;
  workspace: string;
  environment: string;
  mode: "hosted";
  adapter: PaymentAdapter;
}
export type PaymentIntegration =
  SandboxPaymentIntegration | HostedPaymentIntegration;
const scope = (p: Principal) => [p.workspace, p.environment];
const hash = (s: string) => createHash("sha256").update(s).digest("hex");
function canonical(v: unknown): string {
  if (Array.isArray(v)) return "[" + v.map(canonical).join(",") + "]";
  if (v && typeof v === "object")
    return (
      "{" +
      Object.entries(v)
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([k, x]) => JSON.stringify(k) + ":" + canonical(x))
        .join(",") +
      "}"
    );
  return JSON.stringify(v);
}
const day = 86400000;
export class ControlPlane {
  constructor(
    readonly store: PostgresStore,
    private integrations: PaymentIntegration[] = [],
    private clock: () => Date = () => new Date(),
  ) {
    if (new Set(integrations.map((i) => i.id)).size !== integrations.length)
      throw new Error("duplicate integration");
    for (const i of integrations)
      if (
        i.mode === "sandbox" &&
        (i.secret.length < 32 || i.environment !== "test")
      )
        throw new Error(
          "Sandbox payment ingress requires test environment and >=32-character secret",
        );
  }
  private async tx<T>(
    p: Principal,
    fn: (c: PoolClient) => Promise<T>,
    permission?: string,
  ) {
    const c = await this.store.pool.connect();
    try {
      await c.query("BEGIN");
      await c.query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))", [
        JSON.stringify(scope(p)),
      ]);
      if (permission) await revalidate(c, p, permission);
      const result = await fn(c);
      await c.query("COMMIT");
      return result;
    } catch (e) {
      await c.query("ROLLBACK");
      throw e;
    } finally {
      c.release();
    }
  }
  private async audit(
    c: PoolClient,
    p: Principal,
    action: string,
    resource: string,
    details: unknown = {},
  ) {
    await c.query(
      "INSERT INTO sr_audit(id,workspace,environment,actor,action,resource,details) VALUES($1,$2,$3,$4,$5,$6,$7)",
      [
        randomUUID(),
        ...scope(p),
        p.member_id ?? "trusted-host",
        action,
        resource,
        details,
      ],
    );
  }
  private async command<T extends Record<string, unknown>>(
    p: Principal,
    permission: string,
    operation: string,
    key: string,
    input: unknown,
    fn: (c: PoolClient) => Promise<T>,
    authorize?: (c: PoolClient) => Promise<void>,
  ): Promise<T> {
    if (!key || key.length > 200)
      throw new RouterError("missing_idempotency_key");
    return this.tx(
      p,
      async (c) => {
        if (authorize) await authorize(c);
        const digest = hash(canonical(input));
        const prior = await c.query(
          "SELECT * FROM sr_commands WHERE workspace=$1 AND environment=$2 AND operation=$3 AND key=$4",
          [...scope(p), operation, key],
        );
        if (prior.rows[0]) {
          if (prior.rows[0].digest !== digest)
            throw new RouterError("idempotency_conflict", 409);
          return prior.rows[0].result as T;
        }
        const result = await fn(c);
        const saved = "token" in result ? { ...result, token: null } : result;
        await c.query("INSERT INTO sr_commands VALUES($1,$2,$3,$4,$5,$6)", [
          ...scope(p),
          operation,
          key,
          digest,
          saved,
        ]);
        return result;
      },
      permission,
    );
  }
  /** Host-only bootstrap; never exposed over HTTP. Refuses to replace any existing membership. */
  async bootstrap(
    workspace: string,
    environment: string,
    member = "owner",
    suppliedToken?: string,
  ) {
    if (suppliedToken && !/^sr_[a-f0-9]{64}$/.test(suppliedToken))
      throw new RouterError("invalid_bootstrap_key");
    const p: Principal = {
      workspace,
      environment,
      customer: "_admin",
      member_id: member,
      scopes: [...roleScopes.owner],
    };
    return this.tx(p, async (c) => {
      const existing = await c.query(
        "SELECT 1 FROM sr_members WHERE workspace=$1 AND environment=$2",
        scope(p),
      );
      if (existing.rowCount) throw new RouterError("already_bootstrapped", 409);
      await c.query(
        "INSERT INTO sr_members(workspace,environment,id,role) VALUES($1,$2,$3,$4)",
        [...scope(p), member, "owner"],
      );
      const key = await this.insertKey(
        c,
        p,
        member,
        "_admin",
        [...roleScopes.owner],
        undefined,
        new Date(this.clock().getTime() + 30 * day),
        suppliedToken,
      );
      await this.audit(c, p, "workspace.bootstrap", member);
      return key;
    });
  }
  async authenticate(token: string) {
    if (!/^sr_[a-f0-9]{64}$/.test(token))
      throw new RouterError("invalid_key", 401);
    return keyPrincipal(this.store.pool, hash(token));
  }
  private async insertKey(
    c: PoolClient,
    p: Principal,
    member: string,
    customer: string,
    scopes: string[],
    services: string[] | undefined,
    expires: Date,
    suppliedToken?: string,
  ) {
    const id = randomUUID(),
      token = suppliedToken ?? "sr_" + randomBytes(32).toString("hex");
    await c.query(
      "INSERT INTO sr_keys(workspace,environment,id,member_id,customer,hash,scopes,service_ids,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)",
      [
        ...scope(p),
        id,
        member,
        customer,
        hash(token),
        scopes,
        services ?? null,
        expires,
      ],
    );
    return { id, token, expires_at: expires.toISOString() };
  }
  async member(p: Principal, raw: unknown, key: string) {
    const input = MemberInput.parse(raw);
    return this.command(
      p,
      "identity:write",
      "member",
      key,
      input,
      async (c) => {
        const existing = await c.query(
          "SELECT role,active FROM sr_members WHERE workspace=$1 AND environment=$2 AND id=$3",
          [...scope(p), input.id],
        );
        if (
          existing.rows[0]?.role === "owner" &&
          existing.rows[0].active &&
          (input.role !== "owner" || !input.active)
        ) {
          const others = await c.query(
            "SELECT 1 FROM sr_members WHERE workspace=$1 AND environment=$2 AND role='owner' AND active=true AND id<>$3",
            [...scope(p), input.id],
          );
          if (!others.rowCount) throw new RouterError("last_owner", 409);
        }
        await c.query(
          "INSERT INTO sr_members VALUES($1,$2,$3,$4,$5) ON CONFLICT(workspace,environment,id) DO UPDATE SET role=excluded.role,active=excluded.active",
          [...scope(p), input.id, input.role, input.active],
        );
        await this.audit(c, p, "member.updated", input.id, {
          role: input.role,
          active: input.active,
        });
        return input;
      },
    );
  }
  async customer(p: Principal, raw: unknown, key: string) {
    const input = CustomerInput.parse(raw);
    return this.command(
      p,
      "billing:write",
      "customer",
      key,
      input,
      async (c) => {
        const r = await c.query(
          "SELECT external_id FROM sr_customers WHERE workspace=$1 AND environment=$2 AND id=$3",
          [...scope(p), input.id],
        );
        if (r.rows[0] && r.rows[0].external_id !== input.external_id)
          throw new RouterError("customer_conflict", 409);
        await c.query(
          "INSERT INTO sr_customers VALUES($1,$2,$3,$4) ON CONFLICT(workspace,environment,id) DO NOTHING",
          [...scope(p), input.id, input.external_id],
        );
        await this.audit(c, p, "customer.created", input.id);
        return input;
      },
    );
  }
  async issueKey(p: Principal, raw: unknown, key: string) {
    const input = KeyInput.parse(raw);
    return this.command(
      p,
      "identity:write",
      "key.issue",
      key,
      input,
      async (c) => {
        const member = await c.query(
          "SELECT * FROM sr_members WHERE workspace=$1 AND environment=$2 AND id=$3 AND active=true",
          [...scope(p), input.member_id],
        );
        if (!member.rows[0]) throw new RouterError("resource_not_found", 404);
        if (
          input.scopes.some(
            (s) => !roleScopes[member.rows[0].role as Role].includes(s),
          )
        )
          throw new RouterError("scope_escalation", 403);
        if (member.rows[0].role === "app" && !input.service_ids)
          throw new RouterError("service_scope_required");
        if (input.customer_id !== "_admin")
          await this.requireCustomer(c, p, input.customer_id);
        else if (member.rows[0].role === "app")
          throw new RouterError("customer_required");
        if (input.service_ids)
          await this.requireServices(c, p, input.service_ids);
        const expires = new Date(input.expires_at);
        if (
          expires <= this.clock() ||
          expires.getTime() > this.clock().getTime() + 366 * day
        )
          throw new RouterError("invalid_expiry");
        const result = await this.insertKey(
          c,
          p,
          input.member_id,
          input.customer_id,
          input.scopes,
          input.service_ids,
          expires,
        );
        await this.audit(c, p, "key.issued", result.id, {
          member_id: input.member_id,
          scopes: input.scopes,
        });
        return result;
      },
    );
  }
  async revokeKey(p: Principal, id: string, key: string) {
    return this.command(
      p,
      "identity:write",
      "key.revoke",
      key,
      { id },
      async (c) => {
        const r = await c.query(
          "UPDATE sr_keys SET revoked_at=COALESCE(revoked_at,now()) WHERE workspace=$1 AND environment=$2 AND id=$3 RETURNING id",
          [...scope(p), id],
        );
        if (!r.rowCount) throw new RouterError("resource_not_found", 404);
        await this.audit(c, p, "key.revoked", id);
        return { id, revoked: true };
      },
    );
  }
  async rotateKey(p: Principal, id: string, key: string) {
    return this.command(
      p,
      "identity:write",
      "key.rotate",
      key,
      { id },
      async (c) => {
        const r = await c.query(
          "SELECT * FROM sr_keys WHERE workspace=$1 AND environment=$2 AND id=$3 AND revoked_at IS NULL AND expires_at>now()",
          [...scope(p), id],
        );
        const old = r.rows[0];
        if (!old) throw new RouterError("resource_not_found", 404);
        const result = await this.insertKey(
          c,
          p,
          old.member_id,
          old.customer,
          old.scopes,
          old.service_ids ?? undefined,
          old.expires_at,
        );
        await c.query(
          "UPDATE sr_keys SET revoked_at=now() WHERE workspace=$1 AND environment=$2 AND id=$3",
          [...scope(p), id],
        );
        await this.audit(c, p, "key.rotated", id, {
          replacement_id: result.id,
        });
        return result;
      },
    );
  }
  private async requireCustomer(c: PoolClient, p: Principal, id: string) {
    const r = await c.query(
      "SELECT 1 FROM sr_customers WHERE workspace=$1 AND environment=$2 AND id=$3",
      [...scope(p), id],
    );
    if (!r.rowCount) throw new RouterError("resource_not_found", 404);
  }
  private async requireServices(
    c: PoolClient,
    p: Principal,
    ids: string[],
    asset?: string,
  ) {
    for (const id of ids) {
      if (p.service_ids && !p.service_ids.includes(id))
        throw new RouterError("scope_denied", 403);
      const r = await c.query(
        "SELECT config FROM sr_services WHERE workspace=$1 AND environment=$2 AND id=$3 AND active=true",
        [...scope(p), id],
      );
      if (!r.rows[0]) throw new RouterError("resource_not_found", 404);
      if (asset && r.rows[0].config.price.asset !== asset)
        throw new RouterError("asset_mismatch", 422);
    }
  }
  async publishPlan(p: Principal, raw: unknown, key: string) {
    const input = PlanInput.parse(raw);
    return this.command(
      p,
      "billing:write",
      "plan.publish",
      key,
      input,
      async (c) => {
        await this.requireServices(c, p, input.service_ids, input.asset);
        const exists = await c.query(
          "SELECT config FROM sr_plans WHERE workspace=$1 AND environment=$2 AND id=$3 AND version=$4",
          [...scope(p), input.id, input.version],
        );
        if (exists.rows[0]) {
          if (canonical(exists.rows[0].config) !== canonical(input))
            throw new RouterError("version_exists", 409);
          return input;
        }
        await c.query("INSERT INTO sr_plans VALUES($1,$2,$3,$4,$5)", [
          ...scope(p),
          input.id,
          input.version,
          input,
        ]);
        await this.audit(c, p, "plan.published", input.id, {
          version: input.version,
        });
        return input;
      },
    );
  }
  private async plan(c: PoolClient, p: Principal, id: string, version: number) {
    const r = await c.query(
      "SELECT config FROM sr_plans WHERE workspace=$1 AND environment=$2 AND id=$3 AND version=$4",
      [...scope(p), id, version],
    );
    if (!r.rows[0]) throw new RouterError("resource_not_found", 404);
    return PlanInput.parse(r.rows[0].config);
  }
  async subscribe(p: Principal, raw: unknown, key: string) {
    const input = SubscriptionInput.parse(raw);
    return this.command(
      p,
      "billing:write",
      "subscription.create",
      key,
      input,
      async (c) => {
        await this.requireCustomer(c, p, input.customer_id);
        const plan = await this.plan(c, p, input.plan_id, input.plan_version);
        const id = randomUUID();
        await c.query(
          "INSERT INTO sr_subscriptions(workspace,environment,id,customer,plan_id,plan_version,anchor_at,period_days) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",
          [
            ...scope(p),
            id,
            input.customer_id,
            plan.id,
            plan.version,
            this.clock(),
            plan.period_days,
          ],
        );
        await this.audit(c, p, "subscription.created", id);
        return { id, status: "pending" };
      },
    );
  }
  private async subscription(c: PoolClient, p: Principal, id: string) {
    const r = await c.query(
      "SELECT * FROM sr_subscriptions WHERE workspace=$1 AND environment=$2 AND id=$3",
      [...scope(p), id],
    );
    if (!r.rows[0]) throw new RouterError("resource_not_found", 404);
    return r.rows[0];
  }
  async schedulePlan(p: Principal, id: string, raw: unknown, key: string) {
    const input = PlanChangeInput.parse(raw);
    return this.command(
      p,
      "billing:write",
      "subscription.plan",
      key,
      { id, ...input },
      async (c) => {
        const sub = await this.subscription(c, p, id);
        if (sub.cancel_effective_at)
          throw new RouterError("subscription_cancelled", 409);
        const plan = await this.plan(c, p, input.plan_id, input.plan_version);
        if (plan.period_days !== sub.period_days)
          throw new RouterError("period_change_not_supported", 422);
        const pending = await c.query(
          "SELECT 1 FROM sr_payment_orders WHERE workspace=$1 AND environment=$2 AND subscription=$3 AND cycle>$4",
          [...scope(p), id, sub.paid_cycle],
        );
        if (pending.rowCount)
          throw new RouterError("order_already_issued", 409);
        await c.query(
          "UPDATE sr_subscriptions SET next_plan_id=$4,next_plan_version=$5 WHERE workspace=$1 AND environment=$2 AND id=$3",
          [...scope(p), id, plan.id, plan.version],
        );
        await this.audit(c, p, "subscription.plan_scheduled", id, {
          plan_id: plan.id,
          version: plan.version,
        });
        return { id, next_plan_id: plan.id, next_plan_version: plan.version };
      },
    );
  }
  async cancelSubscription(p: Principal, id: string, key: string) {
    return this.command(
      p,
      "billing:write",
      "subscription.cancel",
      key,
      { id },
      async (c) => {
        const sub = await this.subscription(c, p, id);
        const effective =
          sub.cancel_effective_at ??
          (sub.current_period_end && sub.current_period_end > this.clock()
            ? sub.current_period_end
            : this.clock());
        await c.query(
          "UPDATE sr_subscriptions SET cancel_effective_at=$4,status=CASE WHEN $4::timestamptz<=$5 THEN 'cancelled' ELSE status END WHERE workspace=$1 AND environment=$2 AND id=$3",
          [...scope(p), id, effective, this.clock()],
        );
        await this.audit(c, p, "subscription.cancelled", id, {
          effective_at: effective.toISOString(),
        });
        return { id, cancel_effective_at: effective.toISOString() };
      },
    );
  }
  async createOrder(
    p: Principal,
    id: string,
    integrationId: string,
    key: string,
  ) {
    return this.command(
      p,
      "billing:write",
      "order.create",
      key,
      { id, integrationId },
      async (c) => {
        return this.createOrderTx(c, p, id, integrationId);
      },
    );
  }
  private async createOrderTx(
    c: PoolClient,
    p: Principal,
    id: string,
    integrationId: string,
  ) {
    const integration = this.integrations.find(
      (i) =>
        i.id === integrationId &&
        i.workspace === p.workspace &&
        i.environment === p.environment,
    );
    if (!integration) throw new RouterError("resource_not_found", 404);
    const sub = await this.subscription(c, p, id),
      now = this.clock();
    if (sub.cancel_effective_at && sub.cancel_effective_at <= now)
      throw new RouterError("subscription_cancelled", 409);
    const cycle = Math.floor(
      (now.getTime() - sub.anchor_at.getTime()) / (sub.period_days * day),
    );
    if (cycle < 0 || cycle <= sub.paid_cycle)
      throw new RouterError("period_already_paid", 409);
    const existing = await c.query(
      "SELECT * FROM sr_payment_orders WHERE workspace=$1 AND environment=$2 AND subscription=$3 AND cycle=$4",
      [...scope(p), id, cycle],
    );
    if (existing.rows[0]) {
      if (existing.rows[0].integration !== integrationId)
        throw new RouterError("order_integration_conflict", 409);
      return this.orderView(existing.rows[0]);
    }
    const plan = await this.plan(
      c,
      p,
      sub.next_plan_id ?? sub.plan_id,
      sub.next_plan_version ?? sub.plan_version,
    );
    const start = new Date(
        sub.anchor_at.getTime() + cycle * sub.period_days * day,
      ),
      end = new Date(start.getTime() + sub.period_days * day);
    const order = randomUUID();
    const r = await c.query(
      "INSERT INTO sr_payment_orders(workspace,environment,id,subscription,cycle,integration,amount_minor,currency,plan,period_start,period_end) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) RETURNING *",
      [
        ...scope(p),
        order,
        id,
        cycle,
        integration.id,
        plan.amount_minor,
        plan.currency,
        plan,
        start,
        end,
      ],
    );
    await this.audit(c, p, "payment_order.created", order, {
      subscription: id,
      cycle,
    });
    return this.orderView(r.rows[0]);
  }
  /** Customer use cases: host authorization and customer ownership are checked in the same transaction. */
  private async customerContext(
    c: PoolClient,
    host: Principal,
    customer: Principal,
  ) {
    await revalidate(c, customer, "credits:read");
    if (
      customer.workspace !== host.workspace ||
      customer.environment !== host.environment ||
      customer.customer.startsWith("_") ||
      customer.scopes.some(
        (s) => s.startsWith("billing:") || s === "identity:write",
      )
    )
      throw new RouterError("scope_denied", 403);
    await this.requireCustomer(c, host, customer.customer);
  }
  async customerCheckout(
    host: Principal,
    customer: Principal,
    raw: unknown,
    integration: string,
    key: string,
  ) {
    if (!key || key.length > 160)
      throw new RouterError("missing_idempotency_key");
    const offer = PlanChangeInput.parse(raw);
    return this.command(
      host,
      "billing:write",
      "customer.checkout",
      hash(JSON.stringify([customer.customer, key])),
      { customer: customer.customer, ...offer, integration },
      async (c) => {
        await this.customerContext(c, host, customer);
        const plan = await this.plan(
          c,
          host,
          offer.plan_id,
          offer.plan_version,
        );
        if (
          customer.service_ids &&
          plan.service_ids.some((id) => !customer.service_ids!.includes(id))
        )
          throw new RouterError("scope_denied", 403);
        const existing = await c.query(
          "SELECT * FROM sr_subscriptions WHERE workspace=$1 AND environment=$2 AND customer=$3 AND (cancel_effective_at IS NULL OR cancel_effective_at>$4) ORDER BY anchor_at DESC LIMIT 1",
          [...scope(host), customer.customer, this.clock()],
        );
        let sub = existing.rows[0];
        if (
          sub &&
          (sub.plan_id !== plan.id || sub.plan_version !== plan.version)
        )
          throw new RouterError("use_plan_change", 409);
        if (!sub) {
          const created = await c.query(
            "INSERT INTO sr_subscriptions(workspace,environment,id,customer,plan_id,plan_version,anchor_at,period_days) VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING *",
            [
              ...scope(host),
              randomUUID(),
              customer.customer,
              plan.id,
              plan.version,
              this.clock(),
              plan.period_days,
            ],
          );
          sub = created.rows[0];
          await this.audit(c, host, "subscription.created", sub.id, {
            customer: customer.customer,
          });
        }
        return this.createOrderTx(c, host, sub.id, integration);
      },
      (c) => this.customerContext(c, host, customer),
    );
  }
  async customerOrder(host: Principal, customer: Principal, id: string) {
    return this.tx(
      host,
      async (c) => {
        await this.customerContext(c, host, customer);
        const r = await c.query(
          "SELECT o.* FROM sr_payment_orders o JOIN sr_subscriptions s ON (o.workspace,o.environment,o.subscription)=(s.workspace,s.environment,s.id) WHERE o.workspace=$1 AND o.environment=$2 AND o.id=$3 AND s.customer=$4",
          [...scope(host), id, customer.customer],
        );
        if (!r.rows[0]) throw new RouterError("resource_not_found", 404);
        const o = this.orderView(r.rows[0]);
        return {
          ...o,
          payment_status: o.state,
          fulfillment_status:
            o.state === "succeeded"
              ? "fulfilled"
              : o.state === "review_required"
                ? "review-required"
                : "pending",
        };
      },
      "billing:read",
    );
  }
  async customerBilling(host: Principal, customer: Principal) {
    const data = await this.tx(
      host,
      async (c) => {
        await this.customerContext(c, host, customer);
        const subscriptions = await c.query(
          "SELECT id,plan_id,plan_version,next_plan_id,next_plan_version,status,current_period_end,cancel_effective_at,period_days FROM sr_subscriptions WHERE workspace=$1 AND environment=$2 AND customer=$3 ORDER BY anchor_at DESC",
          [...scope(host), customer.customer],
        );
        const entitlements = await c.query(
          "SELECT subscription,service_ids,period_start,period_end FROM sr_entitlements WHERE workspace=$1 AND environment=$2 AND customer=$3 AND period_start<=$4 AND period_end>$4",
          [...scope(host), customer.customer, this.clock()],
        );
        const orders = await c.query(
          "SELECT o.* FROM sr_payment_orders o JOIN sr_subscriptions s ON (o.workspace,o.environment,o.subscription)=(s.workspace,s.environment,s.id) WHERE o.workspace=$1 AND o.environment=$2 AND s.customer=$3 ORDER BY o.period_start DESC LIMIT 20",
          [...scope(host), customer.customer],
        );
        const usage = await c.query(
          "SELECT r.id,r.service->>'id' AS service,r.execution,r.billing,r.created_at,COALESCE((SELECT SUM(s.amount)::text FROM sr_settlements s WHERE (s.workspace,s.environment,s.request)=(r.workspace,r.environment,r.id) AND s.kind='customer_sale'),'0') AS charged FROM sr_requests r WHERE r.workspace=$1 AND r.environment=$2 AND r.customer=$3 ORDER BY r.created_at DESC LIMIT 20",
          [...scope(host), customer.customer],
        );
        return {
          subscriptions: subscriptions.rows.map((s) => ({
            ...s,
            status:
              s.cancel_effective_at && s.cancel_effective_at <= this.clock()
                ? "cancelled"
                : s.current_period_end && s.current_period_end <= this.clock()
                  ? "past_due"
                  : s.status,
          })),
          entitlements: entitlements.rows,
          orders: orders.rows.map((o) => this.orderView(o)),
          usage: usage.rows,
        };
      },
      "billing:read",
    );
    return {
      ...data,
      buckets: await this.buckets(customer),
      windows: await this.windows(customer),
      as_of: this.clock().toISOString(),
    };
  }
  async customerCancel(
    host: Principal,
    customer: Principal,
    id: string,
    key: string,
  ) {
    if (!key || key.length > 160)
      throw new RouterError("missing_idempotency_key");
    return this.command(
      host,
      "billing:write",
      "customer.cancel",
      hash(JSON.stringify([customer.customer, key])),
      { id, customer: customer.customer },
      async (c) => {
        await this.customerContext(c, host, customer);
        const sub = await this.subscription(c, host, id);
        if (sub.customer !== customer.customer)
          throw new RouterError("resource_not_found", 404);
        const effective =
          sub.cancel_effective_at ??
          (sub.current_period_end && sub.current_period_end > this.clock()
            ? sub.current_period_end
            : this.clock());
        await c.query(
          "UPDATE sr_subscriptions SET cancel_effective_at=$4,status=CASE WHEN $4::timestamptz<=$5 THEN 'cancelled' ELSE status END WHERE workspace=$1 AND environment=$2 AND id=$3",
          [...scope(host), id, effective, this.clock()],
        );
        await this.audit(c, host, "subscription.cancelled", id, {
          customer: customer.customer,
          effective_at: effective.toISOString(),
        });
        return { id, cancel_effective_at: effective.toISOString() };
      },
      (c) => this.customerContext(c, host, customer),
    );
  }
  private orderView(o: any) {
    return {
      id: o.id,
      subscription_id: o.subscription,
      cycle: o.cycle,
      integration: o.integration,
      amount_minor: o.amount_minor,
      currency: o.currency,
      state: o.state,
      period_start: o.period_start.toISOString(),
      period_end: o.period_end.toISOString(),
    };
  }
  /** Signed sandbox payment ingress. No bearer key can assert a payment succeeded. */
  async payment(
    integrationId: string,
    raw: Buffer,
    timestamp: string,
    signature: string,
  ) {
    const integration = this.integrations.find((i) => i.id === integrationId);
    if (!integration || integration.mode !== "sandbox")
      throw new RouterError("resource_not_found", 404);
    if (
      !/^\d{10}$/.test(timestamp) ||
      Math.abs(this.clock().getTime() / 1000 - Number(timestamp)) > 300 ||
      !/^v1=[a-f0-9]{64}$/.test(signature)
    )
      throw new RouterError("invalid_signature", 401);
    const expected = createHmac("sha256", integration.secret)
      .update(timestamp + ".")
      .update(raw)
      .digest();
    if (!timingSafeEqual(expected, Buffer.from(signature.slice(3), "hex")))
      throw new RouterError("invalid_signature", 401);
    let json: unknown;
    try {
      json = JSON.parse(raw.toString());
    } catch {
      throw new RouterError("invalid_json");
    }
    return this.settle(integration, PaymentInput.parse(json));
  }
  private async settle(
    integration: PaymentIntegration,
    event: ReturnType<typeof PaymentInput.parse>,
    receipt?: Receipt,
  ) {
    const p: Principal = {
      workspace: integration.workspace,
      environment: integration.environment,
      customer: "_system",
      member_id: "payment:" + integration.id,
      scopes: [],
    };
    return this.tx(p, async (c) => {
      await c.query(
        "INSERT INTO sr_integration_bindings VALUES($1,$2,$3) ON CONFLICT DO NOTHING",
        [integration.id, ...scope(p)],
      );
      const binding = await c.query(
        "SELECT 1 FROM sr_integration_bindings WHERE id=$1 AND workspace=$2 AND environment=$3",
        [integration.id, ...scope(p)],
      );
      if (!binding.rowCount)
        throw new RouterError("integration_binding_conflict", 409);
      const digest = hash(canonical(event));
      const prior = await c.query(
        "SELECT * FROM sr_payment_events WHERE integration=$1 AND event_id=$2",
        [integration.id, event.id],
      );
      if (prior.rows[0]) {
        if (prior.rows[0].digest !== digest)
          throw new RouterError("idempotency_conflict", 409);
        return { accepted: true, outcome: prior.rows[0].outcome };
      }
      const r = await c.query(
        "SELECT * FROM sr_payment_orders WHERE workspace=$1 AND environment=$2 AND id=$3 AND integration=$4",
        [...scope(p), event.order_id, integration.id],
      );
      const order = r.rows[0];
      if (!order) throw new RouterError("resource_not_found", 404);
      if (
        order.amount_minor !== event.amount_minor ||
        order.currency !== event.currency
      )
        throw new RouterError("payment_amount_mismatch", 422);
      if (receipt) {
        const checkout = await c.query(
          "SELECT reference,identity FROM sr_checkouts WHERE workspace=$1 AND environment=$2 AND order_id=$3",
          [...scope(p), order.id],
        );
        if (
          integration.mode !== "hosted" ||
          checkout.rows[0]?.reference !== receipt.reference ||
          checkout.rows[0]?.identity !== integration.adapter.identity
        )
          throw new RouterError("payment_reference_mismatch", 409);
      }
      const sub = await this.subscription(c, p, order.subscription);
      const now = this.clock();
      let outcome = "ignored";
      if (
        order.state !== "succeeded" &&
        order.state !== "review_required" &&
        event.revision > order.revision
      ) {
        if (event.status === "failed") {
          outcome = "failed";
          await c.query(
            "UPDATE sr_payment_orders SET state='failed',revision=$4 WHERE workspace=$1 AND environment=$2 AND id=$3",
            [...scope(p), order.id, event.revision],
          );
          if (!sub.current_period_end || sub.current_period_end <= now)
            await c.query(
              "UPDATE sr_subscriptions SET status=CASE WHEN cancel_effective_at<=$4 THEN 'cancelled' ELSE 'past_due' END WHERE workspace=$1 AND environment=$2 AND id=$3",
              [...scope(p), sub.id, now],
            );
        } else if (
          order.period_end <= now ||
          sub.paid_cycle >= order.cycle ||
          (sub.cancel_effective_at && sub.cancel_effective_at <= now)
        ) {
          outcome = "review_required";
          await c.query(
            "UPDATE sr_payment_orders SET state='review_required',revision=$4 WHERE workspace=$1 AND environment=$2 AND id=$3",
            [...scope(p), order.id, event.revision],
          );
        } else {
          const plan = PlanInput.parse(order.plan);
          p.customer = sub.customer;
          await this.store.grantTx(
            c,
            p,
            "customer_sale",
            plan.asset,
            plan.credits,
            "subscription:" + sub.id + ":" + order.cycle,
            order.period_end,
            plan.service_ids,
            "subscription",
          );
          const bucket = await c.query(
            "SELECT id FROM sr_buckets WHERE workspace=$1 AND environment=$2 AND source=$3",
            [...scope(p), "subscription:" + sub.id + ":" + order.cycle],
          );
          await c.query(
            "INSERT INTO sr_entitlements VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
            [
              ...scope(p),
              sub.id,
              order.cycle,
              sub.customer,
              plan.service_ids,
              order.period_start,
              order.period_end,
              bucket.rows[0].id,
              // Snapshot of the paid cycle's window limits; null when the option is off.
              plan.window_limits?.enabled ? plan.window_limits : null,
            ],
          );
          await c.query(
            "UPDATE sr_payment_orders SET state='succeeded',revision=$4 WHERE workspace=$1 AND environment=$2 AND id=$3",
            [...scope(p), order.id, event.revision],
          );
          await c.query(
            "UPDATE sr_subscriptions SET status='active',paid_cycle=$4,current_period_end=$5,plan_id=$6,plan_version=$7,next_plan_id=NULL,next_plan_version=NULL WHERE workspace=$1 AND environment=$2 AND id=$3",
            [
              ...scope(p),
              sub.id,
              order.cycle,
              order.period_end,
              plan.id,
              plan.version,
            ],
          );
          outcome = "granted";
        }
      }
      await c.query(
        "INSERT INTO sr_payment_events(integration,event_id,workspace,environment,order_id,digest,outcome) VALUES($1,$2,$3,$4,$5,$6,$7)",
        [integration.id, event.id, ...scope(p), order.id, digest, outcome],
      );
      await this.audit(c, p, "payment." + outcome, order.id, {
        event_id: event.id,
        cycle: order.cycle,
      });
      return { accepted: true, outcome };
    });
  }
  /** Reserve dispatch before external I/O. Unknown outcomes never automatically create another checkout. */
  async checkout(p: Principal, orderId: string) {
    const prepared = await this.tx(
      p,
      async (c) => {
        const r = await c.query(
          "SELECT * FROM sr_payment_orders WHERE workspace=$1 AND environment=$2 AND id=$3",
          [...scope(p), orderId],
        );
        const order = r.rows[0];
        if (!order) throw new RouterError("resource_not_found", 404);
        const integration = this.integrations.find(
          (i) =>
            i.id === order.integration &&
            i.workspace === p.workspace &&
            i.environment === p.environment,
        );
        if (!integration || integration.mode !== "hosted")
          throw new RouterError("hosted_payment_required", 422);
        if (order.state !== "pending" || order.period_end <= this.clock())
          throw new RouterError("order_not_payable", 409);
        await c.query(
          "INSERT INTO sr_integration_bindings VALUES($1,$2,$3) ON CONFLICT DO NOTHING",
          [integration.id, ...scope(p)],
        );
        const binding = await c.query(
          "SELECT 1 FROM sr_integration_bindings WHERE id=$1 AND workspace=$2 AND environment=$3",
          [integration.id, ...scope(p)],
        );
        if (!binding.rowCount)
          throw new RouterError("integration_binding_conflict", 409);
        const old = await c.query(
          "SELECT * FROM sr_checkouts WHERE workspace=$1 AND environment=$2 AND order_id=$3",
          [...scope(p), orderId],
        );
        if (old.rows[0]) {
          if (old.rows[0].identity !== integration.adapter.identity)
            throw new RouterError("integration_binding_conflict", 409);
          if (old.rows[0].state !== "ready")
            throw new RouterError("checkout_reconciliation_required", 409);
          return {
            order,
            integration,
            existing: {
              reference: old.rows[0].reference as string,
              url: old.rows[0].url as string,
            },
          };
        }
        await c.query(
          "INSERT INTO sr_checkouts(workspace,environment,order_id,integration,identity,state) VALUES($1,$2,$3,$4,$5,'creating')",
          [...scope(p), orderId, integration.id, integration.adapter.identity],
        );
        return { order, integration, existing: null };
      },
      "billing:write",
    );
    if (prepared.existing) return prepared.existing;
    try {
      const result = await prepared.integration.adapter.create(prepared.order);
      await this.tx(p, async (c) => {
        const saved = await c.query(
          "UPDATE sr_checkouts SET state='ready',reference=$4,url=$5 WHERE workspace=$1 AND environment=$2 AND order_id=$3 AND (reference IS NULL OR reference=$4)",
          [...scope(p), orderId, result.reference, result.url],
        );
        if (!saved.rowCount)
          throw new RouterError("payment_reference_mismatch", 409);
        await this.audit(c, p, "checkout.created", orderId);
      });
      return result;
    } catch {
      await this.tx(p, async (c) => {
        await c.query(
          "UPDATE sr_checkouts SET state='unknown' WHERE workspace=$1 AND environment=$2 AND order_id=$3 AND state='creating'",
          [...scope(p), orderId],
        );
      });
      throw new RouterError("checkout_reconciliation_required", 502);
    }
  }
  async hostedNotification(
    integrationId: string,
    raw: Buffer,
    headers: Record<string, string>,
  ) {
    const i = this.integrations.find((x) => x.id === integrationId);
    if (!i || i.mode !== "hosted")
      throw new RouterError("resource_not_found", 404);
    let reference: string;
    try {
      reference = i.adapter.notification(raw, headers);
    } catch (e) {
      if (e instanceof RouterError) throw e;
      throw new RouterError("invalid_notification");
    }
    if (reference) await this.confirmHosted(i, reference);
    return i.adapter.acknowledgement;
  }
  private async confirmHosted(i: HostedPaymentIntegration, reference: string) {
    // Restrict authenticated lookup to our persisted checkout references.
    const r = await this.store.pool.query(
      "SELECT order_id,identity FROM sr_checkouts WHERE workspace=$1 AND environment=$2 AND integration=$3 AND reference=$4",
      [i.workspace, i.environment, i.id, reference],
    );
    if (!r.rows[0] || r.rows[0].identity !== i.adapter.identity)
      throw new RouterError("checkout_reconciliation_required", 409);
    const receipt = await i.adapter.lookup(reference);
    if (!receipt) return { accepted: true, outcome: "pending" };
    if (receipt.order_id !== r.rows[0].order_id)
      throw new RouterError("payment_reference_mismatch", 409);
    return this.settle(
      i,
      PaymentInput.parse({
        id: "paid:" + reference,
        order_id: receipt.order_id,
        amount_minor: receipt.amount_minor,
        currency: receipt.currency,
        status: "succeeded",
        revision: 1,
      }),
      receipt,
    );
  }
  async reconcilePayment(
    p: Principal,
    orderId: string,
    recoveryReference?: string,
  ) {
    if (
      recoveryReference !== undefined &&
      (typeof recoveryReference !== "string" ||
        recoveryReference.length < 1 ||
        recoveryReference.length > 255)
    )
      throw new RouterError("invalid_reference");
    const row = await this.tx(
      p,
      async (c) => {
        const r = await c.query(
          "SELECT * FROM sr_checkouts WHERE workspace=$1 AND environment=$2 AND order_id=$3",
          [...scope(p), orderId],
        );
        return r.rows[0];
      },
      "billing:write",
    );
    if (!row) throw new RouterError("resource_not_found", 404);
    const i = this.integrations.find(
      (x) =>
        x.id === row.integration &&
        x.workspace === p.workspace &&
        x.environment === p.environment,
    );
    if (!i || i.mode !== "hosted")
      throw new RouterError("resource_not_found", 404);
    if (!row.reference && recoveryReference) {
      // Recovery only attaches a provider-confirmed, matching paid transaction.
      const receipt = await i.adapter.lookup(recoveryReference);
      if (
        !receipt ||
        receipt.order_id !== orderId ||
        receipt.reference !== recoveryReference
      )
        throw new RouterError("payment_reference_mismatch", 409);
      await this.tx(
        p,
        async (c) => {
          const r = await c.query(
            "SELECT * FROM sr_payment_orders WHERE workspace=$1 AND environment=$2 AND id=$3",
            [...scope(p), orderId],
          );
          if (
            r.rows[0]?.amount_minor !== receipt.amount_minor ||
            r.rows[0]?.currency !== receipt.currency ||
            row.identity !== i.adapter.identity
          )
            throw new RouterError("payment_amount_mismatch", 422);
          const updated = await c.query(
            "UPDATE sr_checkouts SET reference=$4 WHERE workspace=$1 AND environment=$2 AND order_id=$3 AND reference IS NULL",
            [...scope(p), orderId, recoveryReference],
          );
          if (!updated.rowCount)
            throw new RouterError("checkout_reconciliation_required", 409);
          await this.audit(c, p, "checkout.reference_recovered", orderId);
        },
        "billing:write",
      );
      row.reference = recoveryReference;
    }
    if (!row.reference)
      throw new RouterError("checkout_reconciliation_required", 409);
    return this.confirmHosted(i, row.reference);
  }
  async grantCredits(p: Principal, raw: unknown, key: string) {
    const input = CreditGrantInput.parse(raw);
    return this.command(
      p,
      "billing:write",
      "credits.grant",
      key,
      input,
      async (c) => {
        await this.requireCustomer(c, p, input.customer_id);
        if (input.service_ids)
          await this.requireServices(c, p, input.service_ids, input.asset);
        if (input.expires_at && new Date(input.expires_at) <= this.clock())
          throw new RouterError("invalid_expiry");
        const target = { ...p, customer: input.customer_id };
        const account = await this.store.grantTx(
          c,
          target,
          "customer_sale",
          input.asset,
          input.amount,
          "manual:" + key,
          input.expires_at ? new Date(input.expires_at) : undefined,
          input.service_ids,
          input.source_kind,
        );
        await this.audit(c, p, "credits.granted", account, {
          customer: input.customer_id,
          amount: input.amount,
          reason: input.reason,
        });
        return { account_id: account, granted: input.amount };
      },
    );
  }
  async getSubscription(p: Principal, id: string) {
    return this.tx(
      p,
      async (c) => {
        const sub = await this.subscription(c, p, id);
        return {
          ...sub,
          status:
            sub.cancel_effective_at && sub.cancel_effective_at <= this.clock()
              ? "cancelled"
              : sub.current_period_end && sub.current_period_end <= this.clock()
                ? "past_due"
                : sub.status,
        };
      },
      "billing:read",
    );
  }
  async customerBalance(p: Principal, customer: string) {
    await this.tx(
      p,
      (c) => this.requireCustomer(c, p, customer),
      "billing:read",
    );
    return this.store.balance({ ...p, customer });
  }
  async buckets(p: Principal, customer = p.customer) {
    return this.tx(
      p,
      async (c) => {
        if (customer !== p.customer && !p.scopes.includes("billing:read"))
          throw new RouterError("scope_denied", 403);
        const rows = await c.query(
          "SELECT b.id,b.source_kind,b.service_ids,b.expires_at,b.remaining::text,b.held::text,a.asset FROM sr_buckets b JOIN sr_accounts a ON (b.workspace,b.environment,b.account)=(a.workspace,a.environment,a.id) WHERE b.workspace=$1 AND b.environment=$2 AND a.owner=$3 ORDER BY b.expires_at NULLS LAST,b.id",
          [...scope(p), customer],
        );
        return rows.rows.map((b) => ({
          ...b,
          available:
            b.expires_at && b.expires_at <= this.clock()
              ? "0"
              : (BigInt(b.remaining) - BigInt(b.held)).toString(),
        }));
      },
      p.scopes.includes("billing:read") ? "billing:read" : "credits:read",
    );
  }
  /**
   * Session/weekly window usage of the customer's paid subscriptions that have the option on.
   * A window that has not opened yet reports nothing spent and no reset time: it starts with the
   * next request. Times and amounts come from the database clock the enforcement uses.
   */
  async windows(p: Principal, customer = p.customer) {
    return this.tx(
      p,
      async (c) => {
        if (customer !== p.customer && !p.scopes.includes("billing:read"))
          throw new RouterError("scope_denied", 403);
        const entitlements = await c.query(
          "SELECT subscription,window_limits FROM sr_entitlements WHERE workspace=$1 AND environment=$2 AND customer=$3 AND window_limits IS NOT NULL AND period_start<=now() AND period_end>now() ORDER BY subscription",
          [...scope(p), customer],
        );
        const out = [];
        for (const e of entitlements.rows)
          for (const spec of windowSpecs(e.window_limits)) {
            const w = await c.query(
              "SELECT window_start,window_end,spent::text,held::text FROM sr_usage_windows WHERE workspace=$1 AND environment=$2 AND subscription=$3 AND kind=$4 AND window_end>now() ORDER BY window_start DESC LIMIT 1",
              [...scope(p), e.subscription, spec.kind],
            );
            const row = w.rows[0];
            const used = row ? BigInt(row.spent) + BigInt(row.held) : 0n;
            out.push({
              subscription: e.subscription as string,
              window: spec.kind,
              hours: spec.hours,
              limit: spec.limit.toString(),
              spent: (row?.spent ?? "0") as string,
              held: (row?.held ?? "0") as string,
              remaining: (spec.limit > used
                ? spec.limit - used
                : 0n
              ).toString(),
              window_start: (row?.window_start ?? null) as Date | null,
              resets_at: (row?.window_end ?? null) as Date | null,
            });
          }
        return out;
      },
      p.scopes.includes("billing:read") ? "billing:read" : "credits:read",
    );
  }
  async auditLog(p: Principal, limit = 50, cursor?: string) {
    if (!Number.isInteger(limit) || limit < 1 || limit > 100)
      throw new RouterError("invalid_limit");
    return this.tx(
      p,
      async (c) => {
        let after: unknown[] = [];
        if (cursor) {
          try {
            after = JSON.parse(Buffer.from(cursor, "base64url").toString());
            if (
              after.length !== 2 ||
              typeof after[0] !== "string" ||
              typeof after[1] !== "string" ||
              !Number.isFinite(Date.parse(after[0]))
            )
              throw new Error();
          } catch {
            throw new RouterError("invalid_cursor");
          }
        }
        const rows = await c.query(
          `SELECT id,actor,action,resource,details,created_at,to_char(created_at AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS cursor_at FROM sr_audit WHERE workspace=$1 AND environment=$2 ${cursor ? "AND (created_at,id)>($4::timestamptz,$5)" : ""} ORDER BY created_at,id LIMIT $3`,
          [...scope(p), limit + 1, ...after],
        );
        const page = rows.rows.slice(0, limit),
          last = page.at(-1),
          data = page.map(({ cursor_at, ...entry }) => entry);
        return {
          data,
          next_cursor:
            rows.rows.length > limit
              ? Buffer.from(JSON.stringify([last.cursor_at, last.id])).toString(
                  "base64url",
                )
              : null,
        };
      },
      "operations:read",
    );
  }
  async advancePeriods() {
    const r = await this.store.pool.query(
      "SELECT DISTINCT workspace,environment FROM sr_subscriptions WHERE status IN ('active','pending','past_due')",
    );
    for (const t of r.rows) {
      const p: Principal = { ...t, customer: "_system", scopes: [] };
      await this.tx(p, async (c) => {
        await c.query(
          "UPDATE sr_subscriptions SET status=CASE WHEN cancel_effective_at<=$3 THEN 'cancelled' ELSE 'past_due' END WHERE workspace=$1 AND environment=$2 AND ((cancel_effective_at<=$3) OR (current_period_end<=$3 AND status='active'))",
          [...scope(p), this.clock()],
        );
      });
    }
    return r.rowCount;
  }
}
