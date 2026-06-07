# JIT / AOT 性能测试 — 方法论交接

> **目的**:对接新 session / 团队成员,把"我们在做什么、怎么测、用什么基准、走什么流程"讲清楚。
> 不含具体测量数字 — 数据每次 bench 都在变,看 `bench-results/` 目录下的最新 log。

---

## 1. 目标

测出 xlayer-reth 启用 **paradigmxyz/revmc** JIT / AOT 后对 chain 性能的真实影响,跟 nojit(纯 interpreter)对比。

要区分三个层面:
- **Chain 内部 CPU 时间** — `OpBuilder::build` 函数耗时,纯 chain side
- **Effective chain TPS** — chain 在满载时的理论上限
- **端到端 wall-clock TPS** — bench 工具(polycli)给出的数字,含 sender + 网络 + idle

---

## 2. 测试基准(三种 mode)

| Mode | 环境变量 | 含义 |
|---|---|---|
| `nojit` | `XLAYER_JIT_ENABLED=0` | 纯 interpreter,baseline |
| `jit_cold` | `XLAYER_JIT_ENABLED=1` + `XLAYER_AOT_DIR=""` | 运行时 LLVM JIT,无持久化,进程退出丢失 |
| `aot` | `XLAYER_JIT_ENABLED=1` + `XLAYER_AOT_DIR=$PROJECT_ROOT/aot-cache` | 启动时从磁盘 dlopen 预编译好的 dylib |

`aot-cache/` 里目前有 4 个 dylib(EntryPoint / Helper / Pay / TestERC20),靠 `setup-golden.sh` 一次性 warmup 后写盘。

---

## 3. 工作负载

**SA-Bench ERC-4337 paid path**:`polycli loadtest erc4337` → 调 `EntryPoint`,内部走 `PayableAccount` + `Helper` + `TestERC20`,签名验证(WebAuthn secp256r1 / ECDSA)+ ERC20 transfer。

参数在 `/Users/jiezhang/meili/jie.zhang1_dacs_at_okg.com/114/Documents/SA-Benchmark/.env`:

```
TOTAL_UOP=50000      # 50k 个 user operation;BATCH_SIZE=1 → 50k 笔 tx
BATCH_SIZE=1
CONCURRENCY=50       # 50 个 polycli sender 并发
RATE_LIMIT=5000      # 上限 5000 tx/s,高于 chain 容量 → chain 是瓶颈
BUNDLER=50
```

**workload 特征**:大量时间在 **precompile**(ECDSA / modexp 签名验证),JIT 不加速 precompile。这决定 JIT 在该 workload 上的天花板。

---

## 4. 测量手段(我们自加的 timing log)

### Log #1 — `xlayer::bench_timing`(每块一行)

位置:`deps/optimism/rust/op-reth/crates/payload/src/builder.rs` 内 `OpBuilder::build()` 函数,在 4 个 phase 间插 `Instant::now()`。

```
INFO xlayer::bench_timing: block_phase_breakdown
  block=N id=0x... n_total_txs=X gas_used=Y
  pre_exec_us=A     # pre-execution(L1Block 系统预加载)
  seq_txs_us=B      # sequencer tx(L1 deposit)
  pool_exec_us=C    # EVM 执行 mempool tx ← JIT 主战场
  finish_us=D       # state root + receipts root + tx root + block 组装
  total_us=A+B+C+D
```

**测量范围**:`OpBuilder::build` 函数从入到出。
**不含**:dev block-time 1s 节拍 idle、sender RPC、网络 RTT、polycli 内部。

### Log #2 — `xlayer::jit::stats`(每秒一行)

位置:`crates/builder/src/evm_jit/executor.rs` 内 `spawn_periodic_jit_stats(JitBackend)`。

```
INFO xlayer::jit::stats: jit_periodic_stats
  resident=N        # 已编译且 resident 的合约数
  compile_ok=N      # 累计编译成功
  compile_err=N
  dispatched=N      # 已派发编译任务
  lookup_hits=N     # JIT/AOT cache 命中
  lookup_misses=N   # 未命中,走 interpreter
  pending=N
  evictions=N
  code_bytes=N      # JIT 编译代码字节数(AOT 模式为 0,dylib 走 mmap)
```

