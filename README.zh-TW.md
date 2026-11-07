# SuperRouter

[English](README.md) · **繁體中文** · [简体中文](README.zh-CN.md) · [日本語](README.ja.md)

以套件形式整合的 AI routing、計價與 serving 基礎設施。你定義 **Service**（要販售的 AI 能力）、選擇背後的模型、設定售價、訂閱與額度，由 SuperRouter 負責選路、計量與結算。

> **狀態：開發版（0.1.0）。** 本地核心可完整執行：Service 版本、模型選路、PostgreSQL 預留／結算、HTTP + SSE、SDK、恢復 worker、多租戶角色與 keys、訂閱與 scoped credits。OpenAI 與 Anthropic adapters 已用官方格式 fixtures 測試，尚未完成真實上游驗收；三家金流 adapters 也尚未用真實商戶帳號跑過。目前沒有託管 Cloud，也沒有 production readiness 宣稱。

## 功能

- **以 Service 為中心的選路。** Service 可綁定指定模型、明確的模型池或任務等級 tier。先套用硬限制（地區、標籤、成本上限），再做加權評分；只有在上游明確拒絕時才有限度 fallback。
- **精確計價。** 以整數（BigInt）計算 input、cached、cache-write、output 費率。呼叫前預留、呼叫後結算；重試不會重複向客戶收費，上游結果不明時保留預留並進入人工對帳，不會悄悄退款。
- **冪等的 serving。** HTTP/SSE、idempotency key、重送狀態、取消意圖與綁定客戶的 principal。不儲存 prompt 與 completion。
- **訂閱與 credits。** 方案、固定日數訂閱、credit buckets、授額，以及額度不足時的 paywall 處理。
- **託管金流。** Stripe Checkout、易支付 V1、應援（OEN）adapters，含簽章驗證與回查。
- **恢復。** Worker 釋放過期預留、重做已確認 usage 的結算，並把不明 dispatch 留待對帳。

## 套件

| 路徑 | 功能 |
| --- | --- |
| `packages/contracts` | Runtime schema、型別、錯誤、SSE parser |
| `packages/core` | Routing、BigInt 計價、預留／執行／結算流程、mock adapter |
| `packages/adapters/storage-postgres` | Migrations、租戶 scope、accounts/buckets/journal、requests、outbox 與恢復 |
| `packages/adapters/provider-openai` | OpenAI Responses 純文字串流 adapter |
| `packages/adapters/provider-anthropic` | Anthropic Messages 純文字串流 adapter |
| `packages/adapters/payments` | Stripe、易支付 V1、應援 hosted checkout adapters |
| `packages/sdk` | HTTP client、SSE、重送狀態與取消 |
| `backends/typescript/billing-presets` | 僅限伺服器端：preset 驗證、idempotent 發布、客戶 BFF |
| `packages/bindings/react` | React 元件：價格表、結帳狀態、餘額、paywall、用量摘要 |
| `backends/typescript/server` | HTTP + SSE server：`createRouterServer`、金流整合接線 |
| `backends/typescript/worker` | 恢復 worker 程式庫與 `super-router-worker` 指令 |
| `apps/server`、`apps/worker` | 本地 demo 入口（mock 模型、demo workspace） |

套件發佈在 `@supersolutions` scope 下，例如 `@supersolutions/super-router-core`、`@supersolutions/super-router-sdk`。

## 快速執行

需要 Node.js 24、npm、Docker。請在 repository 根目錄執行。

```sh
npm ci
npm run build
npm run db:up
npm run demo
npm run demo:subscription
npm run demo:payments
```

Demo 使用 mock 模型，不花模型費用；資料庫、HTTP、串流與帳務是真實執行。預期一次模型呼叫、扣 35 個測試 atoms，以相同 key 重送不再扣款。詳見 [minimal-chat](examples/minimal-chat/README.md)。

```sh
npm run typecheck
npm test
npm run contracts
```

測試需要開發資料庫。設定 `DATABASE_URL` 可覆寫預設的 `localhost:25432`。Tests 使用隨機 workspace，不刪除既有資料。`npm run db:down` 停止專用容器並保留 volume。伺服器設定請參考 `.env.example`。

## 文件

- [PRD](docs/PRD.md)、[技術架構](docs/ARCHITECTURE.md)、[資料模型](docs/DATA_MODEL.md)、[完整目標 API](docs/API_SPEC.md)、[目前實作的 OpenAPI](docs/generated/openapi.json)
- [金流整合](docs/PAYMENTS.md)、[P2 多租戶／訂閱／credits](docs/P2_BILLING.md)、[付費 components／presets](docs/PAID_COMPONENTS_PRESETS.md)
- [P0 決策](docs/adr/0001-p0-baseline.md)、[Provider spike](docs/PROVIDER_SPIKE.md)、[驗證紀錄與限制](docs/P1_VALIDATION.md)
- [範例 App](examples/README.md)、[Agent skill 規劃](skills/README.md)

## 已知限制

沒有個人錢包授權、完整管理 UI 或公開 SaaS；Cloud 禁止上游 BYOK 的規則目前只有發布政策測試。付款為固定日數的人工續付，不是自動週期扣款。上游結果不明時保留預留並進入人工對帳，絕不免費重試。

## 授權

Apache License 2.0。見 [LICENSE](LICENSE) 與 [NOTICE](NOTICE)。

Copyright 2026 SUPER SOLUTION LABS INC.

聯絡：contact@supersolution.io
