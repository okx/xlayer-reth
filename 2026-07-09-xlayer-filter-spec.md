# XLayer Filter 实现方案（Implementation Spec）

- 日期：2026-07-09
- 状态：Draft，待实现
- 组件：XLayer Filter（xlayer-reth 节点内拦截组件，扩展现有 `xlayer-bridge-intercept` crate）
- 角色：本组件是 RCS（Risk Control Service）REST 接口的 **client**；RCS 由独立团队在独立仓库实现，双方的唯一权威接口契约见 `docs/superpowers/specs/2026-07-09-rcs-filter-api-contract.md`（下称"契约文档"）
- 背景/领域上下文：`docs/superpowers/specs/2026-07-06-bridge-withdrawal-risk-control-review.md`（下称"主设计文档"）、`docs/superpowers/CONTEXT.md`、`docs/superpowers/adr/0001~0003`

> 本文档只覆盖 XLayer Filter 组件自身的实现设计。凡涉及 RCS↔Filter 报文格式（字段名、JSON schema、状态枚举）之处，只引用契约文档的章节号并点出字段名，不重新定义或复制 schema。如本文档与契约文档或 ADR 存在冲突，以契约文档与 ADR 为准。

## 1. 范围

### 1.1 覆盖内容

- Filter 组件内部实现：出块模拟路径的拦截函数（同步、零网络 IO）+ 后台异步 worker（tokio task，负责所有网络 IO）。
- Filter 侧缓冲池状态机（第 2 节）。
- 筛选跳过 + 事件匹配 + 动作合并算法（第 3 节）。
- `quota_consistency_hash` 的具体计算与比对时机（第 4 节，Filter 本地机制，ADR-0003）。
- 规则加载与热更新（第 5 节）。
- Mock RCS 测试基础设施与测试计划（第 6、7 节）。

### 1.2 不覆盖内容

- RCS 内部实现（额度台账、Sweeper、TZ/XLayer Sync）——见主设计文档 §4.2.2~§4.2.7，不在本仓库范围。
- RCS↔Filter 的 REST 报文字段定义、JSON schema、黄金测试样例的裁决语义——一律引用契约文档，不复述。本文档下方出现的 JSON 仅为契约文档中已有内容的**指针性摘录**（标注来源章节），不是本文档定义的新 schema。
- 规则 JSON 结构本身的字段校验规则——见契约文档 §3，Filter 与 RCS 必须实现同等强度的校验（契约文档 §3.1 顶部原文），本文档第 5 节只描述 Filter 侧如何加载/热更新，不重复字段表。

### 1.3 与契约文档的引用约定

- 提交请求体结构、字段语义（`tx_hash`/`origin`/`contract_address`/`nonce`/`actions.quota[].{name,address,params}`）→ 契约文档 §2.4。
- 查询响应结构、`status` 枚举（`pending`/`approved`/`denied`/`outdated`，`?tx_hashes=` 模式下含 `completed`）、`decided_at`/`reason` 语义 → 契约文档 §2.5。
- 规则对象字段（`id`/`contract_address`/`origin`/`event_abis`/`audit_types`/`condition`/`action`/`audit_timeout_action`）→ 契约文档 §3。
- `protocol_version`/`content_version` 语义 → 契约文档 §2.1。
- 黄金测试样例（场景 a/b/c/d）→ 契约文档 §4，本文档第 6、7 节按场景编号引用，不复制完整 JSON（除第 6 节第 6.4 小节的一处 worked example，为可读性完整引用场景 a 的报文，与契约文档保持逐字一致）。

## 2. 缓冲池状态机

### 2.1 状态与结构

Filter 内存中的旁路状态表 `bufferPool[tx_hash]`（主设计文档 §4.5.5），字段：

| 字段 | 说明 |
|---|---|
| `status` | `NotSubmitted` \| `Submitted` \| `Pending` \| `Approved` \| `Outdated` |
| `origin` / `nonce` | 提交时快照，供同 nonce 场景观测（替换判定本身在 RCS 侧，见主设计文档 §4.3.3 场景图 2） |
| `actions.quota`（提交内容快照） | 提交时打包的 `actions.quota` 内容（结构见契约文档 §2.4），供打包前重新模拟后计算 `quota_consistency_hash` 时比对基准 |
| `quota_consistency_hash` | 提交时对 `actions.quota` 规范化编码取的哈希（第 4 节） |
| 各阶段截止时间戳 | 依状态而定：`Submitted` 状态下为 `submitted_confirmation_timeout_seconds` 截止时间；`Pending` 状态下为 `risk_module_unresponsive_timeout_seconds` 截止时间 |
| `first_not_submitted_at` | 首次进入 `NotSubmitted` 的时间戳，用于计算跨越多轮重试的累计 `total_retry_timeout_seconds` |

交易实体本身始终留在 reth tx pool；缓冲池只是旁路状态表，不持有交易内容本身（主设计文档 §4.1"交易实体留在 tx pool"）。

### 2.2 状态机图与转移条件

（原始状态机图见主设计文档 §4.3.3；本节固定各转移的具体触发条件与判定顺序，供实现对齐。）

```
NotSubmitted -> Submitted           : 批量提交 (batch_window_ms 触发), 收到 202 (accepted 列表含本 tx_hash)
Submitted    -> Pending             : GET query 确认 RCS 已收录 (status 为 pending/approved/denied/outdated 任一, 即不再"缺席")
Submitted    -> NotSubmitted        : 超过 submitted_confirmation_timeout_seconds (8s) 未确认收录
Pending      -> NotSubmitted        : 超过 risk_module_unresponsive_timeout_seconds (20s) 无终态
                                       (含"查询缺席"情形, 契约文档 §2.5 消歧: 缺席与长期无终态同等对待)
Pending      -> Approved            : GET query 返回 status=approved
Pending      -> Denied              : GET query 返回 status=denied
Pending      -> Outdated            : GET query 返回 status=outdated
Approved     -> Outdated            : 后续轮询发现已转 outdated (多轮未打包成功)
Approved     -> [终态: 放行打包]     : 打包前重新模拟, quota_consistency_hash 一致
Approved     -> [终态: 丢弃]         : 打包前重新模拟, quota_consistency_hash 不一致
Denied       -> [终态: 丢弃]
Outdated     -> [终态: 丢弃, 不再重试打包]
NotSubmitted -> [终态: 按 audit_timeout_action fail-open/fail-close]
                                       : 自首次进入 NotSubmitted 起累计超过 total_retry_timeout_seconds (90s)
                                         仍未拿到终态 (即多轮 NotSubmitted<->Submitted<->Pending 循环失败)
```

