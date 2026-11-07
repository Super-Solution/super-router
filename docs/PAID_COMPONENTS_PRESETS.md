# 付費 Components 與 Presets 規劃

- 日期：2026-11-05
- 狀態：首個本機沙盒 slice 已實作，見 [paid-chat README](../examples/paid-chat/README.md)。其餘表格仍是分期規劃；不代表真實收款或商業上線。
- 依據：[PRD](PRD.md)、[架構](ARCHITECTURE.md)、[P2 帳務](P2_BILLING.md)、[實作計畫](IMPLEMENTATION_PLAN.md)。
- 目標：讓開發者用既有公開帳務契約，採用定價、付款、訂閱、credits 與存取限制的完整旅程；不必複製一套帳本。
- 本文件的「付費 components」指協助 App 收費的元件；不預設元件本身的授權價格或商業方案。

## 1. 目前基線與需要保留的整合介面

檢視基線：`packages/contracts/src/control.ts`、`packages/sdk/src/index.ts`、`packages/adapters/storage-postgres/src/control.ts`、`backends/typescript/server/src/server.ts`。P2 已有方案、固定 UTC 日數週期、subscription、payment order、sandbox 付款事件與 credits；沒有已交付的 browser UI 套件、customer billing session、公開商品目錄、完整終端用量報表或 hosted portal。SDK 的管理方法需要伺服器憑證，不能直接作為前端 billing client。

Stripe、易支付、應援金流的 adapter 正由其他工作實作；本規劃不推定任一家已完成或具有自動續扣、portal、退款等能力。支援矩陣須由實際 adapter 契約測試及商戶測試帳號填入，不能只憑支付品牌宣告支援。

| 必須保留的 seam | 介面要求 | 避免耦合的原因 |
| --- | --- | --- |
| Payment port | 明確分開建立付款、驗證通知、查詢付款、可選 portal／退款；回傳 capability manifest | UI 依能力顯示操作，不散落 provider 分支 |
| 商戶 binding | integration 固定 merchant/workspace/environment；customer 由受驗 session 解析 | 禁止前端更換商戶或付款對象 |
| Order snapshot | 固定方案版本、商品版本、金額、幣別、credits、Service、週期及用途 | 收到通知後不能依當下售價或 browser payload 授額 |
| 付款結果 | 可查 opaque order ID 的付款狀態及授額結果，兩者分開 | return URL、使用者點擊或供應商頁面成功不是授額證據 |
| 事件入口 | adapter 驗證原始通知，再進入共用的去重與原子授額流程 | 不為各 provider 建另一套 ledger，也不把 sandbox HMAC 當正式 provider 簽章 |
| Hosted navigation | server 建立短效 checkout／portal URL，校驗供應商 origin 與宿主 return URL allowlist | 不接受 browser 提交任意 redirect 或外部付款 URL |
| Customer BFF | 宿主驗證使用者、customer ownership 與 session；只暴露自己的資料與動作 | 既有 billing:write 是租戶管理權，不能交給終端使用者 |
| 週期 adapter | 明確聲明 fixed-days/manual-renewal/calendar-recurring 的相容性 | 30 UTC 日不等於每月，未對齊不得啟用自動續扣 |

能力名稱建議：`one_time_checkout`、`manual_cycle_payment`、`automatic_recurring`、`hosted_portal`、`refunds`、`payment_lookup`。每個能力同時記錄 `supported`、`mode`、限制與驗證證據；不是單一布林就代表 production ready。

## 2. 三類交付物

| 類型 | 建議位置；均待實作 | 責任與採用方式 |
| --- | --- | --- |
| 可嵌入 UI components | `packages/bindings/react` | 首版 React bindings 加可替換樣式與文案；透過 customer billing client 操作 BFF，核心不依賴 React。框架支援以實際測試版本為準 |
| 服務端 billing presets | `backends/typescript/billing-presets` | 組合公開 contracts／core／SDK，提供設定驗證、BFF handlers、catalog 與 payment port binding；只在 server 匯入 |
| 可執行範例 | `examples/paid-chat`、`examples/credits-topup`、`examples/subscription-paywall` | 展示宿主登入、UI、BFF、worker 與支付模式；沿用正式公開介面，不以直接 SQL 或假 paid 按鈕代替正式流程 |

建議先交付 framework-neutral customer client 與 headless state，再加 React 元件；不為尚未支援的框架建立空殼。套件命名與 publish scope 於 P2-06 決定，以上路徑不代表已發佈。

## 3. UI 元件規格

