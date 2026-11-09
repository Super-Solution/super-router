import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { AgentClient, AgentProvider } from "@super-solution/super-chat-react";
import {
  ContextBuilder,
  ToolRegistry,
  createOpenAIProvider,
  type Provider,
} from "@super-solution/super-chat-core";
import { Composer, Thread } from "@super-solution/super-chat-ui";
import { PORTS, formatCredits } from "@supersolutions/example-superchat-shared";
import "@super-solution/super-chat-ui/styles.css";
import "@supersolutions/example-superchat-shared/ui.css";
import "./style.css";

type Receipt = {
  at: string;
  request_id: string;
  replayed: boolean;
  execution: string;
  billing: string;
  charged: string | null;
  usage: { input?: number; output?: number } | null;
  error?: string;
};
type Wallet = {
  customer: string;
  key_hint: string;
  available: string;
  service: { version: number; price: Record<string, string | number> } | null;
  receipts: Receipt[];
};
type Msg = { role: "user" | "assistant"; content: string };

const CONSOLE_URL = `http://127.0.0.1:${PORTS.console}`;
const errors: Record<string, string> = {
  insufficient_balance:
    "Out of credits. Add credits for this customer in the developer console, then send again.",
  key_rejected:
    "The router rejected this key. It may have been revoked or expired; paste a new one.",
  invalid_key_format:
    "That doesn’t look like a SuperRouter key (sr_ followed by 64 hex characters).",
  not_connected: "Connect an API key first.",
  idempotency_conflict:
    "That idempotency key was already used for a different message.",
  no_candidate: "No provider could take this request.",
  upstream_failed:
    "Every provider failed before answering. You weren’t charged.",
  internal_error: "The chat server hit an error. Check its terminal.",
};
const explain = (code: string) =>
  errors[code] ?? `The router answered “${code}”.`;

class ApiError extends Error {}
async function api<T>(
  path: string,
  body?: unknown,
  signal?: AbortSignal,
): Promise<T> {
  const r = await fetch(path, {
    method: body === undefined ? "GET" : "POST",
    headers: { "content-type": "application/json" },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    ...(signal ? { signal } : {}),
  });
  const data = await r.json().catch(() => ({}));
  if (!r.ok) throw new ApiError((data as any)?.error?.code ?? "request_failed");
  return data as T;
}

