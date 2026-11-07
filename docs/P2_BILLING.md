# P2 第一段：多租戶權限、訂閱與 credits

日期：2026-11-05。此文件描述本次已實作範圍；不是整個 P2 MVP 已完成的宣告。

## 已交付

- 持久化 tenant/environment membership、五種角色、customer-bound API keys、Service 範圍、到期、撤銷與輪替。
- 不可變方案版本、固定日數訂閱、付款訂單、已付款 entitlement、每期獨立 credits bucket。
- Sandbox 簽章付款 ingress：事件 ID 去重、訂單／週期去重、亂序處理、金額核對、原子授額及稽核。
- 到期不結轉、下期方案變更、期末取消、獨立購買／贈送 credits，以及保守預留後按實際 usage 結算。
- 控制面 HTTP endpoints、SDK helpers、OpenAPI，以及可執行的訂閱範例。

實作入口：[ControlPlane](../packages/adapters/storage-postgres/src/control.ts)、[權限查核](../packages/adapters/storage-postgres/src/access.ts)、[HTTP server](../backends/typescript/server/src/server.ts)、[範例](../examples/subscription-credits/README.md)。

## 1. 權限與信任邊界

| 角色 | 權限 |
| --- | --- |
| owner | 成員、keys、Service、routing、帳務與營運管理；不得移除最後一位 active owner |
| service_admin | 呼叫、Service／routing 設定、營運唯讀；不能調整 credits 或管理成員 |
| billing_admin | 帳務讀寫與營運唯讀；不能建立管理 key 或變更路由 |
| viewer | 帳務與營運唯讀 |
| app | 已綁 customer／Service 的呼叫與自己的 credits 查詢 |

Key 有固定 workspace、environment、member、customer、scopes 與可選 service_ids；app key 必須列 service_ids。有效權限是 key scopes 與「當前 member role」的交集。HTTP 每次重新從 DB 驗證；預留與 dispatch 前，在租戶交易鎖內再次查核，避免舊 principal 繞過降權或撤銷。

已進入上游執行的請求仍需完成記帳，不能因 key 撤銷就抹除成本。若撤銷發生在預留後、dispatch 前，禁止上游呼叫並釋放預留。Service 範圍用於 serving、Service 存取／發布與方案的 Service 驗證；租戶帳務／audit 的管理權則由明確 billing/operations scopes 決定。

Bootstrap 是宿主操作，不存在公開「指定任意 workspace 取得 owner」endpoint。資料庫已有會員時拒絕重新 bootstrap。`npm start` 現在使用 DB authorization；原本的 static-key server constructor 保留給 P1 測試／嵌入相容模式。

Key 只儲存 hash，建立／輪替時只回傳一次 token；同 idempotency key 重送只取得 metadata 與 `token:null`。不把 token 放 audit、command result、範例輸出或檔案。遺失回應時可用具權限的另一個 key 輪替，不提供明文讀回。

## 2. 訂閱與方案

`period_days` 是 1–366 的固定 UTC 日數；30 日不等於曆月。訂閱 anchor 在建立時固定，週期邊界為 `[anchor + n × period, anchor + (n+1) × period)`。

- 初始狀態 pending，未付款不授額。
- 每個 subscription/cycle 只有一筆 payment order；已付款週期不能再開一筆收款。
- 失敗付款不授額；同訂單的較新成功事件可完成授額。
- 新一期未付款，舊額度到期且不結轉；狀態為 past_due。獨立購買額度仍可用於不要求訂閱的 Service。
- 支援同週期長度的下一期方案變更；無即時比例分攤。某週期已有 order 後拒絕變更該 order 的方案。
- Cancel 保留已付款當期權益，於 current_period_end 生效；尚未付款的訂閱立即取消。
- 晚到的成功付款若所屬週期已過期、已取消或被較新週期取代，進入 `review_required`，不倒退狀態、不授額，也不假裝已做現金退款。
- 每個新週期由宿主建立 order、sandbox 來源確認付款；worker 更新到期／取消狀態，不自動發起真實扣款。

Service 可設定 `requires_subscription:true`。這時即使仍有購買 credits，也需具備涵蓋該 Service 的有效 paid entitlement；未設定則可直接使用可用 credits。

## 3. Credits 規則

