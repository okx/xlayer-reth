# XLayer Native AA (EIP-8130) 移植设计方案

> **状态**: 最终版  
> **最后更新**: 2026-04-16  
> **基于**: Claude × Codex 五轮辩论共识

## 1. 概述

将 Base 的 EIP-8130 Native Account Abstraction 实现移植到 XLayer Reth。EIP-8130 在协议层原生支持账户抽象，取代外部 ERC-4337 bundler 方案。

**核心特性**：
- 新交易类型 `0x7B`（AA 交易）
- 2D Nonce 模型（`nonce_key` + `nonce_sequence`），支持多通道并行
- 分阶段原子调用（`calls: Vec<Vec<Call>>`）
- 独立 payer 赞助 gas
- 可插拔验证器（K1/P256/WebAuthn/Delegate/自定义）
- 账户创建（CREATE2）和配置变更

**Base 与 XLayer 共同基础**：
- 均基于 Reth v1.11.3
- 均为 OP-Stack L2
- 类型系统兼容（OpChainSpec, OpPrimitives, OpTransactionSigned）

**关键架构差异**：
- **Base**：单一 Rust monorepo，包含 EL + CL (derivation) + Batcher
- **XLayer**：EL 为 Rust（本仓库 `xlayer-reth`），CL (op-node) 和 Batcher 为 Go（独立仓库 `op-dev/optimism`）

