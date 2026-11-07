# Provider spike：契約、證據與限制

日期：2026-11-04。官方文件已查閱；fixture 測試已實作；**未呼叫真實上游**，目前環境未提供兩家的 API key。

## OpenAI

選定 Responses API，使用固定官方 origin、`stream:true`、`store:false`，以 text delta 提供輸出，完成事件取得 usage。將 cached／cache-write 自總 input 中分離；reasoning 不另加到已包含它的 output。缺少 terminal usage 視為未知，不當成成功零用量。

來源：[Responses streaming events](https://developers.openai.com/api/reference/resources/responses/streaming-events)、[Streaming responses](https://developers.openai.com/api/docs/guides/streaming-responses)、[Prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching)。模型與價格不在此文件硬編碼，需按實際選用模型設定。

## Anthropic

選定 Messages API，純文字 messages，system 轉為頂層欄位；message_start 讀初始用量，message_delta 的 output 是累計值，採覆寫而非累加；message_stop 才完成。Input、cache-read、cache-creation 分開記錄。尚未暴露 cache TTL 設定，部署者的 cache-write 費率必須涵蓋其實際設定。

來源：[Streaming messages](https://platform.claude.com/docs/en/build-with-claude/streaming)、[Prompt caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)。

## 支援與保守策略

| 能力 | P1 |
| --- | --- |
| 文字／串流 | 兩個 adapter 都有實作與 fragmented SSE fixtures |
| 非串流對外呼叫 | core 收集同一 streaming adapter 結果，完成後回 JSON |
| Tools／JSON schema／image/audio | runtime input schema 拒絕，不默默忽略 |
| AbortSignal | 向 fetch 傳遞取消；不聲稱能證明上游沒有計費 |
| HTTP 429 | 明確拒絕、可在預算內 fallback |
| 5xx／連線中斷／缺 terminal | unknown，不自動重新送出 |
| Provider idempotency／lookup | 不假設支援；P1 不自動 lookup 或重發 |
| Live 相容性 | 未驗證；fixture 通過不代表憑證、模型或帳戶可用 |

測試入口：`tests/providers.test.ts`。本地測試不需 API key、不產生模型費用。

## 真實 smoke

先在自己的 shell 以安全方式設定選用 Provider 的 key 與確切 model ID，不把 key 放進命令紀錄或 repo；然後執行：

```sh
SMOKE_PROVIDER=openai npm run smoke:live
# 或已設定 Anthropic 環境後：
SMOKE_PROVIDER=anthropic npm run smoke:live
```

指令上限 64 output tokens、30 秒，只輸出 model、文字字數與 usage，不印 key 或回答原文。此為付費上游測試，與 `npm test`／`npm run demo` 分開。它只驗證 adapter，正式 P1 外部驗收還需用真實模型價格與合理 input 上界，跑完整 router／帳務路徑並保存成本證據。