所有元件接受宿主提供的 locale、字串與 formatting；整數 atoms／minor units 以字串傳送，禁止 Number 浮點換算作帳。預設提供鍵盤操作、焦點回復、螢幕閱讀器狀態及手機版排版。金額、週期、收費商戶與模式必須明示。

| 元件 | 最小資料／操作 | 必須呈現的狀態與邊界 |
| --- | --- | --- |
| `PricingTable` | 已發布可購買 catalog、選擇 product/plan version、付款模式 | loading／empty／unavailable；金額、幣別、固定天數、額度有效期、Service 範圍。不得標示未核實的「每月」或節省百分比 |
| `CheckoutButton` | catalog offer ID、受控 checkout action、idempotency key | creating／redirecting／failed；同次重試沿用 key，內容改變須新 key；不可接受 client 金額或自行授額 |
| `CheckoutStatus` | 自己的 order status 查詢 | unpaid／processing／paid-awaiting-grant／fulfilled／failed／expired／review-required；斷線後重查，不建立第二筆付款。這些是 UI 正規化狀態，不是現有 DB enum |
| `SubscriptionCard` | 自己的 subscription、entitlement、下次應繳與待生效變更 | pending／active／past_due／canceled、期末取消、下期方案；分開顯示可用權益與付款狀態 |
| `BillingPortalButton` | server 建立 portal session | unsupported 時隱藏該入口並提供本地訂閱頁；不把任意 provider 網站稱為 portal |
| `CreditBalance` | asset、可用／預留／到期額度、Service 範圍、讀取時間 | loading／stale／error／empty；區分購買、訂閱、贈送與最近到期。餘額不是法幣，不把全帳戶總量當此 Service 可用量 |
| `TopUpDialog` | server 發布的固定 credits pack、條款與 checkout action | 只讓使用者選 pack，不輸入任意換算或授額量；付款後等待可信授額 |
| `Paywall` / `ServiceGate` | Service、server access decision、可選補額／訂閱 action | subscription-required／insufficient-credits／permission-denied／pending／unknown；不靠隱藏 UI 保護服務，送出推論仍需 core 原子檢查 |
| `UsageSummary` / `UsageTable` | 自己的時間範圍、Service、requests、meters、售價扣款與 pending | pagination／empty／partial／pending-reconciliation；預設不顯示 Provider cost、其他 customer 或完整 prompt |
| `BillingHistory` | 自己的付款、授額與調整摘要 | 付款、credits 消耗、credits 退款、現金退款分開；未開立發票時不得將收據稱稅務發票 |

`Paywall` 的 access decision 只表示檢查時可用；併發、到期與 conservative reservation 均可能使實際呼叫拒絕。UI 應說明「請求可能需要預留最大用量」，不能承諾顯示非零餘額即可執行。

## 4. 開箱可用的服務端 Presets

所有 preset 都先 `validate`／dry-run，再由 owner 或 billing_admin 發布不可覆寫的版本。重跑設定使用穩定的 business key，不重建 customer、重複授額或修改歷史版本。preset 設定中的商戶帳號與 secrets reference 由 host 提供，不打包進前端。

| Preset | 初始範圍 | 必須設定 | 預設規則／缺口 |
| --- | --- | --- | --- |
| `subscription-credits` | 第一批 | plan/version、period_days、amount_minor/currency、asset/credits、service_ids、integration | 初始 pending；可信付款才授當期 bucket；不結轉、不負餘額、不自動超額。首版手動每期付款；自動續扣需 capability 與週期契約 |
| `subscription-paywall` | 第一批 | 上述加 Service `requires_subscription`、公開 feature copy | entitlement 必須涵蓋 Service；取消於期末、變更下期生效、僅同週期長度，無即時比例分攤；功能文字不擴張實際權限 |
| `credits-topup` | 第二批 | 版本化 pack、購買金額、授額、Service 範圍、有效期、integration | 需新增公開 one-time order 與可信授額 use case；不得以管理端 `grantCredits` 讓 browser 模擬購買 |
| `trial-to-paid` | 第二批 | 一次性 trial policy、promotion cap、到期、Service、轉付費 offer | 試用由 host 驗證身份與一次性資格；可信 server 用既有 promotion grant；無卡試用，不預設到期自動扣款。需持久化抗重複領取記錄 |
| `metered-credits` | 第一批 | Service price/version、asset、token meters、最大輸出與成本預算 | prepaid credits 配 per-token 扣費；保留預留／結算／pending。此 preset 不包含月末信用卡後付 |
| `task-pack` | P3 | Task 成功契約、費率、budget、步驟與取消政策 | Task runtime／成功證據完成前僅保留規格，不把每次 inference 包裝成已支援任務計費 |

