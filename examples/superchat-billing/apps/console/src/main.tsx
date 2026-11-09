import { useCallback, useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  PORTS,
  SCENARIOS,
  formatCredits,
} from "@supersolutions/example-superchat-shared";
import "@supersolutions/example-superchat-shared/ui.css";
import "./style.css";

type Key = {
  id: string;
  customer_id: string;
  token_hint: string;
  created_at: string;
  expires_at: string;
  revoked_at?: string;
};
type Overview = {
  workspace: string;
  router: string;
  service: {
    id: string;
    version: number;
    price: Record<string, string | number>;
  } | null;
  customers: { id: string; available: string }[];
  keys: Key[];
  scenario: string;
};
type Attempt = { offering: string; status: string; cost: string | null };
type Req = {
  id: string;
  customer: string;
  service: string;
  service_version: number;
  execution: string;
  billing: string;
  provider_cost: string;
  created_at: string;
  attempts: Attempt[];
  settled: Record<string, string>;
};

const CHAT_URL = `http://127.0.0.1:${PORTS.chat}`;
const messages: Record<string, string> = {
  invalid_customer_id: "Use 2–41 lowercase letters, digits, - or _.",
  invalid_credits: "Enter a positive amount, up to 9 decimals.",
  invalid_rate: "Tokens per credit must be a whole number above zero.",
  router_error: "The router returned an error.",
  internal_error:
    "Something went wrong in the console server. Check its terminal.",
};

