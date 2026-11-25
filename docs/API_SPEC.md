# SuperRouter API 與事件規格

> 實作進度：本文件保留完整目標設計；P1 採用的收斂決策見 [ADR 0001](adr/0001-p0-baseline.md)，已實作 API 見 [OpenAPI](generated/openapi.json)，驗證範圍見 [P1 紀錄](P1_VALIDATION.md)。

版本：0.1 · 2026-11-04 · 提案，尚無可呼叫端點

依據：[PRD](PRD.md)、[架構](ARCHITECTURE.md)、[資料模型](DATA_MODEL.md)。本文件是人工可讀契約；P0 後續需建立可執行 schema／OpenAPI，不能把下列 JSON 當成已發佈 SDK。

## 1. 共通契約

- Base path：`/v1`；JSON 欄位採 snake_case，未知欄位預設拒絕。時間為 UTC RFC 3339，金額 atoms 為十進位整數字串。
- `Authorization: Bearer <key>`；key 決定 workspace、environment 與 scopes。一般 serving 請求不接受任意 workspace override。
- 角色：`invoke`、`service:write`、`routing:write`、`billing:read`、`billing:write`、`connection:write`、`operations:read`、`platform:operate`。Scope 之外仍檢查資源歸屬與 customer 綁定。
- 伺服器產生 `X-Request-Id`；呼叫端 trace ID 只能作關聯欄位，不能覆寫帳務主鍵。
- 列表接受 `limit`（預設 50，上限 100）、不透明 `cursor`；回傳 `data`、`next_cursor`。時間篩選為 `[from, to)`，排序以時間加 ID 穩定分頁。
- 可變管理資源使用 ETag／If-Match；revision 不符回 412。發布後版本不可 PATCH。
- 金額格式：`{"asset":"credit:workspace-demo","atoms":"1200000000","scale":9}`。asset ID 與 scale 必須符合伺服器登錄值，呼叫端不可自行改精度。
- P1/P2 僅 `payer_mode: developer`。Cloud 的 platform billing leg 由伺服器決定；`customer_id` 只指定已授權 customer_sale，不可選取其他開發者的平台帳戶。

## 2. 冪等與結果保留

所有產生扣款、授額、發布或取消的 POST 要求 `Idempotency-Key`。範圍為 workspace、environment、operation、key；伺服器對 canonical body 計算 keyed digest。

- 相同 key／body：返回既有資源與狀態，不發出第二次上游呼叫。
- 相同 key／不同 body：409 `idempotency_conflict`。
- 保存至少 24 小時；仍執行中或 pending 的 key 不得過期。付款／journal business keys 另有長期唯一性，不隨 24 小時窗口移除。
- 已完成非串流回答只有在明確啟用內容保留時才可重播。預設內容未保存時，重送回 303 與 `Location: /v1/requests/{id}`，查狀態但不重建回答。
- 已執行的串流重送也回 303；P1 不支援 SSE resume 或完整重播，忽略重送前已收到的內容由 App 自行處理。
- 24 小時後新 key／已過期 key 可能建立新請求，SDK 不可默默更換 key 重試不明結果。

## 3. Serving endpoints

| Method / path | Scope | 說明 |
| --- | --- | --- |
| `GET /v1/services` | invoke | 可呼叫的已發布 Services，不暴露管理設定與成本 |
| `GET /v1/services/{id}` | invoke | 能力、輸入限制、售價摘要、active version |
| `GET /v1/models` | invoke | 已授權的公開模型資訊 |
| `POST /v1/inferences` | invoke | 執行 Service，支援 JSON／SSE |
| `GET /v1/requests/{id}` | invoke 或 operations:read | 本 key 可見的請求狀態；管理員額外取得 trace |
| `POST /v1/requests/{id}/cancel` | invoke | 請求取消；202 表示已接受，非上游停止或退款完成 |

### 3.1 推論請求

