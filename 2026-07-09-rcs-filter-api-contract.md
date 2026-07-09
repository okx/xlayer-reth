# RCS ↔ XLayer Filter API 契约（RCS-Filter Wire Contract）

- 日期：2026-07-09
- 状态：Binding（双方约束，非草案）

## 1. 目的与范围

本文档是 **风控服务（RCS）** 与 **XLayer Filter**（xlayer-reth 节点内的拦截组件）之间的唯一权威接口契约。RCS 与 Filter 由两个独立团队在两个独立仓库中分别实现；本文档定义两者之间的 REST/JSON 报文格式、字段语义、规则 JSON 结构，以及双方必须实现一致的黄金测试样例。

- **变更规则**：本文档的接口定义与规则 JSON 结构一经确定，**只能通过双方一致同意修改**；任一方单方面变更字段含义、新增必填字段、改变状态语义都视为破坏契约。
- **引用规则**：RCS、Filter 各自的实现设计文档/技术方案，涉及本文档覆盖的 schema 时必须**引用本文件路径**，不得重新定义或复制一份可能漂移的副本。
- **不覆盖范围**：本文档不覆盖 RCS 内部实现（额度台账、Sweeper 调度、TZ/XLayer 同步等），也不覆盖 Filter 内部实现（缓冲池状态机、批量提交调度、`quota_consistency_hash` 的具体编码算法）——`quota_consistency_hash` 是 Filter 本地机制，**不出现在任何 RCS↔Filter 的请求/响应报文中**（见 ADR-0003），本文档黄金样例中提及它仅作为流程说明，不为其计算方式做背书。这些内部实现细节见各自仓库的设计文档，以及本项目的主设计文档 `2026-07-06-bridge-withdrawal-risk-control-review.md`（背景信息，非本契约的组成部分）。

## 2. REST 接口定义

传输协议：**REST + JSON**，RCS 为 server，Filter 为 client。四个接口：

| 方法 | 路径 | 用途 |
|---|---|---|
| GET | `/rules` | 规则全量拉取 |
| GET | `/rules/version` | 规则版本探测（轻量轮询） |
| POST | `/permission-requests/submit` | 批量提交待裁决交易 |
| GET | `/permission-requests/query` | 查询裁决状态 |

### 2.1 版本语义总览

响应中出现两个版本号，语义不同、独立递增，**不得混淆**：

| 字段 | 含义 | 递增时机 | Filter 行为 |
|---|---|---|---|
| `protocol_version` | 本契约本身（REST 报文结构/字段集合）的版本 | 仅当本文档定义的 schema 发生**不兼容变更**时递增（如规则对象新增必填字段、接口语义变化） | Filter 内置一份"已知可处理的 `protocol_version` 集合"（当前实现只需支持 `{1}`）。收到的 `protocol_version` 不在该集合中 → **拒绝本次更新，沿用旧规则缓存，记录告警日志**；节点启动阶段若首次拉取即遇到不支持的 `protocol_version`，按 4.2.1/5.1 的"拉不到规则不启动"策略处理（阻塞重试+告警，不使用默认规则） |
| `content_version` | 规则**内容**的版本（规则数组本身的数据版本，不涉及 schema 结构） | 每次 RCS 侧规则文件内容变化（增/删/改任意一条规则）时递增，`protocol_version` 不变 | Filter 定期 `GET /rules/version` 探测；`content_version` 有变化才发起 `GET /rules` 全量拉取；否则跳过 |

> 消歧：主设计文档 §4.3.1 用"`protocol_version` 在本地支持范围内"描述判定逻辑，未指明是"精确匹配"还是"区间/兼容判定"。本契约采用**精确匹配一个已知支持集合**的读法——因为 `protocol_version` 只在 breaking change 时才递增，语义上不存在"部分兼容"的中间态，Filter 没有必要支持范围判断，只需维护自己代码里实现了哪些版本号。

两个接口返回的版本号必须一致（`GET /rules` 与 `GET /rules/version` 在同一时刻对同一份规则给出相同的 `protocol_version`/`content_version`）。

### 2.2 GET /rules

节点启动阻塞加载、以及 `content_version` 变化后的全量拉取，均调用此接口。

**请求**：无请求体，无查询参数。

**响应** `200 OK`：

