# XLayer Native AA (EIP-8130) — 开发汇报

> 2026-04-17 | 分支: `feat/native-aa` | EL 侧（xlayer-reth）

---

## 一、背景

EIP-8130 是以太坊提出的**协议层原生账户抽象**方案，Base 已在其 OP-Stack L2 上实现。我们将该方案移植到 XLayer Reth，取代外部 ERC-4337 bundler 方案。

**核心能力**：
- 新交易类型 `0x7B`，协议层原生支持
- 2D Nonce 模型（多通道并行交易）
- 分阶段原子调用
- Gas 代付（独立 payer 赞助）
- 可插拔验证器（K1/P256/WebAuthn/Delegate/自定义）
- 协议层账户创建（CREATE2）和配置变更

**为什么做**：ERC-4337 依赖链外 bundler，存在审查风险、gas 效率低、UX 差（如交易确认延迟）。EIP-8130 将 AA 下沉到协议层，与普通交易同等对待。

---

## 二、架构设计

### Crate 结构（3 个新增 crate）

```
crates/
├── eip8130-consensus/   共识类型层    类型定义、gas 计算、签名验证、存储槽推导、原生验证器
├── eip8130-revm/        EVM 执行层    AA 交易 handler、预编译（NonceManager/TxContext）、执行策略
├── eip8130-pool/        交易池层      2D nonce 侧池、mempool 验证、FAL 失效追踪、合并迭代器
```

### 与现有系统的集成点

| 集成点 | 方式 |
|--------|------|
| **FlashblocksBuilder** | `MergedBestTransactions` 合并标准池和 AA 侧池的交易迭代器 |
| **PayloadBuilder** | `AaPoolHandle`（类型擦除）避免向 builder 泛型体系传播新参数 |
| **ChainSpec** | `XLayerHardfork::NativeAA` + `is_native_aa_active_at_timestamp()` 时间戳门控 |
| **Precompiles** | `Eip8130Precompiles` 包装 `EthPrecompiles`，按 hardfork 开关 AA 预编译 |
| **Node (main.rs)** | AA 侧池仅在 sequencer + flashblocks 模式创建 |

### 激活机制（5 组件）

| 组件 | 归属 | 状态 |
|------|------|------|
| 时间戳门控 | EL | ✅ 完成 |
| 共识拒绝（区块内 0x7B → 无效） | EL | ✅ 函数就绪，待接入共识路径 |
| EVM Spec 切换（预编译注册 + handler 路径） | EL | ✅ 开关机制就绪，待接入 EVM 配置 |
| Upgrade Deposit Tx | **Go op-node** | ⏳ Go 侧开发 |
| 系统合约 bytecode | EL + Solidity | ⏳ 待编译嵌入 |

---

## 三、开发进度

### 分阶段完成情况

| Phase | 内容 | 状态 | Commit |
|-------|------|------|--------|
| Phase 0 | 集成穿刺 + crate 骨架 + hardfork 定义 | ✅ | `4edb852` |
| Phase 1 | 核心类型层（TxEip8130、gas、签名、存储、ABI、地址推导） | ✅ | `e917270` |
| Phase 2 | 原生验证器 + EVM 验证管线 + 执行计划生成 | ✅ | `e1fcc14` |
| Phase 3 | AA 交易池（2D nonce 侧池 + mempool 验证 + FAL 失效） | ✅ | `f08b530` |
| Phase 4 | EVM 执行引擎（handler + 预编译 + 执行策略） | ✅ | `88642a0` |
| Phase 5 | 集成层（RPC、payload builder、节点接线） | ✅ | `6432af8` |
| Phase 6 | EL 激活机制（共识拒绝、预编译开关、hardfork trait） | ✅ | `98c8fa2` |
| Phase 7 | E2E 测试 + 跨仓库集成 | ⏳ 待 Go 就绪 | — |

### 量化指标

| 指标 | 数值 |
|------|------|
| 新增/修改文件 | 54 个 |
| 新增代码行 | ~12,000 行（含设计文档 ~1,000 行） |
| AA 三大 crate Rust 代码 | ~10,400 行 |
| 单元测试 | **229 个**，全部通过 |
| Clippy 警告 | 0 |

---

## 四、关键设计决策

1. **3 crate 分层**（consensus / revm / pool）：与 Base 架构对齐，feature gate 隔离 EVM 依赖，消费者按需启用
2. **类型擦除 AA 池句柄**（`AaPoolHandle`）：用 `Arc<dyn Any>` 避免向 FlashblocksBuilder 泛型体系添加新类型参数
3. **双重放置**：AA 交易同时入 AA 侧池和标准 OP 池（`propagate: false`），保持 RPC 向后兼容
4. **Upgrade Deposit 由 Go op-node 生成**：遵循 OP-Stack 现有 hardfork 模式（Canyon/Ecotone/Fjord/Jovian），EL 只执行和验证
5. **hardfork 独立**：`XLayerHardfork::NativeAA` 独立于 OP 主线 hardfork，mainnet/testnet 设为 `Never`，devnet 设为 `Timestamp(0)`

---

## 五、跨仓库依赖（Go 侧）

EL Phase 0-6 可用单元测试独立验证。Phase 7（E2E）需要 Go 侧就绪：

| Go 改动 | 优先级 | 说明 |
|---------|--------|------|
| Hardfork 配置 | P0 | op-node `NativeAATime` 字段 + `IsNativeAA()` |
| Upgrade Deposit Tx 生成 | P0 | 激活区块注入 6 个系统合约部署 deposit tx |
| Span Batch 编解码 0x7B | P0 | `decodeTyped()` / `convertToFullTx()` 添加 case |
| 激活区块 sequencing | P0 | 激活区块 `NoTxPool=true` |

**协调时间线**：
```
EL Phase 0-6 (已完成)   →   Go 开发 hardfork + span batch + upgrade deposit
                              ↓
                         EL Phase 7 + Go 联合 Devnet E2E
```

---

## 六、待完成项

详见 `docs/NATIVE_AA_DEFERRED.md`：

1. ~~**系统合约 bytecode 嵌入**~~：✅ 已完成，6 个 hex 文件从 Base 嵌入，部署地址已更新为 Base 的 `deployer.create(0)` 确定性地址
2. **共识路径接线**：`validate_block_no_aa_tx()` 接入 engine validator（防止对端节点发送非法区块）
3. **EVM 配置接线**：`Eip8130Precompiles.aa_enabled` 在节点 EVM 构建时按 hardfork 设置
4. **Go op-node 改动**：upgrade deposit 生成、span batch 编解码、hardfork 配置

---

## 七、风险评估

| 风险 | 等级 | 缓解措施 |
|------|------|---------|
| 上游 Reth 版本升级可能 break 接口 | 中 | AA 代码通过 trait 扩展而非修改上游代码 |
| Go 侧开发进度影响 E2E 验证 | 中 | EL 已有 229 个单元测试独立覆盖；可用 mock Engine API 测试 |
| 系统合约地址变更 | 低 | 已采用 Base 的确定性 `deployer.create(0)` 地址，与 Base 完全一致 |
| 预编译安全性 | 低 | NonceManager/TxContext 为只读预编译，无状态修改 |
