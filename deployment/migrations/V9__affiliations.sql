CREATE SCHEMA center;
REVOKE ALL ON SCHEMA center FROM PUBLIC;

-- Each value is a permanent fact; the newest time is the current value.
CREATE TABLE center.affiliations (
    id uuid PRIMARY KEY,
    person_id uuid NOT NULL REFERENCES identity.persons (id),
    value bytea NOT NULL CHECK (octet_length(value) = 39),
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    UNIQUE (person_id, created_at)
);

REVOKE ALL ON ALL TABLES IN SCHEMA center FROM PUBLIC;
