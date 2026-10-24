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

CREATE TABLE identity.authentications (
    id uuid PRIMARY KEY,
    operation_id uuid NOT NULL UNIQUE,
    proof bytea NOT NULL CHECK (octet_length(proof) = 32),
    hash bytea NOT NULL CHECK (octet_length(hash) = 32),
    key_id bigint NOT NULL REFERENCES security.keys (id),
    purpose text NOT NULL CHECK (purpose = 'create_session'),
    app_id text NOT NULL CHECK (app_id <> '' AND app_id !~ '[[:space:][:cntrl:]]'),
    transport text NOT NULL CHECK (transport = 'cookie'),
    message_id uuid NOT NULL,
    status text NOT NULL CHECK (status IN ('invalid', 'suspended', 'authenticated', 'registrable')),
    finished_at timestamptz NOT NULL CHECK (isfinite(finished_at)),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id()
);
CREATE INDEX authentications_finished_at_id_idx ON identity.authentications (finished_at, id);
CREATE INDEX authentications_message_id_idx ON identity.authentications (message_id);

CREATE TABLE identity.checks (
    id uuid PRIMARY KEY REFERENCES identity.messages (id) DEFERRABLE INITIALLY DEFERRED,
    hash bytea NOT NULL CHECK (octet_length(hash) = 32),
    key_id bigint NOT NULL REFERENCES security.keys (id),
    expires_at timestamptz NOT NULL CHECK (isfinite(expires_at)),
    failures smallint NOT NULL DEFAULT 0 CHECK (failures BETWEEN 0 AND 5),
    authentication_id uuid UNIQUE REFERENCES identity.authentications (id)
);

CREATE TABLE identity.confirmations (
    id uuid PRIMARY KEY REFERENCES identity.messages (id) DEFERRABLE INITIALLY DEFERRED,
    person_id uuid NOT NULL REFERENCES identity.persons (id),
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    hash bytea NOT NULL CHECK (octet_length(hash) = 32),
    key_id bigint NOT NULL REFERENCES security.keys (id),
    expires_at timestamptz NOT NULL CHECK (isfinite(expires_at)),
    failures smallint NOT NULL DEFAULT 0 CHECK (failures BETWEEN 0 AND 5)
);
CREATE INDEX confirmations_person_id_id_idx ON identity.confirmations (person_id, id);

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
    IF EXISTS (SELECT 1 FROM identity.messages WHERE id = NEW.id) AND NOT EXISTS (
        SELECT 1 FROM identity.messages message
        JOIN identity.challenges challenge ON challenge.id = message.challenge_id
        LEFT JOIN identity.checks verification ON verification.id = message.id
        LEFT JOIN identity.confirmations confirmation ON confirmation.id = message.id
        WHERE message.id = NEW.id AND (
            (challenge.purpose = 'authentication' AND verification.id IS NOT NULL
                AND confirmation.id IS NULL AND verification.expires_at = message.expires_at)
            OR (challenge.purpose IN ('mobile_change', 'mobile_binding') AND confirmation.id IS NOT NULL
                AND verification.id IS NULL AND confirmation.expires_at = message.expires_at
                AND confirmation.person_id = challenge.person_id)
        )
    ) THEN
        RAISE EXCEPTION 'message requires exactly its purpose-specific code and deadline' USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER messages_pair_check AFTER INSERT ON identity.messages
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_message_pair();
CREATE CONSTRAINT TRIGGER checks_pair_check AFTER INSERT ON identity.checks
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_message_pair();
CREATE CONSTRAINT TRIGGER confirmations_pair_check AFTER INSERT ON identity.confirmations
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

REVOKE ALL ON identity.messages, identity.authentications,
    identity.checks, identity.confirmations, identity.claims FROM PUBLIC;
REVOKE ALL ON FUNCTION identity.check_message(), identity.check_message_pair(), identity.check_claim() FROM PUBLIC;
