import {
  PostgresStore,
  LOCAL_DATABASE_URL,
} from "@supersolutions/super-router-storage-postgres";
const store = new PostgresStore(process.env.DATABASE_URL ?? LOCAL_DATABASE_URL);
try {
  await store.migrate();
  console.log("Migrations applied.");
} finally {
  await store.close();
}
