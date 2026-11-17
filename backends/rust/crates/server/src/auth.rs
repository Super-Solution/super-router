//! Static API keys for self-hosting.

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use super_router_contracts::{Principal, RouterError};
use super_router_router::Authenticator;

/// One static API key.
#[derive(Debug, Clone)]
pub struct Key {
    pub secret: String,
    pub principal: Principal,
}

/// Authenticates against a fixed list of keys, comparing SHA-256 digests in constant time.
#[derive(Debug, Clone)]
pub struct StaticKeys {
    keys: Vec<([u8; 32], Principal)>,
}

impl StaticKeys {
    pub fn new(keys: impl IntoIterator<Item = Key>) -> Self {
        Self { keys: keys.into_iter().map(|k| (Sha256::digest(k.secret.as_bytes()).into(), k.principal)).collect() }
    }
}

#[async_trait]
impl Authenticator for StaticKeys {
    async fn authenticate(&self, token: &str) -> Result<Principal, RouterError> {
        // A key list built from an unset variable holds an empty secret, which would otherwise authenticate a
        // request that sent no credentials at all.
        if token.is_empty() {
            return Err(RouterError::new("invalid_key", 401));
        }
        let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        // Every key is compared, so the time does not reveal which one matched.
        let mut found = None;
        for (stored, principal) in &self.keys {
            if bool::from(stored.ct_eq(&digest)) {
                found = Some(principal);
            }
        }
        found.cloned().ok_or_else(|| RouterError::new("invalid_key", 401))
    }
}
