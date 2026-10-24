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

CREATE DOMAIN identity.sealed AS bytea NOT NULL
    CHECK (octet_length(VALUE) >= 37);
CREATE DOMAIN identity.digest AS bytea NOT NULL
    CHECK (octet_length(VALUE) = 32);
CREATE TYPE identity.mobile AS (value identity.sealed, hash identity.digest);
REVOKE ALL ON DOMAIN identity.sealed, identity.digest FROM PUBLIC;
REVOKE ALL ON TYPE identity.mobile FROM PUBLIC;

CREATE TABLE identity.challenges (
    id uuid PRIMARY KEY,
    operation_id uuid NOT NULL UNIQUE,
    purpose text NOT NULL CHECK (purpose = 'authentication'),
    hash bytea NOT NULL CHECK (octet_length(hash) = 32),
    key_id bigint NOT NULL REFERENCES security.challenges (id),
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

CREATE FUNCTION identity.check_challenge() RETURNS trigger
LANGUAGE plpgsql SET search_path = '' AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.status <> 'calling' OR NEW.calls <> 1 THEN
            RAISE EXCEPTION 'challenge must begin with its first call' USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    IF ROW(NEW.id, NEW.operation_id, NEW.purpose, NEW.hash, NEW.key_id, NEW.mobile,
           NEW.source, NEW.created_at, NEW.expires_at)
       IS DISTINCT FROM ROW(OLD.id, OLD.operation_id, OLD.purpose, OLD.hash, OLD.key_id, OLD.mobile,
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

REVOKE ALL ON security.clocks, identity.challenges FROM PUBLIC;
DO $$
DECLARE
    runtime_user text := current_setting('nucleus.runtime_user');
BEGIN
    EXECUTE format('GRANT USAGE ON SCHEMA identity TO %I', runtime_user);
    EXECUTE format('GRANT USAGE ON TYPE identity.mobile TO %I', runtime_user);
    EXECUTE format('GRANT USAGE ON DOMAIN identity.sealed, identity.digest TO %I', runtime_user);
    EXECUTE format('REVOKE ALL ON security.clocks, identity.challenges FROM %I', runtime_user);
    EXECUTE format('GRANT SELECT ON security.clocks, identity.challenges TO %I', runtime_user);
    EXECUTE format('GRANT UPDATE (observed_at) ON security.clocks TO %I', runtime_user);
    EXECUTE format('GRANT INSERT (id, operation_id, purpose, hash, key_id, mobile, source, calls, created_at, expires_at) ON identity.challenges TO %I', runtime_user);
    EXECUTE format('GRANT UPDATE (calls, status, solved_at, finished_at) ON identity.challenges TO %I', runtime_user);
END
$$;