判定顺序（每次异步 worker 处理一个 `bufferPool` 条目时）：

1. 先检查 `first_not_submitted_at` 是否已超过 `total_retry_timeout_seconds`（外层阈值，跨状态循环累计计时，不因中途进入 `Submitted`/`Pending` 而重置）——超过则立即按命中规则的 `audit_timeout_action` 了结，不再进入下面的常规状态转移判断。
2. 否则按当前 `status` 走上表对应的常规转移。

`audit_timeout_action` 缺省值处理：Filter 加载规则时，若某条 `action=audit` 规则省略 `audit_timeout_action`，按契约文档 §3.6 消歧内容，加载期即在内存中补齐为 `"allow"`，不在状态机判定时再做缺省判断（避免加载路径与运行路径出现不一致解释）。若一笔交易的多个命中项来自不同规则、各自 `audit_timeout_action` 不同，取更严格者（`deny` 优先于 `allow`）——主文档 §4.3.3 已明确这条规则，与第 3.4 节"多命中项按最严格优先合并"的原则一致。

### 2.3 时间参数

| 参数 | 值 | 驱动的转移 |
|---|---|---|
| `submitted_confirmation_timeout_seconds` | 8s | `Submitted -> NotSubmitted` |
| `risk_module_unresponsive_timeout_seconds` | 20s | `Pending -> NotSubmitted` |
| `total_retry_timeout_seconds` | 90s | `NotSubmitted -> [终态 fail-open/fail-close]`（跨状态循环累计） |
| `batch_window_ms` | 200ms | 触发 `NotSubmitted -> Submitted` 批量提交的时间窗口 |

这四个参数是 Filter 进程的本地配置（不通过 `GET /rules` 分发，主设计文档 §4.4.1、契约文档 §4 前言），与 RCS 侧的 `approval_ttl_seconds`/`outdated_release_delay_seconds`/`reservation_sweep_interval_seconds` 是两组独立配置，仅在下面 2.4 节的不等式约束上产生耦合。

### 2.4 为何 total_retry_timeout_seconds 必须大于 RCS 主备切换宽限期

主设计文档 §4.1 建议 RCS 主备切换/启动宽限期时长 ≥ `approval_ttl_seconds + outdated_release_delay_seconds`；按契约文档 §4 固定的黄金样例默认值（`approval_ttl_seconds=30s`、`outdated_release_delay_seconds=20s`）计算，宽限期建议值为 **50s**。

`total_retry_timeout_seconds=90s` 明显大于该宽限期（主设计文档 §4.3.3 原文："90 秒明显大于 4.1 主备切换宽限期建议值...留出安全余量"）。原因：

- RCS 切换期间，新活跃实例在宽限期内不接 Filter 请求（主设计文档 §4.1"切换/启动宽限期"）。这期间 Filter 侧的 `Submitted`/`Pending` 交易会因收不到响应而超时转回 `NotSubmitted`，并在 `NotSubmitted` 状态下持续重试提交——这是**预期的、可恢复的**正常路径（契约文档 §2.4 幂等性保证重复提交安全），不应被误判为需要 fail-open/fail-close 了结的异常。
- 如果 `total_retry_timeout_seconds` 小于或接近 RCS 切换宽限期，一次正常的**计划内**切换就会让所有在途 `audit` 交易在 RCS 还没来得及重新处理请求之前就被 Filter 提前判定为"长期无响应"，触发 `audit_timeout_action` 了结——这会把本该只是"延迟一次裁决"的正常运维操作，放大成大规模的 fail-open（可能导致漏审计）或 fail-close（可能导致大量正常提现被误拒）。
- 因此实现时必须保证：`total_retry_timeout_seconds` > RCS 侧实际部署的切换宽限期时长。以本组件角度看，这是一条**跨组件的部署时约束**（Filter 无法在运行时校验 RCS 的宽限期配置），需要在部署文档/上线检查表中显式标注，并在集成测试里用 mock RCS 模拟"宽限期内持续无响应 50s 后恢复"的场景验证 Filter 不会提前 fail-open/fail-close（见第 7 节测试计划）。

## 3. 筛选跳过与事件匹配算法

匹配流程总览见主设计文档 §4.6"匹配流程"流程图；本节固定实现细节。

### 3.1 阶段零：tx_hash 去重（缓冲池命中检查）

出块模拟每次拿到一笔交易的执行结果（N 条日志 + `origin`/`nonce`/`value` 等交易级字段）时，先查 `bufferPool[tx_hash]`：

- 已有**非终态**记录（`NotSubmitted`/`Submitted`/`Pending`/`Approved`/`Outdated` 均属非终态，因为这些状态仍会被 Filter 继续跟踪处理；已放行打包或已丢弃的条目从缓冲池移除，不留痕）→ 直接复用已有状态，跳过后续所有匹配逻辑，不重新解码日志。
- 无记录 → 进入 3.2。

这一步是最低成本的短路径，避免对已经在裁决流程中的交易反复重新匹配规则（主设计文档 §3.6 提到的 O(N×M) 复杂度问题，第一层优化）。

### 3.2 阶段一：筛选跳过（快速排除，不解码日志）

对规则索引中的每条规则，先用交易级字段做零解码开销的快速排除：

