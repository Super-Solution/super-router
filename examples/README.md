# 範例 App 規劃

minimal-chat 與 subscription-credits 已可執行，使用 mock 模型、sandbox 付款與真實 PostgreSQL／HTTP；self-host-byok 仍為後續規格。範例採公開 SDK 與 core 管理介面，不直接修改帳本。

| 範例 | 階段 | 學習目標 |
| --- | --- | --- |
| [minimal-chat](minimal-chat/README.md) | P1 | 最小 Service、串流、狀態、用量與結算 |
| [subscription-credits](subscription-credits/README.md) | P2 | 週期訂閱、credits、付款去重與額度不足 |
| [payments](payments/README.md) | P2 金流延伸 | Stripe／易支付 V1／應援 checkout 與回查協定 fixtures |
| [self-host-byok](self-host-byok/README.md) | P2（P1 先驗證核心） | 自有 Provider keys、政策、fallback 與成本 |

每個可執行範例須提供版本要求、安裝／啟動命令、`.env.example`、測試資料、mock 與真實付費模式的區分、預期輸出與清理方式。Secrets 僅放伺服器；測試帳務不得寫入正式環境。
