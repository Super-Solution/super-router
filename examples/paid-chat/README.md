# Paid chat：本機沙盒工作台

此為 self-host 本機範例，不是 Cloud 部署。Cloud 不提供 BYOK；此範例也不接受上游 provider keys。

這是可執行的「訂閱 → 獨立付款頁 → 可信通知授額 → 對話／媒體計量 → 扣款 → 期末取消」範例。付款與模型均為明示的本機模擬；不扣真實款項，不呼叫付費模型，不產生實際圖片、影片或音檔。

## 啟動

需 Node 24、npm、Docker。從 repository root 執行：

```sh
npm ci
npm run db:up
npm run demo:paid-chat
```

開啟 <http://127.0.0.1:4317>，按「使用沙盒示範帳戶」，選 Starter／Creator，再到獨立的 4318 沙盒付款頁確認。尚未按付款就返回時，訂單保持 pending；只有簽名通知經 adapter 驗證、付款查詢與核心原子授額完成後，額度才會出現。

也能輸入此 demo workspace 已發行的 **customer SuperRouter API key**。這不是 OpenAI 等上游供應商的 API key，也不是管理員 key。輸入欄使用 password；連線後清空。瀏覽器只持有 HttpOnly、SameSite=Strict 的短效 session cookie，不使用 localStorage／sessionStorage。伺服器只在記憶體保留 customer key，每次請求重新驗證撤銷／期限。重啟須重新登入。商戶憑證不交給瀏覽器。

未提供登入／註冊產品；示範帳戶僅允許 loopback 沙盒操作，不接受任意 customer ID。BFF 路由固定自己的 workspace、environment、integration、可售 offer，拒絕額外 customer／price／商戶欄位。宿主部署時應替換登入及持久 session 層，並配置自己的公開 catalog／支付 adapter；此 server 不可當 production server 使用。

## 計費

Starter TWD 99／固定 30 天含 10 credits；Creator TWD 249／30 天含 30 credits。每 credit 為 1,000,000,000 ledger atoms。金額使用 ISO minor units；所有 atoms／金額以 decimal integer 字串傳輸，以 BigInt 精確格式化。當期額度不結轉，需手動續期；不宣稱曆月／自動續扣。

- 對話：輸入 0.000002、輸出 0.000003 credits/token；MockProvider 回傳可信 token meters。
- 圖片：0.2 credits／張，最多 4 張。
- 影片：0.05 credits／秒，最多 30 秒；核心以 video_ms 計量。
- 音訊：0.01 credits／秒，最多 60 秒；核心以 audio_ms 計量。
- 任務：0.1 credits／成功次，固定一次。

媒體使用 `MockTaskProvider`，回傳明示模擬的文字回條與可信用量，不產生二進位媒體。輸入只接受文字，附件會明確拒絕。Stop 會傳遞中止訊號，但已發送的請求可能保留 pending 預留額度，不能保證退款；再次重試沿用原 key。成功回覆的 Retry 會先確認新請求將再次計費；網路失敗重試沿用原 request key，已處理請求只顯示原狀態且不重複扣款。送出時仍由 core 保留最大用量，介面上的餘額／權益不是永久放行憑證。價格、model supply cost、session 及 raw prompt 不暴露給其他 customer；usage 只回傳自己的最近 20 筆摘要，沒有完整歷史分頁。

## 可重用的部分

`@supersolutions/super-router-billing-presets` 是 server-only 套件，提供 preset 驗證、idempotent 發布、固定商戶 customer BFF；`@supersolutions/super-router-ui` 提供 browser client、PricingTable、CheckoutStatus、CreditBalance、SubscriptionCard、Paywall、UsageSummary。`@supersolutions/super-chat-core`、`-client`、`-react`、`-ui` 固定使用 npm 0.1.0，實際使用 AgentClient／AgentProvider／Thread／Composer。

公開 catalog 只從目前啟用 Service 版本讀取 retail rate／asset／version；開發者發布新費率後重新整理即可看到新比率，不會暴露上游成本。可整除時顯示「1 credit＝N tokens」，其他情況保留精確單位費率。客戶送出時只能帶目前顯示的 `expected_service_version`，不能指定售價；費率已換版時，新請求回傳 409 且不預留／扣款，畫面重載費率並要求檢視後再次送出。已接受請求的原 key 重播保留原先帳務結果。

