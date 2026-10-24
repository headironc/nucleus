# Repository Instructions

## Working in This Repository

- Before repository work, read [the documentation index and maintenance rules](docs/README.md) and [design and implementation boundaries](docs/engineering/delivery.md#设计与实施边界), including the implementation-slice rules. Apply both as mandatory instructions.
- Inspect the current source and working tree to establish delivered behavior. Preserve unrelated edits, including unfinished work. Do not restore removed implementations or use historical layouts as the target design.
- Project rules and their required references live in this repository and take precedence over personal preferences within their declared scope. If a required source is unavailable, report the gap and pause only work that depends on it.

## Required Reading by Task

- Before domain decisions, read [the capability overview](docs/overview.md), [the architecture and reconstruction scope](docs/architecture.md), then the affected owners in the documentation index. The architecture owns shared transaction, construction, recovery and runtime targets. Scheduling work also requires [Schedule's authority and reading order](docs/engineering/capacity.md#schedule-接入依据).
- Before database work or commands, read [database operation safety](docs/database.md#操作安全). Before changing migrations, also read [migration rules](docs/engineering/sql.md#migrations).
- Before designing, changing or reviewing Rust, read the applicable [Rust rules](docs/engineering/rust.md). For SQL or database behavior, read the applicable [SQL rules](docs/engineering/sql.md).
- Follow the affected contracts for [settings](docs/settings.md), [identifiers](docs/identifiers.md), [HTTP](docs/http/api.md), [avatars](docs/identity/avatar.md) and [local files](docs/storage/files.md). Read their required references within the stated scope.
- Before validation, apply [check scope and execution boundaries](docs/engineering/verification.md#检查范围). Use the applicable commands from [documentation maintenance](docs/README.md#文档维护), [Rust workspace validation](docs/engineering/rust.md#workspace-validation), and [database verification](docs/database.md#交付验证). Reuse [design evidence](docs/engineering/evidence.md) only for the revisions and behavior it covers.

## Commits and Review

- Commit, push, open a PR, merge or deploy only with authorization for that action and target. A request to edit or review does not authorize publication. Reuse valid authorization without asking again.
- Use signed atomic Conventional Commits such as `docs(instructions): clarify implementation scope`, with short imperative subjects. Preserve available authoring-model and session trailers. Stage only the intended slice, including its documentation and validation; keep intermediate states runnable.
- PRs target `main`. Describe the outcome, reason, checks and remaining limitations; link relevant issues. Preserve a PR-number suffix when assembling its merge commit.
- Before merge, obtain review coverage for the actual candidate and target baseline, resolve blocking findings and record dispositions for suggestions. Check the actual GitHub protections and reviewer runtime state; workflow YAML or a green job alone does not prove review coverage. Review success does not authorize merge.

## Code Review Rules

- Read the affected business owners and applicable local Rust/SQL sections before judging the change. Flag violations of the selected outcome, required invariants or failure recovery, with a concrete trigger and consequence.
- Flag a PR that combines independently acceptable slices, builds facilities without a current consumer, or omits database protections, recovery or acceptance evidence required by its advertised outcome.
- Check safe error reporting and controlled construction against [types and queries](docs/architecture.md#类型构造与查询), and original-operation binding and unknown outcomes against [operation recovery](docs/architecture.md#原操作与恢复). Formatting and lint enforcement belong to CI. Report coverage limits and unrelated existing defects separately from blockers introduced or affected by the candidate.