- 规则 `contract_address` 非空且 ≠ `tx.to` → 排除该规则。
- 规则 `origin` 非空且 ≠ `tx.origin` → 排除该规则。

**关于 `contract_address` 为何对某些规则不安全**（契约文档 §3.1、主设计文档 §4.6 字段说明的核心论证，Filter 实现必须在代码注释/规则加载校验层面体现这条约束，即使协议层不强制拒绝）：

- `contract_address` 字段语义是 `tx.to`，即**这笔交易直接调用的合约地址**。
- 一个事件（如 ERC20 标准 `Transfer`）可能由 `tx.to` 指向的合约**通过任意深度的内部调用**间接触发——例如本方案的 claim/relayer 合约（契约文档黄金样例中的 `CLAIM_CONTRACT`）内部调用 bridge 合约，bridge 合约再调用 token 合约触发 `Transfer`。此时该笔交易的日志里确实包含目标 `Transfer` 事件，但 `tx.to` 是 `CLAIM_CONTRACT`，不是 token 合约地址，也不是 bridge 合约地址。
- 如果给这类规则设置了 `contract_address`（例如错误地设为 token 合约地址或 bridge 合约地址），筛选跳过阶段会因为 `tx.to`（`CLAIM_CONTRACT`）不匹配而**直接排除掉这条规则**，导致本该命中的 `Transfer` 事件永远不会进入事件匹配阶段——即"漏判"。这正是黄金样例场景 a/c/d 里 `contract_address` 必须留空、只能靠 `origin`/事件参数级条件筛选的原因。
- 相反，`origin` 字段语义是签名交易的 EOA（`tx.origin`），**不受调用层级影响**——不管中间经过多少层合约调用，最外层签名者是固定的，所以 `origin` 快速排除永远安全，可以随时设置。
- 因此实现上的判定原则：**只有当某个事件被约定为只能由目标合约自身直接调用触发（不经中间合约包装）时，才可以对该规则设置 `contract_address`**——如主设计文档示例 3（`Deposit` 事件，只由 bridge 合约自己直接调用触发）、示例 4（`updateAssetOwner` 管理函数，同样只能直接调用）。对标准 ERC20/ERC1155 转账类事件（示例 1、2，以及黄金样例场景 a/b/c/d 的 `Transfer`），必须留空 `contract_address`。
- 实现建议：规则加载阶段可以（非契约强制，属 Filter 侧质量保护措施）对声明了 `contract_address` 的规则打印告警日志，提示运维复核该事件是否确实只能被直接调用触发；但不能仅凭"事件名是 Transfer/TransferSingle"做自动拒绝，因为这是业务语义判断，不是可机械校验的语法规则。

未被排除的规则进入 3.3，成为"候选规则"。

### 3.3 阶段二：事件匹配（topic0 索引 + 具名事件解码）

- 候选规则按其 `event_abis` 声明的每个具名事件的 `topic0`（由事件签名 `name` + `inputs[].type` 顺序计算的标准 keccak256）在预建索引中查找。索引结构：`topic0 -> [规则 id 列表]`（构建时机见第 5 节）。
- 对交易的每条日志，用其 `topics[0]` 命中索引，找到可能相关的候选规则集合。
- 对每条候选规则声明的每个具名事件，在交易日志里查找一条匹配日志（`log.address` 命中该事件的宿主关系不做限定——`event_abis` 只声明事件本身的 ABI，不绑定特定合约地址，除非规则通过 `condition` 里的 `<name>.address` 变量显式约束）：
  - 找到 → 按 ABI `inputs` 顺序解码该日志的 `data`/`topics`，按 `inputs[].name` 打包成具名参数，供 `<name>.<param_name>` 变量引用；`<name>.address` = 该日志的 `log.address`。
  - 找不到 → 该具名事件的全部参数变量及 `<name>.address` 均为 `null`（主设计文档 §4.6"完整变量清单"）。
  - 同一具名事件若匹配到多条日志，取第一条，不做多组配对（主设计文档 §4.6 明确约定，契约文档 §3.2 同此约定）。
- 补齐全部具名事件变量后，对该候选规则求值 `condition`（JSONLogic）。

复杂度：索引查找把候选规则集合从"全部 M 条规则"降到"topic0 命中的规则子集"，摊销复杂度从 O(N×M) 降为接近 O(N)（主设计文档 §3.6、§5.2"事件匹配策略"）。

### 3.4 阶段三：动作合并（severity-based merge）

合并遵循优先级 `deny > audit > allow`，且合并粒度分两层：

**(a) 单条日志内多条候选规则命中的合并**（一条日志可能被多条规则的 `condition` 同时判真，因为多条规则可以共享同样的 `topic0` 索引项）：

- 取该日志上全部 `condition` 为真的候选规则的 `action`，按严格程度合并：任一为 `deny` → 该日志判 `deny`；否则任一为 `audit` → 该日志判 `audit`（`audit_types` 取全部命中 `audit` 规则的 `audit_types` 并集）；否则（全部为 `allow` 或无命中）→ 该日志判 `allow`/不命中。
- `allow` 不能覆盖同一日志上同时命中的 `deny`/`audit`（主设计文档 §4.6"动作语义"）。

**(b) 跨日志（整笔交易维度）的合并**：

- 交易内**任意一条日志**判定 `deny` → 立即整笔 `mark_invalid` 丢弃，终止对该交易剩余日志的扫描（不进缓冲池、不请求 RCS）。这是短路优化：一旦确定 deny，没有必要继续跑完剩余日志的匹配。
- 若无 `deny`，交易内**存在至少一条日志**判定 `audit` → 按 `audit_types` 分组，将各日志命中的具名事件（`name`/`address`/`params`）打包进对应 `actions[<type>][]` 数组（结构见契约文档 §2.4），整笔进入缓冲池、走 202 提交流程。
- 若无 `deny` 且无 `audit`（全部日志判 `allow` 或无命中）→ 直接放行，正常打包，不进缓冲池、不请求 RCS。

