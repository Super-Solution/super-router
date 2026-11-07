# Minimal Chat

可執行的 P1 範例：**mock 模型、真實 PostgreSQL、真實 HTTP/SSE 與 SDK**。不需要 Provider key，不會呼叫付費模型。

從 repository 根目錄執行：

```sh
npm ci
npm run build
npm run db:up
npm run demo
```

需 Node.js 24 與 Docker。資料庫只綁 `127.0.0.1:25432`；若使用自己的資料庫，設定 `DATABASE_URL`。請使用專屬開發資料庫，migration 會建立 `sr_*` tables。

範例每次建立新的 demo workspace，啟動隨機 localhost port，透過 SDK 消費 SSE，查詢結果與帳目，再重送相同 key 驗證只執行一次。預期：

- `request.started` → `output.delta` → `request.completed`。
- `execution_status=succeeded`、`billing_status=settled`。
- 測試初始餘額 100000 atoms，扣款 35，剩餘 99965、held 0。
- 重送得到 `request.replayed`，`provider calls 1`。
- trace 顯示 Provider cost 15 與 customer charge 35，分屬不同資產，不是換匯。

程式結束會關閉 HTTP server 與 DB pool；資料保留於本地 Docker volume，便於查驗。`npm run db:down` 停止開發 DB 且保留資料。只有確定不要開發資料時才自行執行 `docker compose down -v`。

## 常駐 mock server／worker

常駐入口已升級為 P2 DB authorization，不再自動 seed P1 customer/service。設定自己的 `SUPERROUTER_API_KEY`（`sr_` 加 64 個隨機 hex 字元）與 `SUPERROUTER_DIGEST_KEY`（至少 32 字元）；範本見根目錄 `.env.example`。目前命令不自動讀 `.env`，需由 shell 或部署者載入。

```sh
npm start
# 另一個終端：
npm run worker
```

Server 只綁 localhost，預設 3410。首次以提供的 key bootstrap demo/test owner，重啟驗證既有 key，不覆寫權限。Owner key 有效 30 日，輪替後需更新環境值。可直接執行 `npm run demo:subscription` 查看完整 P2 組裝範例。Worker 恢復過期請求並保留 outbox，未配置 subscriber 時不假裝事件已交付。這個入口是本地開發用，不是 production deployment。

## 嵌入模式

`index.ts` 使用 `Router`、`PostgresStore`、`MockProvider` 組裝核心，再建立 HTTP server；宿主可直接呼叫 `router.infer(principal, input, idempotencyKey, onEvent)`，不需要管理 UI。Principal 必須來自宿主驗證，不能由瀏覽器任意傳入。

真實 Provider adapter 的 smoke 與限制見 [Provider spike](../../docs/PROVIDER_SPIKE.md)。
