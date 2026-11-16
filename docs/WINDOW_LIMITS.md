# 訂閱的 session ＋ weekly 用量視窗

日期：2026-11-16。範圍：訂閱方案可選的「短視窗（X 小時）＋ 週視窗」雙限制，疊加在既有的 credit 模式上。預設關閉，沒開的方案行為與之前完全相同。

## 它做什麼

訂閱每期授予一包 credits（[P2_BILLING](P2_BILLING.md)）。視窗限制不增加 credits，只限制這包 credits **花得多快**：

| 視窗 | 長度 | 說明 |
| --- | --- | --- |
| session | 1–72 小時，方案自訂（常見為 5） | 短期爆量上限 |
| weekly | 固定 168 小時 | 長期節奏上限 |

兩個視窗同時生效，任一個滿了就拒絕。視窗以 credits 計量（與方案的 `asset` 相同單位），所以仍是 credit 制，沒有另一套請求次數計價。

## 視窗怎麼計時

視窗在「第一次從訂閱 bucket 預留」時開啟，持續其長度後關閉；關閉後的下一次預留再開新視窗（與 Claude／Codex 類產品的 session 與 weekly 行為一致）。不是固定在日曆整點，也不是滾動視窗。

- 預留時先佔用（held），結算時換成實際扣款（spent），釋放或失敗則歸還。限制檢查用 `spent + held + 本次預留 ≤ limit`。
- 預留是保守上界（例：範例服務每次預留 2144、實際扣 35）。所以視窗在「還剩不到一次預留」時就會拒絕，而不是等到用盡。
- 沒有任何實際扣款的視窗（例如請求全被釋放）會被刪除，失敗的請求不會讓 session 開始計時。
- 退款會把額度還給原本計入的視窗（即使視窗已經關閉也只是記帳更新）。

## 開關與方案

在方案版本（`PlanInput`）上加 `window_limits`：

```json
{
  "id": "pro", "version": 2, "period_days": 30,
  "asset": "credit:demo", "credits": "500000000", "service_ids": ["chat"],
  "amount_minor": "2000", "currency": "USD",
  "window_limits": {
    "enabled": true,
    "session": { "hours": 5, "credits": "40000000" },
    "weekly":  { "credits": "200000000" }
  }
}
```

- `enabled:false` 或省略：不限制，也不寫任何視窗列。
- `enabled:true` 至少要有 `session` 或 `weekly`；兩者都有時 session 不可大於 weekly。
- 方案版本發布後不可變。要開關或調整，發布新版本；新版本在**下一期**（或升級）生效。
- 付款授額時，限制連同該期 bucket 一起快照在 `sr_entitlements.window_limits`，所以已付款的當期不受之後的方案編輯影響。

## 與其他 credits 的關係

視窗只限制訂閱 bucket。視窗滿了之後，預留會落到下一個符合條件的 bucket（購買的 credits、贈送額度），行為和 bucket 用完相同。沒有其他 bucket 可用時才回錯誤：

- 視窗造成不足：`429 window_limit_reached`。
- 本來就沒有足夠餘額：`402 insufficient_balance`。

## 錯誤與查詢

```json
{ "error": { "code": "window_limit_reached", "retryable": true, "request_id": "…",
  "details": { "window": "weekly", "limit": "200000000", "used": "199990000", "resets_at": "2026-11-23T08:00:00.000Z" } } }
```

- 兩個視窗都滿時，回報較晚重置的那個。
- `retryable:false` 且 `resets_at:null`：單次預留就超過視窗上限（上限設得比一次呼叫的預留上界還小），等待也不會通過；請調高上限或調低 `max_output_tokens`／輸入上界。
- 被拒絕的請求不預留、不呼叫上游、不留下請求紀錄。

查詢：

- `GET /v1/windows`：目前 customer 自己的視窗用量（`credits:read`）。
- `GET /v1/admin/customers/{id}/windows`：指定 customer（`billing:read`）。
- `customerBilling` 回傳多一個 `windows`。Go 與 Python 伺服器也提供 `GET /v1/windows`。
- SDK：`client.windows()`。

每筆包含 `window`、`hours`、`limit`、`spent`、`held`、`remaining`、`window_start`、`resets_at`。尚未開啟的視窗 `spent` 為 0、`window_start` 與 `resets_at` 為 `null`，下一次請求才開始計時。

## 實作位置

- 契約：`packages/contracts/src/control.ts`（`WindowLimits`、`PlanInput.window_limits`）。
- 儲存：`packages/adapters/storage-postgres/src/windows.ts`；`reserve`、`finishTx`、`refund` 在同一個租戶交易內維護視窗；migration `006_window_limits.sql`（`sr_usage_windows`、`sr_allocations.windows`、`sr_entitlements.window_limits`）。
- Go：`backends/go/store/postgres/windows.go`（`Reserve`、`finishTx`、`Windows`、`EntitleWith`），測試 `windows_test.go`。
- Python：`backends/python/src/super_router/stores/windows.py`（`PostgresStore.reserve`、`windows`、`entitle`），測試 `tests/test_windows.py`。三個後端共用同一份 migration 與同樣的視窗語意；Go 與 Python 只有資料面，entitlement 與方案仍由 TypeScript 控制面寫入。
- 測試：`tests/window-limits.test.ts`（真實 PostgreSQL：額度計算、重開、週視窗獨立、兩窗皆滿、單次超過上限、落到購買 credits、退款回補、失敗不計時、併發不超額、HTTP）。

## 限制

- 三個後端的記憶體 store 沒有「bucket 屬於哪個訂閱」的模型，所以**不執行**視窗限制；只有 PostgreSQL store 會。
- 視窗用資料庫時鐘（`now()`），多節點不需時鐘同步，但測試要用 SQL 把視窗往前移來模擬時間流逝。
- 目前只有兩個固定視窗種類；weekly 固定 168 小時、不對齊訂閱起算日。
- 沒有即時開關單一訂閱的 API；調整限制需發布新方案版本。
- 視窗只計訂閱 bucket 的消耗，不計購買或贈送 credits 的消耗。
