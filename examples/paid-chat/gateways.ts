/** Local protocol fixtures exercise the published adapters; no external requests or charges. */
import { createHash, createHmac } from "node:crypto";
import {
  StripePayments,
  OenPayments,
  EpayV1Payments,
  epaySign,
  type PaymentAdapter,
  type PaymentOrder,
} from "@supersolutions/super-router-payments";
import { RouterError } from "@supersolutions/super-router-contracts";
import { LocalSandboxPayments } from "./sandbox.js";
export type GatewayId = "sandbox" | "stripe" | "oen" | "epay";
export interface Gateway {
  id: GatewayId;
  name: string;
  mode: "sandbox" | "fixture" | "test" | "live";
  integration: string;
  currencies: string[];
  allowedOrigins: string[];
  adapter: PaymentAdapter;
  fixture?: ProtocolFixture;
}
export class ProtocolFixture {
  readonly capabilities = {
    hosted_checkout: true,
    automatic_recurring: false,
    refunds: false,
    portal: false,
  } as const;
  readonly acknowledgement: string;
  readonly identity: string;
  private inner: PaymentAdapter;
  private local: LocalSandboxPayments;
  private orders = new Map<string, PaymentOrder>();
  private paid = new Map<string, PaymentOrder>();
  readonly requests: { method: string; path: string }[] = [];
  constructor(
    readonly method: Exclude<GatewayId, "sandbox">,
    readonly origin: string,
    private secret: string,
  ) {
    this.local = new LocalSandboxPayments(origin, secret + ":" + method);
    const fetcher = (async (input, init) =>
      this.respond(String(input), init ?? {})) as typeof fetch;
    this.inner =
      method === "stripe"
        ? new StripePayments(
            {
              secretKey: "fixture-only",
              webhookSecret: secret,
              accountId: "acct_fixture",
              live: false,
              successUrl: "https://return.fixture.invalid/",
              cancelUrl: "https://return.fixture.invalid/",
            },
            fetcher,
          )
        : method === "oen"
          ? new OenPayments(
              {
                token: "fixture-only",
                webhookSecret: secret,
                merchantId: "fixture",
                live: false,
                successUrl: "https://return.fixture.invalid/",
                failureUrl: "https://return.fixture.invalid/",
              },
              fetcher,
            )
          : new EpayV1Payments(
              {
                endpoint: "https://epay.fixture.invalid/",
                pid: "1001",
                key: secret,
                notifyUrl: "https://notify.fixture.invalid/",
                returnUrl: "https://return.fixture.invalid/",
              },
              fetcher,
            );
    this.identity = "local-protocol-fixture:" + this.inner.identity;
    this.acknowledgement = this.inner.acknowledgement;
  }
  private reference(id: string) {
    return this.method === "stripe"
      ? "cs_fixture_" +
          createHash("sha256").update(id).digest("hex").slice(0, 24)
      : this.method === "oen"
        ? createHash("sha256").update(id).digest("hex").slice(0, 27)
        : id;
  }
  private respond(raw: string, init: RequestInit) {
    const url = new URL(raw);
    this.requests.push({ method: init.method ?? "GET", path: url.pathname });
    let result: unknown;
    if (this.method === "stripe" && url.pathname === "/v1/account")
      result = { id: "acct_fixture" };
    else if (this.method === "stripe" && init.method === "POST") {
      const d = new URLSearchParams(String(init.body));
      const id = d.get("client_reference_id")!;
      const o = this.orders.get(id);
      if (
        !o ||
        d.get("mode") !== "payment" ||
        d.get("line_items[0][quantity]") !== "1" ||
        d.get("line_items[0][price_data][unit_amount]") !== o.amount_minor ||
        d.get("line_items[0][price_data][currency]") !==
          o.currency.toLowerCase()
      )
        throw new RouterError("fixture_protocol_mismatch", 422);
      result = {
        id: this.reference(id),
        url: "https://checkout.stripe.com/c/pay/" + this.reference(id),
        livemode: false,
      };
    } else if (this.method === "oen" && init.method === "POST") {
      const d = JSON.parse(String(init.body));
      const o = this.orders.get(d.orderId);
      if (
        !o ||
        o.currency !== "TWD" ||
        d.currency !== "TWD" ||
        !Number.isSafeInteger(d.amount) ||
        BigInt(d.amount) * 100n !== BigInt(o.amount_minor) ||
        d.productDetails?.length !== 1 ||
        d.productDetails[0].unitPrice !== d.amount ||
        d.productDetails[0].quantity !== 1
      )
        throw new RouterError("fixture_protocol_mismatch", 422);
      result = { code: "S0000", data: { id: this.reference(d.orderId) } };
    } else {
      const ref =
        this.method === "epay"
          ? url.searchParams.get("out_trade_no")!
          : url.pathname.split("/").pop()!;
      const order = [...this.orders.values()].find(
        (o) => this.reference(o.id) === ref,
      );
      if (!order) throw new RouterError("fixture_order_unknown", 404);
      const paid = this.paid.get(order.id),
        o = paid ?? order;
      if (this.method === "stripe")
        result = {
          id: ref,
          livemode: false,
          payment_status: paid ? "paid" : "unpaid",
          client_reference_id: o.id,
          amount_total: Number(o.amount_minor),
          currency: o.currency.toLowerCase(),
        };
      else if (this.method === "oen")
        result = {
          code: "S0000",
          data: {
            transactionId: ref,
            orderId: o.id,
            status: paid ? "charged" : "created",
            amount: Number(BigInt(o.amount_minor) / 100n),
            refundAmount: 0,
          },
        };
      else
        result = {
          code: 1,
          pid: 1001,
          out_trade_no: o.id,
          status: paid ? 1 : 0,
          money: minor(o.amount_minor),
        };
    }
    return new Response(JSON.stringify(result), {
      headers: { "Content-Type": "application/json" },
    });
  }
  async create(order: PaymentOrder) {
    const safe = {
      id: order.id,
      amount_minor: order.amount_minor,
      currency: order.currency,
    };
    this.orders.set(safe.id, safe);
    const created = await this.inner.create(safe);
    if (this.method === "epay") {
      const url = new URL(created.url),
        d: Record<string, string> = {};
      for (const [k, v] of url.searchParams) {
        if (k in d) throw new RouterError("fixture_protocol_mismatch", 422);
        d[k] = v;
      }
      if (
        url.origin !== "https://epay.fixture.invalid" ||
        url.pathname !== "/submit.php" ||
        d.pid !== "1001" ||
        d.out_trade_no !== safe.id ||
        d.money !== minor(safe.amount_minor) ||
        d.sign_type !== "MD5" ||
        d.sign !== epaySign(d, this.secret) ||
        d.notify_url !== "https://notify.fixture.invalid/" ||
        d.return_url !== "https://return.fixture.invalid/"
      )
        throw new RouterError("fixture_protocol_mismatch", 422);
    }
    const local = await this.local.create(safe);
    return {
      reference: created.reference,
      url: local.url + "&provider=" + this.method,
    };
  }
  ticket(ticket: string) {
    return this.local.ticket(ticket);
  }
  /** Test/process-only helper, never exposed as an HTTP mutation or customer override. */
  markPaid(
    orderId: string,
    overrides: Partial<Pick<PaymentOrder, "amount_minor" | "currency">> = {},
  ) {
    const o = this.orders.get(orderId);
    if (!o) throw new RouterError("fixture_order_unknown", 404);
    this.paid.set(orderId, { ...o, ...overrides });
  }
  complete(ticket: string) {
    const o = this.ticket(ticket);
    this.orders.set(o.id, o);
    this.markPaid(o.id);
    return this.notification(o.id);
  }
  notification(
    orderId: string,
    overrides: {
      amount_minor?: string;
      currency?: string;
      badSignature?: boolean;
    } = {},
  ) {
    const order = this.orders.get(orderId);
    if (!order) throw new RouterError("fixture_order_unknown", 404);
    const reference = this.reference(orderId),
      o = { ...order, ...overrides };
    let raw: Buffer, headers: Record<string, string>;
    if (this.method === "epay") {
      const d: Record<string, string> = {
        pid: "1001",
        out_trade_no: orderId,
        trade_status: "TRADE_SUCCESS",
        money: minor(o.amount_minor),
        sign_type: "MD5",
      };
      d.sign = epaySign(d, this.secret);
      if (overrides.badSignature) d.sign = "0".repeat(32);
      raw = Buffer.from(new URLSearchParams(d).toString());
      headers = { "content-type": "application/x-www-form-urlencoded" };
    } else {
      raw = Buffer.from(
        JSON.stringify(
          this.method === "stripe"
            ? {
                livemode: false,
                type: "checkout.session.completed",
                data: { object: { id: reference } },
              }
            : { merchantId: "fixture", purpose: "charge", id: reference },
        ),
      );
      const t = Math.floor(Date.now() / 1000);
      const sig = createHmac("sha256", this.secret)
        .update(t + ".")
        .update(raw)
        .digest("hex");
      headers = {
        [this.method === "stripe" ? "stripe-signature" : "oenpay-signature"]:
          `t=${t},v1=${overrides.badSignature ? "0".repeat(64) : sig}`,
        "content-type": "application/json",
      };
    }
    return { raw, headers };
  }
  get adapter(): PaymentAdapter {
    return {
      identity: this.identity,
      capabilities: this.capabilities,
      acknowledgement: this.acknowledgement,
      create: (o) => this.create(o),
      notification: (raw, headers) => this.inner.notification(raw, headers),
      lookup: (reference) => this.lookup(reference),
    };
  }

