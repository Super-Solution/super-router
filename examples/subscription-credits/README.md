# Subscription + Credits

可執行範例：**真實 PostgreSQL、持久化多租戶權限、HTTP／SDK、訂閱帳務；模型為 mock，付款為 sandbox**。不需要模型 API keys 或信用卡，不會產生模型或金流費用。

從 repository 根目錄執行：

```sh
npm ci
npm run build
npm run db:up
npm run demo:subscription
```

需要 Node.js 24 與 Docker。可用 `DATABASE_URL` 指向專屬開發 DB；不要對正式資料庫執行範例。

## 可觀察流程

1. 建立獨立 workspace 與 owner；owner key 和簽章 secret 只留在記憶體。
2. 透過 HTTP／SDK 建立 customer、App member 及只能使用 `chat` 的 customer-bound key。
3. 發布 30 日方案，包含 2144 個測試 credit atoms，建立訂閱與 sandbox order。
4. 簽章付款事件送兩次，實際只授額一次。
5. 呼叫模型成功，扣 35，剩餘 2109；不足以再次預留 2144 時，第二次呼叫在 dispatch 前被拒絕。
6. App 自行授額被拒絕；owner 加入 1000 個獨立購買額度後，可再次呼叫。
7. 取消下期續訂，當期 paid entitlement 保留。
8. 撤銷 App key，後續請求回 401；最終模型呼叫次數為 2。

範例內有 assertions，行為不符會以非零狀態結束。所有價格均為測試資料；`amount_minor=999` 並未真的收取 USD。輸出不包含 API keys、簽章 secret 或 Bearer headers。

每次使用新的 workspace、隨機 localhost port；程式結束關閉 server/pool，資料保留在開發 DB 供查驗。`npm run db:down` 停止資料庫且保留 volume。

## 進一步驗證

`npm test` 包含 key 輪替／撤銷、降權競爭、多租戶隔離、付款簽章、事件重送、訂單去重、續期失敗、方案變更、取消、過期、不結轉，以及 scoped credits 與扣款順序測試。

詳見 [P2 規格與限制](../../docs/P2_BILLING.md)。正式金流、管理 UI 與自動週期扣卡尚未提供。
