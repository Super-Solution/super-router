# SuperRouter

可整合套件形式的 AI routing、計價與 serving 平台。現有本地核心可執行：Service 版本、模型選路、PostgreSQL 預留／結算、HTTP/SSE、SDK 與恢復 worker；P2 已加入持久化角色／keys、訂閱與 scoped credits。

目前是開發版，套件為 private／UNLICENSED，尚未發佈或部署 Cloud。OpenAI／Anthropic adapters 已以官方格式 fixtures 測試，未完成真實上游驗收；沒有 production readiness 宣稱。

## 快速執行

需要 Node.js 24、npm、Docker。請在此 repository 根目錄執行：

```sh
npm ci
npm run build
npm run db:up
npm run demo
npm run demo:subscription
npm run demo:payments
```

Demo 使用 mock 模型，不花模型費用；資料庫、HTTP、串流與帳務是真實執行。預期一次模型呼叫、扣 35 個測試 atoms、重送不再扣款。詳見 [最小範例](examples/minimal-chat/README.md)。

```sh
npm run typecheck
npm test
npm run contracts
```

測試需要開發 DB；設定 `DATABASE_URL` 可覆寫預設 localhost:25432。Tests 使用隨機 workspace，不刪除既有資料。`npm run db:down` 停止專用容器並保留 volume。

## 套件

| 路徑 | 功能 |
| --- | --- |
| `packages/contracts` | Runtime schema、型別、錯誤、SSE parser |
| `packages/core` | Routing、BigInt 計價、預留／執行／結算流程、mock adapter |
| `packages/storage-postgres` | Migrations、租戶 scope、accounts/buckets/journal、requests、outbox/recovery |
| `packages/provider-openai` | OpenAI Responses 純文字串流 adapter |
| `packages/provider-anthropic` | Anthropic Messages 純文字串流 adapter |
| `packages/payments` | Stripe／易支付 V1／應援 hosted checkout adapters |
| `packages/sdk` | HTTP client、SSE、重送狀態與取消 |
| `apps/server`、`apps/worker` | 本地 mock composition root、HTTP server、恢復 worker |

## 文件與進度

- [三家金流整合與限制](docs/PAYMENTS.md)、[付費 components／presets 規劃](docs/PAID_COMPONENTS_PRESETS.md)。
- [P2 多租戶／訂閱／credits 規格與驗證](docs/P2_BILLING.md)。

- [P0 決策](docs/adr/0001-p0-baseline.md)、[Provider spike](docs/PROVIDER_SPIKE.md)、[本次驗證與限制](docs/P1_VALIDATION.md)。
- [PRD](docs/PRD.md)、[技術架構](docs/ARCHITECTURE.md)、[資料模型](docs/DATA_MODEL.md)。
- [完整目標 API](docs/API_SPEC.md)、[目前實作的 OpenAPI](docs/generated/openapi.json)、[實作計畫](docs/IMPLEMENTATION_PLAN.md)。
- [範例 App 規劃](examples/README.md)、[Agent skill 規劃](skills/README.md)。[訂閱範例](examples/subscription-credits/README.md) 已可執行；BYOK 部署範例與可安裝 skill 仍待後續階段。

目前提供 sandbox 與三家 hosted payment adapters、固定日數訂閱；尚未完成真實商戶付款驗收，也沒有個人錢包授權、完整管理 UI 或公開 SaaS。Cloud 的上游 BYOK 禁令有發布政策測試，但尚無實際 Cloud 服務。執行過程的 unknown 結果保留預留並進入人工對帳，不自動免費重試。
