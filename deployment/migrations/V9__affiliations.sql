CREATE SCHEMA center;
REVOKE ALL ON SCHEMA center FROM PUBLIC;

CREATE TABLE center.completions (
    family text NOT NULL CHECK (family = 'center_affiliation'),
    operation_id uuid PRIMARY KEY,
    action text NOT NULL CONSTRAINT completions_action_check CHECK (action IN ('fill', 'correct', 'system_correct')),
    target text NOT NULL CHECK (target <> ''),
    input bytea NOT NULL CHECK (octet_length(input) >= 37),
    subject text NOT NULL CONSTRAINT completions_subject_check CHECK (subject IN ('session', 'system')),
    CONSTRAINT completions_subject_action_check CHECK (
        (subject = 'session' AND action IN ('fill', 'correct'))
        OR (subject = 'system' AND action = 'system_correct')
    ),
    person_id uuid NOT NULL REFERENCES identity.persons (id),
    revision_id uuid NOT NULL,
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    expires_at timestamptz CHECK (expires_at IS NULL),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    FOREIGN KEY (revision_id, person_id) REFERENCES identity.revisions (id, person_id)
);

CREATE TABLE center.affiliations (
    id uuid PRIMARY KEY,
    person_id uuid NOT NULL REFERENCES identity.persons (id),
    value bytea NOT NULL CHECK (octet_length(value) = 39),
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    operation_id uuid NOT NULL UNIQUE REFERENCES center.completions (operation_id) DEFERRABLE INITIALLY DEFERRED,
    sequence bigint NOT NULL CHECK (sequence > 0),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    UNIQUE (person_id, created_at),
    UNIQUE (person_id, sequence),
    UNIQUE (id, person_id, sequence, created_at)
);

CREATE TABLE center.profiles (
    id uuid PRIMARY KEY REFERENCES identity.persons (id),
    affiliation_id uuid NOT NULL,
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    sequence bigint NOT NULL CHECK (sequence > 0),
    version bigint NOT NULL CHECK (version > 0),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    FOREIGN KEY (affiliation_id, id, sequence, created_at)
        REFERENCES center.affiliations (id, person_id, sequence, created_at) DEFERRABLE INITIALLY DEFERRED
);

ALTER TABLE center.affiliations ADD FOREIGN KEY (person_id)
    REFERENCES center.profiles (id) DEFERRABLE INITIALLY DEFERRED;

-- Structural pairing only: every newly allocated sequence has a record from this transaction.
CREATE FUNCTION center.check_profile() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE
    previous_sequence bigint;
    previous_version bigint;
BEGIN
    previous_sequence := CASE WHEN TG_OP = 'INSERT' THEN 0 ELSE OLD.sequence END;
    previous_version := CASE WHEN TG_OP = 'INSERT' THEN 0 ELSE OLD.version END;
    IF NEW.sequence <= previous_sequence OR NEW.version <> previous_version + 1
        OR (TG_OP = 'UPDATE' AND NEW.id <> OLD.id)
        OR EXISTS (
            SELECT 1 FROM pg_catalog.generate_series(previous_sequence + 1, NEW.sequence) expected(sequence)
            WHERE NOT EXISTS (
                SELECT 1 FROM center.affiliations record
                WHERE record.person_id = NEW.id AND record.sequence = expected.sequence
                    AND record.xmin = pg_catalog.pg_current_xact_id()::text::xid
            )
        ) THEN
        RAISE EXCEPTION 'state advancement requires its new contiguous records'
            USING ERRCODE = '23514', CONSTRAINT = 'profiles_records_check';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER profiles_records_check AFTER INSERT OR UPDATE ON center.profiles
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION center.check_profile();

REVOKE ALL ON ALL TABLES IN SCHEMA center FROM PUBLIC;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA center FROM PUBLIC;