**(c) 命中项级别的最终裁决合并**（缓冲池条目收到 RCS 查询结果后）：一个 event 需要其所属的每个 `actions` key 对应的审计都通过才算通过；`actions` 某 key 下全部 event 通过才算这个 key 对应的命中项 approved；一笔交易全部 key 都 approved 才整笔放行（契约文档 §2.4 裁决语义原文）——这一层合并逻辑在 RCS 侧完成，Filter 只需按查询接口返回的整笔交易级 `status` 采取动作，不需要在本地重新执行这层合并。

### 3.5 拆分小额规避（已知取舍，非本组件缺陷）

同一笔交易内多条日志各自独立命中、独立生成提交项，不做跨事件的金额汇总（主设计文档 §3.6、§5.1 对应行）。实现时不应引入任何"跨日志汇总金额后再判断阈值规则"的逻辑——这是主设计文档明确记录的取舍，账本层面的总额度控制（RCS 侧 `reserved_total`）不受影响，只有针对单笔事件金额的阈值类规则可能被绕开，接受该限制。

## 4. quota_consistency_hash

依据 ADR-0003，本节所有内容均为 **Filter 本地机制**，RCS 不参与、不感知，任何字段都不出现在 RCS↔Filter 的请求/响应报文中（契约文档 §1"不覆盖范围"、§4 场景 a 末尾"本契约不覆盖其计算细节"）。

### 4.1 计算范围与时机

- 覆盖范围：仅 `actions.quota` 的内容（不是整个 `actions`）——`quota_consistency_hash` 是 `quota` 审计类型自己的一致性校验机制，不是跨审计类型的通用框架功能（ADR-0003 推论）。未来新增审计类型如需类似校验，应自行设计，不得复用本字段。
- 计算时机与存储：
  1. **提交时**：Filter 完成 3.4 节的动作合并、确定要提交的 `actions.quota` 内容后，立即对该内容计算一次哈希，存入 `bufferPool[tx_hash].quota_consistency_hash`，同时把当次 `actions.quota` 内容本身也存入 `bufferPool[tx_hash]`（供第 2 步比对基准，或用于打包前"重新模拟"的输入还原）。
  2. **打包前（收到 `Approved` 之后）**：Filter 对该交易重新模拟执行（拿最新链上状态重跑一次），按同样的匹配流程（第 3 节）重新生成一份 `actions.quota` 内容，对其重新计算一次哈希。
  3. **比对**：将第 2 步的哈希与 `bufferPool` 中存的第 1 步哈希比较，一致 → 放行打包；不一致 → 直接丢弃（缓冲池状态机 `Approved -> [终态: 丢弃]`，第 2.2 节）。

### 4.2 编码方式：规范化编码，非 JSON 序列化字符串哈希

依据 ADR-0003 原文，哈希必须对 `actions.quota` 内容做**规范化编码**后取 keccak256，不能直接对 JSON 序列化后的字符串取哈希——JSON 序列化格式不固定（key 顺序、空格、数字表示等都可能因序列化库/版本而变化），直接哈希 JSON 字符串会导致语义相同但序列化结果不同的两次提交被误判为不一致（假阴性：本该判定一致却判不一致，导致本该正常放行的交易被错误丢弃）。

实现约束：

- 规范化编码必须对以下情形保证确定性输出：
  - `actions.quota` 数组内元素的顺序（若匹配流程本身对同一具名事件"取第一条日志"的规则是确定的——见 3.3 节"取第一条不做多组配对"，且日志本身在交易执行结果里的顺序是确定的，则数组顺序天然确定，不需要额外排序步骤；但编码格式本身仍需要固定字段序列化顺序，例如始终按 `name` → `address` → `params` 的固定字段顺序编码每个 event 对象，`params` 内部按该事件 ABI `inputs` 声明顺序而非任意 map 遍历顺序编码，避免因不同哈希实现之间 map 遍历顺序不确定引入的非确定性）。
  - 数值类型（如 `value`/`amount` 这类 uint256 字符串）必须按固定的规范化数值表示编码（如统一按十进制字符串或统一按大端字节定长编码），不能依赖某个中间层对数字的"美化"格式化（如千分位、科学计数法）。
  - 地址类字段统一大小写（建议全部小写十六进制，不做 EIP-55 checksum 编码，避免因是否应用 checksum 造成的不一致）。
- 推荐实现路径：复用 xlayer-reth 已依赖的 `alloy-primitives`/`alloy-dyn-abi` 生态中面向 ABI 编码的原语（如 `abi_encode`/`sol_data` 风格的定长编码），对 `actions.quota` 按上述固定字段顺序拼接后整体 keccak256，而不是引入额外的通用规范化 JSON 库（如 RFC 8785 JCS）——因为本方案的编码对象结构固定（就是 `actions.quota` 这个特定 schema），用 ABI 风格的确定性编码比引入通用 JSON 规范化更贴合已有技术栈，且性能更好。
- 具体编码算法（字段顺序、每种类型的字节表示）作为实现细节，由本组件自行确定并在代码中以常量/文档注释形式固定下来，不需要与 RCS 对齐（RCS 完全不参与、不感知这个哈希，契约文档 §1）。**注意**：算法一旦确定，其内部实现变更本身不需要跨团队协调，但必须保证同一 Filter 二进制版本内"提交时计算"与"打包前计算"两处调用的是同一套编码逻辑（建议提取为单一共享函数，避免两处实现漂移导致的假阳性不一致）。

### 4.3 不一致后的预占回收路径

`quota_consistency_hash` 不一致导致的丢弃，不是 RCS 可见事件——RCS 眼中这笔交易仍然是 `approved`（预占仍占用）。预占的回收完全依赖 RCS 侧标准的 outdated 两阶段流程自动完成（`approval_ttl_seconds` 超时转 outdated，`outdated_release_delay_seconds` 后释放，见主设计文档 §4.3.3、§5.1"放行时实际执行结果与审批时不一致"行）——Filter 侧**不需要**、也**没有接口**主动通知 RCS"这笔我丢弃了"。实现时不应尝试新增任何主动撤销/取消提交的调用，契约文档四个接口里没有这样的操作。

