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

CREATE TABLE identity.exchanges (
    id uuid PRIMARY KEY,
    operation_id uuid NOT NULL UNIQUE,
    app_id text NOT NULL CHECK (app_id <> '' AND app_id !~ '[[:space:][:cntrl:]]'),
    proof bytea NOT NULL CHECK (octet_length(proof) = 32),
    hash bytea NOT NULL CHECK (octet_length(hash) = 32),
    key_id bigint NOT NULL REFERENCES security.keys (id),
    code_hash bytea NOT NULL CHECK (octet_length(code_hash) = 32),
    lookup_key_id bigint NOT NULL REFERENCES security.keys (id),
    worker_id uuid NOT NULL,
    claimed_at timestamptz NOT NULL CHECK (isfinite(claimed_at)),
    deadline timestamptz NOT NULL CHECK (isfinite(deadline) AND deadline = claimed_at + interval '30 seconds'),
    status text NOT NULL CHECK (status IN ('claimed', 'resolved', 'exhausted')),
    reason text CHECK (reason IN ('authentication_invalid', 'mobile_unsupported', 'verification_required')),
    mobile_hash bytea CHECK (octet_length(mobile_hash) = 32),
    finished_at timestamptz CHECK (isfinite(finished_at) AND finished_at >= claimed_at),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    UNIQUE (lookup_key_id, code_hash),
    CHECK ((status = 'claimed' AND reason IS NULL AND mobile_hash IS NULL AND finished_at IS NULL)
        OR (status = 'resolved' AND reason IS NULL AND mobile_hash IS NOT NULL AND finished_at IS NOT NULL AND finished_at < deadline)
        OR (status = 'exhausted' AND reason IS NOT NULL AND mobile_hash IS NULL AND finished_at IS NOT NULL))
);
CREATE INDEX exchanges_finished_at_id_idx ON identity.exchanges (finished_at, id);

CREATE FUNCTION identity.check_exchange() RETURNS trigger
LANGUAGE plpgsql SET search_path = '' AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.status <> 'claimed' THEN
            RAISE EXCEPTION 'exchange starts with its original worker claim' USING ERRCODE = '23514';
        END IF;
    ELSIF OLD.status <> 'claimed' OR NEW.status = 'claimed'
        OR ROW(NEW.id, NEW.operation_id, NEW.app_id, NEW.proof, NEW.hash, NEW.key_id,
            NEW.code_hash, NEW.lookup_key_id, NEW.worker_id, NEW.claimed_at, NEW.deadline)
            IS DISTINCT FROM ROW(OLD.id, OLD.operation_id, OLD.app_id, OLD.proof, OLD.hash, OLD.key_id,
            OLD.code_hash, OLD.lookup_key_id, OLD.worker_id, OLD.claimed_at, OLD.deadline) THEN
        RAISE EXCEPTION 'exchange has one immutable claim and one terminal transition' USING ERRCODE = '23514';
    END IF;
    IF NEW.transaction_id <> pg_catalog.pg_current_xact_id() THEN
        RAISE EXCEPTION 'exchange transition belongs to its current transaction' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER exchanges_transition_check BEFORE INSERT OR UPDATE ON identity.exchanges
    FOR EACH ROW EXECUTE FUNCTION identity.check_exchange();

CREATE FUNCTION identity.check_exchange_deletion() RETURNS trigger
LANGUAGE plpgsql SET search_path = '' AS $$
BEGIN
    IF OLD.status = 'claimed' OR OLD.finished_at + interval '24 hours' > pg_catalog.clock_timestamp() THEN
        RAISE EXCEPTION 'exchange execution and occupancy retention must end before deletion' USING ERRCODE = '23514';
    END IF;
    RETURN OLD;
END
$$;
CREATE TRIGGER exchanges_deletion_check BEFORE DELETE ON identity.exchanges
    FOR EACH ROW EXECUTE FUNCTION identity.check_exchange_deletion();

CREATE TABLE identity.authentications (
    id uuid PRIMARY KEY,
    operation_id uuid NOT NULL UNIQUE,
    proof bytea NOT NULL CHECK (octet_length(proof) = 32),
    hash bytea NOT NULL CHECK (octet_length(hash) = 32),
    key_id bigint NOT NULL REFERENCES security.keys (id),
    purpose text NOT NULL CHECK (purpose = 'create_session'),
    app_id text NOT NULL CHECK (app_id <> '' AND app_id !~ '[[:space:][:cntrl:]]'),
    transport text NOT NULL,
    message_id uuid,
    exchange_id uuid UNIQUE REFERENCES identity.exchanges (id),
    status text NOT NULL CHECK (status IN ('invalid', 'suspended', 'authenticated', 'registrable')),
    finished_at timestamptz NOT NULL CHECK (isfinite(finished_at)),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    CHECK ((transport = 'cookie' AND message_id IS NOT NULL AND exchange_id IS NULL)
        OR (transport = 'bearer' AND message_id IS NULL AND exchange_id IS NOT NULL AND status <> 'invalid'))
);
CREATE INDEX authentications_finished_at_id_idx ON identity.authentications (finished_at, id);
CREATE INDEX authentications_message_id_idx ON identity.authentications (message_id);

CREATE FUNCTION identity.check_session_operation() RETURNS trigger
LANGUAGE plpgsql SET search_path = '' AS $$
BEGIN
    PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtextextended('identity/session-operation:' || NEW.operation_id::text, 0));
    IF TG_TABLE_NAME = 'exchanges' THEN
        IF EXISTS (SELECT 1 FROM identity.authentications WHERE operation_id = NEW.operation_id) THEN
            RAISE EXCEPTION 'session operation already has an authentication' USING ERRCODE = '23514';
        END IF;
    ELSIF EXISTS (SELECT 1 FROM identity.exchanges WHERE operation_id = NEW.operation_id
        AND (NEW.exchange_id IS DISTINCT FROM id OR NEW.transport <> 'bearer')) THEN
        RAISE EXCEPTION 'session operation retains its original exchange' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER exchanges_operation_check BEFORE INSERT ON identity.exchanges
    FOR EACH ROW EXECUTE FUNCTION identity.check_session_operation();
CREATE TRIGGER authentications_operation_check BEFORE INSERT ON identity.authentications
    FOR EACH ROW EXECUTE FUNCTION identity.check_session_operation();

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

REVOKE ALL ON identity.messages, identity.authentications, identity.exchanges,
    identity.checks, identity.confirmations, identity.claims FROM PUBLIC;
REVOKE ALL ON FUNCTION identity.check_message(), identity.check_message_pair(), identity.check_claim(), identity.check_exchange(), identity.check_exchange_deletion(), identity.check_session_operation() FROM PUBLIC;
