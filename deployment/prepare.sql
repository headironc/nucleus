-- Run as the administrator in the explicitly selected new database.
-- Bind nucleus.deployment_user and nucleus.runtime_user, then run roles.sql first.
BEGIN;
DO $$
DECLARE
    deployment_user text := current_setting('nucleus.deployment_user');
    runtime_user text := current_setting('nucleus.runtime_user');
BEGIN
    EXECUTE format('REVOKE ALL ON DATABASE %I FROM PUBLIC', current_database());
    EXECUTE format('REVOKE ALL ON DATABASE %I FROM %I', current_database(), runtime_user);
    EXECUTE format('GRANT CONNECT ON DATABASE %I TO %I, %I', current_database(), deployment_user, runtime_user);
    EXECUTE format('GRANT CREATE ON DATABASE %I TO %I', current_database(), deployment_user);
    EXECUTE format('REVOKE ALL ON SCHEMA public FROM PUBLIC, %I', runtime_user);
    EXECUTE format('ALTER DEFAULT PRIVILEGES FOR ROLE %I REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC', deployment_user);
    EXECUTE format('ALTER DEFAULT PRIVILEGES FOR ROLE %I REVOKE USAGE ON TYPES FROM PUBLIC', deployment_user);
END
$$;
COMMIT;
