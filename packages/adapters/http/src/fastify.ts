import type { IncomingMessage, ServerResponse } from "node:http";
import { serve, type AdapterOptions, type FetchHandler } from "./node.js";

export type { FetchHandler, AdapterOptions } from "./node.js";

/** Minimal structural types so this package does not depend on fastify. */
interface FastifyLikeRequest { raw: IncomingMessage }
interface FastifyLikeReply { raw: ServerResponse; hijack: () => unknown }
interface FastifyLike {
  addContentTypeParser: (type: string, parser: (req: IncomingMessage, payload: unknown, done: (e: Error | null) => void) => void) => void;
  route: (opts: { method: string[]; url: string; handler: (req: FastifyLikeRequest, reply: FastifyLikeReply) => Promise<void> }) => void;
}

/** Fastify plugin: `app.register(fastifyPlugin(handler), { prefix: "/api" })`. Fastify never parses the body, the handler reads it. */
export function fastifyPlugin(handler: FetchHandler, options: AdapterOptions = {}) {
  return async (app: FastifyLike): Promise<void> => {
    // Leave every body unread so the handler can stream it.
    app.addContentTypeParser("*", (_req, _payload, done) => done(null));
    app.addContentTypeParser("application/json", (_req, _payload, done) => done(null));
    app.route({
      method: ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"],
      url: "/*",
      handler: async (req, reply) => {
        reply.hijack();
        await serve(handler, req.raw, reply.raw, options);
      },
    });
  };
}