```json
{
  "service": "svc_customer_support",
  "service_version": 1,
  "customer_id": "cus_demo",
  "payer_mode": "developer",
  "input": {
    "messages": [{"role": "user", "content": "請說明如何開始使用。"}]
  },
  "limits": {
    "max_output_tokens": 512,
    "timeout_ms": 30000,
    "max_charge": {"asset": "credit:workspace-demo", "atoms": "2000000000", "scale": 9}
  },
  "stream": false
}
```

`service` 必填 ID；`service_version` 省略時解析 active version 並固定。`customer_id` 在 Service 有 customer_sale 時必填，且只允許可信伺服器代理或綁定該 customer 的 key。`max_charge` 限制 customer_sale；不改寫平台價目、供應預算或 routing policy。App 可降低限制，不可提升 Service 上限。

`input.messages` 至少一則，允許 system／user／assistant 的文字；structured output 另由 schema capability 定義，不接受任意 Provider 原生參數；只有宣告支援 `structured_output` 的供應商轉接器會收到 `response_format`，其餘回 422 `unsupported_capability`。限制超出支援範圍回 422。

工具呼叫（TypeScript 參考實作、Go 與 Rust 資料平面）：`input.tools`（最多 64 個，`name`、`description`、`input_schema`）與 `input.tool_choice`（`auto`、`none`、`required` 或 `{ "name": ... }`）。assistant 訊息可帶 `tool_calls`（`id`、`name`、`arguments` 為 JSON 物件字串），結果以 `{ "role": "tool", "tool_call_id": ..., "content": ..., "is_error": ... }` 回傳；每個結果必須對應先前的呼叫，每個呼叫都必須有結果。user 訊息的 `content` 也可以是 `text` 與 `image`（`https:` 或 base64 的 png、jpeg、gif、webp）組成的陣列。音訊與檔案尚不支援。

串流事件新增 `output.tool_call.delta`（`index`、首個事件帶 `id` 與 `name`、`arguments_delta`）與 `output.tool_call.completed`（完整的呼叫，僅在模型正常結束時送出，被截斷的呼叫不會送出）。`request.completed`／`request.failed` 新增 `finish_reason`（`stop`、`length`、`tool_calls`，以及 Rust／Go 的 `content_filter`：供應商的內容政策中止了回答）與 `usage`。非串流回應的 `output.tool_calls` 與 `finish_reason` 語意相同。

沒有宣告 tools 的請求，其 canonical JSON 與冪等摘要完全不變。Provider 必須宣告 `capabilities.tools`／`capabilities.images`；沒有任何可用 offering 時回 422 `unsupported_capability`，且不會預留任何款項。`POST /v1/inferences` 的請求本文上限為 4 MB（驗證身分後才讀取），其他路由維持 256 KB。Go 與 Rust 資料平面以 `conformance/tools.v1.json` 對照參考實作（合約解析、canonical JSON、OpenAI／Anthropic 的請求本文與串流解析）；Python 資料平面尚未實作工具呼叫，收到這些欄位會以 `invalid_request` 拒絕。

取樣、推理與標籤（四種語言的資料平面皆解析，`conformance/contracts.v1.json`）：請求頂層可帶 `sampling`（`temperature` 0 到 2、`top_p` 大於 0 且至多 1、`stop` 最多 4 個各 1 到 64 字元的字串、`seed` 安全整數；至少一項）、`reasoning_effort`（`low`、`medium`、`high`）與 `tags`（1 到 16 組名稱／值，名稱 `[A-Za-z0-9_.:-]{1,64}`，值 1 到 256 字元且不含控制字元）。省略這些欄位時 canonical JSON 與冪等摘要完全不變。每個控制項都是 provider 的一項 `capabilities`（`temperature`、`top_p`、`stop`、`seed`、`reasoning_effort`）：沒有任何可用 offering 能承載時回 422 `unsupported_capability`，且不預留款項，不會默默忽略。目前 OpenAI Responses 承載 `temperature`、`top_p`、`reasoning_effort`（沒有 `stop` 與 `seed`）；Anthropic Messages 承載 `temperature`、`top_p`、`stop`；Gemini（Rust、Go）承載 `temperature`、`top_p`、`stop`；`seed` 與 Anthropic／Gemini 的 `reasoning_effort` 尚無 provider 承載。以上都只用固定的測試資料（fixtures）驗證，未對真實上游驗證。`tags` 目前只會被驗證並納入冪等摘要，尚未寫入請求紀錄或用於限額；它們是給 Observer（尚未提供）與日後的歸因查詢用的。

