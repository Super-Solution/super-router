# SuperRouter 分階段實作與驗證計畫

版本：0.2 · 2026-11-04 · 設計 backlog；實際進度見 [P1 驗證紀錄](P1_VALIDATION.md)

依據：[PRD](PRD.md)、[架構](ARCHITECTURE.md)、[資料模型](DATA_MODEL.md)、[API](API_SPEC.md)。先完成套件垂直切片，再擴展管理面與 Cloud 商業流程。階段不是日曆承諾，工期應於選型與 adapter spike 後估算。

## 1. 目前交付與剩餘設計

已撰寫產品與技術文件，並實作六個本地套件、PostgreSQL migrations、server／worker、SDK 與最小範例。P0 的開發選型及安全基線見 [ADR 0001](adr/0001-p0-baseline.md)。

P1 本地流程與 fixtures 已有自動測試；真實 Provider smoke 尚未執行，因此完整 P1 外部驗收仍未通過。正式授權、付款與 production SLO 是 P2 release gate。以下階段定義保留作驗收依據，不因部分完成而改寫退出標準。

## 2. P0：設計與契約

| 工作 | 交付 | 依賴／驗收 |
| --- | --- | --- |
| P0-01 選型與套件邊界 | ADR、runtime 相容矩陣、套件發佈範圍 | 驗證 embedded／server 使用同 core；FR-09 |
| P0-02 Provider spike | 兩家能力、usage、取消、重試、查詢結果的證據表 | 以官方文件及測試確認，標示不支援能力；FR-01 |
| P0-03 帳務演算 | 三層帳務範例、預留上界、rounding fixtures | 覆蓋 cache、fallback、partial、退款、到期；FR-05, FR-06 |
| P0-04 可執行契約 | OpenAPI／JSON schema、事件 schema、資料 migration 草案 | API 範例通過 schema；不可用 endpoint 標明階段 |
| P0-05 威脅與失敗模型 | 跨租戶、BYOK 邊界、SSRF、付款偽造、dispatch crash 測試清單 | 明確每個風險的控制與測試 owner |

退出：以上設計可支持第一筆端到端請求，未知的上游行為有明確 fail-closed 或 pending 策略。文件存在本身不等於 P0 完成。

## 3. P1：核心套件與垂直切片

| 工作 | 依賴 | 交付與驗收 |
| --- | --- | --- |
| P1-01 Monorepo／contracts | P0-01/04 | 型別、錯誤、schema、建置與套件 export；不依賴 UI |
| P1-02 Storage／tenant foundation | P1-01 | Migration、租戶外鍵、key scopes、測試 DB；跨租戶讀寫拒絕 |
| P1-03 Ledger／reservation | P1-02、P0-03 | Balanced journal、原子多 leg 預留／結算、bucket 到期、唯一 business keys；AC-05, AC-06 |
| P1-04 Catalog／Service versions | P1-02 | offering、group、policy、price、發布與版本固定；AC-04, AC-08 |
| P1-05 Routing | P1-04 | 硬限制、固定尺度評分、dry-run、總成本預算；AC-02 |
| P1-06 Provider adapters | P0-02、P1-01 | 至少兩家 adapter、mock fault harness、usage 正規化；不得以 mock 代替真實相容性結論 |
| P1-07 Request runner | P1-03/05/06 | dispatch、SSE、受限 fallback、cancel、unknown；AC-01, AC-03, AC-07 |
| P1-08 Recovery worker | P1-07 | outbox、leases／fencing、pending 對帳、重啟恢復；AC-06 |
| P1-09 Server／SDK／minimal example | P1-07/08 | 最小 serving 與狀態 API、嵌入及 HTTP 範例；AC-12 |

退出：兩個 Provider 可供同 Service 選路；一次請求能查到各 Attempts、供應成本與各 billing leg；併發、重送、串流中斷及 crash 測試通過。至少有記錄清楚的真實 Provider smoke run，若只有模擬測試則 P1 不能宣稱上游已驗證。

## 4. P2：可營運 MVP

2026-11-05 進度：P2-01／P2-02 的 DB 角色、key lifecycle、audit、訂閱／credits 與 sandbox 付款流程已實作；訂閱範例可執行。詳見 [P2 紀錄](P2_BILLING.md)。正式支付、UI、營運上線與其他 P2 工作尚未完成。

| 工作 | 交付與驗收 |
| --- | --- |
| P2-01 Tenant／RBAC 強化 | 管理角色、customer-bound keys、key 輪替、audit、匯出隔離；AC-10 |
| P2-02 Subscription／payment | 固定週期、付款 inbox、business 去重、授額、取消、退款；AC-09 |
| P2-03 Cloud supply policy | 平台 operator 與 tenant connections 分離，Cloud 不接受 BYOK；AC-11 |
| P2-04 完整 routing／Health | 觀測窗口、缺值、circuit breaker、恢復探測、品質評級來源 |
| P2-05 管理 App | Service、routing、價目、訂閱、客戶、Health、Logs、用量、成本、收入與 pending 操作 |
| P2-06 SDK／skill／examples | 完成三個範例及可安裝 skill；公開 API 串接、無內部 SQL 捷徑；AC-12 |
| P2-07 營運就緒 | Webhook dead-letter、備份還原、schema 升級／回滾、負載及故障演練 |
| P2-08 商業上線門檻 | 付款測試、正式價格、Provider 供應授權、退款政策、SLO／RPO／RTO 與資料保留規則定案 |

