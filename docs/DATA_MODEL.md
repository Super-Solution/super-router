# SuperRouter 資料模型與狀態機

> 實作進度：本文件保留完整目標設計；P1 採用的收斂決策見 [ADR 0001](adr/0001-p0-baseline.md)，已實作 API 見 [OpenAPI](generated/openapi.json)，驗證範圍見 [P1 紀錄](P1_VALIDATION.md)。

版本：0.1 · 2026-11-04 · 邏輯／關聯式 schema 設計，尚非 migration

依據：[PRD](PRD.md)、[架構](ARCHITECTURE.md)。以下名稱作為實作基線；不預設 ORM。

## 1. 共通規則

- 內部 ID 使用 UUID；API ID 為不透明字串。時間使用 UTC 與具時區 timestamp；期間採 `[start, end)`。
- 所有租戶資料帶 `workspace_id`、環境相關資料另帶 `environment_id`。子資料使用包含租戶維度的複合外鍵，禁止只用全域 ID 檢查關聯。
- `created_at` 必填；可變資料有 `updated_at` 與 optimistic concurrency `revision`。已發布版本及 posted journal 不允許原地修改。
- JSON 用於版本化政策、能力與原始證據；身份、狀態、外鍵、金額與唯一性不得只藏在 JSON。
- 所有索引以實際 query plan 驗證；以下列出正確性約束與必要查詢路徑，不宣稱已優化。

## 2. 身份與設定

表內省略共通 ID／timestamps；`FK` 表示需實體外鍵或對跨安全域資源的明確映射。

| Table | 關鍵欄位 | 約束／索引 |
| --- | --- | --- |
| workspaces | name, deployment_mode, status | mode 由部署設定控制，租戶不可自行改成 self-host |
| projects | workspace_id, slug | unique(workspace_id, slug) |
| environments | workspace_id, project_id, name | unique(project_id, name)，project 複合 FK |
| memberships | workspace_id, principal_id, role | unique(workspace_id, principal_id) |
| api_keys | workspace_id, environment_id, prefix, hash, scopes, expires_at, revoked_at | hash unique；不存明文 key |
| customers | workspace_id, environment_id, external_id, status | unique(workspace_id, environment_id, external_id) |
| key_customer_bindings | key_id, customer_id | 同租戶／環境 FK；未綁定一般 key 不可冒用別的 customer |
| models | canonical_name, revision_name, capability_schema | identity immutable；alias 與 model identity 分離 |
| provider_connections | owner_scope, owner_workspace_id, provider_kind, secret_ref, region, status | 平台與租戶 connection 的 owner XOR 約束 |
| offerings | connection_id, model_id, upstream_model_name, capabilities, status | unique(connection_id, upstream_model_name) |
| offering_grants | workspace_id, environment_id, offering_id | 明確授權租戶使用平台 offering；一般租戶不能查 secret_ref |
| compliance_assertions | offering_id, label, source, verification_state, expires_at | 到期／未知標籤不能滿足 required assertion |
| supply_price_versions | offering_id, currency, meters, effective_at, source | immutable；相同 offering 生效排序無歧義 |
| model_groups | workspace_id, environment_id, slug, kind | kind = pool／tier；slug 同環境唯一 |
| group_versions | group_id, version, capabilities, downgrade_policy | unique(group_id, version)；發布後 immutable |
| group_members | group_version_id, model_id, offering_filter | 不可將不符能力模型加入已發布集合 |
| routing_policy_versions | workspace_id, environment_id, policy_key, version, rules | 唯一 key/version；schema version 必填 |
| services | workspace_id, environment_id, slug, active_version_id, status | slug 同環境唯一；active version 必須屬於自己 |
| service_versions | service_id, version, selection_mode, model_id, group_version_id, policy_version_id, price_version_id, contract | specific 與 group 目標 XOR；所有 references 固定版本 |
| price_versions | workspace_id, environment_id, asset_id, meter_rates, rounding_policy, failure_policy | 發布後 immutable，明確定義 partial output 收費 |

模型、平台 offering 等共用 catalog 與 tenant 表不同安全域。租戶只讀已授權的公開 catalog projection；不能因為模型共用就直接 join 讀平台 connection secrets。

## 3. 請求與成本證據

