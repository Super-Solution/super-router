import { createHash, createHmac, timingSafeEqual } from "node:crypto";
import { RouterError } from "@supersolutions/super-router-contracts";
export interface PaymentOrder {
  id: string;
  amount_minor: string;
  currency: string;
}
export interface Checkout {
  reference: string;
  url: string;
}
export interface Receipt {
  reference: string;
  order_id: string;
  amount_minor: string;
  currency: string;
}
export interface PaymentAdapter {
  readonly identity: string;
  readonly capabilities: {
    hosted_checkout: true;
    automatic_recurring: false;
    refunds: false;
    portal: false;
  };
  create(order: PaymentOrder): Promise<Checkout>;
  notification(raw: Buffer, headers: Record<string, string>): string;
  lookup(reference: string): Promise<Receipt | null>;
  readonly acknowledgement: string;
}
const capabilities = {
  hosted_checkout: true,
  automatic_recurring: false,
  refunds: false,
  portal: false,
} as const;
const fail = (code = "invalid_payment_response"): never => {
  throw new RouterError(code, 422);
};
function https(value: string) {
  const u = new URL(value);
  if (u.protocol !== "https:" || u.username || u.password || u.hash)
    fail("invalid_payment_url");
  return u;
}
function equal(a: string, b: string) {
  return (
    a.length === b.length && timingSafeEqual(Buffer.from(a), Buffer.from(b))
  );
}
export function verifyHmac(
  raw: Buffer,
  header: string,
  secret: string,
  now = Date.now(),
) {
  const entries = header.split(",").map((x) => x.trim().split("="));
  const t = entries.find((x) => x[0] === "t")?.[1];
  if (!t || !/^\d{10}$/.test(t) || Math.abs(now / 1000 - Number(t)) > 300)
    throw new RouterError("invalid_signature", 401);
  const expected = createHmac("sha256", secret)
    .update(t + ".")
    .update(raw)
    .digest("hex");
  if (!entries.some((x) => x[0] === "v1" && !!x[1] && equal(expected, x[1])))
    throw new RouterError("invalid_signature", 401);
}
function integer(value: unknown): string {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value <= 0)
    return fail();
  return String(value);
}
function id(value: unknown): string {
  if (typeof value !== "string" || !value || value.length > 255) return fail();
  return value;
}
async function request(
  fetcher: typeof fetch,
  url: string,
  init: RequestInit = {},
) {
  try {
    const r = await fetcher(url, {
      ...init,
      redirect: "error",
      signal: AbortSignal.timeout(8000),
    });
    if (!r.ok) fail("payment_provider_error");
    return (await r.json()) as any;
  } catch {
    throw new RouterError("payment_provider_unavailable", 502);
  }
}
export class StripePayments implements PaymentAdapter {
  readonly capabilities = capabilities;
  readonly acknowledgement = "ok";
  readonly identity: string;
  constructor(
    private config: {
      secretKey: string;
      webhookSecret: string;
      accountId: string;
      live: boolean;
      successUrl: string;
      cancelUrl: string;
    },
    private fetcher: typeof fetch = fetch,
  ) {
    https(config.successUrl);
    https(config.cancelUrl);
    if (!config.secretKey || !config.webhookSecret || !config.accountId)
      fail("invalid_payment_config");
    this.identity = `stripe:${config.accountId}:${config.live}`;
  }
  private api(path: string, init: RequestInit = {}) {
    return request(this.fetcher, "https://api.stripe.com/v1/" + path, {
      ...init,
      headers: {
        Authorization: "Bearer " + this.config.secretKey,
        "Stripe-Version": "2026-10-28.endive",
        ...init.headers,
      },
    });
  }
  async create(o: PaymentOrder) {
    if ((await this.api("account")).id !== this.config.accountId)
      fail("merchant_mismatch");
    if (!/^(USD|TWD|CNY|EUR|JPY)$/.test(o.currency))
      fail("unsupported_currency");
    const data = new URLSearchParams({
      mode: "payment",
      success_url: this.config.successUrl,
      cancel_url: this.config.cancelUrl,
      client_reference_id: o.id,
      "line_items[0][quantity]": "1",
      "line_items[0][price_data][currency]": o.currency.toLowerCase(),
      "line_items[0][price_data][unit_amount]": o.amount_minor,
      "line_items[0][price_data][product_data][name]":
        "SuperRouter subscription period",
    });
    const r = await this.api("checkout/sessions", {
      method: "POST",
      headers: {
        "Content-Type": "application/x-www-form-urlencoded",
        "Idempotency-Key": o.id,
      },
      body: data,
    });
    if (
      r.livemode !== this.config.live ||
      https(r.url).hostname !== "checkout.stripe.com"
    )
      fail();
    return { reference: id(r.id), url: r.url };
  }
  notification(raw: Buffer, headers: Record<string, string>) {
    verifyHmac(
      raw,
      headers["stripe-signature"] ?? "",
      this.config.webhookSecret,
    );
    const e = JSON.parse(raw.toString());
    if (
      e.livemode !== this.config.live ||
      ![
        "checkout.session.completed",
        "checkout.session.async_payment_succeeded",
      ].includes(e.type)
    )
      return "";
    return id(e.data?.object?.id);
  }
  async lookup(reference: string) {
    if ((await this.api("account")).id !== this.config.accountId)
      fail("merchant_mismatch");
    const r = await this.api(
      "checkout/sessions/" + encodeURIComponent(reference),
    );
    if (r.id !== reference || r.livemode !== this.config.live) fail();
    if (r.payment_status !== "paid") return null;
    return {
      reference,
      order_id: id(r.client_reference_id),
      amount_minor: integer(r.amount_total),
      currency: id(r.currency).toUpperCase(),
    };
  }
}
export class OenPayments implements PaymentAdapter {
  readonly capabilities = capabilities;
  readonly acknowledgement = "ok";
  readonly identity: string;
  private base: string;
  constructor(
    private config: {
      token: string;
      webhookSecret: string;
      merchantId: string;
      live: boolean;
      successUrl: string;
      failureUrl: string;
    },
    private fetcher: typeof fetch = fetch,
  ) {
    https(config.successUrl);
    https(config.failureUrl);
    if (
      !/^[a-z0-9-]+$/.test(config.merchantId) ||
      !config.token ||
      !config.webhookSecret
    )
      fail("invalid_payment_config");
    this.base = `https://payment-api${config.live ? "" : ".testing"}.oen.tw`;
    this.identity = `oen:${config.merchantId}:${config.live}`;
  }
  private async api(path: string, init: RequestInit = {}) {
    const r = await request(this.fetcher, this.base + path, {
      ...init,
      headers: {
        Authorization: "Bearer " + this.config.token,
        "Content-Type": "application/json",
        ...init.headers,
      },
    });
    if (r.code !== "S0000") fail("payment_provider_error");
    return r.data;
  }
  async create(o: PaymentOrder) {
    // SuperRouter stores ISO minor units; OEN takes whole TWD dollars.
    if (o.currency !== "TWD" || BigInt(o.amount_minor) % 100n !== 0n)
      fail("unsupported_amount");
    const amount = Number(BigInt(o.amount_minor) / 100n);
    integer(amount);
    const r = await this.api("/checkout", {
      method: "POST",
      headers: { "Idempotency-Key": o.id },
      body: JSON.stringify({
        merchantId: this.config.merchantId,
        orderId: o.id,
        amount,
        currency: "TWD",
        successUrl: this.config.successUrl,
        failureUrl: this.config.failureUrl,
        productDetails: [
          {
            productionCode: o.id,
            description: "SuperRouter subscription period",
            quantity: 1,
            unit: "period",
            unitPrice: amount,
          },
        ],
      }),
    });
    const reference = id(r.id);
    if (!/^[a-zA-Z0-9]{27}$/.test(reference)) fail();
    return {
      reference,
      url: `https://${this.config.merchantId}${this.config.live ? "" : ".testing"}.oen.tw/checkout/${reference}`,
    };
  }
  notification(raw: Buffer, headers: Record<string, string>) {
    verifyHmac(
      raw,
      headers["oenpay-signature"] ?? "",
      this.config.webhookSecret,
    );
    const e = JSON.parse(raw.toString());
    if (e.merchantId !== this.config.merchantId) fail("merchant_mismatch");
    if (e.purpose !== "charge") return "";
    return id(e.id);
  }
  async lookup(reference: string) {
    const r = await this.api("/transactions/" + encodeURIComponent(reference));
    if (r.transactionId !== reference) fail();
    if (!["charged", "claimed"].includes(r.status)) return null;
    if (r.refundAmount > 0) fail("payment_review_required");
    return {
      reference,
      order_id: id(r.orderId),
      amount_minor: (BigInt(integer(r.amount)) * 100n).toString(),
      currency: "TWD",
    };
  }
}
export function epaySign(data: Record<string, string>, secret: string) {
  return createHash("md5")
    .update(
      Object.keys(data)
        .filter((k) => k !== "sign" && k !== "sign_type" && data[k] !== "")
        .sort()
        .map((k) => k + "=" + data[k])
        .join("&") + secret,
    )
    .digest("hex");
}
function yuan(value: string) {
  if (!/^\d+(\.\d{1,2})?$/.test(value)) return fail();
  const [a, b = ""] = value.split(".");
  return (BigInt(a!) * 100n + BigInt(b.padEnd(2, "0"))).toString();
}
export class EpayV1Payments implements PaymentAdapter {
  readonly capabilities = capabilities;
  readonly acknowledgement = "success";
  readonly identity: string;
  private base: string;
  constructor(
    private config: {
      endpoint: string;
      pid: string;
      key: string;
      notifyUrl: string;
      returnUrl: string;
    },
    private fetcher: typeof fetch = fetch,
  ) {
    const u = https(config.endpoint);
    if (
      u.search ||
      u.pathname !== "/" ||
      !/^\d+$/.test(config.pid) ||
      !config.key
    )
      fail("invalid_payment_config");
    https(config.notifyUrl);
    https(config.returnUrl);
    this.base = u.origin;
    this.identity = `epay-v1:${this.base}:${config.pid}`;
  }
  async create(o: PaymentOrder) {
    if (o.currency !== "CNY") fail("unsupported_currency");
    const n = BigInt(o.amount_minor);
    if (n <= 0n) fail();
    const data: Record<string, string> = {
      pid: this.config.pid,
      out_trade_no: o.id,
      notify_url: this.config.notifyUrl,
      return_url: this.config.returnUrl,
      name: "SuperRouter subscription period",
      money: `${n / 100n}.${String(n % 100n).padStart(2, "0")}`,
    };
    data.sign = epaySign(data, this.config.key);
    data.sign_type = "MD5";
    return {
      reference: o.id,
      url: this.base + "/submit.php?" + new URLSearchParams(data),
    };
  }
  notification(raw: Buffer) {
    const params = new URLSearchParams(raw.toString());
    const data: Record<string, string> = {};
    for (const [k, v] of params) {
      if (k in data) fail("duplicate_parameter");
      data[k] = v;
    }
    if (
      data.pid !== this.config.pid ||
      data.sign_type !== "MD5" ||
      !equal(epaySign(data, this.config.key), data.sign ?? "")
    )
      throw new RouterError("invalid_signature", 401);
    if (data.trade_status !== "TRADE_SUCCESS") return "";
    return id(data.out_trade_no);
  }
  async lookup(reference: string) {
    // Legacy protocol requires query authentication. Never log URLs or fetch errors.
    const r = await request(
      this.fetcher,
      this.base +
        "/api.php?" +
        new URLSearchParams({
          act: "order",
          pid: this.config.pid,
          key: this.config.key,
          out_trade_no: reference,
        }),
    );
    if (
      r.code !== 1 ||
      String(r.pid) !== this.config.pid ||
      r.out_trade_no !== reference
    )
      fail();
    if (Number(r.status) !== 1) return null;
    return {
      reference,
      order_id: reference,
      amount_minor: yuan(String(r.money)),
      currency: "CNY",
    };
  }
}
