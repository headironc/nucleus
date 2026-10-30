-- Registration composes complete Identity and Access facts, never a second authentication.
CREATE TABLE identity.attempts (
    id uuid PRIMARY KEY,
    operation_id uuid NOT NULL UNIQUE,
    credential_id uuid NOT NULL,
    authentication_id uuid NOT NULL REFERENCES identity.authentications (id),
    proof bytea NOT NULL CHECK (octet_length(proof) = 32),
    hash bytea NOT NULL CHECK (octet_length(hash) = 32),
    key_id bigint NOT NULL REFERENCES security.keys (id),
    mobile identity.mobile NOT NULL,
    accepted_at timestamptz NOT NULL CHECK (isfinite(accepted_at)),
    status text NOT NULL CHECK (status IN ('registered', 'mobile_taken', 'id_number_taken')),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    CHECK ((mobile).value IS NOT NULL AND (mobile).hash IS NOT NULL)
);
CREATE INDEX attempts_mobile_time_idx ON identity.attempts (((mobile).hash), accepted_at, id);
CREATE INDEX attempts_accepted_at_id_idx ON identity.attempts (accepted_at, id);

CREATE FUNCTION identity.lock_registration(uuid) RETURNS void
LANGUAGE sql VOLATILE SECURITY DEFINER SET search_path = '' AS $$
    SELECT FROM identity.registrations WHERE id = $1 FOR UPDATE
$$;

CREATE FUNCTION identity.check_attempt_admission() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE credential record; observed_at timestamptz;
BEGIN
    PERFORM identity.lock_registration(NEW.credential_id);
    SELECT * INTO credential FROM identity.registrations WHERE id = NEW.credential_id;
    IF NOT FOUND OR credential.authentication_id <> NEW.authentication_id
        OR (credential.mobile).hash <> (NEW.mobile).hash
        OR NEW.accepted_at < credential.created_at OR NEW.accepted_at >= credential.expires_at
        OR NOT EXISTS (SELECT 1 FROM identity.authentications WHERE id = NEW.authentication_id AND status = 'registrable') THEN
        RAISE EXCEPTION 'attempt requires its valid registration credential' USING ERRCODE = '23514';
    END IF;
    observed_at := pg_catalog.clock_timestamp();
    IF NEW.accepted_at > observed_at THEN
        RAISE EXCEPTION 'attempt requires observed trusted time' USING ERRCODE = '23514';
    END IF;
    -- This protects the committed quota even for a direct runtime INSERT.
    PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtextextended('registration/quota:' || pg_catalog.encode((NEW.mobile).hash, 'hex'), 0));
    IF EXISTS (SELECT 1 FROM identity.attempts WHERE (mobile).hash = (NEW.mobile).hash AND accepted_at > NEW.accepted_at)
        OR (SELECT count(*) FROM identity.attempts WHERE (mobile).hash = (NEW.mobile).hash
            AND accepted_at > NEW.accepted_at - interval '1 hour') >= 5 THEN
        RAISE EXCEPTION 'attempt exceeds the committed mobile admission quota' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER attempts_admission_check BEFORE INSERT ON identity.attempts
    FOR EACH ROW EXECUTE FUNCTION identity.check_attempt_admission();