| Table | 關鍵欄位 | 約束／索引 |
| --- | --- | --- |
| requests | workspace_id, environment_id, customer_id, service_version_id, payer_mode, execution_status, billing_status, config_snapshot_hash, deadline_at | index(tenant, created_at, id)、index(status, updated_at) |
| idempotency_records | tenant, environment_id, operation, key_digest, body_digest, request_id, expires_at | unique(tenant, environment, operation, key_digest) |
| route_decisions | request_id, sequence, policy_version_id, health_snapshot_at, candidates, selected_offering_id | unique(request_id, sequence)；敏感候選細節限管理員 |
| attempts | request_id, sequence, offering_id, supply_price_version_id, dispatch_token, upstream_request_id, status, lease_generation, output_started_at | unique(request_id, sequence)、dispatch_token unique |
| usage_records | attempt_id, revision, source, status, meters, evidence_ref, adapter_version | unique(attempt_id, revision)；追加 revision 不覆寫 |
| provider_cost_records | attempt_id, usage_revision, supply_price_version_id, asset_id, amount_atoms, status | 成本 revision 唯一；不假裝是付款交易 |
| health_samples | offering_id, region, sampled_at, outcome, latency, first_token_latency | index(offering_id, region, sampled_at) |

內容預設不持久化。Request 可保存 metadata 與 payload 的伺服器 keyed digest 做冪等比對；digest 不記原文且不能用一般未加密 hash 暴露短 prompt。執行完成後不保留回答的部署，API 只能重查狀態而不能重播內容。

Usage meters 必須互斥：例如 `input_uncached_tokens`、`input_cached_tokens`、`output_tokens`。如原始 input total 包含 cached，adapter 先減去 cached；對非可加總的 reasoning／cache write 指標另列，未定義計價映射時不能重複加總。保留 Provider 原始數據時須先剔除內容與敏感欄位。

## 4. 金額與帳本

### 4.1 精度與資產

建議基線：asset 固定 `scale = 9`，所有餘額／journal 使用 `NUMERIC(38,0)` 的 atoms；API 用十進位整數字串，禁止 JavaScript Number 承載。資產包含單一 currency 或 workspace-defined credits，asset 一旦使用不得改 scale。付款通道的最小支付單位由 adapter 另做精確轉換。

Meter rate 以整數 `rate_atoms` 加整數 `per_units` 表示。在單一 billing leg 中先精確加總各 meter 的有理數成本，再對總額向上取整一次；預留也向上取整。退款直接反轉原 atoms，不能重算當前價格。Provider 發票採其來源規則記錄，差異另記 revision，不強迫符合客戶售價捨入。

### 4.2 帳務表

| Table | 關鍵欄位 | 約束／索引 |
| --- | --- | --- |
| assets | owner_scope, code, kind, scale | 同 owner/code 唯一；currency 與 workspace credits 不混用 |
| ledgers | owner_scope, workspace_id, purpose | platform_sale／customer_sale 分開 |
| ledger_accounts | ledger_id, asset_id, owner_id, kind, posted_balance_atoms, held_atoms | balance 是可重建投影；所有帳戶及 counterpart 同 ledger/asset |
| credit_buckets | account_id, source_kind, source_id, expires_at, granted_atoms, remaining_atoms, held_atoms | remaining >= held >= 0；授額來源唯一 |
| billing_legs | request_id, kind, payer_account_id, price_version_id, status | unique(request_id, kind)；P3 task leg 另以 task_run owner XOR |
| reservations | billing_leg_id, status, reserved_atoms, deadline_at | 一個 active hold／leg；deadline 到期不能直接釋放 |
| reservation_allocations | reservation_id, bucket_id, held_atoms | 同 account/asset；sum allocations = reservation amount |
| budget_holds | request_id, budget_id, amount_atoms, status | 與扣款 reservation 分離；供應支出上界 |
| settlements | billing_leg_id, amount_atoms, usage_digest, status | unique(billing_leg_id)，最終結算只一次 |
| journal_transactions | ledger_id, asset_id, operation, business_key, correction_of, reason | unique(ledger_id, business_key)；posted immutable |
| journal_lines | transaction_id, account_id, signed_atoms, bucket_id | 同 ledger/asset；一筆 transaction 總和為 0 |
| reconciliation_cases | request_id, attempt_id, kind, status, next_check_at, evidence_ref, resolution_actor | index(status, next_check_at)；保留裁決理由 |

Ledger 採單一資產平衡記錄：授額由 issuer counterpart 移轉到客戶帳戶，扣款反向移轉至相應的 consumed counterpart。這是用量帳本，不代表完整法定會計總帳；貨幣收入與負債認列另由財務整合處理。Credit reservation 不改 posted balance，只增加 held projection；settlement 才寫 journal 並減少 hold。一般 customer spendable account 不得負值，issuer counterpart 可依帳戶類型允許負值。

每筆 journal transaction 的跨列平衡需在同交易驗證，透過受控寫入函式或 deferred constraint trigger 強制；不是單列 CHECK 就能保證。資料庫角色禁止繞過帳務入口直接改投影。

