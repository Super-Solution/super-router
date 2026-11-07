/** A fetch-style handler: the only contract a service has to implement to run under any framework. */
export type FetchHandler = (request: Request) => Response | Promise<Response>;

export interface AdapterOptions {
  /** Origin used to build absolute request URLs when the server cannot know it. Default `http://localhost`. */
  origin?: string;
  /** Called for an error thrown by the handler. Default: respond 500 with an empty JSON error. */
  onError?: (error: unknown) => Response | Promise<Response>;
}

export const defaultOnError = (): Response =>
  new Response(JSON.stringify({ error: { code: "internal_error" } }), {
    status: 500,
    headers: { "content-type": "application/json" },
  });