## 5. 规则加载与热更新

### 5.1 启动阶段：阻塞加载

- 节点启动时，Filter 同步阻塞调用 `GET /rules`（契约文档 §2.2）。
- 拉取失败（网络错误或非 2xx）→ 指数退避重试，打日志，**不设次数上限**，**不使用默认规则跑过渡期**（主设计文档 §4.2.1、§5.1，契约文档 §2.2"失败处理"）。节点在拉到一份有效规则之前不参与出块。
- 拉取成功后校验 `protocol_version` 是否在本地已知支持集合内（当前实现仅需支持 `{1}`，契约文档 §2.1）：
  - 支持 → 用响应里的 `rules` 数组构建规则索引（3.3 节的 `topic0 -> [规则 id]` 索引），加载校验规则见下 5.4，通过后原子生效，节点开始参与出块。
  - 不支持 → 按"拉不到规则不启动"同等策略处理：阻塞重试 + 告警，不使用不兼容的规则包（因为版本不兼容意味着 schema 可能有 Filter 无法正确解析的必填字段变化，强行按旧 schema 解析新协议版本的数据是不安全的）。

### 5.2 运行期：后台轮询 + 热更新

- 后台 tokio task 定期调用 `GET /rules/version`（契约文档 §2.3），获取 `{ protocol_version, content_version }`。
- `content_version` 与本地当前生效版本不同 → 触发一次 `GET /rules` 全量拉取。
- `content_version` 无变化 → 跳过，不做任何拉取。
- 轮询间隔本身是 Filter 本地配置项（未在契约文档/主设计文档给出具体推荐数值，需实现时选定一个合理值，如若干秒级，并允许运维调整；不属于协议契约的一部分，实现者可自行设定初始默认值并在配置文档中说明）。

### 5.3 protocol_version 兼容性处理

- 全量拉取回来的 `protocol_version` 若在本地已知支持集合内 → 用新规则数组重新构建一份新的索引结构，构建完成后**原子替换**本地规则缓存与索引指针（保证同一时刻拦截路径读到的规则缓存和索引是同一版本，不会出现规则缓存已更新但索引还是旧版本的中间态；实现上可用不可变数据结构 + 原子指针切换，如 `Arc` 的 swap，避免拦截热路径读取时加锁）。
- 若不在支持集合内 → **拒绝本次更新，沿用旧规则，记录告警日志**（主设计文档 §4.3.1 阶段二，契约文档 §2.1 表格右列）。这与启动阶段的处理不同：启动阶段没有"旧规则"可沿用，必须阻塞重试；运行期已经持有一份有效规则，可以继续运行、只是错过这次更新。
- 两个接口返回的版本号在同一时刻对同一份规则必须一致（契约文档 §2.1 末段）；Filter 不需要额外校验这一点（这是 RCS 侧必须保证的契约义务），但如果实现中发现 `GET /rules/version` 与紧随其后的 `GET /rules` 返回的版本号不一致（竞态：轮询期间 RCS 刚好又发生一次更新），应按最终 `GET /rules` 响应里携带的版本号为准（响应自带的版本号总是权威的，"探测"接口只是提示是否需要拉取，不是数据源本身）。

### 5.4 规则加载校验

Filter 加载规则数组时必须实现与 RCS 同等强度的校验（契约文档 §3.1 顶部原文的强制要求），校验失败的规则**直接拒绝整条规则**（不是忽略某字段，也不是拒绝整个规则包）：

1. `event_abis` 中任一事件的 `inputs[].name` 缺失或在该事件内重复 → 拒绝该规则。
2. `audit_types` 含 `"quota"` 但 `event_abis` 中存在事件形状不满足契约文档 §3.3 两种固定形状（ERC20 `Transfer` 的 `from,to,value`；ERC1155 `TransferSingle` 的 `operator,from,to,id,value`）之一 → 拒绝该规则。
3. `event_abis` 为空 → 拒绝该规则（"没有任何 `event_abis` 条目的规则永远不会被匹配流程触达，等价于死规则"，契约文档 §3.1）。
4. `action="audit"` 且省略 `audit_timeout_action` → 加载期在内存中补齐为 `"allow"`（契约文档 §3.6 消歧内容，5.4 节此处只是重申，实际处理逻辑见 2.2 节）。

被拒绝的规则应记录告警日志（包含规则 `id`、拒绝原因），但不应导致整批规则加载失败——其余通过校验的规则正常生效。

## 6. Mock RCS 测试方案

### 6.1 设计目标

Mock RCS 是 Filter 集成测试的测试替身，定位：

- **不实现真实裁决逻辑**：不做任何额度数学运算（不维护 `available`/`reserved`/`ledger`），不实现同 nonce 替换检测算法本身的判定逻辑。
- **纯 fixture 驱动**：测试用例显式注册"当 tx_hash=X 被提交/查询时，返回 Y"，Mock 按注册的脚本原样应答，不做任何推断。
- **直接复用黄金测试样例**：契约文档 §4 的四个场景（a/b/c/d）作为预置 fixture 素材，逐字复用其请求/响应 JSON，不允许 Filter 测试代码另起一套等价但不同的测试数据（契约文档 §4 前言："双方测试代码应直接复用这些常量，不要各自另起一套"）。

### 6.2 控制面（测试用例如何驱动 Mock）

Mock 对测试代码暴露的编程接口（非 REST，是同进程/同测试 harness 内的控制 API，因为 Mock 本身作为 Filter 集成测试的一部分启动，不是独立部署的服务）：