扣額預設排序：最早到期 bucket → 同期限下 subscription、promotion、purchased → bucket ID；無期限排最後。Bucket 在預留時有效即可供該請求最終結算；到期不搶走 active hold。到期後釋放的未用 hold 立即以 expiration journal 失效；退款回原 bucket，若已過期則保留退款與到期兩筆紀錄，不默默延長有效期。

### 4.3 預留與結算交易

按 `(ledger_id, account_id, bucket_id)` 穩定順序取鎖，所有 billing legs 同交易：驗證權限與 available → 分配 buckets → 插入 holds → 更新 held projection → commit。衝突時有限次重試整個交易，不重試外部 Provider 呼叫。

結算鎖定相同資源，先檢查既有 settlement，再驗證 `actual <= reserved`，寫入 balanced journal、bucket 扣除、釋放 hold、settlement、outbox 後提交。若實際超出預留，不強行使帳戶負值：標記異常，依既定 customer cap 吸收差額／人工裁決，並修正 adapter 上界；不可把錯誤估算當作追加扣款授權。

## 5. 訂閱、付款與非同步

| Table | 關鍵欄位 | 約束／索引 |
| --- | --- | --- |
| plan_versions | tenant, plan_key, version, price, period_rule, entitlement_template | immutable |
| subscriptions | customer_id, plan_version_id, status, current_period_start/end, cancel_at_period_end, external_ref | 外部 ref 在付款 integration 下唯一 |
| entitlement_grants | subscription_id, period_start/end, service_scope, quota, source_payment_id | unique(subscription_id, period_start, grant_kind) |
| payment_integrations | owner, processor_kind, secret_ref, merchant_ref | owner 與 ledger 對應，不共用商戶上下文 |
| payment_events | integration_id, external_event_id, payload_digest, processing_status | unique(integration_id, external_event_id) |
| payment_orders | integration_id, external_order_id, account_id, amount, asset, status | 事件與 business order 都去重，避免不同事件重複授額 |
| outbox_events | event_id, tenant, aggregate_id, aggregate_revision, event_type, payload, created_at | 與業務交易原子寫入 |
| webhook_deliveries | event_id, endpoint_id, attempt_no, state, next_attempt_at | unique(event_id, endpoint_id, attempt_no) |
| jobs | kind, business_key, state, lease_owner, lease_generation, lease_until | business key 去重，過期租約受 fencing 保護 |
| audit_events | tenant, actor, action, resource, redacted_diff, reason | 追加式；index(tenant, created_at) |

亂序付款事件不只依到達時間覆寫 subscription。以受驗證的訂單與對應 period 狀態機處理，必要時向付款來源查詢；續期 entitlement 必須綁定已確認付款與唯一週期。

## 6. 狀態機

| 聚合 | 狀態與合法方向 |
| --- | --- |
| Service version | draft → published → retired；published 不再修改，回滾是 active pointer 切到仍允許的舊版本 |
| Request execution | accepted → running → succeeded／partial／failed／cancelled；running → unknown → 已確認終態；accepted 可直接 failed／cancelled |
| Request billing | reserved → pending／settled／released；pending → settled／released；終態以 correction 處理退款，不退回 reserved |
| Attempt | prepared → dispatched → streaming → succeeded／partial／failed／cancelled；dispatched 也可直接 succeeded／failed；任何可能已發送但無證據狀態 → unknown；unknown 經對帳到終態 |
| Reservation | active → captured／released；unknown 情況保持 active 並連 reconciliation case |
| Subscription | pending → active；active → past_due／cancelled／expired；past_due → active（已確認新週期付款）／cancelled／expired |
| Payment order | pending → succeeded／failed；succeeded → partially_refunded／refunded／disputed；修正需來源證據 |
| Outbox delivery | pending → delivering → delivered；失敗 → retry_wait → delivering；超限 → dead_letter，可受控重送 |

所有狀態轉換帶 expected revision 或行鎖。Request 失敗與 provider cost pending 可以同時存在；輸出成功但帳務 pending 也合法。多個 leg 尚未全終態時 request billing 保持 pending，不能只看 customer_sale。

## 7. P3／P4 擴充預留

P3：`task_definitions`、`task_versions`、`task_runs`、`task_steps`。Task success contract 版本化；多個子 request 記錄供應成本但不再次扣相同 customer task fee。Task 有自己的 billing leg 與總成本 budget，子呼叫必須扣該 budget。

P4：`registered_apps`、`authorization_grants`、`grant_budget_holds`。Grant 綁 subject、App、Service scope、期限與付款帳戶；預留交易中鎖 grant budget 並驗證撤銷。沒有 grant 不可把 `payer_mode` 改成 user。詳細身份協定另立設計，不把這些表視為完整 OAuth 實作。
