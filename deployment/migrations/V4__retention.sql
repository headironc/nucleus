CREATE SEQUENCE identity.ordinals AS bigint MINVALUE 1 CACHE 1 NO CYCLE;

-- Cross-table guards require fresh statement snapshots after lock waits.
CREATE FUNCTION identity.check_isolation() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF pg_catalog.current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION 'C1 writes require read committed isolation' USING ERRCODE = '25000';
    END IF;
    RETURN NULL;
END
$$;
CREATE TRIGGER challenges_isolation_check BEFORE INSERT OR UPDATE OR DELETE ON identity.challenges
    FOR EACH STATEMENT EXECUTE FUNCTION identity.check_isolation();
CREATE TRIGGER messages_isolation_check BEFORE INSERT OR UPDATE OR DELETE ON identity.messages
    FOR EACH STATEMENT EXECUTE FUNCTION identity.check_isolation();
CREATE TRIGGER checks_isolation_check BEFORE INSERT OR UPDATE OR DELETE ON identity.checks
    FOR EACH STATEMENT EXECUTE FUNCTION identity.check_isolation();
CREATE TRIGGER confirmations_isolation_check BEFORE INSERT OR UPDATE OR DELETE ON identity.confirmations
    FOR EACH STATEMENT EXECUTE FUNCTION identity.check_isolation();
CREATE TRIGGER claims_isolation_check BEFORE INSERT OR UPDATE OR DELETE ON identity.claims
    FOR EACH STATEMENT EXECUTE FUNCTION identity.check_isolation();

CREATE FUNCTION identity.check_message_deletion() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE
    message record;
    observed_at timestamptz;
BEGIN
    -- Check and claim deletion acquire the same parent lock as Message deletion.
    PERFORM 1 FROM identity.messages WHERE id = OLD.id FOR UPDATE;
    SELECT status, expires_at, finished_at INTO STRICT message FROM identity.messages WHERE id = OLD.id;
    observed_at := pg_catalog.clock_timestamp();
    IF message.status = 'sending'
        OR observed_at < greatest(message.expires_at, message.finished_at) + interval '24 hours' THEN
        RAISE EXCEPTION 'message responsibilities have not ended' USING ERRCODE = '23514';
    END IF;
    RETURN OLD;
END
$$;
CREATE TRIGGER messages_deletion_check BEFORE DELETE ON identity.messages
    FOR EACH ROW EXECUTE FUNCTION identity.check_message_deletion();
CREATE TRIGGER checks_deletion_check BEFORE DELETE ON identity.checks
    FOR EACH ROW EXECUTE FUNCTION identity.check_message_deletion();
CREATE TRIGGER confirmations_deletion_check BEFORE DELETE ON identity.confirmations
    FOR EACH ROW EXECUTE FUNCTION identity.check_message_deletion();
CREATE TRIGGER claims_deletion_check BEFORE DELETE ON identity.claims
    FOR EACH ROW EXECUTE FUNCTION identity.check_message_deletion();

CREATE FUNCTION identity.check_message_child_deletion() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM identity.messages WHERE id = OLD.id) THEN
        RAISE EXCEPTION 'message companion deletion requires message deletion' USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER claims_deletion_pair_check AFTER DELETE ON identity.claims
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_message_child_deletion();
CREATE CONSTRAINT TRIGGER checks_deletion_pair_check AFTER DELETE ON identity.checks
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_message_child_deletion();
CREATE CONSTRAINT TRIGGER confirmations_deletion_pair_check AFTER DELETE ON identity.confirmations
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_message_child_deletion();

CREATE FUNCTION identity.check_message_parent_deletion() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM identity.challenges WHERE id = OLD.challenge_id) THEN
        RAISE EXCEPTION 'message deletion requires challenge deletion' USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER messages_deletion_parent_check AFTER DELETE ON identity.messages
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_message_parent_deletion();

CREATE FUNCTION identity.check_challenge_deletion() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE
    challenge record;
    observed_at timestamptz;
BEGIN
    PERFORM 1 FROM identity.challenges WHERE id = OLD.id FOR UPDATE;
    SELECT id, status, expires_at, solved_at, finished_at INTO challenge
        FROM identity.challenges WHERE id = OLD.id;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'deletion requires the original challenge' USING ERRCODE = '23514';
    END IF;
    observed_at := pg_catalog.clock_timestamp();
    IF challenge.status NOT IN ('passed', 'rejected', 'inconclusive')
        OR observed_at < greatest(challenge.expires_at, challenge.finished_at) + interval '24 hours'
        OR (challenge.status = 'passed'
            AND observed_at < least(challenge.solved_at, challenge.finished_at) + interval '300 seconds')
        OR EXISTS (SELECT 1 FROM identity.messages WHERE challenge_id = challenge.id) THEN
        RAISE EXCEPTION 'challenge responsibilities have not ended' USING ERRCODE = '23514';
    END IF;
    RETURN OLD;
END
$$;
CREATE TRIGGER challenges_deletion_check BEFORE DELETE ON identity.challenges
    FOR EACH ROW EXECUTE FUNCTION identity.check_challenge_deletion();

REVOKE ALL ON identity.ordinals FROM PUBLIC;
REVOKE ALL ON FUNCTION identity.check_isolation(), identity.check_message_deletion(),
    identity.check_message_child_deletion(), identity.check_message_parent_deletion(),
    identity.check_challenge_deletion() FROM PUBLIC;