function App() {
  const [wallet, setWallet] = useState<Wallet | null>(null);
  const [checked, setChecked] = useState(false);
  const [key, setKey] = useState("");
  const [error, setError] = useState("");
  const [connecting, setConnecting] = useState(false);
  const last = useRef<{ messages: Msg[]; key: string } | undefined>(undefined);
  const pending = useRef<{ fingerprint: string; key: string } | undefined>(
    undefined,
  );
  const [canReplay, setCanReplay] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setWallet(await api<Wallet>("/api/wallet"));
    } catch (e) {
      setWallet(null);
      if (e instanceof ApiError && e.message === "key_rejected")
        setError(explain(e.message));
    } finally {
      setChecked(true);
    }
  }, []);
  useEffect(() => {
    void refresh();
  }, [refresh]);

  const client = useMemo(() => {
    const base = createOpenAIProvider({
      transport: {
        kind: "custom",
        credentialSafe: true,
        fetch: async () => {
          throw new Error("unused_transport");
        },
      },
      dialect: "chat",
    });
    const provider: Provider = {
      ...base,
      async generate(req, opts) {
        const messages: Msg[] = req.messages
          .filter((m) => m.role === "user" || m.role === "assistant")
          .map((m) => ({
            role: m.role as Msg["role"],
            content: m.parts
              .filter((p) => p.type === "text")
              .map((p) => (p as { text: string }).text)
              .join("\n"),
          }))
          .filter((m) => m.content.trim());
        // A retry of the same conversation reuses its idempotency key, so it is never charged twice.
        const fingerprint = JSON.stringify(messages);
        if (pending.current?.fingerprint !== fingerprint)
          pending.current = { fingerprint, key: "chat-" + crypto.randomUUID() };
        setError("");
        try {
          const out = await api<{ reply: string | null; receipt: Receipt }>(
            "/api/chat",
            { messages, idempotency_key: pending.current.key },
            opts?.signal,
          );
          last.current = { messages, key: pending.current.key };
          setCanReplay(true);
          pending.current = undefined;
          void refresh();
          const r = out.receipt;
          const held =
            "The provider stopped before reporting usage. This request is held for reconciliation, so you haven’t been charged yet.";
          const text =
            r.billing === "pending"
              ? (out.reply ? out.reply + "\n\n— " : "") + held
              : (out.reply ??
                "This request was already handled. Nothing new was charged.");
          return {
            parts: [{ type: "text", text }],
            finishReason: "stop" as const,
            usage: {
              inputTokens: r.usage?.input ?? 0,
              outputTokens: r.usage?.output ?? 0,
              totalTokens: (r.usage?.input ?? 0) + (r.usage?.output ?? 0),
            },
            modelId: "chat",
          };
        } catch (e) {
          const code = e instanceof ApiError ? e.message : "request_failed";
          if (code !== "request_failed") pending.current = undefined;
          setError(explain(code));
          void refresh();
          throw e;
        }
      },
    };
    return new AgentClient({
      provider,
      model: "chat",
      contextBuilder: new ContextBuilder({
        identity: "",
        contextWindow: 16000,
      }),
      tools: new ToolRegistry(),
      toolResolution: { presets: [] },
      mode: "sync",
      maxSteps: 1,
    });
  }, [refresh]);

  async function connect(e: React.FormEvent) {
    e.preventDefault();
    setConnecting(true);
    setError("");
    try {
      setWallet(await api<Wallet>("/api/connect", { key }));
      setKey("");
    } catch (err) {
      setError(
        explain(err instanceof ApiError ? err.message : "request_failed"),
      );
    } finally {
      setConnecting(false);
    }
  }
  async function disconnect() {
    await api("/api/disconnect", {});
    setWallet(null);
    setCanReplay(false);
    last.current = undefined;
  }
  async function replay() {
    if (!last.current) return;
    setError("");
    try {
      await api("/api/chat", {
        messages: last.current.messages,
        idempotency_key: last.current.key,
      });
    } catch (e) {
      setError(explain(e instanceof ApiError ? e.message : "request_failed"));
    }
    void refresh();
  }

  return (
    <>
      <header className="topbar">
        <span className="brand">
          <Logo /> Superchat <small>with SuperRouter billing</small>
        </span>
        <span className="spacer" />
        <a
          className="btn btn--ghost"
          href={CONSOLE_URL}
          target="_blank"
          rel="noreferrer"
        >
          Developer console
        </a>
      </header>
      <main className="layout">
        <section className="chat" aria-label="Chat">
          <AgentProvider client={client}>
            <Thread
              empty={
                <div className="empty">
                  <h1>Every message is a metered request.</h1>
                  <p>
                    This chat runs on super-chat. Each message goes through
                    SuperRouter with your API key: credits are held first, then
                    settled on the tokens actually used.
                  </p>
                </div>
              }
            />
            <fieldset className="composer" disabled={!wallet}>
              <Composer
                placeholder={
                  wallet ? "Ask anything…" : "Connect an API key to start"
                }
                maxInputTokens={2000}
              />
            </fieldset>
          </AgentProvider>
        </section>

        <aside className="side" aria-label="Wallet">
          {error && (
            <p className="notice notice--bad" role="alert">
              {error}
            </p>
          )}
          {!checked ? null : !wallet ? (
            <form className="panel connect" onSubmit={connect}>
              <div className="panel__head">
                <h2>Connect an API key</h2>
              </div>
              <div className="panel__body stack">
                <p className="small">
                  Issue a key for a customer in the{" "}
                  <a href={CONSOLE_URL} target="_blank" rel="noreferrer">
                    developer console
                  </a>{" "}
                  and paste it here. The key stays on this app’s server; the
                  browser only keeps a session cookie.
                </p>
                <label className="field">
                  SuperRouter API key
                  <input
                    className="input mono"
                    type="password"
                    autoComplete="off"
                    spellCheck={false}
                    placeholder="sr_…"
                    value={key}
                    onChange={(e) => setKey(e.target.value)}
                  />
                </label>
                <button
                  className="btn btn--accent"
                  disabled={!key.trim() || connecting}
                >
                  {connecting ? "Checking key…" : "Connect"}
                </button>
              </div>
            </form>
          ) : (
            <>
              <section className="panel">
                <div className="panel__head">
                  <h2>{wallet.customer}</h2>
                  <button
                    className="btn btn--ghost btn--sm"
                    onClick={disconnect}
                  >
                    Disconnect
                  </button>
                </div>
                <div className="panel__body">
                  <p className="balance">
                    <span className="muted">Available</span>
                    <strong className="mono">
                      {formatCredits(wallet.available, 4)}
                    </strong>
                    <span className="muted">credits</span>
                  </p>
                  <p className="small muted">
                    Key <code>{wallet.key_hint}</code> · price v
                    {wallet.service?.version ?? "—"}
                  </p>
                </div>
              </section>

              <section className="panel">
                <div className="panel__head">
                  <h2>Receipts</h2>
                  <button
                    className="btn btn--ghost btn--sm"
                    disabled={!canReplay}
                    onClick={replay}
                    title="Send the last message again with the same Idempotency-Key"
                  >
                    Resend last (same key)
                  </button>
                </div>
                <ul className="receipts">
                  {wallet.receipts.length === 0 && (
                    <li className="muted small">No requests yet.</li>
                  )}
                  {wallet.receipts.map((r, i) => (
                    <li
                      key={r.request_id + r.at + i}
                      className={i === 0 ? "flash" : undefined}
                    >
                      <div className="receipt__top">
                        <span>{receiptTitle(r)}</span>
                        <strong className="mono">
                          {r.charged ? "−" + formatCredits(r.charged, 6) : "0"}
                        </strong>
                      </div>
                      <div className="receipt__meta muted">
                        {new Date(r.at).toLocaleTimeString()}
                        {r.usage &&
                          ` · ${r.usage.input ?? 0} in / ${r.usage.output ?? 0} out tokens`}
                        {r.request_id && (
                          <>
                            {" · "}
                            <code>{r.request_id.slice(0, 8)}</code>
                          </>
                        )}
                      </div>
                    </li>
                  ))}
                </ul>
              </section>
            </>
          )}
        </aside>
      </main>
    </>
  );
}

function receiptTitle(r: Receipt) {
  if (r.error === "insufficient_balance") return "Declined · out of credits";
  if (r.error) return "Declined · " + r.error;
  if (r.replayed) return "Replayed · not charged again";
  if (r.billing === "settled") return "Charged";
  if (r.billing === "released") return "Not charged · provider failed";
  if (r.billing === "pending") return "Held · awaiting reconciliation";
  return r.billing;
}

function Logo() {
  return (
    <svg width="20" height="20" viewBox="0 0 20 20" aria-hidden="true">
      <rect width="20" height="20" rx="5" fill="#0a0a0a" />
      <path
        d="M5.5 13.5L10 6.5l4.5 7"
        stroke="#ff5b14"
        strokeWidth="2.2"
        fill="none"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

createRoot(document.getElementById("root")!).render(<App />);