| 控制操作 | 用途 |
|---|---|
| `register_submit_response(tx_hash, response)` | 注册：当 `POST /permission-requests/submit` 请求中包含该 `tx_hash` 时，其在响应 `accepted`/`rejected_malformed` 中的归属；未注册的 `tx_hash` 默认落入 `accepted`（模拟"正常收到"路径，覆盖大多数场景不需要每个用例都显式声明这一步） |
| `register_query_state(tx_hash, status, decided_at, reason)` | 注册：当 `GET /permission-requests/query` 查询命中该 `tx_hash` 时（无论按 `status=` 还是按 `tx_hashes=` 模式），返回的状态记录；支持随时更新同一 `tx_hash` 的注册状态，用于模拟场景 d 那种随时间推进的状态转移（`pending -> approved -> outdated`，测试代码在时间线的每个断点调用一次覆盖注册） |
| `unregister_query_state(tx_hash)` | 移除某 `tx_hash` 的状态记录，使其后续查询命中契约文档 §2.5"静默缺席"约定（不出现在 `txs` 数组中），用于模拟 `terminal_entry_retention_seconds` 后被清理、或"从未提交过"的场景 |
| `set_rules_fixture(protocol_version, content_version, rules)` | 注册测试用的规则集，供 `GET /rules`/`GET /rules/version` 返回；测试用例按需构造契约文档 §3 结构的规则数组（可复用黄金样例场景 a/b 的规则对象） |
| `bump_rules_version(new_content_version, new_rules)` | 在已运行的 Mock 上原地更新规则集与 `content_version`（`protocol_version` 不变），用于测试 Filter 的热更新轮询路径（5.2 节） |
| `set_rules_protocol_version(new_protocol_version)` | 单独更新 `protocol_version`（不改 `rules`/`content_version`），用于测试 5.3 节"不支持的 protocol_version"分支 |
| `call_log()` / `call_count(endpoint)` | 读取 Mock 收到的全部请求记录（含请求体），用于断言"未产生任何出站调用"（黄金场景 b 的关键断言点）或断言 Filter 实际提交的报文内容 |
| `set_unavailable(bool)` | 切换 Mock 进入"不可达"模式：后续全部请求（含 `GET /rules`）直接返回网络错误/超时，用于测试 §5.1 阻塞重试、§2.4 的宽限期不早退场景 |

### 6.3 幂等性与规则接口行为

- **提交幂等性**（对应契约文档 §2.4 幂等性约定）：Mock 内部维护一个"已见过的 tx_hash 集合"。同一 `tx_hash` 第二次出现在提交请求中：
  - 若之前从未注册过显式的 `register_query_state`（即该 tx 首次提交后还没有裁决结果）→ 仍返回 `accepted`，不改变任何内部状态（模拟"已登记为 pending，等待裁决"）。
  - 若之前已经 `register_query_state` 为任意状态（`approved`/`denied`/`outdated`/`completed`）→ 同样返回 `accepted`（幂等：不重新登记、不重新裁决，直接计入 `accepted`），且**不清除**已注册的查询状态——测试用例若想模拟"重复提交后状态发生变化"，必须显式再调用一次 `register_query_state` 更新，Mock 不会自动改变状态。
  - 这一行为直接对应契约文档 §2.4 原文："若 RCS 已见过该 tx_hash...不重新登记、不重新裁决，直接把该 tx_hash 计入 accepted"。
- **GET /rules 行为**：返回当前通过 `set_rules_fixture`/`bump_rules_version`/`set_rules_protocol_version` 注册的最新规则集快照；未调用过 `set_rules_fixture` 时的默认行为由测试 harness 决定是返回空规则数组还是要求测试用例必须先设置（建议：要求显式设置，未设置时返回 500 或明确的"未配置"错误，强制每个集成测试用例显式声明其规则前置条件，避免隐式依赖上一个用例遗留的状态）。
- **GET /rules/version 行为**：返回与当前 `GET /rules` 快照一致的 `{protocol_version, content_version}`，保证契约文档 §2.1"两个接口返回的版本号必须一致"这条约束在 Mock 上天然满足（因为两者读同一份内部状态）。

### 6.4 Worked Example：场景 a 端到端集成测试

以下描述一个完整的 Filter 集成测试用例，端到端覆盖"模拟一笔交易 → Filter 提交正确报文 → Mock 按场景 a 应答 → 断言 Filter 缓冲池终态与打包决策"。

**测试前置**：

1. `set_rules_fixture(protocol_version=1, content_version=1, rules=[场景a规则对象])`（规则 JSON 逐字取自契约文档 §4 场景 a："risk-check-erc20-tokenx-bridge"）。
2. 启动 Filter（指向该 Mock 的地址），Filter 阻塞加载规则成功，建好索引。

**模拟交易**：构造一笔交易的执行结果，包含一条日志：`TOKEN_X` 合约（`0x0202...0202`）上的 `Transfer(from=BRIDGE_ERC20=0x0101...0101, to=RECIPIENT=0x0303...0303, value=1000000000000000000)`；交易级字段 `tx_hash=TX_A`、`origin=ORIGIN=0x0404...0404`、`contract_address(tx.to)=CLAIM_CONTRACT=0x0505...0505`、`nonce=1`（均取自契约文档 §4 固定常量表）。

**预期 Filter 内部行为**（按第 3 节算法逐步验证）：

1. 阶段零：`bufferPool[TX_A]` 无记录，进入匹配。
2. 阶段一：规则 `risk-check-erc20-tokenx-bridge` 的 `contract_address`/`origin` 均为 `null`，不做快速排除，进入候选集。
3. 阶段二：该规则声明的具名事件 `transfer` 的 `topic0` 命中交易日志里的 `Transfer` 日志；解码得到 `transfer.address = TOKEN_X`、`transfer.from = BRIDGE_ERC20`、`transfer.to = RECIPIENT`、`transfer.value = "1000000000000000000"`。`condition`（`transfer.address == TOKEN_X and transfer.from == BRIDGE_ERC20`）求值为真。
4. 阶段三：该规则 `action=audit`，无其它规则命中该日志 → 该日志判定 `audit`；交易内只有这一条日志、无 `deny` → 整笔交易判定 `audit`，`actions.quota` 打包为：

