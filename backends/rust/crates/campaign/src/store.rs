use std::sync::Arc;

use sqlx::PgPool;

use crate::error::{invalid, Error};
use crate::sink::GrantSink;

/// The extension: a pool whose `search_path` names its own schema, the sink it grants through, and the host's
/// secret (at least 32 bytes) that keys every stored hash. **Keep the secret stable**: a changed secret makes every
/// stored code unredeemable.
pub struct Campaigns {
    pub(crate) pool: PgPool,
    pub(crate) sink: Arc<dyn GrantSink>,
    pub(crate) key: Vec<u8>,
}

impl Campaigns {
    pub fn new(pool: PgPool, sink: Arc<dyn GrantSink>, secret: &[u8]) -> Result<Self, Error> {
        if secret.len() < 32 {
            return invalid("the secret must be at least 32 bytes");
        }
        Ok(Self { pool, sink, key: secret.to_vec() })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

/// `n` random bytes from the operating system.
pub(crate) fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("the operating system's random number generator failed");
    b
}
