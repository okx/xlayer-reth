IMPL-blacklist-full-alignment — XLOP-1100 op-reth 侧黑名单功能完整接线与对齐(PRD + op-geth)

# Background

XLOP-1100 要求在 op-geth 与 op-reth 两个出块端各实现一套等价的链层应急冻结黑名单拦截:名单地址的资产转移(原生 ETH、ERC20/721/1155、proxy 多跳、参数型 transferFrom/permit、selfdestruct、L1 forced deposit)在 L2 无法落账;两端对同一区块产出一致 state root + 逐字段一致 receipt,否则分叉。

op-geth 侧已实现完毕(参考 `/Users/oker/meili/.../xlayer/op-geth` 分支 `xl/blacklist_latest`,core/blacklist_*_xlayer.go),是本仓的对齐真相源。详见记忆 [[project_blacklist-alignment-spec]]。

op-reth 侧现状(经 /review):`crates/blacklist` 纯逻辑层质量好且对齐(三检查、翻页、deposit 字段都对、40 UT);但 `crates/blacklist-node` 适配层几乎全部未接进 `bin/node`,运行时整体 no-op:
- 快照恒空(`store_snapshot`/`read_snapshot_at`/`RethMirrorViewCaller` 生产路径零调用)。
- 出块面 hook(context.rs:659-673)传入空 `BlacklistInspector::new()`,check①/③ 输入恒空,只有 check②(log)有逻辑。
- deposit 处置(`apply_included_as_reverted`)生产路径零调用。
- follower newPayload 验证面完全未接(main.rs:183 只 `.payload()`,未 `.executor()`)。
- 入口关(pool validator)已写未接。
- 读名单 gas 被忽略(view.rs:47 丢弃 `_gas`,实际非 50M)。

本 IMPL 的目标:一次性把上述缺口全部接通并与 op-geth 逐字段对齐,不留坑。

参考设计文档:`context-kg/technical/modules/module-blacklist.md`(模块结构 + Status)、`context-kg/technical/pitfalls/upstream-component-type-pinning.md`(为何 follower 面必须改 deps/optimism)、`context-kg/technical/arch/dependency.md`(依赖方向)。

# Rules/Parameters

跨端共识硬约束(任一不一致即分叉,全部以 op-geth 为准)。统一锚点:判定发生在「ApplyMessage/transact 之后、状态最终化(commit/Finalise)之前」,出块与 follower 共用同一判定核心(`crates/blacklist`),仅「命中后处置」按路径与 tx 类型分流。

read(FR-4):ABI `getBlacklist(uint256,uint256)->(uint256,address[])`;PAGE_SIZE=1024;读 gas=50_000_000(显式);MAX_ENTRIES=300000;caller=SystemAddress 0xff..fe;翻页 start=已消费条目数;按槽位截断 n=min(total,MAX_ENTRIES);零地址占槽但不入集;空页/防空转停止;fail-open(call/decode/total 非法/total==0→空+Error);未部署(无 code)→空;读父/块首状态;Difficulty==0 时 prevrandao 用 MixDigest(merge 指令集)。

三检查(FR-2)优先级 call > log > balance,均以「已提交效果」为准:
- check①:帧树,每帧 touched={from,to},committed iff 帧及全部祖先未 revert。
- check②:committed per-tx logs;topic0 ∈ {ERC20/721 Transfer, ERC1155 TransferSingle/Batch};ERC20 topics≥3 查 [1]/[2],ERC1155 topics≥4 查 [2]/[3]。
- check③:候选=名单地址中本 tx 余额发生变化者(含 selfdestruct beneficiary);`net = balEnd - balStart - feeDelta ≠ 0` 即命中。
  - balStart = 本 tx 执行前该地址 committed 余额;balEnd = transact 之后、revert 之前 committed 余额。
  - feeDelta 排除集(穷举,等价 op-geth reason {BalanceIncreaseRewardTransactionFee, BalanceDecreaseGasBuy, BalanceIncreaseGasReturn}):
    - 当名单地址 == 交易 sender:`feeDelta += -(result.gasUsed × effectiveGasPrice)`(L2 执行 gas 的买入−退还净额)。
    - 当名单地址 == coinbase:`feeDelta += +(result.gasUsed × priorityFeePerGas)`,`priorityFeePerGas = effectiveGasPrice - baseFee`。
    - 严禁把 L1 data fee / operator fee / base fee 销毁纳入 feeDelta(op-geth 未纳入;这些流向 vault,vault 不会是名单地址;名单地址作为 sender 时会被 check① 优先命中)。
  - selfdestruct 分类(category=selfdestruct vs eth_balance)仅为 metric 标签,不进 state root/receipt,不影响共识。

deposit included-as-reverted(FR-3),命中后处置序列(逐字段照抄 op-geth `applyTransactionWithBlacklistGate`):
1. 执行前快照 state;deposit+regolith 时先存 `pre_exec_nonce = N = GetNonce(from)`。
2. 命中后回滚到执行前 state 快照。
3. deposit 专属:re-mint(若 `msg.mint` 存在,`AddBalance(from, mint, Mint)`);`SetNonce(from, N+1)`;把未消耗 gas `tx.gasLimit - result.gasUsed` 同时计入 block used 与 cumulative used(等价 op-geth `gp.ChargeUsed`),使 header.GasUsed 与 receipt.CumulativeGasUsed 都反映全额 gasLimit。
4. receipt:`status=0`;deposit 时 `gasUsed = tx.gasLimit`(显式 override,不依赖自然 gas);`logs=∅` + bloom 重算;`DepositNonce = N`(执行前,≠账户 N+1);Canyon 激活时 `DepositReceiptVersion=1`。
豁免发送者(永不拦截):SystemAddress 0xff..fe、L1AttributesDepositor 0xDeaD..0001(evaluate 最前判)。

