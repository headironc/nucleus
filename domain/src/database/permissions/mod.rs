//! Runtime privileges: one manifest, the grants generated from it and the catalog check.
//!
//! Deployment applies [`grants`] after its migrations, and deployment and startup call
//! [`verify`]. Both read only the manifest, so a privilege changes in one place.

mod manifest;

use platform::database::User;
use tokio_postgres::Client;

use crate::database::Error;

/// Every privilege the runtime identity holds, and the definer functions that act for it.
pub(crate) struct Manifest {
    /// Schemas with USAGE.
    schemas: &'static [&'static str],
    /// Types and domains with USAGE.
    types: &'static [&'static str],
    /// Sequences with USAGE.
    sequences: &'static [&'static str],
    /// Function signatures with EXECUTE.
    functions: &'static [&'static str],
    tables: &'static [Table],
    /// `SECURITY DEFINER` functions and the relation whose owner must own them. They are
    /// verified, not granted: each has an empty search path and no PUBLIC EXECUTE.
    definers: &'static [(&'static str, &'static str)],
}

/// Table-level privileges and the columns open to INSERT and UPDATE, each a comma-separated
/// column list as in the GRANT statement.
pub(crate) struct Table {
    name: &'static str,
    privileges: &'static [TablePrivilege],
    insert: &'static str,
    update: &'static str,
}

#[derive(Clone, Copy)]
pub(crate) enum TablePrivilege {
    Select,
    Insert,
    Update,
    Delete,
}

const fn table(
    name: &'static str,
    privileges: &'static [TablePrivilege],
    insert: &'static str,
    update: &'static str,
) -> Table {
    Table {
        name,
        privileges,
        insert,
        update,
    }
}