async function api<T>(path: string, body?: unknown): Promise<T> {
  const r = await fetch(path, {
    method: body === undefined ? "GET" : "POST",
    headers: { "content-type": "application/json" },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  const data = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error((data as any)?.error?.code ?? "request_failed");
  return data as T;
}
const explain = (e: unknown) => {
  const code = e instanceof Error ? e.message : String(e);
  return messages[code] ?? `Error: ${code}`;
};

/** Tokens one credit buys for a per-token price in atoms (1 credit = 1e9 atoms). */
function perCredit(
  atoms: string | number | undefined,
  per: string | number | undefined,
) {
  if (atoms === undefined) return "—";
  const a = BigInt(atoms),
    p = BigInt(per ?? 1);
  if (a === 0n) return "free";
  const units = (1_000_000_000n * p) / a;
  return units.toLocaleString("en-US") + ((1_000_000_000n * p) % a ? "+" : "");
}

function statusPill(r: Req) {
  if (r.billing === "settled")
    return <span className="pill pill--ok">Settled</span>;
  if (r.billing === "released")
    return <span className="pill">Released · not charged</span>;
  if (r.billing === "pending" || r.billing === "reserved")
    return <span className="pill pill--warn">Pending reconciliation</span>;
  return <span className="pill">{r.billing}</span>;
}

function App() {
  const [data, setData] = useState<Overview>();
  const [requests, setRequests] = useState<Req[]>([]);
  const [error, setError] = useState("");
  const [issued, setIssued] = useState<{ token: string; customer: string }>();
  const [copied, setCopied] = useState(false);
  const [busy, setBusy] = useState("");
  const seen = useRef(new Set<string>());
  const [fresh, setFresh] = useState(new Set<string>());

  const load = useCallback(async () => {
    try {
      setData(await api<Overview>("/api/overview"));
    } catch (e) {
      setError(explain(e));
    }
  }, []);
  const loadRequests = useCallback(async () => {
    try {
      const r = await api<{ data: Req[] }>("/api/requests");
      const added = new Set(
        r.data
          .filter((x) => seen.current.size && !seen.current.has(x.id))
          .map((x) => x.id),
      );
      r.data.forEach((x) => seen.current.add(x.id));
      if (added.size) {
        setFresh(added);
        void load();
      }
      setRequests(r.data);
    } catch {
      /* the log refreshes again shortly */
    }
  }, [load]);

  useEffect(() => {
    void load();
    void loadRequests();
    const t = setInterval(loadRequests, 2000);
    return () => clearInterval(t);
  }, [load, loadRequests]);

  const act = async (label: string, fn: () => Promise<unknown>) => {
    setBusy(label);
    setError("");
    try {
      await fn();
      await load();
    } catch (e) {
      setError(explain(e));
    } finally {
      setBusy("");
    }
  };

  if (!data)
    return (
      <main className="wrap">
        <p className="muted">{error || "Loading the workspace…"}</p>
      </main>
    );

  const price = data.service?.price ?? {};
  return (
    <>
      <header className="topbar">
        <span className="brand">
          <Logo /> SuperRouter <small>Developer console</small>
        </span>
        <span className="chip mono">{data.workspace}</span>
        <span className="chip">
          Router <code>{data.router}</code>
        </span>
        <span className="spacer" />
        <a
          className="btn btn--ghost"
          href={CHAT_URL}
          target="_blank"
          rel="noreferrer"
        >
          Open the chat app
        </a>
      </header>

      <main className="wrap grid">
        {error && (
          <p className="notice notice--bad span-all" role="alert">
            {error}
          </p>
        )}

        {issued && (
          <section
            className="notice notice--accent span-all issued"
            aria-live="polite"
          >
            <div>
              <strong>New API key for {issued.customer}.</strong> Copy it now;
              it won’t be shown again. Paste it into the chat app to call the
              router as this customer.
            </div>
            <div className="issued__row">
              <code className="issued__token">{issued.token}</code>
              <button
                className="btn btn--accent"
                onClick={async () => {
                  await navigator.clipboard
                    .writeText(issued.token)
                    .catch(() => undefined);
                  setCopied(true);
                }}
              >
                {copied ? "Copied" : "Copy key"}
              </button>
              <a
                className="btn btn--ghost"
                href={CHAT_URL}
                target="_blank"
                rel="noreferrer"
              >
                Open chat
              </a>
              <button
                className="btn btn--ghost"
                onClick={() => setIssued(undefined)}
              >
                Done
              </button>
            </div>
          </section>
        )}

        <Customers
          data={data}
          busy={busy}
          act={act}
          onIssued={(token, customer) => {
            setCopied(false);
            setIssued({ token, customer });
          }}
        />

        <section className="panel">
          <div className="panel__head">
            <h2>Price of the chat Service</h2>
            <span className="chip">version {data.service?.version ?? "—"}</span>
          </div>
          <div className="panel__body stack">
            <p className="muted small">
              What your customers pay, in credits. Publishing creates a new
              Service version; requests already accepted keep their old price.
            </p>
            <dl className="rates">
              <div>
                <dt>Input</dt>
                <dd>
                  <strong>{perCredit(price.input, price.per)}</strong> tokens
                  per credit
                </dd>
              </div>
              <div>
                <dt>Output</dt>
                <dd>
                  <strong>{perCredit(price.output, price.per)}</strong> tokens
                  per credit
                </dd>
              </div>
            </dl>
            <RateForm busy={busy} act={act} />
          </div>
        </section>

        <section className="panel">
          <div className="panel__head">
            <h2>Provider behaviour</h2>
            <span className="chip">mock providers</span>
          </div>
          <div className="panel__body stack">
            <p className="muted small">
              Make the local providers misbehave, then send a chat message and
              watch the log.
            </p>
            <div
              className="scenarios"
              role="radiogroup"
              aria-label="Provider behaviour"
            >
              {SCENARIOS.map((s) => (
                <label
                  key={s.id}
                  className={
                    "scenario" + (data.scenario === s.id ? " is-on" : "")
                  }
                >
                  <input
                    type="radio"
                    name="scenario"
                    checked={data.scenario === s.id}
                    onChange={() =>
                      act("scenario", () =>
                        api("/api/scenario", { scenario: s.id }),
                      )
                    }
                  />
                  <span className="scenario__label">{s.label}</span>
                  <span className="scenario__note">{s.note}</span>
                </label>
              ))}
            </div>
          </div>
        </section>

        <section className="panel span-all">
          <div className="panel__head">
            <h2>Requests</h2>
            <span className="muted small">
              Live · every request across customers, newest first
            </span>
          </div>
          <div className="table-box">
            <table>
              <thead>
                <tr>
                  <th>Time</th>
                  <th>Customer</th>
                  <th>Service</th>
                  <th>Attempts</th>
                  <th className="r">Provider cost (USD)</th>
                  <th className="r">Charged (credits)</th>
                  <th>Status</th>
                </tr>
              </thead>
              <tbody>
                {requests.length === 0 && (
                  <tr>
                    <td colSpan={7} className="muted">
                      No requests yet. Issue a key, open the chat app and send a
                      message.
                    </td>
                  </tr>
                )}
                {requests.map((r) => (
                  <tr
                    key={r.id}
                    className={fresh.has(r.id) ? "flash" : undefined}
                  >
                    <td className="mono muted">
                      {new Date(r.created_at).toLocaleTimeString()}
                    </td>
                    <td>{r.customer}</td>
                    <td className="mono">
                      {r.service}@v{r.service_version}
                    </td>
                    <td>
                      <div className="attempts">
                        {r.attempts.map((a, i) => (
                          <span
                            key={i}
                            className={"pill " + attemptTone(a.status)}
                            title={a.status}
                          >
                            {a.offering} · {attemptLabel(a.status)}
                          </span>
                        ))}
                      </div>
                    </td>
                    <td className="r mono">
                      {formatCredits(r.provider_cost, 6)}
                    </td>
                    <td className="r mono">
                      {r.settled.customer_sale &&
                      BigInt(r.settled.customer_sale) > 0n ? (
                        <strong>
                          {formatCredits(r.settled.customer_sale, 6)}
                        </strong>
                      ) : (
                        <span className="muted">0</span>
                      )}
                    </td>
                    <td>{statusPill(r)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </section>
      </main>
    </>
  );
}

const attemptLabel = (s: string) =>
  ({
    succeeded: "ok",
    failed: "did not run",
    unknown: "outcome unknown",
    dispatched: "running",
  })[s] ?? s;
const attemptTone = (s: string) =>
  s === "succeeded"
    ? "pill--ok"
    : s === "unknown" || s === "dispatched"
      ? "pill--warn"
      : "pill--bad";

function Customers({
  data,
  busy,
  act,
  onIssued,
}: {
  data: Overview;
  busy: string;
  act: (label: string, fn: () => Promise<unknown>) => Promise<void>;
  onIssued: (token: string, customer: string) => void;
}) {
  const [topups, setTopups] = useState<Record<string, string>>({});
  const [newId, setNewId] = useState("");
  return (
    <section className="panel span-all">
      <div className="panel__head">
        <h2>Customers and API keys</h2>
        <form
          className="inline"
          onSubmit={(e) => {
            e.preventDefault();
            void act("customer", () =>
              api("/api/customers", { id: newId.trim() }),
            ).then(() => setNewId(""));
          }}
        >
          <label className="visually-hidden" htmlFor="new-customer">
            New customer ID
          </label>
          <input
            id="new-customer"
            className="input"
            placeholder="new-customer-id"
            value={newId}
            onChange={(e) => setNewId(e.target.value)}
          />
          <button
            className="btn btn--ghost"
            disabled={!newId.trim() || busy === "customer"}
          >
            Add customer
          </button>
        </form>
      </div>
      <div className="table-box">
        <table>
          <thead>
            <tr>
              <th>Customer</th>
              <th className="r">Available credits</th>
              <th>Top up</th>
              <th>API keys</th>
            </tr>
          </thead>
          <tbody>
            {data.customers.map((c) => {
              const keys = data.keys.filter((k) => k.customer_id === c.id);
              return (
                <tr key={c.id}>
                  <td>
                    <strong>{c.id}</strong>
                  </td>
                  <td className="r mono">
                    <strong>{formatCredits(c.available, 4)}</strong>
                  </td>
                  <td>
                    <form
                      className="inline"
                      onSubmit={(e) => {
                        e.preventDefault();
                        void act("grant:" + c.id, () =>
                          api("/api/grants", {
                            customer_id: c.id,
                            credits: topups[c.id] || "1",
                            idempotency_key: crypto.randomUUID(),
                          }),
                        );
                      }}
                    >
                      <label
                        className="visually-hidden"
                        htmlFor={"topup-" + c.id}
                      >
                        Credits for {c.id}
                      </label>
                      <input
                        id={"topup-" + c.id}
                        className="input input--num"
                        inputMode="decimal"
                        placeholder="1"
                        value={topups[c.id] ?? ""}
                        onChange={(e) =>
                          setTopups({ ...topups, [c.id]: e.target.value })
                        }
                      />
                      <button
                        className="btn btn--ghost btn--sm"
                        disabled={busy === "grant:" + c.id}
                      >
                        Add credits
                      </button>
                    </form>
                  </td>
                  <td>
                    <div className="keys">
                      {keys.map((k) => (
                        <div key={k.id} className="key">
                          <code>{k.token_hint}</code>
                          {k.revoked_at ? (
                            <span className="pill pill--bad">Revoked</span>
                          ) : (
                            <>
                              <span className="pill pill--ok">Active</span>
                              <button
                                className="btn btn--danger btn--sm"
                                disabled={busy === "revoke:" + k.id}
                                onClick={() =>
                                  act("revoke:" + k.id, () =>
                                    api(`/api/keys/${k.id}/revoke`, {}),
                                  )
                                }
                              >
                                Revoke
                              </button>
                            </>
                          )}
                        </div>
                      ))}
                      <button
                        className="btn btn--accent btn--sm"
                        disabled={busy === "key:" + c.id}
                        onClick={() =>
                          act("key:" + c.id, async () => {
                            const k = await api<{ token: string }>(
                              "/api/keys",
                              { customer_id: c.id },
                            );
                            onIssued(k.token, c.id);
                          })
                        }
                      >
                        Issue API key
                      </button>
                    </div>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </section>
  );
}

function RateForm({
  busy,
  act,
}: {
  busy: string;
  act: (label: string, fn: () => Promise<unknown>) => Promise<void>;
}) {
  const [input, setInput] = useState("50000");
  const [output, setOutput] = useState("20000");
  return (
    <form
      className="rate-form"
      onSubmit={(e) => {
        e.preventDefault();
        void act("rate", () =>
          api("/api/rate", {
            input_tokens_per_credit: input,
            output_tokens_per_credit: output,
          }),
        );
      }}
    >
      <label className="field">
        Input tokens per credit
        <input
          className="input"
          inputMode="numeric"
          value={input}
          onChange={(e) => setInput(e.target.value)}
        />
      </label>
      <label className="field">
        Output tokens per credit
        <input
          className="input"
          inputMode="numeric"
          value={output}
          onChange={(e) => setOutput(e.target.value)}
        />
      </label>
      <button className="btn btn--ghost" disabled={busy === "rate"}>
        Publish new price
      </button>
    </form>
  );
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
