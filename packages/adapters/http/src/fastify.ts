import type { IncomingMessage, ServerResponse } from "node:http";
import { serve, type AdapterOptions, type FetchHandler } from "./node.js";

export type { FetchHandler, AdapterOptions } from "./node.js";

/** Structural type for the parts of a Fastify instance used, loose enough for every Fastify major version. */
interface FastifyLike {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  addContentTypeParser: (...args: any[]) => unknown;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  route: (...args: any[]) => unknown;
}
interface FastifyLikeRequest {
  raw: IncomingMessage;
}
interface FastifyLikeReply {
  raw: ServerResponse;
  hijack: () => unknown;
}

/** Fastify plugin: `app.register(fastifyPlugin(handler), { prefix: "/api" })`. Fastify never parses the body, the handler reads it. */
export function fastifyPlugin(
  handler: FetchHandler,
  options: AdapterOptions = {},
) {
  return async (app: FastifyLike): Promise<void> => {
    // Leave every body unread so the handler can stream it.
    app.addContentTypeParser(
      "*",
      (_req: unknown, _payload: unknown, done: (e: null) => void) => done(null),
    );
    app.addContentTypeParser(
      "application/json",
      (_req: unknown, _payload: unknown, done: (e: null) => void) => done(null),
    );
    app.route({
      method: ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"],
      url: "/*",
      handler: async (req: FastifyLikeRequest, reply: FastifyLikeReply) => {
        reply.hijack();
        await serve(handler, req.raw, reply.raw, options);
      },
    });
  };
}
