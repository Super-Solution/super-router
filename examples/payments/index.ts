/** Offline protocol demonstration: no network, secrets, or real charges. */
import {
  StripePayments,
  OenPayments,
  EpayV1Payments,
} from "@superrouter/payments";
const fake = (handler: (url: string, init: RequestInit) => unknown) =>
  (async (url, init) =>
    new Response(
      JSON.stringify(handler(String(url), init ?? {})),
    )) as typeof fetch;
const order = { id: "demo-order", amount_minor: "120000", currency: "TWD" };
const ref = "a".repeat(27);
const stripe = new StripePayments(
  {
    secretKey: "fixture",
    webhookSecret: "fixture",
    accountId: "acct_demo",
    live: false,
    successUrl: "https://example.com/success",
    cancelUrl: "https://example.com/cancel",
  },
  fake((url, init) =>
    url.endsWith("/account")
      ? { id: "acct_demo" }
      : init.method === "POST"
        ? {
            id: "cs_demo",
            url: "https://checkout.stripe.com/c/pay/demo",
            livemode: false,
          }
        : {
            id: "cs_demo",
            livemode: false,
            payment_status: "paid",
            client_reference_id: order.id,
            amount_total: 120000,
            currency: "twd",
          },
  ),
);
const oen = new OenPayments(
  {
    token: "fixture",
    webhookSecret: "fixture",
    merchantId: "demo",
    live: false,
    successUrl: "https://example.com/success",
    failureUrl: "https://example.com/failure",
  },
  fake((_url, init) => ({
    code: "S0000",
    data:
      init.method === "POST"
        ? { id: ref }
        : {
            transactionId: ref,
            orderId: order.id,
            status: "charged",
            amount: 1200,
          },
  })),
);
const epay = new EpayV1Payments(
  {
    endpoint: "https://pay.example/",
    pid: "1001",
    key: "fixture",
    notifyUrl: "https://example.com/notify",
    returnUrl: "https://example.com/return",
  },
  fake(() => ({
    code: 1,
    pid: 1001,
    out_trade_no: order.id,
    status: 1,
    money: "1200.00",
  })),
);
for (const [name, adapter, currency] of [
  ["stripe", stripe, "TWD"],
  ["oen", oen, "TWD"],
  ["epay-v1", epay, "CNY"],
] as const) {
  const checkout = await adapter.create({ ...order, currency });
  const receipt = await adapter.lookup(checkout.reference);
  // Checkout links may contain signed bearer values; print only redacted status.
  console.log({
    provider: name,
    fixture: true,
    checkout_created: !!checkout.url,
    receipt,
    capabilities: adapter.capabilities,
  });
}
