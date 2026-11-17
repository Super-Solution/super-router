//! Database-backed API keys: the control plane's `sr_keys` and `sr_members` tables, which the TypeScript control
//! plane writes. Only the SHA-256 of a token is stored; revoked, expired and deactivated-member keys are refused.

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPool;
use sqlx::{PgConnection, Row};
use super_router_contracts::{role_scopes, Principal, RouterError};
use super_router_router::{Authenticator, StoreError, StoreResult};

use crate::util::{db, fail, internal, json_strings};

/// Authenticates bearer tokens against the control plane's key table.
#[derive(Clone)]
pub struct Keys {
    pool: PgPool,
}

impl Keys {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Turns a token into the principal its key grants: the key's scopes narrowed to what the member's role still allows.
    pub async fn authenticate(&self, token: &str) -> StoreResult<Principal> {
        if !valid_token(token) {
            return Err(fail("invalid_key", 401));
        }
        let mut conn = self.pool.acquire().await.map_err(db)?;
        principal_by_hash(&mut conn, &hash_token(token)).await
    }
}

#[async_trait]
impl Authenticator for Keys {
    async fn authenticate(&self, token: &str) -> Result<Principal, RouterError> {
        Keys::authenticate(self, token).await.map_err(|e| match e {
            StoreError::Router(re) => re,
            StoreError::Internal(_) => RouterError::new("internal_error", 500),
        })
    }
}

/// `^sr_[a-f0-9]{64}$`
fn valid_token(token: &str) -> bool {
    token.strip_prefix("sr_").is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

pub(crate) async fn principal_by_hash(conn: &mut PgConnection, hash: &str) -> StoreResult<Principal> {
    let row = sqlx::query(
        "SELECT k.workspace,k.environment,k.customer,k.id,k.member_id,to_json(k.scopes)::text AS scopes,to_json(k.service_ids)::text AS service_ids,m.role,m.active \
         FROM sr_keys k JOIN sr_members m ON (k.workspace,k.environment,k.member_id)=(m.workspace,m.environment,m.id) \
         WHERE k.hash=$1 AND k.revoked_at IS NULL AND k.expires_at>now()",
    )
    .bind(hash)
    .fetch_optional(&mut *conn)
    .await
    .map_err(db)?;
    let Some(row) = row else { return Err(fail("invalid_key", 401)) };
    if !row.try_get::<bool, _>("active").map_err(db)? {
        return Err(fail("invalid_key", 401));
    }
    let get = |c: &str| row.try_get::<String, _>(c).map_err(db);
    let granted = json_strings(row.try_get::<Option<String>, _>("scopes").map_err(db)?).ok_or_else(|| internal("a key has no scopes"))?;
    let allowed = role_scopes(&get("role")?).unwrap_or(&[]);
    Ok(Principal {
        workspace: get("workspace")?,
        environment: get("environment")?,
        customer: get("customer")?,
        auth_key_id: get("id")?,
        member_id: get("member_id")?,
        scopes: granted.into_iter().filter(|s| allowed.contains(&s.as_str())).collect(),
        service_ids: json_strings(row.try_get::<Option<String>, _>("service_ids").map_err(db)?),
    })
}

/// Inside a write transaction, re-reads the principal's key and refuses the write if the key was revoked, expired,
/// moved to another customer or narrowed since it was verified. Principals that did not come from a key
/// (`auth_key_id` empty) pass.
pub async fn revalidate_key(conn: &mut PgConnection, p: &Principal) -> StoreResult<()> {
    if p.auth_key_id.is_empty() {
        return Ok(());
    }
    let row = sqlx::query("SELECT hash FROM sr_keys WHERE workspace=$1 AND environment=$2 AND id=$3")
        .bind(&p.workspace)
        .bind(&p.environment)
        .bind(&p.auth_key_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(db)?;
    let Some(row) = row else { return Err(fail("invalid_key", 401)) };
    let current = principal_by_hash(conn, &row.try_get::<String, _>("hash").map_err(db)?).await?;
    if current.customer != p.customer || p.scopes.iter().any(|s| !current.has_scope(s)) {
        return Err(fail("scope_denied", 403));
    }
    if let Some(allowed) = &current.service_ids {
        match &p.service_ids {
            None => return Err(fail("scope_denied", 403)),
            Some(mine) if mine.iter().any(|s| !allowed.contains(s)) => return Err(fail("scope_denied", 403)),
            Some(_) => {}
        }
    }
    Ok(())
}

/// The standard write revalidation for hosts that authenticate with database-backed keys: pass it as
/// [`crate::Options::revalidate`].
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyRevalidator;

#[async_trait]
impl crate::Revalidate for KeyRevalidator {
    async fn revalidate(&self, conn: &mut PgConnection, p: &Principal, _scope: &str) -> StoreResult<()> {
        revalidate_key(conn, p).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_shape() {
        let ok = format!("sr_{}", "a1".repeat(32));
        assert!(valid_token(&ok));
        for bad in [
            "",
            "sr_",
            "sr_nope",
            &format!("sr_{}", "A1".repeat(32)),
            &format!("sr_{}", "a".repeat(63)),
            &format!("sr_{}", "a".repeat(65)),
            &format!("xx_{}", "a".repeat(64)),
        ] {
            assert!(!valid_token(bad), "{bad}");
        }
        assert_eq!(hash_token("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