退出：Cloud Developer pays 與 self-host BYOK 都可從新環境重現；三層帳目可對帳。內部可用、測試環境通過與公開上線分開記錄，不將 UI 完成當成商業就緒。

## 5. P3：Tasks 與模態擴充

1. 定義 Task 的輸入、成功契約、可驗證結果、取消與部分結果政策。
2. 建立 task run／steps，沿用持久化 jobs、租約與 outbox；支援總任務預算。
3. 子請求記成本，不重複收 customer task fee；任務 success 才結算對應 leg。
4. 對外部工具副作用要求 operation id 與宿主冪等契約，無法保證時停在待確認而非自動重做。
5. 每新增一種模態，先定義 meters／上界／失敗收費，再加入 adapter 與範例。

退出：AC-13 通過；不得以一次 HTTP 200 取代任務成功判定，也不得把「模型生成成功」誤當外部業務動作完成。

## 6. P4：Connect with SuperRouter

1. 身份協定、App 註冊、consent、redirect 驗證及 token lifecycle 安全設計。
2. Grant scope、期限、授權預算與撤銷；在預留交易內驗證。
3. User pays 與 Developer pays 嚴格分流，餘額不足不自動換 payer。
4. 使用者可檢視與撤銷 App，查看授權內各項消費；處理正在執行的 hold。

退出：AC-14 通過，授權競爭與撤銷行為有實際測試。分潤、跨部署錢包互通與轉帳仍不在本階段默認範圍。

## 7. 驗證矩陣

| 測試層 | 必測案例 | 證據 |
| --- | --- | --- |
| 純規則 | filters、weights、missing health、price rounding、cache meters | 可重現 fixtures |
| DB 整合 | 多 leg 原子性、競爭扣款、相同 business key、bucket expiry、退款 | 真實 DB 交易測試，不用記憶體 mock 代替 |
| Provider 契約 | 能力拒絕、usage mapping、串流、timeout、cancel | adapter 測試加受控真實 smoke；成本記錄 |
| 故障注入 | transaction A/B/C 前後 crash、unknown dispatch、worker lease 失效 | request／journal／outbox 狀態與恢復結果 |
| HTTP／SDK | JSON schema、重送、SSE 斷線、303、錯誤碼 | 公開 API 端到端測試 |
| 租戶／權限 | ID 猜測、匯出、越權授額、Cloud BYOK、撤銷 key | 拒絕結果與無跨租戶副作用證據 |
| 付款 | 重複／亂序事件、不同 event 同 order、退款、續期 | sandbox 付款來源與本地帳目對帳 |
| 營運 | DB 中斷、備份還原、schema 升級、webhook 死信 | 操作紀錄、恢復時間與殘餘差異 |
| DX | clean install、三類 examples、skill 指令 | 可照做的執行紀錄及版本矩陣 |

## 8. PRD 可追溯性

| FR | 工作 | AC |
| --- | --- | --- |
| FR-01 | P1-06/07 | AC-01, AC-03, AC-07 |
| FR-02 | P1-04 | AC-08 |
| FR-03 | P1-04/05 | AC-04, AC-08 |
| FR-04 | P1-05、P2-04 | AC-02, AC-03 |
| FR-05 | P0-03、P1-03、P2-02、P3 | AC-05, AC-09, AC-13 |
| FR-06 | P1-03/07/08 | AC-05, AC-06, AC-07 |
| FR-07 | P2-02/08 | AC-09 |
| FR-08 | P2-01/05/07 | AC-01, AC-10 |
| FR-09 | P1-01/09、P2-03/06 | AC-11, AC-12 |
| FR-10 | P4 | AC-14 |

每階段完成報告必須列實際 commit／版本、通過的 checks、部署環境、尚未驗證項目與已知風險。沒有真實執行或上線證據時，不將設計、mock、背景工作或預覽視為完成。

## 9. 金流延伸實作紀錄（2026-11-05）

已新增 Stripe hosted Checkout、易支付 V1 MD5 相容協定、應援 Payment API adapters；支付既有固定期數 subscription。包含 merchant binding、先記錄再外呼的 checkout、驗簽與交易回查、原子去重發放、逾時對帳恢復、HTTP／SDK、OpenAPI、fixtures 與 PostgreSQL 測試。詳見 [PAYMENTS](PAYMENTS.md)。

此紀錄不表示原生 recurring、獨立 top-up 商品、現金退款、定時金流 reconciliation worker 或 production merchant 驗收已完成。下一步依逐家商戶測試與 [Sol 的 paid components/presets 規劃](PAID_COMPONENTS_PRESETS.md) 推進 customer BFF 與 UI 能力；不得把 billing admin key 下發前端。目前工作目錄沒有 Git metadata，未建立 commit／release。