deposit check① 偏离(跨端契约变更,决策 B,2026-06-12):deposit(L1→L2)的判定只跑 check②(Transfer event)+ check③(ETH balance),去掉 check①(committed CALL 触达)。原因:follower 验证路径的上游 EVM 无法挂载我们的 inspector(类型钉死,见 [[upstream-component-type-pinning]]),拿不到帧树 → 做不了 check①;为保证 sequencer/follower 两端逐字段一致,sequencer 的 deposit 判定也同步去掉 check①(普通 L2 tx 仍三检查全跑,sequencer 出块路径已挂 inspector)。op-geth 必须同步:其 `Evaluate`(blacklist_tracer_xlayer.go)对 deposit 跳过 `committedTouch`。唯一覆盖损失:一笔 deposit 仅"CALL 触达"黑名单地址、但无任何 Transfer event 且无 ETH 余额变动(方案文档 A-7 / B-13 的纯触达子情形)——攻击者注入 deposit 必为转移资产,必触发 ②/③,故无实战风险。验收矩阵 A-7/B-13 口径改为"deposit 有资产变动才保证拦截"。

路径分流(FR-5):
- 出块(sequencer)路径:普通 L2 tx 命中 → 整笔剔除(不 commit、移出 mempool);deposit 命中 → included-as-reverted。
- follower(newPayload 验证/导入 + flashblocks 回放)路径:普通 L2 tx 命中 → 不拦(原样执行,跟随 sequencer);deposit 命中 → included-as-reverted(与出块逐字段一致)。

ingress(FR-1):先查 top-level `to`(不恢复签名),再恢复 `from`(恢复失败不因 from reject);空快照放行;pool reset(commit+reorg)刷新快照。
错误(FR-7):reject 返回 JSON-RPC -32000,message 固定 `xlayer-blacklist: sender or recipient is on the blacklist`,无动态字段;不暴露任何名单查询 RPC。
metrics(FR-7):`xlayer_blacklist_{cache_size, pool_rejected_total, snapshot_read_duration_seconds, exec_revert_total{hook=call|log|selfdestruct|eth_balance}}`。
chain_id 派发(FR-6):仅 195/1952/196 启用,硬编码 mirror 地址(195 devnet=0x73511669fd4dE447feD18BB79bAFeAC93aB7F31f),无 CLI 开关。

# Acceptance Criteria Traceability

| 验收标准(PRD) | 行为规格 | 系统层 | 对应 Step | 状态 |
|---|---|---|---|---|
| AC1 add(0xAAA)落 block N,从 N+1 起 0xAAA 转出/被转入,入口关+执行关均拦截普通 tx 不上链;处理 block N 读父状态快照不含 0xAAA | 块首读父 state 快照(N+1 生效);ingress reject;出块执行关 mark_invalid + 不 commit | builder / pool / view | Step 1,2,5 | 未覆盖→本 IMPL 覆盖 |
| AC2 deposit 经 OptimismPortal 强制注入,出块/follower 均 included-as-reverted(必上链 status=0 gasUsed=gasLimit keep-mint nonce=N+1),逐字段一致 | deposit 处置序列(见 Rules FR-3),出块与 follower 两面调同一 `apply_included_as_reverted` | builder / executor(deps/optimism) | Step 3,4 | 未覆盖→覆盖 |
| AC3 参数型攻击(transferFrom/permit/Permit2/EIP-3009/ERC721/ERC1155/原生 ETH/selfdestruct)三检查均拦截 | check①(挂载 inspector)、check②(已有)、check③(余额重建)在出块面真正生效 | builder | Step 2 | 未覆盖→覆盖 |
| AC4 内层子调用触达 0xAAA 但回滚、整笔成功 → 不拦截(status=1),两端结论一致 | 帧树 ancestor-revert 传播(check① committed_touches)在真实执行下生效 | builder / executor | Step 2,4 | 纯逻辑已测→接线后端到端覆盖 |
| AC5 未识别 chain_id 或名单为空 → 行为等同未引入,TPS 无可感知下降 | `is_enabled`=false 或空快照 → ingress 直通 + 执行关短路 no-op | 全层 | Step 1,2,5 | 部分→补端到端 |
| AC6(FR-2)普通 L2 tx 出块整剔+移出 mempool;follower 原样执行不拦 | 出块 mark_invalid;follower 对普通 tx 不进入处置分支 | builder / executor | Step 2,4 | 未覆盖→覆盖 |
| AC7(FR-3)豁免发送者集永不拦截 | `is_exempt_sender` 在 deposit 判定最前短路 | builder / executor | Step 3,4 | 纯逻辑已测→接线覆盖 |
| AC8(FR-4)超 MAX_ENTRIES 确定性截断同一前缀;未部署→空 no-op | 翻页截断(已实现);未部署短路(需 follower/builder 读路径接通) | view / snapshot | Step 1 | 部分→补量级断言 |
| AC9(FR-7)Pool 拒绝 -32000+固定 message;不暴露 isBlocked/blacklist_* RPC;四项 metrics 真实更新 | ingress reject 映射;无新增 RPC(负向);store/record 被真实调用 | pool / runtime | Step 1,5,6 | 部分→接线后真实更新 |

# Change Scope

## Step 1: 块首快照填充(FR-4)+ 读名单 gas 对齐 50M

让 `BlacklistRuntimeCtx` 的快照在每块块首从父 state 真实读出,并修正读 gas。这是所有后续检查生效的前提(当前快照恒空导致整体 no-op)。

### Change List

