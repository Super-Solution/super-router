import { startConsole } from "./server.js";

const app = await startConsole();
for (const signal of ["SIGINT", "SIGTERM"] as const)
  process.once(signal, () => void app.close().then(() => process.exit(0)));
