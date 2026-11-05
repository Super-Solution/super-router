# Self-host BYOK — 規格

狀態：待核心完成後實作，尚無部署配置或啟動命令。

流程：啟動 DB、server、worker → 用 secret reference 建立兩個 connections → 設定 pool／tier、成本與地區限制 → 呼叫 → 故障注入 → 查看所有 Attempts 與成本。

需驗證不依賴 Cloud 帳號即可使用核心；secret 不回傳原文；fallback 不越過硬限制；未知結果不盲目重送。附備份還原與最小清理步驟，禁止把真實 key 放進範例。驗收對應 AC-02、AC-03、AC-11、AC-12。