提示快取（`conformance/contracts.v1.json`、`conformance/tools.v1.json`）：請求可帶 `cache: {"mode": "auto" | "none"}`（不接受其他欄位，沒有 `ttl`）。這是提示，不是 capability：`auto` 只對有明確開關的 provider 生效，目前只有 Anthropic Messages，它會在上游請求頂層加 `cache_control: {"type": "ephemeral"}`（5 分鐘；依 Anthropic 官方文件，快取至最後一個可快取區塊，首次寫入為輸入價的 1.25 倍、讀取為 0.1 倍）；OpenAI 與 Gemini 本來就自動快取，不送任何東西。省略 `cache` 或 `none` 時完全不送，**預設為關閉**，因此沒有人會在不知情下付寫入費；這個預設值（開或關）是 owner 的決定，改它不需要改契約。計費沿用既有的 `cache_write` 與 `cached` meter（預留上限本來就取輸入、快取讀取、快取寫入三者的最高費率，所以預留夠用）。**不含** 1 小時快取（它需要第五個 meter，會動 `Meters`、`charge()` 與 `pricing.v1.json`，等 owner 決定）。自動快取在 Amazon Bedrock 上會被上游以 400 拒絕，所以只適用 Anthropic 自家 API。以上同樣只用固定的測試資料驗證，未對真實上游驗證。

輸出與逾時上限：`limits.max_output_tokens`、offering 與 Service 的 `max_output_tokens` 上限為 131072（原為 8192，無法上架 128,000 token 輸出的模型）；`limits.timeout_ms` 上限為 600000（原為 120000，超過兩分鐘的推理串流會被切斷）。實際生效的仍是 Service 與 offering 自己設定的值，這只是契約的天花板。

### 3.2 成功回應

```json
{
  "id": "req_demo",
  "execution_status": "succeeded",
  "billing_status": "settled",
  "service_version": 1,
  "model": "model_demo_v1",
  "tier": "chat/low",
  "output": {"role": "assistant", "content": "可以先建立一個 Service。"},
  "usage": {
    "status": "confirmed",
    "meters": {"input_uncached_tokens": 20, "input_cached_tokens": 0, "output_tokens": 18}
  },
  "billing": {
    "customer_charge": {"asset": "credit:workspace-demo", "atoms": "38000000", "scale": 9}
  }
}
```

上例價格為虛構範例，不代表正式售價。Usage 僅描述交付的 billable result；管理 trace 可看到所有失敗 Attempt 的 usage 與供應成本。沒有 customer_sale 時 `customer_charge` 為 null；待對帳時也為 null 並標示 pending，不能用零代表未知。平台費及供應成本僅對具權限的開發者／operator 顯示。

### 3.3 SSE

在驗證與預留成功後才送出 HTTP 200 `text/event-stream`。每個 event 有遞增 sequence 及 request ID；順序為：

```text
event: request.started
data: {"request_id":"req_demo","sequence":1,"service_version":1}

event: output.delta
data: {"request_id":"req_demo","sequence":2,"text":"你好"}

event: request.completed
data: {"request_id":"req_demo","sequence":3,"execution_status":"succeeded","billing_status":"pending"}
```

