# SQL Rules

These rules apply to Nucleus SQL design, implementation and review. This repository owns the adopted policy. Read applicable sections together with [database operation safety](../database.md#操作安全), the affected business documents and [models and queries](../architecture.md#类型构造与查询). [Rust database-error handling](rust.md#errors-and-validation) also uses [Database failures](#database-failures). No rule here authorizes database changes. Establish fact ownership, transaction boundaries and recovery before choosing tables or fields; naming preferences do not decide business rules.

## SQL correctness

- Keep schema columns, constraints, queries, row decoding, and grants consistent.
- Keep SQL aliases, selected columns, row decoding names, and result types aligned.
- In PostgreSQL `UPDATE ... SET`, do not prefix an assignment target with the target table name or alias. Composite subfields and array subscripts are valid target syntax; inspect what each name denotes before reporting a defect.
- Treat enforced database constraints as guarantees rather than requesting redundant preflight reads.
- Allow application prevalidation to improve feedback without replacing necessary database constraints with a query-before-write check.
- For error classification and transaction outcomes, apply the [database failure rules](#database-failures).

## Identifier naming

- Write every identifier in whole words. Do not shorten a word into an initial, a truncation, or an invented short form, in a table, column, constraint, index, or alias name alike.
- Keep an abbreviation only where it is the written form the domain itself uses, as in `id`, `url`, or `sku`.
- The database's own object-name suffixes are that written form too. Keep `_pkey`, `_fkey`, `_key`, `_idx`, `_excl` and `_check` as [the naming conventions below](#constraint-and-index-names) spell them.
- Shorten a word only where a stated limit leaves no whole-word name, as when a constraint over several columns would pass [the identifier length limit](#constraint-and-index-names). Shorten the fewest words that bring it under, and keep the result readable; convenience is not such a limit.
- Allow database reserved words as identifiers when used legally with the database's required identifier quoting.
- Do not rename an otherwise suitable identifier solely because it is a database reserved word.
- Apply required identifier quoting consistently at every SQL reference, including references to tables, schemas, and columns.

## Data modeling

- Organize tables by business responsibility, fact ownership, and lifecycle.
- Prefer separate models for processes with different purposes or lifecycles; similar fields alone do not justify combining them into one table.
- Different operation names alone do not require separate tables when the records belong to the same responsibility and lifecycle.
- Do not map every flow step to a table or combine independent responsibilities merely to reduce the table count.
- Support meaningful nested groups without prescribing a storage format. Choose the representation from its ownership, constraints, indexes, references, query needs, and maintenance requirements.
- For nested data, define and enforce child-field types, absence conditions, and combination rules in the selected database representation.
- Verify required unique constraints or indexes on the actual nested keys and their business scope. An application structure or input check alone does not establish database enforcement.

## Tables and schemas

- Use one complete English word in lowercase plural form for each table name. Multiword table names are prohibited; do not concatenate words or invent abbreviations to evade this rule.
- Name relationship tables for their business meaning, as in `memberships`, `assignments`, or `grants`.
- Do not use vague relationship names such as `relations` or `mappings`.
- Name schemas for business domains or capabilities.
- Use schemas to distinguish the same table name in different business contexts, as in `billing.accounts` and `ledger.accounts`.
- Reconsider the concepts and their ownership when names collide within a schema.
- Do not invent schema boundaries solely to conceal a naming conflict.
- Qualify cross-schema references with `schema.table`.

## Columns

- Add a column only to answer an actual business, recovery, or validation need; do not add status, reason, or timestamp columns from a template alone.
- Name the primary identifier column `id`.
- Name foreign-key columns for their reference role, as in `user_id` or `approver_id`.
- Use lowercase `snake_case` for column names.
- Keep column names as short as whole words and clear meaning permit.
- Omit information already supplied by the table, as in `users.name`, `roles.code`, and `users.mobile`.
- Allow multiple words when needed for clarity.
- Name Boolean columns with natural state words such as `active`, `enabled`, or `locked`.
- Do not mechanically prefix Boolean column names with `is_`, `has_`, or `can_`.
- Name time-point columns for their event or state transition with `_at`, as in `created_at`, `updated_at`, or `expires_at`.
- Name date-only columns with `_on`, as in `starts_on` or `ends_on`.

## Constraint and index names

- Follow PostgreSQL's default naming conventions for constraints and indexes, except for the explicit check-constraint naming preference below.
- Allow the database to name simple objects when no stable explicit reference is required.
- Explicitly name objects referenced by application error mapping or later migrations and keep those names stable.
- Use `<table>_pkey` for primary keys and `<table>_<columns>_fkey` for foreign keys.
- Use `<table>_<columns>_key` for unique constraints.
- Use `<table>_<columns>_idx` for ordinary indexes.
- Use `<table>_<columns>_excl` for exclusion constraints.
- Use `<table>_<rule>_check` for explicitly named check constraints, as in `products_price_positive_check`.
- List multiple column names in their definition order, as in `memberships_team_id_user_id_key`.
- Add concise condition or rule meaning when naming partial indexes, expression indexes, or complex check constraints.
- Keep names within PostgreSQL's default 63-byte identifier limit rather than relying on automatic truncation.

## Nullability and types

- Derive `NULL` permissions from valid lifecycle states. Prefer construction timing and responsibility boundaries that avoid unnecessary absence.
- Do not split tables artificially solely to eliminate `NULL`.
- Do not fill missing values with meaningless defaults.
- Choose types that accurately express the business meaning of each column.
- Choose quantity, monetary, and ratio types from their actual range and precision requirements.
- Set string length limits only when the business imposes a limit.
- Do not mechanically use `varchar(255)` for every string.
- Model time points, dates, and durations separately.
- Do not represent temporal values with arbitrary strings or unitless numbers.
- Do not store structured fields as JSON solely for insertion convenience.

## Record lifecycle

- Retain short-lived material for its actual dependencies, recovery window, and replay or duplicate-prevention requirements; keep concrete durations in the owning project.
- Delete a complete row when its responsibility has ended and no retention obligation remains, rather than clearing fields and retaining an empty shell.
- Choose foreign keys and deletion actions from the relationship and both sides' lifecycles; do not apply `CASCADE`, `SET NULL`, or omission of foreign keys uniformly.

## Integrity and transactions

- Use `NOT NULL`, `UNIQUE`, foreign keys, and `CHECK` to enforce required values, uniqueness, reference integrity, and suitable row-local conditions.
- Design transaction and concurrency control explicitly for cross-row, cross-table, and business-process invariants.
- Put writes that must succeed or fail together in the same transaction.
- Keep transactions short and avoid waiting for external network requests inside them.
- Choose constraints, locks, or isolation levels for the actual concurrency risk.
- Retry only when the owning use case permits it, rerunning the complete affected transaction with the original operation identity and bounded attempts. Follow [write coordination](../architecture.md#并发协调) to resolve an unknown commit before deciding whether to retry. A database rollback or retry never authorizes repeating a supplier call or another external effect; follow that operation's own recovery contract.

## Database failures

Apply the [project error and redaction contract](../architecture.md#类型构造与查询) and [unknown-commit recovery](../architecture.md#未知提交) to database failures:

- Convert a constraint error into a business rejection only at the boundary that owns that constraint's known business meaning. Keep the conversion in one place; ordinary callers propagate it, and handlers match it when an action depends on that outcome.
- Identify PostgreSQL constraints by SQLSTATE and the exact constraint identity, using schema and table information when needed. Never classify by message text; unavailable identity fields do not justify guessing a business rejection.
- A known constraint may become a business rejection without a database cause when that rejection fully expresses the determined outcome. Preserve any context needed by its callers.
- Keep unmapped constraints, connection failures, and timeouts as technical failures with useful safe structured causes. Keep raw detail and secrets out of errors and logs, following the project's redaction contract.
- Distinguish statement execution, savepoint recovery, and commit completion. A constraint failure, connection interruption, timeout, or missing row alone does not prove the whole transaction remained uncommitted; determine its outcome from the operation stage and available transaction evidence.

## Indexes and queries

- Create indexes for actual query or constraint requirements.
- Do not mechanically index every column.
- Design composite indexes for the relevant filtering, joins, and ordering.
- Check whether existing indexes cover the need before adding another index.
- Verify execution plans for important queries and consider write and storage costs.
- List the required columns in business queries instead of using `SELECT *`.
- Alias a table with whole words, most simply the table name's singular form, as in `invoices invoice`. Do not use an initial or an arbitrary name such as `i`, `t1`, or `t2`.
- Use explicit `ORDER BY` when consumers depend on result order.
- Include a unique key as the final pagination ordering tie-breaker.
- Bind SQL values as parameters rather than concatenating external input.
- Select dynamic table names, column names, and sort directions through fixed mappings or allowlists.
- Prefer parameter-binding query builders when constructing dynamic queries.

## Migrations

- Before changing a migration file, verify whether it has run against a database whose data persists. Commit history or merge state alone does not establish deployment state.
- Edit a migration that has not run against such a database in place. Disposable isolated test databases do not freeze a migration.
- Keep a migration file unchanged after it has run against such a database, and deliver later corrections through new migrations.
- Inspect existing data and impact before deleting data, tightening constraints, or changing column types.
- Specify code and migration execution order when rollout requires stages.
- Use migration tests to verify the resulting schema and critical data transformations.

## References

Adapted from [agents SQL preferences at `730ab31`](https://github.com/silence-force/agents/blob/730ab31266a7b0d1a7391dfce847da51c7214d45/delivery/codex/policy/sql.md). This repository maintains the adopted rules; upstream changes require review, not automatic synchronization. The sources below explain PostgreSQL behavior and alternative conventions; the statements above define Nucleus's choices.

- [PostgreSQL 18: schemas](https://www.postgresql.org/docs/18/ddl-schemas.html)
- [PostgreSQL 18: identifiers](https://www.postgresql.org/docs/18/sql-syntax-lexical.html)
- [PostgreSQL 18: constraints](https://www.postgresql.org/docs/18/ddl-constraints.html)
- [PostgreSQL 18: error codes](https://www.postgresql.org/docs/18/errcodes-appendix.html)
- [PostgreSQL 18: structured error fields](https://www.postgresql.org/docs/18/protocol-error-fields.html)
- [PostgreSQL 18: UPDATE targets](https://www.postgresql.org/docs/18/sql-update.html)
- [PostgreSQL 18: index naming implementation](https://github.com/postgres/postgres/blob/REL_18_STABLE/src/backend/commands/indexcmds.c)
- [PostgreSQL 18: foreign-key naming implementation](https://github.com/postgres/postgres/blob/REL_18_STABLE/src/backend/commands/tablecmds.c)
- [GitLab: alternative constraint naming conventions](https://docs.gitlab.com/development/database/constraint_naming_convention/)