/// One privilege on one object: the unit that the grants and the catalog check share.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Privilege {
    object: Object,
    name: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Object {
    Schema(&'static str),
    Type(&'static str),
    Sequence(&'static str),
    Function(&'static str),
    Table(&'static str),
    Column(&'static str, &'static str),
}

impl Object {
    fn kind(self) -> &'static str {
        match self {
            Self::Schema(_) => "schema",
            Self::Type(_) => "type",
            Self::Sequence(_) => "sequence",
            Self::Function(_) => "function",
            Self::Table(_) => "table",
            Self::Column(..) => "column",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Schema(name)
            | Self::Type(name)
            | Self::Sequence(name)
            | Self::Function(name)
            | Self::Table(name)
            | Self::Column(name, _) => name,
        }
    }

    fn attribute(self) -> &'static str {
        match self {
            Self::Column(_, column) => column,
            _ => "",
        }
    }
}

/// Expand the manifest into single privileges, in declaration order.
fn privileges(manifest: &Manifest) -> Vec<Privilege> {
    let mut privileges = Vec::new();
    let mut add = |object, name| privileges.push(Privilege { object, name });
    for schema in manifest.schemas {
        add(Object::Schema(schema), "USAGE");
    }
    for name in manifest.types {
        add(Object::Type(name), "USAGE");
    }
    for sequence in manifest.sequences {
        add(Object::Sequence(sequence), "USAGE");
    }
    for function in manifest.functions {
        add(Object::Function(function), "EXECUTE");
    }
    for table in manifest.tables {
        for privilege in table.privileges {
            let name = match privilege {
                TablePrivilege::Select => "SELECT",
                TablePrivilege::Insert => "INSERT",
                TablePrivilege::Update => "UPDATE",
                TablePrivilege::Delete => "DELETE",
            };
            add(Object::Table(table.name), name);
        }
        for (columns, name) in [(table.insert, "INSERT"), (table.update, "UPDATE")] {
            for column in columns
                .split(',')
                .map(str::trim)
                .filter(|column| !column.is_empty())
            {
                add(Object::Column(table.name, column), name);
            }
        }
    }
    privileges
}

/// The runtime grants, which the deployment identity runs after its migrations with
/// `nucleus.runtime_user` bound.
///
/// The statement first revokes every runtime grant on objects of the deployment identity, so
/// repeating it on an existing database leaves exactly the manifest. Dependent grants made
/// through that identity are revoked as well; independent grants to other roles are preserved.
pub fn grants() -> String {
    render(&privileges(&manifest::MANIFEST))
}

fn render(privileges: &[Privilege]) -> String {
    let mut sql = String::from(RESET);
    let mut index = 0;
    while let Some(privilege) = privileges.get(index) {
        index += 1;
        let target = match privilege.object {
            Object::Schema(name) => format!("ON SCHEMA {}", quoted(name)),
            Object::Type(name) => format!("ON TYPE {}", quoted(name)),
            Object::Sequence(name) => format!("ON SEQUENCE {}", quoted(name)),
            Object::Function(signature) => format!("ON FUNCTION {signature}"),
            Object::Table(name) => format!("ON TABLE {}", quoted(name)),
            Object::Column(name, column) => {
                // Consecutive columns of one table and privilege share one statement.
                let mut columns = vec![quoted(column)];
                while let Some(Privilege {
                    object: Object::Column(next, column),
                    name: next_privilege,
                }) = privileges.get(index)
                {
                    if *next != name || *next_privilege != privilege.name {
                        break;
                    }
                    columns.push(quoted(column));
                    index += 1;
                }
                format!("({}) ON TABLE {}", columns.join(", "), quoted(name))
            }
        };
        // Manifest names are plain identifiers and signatures; a test rejects quote characters.
        sql.push_str(&format!(
            "    EXECUTE pg_catalog.format('GRANT {} {target} TO %I', runtime_user);\n",
            privilege.name
        ));
    }
    sql.push_str("END\n$grants$;\n");
    sql
}

fn quoted(name: &str) -> String {
    name.split('.')
        .map(|part| format!("\"{part}\""))
        .collect::<Vec<_>>()
        .join(".")
}

/// Revoke every runtime grant on the schemas, relations, routines and types that the
/// deployment identity owns, before the manifest grants are added again.
const RESET: &str = r#"DO $grants$
DECLARE
    runtime_user text := pg_catalog.current_setting('nucleus.runtime_user');
    schema text;
    qualified text;
BEGIN
    FOR schema IN SELECT namespace.nspname FROM pg_catalog.pg_namespace namespace
        WHERE namespace.nspowner = current_user::regrole
    LOOP
        EXECUTE pg_catalog.format('REVOKE ALL ON SCHEMA %I FROM %I CASCADE', schema, runtime_user);
        EXECUTE pg_catalog.format('REVOKE ALL ON ALL TABLES IN SCHEMA %I FROM %I CASCADE', schema, runtime_user);
        EXECUTE pg_catalog.format('REVOKE ALL ON ALL SEQUENCES IN SCHEMA %I FROM %I CASCADE', schema, runtime_user);
        EXECUTE pg_catalog.format('REVOKE ALL ON ALL ROUTINES IN SCHEMA %I FROM %I CASCADE', schema, runtime_user);
    END LOOP;
    FOR qualified IN SELECT pg_catalog.format('%I.%I', namespace.nspname, element.typname)
        FROM pg_catalog.pg_type element
        JOIN pg_catalog.pg_namespace namespace ON namespace.oid = element.typnamespace
        WHERE namespace.nspowner = current_user::regrole AND element.typowner = current_user::regrole
            AND (element.typrelid = 0 OR EXISTS (SELECT 1 FROM pg_catalog.pg_class relation
                WHERE relation.oid = element.typrelid AND relation.relkind = 'c'))
            AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_type base WHERE base.typarray = element.oid)
    LOOP
        EXECUTE pg_catalog.format('REVOKE ALL ON TYPE %s FROM %I CASCADE', qualified, runtime_user);
    END LOOP;
"#;

/// One way in which the effective catalog differs from the manifest.
#[derive(Debug, PartialEq, Eq)]
struct Difference {
    excess: bool,
    kind: String,
    object: String,
    attribute: String,
    privilege: String,
}

/// Check the runtime identity against the manifest and the fixed role and session boundary.
pub async fn verify(client: &Client, user: &User) -> Result<(), Error> {
    if !compare(client, user, &privileges(&manifest::MANIFEST))
        .await?
        .is_empty()
    {
        return Err(Error::Permissions);
    }
    let (routines, owners): (Vec<_>, Vec<_>) = manifest::MANIFEST.definers.iter().copied().unzip();
    let valid: bool = client
        .query_one(BOUNDARY, &[user, &routines, &owners])
        .await?
        .try_get(0)?;
    if !valid {
        return Err(Error::Permissions);
    }
    Ok(())
}

/// Compare in both directions. Each declared privilege must be held directly, through an
/// inherited role or through PUBLIC; no other privilege may be reachable through any role the
/// identity can use, including roles it can only SET ROLE to. Table-level privileges cover their
/// columns, and every schema outside the system catalogs is in scope. Grant options are
/// separate privileges and are never part of the runtime manifest.
async fn compare(
    client: &Client,
    user: &User,
    privileges: &[Privilege],
) -> Result<Vec<Difference>, Error> {
    let kinds: Vec<_> = privileges.iter().map(|item| item.object.kind()).collect();
    let objects: Vec<_> = privileges.iter().map(|item| item.object.name()).collect();
    let attributes: Vec<_> = privileges
        .iter()
        .map(|item| item.object.attribute())
        .collect();
    let names: Vec<_> = privileges.iter().map(|item| item.name).collect();
    let rows = client
        .query(COMPARISON, &[user, &kinds, &objects, &attributes, &names])
        .await?;
    rows.iter()
        .map(|row| {
            Ok(Difference {
                excess: row.try_get(0)?,
                kind: row.try_get(1)?,
                object: row.try_get(2)?,
                attribute: row.try_get(3)?,
                privilege: row.try_get(4)?,
            })
        })
        .collect()
}

const COMPARISON: &str = r#"
WITH expected AS (
    SELECT declared.kind, declared.object, declared.attribute, declared.privilege,
        CASE declared.kind
            WHEN 'schema' THEN pg_catalog.to_regnamespace(declared.object)::oid
            WHEN 'type' THEN pg_catalog.to_regtype(declared.object)::oid
            WHEN 'function' THEN pg_catalog.to_regprocedure(declared.object)::oid
            ELSE pg_catalog.to_regclass(declared.object)::oid
        END AS target
    FROM unnest($2::text[], $3::text[], $4::text[], $5::text[])
        AS declared(kind, object, attribute, privilege)
), members AS (
    SELECT role.oid FROM pg_catalog.pg_roles role
    WHERE pg_catalog.pg_has_role($1::text, role.oid, 'SET')
), namespaces AS (
    SELECT namespace.oid FROM pg_catalog.pg_namespace namespace
    WHERE namespace.nspname !~ '^pg_' AND namespace.nspname <> 'information_schema'
), relations AS (
    SELECT relation.oid, relation.relkind FROM pg_catalog.pg_class relation
    WHERE relation.relnamespace IN (SELECT oid FROM namespaces)
), reachable(kind, target, attribute, privilege) AS (
    SELECT 'schema', namespace.oid, '', privilege.name
    FROM namespaces namespace, unnest(ARRAY['USAGE', 'CREATE',
        'USAGE WITH GRANT OPTION', 'CREATE WITH GRANT OPTION']) privilege(name)
    WHERE EXISTS (SELECT 1 FROM members
        WHERE pg_catalog.has_schema_privilege(members.oid, namespace.oid, privilege.name))
    UNION ALL
    SELECT 'table', relation.oid, '', privilege.name
    FROM relations relation, unnest(ARRAY['SELECT', 'INSERT', 'UPDATE', 'DELETE', 'TRUNCATE',
        'REFERENCES', 'TRIGGER', 'MAINTAIN', 'SELECT WITH GRANT OPTION',
        'INSERT WITH GRANT OPTION', 'UPDATE WITH GRANT OPTION', 'DELETE WITH GRANT OPTION',
        'TRUNCATE WITH GRANT OPTION', 'REFERENCES WITH GRANT OPTION',
        'TRIGGER WITH GRANT OPTION', 'MAINTAIN WITH GRANT OPTION']) privilege(name)
    WHERE relation.relkind IN ('r', 'p', 'v', 'm', 'f') AND EXISTS (SELECT 1 FROM members
        WHERE pg_catalog.has_table_privilege(members.oid, relation.oid, privilege.name))
    UNION ALL
    SELECT 'column', relation.oid, attribute.attname::text, privilege.name
    FROM relations relation
    JOIN pg_catalog.pg_attribute attribute ON attribute.attrelid = relation.oid,
        unnest(ARRAY['SELECT', 'INSERT', 'UPDATE', 'REFERENCES', 'SELECT WITH GRANT OPTION',
            'INSERT WITH GRANT OPTION', 'UPDATE WITH GRANT OPTION',
            'REFERENCES WITH GRANT OPTION']) privilege(name)
    WHERE relation.relkind IN ('r', 'p', 'v', 'm', 'f')
        AND attribute.attnum > 0 AND NOT attribute.attisdropped
        AND EXISTS (SELECT 1 FROM members WHERE pg_catalog.has_column_privilege(
            members.oid, relation.oid, attribute.attnum, privilege.name))
    UNION ALL
    SELECT 'sequence', relation.oid, '', privilege.name
    FROM relations relation, unnest(ARRAY['USAGE', 'SELECT', 'UPDATE', 'USAGE WITH GRANT OPTION',
        'SELECT WITH GRANT OPTION', 'UPDATE WITH GRANT OPTION']) privilege(name)
    WHERE relation.relkind = 'S' AND EXISTS (SELECT 1 FROM members
        WHERE pg_catalog.has_sequence_privilege(members.oid, relation.oid, privilege.name))
    UNION ALL
    SELECT 'function', routine.oid, '', privilege.name
    FROM pg_catalog.pg_proc routine,
        unnest(ARRAY['EXECUTE', 'EXECUTE WITH GRANT OPTION']) privilege(name)
    WHERE routine.pronamespace IN (SELECT oid FROM namespaces) AND EXISTS (SELECT 1 FROM members
        WHERE pg_catalog.has_function_privilege(members.oid, routine.oid, privilege.name))
    UNION ALL
    SELECT 'type', element.oid, '', privilege.name
    FROM pg_catalog.pg_type element,
        unnest(ARRAY['USAGE', 'USAGE WITH GRANT OPTION']) privilege(name)
    WHERE element.typnamespace IN (SELECT oid FROM namespaces)
        AND (element.typrelid = 0
            OR element.typrelid IN (SELECT oid FROM relations WHERE relkind = 'c'))
        AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_type base WHERE base.typarray = element.oid)
        AND EXISTS (SELECT 1 FROM members
            WHERE pg_catalog.has_type_privilege(members.oid, element.oid, privilege.name))
)
SELECT false, expected.kind, expected.object, expected.attribute, expected.privilege
FROM expected
WHERE NOT COALESCE(CASE expected.kind
    WHEN 'schema' THEN
        pg_catalog.has_schema_privilege($1::text, expected.target, expected.privilege)
    WHEN 'type' THEN
        pg_catalog.has_type_privilege($1::text, expected.target, expected.privilege)
    WHEN 'function' THEN
        pg_catalog.has_function_privilege($1::text, expected.target, expected.privilege)
    WHEN 'sequence' THEN
        pg_catalog.has_sequence_privilege($1::text, expected.target, expected.privilege)
    WHEN 'table' THEN
        pg_catalog.has_table_privilege($1::text, expected.target, expected.privilege)
    WHEN 'column' THEN pg_catalog.has_column_privilege($1::text, expected.target,
        (SELECT attribute.attnum FROM pg_catalog.pg_attribute attribute
            WHERE attribute.attrelid = expected.target AND attribute.attname = expected.attribute
                AND attribute.attnum > 0 AND NOT attribute.attisdropped),
        expected.privilege)
END, false)
UNION ALL
SELECT true, reachable.kind, CASE reachable.kind
        WHEN 'schema' THEN reachable.target::regnamespace::text
        WHEN 'type' THEN reachable.target::regtype::text
        WHEN 'function' THEN reachable.target::regprocedure::text
        ELSE reachable.target::regclass::text
    END, reachable.attribute, reachable.privilege
FROM reachable
WHERE NOT EXISTS (SELECT 1 FROM expected
    WHERE expected.target = reachable.target AND expected.privilege = reachable.privilege
        AND (expected.kind = reachable.kind AND expected.attribute = reachable.attribute
            OR expected.kind = 'table' AND reachable.kind = 'column'))
ORDER BY 1, 2, 3, 4, 5
"#;

/// Session, database, role, ownership and definer conditions outside object privileges.
const BOUNDARY: &str = r#"
SELECT pg_catalog.current_setting('session_replication_role') <> 'replica'
    AND pg_catalog.has_database_privilege($1::text, pg_catalog.current_database(), 'CONNECT')
    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members membership
        WHERE membership.admin_option
            AND pg_catalog.pg_has_role($1::text, membership.member, 'SET'))
    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles forbidden
        WHERE forbidden.rolname IN ('pg_read_server_files', 'pg_write_server_files',
            'pg_execute_server_program')
            AND pg_catalog.pg_has_role($1::text, forbidden.oid, 'MEMBER'))
    AND NOT EXISTS (SELECT 1 FROM unnest($2::text[], $3::text[]) definer(routine, relation)
        WHERE NOT EXISTS (SELECT 1 FROM pg_catalog.pg_proc routine
            WHERE routine.oid = pg_catalog.to_regprocedure(definer.routine)
                AND routine.prosecdef AND routine.proconfig = ARRAY['search_path=""']
                AND routine.proowner = (SELECT relation.relowner FROM pg_catalog.pg_class relation
                    WHERE relation.oid = pg_catalog.to_regclass(definer.relation))
                AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE(routine.proacl,
                        pg_catalog.acldefault('f', routine.proowner))) privilege
                    WHERE privilege.grantee = 0 AND privilege.privilege_type = 'EXECUTE')))
    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles role
        WHERE pg_catalog.pg_has_role($1::text, role.oid, 'SET') AND (
            role.rolsuper OR role.rolcreaterole OR role.rolcreatedb OR role.rolreplication
            OR role.rolbypassrls
            OR pg_catalog.has_parameter_privilege(role.oid, 'session_replication_role',
                'SET,ALTER SYSTEM')
            OR pg_catalog.has_database_privilege(role.oid, pg_catalog.current_database(),
                'CREATE,TEMP,CONNECT WITH GRANT OPTION')
            OR EXISTS (SELECT 1 FROM pg_catalog.pg_namespace namespace
                WHERE pg_catalog.pg_has_role(role.oid, namespace.nspowner, 'USAGE')
                    OR pg_catalog.has_schema_privilege(role.oid, namespace.oid, 'CREATE'))
            OR EXISTS (SELECT 1 FROM pg_catalog.pg_class relation
                JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace
                WHERE namespace.nspname !~ '^pg_' AND namespace.nspname <> 'information_schema'
                    AND pg_catalog.pg_has_role(role.oid, relation.relowner, 'USAGE'))
            OR EXISTS (SELECT 1 FROM pg_catalog.pg_proc routine
                JOIN pg_catalog.pg_namespace namespace ON namespace.oid = routine.pronamespace
                WHERE namespace.nspname !~ '^pg_' AND namespace.nspname <> 'information_schema'
                    AND pg_catalog.pg_has_role(role.oid, routine.proowner, 'USAGE'))
            OR EXISTS (SELECT 1 FROM pg_catalog.pg_type element
                JOIN pg_catalog.pg_namespace namespace ON namespace.oid = element.typnamespace
                WHERE namespace.nspname !~ '^pg_' AND namespace.nspname <> 'information_schema'
                    AND pg_catalog.pg_has_role(role.oid, element.typowner, 'USAGE'))))
