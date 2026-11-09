// Back end for the chat app. The browser pastes a SuperRouter API key once; the key then
// lives only in this server's memory, bound to an HttpOnly session cookie.
import {
  createServer,
  type IncomingMessage,
  type ServerResponse,
} from "node:http";
import { once } from "node:events";
import { randomBytes } from "node:crypto";
import { fileURLToPath } from "node:url";
import {
  CREDIT_ASSET,
  HOST,
  MAX_OUTPUT_TOKENS,
  PORTS,
  ROUTER_URL,
  SERVICE_ID,
} from "@supersolutions/example-superchat-shared";
import {
  HttpError,
  assertLocal,
  readJSON,
  routerFetch,
  send,
  serveStatic,
} from "@supersolutions/example-superchat-shared/http";

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
type Session = {
  key: string;
  customer: string;
  hint: string;
  receipts: Receipt[];
};
type Message = { role: "user" | "assistant" | "system"; content: string };

const COOKIE = "superchat_sid";
const KEY = /^sr_[a-f0-9]{64}$/;
const IDEM = /^[A-Za-z0-9_.:-]{8,120}$/;

function sessionId(req: IncomingMessage) {
  const match = /(?:^|;\s*)superchat_sid=([a-f0-9]{48})/.exec(
    req.headers.cookie ?? "",
  );
  return match?.[1];
}

