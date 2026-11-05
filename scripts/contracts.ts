import { CreditRateConfig } from "@superrouter/core";
import { writeFile, mkdir } from "node:fs/promises";
import {
  z,
  Invocation,
  Service,
  EventSchema,
  MemberInput,
  CustomerInput,
  KeyInput,
  PlanInput,
  SubscriptionInput,
  PlanChangeInput,
  PaymentInput,
  CreditGrantInput,
} from "@superrouter/contracts";
await mkdir("docs/generated", { recursive: true });
const schema = (s: z.ZodType) => z.toJSONSchema(s, { target: "draft-2020-12" });
const schemas = {
  CreditRateConfig: schema(CreditRateConfig),
  Invocation: schema(Invocation),
  Service: schema(Service),
  DomainEvent: schema(EventSchema),
  MemberInput: schema(MemberInput),
  CustomerInput: schema(CustomerInput),
  KeyInput: schema(KeyInput),
  PlanInput: schema(PlanInput),
  SubscriptionInput: schema(SubscriptionInput),
  PlanChangeInput: schema(PlanChangeInput),
  PaymentInput: schema(PaymentInput),
  CreditGrantInput: schema(CreditGrantInput),
};
const idem = {
  name: "Idempotency-Key",
  in: "header",
  required: true,
  schema: { type: "string", minLength: 1, maxLength: 200 },
};
const errors = Object.fromEntries(
  [400, 401, 402, 403, 404, 409, 422, 429, 500, 503].map((x) => [
    String(x),
    {
      description:
        "See error.code; no automatic payer switch or new idempotency key.",
    },
  ]),
);
const api = {
  openapi: "3.1.0",
  info: {
    title: "SuperRouter P1 + P2 billing slice API",
    version: "0.1.0",
    description:
      "Implemented routes. P2 control routes require database-authenticated ControlPlane. Hosted payments support Stripe, OEN and Epay V1. Broader target design is docs/API_SPEC.md.",
  },
  security: [{ bearer: [] }],
  components: {
    securitySchemes: { bearer: { type: "http", scheme: "bearer" } },
    schemas,
  },
  paths: {
    ...Object.fromEntries(
      ["checkout", "reconcile"].map((action) => [
        "/v1/admin/payment-orders/{id}/" + action,
        {
          post: {
            operationId: action + "Payment",
            requestBody: {
              required: action === "reconcile",
              content: {
                "application/json": {
                  schema: {
                    type: "object",
                    properties:
                      action === "reconcile"
                        ? {
                            reference: {
                              type: "string",
                              minLength: 1,
                              maxLength: 255,
                            },
                          }
                        : {},
                    additionalProperties: false,
                  },
                },
              },
            },
            parameters: [
              idem,
              {
                name: "id",
                in: "path",
                required: true,
                schema: { type: "string" },
              },
            ],
            responses: {
              "200": {
                description:
                  "Hosted checkout or authoritative reconciliation result.",
              },
              ...errors,
            },
          },
        },
      ]),
    ),
    "/v1/integrations/payments/{id}/notify": Object.fromEntries(
      ["get", "post"].map((method) => [
        method,
        {
          security: [],
          parameters: [
            {
              name: "id",
              in: "path",
              required: true,
              schema: { type: "string" },
            },
          ],
          description:
            "Provider-authenticated callback. GET Epay V1 signed query; POST Stripe/OEN raw signed JSON. No bearer key can assert payment.",
          responses: {
            "200": {
              description:
                "Provider acknowledgement after authoritative lookup and atomic settlement.",
            },
            ...errors,
          },
        },
      ]),
    ),
    "/v1/inferences": {
      post: {
        operationId: "infer",
        parameters: [idem],
        requestBody: {
          required: true,
          content: {
            "application/json": {
              schema: { $ref: "#/components/schemas/Invocation" },
            },
          },
        },
        responses: {
          "200": {
            description: "JSON result or SSE events; billing may be pending.",
          },
          "303": {
            description:
              "Replay: Location points to request status; output not retained.",
          },
          ...errors,
        },
      },
    },
    "/v1/services": {
      get: {
        operationId: "listServices",
        responses: {
          "200": {
            description: "Authorized published Service summaries (P1 max 100).",
          },
          ...errors,
        },
      },
    },
    "/v1/requests/{id}": {
      get: {
        operationId: "getRequest",
        parameters: [
          {
            name: "id",
            in: "path",
            required: true,
            schema: { type: "string" },
          },
        ],
        responses: {
          "200": {
            description: "Execution and billing states only; no stored answer.",
          },
          ...errors,
        },
      },
    },
    "/v1/requests/{id}/cancel": {
      post: {
        operationId: "cancelRequest",
        parameters: [
          idem,
          {
            name: "id",
            in: "path",
            required: true,
            schema: { type: "string" },
          },
        ],
        responses: {
          "202": {
            description:
              "Cancellation intent recorded; not confirmation of upstream cancellation.",
          },
          ...errors,
        },
      },
    },
    "/v1/admin/services/{id}/credit-rates": {
      post: {
        operationId: "publishCreditRateVersion",
        parameters: [
          idem,
          {
            name: "id",
            in: "path",
            required: true,
            schema: { type: "string" },
          },
        ],
        requestBody: {
          required: true,
          content: {
            "application/json": {
              schema: {
                type: "object",
                additionalProperties: false,
                required: ["expected_version", "config"],
                properties: {
                  expected_version: { type: "integer", minimum: 1 },
                  config: { $ref: "#/components/schemas/CreditRateConfig" },
                },
              },
            },
          },
        },
        responses: {
          "201": {
            description:
              "New immutable customer-sale pricing version; platform supply unchanged.",
          },
          ...errors,
        },
      },
    },
    "/v1/admin/service-versions": {
      post: {
        operationId: "publishServiceVersion",
        parameters: [idem],
        requestBody: {
          required: true,
          content: {
            "application/json": {
              schema: { $ref: "#/components/schemas/Service" },
            },
          },
        },
        responses: {
          "201": {
            description:
              "Immutable version published; P1 single-step workflow.",
          },
          ...errors,
        },
      },
    },
    "/v1/admin/balance": {
      get: {
        operationId: "getBoundBalances",
        responses: {
          "200": {
            description: "Bound customer and platform account balances.",
          },
          ...errors,
        },
      },
    },
    "/v1/admin/routing/dry-run": {
      post: {
        operationId: "dryRun",
        parameters: [idem],
        requestBody: {
          required: true,
          content: {
            "application/json": {
              schema: { $ref: "#/components/schemas/Invocation" },
            },
          },
        },
        responses: {
          "200": {
            description:
              "Eligible scores and excluded reasons. No upstream calls or reservations.",
          },
          ...errors,
        },
      },
    },
  },
};
const paths = api.paths as Record<string, unknown>;
const post = (
  operationId: string,
  schemaName?: string,
  description = "Idempotent command; API key token is returned only on initial creation.",
) => ({
  operationId,
  parameters: [idem],
  ...(schemaName
    ? {
        requestBody: {
          required: true,
          content: {
            "application/json": {
              schema: { $ref: "#/components/schemas/" + schemaName },
            },
          },
        },
      }
    : {}),
  responses: { "200": { description }, "201": { description }, ...errors },
});
const pathParam = {
  name: "id",
  in: "path",
  required: true,
  schema: { type: "string" },
};
for (const [path, name] of Object.entries({
  members: "MemberInput",
  customers: "CustomerInput",
  keys: "KeyInput",
  plans: "PlanInput",
  subscriptions: "SubscriptionInput",
  "credits/grants": "CreditGrantInput",
}))
  paths["/v1/admin/" + path] = {
    post: post("create_" + path.replaceAll("/", "_"), name),
  };