訂閱每期產生獨立 bucket，帶有效期與可使用的 Service IDs。推論預留只選擇未到期且涵蓋該 Service 的 buckets；不能把另一個方案的額度用於其他 Service。

扣除順序：最早到期 → 同期限下 subscription／promotion／purchased → bucket ID；結算沿用相同順序。無到期時間排最後。購買或贈送授額只能由 billing 管理角色操作，須帶 reason，並有 command 去重和 audit。

沿用 P1 的 BigInt 計價、預留／釋放、退款上限及原 bucket 還原。Credits 是計量資產；payment order 的 `amount_minor` 是獨立的測試貨幣最小單位，兩者不混用。

可用餘額仍可能不足以滿足保守預留：範例每次需預留 2144 atoms，實際扣 35，剩餘 2109 時下一次仍會被拒絕。這不是餘額遺失；客戶上限保護以可支付的最大用量為準。

## 4. Sandbox 付款安全契約

目前只允許 host-configured、`mode=sandbox`、`environment=test` 的 integration；這不是 Stripe 等正式支付服務的替代品。一般 Bearer key 無法提交 `paid:true` 來授額。

Endpoint：`POST /v1/integrations/payments/{id}/events`。

- `X-Payment-Timestamp`：Unix seconds；接受前後 300 秒。
- `X-Payment-Signature`：`v1=` 加 `HMAC-SHA256(secret, timestamp + '.' + raw_body)` 的 hex。
- Body：event `id`、`order_id`、遞增 `revision`、`status`（succeeded/failed）、`amount_minor`、`currency`。
- Integration ID 在 DB 綁定 workspace/environment，禁止另一租戶重新使用同 ID。
- 額度來自伺服器保存的 plan/order snapshot，付款 payload 不能決定 credits 數量。
- 同 event ID、不同內容回 409；不同 event ID 指向已成功 order 不再授額；晚到失敗事件不覆寫成功。
- 付款事件、order 狀態、entitlement、bucket、journal 與 audit 在同一交易提交；只有 commit 後回 202。

此 slice 採同步、短交易 ingress。資料庫不可用時回失敗，來源以原 event ID 和新的 timestamp/signature 重送；尚未提供獨立持久化 inbox worker、外部 webhook delivery 或正式支付退款／拒付。

## 5. API 與資料變更

已實作契約以 [generated OpenAPI](generated/openapi.json) 為準。新增：

- `POST /v1/admin/members`、`customers`、`keys`、`keys/{id}/rotate|revoke`。
- `POST /v1/admin/plans`、`subscriptions`、`subscriptions/{id}/plan|cancel|payment-orders`。
- `GET /v1/admin/subscriptions/{id}`、`customers/{id}/balance`、`audit-events`。
- `POST /v1/admin/credits/grants`；`GET /v1/credits` 僅限已驗證 customer。

Migration 003：members、customers、keys、commands、audit、plans、subscriptions、orders、payment_events、entitlements；buckets 新增來源類型與 Service 範圍。Migration 004：integration 的租戶綁定。舊 P1 資料保留，不重置資料庫。

## 6. 驗證與後續邊界

驗證命令：

```sh
npm run typecheck
npm run contracts
npm test
npm run format:check
npm run demo
npm run demo:subscription
```

本次最終驗證：**40／40 測試通過**（包含 15 個新增 P2 測試），無 skipped；typecheck、format:check 及契約生成通過。P1 最小範例與 P2 訂閱範例皆已實際執行並通過 assertions。

使用真實 PostgreSQL 與 HTTP；模型 mock、付款 sandbox。週期切換測試使用注入 clock，檢查 30 日邊界，不需真的等待 30 天。Audit cursor 保留 PostgreSQL 微秒，避免分頁重複。

此範圍對應 P2-01 的 membership／key／audit 核心與 P2-02 的訂閱／credits／付款事件核心。尚不包含完整管理 UI、正式商戶支付、Cloud 註冊／供應、外部 webhook、production DB roles、SLO／備份演練或 P2 全部退出條件。P1 的真實 Provider smoke 仍待 credentials 與模型設定。

本次另修正舊回歸測試的兩項環境依賴：outbox 改以測試租戶領取事件，到期／恢復測試直接設定資料庫到期邊界，不再依賴毫秒級 sleep 或歷史資料量。
