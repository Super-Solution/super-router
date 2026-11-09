// Small HTTP helpers for the console and chat back ends. Server-side only.
import type { IncomingMessage, ServerResponse } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";

export class HttpError extends Error {
  constructor(
    public status: number,
    public code: string,
  ) {
    super(code);
  }
}

export function send(res: ServerResponse, status: number, data: unknown) {
  res.writeHead(status, {
    "content-type": "application/json; charset=utf-8",
    "cache-control": "no-store",
  });
  res.end(JSON.stringify(data));
}

export async function readJSON<T = Record<string, unknown>>(
  req: IncomingMessage,
  limit = 64_000,
): Promise<T> {
  let size = 0;
  const chunks: Buffer[] = [];
  for await (const chunk of req) {
    size += chunk.length;
    if (size > limit) throw new HttpError(413, "body_too_large");
    chunks.push(Buffer.from(chunk));
  }
  try {
    const value = JSON.parse(Buffer.concat(chunks).toString() || "{}");
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error();
    return value as T;
  } catch {
    throw new HttpError(400, "invalid_json");
  }
}

/**
 * These servers hold secrets and bind to loopback. Refuse requests whose Host or Origin
 * is not this server, so another website open in the browser cannot drive them.
 */
export function assertLocal(req: IncomingMessage, port: number) {
  const allowed = [`127.0.0.1:${port}`, `localhost:${port}`];
  if (!allowed.includes(req.headers.host ?? ""))
    throw new HttpError(403, "invalid_host");
  const origin = req.headers.origin;
  if (
    req.method !== "GET" &&
    origin &&
    !allowed.some((h) => origin === "http://" + h)
  )
    throw new HttpError(403, "invalid_origin");
}

const types: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".woff2": "font/woff2",
};

/** Serve a built Vite app from `dir`, falling back to index.html. */
export async function serveStatic(
  res: ServerResponse,
  dir: string,
  pathname: string,
) {
  const safe = normalize(pathname).replace(/^(\.\.[/\\])+/, "");
  const file = join(dir, safe === "/" ? "index.html" : safe);
  try {
    const body = await readFile(
      file.startsWith(dir) ? file : join(dir, "index.html"),
    );
    res.writeHead(200, {
      "content-type": types[extname(file)] ?? "application/octet-stream",
    });
    res.end(body);
  } catch {
    try {
      const body = await readFile(join(dir, "index.html"));
      res.writeHead(200, { "content-type": types[".html"] });
      res.end(body);
    } catch {
      res.writeHead(503, { "content-type": "text/plain" });
      res.end("Front end not built. Run: npm run build:superchat");
    }
  }
}

/** Call the router with a bearer key; throw HttpError with the router's error code. */
export async function routerFetch<T>(
  base: string,
  key: string,
  path: string,
  init: { method?: string; body?: unknown; idempotencyKey?: string } = {},
): Promise<T> {
  const response = await fetch(base + path, {
    method: init.method ?? "GET",
    redirect: "manual",
    headers: {
      authorization: "Bearer " + key,
      "content-type": "application/json",
      ...(init.idempotencyKey
        ? { "idempotency-key": init.idempotencyKey }
        : {}),
    },
    ...(init.body === undefined ? {} : { body: JSON.stringify(init.body) }),
  });
  if (!response.ok) {
    let code = "router_error";
    try {
      code =
        ((await response.json()) as { error?: { code?: string } }).error
          ?.code ?? code;
    } catch {
      /* non-JSON body */
    }
    throw new HttpError(response.status, code);
  }
  return (await response.json()) as T;
}