| 文件 | 类型 | 描述 |
|---|---|---|
| crates/blacklist-node/src/view.rs | 修改 | `RethMirrorViewCaller::static_call` 当前丢弃 `_gas` 走 `transact_system_call`(gas 固定 30M)。改为照搬本仓既有只读模拟范式(builder_tx.rs:233-237/306-312):把读 EVM 收窄为 OpEvm-concrete,`modify_cfg(disable_balance_check=true; disable_block_gas_limit=true)`,构造 `OpTransactionRequest{ from=SYSTEM_ADDRESS, to=mirror, value=0, gas_limit=PER_PAGE_GAS(50M), input=calldata }` 走 `transact_raw`,取 output 后丢弃 state(只读)。与 op-geth `evm.StaticCall(SystemAddress, mirror, input, 50M)` 行为等价(决策 B:保证与 op-geth 同为 50M;非 Success → `CallFailed` fail-open)。 |
| crates/blacklist-node/src/view.rs / runtime.rs | 修改 | 构造读名单只读 EVM 时,EVM env 必须按 merge 规则设 prevrandao(区块 Difficulty==0 时用 MixDigest)。否则现代 mirror 字节码(PUSH0 等)触发 invalid opcode → staticcall revert → 静默 fail-open 空表(对齐 op-geth blacklist_xlayer.go:268-279 的 CRITICAL 注释)。 |
| crates/blacklist-node/src/runtime.rs | 修改 | 新增 `refresh_snapshot_from_evm(&self, evm: &mut impl Evm)`:`read_snapshot_at(&mut RethMirrorViewCaller::new(evm), mirror)` → `record_snapshot_read(耗时)` → `store_snapshot(snap)`;`mirror` 为 `None`(未启用)时直接 `store_snapshot(empty)` no-op。 |
| crates/builder/src/flashblocks/context.rs | 修改 | 在 `execute_sequencer_transactions`(:270)与 `execute_best_transactions`(tx 循环之前)各插入一次:在块首、任何 tx 执行/commit 之前,用 **parent header 对应的独立只读 state(`new_simulation_state` 模式)** 构造 EVM 调 `bl_ctx.refresh_snapshot_from_evm(...)`。严禁复用正在出块的 building EVM 的 DB(可能含本块前序 flashblock 已 commit 改动 → 读到块内 delta → 跨端分叉,见 module-blacklist Remaining 明令)。每块一次,循环内复用快照。 |
| crates/builder/src/flashblocks/builder.rs | 确认 | builder 路径已透传 `blacklist_ctx`(:317);确认 sequencer 出块路径拿到非 None ctx。 |

### Step 1 UT

| # | 测试名 | 输入 | 期望输出 | 断言方式 |
|---|---|---|---|---|
| 1 | view_static_call_passes_50m_gas | fake EVM 记录传入 gas | 记录值 == 50_000_000 | 精确值 |
| 2 | view_static_call_non_success_is_fail_open | EVM 返回 Revert/Halt | `Err(CallFailed)` | 精确值 |
| 3 | refresh_disabled_chain_stores_empty | ctx(chain=10 未启用) | `load_snapshot().is_empty()` | 精确值 |
| 4 | refresh_populates_from_mirror | fake EVM 返回 2 地址 | `load_snapshot().len()==2` 且 cache_size gauge==2 | 精确值 |

### Step 1 IT

| # | 测试名 | 场景 | 验证点 | 断言方式 |
|---|---|---|---|---|
| 1 | it_snapshot_n_plus_one_effective | mirror 在 block N add(0xAAA);执行 N 与 N+1 | 处理 N 快照不含 0xAAA;处理 N+1 含 0xAAA | 精确值 |
| 2 | it_undeployed_mirror_noop | mirror 地址无 code | 快照空、行为等同未引入 | 精确值 |
| 3 | it_modern_bytecode_mirror_enumerated | 部署含 PUSH0 的现代字节码 mirror,Difficulty==0 块 | 成功枚举出名单(prevrandao=MixDigest 生效),非 fail-open 空 | 精确值 |

### Step 1 Testability Design(涉及 EVM staticcall I/O)

| I/O 类型 | 当前耦合 | Mock 策略 | 接口位置 |
|---|---|---|---|
| mirror staticcall | `RethMirrorViewCaller` 直接持 revm EVM | 纯逻辑 `read_snapshot_at` 已经过 `MirrorViewCaller` trait 解耦(`crates/blacklist/src/snapshot.rs`,FakeMirror 已存在);view.rs 的 gas 传递用 fake `Evm` 或记录型替身验证 | `MirrorViewCaller`(已存在) |

## Step 2: 出块面三检查全生效(FR-2,check①/③ + 余额重建)

当前出块 hook 用空 inspector,只有 check② 生效。挂载真实 revm inspector + 重建 check③ 余额候选,使 call/balance 在出块面真正命中。

### Change List

| 文件 | 类型 | 描述 |
|---|---|---|
| crates/blacklist/src/snapshot.rs | 修改 | `BlacklistSnapshot` 当前只暴露 contains/len/is_empty,无迭代器。新增 `iter()`,供 check③ 候选发现枚举名单地址(否则无法采 pre_balance)。 |
| crates/blacklist-node/src/balance.rs | 新增 | `reconstruct_balance_candidates(snapshot, state: &EvmState, pre_balances, sender, coinbase, gas_used, effective_gas_price, base_fee, selfdestruct_addrs) -> Vec<BalanceCandidate>`:候选集 = **本 tx state diff ∩ 名单**(先收窄,避免每 tx 遍历整张名单——MAX_ENTRIES=30w 时全集预读会拖垮 TPS,违反 AC5;diff∩snapshot 等价于 op-geth「OnBalanceChange 观测集 ∩ snapshot」且完备);对候选 balStart 取 pre_balance、balEnd 取 diff 后余额;feeDelta 按 Rules FR-3 重建(sender/coinbase 两支);`selfdestruct` 标志来自 inspector selfdestruct 观测集。纯函数可单测。 |
| crates/blacklist-node/src/balance.rs | 说明 | pre_balance 采集:对「本 tx state diff ∩ 名单」这一小集合,从 transact 前的 DB(committed 父态)回查 balStart(revm `Account.original_info` 不可靠,不能用)。collection 范围 = diff∩snapshot,非整张名单。 |
| crates/blacklist-node/src/inspector.rs | 修改 | 新增 `selfdestruct_targets(&self) -> &[Address]` 暴露 selfdestruct 观测地址供 balance 重建打 category;`reset(&mut self)` 供出块循环每 tx 复位(evm 循环外建一次时无法每 tx move 出 inspector)。 |
| crates/builder/src/flashblocks/context.rs | 修改 | `execute_best_transactions`:建 evm 改为 `evm_with_env_and_inspector(db, env, XLayerRevmInspector::new())`(替换 :501 一带的 `evm_with_env`,仅出块路径);每笔 tx 前 `inspector.reset()`,transact 后用 `inspector.observations()` + `reconstruct_balance_candidates(...)` 组装完整 `BlacklistInspector`,替换 :662 的空 inspector 传入 `evaluate`。 |
| crates/builder/src/flashblocks/context.rs | 确认 | 若出块/follower 路径原本可能挂其他 inspector(debug/trace),用 `evm_with_env_and_inspector` 直接替换会丢失既有 tracing(op-geth 为此专测 CombineHooks)。确认本仓出块面是否单一 inspector;若存在并存需求,改为合并而非替换,并补「合并不丢」UT;若单一,显式记「无 CombineHooks 等价需求」。 |
| crates/builder/src/flashblocks/context.rs | 修改 | 删除/订正 :653-658 注释中"只 result.logs()"措辞,改为三检查全活。 |