```json
{
  "protocol_version": 1,
  "content_version": 42,
  "rules": [ /* 见第 3 节，Rule 对象数组 */ ]
}
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `protocol_version` | integer | 否 | 见 2.1 |
| `content_version` | integer | 否 | 见 2.1 |
| `rules` | array\<Rule\> | 否（可为空数组） | 规则对象数组，见第 3 节；空数组合法，代表"目前没有任何规则"（一切交易直接放行，因为没有规则命中） |

**幂等性**：GET 无副作用，天然幂等，重复调用返回当前最新状态即可，不要求两次调用返回完全相同内容（`content_version` 可能已经变化）。

**失败处理**：网络失败或非 2xx 响应，Filter 按 4.2.1/5.1 的指数退避策略重试，不设次数上限，不使用默认规则跑过渡期。

### 2.3 GET /rules/version

**请求**：无请求体，无查询参数。

**响应** `200 OK`：

```json
{ "protocol_version": 1, "content_version": 42 }
```

字段定义同 2.2。此接口**不返回 `rules` 数组**，仅用于低成本探测是否需要全量拉取。

### 2.4 POST /permission-requests/submit

批量提交拦截到的交易，仅确认入队，**不含裁决结果**——裁决结果永远只能通过 2.5 查询接口获得，提交与查询之间没有请求-响应绑定关系。

**请求** `POST /permission-requests/submit`：

```json
{
  "xlayer_block_height": 1000000,
  "txs": [
    {
      "tx_hash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "origin": "0x0404040404040404040404040404040404040404",
      "contract_address": "0x0505050505050505050505050505050505050505",
      "nonce": 1,
      "actions": {
        "quota": [
          {
            "name": "transfer",
            "address": "0x0202020202020202020202020202020202020202",
            "params": {
              "from": "0x0101010101010101010101010101010101010101",
              "to": "0x0303030303030303030303030303030303030303",
              "value": "1000000000000000000"
            }
          }
        ]
      }
    }
  ]
}
```

字段表：

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `xlayer_block_height` | integer | 否 | 本次提交对应的模拟出块高度；用于 RCS 判断该交易是否已被 XLayer 同步水位覆盖再裁决（见主文档 4.5.2），仅供裁决使用，不代表最终上链块高 |
| `txs` | array | 否（可为空数组） | 本批次提交的交易列表 |
| `txs[].tx_hash` | string (0x + 64 hex) | 否 | 交易哈希，全局唯一标识，幂等键 |
| `txs[].origin` | string (0x + 40 hex) | 否 | `tx.origin`，签名该交易的 EOA；RCS 用于同 `(origin, nonce)` 替换交易检测（见主文档 4.3.3、4.5.2），**不参与账本记账键** |
| `txs[].contract_address` | string (0x + 40 hex) | 否 | `tx.to`；仅供观测/审计，RCS 裁决逻辑不使用此字段判断额度或权限；**不代表** `actions` 里事件日志的 `log.address`（日志可能来自被中间合约调用的下游合约） |
| `txs[].nonce` | integer | 否 | 交易 nonce，与 `origin` 组成同 nonce 替换检测的键 |
| `txs[].actions` | object（`{ 审计类型: Event[] }`） | 否，且**至少含一个 key**（能走到提交流程的交易必然存在至少一个 `audit` 命中项，否则不会被提交） | 审计类型 → 命中事件数组的映射；今天唯一实现的 key 是 `"quota"` |
| `txs[].actions[<type>][].name` | string | 否 | 事件名，对应命中规则 `event_abis` 里的 key（规则本地名，非链上事件名） |
| `txs[].actions[<type>][].address` | string (0x + 40 hex) | 否 | 触发该事件日志的 `log.address`（即事件所属合约地址，如 ERC20 token 合约地址），**不是** `tx.to` |
| `txs[].actions[<type>][].params` | object（具名） | 否 | 按该事件 ABI `inputs[].name` 组织的具名解码结果；字段集合由命中规则的 `event_abis` 决定，非固定 schema——参见第 3 节 `quota` 固定形状要求 |

**响应** `202 Accepted`：

```json
{ "accepted": ["0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"], "rejected_malformed": [] }
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `accepted` | array\<string\> | 本次请求中被成功登记（或已存在、幂等命中）的 `tx_hash` 列表 |
| `rejected_malformed` | array\<string\> | 本次请求中格式非法（缺失必填字段、类型不匹配等）而被直接拒绝、**未登记**的 `tx_hash` 列表 |

> 消歧：主文档示例中 `rejected_malformed` 只给出空数组，未展示非空元素的完整形状。本契约采用与 `accepted` 对称的读法——`rejected_malformed` 是纯 `tx_hash` 字符串数组，不携带拒绝原因；RCS 可在自己的日志中记录详细原因，但**不通过这个字段下发**，避免为一个运维排查用的辅助信息定义额外的响应 schema。

