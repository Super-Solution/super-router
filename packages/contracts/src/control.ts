import { z } from "zod";
const id = z
  .string()
  .min(1)
  .max(120)
  .regex(/^[a-zA-Z0-9_.:/-]+$/);
const positive = z.string().regex(/^[1-9][0-9]{0,29}$/);
export const Role = z.enum([
  "owner",
  "service_admin",
  "billing_admin",
  "viewer",
  "app",
]);
export type Role = z.infer<typeof Role>;
export const roleScopes: Record<Role, readonly string[]> = {
  owner: [
    "invoke",
    "identity:write",
    "service:write",
    "routing:write",
    "billing:write",
    "billing:read",
    "operations:read",
  ],
  service_admin: [
    "invoke",
    "service:write",
    "routing:write",
    "operations:read",
  ],
  billing_admin: ["billing:write", "billing:read", "operations:read"],
  viewer: ["billing:read", "operations:read"],
  app: ["invoke", "credits:read"],
};
export const MemberInput = z
  .object({ id, role: Role, active: z.boolean().default(true) })
  .strict();
export const CustomerInput = z.object({ id, external_id: id }).strict();
export const KeyInput = z
  .object({
    member_id: id,
    customer_id: id,
    scopes: z.array(z.string()).min(1).max(16),
    service_ids: z.array(id).min(1).optional(),
    expires_at: z.string().datetime(),
  })
  .strict();
/** The long window is a fixed 7 days; only the short window's length is configurable. */
export const WEEK_HOURS = 168;
export const SESSION_MAX_HOURS = 72;
/**
 * Optional usage-rate limits on a plan's credits: a short "session" window and a long weekly
 * window, both counted in the plan's asset. A window opens at the first reservation drawn from
 * the subscription's bucket after the previous window ended and closes after its length, so the
 * limits shape how fast the period's credits can be spent; they never add credits.
 * `enabled:false` keeps the configuration on the plan version without enforcing it.
 */
export const WindowLimits = z
  .object({
    enabled: z.boolean(),
    session: z
      .object({
        hours: z.number().int().min(1).max(SESSION_MAX_HOURS),
        credits: positive,
      })
      .strict()
      .optional(),
    weekly: z.object({ credits: positive }).strict().optional(),
  })
  .strict()
  .superRefine((v, ctx) => {
    if (v.enabled && !v.session && !v.weekly)
      ctx.addIssue({
        code: "custom",
        message:
          "enabled window limits need a session limit, a weekly limit, or both",
      });
    if (
      v.session &&
      v.weekly &&
      BigInt(v.session.credits) > BigInt(v.weekly.credits)
    )
      ctx.addIssue({
        code: "custom",
        message: "the session limit cannot exceed the weekly limit",
      });
  });
export type WindowLimits = z.infer<typeof WindowLimits>;
export const PlanInput = z
  .object({
    id,
    version: z.number().int().positive(),
    period_days: z.number().int().min(1).max(366),
    asset: id,
    credits: positive,
    service_ids: z.array(id).min(1).max(100),
    amount_minor: positive,
    currency: z.string().regex(/^[A-Z]{3}$/),
    window_limits: WindowLimits.optional(),
  })
  .strict();
export type PlanInput = z.infer<typeof PlanInput>;
export const SubscriptionInput = z
  .object({
    customer_id: id,
    plan_id: id,
    plan_version: z.number().int().positive(),
  })
  .strict();
export const PlanChangeInput = z
  .object({ plan_id: id, plan_version: z.number().int().positive() })
  .strict();
export const PaymentInput = z
  .object({
    id,
    order_id: id,
    revision: z.number().int().positive(),
    status: z.enum(["succeeded", "failed"]),
    amount_minor: positive,
    currency: z.string().regex(/^[A-Z]{3}$/),
  })
  .strict();
export const CreditGrantInput = z
  .object({
    customer_id: id,
    asset: id,
    amount: positive,
    reason: z.string().min(5).max(300),
    source_kind: z.enum(["promotion", "purchased"]).default("promotion"),
    service_ids: z.array(id).min(1).optional(),
    expires_at: z.string().datetime().optional(),
  })
  .strict();

export type MemberInput = z.infer<typeof MemberInput>;
export type CustomerInput = z.infer<typeof CustomerInput>;
export type KeyInput = z.infer<typeof KeyInput>;
export type SubscriptionInput = z.infer<typeof SubscriptionInput>;
export type CreditGrantInput = z.infer<typeof CreditGrantInput>;