### Step 2 UT

| # | 测试名 | 输入 | 期望输出 | 断言方式 |
|---|---|---|---|---|
| 1 | reconstruct_sender_fee_stripped | 名单地址=sender,净变化==gas 费 | 该候选 net==0(不命中) | 精确值 |
| 2 | reconstruct_coinbase_reward_stripped | 名单地址=coinbase,净==priorityFee 奖励 | net==0 | 精确值 |
| 3 | reconstruct_value_transfer_hits | 名单地址收到 value 转账(非费用) | 候选 net≠0、selfdestruct=false | 精确值 |
| 4 | reconstruct_l1fee_not_stripped | 名单地址=sender 且仅 L1 fee 变化 | L1 fee 不计入 feeDelta(net≠0) | 精确值 |
| 5 | reconstruct_selfdestruct_flag | selfdestruct 观测集含该地址 | 候选 selfdestruct=true | 精确值 |
| 6 | reconstruct_non_listed_skipped | 变化地址不在名单 | 不产生候选 | 精确值 |
| 7 | reconstruct_sender_plus_value_still_hits | 名单地址=sender 且同时收到 value 转入 | feeDelta 只扣 gas 费、保留 value 净额,net≠0 命中 | 精确值 |
| 8 | reconstruct_coinbase_plus_value_still_hits | 名单地址=coinbase 且额外收到非 reward 转入 | 只扣 reward,net≠0 命中 | 精确值 |
| 9 | reconstruct_sstore_refund_fee_exact | 名单=sender,带 SSTORE 退款(effectiveGasPrice×gasUsed 含退款影响) | feeDelta 与逐 wei 期望一致、net==0 | 精确值 |

### Step 2 IT

| # | 测试名 | 场景 | 验证点 | 断言方式 |
|---|---|---|---|---|
| 1 | it_build_call_hit_dropped | proxy 多跳 committed 触达 0xAAA | tx 被 mark_invalid、不在 block、exec_revert{hook=call}+1 | 精确值 |
| 2 | it_build_balance_hit_dropped | 原生 ETH 转给 0xAAA | tx 被剔除、exec_revert{hook=eth_balance}+1 | 精确值 |
| 3 | it_build_log_hit_metric | committed Transfer event 命中 0xAAA | exec_revert{hook=log}+1 | 精确值 |
| 4 | it_build_selfdestruct_hit_metric | selfdestruct beneficiary=0xAAA | exec_revert{hook=selfdestruct}+1 | 精确值 |
| 5 | it_build_inner_revert_not_hit | 内层触达 0xAAA 但子调用回滚、整笔成功 | 不剔除、status=1 | 精确值 |

> 验证关口(架构决策②前置):Step 2 的 UT7/8/9 是「公式重建 feeDelta == op-geth reason 集」的逐 wei 等价验证关口。这些通过前,check③ 不得视为与 op-geth 等价上线。条件允许时用 op-geth 共享对抗向量(sender=名单含退款、coinbase=名单、fee+真实转账叠加)逐 wei 比对 op-reth 与 op-geth 的 net 值。

## Step 3: 出块面 deposit included-as-reverted(FR-3)

在 sequencer deposit 执行循环里接入命中判定与处置。当前 `execute_sequencer_transactions` 完全无拦截。

### Change List

| 文件 | 类型 | 描述 |
|---|---|---|
| crates/blacklist-node/src/deposit_apply.rs | 新增 | `apply_reverted_deposit_to_state(state diff, &RevertedDepositOutcome, msg_mint, sender)`:对一笔命中 deposit,产出「回滚后保留 mint + nonce N+1」的最终 state 写入指令,供 builder 与 follower 两处复用。封装 op-geth 的 revert→re-mint→SetNonce 序列为可复用单元。 |
| crates/builder/src/flashblocks/context.rs | 修改 | `execute_sequencer_transactions`:在 :306 `evm.transact` 之后、:336 `build_receipt`/:339 `commit` 之前插入:若 `blacklist_ctx` 启用且 `sequencer_tx.is_deposit()` 且非豁免发送者,用 inspector 观测 + 余额重建 `evaluate`;命中则:(a) 用执行前快照回滚本 tx 的 state(`evm.transact` 返回的 `state` 不 commit,改写为回滚态),(b) 调 `apply_included_as_reverted(depositor_nonce, tx.gas_limit, mint, canyon_active)` 取 overrides,(c) `apply_reverted_deposit_to_state` 保留 mint + 设 nonce N+1,(d) gas:把 `tx.gas_limit - result.gas_used` 计入 cumulative(全额 gasLimit),(e) receipt 用 override 重建(status=0、gasUsed=gasLimit、DepositNonce=N、Canyon version、logs=∅、bloom 重算)。 |
| crates/builder/src/flashblocks/context.rs | 修改 | deposit 循环同样需挂 inspector(同 Step 2),并在执行前缓存 depositor_nonce(:293 已有,复用为 pre_exec_nonce=N)。 |

### Step 3 UT

