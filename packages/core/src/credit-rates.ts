import {
  z,
  Price,
  RouterError,
  type Principal,
  type Service,
} from "@supersolutions/super-router-contracts";
/** Credits are a fixed accounting unit. Developers configure units per credit, never the scale. */
export const CREDIT_ATOMS = 1_000_000_000n;
const integer = z.string().regex(/^[1-9][0-9]{0,29}$/);
const ratio = z
  .object({ units: integer, credits: integer.default("1") })
  .strict();
export const CreditRateConfig = z
  .object({
    asset: z
      .string()
      .min(1)
      .max(120)
      .regex(/^[a-zA-Z0-9_.:/-]+$/),
    tokens: z
      .object({
        input: ratio,
        cached: ratio,
        cache_write: ratio,
        output: ratio,
      })
      .strict()
      .optional(),
    media: z
      .object({
        images: ratio.optional(),
        video_ms: ratio.optional(),
        audio_ms: ratio.optional(),
        tasks: ratio.optional(),
      })
      .strict()
      .optional(),
  })
  .strict();
export type CreditRateConfig = z.input<typeof CreditRateConfig>;
const tokenKeys = ["input", "cached", "cache_write", "output"] as const;
const mediaKeys = ["images", "video_ms", "audio_ms", "tasks"] as const;
function gcd(a: bigint, b: bigint): bigint {
  while (b) {
    [a, b] = [b, a % b];
  }
  return a;
}
function rate(value: z.output<typeof ratio>) {
  const numerator = CREDIT_ATOMS * BigInt(value.credits),
    denominator = BigInt(value.units),
    g = gcd(numerator, denominator);
  return { numerator: numerator / g, denominator: denominator / g };
}
/** Compile exact rational units-per-credit to existing wire Price. Token mode requires all four categories; media categories are explicit. */
export function compileCreditPrice(raw: unknown): Price {
  const parsed = CreditRateConfig.safeParse(raw);
  if (!parsed.success) throw new RouterError("invalid_credit_rates");
  const config = parsed.data;
  if (
    (!config.tokens && !mediaKeys.some((k) => config.media?.[k])) ||
    (config.tokens && tokenKeys.some((k) => !config.tokens?.[k]))
  )
    throw new RouterError("invalid_credit_rates");
  const rates = tokenKeys.map((key) =>
    config.tokens?.[key]
      ? rate(config.tokens[key]!)
      : { numerator: 0n, denominator: 1n },
  );
  let per = 1n;
  for (const r of rates) {
    per = (per / gcd(per, r.denominator)) * r.denominator;
    if (per > 1_000_000_000n)
      throw new RouterError("unsupported_rate_precision", 422);
  }
  const tokens = Object.fromEntries(
    tokenKeys.map((key, i) => [
      key,
      (rates[i]!.numerator * (per / rates[i]!.denominator)).toString(),
    ]),
  );
  const media: Record<string, { atoms: string; per: number }> = {};
  for (const key of mediaKeys) {
    const value = config.media?.[key];
    if (!value) continue;
    const r = rate(value);
    if (r.denominator > 1_000_000_000n)
      throw new RouterError("unsupported_rate_precision", 422);
    media[key] = { atoms: r.numerator.toString(), per: Number(r.denominator) };
  }
  const compiled = Price.safeParse({
    asset: config.asset,
    ...tokens,
    per: Number(per),
    ...(Object.keys(media).length ? { media } : {}),
  });
  if (!compiled.success)
    throw new RouterError("unsupported_rate_precision", 422);
  return compiled.data;
}
export interface CreditRatePublisher {
  getService(p: Principal, id: string, version?: number): Promise<Service>;
  publish(
    p: Principal,
    service: unknown,
    mode?: "cloud" | "self-host",
    expectedVersion?: number,
  ): Promise<Service | undefined>;
}
/** Atomically advance only the customer-sale price; supply and platform prices remain server snapshots. */
export async function publishCreditRateVersion(
  store: CreditRatePublisher,
  p: Principal,
  id: string,
  expectedVersion: number,
  config: unknown,
  mode: "cloud" | "self-host" = "self-host",
) {
  if (!p.scopes.includes("service:write"))
    throw new RouterError("scope_denied", 403);
  if (
    !Number.isSafeInteger(expectedVersion) ||
    expectedVersion < 1 ||
    expectedVersion >= Number.MAX_SAFE_INTEGER
  )
    throw new RouterError("invalid_service_version");
  const current = await store.getService(p, id);
  if (current.version !== expectedVersion)
    throw new RouterError("service_version_conflict", 409);
  const price = compileCreditPrice(config);
  const parsedConfig = CreditRateConfig.parse(config);
  const requiredMeter = current.task_kind
    ? (
        {
          image: "images",
          video: "video_ms",
          audio: "audio_ms",
          task: "tasks",
        } as const
      )[current.task_kind]
    : undefined;
  if (
    requiredMeter
      ? !parsedConfig.media?.[requiredMeter] ||
        !!parsedConfig.tokens ||
        mediaKeys.some((k) => k !== requiredMeter && !!parsedConfig.media?.[k])
      : !parsedConfig.tokens || !!parsedConfig.media
  )
    throw new RouterError("credit_rate_kind_mismatch", 422);
  if (price.asset !== current.price.asset)
    throw new RouterError("credit_asset_mismatch", 422);
  const next = { ...current, version: current.version + 1, price };
  await store.publish(p, next, mode, expectedVersion);
  return next;
}
