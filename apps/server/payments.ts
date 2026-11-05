import {
  StripePayments,
  OenPayments,
  EpayV1Payments,
} from "@superrouter/payments";
import type { PaymentIntegration } from "@superrouter/storage-postgres";
/** Server configuration only. No credentials accepted in tenant HTTP requests. */
export function paymentIntegrations(
  workspace: string,
  environment: string,
  env: NodeJS.ProcessEnv = process.env,
): PaymentIntegration[] {
  const integrations: PaymentIntegration[] = [];
  const required = (key: string) => {
    const value = env[key];
    if (!value) throw new Error("Missing payment configuration: " + key);
    return value;
  };
  const live = environment === "live";
  if (env.STRIPE_SECRET_KEY)
    integrations.push({
      id: "stripe",
      workspace,
      environment,
      mode: "hosted",
      adapter: new StripePayments({
        secretKey: required("STRIPE_SECRET_KEY"),
        webhookSecret: required("STRIPE_WEBHOOK_SECRET"),
        accountId: required("STRIPE_ACCOUNT_ID"),
        live,
        successUrl: required("PAYMENT_SUCCESS_URL"),
        cancelUrl: required("PAYMENT_CANCEL_URL"),
      }),
    });
  if (env.OEN_TOKEN)
    integrations.push({
      id: "oen",
      workspace,
      environment,
      mode: "hosted",
      adapter: new OenPayments({
        token: required("OEN_TOKEN"),
        webhookSecret: required("OEN_WEBHOOK_SECRET"),
        merchantId: required("OEN_MERCHANT_ID"),
        live,
        successUrl: required("PAYMENT_SUCCESS_URL"),
        failureUrl: required("PAYMENT_CANCEL_URL"),
      }),
    });
  if (env.EPAY_KEY)
    integrations.push({
      id: "epay-v1",
      workspace,
      environment,
      mode: "hosted",
      adapter: new EpayV1Payments({
        key: required("EPAY_KEY"),
        pid: required("EPAY_PID"),
        endpoint: required("EPAY_ENDPOINT"),
        notifyUrl: required("EPAY_NOTIFY_URL"),
        returnUrl: required("PAYMENT_SUCCESS_URL"),
      }),
    });
  return integrations;
}
