import type {
  Service,
  MemberInput,
  CustomerInput,
  KeyInput,
  PlanInput,
  SubscriptionInput,
  CreditGrantInput,
} from "@superrouter/contracts";
import {
  parseSSE,
  type Invocation,
  type StreamEvent,
} from "@superrouter/contracts";
export class SuperRouterClient {
  constructor(
    private base: string,
    private key: string,
  ) {}
  private async send(path: string, init: RequestInit = {}) {
    const r = await fetch(this.base + path, {
      ...init,
      redirect: "manual",
      headers: {
        Authorization: `Bearer ${this.key}`,
        "Content-Type": "application/json",
        ...init.headers,
      },
    });
    if (r.status === 303) return r;
    if (!r.ok) {
      const e = (await r.json()) as any;
      throw Object.assign(new Error(e.error?.code ?? "http_error"), {
        status: r.status,
        request_id: e.error?.request_id,
      });
    }
    return r;
  }
  async infer(input: Invocation, key: string) {
    const r = await this.send("/v1/inferences", {
      method: "POST",
      headers: { "Idempotency-Key": key },
      body: JSON.stringify({ ...input, stream: false }),
    });
    if (r.status === 303)
      return this.getRequest(
        (r.headers.get("location") ?? "").split("/").pop()!,
      );
    return r.json();
  }
  async *stream(
    input: Invocation,
    key: string,
    signal?: AbortSignal,
  ): AsyncGenerator<StreamEvent> {
    const r = await this.send("/v1/inferences", {
      method: "POST",
      headers: { "Idempotency-Key": key },
      body: JSON.stringify({ ...input, stream: true }),
      signal,
    });
    if (r.status === 303) {
      yield {
        type: "request.replayed",
        request_id: r.headers.get("location")!.split("/").pop()!,
        sequence: 0,
      };
      return;
    }
    if (!r.body) throw new Error("empty_stream");
    let terminal = false;
    for await (const frame of parseSSE(r.body)) {
      if (["request.completed", "request.failed"].includes(frame.event))
        terminal = true;
      yield { type: frame.event, ...JSON.parse(frame.data) };
    }
    if (!terminal)
      throw new Error(
        "stream_interrupted: query request status before retrying",
      );
  }
  async getRequest(id: string) {
    return (await this.send("/v1/requests/" + encodeURIComponent(id))).json();
  }
  async cancel(id: string, key: string) {
    return (
      await this.send("/v1/requests/" + encodeURIComponent(id) + "/cancel", {
        method: "POST",
        headers: { "Idempotency-Key": key },
        body: "{}",
      })
    ).json();
  }

  private async post(path: string, input: unknown, key: string) {
    return (
      await this.send(path, {
        method: "POST",
        headers: { "Idempotency-Key": key },
        body: JSON.stringify(input),
      })
    ).json();
  }
  createMember(input: MemberInput, key: string) {
    return this.post("/v1/admin/members", input, key);
  }
  createCustomer(input: CustomerInput, key: string) {
    return this.post("/v1/admin/customers", input, key);
  }
  issueKey(
    input: KeyInput,
    key: string,
  ): Promise<{ id: string; token: string | null; expires_at: string }> {
    return this.post("/v1/admin/keys", input, key);
  }
  revokeKey(id: string, key: string) {
    return this.post(
      "/v1/admin/keys/" + encodeURIComponent(id) + "/revoke",
      {},
      key,
    );
  }
  /** Developer admin only. Config is validated server-side; retry a stale version by reading current service. */
  publishCreditRateVersion(
    serviceId: string,
    expectedVersion: number,
    config: unknown,
    key: string,
  ): Promise<Service> {
    return this.post(
      "/v1/admin/services/" + encodeURIComponent(serviceId) + "/credit-rates",
      { expected_version: expectedVersion, config },
      key,
    );
  }
  publishPlan(input: PlanInput, key: string) {
    return this.post("/v1/admin/plans", input, key);
  }
  subscribe(
    input: SubscriptionInput,
    key: string,
  ): Promise<{ id: string; status: string }> {
    return this.post("/v1/admin/subscriptions", input, key);
  }
  paymentOrder(
    id: string,
    integration: string,
    key: string,
  ): Promise<{
    id: string;
    amount_minor: string;
    currency: string;
    state: string;
  }> {
    return this.post(
      "/v1/admin/subscriptions/" + encodeURIComponent(id) + "/payment-orders",
      { integration_id: integration },
      key,
    );
  }
  checkout(
    orderId: string,
    key: string,
  ): Promise<{ reference: string; url: string }> {
    return this.post(
      "/v1/admin/payment-orders/" + encodeURIComponent(orderId) + "/checkout",
      {},
      key,
    );
  }
  reconcilePayment(orderId: string, key: string, reference?: string) {
    return this.post(
      "/v1/admin/payment-orders/" + encodeURIComponent(orderId) + "/reconcile",
      { reference },
      key,
    );
  }
  cancelSubscription(id: string, key: string) {
    return this.post(
      "/v1/admin/subscriptions/" + encodeURIComponent(id) + "/cancel",
      {},
      key,
    );
  }
  grantCredits(input: CreditGrantInput, key: string) {
    return this.post("/v1/admin/credits/grants", input, key);
  }
  async credits() {
    return (await this.send("/v1/credits")).json();
  }
  async subscription(id: string) {
    return (
      await this.send("/v1/admin/subscriptions/" + encodeURIComponent(id))
    ).json();
  }
}
