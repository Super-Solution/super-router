# SuperRouter

[English](../../README.md) · [繁體中文](README.zh-TW.md) · [简体中文](README.zh-CN.md) · **日本語**

パッケージとして組み込める AI ルーティング、課金、サービング基盤です。**Service**（販売する AI 機能）を定義し、背後のモデル、価格、サブスクリプション、クレジットを設定すれば、SuperRouter がリクエストのルーティング、使用量の計測、精算を行います。

> **ステータス：開発版（0.1.0）。** ローカルのコアは一通り動作します。Service のバージョン管理、モデル選択、PostgreSQL による予約／精算、HTTP + SSE、SDK、復旧 worker、マルチテナントのロールと key、サブスクリプション、スコープ付きクレジットを含みます。OpenAI と Anthropic の adapter は公式形式の fixture でテスト済みですが、実際の上流での受け入れ確認は未完了です。3 つの決済 adapter も実際の加盟店アカウントでは未検証です。ホスト型 Cloud はなく、本番運用可能という主張もしていません。

## 機能

- **Service 中心のルーティング。** Service は特定モデル、明示的なモデルプール、タスクレベルの tier に紐付けられます。まずハード制約（地域、タグ、コスト上限）を適用し、次に重み付けスコアリングを行います。fallback は上流が明示的に拒否した場合に限り、範囲を限定して行います。
- **正確な課金。** input、cached、cache-write、output の各レートを整数（BigInt）で計算します。呼び出し前に予約し、呼び出し後に精算します。再試行で顧客に二重請求することはなく、上流の結果が不明な場合は予約を保持して手動照合に回し、黙って返金することはありません。
- **冪等なサービング。** HTTP/SSE、idempotency key、再送状態、キャンセル意図、顧客に紐付いた principal に対応します。prompt と completion は保存しません。
- **サブスクリプションとクレジット。** プラン、固定日数のサブスクリプション、credit bucket、付与、残高不足時の paywall 処理。
- **ホスト型決済。** Stripe Checkout、Epay V1、OEN（應援）の adapter。署名検証と照会に対応します。
- **復旧。** Worker が期限切れの予約を解放し、確認済みの使用量を再精算し、不明な dispatch を照合待ちにします。

## パッケージ

| パス | 役割 |
| --- | --- |
| `packages/contracts` | ランタイムスキーマ、型、エラー、SSE パーサー |
| `packages/core` | ルーティング、BigInt 計算、予約／実行／精算フロー、mock adapter |
| `packages/adapters/storage-postgres` | Migration、テナントスコープ、accounts/buckets/journal、requests、outbox と復旧 |
| `packages/adapters/provider-openai` | OpenAI Responses テキストストリーミング adapter |
| `packages/adapters/provider-anthropic` | Anthropic Messages テキストストリーミング adapter |
| `packages/adapters/payments` | Stripe、Epay V1、OEN のホスト型チェックアウト adapter |
| `packages/sdk` | HTTP クライアント、SSE、再送状態、キャンセル |
| `backends/typescript/billing-presets` | サーバー専用：preset の検証、冪等な公開、顧客向け BFF |
| `packages/bindings/react` | React コンポーネント：料金表、チェックアウト状態、残高、paywall、使用量サマリー |
| `backends/typescript/server` | HTTP + SSE サーバー：`createRouterServer`、決済連携の組み立て |
| `backends/typescript/worker` | 復旧 worker ライブラリと `super-router-worker` コマンド |
| `apps/server`、`apps/worker` | ローカルのデモ用エントリ（mock モデル、demo workspace） |

パッケージは `@supersolutions` スコープで公開されます。例：`@supersolutions/super-router-core`、`@supersolutions/super-router-sdk`。

## クイックスタート

Node.js 24、npm、Docker が必要です。リポジトリのルートで実行してください。

```sh
npm ci
npm run build
npm run db:up
npm run demo
npm run demo:subscription
npm run demo:payments
```

デモは mock モデルを使うため、モデル利用料はかかりません。データベース、HTTP、ストリーミング、会計は実際に動作します。モデル呼び出しが 1 回行われてテスト用の 35 atoms が課金され、同じ key での再送では追加課金されません。詳細は [minimal-chat](../../examples/minimal-chat/README.md) を参照してください。

```sh
npm run typecheck
npm test
npm run contracts
```

テストには開発用データベースが必要です。`DATABASE_URL` を設定すると既定の `localhost:25432` を上書きできます。テストはランダムな workspace を使い、既存データを削除しません。`npm run db:down` は専用コンテナを停止し、volume は保持します。サーバー設定は `.env.example` を参照してください。

## ドキュメント

設計ドキュメントは現在、繁体字中国語で書かれています。

- [PRD](../../docs/PRD.md)、[アーキテクチャ](../../docs/ARCHITECTURE.md)、[データモデル](../../docs/DATA_MODEL.md)、[目標 API](../../docs/API_SPEC.md)、[実装済み OpenAPI](../../docs/generated/openapi.json)
- [決済](../../docs/PAYMENTS.md)、[P2 課金](../../docs/P2_BILLING.md)、[有料コンポーネントと preset](../../docs/PAID_COMPONENTS_PRESETS.md)
- [P0 の決定](../../docs/adr/0001-p0-baseline.md)、[Provider spike](../../docs/PROVIDER_SPIKE.md)、[検証記録と制限](../../docs/P1_VALIDATION.md)
- [サンプル](../../examples/README.md)、[Agent skill の計画](../../skills/README.md)

## 既知の制限

個人ウォレットの認可、完全な管理 UI、公開 SaaS はありません。Cloud で上流 BYOK を禁止するルールは、現時点ではリリースポリシーのテストとしてのみ存在します。支払いは固定日数の手動更新で、自動の定期課金ではありません。上流の結果が不明な場合は予約を保持して手動照合に回し、無料で再試行することはありません。

## ライセンス

Apache License 2.0。[LICENSE](../../LICENSE) と [NOTICE](../../NOTICE) を参照してください。

Copyright 2026 SUPER SOLUTION LABS INC.

連絡先：contact@supersolution.io
