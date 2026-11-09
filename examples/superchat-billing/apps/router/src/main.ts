import { startRouter } from "./index.js";

const router = await startRouter();
for (const signal of ["SIGINT", "SIGTERM"] as const)
  process.once(signal, () => void router.close().then(() => process.exit(0)));