Terminal event 為 `request.completed` 或 `request.failed`，最多一個；連線遺失可能完全收不到 terminal，客戶端必須 GET 狀態。HTTP headers 送出後錯誤使用 SSE event，不假裝能改 HTTP status。首個 output delta 後禁止透明 fallback。連線斷開預設觸發取消意圖；上游取消不確定時進 unknown／pending。SSE completed 可以 billing pending，最終帳務由 GET 或 webhook 得知。

## 4. 管理 API

下表集合路徑均在 `/v1/admin` 下；GET detail 採 `/{id}`、列表按共通分頁。POST create 回 201，明確 action 回 200 或處理中 202。

| 資源／動作 | Methods | Scope / 階段 |
| --- | --- | --- |
| `/services`、`/services/{id}/versions` | GET、POST | service:write；P1 |
| `/service-versions/{id}` | GET、PATCH draft | service:write；P1 |
| `/service-versions/{id}/publish` | POST，固定關聯版本並切換 active pointer | service:write；P1 |
| `/services/{id}/disable` | POST | service:write；P1 |
| `/model-groups`、`/model-groups/{id}/versions` | GET、POST | routing:write；P1 |
| `/routing-policies`、`/routing-policies/{id}/versions` | GET、POST | routing:write；P1 |
| `/routing/dry-run` | POST，不呼叫上游 | routing:write；P1 |
| `/connections` | GET、POST | connection:write；self-host only |
| `/connections/{id}/rotate`、`/connections/{id}/disable` | POST | connection:write；self-host only |
| `/prices`、`/prices/{id}/versions` | GET、POST | billing:write；P1 |
| `/customers` | GET、POST | billing:write；P1 |
| `/accounts/{id}/balance`、`/accounts/{id}/entries` | GET | billing:read；P1 |
| `/accounts/{id}/adjustments` | POST，reason 必填 | billing:write；P1 |
| `/settlements/{id}/refunds` | POST，金額不得超過尚未退款額 | billing:write；P2 |
| `/plans`、`/plans/{id}/versions` | GET、POST | billing:write；P2 |
| `/subscriptions`、`/subscriptions/{id}/cancel` | POST | billing:write；P2 |
| `/subscriptions/{id}` | GET | billing:read；P2 |
| `/payment-orders` | POST | billing:write；P2 |
| `/requests/{id}/trace`、`/health`、`/audit-events` | GET | operations:read；P1 基礎、P2 完整 |
| `/reports/usage`、`/reports/costs`、`/reconciliation-cases` | GET | 對應 operations／billing read；P2 |
| `/reconciliation-cases/{id}/resolve` | POST，證據、處理方式與 reason | billing:write；P2 |
| `/webhook-endpoints` | GET、POST | workspace admin；P2 |

平台 connection 管理由隔離的 `/v1/platform/connections` 與 `platform:operate` 控制；不能讓 Cloud workspace manager 使用 self-host 路徑建立供應來源。

### 4.1 發布 Service version 範例

先建立 Service 與 draft version，再呼叫 publish；publish body 只帶 expected revision。以下為 draft 內容：

```json
{
  "selection": {"mode": "tier", "group_version_id": "gv_chat_low_1"},
  "routing_policy_version_id": "rp_balanced_1",
  "price_version_id": "price_chat_1",
  "contract": {
    "input_kind": "chat_text",
    "output_kind": "chat_text",
    "max_output_tokens": 2048,
    "partial_output_billing": "confirmed_usage",
    "unknown_usage_policy": "reconcile_then_resolve"
  }
}
```

Publish 需驗證同租戶 references、能力、價格 asset、有限預算與規則完整性；不符回 422，不能發布後等第一次呼叫才發現沒有合法設定。

### 4.2 Routing dry-run

輸入為 Service/version、需求能力、估計 input/output tokens；回傳 `snapshot_at`、`selected_offering`、`eligible`、`excluded`、每個候選的原因碼、正規化分數與估計成本。僅供管理員；不預留、不保證後續可用、不洩漏其他租戶的 offering。

### 4.3 調整與退款

