CREATE TABLE security.messages (
    id bigint PRIMARY KEY CHECK (id >= 0),
    fingerprint bytea NOT NULL CHECK (octet_length(fingerprint) = 32),
    active boolean NOT NULL
);
CREATE UNIQUE INDEX messages_active_idx ON security.messages (active) WHERE active;

CREATE TABLE identity.messages (
    id uuid PRIMARY KEY,
    challenge_id uuid NOT NULL UNIQUE REFERENCES identity.challenges (id),
    ordinal bigint NOT NULL UNIQUE CHECK (ordinal > 0),
    worker_id uuid NOT NULL,
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    expires_at timestamptz NOT NULL CHECK (isfinite(expires_at) AND expires_at > created_at),
    status text NOT NULL DEFAULT 'sending',
    finished_at timestamptz CHECK (isfinite(finished_at) AND finished_at >= created_at),
    CONSTRAINT messages_status_check CHECK (
        (status = 'sending' AND finished_at IS NULL)
        OR (status IN ('sent', 'failed', 'unknown') AND finished_at IS NOT NULL)
    )
);
CREATE INDEX messages_created_at_id_idx ON identity.messages (created_at, id);

CREATE TABLE identity.checks (
    id uuid PRIMARY KEY REFERENCES identity.messages (id) DEFERRABLE INITIALLY DEFERRED,
    hash bytea NOT NULL CHECK (octet_length(hash) = 32),
    key_id bigint NOT NULL REFERENCES security.messages (id),
    expires_at timestamptz NOT NULL CHECK (isfinite(expires_at))
);
ALTER TABLE identity.messages ADD CONSTRAINT messages_id_fkey
    FOREIGN KEY (id) REFERENCES identity.checks (id) DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE identity.claims (
    id uuid PRIMARY KEY REFERENCES identity.messages (id),
    worker_id uuid NOT NULL,
    created_at timestamptz NOT NULL CHECK (isfinite(created_at))
);

CREATE FUNCTION identity.check_message() RETURNS trigger
LANGUAGE plpgsql SET search_path = '' AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.status <> 'sending' OR NOT EXISTS (
            SELECT 1 FROM identity.challenges challenge
            WHERE challenge.id = NEW.challenge_id AND challenge.status = 'passed'
                AND NEW.created_at >= challenge.finished_at
                AND NEW.created_at < least(challenge.solved_at, challenge.finished_at) + interval '300 seconds'
        ) THEN
            RAISE EXCEPTION 'message requires a current passed challenge' USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    IF ROW(NEW.id, NEW.challenge_id, NEW.ordinal, NEW.worker_id, NEW.created_at, NEW.expires_at)
       IS DISTINCT FROM ROW(OLD.id, OLD.challenge_id, OLD.ordinal, OLD.worker_id, OLD.created_at, OLD.expires_at)
       OR OLD.status <> 'sending' OR NEW.status NOT IN ('sent', 'failed', 'unknown') THEN
        RAISE EXCEPTION 'invalid message transition' USING ERRCODE = '23514';
    END IF;
    IF NEW.status IN ('sent', 'unknown') AND NOT EXISTS (
        SELECT 1 FROM identity.claims claim WHERE claim.id = NEW.id
            AND claim.worker_id = NEW.worker_id AND claim.created_at <= NEW.finished_at
    ) THEN
        RAISE EXCEPTION 'message outcome requires its original claim' USING ERRCODE = '23514';
    END IF;
    IF EXISTS (SELECT 1 FROM identity.claims claim WHERE claim.id = NEW.id AND claim.created_at > NEW.finished_at) THEN
        RAISE EXCEPTION 'message conclusion predates its claim' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER messages_transition_check BEFORE INSERT OR UPDATE ON identity.messages
    FOR EACH ROW EXECUTE FUNCTION identity.check_message();

CREATE FUNCTION identity.check_message_pair() RETURNS trigger
LANGUAGE plpgsql SET search_path = '' AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM identity.checks verification JOIN identity.messages message ON message.id = verification.id
        WHERE verification.id = NEW.id AND verification.expires_at = message.expires_at
    ) THEN
        RAISE EXCEPTION 'message and code deadline must agree' USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER checks_pair_check AFTER INSERT ON identity.checks
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_message_pair();

CREATE FUNCTION identity.check_claim() RETURNS trigger
LANGUAGE plpgsql SET search_path = '' AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM identity.messages message WHERE message.id = NEW.id
            AND message.worker_id = NEW.worker_id AND message.status = 'sending'
            AND NEW.created_at >= message.created_at
            AND NEW.created_at <= message.expires_at - interval '60 seconds'
    ) THEN
        RAISE EXCEPTION 'claim requires its original eligible message' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER claims_insert_check BEFORE INSERT ON identity.claims
    FOR EACH ROW EXECUTE FUNCTION identity.check_claim();

REVOKE ALL ON security.messages, identity.messages, identity.checks, identity.claims FROM PUBLIC;
REVOKE ALL ON FUNCTION identity.check_message(), identity.check_message_pair(), identity.check_claim() FROM PUBLIC;
DO $$
DECLARE
    runtime_user text := current_setting('nucleus.runtime_user');
BEGIN
    EXECUTE format('REVOKE ALL ON security.messages, identity.messages, identity.checks, identity.claims FROM %I', runtime_user);
    EXECUTE format('GRANT SELECT ON security.messages, identity.messages, identity.checks, identity.claims TO %I', runtime_user);
    EXECUTE format('GRANT INSERT (id, challenge_id, ordinal, worker_id, created_at, expires_at) ON identity.messages TO %I', runtime_user);
    EXECUTE format('GRANT INSERT (id, hash, key_id, expires_at) ON identity.checks TO %I', runtime_user);
    EXECUTE format('GRANT INSERT (id, worker_id, created_at) ON identity.claims TO %I', runtime_user);
    EXECUTE format('GRANT UPDATE (status, finished_at) ON identity.messages TO %I', runtime_user);
END
$$;