export async function startChat(options: { quiet?: boolean } = {}) {
  const sessions = new Map<string, Session>();
  const dist = fileURLToPath(new URL("./dist/", import.meta.url));

  const current = (req: IncomingMessage) => {
    const s = sessions.get(sessionId(req) ?? "");
    if (!s) throw new HttpError(401, "not_connected");
    return s;
  };
  /** A revoked or expired key ends the session instead of failing on every call. */
  const guard = async <T>(
    req: IncomingMessage,
    s: Session,
    fn: () => Promise<T>,
  ) => {
    try {
      return await fn();
    } catch (e) {
      if (e instanceof HttpError && e.status === 401) {
        sessions.delete(sessionId(req) ?? "");
        throw new HttpError(401, "key_rejected");
      }
      throw e;
    }
  };

  async function wallet(s: Session) {
    const [credits, services] = await Promise.all([
      routerFetch<{ buckets: { asset: string; available: string }[] }>(
        ROUTER_URL,
        s.key,
        "/v1/credits",
      ),
      routerFetch<{
        data: {
          id: string;
          version: number;
          price: Record<string, string | number>;
        }[];
      }>(ROUTER_URL, s.key, "/v1/services"),
    ]);
    const available = credits.buckets
      .filter((b) => b.asset === CREDIT_ASSET)
      .reduce((sum, b) => sum + BigInt(b.available), 0n);
    return {
      customer: s.customer,
      key_hint: s.hint,
      available: available.toString(),
      service: services.data.find((x) => x.id === SERVICE_ID) ?? null,
      receipts: s.receipts,
    };
  }

  async function chat(
    s: Session,
    messages: Message[],
    idempotencyKey: string,
  ): Promise<{ reply: string | null; receipt: Receipt }> {
    const response = await fetch(ROUTER_URL + "/v1/inferences", {
      method: "POST",
      redirect: "manual",
      headers: {
        authorization: "Bearer " + s.key,
        "content-type": "application/json",
        "idempotency-key": idempotencyKey,
      },
      body: JSON.stringify({
        service: SERVICE_ID,
        customer_id: s.customer,
        input: { messages },
        limits: { max_output_tokens: MAX_OUTPUT_TOKENS, timeout_ms: 30000 },
        stream: false,
      }),
    });
    const at = new Date().toISOString();
    if (response.status === 303) {
      // Same key and body as an earlier request: the router returns that request, not a new answer.
      const id =
        (response.headers.get("location") ?? "").split("/").pop() ?? "";
      const status = await routerFetch<{
        execution_status: string;
        billing_status: string;
      }>(ROUTER_URL, s.key, "/v1/requests/" + encodeURIComponent(id));
      return {
        reply: null,
        receipt: {
          at,
          request_id: id,
          replayed: true,
          execution: status.execution_status,
          billing: status.billing_status,
          charged: null,
          usage: null,
        },
      };
    }
    const data = (await response.json().catch(() => ({}))) as any;
    if (!response.ok)
      throw new HttpError(response.status, data?.error?.code ?? "router_error");
    const charged =
      (
        data.billing?.charges as { kind: string; atoms: string }[] | undefined
      )?.find((c) => c.kind === "customer_sale")?.atoms ?? null;
    return {
      reply: data.output?.content || null,
      receipt: {
        at,
        request_id: data.id,
        replayed: false,
        execution: data.execution_status,
        billing: data.billing_status,
        charged,
        usage: data.usage?.meters ?? null,
      },
    };
  }

  const server = createServer(
    async (req: IncomingMessage, res: ServerResponse) => {
      const url = new URL(req.url ?? "/", "http://localhost");
      try {
        assertLocal(req, PORTS.chat);
        const route = `${req.method} ${url.pathname}`;
        if (route === "POST /api/connect") {
          const { key } = await readJSON<{ key?: unknown }>(req);
          if (typeof key !== "string" || !KEY.test(key.trim()))
            throw new HttpError(400, "invalid_key_format");
          const token = key.trim();
          const me = await routerFetch<{
            customer_id: string;
            scopes: string[];
          }>(ROUTER_URL, token, "/v1/me").catch((e) => {
            throw e instanceof HttpError && e.status === 401
              ? new HttpError(401, "key_rejected")
              : e;
          });
          if (!me.scopes.includes("invoke"))
            throw new HttpError(403, "key_cannot_invoke");
          const sid = randomBytes(24).toString("hex");
          const old = sessionId(req);
          if (old) sessions.delete(old);
          sessions.set(sid, {
            key: token,
            customer: me.customer_id,
            hint: token.slice(0, 7) + "…" + token.slice(-4),
            receipts: [],
          });
          res.setHeader(
            "set-cookie",
            `${COOKIE}=${sid}; HttpOnly; SameSite=Strict; Path=/`,
          );
          return send(res, 200, await wallet(sessions.get(sid)!));
        }
        if (route === "POST /api/disconnect") {
          sessions.delete(sessionId(req) ?? "");
          res.setHeader(
            "set-cookie",
            `${COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0`,
          );
          return send(res, 200, { ok: true });
        }
        if (route === "GET /api/wallet") {
          const s = current(req);
          return send(res, 200, await guard(req, s, () => wallet(s)));
        }
        if (route === "POST /api/chat") {
          const s = current(req);
          const body = await readJSON<{
            messages?: unknown;
            idempotency_key?: unknown;
          }>(req);
          const messages = Array.isArray(body.messages)
            ? (body.messages as Message[]).filter(
                (m) =>
                  m &&
                  ["user", "assistant"].includes(m.role) &&
                  typeof m.content === "string" &&
                  m.content.trim(),
              )
            : [];
          if (
            !messages.length ||
            messages.length > 40 ||
            messages.some((m) => m.content.length > 8000)
          )
            throw new HttpError(400, "invalid_messages");
          if (
            typeof body.idempotency_key !== "string" ||
            !IDEM.test(body.idempotency_key)
          )
            throw new HttpError(400, "invalid_idempotency_key");
          try {
            const out = await guard(req, s, () =>
              chat(s, messages, body.idempotency_key as string),
            );
            s.receipts.unshift(out.receipt);
            s.receipts.length = Math.min(s.receipts.length, 20);
            return send(res, 200, out);
          } catch (e) {
            if (e instanceof HttpError && e.code !== "key_rejected") {
              s.receipts.unshift({
                at: new Date().toISOString(),
                request_id: "",
                replayed: false,
                execution: "rejected",
                billing: "not_charged",
                charged: null,
                usage: null,
                error: e.code,
              });
              s.receipts.length = Math.min(s.receipts.length, 20);
            }
            throw e;
          }
        }
        if (url.pathname.startsWith("/api/"))
          throw new HttpError(404, "not_found");
        await serveStatic(res, dist, url.pathname);
      } catch (error) {
        const e =
          error instanceof HttpError
            ? error
            : new HttpError(500, "internal_error");
        if (!(error instanceof HttpError)) console.error("[chat]", error);
        send(res, e.status, { error: { code: e.code } });
      }
    },
  );
  server.listen(PORTS.chat, HOST);
  await once(server, "listening");
  if (!options.quiet) console.log(`[chat]    http://${HOST}:${PORTS.chat}`);
  return {
    server,
    close: () => new Promise<void>((r) => server.close(() => r())),
  };
}
