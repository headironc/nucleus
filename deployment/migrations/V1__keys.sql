CREATE SCHEMA security;
REVOKE ALL ON SCHEMA security FROM PUBLIC;

CREATE TABLE security.challenges (
    id bigint PRIMARY KEY CHECK (id >= 0),
    fingerprint bytea NOT NULL CHECK (octet_length(fingerprint) = 32),
    active boolean NOT NULL
);
CREATE UNIQUE INDEX challenges_active_idx ON security.challenges (active) WHERE active;

CREATE TABLE security.mobiles (
    id bigint PRIMARY KEY CHECK (id >= 0),
    fingerprint bytea NOT NULL CHECK (octet_length(fingerprint) = 32),
    active boolean NOT NULL
);
CREATE UNIQUE INDEX mobiles_active_idx ON security.mobiles (active) WHERE active;

CREATE TABLE security.encryptions (
    id bigint PRIMARY KEY CHECK (id >= 0),
    fingerprint bytea NOT NULL CHECK (octet_length(fingerprint) = 32),
    active boolean NOT NULL
);
CREATE UNIQUE INDEX encryptions_active_idx ON security.encryptions (active) WHERE active;

REVOKE ALL ON security.challenges, security.mobiles, security.encryptions FROM PUBLIC;
DO $$
DECLARE
    runtime_user text := current_setting('nucleus.runtime_user');
BEGIN
    EXECUTE format('GRANT USAGE ON SCHEMA security TO %I', runtime_user);
    EXECUTE format('REVOKE ALL ON security.challenges, security.mobiles, security.encryptions FROM %I', runtime_user);
    EXECUTE format('GRANT SELECT ON security.challenges, security.mobiles, security.encryptions TO %I', runtime_user);
END
$$;