**幂等性**：按 `tx_hash` 幂等。同一 `tx_hash` 重复提交（无论是 Filter 侧缓冲池状态机的 `NotSubmitted` 重试，还是节点重启后的重新提交）：
- 若 RCS 尚未见过该 `tx_hash` → 正常登记为 `pending`，返回于 `accepted`；
- 若 RCS 已见过该 `tx_hash`（无论当前是 `pending`/`approved`/`outdated`/`denied`/`completed`）→ **不重新登记、不重新裁决**，直接把该 `tx_hash` 计入 `accepted`（表示"已收到，且已经在系统里"）。提交响应本身**不返回该 tx 当前的裁决状态**——调用方必须用 2.5 查询接口获取实际状态。

`actions` 内容不一致的重复提交（同一 `tx_hash` 但 `actions` 与首次提交不同）不在本契约定义范围内；这在协议上不应发生，因为 `tx_hash` 本身已经唯一确定了交易内容和其产生的日志。

### 2.5 GET /permission-requests/query

两种互斥的查询模式，**调用方必须且只能传其中一种**（本契约未定义两者同时出现时的行为，视为调用方错误）：

| 模式 | 参数 | 用途 |
|---|---|---|
| 按状态查询 | `?status=pending\|approved\|denied\|outdated` | Filter 常规轮询用，一次拿到某状态下的全部条目 |
| 按哈希查询 | `?tx_hashes=0x..,0x..` | 运维排查/补漏用，不限制个数，调用方是可信内部工具 |

> 消歧：`status` 枚举不包含 `completed`。`completed` 是 RCS 内部状态机（主文档 4.5.2）的终态之一，但 Filter 一旦观测到 `approved` 并成功打包上链就不再关心该 `tx_hash` 的后续状态，因此按状态批量查询没有 `completed` 场景。`?tx_hashes=` 模式面向运维排查，本契约允许其返回值里出现 `status="completed"`（用于人工核实一笔交易的最终归宿），这是两种模式在可返回状态集合上的唯一差异。
>
> 消歧：查询一个 RCS 从未见过的 `tx_hash`（未提交过，或已超过 `terminal_entry_retention_seconds` 从内存清理）——本契约采用"静默缺席"读法：该 `tx_hash` 不出现在 `txs` 数组中，不视为错误、不返回非 2xx 状态码。调用方（尤其 Filter 的 `risk_module_unresponsive_timeout_seconds` 超时判断）应把"缺席"和"长期无终态"同等对待。

**响应** `200 OK`：

