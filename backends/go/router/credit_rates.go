// SPDX-License-Identifier: Apache-2.0

package router

import (
	"context"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/pricing"
)

// PublishCreditRateVersion advances only the customer-sale price of a service to a new version compiled from
// exact units-per-credit rates. Supply and platform prices stay as the server's snapshot. config is a decoded
// JSON value; expectedVersion must be the service's current version.
func PublishCreditRateVersion(ctx context.Context, store DataStore, p contracts.Principal, id string, expectedVersion int64, config any, mode string) (contracts.Service, error) {
	if !p.HasScope("service:write") {
		return contracts.Service{}, fail("scope_denied", 403)
	}
	if expectedVersion < 1 || expectedVersion >= 1<<53-1 {
		return contracts.Service{}, fail("invalid_service_version", 400)
	}
	current, err := store.GetService(ctx, p, id, nil)
	if err != nil {
		return contracts.Service{}, err
	}
	if current.Version != expectedVersion {
		return contracts.Service{}, fail("service_version_conflict", 409)
	}
	price, err := pricing.CompileCreditPrice(config)
	if err != nil {
		return contracts.Service{}, err
	}
	cfg, _ := config.(map[string]any)
	_, hasTokens := cfg["tokens"]
	_, hasMediaKey := cfg["media"]
	media, _ := cfg["media"].(map[string]any)
	var mismatch bool
	switch {
	case current.TaskKind != nil && len(current.Price.Units) > 0:
		// A service billed in custom units keeps being billed in custom units.
		mismatch = len(price.Units) == 0 || hasTokens || hasMediaKey
	case current.TaskKind != nil:
		meter := contracts.KindMeter[*current.TaskKind]
		_, hasMeter := media[meter]
		mismatch = !hasMeter || hasTokens || len(price.Units) > 0
		for k := range media {
			if k != meter {
				mismatch = true
			}
		}
	default:
		mismatch = !hasTokens || len(media) > 0 || len(price.Units) > 0 || hasMediaKey
	}
	if mismatch {
		return contracts.Service{}, fail("credit_rate_kind_mismatch", 422)
	}
	if price.Asset != current.Price.Asset {
		return contracts.Service{}, fail("credit_asset_mismatch", 422)
	}
	next := current
	next.Version = current.Version + 1
	next.Price = price
	if _, err := store.Publish(ctx, p, next, mode, &expectedVersion); err != nil {
		return contracts.Service{}, err
	}
	return next, nil
}
