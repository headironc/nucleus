//! The migration verifier and running consumer share the delivered grant boundary.

use platform::database::User;
use tokio_postgres::Client;

use crate::database::Error;

pub async fn verify(client: &Client, user: &User) -> Result<(), Error> {
    let valid: bool = client.query_one(r#"
        WITH insertions(relation, attribute) AS (
            SELECT attribute.attrelid, attribute.attname
            FROM pg_catalog.pg_attribute attribute
            WHERE attribute.attnum > 0 AND NOT attribute.attisdropped AND (
                (attribute.attrelid = 'identity.challenges'::regclass AND attribute.attname NOT IN ('status', 'solved_at', 'finished_at'))
                OR (attribute.attrelid = 'identity.messages'::regclass AND attribute.attname NOT IN ('status', 'finished_at'))
                OR attribute.attrelid IN ('identity.checks'::regclass, 'identity.claims'::regclass)
            )
        ), updates(relation, attribute) AS (
            VALUES ('security.clocks'::regclass, 'observed_at'),
                ('identity.challenges'::regclass, 'calls'), ('identity.challenges'::regclass, 'status'),
                ('identity.challenges'::regclass, 'solved_at'), ('identity.challenges'::regclass, 'finished_at'),
                ('identity.messages'::regclass, 'status'), ('identity.messages'::regclass, 'finished_at')
        )
        SELECT current_setting('session_replication_role') <> 'replica'
        AND has_database_privilege($1::text, current_database(), 'CONNECT')
        AND has_schema_privilege($1::text, 'identity', 'USAGE')
        AND has_schema_privilege($1::text, 'security', 'USAGE')
        AND has_table_privilege($1::text, 'identity.challenges', 'SELECT')
        AND has_table_privilege($1::text, 'security.clocks', 'SELECT')
        AND has_table_privilege($1::text, 'security.challenges', 'SELECT')
        AND has_table_privilege($1::text, 'security.mobiles', 'SELECT')
        AND has_table_privilege($1::text, 'security.encryptions', 'SELECT')
        AND has_table_privilege($1::text, 'security.messages', 'SELECT')
        AND has_table_privilege($1::text, 'identity.messages', 'SELECT')
        AND has_table_privilege($1::text, 'identity.checks', 'SELECT')
        AND has_table_privilege($1::text, 'identity.claims', 'SELECT')
        AND has_table_privilege($1::text, 'identity.challenges', 'DELETE')
        AND has_table_privilege($1::text, 'identity.messages', 'DELETE')
        AND has_table_privilege($1::text, 'identity.checks', 'DELETE')
        AND has_table_privilege($1::text, 'identity.claims', 'DELETE')
        AND has_sequence_privilege($1::text, 'identity.ordinals', 'USAGE')
        AND NOT EXISTS (
            SELECT 1 FROM insertions WHERE NOT has_column_privilege($1::text, relation, attribute, 'INSERT')
        )
        AND NOT EXISTS (
            SELECT 1 FROM updates WHERE NOT has_column_privilege($1::text, relation, attribute, 'UPDATE')
        )
        AND NOT EXISTS (
            SELECT 1 FROM pg_catalog.pg_roles role
            WHERE pg_has_role($1::text, role.oid, 'MEMBER') AND (
                role.rolsuper OR role.rolcreaterole OR role.rolcreatedb OR role.rolreplication OR role.rolbypassrls
                OR role.rolname IN ('pg_read_server_files', 'pg_write_server_files', 'pg_execute_server_program')
                OR has_parameter_privilege(role.oid, 'session_replication_role', 'SET,ALTER SYSTEM')
                OR has_database_privilege(role.oid, current_database(), 'CREATE,TEMP')
                OR has_schema_privilege(role.oid, 'deployment', 'USAGE')
                OR has_sequence_privilege(role.oid, 'identity.ordinals', 'SELECT,UPDATE')
                OR EXISTS (SELECT 1 FROM pg_catalog.pg_class sequence
                    WHERE sequence.oid = 'identity.ordinals'::regclass
                        AND pg_has_role(role.oid, sequence.relowner, 'MEMBER'))
                OR EXISTS (
                    SELECT 1 FROM pg_catalog.pg_namespace namespace
                    WHERE pg_has_role(role.oid, namespace.nspowner, 'MEMBER')
                        OR has_schema_privilege(role.oid, namespace.oid, 'CREATE')
                )
                OR EXISTS (
                    SELECT 1 FROM pg_catalog.pg_class relation
                    WHERE relation.oid IN ('security.challenges'::regclass, 'security.mobiles'::regclass, 'security.encryptions'::regclass,
                        'identity.challenges'::regclass, 'security.clocks'::regclass, 'security.messages'::regclass,
                        'identity.messages'::regclass, 'identity.checks'::regclass, 'identity.claims'::regclass)
                    AND (pg_has_role(role.oid, relation.relowner, 'MEMBER')
                        OR has_table_privilege(role.oid, relation.oid, 'INSERT,UPDATE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
                        OR (relation.oid NOT IN ('identity.challenges'::regclass, 'identity.messages'::regclass,
                            'identity.checks'::regclass, 'identity.claims'::regclass)
                            AND has_table_privilege(role.oid, relation.oid, 'DELETE'))
                        OR EXISTS (
                            SELECT 1 FROM pg_catalog.pg_attribute attribute
                            WHERE attribute.attrelid = relation.oid AND attribute.attnum > 0 AND NOT attribute.attisdropped
                            AND (has_column_privilege(role.oid, relation.oid, attribute.attnum, 'REFERENCES')
                                OR (NOT EXISTS (SELECT 1 FROM insertions WHERE insertions.relation = relation.oid AND insertions.attribute = attribute.attname)
                                    AND has_column_privilege(role.oid, relation.oid, attribute.attnum, 'INSERT'))
                                OR (NOT EXISTS (SELECT 1 FROM updates WHERE updates.relation = relation.oid AND updates.attribute = attribute.attname)
                                    AND has_column_privilege(role.oid, relation.oid, attribute.attnum, 'UPDATE')))
                        ))
                )
            )
        )
    "#, &[user]).await?.try_get(0)?;
    if !valid {
        return Err(Error::Permissions);
    }
    Ok(())
}