```json
{
  "txs": [
    { "tx_hash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "status": "approved", "decided_at": 1751000002, "reason": null }
  ]
}
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `txs[].tx_hash` | string | 否 | 交易哈希 |
| `txs[].status` | string | 否 | `pending` \| `approved` \| `denied` \| `outdated` \|（仅 `?tx_hashes=` 模式）`completed`；整笔交易（跨全部命中项）的最终裁决结果，**不是**命中项级别的状态 |
| `txs[].decided_at` | integer（unix 秒） | **可空**——`status="pending"` 时为 `null`；其余状态下为首次做出终态裁决（`approved`/`denied`）的时间戳。交易由 `approved` 转 `outdated` 不更新此字段——`decided_at` 记录的是"裁决发生的时刻"，不是"状态最近一次变化的时刻" | 裁决时间 |
| `txs[].reason` | string | 可空，恒为可选 | 自由文本，RCS 认为有必要时附带的裁决说明（如"额度不足"、"同 nonce 已被消费"）；不强制填、不定义固定枚举，Filter 不解析、不依赖它做任何判断逻辑，仅用于运维排查 |

**幂等性**：GET 无副作用，天然幂等；两次相邻调用之间状态可能已经推进（如 `pending`→`approved`），这是预期行为，不是不一致。

## 3. 规则 JSON 结构

`GET /rules` 响应里 `rules` 数组的每个元素（Rule 对象）：

```json
{
  "id": "string",
  "contract_address": "0x.. | null",
  "origin": "0x.. | null",
  "event_abis": {
    "<name>": {
      "type": "event",
      "name": "string",
      "inputs": [ { "name": "string", "type": "string", "indexed": true } ],
      "anonymous": false
    }
  },
  "audit_types": ["quota"],
  "condition": "<JSONLogic 表达式>",
  "action": "deny | allow | audit",
  "audit_timeout_action": "deny | allow，可选"
}
```

### 3.1 字段总览

| 字段 | 类型 | 可空/可选 | 说明 |
|---|---|---|---|
| `id` | string | 否 | 规则唯一标识，在 `rules` 数组内必须唯一（用于日志/告警定位，RCS 裁决逻辑不依赖它——见 4.4.3，RCS 只按审计类型分组处理，不追踪是哪条规则命中） |
| `contract_address` | string \| null | 可空 | `tx.to` 快速排除条件。**只应用于确定该事件只会被直接调用触发、不会被中间合约包裹调用的场景**（如管理类函数）；事件可能经由任意深度中间合约调用产生时（如标准 ERC20 `Transfer`）必须留空，否则会漏判 |
| `origin` | string \| null | 可空 | `tx.origin` 快速排除条件，无 `contract_address` 那样的漏判风险，可随时设置 |
| `event_abis` | object（`{ name: EventABI }`） | 否，且**至少一个条目** | 具名事件声明，见 3.2。规则通过声明的具名事件的 `topic0` 被编入匹配索引，成为"候选规则"——**没有任何 `event_abis` 条目的规则永远不会被匹配流程触达，等价于死规则**；本契约按此把"至少一个条目"定为强制约束（主文档未用"MUST"措辞明确写出这一点，但由 4.6 的匹配流程决定，是唯一自洽的读法） |
| `audit_types` | array\<string\> | 可选，默认 `["quota"]` | 仅当 `action="audit"` 时有意义；声明该规则命中后要跑哪些审计逻辑 |
| `condition` | JSONLogic 表达式 \| `true` | 否 | 见 3.4/3.5 |
| `action` | `"deny" \| "allow" \| "audit"` | 否 | 见 3.6 |
| `audit_timeout_action` | `"deny" \| "allow"` | 可选，仅当 `action="audit"` 时有意义 | 见 3.6 |

规则加载时校验失败（见下）的规则**直接拒绝整条规则**（不是忽略某个字段），RCS 与 Filter 都必须在各自的规则加载路径上实现同等强度的校验，防止一方接受、另一方拒绝导致行为不一致：

1. `event_abis` 中任一事件的 `inputs[].name` 缺失或在该事件内重复 → 拒绝该规则（见 3.2）。
2. `audit_types` 含 `"quota"` 但 `event_abis` 中存在事件形状不满足 3.3 两种固定形状之一 → 拒绝该规则。
3. `event_abis` 为空 → 拒绝该规则（见上表 `event_abis` 行的说明）。

### 3.2 event_abis

`{ 具名: ABI 事件定义 }` 的映射；具名（map key）由规则作者自定义，同一条规则可声明多个（用于需要比较同一笔交易内多条日志的场景）。

| 字段 | 类型 | 说明 |
|---|---|---|
| `type` | `"event"` | 固定值 |
| `name` | string | 链上事件的真实 Solidity 名称（如 `"Transfer"`），与 `inputs` 的类型顺序一起决定该事件的 `topic0`（标准 keccak256 签名哈希），用于建立匹配索引 |
| `inputs` | array\<{name, type, indexed}\> | ABI 参数列表。**`name` 必须填写且在该事件内不重复**——这是规则作者自己维护的一份 ABI 拷贝，只影响 Filter 怎么给解码结果打标签，不参与链上编解码或 topic0 计算，所以即使链上原始 ABI 本身有匿名/重名参数，规则作者也可以在这里补全/去重命名。违反此约束的规则在加载时直接拒绝 |
| `anonymous` | boolean | 标准 ABI 字段，决定 `topic0` 是否作为该事件日志的一个 topic（标准 Solidity `event` 语义），用于正确匹配/解码 |

匹配时，具名事件在该笔交易日志里找不到匹配的日志 → 该具名事件的所有参数（`<name>.<param_name>`）和 `<name>.address` 均为 `null`（不是规则不命中，是变量取值为 `null`，再交给 `condition` 求值，见示例 3 的"配套事件缺失即判 deny"用法）。同一具名事件若匹配到多条日志，取第一条参与 `condition` 求值/参与提交，不做多组配对。

### 3.3 quota 审计类型的固定形状要求

`quota` 是内置硬编码约定，**固定只认两种标准事件形状**，规则作者不能声明第三种：

| 链上事件 | 必需 `inputs` 形状（按 name） | RCS 记账解析 |
|---|---|---|
| ERC20 `Transfer` | `from`, `to`, `value` | `to = params.to`，`amount = params.value` |
| ERC1155 `TransferSingle` | `operator`, `from`, `to`, `id`, `value` | `to = params.to`，`amount = params.value`；`id`（`token_id`）**只用于解出 amount 语境，不参与账本记账键**——账本 `(to, token)` 的 `token` 只到合约地址，同一 ERC1155 合约下所有 `token_id` 共享一个额度桶（见 `adr/0001-erc1155-quota-pooled-by-contract-not-token-id.md`） |

判定范围：一条规则若 `action="audit"` 且 `audit_types` 包含 `"quota"`，则该规则 `event_abis` 里**声明的每一个**具名事件都必须满足上述两种形状之一（`inputs[].name` 集合与对应的语义角色完全匹配）——因为该规则命中时，其声明的每个具名事件在交易中若找到匹配日志，都会作为独立条目进入 `actions.quota[]`（见 2.4 字段表），RCS 会对 `actions.quota[]` 里的每一条都尝试按上表解析 `to`/`amount`。任一事件形状不满足，加载时**拒绝整条规则**（不是仅拒绝该事件）。

对不满足上述形状、又需要审计的事件，应定义新的 `audit_type`（自带自己的解析方式），不能把 `quota` 套用到不匹配的形状上。

`to` 即 TZ 侧 withdraw 声明的接收方地址，与 `origin`（谁提交 claim 交易）无关；`origin` 只用于同 nonce 替换检测（见主文档 4.3.3），不参与账本记账键。

### 3.4 condition 变量引用表

| 变量 | 含义 |
|---|---|
| `contract_address` | `tx.to` |
| `origin` | `tx.origin` |
| `value` | 交易的原生 ETH 转账额 |
| `nonce` | 交易 nonce |
| `<name>.<param_name>` | `event_abis` 里名为 `<name>` 的事件解码出的、ABI 里叫 `<param_name>` 的参数；该事件在交易里没有匹配到日志时，其所有参数均为 `null` |
| `<name>.address` | 该具名事件命中日志的 `log.address`；没匹配到为 `null` |

### 3.5 JSONLogic 操作符参考表

完整规范见 https://jsonlogic.com/ ；本方案约定的变量名见 3.4。

| 操作符 | 用法 | 示例 |
|---|---|---|
| `var` | 引用 3.4 变量 | `{"var": "origin"}` |
| `==` / `!=` | 相等 / 不等 | `{"==": [{"var": "origin"}, "0x.."]}` |
| `in` | 成员判断，列表黑名单/白名单 | `{"in": [{"var": "transfer.from"}, ["0xA...", "0xB..."]]}` |
| `and` / `or` | 逻辑与 / 或，任意个数条件 | `{"and": [cond1, cond2]}` |
| `!` | 逻辑非 | `{"!": {"==": [...]}}` |
| `>` / `>=` / `<` / `<=` | 数值比较；uint256 精度取决于 JSONLogic 引擎的大整数支持，超出安全整数范围时不保证精确，谨慎用于金额阈值判断 | `{">": [{"var": "transfer.value"}, 1000000]}` |
| `true`（字面量） | 无条件为真，不需要额外判断时直接写 | `"condition": true` |

### 3.6 action / audit_timeout_action 语义

优先级：`deny > audit > allow`（同一笔交易内多条命中项按最严格优先合并）。

| `action` | 效果 |
|---|---|
| `deny` | Filter 侧 `mark_invalid` 整笔丢弃，**不进缓冲池、不请求 RCS**；命中即终止扫描。这是 Filter-only 行为，RCS 完全不参与、也感知不到这类交易 |
| `allow` | 该日志明确不需裁决；不能覆盖同一日志上同时命中的 `deny`/`audit` |
| `audit` | 该日志各具名事件按规则 `audit_types` 声明的每个类型，归入 `actions` 对应 key 的 events 数组，原样送 RCS 裁决 |

`audit_timeout_action` 仅当 `action="audit"` 时有意义，决定该规则命中的交易在 RCS 长时间无响应（超过 `total_retry_timeout_seconds`）时的兜底处理：`allow` = fail-open（直接放行），`deny` = fail-close（直接丢弃）。

> 消歧：主文档未明确说明 `action="audit"` 的规则省略 `audit_timeout_action` 时的默认行为。`adr/0002-timeout-action-defaults-to-fail-open.md` 给出的判断原则是"默认选 `allow`（在线就兜底，不在线就不兜底），只有该规则防御的场景没有其它防线兜底时才用 `deny`"。本契约据此明确规定：**`action="audit"` 的规则若省略 `audit_timeout_action`，RCS/Filter 均按 `allow` 处理**——双方在各自加载规则时，若遇到 `action="audit"` 且该字段缺失，必须在内存中把它当作 `"allow"`，不能分别实现出不一致的默认值。

## 4. 黄金测试样例（Golden Fixtures）

以下固定值在全部四个场景中复用，双方测试代码应直接复用这些常量，不要各自另起一套。

**固定地址**（40 位 16 进制，`0x` + 40 hex）：

| 常量 | 值 | 角色 |
|---|---|---|
| `BRIDGE_ERC20` | `0x0101010101010101010101010101010101010101` | ERC20 提现的转出方（bridge 合约地址） |
| `TOKEN_X` | `0x0202020202020202020202020202020202020202` | ERC20 token 合约地址；同时是账本 `(to, token)` 的 `token` |
| `RECIPIENT` | `0x0303030303030303030303030303030303030303` | 提现接收地址（`to`） |
| `ORIGIN` | `0x0404040404040404040404040404040404040404` | 发起 claim 交易的 EOA（场景 a/c/d 复用同一用户） |
| `CLAIM_CONTRACT` | `0x0505050505050505050505050505050505050505` | `tx.to`，claim/relayer 合约（**不是** bridge 合约本身，用于体现"事件可能经中间合约调用产生"） |
| `BLACKLISTED_FROM` | `0x0606060606060606060606060606060606060606` | 已确认被攻破/恶意的合约地址（场景 b 专用） |
| `ORIGIN_B` | `0x0707070707070707070707070707070707070707` | 场景 b 的交易发起 EOA（与场景 a/c/d 的正常用户区分） |

**固定交易哈希**（`0x` + 64 hex）：

| 常量 | 值 |
|---|---|
| `TX_A` | `0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` |
| `TX_B` | `0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb` |
| `TX_C` | `0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc` |
| `TX_D` | `0xdddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd` |

金额使用 18 位精度整数字符串（1 枚代币 = `1000000000000000000`）。

RCS 侧超时/调度相关的时间参数是**各自进程本地配置**，不通过 `GET /rules` 分发（见 2 节前言/主文档 4.4.1），因此严格来说不属于本契约的"线上协议"范畴；但场景 d 的时间线依赖它们才能被两队实现出一致的测试断言，故本契约按主文档 §4.2.6/4.3.3 给出的具体数值固定下来，供双方测试用例对齐：`approval_ttl_seconds=30`、`outdated_release_delay_seconds=20`、`reservation_sweep_interval_seconds=5`。若某一方实际部署改变了这些配置值，场景 d 中的绝对时间点应等比例平移，但状态转换的**顺序**（pending→approved→outdated，reserved 释放）不变。

---

### 场景 a：审计通过（额度充足 → approved）

**规则**（`action="audit"`，`audit_types=["quota"]`，命中 ERC20 `Transfer`，`contract_address` 留空因为 `Transfer` 可能经中间合约触发）：

```json
{
  "id": "risk-check-erc20-tokenx-bridge",
  "contract_address": null,
  "origin": null,
  "event_abis": {
    "transfer": {
      "type": "event", "name": "Transfer",
      "inputs": [
        { "name": "from", "type": "address", "indexed": true },
        { "name": "to", "type": "address", "indexed": true },
        { "name": "value", "type": "uint256", "indexed": false }
      ],
      "anonymous": false
    }
  },
  "audit_types": ["quota"],
  "condition": {
    "and": [
      { "==": [{ "var": "transfer.address" }, "0x0202020202020202020202020202020202020202"] },
      { "==": [{ "var": "transfer.from" }, "0x0101010101010101010101010101010101010101"] }
    ]
  },
  "action": "audit",
  "audit_timeout_action": "allow"
}
```

**账本前置条件**（`(RECIPIENT, TOKEN_X)`）：`tz_withdrawn_total = 5000000000000000000`（5 枚），`xlayer_completed_total = 2000000000000000000`（2 枚），`reserved = 0` → `available = 3000000000000000000`（3 枚）。

**触发**：交易在 `TOKEN_X` 合约上产生一条 `Transfer(from=BRIDGE_ERC20, to=RECIPIENT, value=1 枚)` 日志，命中上述规则，`action=audit` → Filter 打包提交项。

**Filter → RCS 提交请求**：

```json
{
  "xlayer_block_height": 1000000,
  "txs": [
    {
      "tx_hash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "origin": "0x0404040404040404040404040404040404040404",
      "contract_address": "0x0505050505050505050505050505050505050505",
      "nonce": 1,
      "actions": {
        "quota": [
          {
            "name": "transfer",
            "address": "0x0202020202020202020202020202020202020202",
            "params": {
              "from": "0x0101010101010101010101010101010101010101",
              "to": "0x0303030303030303030303030303030303030303",
              "value": "1000000000000000000"
            }
          }
        ]
      }
    }
  ]
}
```

**RCS 响应（202）**：

```json
{ "accepted": ["0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"], "rejected_malformed": [] }
```

**裁决**：`available(3 枚) >= 请求金额(1 枚)` → `approved`；`reserved(RECIPIENT, TOKEN_X)` 由 `0` 变为 `1000000000000000000`。

**Filter → RCS 查询**（`?tx_hashes=0xaaaa...aaaa` 或 `?status=approved`）：

```json
{
  "txs": [
    { "tx_hash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "status": "approved", "decided_at": 1751000002, "reason": null }
  ]
}
```

Filter 侧收到 `approved` 后，按本地 `quota_consistency_hash` 校验（本契约不覆盖其计算细节，见第 1 节）确认打包前重新模拟的结果与提交时一致，一致则放行打包。

---

### 场景 b：黑名单拒绝（Filter-only，RCS 不参与）

本场景**从头到尾没有任何 REST 请求**——`action=deny` 的规则命中在 Filter 内部直接 `mark_invalid` 丢弃交易，不进缓冲池、不提交给 RCS。RCS 的测试套件不需要实现本场景；本场景仅约束 Filter 侧行为，之所以收录进本契约，是为了明确划出"deny 与 RCS 无关"这条边界，避免任何一方误以为 deny 也会走一次提交/查询往返。

**规则**：

```json
{
  "id": "blk-compromised-contract",
  "contract_address": null,
  "origin": null,
  "event_abis": {
    "transfer": {
      "type": "event", "name": "Transfer",
      "inputs": [
        { "name": "from", "type": "address", "indexed": true },
        { "name": "to", "type": "address", "indexed": true },
        { "name": "value", "type": "uint256", "indexed": false }
      ],
      "anonymous": false
    }
  },
  "condition": { "in": [{ "var": "transfer.from" }, ["0x0606060606060606060606060606060606060606"]] },
  "action": "deny"
}
```

**触发**：交易 `TX_B`（`origin=ORIGIN_B`）在 `TOKEN_X` 合约上产生一条 `Transfer(from=BLACKLISTED_FROM, to=RECIPIENT, value=1000000000000000000)` 日志。`transfer.from == BLACKLISTED_FROM` 命中 `in` 判断 → `condition` 为真 → `action=deny`。

**Filter 侧行为**（无网络交互）：

1. 事件匹配阶段发现候选命中 `deny` → 立即 `mark_invalid`，整笔交易被排除出本次打包模拟。
2. 不写入缓冲池（`bufferPool[TX_B]` 不存在任何记录）。
3. 不调用 `POST /permission-requests/submit`。
4. 后续也不会出现在任何 `GET /permission-requests/query` 响应里（RCS 从未见过这个 `tx_hash`）。

Filter 单测断言点：`TX_B` 命中规则 `blk-compromised-contract`、合并后的最终 `action` 为 `deny`、缓冲池中不存在该 `tx_hash` 的记录、且没有产生任何出站 RCS 调用（mock RCS client 的调用计数为 0）。

---

### 场景 c：额度不足 → RCS 拒绝（denied）

复用场景 a 的规则 `risk-check-erc20-tokenx-bridge`。

**账本前置条件**（`(RECIPIENT, TOKEN_X)`，独立场景，可单独 seed）：`tz_withdrawn_total = 5000000000000000000`（5 枚），`xlayer_completed_total = 2000000000000000000`（2 枚），`reserved = 1000000000000000000`（1 枚——precondition 为已有另一笔在途 `approved` 交易占用，恰好对应场景 a 中 `TX_A` 裁决后的状态；两个场景可以顺序运行以复现这一累积效果，也可以独立 seed `reserved` 值单跑本场景）→ `available = 2000000000000000000`（2 枚）。

**触发**：交易 `TX_C`（同一 `ORIGIN`，`nonce=2`，与 `TX_A` 的 `nonce=1` 不冲突）产生 `Transfer(from=BRIDGE_ERC20, to=RECIPIENT, value=3 枚)`，命中同一规则。

**Filter → RCS 提交请求**：

```json
{
  "xlayer_block_height": 1000010,
  "txs": [
    {
      "tx_hash": "0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
      "origin": "0x0404040404040404040404040404040404040404",
      "contract_address": "0x0505050505050505050505050505050505050505",
      "nonce": 2,
      "actions": {
        "quota": [
          {
            "name": "transfer",
            "address": "0x0202020202020202020202020202020202020202",
            "params": {
              "from": "0x0101010101010101010101010101010101010101",
              "to": "0x0303030303030303030303030303030303030303",
              "value": "3000000000000000000"
            }
          }
        ]
      }
    }
  ]
}
```

**RCS 响应（202）**：

```json
{ "accepted": ["0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"], "rejected_malformed": [] }
```

**裁决**：`available(2 枚) < 请求金额(3 枚)` → `denied`；`reserved(RECIPIENT, TOKEN_X)` 不变（拒绝的交易不预占任何额度）。

**Filter → RCS 查询**：

```json
{
  "txs": [
    {
      "tx_hash": "0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
      "status": "denied",
      "decided_at": 1751000012,
      "reason": "insufficient available quota: available=2000000000000000000, requested=3000000000000000000"
    }
  ]
}
```

`reason` 为自由文本示例（本契约不固定其内容，见 2.5），Filter 侧看到 `denied` 直接丢弃 `TX_C`，不再重试打包。

---

### 场景 d：审批超时 → outdated → 释放预占

复用场景 a 的规则 `risk-check-erc20-tokenx-bridge`。

**账本前置条件**（独立场景，与 a/c 互不依赖）：`tz_withdrawn_total = 5000000000000000000`（5 枚），`xlayer_completed_total = 2000000000000000000`（2 枚），`reserved = 0` → `available = 3000000000000000000`（3 枚）。

**触发**：交易 `TX_D`（`nonce=3`）产生 `Transfer(from=BRIDGE_ERC20, to=RECIPIENT, value=2 枚)`，命中同一规则，且获批之后**始终未被成功打包上链**（如持续被更高优先级交易挤出出块窗口）。

**Filter → RCS 提交请求**（`t = 1751000000`）：

```json
{
  "xlayer_block_height": 1000020,
  "txs": [
    {
      "tx_hash": "0xdddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
      "origin": "0x0404040404040404040404040404040404040404",
      "contract_address": "0x0505050505050505050505050505050505050505",
      "nonce": 3,
      "actions": {
        "quota": [
          {
            "name": "transfer",
            "address": "0x0202020202020202020202020202020202020202",
            "params": {
              "from": "0x0101010101010101010101010101010101010101",
              "to": "0x0303030303030303030303030303030303030303",
              "value": "2000000000000000000"
            }
          }
        ]
      }
    }
  ]
}
```

**RCS 响应（202）**：`{ "accepted": ["0xdddd...dddd"], "rejected_malformed": [] }`

**时间线**（`approval_ttl_seconds=30`、`outdated_release_delay_seconds=20`、`reservation_sweep_interval_seconds=5`，均为主文档 §4.2.6/4.3.3 建议默认值）：

| t (unix 秒) | RCS 内部状态（`reserved(RECIPIENT,TOKEN_X)`） | `GET .../query` 对 `TX_D` 的响应 `status`/`decided_at` | 说明 |
|---|---|---|---|
| `1751000000` | `pending`，`reserved=0` | `"pending"` / `decided_at: null` | 刚提交，等待 XLayer Sync 推进水位触发裁决 |
| `1751000002` | `approved`，`reserved=2000000000000000000`（2 枚） | `"approved"` / `decided_at: 1751000002` | 水位推进触发裁决，额度足够，预占 2 枚 |
| `1751000035`（第一个 ≥ `1751000002+30` 的 5 秒扫描 tick） | `outdated`，`reserved` 不变（仍 2 枚，预占不释放） | `"outdated"` / `decided_at: 1751000002` | Sweeper 检测到超过 `approval_ttl_seconds` 仍未完成 → 转 `outdated`；`decided_at` 保持首次裁决时间不变 |
| `1751000055`（第一个 ≥ `1751000035+20` 的 5 秒扫描 tick） | 终态 `outdated`，`reserved` 释放为 `0` | `"outdated"` / `decided_at: 1751000002` | Sweeper 检测到超过 `outdated_release_delay_seconds` 仍未完成 → **释放预占**，额度回补；对外 `status` 字符串**不变**，仍为 `"outdated"`——契约只有一个 `outdated` 状态值，覆盖"预占中的 outdated"与"预占已释放的 outdated"两个阶段，两者在查询响应里不可区分，Filter 也不需要区分（一旦看到 `outdated` 就丢弃，不再关心预占是否已释放） |
| `> 1751000055 + terminal_entry_retention_seconds`（建议 300 秒） | 条目从内存清理 | `TX_D` 不再出现在任何响应里（见 2.5"静默缺席"约定） | 内存清理是 RCS 内部行为，Filter 早已在 `t=1751000035` 观测到 `outdated` 时丢弃了 `TX_D`，不会再查询它 |

Filter 侧行为：在 `t≈1751000035` 首次查询到 `outdated` 时立即丢弃 `TX_D`、不再重试打包（缓冲池状态机 `Approved→Outdated→[*]`）；本场景不涉及 `quota_consistency_hash` 校验（交易从未进入"打包前校验"这一步）。

## 5. 参考

- 主设计文档：`docs/superpowers/specs/2026-07-06-bridge-withdrawal-risk-control-review.md`（§4.4 接口定义、§4.6 规则设计、§4.3 流程时序图 —— 背景与设计动机，非契约本身）
- 领域术语表：`docs/superpowers/CONTEXT.md`
- ADR-0001：`docs/superpowers/adr/0001-erc1155-quota-pooled-by-contract-not-token-id.md`（ERC1155 额度按合约地址汇总）
- ADR-0002：`docs/superpowers/adr/0002-timeout-action-defaults-to-fail-open.md`（`audit_timeout_action` 省略时默认 `allow`）
- ADR-0003：`docs/superpowers/adr/0003-quota-consistency-hash-is-filter-local.md`（`quota_consistency_hash` 是 Filter 本地机制，不出现在本契约的任何报文中）
- JSONLogic 规范：https://jsonlogic.com/
