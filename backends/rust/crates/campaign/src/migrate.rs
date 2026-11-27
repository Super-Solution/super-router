use include_dir::{include_dir, Dir};
use sqlx::{Executor, PgPool, Row};

use crate::{error::Error, store::Campaigns};

static MIGRATIONS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/migrations");

/// The embedded migrations as `(file name, bytes)`, in version order. A host that applies migrations itself takes
/// these; the bookkeeping table is `sc_migrations`.
pub fn embedded_migrations() -> Vec<(&'static str, &'static [u8])> {
    let mut files: Vec<_> = MIGRATIONS
        .files()
        .filter_map(|f| Some((f.path().file_name()?.to_str()?, f.contents())))
        .filter(|(n, _)| n.ends_with(".sql"))
        .collect();
    files.sort_by_key(|(n, _)| *n);
    files
}

impl Campaigns {
    /// Applies the migrations that have not run, in the schema the pool's `search_path` names, under an advisory
    /// lock so several instances can start at once. A database newer than this build is refused.
    pub async fn migrate(pool: &PgPool) -> Result<(), Error> {
        let mut tx = pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('superrouter:campaign:migrate', 0))").execute(&mut *tx).await?;
        sqlx::query("CREATE TABLE IF NOT EXISTS sc_migrations (version integer PRIMARY KEY)").execute(&mut *tx).await?;
        let applied: Vec<i32> =
            sqlx::query("SELECT version FROM sc_migrations").fetch_all(&mut *tx).await?.iter().map(|r| r.get("version")).collect();
        let files = embedded_migrations();
        let latest = files.iter().filter_map(|(n, _)| n.get(..3)?.parse::<i32>().ok()).max().unwrap_or(0);
        if let Some(ahead) = applied.iter().find(|v| **v > latest) {
            return Err(Error::Invalid(format!("the campaign schema is ahead of this build: migration {ahead} is newer than {latest}")));
        }
        for (name, body) in files {
            let version: i32 = name
                .get(..3)
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| Error::Invalid(format!("migration {name:?} has no version prefix")))?;
            if applied.contains(&version) {
                continue;
            }
            let sql = std::str::from_utf8(body).map_err(|_| Error::Invalid(format!("migration {name} is not UTF-8")))?;
            Executor::execute(&mut *tx, sql).await.map_err(|e| Error::Invalid(format!("migration {name}: {e}")))?;
            sqlx::query("INSERT INTO sc_migrations VALUES ($1)").bind(version).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Whether every embedded migration is applied (for a host that migrates elsewhere and checks at start-up).
    pub async fn check_applied(pool: &PgPool) -> Result<(), Error> {
        let applied: Vec<i32> =
            sqlx::query("SELECT version FROM sc_migrations").fetch_all(pool).await?.iter().map(|r| r.get("version")).collect();
        for (name, _) in embedded_migrations() {
            let version: i32 = name.get(..3).and_then(|v| v.parse().ok()).unwrap_or(0);
            if !applied.contains(&version) {
                return Err(Error::Invalid(format!("campaign migration {name} is not applied")));
            }
        }
        Ok(())
    }
}
