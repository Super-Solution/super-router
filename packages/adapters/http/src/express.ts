import type { IncomingMessage, ServerResponse } from "node:http";
import { serve, type AdapterOptions, type FetchHandler } from "./node.js";

export type { FetchHandler, AdapterOptions } from "./node.js";

interface ExpressRequest extends IncomingMessage {
  originalUrl?: string;
  body?: unknown;
}
type Next = (error?: unknown) => void;

/** Express middleware / route handler. Works with or without a body parser and under any mount prefix. */
export function expressHandler(
  handler: FetchHandler,
  options: AdapterOptions = {},
) {
  return (req: ExpressRequest, res: ServerResponse, next: Next): void => {
    // A body parser leaves `{}` for requests without a body; only trust it when it parsed something.
    const parsed =
      req.body !== undefined &&
      !(
        typeof req.body === "object" &&
        Object.keys(req.body as object).length === 0
      )
        ? req.body
        : undefined;
    serve(handler, req, res, {
      ...options,
      url: req.originalUrl ?? req.url,
      parsedBody: parsed,
    }).catch(next);
  };
}
