# Publishing to crates.io

Ten crates of the Rust backend (`backends/rust`) are public. They are released together, at one version, by the manual
workflow **Publish crates** (`.github/workflows/crates-publish.yml`), which uses crates.io Trusted Publishing (OIDC):
no crates.io token and no repository secret. The **first** release of every crate is different: a crate must exist
before it can have a trusted publisher, so that release is published by hand, once, with a short-lived token
(see [First release](#first-release-by-hand-once)).

| Crate | Published | Why |
| --- | --- | --- |
| `super-router-pricing` | yes | exact money arithmetic, useful on its own |
| `super-router-sse` | yes | bounded SSE reader, useful on its own |
| `super-router-contracts` | yes | wire types and strict parsing; every other crate builds on it |
| `super-router-routing` | yes | offering selection |
| `super-router-router` | yes | the data plane (`Router::infer`) and the `Store` / `DataStore` / `Authenticator` ports |
| `super-router-store-memory` | yes | in-memory store for tests, demos and single-process self-hosting |
| `super-router-store-postgres` | yes | PostgreSQL store with embedded migrations |
| `super-router-provider-openai` | yes | OpenAI Responses provider |
| `super-router-provider-anthropic` | yes | Anthropic Messages provider |
| `super-router-server` | yes | the HTTP API as an axum `Router` |
| `super-router-cli` | **no** (`publish = false`) | a binary; undecided, see below |
| `super-router-dataplane-test` | **no** (`publish = false`) | cannot be published as it is, see below |

`super-router-cli` builds the `super-router-server` binary. `cargo install super-router-cli` would install a binary
named after a different crate (the library `super-router-server`), and a crate name cannot be changed after the first
publish. Release binaries or a container image are the usual way to ship a server, so the crate stays unpublished until
that is decided.

`super-router-dataplane-test` reads `conformance/dataplane.v1.json` from the repository at run time
(`../../conformance`), a path that does not exist in a published crate. To publish it, embed the data with
`include_str!` from a copy inside the crate, and keep that copy in step with the repository's (the check script can
compare them).

## What a release checks

`node scripts/check-crates-release.mjs <version> [--online]` fails unless:

- the set of publishable crates equals `PUBLISH_ORDER` in the script (a new publishable crate fails until it is added);
- every crate has the version, `license = "Apache-2.0"`, `repository`, `homepage`, `description`, one to five
  `keywords`, `categories`, a `README.md` and a `rust-version`;
- every internal dependency names exactly the release version (`version = "X.Y.Z"` in `[workspace.dependencies]`) and
  is published earlier; a dev-dependency on another workspace crate is path-only (cargo then drops it on publish);
- each crate's `LICENSE` and `NOTICE` equal the repository's (crates.io needs them inside the crate; copy them again
  after changing the root files);
- with `--online`, no crate already exists on crates.io at that version.

The workflow then runs `cargo fmt`, `cargo clippy -D warnings` and `cargo test --workspace` (the PostgreSQL tests skip
without a database, as in the npm workflow), checks that the sources build on the declared `rust-version` with
dependencies that support it, and runs `cargo publish --workspace --dry-run`. A real run then publishes in dependency
order with `cargo publish --workspace`, which waits for each crate to appear in the index before publishing the next.

Run the same checks yourself from the repository root:

```bash
node scripts/check-crates-release.mjs 0.2.0 --online
cd backends/rust && cargo publish --workspace --dry-run --locked
```

## First release, by hand, once

Do this on your own machine, from a clean checkout of `main` at the release commit. Nothing here can be undone except
by yanking: a published version stays on crates.io for ever.

1. **Account.** Sign in to crates.io with GitHub (it is the only login). Verify the email address on
   <https://crates.io/settings/profile>; crates.io refuses to publish without one.
2. **Token.** <https://crates.io/settings/tokens> → New token. Name `super-router-first-release`, expiry **1 day**,
   scopes `publish-new` and `publish-update`, crate pattern `super-router-*`. Copy it once. Keep it out of chat, files
   and shell history: `read -s CARGO_REGISTRY_TOKEN && export CARGO_REGISTRY_TOKEN`.
3. **Check.**
   ```bash
   git checkout main && git pull --ff-only && git status --short   # must print nothing
   node scripts/check-crates-release.mjs 0.2.0 --online
   cd backends/rust && cargo publish --workspace --dry-run --locked
   ```
4. **Publish.** `cargo publish --workspace --locked` (still in `backends/rust`). New crates are rate limited on
   crates.io (to my recollection: a burst of five, then one every ten minutes, which `cargo` reports as HTTP 429 with a
   time to retry after). Ten new crates therefore stop partway. Either wait and publish the rest, in the order the check
   script prints, one by one (`cargo publish -p super-router-routing --locked`, and so on), or ask crates.io support
   (help@crates.io) to raise the limit before you start. Do not change any file between the first and the last
   publish.
5. **Owners.** Make the organization the owner, not one person. Once: GitHub → organization `Super-Solution` → Teams →
   New team `crates-publishers` (visible, members: the people who may manage crate ownership). crates.io must be allowed
   to see the organization's teams: in your GitHub organization settings, approve the crates.io OAuth application
   (Settings → Third-party access), and sign in to crates.io again so it reads your memberships. Then, for each of the ten
   crates, with the token still set:
   ```bash
   for c in pricing sse contracts routing router store-memory store-postgres provider-openai provider-anthropic server; do
     cargo owner --add github:Super-Solution:crates-publishers "super-router-$c"
   done
   ```
   `cargo owner --list super-router-pricing` shows the result. Your own account stays an owner until you remove it
   (`cargo owner --remove`); keep at least one person besides the team until you have seen the team work.
6. **Trusted publishers.** For each of the ten crates: crates.io → the crate → Settings → Trusted Publishing → add a
   GitHub publisher with repository owner `Super-Solution`, repository `super-router`, workflow filename
   `crates-publish.yml`, environment `crates-io`.
7. **GitHub environment.** Repository → Settings → Environments → New environment `crates-io`; add yourself as a required
   reviewer. It holds no secrets: the name only appears in the trusted publishers and gates the publish job.
8. **Delete the token** on <https://crates.io/settings/tokens> and unset it in your shell. Optionally turn on
   "Trusted Publishing only" for each crate, which makes crates.io refuse token publishes altogether.
9. **Tag.** Create the `vX.Y.Z` tag and GitHub Release for the commit you published, as for the npm packages.

## Releasing

1. Choose the version. All ten crates move together; while the major version is 0, a minor bump may break the API and a
   patch only fixes bugs. A pre-release is `X.Y.Z-alpha.N`, `-beta.N` or `-rc.N`; crates.io never selects a pre-release
   unless it is requested by exact version.
2. Edit `version` under `[workspace.package]` and every `version = "..."` of the ten internal crates under
   `[workspace.dependencies]` in `backends/rust/Cargo.toml`, run `cargo check --workspace` so `Cargo.lock` follows, and
   run `node scripts/check-crates-release.mjs X.Y.Z`. Commit and merge to `main`.
3. Actions → **Publish crates** → Run workflow on `main` with the version and **dry_run on**. Fix what it reports.
4. Run it again with **dry_run off**. The publish job waits for the `crates-io` environment approval.
5. Tag `vX.Y.Z` and create the GitHub Release.

A crate version cannot be overwritten. After a partial release (the job failed after some crates went out), do not
run the workflow again with the same version: publish the missing crates by hand in the printed order with a
short-lived token, or release the next patch version for all of them. A bad release is yanked with
`cargo yank --version X.Y.Z <crate>` for every crate, then fixed in the next patch; yanking keeps existing lockfiles
working and stops new resolution.