### Log #3 — polycli `Final TPS`(SA-Bench 自己产出)

在 `SA-Benchmark/result_*.out` 末尾,wall-clock `TPS = TOTAL_UOP / 总耗时秒`。

---

## 5. 流程

### 一次性 setup(每个新机器 / clean repo 后跑一次)

```bash
./setup-golden.sh
```

干啥:启 node → hardhat 部署 13 合约 → 存到 `golden-state/`(~2 MB DB snapshot)。
之后所有 bench 直接 cp `golden-state` → `jit-data`,**跳过 hardhat**(原本要 ~12 min)。

幂等:`golden-state/` 已存在则拒绝覆盖。`--force` 强制重做。

### 单次 bench(单 mode 单 round)

```bash
MODE={nojit|jit_cold|aot} ./run-bench-aot.sh
```

内部流程(`run-bench-aot.sh`):
1. kill 旧 node
2. cp `golden-state/` → `jit-data/`(检测到就走这条;否则降级到 hardhat 部署)
3. start node(`run.sh`,按 MODE 设 `XLAYER_JIT_ENABLED` / `XLAYER_AOT_DIR`)
4. wait for RPC ready
5. polycli warmup 2000 UOP(让 JIT 触发编译热合约)
6. drain txpool
7. polycli 真 bench `TOTAL_UOP`(默认 50k)
8. stop node

输出:
- `bench-results/aot-node-${MODE}-${TIMESTAMP}.log` — node 全部 log(含两条 timing target)
- `bench-results/aot-result-${MODE}-${TIMESTAMP}.out` — polycli 输出 + Final TPS
- `bench-results/aot-deploy-${MODE}-${TIMESTAMP}.log` — hardhat 输出(走 golden state 后通常为空)
- `bench-results/aot-warmup-${MODE}-${TIMESTAMP}.out` — warmup 阶段输出

### 多轮多 mode 驱动

```bash
ROUNDS=2 COOL_DOWN=120 ./run-paid-Nx3.sh
```

顺序:
```
for r in 1..ROUNDS:
    for mode in (nojit, jit_cold, aot):
        sleep COOL_DOWN              # pre-bench cool-down(已改成 bench 前)
        run_bench(mode, r)
```

每个 mode 跑前 sleep,保证起跑线一致。**Cool-down 不能省**,低于 ~120s thermal 不会完全 recovery。

输出目录:`bench-results/paid-${ROUNDS}x3-${TIMESTAMP}/`。

### 数据分析

```bash
python3 scripts/parse-bench-timing.py \
    --label nojit    bench-results/aot-node-nojit-${TS}.log \
    --label jit_cold bench-results/aot-node-jit_cold-${TS}.log \
    --label aot      bench-results/aot-node-aot-${TS}.log
```

聚合策略:
- **过滤 heavy block**(`n_total_txs >= 100`)— 只看真在跑 bench 的块,排除 dev mode 心跳空块
- 每个 phase 输出 p50 / p95 / mean
- 出 Δ vs baseline(第一个 --label 为 baseline)

更详细的分析(per-tx EVM 时间、chain CPU 占 wall-clock 比例、effective chain TPS 等)目前是手写 python ad-hoc 脚本,可以以后整理进 parser。

---

## 6. 关键文件

| 文件 | 角色 |
|---|---|
| `bin/xlayer-reth-jit` | 编译好的 node binary(~280 MB)|
| `golden-state/` | 一次性部署后的 chain state snapshot |
| `aot-cache/` | 4 个 dylib(EntryPoint / Helper / Pay / TestERC20)|
| `setup-golden.sh` | 一次性部署脚本 |
| `run.sh` | 启 node(JIT 由环境变量控制)|
| `run-bench-aot.sh` | 单 mode bench |
| `run-paid-Nx3.sh` | 3 modes × N rounds 驱动 |
| `run-fb.sh` / `run-bench-fb.sh` / `run-paid-Nx3-fb.sh` | flashblocks 变体(未跑过)|
| `scripts/parse-bench-timing.py` | log → 表格 |
| `bench-results/` | 所有 bench 输出 |
| `crates/builder/src/evm_jit/executor.rs` | JIT periodic stats dump 加在这 |
| `deps/optimism/rust/op-reth/crates/payload/src/builder.rs` | phase timing 加在这 |