| # | 测试名 | 输入 | 期望输出 | 断言方式 |
|---|---|---|---|---|
| 1 | deposit_apply_keeps_mint | outcome.keep_mint=Some(m) | state 写入含 +m 到 sender | 精确值 |
| 2 | deposit_apply_sets_nonce_n_plus_1 | pre_nonce=7 | sender account nonce==8 | 精确值 |
| 3 | deposit_apply_no_mint_when_none | keep_mint=None | 不加 balance | 精确值 |

### Step 3 IT

| # | 测试名 | 场景 | 验证点 | 断言方式 |
|---|---|---|---|---|
| 1 | it_build_deposit_reverted | 出块路径打包命中 deposit | receipt status=0、gasUsed=gasLimit、DepositNonce=N、账户 nonce=N+1、mint 保留、logs 空、**DepositReceiptVersion(Canyon 激活=Some(1))**;header.GasUsed 含全额 gasLimit | 精确值 |
| 2 | it_build_deposit_reverted_pre_canyon | Canyon 未激活的命中 deposit | DepositReceiptVersion==None,其余同上 | 精确值 |
| 3 | it_build_exempt_deposit_not_gated | from∈豁免集 | status=1 正常上链 | 精确值 |
| 4 | it_build_deposit_cumulative_across_txs | 命中 deposit 后紧跟一笔正常 tx | 后续 tx 的 receipt.CumulativeGasUsed == 命中 deposit 全额 gasLimit + 本笔 gasUsed(对齐 op-geth C-1,receipts root 一致) | 精确值 |
| 5 | it_build_deposit_extra_zero_no_double_count | gasLimit 恰等于自然消耗(extra==0) | cumulative 不双计、gasUsed==gasLimit | 精确值 |

> 注:UT4/5 对应 op-geth `blacklist_deposit_xlayer_test.go` 的 4 个 C-1 回归(CumulativeGasUsed / CumulativeAcrossTxs / ExtraZero / ImportPath),直接关系 receipts root 跨端一致,Step 4 的 follower 面亦需对等 IT(见 Step 4 IT)。

## Step 4: follower 面接线 + deps/optimism deposit 钩子(FR-3/FR-5)

follower 的 newPayload 验证与 flashblocks 回放路径走上游 `OpBlockExecutor`,无 pre-commit 钩子(见 pitfall upstream-component-type-pinning)。在 deps/optimism 的执行器加 deposit 钩子,并经上游 `OpEvmConfig` 的 optional ctx 字段(不包装、不改对外类型)把共享 ctx 注入 follower 执行面。**不**走 `.executor()`/wrapper-EvmConfig(pitfall 证明编译不过,见架构决策 C)。

### Change List

| 文件 | 类型 | 描述 |
|---|---|---|
| deps/optimism/rust/alloy-op-evm/src/block/mod.rs | 修改 | 在 `OpBlockExecutor` 加一个可选的 per-tx deposit 钩子字段(默认 None=no-op,不改任何现有行为)。在 `commit_transaction`(~:315 build_receipt / ~:351 commit 之间)对 deposit 调用钩子:钩子可返回「included-as-reverted 指令」(回滚本 tx state、保留 mint、nonce N+1、receipt status=0/gasUsed=gasLimit/DepositNonce=N、logs 清空、全额 gasLimit 记账)。钩子 trait 定义放 alloy-op-evm,实现在本仓。这是唯一的上游改动。 |
| crates/blacklist-node/src/executor.rs | 新增 | 实现该 deposit 钩子:持 `BlacklistRuntimeCtx` + per-tx inspector;follower 路径只处置 deposit(普通 L2 tx 不拦,FR-5),命中判定调用 `crates/blacklist` 同一 `evaluate` + `apply_included_as_reverted` + `apply_reverted_deposit_to_state`(与 Step 3 复用)。 |
| crates/blacklist-node/src/evm_config.rs | 修改 | `XLayerBlacklistEvmConfig` 的 `block_executor_factory`/executor 构造改为:把 deposit 钩子注入上游 executor(经 Step 4 的上游钩子字段),并在块首读快照存入 ctx(follower 路径快照填充,等价 Step 1)。订正 :15-20 失实注释。 |
| deps/optimism .../op-reth crates/node(OpEvmConfig / OpExecutorBuilder 构造) | 修改 | 给上游 `OpEvmConfig`(或其 executor factory)加一个 optional `blacklist_ctx: Option<BlacklistRuntimeCtx>` 字段,默认 None=no-op。executor 构造时把该 ctx 透传给 OpBlockExecutor 的 deposit 钩子。关键:不改 `OpEvmConfig` / `OpAddOns` 的对外关联类型(只加私有字段),避免触发 pitfall 的 type-pinning(见架构决策)。 |
| bin/node/src/main.rs | 修改 | follower/flashblocks 用的 `OpEvmConfig::optimism(...)` 经新增的 `.with_blacklist_ctx(bl_ctx.clone())`(上游字段 setter)携带共享 ctx;`FlashblockSequenceValidator::new`(:205)仍传 `OpEvmConfig`(类型不变),但该 config 已携 ctx。**不**新增 `.executor()`/wrapper-EvmConfig(撤销原 wrapper 路线)。 |
| crates/blacklist-node/src/{evm_config,executor_builder}.rs | 删除/废弃 | `XLayerBlacklistEvmConfig` / `XLayerExecutorBuilder` 这两个 wrapper 类型撤销(pitfall 证明经 `.executor()`/`FlashblockSequenceValidator::new` 接 wrapper 编译不过)。follower 面改由上游字段注入 + OpBlockExecutor 钩子达成。 |
| crates/builder/src/flashblocks/handler_ctx.rs | 修改 | :93 `blacklist_ctx: None` 改为传入共享 ctx,使 flashblocks follower 回放路径也走 deposit 处置(普通 tx 仍不拦)。 |
| ctx 所有权 | 说明 | main.rs:157 `with_blacklist_ctx(bl_ctx)` 当前 move 进 payload builder;改为先 `bl_ctx.clone()` 分发给 builder/follower/ingress 三面(共享同一 `SnapshotHandle`),move 在最后一处。 |

