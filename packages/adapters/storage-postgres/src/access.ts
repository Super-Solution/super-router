import type { Pool, PoolClient } from "pg";
import {
  RouterError,
  roleScopes,
  type Principal,
  type Role,
} from "@supersolutions/super-router-contracts";
export async function keyPrincipal(
  db: Pool | PoolClient,
  hash: string,
): Promise<Principal> {
  const r = await db.query(
    "SELECT k.*,m.role,m.active FROM sr_keys k JOIN sr_members m ON (k.workspace,k.environment,k.member_id)=(m.workspace,m.environment,m.id) WHERE k.hash=$1 AND k.revoked_at IS NULL AND k.expires_at>now()",
    [hash],
  );
  const k = r.rows[0];
  if (!k || !k.active) throw new RouterError("invalid_key", 401);
  return {
    workspace: k.workspace,
    environment: k.environment,
    customer: k.customer,
    scopes: k.scopes.filter((s: string) =>
      roleScopes[k.role as Role].includes(s),
    ),
    auth_key_id: k.id,
    member_id: k.member_id,
    service_ids: k.service_ids ?? undefined,
  };
}
export async function revalidate(
  db: Pool | PoolClient,
  p: Principal,
  required?: string,
) {
  if (p.auth_key_id) {
    const r = await db.query(
      "SELECT hash FROM sr_keys WHERE workspace=$1 AND environment=$2 AND id=$3",
      [p.workspace, p.environment, p.auth_key_id],
    );
    if (!r.rows[0]) throw new RouterError("invalid_key", 401);
    const current = await keyPrincipal(db, r.rows[0].hash);
    if (
      current.customer !== p.customer ||
      p.scopes.some((s) => !current.scopes.includes(s))
    )
      throw new RouterError("scope_denied", 403);
    if (
      current.service_ids &&
      (!p.service_ids ||
        p.service_ids.some((s) => !current.service_ids!.includes(s)))
    )
      throw new RouterError("scope_denied", 403);
  }
  if (required && !p.scopes.includes(required))
    throw new RouterError("scope_denied", 403);
}