調整請求包含 account、asset、signed amount、reason 及 external reference；權限限定帳戶 owner。退款引用原 settlement 與部分／全部 atoms，按原 bucket allocations 還原。支付渠道的現金退款另有 payment order 流程；回補 credits 不等於信用卡退款成功。

## 5. 錯誤契約

```json
{
  "error": {
    "code": "no_eligible_offering",
    "message": "沒有符合此服務限制的可用模型供應。",
    "retryable": false,
    "request_id": "req_demo",
    "details": {"reason_codes": ["region_mismatch"]}
  }
}
```

| HTTP | Code 範例 | 行為 |
| --- | --- | --- |
| 400 | invalid_request、missing_idempotency_key | 修正請求 |
| 401 | invalid_key、expired_key | 不重試原憑證 |
| 403 | scope_denied、byok_not_allowed | 不以換 payer 繞過 |
| 404 | resource_not_found | 跨租戶物件亦回 404，避免枚舉 |
| 402 | insufficient_balance、budget_exceeded | 不發送上游；不自動換帳戶 |
| 409 | idempotency_conflict、state_conflict | 查詢既有資源 |
| 412 | revision_mismatch | 重新讀取後決定修改 |
| 422 | unsupported_capability、no_eligible_offering | 修改能力／政策，不盲目重試 |
| 429 | rate_limited | Retry-After；相同邏輯請求保持 key |
| 502 | upstream_failed | 是否可重試由確定執行狀態決定 |
| 503 | billing_unavailable、supply_unavailable | 尚未執行才能安全重送；保持 key |
| 504 | execution_timeout | 必須先查 request 狀態，可能仍 pending |

固定政策無候選回 422；暫時全部供應不可用回 503。Unknown 結果須附已有 request ID，不能因 HTTP 5xx 就認定未執行。

## 6. 付款 ingress 與事件

付款入口：`POST /v1/integrations/payments/{integration_id}/events`。依 adapter 驗證原始 body 簽章、時效與商戶上下文；不可把任意 caller 提交的 `paid:true` 當付款證據。原子寫 payment inbox 後回 202；處理失敗由 worker 重試。外部 event ID 與 order/period business key 分別去重。

對外事件至少一次投遞、不保證跨 aggregate 順序；接收端依 event ID 去重，依 aggregate revision 判斷順序，需要時 GET 最新狀態。

```json
{
  "id": "evt_demo",
  "type": "billing.settled",
  "schema_version": 1,
  "occurred_at": "2026-11-04T15:00:00Z",
  "workspace_id": "ws_demo",
  "environment_id": "env_test",
  "aggregate": {"type": "request", "id": "req_demo", "revision": 4},
  "data": {"request_id": "req_demo", "billing_status": "settled"}
}
```

事件種類：`request.completed`、`request.failed`、`billing.settled`、`billing.released`、`billing.corrected`、`subscription.updated`、`entitlement.granted`、`reconciliation.required`。不在事件放 prompt、completion 或 secrets。

Webhook 簽章提案：HMAC-SHA256(secret, timestamp + `.` + raw_body)，header 傳 timestamp、key ID、signature；接收端驗證 5 分鐘窗口並保留 event 去重。每次重送更新簽章 timestamp，event ID 不變。輪替期間接受明確列出的新舊 key IDs。

投遞收到 2xx 即確認；網路錯誤／非 2xx 以指數退避加 jitter 重送，最多 24 小時後 dead-letter；禁止跟隨到未驗證 URL 的 redirect，且設定 egress 限制。24 小時是提案預設，可版本化調整；管理員可重送同 event，不新建業務扣款。

## 7. 後續 API

P3：Task 定義／版本、建立 run、查詢、取消及 task events；需要先固定成功契約，不能只為現有推論換名稱。

P4：App 註冊、使用者授權／撤銷、grant 限額與 User pays。協定、token 類型與 consent UX 另立身份安全設計；P1/P2 對這些能力回 unsupported，不提供假授權入口。
