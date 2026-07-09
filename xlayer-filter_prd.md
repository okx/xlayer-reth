> **Jira**: [XLOP-1142](https://okcoin.atlassian.net/browse/XLOP-1142)
> **Lark Doc**: https://okg-block.sg.larksuite.com/docx/HZE2djuLWo0Hi3xEJujlNr66gWg

**PRD：在 xlayer-reth 出块路径引入规则驱动的交易风控拦截组件（XLayer Filter）**

# Summary

- Goal：在 xlayer-reth 出块路径引入规则驱动的交易风控拦截组件（XLayer Filter），命中 deny 规则的交易在出块阶段被拦下、不进区块；命中 audit 规则的交易经外部风控服务（RCS）裁决通过后才允许打包。拦什么/审什么由 RCS 下发的规则决定，不在 Filter 代码内硬编码。
- Not doing：不实现 RCS（风控服务，含额度台账/Sweeper/裁决），Filter 仅作为 RCS REST 接口的 client；不做真实 RCS 的 testnet 端到端验证。
- Top risk：Filter 接在出块热路径上，拦截逻辑的正确性与性能直接影响出块结果；且 `total_retry_timeout_seconds` 必须大于 RCS 实际部署的主备切换宽限期，否则一次计划内切换会放大成大规模 fail-open/fail-close，此约束 Filter 运行时无法自校验。
- Blockers：None

# Background

xlayer-reth 出块模拟路径目前对交易没有基于规则的风控拦截：`crates/builder/src/flashblocks/best_txs.rs` 已有 `mark_invalid(sender, nonce)` 钩子，但没有任何黑名单/额度判断逻辑 [Repo]。

XLayer Filter 是一个通用的、规则驱动的交易拦截组件：它对出块中的交易做事件匹配、按 JSONLogic 规则求值、合并动作，并对需要裁决的交易向 RCS 提交/查询。风控策略（拦哪些地址、审哪些额度）完全由 RCS 通过 REST 下发的规则表达，Filter 代码不含任何具体业务语义。当前规则/黄金样例覆盖的业务场景是代币转账风控（黑名单 + 额度，见契约文档 §4 golden fixtures），但这属于规则内容，不是 Filter 的需求边界。

风控分两侧：RCS（Risk Control Service，独立团队/独立仓库，负责额度台账与裁决）与 XLayer Filter（本仓库节点内的拦截组件，作为 RCS 的 REST client）。两侧唯一权威接口契约见 `2026-07-09-rcs-filter-api-contract.md`（Binding），Filter 组件实现设计见 `2026-07-09-xlayer-filter-spec.md`（Draft）。本 PRD 只覆盖 xlayer-reth 侧的 Filter 组件，RCS 本次不实现。

当前仓库无对应拦截 crate、无任何 `permission-request`/`jsonlogic`/`quota_consistency` 相关代码，属从零构建 [Repo]。workspace 已依赖 `reqwest`（做 RCS client）与 `hyper`/`hyper-util`（可做进程内 Mock RCS）[Repo]。契约文档引用的主设计文档与 ADR-0001~0003 不在本仓库，Background 与需求基于上述两份 spec 与实际代码。

# Goals

- G-1：交易在出块阶段按 RCS 下发的规则被正确拦截——deny 交易不进区块，audit 交易仅在 RCS 裁决 approved 且打包前一致性校验通过后才进区块，未命中交易不受影响。
- G-2：Filter 与 RCS 通过契约文档定义的 REST 接口严格对齐，Filter 侧行为可用 Mock RCS 以契约黄金 fixture（场景 a/b/c/d）完整验证，无需真实 RCS。
- G-3：规则可版本化热更新；RCS 未部署阶段节点仍能通过风控总开关关闭 Filter 后正常出块。

# Scope

## In scope

- FR-1：出块路径拦截接入
- FR-2：规则启动阻塞加载与版本校验
- FR-3：规则运行期热更新
- FR-4：事件匹配与动作合并
- FR-5：audit 交易提交与裁决轮询（缓冲池状态机）
- FR-6：超时兜底与主备切换宽限期约束
- FR-7：quota_consistency_hash 打包前一致性校验
- FR-8：规则加载校验
- FR-9：风控总开关
- FR-10：Mock RCS 测试替身与黄金 fixture 覆盖（UT/IT）

## Out of scope

- RCS 内部实现（额度台账、Sweeper 调度、TZ/XLayer Sync、裁决数学）——他方仓库。
- 真实 RCS 的 testnet 端到端验证（依赖真实 RCS 部署与真实链环境）。
- `quota_consistency_hash` 的具体编码算法（字段字节表示等）——属 TD/实现细节，本 PRD 不固定。
- 告警接入监控/告警系统——本 PRD 仅要求本地结构化日志。
- `quota` 之外的新审计类型。
- 同 nonce 替换交易检测的裁决逻辑（归 RCS；Filter 仅忠实上送 `origin`/`nonce`，不在本地判定替换）。

# Functional Requirements

## FR-1：出块路径拦截接入 · implements G-1

出块模拟处理每笔交易时执行规则匹配（FR-4），按合并后的动作决定：deny → 调用 `mark_invalid` 排除出本次打包；audit → 暂不打包、进入待裁决流程（FR-5）；allow/未命中 → 按原有出块逻辑正常打包。约束：命中 audit 的交易在取得 approved 前，其状态变更不得提交进当前出块状态（不污染同区块内后续交易的执行基线）；交易实体始终留在 tx pool，其裁决与打包可跨越多个出块轮次由后续轮次重新拾取。

AC：
- Given 风控开关开启且已加载有效规则，When 出块模拟遍历到一笔命中 `action=deny` 规则的交易，Then 该交易不出现在产出区块中，且全程不产生任何对 RCS 的请求。
- Given 一笔交易命中 `action=audit` 规则，When 出块模拟处理该交易，Then 该交易本轮暂不进区块，进入待裁决流程，直到取得 approved 且打包前一致性校验通过才允许进区块。
- Given 一笔命中 audit 的交易在本轮出块被处理但尚未取得 approved，When 本轮继续处理同区块内后续交易，Then 该 audit 交易的状态变更不被提交进当前区块（后续交易的执行基线不受其影响），且交易实体保留在 tx pool 待后续轮次重新拾取。
- Given 一笔交易未命中任何规则，When 出块模拟处理该交易，Then 该交易按原有出块逻辑参与打包，行为与未引入 Filter 时一致。

## FR-2：规则启动阻塞加载与版本校验 · implements G-3

风控开关开启时，节点启动阶段同步阻塞全量拉取规则（GET /rules），拉到有效规则且 `protocol_version` 精确属于本地支持集合 `{1}` 前不参与出块；拉取失败或版本不支持均按"拉不到规则不启动"处理。风控开关（FR-9）是"是否进入本阻塞加载流程"的上游门控：开关开启即要求必须拿到有效规则才出块，开关不削弱本条 fail-safe 语义。

AC：
- Given 风控开关开启，When 节点启动，Then 节点在成功全量拉取规则且 `protocol_version ∈ {1}` 之前不参与出块。
- Given RCS 不可达或返回非 2xx，When 节点启动拉取规则，Then 节点持续指数退避重试（无次数上限）、不使用任何默认规则、不出块，直到拉到有效规则。
- Given 启动拉取到的 `protocol_version` 不在 `{1}` 内（如 2），When 校验版本，Then 拒绝该规则包并按"拉不到规则"同等处理（阻塞重试 + 告警），不按旧 schema 强行解析。

## FR-3：规则运行期热更新 · implements G-3

运行期后台轮询 GET /rules/version（默认间隔 2s，可配），仅当 `content_version` 变化才发起 GET /rules 全量拉取并原子替换规则与匹配索引；`protocol_version` 不支持时拒绝本次更新、沿用旧规则、告警，节点继续出块。

AC：
- Given 节点运行中已有生效规则，When 轮询发现 `content_version` 变化，Then 触发一次全量拉取并原子替换规则与索引，替换期间出块路径读到的规则/索引始终是同一版本（旧或新），无中间态。
- Given `content_version` 未变化，When 轮询，Then 不发起全量拉取。
- Given `content_version` 变化触发全量拉取、且拉回的 `protocol_version` 不在 `{1}` 内，When 校验，Then 拒绝本次更新、沿用旧规则、记录告警，节点继续正常出块（区别于启动阶段的阻塞行为）。

## FR-4：事件匹配与动作合并 · implements G-1

对每笔交易依次做：缓冲池去重短路 → 筛选跳过（`contract_address`/`origin` 快速排除，不解码日志）→ topic0 索引选出候选规则 → 具名事件解码后按 JSONLogic 求值 `condition` → 按 `deny > audit > allow` 合并动作（单日志内跨规则合并 + 跨日志整笔合并）。

AC：
- Given 某 `tx_hash` 已在缓冲池且为非终态，When 出块再次遍历到该交易，Then 直接复用已有状态、不重新解码/匹配日志、不产生重复 RCS 提交。
- Given 规则 `contract_address` 非空且 ≠ `tx.to`，When 筛选跳过，Then 该规则被快速排除，不对该交易日志做任何解码。
- Given 一笔交易的多条日志中任意一条命中 `deny`，When 动作合并，Then 整笔交易判 `deny` 并立即终止对剩余日志的扫描（不进缓冲池、不请求 RCS）。
- Given 同一条日志同时命中 `allow` 与 `audit` 规则，When 合并，Then 结果为 `audit`（`allow` 不能覆盖 `audit`/`deny`）；多条 `audit` 命中时 `audit_types` 取并集。
- Given 某具名事件在交易日志中找不到匹配日志，When 求值 `condition`，Then 该事件全部参数与 `<name>.address` 取 `null` 参与求值（视为变量取值为空，不是规则不命中）。

## FR-5：audit 交易提交与裁决轮询 · implements G-1, G-2

audit 交易进入 Filter 侧缓冲池状态机（见 State machine 节），在 `batch_window_ms`（200ms）窗口内批量 POST /permission-requests/submit，随后轮询 GET /permission-requests/query 按返回状态推进；提交请求/查询响应字段严格对齐契约 §2.4/§2.5，提交按 `tx_hash` 幂等。缓冲池状态在出块轮次间持久，audit 交易的裁决与打包跨越多个出块轮次；提交上送 `origin`/`nonce` 供 RCS 做同 nonce 替换检测，Filter 不在本地实现替换判定（见 Out of scope）。

AC：
- Given 一批 audit 交易在 200ms 窗口内累积，When 窗口触发批量提交，Then 提交请求体字段与契约 §2.4 一致，Filter 依 202 响应 `accepted` 列表将对应交易置为 Submitted。
- Given 提交后某 `tx_hash` 出现在响应 `rejected_malformed`（未在 `accepted`），When 处理提交响应，Then 该交易不置为 Submitted，回落 NotSubmitted 重试并记录告警。
- Given 已提交交易，When 轮询 query 返回 `approved`，Then 状态推进到 Approved（进入 FR-7 打包前一致性校验）。
- Given 已提交交易，When 轮询 query 返回 `denied`，Then 丢弃该交易且不再重试打包。
- Given 已提交交易，When 轮询 query 返回 `outdated`，Then 丢弃该交易且不再重试打包。
- Given 轮询 query 返回未识别的 `status` 值，When 处理响应，Then 该交易不被置为任何终态，按"缺席/无终态"走超时回落（FR-6），不乐观放行打包。
- Given 同一 `tx_hash` 因重试被重复提交且 RCS 已见过该 `tx_hash`，When 再次提交，Then 提交幂等——不重复登记/裁决，该 `tx_hash` 计入 `accepted`（依赖契约 §2.4 幂等保证）。
- Given 查询一个 RCS 从未见过或已清理的 `tx_hash`，When query 返回，Then 该 `tx_hash` 缺席于响应（不报错），Filter 将"缺席"与"长期无终态"同等对待。

## FR-6：超时兜底与主备切换宽限期约束 · implements G-2

各状态由本地超时参数驱动转移（Submitted 8s、Pending 20s 回落 NotSubmitted）；自首次进入 NotSubmitted 起累计滞留超过 `total_retry_timeout_seconds`（90s）仍无终态，按命中规则 `audit_timeout_action` 了结（`allow`=fail-open 放行，`deny`=fail-close 丢弃）。`total_retry_timeout_seconds` 必须大于 RCS 部署的主备切换宽限期。

AC：
- Given audit 交易自首次进入 NotSubmitted 起累计滞留超过 90s 仍无终态，When 超时判定，Then 按命中规则 `audit_timeout_action` 了结：`allow` → 放行打包，`deny` → 丢弃。
- Given `action=audit` 规则省略 `audit_timeout_action`，When 加载规则，Then 内存中补齐为 `allow`；同一交易多命中项 `audit_timeout_action` 不同时取更严格者（`deny` 优先）。
- Given RCS 主备切换宽限期内持续 50s 无响应后恢复（50s 为宽限期建议值下限代表点），When Filter 在此期间处理在途 audit 交易，Then Filter 经 Pending→NotSubmitted→Submitted 循环重试，且在累计 90s 阈值前不提前触发 `audit_timeout_action`（验证点为"累计 <90s 不早退"；逼近 90s 边界的临界用例在 IMPL 阶段补）。

## FR-7：quota_consistency_hash 打包前一致性校验 · implements G-1

audit 交易获 approved 后，打包前对其重新模拟执行、按同一匹配流程重算 `actions.quota` 的一致性哈希，与提交时存的哈希比对：一致则放行打包，不一致则丢弃且不主动通知 RCS（预占由 RCS 侧 outdated 两阶段自动回收）。哈希对 `actions.quota` 做规范化编码后计算，而非对 JSON 序列化字符串取哈希。

AC：
- Given 一笔 audit 交易已获 approved 且打包前重新模拟结果与提交时一致，When 计算并比对一致性哈希，Then 哈希一致 → 该交易放行进入正常打包。
- Given 打包前重新模拟得到的 `actions.quota` 与提交时不同，When 比对，Then 哈希不一致 → 丢弃该交易，且 Filter 不发起任何对 RCS 的撤销/取消调用。
- Given 两份 `actions.quota` 语义相同但 JSON 表层格式不同（key 顺序、空格），When 计算一致性哈希，Then 两次结果相同（验证规范化编码而非 JSON 字符串哈希）。

## FR-8：规则加载校验 · implements G-1

Filter 加载规则时实现与 RCS 同等强度的校验，校验失败直接拒绝整条规则（非忽略字段、非整包失败），被拒规则记录告警（含规则 id 与原因），其余规则正常生效。

AC：
- Given 某规则 `event_abis` 中任一事件的 `inputs[].name` 缺失或在该事件内重复，When 加载，Then 拒绝该条规则并告警，其余规则正常生效。
- Given `audit_types` 含 `quota` 但存在事件形状不满足 ERC20 `Transfer`（`from,to,value`）/ERC1155 `TransferSingle`（`operator,from,to,id,value`）两种固定形状之一，When 加载，Then 拒绝该条规则。
- Given 某规则 `event_abis` 为空，When 加载，Then 拒绝该条规则。
- Given 一条各字段均合法的 audit/deny 规则，When 加载，Then 该规则进入匹配索引并生效（可由后续命中验证）、无告警。

## FR-9：风控总开关 · implements G-3

新增风控总开关：关闭时 Filter 完全 bypass（不加载规则、不调用 RCS、所有交易按原有出块逻辑打包），供 RCS 未部署阶段节点正常运行；开启时按 FR-2 阻塞加载后出块。

AC：
- Given 风控开关关闭，When 节点出块，Then Filter 不加载规则、不产生任何对 RCS 的请求、所有交易按原有逻辑打包，节点正常出块。
- Given 风控开关配置为开启并（重）启动节点，When 节点启动，Then 进入 FR-2 阻塞加载流程（而非 bypass）；阻塞加载本身的正确性由 FR-2 验证。

## FR-10：Mock RCS 测试替身与黄金 fixture 覆盖 · implements G-2

提供 Mock RCS 测试替身（进程内，可注册提交/查询/规则响应脚本），交付覆盖契约 §4 黄金场景 a/b/c/d，以及宕机重试、启动不可达、主备切换宽限期不早退、热更新、`protocol_version` 不支持的 UT/IT。测试数据逐字复用契约 §4 固定常量与报文，不另起一套。

AC：
- Given Mock RCS 按契约 §4 场景 a 脚本应答，When 运行对应集成测试，Then Filter 提交报文与场景 a "Filter → RCS 提交请求"逐字段一致，且该交易最终被放行打包。
- Given 契约 §4 场景 b（`deny`）规则命中，When 运行对应集成测试，Then 该交易被丢弃、不进缓冲池，且对 Mock RCS 的提交/查询调用计数均为 0。
- Given 契约 §4 场景 d 时间线，When 依次驱动 Mock RCS 状态 `pending→approved→outdated`，Then Filter 在首次观测到 `outdated` 时丢弃该交易、此后不再重试打包，且不触发一致性校验。

# State machine

缓冲池条目 `bufferPool[tx_hash].status`（Filter 侧本地状态，区别于契约 §2.5 的 RCS 对外 `status`）：

- 状态：`NotSubmitted` | `Submitted` | `Pending` | `Approved` | `Outdated`，以及终态「放行打包」/「丢弃」（终态后条目从缓冲池移除）。
- 主转移：`NotSubmitted→Submitted`（批量提交收到 202）；`Submitted→Pending`（query 确认收录）；`Submitted→NotSubmitted`（8s 未确认）；`Pending→{Approved|Denied 丢弃|Outdated 丢弃}`（query 返回对应状态）；`Pending→NotSubmitted`（20s 无终态，含查询缺席）；`Approved→Outdated`（后续轮询发现转 outdated）；`Approved→放行/丢弃`（打包前一致性校验一致/不一致）。
- 兜底转移：任一状态自 `first_not_submitted_at` 起累计 > 90s（`total_retry_timeout_seconds`），按 `audit_timeout_action` 了结（FR-6）；此判定优先于常规转移。
- 非法/未知裁决状态：query 返回未识别的 `status` 视同"缺席/无终态"，走超时回落路径，不做乐观放行。

# Preconditions & Impact Surface

| # | Surface | Current fact / evidence | Expected change / constraint | Risk / impact | Blocking? |
|---|---|---|---|---|---|
| I-1 | 出块路径（builder 出块模拟） | `crates/builder/src/flashblocks/best_txs.rs:64` 已有 `mark_invalid` 钩子，无风控逻辑；出块循环单遍串行、模拟成功即 commit 状态 [Repo] | 在出块模拟对每笔交易接入 Filter，deny 交易调用 `mark_invalid`；audit 交易在取得 approved 前不提交状态变更、实体留 tx pool 跨轮次重新拾取（见 FR-1/FR-5） | 热路径，正确性/性能直接影响出块结果；待裁决交易若污染状态基线会影响同区块其它交易 | Yes |
| I-2 | RCS REST 契约（上下游） | 契约文档 §2 定义 4 个接口，状态为 Binding [User Input] | Filter 作为 client 实现，报文严格对齐契约，不重定义 schema | RCS 由他方实现，任一方字段漂移即破坏契约 | No |
| I-3 | 新增配置项（部署/ops） | 无相关配置 [Repo] | 新增：风控开关、RCS base URL、`batch_window_ms=200ms`、`submitted_confirmation_timeout_seconds=8`、`risk_module_unresponsive_timeout_seconds=20`、`total_retry_timeout_seconds=90`、规则版本轮询间隔=2s | 部署文档须显式标注 `total_retry_timeout_seconds` > RCS 主备切换宽限期（建议 ≥50s），Filter 无法运行时自校验 | Yes |
| I-4 | 新增拦截组件与依赖 | 无对应拦截 crate；`reqwest`/`hyper` 已在 workspace [Repo] | 新增独立拦截 crate 并接入 builder；复用 `reqwest` 做 client、`hyper` 做 Mock。crate 命名见 Open Items | 无数据迁移；crate 拆分方式属 TD | No |
| I-5 | RCS 未部署 | 生产环境本次无 RCS [User Input] | 靠风控开关关闭时 bypass 保证节点可运行；真实 RCS 的 testnet e2e 不在本 PRD | 存在真 RCS 端到端覆盖缺口，上线接真 RCS 前需补 e2e | No |

# Open Items

- Blocking：None
- Non-blocking：
  - 拦截 crate 命名：实现方案 spec §1 写作 `xlayer-bridge-intercept`，但组件本身与"bridge"无耦合（拦截逻辑通用、场景由规则决定）。建议改用中性名（如 `xlayer-filter`/`xlayer-tx-filter`），最终名由 TD 与 spec 作者对齐后确定。
  - `quota_consistency_hash` 具体编码算法（字段顺序、每类型字节表示）由 TD/实现确定并在代码中固定，需保证"提交时"与"打包前"两处调用同一套编码逻辑；本 PRD 不固定算法。
  - 告警接入监控/告警系统的渠道与格式待与运维对齐；本 PRD 仅要求本地结构化日志（含规则 id + 原因）。
  - GET /rules/version 轮询间隔默认取 2s，上线后可按规则变更频率与可接受生效延迟调整。
  - `contract_address` 误设检测：本 PRD 采用"仅告警、不自动拒绝"（业务语义判断，不可机械校验）；是否引入启发式黑名单待后续与规则运维流程 owner 讨论。
  - [待确认：风控总开关是否需要运行时动态开关，还是仅启动期配置读取 — impact：动态开关需额外的热切换安全处理（切换瞬间在途 audit 交易的归属）]。推荐：仅启动期配置，运行时不支持动态切换。
