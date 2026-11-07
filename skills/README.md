# SuperRouter Agent Skill 規劃

目前是交付規格，尚無可安裝的 `SKILL.md`。待公開 SDK／API 可驗證後再撰寫並測試實際 skill，避免提供不存在的命令。

預計路徑：`skills/superrouter/SKILL.md`，支援 coding agent 在開發者專案中：

1. 辨識 embedded、standalone、Cloud 整合方式與已安裝版本。
2. 使用公開 API 建立 Service、specific／pool／tier、routing 與價格。
3. 依 customer／payer 邊界加入 server-side 呼叫與 SSE。
4. 透過 request ID 診斷候選排除、Provider failure、pending 結算及額度問題。
5. 參照 examples 驗證，回報實際通過項目與未測能力。

Skill 不內嵌 credentials、不直接改帳本、不自動部署或發佈產品。需遵守使用者授權範圍，測試付費呼叫前說明實際模式與預算；對不存在的 API 不臆造 fallback 操作。驗收需在乾淨專案依正式公開介面完成一個端到端整合，並成功辨識至少一個 routing 與一個 billing 異常。
