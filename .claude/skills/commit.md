# Skill: commit

Generate a well-formed conventional commit message for the current staged changes and create the commit.

TRIGGER when: user asks to "commit", "create a commit", "write a commit message", or uses `/commit`.
DO NOT TRIGGER when: user is asking about git history or how commits work in general.

## Instructions

### 1. Review Staged Changes

Run `git diff --staged` to see all staged changes. If nothing is staged, run `git status` and inform the user.

### 2. Determine Commit Type

Choose the appropriate conventional commit type based on the changes:

| Type | When to use |
|------|-------------|
| `feat` | New feature or capability added |
| `fix` | Bug fix |
| `refactor` | Code restructuring without behavior change |
| `perf` | Performance improvement |
| `test` | Adding or updating tests |
| `docs` | Documentation only changes |
| `chore` | Build process, dependencies, tooling |
| `ci` | CI/CD configuration changes |
| `style` | Code style/formatting only (no logic change) |

### 3. Determine Scope (optional)

Use the crate or component name as the scope if the change is isolated:
- `feat(rpc)`: changes in `crates/rpc/`
- `fix(chainspec)`: changes in `crates/chainspec/`
- `feat(flashblocks)`: changes in `crates/flashblocks/`
- `chore(builder)`: changes in `crates/builder/`
- Omit scope for cross-cutting changes

### 4. Write the Commit Message

Follow this format:
```
<type>(<scope>): <short description>

<optional body explaining WHY the change was made>

<optional footer with breaking changes or issue references>
```

Rules:
- Subject line: imperative mood, lowercase, no period, max 72 chars
- Body: explain motivation and context, not what the diff shows
- Breaking changes: prefix footer with `BREAKING CHANGE:`
- Reference issues: `Closes #<number>` or `Fixes #<number>`

### 5. Create the Commit

Stage the appropriate files and create the commit with the generated message.

Example commit messages:
```
feat(rpc): add eth_getTransactionByHash override for XLayer

fix(chainspec): correct genesis block timestamp for mainnet

refactor(builder): extract payload validation into separate module

docs: update BUILDING_ON_RETH guide with database examples

chore: bump reth dependencies to v1.4.0
```
