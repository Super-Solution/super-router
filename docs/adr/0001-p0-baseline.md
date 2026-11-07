# ADR 0001：P0 開發基線與 P1 決策

日期：2026-11-04。狀態：採用於本地 P1 實作；不代表正式商業上線或外部模型驗收。

## 決策

| 主題 | 決定 | 後果 |
| --- | --- | --- |
| Runtime | Node.js 24.x；本次驗證 24.21.0 | 暫不支援其他 major、瀏覽器或 edge runtime |
| 語言／建置 | TypeScript 5.9.3、ES modules、npm workspaces、各包輸出 declarations | 根目錄 build 依序編譯六個套件，lockfile 固定依賴 |
| 套件命名／授權 | 本地 `@supersolutions/super-router-*`，private + UNLICENSED（2026-11-07 更新：改為 `@supersolutions` npm scope、Apache-2.0、可發佈，見 README） | 此列為 P0 當時決定；授權與 scope 已於 2026-11-07 由 SUPER SOLUTION LABS INC 決定 |
| 儲存 | PostgreSQL 17；本地容器 17.6 | 實際交易測試，不以記憶體資料庫代替；image digest 在驗證紀錄保留 |
| 交易鎖 | 每 workspace/environment 交易 advisory lock | P1 優先簡單正確；同租戶所有短帳務交易序列化，網路呼叫不持鎖。高吞吐 row-level lock 優化留待量測 |
| 帳本 | 單一 immutable transfer row 明示 debit、credit、同額 amount | 平衡由單列結構保證；不需先實作可任意新增不平衡 journal lines 的 API |
| Bucket | 到期優先；相同到期以 ID 排序 | P1 尚無訂閱／贈送類別優先，P2 引入時版本化排序 |
| 資產精度 | 固定 scale 9；atoms `NUMERIC(38,0)`；輸入最多 30 digits | 計算用 BigInt；API 金額用字串；無外匯轉換 |
| 價目 | input、cached、cache_write、output rates；相同整數分母 per | 整個 leg 加總後 ceil 一次；退款反轉原 atoms |
| 預留 | 每個 offering 的完整 input token 上界，加要求的 output 上限 | 不假裝通用 tokenizer 有精確估算；上界必須由部署者按實際模型 context ceiling 設定，不可用平均輸入量 |
| 模型與售價 | 不硬編碼商業模型價格；Service config 明確設定 | Mock 僅用虛構模型／價格；live smoke 要求提供確切 model ID |
| Provider | OpenAI Responses + Anthropic Messages，初期僅純文字 streaming | 外部 tools、多模態、reasoning 文字輸出與 provider-native extension 尚不支援 |
| Provider 重試 | 僅明確 HTTP 拒絕且允許重試（429）自動 fallback | 5xx、斷線、無 terminal usage 都視為 unknown；不保證上游 exactly-once |
| 保留 | 不存 prompt／completion；HMAC digest 比對輸入 | 同 key 重送查狀態，不重播內容；上游 `store:false` 不等於對第三方 retention 的保證 |
| 冪等期 | P1 本地不自動刪除 idempotency records | 比設計最短 24h 更保守；P2 加保留清理與終態 tombstone 策略 |
| 恢復 | DB 中已確認成功 usage 可重做結算；不明 dispatch 只進待對帳 | 不重發未知請求；outbox 至少一次 claim，generation 防止過期 ack |
| Secret | Provider constructor 注入伺服器端取 key 函式；HTTP key 僅雜湊比較 | 不把 Provider key 存 DB、不提供 Cloud BYOK 入口；正式 SecretStore／輪替屬 P2 |
| 付款責任 | P1 不接受真實儲值／訂閱；只有受權測試授額 | P2 才整合支付；平台售額由平台收款、開發者對自己客戶收費由其商戶負責，正式合約尚需上線前確認 |

## 三層帳務範例

下列均為測試價格，非任何 Provider 的公開價格：

- Provider：input=1、output=1 atom/token，10 input + 5 output → 15 USD atoms 成本證據。
- 平台銷售：若啟用 platform_price，獨立從開發者 platform account 扣費。
- Customer sale：input=2、output=3 credit atoms/token → 35 credit atoms；這不是把 15 USD atoms 換匯。
- input 上界 1024、output 上限 32 → customer 預留 2144；完成後扣 35，釋放 2109。
- 首次 429 明確拒絕、第二次成功：兩個 Attempts，失敗 cost=0、成功 cost=15，customer 只扣 35。
- 串流已輸出但 terminal usage 遺失：pending 並保留 hold，不猜成零成本。
- Refund 10 atoms 後同 key 重送只退一次；不能超過已扣且未退金額；原 bucket 已到期則退款立即再到期。

## P1 相對完整設計的收斂

Service、tier、policy、price 暫時放在同一 immutable Service version JSON，以 runtime schema 驗證，避免第一輪先建完整管理 CRUD。資料庫索引保存租戶、Service ID、version；每筆 Request 複製解析後版本與 route decision。P2 再拆獨立 catalog 管理資源，不更改既有 request snapshot。

管理 API 先提供單步 `POST /v1/admin/service-versions`；SDK／server 只保證 [generated OpenAPI](../generated/openapi.json) 的實作範圍。[API 設計](../API_SPEC.md) 仍是完整目標，不表示每個端點已存在。

目前 standalone main 是 localhost mock composition root；核心與兩個真實 adapter 可供宿主伺服器組裝。沒有實際付款、Cloud 註冊、完整 RBAC 管理、服務目錄分頁、全域限流、production deployment 或 UI。

## 安全與驗證決策

| 風險 | P1 控制／測試 owner | 上線前剩餘項 |
| --- | --- | --- |
| 跨租戶／customer 冒用 | Storage scope + FK、server principal binding；`tests/core.test.ts`、`tests/http.test.ts` | 正式身份與 key 管理、資料庫非 superuser runtime role |
| 併發／重送雙扣 | 原子 reservation、唯一 settlement、鎖與 immutable journal；core tests | 高負載測試、tenant 公平性與多節點觀測 |
| 上游執行不明 | ProviderError definitive 分類、保留 hold；provider/core tests | 真實上游 smoke、特定 Provider 的安全查詢能力 |
| Worker 重做 | request transaction lock、outbox lease generation；core tests | 外部投遞重試／死信與完整營運 UI |
| SSRF／secret 外洩 | Adapter 固定 HTTPS 官方 origin、redirect:error、錯誤不含原文 | Self-host private endpoint allowlist、production secret lifecycle |
| 偽造付款 | P1 無付款 endpoint；一般 invoke key 不可 grant | P2 payment signature／order dedupe tests |
| prompt 落盤 | requests 不包含 input/output、keyed digest | 可配置 retention／刪除及存取稽核 |

P0 的實作選型已定案；正式發佈授權、付款合約、production SLO 屬 P2 release gate，並非以假資料替代已完成的決策。
