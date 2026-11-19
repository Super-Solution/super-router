# SuperRouter

[English](../../README.md) · [繁體中文](README.zh-TW.md) · **简体中文** · [日本語](README.ja.md)

以软件包形式集成的 AI routing、计价与 serving 基础设施。你定义 **Service**（要销售的 AI 能力）、选择背后的模型、设置售价、订阅与额度，由 SuperRouter 负责选路、计量与结算。

> **状态：开发版（0.3.0）。** 本地核心可完整运行：Service 版本、模型选路、PostgreSQL 预留／结算、HTTP + SSE、SDK、恢复 worker、多租户角色与 keys、订阅与 scoped credits。OpenAI 与 Anthropic adapters 已用官方格式 fixtures 测试，尚未完成真实上游验收；三家支付 adapters 也尚未用真实商户账号跑过。目前没有托管 Cloud，也没有 production readiness 声明。

## 功能

- **以 Service 为中心的选路。** Service 可绑定指定模型、明确的模型池或任务级别 tier。先应用硬性限制（地区、标签、成本上限），再做加权评分；只有在上游明确拒绝时才有限度 fallback。
- **精确计价。** 以整数（BigInt）计算 input、cached、cache-write、output 费率。调用前预留、调用后结算；重试不会重复向客户收费，上游结果不明时保留预留并进入人工对账，不会悄悄退款。
- **幂等的 serving。** HTTP/SSE、idempotency key、重发状态、取消意图与绑定客户的 principal。不存储 prompt 与 completion。
- **订阅与 credits。** 套餐、固定天数订阅、credit buckets、授额，以及额度不足时的 paywall 处理。
- **托管支付。** Stripe Checkout、易支付 V1、应援（OEN）adapters，含签名验证与回查。
- **恢复。** Worker 释放过期预留、重做已确认 usage 的结算，并把不明 dispatch 留待对账。

## 软件包

| 路径 | 功能 |
| --- | --- |
| `packages/contracts` | Runtime schema、类型、错误、SSE parser |
| `packages/core` | Routing、BigInt 计价、预留／执行／结算流程、mock adapter |
| `packages/adapters/storage-postgres` | Migrations、租户 scope、accounts/buckets/journal、requests、outbox 与恢复 |
| `packages/adapters/provider-openai` | OpenAI Responses 纯文本流式 adapter |
| `packages/adapters/provider-anthropic` | Anthropic Messages 纯文本流式 adapter |
| `packages/adapters/payments` | Stripe、易支付 V1、应援 hosted checkout adapters |
| `packages/sdk` | HTTP client、SSE、重发状态与取消 |
| `backends/typescript/billing-presets` | 仅限服务端：preset 验证、幂等发布、客户 BFF |
| `packages/bindings/react` | React 组件：价格表、结账状态、余额、paywall、用量摘要 |
| `backends/typescript/server` | HTTP + SSE server：`createRouterServer`、支付集成接线 |
| `backends/typescript/worker` | 恢复 worker 库与 `super-router-worker` 命令 |
| `apps/server`、`apps/worker` | 本地 demo 入口（mock 模型、demo workspace） |

软件包发布在 `@supersolutions` scope 下，例如 `@supersolutions/super-router-core`、`@supersolutions/super-router-sdk`。

## 快速运行

需要 Node.js 24、npm、Docker。请在仓库根目录执行。

```sh
npm ci
npm run build
npm run db:up
npm run demo
npm run demo:subscription
npm run demo:payments
```

Demo 使用 mock 模型，不花模型费用；数据库、HTTP、流式与账务是真实运行。预期一次模型调用、扣 35 个测试 atoms，以相同 key 重发不再扣款。详见 [minimal-chat](../../examples/minimal-chat/README.md)。

```sh
npm run typecheck
npm test
npm run contracts
```

测试需要开发数据库。设置 `DATABASE_URL` 可覆盖默认的 `localhost:25432`。Tests 使用随机 workspace，不删除既有数据。`npm run db:down` 停止专用容器并保留 volume。服务端配置请参考 `.env.example`。

## 文档

设计文档目前以繁体中文撰写。

- [PRD](../../docs/PRD.md)、[技术架构](../../docs/ARCHITECTURE.md)、[数据模型](../../docs/DATA_MODEL.md)、[完整目标 API](../../docs/API_SPEC.md)、[当前实现的 OpenAPI](../../docs/generated/openapi.json)
- [支付集成](../../docs/PAYMENTS.md)、[P2 多租户／订阅／credits](../../docs/P2_BILLING.md)、[付费 components／presets](../../docs/PAID_COMPONENTS_PRESETS.md)
- [P0 决策](../../docs/adr/0001-p0-baseline.md)、[Provider spike](../../docs/PROVIDER_SPIKE.md)、[验证记录与限制](../../docs/P1_VALIDATION.md)
- [示例应用](../../examples/README.md)、[Agent skill 规划](../../skills/README.md)

## 已知限制

没有个人钱包授权、完整管理 UI 或公开 SaaS；Cloud 禁止上游 BYOK 的规则目前只有发布策略测试。付款为固定天数的人工续费，不是自动周期扣款。上游结果不明时保留预留并进入人工对账，绝不免费重试。

## 许可证

Apache License 2.0。见 [LICENSE](../../LICENSE) 与 [NOTICE](../../NOTICE)。

Copyright 2026 SUPER SOLUTION LABS INC.

联系：contact@supersolution.io
