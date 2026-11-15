# Publishing to npm

Thirteen packages are public (`@supersolutions/super-router-*`).
They are released together, at one version, by the manual workflow **Publish npm packages**
(`.github/workflows/npm-publish.yml`). It uses npm Trusted Publishing (OIDC): there is no npm token
and no repository secret.

## One-time setup (npm maintainer, with account 2FA)

For each of the thirteen packages, add a trusted publisher in the package's npm settings (or with
`npm trust github <package> --file npm-publish.yml --repo Super-Solution/super-router --allow-publish`,
npm 11.15 or newer): organization `Super-Solution`, repository `super-router`, workflow filename
`npm-publish.yml`, environment empty. npm only lets you configure a package that already exists, so a
package that has never been published first needs a placeholder version. Configure the trust close to
the release: a new configuration expires if it has not completed a publish within two days.

The repository is private, so npm cannot attach provenance; the workflow sets
`NPM_CONFIG_PROVENANCE=false`. Delete that line when the repository becomes public.

## Releasing

1. Commit the new version in all thirteen manifests and in every internal exact dependency, then run
   `scripts/check-all.sh` (the Postgres-backed tests are not run by the workflow).
2. Run **Publish npm packages** on `main` with that exact version (`X.Y.Z` publishes to `latest`,
   `X.Y.Z-next.N` to `next`).

The workflow checks, before it builds, that the set of public workspace packages equals the list in
`scripts/check-npm-release.mjs` (a new public package fails until it is added), that every package has
the requested version, and that each internal dependency is published earlier. It then runs
`npm run typecheck`, packs every package, and publishes the tarballs in dependency order. Already
published versions cannot be overwritten: after a partial release, inspect the registry before
dispatching again with a new version.

`super-router-cloud` carries a copy of these package names. Do not configure it as a publisher.
