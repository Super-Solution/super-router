# super-router-campaign

Campaigns for Super Router as an **extension**: a referral program and redeem codes. Not in the router's core and not
needed by it. The router already has the one primitive every giveaway needs, `grant(customer, asset, amount, source,
expires)`, idempotent on `source`. This crate decides who gets a grant and how much, and gives it through that call.

```toml
super-router-campaign = "0.5"   # not published yet: see below
```

## The seam

```text
host facts ──►  Campaigns  ──►  GrantSink  ──►  the router's ledger (or any ledger)
 attach / purchase / redeem       one idempotent grant, key = program + the fact's own id
```

- **Facts in:** the host reports `attach` (a new customer arrived with an invite code), `purchase` (a customer paid),
  `redeem` (a customer typed a code), and calls `release_due` on a timer.
- **Effects out:** one `Grant { customer, amount, kind, expires_ms, key }` through a [`GrantSink`] the host
  provides. `DataStoreSink` is a ready sink for any router `DataStore`; a host whose ledger wants the bucket kind
  writes its own (about twenty lines).
- **Its own tables.** `Campaigns::migrate(&pool)` creates `sc_*` tables in the schema the pool's `search_path` names,
  with its own `sc_migrations` history. It never reads or writes the router's tables.
- **Units.** Amounts are integers in the host's own unit; the sink converts to the router's atoms.
- **No app names, no payment provider, no environment variables.** Rates, holds and limits are data the host stores
  (`Settings`, rate classes) and the extension validates.

## Referral

An inviter has a code (made on first use). A new customer attaches it once, inside a window after their account was
made and before their first purchase; the first attachment wins. A purchase by the invited customer pays the inviter
`floor(base × rate)` where the rate comes from the inviter's **rate class** (`default` is 3%; a host adds more, for
example 5% for partners). The rebate is **held** for `hold_days` (default 7) and only then granted, so a disputed
payment can still void it (`void_referred`). Granted rebates are never taken back: the router has no revoke.

Abuse handling: self-referral is refused; two people inviting each other are flagged; a referral that shares a hashed
signal (normalised e-mail, network prefix, device id, whatever the host reports) with the inviter, or with more than
`max_per_signal` of the inviter's other invitees, is **flagged** and earns nothing until a person approves it
(`review`). Optional per-inviter cap and rebate window.

## Codes

`create_batch` makes serial codes (`XXXX-XXXX-XXXX`, 55 random bits plus a check symbol) or one chosen word, each worth
an amount, usable once or by up to N customers (each customer once per code), redeemable until a date, with credits that
optionally expire. The codes are returned **once**; only keyed hashes and the last four symbols are stored. Every
redemption is audited. A customer who tries too many unknown codes, and a network that does, is locked out for a window.

## Not built

- **Reset** (reset one model's usage; reset quota and time together; reset quota but not time) acts on router windows
  and buckets. It needs one general admin primitive in the core first; see the decision record.
- Go, Python and TypeScript ports. The pure parts share `conformance/campaign.v1.json`.
- A hosted API (super-router-cloud).

## Tests

```bash
CAMPAIGN_TEST_DATABASE_URL=postgres://… cargo test -p super-router-campaign
```

Without a database the integration tests skip, unless `SR_REQUIRE_POSTGRES` is set, as in CI.