> **本方案范围**：仅覆盖 EL 侧（本仓库）的改动。CL/Batcher 的 Go 侧改动见 [第 12 节](#12-跨仓库依赖el-与-go-clbatcher-的分工)。

---

## 2. Crate 结构设计

新增 **3 个 crate**，遵循 Base 的架构模式：

```
crates/
├── eip8130-consensus/                   # 新增：核心 AA 类型和逻辑
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs                       # 模块入口 + feature gates
│       ├── constants.rs                 # 常量（gas costs, limits, tx type id）
│       ├── types.rs                     # Call, Owner, AccountChangeEntry, etc.
│       ├── tx.rs                        # TxEip8130 结构体 + RLP 编解码
│       ├── verifier.rs                  # NativeVerifier, VerifierKind 路由
│       ├── signature.rs                 # 签名哈希计算 + sender_auth 解析
│       ├── address.rs                   # CREATE2 地址推导
│       ├── gas.rs                       # Intrinsic gas 计算
│       ├── storage.rs                   # 存储槽推导（owner_config, nonce, lock）
│       ├── abi.rs                       # sol! ABI 定义
│       ├── predeploys.rs               # 系统合约 & 预编译地址
│       ├── purity.rs                    # 字节码纯度扫描器
│       ├── accessors.rs                 # [feature=evm] 链上状态读写
│       ├── execution.rs                 # [feature=evm] 执行计划生成
│       ├── validation.rs               # [feature=evm] 交易验证管线
│       └── native_verifier.rs           # [feature=native-verifier] Rust 密码学
│
├── eip8130-revm/                        # 新增：EVM 执行引擎扩展
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── handler.rs                   # AA 交易执行流程
│       ├── eip8130_parts.rs             # 执行时数据结构
│       ├── eip8130_policy.rs            # 执行策略
│       └── precompiles.rs               # NonceManager/TxContext 预编译注册
│
├── eip8130-pool/                        # 新增：AA 交易池
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── pool.rs                      # Eip8130Pool（2D nonce 侧池）
│       ├── validate.rs                  # Mempool 验证逻辑
│       ├── invalidation.rs              # FAL 失效追踪
│       ├── best.rs                      # AA 交易排序迭代器
│       └── transaction.rs              # 池内交易类型适配
│
├── builder/                             # 已有：修改 payload builder
├── chainspec/                           # 已有：添加 AA hardfork
├── rpc/                                 # 已有：添加 AA RPC 扩展
└── ...
```

### 2.1 为什么是 3 个 crate？

这个拆分遵循 Base 的实际架构。Base 有 16 个 crate 涉及 AA 代码，但核心逻辑集中在 3 层：

| 层级 | Base 对应 | XLayer crate | 职责 |
|------|----------|-------------|------|
| 共识类型 | `base-alloy-consensus` | `xlayer-eip8130-consensus` | 纯类型 + 常量 + gas + 签名 + 存储 |
| EVM 执行 | `base-revm` | `xlayer-eip8130-revm` | handler + 预编译 |
| 交易池 | `base-node` (pool 部分) | `xlayer-eip8130-pool` | 侧池 + 验证 + 失效追踪 |

**不拆得更细的原因**：

- **consensus crate 内的 feature gate 已足够**：`accessors.rs`/`execution.rs`/`validation.rs` 用 `#[cfg(feature = "evm")]` 隔离，不需要的消费者不开 feature 即可，无需额外 crate
- **handler 强依赖其余 revm 文件**：`handler.rs` 和 `eip8130_parts.rs`/`precompiles.rs` 之间调用密集，拆开只会增加 re-export 样板
- **粒度太细的问题**：`handler.rs`(~3,500 行) 如果拆出来，`eip8130_parts.rs` 和 `precompiles.rs` 也得跟着拆（它们是 handler 的数据结构），结果是 3 个微 crate 互相依赖，不比 1 个 crate 简单

### 2.2 Feature gate 设计

```toml
# crates/eip8130-consensus/Cargo.toml
[features]
default = []
evm = ["dep:revm"]                              # accessors, execution, validation
native-verifier = ["dep:p256", "dep:sha2", "dep:k256"]  # native_verifier.rs
```

消费者按需开启：
- `eip8130-revm` → 依赖 `eip8130-consensus`（无需 feature）
- `eip8130-pool` → 依赖 `eip8130-consensus[evm, native-verifier]` + `eip8130-revm`
- `rpc` → 依赖 `eip8130-consensus`（无需 feature）

---

## 3. 依赖关系

```
                    ┌─────────────────────────┐
                    │  xlayer-eip8130-consensus│
                    │  alloy-*, alloy-rlp     │
                    │  [opt: revm, p256, k256] │
                    └───────────┬─────────────┘
                                │
               ┌────────────────┼──────────────────┐
               │                │                  │
      ┌────────▼─────────┐     │        ┌──────────▼────────┐
      │ xlayer-eip8130-  │     │        │ rpc               │
      │ revm             │     │        │ (AA RPC 扩展)      │
      │ revm, consensus  │     │        │ jsonrpsee         │
      └────────┬─────────┘     │        └──────────┬────────┘
               │               │                   │
      ┌────────▼─────────┐     │                   │
      │ xlayer-eip8130-  │     │                   │
      │ pool             │     │                   │
      │ consensus[evm,   │     │                   │
      │  native-verifier]│     │                   │
      │ + revm crate     │     │                   │
      └────────┬─────────┘     │                   │
               │               │                   │
               └───────────────┼───────────────────┘
                               │
                    ┌──────────▼──────────┐
                    │  builder            │
                    │  (payload builder)  │
                    └──────────┬──────────┘
                               │
                    ┌──────────▼──────────┐
                    │  bin/node           │
                    │  (组装入口)          │
                    └─────────────────────┘
```

### 3.1 为什么 pool 依赖 revm crate？

交易池验证自定义验证器签名时，需要做 STATICCALL 调用链上验证器合约。这个操作需要 revm 执行环境。Base 也有同样的依赖：pool 验证不仅仅是纯数据校验，还涉及 EVM 调用。

### 3.2 常量镜像与一致性保障

**关键发现**：Base 的 `base-revm` crate **不依赖** `base-alloy-consensus`（为避免循环依赖），而是在 `handler.rs` 中镜像了约 15 个常量/函数，带有 `// Mirrors base_alloy_consensus::...` 注释。

**XLayer 的做法**：同样采用镜像模式，但加入编译时断言确保一致性：

```rust
// crates/eip8130-revm/src/handler.rs
// Mirrors xlayer_eip8130_consensus::predeploys::K1_VERIFIER_ADDRESS
const K1_VERIFIER_ADDRESS: Address = address!("0x...0001");
// Mirrors xlayer_eip8130_consensus::predeploys::REVOKED_VERIFIER
const REVOKED_VERIFIER: Address = Address::MAX;
// ... 约 15 个常量
```

**镜像常量注册表**（所有需要跨 crate 同步的常量）：

| 常量 | 规范位置 (consensus) | 镜像位置 (revm) | Go 镜像 |
|------|---------------------|----------------|---------|
| `AA_TX_TYPE_ID` (0x7B) | `constants.rs` | `handler.rs` | `span_batch_tx.go` |
| `K1_VERIFIER_ADDRESS` | `predeploys.rs` | `handler.rs` | — |
| `REVOKED_VERIFIER` | `predeploys.rs` | `handler.rs` | — |
| `NONCE_MANAGER_ADDRESS` | `predeploys.rs` | `handler.rs` | — |
| `TX_CONTEXT_ADDRESS` | `predeploys.rs` | `handler.rs` | — |
| `ACCOUNT_CONFIG_ADDRESS` | `predeploys.rs` | `handler.rs` | — |
| `DEFAULT_ACCOUNT_ADDRESS` | `predeploys.rs` | `handler.rs` | — |
| `NONCE_KEY_MAX` | `constants.rs` | `handler.rs` | — |
| `nonce_slot()` | `storage.rs` | `handler.rs` | — |
| `owner_config_slot()` | `storage.rs` | `handler.rs` | — |
| `lock_slot()` | `storage.rs` | `handler.rs` | — |
| `expiring_seen_slot()` | `storage.rs` | `handler.rs` | — |
| `expiring_ring_slot()` | `storage.rs` | `handler.rs` | — |
| `account_state_slot()` | `storage.rs` | `handler.rs` | — |

**一致性检查方式**：在 `eip8130-revm` 的测试中使用 `const_assert!` 或编译时检查（而非仅运行时测试），确保镜像常量与规范来源完全一致。

---

## 4. 关键设计决策

### 4.1 交易池架构：双重放置（Dual Placement）

**这是与初始设计的重大变化**。仅靠独立侧池是不够的。

**方案**：AA 交易同时进入两个池：

```
                    ┌──────────────────────────────────────────────┐
                    │         XLayerTransactionPool                │
                    │                                              │
eth_send ───►       │  ┌─────────────────┐   ┌─────────────────┐  │
RawTx        ───────┤  │ Standard OP Pool │   │ Eip8130 Side   │  │
                    │  │ (原有 pool)       │   │ Pool            │  │
                    │  │                  │   │ (2D nonce 排序) │  │
                    │  │  - Legacy tx     │   │                 │  │
                    │  │  - EIP-1559 tx   │   │  - AA 独立排序  │  │
                    │  │  - EIP-7702 tx   │   │  - FAL 失效追踪│  │
                    │  │  - Deposit tx    │   │  - 自定义验证器  │  │
                    │  │  - AA tx ◄──┐    │   │    STATICCALL   │  │
                    │  │ (propagate: │    │   └────────┬────────┘  │
                    │  │  false)     │    │            │           │
                    │  └──────┬──────┘    │   ┌────────▼────────┐  │
                    │         │      ▲    │   │ Dual Placement  │  │
                    │         │      └────┼───│ Router          │  │
                    │         │           │   └─────────────────┘  │
                    │  ┌──────▼──────┐    │                        │
                    │  │ Merged Best │    │                        │
                    │  │ Transactions│◄───┘                        │
                    │  └──────┬──────┘                             │
                    └─────────┼────────────────────────────────────┘
                              ▼
                       Payload Builder
```

**为什么需要双重放置**：
- **侧池**：AA 交易需要独立的排序逻辑（2D nonce）、验证（自定义验证器 STATICCALL）、失效追踪（FAL）
- **标准池（propagate: false）**：AA 交易进入标准池但不广播。目的是让 `eth_getTransactionByHash`、`txpool_content` 等 RPC 不需要额外修改就能看到 AA 交易

**Base 参考实现**（已验证）：
- `base/crates/txpool/src/validator.rs:336-395` — AA tx 同时加入 `eip8130_pool` 和标准池（返回 `ValidTransaction::Valid` + `propagate: false`）
- `base/crates/txpool/src/base_pool.rs:29-43` — `BaseTransactionPool` 包装器合并两个池的查询结果
- `base/crates/txpool/src/base_pool.rs:260-270` — `get_pooled_transaction_element` 先查标准池再查侧池

### 4.2 Hardfork 激活

**需要团队决策**：AA 使用独立 fork 还是附加到已有 fork？

| 方案 | 优点 | 缺点 |
|------|------|------|
| **A: 独立 `NativeAA` fork** | 清晰隔离，可独立调度 | 多一个 fork 管理 |
| **B: 附加到已有 fork**（如 Karst） | 减少 fork 数量 | AA 和其他功能绑定，无法独立上线 |

> **建议**：方案 A（独立 fork），除非团队明确希望 AA 随某个 fork 一起上线。

激活需要以下 **5 个组件**，分布在 EL 和 CL 两侧：

| 组件 | 归属 | 说明 | Phase |
|------|------|------|-------|
| **时间戳门控** | EL + CL | `is_aa_active(timestamp)` 检查 | Phase 0 |
| **共识拒绝** | EL | 激活前，区块内出现 0x7B 交易 → 区块无效 | Phase 0 骨架 → Phase 6 完整 |
| **EVM Spec 选择** | EL | 激活后，EVM 使用 AA-aware spec（注册预编译、修改 handler） | Phase 4 |
| **Upgrade Deposit Tx** | **CL (Go op-node)** | op-node 在 PayloadAttributes 中注入系统合约部署交易；EL 只执行和验证 | Phase 6 (Go 侧) |
| **预编译桩** | EL | NonceManager/TxContext 预编译在激活后注册 | Phase 4 |

> **重要**：Upgrade deposit tx 由 **Go op-node** 生成（通过 `derive/attributes.go`），不是由 EL payload builder 生成。这与 OP-Stack 现有 hardfork（Canyon/Ecotone/Fjord/Jovian）的激活模式一致：op-node 构造 PayloadAttributes 时附带 upgrade deposits，EL 只负责执行。

```rust
// Phase 0 骨架代码 (EL 侧)
pub fn is_aa_active(chain_spec: &OpChainSpec, timestamp: u64) -> bool {
    chain_spec
        .xlayer_aa_time()
        .map_or(false, |t| timestamp >= t)
}
```

激活前行为：
- 交易池拒绝 `0x7B` 类型交易
- 共识层拒绝包含 `0x7B` 交易的区块
- RPC 不返回 AA 特有字段

激活时行为（激活区块）：
- CL (op-node) 在 PayloadAttributes 中注入 upgrade deposit tx
- EL 执行 upgrade deposits → 系统合约部署
- EL 注册 NonceManager/TxContext 预编译
- EL 启用 AA handler 路径

### 4.3 预部署地址

**方案**: 预编译地址与 Base 一致（协议标准），系统合约地址 XLayer 独立部署。

开发/测试阶段使用确定性占位地址，便于早期测试：

```rust
// 预编译 - 与 Base 一致（协议标准）
pub const NONCE_MANAGER_ADDRESS: Address = address!("0x...aa02");
pub const TX_CONTEXT_ADDRESS: Address = address!("0x...aa03");

// K1 验证器地址保持 address(1)（协议标准）
pub const K1_VERIFIER_ADDRESS: Address = address!("0x...0001");

// 系统合约 - dev/test 占位地址（Phase 1 定义，生产部署后替换）
pub const ACCOUNT_CONFIG_ADDRESS: Address = address!("0xAA00...0001");
pub const DEFAULT_ACCOUNT_ADDRESS: Address = address!("0xAA00...0002");
pub const P256_RAW_VERIFIER_ADDRESS: Address = address!("0xAA00...0003");
pub const P256_WEBAUTHN_VERIFIER_ADDRESS: Address = address!("0xAA00...0004");
pub const DELEGATE_VERIFIER_ADDRESS: Address = address!("0xAA00...0005");
```

> Phase 1 即定义占位地址，不等到 Phase 6。测试可使用 dev genesis 在这些地址预部署 bytecode。

### 4.4 EVM Handler 扩展

在现有 `OpEvmConfig` 基础上扩展，不修改上游代码。

AA 交易执行流程：

```
1. 检查锁定状态（reject locked accounts 的 config changes）
2. 从 payer 扣除 gas（写 precompile storage）
3. 递增 nonce（写 NonceManager storage）
4. Auto-delegate 裸 EOA 到 DEFAULT_ACCOUNT_ADDRESS
5. 处理 account_changes：
   a. CREATE2 部署新账户
   b. 配置变更（add/revoke owner）
   c. EIP-7702 delegation
6. 填充 TxContext 预编译数据
7. 分阶段执行 calls：
   - 每个 phase 内部原子（全成功或全回滚）
   - phase 之间独立（一个 phase 失败不影响其他）
8. 退还未用 gas 给 payer
9. 生成 receipt（含 payer + phaseStatuses）
```

### 4.5 Verifier Gas 成本

沿用 Base 的 gas 成本表（可通过 chainspec 参数覆盖）：

| Verifier | Gas |
|----------|-----|
| K1 (secp256k1) | 6,000 |
| P256 Raw | 9,500 |
| P256 WebAuthn | 15,000 |
| Delegate (额外开销) | 3,000 |
| 自定义 (STATICCALL) | 运行时计量，上限 200,000 |

### 4.6 系统合约

需要部署的 Solidity 合约（Base 已有完整实现，`contracts/eip-8130` 子模块可复用）：

| 合约 | 用途 | 部署方式 |
|------|------|---------|
| AccountConfiguration | Owner 管理、CREATE2、Config Change、Lock | Upgrade deposit tx (CL 注入) |
| DefaultAccount | EOA auto-delegate 目标 | Upgrade deposit tx (CL 注入) |
| DefaultHighRateAccount | 锁定模式高频变体 | Upgrade deposit tx (CL 注入) |
| P256Verifier | P256 ECDSA 验证 | Upgrade deposit tx (CL 注入) |
| WebAuthnVerifier | WebAuthn 验证 | Upgrade deposit tx (CL 注入) |
| DelegateVerifier | 一跳委托 | Upgrade deposit tx (CL 注入) |

> 系统合约 bytecode 在 Phase 1 编译并嵌入 EL 代码（用于 dev genesis 预部署和测试）。正式激活时由 CL 通过 upgrade deposit tx 部署。

---

## 5. 分阶段开发计划（TDD）

### Phase 0: 集成穿刺（Integration Spike）（~2 天）

**目标**: 证明 crate 接线可行，建立 hardfork 门控骨架。

```
Step 1: 创建 3 个 crate 的 Cargo.toml 和空 lib.rs
  - 确认 workspace 编译通过
  - 确认依赖关系正确

Step 2: Hardfork 门控骨架
  - chainspec 添加 xlayer_aa_time 字段
  - is_aa_active() 函数
  - 交易池 early-return guard（未激活时直接拒绝 0x7B）
  - 共识 early-return guard（未激活时拒绝含 0x7B 的区块）

Step 3: bin/node 接线
  - 编译时引入 3 个新 crate
  - 确认现有功能不受影响
```

**验收标准**:
- `just check` 全部通过
- `just test` 无回归
- 新 crate 编译成功但功能为空

---

### Phase 1: 核心类型层（~1 周）

**目标**: `crates/eip8130-consensus` 编译通过，全部单元测试绿色。含系统合约 bytecode 编译和 dev 占位地址。

**TDD 顺序**:

```
Step 1: constants.rs
  RED:   test AA_TX_TYPE_ID == 0x7B, AA_BASE_COST == 15000, etc.
  GREEN: 定义常量
  
Step 2: types.rs (Call, Owner, OwnerChange, AccountChangeEntry)
  RED:   test Call RLP round-trip
  RED:   test Owner RLP round-trip
  RED:   test AccountChangeEntry::Create RLP round-trip
  RED:   test AccountChangeEntry::ConfigChange RLP round-trip
  RED:   test OwnerScope::has()
  GREEN: 实现类型 + Encodable/Decodable

Step 3: verifier.rs (NativeVerifier, VerifierKind)
  RED:   test NativeVerifier round-trip (address → enum → address)
  RED:   test custom verifier stays custom
  GREEN: 实现验证器路由

Step 4: predeploys.rs（含 dev 占位地址）
  RED:   test is_native_verifier for known addresses
  GREEN: 定义地址常量（dev 占位地址）

Step 5: tx.rs (TxEip8130)
  RED:   test RLP round-trip
  RED:   test EIP-2718 round-trip
  RED:   test Transaction trait getters
  RED:   test sender/payer signing differ (domain separation)
  RED:   test is_eoa / is_self_pay
  RED:   test optional address encoding
  RED:   test multi-phase calls round-trip
  GREEN: 实现 TxEip8130 + all traits

Step 6: signature.rs
  RED:   test parse_eoa_auth (65 bytes)
  RED:   test parse_eoa_wrong_length → error
  RED:   test parse_configured_k1
  RED:   test sender/payer hashes deterministic and differ
  RED:   test config_change_digest
  GREEN: 实现签名哈希计算

Step 7: gas.rs
  RED:   test calldata_gas basic
  RED:   test nonce_key_cost warm vs cold
  RED:   test bytecode_cost (no create / with create)
  RED:   test account_changes_cost (applied vs skipped)
  RED:   test sender/payer verification gas for each verifier type
  RED:   test intrinsic_gas smoke
  RED:   test delegate inner verifier extraction
  GREEN: 实现 intrinsic gas 计算

Step 8: address.rs
  RED:   test deployment_header is 14 bytes
  RED:   test effective_salt order-independent
  RED:   test create2_address deterministic
  RED:   test different owners → different address
  GREEN: 实现 CREATE2 地址推导

Step 9: storage.rs
  RED:   test owner_config_slot deterministic
  RED:   test owner_config_slot matches Solidity fixture
  RED:   test nonce_slot deterministic
  RED:   test AccountState parse/encode round-trip
  RED:   test write_sequence preserves lock fields
  RED:   test parse_owner_config round-trip
  GREEN: 实现存储槽推导

Step 10: abi.rs + purity.rs
  GREEN: abi.rs 直接移植 sol! 定义（声明式）
  RED:   test pure bytecode passes
  RED:   test SLOAD bytecode fails
  RED:   test STATICCALL to known precompile passes
  RED:   test hidden JUMPDEST detection
  GREEN: 实现 PurityScanner

Step 11: 系统合约 bytecode
  - 编译 Base 的 contracts/eip-8130 Solidity 合约
  - 嵌入编译后的 hex bytecode 到 crate 中
  - 用于 dev genesis 预部署测试
```

**验收标准**:
- `cargo test -p xlayer-eip8130-consensus` 全绿
- `cargo clippy -p xlayer-eip8130-consensus -- -D warnings` 无警告
- 所有 Base 的已有测试向量通过

---

### Phase 2: 原生验证器 + EVM 验证层（~1 周）

**目标**: 能验证 AA 交易的签名、nonce、余额、锁定状态。

**TDD 顺序**:

```
Step 1: native_verifier.rs [feature=native-verifier]
  RED:   test K1 verify with known test vector
  RED:   test P256 raw verify
  RED:   test WebAuthn verify (full envelope)
  RED:   test Delegate wrapping K1
  RED:   test invalid signature → error
  GREEN: 实现原生密码学验证

Step 2: accessors.rs [feature=evm]
  RED:   test read_owner_config from mock state
  RED:   test read_nonce from mock state
  RED:   test read_lock_state
  RED:   test is_owner_authorized
  GREEN: 实现链上状态读取

Step 3: validation.rs [feature=evm]
  RED:   test validate_structure (field range checks)
  RED:   test validate_nonce (contiguous from on-chain)
  RED:   test validate_expiry
  RED:   test check_sender_authorization (EOA K1)
  RED:   test check_payer_authorization
  RED:   test check_lock_state
  GREEN: 实现验证管线

Step 4: execution.rs [feature=evm]
  RED:   test build_execution_calls for simple tx
  RED:   test config_change_writes
  RED:   test nonce_increment_write
  RED:   test gas_refund calculation
  GREEN: 实现执行计划生成
```

**验收标准**:
- `cargo test -p xlayer-eip8130-consensus --all-features` 全绿
- 能对构造的 AA 交易完成全部校验步骤

---

### Phase 3: 交易池 + 双重放置（~1.5 周）

**目标**: 节点能接收、验证、存储、排序 AA 交易。包含完整的双重放置机制。

**TDD 顺序**:

```
Step 1: transaction.rs (池内交易类型)
  RED:   test Eip8130Metadata creation
  RED:   test priority ordering
  GREEN: 实现池内交易适配

Step 2: pool.rs (Eip8130Pool 侧池)
  RED:   test add single tx → pool contains it
  RED:   test add duplicate tx → rejected
  RED:   test nonce ordering within lane
  RED:   test multiple nonce_key lanes independent
  RED:   test eviction by lowest priority fee
  RED:   test expiry eviction
  RED:   test pool size limits
  GREEN: 实现 2D nonce 侧池

Step 3: validate.rs (Mempool 验证)
  RED:   test valid K1 self-pay tx → accepted
  RED:   test invalid signature → rejected
  RED:   test insufficient payer balance → rejected
  RED:   test nonce gap → rejected
  RED:   test expired tx → rejected
  RED:   test custom verifier purity check
  RED:   test custom verifier STATICCALL validation
  GREEN: 实现 mempool 验证（含 STATICCALL）

Step 4: invalidation.rs (FAL 追踪)
  RED:   test canonical update evicts conflicting tx
  RED:   test reorg re-inserts reverted tx
  RED:   test FAL entry tracking across state accesses
  GREEN: 实现 First-Access-to-Later 失效追踪

Step 5: 双重放置集成 (XLayerTransactionPool)
  RED:   test standard tx → OP pool only
  RED:   test AA tx → both side pool and OP pool
  RED:   test AA tx in OP pool has propagate=false
  RED:   test eth_getTransactionByHash finds AA tx via OP pool
  RED:   test MergedBestTransactions yields both standard and AA txs
  RED:   test AA tx ordering respects 2D nonce from side pool
  GREEN: 实现双重放置包装池
```

**验收标准**:
- `cargo test -p xlayer-eip8130-pool` 全绿
- 集成测试：启动 devnet 节点，发送 AA tx，`txpool_content` 可见
- AA 交易不通过 P2P 广播（propagate: false）

---

### Phase 4: EVM 执行引擎（~1.5 周）

**目标**: AA 交易能在 EVM 中正确执行。

**TDD 顺序**:

```
Step 1: precompiles.rs (NonceManager + TxContext)
  RED:   test NonceManager getNonce returns correct value
  RED:   test TxContext getSender/getPayer/getOwnerId
  RED:   test invalid calldata → error
  GREEN: 实现预编译合约

Step 2: eip8130_parts.rs + eip8130_policy.rs
  RED:   test Eip8130Parts from TxEip8130
  RED:   test policy enforcement
  GREEN: 实现执行时数据结构和策略

Step 3: handler.rs (核心执行逻辑) - 分步实现
  3a. 最小路径：EOA K1 自付费 + 单阶段单调用
    RED:   test simple K1 self-pay tx executes
    RED:   test gas deducted from sender
    RED:   test nonce incremented
    RED:   test receipt has correct status
    GREEN: 实现基础执行流

  3b. Payer 赞助
    RED:   test external payer gas deduction
    RED:   test payer refund for unused gas
    GREEN: 扩展 payer 支持

  3c. 多阶段调用
    RED:   test multi-phase: all succeed
    RED:   test multi-phase: phase 2 fails, phase 1 committed
    RED:   test phaseStatuses in receipt
    GREEN: 实现分阶段执行

  3d. Account changes
    RED:   test CREATE2 account deployment
    RED:   test config change (add owner)
    RED:   test auto-delegate bare EOA
    GREEN: 实现 account_changes 处理

Step 4: 镜像常量验证
  RED:   const_assert! 或编译时检查镜像常量与 consensus crate 一致
  GREEN: 编写 cross-crate 常量一致性断言
```

**验收标准**:
- 集成测试：发送 AA tx → 出块 → receipt 正确 → 链上状态变化正确
- 镜像常量与 consensus crate 100% 一致（编译时保证）

---

### Phase 5: RPC 扩展（~1 周）

**目标**: 完整用户查询接口。

```
Step 1: RPC 扩展
  - eth_getTransactionCount(addr, block, nonceKey?) → 2D nonce 查询
    - 向后兼容：2 参数调用行为不变
    - nonceKey 对非 AA 账户返回 0（与标准 nonce 一致）
    - nonceKey 对不存在的地址返回 0
  - Receipt 扩展：payer + phaseStatuses 字段
  - eth_sendRawTransaction 识别 0x7B 交易
  
Step 2: Payload Builder 集成
  - MergedBestTransactions：合并标准池 + AA 侧池
  - AA 交易的 gas 预算管理
  - 与 flashblocks builder 兼容

Step 3: Node Builder 组装
  - XLayerTransactionPool 替换现有 pool
  - AA handler 注册
  - RPC 扩展注册
```

**验收标准**:
- E2E：用户签名 AA tx → RPC 提交 → 入池 → 出块 → 查询 receipt
- `eth_getTransactionCount` 2 参数调用行为完全不变（向后兼容）

---

### Phase 6: EL 激活机制（~1 周）

**目标**: EL 侧完整的 hardfork 激活机制。

> **注意**: Upgrade deposit tx 由 **Go op-node** 生成（通过 `derive/attributes.go`），不是 EL 的职责。EL 只负责执行和验证这些 deposits。CL/Batcher 的改动见 [第 12 节](#12-跨仓库依赖el-与-go-clbatcher-的分工)。

```
Step 1: 共识拒绝 (EL)
  - 激活前：区块包含 0x7B 交易 → block_validation 失败
  - 激活后：正常处理

Step 2: Upgrade Deposit 执行与验证 (EL)
  - EL 执行 CL 注入的 upgrade deposit tx
  - 验证执行结果：系统合约正确部署到预期地址
  - 参考 Base 的 crates/consensus/upgrades/src/base_v1.rs（bytecode 来源）

Step 3: EVM Spec 切换 (EL)
  - 激活后注册 NonceManager/TxContext 预编译
  - 激活后启用 AA handler 路径

Step 4: 系统合约 bytecode 冻结
  - 确认编译后的 bytecode 与 Base 一致
  - 替换 dev 占位地址为正式部署地址（如已确定）
```

**验收标准**:
- EL 单独测试：mock Engine API 发送含 upgrade deposit 的 PayloadAttributes → 系统合约部署成功
- 激活前 block 包含 0x7B → 拒绝
- 激活后 AA 交易正常执行

**注意**：完整的 Devnet E2E（创世 → 激活 → AA 可用）需要 Go CL 侧同步就绪。EL 可以先用 mock Engine API 测试。

---

### Phase 7: E2E 测试 + 高级功能（~1 周）

**目标**: 生产级功能完整和全面测试。

**功能测试**：
```
- P256/WebAuthn/Delegate 验证器端到端测试
- 自定义验证器 STATICCALL 路径
- Purity scanner 集成测试
- Expiring nonce (nonce-free mode)
- 多通道并行交易测试
- Payer 赞助完整流程
- Reorg 处理和 FAL 失效
- 性能测试和优化
- 与 flashblocks 兼容性测试
- 与 bridge intercept 兼容性测试
```

**跨仓库集成测试**（需 Go CL/Batcher 就绪）：
```
- Go→Rust span batch 0x7B 编码/解码互操作 golden test
- 激活区块 upgrade deposit 排序正确性
- 激活区块 NoTxPool=true 行为验证
- AA 激活边界的 reorg 处理
- 完整路径：用户签名 → RPC → 入池 → sequencer 出块 → batcher 提交 L1 → derivation 重建
```

---

## 6. 架构辩论记录

> 以下是 Claude 与 Codex 五轮辩论的关键分歧点和最终共识，记录在此供团队 review。

### 6.1 分歧点汇总（第 1-3 轮）

| # | 议题 | Claude 立场 | Codex 立场 | 最终共识 | 谁让步 |
|---|------|------------|-----------|---------|--------|
| 1 | Feature-gated EVM 代码放在 consensus crate 内还是单独拆 crate | 放 consensus 内用 `#[cfg(feature)]`。Base 就是这么做的，消费者不开 feature 就不编。 | 拆出独立 crate 更干净，避免 consensus crate 膨胀 | **保留在 consensus crate**，用 feature gate | Codex 让步。理由：Base 的模式已验证可行，拆出来只增加样板 |
| 2 | Hardfork 激活复杂度 | 初始方案只有时间戳检查 | 激活远不止时间戳：还需要共识拒绝、EVM spec 选择、upgrade deposit tx、预编译桩 | **Codex 正确**，扩展为 5 组件激活。Phase 0 放骨架，Phase 6 完整实现 | Claude 让步。这是设计中最重要的纠正 |
| 3 | Core boundary 风险等级 | LOW 风险（因为 feature gate 已足够隔离） | HIGH 风险（consensus crate 边界模糊会传染到下游） | **LOW**，但增加 cross-feature 编译测试 CI job | Codex 让步，但贡献了有价值的缓解措施 |
| 4 | Pool 是否依赖 revm crate | 初始设计未考虑 | 必须依赖——自定义验证器签名检验需要 STATICCALL | **Codex 正确**，pool 依赖 eip8130-revm | Claude 接受。这是依赖图的重要修正 |
| 5 | Crate 命名 | `eip8130-core` | `eip8130-consensus` 更准确 | **`eip8130-consensus`** | Claude 接受。命名更贴切 |

### 6.2 分歧点汇总（第 4-5 轮：最终评审）

| # | 议题 | Claude 立场 | Codex 立场 | 最终共识 | 谁让步 |
|---|------|------------|-----------|---------|--------|
| 6 | Upgrade deposit tx 归属 | EL sequencer 在 payload building 时注入 | **必须由 Go op-node 生成**（通过 PayloadAttributes），EL 只执行 | **Codex 正确**。与 OP-Stack 现有 hardfork 模式一致 | Claude 让步。这是共识边界错误，最严重的纠正 |
| 7 | EL Phase 0-4 独立性 | 完全不依赖 Go | 高估了——真实 devnet 需要 Go 更早介入 | 窄化为"EL Phase 0-4 可用单元测试和 mock Engine API 独立验证"。真正的 devnet 集成需 Go | 双方妥协 |
| 8 | `eth_getTransactionCount` 重载 | 添加可选第三参数 nonceKey（Base 做法） | 创建独立 AA namespace 更安全 | **保留 Base 做法**（可选参数），但必须明确向后兼容保证和边界行为 | Codex 部分让步。接受方案但要求补充 error semantics |
| 9 | 常量镜像方式 | 手动镜像 + CI 测试 | 共享 crate 更安全 | **手动镜像是正确的**（Base 的有意选择），但升级为 `const_assert!` 编译时检查，并增加常量注册表文档 | Codex 让步架构方案，Claude 接受改进措施 |
| 10 | 合约地址时机 | Phase 6 编译 bytecode，Phase 1 用 TODO_DEPLOY | 太晚——Phase 1-4 测试需要真实地址 | Phase 1 定义 dev 占位地址 + 编译 bytecode，Phase 6 冻结正式地址 | 双方妥协 |

### 6.3 辩论要点详解

#### 分歧 1: Feature gate vs 独立 crate

**Codex 论点**：consensus crate 应该是纯类型，EVM 相关代码（accessors/execution/validation）放入独立 crate 更符合关注点分离。

**Claude 反驳**：
1. Base 已经验证了 feature gate 模式可行（`base-alloy-consensus` 就是这么做的）
2. `accessors.rs`/`execution.rs`/`validation.rs` 这些文件与类型紧密耦合
3. 拆出独立 crate 需要大量 re-export
4. 消费者不需要 EVM 功能时，不开 `evm` feature 即可——编译时零成本

**最终**：Codex 同意 "Base 的模式已验证可行"。

#### 分歧 2: Hardfork 激活复杂度

这是 Codex 在第 1-3 轮中最有价值的贡献。原始设计只有时间戳检查，Codex 指出完整激活需要 5 个组件。妥协方案：Phase 0 放 early-return guard（骨架），Phase 6 完整实现。

#### 分歧 6: Upgrade Deposit Tx 归属（最严重纠正）

这是 Codex 在第 4-5 轮中最关键的发现。原始设计错误地将 upgrade deposit 生成放在 EL payload builder 中。

**Codex 论点**：
1. OP-Stack 中，op-node 通过 `derive/attributes.go` 构造 PayloadAttributes 时注入 upgrade deposits
2. Sequencer 模式下，op-node 发送 `NoTxPool=true` 标记给 EL
3. 如果 EL 也注入 deposits，会导致 safe/unsafe derivation 不一致

**Claude 反思**：Base 的 Rust monorepo 在 `crates/consensus/upgrades/src/base_v1.rs` 中生成 deposits，但那是因为 Base 的整个栈都是 Rust。XLayer 的 CL 是 Go，必须遵循 Go op-node 的 PayloadAttributes 流程。

---

## 7. 与 XLayer 现有功能的兼容性

### 7.1 Flashblocks

| 场景 | 处理方式 |
|------|---------|
| Flashblocks builder 包含 AA 交易 | Payload builder 的 MergedBestTransactions 自动包含 |
| Flashblocks 缓存重放 | DefaultPayloadBuilder 重放时需识别 AA 交易 |
| Flashblocks state cache | AA 交易的状态变化需正确反映在 cache 中 |

### 7.2 Bridge Intercept

AA 交易的 calls 中如果包含对 bridge 合约的调用，intercept 逻辑需要能检测。

**方案**: 在 AA 交易的 calls 解析阶段检查每个 call 的 `to` 地址。

### 7.3 Legacy RPC

Legacy RPC 路由层不受影响——AA 交易只在激活后出现。

---

## 8. 风险和缓解

| 风险 | 影响 | 缓解 |
|------|------|------|
| Handler 改动破坏标准交易执行 | 高 | AA handler 逻辑只在 tx_type == 0x7B 时激活 |
| 交易池双重放置引入复杂性 | 中 | 标准交易不经过双重放置路径 |
| 系统合约 bytecode 不兼容 | 中 | 使用 Base 的 Solidity 源码原样编译 |
| Reth 上游升级后需要 rebase | 中 | AA 代码隔离在独立 crate，减少冲突面 |
| P256/WebAuthn 密码学实现差异 | 低 | 使用相同的 Rust crate（p256, sha2） |
| 镜像常量不同步 | 低 | `const_assert!` 编译时检查 + 常量注册表文档 |
| Hardfork 激活遗漏组件 | 中 | Phase 0 骨架确保所有 guard point 都有占位 |
| Go CL/Batcher 不同步 | 中 | EL Phase 0-4 用 mock Engine API 独立测试；Go 侧最晚在 Phase 5-6 前启动 |
| Span batch 编码 Go/Rust 不一致 | 中 | Base 的 Rust 实现作为参考规范；Phase 7 编写 Go↔Rust golden test |
| Upgrade deposit EL/CL 不一致 | 高 | 明确 CL 生成 / EL 执行的分工；Phase 7 编写 deposit 排序 golden test |

---

## 9. 测试策略

| 层级 | 工具 | 覆盖范围 |
|------|------|---------|
| 单元测试 | `cargo test` | 每个模块的独立逻辑 |
| 常量一致性 | `const_assert!` | revm 镜像常量 vs consensus 常量（编译时） |
| 集成测试 | `crates/tests/` | 节点级多模块交互 |
| E2E 测试 | devnet + 脚本 | 完整用户流程 |
| 兼容性测试 | 回归测试 | 标准交易不受影响 |
| 跨仓库 golden test | 自定义工具 | Go↔Rust span batch round-trip, deposit ordering |
| 性能测试 | criterion | AA 交易处理延迟和 TPS |

每个 Phase 结束后运行完整测试套件，确保无回归。

---

## 10. 文件清单（预估）

| Crate | 文件 | 预估行数 | 来源 |
|-------|------|---------|------|
| eip8130-consensus | constants.rs | ~190 | 移植 Base |
| eip8130-consensus | types.rs | ~470 | 移植 Base |
| eip8130-consensus | tx.rs | ~790 | 移植 Base |
| eip8130-consensus | verifier.rs | ~115 | 移植 Base |
| eip8130-consensus | signature.rs | ~180 | 移植 Base |
| eip8130-consensus | gas.rs | ~690 | 移植 Base |
| eip8130-consensus | address.rs | ~175 | 移植 Base |
| eip8130-consensus | storage.rs | ~450 | 移植 Base |
| eip8130-consensus | abi.rs | ~112 | 移植 Base |
| eip8130-consensus | predeploys.rs | ~126 | 移植 + dev 地址 |
| eip8130-consensus | purity.rs | ~970 | 移植 Base |
| eip8130-consensus | accessors.rs | ~130 | 移植 Base [feature=evm] |
| eip8130-consensus | execution.rs | ~260 | 移植 Base [feature=evm] |
| eip8130-consensus | validation.rs | ~690 | 移植 Base [feature=evm] |
| eip8130-consensus | native_verifier.rs | ~960 | 移植 Base [feature=native-verifier] |
| **小计** | | **~5,308** | |
| eip8130-revm | handler.rs | ~3,500 | 移植 + 适配 XLayer |
| eip8130-revm | eip8130_parts.rs | ~540 | 移植 Base |
| eip8130-revm | eip8130_policy.rs | ~200 | 移植 Base |
| eip8130-revm | precompiles.rs | ~1,130 | 移植 Base |
| **小计** | | **~5,370** | |
| eip8130-pool | pool.rs | ~2,150 | 移植 Base |
| eip8130-pool | validate.rs | ~2,050 | 移植 Base |
| eip8130-pool | invalidation.rs | ~670 | 移植 Base |
| eip8130-pool | best.rs | ~280 | 移植 Base |
| eip8130-pool | transaction.rs | ~550 | 移植 + 适配 XLayer |
| **小计** | | **~5,700** | |
| rpc (修改) | aa.rs | ~180 | 移植 Base |
| builder (修改) | 若干处 | ~200 | 新增 AA 交易选择 |
| chainspec (修改) | 若干处 | ~80 | 新增 hardfork + 激活逻辑 |
| **修改小计** | | **~460** | |
| **总计** | | **~16,838** | |

---

## 11. 时间线总览

| Phase | 内容 | 预估时间 | 依赖 |
|-------|------|---------|------|
| Phase 0 | 集成穿刺 + hardfork 骨架 | ~2 天 | 无 |
| Phase 1 | 核心类型层 + bytecode 编译 | ~1 周 | Phase 0 |
| Phase 2 | 原生验证器 + EVM 验证层 | ~1 周 | Phase 1 |
| Phase 3 | 交易池 + 双重放置 | ~1.5 周 | Phase 2 |
| Phase 4 | EVM 执行引擎 | ~1.5 周 | Phase 2 |
| Phase 5 | RPC 扩展 | ~1 周 | Phase 3 + 4 |
| Phase 6 | EL 激活机制 | ~1 周 | Phase 4 |
| Phase 7 | E2E + 高级功能 + 跨仓库集成 | ~1 周 | Phase 5 + 6 + Go 就绪 |
| **总计** | | **~8-9 周** | |

注：Phase 3 和 Phase 4 可以并行开发（它们都只依赖 Phase 2）。

---

## 12. 跨仓库依赖：EL 与 Go CL/Batcher 的分工

XLayer 的架构与 Base 不同：Base 是单一 Rust monorepo（EL + CL + Batcher），而 XLayer 的 CL (op-node) 和 Batcher (op-batcher) 是独立的 Go 仓库。

### 12.1 职责划分

| 关注点 | EL (本仓库, Rust) | CL / op-node (Go) | Batcher (Go) |
|--------|-------------------|-------------------|--------------|
| 交易类型定义 (0x7B) | TxEip8130 类型、RLP 编解码 | 需识别 0x7B 类型 | 需编码 0x7B |
| 交易池 | 双重放置侧池 + 验证 | — | — |
| EVM 执行 | handler + 预编译 | — | — |
| 共识验证 | 区块内 tx 类型检查 | 区块内 tx 类型检查 | — |
| **Upgrade Deposit Tx** | **执行和验证** | **生成**（通过 PayloadAttributes） | — |
| Span Batch 编码 | — | 解码 span batch 中的 0x7B | 编码 0x7B 到 span batch |
| Hardfork 配置 | chainspec 时间戳 | 配置文件时间戳 | 配置文件时间戳 |
| RPC | AA 扩展接口 | — | — |
| P2P | AA tx 不广播 (propagate: false) | — | — |

### 12.2 Go 侧需要的改动

> Go 仓库路径：`/Users/xzavieryuan/workspace/op-dev/optimism`（OKX fork of optimism）

#### CL / op-node (Go)

| 改动 | 优先级 | 涉及文件 | 说明 |
|------|--------|---------|------|
| 新增 AA hardfork 名称 | **P0** | `op-core/forks/forks.go` | 在 `All` 列表中添加新 fork（如 `NativeAA`） |
| Hardfork 配置字段 | **P0** | `op-node/rollup/types.go` | Config 结构体添加 `NativeAATime *uint64`，添加 `IsNativeAA()` 方法，`ActivationTime()`/`SetActivationTime()` 添加 case |
| ChainSpec 运行时检查 | **P0** | `op-node/rollup/chain_spec.go` | 添加 `IsNativeAA()` 方法，`CheckForkActivation()` 添加 case |
| XLayer 硬编码时间戳 | **P0** | `op-node/rollup/config_xlayer.go` | `XLayerForkConfig` 添加 `NativeAATime` 字段 |
| **Upgrade deposit tx 生成** | **P0** | `op-node/rollup/derive/attributes.go` | 激活区块的 PayloadAttributes 中注入 6 个系统合约部署 deposit tx |
| 激活区块 sequencing 策略 | **P0** | `op-node/rollup/sequencing/sequencer.go` | 激活区块设置 `NoTxPool=true`（与 Ecotone/Fjord 模式一致） |
| Span batch 解码 0x7B | **P0** | `op-node/rollup/derive/span_batch_tx.go` | `decodeTyped()` 添加 `case 0x7B`，新增 `spanBatchEip8130TxData` 结构体 |
| Span batch → full tx | **P0** | `op-node/rollup/derive/span_batch_tx.go` | `convertToFullTx()` 和 `newSpanBatchTx()` 添加 0x7B case |
| Span batch tx_tos 处理 | **P0** | `op-node/rollup/derive/span_batch_txs.go` | AA 交易无顶层 `to`，设置 contract-creation bit |
| 激活前拒绝 0x7B | **P0** | `op-node/rollup/derive/batches.go` | batch 验证中检查：`!cfg.IsNativeAA(timestamp)` 时 batch 包含 0x7B → 拒绝 |

#### Batcher / op-batcher (Go)

| 改动 | 优先级 | 涉及文件 | 说明 |
|------|--------|---------|------|
| 0x7B 交易编码 | **P0** | `op-node/rollup/derive/span_batch_tx.go` | 编码路径与解码路径在同一文件，`MarshalBinary()` 自动适配 |
| contract-creation bit | **P0** | `op-node/rollup/derive/span_batch_txs.go` | AA 交易无顶层 `to`，span batch 中设置 contract-creation bit |

> **注意**：XLayer 使用 xlayer-reth (Rust) 作为 EL，Go 仓库中的 `op-geth` 不在生产路径上，无需为 AA 修改 op-geth 代码。

### 12.3 协调时间线

```
EL Phase 0-4     Go 无需改动（EL 用单元测试 + mock Engine API 独立验证）
                  ↓
EL Phase 5       Go: 开始 hardfork 配置 + span batch 编解码
                  ↓
EL Phase 6       Go: 完成 upgrade deposit 生成 + 激活区块 sequencing
                  ↓
EL Phase 7       EL + Go 联合 Devnet E2E 测试
```

> **注意**：EL Phase 0-4 可用单元测试和 mock Engine API 独立验证，不需要 Go 侧改动。但 Phase 5 开始需要 Go 侧至少完成 hardfork 配置和 span batch 支持，Phase 6 需要 Go 完成 upgrade deposit 生成。

### 12.4 EL 独立测试策略

在 Go 侧就绪之前，EL 可以用以下方式独立测试：

| 测试场景 | 方法 |
|---------|------|
| AA 交易执行 | Mock Engine API 直接向 EL 发送含 AA tx 的 payload |
| 交易池 | 通过 eth_sendRawTransaction 直接提交 AA tx |
| 共识拒绝 | 构造含 0x7B 的 block，验证 EL 拒绝 |
| Upgrade deposits | Mock Engine API 发送含 upgrade deposit 的 PayloadAttributes |
| 完整 E2E | 需等待 Go 侧就绪 |
