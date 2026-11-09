// Starts the whole example in one process: router (with PostgreSQL), console and chat.
import { startRouter } from "@supersolutions/example-superchat-router";
import { startConsole } from "./apps/console/server.js";
import { startChat } from "./apps/chat/server.js";
import { PORTS } from "@supersolutions/example-superchat-shared";

const router = await startRouter();
const consoleApp = await startConsole();
const chat = await startChat();
console.log(`
Open the developer console  http://127.0.0.1:${PORTS.console}
Open the chat app           http://127.0.0.1:${PORTS.chat}

Models are local mocks: nothing is sent to a paid provider. Ctrl-C to stop.`);
for (const signal of ["SIGINT", "SIGTERM"] as const)
  process.once(signal, async () => {
    await Promise.all([chat.close(), consoleApp.close()]);
    await router.close();
    process.exit(0);
  });
