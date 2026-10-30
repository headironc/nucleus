CREATE SCHEMA identity;
REVOKE ALL ON SCHEMA identity FROM PUBLIC;

CREATE TABLE security.clocks (
    id boolean PRIMARY KEY CHECK (id),
    observed_at timestamptz NOT NULL CHECK (isfinite(observed_at))
);
INSERT INTO security.clocks VALUES (true, clock_timestamp());

CREATE FUNCTION security.check_clock() RETURNS trigger
LANGUAGE plpgsql SET search_path = '' AS $$
BEGIN
    IF NEW.observed_at < OLD.observed_at THEN
        RAISE EXCEPTION 'trusted time cannot decrease' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER clocks_transition_check BEFORE UPDATE ON security.clocks
    FOR EACH ROW EXECUTE FUNCTION security.check_clock();
REVOKE ALL ON FUNCTION security.check_clock() FROM PUBLIC;

-- Each application start and deployment command advances the generation inside the
-- exclusive barrier. Write and recovery transactions share-lock the row and require
-- the generation their process advanced.
CREATE TABLE security.instances (
    id boolean PRIMARY KEY CHECK (id),
    generation bigint NOT NULL CHECK (generation >= 0)
);
INSERT INTO security.instances VALUES (true, 0);

CREATE FUNCTION security.check_instance() RETURNS trigger
LANGUAGE plpgsql SET search_path = '' AS $$
BEGIN
    IF NEW.generation <> OLD.generation + 1 THEN
        RAISE EXCEPTION 'instance generation only advances by one' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER instances_transition_check BEFORE UPDATE ON security.instances
    FOR EACH ROW EXECUTE FUNCTION security.check_instance();
REVOKE ALL ON FUNCTION security.check_instance() FROM PUBLIC;

CREATE DOMAIN identity.sealed AS bytea NOT NULL
    CHECK (octet_length(VALUE) >= 37);
CREATE DOMAIN identity.digest AS bytea NOT NULL
    CHECK (octet_length(VALUE) = 32);
CREATE TYPE identity.mobile AS (value identity.sealed, hash identity.digest);
REVOKE ALL ON DOMAIN identity.sealed, identity.digest FROM PUBLIC;
REVOKE ALL ON TYPE identity.mobile FROM PUBLIC;

-- Authenticated challenges retain their person after the current account is removed. The
-- current real-name revision, last transition and object-local counters are sealed state.
CREATE TABLE identity.persons (
    id uuid PRIMARY KEY,
    revision_id uuid NOT NULL,
    changed_at timestamptz NOT NULL CHECK (isfinite(changed_at)),
    sequence bigint NOT NULL CHECK (sequence > 0),
    version bigint NOT NULL CHECK (version > 0),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1)
);
CREATE TABLE identity.revisions (
    id uuid PRIMARY KEY,
    person_id uuid NOT NULL REFERENCES identity.persons (id),
    name bytea NOT NULL CHECK (octet_length(name) BETWEEN 38 AND 293),
    id_number bytea NOT NULL CHECK (octet_length(id_number) = 55),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    UNIQUE (id, person_id)
);
CREATE INDEX revisions_person_id_idx ON identity.revisions (person_id);
-- Person and its current Revision refer to each other.
ALTER TABLE identity.persons ADD FOREIGN KEY (revision_id, id)
    REFERENCES identity.revisions (id, person_id) DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE identity.transfers (
    id uuid PRIMARY KEY,
    person_id uuid NOT NULL REFERENCES identity.persons (id),
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    revision_id uuid NOT NULL,
    -- Original identities survive legal session and code cleanup.
    session_id uuid NOT NULL,
    message_id uuid UNIQUE,
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    operation_id uuid NOT NULL UNIQUE,
    hash bytea NOT NULL CHECK (octet_length(hash) = 32),
    key_id bigint NOT NULL REFERENCES security.keys (id),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    FOREIGN KEY (revision_id, person_id) REFERENCES identity.revisions (id, person_id),
    CHECK ((operator_id = revision_id) = (message_id IS NOT NULL)),
    UNIQUE (person_id, created_at)
);

