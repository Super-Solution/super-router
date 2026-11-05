import type { ReactNode } from "react";
export interface PlanOffer {
  id: string;
  name: string;
  description: string;
  payment_methods?: string[];
  plan: {
    id: string;
    version: number;
    amount_minor: string;
    currency: string;
    credits: string;
    period_days: number;
    service_ids: string[];
  };
}
export interface Bucket {
  id: string;
  asset: string;
  available: string;
  held: string;
  source_kind: string;
  service_ids: string[] | null;
  expires_at: string | null;
}
export interface Subscription {
  id: string;
  status: string;
  plan_id: string;
  plan_version: number;
  period_days: number;
  current_period_end: string | null;
  cancel_effective_at: string | null;
}
export interface BillingSnapshot {
  customer: string;
  subscriptions: Subscription[];
  entitlements: { service_ids: string[] }[];
  buckets: Bucket[];
  orders: {
    id: string;
    state: string;
    amount_minor: string;
    currency: string;
    payment_method?: string;
    subscription_id?: string;
  }[];
  usage: {
    id: string;
    service: string;
    execution: string;
    billing: string;
    charged: string;
    created_at: string;
  }[];
  as_of: string;
}
/** Exact decimal formatting: monetary values and ledger atoms never pass through Number. */
export function formatIntegerUnits(value: string, scale = 9) {
  const n = BigInt(value),
    sign = n < 0n ? "-" : "",
    digits = (n < 0n ? -n : n).toString().padStart(scale + 1, "0");
  if (!scale) return sign + digits;
  const whole = digits.slice(0, -scale),
    fraction = digits.slice(-scale).replace(/0+$/, "");
  return sign + whole + (fraction ? "." + fraction : "");
}
export function currencyScale(currency: string) {
  if (currency === "JPY") return 0;
  if (["USD", "TWD", "CNY", "EUR"].includes(currency)) return 2;
  throw new Error("Unsupported currency scale");
}
export function createBillingClient(base = "") {
  async function request<T>(
    path: string,
    data?: unknown,
    key?: string,
    signal?: AbortSignal,
  ): Promise<T> {
    const res = await fetch(base + path, {
      method: data === undefined ? "GET" : "POST",
      credentials: "same-origin",
      signal,
      headers:
        data === undefined
          ? {}
          : {
              "Content-Type": "application/json",
              ...(key ? { "Idempotency-Key": key } : {}),
            },
      ...(data === undefined ? {} : { body: JSON.stringify(data) }),
    });
    const body = await res.json();
    if (!res.ok) throw new Error(body.error ?? "request_failed");
    return body;
  }
  return {
    request,
    me: () => request<BillingSnapshot>("/api/billing/me"),
    checkout: (id: string, key: string, paymentMethod?: string) =>
      request<{ order_id: string; redirect_url: string }>(
        "/api/billing/checkout",
        {
          offer_id: id,
          ...(paymentMethod ? { payment_method: paymentMethod } : {}),
        },
        key,
      ),
    cancel: (id: string, key: string) =>
      request("/api/billing/subscription/cancel", { subscription_id: id }, key),
  };
}
export function PricingTable({
  offers,
  busy,
  disabled,
  onSelect,
}: {
  offers: PlanOffer[];
  busy?: string;
  disabled?: boolean;
  onSelect: (id: string) => void;
}) {
  return (
    <div className="sr-pricing">
      {offers.map((o, i) => (
        <article
          className={"sr-plan " + (i === 1 ? "sr-featured" : "")}
          key={o.id}
        >
          <span className="sr-eyebrow">
            {i === 0 ? "輕量開始" : "更多創作空間"}
          </span>
          <h3>{o.name}</h3>
          <p className="sr-price">
            {o.plan.currency}{" "}
            {formatIntegerUnits(
              o.plan.amount_minor,
              currencyScale(o.plan.currency),
            )}
            <small>／{o.plan.period_days} 天</small>
          </p>
          <p>{o.description}</p>
          <p className="sr-plan-credits">
            {formatIntegerUnits(o.plan.credits)} credits
            <span>當期有效・不結轉</span>
          </p>
          <button disabled={disabled || !!busy} onClick={() => onSelect(o.id)}>
            {busy === o.id ? "建立付款中…" : "選擇方案"}
          </button>
        </article>
      ))}
    </div>
  );
}
export function CreditBalance({
  buckets,
  service,
  asset,
  asOf,
}: {
  buckets: Bucket[];
  service: string;
  asset: string;
  asOf?: string;
}) {
  const relevant = buckets.filter(
    (b) =>
      b.asset === asset && (!b.service_ids || b.service_ids.includes(service)),
  );
  const available = relevant.reduce((n, b) => n + BigInt(b.available), 0n),
    held = relevant.reduce((n, b) => n + BigInt(b.held), 0n);
  return (
    <div className="sr-balance">
      <span>此服務可用額度</span>
      <strong>
        {formatIntegerUnits(String(available))}
        <small> credits</small>
      </strong>
      <p>
        預留 {formatIntegerUnits(String(held))} · {relevant.length} 筆額度
      </p>
      {asOf && (
        <time dateTime={asOf}>
          更新於 {new Date(asOf).toLocaleTimeString("zh-TW")}
        </time>
      )}
    </div>
  );
}
export function SubscriptionCard({
  subscription,
  onCancel,
  busy,
}: {
  subscription?: Subscription;
  onCancel: (id: string) => void;
  busy?: boolean;
}) {
  if (!subscription)
    return (
      <div className="sr-subscription">
        <strong>尚未訂閱</strong>
        <p>選擇方案並完成付款後，即可開始。</p>
      </div>
    );
  const status: Record<string, string> = {
    pending: "等待付款",
    active: "使用中",
    past_due: "等待續期",
    cancelled: "已取消",
  };
  return (
    <div className="sr-subscription">
      <div>
        <strong>{subscription.plan_id}</strong>
        <span className="sr-badge">
          {status[subscription.status] ?? subscription.status}
        </span>
      </div>
      <p>
        {subscription.current_period_end
          ? "當期至 " +
            new Date(subscription.current_period_end).toLocaleDateString(
              "zh-TW",
            )
          : `每期固定 ${subscription.period_days} 天・手動付款`}
      </p>
      {subscription.cancel_effective_at ? (
        <p>
          {subscription.current_period_end
            ? "已安排取消，當期權益保留至到期。"
            : "已取消，尚未付款授額。"}
        </p>
      ) : (
        <button
          className="sr-link"
          disabled={busy}
          onClick={() => onCancel(subscription.id)}
        >
          取消下期續用
        </button>
      )}
    </div>
  );
}
export function CheckoutStatus({ state }: { state?: string }) {
  const text: Record<string, string> = {
    pending: "等待可信付款通知，回到此頁不會直接授額。",
    succeeded: "付款已確認，當期 credits 已入帳。",
    failed: "付款失敗，尚未授額。",
    review_required: "付款需人工核對，尚未授額。",
  };
  return state ? (
    <div className="sr-status" role="status">
      {text[state] ?? "正在確認付款與授額狀態…"}
    </div>
  ) : null;
}
export function Paywall({
  allowed,
  children,
}: {
  allowed: boolean;
  children: ReactNode;
}) {
  return (
    <>
      {!allowed && (
        <div className="sr-paywall">
          先完成訂閱付款，再開始創作。每次請求需預留最大用量，實際扣款依可信用量結算。
        </div>
      )}
      {children}
    </>
  );
}
export function UsageSummary({ usage }: { usage: BillingSnapshot["usage"] }) {
  return (
    <section className="sr-usage">
      <h3>最近用量</h3>
      {!usage.length ? (
        <p>完成第一筆創作後，這裡會顯示實際扣款。</p>
      ) : (
        <div className="sr-table-scroll">
          <table>
            <thead>
              <tr>
                <th>服務</th>
                <th>狀態</th>
                <th>實際 credits</th>
              </tr>
            </thead>
            <tbody>
              {usage.map((u) => (
                <tr key={u.id}>
                  <td>{u.service}</td>
                  <td>{u.billing === "settled" ? "已結算" : u.billing}</td>
                  <td>{formatIntegerUnits(u.charged)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}