### Step 4 UT

| # | 测试名 | 输入 | 期望输出 | 断言方式 |
|---|---|---|---|---|
| 1 | follower_hook_normal_tx_not_gated | 普通 L2 tx 命中 | 钩子不处置(原样) | 精确值 |
| 2 | follower_hook_deposit_reverted | deposit 命中 | 返回 included-as-reverted 指令 | 精确值 |
| 3 | follower_hook_exempt_not_gated | from∈豁免集 deposit | 不处置 | 精确值 |
| 4 | follower_hook_none_is_noop | 钩子=None | 上游行为不变 | 精确值 |

### Step 4 IT

| # | 测试名 | 场景 | 验证点 | 断言方式 |
|---|---|---|---|---|
| 1 | it_follower_deposit_matches_builder | 同一命中 deposit 分别走出块与 follower | 两端 receipt 逐字段一致(含 status/gasUsed/DepositNonce/account nonce/mint/logs/**DepositReceiptVersion**)、state root 一致 | 精确值 |
| 2 | it_follower_normal_tx_followed | 已上链普通 L2 tx 在 follower 验证 | 不拦、按区块原样执行 | 精确值 |
| 3 | it_flashblocks_replay_deposit_reverted | flashblocks 回放命中 deposit | included-as-reverted 一致 | 精确值 |
| 4 | it_follower_deposit_cumulative_across_txs | follower 端命中 deposit 后跟普通 tx | 后续 tx CumulativeGasUsed 按全额 gasLimit 推进(对齐 op-geth ImportPath C-1) | 精确值 |

### Step 4 Testability Design(涉及上游 executor I/O)

| I/O 类型 | 当前耦合 | Mock 策略 | 接口位置 |
|---|---|---|---|
| OpBlockExecutor commit | 上游 struct 无缝 | 新增的 deposit 钩子 trait 即 mock 接口;UT 用内存 state + 钩子 trait 替身,IT 用真实 executor | alloy-op-evm deposit 钩子 trait + `crates/blacklist-node/src/executor.rs` |

## Step 5: 入口关接线(FR-1)+ pool reset 刷新

入口关是「尽力过滤」层(PRD FR-1:快照滞后不构成安全缺口,执行关兜底),不是安全关。因此不得用 wrapper validator(pitfall:包装 `OpTransactionValidator` 改变 `N::Pool` → break add-ons/RPC 栈)。改为在上游 submodule 给 pool/validator 加 optional 黑名单 ctx 字段(与 deposit 钩子同一受控例外口径,类型不变),或经上游已有的 ingress 过滤点注入。

### Change List

| 文件 | 类型 | 描述 |
|---|---|---|
| deps/optimism .../op-reth crates/txpool(OpTransactionValidator / OpPoolBuilder) | 修改 | 加 optional `blacklist_ctx: Option<BlacklistRuntimeCtx>` 字段,默认 None=no-op;validator 内部在准入时做 top-level from/to 检查。不改对外关联类型(`N::Pool` 不变),避免 pitfall。撤销 `XLayerBlacklistPoolBuilder`/`XLayerBlacklistTxValidator` wrapper 路线。 |
| crates/blacklist-node/src/validator.rs | 修改 | 拆出可被上游字段复用的纯准入判定函数(空快照放行;先查 top-level `to` 命中 reject;再恢复 `from`,失败不因 from reject;reject 记 `record_pool_reject` + 返回 `ErrBlacklisted` -32000 固定串),供上游 OpTransactionValidator 调用。 |
| 快照刷新 | 修改 | 不另起 reset 回调。ingress 与执行关共享同一 `SnapshotHandle`(三面共用 ctx):builder/follower 执行面每块块首刷新(Step 1)即让 ingress 受益;reorg 由 follower executor 路径的块首刷新覆盖。复用同一 handle,避免第二套刷新机制(对齐 M2)。 |

### Step 5 UT

| # | 测试名 | 输入 | 期望输出 | 断言方式 |
|---|---|---|---|---|
| 1 | validate_empty_snapshot_passes | 空快照 + 任意 tx | 放行 | 精确值 |
| 2 | validate_to_hit_rejects | to=0xAAA 在名单 | reject + pool_rejected+1 | 精确值 |
| 3 | validate_from_hit_rejects | from=0xAAA 在名单 | reject | 精确值 |
| 4 | validate_from_recovery_fail_not_rejected_on_from | 签名无法恢复 | 不因 from reject(交后续 validation) | 精确值 |
| 5 | validate_reject_error_is_minus_32000_fixed_msg | reject | error 码 -32000、message 固定串、无动态字段 | 精确值 |

### Step 5 IT

| # | 测试名 | 场景 | 验证点 | 断言方式 |
|---|---|---|---|---|
| 1 | it_ingress_rejects_via_rpc_and_p2p | from/to=0xAAA tx 经 RPC 与 P2P 提交 | 两入口均不入池;RPC 返回 -32000 | 精确值 |
| 2 | it_ingress_reorg_refresh | reorg 后 | 快照按新链块首重建,无旧链残留 | 精确值 |

## Step 6: 可观测性/错误/文档同步与注释订正

### Change List

| 文件 | 类型 | 描述 |
|---|---|---|
| crates/blacklist-node/src/evm_config.rs | 修改 | 订正 :15-20 关于 deposit 拦截"is applied at context.rs:303"的失实注释,改述为已接线的真实位置(Step 3/4)。 |
| context-kg/technical/modules/module-blacklist.md | 修改 | 更新 Status:从"partially shipped/未接线"改为"已接线生效";更新「Remaining」清单。 |
| context-kg/technical/arch/dependency.md | 修改 | 登记 `xlayer_blacklist` / `xlayer_blacklist_node` 两个新 crate 及 `bin/node → blacklist-node`、`builder → blacklist-node` 边;登记 deps/optimism 的 deposit 钩子依赖。 |
| crates/blacklist/src/snapshot.rs + deposit.rs | 修改 | `SYSTEM_ADDRESS` 当前在两文件各定义一次,合并为单一定义并 re-export(单一真相源)。 |

### Step 6 UT

| # | 测试名 | 输入 | 期望输出 | 断言方式 |
|---|---|---|---|---|
| 1 | metrics_four_names_exist | 注册表 | 四项 `xlayer_blacklist_*` 指标存在且命名精确 | 精确值 |
| 2 | no_blacklist_query_rpc | RPC 模块表 | 无 isBlocked/blacklist_* 方法 | 精确值(负向) |
| 3 | metrics_unknown_category_noop | `increment_exec_revert` 传未知/越界 category | 不 panic、安全忽略(对齐 op-geth metrics_test) | 精确值 |
| 4 | metrics_all_four_hook_labels_increment | 分别命中 call/log/eth_balance/selfdestruct | 对应 `exec_revert_total{hook=…}` 各 +1(log/selfdestruct 两 label 也覆盖) | 精确值 |

> AC8 槽位截断:`crates/blacklist/src/snapshot.rs` 已有 over-cap UT,但需补强到 op-geth `TestAccumulatePage_SlotTruncation` 同等强度——断言「零地址占槽不入集 + 按槽位(非 post-skip set size)在同一 index 截断」,而非仅测 30w 规模。若现有 UT 已含槽位语义则标注复用。

## Step 7:(已移出范围)共享对抗向量 / 跨端一致性 E2E

PRD 原 Blocking 项「共享对抗测试向量」已存在(E2E/向量已就绪),本 IMPL 不重复建立,作为既有依赖使用:Step 1–5 的实现完成后,直接复用现有共享向量 / e2e harness 验证 op-reth 与 op-geth 的跨端逐字段一致。各 Step 自带的 IT(it_build_*、it_follower_deposit_matches_builder 等)覆盖本仓内的路径正确性;跨端一致由既有 E2E 兜底。

# Impact Analysis

## Dependency Direction Verification

| 新增项 | 目标模块 | 检查的禁止规则 | 违反 |
|---|---|---|---|
| `bin/node → blacklist-node`(.executor/.pool) | blacklist-node | 规则1 二进制依赖 crate,方向正确 | 否 |
| `builder → blacklist-node`(已存在 blacklist_ctx) | blacklist-node | builder 依赖适配 crate,无环 | 否 |
| `blacklist-node → blacklist`(纯逻辑) | blacklist | 适配层依赖纯逻辑层 | 否 |
| `blacklist-node → deps/optimism 上游组件 optional ctx 字段`(OpBlockExecutor deposit 钩子 + OpEvmConfig/OpTransactionValidator 的 optional blacklist_ctx) | alloy-op-evm / op-reth node·txpool | 上游扩展点;follower+ingress 无 no-fork 退路(pitfall 记录 wrapper 编译不过) | 否(受控例外,默认 None=no-op、不改对外类型,见架构决策 C) |
| `crates/blacklist-node/src/{balance,deposit_apply,executor}.rs` 新文件 | blacklist-node 内部 | 同 crate 内,无跨界 | 否 |

### Architecture Decisions

| 决策项 | 候选 | 选择 | 依据 | 排除理由 |
|---|---|---|---|---|
| follower deposit + ingress 注入方式 | A 自建 OpAddOns/executor 栈 / B 在 xlayer-reth 包装节点组件(.executor/.pool/wrapper-EvmConfig) / C 在 deps/optimism submodule 给上游组件(OpBlockExecutor/OpEvmConfig/OpTransactionValidator)加 optional ctx 字段,类型不变 | C | pitfall upstream-component-type-pinning 用编译器报错证明 B 必然失败(wrapper 改变 N::Pool/EvmConfig 关联类型 → break add-ons/RPC/engine-validator 栈);C 只加私有字段不改对外类型,默认 None=no-op,与 deposit 钩子同一受控例外口径 | B 编译不过(Critical);A 重写共识级组件,维护与分叉风险过高 |
| check③ 余额观测 | A fork revm 加 on_balance_change 钩子 / B 从 ResultAndState state diff + 重建 feeDelta(候选集收窄为 diff∩snapshot) | B + 强制验证关口 | revm Inspector 无余额钩子;`balEnd-balStart-feeDelta` 三量可从 diff + gas 账重建。但「公式重建 == op-geth reason 集」是断言,EIP-1559/SSTORE 退款/OP 各 vault 下可能逐 wei 偏差 → 必须在 Step 2 加逐 wei 对抗向量验证关口(sender=名单含退款、coinbase=名单、fee+真实转账叠加)后才采信 | A 多背一个 revm fork;B 需附验证关口,验证通过前不得视为等价 |
| 读名单 50M gas | A fork alloy-evm 改 transact_system_call 签名 / B 本仓自拼带 gas 的 staticcall | B | alloy-evm `transact_system_call` 无 gas 入参且 revm 系统调用 gas 固定;改用显式 gas 的普通 staticcall 可在本仓达成 | A 多背一个 alloy-evm fork |

注(Critical/Major 修正):原 Step 4/5 的 `.executor()`/`.pool()`/`XLayerBlacklistEvmConfig` wrapper 路线已撤销(与上行决策 C 冲突,编译不过);三个执行面统一为:出块面=builder-field threading(已有模式),follower+ingress=deps/optimism 上游组件 optional ctx 字段。这使「受控 fork」的 submodule 改动面从「一个 no-op 钩子」扩展为「OpBlockExecutor deposit 钩子 + OpEvmConfig/validator 的 optional ctx 字段」,仍是单一 fork(deps/optimism)、默认 no-op、不改对外类型。

## Documentation Reverse Tracing

- `module-blacklist.md` Status 段、Remaining 段描述"未接线" → Step 6 更新为已接线。
- `evm_config.rs` 文件头注释 deposit 拦截位置失实 → Step 6 订正。
- `upstream-component-type-pinning.md` 规则"do not wrap upstream components" → 本 IMPL 严格遵循:follower+ingress 不包装节点组件,改为在 submodule 给上游组件加 optional ctx 字段(不改对外类型);Step 6 在该 pitfall 追加一条说明(字段注入 vs wrapper 方式,后者编译不过)。

## Three-Layer Consistency Verification

| 行为 | 代码实现 | 设计文档描述 | checklist 覆盖 |
|---|---|---|---|
| 块首快照填充 | Step 1(N) | module-blacklist Remaining→更新(Step 6) | 无 checklist 体系,IT 覆盖 |
| 出块面三检查 | Step 2(N) | 同上 | IT 覆盖 |
| deposit included-as-reverted(出块+follower) | Step 3/4(N) | 同上 | 共享向量 Step 7 |
| ingress 接线 | Step 5(N) | 同上 | IT 覆盖 |

注:本仓无 `context-kg/quality/checklist.md` 体系,一致性以「代码↔设计文档↔共享对抗向量」三方对齐替代。

## Naming Consistency Scan

- `SYSTEM_ADDRESS`:snapshot.rs 与 deposit.rs 重复定义同值 → Step 6 合并单一定义。
- metric 名 `xlayer_blacklist_*`:已统一在 `metrics.rs` 常量,确认无散落硬编码。
- 错误串 `xlayer-blacklist: sender or recipient is on the blacklist`:与 op-geth `ErrBlacklisted` 逐字一致,确认 error.rs 一致。

# Testing Requirements

- UT 放 `crates/{blacklist,blacklist-node}/src/*.rs` 的 `#[cfg(test)] mod tests`(纯逻辑/重建函数);跨路径/端到端 IT 放 `crates/tests/{operations,flashblocks-tests,e2e-tests}`。
- 断言用精确值(`assert_eq!`),不用 is_ok/is_err 模糊断言。
- 新增纯函数(balance 重建、deposit_apply)100% UT;接线/执行面经 IT 覆盖。
- 测试基于功能规格(op-geth 行为/PRD AC)写断言,不基于实现。
- 跑 cargo test 链接失败先 export TMPDIR 到仓库内目录([[project_cargo-test-tmpdir]])。

# Coverage Check

| 模块 | 新增/修改函数 | UT 覆盖 | IT 覆盖 | 目标 |
|---|---|---|---|---|
| blacklist-node/balance.rs | reconstruct_balance_candidates() | 6 UT | — | 100% |
| blacklist-node/deposit_apply.rs | apply_reverted_deposit_to_state() | 3 UT | — | 100% |
| blacklist-node/view.rs | static_call() 50M gas | 2 UT | — | 100% |
| blacklist-node/runtime.rs | refresh_snapshot_from_evm() | 2 UT | IT-Step1 | 100% |
| blacklist-node/validator.rs | validate_transaction() | 5 UT | IT-Step5 | 100% |
| blacklist-node/executor.rs | follower deposit 钩子 | 4 UT | IT-Step4 | 100% |
| builder/flashblocks/context.rs | 出块面三检查 + deposit 接入 | — | IT-Step2/3 | IT 覆盖 |
| deps/optimism OpBlockExecutor | deposit 钩子(默认 no-op) | — | IT-Step4 | IT 覆盖 |

# AC Verification Test Matrix

| 验收标准 | 行为规格 | 验证测试名 | 所在 Step | 类型 | 断言 |
|---|---|---|---|---|---|
| AC1 | 块首读父 state(N+1 生效)+ ingress/exec 拦截 | it_snapshot_n_plus_one_effective + it_ingress_rejects_via_rpc_and_p2p | 1,5 | IT | 快照成员精确 + tx 不上链 |
| AC2 | deposit 两路径逐字段一致 | it_follower_deposit_matches_builder | 4 | IT | receipt 逐字段==、state root== |
| AC3 | 三检查命中参数型攻击 | it_build_call_hit_dropped + it_build_balance_hit_dropped | 2 | IT | tx 剔除 + hook 标签 |
| AC4 | 内层回滚不命中 status=1 | it_build_inner_revert_not_hit | 2 | IT | status==1 不拦 |
| AC5 | 未识别链/空名单 no-op | it_undeployed_mirror_noop | 1 | IT | 行为等同未引入 |
| AC6 | 出块剔除 / follower 原样 | it_build_call_hit_dropped + it_follower_normal_tx_followed | 2,4 | IT | 出块 mark_invalid / follower 不拦 |
| AC7 | 豁免发送者永不拦截 | it_build_exempt_deposit_not_gated + follower_hook_exempt_not_gated | 3,4 | IT/UT | status==1 |
| AC8 | 超 MAX_ENTRIES 截断 / 未部署空 | over_cap_truncates_to_max_entries(补 30w 量级)+ it_undeployed_mirror_noop | 1 | UT/IT | 同一前缀 / 空 |
| AC9 | -32000 固定串 + 无查询 RPC + 四指标 | validate_reject_error_is_minus_32000_fixed_msg + no_blacklist_query_rpc + metrics_four_names_exist | 5,6 | UT | 精确值 |

# Out of Scope

- 合约仓 `L2BlacklistMirror` 实现与 1952/196 真实地址 —— 由外部 contracts 仓负责;本仓只依赖 `getBlacklist` ABI 与硬编码地址,占位地址不影响 op-reth 代码/测试一致性(devnet 195 地址已定可端到端测)。
- op-geth 侧实现 —— 已完成,是对齐基准而非本仓改动;不做不会引入本仓内部不一致。
- L1 admin Registry 经 OptimismPortal 下发名单 —— PRD 本期 Not doing;本期名单仅 L2 直接 add,与本仓拦截逻辑无依赖。
- fault proof(op-program)一致性 —— PRD 本期不处理;不在 op-reth 执行端范围,不影响 sequencer/follower 一致性。
- revm / alloy-evm fork —— 经架构决策以 no-fork 重建方案替代(check③ 余额重建 + 显式 gas staticcall);不做不会造成与 op-geth 的共识不一致(重建与 op-geth reason 集等价)。