CREATE TABLE identity.challenges (
    id uuid PRIMARY KEY,
    operation_id uuid NOT NULL UNIQUE,
    purpose text NOT NULL CHECK (purpose IN ('authentication', 'mobile_change', 'mobile_binding')),
    person_id uuid REFERENCES identity.persons (id),
    -- Original identity survives Session deletion; it is not a current access reference.
    session_id uuid,
    transfer_id uuid REFERENCES identity.transfers (id),
    hash bytea NOT NULL CHECK (octet_length(hash) = 32),
    key_id bigint NOT NULL REFERENCES security.keys (id),
    -- A typed NULL extracted from a null composite can bypass a domain's NOT NULL.
    mobile identity.mobile NOT NULL CHECK ((mobile).value IS NOT NULL AND (mobile).hash IS NOT NULL),
    source inet NOT NULL CHECK (
        masklen(source) = CASE family(source) WHEN 4 THEN 32 ELSE 128 END
        AND NOT source <<= inet '::ffff:0:0/96'
    ),
    calls smallint NOT NULL CHECK (calls BETWEEN 1 AND 2),
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    expires_at timestamptz NOT NULL CHECK (
        isfinite(expires_at) AND expires_at = created_at + interval '10 seconds'
    ),
    status text NOT NULL DEFAULT 'calling',
    solved_at timestamptz CHECK (isfinite(solved_at)),
    finished_at timestamptz CHECK (isfinite(finished_at) AND finished_at >= created_at),
    CONSTRAINT challenges_key_id_hash_key UNIQUE (key_id, hash),
    CONSTRAINT challenges_identity_check CHECK (
        (purpose = 'authentication' AND person_id IS NULL AND session_id IS NULL AND transfer_id IS NULL)
        OR (purpose = 'mobile_change' AND person_id IS NOT NULL AND session_id IS NOT NULL AND transfer_id IS NULL)
        OR (purpose = 'mobile_binding' AND person_id IS NOT NULL AND session_id IS NOT NULL AND transfer_id IS NOT NULL)
    ),
    CONSTRAINT challenges_status_check CHECK (
        (status = 'calling' AND solved_at IS NULL AND finished_at IS NULL)
        OR (status = 'retryable' AND calls = 1 AND solved_at IS NULL
            AND finished_at IS NOT NULL AND finished_at < expires_at)
        OR (status IN ('rejected', 'inconclusive') AND solved_at IS NULL AND finished_at IS NOT NULL)
        OR (status = 'passed' AND solved_at IS NOT NULL AND finished_at IS NOT NULL
            AND solved_at <= finished_at + interval '2 seconds'
            AND finished_at < least(solved_at, finished_at) + interval '300 seconds')
    )
);

CREATE INDEX challenges_transfer_id_idx ON identity.challenges (transfer_id);

CREATE FUNCTION identity.check_challenge() RETURNS trigger
LANGUAGE plpgsql SET search_path = '' AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.status <> 'calling' OR NEW.calls <> 1 THEN
            RAISE EXCEPTION 'challenge must begin with its first call' USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    IF ROW(NEW.id, NEW.operation_id, NEW.purpose, NEW.person_id, NEW.session_id, NEW.transfer_id, NEW.hash, NEW.key_id, NEW.mobile,
           NEW.source, NEW.created_at, NEW.expires_at)
       IS DISTINCT FROM ROW(OLD.id, OLD.operation_id, OLD.purpose, OLD.person_id, OLD.session_id, OLD.transfer_id, OLD.hash, OLD.key_id, OLD.mobile,
                            OLD.source, OLD.created_at, OLD.expires_at) THEN
        RAISE EXCEPTION 'original challenge facts are immutable' USING ERRCODE = '23514';
    END IF;
    IF NOT (
        (OLD.status = 'calling' AND NEW.calls = OLD.calls
            AND NEW.status IN ('passed', 'rejected', 'retryable', 'inconclusive'))
        OR (OLD.status = 'retryable' AND OLD.calls = 1 AND NEW.calls = 2
            AND NEW.status = 'calling' AND clock_timestamp() < OLD.expires_at)
        OR (OLD.status = 'retryable' AND NEW.calls = OLD.calls AND NEW.status = 'inconclusive'
            AND NEW.finished_at >= OLD.finished_at)
    ) THEN
        RAISE EXCEPTION 'invalid challenge transition' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER challenges_transition_check BEFORE INSERT OR UPDATE ON identity.challenges
    FOR EACH ROW EXECUTE FUNCTION identity.check_challenge();
REVOKE ALL ON FUNCTION identity.check_challenge() FROM PUBLIC;

REVOKE ALL ON security.clocks, security.instances, identity.challenges FROM PUBLIC;