"#;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{collections::HashSet, time::Duration};

    use platform::database::User;
    use tokio::{runtime::Runtime, time::timeout};

    use super::{
        Difference, Object, Privilege, compare, grants, manifest::MANIFEST, privileges, render,
        verify,
    };
    use crate::identity::authentication::{Database, server};

    fn difference(excess: bool, privilege: Privilege) -> Difference {
        Difference {
            excess,
            kind: privilege.object.kind().into(),
            object: privilege.object.name().into(),
            attribute: privilege.object.attribute().into(),
            privilege: privilege.name.into(),
        }
    }

    #[test]
    fn manifest_entries_are_unique_plain_names() {
        let declared = privileges(&MANIFEST);
        let unique: HashSet<_> = declared.iter().collect();
        assert_eq!(
            unique.len(),
            declared.len(),
            "a privilege is declared twice"
        );
        let definers = MANIFEST
            .definers
            .iter()
            .flat_map(|(routine, relation)| [*routine, *relation]);
        // Plain names need no escaping inside the generated format strings.
        for name in declared
            .iter()
            .flat_map(|privilege| [privilege.object.name(), privilege.object.attribute()])
            .chain(definers)
        {
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_.(),".contains(c)),
                "{name}"
            );
        }
    }

    #[test]
    fn grants_group_consecutive_columns_of_one_privilege() {
        let sql = render(&[
            Privilege {
                object: Object::Table("workforce.rules"),
                name: "SELECT",
            },
            Privilege {
                object: Object::Column("workforce.rules", "id"),
                name: "INSERT",
            },
            Privilege {
                object: Object::Column("workforce.rules", "position"),
                name: "INSERT",
            },
            Privilege {
                object: Object::Column("workforce.rules", "position"),
                name: "UPDATE",
            },
            Privilege {
                object: Object::Function("workforce.lock_directory()"),
                name: "EXECUTE",
            },
        ]);
        let statements: Vec<_> = sql.lines().filter(|line| line.contains("'GRANT")).collect();
        assert_eq!(
            statements,
            [
                r#"    EXECUTE pg_catalog.format('GRANT SELECT ON TABLE "workforce"."rules" TO %I', runtime_user);"#,
                r#"    EXECUTE pg_catalog.format('GRANT INSERT ("id", "position") ON TABLE "workforce"."rules" TO %I', runtime_user);"#,
                r#"    EXECUTE pg_catalog.format('GRANT UPDATE ("position") ON TABLE "workforce"."rules" TO %I', runtime_user);"#,
                r#"    EXECUTE pg_catalog.format('GRANT EXECUTE ON FUNCTION workforce.lock_directory() TO %I', runtime_user);"#,
            ]
        );
        assert!(sql.starts_with("DO $grants$") && sql.ends_with("END\n$grants$;\n"));
    }

    #[test]
    fn migrated_catalog_equals_the_manifest_in_both_directions() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(60), async {
                let database = Database::new(server).await;
                let user = User::new(database.user.clone()).expect("runtime user");
                let admin = database
                    .connections("postgres")
                    .recover()
                    .await
                    .expect("fixture administrator");
                let declared = privileges(&MANIFEST);
                assert_eq!(
                    compare(admin.client(), &user, &declared)
                        .await
                        .expect("comparison"),
                    []
                );
                for (removed, target) in [
                    (Object::Schema("center"), "SCHEMA center"),
                    (Object::Type("identity.mobile"), "TYPE identity.mobile"),
                    (
                        Object::Sequence("identity.ordinals"),
                        "SEQUENCE identity.ordinals",
                    ),
                    (
                        Object::Function("identity.cancelling(uuid)"),
                        "FUNCTION identity.cancelling(uuid)",
                    ),
                    (Object::Table("identity.claims"), "TABLE identity.claims"),
                    (
                        Object::Column("identity.challenges", "id"),
                        "TABLE identity.challenges",
                    ),
                ] {
                    let removed = declared
                        .iter()
                        .copied()
                        .find(|privilege| privilege.object == removed && privilege.name != "SELECT")
                        .expect("declared privilege");
                    // Without its manifest entry the migrated grant is reported as excess.
                    let reduced: Vec<_> = declared
                        .iter()
                        .copied()
                        .filter(|privilege| *privilege != removed)
                        .collect();
                    assert_eq!(
                        compare(admin.client(), &user, &reduced)
                            .await
                            .expect("comparison"),
                        [difference(true, removed)]
                    );
                    // Without the grant the manifest entry is reported as missing.
                    let privilege = match removed.object {
                        Object::Column(_, column) => format!("{} ({column})", removed.name),
                        _ => removed.name.into(),
                    };
                    // Interpolated names are fixed test inputs and the generated role.
                    admin
                        .client()
                        .batch_execute(&format!(
                            "REVOKE {privilege} ON {target} FROM {}",
                            database.user
                        ))
                        .await
                        .expect("remove one grant");
                    assert_eq!(
                        compare(admin.client(), &user, &declared)
                            .await
                            .expect("comparison"),
                        [difference(false, removed)]
                    );
                    assert!(matches!(
                        verify(admin.client(), &user).await,
                        Err(crate::database::Error::Permissions)
                    ));
                    admin
                        .client()
                        .batch_execute(&format!(
                            "GRANT {privilege} ON {target} TO {}",
                            database.user
                        ))
                        .await
                        .expect("restore the grant");
                }
                verify(admin.client(), &user)
                    .await
                    .expect("restored manifest");
                admin.terminate().await.expect("administrator ended");
            })
            .await
            .expect("comparison budget");
        });
    }

    #[test]
    fn grant_options_are_rejected_directly_and_through_roles() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for membership in [None, Some("INHERIT TRUE"), Some("INHERIT FALSE, SET TRUE")] {
                timeout(Duration::from_secs(60), async {
                    let database = Database::dedicated(server.clone()).await;
                    let user = User::new(database.user.clone()).expect("runtime user");
                    let admin = database
                        .connections("postgres")
                        .recover()
                        .await
                        .expect("admin");
                    let group = format!("delegate_{}", uuid::Uuid::new_v4().simple());
                    let grantee = if let Some(options) = membership {
                        admin
                            .client()
                            .batch_execute(&format!(
                                "CREATE ROLE {group}; GRANT {group} TO {} WITH {options}",
                                database.user
                            ))
                            .await
                            .expect("reachable role");
                        &group
                    } else {
                        &database.user
                    };
                    let database_target: String = admin
                        .client()
                        .query_one("SELECT format('DATABASE %I', current_database())", &[])
                        .await
                        .expect("database target")
                        .get(0);
                    for (privilege, target) in [
                        ("USAGE", "SCHEMA center"),
                        ("USAGE", "TYPE identity.mobile"),
                        ("USAGE", "SEQUENCE identity.ordinals"),
                        ("EXECUTE", "FUNCTION identity.cancelling(uuid)"),
                        ("SELECT", "TABLE identity.claims"),
                        ("INSERT (id)", "TABLE identity.challenges"),
                        ("CONNECT", database_target.as_str()),
                    ] {
                        verify(admin.client(), &user)
                            .await
                            .expect("ordinary privileges accepted");
                        admin
                            .client()
                            .batch_execute(&format!(
                                "GRANT {privilege} ON {target} TO {grantee} WITH GRANT OPTION"
                            ))
                            .await
                            .expect("grant delegation");
                        assert!(
                            matches!(
                                verify(admin.client(), &user).await,
                                Err(crate::database::Error::Permissions)
                            ),
                            "{membership:?}: {privilege} ON {target}"
                        );
                        admin
                            .client()
                            .batch_execute(&format!(
                                "REVOKE GRANT OPTION FOR {privilege} ON {target} FROM {grantee}"
                            ))
                            .await
                            .expect("remove delegation only");
                        verify(admin.client(), &user)
                            .await
                            .expect("ordinary grant remains valid");
                    }
                    if membership.is_some() {
                        admin
                            .client()
                            .batch_execute(&format!("DROP OWNED BY {group}; DROP ROLE {group}"))
                            .await
                            .expect("remove fixture role");
                    }
                    admin.terminate().await.expect("admin ended");
                })
                .await
                .expect("grant option scenario budget");
            }
        });
    }

    #[test]
    fn membership_options_bound_runtime_authority() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(60), async {
                let database = Database::dedicated(server.clone()).await;
                let user = User::new(database.user.clone()).expect("runtime user");
                let admin = database
                    .connections("postgres")
                    .recover()
                    .await
                    .expect("admin");
                let group = format!("group_{}", uuid::Uuid::new_v4().simple());
                let bridge = format!("bridge_{}", uuid::Uuid::new_v4().simple());
                admin
                    .client()
                    .batch_execute(&format!(
                        "CREATE ROLE {group}; CREATE ROLE {bridge}; \
                     GRANT USAGE ON SCHEMA center TO {group}; \
                     GRANT {group} TO {} WITH ADMIN TRUE, INHERIT FALSE, SET FALSE",
                        database.user
                    ))
                    .await
                    .expect("admin-only membership");
                assert!(
                    matches!(
                        verify(admin.client(), &user).await,
                        Err(crate::database::Error::Permissions)
                    ),
                    "ADMIN delegates even without INHERIT or SET"
                );
                admin
                    .client()
                    .batch_execute(&format!(
                        "GRANT {group} TO {} WITH ADMIN FALSE; \
                     GRANT INSERT ON identity.challenges TO {group}",
                        database.user
                    ))
                    .await
                    .expect("inaccessible excess grant");
                verify(admin.client(), &user)
                    .await
                    .expect("unusable membership adds no authority");
                for option in ["INHERIT TRUE", "SET TRUE"] {
                    admin
                        .client()
                        .batch_execute(&format!("GRANT {group} TO {} WITH {option}", database.user))
                        .await
                        .expect("activate excess authority");
                    assert!(
                        matches!(
                            verify(admin.client(), &user).await,
                            Err(crate::database::Error::Permissions)
                        ),
                        "{option}"
                    );
                    admin
                        .client()
                        .batch_execute(&format!(
                            "GRANT {group} TO {} WITH INHERIT FALSE, SET FALSE",
                            database.user
                        ))
                        .await
                        .expect("deactivate authority");
                }
                admin
                    .client()
                    .batch_execute(&format!(
                        "REVOKE {group} FROM {0}; \
                     GRANT {bridge} TO {0} WITH INHERIT FALSE, SET TRUE; \
                     GRANT {group} TO {bridge} WITH INHERIT TRUE, SET FALSE",
                        database.user
                    ))
                    .await
                    .expect("SET followed by inheritance");
                assert!(
                    matches!(
                        verify(admin.client(), &user).await,
                        Err(crate::database::Error::Permissions)
                    ),
                    "SET role inherits excess authority"
                );
                admin
                    .client()
                    .batch_execute(&format!(
                        "REVOKE INSERT ON identity.challenges FROM {group}; \
                     GRANT {group} TO {bridge} WITH ADMIN TRUE, INHERIT FALSE, SET FALSE"
                    ))
                    .await
                    .expect("SET role can delegate an otherwise inaccessible group");
                assert!(
                    matches!(
                        verify(admin.client(), &user).await,
                        Err(crate::database::Error::Permissions)
                    ),
                    "SET role holds ADMIN"
                );
                admin
                    .client()
                    .batch_execute(&format!(
                        "GRANT {bridge} TO {} WITH INHERIT TRUE, SET FALSE",
                        database.user
                    ))
                    .await
                    .expect("ADMIN is not inherited");
                verify(admin.client(), &user)
                    .await
                    .expect("unusable ADMIN is not runtime authority");
                admin
                    .client()
                    .batch_execute(&format!(
                        "GRANT pg_read_server_files TO {} WITH INHERIT FALSE, SET FALSE",
                        database.user
                    ))
                    .await
                    .expect("prohibited predefined membership");
                assert!(
                    matches!(
                        verify(admin.client(), &user).await,
                        Err(crate::database::Error::Permissions)
                    ),
                    "the explicit server-role membership ban still applies"
                );
                admin
                    .client()
                    .batch_execute(&format!(
                        "REVOKE pg_read_server_files FROM {}",
                        database.user
                    ))
                    .await
                    .expect("remove prohibited membership");
                admin
                    .client()
                    .batch_execute(&format!(
                        "DROP OWNED BY {group}, {bridge}; DROP ROLE {group}, {bridge}"
                    ))
                    .await
                    .expect("remove fixture roles");
                admin.terminate().await.expect("admin ended");
            })
            .await
            .expect("membership scenario budget");
        });
    }

    #[test]
    fn grant_replacement_revokes_dependent_chains_and_preserves_independent_grants() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(60), async {
                let database = Database::dedicated(server.clone()).await;
                let user = User::new(database.user.clone()).expect("runtime user");
                let admin = database.connections("postgres").recover().await.expect("admin");
                let owner = database.connections(&database.owner).recover().await.expect("owner");
                let runtime = database.connections(&database.user).recover().await.expect("runtime");
                let delegate = format!("delegate_{}", uuid::Uuid::new_v4().simple());
                let recipient = format!("recipient_{}", uuid::Uuid::new_v4().simple());
                admin.client().batch_execute(&format!("CREATE ROLE {delegate}; CREATE ROLE {recipient}"))
                    .await.expect("downstream roles");
                owner.client().batch_execute(&format!("GRANT USAGE ON SCHEMA identity TO {delegate}, {recipient}"))
                    .await.expect("downstream roles can resolve qualified objects");
                owner.client().query_one(
                    "SELECT set_config('nucleus.runtime_user', $1, false)", &[&database.user]
                ).await.expect("bind runtime identity");
                let cases = [
                    ("USAGE", "SCHEMA center", "has_schema_privilege($1::text, 'center', $2::text)"),
                    ("USAGE", "TYPE identity.mobile", "has_type_privilege($1::text, 'identity.mobile', $2::text)"),
                    ("USAGE", "SEQUENCE identity.ordinals", "has_sequence_privilege($1::text, 'identity.ordinals', $2::text)"),
                    ("EXECUTE", "FUNCTION identity.cancelling(uuid)", "has_function_privilege($1::text, 'identity.cancelling(uuid)', $2::text)"),
                    ("SELECT", "TABLE identity.claims", "has_table_privilege($1::text, 'identity.claims', $2::text)"),
                    ("INSERT (id)", "TABLE identity.challenges", "has_column_privilege($1::text, 'identity.challenges', 'id', $2::text)"),
                ];
                for (privilege, target, _) in cases {
                    owner.client().batch_execute(&format!(
                        "GRANT {privilege} ON {target} TO {} WITH GRANT OPTION", database.user
                    )).await.expect("runtime delegation capability");
                    runtime.client().batch_execute(&format!(
                        "GRANT {privilege} ON {target} TO {delegate} WITH GRANT OPTION"
                    )).await.expect("runtime delegates authority");
                    admin.client().batch_execute(&format!(
                        "SET ROLE {delegate}; GRANT {privilege} ON {target} TO {recipient} WITH GRANT OPTION; RESET ROLE"
                    )).await.expect("second delegation edge");
                    owner.client().batch_execute(&format!("GRANT {privilege} ON {target} TO {recipient}"))
                        .await.expect("independent ordinary grant on the same object");
                }
                let invalid = render(&[Privilege {
                    object: Object::Table("identity.absent_test_relation"), name: "SELECT",
                }]);
                let error = owner.client().batch_execute(&invalid).await.expect_err("replacement fails after revocation");
                assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::UNDEFINED_TABLE));
                for (privilege, target, inquiry) in cases {
                    let name = privilege.split_whitespace().next().expect("privilege name");
                    let grant_option = format!("{name} WITH GRANT OPTION");
                    for role in [&database.user, &delegate, &recipient] {
                        let held: bool = admin.client().query_one(&format!("SELECT {inquiry}"), &[role, &grant_option])
                            .await.expect("delegation survived rollback").get(0);
                        assert!(held, "{role}: {privilege} ON {target} rolled back atomically");
                    }
                }
                for _ in 0..2 {
                    owner.client().batch_execute(&grants()).await.expect("reconcile manifest and dependent grants");
                    verify(admin.client(), &user).await.expect("runtime matches the manifest");
                    for (privilege, target, inquiry) in cases {
                        let name = privilege.split_whitespace().next().expect("privilege name");
                        let grant_option = format!("{name} WITH GRANT OPTION");
                        for (role, expected) in [(&delegate, false), (&recipient, true)] {
                            let held: bool = admin.client().query_one(&format!("SELECT {inquiry}"), &[role, &name])
                                .await.expect("ordinary privilege").get(0);
                            assert_eq!(held, expected, "{role}: {privilege} ON {target}");
                            let delegated: bool = admin.client().query_one(&format!("SELECT {inquiry}"), &[role, &grant_option])
                                .await.expect("delegation capability").get(0);
                            assert!(!delegated, "{role}: {privilege} ON {target}");
                        }
                    }
                }
                runtime.terminate().await.expect("runtime ended");
                owner.terminate().await.expect("owner ended");
                admin.client().batch_execute(&format!("DROP OWNED BY {delegate}, {recipient}; DROP ROLE {delegate}, {recipient}"))
                    .await.expect("remove downstream fixture roles");
                admin.terminate().await.expect("admin ended");
            }).await.expect("delegation reconciliation budget");
        });
    }

    #[test]
    fn generated_grants_replace_runtime_privileges_with_the_manifest() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(60), async {
                let database = Database::new(server).await;
                let user = User::new(database.user.clone()).expect("runtime user");
                let admin = database
                    .connections("postgres")
                    .recover()
                    .await
                    .expect("fixture administrator");
                let owner = database
                    .connections(&database.owner)
                    .recover()
                    .await
                    .expect("deployment identity");
                owner
                    .client()
                    .query_one(
                        "SELECT pg_catalog.set_config('nucleus.runtime_user', $1, false)",
                        &[&database.user],
                    )
                    .await
                    .expect("bound runtime identity");
                // Interpolated names are fixed test inputs and the generated role.
                admin
                    .client()
                    .batch_execute(&format!(
                        "GRANT INSERT ON identity.challenges TO {0}; \
                        GRANT USAGE ON SCHEMA deployment TO {0}",
                        database.user
                    ))
                    .await
                    .expect("grants outside the manifest");
                let declared = privileges(&MANIFEST);
                let removed = Privilege {
                    object: Object::Column("identity.challenges", "id"),
                    name: "INSERT",
                };
                let reduced: Vec<_> = declared
                    .iter()
                    .copied()
                    .filter(|privilege| *privilege != removed)
                    .collect();
                owner
                    .client()
                    .batch_execute(&render(&reduced))
                    .await
                    .expect("grants without one entry");
                assert_eq!(
                    compare(admin.client(), &user, &declared)
                        .await
                        .expect("comparison"),
                    [difference(false, removed)]
                );
                for _ in 0..2 {
                    owner
                        .client()
                        .batch_execute(&grants())
                        .await
                        .expect("manifest grants");
                    verify(admin.client(), &user).await.expect("exact manifest");
                }
                owner.terminate().await.expect("deployment identity ended");
                admin.terminate().await.expect("administrator ended");
            })
            .await
            .expect("generation budget");
        });
    }
}
