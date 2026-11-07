# Hosted payments：Stripe、易支付 V1、應援

本次提供 `@supersolutions/super-router-payments` 三個可替換 adapter，以及 PostgreSQL checkout dispatch、HTTP ingress、SDK 和可執行離線範例。真實 HTTP client 已實作；目前驗證為協定 fixtures 與本地 PostgreSQL，尚未用商戶帳號完成實際付款。

## 能力與金額

| Adapter | 建立付款 | 確認付款 | 本版限制 |
| --- | --- | --- | --- |
| StripePayments | hosted Checkout `mode=payment` | raw HMAC 驗簽、account 核對、session 回查 paid | USD/TWD/CNY/EUR/JPY；固定期數人工續付 |
| EpayV1Payments | 簽名 submit.php URL | V1 MD5 通知、api.php 訂單回查 | CNY；自訂 HTTPS 相容站點；不包含 V2 RSA |
| OenPayments | Payment API checkout | raw HMAC 驗簽、transaction 回查 | TWD 整元；需 CRM 開啟通知簽章 |

所有 `amount_minor` 都是 ISO minor units 字串，TWD 120000 = NT$1200。OEN API 接收整元，非 100 倍數會拒絕；易支付轉成兩位小數字串，不用浮點數計費。Capability manifest 明確回報 automatic_recurring/refunds/portal=false；不要因業者原生提供該功能就顯示可用。

本版付款支付既有 subscription 的固定 UTC `period_days` 期數，非每月自動扣款。獨立 credits top-up 商品、原生 recurring、現金退款／爭議帳務仍需後續實作。既有 credits 使用退款不等於信用卡退款。

## 呼叫流程與 API

1. Billing admin 建立 plan、customer、subscription，再用既有 `/v1/admin/subscriptions/{id}/payment-orders` 指定 integration_id（stripe / oen / epay-v1）。訂單鎖定當期價格與 credits。
2. `POST /v1/admin/payment-orders/{id}/checkout`，需要 bearer `billing:write` 與 Idempotency-Key。回傳 `{reference,url}`。SDK `checkout(orderId,key)`；不接受前端覆寫價格或導回 URL。
3. 前端導向 url。結帳網址可能包含簽名，勿寫一般 access logs／analytics。此版本 API 是商戶後端介面，不應把 billing admin key 發給顧客；付費 components 需接 BFF。
4. 業者呼叫 `/v1/integrations/payments/{integrationId}/notify`：Stripe/OEN POST 原始 JSON 與原生簽章 header；易支付 GET 簽名 query。驗簽、回查、核對持久化 reference／order／金額／幣別／租戶後才入帳。Stripe/OEN 回 200，易支付回純文字 success。無法回查時非 2xx，不能當作成功。
5. 通知遺失時呼叫 `POST /v1/admin/payment-orders/{id}/reconcile`，body `{}`；SDK `reconcilePayment(orderId,key)`。這是需排程／操作人員執行的回查入口，本版 worker 尚未定時掃描金流訂單。
6. 顯示 subscription 狀態／credits 餘額；return URL 本身絕不觸發入帳。

所有入帳繼續使用同一個 tenant advisory transaction lock、immutable payment event、grant source unique、entitlement unique。重送、並行 callback／reconcile 只發一次。付款晚於期末或取消後，轉 review_required，留待商戶處理現金退款，不發過期 credits。

## 中斷與恢復

Migration 005 新增 sr_checkouts：order FK、integration、不可在流程中更換的 merchant identity、creating/ready/unknown、reference/url。網路請求在 DB transaction 外進行；先持久化 creating。重送 ready 取回原結果，creating/unknown 不自動再呼叫業者。訂單即為 dispatch 冪等單位，不依不同 HTTP key 另開 checkout。

供應商建立成功但回應遺失時，從商戶後台／查詢 API 找到原交易，呼叫 reconcile body `{"reference":"業者原交易識別"}`（SDK 第三參數）。系統只接受 provider 回查已付款且 order、amount、currency 相符的交易，原子綁定後才入帳。易支付 reference 是商戶 out_trade_no，Stripe 是 cs_ session，應援是 27 字元內部 transactionId。未付或查無交易仍維持待對帳，不清除紀錄重新建立。應援 checkout 時效短，過期重開付款連結的產品流程尚未提供。

## 設定與執行

`npm run demo:payments`：三家離線 HTTP fixture，不會發網路請求或扣款。
`npm test`：包括實際 PostgreSQL 的去重、跨租戶拒絕、金額差異、逾時 fence、恢復、晚到付款。

`backends/typescript/server/src/payments.ts` 提供 `paymentIntegrations(workspace,environment,env)`。預設本地 server 對 demo/test 啟用有設定的業者。正式部署的 composition root 必須明確使用 live 租戶環境和獨立 merchant credentials；本地預設不會幫你開正式商戶。

| 環境變數 | 用途 |
| --- | --- |
| PAYMENT_SUCCESS_URL / PAYMENT_CANCEL_URL | 固定 HTTPS 顧客導回頁，三家共用 |
| STRIPE_SECRET_KEY / STRIPE_WEBHOOK_SECRET / STRIPE_ACCOUNT_ID | Stripe secret key、endpoint secret、acct_ merchant identity |
| OEN_TOKEN / OEN_WEBHOOK_SECRET / OEN_MERCHANT_ID | 應援 token、完整 pay_whsec_ secret、商家子網域名稱 |
| EPAY_ENDPOINT / EPAY_PID / EPAY_KEY / EPAY_NOTIFY_URL | 相容站點 HTTPS origin、商戶 ID、V1 密鑰、上述 notify endpoint |

金鑰只從環境／secret manager 注入，不提交 repo。Stripe webhook 設定 checkout.session.completed 與 checkout.session.async_payment_succeeded。應援的通知 URL 與 secret 在 CRM 配置。易支付必須確認站點實際提供 V1 query API 與通知格式；協定相容不代表所有分支都已驗收。V1 查詢要求 query key，反向代理與出站追蹤應遮蔽完整 query；adapter 不輸出供應商錯誤內容。

上線前還需逐商戶完成付款、延遲付款、通知重送／遺失、金額與幣別、取消／過期，以及憑證輪替驗收。未提供 production deployment、費用承擔／代收代付資格或稅務自動化。

## 實作依據（2026-11-05 查核）

- [Stripe Checkout create](https://docs.stripe.com/api/checkout/sessions/create)、[webhook](https://docs.stripe.com/events/manage-webhook-endpoints)：固定 API version 2026-10-28.endive。
- [易支付 V1 相容協定](https://pay.lanfucai.com/doc/v1_legacy_api.html)：submit.php、MD5、通知與單筆查詢格式。站點只是協定參考，不是平台預設收款商戶。
- [應援 Checkout](https://developer.oen.tw/api/checkout/)、[交易物件](https://developer.oen.tw/api/objects/transaction/)、[通知](https://developer.oen.tw/developers/webhooks/)：以目前逐頁文件為準。
