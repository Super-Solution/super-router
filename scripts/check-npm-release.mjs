#!/usr/bin/env node
// Release gate for the npm workflow (.github/workflows/npm-publish.yml).
//
//   node scripts/check-npm-release.mjs <version>
//
// Checks that every public workspace package is in the publish list below, carries
// <version>, and that each exact internal dependency is published earlier. On success it
// prints one package directory per line, in publish order. Nothing is written or published.
import fs from "node:fs";
import path from "node:path";

// Dependency order. A new public package must be added here (the check fails until it is).
export const PUBLISH_ORDER = [
  "@supersolutions/super-router-contracts",
  "@supersolutions/super-router-core",
  "@supersolutions/super-router-fetch-adapters",
  "@supersolutions/super-router-payments",
  "@supersolutions/super-router-provider-anthropic",
  "@supersolutions/super-router-provider-openai",
  "@supersolutions/super-router-sdk",
  "@supersolutions/super-router-storage-memory",
  "@supersolutions/super-router-storage-postgres",
  "@supersolutions/super-router-billing-presets",
  "@supersolutions/super-router-server",
  "@supersolutions/super-router-worker",
  "@supersolutions/super-router-ui",
];

const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-next\.(0|[1-9]\d*))?$/;

export function releasePackages(root, version) {
  if (!SEMVER.test(version ?? "")) {
    throw new Error("version must be X.Y.Z or X.Y.Z-next.N");
  }
  const rootManifest = JSON.parse(fs.readFileSync(path.join(root, "package.json"), "utf8"));
  const found = new Map();
  for (const dir of rootManifest.workspaces) {
    const manifest = JSON.parse(fs.readFileSync(path.join(root, dir, "package.json"), "utf8"));
    if (manifest.private === true) continue;
    found.set(manifest.name, { dir, manifest });
  }
  const errors = [];
  for (const name of found.keys()) {
    if (!PUBLISH_ORDER.includes(name)) errors.push(`${name} is public but not in PUBLISH_ORDER`);
  }
  for (const name of PUBLISH_ORDER) {
    if (!found.has(name)) errors.push(`${name} is in PUBLISH_ORDER but is not a public workspace package`);
  }
  const position = new Map(PUBLISH_ORDER.map((name, index) => [name, index]));
  for (const [name, { manifest }] of found) {
    if (manifest.version !== version) errors.push(`${name} is ${manifest.version}, not ${version}`);
    if (manifest.publishConfig?.access !== "public") errors.push(`${name} has no publishConfig.access "public"`);
    for (const field of ["dependencies", "peerDependencies", "optionalDependencies"]) {
      for (const [dep, range] of Object.entries(manifest[field] ?? {})) {
        if (!found.has(dep)) continue;
        if (range !== version) errors.push(`${name} ${field}.${dep} is "${range}", not "${version}"`);
        if (position.get(dep) > position.get(name)) errors.push(`${dep} must be published before ${name}`);
      }
    }
  }
  if (errors.length > 0) throw new Error(errors.join("\n"));
  return PUBLISH_ORDER.map((name) => ({ name, dir: found.get(name).dir }));
}

if (import.meta.url === `file://${process.argv[1]}`) {
  try {
    const list = releasePackages(process.cwd(), process.argv[2]);
    for (const { dir } of list) console.log(dir);
  } catch (error) {
    console.error(error.message);
    process.exit(1);
  }
}
