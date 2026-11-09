import { createServer } from "node:http";
import { nodeListener } from "@supersolutions/node-fetch-adapters/node";
import { type Router, type DataStore } from "@supersolutions/super-router-core";
import type { Principal } from "@supersolutions/super-router-contracts";
import type { ControlPlane } from "@supersolutions/super-router-storage-postgres";
import { createRouterHandler } from "./handler.js";

/** A plain `node:http` server around {@link createRouterHandler}. Use the handler directly to mount in a framework. */
export function createRouterServer(
  router: Router,
  store: DataStore,
  keys: { key: string; principal: Principal }[] | ControlPlane,
  mode: "cloud" | "self-host" = "self-host",
) {
  return createServer(
    nodeListener(createRouterHandler({ router, store, keys, mode })),
  );
}