CREATE FUNCTION identity.check_registration_completion() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE attempt record; receipt record;
BEGIN
    IF TG_TABLE_NAME = 'attempts' THEN
        SELECT * INTO STRICT attempt FROM identity.attempts WHERE id = NEW.id;
        IF attempt.status <> 'registered' THEN
            IF EXISTS (SELECT 1 FROM identity.receipts WHERE id = NEW.id OR operation_id = attempt.operation_id)
                OR NOT EXISTS (SELECT 1 FROM identity.registrations WHERE id = attempt.credential_id) THEN
                RAISE EXCEPTION 'refusal must retain its credential without a receipt' USING ERRCODE = '23514';
            END IF;
            RETURN NULL;
        END IF;
        SELECT * INTO receipt FROM identity.receipts WHERE id = NEW.id;
    ELSE
        SELECT * INTO STRICT receipt FROM identity.receipts WHERE id = NEW.id;
        SELECT * INTO attempt FROM identity.attempts WHERE id = NEW.id;
    END IF;
    IF receipt.id IS NULL OR attempt.id IS NULL OR attempt.status <> 'registered'
        OR ROW(receipt.operation_id, receipt.credential_id, receipt.proof)
            IS DISTINCT FROM ROW(attempt.operation_id, attempt.credential_id, attempt.proof)
        OR receipt.transaction_id <> pg_catalog.pg_current_xact_id() OR attempt.transaction_id <> pg_catalog.pg_current_xact_id()
        OR EXISTS (SELECT 1 FROM identity.registrations WHERE id = receipt.credential_id)
        OR NOT EXISTS (
            SELECT 1 FROM identity.accounts account
            JOIN identity.persons person ON person.id = account.person_id
            JOIN identity.revisions revision ON revision.id = person.revision_id AND revision.person_id = person.id
            JOIN identity.avatars avatar ON avatar.account_id = account.id AND avatar.released_at IS NULL
            JOIN access.holders holder ON holder.id = account.id AND holder.person_id = person.id
            JOIN access.appointments appointment ON appointment.id = holder.assignment_id
            JOIN access.roles role ON role.id = holder.role_id
            JOIN identity.sessions session ON session.receipt_id = receipt.id AND session.account_id = account.id
            JOIN identity.handoffs handoff ON handoff.id = session.id
            JOIN identity.authentications authentication ON authentication.id = attempt.authentication_id
            WHERE account.id = receipt.account_id AND person.id = receipt.person_id AND revision.id = receipt.revision_id
                AND (account.mobile).hash = (attempt.mobile).hash AND person.sequence = 1
                AND EXISTS (SELECT 1 FROM identity.origins origin
                    WHERE origin.person_id = person.id AND origin.account_id = account.id
                        AND origin.revision_id = revision.id AND origin.operation_id = receipt.operation_id
                        AND origin.created_at = attempt.accepted_at AND origin.transaction_id = receipt.transaction_id)
                AND account.created_at = attempt.accepted_at AND session.created_at = attempt.accepted_at
                AND session.authentication_id = authentication.id AND authentication.status = 'registrable'
                AND session.app_id = authentication.app_id AND receipt.app_id = authentication.app_id
                AND receipt.transport = authentication.transport AND receipt.method = session.method
                AND appointment.operation_id = receipt.operation_id AND appointment.person_id = person.id
                AND appointment.revision_id = revision.id AND appointment.operator_id = revision.id
                AND appointment.assigned_at = attempt.accepted_at AND appointment.sequence = 1
                AND role.scope = 'institution' AND role.level = 1 AND role.name = '普通账户'
                AND account.transaction_id = receipt.transaction_id
                AND revision.transaction_id = receipt.transaction_id AND avatar.transaction_id = receipt.transaction_id
                AND appointment.transaction_id = receipt.transaction_id
                AND holder.xmin = pg_catalog.pg_current_xact_id()::text::xid
                AND session.transaction_id = receipt.transaction_id
        ) THEN
        RAISE EXCEPTION 'registration requires complete matching identity access and session facts' USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER attempts_completion_check AFTER INSERT ON identity.attempts
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_registration_completion();
CREATE CONSTRAINT TRIGGER receipts_completion_check AFTER INSERT ON identity.receipts
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_registration_completion();

CREATE FUNCTION identity.check_creation() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE target uuid; matched boolean;
BEGIN
    IF TG_TABLE_NAME = 'revisions' THEN
        IF EXISTS (SELECT 1 FROM identity.corrections
            WHERE id = NEW.id AND person_id = NEW.person_id AND transaction_id = NEW.transaction_id) THEN
            RETURN NULL;
        END IF;
    END IF;
    IF TG_TABLE_NAME = 'appointments' THEN
        IF NEW.sequence > 1 THEN RETURN NULL; END IF;
    END IF;
    IF TG_TABLE_NAME = 'avatars' THEN
        -- A replacement insert is paired with its permanent advancement and the release
        -- written by this same transaction, not with a new-account creation.
        IF EXISTS (SELECT 1 FROM identity.portraits portrait
            JOIN identity.accounts account ON account.id = portrait.account_id
            JOIN identity.avatars released ON released.account_id = portrait.account_id
            WHERE portrait.account_id = NEW.account_id AND portrait.revision = NEW.revision
                AND portrait.transaction_id = pg_catalog.pg_current_xact_id()
                AND account.avatar = portrait.revision
                AND released.id <> NEW.id AND released.revision = portrait.previous_revision
                AND released.release = 'replacement'
                AND released.release_operation_id = portrait.operation_id
                AND released.released_at = portrait.portrait_at
                AND released.release_transaction_id = portrait.transaction_id) THEN
            RETURN NULL;
        END IF;
    END IF;
    IF TG_TABLE_NAME IN ('accounts', 'holders') THEN target := NEW.id;
    ELSIF TG_TABLE_NAME = 'avatars' THEN target := NEW.account_id;
    ELSE SELECT id INTO target FROM identity.accounts WHERE person_id = NEW.person_id;
    END IF;
    SELECT EXISTS (
        SELECT 1 FROM identity.accounts account
        JOIN identity.persons person ON person.id = account.person_id
        JOIN identity.revisions revision ON revision.id = person.revision_id
        JOIN identity.origins origin ON origin.person_id = person.id AND origin.account_id = account.id
            AND origin.revision_id = revision.id
        JOIN identity.avatars avatar ON avatar.account_id = account.id AND avatar.released_at IS NULL
        JOIN access.holders holder ON holder.id = account.id
        JOIN access.appointments appointment ON appointment.id = holder.assignment_id
        WHERE account.id = target
            AND account.transaction_id = pg_catalog.pg_current_xact_id()
            AND origin.transaction_id = account.transaction_id AND revision.transaction_id = account.transaction_id
            AND avatar.transaction_id = account.transaction_id
            AND holder.xmin = pg_catalog.pg_current_xact_id()::text::xid
            AND appointment.transaction_id = account.transaction_id
            AND (TG_TABLE_NAME <> 'revisions' OR revision.id = NEW.id)
            AND (TG_TABLE_NAME <> 'avatars' OR avatar.id = NEW.id)
            AND (TG_TABLE_NAME <> 'appointments' OR appointment.id = NEW.id)
            AND (EXISTS (SELECT 1 FROM identity.receipts WHERE account_id = target AND transaction_id = account.transaction_id)
                OR appointment.source = 'initialization')
    ) INTO matched;
    IF NOT matched THEN
        RAISE EXCEPTION 'new identity and first assignment require a complete creation operation' USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER revisions_creation_check AFTER INSERT ON identity.revisions
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_creation();
CREATE CONSTRAINT TRIGGER accounts_creation_check AFTER INSERT ON identity.accounts
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_creation();
CREATE CONSTRAINT TRIGGER avatars_creation_check AFTER INSERT ON identity.avatars
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_creation();
CREATE CONSTRAINT TRIGGER appointments_creation_check AFTER INSERT ON access.appointments
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_creation();