for (const action of ["revoke", "rotate"])
  paths["/v1/admin/keys/{id}/" + action] = {
    post: { ...post(action + "Key"), parameters: [idem, pathParam] },
  };
for (const [action, name] of [
  ["cancel", undefined],
  ["plan", "PlanChangeInput"],
  ["payment-orders", undefined],
] as const)
  paths["/v1/admin/subscriptions/{id}/" + action] = {
    post: {
      ...post("subscription_" + action, name),
      parameters: [idem, pathParam],
      ...(action === "payment-orders"
        ? {
            requestBody: {
              required: true,
              content: {
                "application/json": {
                  schema: {
                    type: "object",
                    properties: { integration_id: { type: "string" } },
                    required: ["integration_id"],
                    additionalProperties: false,
                  },
                },
              },
            },
          }
        : {}),
    },
  };
for (const [path, description] of [
  [
    "/v1/admin/subscriptions/{id}",
    "Subscription period, cancellation and scheduled plan.",
  ],
  [
    "/v1/admin/customers/{id}/balance",
    "Customer balances with expiry-aware buckets.",
  ],
  ["/v1/credits", "Only the authenticated customer credit buckets."],
  [
    "/v1/admin/audit-events",
    "Tenant/environment audit log, limit 1..100, opaque cursor.",
  ],
])
  paths[path!] = {
    get: {
      parameters: path!.includes("{id}")
        ? [pathParam]
        : path!.endsWith("audit-events")
          ? [
              {
                name: "limit",
                in: "query",
                schema: { type: "integer", minimum: 1, maximum: 100 },
              },
              { name: "cursor", in: "query", schema: { type: "string" } },
            ]
          : [],
      responses: { "200": { description }, ...errors },
    },
  };
paths["/v1/integrations/payments/{id}/events"] = {
  post: {
    operationId: "sandboxPaymentEvent",
    security: [],
    parameters: [
      pathParam,
      {
        name: "X-Payment-Timestamp",
        in: "header",
        required: true,
        schema: { type: "string" },
      },
      {
        name: "X-Payment-Signature",
        in: "header",
        required: true,
        schema: { type: "string" },
      },
    ],
    requestBody: {
      required: true,
      content: {
        "application/json": {
          schema: { $ref: "#/components/schemas/PaymentInput" },
        },
      },
    },
    responses: {
      "202": {
        description:
          "Committed atomically. outcome is granted, failed, ignored or review_required. HMAC(raw body) required; bearer credentials do not authorize payment.",
      },
      ...errors,
    },
  },
};
await writeFile(
  "docs/generated/openapi.json",
  JSON.stringify(api, null, 2) + "\n",
);
await writeFile(
  "docs/generated/event.schema.json",
  JSON.stringify(schemas.DomainEvent, null, 2) + "\n",
);
console.log("Generated OpenAPI and event schema from runtime validators.");