```json
{
  "quota": [
    { "name": "transfer", "address": "0x0202020202020202020202020202020202020202",
      "params": { "from": "0x0101010101010101010101010101010101010101",
                  "to": "0x0303030303030303030303030303030303030303",
                  "value": "1000000000000000000" } }
  ]
}
```

5. Filter 计算 `quota_consistency_hash`（第 4 节算法）存入 `bufferPool[TX_A]`，状态置为 `NotSubmitted`。

**断言点 1（提交报文）**：下一个 `batch_window_ms` 窗口触发批量提交后，断言 Mock `call_log()` 中记录的 `POST /permission-requests/submit` 请求体，与契约文档 §4 场景 a"Filter → RCS 提交请求"给出的 JSON **逐字段一致**（`xlayer_block_height`/`tx_hash`/`origin`/`contract_address`/`nonce`/`actions.quota[0].{name,address,params}`）。

**Mock 应答**：Mock 按提交请求返回 `202 { "accepted": ["0xaaaa...aaaa"], "rejected_malformed": [] }`（默认行为，未显式 `register_submit_response` 时按 6.3 节默认落入 `accepted`）。Filter 状态转为 `Submitted`。

**测试驱动查询结果**：测试代码调用 `register_query_state(TX_A, status="approved", decided_at=1751000002, reason=None)`，模拟契约文档 §4 场景 a"Filter → RCS 查询"给出的响应。

**断言点 2（状态转移）**：Filter 下一次轮询 `GET /permission-requests/query`（`?status=approved` 或 `?tx_hashes=`）后，断言 `bufferPool[TX_A].status` 从 `Submitted` 经 `Pending` 转为 `Approved`（若测试 harness 允许逐步驱动，可分别断言 `Submitted->Pending`——收到任意非缺席响应即确认收录；`Pending->Approved`——收到 `status=approved`）。

**断言点 3（打包决策）**：Filter 触发打包前重新模拟（测试用例保持链上状态不变，模拟"无变化"的一致场景），重新计算 `quota_consistency_hash` 与存量一致 → 断言最终该交易被放行进入正常打包流程（而非丢弃），且 `bufferPool[TX_A]` 条目被移除（终态清理）。

该测试用例应作为集成测试套件里的基准正向路径（happy path），后续场景 c（额度不足）、场景 d（超时→outdated）可在此基础上只替换"测试驱动查询结果"步骤里的注册状态与断言点 3 的预期分支。

## 7. 测试计划

### 7.1 单测清单

（对应主设计文档 §5.3"Filter 单测"逐项落地为具体测试用例，不复述该章节原文，仅列出实现时的用例分组）

- 规则加载校验：
  - `event_abis` 中 `inputs[].name` 缺失/重复 → 规则被拒绝，其余规则正常加载（对应 5.4 节校验规则 1）。
  - `audit_types` 含 `quota` 但事件形状不满足 ERC20 `Transfer`/ERC1155 `TransferSingle` 固定形状 → 规则被拒绝（对应 5.4 节校验规则 2；用例应覆盖"字段名对但缺字段"“字段名不对”两类反例）。
  - `event_abis` 为空 → 规则被拒绝（对应 5.4 节校验规则 3）。
  - `action=audit` 省略 `audit_timeout_action` → 加载后内存中读到的值为 `"allow"`（对应 5.4 节校验规则 4）。
- 筛选跳过：
  - `contract_address` 设置且与 `tx.to` 不符 → 规则被排除，不解码任何日志（可用 mock 解码函数的调用计数断言"零解码"）。
  - `origin` 设置且与 `tx.origin` 不符 → 同上。
  - 两者均为 `null` → 规则进入候选集，不做排除。
- 事件匹配与索引：
  - 规则索引按 `topic0` 正确分组，多条规则共享同一 `topic0` 时全部进入候选集。
  - 具名事件在交易日志中找不到匹配 → 对应变量（`<name>.<param_name>`、`<name>.address`）取值为 `null`，且 `condition` 能基于 `null` 正确求值（覆盖场景 3.6 节示例 3 的"配套事件缺失即判 deny"分支）。
  - 同一具名事件匹配到多条日志 → 只取第一条参与求值，不做多组配对。
- 动作合并：
  - 单条日志被多条规则命中，取最严格 action（`deny > audit > allow`）。
  - 同为 `audit` 的多条规则命中同一日志，`audit_types` 取并集。
  - 交易内任意日志 `deny` → 整笔立即丢弃，跳过剩余日志扫描（可用调用计数断言"扫描提前终止"）。
  - 交易内无 `deny`、存在 `audit` → 按 `audit_types` 分组打包 `actions`。
  - 交易内全部 `allow`/无命中 → 直接放行，不进缓冲池。
- 缓冲池状态机：
  - 全路径覆盖：`NotSubmitted->Submitted->Pending->Approved->[终态]`、`...->Denied->[终态]`、`...->Outdated->[终态]`。
  - `Approved->Outdated` 分支（多轮轮询后发现已转 outdated）。
  - `Submitted->NotSubmitted`、`Pending->NotSubmitted` 各自的超时触发条件（8s/20s 边界值测试）。
  - `total_retry_timeout_seconds`（90s）跨状态循环累计计时的正确性：多次 `NotSubmitted<->Submitted<->Pending` 循环后，累计时长超过 90s 才触发 fail-open/fail-close，且触发的是命中规则的 `audit_timeout_action`（分别测试 `allow`/`deny` 两种取值）。
  - tx_hash 去重：已有非终态记录的 tx_hash 命中缓冲池短路，不重新扫描日志。