核心 customer use cases 在同一交易驗證 customer ownership，包含 checkout 冪等重試、讀自己的 order、billing、期末取消。新的 request key 不會為同 customer 的同一有效訂閱再建一份 subscription。待付款方案要更換時，先取消再重新選擇。已付款當期取消後，權益保留到期；沒有即時退款或比例退費。Portal、自動續扣、補額商品、試用及跨供應商真實收款驗收尚未提供。

`LocalSandboxPayments` 是獨立明示的本機 adapter，不冒充 Stripe／易支付／應援金流。現有正式 adapter 能透過相同 `ControlPlane` hosted port 接入；本範例預設只配置本機能力，也可依下節設定商戶測試／正式通路；不宣稱 live 商戶已驗收。

## 驗證與清理

```sh
npm run typecheck
npm test
npx playwright install chromium
npm run test:paid-chat:browser
```

可用 `PLAYWRIGHT_EXECUTABLE_PATH` 指向本機 Chromium／Brave。瀏覽器測試使用 4321／4322 完成付款、chat、5 秒影片結算、取消、登出，並保存 desktop／390px mobile screenshot 至 `/tmp/superrouter-paid-chat`，驗證沒有水平溢位與 JS runtime errors。HTTP 測試使用 4319／4320。

設定名稱見 `.env.example`；程序不自動載入 env 檔，請由宿主環境注入。預設 sandbox fixture signing material 是公開的本機開發資料，只搭配固定 test workspace 和 loopback bind；可用 `PAID_SANDBOX_SECRET` 注入自己的 server-only fixture，重啟保持相同值使待付款連結可再次提交。不同值請用新 sandbox workspace／乾淨 DB。bootstrap、catalog、customers、services 重跑不重複授額；只在登入時建立短效 app key。

Ctrl-C 關閉 app／checkout。`npm run db:down` 停止資料庫並保留 volume；刪除 volume 會清除整個本機資料庫，請僅在確認不要其他 demo 資料時使用 Docker 的明確 volume cleanup。HTTP／browser tests 使用獨立 workspace，資料保留供檢查。

## 同一 App 選擇 Stripe、應援金流、易支付

預設 `PAID_PAYMENT_MODE=fixture`。同一個「付款通路」選單提供原有沙盒，以及 Stripe／應援金流／易支付 V1 本機協定模擬；三家實際執行 `@supersolutions/super-router-payments` adapters，注入本機 fetch fixture，沒有外部付款／供應商網路請求。fixture 驗證 adapter 實際建立的金額、幣別及商戶協定欄位，再顯示獨立模擬付款頁。付款按鈕透過實際 HTTP 通知入口送出供應商格式與簽章；核心再經相同 adapter 的 authoritative lookup、order snapshot 核對及原子 ledger 授額。

1. 連線示範顧客，先選「付款通路」，再選方案。
2. Stripe／應援金流使用 TWD Starter 99／Creator 249；易支付使用獨立 CNY Starter 19／Creator 49。這些是獨立版本化商品定價，**沒有匯率轉換**，各自授予 10／30 credits。
3. 可先按「稍後付款，返回應用」驗證仍未授額，再用「繼續付款」沿用原訂單／原通路。回到 App 會顯示原通路、幣別、付款狀態及自己的額度。
4. 要在同一 App 試另一家，按「開啟新沙盒顧客」明確建立隔離顧客，再選通路。新顧客沒有額度或授額；只有付款通知才授額。按「返回示範顧客」可回到原固定示範顧客及原餘額。這些新顧客不模擬正式註冊，也不輸出／儲存其 key；session 過期或重啟後可重新建立新沙盒顧客，原固定顧客仍可重連。

一份當期待付款訂單綁定既定通路；不能換通路重新開同一張帳單，也不能按不同按鈕重複取得當期額度。需要更換未付方案時先取消；測多家建議使用新沙盒顧客。所有通路首版只有單次 hosted checkout、manual cycle payment 和 lookup；沒有 automatic recurring、portal 或 refunds。模擬通過不代表實際商戶驗收通過。