第一批可組合 `subscription-credits` + `subscription-paywall` + `metered-credits`，形成「付款取得方案 → 當期額度 → 依 token 消耗」；同一次使用不可再收一筆 task fee。`credits-topup` 不要求 active subscription 的 Service 才可單靠購買額度使用；若 Service 要求訂閱，補額不能繞過 entitlement。

## 5. API、資料與權限契約

### 5.1 現有 API 的可重用範圍

實際 schema 以 [generated OpenAPI](generated/openapi.json) 及原始 contracts 為準。方案與 subscription 管理使用 `/v1/admin/plans`、`/v1/admin/subscriptions`、subscription 的 `/plan`、`/cancel`、`/payment-orders`。自己的 credits 使用 `GET /v1/credits`；目前回傳 buckets，不能假設已提供完整 available/held/expiry 聚合 DTO。管理端 balance、subscription 與 audit endpoints 不可直接授權給任意終端使用者。

現有一般 POST 要求 `Idempotency-Key`；payment ingress 使用來源簽章及 event 去重。新增 customer API 同樣需明確規定冪等 key 範圍、保存期限、相同 key 不同內容的 409，以及重試時的 order/session 行為。外部建單逾時為 unknown 時先 lookup／對帳，不盲目再收一次。

### 5.2 建議的 customer-facing BFF

以下是**待實作的宿主路由提案**，不是現有 SuperRouter endpoint，也不強迫宿主採用此 URL。BFF 只從登入 session 推導 workspace、environment、customer；忽略／拒絕 browser 覆寫身份。session cookie 必須搭配 CSRF／origin 檢查，寫入限流；付款來源通知另依 adapter 驗證。

| 路由提案 | 輸入與最小輸出 | server-side 檢查 |
| --- | --- | --- |
| `GET /api/billing/catalog` | 發布版本、公開價格、週期、pack、capabilities | 僅公開可售清單，沒有商戶 secrets／上游成本 |
| `POST /api/billing/checkout` | offer ID/version、受控 return target → order ID、expires_at、redirect URL | ownership、可購買版本、integration binding；伺服器計價、持久化 intent，再調用支付 port |
| `GET /api/billing/orders/:id` | payment status、fulfillment status、review reason、poll hint | 僅自己的 order；不洩漏 provider raw payload |
| `GET /api/billing/me` | subscription、entitlements、Service-scoped balances、as_of | 自己的資料；subscription status 與權益分開 |
| `POST /api/billing/subscription/cancel` | 受控 subscription ref → effective_at | 自己的 subscription；只能排程既定取消，不能調價或 grants |
| `POST /api/billing/subscription/change` | plan/version → next-period change | 可購買方案、相同週期長度與既有 order 限制 |
| `POST /api/billing/portal` | return target → short-lived URL | ownership、capability、merchant customer mapping；缺能力明確 unsupported |
| `GET /api/billing/usage` | cursor、受限期間、Service → 自己的用量及扣款 | Service scope、資料量限制、隔離成本與原文 |
| `GET /api/billing/access` | Service → decision/reason/as_of | 同源 domain rules；不回傳可重播的永久放行 token |

第一版可在宿主 server 用既有管理 API 實作必要操作，但必須先檢查 ownership，不能成為任意 `/admin/*` 代理。產品化時增加 customer-scoped use cases／scopes；新的 `subscription:self:write` 等名稱需在 contracts 定義後才使用，不是假定既有 app role 已有此權限。

建議 DTO 使用 decimal integer 字串並帶 `asset`／`currency`／`scale`、`service_ids`、`expires_at`、`as_of`。訂閱 ID、payment order ID、external merchant customer ID 必須各有持久化映射。付款成功但授額仍 pending 的情況在 UI 顯示等待，不再次收款；到期／取消後才收到成功款的 order 依 P2 進入 review，不倒退狀態。

### 5.3 商戶與資料邊界

| 主體 | 負責 | 不自動包含 |
| --- | --- | --- |
| 開發者／self-host 商戶 | 商戶申請、商品與條款、退款／拒付政策、稅與發票、客服、帳號身份、退款審核與對帳 | 安裝 preset 不等於商戶核准或符合上線要求 |
| SuperRouter core／preset | 按可信契約記帳、隔離權限、保留快照、去重、退款額度規則、audit 與可追蹤狀態 | 不替商戶保證付款供應商結算、稅務文件或爭議結果 |
| Payment adapter | provider 驗簽、金額／幣別／商戶／模式檢查、通知映射、可選查詢與退款 | 不能自己指定 credits，也不能繞過同一授額交易 |
| 宿主 UI／BFF | 登入、自己的資料、操作同意、可讀錯誤、回呼與支付模式展示 | 不保管商戶 secret、不接收卡號、不直接鑄造 credits |

