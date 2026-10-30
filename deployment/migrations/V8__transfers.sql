CREATE TABLE identity.rejections (
    purpose text NOT NULL CHECK (purpose IN ('transfer', 'completion')),
    family text GENERATED ALWAYS AS (CASE WHEN purpose = 'completion' THEN 'mobile_completion' END) STORED,
    operation_id uuid NOT NULL,
    account_id uuid,
    message_id uuid NOT NULL REFERENCES identity.confirmations (id),
    person_id uuid NOT NULL REFERENCES identity.persons (id),
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    revision_id uuid NOT NULL,
    session_id uuid NOT NULL,
    rejected_at timestamptz NOT NULL CHECK (isfinite(rejected_at)),
    hash bytea NOT NULL CHECK (octet_length(hash) = 32),
    key_id bigint NOT NULL REFERENCES security.keys (id),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    PRIMARY KEY (purpose, operation_id),
    CHECK ((purpose = 'completion') = (account_id IS NOT NULL)),
    FOREIGN KEY (family, operation_id) REFERENCES identity.completions (family, operation_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (account_id, person_id) REFERENCES identity.accounts (id, person_id),
    FOREIGN KEY (revision_id, person_id) REFERENCES identity.revisions (id, person_id)
);
CREATE INDEX rejections_message_id_idx ON identity.rejections (message_id);

-- Match the transaction owner's LP32(domain, UUID bytes), SHA-256, signed first eight bytes.
CREATE FUNCTION identity.lock_transfer(uuid) RETURNS void
LANGUAGE sql VOLATILE SET search_path = '' AS $$
    SELECT pg_catalog.pg_advisory_xact_lock(('x' || pg_catalog.substr(pg_catalog.encode(pg_catalog.sha256(
        pg_catalog.int4send(18) || pg_catalog.convert_to('transfer/operation', 'UTF8')
        || pg_catalog.int4send(16) || pg_catalog.uuid_send($1)), 'hex'), 1, 16))::bit(64)::bigint)
$$;

CREATE FUNCTION identity.lock_completion(uuid) RETURNS void
LANGUAGE sql VOLATILE SET search_path = '' AS $$
    SELECT pg_catalog.pg_advisory_xact_lock(('x' || pg_catalog.substr(pg_catalog.encode(pg_catalog.sha256(
        pg_catalog.int4send(19) || pg_catalog.convert_to('transfer/completion', 'UTF8')
        || pg_catalog.int4send(16) || pg_catalog.uuid_send($1)), 'hex'), 1, 16))::bit(64)::bigint)
$$;

CREATE FUNCTION identity.check_mobile_sender() RETURNS trigger
LANGUAGE plpgsql VOLATILE SECURITY DEFINER SET search_path = '' AS $$
DECLARE
    original record;
    actor uuid;
    subject uuid;
    observed_at timestamptz;
BEGIN
    IF TG_TABLE_NAME = 'challenges' THEN
        original := NEW;
    ELSE
        SELECT challenge.purpose, challenge.person_id, challenge.session_id, challenge.transfer_id, challenge.mobile INTO STRICT original FROM identity.challenges challenge
            JOIN identity.messages message ON message.challenge_id = challenge.id WHERE message.id = NEW.id;
    END IF;
    IF original.purpose = 'authentication' THEN RETURN NEW; END IF;
    SELECT account.id INTO subject FROM identity.accounts account WHERE account.person_id = original.person_id;
    SELECT account_id INTO actor FROM identity.sessions WHERE id = original.session_id;
    IF actor IS NULL OR subject IS NULL THEN
        RAISE EXCEPTION 'mobile verification requires current operator and target accounts' USING ERRCODE = '23514';
    END IF;
    IF actor <> subject THEN
        PERFORM pg_catalog.pg_advisory_xact_lock_shared(-3487418652284273221);
        PERFORM identity.lock_accounts(actor, subject);
        IF original.purpose <> 'mobile_binding' OR NOT EXISTS (
            SELECT 1 FROM access.holders target JOIN access.roles target_role ON target_role.id = target.role_id
            JOIN access.holders manager ON manager.id = actor JOIN access.roles manager_role ON manager_role.id = manager.role_id
            WHERE target.id = subject AND target_role.scope = 'institution' AND manager_role.scope = 'institution'
                AND target_role.level < manager_role.level
                AND EXISTS (SELECT 1 FROM access.permissions permission
                    WHERE permission.role_id = manager_role.id AND permission.capability_id = 'institution.accounts')
        ) THEN
            RAISE EXCEPTION 'mobile verification requires current management qualification' USING ERRCODE = '23514';
        END IF;
    ELSE
        PERFORM identity.lock_account(subject);
    END IF;
    observed_at := pg_catalog.clock_timestamp();
    IF NOT EXISTS (
        SELECT 1 FROM identity.accounts account JOIN identity.sessions session ON session.account_id = actor
        JOIN identity.accounts manager ON manager.id = session.account_id
        WHERE account.id = subject AND session.id = original.session_id
            AND session.created_at <= observed_at AND observed_at < session.expires_at
            AND ((original.purpose = 'mobile_change' AND (account.mobile).hash = (original.mobile).hash)
                OR (original.purpose = 'mobile_binding' AND (account.mobile).hash <> (original.mobile).hash
                    AND EXISTS (SELECT 1 FROM identity.transfers transfer JOIN identity.persons person ON person.id = transfer.person_id
                        JOIN identity.revisions operator ON operator.id = transfer.operator_id
                        WHERE transfer.id = original.transfer_id AND transfer.person_id = original.person_id
                            AND operator.person_id = manager.person_id
                            AND transfer.session_id = original.session_id AND transfer.revision_id = person.revision_id
                            AND transfer.created_at <= observed_at
                            AND NOT EXISTS (SELECT 1 FROM identity.replacements WHERE id = transfer.id)
                            AND NOT EXISTS (SELECT 1 FROM identity.transfers later
                                WHERE later.person_id = transfer.person_id AND later.created_at > transfer.created_at))))
            AND NOT EXISTS (SELECT 1 FROM identity.revocations WHERE id = session.id)
            AND NOT EXISTS (SELECT 1 FROM identity.suspensions suspension
                WHERE suspension.account_id = manager.id AND NOT EXISTS (SELECT 1 FROM identity.resumptions WHERE id = suspension.id))
    ) THEN
        RAISE EXCEPTION 'mobile verification requires its current account and session' USING ERRCODE = '23514';
    END IF;
    IF original.purpose = 'mobile_binding' AND EXISTS (
        SELECT 1 FROM identity.transfers transfer WHERE transfer.id = original.transfer_id
            AND observed_at >= transfer.created_at + interval '10 minutes'
    ) THEN
        RAISE EXCEPTION 'mobile verification requires an unexpired transfer'
            USING ERRCODE = '23514', SCHEMA = TG_TABLE_SCHEMA, TABLE = TG_TABLE_NAME,
                CONSTRAINT = TG_TABLE_NAME || '_transfer_expiry_check';
    END IF;
    IF TG_TABLE_NAME = 'confirmations' THEN
        IF NOT EXISTS (SELECT 1 FROM identity.accounts account JOIN identity.persons person ON person.id = account.person_id
            WHERE account.id = actor AND person.revision_id = NEW.operator_id) THEN
            RAISE EXCEPTION 'mobile confirmation requires the current operator revision' USING ERRCODE = '23514';
        END IF;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER challenges_sender_check BEFORE INSERT ON identity.challenges
    FOR EACH ROW EXECUTE FUNCTION identity.check_mobile_sender();
CREATE TRIGGER confirmations_sender_check BEFORE INSERT ON identity.confirmations
    FOR EACH ROW EXECUTE FUNCTION identity.check_mobile_sender();
CREATE TRIGGER claims_sender_check BEFORE INSERT ON identity.claims
    FOR EACH ROW EXECUTE FUNCTION identity.check_mobile_sender();

CREATE FUNCTION identity.check_confirmation() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.failures <> 0 THEN
            RAISE EXCEPTION 'new confirmation cannot already be used' USING ERRCODE = '23514';
        END IF;
    ELSIF ROW(NEW.id, NEW.person_id, NEW.operator_id, NEW.hash, NEW.key_id, NEW.expires_at)
            IS DISTINCT FROM ROW(OLD.id, OLD.person_id, OLD.operator_id, OLD.hash, OLD.key_id, OLD.expires_at)
        OR OLD.failures >= 5 OR NEW.failures <> OLD.failures + 1 THEN
        RAISE EXCEPTION 'invalid confirmation transition' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER confirmations_transition_check BEFORE INSERT OR UPDATE ON identity.confirmations
    FOR EACH ROW EXECUTE FUNCTION identity.check_confirmation();

CREATE FUNCTION identity.check_transfer() RETURNS trigger
LANGUAGE plpgsql VOLATILE SECURITY DEFINER SET search_path = '' AS $$
DECLARE
    actor uuid;
    subject uuid;
    occurred_at timestamptz;
    observed_at timestamptz;
    verification record;
BEGIN
    PERFORM identity.lock_transfer(NEW.operation_id);
    IF (TG_TABLE_NAME = 'transfers' AND EXISTS (SELECT 1 FROM identity.rejections WHERE purpose = 'transfer' AND operation_id = NEW.operation_id))
        OR (TG_TABLE_NAME = 'rejections' AND EXISTS (SELECT 1 FROM identity.transfers WHERE operation_id = NEW.operation_id)) THEN
        RAISE EXCEPTION 'transfer operation already has a conclusion' USING ERRCODE = '23514';
    END IF;
    SELECT id INTO subject FROM identity.accounts
        WHERE person_id = NEW.person_id AND cancellation_id IS NULL;
    SELECT account_id INTO actor FROM identity.sessions WHERE id = NEW.session_id;
    IF actor IS NULL OR subject IS NULL THEN
        RAISE EXCEPTION 'transfer requires current operator and target accounts' USING ERRCODE = '23514';
    END IF;
    IF actor <> subject THEN
        -- SHA-256 prefix of LP32("access/scope", "institution"), shared with database::transaction.
        PERFORM pg_catalog.pg_advisory_xact_lock_shared(-3487418652284273221);
    END IF;
    PERFORM identity.lock_accounts(actor, subject);
    PERFORM 1 FROM identity.confirmations WHERE id = NEW.message_id FOR UPDATE;
    observed_at := pg_catalog.clock_timestamp();
    IF TG_TABLE_NAME = 'transfers' THEN occurred_at := NEW.created_at;
    ELSE occurred_at := NEW.rejected_at; END IF;
    IF occurred_at < pg_catalog.transaction_timestamp() OR occurred_at > observed_at
        OR NEW.transaction_id <> pg_catalog.pg_current_xact_id() OR NOT EXISTS (
        SELECT 1 FROM identity.accounts account JOIN identity.persons person ON person.id = account.person_id
        JOIN identity.accounts manager ON manager.id = actor JOIN identity.persons operator ON operator.id = manager.person_id
        JOIN identity.sessions session ON session.account_id = manager.id
        WHERE account.id = subject AND person.id = NEW.person_id AND person.revision_id = NEW.revision_id
            AND operator.revision_id = NEW.operator_id AND session.id = NEW.session_id
            AND session.created_at <= occurred_at AND occurred_at < session.expires_at
            AND NOT EXISTS (SELECT 1 FROM identity.revocations WHERE id = session.id)
            AND NOT EXISTS (SELECT 1 FROM identity.suspensions suspension
                WHERE suspension.account_id = manager.id AND NOT EXISTS (SELECT 1 FROM identity.resumptions WHERE id = suspension.id))
    ) THEN
        RAISE EXCEPTION 'transfer requires its current operator, session and target revision' USING ERRCODE = '23514';
    END IF;
    IF actor <> subject THEN
        IF TG_TABLE_NAME <> 'transfers' OR NEW.message_id IS NOT NULL OR NOT EXISTS (
            SELECT 1 FROM access.holders target JOIN access.roles target_role ON target_role.id = target.role_id
            JOIN access.holders manager ON manager.id = actor JOIN access.roles manager_role ON manager_role.id = manager.role_id
            WHERE target.id = subject AND target.person_id = NEW.person_id
                AND target_role.scope = 'institution' AND manager_role.scope = 'institution'
                AND target_role.level < manager_role.level
                AND EXISTS (SELECT 1 FROM access.permissions permission
                    WHERE permission.role_id = manager_role.id AND permission.capability_id = 'institution.accounts')
        ) THEN
            RAISE EXCEPTION 'assisted transfer requires an authorized manager and a lower current target' USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    SELECT confirmation.failures, confirmation.expires_at, message.created_at, message.ordinal, (challenge.mobile).hash AS mobile
        INTO verification FROM identity.confirmations confirmation JOIN identity.messages message ON message.id = confirmation.id
        JOIN identity.challenges challenge ON challenge.id = message.challenge_id JOIN identity.accounts account ON account.id = subject
        WHERE confirmation.id = NEW.message_id AND confirmation.person_id = NEW.person_id
            AND challenge.purpose = 'mobile_change'
            AND (challenge.mobile).hash = (account.mobile).hash;
    IF NOT FOUND OR verification.created_at > occurred_at OR verification.expires_at <= occurred_at
        OR (TG_TABLE_NAME = 'transfers' AND verification.failures >= 5)
        OR (TG_TABLE_NAME = 'rejections' AND verification.failures = 0)
        OR EXISTS (SELECT 1 FROM identity.transfers WHERE person_id = NEW.person_id AND created_at >= verification.created_at)
        OR EXISTS (SELECT 1 FROM identity.confirmations confirmation JOIN identity.messages message ON message.id = confirmation.id
            JOIN identity.challenges challenge ON challenge.id = message.challenge_id
            WHERE confirmation.person_id = NEW.person_id AND challenge.purpose = 'mobile_change'
                AND (challenge.mobile).hash = verification.mobile AND message.ordinal > verification.ordinal) THEN
        RAISE EXCEPTION 'transfer requires the latest unused current-phone confirmation' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER transfers_insert_check BEFORE INSERT ON identity.transfers
    FOR EACH ROW EXECUTE FUNCTION identity.check_transfer();
CREATE TRIGGER rejections_insert_check BEFORE INSERT ON identity.rejections
    FOR EACH ROW WHEN (NEW.purpose = 'transfer') EXECUTE FUNCTION identity.check_transfer();

CREATE FUNCTION identity.check_rejection() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE
    message_id uuid;
BEGIN
    IF TG_TABLE_NAME = 'confirmations' THEN message_id := NEW.id; ELSE message_id := NEW.message_id; END IF;
    IF NOT EXISTS (SELECT 1 FROM identity.confirmations confirmation WHERE confirmation.id = message_id
        AND confirmation.failures = (SELECT count(*) FROM identity.rejections rejection WHERE rejection.message_id = confirmation.id)) THEN
        RAISE EXCEPTION 'confirmation failures require paired original rejections' USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER confirmations_rejections_check AFTER UPDATE ON identity.confirmations
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_rejection();
CREATE CONSTRAINT TRIGGER rejections_pair_check AFTER INSERT ON identity.rejections
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_rejection();

CREATE TRIGGER transfers_isolation_check BEFORE INSERT OR DELETE ON identity.transfers
    FOR EACH STATEMENT EXECUTE FUNCTION identity.check_isolation();
CREATE TRIGGER rejections_isolation_check BEFORE INSERT OR DELETE ON identity.rejections
    FOR EACH STATEMENT EXECUTE FUNCTION identity.check_isolation();

CREATE FUNCTION identity.check_transfer_deletion() RETURNS trigger
LANGUAGE plpgsql VOLATILE SECURITY DEFINER SET search_path = '' AS $$
DECLARE
    observed_at timestamptz;
BEGIN
    IF TG_TABLE_NAME = 'transfers' THEN
        PERFORM identity.lock_transfer(OLD.operation_id);
    ELSIF OLD.purpose = 'completion' THEN
        PERFORM identity.lock_completion(OLD.operation_id);
    ELSE
        PERFORM identity.lock_transfer(OLD.operation_id);
    END IF;
    observed_at := pg_catalog.clock_timestamp();
    IF TG_TABLE_NAME = 'transfers' THEN
        IF observed_at < OLD.created_at + interval '24 hours 10 minutes'
            OR EXISTS (SELECT 1 FROM identity.messages WHERE id = OLD.message_id)
            OR EXISTS (SELECT 1 FROM identity.challenges WHERE transfer_id = OLD.id) THEN
            RAISE EXCEPTION 'transfer responsibilities have not ended' USING ERRCODE = '23514';
        END IF;
    ELSIF EXISTS (SELECT 1 FROM identity.confirmations WHERE id = OLD.message_id AND expires_at + interval '24 hours' > observed_at) THEN
        RAISE EXCEPTION 'rejection responsibilities have not ended' USING ERRCODE = '23514';
    END IF;
    RETURN OLD;
END
$$;
CREATE TRIGGER transfers_deletion_check BEFORE DELETE ON identity.transfers
    FOR EACH ROW EXECUTE FUNCTION identity.check_transfer_deletion();
CREATE TRIGGER rejections_deletion_check BEFORE DELETE ON identity.rejections
    FOR EACH ROW EXECUTE FUNCTION identity.check_transfer_deletion();

REVOKE ALL ON identity.transfers, identity.rejections, identity.replacements FROM PUBLIC;
REVOKE ALL ON FUNCTION identity.lock_transfer(uuid), identity.check_mobile_sender(), identity.check_confirmation(), identity.check_transfer(), identity.check_rejection(), identity.check_transfer_deletion() FROM PUBLIC;
REVOKE ALL ON FUNCTION identity.lock_completion(uuid) FROM PUBLIC;