三家完整 browser 回歸：

```sh
npm run build
npx tsx scripts/paid-gateways-browser.ts
```

若未安裝 Playwright Chromium，先 `npx playwright install chromium`，或設定 `PLAYWRIGHT_EXECUTABLE_PATH`。截圖輸出 `/tmp/superrouter-paid-gateways`。HTTP fixtures 測試在 `tests/paid-chat-gateways.test.ts`，涵蓋錯誤簽章、authoritative 金額不符、重送、跨顧客越權、付款授額與實際扣款。

## 可設定商戶測試／正式通路（尚未 live 驗收）

仍是同一份 App/BFF，將 `PAID_PAYMENT_MODE=merchant` 明確啟用；設定名稱見 `.env.example`。此模式完全移除原本沙盒／本機模擬通路、停用 demo login／新沙盒顧客及 `/checkout` 模擬付款，不使用公開 fixture owner key。必須提供私密的 `PAID_SERVER_SECRET`、`PAID_HOST_KEY`、獨立 `PAID_WORKSPACE`、HTTPS `PAID_PUBLIC_ORIGIN`，以及各家 adapter 所需商戶憑證。客戶只能用該 workspace/environment 的 app key 登入；商戶 secret／owner key 只放伺服器環境，不能放前端、URL、終端輸出或版控。

各通路 `PAID_STRIPE_MODE`、`PAID_OEN_MODE`、`PAID_EPAY_MODE` 預設 `off`，可明確設為 `test` 或 `live`。同一次啟動的所有 enabled 通路必須同模式，混合 test/live 會拒絕啟動。test 使用 `environment=test`，live 使用 `environment=live`，且建議不同 workspace、資料庫與不同 `PAID_HOST_KEY`，避免測試付款進入正式顧客額度帳本。易支付 V1 本身沒有 test/livemode 標記；`test` 必須由商戶提供專用測試站／測試商戶，不能用正式站的真實收款能力冒稱測試。

範例 listener 一律 bind loopback。選用的拓撲是 **同一個公開 HTTPS App origin**：以 HTTPS reverse proxy 將 `PAID_PUBLIC_ORIGIN` 的完整 App、`/api/*` 及 assets 代理到 `127.0.0.1:PAID_CHAT_PORT`，保留公開 `Host` header；BFF 的 Host、Origin、Secure cookie 均檢查該公開 origin。使用者開啟公開 HTTPS URL，不能在 merchant 模式用 loopback URL登入。公開 origin 不接受額外 path、query、credentials 或 fragment。不要把 loopback checkout 模擬 listener 對外公開。

通知端點（只允許已啟用 gateway；限制 raw body／query 64 KiB）：

| 商戶 | 公開通知入口 | 驗證與回應 |
| --- | --- | --- |
| Stripe | `POST /api/payments/stripe/notify` | 原始 body + `stripe-signature`；adapter 查詢支付狀態；成功 `ok` |
| 應援金流 | `POST /api/payments/oen/notify` | 原始 body + adapter 的 `oenpay-signature` 契約；查詢交易；成功 `ok` |
| 易支付 V1 | `GET /api/payments/epay/notify?<原始簽名參數>`，亦接受 POST form | 原始 query／form、pid／MD5、authoritative 查詢；成功 `success` |

Stripe／應援金流需在商戶平台配置對應 HTTPS webhook URL；OEN live 通知需可由公網連到 HTTPS 443。OEN 的實際通知簽章／事件契約仍須依商戶開通資料完成驗收；本範例沿用現有 adapter 能力，不因有設定欄位就宣稱商戶已核准或所有事件已支援。易支付 endpoint 及 notify_url 由 server 固定，不能由 browser 覆寫。

供應商 return URL 固定為 `PAID_PUBLIC_ORIGIN/?payment_return=1`，返回不授額。即使沒有 order query，登入後仍會查詢自己的 orders／balance，恢復原訂單狀態。退款／發票／爭議／對帳營運需另行建置；模型仍是明示 Mock，啟用 live 只代表設定了真實收款路徑，不代表完成商業部署。不要在尚未完成商戶測試與責任確認時進行實際付款。