CREATE FUNCTION identity.check_credential() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF TG_TABLE_NAME = 'handoffs' THEN
        IF NOT EXISTS (SELECT 1 FROM identity.sessions WHERE id = NEW.id
            AND transaction_id = pg_catalog.pg_current_xact_id()
            AND NEW.expires_at > created_at AND NEW.expires_at <= expires_at) THEN
            RAISE EXCEPTION 'handoff deadline must belong to its new session' USING ERRCODE = '23514';
        END IF;
    ELSIF TG_TABLE_NAME = 'issuances' THEN
        IF NEW.receipt_id IS NULL THEN
            IF NOT EXISTS (SELECT 1 FROM identity.authentications WHERE id = NEW.authentication_id
                AND finished_at = NEW.created_at AND status = 'authenticated' AND app_id = NEW.app_id
                AND transaction_id = pg_catalog.pg_current_xact_id()) THEN
                RAISE EXCEPTION 'session requires its new successful authentication' USING ERRCODE = '23514';
            END IF;
        ELSIF NOT EXISTS (SELECT 1 FROM identity.receipts receipt JOIN identity.attempts attempt ON attempt.id = receipt.id
            WHERE receipt.id = NEW.receipt_id AND attempt.authentication_id = NEW.authentication_id
                AND receipt.account_id = NEW.account_id AND receipt.transaction_id = pg_catalog.pg_current_xact_id()
                AND attempt.transaction_id = receipt.transaction_id AND attempt.accepted_at = NEW.created_at
                AND attempt.status = 'registered') THEN
            RAISE EXCEPTION 'session requires its new successful registration' USING ERRCODE = '23514';
        END IF;
    ELSIF NOT EXISTS (SELECT 1 FROM identity.authentications WHERE id = NEW.authentication_id
        AND finished_at = NEW.created_at AND status = 'registrable' AND transaction_id = pg_catalog.pg_current_xact_id()) THEN
        RAISE EXCEPTION 'credential requires its new registrable authentication' USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER issuances_authentication_check AFTER INSERT ON identity.issuances
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_credential();
CREATE CONSTRAINT TRIGGER registrations_authentication_check AFTER INSERT ON identity.registrations
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_credential();
CREATE CONSTRAINT TRIGGER handoffs_session_check AFTER INSERT ON identity.handoffs
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_credential();