---

## 6.5 Instrumentation reference (2026-06-07 更新)

完整 schema + 设计动机见:
- `docs/superpowers/specs/2026-06-07-jit-bench-timing-instrumentation-design.md`
- `docs/superpowers/plans/2026-06-07-jit-bench-timing-instrumentation.md`

| 数据点 | 来源 | 输出文件 |
|---|---|---|
| 每个 phase 的 wall-clock | shell `ts_log` | `bench-results/aot-cycle-${MODE}-${TS}.tslog` |
| 每 round × mode + cool_down 的边界 | shell `ts_log_outer` | `bench-results/paid-Nx3-*/paid-Nx3-cycle.tslog` |
| 每块 chain CPU 4 段 + `n_seq_txs` / `n_pool_txs` | `xlayer::bench_timing` info!(op-reth `OpBuilder::build`) | node log |
| 每秒 JIT counter 累计 | `xlayer::jit::stats` info!(`executor.rs`) | node log |
| AOT dlopen 耗时 + 装载 dylib 数 | `xlayer::jit::aot` info!(`build_aot_store_from_env`,含 `dlopen_us` 字段) | node log |
| polycli Final TPS | SA-Bench 自己 emit | `bench-results/aot-result-${MODE}-${TS}.out` |

**Parser 新用法**:
```bash
# 单 run 出 JSON + 4 张终端表
python3 scripts/parse-bench-timing.py \
    --run-dir bench-results \
    --mode aot \
    --timestamp 20260607_143012 \
    [--min-block-txs 100]    # 心跳过滤阈值,默认 100

# 老用法仍兼容(legacy --label mode)
python3 scripts/parse-bench-timing.py \
    --label nojit X.log --label aot Y.log
```

**JSON 关键字段**(`bench-results/cycle-timing-${MODE}-${TS}.json`):
- `wall_clock_by_phase.*_us`: 9 个 phase 微秒数 + `total_run_us`(总)
- `bench_window.{warmup,real}_{start,end}_block`: 4 个 block sentinels + `n_real_blocks` + `n_blocks_filtered_light`
- `chain_blocks_real.phase_us.{pre_exec,seq_txs,pool_exec,finish,total}`: real 窗口内,过滤心跳后,4+1 段 chain CPU 的 p50/p95/mean
- `chain_blocks_real.per_tx_pool_us`: `Σpool_exec_us / Σn_pool_txs` — 单 tx 纯 EVM 时间
- `chain_blocks_real.per_mgas_total_us`: `Σtotal_us / (Σgas_used/1e6)` — 比 us/tx 更稳的归一化指标
- `jit_stats_delta`: real 窗口和 warmup 窗口内的 JIT counter 增量(分开,真正反映 bench 期间 JIT 行为)
- `aot.{dlopen_us, n_dylib_loaded}`: AOT 启动 dlopen 耗时 + 装载 dylib 数(AOT mode 才有)
- `tps.wall_clock_bench_ratio`: chain block CPU 占 polycli wall-clock 的比例(典型 ~10%,即 §9 老问题)
- `meta.warnings`: 数据缺失场景的告警(`tslog_missing` / `tslog_partial` / `no_bench_timing_lines` 等)

**Parser 单元测试**: `python3 -m unittest scripts.tests.test_parser -v` — 8 个 fixture 测试覆盖 complete-run / partial-tslog / no-bench-timing 三类场景

---

## 7. 当前 git 状态(2026-06-07 更新)

