import type { IncomingMessage, ServerResponse } from "node:http";
import { Readable } from "node:stream";
import { once } from "node:events";
import { defaultOnError, type AdapterOptions, type FetchHandler } from "./index.js";

export type { FetchHandler, AdapterOptions } from "./index.js";

/** Body that a framework already read and parsed (for example `express.json()`). */
export interface NodeBridgeOptions extends AdapterOptions {
  /** Full URL path (with query) of the incoming request. Default `req.url`. */
  url?: string;
  /** Body the framework already consumed. Re-encoded as JSON when it is an object. */
  parsedBody?: unknown;
}

const METHODS_WITHOUT_BODY = new Set(["GET", "HEAD"]);

function toHeaders(req: IncomingMessage): Headers {
  const headers = new Headers();
  for (const [name, value] of Object.entries(req.headers)) {
    if (value === undefined) continue;
    if (Array.isArray(value)) for (const v of value) headers.append(name, v);
    else headers.set(name, value);
  }
  return headers;
}

function toBody(req: IncomingMessage, parsed: unknown): BodyInit | null {
  if (METHODS_WITHOUT_BODY.has(req.method ?? "GET")) return null;
  if (parsed !== undefined) {
    if (typeof parsed === "string" || parsed instanceof Uint8Array) return parsed as BodyInit;
    // The framework already consumed the stream: hand over what it parsed.
    return JSON.stringify(parsed);
  }
  return Readable.toWeb(req) as unknown as BodyInit;
}

/** Build a web `Request` from a Node request. Aborts when the client disconnects before the response ends. */
export function toRequest(req: IncomingMessage, res: ServerResponse, options: NodeBridgeOptions = {}): Request {
  const controller = new AbortController();
  res.once("close", () => {
    if (!res.writableEnded) controller.abort();
  });
  const body = toBody(req, options.parsedBody);
  const init: RequestInit & { duplex?: "half" } = {
    method: req.method ?? "GET",
    headers: toHeaders(req),
    signal: controller.signal,
  };
  if (body !== null) {
    init.body = body;
    init.duplex = "half";
  }
  return new Request(new URL(options.url ?? req.url ?? "/", options.origin ?? "http://localhost"), init);
}

/** Write a web `Response` to a Node response: headers are flushed immediately, the body streams with backpressure. */
export async function writeResponse(res: ServerResponse, response: Response): Promise<void> {
  const headers: Record<string, string | string[]> = {};
  response.headers.forEach((value, name) => {
    if (name !== "set-cookie") headers[name] = value;
  });
  const cookies = response.headers.getSetCookie();
  if (cookies.length) headers["set-cookie"] = cookies;
  res.writeHead(response.status, response.statusText || undefined, headers);
  res.flushHeaders();
  if (!response.body) {
    res.end();
    return;
  }
  const reader = response.body.getReader();
  const closed = once(res, "close").then(() => {
    throw new Error("client_closed");
  });
  closed.catch(() => undefined);
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      if (res.destroyed) throw new Error("client_closed");
      if (!res.write(value)) await Promise.race([once(res, "drain"), closed]);
    }
    res.end();
  } catch (error) {
    await reader.cancel().catch(() => undefined);
    if (!res.destroyed) res.destroy();
    if (!(error instanceof Error) || error.message !== "client_closed") throw error;
  }
}

/** Serve a fetch handler on the plain `(req, res)` signature of `node:http`. */
export function nodeListener(handler: FetchHandler, options: AdapterOptions = {}) {
  return async (req: IncomingMessage, res: ServerResponse): Promise<void> => {
    await serve(handler, req, res, options);
  };
}

/** One request through the handler, shared by the Node, Express and Fastify adapters. */
export async function serve(
  handler: FetchHandler,
  req: IncomingMessage,
  res: ServerResponse,
  options: NodeBridgeOptions = {},
): Promise<void> {
  let response: Response;
  try {
    response = await handler(toRequest(req, res, options));
  } catch (error) {
    response = await (options.onError ?? defaultOnError)(error);
  }
  await writeResponse(res, response);
}
