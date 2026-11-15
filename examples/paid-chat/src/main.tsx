import React, { useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { AgentClient } from "@supersolutions/super-chat-client";
import { AgentProvider } from "@supersolutions/super-chat-react";
import {
  ContextBuilder,
  ToolRegistry,
  createOpenAIProvider,
  type Provider,
} from "@supersolutions/super-chat-core";
import { Composer, Thread } from "@supersolutions/super-chat-ui";
import {
  createBillingClient,
  PricingTable,
  CreditBalance,
  SubscriptionCard,
  CheckoutStatus,
  Paywall,
  UsageSummary,
  formatIntegerUnits,
  type BillingSnapshot,
  type PlanOffer,
} from "@supersolutions/super-router-ui";
import "@supersolutions/super-chat-ui/styles.css";
import "./style.css";
const api = createBillingClient();
type Mode = "chat" | "image" | "video" | "audio" | "task";
type Catalog = {
  mode: string;
  payment_methods: {
    id: string;
    name: string;
    mode: string;
    currencies: string[];
  }[];
  offers: PlanOffer[];
  services: Record<
    Mode,
    {
      id: string;
      asset: string;
      version: number;
      unit: string;
      rate: string;
      max: number;
    }
  >;
};
const names: Record<Mode, string> = {
  chat: "對話",
  image: "圖片",
  video: "影片",
  audio: "音訊",
  task: "任務",
};
function App() {
  const [catalog, setCatalog] = useState<Catalog>(),
    [me, setMe] = useState<BillingSnapshot>(),
    [key, setKey] = useState(""),
    [error, setError] = useState(""),
    [connecting, setConnecting] = useState(false),
    [busy, setBusy] = useState(""),
    [paymentMethod, setPaymentMethod] = useState("sandbox"),
    [mode, setMode] = useState<Mode>("chat"),
    [quantity, setQuantity] = useState(1),
    [last, setLast] = useState<any>(),
    [order, setOrder] = useState(
      new URLSearchParams(location.search).get("order") ?? "",
    ),
    [orderState, setOrderState] = useState("");
  useEffect(() => {
    if (
      catalog?.payment_methods.length &&
      !catalog.payment_methods.some((m) => m.id === paymentMethod)
    )
      setPaymentMethod(catalog.payment_methods[0]!.id);
  }, [catalog, paymentMethod]);
  useEffect(() => {
    if (me && !order && me.orders[0]) setOrder(me.orders[0].id);
    const pending = me?.orders.find(
      (o) => o.subscription_id === me.subscriptions[0]?.id,
    );
    if (pending?.payment_method) setPaymentMethod(pending.payment_method);
  }, [me?.customer, me?.orders[0]?.id]);
  const runtime = useRef({
    mode,
    quantity,
    expected_service_version: catalog?.services[mode].version,
  });
  runtime.current = {
    mode,
    quantity,
    expected_service_version: catalog?.services[mode].version,
  };
  const pending = useRef<{ fingerprint: string; key: string } | undefined>(
      undefined,
    ),
    checkoutKeys = useRef(new Map<string, string>());
  const refresh = async () => {
    try {
      const data = await api.me();
      setMe(data);
      return data;
    } catch (e) {
      if (
        String(e).includes("session_required") ||
        String(e).includes("invalid_key")
      )
        setMe(undefined);
      throw e;
    }
  };
  useEffect(() => {
    api
      .request<Catalog>("/api/catalog")
      .then(setCatalog)
      .catch(() => setError("無法載入方案，請確認本機服務。"));
    refresh().catch(() => {});
  }, []);
  useEffect(() => {
    if (!me || !order) return;
    let stopped = false;
    const poll = async () => {
      try {
        const status = await api.request<any>(
          "/api/billing/orders/" + encodeURIComponent(order),
        );
        if (stopped) return;
        setOrderState(status.state);
        await refresh();
        if (["succeeded", "failed", "review_required"].includes(status.state))
          clearInterval(timer);
      } catch (e) {
        if (!stopped) setError(message(e));
      }
    };
    poll();
    const timer = setInterval(poll, 2500);
    return () => {
      stopped = true;
      clearInterval(timer);
    };
  }, [order, !!me]);
  const client = useMemo(() => {
    const base = createOpenAIProvider({
      transport: {
        kind: "custom",
        credentialSafe: true,
        fetch: async () => {
          throw new Error("unused_transport");
        },
      },
      dialect: "responses",
    });
    const provider: Provider = {
      ...base,
      async generate(req, opts) {
        const prompt =
          [...req.messages]
            .reverse()
            .find((m) => m.role === "user")
            ?.parts.filter((p) => p.type === "text")
            .map((p) => (p as { text: string }).text)
            .join("\n") ?? "";
        const data = { ...runtime.current, prompt },
          fingerprint = JSON.stringify(data);
        if (!pending.current || pending.current.fingerprint !== fingerprint)
          pending.current = { fingerprint, key: crypto.randomUUID() };
        try {
          const r = await api.request<any>(
            "/api/infer",
            data,
            pending.current.key,
            opts?.signal,
          );
          setLast(r.replayed ? undefined : r);
          await refresh().catch(() =>
            setError("請求已完成，餘額暫時無法更新，請稍後重新整理。"),
          );
          pending.current = undefined;
          return {
            parts: [
              {
                type: "text",
                text:
                  r.output?.content ??
                  `此請求已處理（${r.execution_status}／${r.billing_status}），不會再次扣款。先前回覆未持久化。`,
              },
            ],
            finishReason: "stop" as const,
            usage: {
              inputTokens: r.usage?.meters?.input ?? 0,
              outputTokens: r.usage?.meters?.output ?? 0,
              totalTokens:
                (r.usage?.meters?.input ?? 0) + (r.usage?.meters?.output ?? 0),
            },
            modelId: "SuperRouter Sandbox",
          };
        } catch (e) {
          if (e instanceof Error && e.message === "service_version_changed") {
            pending.current = undefined;
            try {
              setCatalog(await api.request<Catalog>("/api/catalog"));
            } catch {}
          }
          setError(message(e));
          throw e;
        }
      },
    };
    return new AgentClient({
      provider,
      model: "SuperRouter Sandbox",
      contextBuilder: new ContextBuilder({
        identity: "",
        contextWindow: 16000,
      }),
      tools: new ToolRegistry(),
      toolResolution: { presets: [] },
      mode: "sync",
      maxSteps: 1,
    });
  }, [me?.customer]);
  async function connect(demo = false, fresh = false) {
    setConnecting(true);
    setError("");
    try {
      await api.request(
        "/api/session",
        demo ? { demo: true, ...(fresh ? { new_demo: true } : {}) } : { key },
      );
      setKey("");
      setOrder("");
      setOrderState("");
      setLast(undefined);
      checkoutKeys.current.clear();
      history.replaceState({}, "", location.pathname);
      await refresh();
    } catch (e) {
      setError(message(e));
    } finally {
      setConnecting(false);
    }
  }
  async function checkout(id: string) {
    setBusy(id);
    setError("");
    try {
      const stored = me?.orders.find(
        (o) => o.subscription_id === me.subscriptions[0]?.id,
      );
      const method = stored?.payment_method ?? paymentMethod;
      const intent = id + ":" + method;
      let k = checkoutKeys.current.get(intent);
      if (!k) {
        k = crypto.randomUUID();
        checkoutKeys.current.set(intent, k);
      }
      const c = await api.checkout(id, k, method);
      setOrder(c.order_id);
      location.assign(c.redirect_url);
    } catch (e) {
      setError(message(e));
      await refresh().catch(() => {});
    } finally {
      setBusy("");
    }
  }
  async function cancel(id: string) {
    setBusy("cancel");
    try {
      await api.cancel(id, crypto.randomUUID());
      checkoutKeys.current.clear();
      await refresh();
      setError("");
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy("");
    }
  }
  const selected = catalog?.services[mode],
    allowed = !!me?.entitlements.some((e) =>
      e.service_ids.includes(selected?.id ?? ""),
    );
  return (
    <main>
      <header>
        <a className="brand" href="/" aria-label="SuperRouter 首頁">
          <span className="brand-icon">S</span>SuperRouter
          <span className="brand-dot">/</span>
          <span className="brand-sub">創作工作台</span>
        </a>
        <span className="mode-badge">
          <i />
          {catalog?.mode === "hosted"
            ? catalog.payment_methods[0]?.mode === "live"
              ? "商戶實際收款"
              : "商戶測試"
            : "本機模擬"}
        </span>
      </header>
      <section className="intro">
        <span className="eyebrow">ONE BALANCE. EVERY CREATION.</span>
        <h1>
          讓每一次創作，
          <br className="mobile-break" />
          都有清楚的帳。
        </h1>
        <p>選一個方案，用同一份 credits 體驗對話與媒體任務。</p>
      </section>
      <section className="connection" aria-label="連線設定">
        <div>
          <strong>{me ? "已連線 · " + me.customer : "從這裡開始"}</strong>
          <p>
            {me
              ? "額度與付款紀錄只屬於目前帳戶。"
              : catalog?.mode === "hosted"
                ? "輸入商戶 App 已發行的客戶 SuperRouter 金鑰。"
                : "免填金鑰即可體驗，或使用自己的客戶端金鑰。"}
          </p>
        </div>
        {me ? (
          <div className="connect-actions">
            {catalog?.mode === "sandbox" && (
              <>
                <button
                  className="secondary"
                  disabled={connecting}
                  onClick={() => connect(true, true)}
                >
                  開啟新沙盒顧客
                </button>
                <button
                  className="sr-link"
                  disabled={connecting}
                  onClick={() => connect(true)}
                >
                  返回示範顧客
                </button>
              </>
            )}
            <button
              className="secondary"
              onClick={async () => {
                await api.request("/api/logout", {});
                setMe(undefined);
                setLast(undefined);
                setOrder("");
                setOrderState("");
              }}
            >
              登出
            </button>
          </div>
        ) : (
          <div className="connect-actions">
            {catalog?.mode === "sandbox" && (
              <button disabled={connecting} onClick={() => connect(true)}>
                {connecting ? "連線中…" : "使用沙盒示範帳戶"}
              </button>
            )}
            <details>
              <summary>使用 SuperRouter API key</summary>
              <form
                onSubmit={(e) => {
                  e.preventDefault();
                  connect();
                }}
              >
                <label htmlFor="api-key">客戶 SuperRouter API key</label>
                <div>
                  <input
                    id="api-key"
                    type="password"
                    autoComplete="off"
                    value={key}
                    onChange={(e) => setKey(e.target.value)}
                    placeholder="sr_…"
                  />
                  <button disabled={connecting || !key}>連線</button>
                </div>
                <small>
                  這是應用客戶金鑰。僅保留於記憶體，不寫入瀏覽器儲存空間。
                </small>
              </form>
            </details>
          </div>
        )}
      </section>
      {error && (
        <div className="error" role="alert">
          <span>{error}</span>
          <button
            className="sr-link"
            onClick={() => setError("")}
            aria-label="關閉錯誤"
          >
            ✕
          </button>
        </div>
      )}
      <div className="workspace">
        <aside>
          <div className="section-heading">
            <h2>選擇方案</h2>
            <span>
              {catalog?.payment_methods
                .find((m) => m.id === paymentMethod)
                ?.currencies.join("/")}{" "}
              · 手動續期
            </span>
          </div>
          {catalog && (
            <div className="gateway-picker">
              <label htmlFor="payment-method">付款通路</label>
              <select
                id="payment-method"
                aria-label="付款通路"
                value={paymentMethod}
                disabled={
                  !!busy ||
                  !!me?.subscriptions.find(
                    (s) => s.status === "active" || s.status === "pending",
                  )
                }
                onChange={(e) => setPaymentMethod(e.target.value)}
              >
                {catalog.payment_methods.map((m) => (
                  <option key={m.id} value={m.id}>
                    {m.name} ·{" "}
                    {m.mode === "fixture"
                      ? "本機模擬"
                      : m.mode === "sandbox"
                        ? "本機沙盒"
                        : m.mode === "live"
                          ? "實際收款"
                          : "商戶測試"}
                  </option>
                ))}
              </select>
              <p>
                {catalog.payment_methods.find((m) => m.id === paymentMethod)
                  ?.mode === "live"
                  ? "此通路會收取真實款項，請確認商戶與價格後付款。"
                  : catalog.mode === "sandbox"
                    ? "本機模擬付款，不連接供應商，不收取真實款項。"
                    : "商戶測試通路，請使用對應測試帳戶。"}
              </p>
            </div>
          )}
          {catalog ? (
            <PricingTable
              offers={catalog.offers.filter(
                (o) =>
                  !o.payment_methods ||
                  o.payment_methods.includes(paymentMethod),
              )}
              disabled={
                !me ||
                !!me.subscriptions.find(
                  (s) => s.status === "active" || s.status === "pending",
                )
              }
              busy={busy}
              onSelect={checkout}
            />
          ) : (
            <p role="status">載入方案…</p>
          )}
          <p className="fine-print">
            {catalog?.mode === "hosted"
              ? "收費商戶：此 App 設定的商戶。固定 30 天，非曆月；手動付款，不自動扣款。實際收款模式會產生真實費用。"
              : "示範商戶：SuperRouter 本機。固定 30 天，非曆月；不自動扣款。沙盒不收取真實款項。"}
          </p>
          <CheckoutStatus state={orderState} />
          {me && (
            <>
              <SubscriptionCard
                subscription={me.subscriptions[0]}
                onCancel={cancel}
                busy={!!busy}
              />
              {me.subscriptions[0]?.status === "pending" && (
                <button
                  className="secondary resume"
                  onClick={() =>
                    checkout(
                      catalog!.offers.find(
                        (o) =>
                          o.plan.id === me.subscriptions[0]!.plan_id &&
                          o.plan.version === me.subscriptions[0]!.plan_version,
                      )?.id ?? catalog!.offers[0]!.id,
                    )
                  }
                >
                  繼續付款
                </button>
              )}
              <CreditBalance
                buckets={me.buckets}
                service={selected?.id ?? ""}
                asset={selected?.asset ?? "credit:paid-demo"}
                asOf={me.as_of}
              />
            </>
          )}
        </aside>
        <section className="studio">
          <div className="section-heading">
            <h2>創作空間</h2>
            <span className="sr-badge">Mock 模型</span>
          </div>
          <div className="task-tabs" role="group" aria-label="任務類型">
            {(Object.keys(names) as Mode[]).map((k) => (
              <button
                key={k}
                className={k === mode ? "active" : ""}
                aria-pressed={k === mode}
                onClick={() => {
                  setMode(k);
                  setQuantity(1);
                  setLast(undefined);
                }}
              >
                {names[k]}
              </button>
            ))}
          </div>
          <div className="rate-line">
            <span>
              {selected?.rate} <small>· 費率 v{selected?.version}</small>
            </span>
            {mode !== "chat" && (
              <label>
                數量{" "}
                <input
                  aria-label="任務數量"
                  type="number"
                  min="1"
                  max={selected?.max ?? 1}
                  value={quantity}
                  onChange={(e) => setQuantity(Number(e.target.value))}
                />
                {selected?.unit}
              </label>
            )}
          </div>
          <p className="simulation-note">
            {mode === "chat"
              ? "對話由本機 Mock 模型回覆。"
              : "媒體任務為模擬執行回條，不產生實際圖片、影片或音檔。"}
          </p>
          <Paywall allowed={allowed}>
            <div
              className={"chat-shell " + (!allowed ? "locked" : "")}
              aria-disabled={!allowed}
              onClickCapture={(event) => {
                const button = (event.target as Element).closest("button");
                if (
                  button?.textContent?.trim() === "Retry" &&
                  !pending.current &&
                  !window.confirm("重新生成會建立新請求並再次計費，是否繼續？")
                ) {
                  event.preventDefault();
                  event.stopPropagation();
                }
              }}
            >
              <AgentProvider client={client}>
                <Thread
                  empty={
                    <div className="empty-state">
                      <div className="empty-orb">✦</div>
                      <h3>把想法放進來。</h3>
                      <p>
                        {mode === "chat"
                          ? "輸入訊息，開始第一段對話。"
                          : "描述你的創作，查看依用量結算的結果。"}
                      </p>
                    </div>
                  }
                />
                <fieldset disabled={!allowed || !me}>
                  <Composer
                    placeholder={
                      allowed ? "輸入你的想法…" : "完成付款後，即可開始…"
                    }
                    maxInputTokens={2000}
                    onAttachFile={() => {
                      setError("此示範只接受文字描述，尚未支援附件。");
                    }}
                  />
                </fieldset>
              </AgentProvider>
            </div>
          </Paywall>
          {last && (
            <div className="last-charge" role="status">
              <span>
                最近一次 ·{" "}
                {last.billing_status === "settled" ? "已結算" : "等待結算"}
              </span>
              <strong>
                {last.billing.charges.length
                  ? formatIntegerUnits(last.billing.charges[0].atoms)
                  : "—"}{" "}
                <small>credits</small>
              </strong>
              <span>
                {Object.entries(last.usage.meters ?? {})
                  .filter(
                    ([k, v]) =>
                      !["input", "cached", "cache_write", "output"].includes(
                        k,
                      ) && Number(v) > 0,
                  )
                  .map(([k, v]) =>
                    k === "video_ms"
                      ? `影片 ${Number(v) / 1000} 秒`
                      : k === "audio_ms"
                        ? `音訊 ${Number(v) / 1000} 秒`
                        : k === "images"
                          ? `圖片 ${v} 張`
                          : `任務 ${v} 次成功`,
                  )
                  .join(" · ")}
              </span>
            </div>
          )}
          {me && <UsageSummary usage={me.usage} />}
        </section>
      </div>
      <footer>
        SuperRouter × super-chat · 付款、額度與實際用量，在同一條旅程。
      </footer>
    </main>
  );
}
function message(e: unknown) {
  const code = e instanceof Error ? e.message : String(e);
  const messages: Record<string, string> = {
    session_required: "連線已到期，請重新連線。",
    invalid_key: "金鑰已失效或被撤銷，請重新連線。",
    customer_key_required: "請使用此應用的客戶金鑰，管理員金鑰不能登入。",
    service_version_changed:
      "費率已更新，本次未扣款。請查看新版費率，再次送出。",
    expected_service_version_required: "請重新整理以載入目前費率。",
    insufficient_balance:
      "可用 credits 不足以預留此請求，請降低用量或等待續期。",
    subscription_required: "此服務需要有效訂閱。",
    use_plan_change: "已有待付款或有效方案。可先取消待付款方案，再重新選擇。",
    period_already_paid: "當期已付款，請刷新查看額度。",
    origin_denied: "來源驗證失敗，請由本機應用重新操作。",
  };
  return messages[code] ?? "操作尚未完成：" + code;
}
createRoot(document.getElementById("root")!).render(<App />);