| 项 | 值 |
|---|---|
| xlayer-reth 分支 | `xl/jit-timing` |
| 包含 commit | baseline + JIT periodic stats + bump op-reth + AOT dlopen_us + 4 个 shell/parser commit + 多个 reviewer 修正 + docs |
| op-reth submodule 分支 | `xl/jit-bench-timing` |
| 包含 commit | 4-phase timing + `n_seq_txs` / `n_pool_txs`(`cumulative_tx_count` 加入 `ExecutionInfo`) |
| 是否已 push | 见 Task 16 完成状态 |

---

## 8. 已知噪声源 + 应对

| 噪声 | 应对 |
|---|---|
| **Thermal**(macOS turbo headroom 降频)| `COOL_DOWN` 在 bench 前,至少 120s,推荐 300s |
| **Mode 顺序固定**(nojit→jit→aot)| 待做:driver 加 mode 顺序轮换 |
| **Block 大小分布随机**(polycli batch 时序)| 过滤 heavy block;多轮取聚合;看 `us/Mgas` 比 `us/tx` 略稳 |
| **OS file cache 累积**(多轮跑下来污染)| golden state 每次 cp;`jit-data` 每次清(start_node 内)|
| **AOT 永远当轮第 3**(测序偏置)| Mode 顺序轮换;或 Run 之间增加 ~5min cool |

---

## 9. 三个层面 TPS 各自含义(避免概念混淆)

| 名字 | 公式 | 含义 | 噪声 |
|---|---|---|---|
| **polycli Final TPS** | `TOTAL_UOP / wall-clock` | 含 sender 反馈,SA-Bench 直接输出 | 中 |
| **Effective chain TPS** | `sum(n_tx) / sum(total_us)` | chain CPU 满载理论上限 | 小 |
| **per-tx EVM us/tx** | `sum(pool_exec_us) / sum(n_tx)` | 单 tx 纯 EVM 时间 | 大(随 block 大小漂)|

**最稳的对比指标:Effective chain TPS** 或 `Σtotal_us / Σn_tx`(per-tx chain CPU)。

**注意**:`pool_exec + finish` 占 chain block 内部 99.9%,但 chain block 时间只占 polycli wall-clock 的 ~10%。这两个百分比分母不同,容易混淆。

---

## 10. 一些重要约束

- **不修改 paradigmxyz/revmc**(用 upstream main + git rev,0 私有 patch)
- **不修改 reth core** 的 trait(只在 xlayer-reth 加 wrapper)
- op-reth submodule 改动会推到 `xl/jit-bench-timing` 分支
- xlayer-reth 改动会推到 `xl/jit-timing` 分支
- 都 push 到 gitlab(自动 mirror 到 GitHub)

---

## 11. 决策点 / 下一步候选

| 选项 | 工作量 | 解决什么 |
|---|---|---|
| A. Mode 顺序轮换 | 改 driver ~30 lines | 消除测序偏置 |
| B. Storage-heavy workload | 写 SSTORE 循环合约 + 部署 + 跑 polycli | 验证 JIT 在合适 workload 下真实上限 |
| C. flashblocks 模式 bench | 已写好 `run-fb.sh`,直接跑 | 看 sequencer 主战场 |
| D. polycli RPC 拆开计时 | 改 polycli 加 instrumentation | 把 wall-clock 89% 那块拆开 |
| E. Commit + push 现有改动 | 几个 git commit | 工作落到 gitlab |
| F. 收尾写 lark 报告 | 整理表格 | 输出结论 |

---

## 12. 一句话快速 brief

> 接着 xlayer-reth `xl/jit-timing` 分支的 bench 工作。已经在 op-reth `OpBuilder::build` + xlayer-builder `executor.rs` 加了 `xlayer::bench_timing` + `xlayer::jit::stats` 两条 log。bench 流程用 `setup-golden.sh` + `run-paid-Nx3.sh` 跑 3 mode × N round,cool-down 已在 bench 前。Parser 在 `scripts/parse-bench-timing.py`。最新数据在 `bench-results/`。

---

*最后更新:每次 bench 后 `bench-results/` 会刷新,这个文件本身是方法论文档,数据看实测。*
