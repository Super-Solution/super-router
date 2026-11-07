# 三家 hosted payments 範例

需要 Node.js 24；在 repository 根目錄執行 `npm ci`、`npm run demo:payments`。

這是可直接執行的 adapter 協定範例，使用注入的 HTTP fixtures，無外部請求、無扣款、無 DB 寫入，不需金鑰或清理。每家應輸出 fixture=true、checkout_created=true、相符的 receipt 和明確的 capability manifest。Stripe／應援示範 NT$1200，易支付示範 CNY 1200。

完整 PostgreSQL／HTTP 的訂閱與付款整合測試執行 `npm run db:up`、`npm test`。測試使用隨機 workspace 並保留紀錄，可用 `npm run db:down` 停止容器，勿刪除共用 volume。

要連真實業者，使用 `@supersolutions/super-router-server` 的 `paymentIntegrations` composition factory 與 [金流設定文件](../../docs/PAYMENTS.md)。`.env.example` 只有變數名稱；透過 shell 或 secret manager 注入，server 不會自行載入檔案。預設 demo/test；不得將正式密鑰當成測試密鑰。此範例不代替商戶端付款驗收。
