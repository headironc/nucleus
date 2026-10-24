CREATE SCHEMA security;
REVOKE ALL ON SCHEMA security FROM PUBLIC;

-- Every key use derives from one of these root versions; each key_id in every capability references it.
CREATE TABLE security.keys (
    id bigint PRIMARY KEY CHECK (id >= 0),
    fingerprint bytea NOT NULL CHECK (octet_length(fingerprint) = 32),
    active boolean NOT NULL
);
CREATE UNIQUE INDEX keys_active_idx ON security.keys (active) WHERE active;

REVOKE ALL ON security.keys FROM PUBLIC;
