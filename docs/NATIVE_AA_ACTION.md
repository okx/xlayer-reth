# Native AA 开发行动规范

## 每阶段开发流程

```
1. 编写代码
   - 对照 NATIVE_AA_DESIGN.md 对应 Phase 的 TDD 顺序
   - 先写测试（RED），再写实现（GREEN）

2. Codex Review 代码
   - 将代码交给 Codex review
   - Claude 与 Codex 讨论分歧，逐条解决
   - 修改代码直到双方都满意

3. 补全测试
   - 补充遗漏的边界用例和错误路径
   - 确保覆盖设计文档中列出的所有 RED 用例

4. Codex Review 测试
   - 将测试代码交给 Codex review
   - Claude 与 Codex 讨论分歧，逐条解决
   - 修改测试直到双方都满意

5. 验收
   - cargo test 对应 crate 全绿
   - cargo clippy 无警告
   - 双方确认通过 → 进入下一阶段
```

## 不可测试项处理

- 记录到 `docs/NATIVE_AA_DEFERRED.md`
- 格式：阶段 + 项目 + 原因 + 前置条件
- 不阻塞后续阶段

## 自检清单（每步必查）

- [ ] 当前改动是否符合 NATIVE_AA_DESIGN.md 对应阶段的描述？
- [ ] 是否遵循本文档的流程（代码→review→测试→review→验收）？
- [ ] 是否有跳过的项需要记录到 DEFERRED 文档？
