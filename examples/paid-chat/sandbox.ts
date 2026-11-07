/** Deliberate local simulator. It is not an implementation of any payment brand. */
import { createHmac, timingSafeEqual } from "node:crypto";
import { RouterError } from "@supersolutions/super-router-contracts";
import type {
  PaymentAdapter,
  PaymentOrder,
  Receipt,
} from "@supersolutions/super-router-payments";
export class LocalSandboxPayments implements PaymentAdapter {
  readonly identity = "local-sandbox:v1";
  readonly acknowledgement = "ok";
  readonly capabilities = {
    hosted_checkout: true,
    automatic_recurring: false,
    refunds: false,
    portal: false,
  } as const;
  private paid = new Map<string, Receipt>();
  constructor(
    readonly origin: string,
    private secret: string,
  ) {}
  private sign(value: string) {
    return createHmac("sha256", this.secret).update(value).digest("hex");
  }
  async create(order: PaymentOrder) {
    const data = Buffer.from(JSON.stringify(this.project(order))).toString(
      "base64url",
    );
    return {
      reference: order.id,
      url: this.origin + "/checkout?ticket=" + data + "." + this.sign(data),
    };
  }
  private project(value: unknown): PaymentOrder {
    const order = value as Partial<PaymentOrder> | null;
    if (
      !order ||
      typeof order !== "object" ||
      typeof order.id !== "string" ||
      !/^[a-zA-Z0-9_-]{1,120}$/.test(order.id) ||
      typeof order.amount_minor !== "string" ||
      !/^[1-9][0-9]{0,29}$/.test(order.amount_minor) ||
      typeof order.currency !== "string" ||
      !/^[A-Z]{3}$/.test(order.currency)
    )
      throw new RouterError("invalid_ticket", 403);
    return {
      id: order.id,
      amount_minor: order.amount_minor,
      currency: order.currency,
    };
  }
  ticket(value: string): PaymentOrder {
    try {
      const parts = value.split(".");
      if (parts.length !== 2 || value.length > 16384) throw Error();
      const [data, sig] = parts;
      if (
        !data ||
        !sig ||
        !/^[A-Za-z0-9_-]+$/.test(data) ||
        !/^[a-f0-9]{64}$/.test(sig) ||
        Buffer.from(data, "base64url").toString("base64url") !== data ||
        !timingSafeEqual(Buffer.from(this.sign(data)), Buffer.from(sig))
      )
        throw Error();
      // Older signed full-row tickets remain readable, but extra fields are never consumed.
      return this.project(
        JSON.parse(Buffer.from(data, "base64url").toString()),
      );
    } catch {
      throw new RouterError("invalid_ticket", 403);
    }
  }
  complete(ticket: string) {
    const order = this.ticket(ticket);
    this.paid.set(order.id, {
      ...order,
      order_id: order.id,
      reference: order.id,
    });
    const raw = Buffer.from(JSON.stringify({ reference: order.id }));
    return { raw, headers: { "x-local-signature": this.sign(raw.toString()) } };
  }
  notification(raw: Buffer, headers: Record<string, string>) {
    const sig = headers["x-local-signature"] ?? "";
    if (
      !/^[a-f0-9]{64}$/.test(sig) ||
      !timingSafeEqual(Buffer.from(this.sign(raw.toString())), Buffer.from(sig))
    )
      throw new RouterError("invalid_signature", 401);
    return String(JSON.parse(raw.toString()).reference);
  }
  async lookup(ref: string) {
    return this.paid.get(ref) ?? null;
  }
}