CREATE FUNCTION identity.check_authentication_deletion() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE observed_at timestamptz;
BEGIN
    observed_at := pg_catalog.clock_timestamp();
    IF TG_TABLE_NAME = 'authentications' THEN
        IF observed_at < OLD.finished_at + interval '24 hours'
            OR EXISTS (SELECT 1 FROM identity.checks WHERE id = OLD.message_id) THEN
            RAISE EXCEPTION 'authentication responsibilities have not ended' USING ERRCODE = '23514';
        END IF;
    ELSIF TG_TABLE_NAME = 'registrations' THEN
        IF observed_at < OLD.expires_at
            AND NOT EXISTS (SELECT 1 FROM identity.registrations replacement
                WHERE (replacement.mobile).hash = (OLD.mobile).hash AND replacement.id <> OLD.id AND replacement.created_at >= OLD.created_at)
            AND NOT EXISTS (SELECT 1 FROM identity.receipts receipt JOIN identity.attempts attempt ON attempt.id = receipt.id
                WHERE receipt.credential_id = OLD.id AND attempt.authentication_id = OLD.authentication_id
                    AND receipt.transaction_id = pg_catalog.pg_current_xact_id() AND attempt.status = 'registered') THEN
            RAISE EXCEPTION 'registration requires expiry replacement or complete consumption' USING ERRCODE = '23514';
        END IF;
    ELSIF TG_TABLE_NAME = 'sessions' THEN
        IF EXISTS (SELECT 1 FROM identity.handoffs WHERE id = OLD.id)
            OR (NOT identity.cancelling(OLD.account_id)
                AND observed_at < LEAST(OLD.expires_at, OLD.revoked_at) + interval '24 hours') THEN
            RAISE EXCEPTION 'session retention and delivery responsibilities have not ended' USING ERRCODE = '23514';
        END IF;
    ELSIF observed_at < OLD.expires_at AND NOT EXISTS (SELECT 1 FROM identity.revocations WHERE id = OLD.id)
        AND NOT EXISTS (SELECT 1 FROM identity.sessions session WHERE session.id = OLD.id AND identity.cancelling(session.account_id)) THEN
        RAISE EXCEPTION 'credential responsibilities have not ended' USING ERRCODE = '23514';
    END IF;
    RETURN OLD;
END
$$;
CREATE CONSTRAINT TRIGGER authentications_deletion_check AFTER DELETE ON identity.authentications
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_authentication_deletion();
CREATE TRIGGER sessions_deletion_check BEFORE DELETE ON identity.sessions
    FOR EACH ROW EXECUTE FUNCTION identity.check_authentication_deletion();
CREATE TRIGGER handoffs_deletion_check BEFORE DELETE ON identity.handoffs
    FOR EACH ROW EXECUTE FUNCTION identity.check_authentication_deletion();
CREATE CONSTRAINT TRIGGER registrations_deletion_check AFTER DELETE ON identity.registrations
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_authentication_deletion();

CREATE FUNCTION identity.check_registration_deletion() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE observed_at timestamptz;
BEGIN
    IF TG_TABLE_NAME = 'receipts' THEN
        IF EXISTS (SELECT 1 FROM identity.accounts WHERE id = OLD.account_id AND cancellation_id IS NULL) THEN
            RAISE EXCEPTION 'receipt belongs to the retained account' USING ERRCODE = '23514';
        END IF;
    ELSE
        observed_at := pg_catalog.clock_timestamp();
        IF observed_at < OLD.accepted_at + interval '24 hours'
            OR EXISTS (SELECT 1 FROM identity.registrations WHERE id = OLD.credential_id AND expires_at > observed_at) THEN
            RAISE EXCEPTION 'attempt recovery and quota responsibilities have not ended' USING ERRCODE = '23514';
        END IF;
    END IF;
    RETURN NULL;
END
$$;
CREATE TRIGGER attempts_immutable_check BEFORE UPDATE ON identity.attempts
    FOR EACH ROW EXECUTE FUNCTION access.reject_history_mutation();
CREATE TRIGGER receipts_immutable_check BEFORE UPDATE ON identity.receipts
    FOR EACH ROW EXECUTE FUNCTION access.reject_history_mutation();
CREATE CONSTRAINT TRIGGER attempts_deletion_check AFTER DELETE ON identity.attempts
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_registration_deletion();
CREATE CONSTRAINT TRIGGER receipts_deletion_check AFTER DELETE ON identity.receipts
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_registration_deletion();
CREATE TRIGGER attempts_isolation_check BEFORE INSERT OR DELETE ON identity.attempts
    FOR EACH STATEMENT EXECUTE FUNCTION identity.check_isolation();
CREATE TRIGGER receipts_isolation_check BEFORE INSERT OR DELETE ON identity.receipts
    FOR EACH STATEMENT EXECUTE FUNCTION identity.check_isolation();

REVOKE ALL ON identity.attempts FROM PUBLIC;
REVOKE ALL ON FUNCTION identity.lock_registration(uuid), identity.check_attempt_admission(),
    identity.check_registration_completion(), identity.check_creation(), identity.check_registration_deletion(),
    identity.check_credential(), identity.check_authentication_deletion() FROM PUBLIC;