Cloud 的「開發者購買 SuperRouter 平台額度」與「App 向自己的 customer 收款」使用不同 merchant context、catalog、orders 與帳戶；不能因付款 port 共用就混帳。Connect with SuperRouter、平台代收／分潤、跨 workspace credits、外匯、任意轉帳均留在另行規格。

現金退款與 credits 回補是不同操作；已消耗額度、拒付或撤回權益須經指定政策與追加式 journal。未完成退款 adapter 與流程時只提供客服／review 入口，不提供會顯示「已退款」的假成功按鈕。

## 6. 範例與完整使用旅程

1. `paid-chat`：登入 → PricingTable → pending subscription/order → hosted checkout → 回站顯示 processing → 可信通知授額 → 查詢自己的 balance／entitlement → chat 推論 → UsageSummary → 期末取消。首版可用 sandbox；畫面及 README 持續標示模式。
2. `credits-topup`：pack 選擇 → one-time order → 通知授額 → CreditBalance → 使用到預留不足 → Paywall 顯示補額。需等 one-time order 核心完成才稱可執行；未完成時只交付清楚標記的設計。
3. `subscription-paywall`：要求訂閱的 Service → trial／pending／past_due／active／cancel_at_period_end 狀態切換 → 下期方案變更 → 不支援 portal 的 provider 使用本地 SubscriptionCard。付款失敗不授新額；仍有效的獨立購買額度保留，但不能繞過 subscription-required。

每個範例提供 `.env.example` 的名稱與用途、sandbox 初始化、worker 啟動、cleanup、一次完整 assertions 與狀態查詢。secrets 只由環境／SecretStore 取得，範例輸出不含 token、簽章或付款敏感資料。live 操作須明列真實費用與所需商戶能力；本文件不宣稱已跑過。

## 7. 分階段交付與驗收

| 階段 | 交付 | 完成門檻 |
| --- | --- | --- |
| A：契約與 headless | capability manifest、catalog/DTO、customer ownership、order status、preset validation、BFF handlers | 公開契約可生成；雙租戶／雙 customer 越權讀寫拒絕；不輸出 secrets；不改現有 P2 週期語意 |
| B：第一個可採用旅程 | React PricingTable／Checkout／Status／SubscriptionCard／CreditBalance／Paywall，加 subscription presets 與 paid-chat | clean install 能以 sandbox 完成訂閱 → 授額 → 推論 → 扣款 → 取消；回站未收到通知時保持 pending |
| C：補額、用量及管理 | one-time packs、trial policy、Usage／History、能力限定 portal、第二與第三範例 | 重複領試用防護、補額一次授額、自己的分頁用量隔離；已持有額度與訂閱權益分別驗證 |
| D：逐家支付商戶驗收 | Stripe、易支付、應援金流各自填 capability／限制／模式證據；對帳、退款與恢復 runbook | 每家測試帳號完整旅程；重複、亂序、金額錯誤、簽章錯誤、外部成功後 DB 失敗及重放不重複授額；能力缺項如實記錄 |
| E：商業上線 | 接 P2-07／08：真實商戶、價格、退款／稅務、備份／告警與部署證據 | live 商戶能力與受控實際交易確認；對帳、退款及客服責任有 owner。sandbox 通過不替代此門檻 |

額外必測：取消後晚到款進 review；30 日與曆月差異；checkout 重試／斷線恢復；過期 session／登出；return URL 注入；前端竄改 price/customer/integration；非零餘額但預留不足；stream 中斷 pending；paid 與 fulfillment 分離；僅鍵盤完成付款與取消旅程。

已交付首個 slice：`backends/typescript/billing-presets`、`packages/bindings/react`、`examples/paid-chat`，組合 subscription credits/paywall 與 token/media metered credits。customer BFF、交易內 ownership／replay 驗證、獨立本機沙盒 checkout、可信通知及精確額度顯示均可執行；實際使用 npm super-chat 0.5.0 Composer／Thread。驗證方式及已知限制見 [README](../examples/paid-chat/README.md)。one-time topup、trial、完整分頁 usage/history、portal、下一期換方案 UI、逐家 live 商戶與正式部署仍待後續階段，不以本機 sandbox 測試替代。