  async lookup(reference: string) {
    return this.inner.lookup(reference);
  }
}
function minor(value: string) {
  const n = BigInt(value);
  return `${n / 100n}.${String(n % 100n).padStart(2, "0")}`;
}
export function buildGateways(config: {
  mode: "fixture" | "merchant";
  checkoutOrigin: string;
  workspace: string;
  secret: string;
  publicOrigin?: string;
  env?: NodeJS.ProcessEnv;
}): Record<string, Gateway> {
  const env = config.env ?? process.env,
    result: Record<string, Gateway> = {};
  const add = (
    id: GatewayId,
    name: string,
    mode: Gateway["mode"],
    adapter: PaymentAdapter,
    currencies: string[],
    allowedOrigins: string[],
    fixture?: ProtocolFixture,
  ) => {
    result[id] = {
      id,
      name,
      mode,
      adapter,
      currencies,
      allowedOrigins,
      integration:
        (id === "sandbox" ? "paid-local-" : "paid-" + mode + "-" + id + "-") +
        config.workspace,
      ...(fixture ? { fixture } : {}),
    };
  };
  if (config.mode === "fixture") {
    add(
      "sandbox",
      "本機沙盒",
      "sandbox",
      new LocalSandboxPayments(config.checkoutOrigin, config.secret),
      ["TWD"],
      [config.checkoutOrigin],
    );
    for (const [id, name, currencies] of [
      ["stripe", "Stripe", ["TWD"]],
      ["oen", "應援金流", ["TWD"]],
      ["epay", "易支付 V1", ["CNY"]],
    ] as const) {
      const fixture = new ProtocolFixture(
        id,
        config.checkoutOrigin,
        config.secret,
      );
      add(
        id,
        name,
        "fixture",
        fixture.adapter,
        [...currencies],
        [config.checkoutOrigin],
        fixture,
      );
    }
    return result;
  }
  const publicURL = new URL(config.publicOrigin ?? "");
  if (
    publicURL.protocol !== "https:" ||
    publicURL.username ||
    publicURL.password ||
    publicURL.search ||
    publicURL.hash ||
    publicURL.pathname !== "/"
  )
    throw new Error("Merchant mode requires HTTPS PAID_PUBLIC_ORIGIN origin");
  const back = publicURL.origin + "/?payment_return=1";
  const required = (name: string) => {
    if (!env[name]) throw new Error("Missing merchant configuration: " + name);
    return env[name]!;
  };
  for (const id of ["stripe", "oen", "epay"] as const) {
    const mode = env["PAID_" + id.toUpperCase() + "_MODE"] ?? "off";
    if (mode === "off") continue;
    if (mode !== "test" && mode !== "live")
      throw new Error("Invalid payment mode");
    if (id === "stripe")
      add(
        id,
        "Stripe",
        mode,
        new StripePayments({
          secretKey: required("PAID_STRIPE_SECRET_KEY"),
          webhookSecret: required("PAID_STRIPE_WEBHOOK_SECRET"),
          accountId: required("PAID_STRIPE_ACCOUNT_ID"),
          live: mode === "live",
          successUrl: back,
          cancelUrl: back,
        }),
        ["TWD"],
        ["https://checkout.stripe.com"],
      );
    else if (id === "oen") {
      const merchantId = required("PAID_OEN_MERCHANT_ID");
      add(
        id,
        "應援金流",
        mode,
        new OenPayments({
          token: required("PAID_OEN_TOKEN"),
          webhookSecret: required("PAID_OEN_WEBHOOK_SECRET"),
          merchantId,
          live: mode === "live",
          successUrl: back,
          failureUrl: back,
        }),
        ["TWD"],
        [`https://${merchantId}${mode === "live" ? "" : ".testing"}.oen.tw`],
      );
    } else {
      const endpoint = required("PAID_EPAY_ENDPOINT");
      add(
        id,
        "易支付 V1",
        mode,
        new EpayV1Payments({
          endpoint,
          pid: required("PAID_EPAY_PID"),
          key: required("PAID_EPAY_KEY"),
          notifyUrl: publicURL.origin + "/api/payments/epay/notify",
          returnUrl: back,
        }),
        ["CNY"],
        [new URL(endpoint).origin],
      );
    }
  }
  if (new Set(Object.values(result).map((g) => g.mode)).size > 1)
    throw new Error(
      "Test and live merchants require separate workspaces/environments",
    );
  if (!Object.keys(result).length)
    throw new Error("Merchant mode requires an explicitly enabled gateway");
  return result;
}
