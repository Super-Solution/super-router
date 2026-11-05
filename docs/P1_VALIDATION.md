# P1 本地交付與驗證紀錄

日期：2026-11-04。狀態：P0 開發決策已採用，P1 核心本地流程可執行；**完整 P1 外部驗收仍缺真實 Provider smoke**。

## 實際交付

- 六個 TypeScript workspace packages：contracts、core、storage-postgres、provider-openai、provider-anthropic、sdk。
- PostgreSQL 兩個版本化 migrations，immutable journal／settlement、租戶 scope、credit buckets、原子多 leg 預留、退款、outbox 與待對帳紀錄。
- Specific／pool／tier 選路、地區與標籤限制、權重評分、版本固定、成本上界、受限 fallback。
- Server、SSE、JSON 回應、冪等重送 303、cancel intent、customer-bound principal。
- Worker 處理過期預留：尚未 dispatch 釋放、已確認成功 usage 重做結算、未知 dispatch 保留 hold，不重發上游。
- 最小可執行範例與可生成 OpenAPI／事件 schema。

## 環境與命令

本機 Node.js 24.21.0，npm 11.19.0；套件版本記錄於 package-lock.json。資料庫獨立於機器上既有服務，使用 `superrouter-postgres-1`，僅綁 localhost:25432。

測試 image：`postgres:17.6-alpine`，此次下載的 RepoDigest 為 `postgres@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94`。

```sh
npm run typecheck
npm test
npm run format:check
npm run contracts
npm run demo
```

本次最終結果：`typecheck` 通過；`npm test` **25／25 通過**、無 skipped；`format:check` 通過；contracts 生成成功；demo 已核對 settled、held=0、provider calls=1。測試使用真實 PostgreSQL，不使用記憶體 storage；Provider 部分使用注入 HTTP fixtures／mock，不接外部付費服務。

## 驗證覆蓋

- 精確整數計價、rational rounding、cache 不重複計量。
- 併發只允許可支付的預留；相同 key 併發只發送一次。
- 多 leg 任一不足即全數回滾；不在 customer 免費後放行平台消耗。
- 價格變更後同 key 仍查原 request；Service／journal SQL 修改受到 immutable trigger 拒絕。
- 兩種真實 adapter 實作在同 Service 上使用 HTTP fixtures fallback，記錄所有 Attempts。
- 失敗重試不重複向 customer 收費，unknown 不被當作零成本。
- 已確認部分輸出結算；無最終 usage 中斷保留預留且不 fallback。
- DB pool 重開後狀態仍在；dispatch crash 不重發；成功 usage 落盤後 crash 可結算。
- Outbox generation 拒絕 stale ack；對外 subscriber 尚未配置，不假稱事件已投遞。
- 退款去重／上限／原 bucket 已到期行為，原始 grant intent 去重。
- HTTP／SDK SSE、重送、無效 key、customer 冒用、未知參數與 cancel intent。
- Generated schema 與 runtime schema 一致；P1 不接受 User pays 或權重總和不合法。

## 最小範例觀察

`npm run demo` 建立隨機隔離 workspace，開始 localhost HTTP server，再以 SDK 串流呼叫。已觀察到：

```text
request.started -> output.delta -> request.completed
execution_status: succeeded
billing_status: settled
balance: 99965 atoms
held: 0
provider calls: 1
replay: request.replayed
provider cost: 15 USD atoms
customer charge: 35 credit atoms
```

價格是刻意簡化的測試 fixture，不能當公開模型價格。Demo 結束會關閉 server／pool；DB 容器與測試資料保留，方便繼續開發。

## 限制與 P1 外部驗收

1. 未提供 OpenAI／Anthropic key，因此未執行付費呼叫；不能據 fixtures 宣稱模型或帳戶相容。見 [Provider spike](PROVIDER_SPIKE.md) 的 live smoke 指令。正式驗收還需真實模型 context 上界、價格與完整 routing／帳務 smoke。
2. Serving 只支援純文字；其他 Provider 能力不會被默默傳遞。OpenAI terminal incomplete 有 usage 時可按部分結果結算。
3. 部署者必須提供可信的 offering input 上界／費率。若已回報用量超出預留上界，保留 pending，不追加未授權客戶扣款；平台仍可能已發生成本，需要調整配置與對帳。
4. P1 啟動入口是 localhost mock app；沒有正式身份伺服器、production key lifecycle、通用限流、註冊、支付、訂閱或管理 UI。
5. 核心管理操作可透過可信宿主 API 呼叫；大部分完整目標管理 endpoints 尚屬 P2。以 generated OpenAPI 為已實作端點清單。
6. 未知成本需人工核對。Customer waiver 不宣稱 Provider 成本已解決；成本待對帳 case 保持開啟。自動上游 outcome lookup 與完整修正流程尚未驗證。
7. 本地使用開發 DB owner。正式上線必須配置獨立 migration／runtime roles、secret 管理、備份還原與 SLO，不能直接暴露 demo server。
8. request recovery 以短交易鎖序列化，沒有重新執行長期任務；outbox 有租約與 fencing。高吞吐、多節點壓測及外部 webhook dead-letter 屬後續階段。

## 計畫對應

| 工作 | 現況 |
| --- | --- |
| P0-01 | ADR 採用 runtime、private packages、storage 與部署邊界 |
| P0-02 | 官方資料與 fixtures 完成；live 資格待提供 credentials／model |
| P0-03 | 計價、預留、refund、expiry 演算與真實 DB 測試 |
| P0-04 | Runtime schema、generated OpenAPI／event schema、migrations |
| P0-05 | ADR 威脅／失敗控制與測試對應 |
| P1-01–05 | 本地套件、storage、帳務、Service 與 routing 已實作 |
| P1-06 | 兩個 adapter 實作／fixtures；外部 live 未驗證 |
| P1-07–09 | Request runner、恢復 worker、HTTP／SDK／最小範例已執行 |

不把本紀錄當成 Cloud 部署、真實付款、外部模型驗收或 production readiness 證據。