- `quota_consistency_hash`：
  - 相同 `actions.quota` 内容（含字段顺序、JSON 空格等表层差异）→ 哈希结果确定性一致（验证"规范化编码"而非"JSON 字符串哈希"，第 4.2 节）。
  - 不同 `actions.quota` 内容 → 哈希不同。
  - 多命中项场景下，哈希只覆盖 `actions.quota`，不受同一提交请求中其它假设字段变化影响（当前仅有 `quota` 一种审计类型，此用例为前瞻性覆盖，防止未来误扩大哈希覆盖范围时的回归）。

### 7.2 集成测试清单（显式标注复用的黄金 fixture）

| 测试场景 | 复用的黄金 fixture（契约文档 §4） | 覆盖点 |
|---|---|---|
| 正常放行全流程 | 场景 a | 6.4 节 worked example；提交报文正确性、`Submitted->Pending->Approved` 转移、`quota_consistency_hash` 一致后放行打包 |
| Filter-only 黑名单丢弃，RCS 零调用 | 场景 b | 规则 `blk-compromised-contract` 命中 `deny`；断言不进缓冲池、`call_count("submit")==0`、`call_count("query")==0`（对应场景 b 原文"mock RCS client 的调用计数为 0"这一断言点） |
| 额度不足拒绝 | 场景 c | `register_query_state` 返回 `status=denied, reason="insufficient available quota..."`；断言 `Pending->Denied->[终态: 丢弃]`，且 Filter 不解析/不依赖 `reason` 内容做任何分支判断（仅记录日志） |
| 审批超时转 outdated | 场景 d | 按黄金样例时间线（`t=1751000000/1751000002/1751000035/1751000060`）依次调用 `register_query_state` 推进状态；断言 `Approved->Outdated` 转移发生在 Filter 观测到 `status=outdated` 的首次轮询（`t≈1751000035`），且此后不再重试打包；本场景不触发 `quota_consistency_hash` 校验（交易从未进入打包前重新模拟这一步，与场景 d 原文一致） |
| RCS 完全不可达（宕机/网络分区） | 无对应黄金场景，自建 fixture | `set_unavailable(true)`；断言提交/轮询按指数退避重试；配合场景 a 的 tx，验证累计滞留超过 `total_retry_timeout_seconds` 后按命中规则 `audit_timeout_action` 处理（对应主设计文档 §5.1 首行） |
| 节点启动时无法连接 RCS | 无对应黄金场景，自建 fixture | 启动阶段 `set_unavailable(true)`；断言节点阻塞、指数退避重试且不设次数上限、不使用默认规则；随后 `set_unavailable(false)` 并提供 `set_rules_fixture`，断言节点能恢复并正常启动 |
| RCS 主备切换宽限期内的 Filter 行为（不早退） | 无对应黄金场景，自建 fixture，基于场景 a 的 tx | 模拟"提交后立即进入 `set_unavailable(true)` 持续 50s（对应第 2.4 节的宽限期建议值），再恢复"；断言 Filter 在此期间正常经历 `Pending->NotSubmitted->Submitted` 循环重试，且**不会**在 90s 累计阈值前提前触发 `audit_timeout_action`；这是第 2.4 节论证的直接回归测试 |
| 规则热更新（content_version 变化） | 无对应黄金场景，复用场景 a/b 规则对象组合 | `bump_rules_version` 后断言新提交的交易按新规则匹配，旧提交已生效的判定不受影响（若已在缓冲池中，第 3.1 节"复用已有状态"逻辑保证不会因规则更新而重新匹配） |
| protocol_version 不支持 | 无对应黄金场景，自建 fixture | `set_rules_protocol_version(2)`（假设本地只支持 `{1}`）；运行期断言"拒绝更新, 沿用旧规则并告警"；启动期断言"阻塞重试, 不使用默认规则" |
| `NotSubmitted` 循环重试后最终恢复 | 场景 a 变体：先 `set_unavailable(true)` 若干轮，再恢复并按场景 a 剩余步骤应答 | 验证多轮 `NotSubmitted<->Submitted` 循环后，一旦 RCS 恢复响应，交易能正常继续走完场景 a 的后续流程直到放行，不因为经历过若干次超时循环而破坏后续状态转移的正确性 |

### 7.3 端到端测试（testnet，超出本组件单独可验证范围，仅列出与本组件相关的验证点）

依主设计文档 §5.3"端到端（testnet）"，Filter 侧需配合验证：多命中项提现场景（含合约内部调用桥合约的多层调用场景，验证 3.2 节 `contract_address` 留空处理是否真的在多层调用场景下正确命中）；构造审批后状态变化触发 `quota_consistency_hash` 不一致的场景（验证第 4 节丢弃路径与预占依赖 RCS 侧自动回收，Filter 不主动通知）。这两类测试依赖真实 RCS 部署与真实链上环境，不在本组件的 mock 集成测试范围内，仅在此列出对接点供端到端测试计划引用。

## 8. 待确认的开放问题（Open Questions）

以下问题在阅读源材料时未找到明确结论，本文档按最合理的推断给出了实现建议，但建议在实现前与主设计文档作者/RCS 团队确认，避免行为漂移：

1. **`GET /rules/version` 轮询间隔的具体数值**：主设计文档与契约文档均未给出建议值（不同于 `batch_window_ms`/各超时参数均有明确建议值）。本文档 5.2 节建议实现时自行选定一个合理的秒级默认值并留作可配置项，但未给出具体数字，需要实现者/运维根据实际规则变更频率和可接受的生效延迟权衡确定。
2. **规则加载校验的"告警日志"具体格式/去向**（5.4 节、3.2 节告警）：源材料多处提到"记录告警日志"但未定义日志格式、告警渠道（是否需要接入监控告警系统而不只是本地日志），实现时需要与运维/监控团队对齐现有告警接入方式。
3. **`contract_address` 误设的自动检测**（3.2 节实现建议部分）：本文档建议"可以打印告警但不能自动拒绝"，因为这是业务语义判断；如果未来需要更强的保护（例如维护一份"已知会被中间合约包装的事件签名"黑名单在加载时做启发式检测），需要与规则运维流程的所有者进一步讨论，本文档不代其做出决定。

---

（完）
