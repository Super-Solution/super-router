#!/usr/bin/env node
// Release gate for the crates.io workflow (.github/workflows/crates-publish.yml).
//
//   node scripts/check-crates-release.mjs <version> [--online]
//
// Checks the Rust workspace in backends/rust: the set of publishable crates equals the list below, every one carries
// <version> and the metadata crates.io needs, every internal dependency names exactly <version> and is published
// earlier, and the LICENSE and NOTICE shipped in each crate equal the repository's. With --online it also fails when a
// crate version already exists on crates.io (a release cannot be repeated or overwritten). Nothing is written or
// published. On success it prints the crates in publish order, one per line.
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

// Dependency order. A new publishable crate must be added here (the check fails until it is), and a crate that stops
// being publishable (`publish = false`) must be removed.
export const PUBLISH_ORDER = [
  "super-router-pricing",
  "super-router-sse",
  "super-router-contracts",
  "super-router-provider-anthropic",
  "super-router-provider-openai",
  "super-router-provider-gemini",
  "super-router-routing",
  "super-router-router",
  "super-router-server",
  "super-router-store-memory",
  "super-router-store-postgres",
];

const REPOSITORY = "https://github.com/Super-Solution/super-router";
const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-(alpha|beta|rc)\.(0|[1-9]\d*))?$/;

export function checkRelease(root, version, metadata) {
  if (!SEMVER.test(version ?? "")) {
    throw new Error("version must be X.Y.Z or X.Y.Z-(alpha|beta|rc).N");
  }
  const publishable = new Map();
  const unpublishable = new Set();
  for (const pkg of metadata.packages) {
    // `publish` is null when any registry is allowed and [] when `publish = false`.
    if (Array.isArray(pkg.publish) && pkg.publish.length === 0) unpublishable.add(pkg.name);
    else publishable.set(pkg.name, pkg);
  }
  const errors = [];
  for (const name of publishable.keys()) {
    if (!PUBLISH_ORDER.includes(name)) errors.push(`${name} is publishable but not in PUBLISH_ORDER`);
  }
  for (const name of PUBLISH_ORDER) {
    if (!publishable.has(name)) errors.push(`${name} is in PUBLISH_ORDER but is not a publishable workspace crate`);
  }
  const position = new Map(PUBLISH_ORDER.map((name, index) => [name, index]));
  const license = fs.readFileSync(path.join(root, "LICENSE"), "utf8");
  const notice = fs.readFileSync(path.join(root, "NOTICE"), "utf8");
  for (const [name, pkg] of publishable) {
    const dir = path.dirname(pkg.manifest_path);
    if (pkg.version !== version) errors.push(`${name} is ${pkg.version}, not ${version}`);
    if (pkg.license !== "Apache-2.0") errors.push(`${name} license is ${pkg.license}, expected Apache-2.0`);
    if (pkg.repository !== REPOSITORY) errors.push(`${name} repository is ${pkg.repository}, expected ${REPOSITORY}`);
    if (!pkg.description) errors.push(`${name} has no description`);
    if (!pkg.homepage) errors.push(`${name} has no homepage`);
    if (pkg.keywords.length === 0 || pkg.keywords.length > 5) errors.push(`${name} needs 1 to 5 keywords`);
    if (pkg.categories.length === 0) errors.push(`${name} has no categories`);
    if (!pkg.readme || !fs.existsSync(path.join(dir, pkg.readme))) errors.push(`${name} has no README file`);
    if (!pkg.rust_version) errors.push(`${name} has no rust-version`);
    for (const [file, expected] of [["LICENSE", license], ["NOTICE", notice]]) {
      const target = path.join(dir, file);
      if (!fs.existsSync(target)) errors.push(`${name} has no ${file} (copy it from the repository root)`);
      else if (fs.readFileSync(target, "utf8") !== expected) errors.push(`${name} ${file} differs from the repository's`);
    }
    for (const dep of pkg.dependencies) {
      if (!dep.path) continue; // an external dependency
      if (unpublishable.has(dep.name) && dep.kind !== "dev") {
        errors.push(`${name} depends on ${dep.name}, which is not published`);
      }
      if (dep.kind === "dev") {
        // cargo drops a dev-dependency that has no version; with one, crates.io must already have it.
        if (dep.req !== "*") errors.push(`${name} dev-dependency ${dep.name} must be path-only (it names ${dep.req})`);
        continue;
      }
      if (dep.req !== `^${version}`) errors.push(`${name} depends on ${dep.name} "${dep.req}", not "${version}"`);
      if (position.get(dep.name) > position.get(name)) errors.push(`${dep.name} must be published before ${name}`);
    }
  }
  if (errors.length > 0) throw new Error(errors.join("\n"));
  return PUBLISH_ORDER;
}

async function alreadyPublished(names, version) {
  const found = [];
  for (const name of names) {
    const response = await fetch(`https://crates.io/api/v1/crates/${name}/${version}`, {
      headers: { "user-agent": `super-router-release-check (${REPOSITORY})` },
    });
    if (response.status === 200) found.push(name);
    else if (response.status !== 404) throw new Error(`crates.io answered ${response.status} for ${name} ${version}`);
  }
  return found;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  const online = args.includes("--online");
  const [version] = args.filter((a) => !a.startsWith("--"));
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
  try {
    const metadata = JSON.parse(
      execFileSync("cargo", ["metadata", "--no-deps", "--format-version", "1", "--locked"], {
        cwd: path.join(root, "backends", "rust"),
        encoding: "utf8",
        maxBuffer: 64 * 1024 * 1024,
      }),
    );
    const order = checkRelease(root, version, metadata);
    if (online) {
      const found = await alreadyPublished(order, version);
      if (found.length > 0) throw new Error(`already on crates.io at ${version}: ${found.join(", ")}`);
    }
    console.log(order.join("\n"));
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exit(1);
  }
}
