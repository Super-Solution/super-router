//! The embedded migrations are this backend's own `migrations` folder: ordered, versioned, applied once, race-safe.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use super_router_store_postgres::{embedded_migrations, Options, PostgresStore};

#[test]
fn the_embedded_migrations_are_this_backends_own_folder() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let mut on_disk: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())).filter_map(Result::ok) {
        if entry.path().extension().is_some_and(|x| x == "sql") {
            on_disk.insert(entry.file_name().to_string_lossy().into_owned(), std::fs::read(entry.path()).unwrap());
        }
    }
    let embedded: BTreeMap<String, Vec<u8>> = embedded_migrations().into_iter().map(|(n, b)| (n.to_string(), b.to_vec())).collect();
    assert!(embedded.len() >= 33, "only {} embedded migrations", embedded.len());
    assert_eq!(on_disk, embedded);
}

#[test]
fn every_migration_has_a_three_digit_version_prefix_in_order() {
    let names: Vec<&str> = embedded_migrations().into_iter().map(|(n, _)| n).collect();
    for (i, name) in names.iter().enumerate() {
        assert_eq!(name.get(..3).and_then(|v| v.parse::<usize>().ok()), Some(i + 1), "{name}");
        assert_eq!(name.as_bytes()[3], b'_', "{name}");
    }
}

#[tokio::test]
async fn migrate_is_idempotent_and_safe_to_race() {
    let Some(store) = common::open().await else { return };
    let runs: Vec<_> = (0..4)
        .map(|_| {
            let store = store.clone();
            tokio::spawn(async move { store.migrate().await })
        })
        .collect();
    for run in runs {
        run.await.unwrap().expect("a concurrent migrate must succeed");
    }
    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM sr_migrations").fetch_one(store.pool()).await.unwrap();
    assert_eq!(applied as usize, embedded_migrations().len());
}

/// A database whose schema is newer than this build is refused, in a throwaway database so nothing is poisoned.
#[tokio::test]
async fn a_schema_that_is_ahead_of_this_build_is_refused() {
    let Some(store) = common::open().await else { return };
    let name = format!("sr_ahead_{}", uuid_hex());
    if sqlx::query(&format!("CREATE DATABASE {name}")).execute(store.pool()).await.is_err() {
        eprintln!("skipping: this role cannot create databases");
        return;
    }
    let url = common::database_url();
    let (base, query) = url.split_once('?').map_or((url.as_str(), None), |(b, q)| (b, Some(q)));
    let (prefix, _) = base.rsplit_once('/').unwrap();
    let scratch_url = format!("{prefix}/{name}{}", query.map(|q| format!("?{q}")).unwrap_or_default());
    let scratch = PostgresStore::connect(&scratch_url, 2, Options::default()).await.unwrap();
    scratch.migrate().await.unwrap();
    sqlx::query("INSERT INTO sr_migrations VALUES(9999)").execute(scratch.pool()).await.unwrap();
    let err = scratch.migrate().await.unwrap_err();
    assert!(err.to_string().contains("ahead"), "{err}");
    scratch.pool().close().await;
    sqlx::query(&format!("DROP DATABASE {name}")).execute(store.pool()).await.unwrap();
}

fn uuid_hex() -> String {
    // Four bytes of the clock are plenty for a throwaway database name.
    format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())
}
