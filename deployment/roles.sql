-- Bind both session inputs with set_config before executing this check.
DO $$
DECLARE
    deployment_user text := current_setting('nucleus.deployment_user');
    runtime_user text := current_setting('nucleus.runtime_user');
BEGIN
    IF deployment_user = runtime_user OR (
        SELECT count(*) FROM pg_catalog.pg_roles
        WHERE rolname IN (deployment_user, runtime_user) AND rolcanlogin
    ) <> 2 THEN
        RAISE EXCEPTION 'two distinct database login roles are required';
    END IF;
    IF EXISTS (
        SELECT 1 FROM pg_catalog.pg_roles
        WHERE (rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls
            OR rolname IN ('pg_read_server_files', 'pg_write_server_files', 'pg_execute_server_program'))
          AND (pg_catalog.pg_has_role(deployment_user, oid, 'MEMBER')
            OR pg_catalog.pg_has_role(runtime_user, oid, 'MEMBER'))
    ) OR pg_catalog.pg_has_role(runtime_user, deployment_user, 'MEMBER') OR EXISTS (
        SELECT 1 FROM pg_catalog.pg_database
        WHERE datname = current_database()
          AND pg_catalog.pg_has_role(runtime_user, datdba, 'MEMBER')
    ) THEN
        RAISE EXCEPTION 'database roles have excessive privileges';
    END IF;
END
$$;
