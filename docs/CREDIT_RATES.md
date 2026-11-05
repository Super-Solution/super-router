# Developer configurable credit rates

One credit remains exactly **1,000,000,000 accounting atoms** (scale 9). Developers change how many tokens, images, milliseconds or successful tasks one credit buys; they do not redefine the ledger scale. Customer credit prices are separate from SuperRouter Cloud platform prices and provider supply costs.

`compileCreditPrice` and `publishCreditRateVersion` are exported from `@superrouter/core` (and re-exported by the server-only billing presets package). Configuration uses positive decimal integer **strings**, so no floating-point conversion occurs:

```ts
const config = {
  asset: 'credit:demo',
  tokens: {
    input: {units: '1000'},
    cached: {units: '2000'},
    cache_write: {units: '500'},
    output: {units: '250'},
  },
};
// Default credits:'1': one credit buys 1000 input tokens, 2000 cached tokens, etc.
// units:'1000',credits:'2' means 1000 units cost two credits.
```

Token configuration requires **all four** token categories; an empty configuration is rejected. A media service instead configures its matching single meter, e.g. `{asset:'credit:demo',media:{video_ms:{units:'20000'}}}` means one credit buys 20 video seconds. Image/task units are counts; video/audio units are integer milliseconds. The generic compiler can compose token/media prices; publishing validates the actual service kind and rejects missing or incompatible meters. Omitted media meters are unpriced, not implicitly enabled. No empty or wrong-kind edit can silently make an existing service free.

Compilation reduces each ratio with bigint arithmetic. Token rates share an exact least-common-multiple denominator; media meters retain independent denominators. Denominators above 1,000,000,000 or numerators outside the existing 30-digit atom limit fail with `unsupported_rate_precision`; rates are never silently rounded to fit. Actual usage still rounds charged atoms upward according to the documented Price calculation.

The authenticated developer API is:

```http
POST /v1/admin/services/chat/credit-rates
Authorization: Bearer <service-admin-key>
Idempotency-Key: <unique-command-key>
Content-Type: application/json

{"expected_version":1,"config":{"asset":"credit:demo","tokens":{"input":{"units":"1000"},"cached":{"units":"2000"},"cache_write":{"units":"500"},"output":{"units":"250"}}}}
```

`SuperRouterClient.publishCreditRateVersion(serviceId, expectedVersion, config, commandKey)` calls this endpoint. The asset must match the existing customer credit asset. `service:write` and key service restrictions apply. The server clones the existing service and changes **only customer-sale price and version**; platform prices, provider offerings, routing weights and bounds remain that service's snapshots.

Publishing performs an atomic expected-version check under the workspace transaction lock. Concurrent edits cannot both win or reactivate an older version. A successful command advances the version by one; repeating its old `expected_version` returns **409**, even with the same command key. Read the active service version before retrying. The endpoint does not promise command-response replay; its compare-and-set contract prevents duplicate revisions. The existing generic service publisher also rejects newly inserted versions older than the latest version.

Already reserved requests retain their old service/price snapshots, and the original inference idempotency key still replays that original request. New invocations use the active price. Explicitly pinning an inactive `service_version` on a new invocation returns `inactive_service_version`; it cannot bypass updated prices. Historical versions remain available for audit and request records.

In **SuperRouter Cloud**, the platform owner provisions the initial platform-backed service and platform economics. Database-authenticated developer keys cannot initialize Cloud supply, remove/change `platform_price`, or replace provider offerings. This first boundary protects the entire offering object; later granular catalog/routing edits require an explicit platform API. The dedicated credit-rate endpoint preserves those fields while allowing customer-sale rate changes. In-process trusted host provisioning is an operator capability, not a customer bearer API; a Cloud HTTP deployment must use the database-backed ControlPlane authentication path. `createRouterServer` rejects Cloud construction with static principal arrays before opening a listener; static-key servers remain available in self-host mode. Do not expose trusted static host credentials to developer browsers.

In **self-host** mode the installing owner controls their own deployment and may configure BYOK and supply through the existing generic publisher. The credit-rate helper still modifies only customer-sale prices. This slice implements rate configuration and boundary checks; it is not a deployed Cloud service or a pricing administration dashboard.
