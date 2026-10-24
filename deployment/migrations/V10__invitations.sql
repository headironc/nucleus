CREATE TABLE access.invitations (
    id uuid PRIMARY KEY,
    domain text NOT NULL DEFAULT 'workforce' CHECK (domain = 'workforce'),
    person_id uuid NOT NULL,
    grant_id uuid NOT NULL,
    epoch bigint NOT NULL CHECK (epoch >= 0),
    role_id uuid NOT NULL,
    change_id uuid NOT NULL,
    operator_id uuid NOT NULL,
    capacity bigint NOT NULL CHECK (capacity > 0),
    expires_at timestamptz NOT NULL CHECK (isfinite(expires_at)),
    code bytea NOT NULL CHECK (octet_length(code) = 69),
    hash bytea NOT NULL CHECK (octet_length(hash) = 32),
    key_id bigint NOT NULL REFERENCES security.keys (id),
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    operation_id uuid NOT NULL UNIQUE,
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    current_capacity bigint NOT NULL CHECK (current_capacity > 0),
    current_expiry timestamptz NOT NULL CHECK (isfinite(current_expiry)),
    used bigint NOT NULL CHECK (used >= 0 AND used <= current_capacity),
    withdrawn_at timestamptz,
    changed_at timestamptz NOT NULL CHECK (isfinite(changed_at)),
    sequence bigint NOT NULL CHECK (sequence > 0),
    version bigint NOT NULL CHECK (version > 0),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    CHECK (expires_at > created_at AND changed_at >= created_at AND current_expiry > changed_at),
    CHECK (withdrawn_at IS NULL OR withdrawn_at = changed_at),
    UNIQUE (key_id, hash),
    FOREIGN KEY (change_id, role_id, domain) REFERENCES access.changes (id, role_id, scope),
    FOREIGN KEY (grant_id, person_id) REFERENCES access.grants (id, person_id),
    FOREIGN KEY (operator_id, person_id) REFERENCES identity.revisions (id, person_id)
);
CREATE INDEX invitations_role_id_idx ON access.invitations (role_id);
CREATE INDEX invitations_grant_id_idx ON access.invitations (grant_id);

CREATE TABLE access.creations (
    id uuid PRIMARY KEY REFERENCES access.invitations (id) DEFERRABLE INITIALLY DEFERRED,
    sequence bigint NOT NULL CHECK (sequence = 1),
    capacity bigint NOT NULL CHECK (capacity > 0),
    expires_at timestamptz NOT NULL CHECK (isfinite(expires_at)),
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    operation_id uuid NOT NULL UNIQUE,
    family text NOT NULL DEFAULT 'invitation' CHECK (family = 'invitation'),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    CHECK (expires_at > created_at),
    FOREIGN KEY (family, operation_id) REFERENCES access.completions (family, operation_id)
        DEFERRABLE INITIALLY DEFERRED
);

CREATE TABLE access.adjustments (
    id uuid PRIMARY KEY,
    invitation_id uuid NOT NULL REFERENCES access.invitations (id),
    sequence bigint NOT NULL CHECK (sequence > 1),
    capacity bigint NOT NULL CHECK (capacity > 0),
    expires_at timestamptz NOT NULL CHECK (isfinite(expires_at)),
    used bigint NOT NULL CHECK (used >= 0 AND used <= capacity),
    adjusted_at timestamptz NOT NULL CHECK (isfinite(adjusted_at)),
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    operation_id uuid NOT NULL UNIQUE,
    family text NOT NULL DEFAULT 'invitation_adjustment' CHECK (family = 'invitation_adjustment'),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    CHECK (expires_at > adjusted_at),
    UNIQUE (invitation_id, sequence),
    FOREIGN KEY (family, operation_id) REFERENCES access.completions (family, operation_id)
        DEFERRABLE INITIALLY DEFERRED
);

CREATE TABLE access.withdrawals (
    id uuid PRIMARY KEY REFERENCES access.invitations (id),
    sequence bigint NOT NULL CHECK (sequence > 1),
    capacity bigint NOT NULL CHECK (capacity > 0),
    expires_at timestamptz NOT NULL CHECK (isfinite(expires_at)),
    used bigint NOT NULL CHECK (used >= 0 AND used <= capacity),
    withdrawn_at timestamptz NOT NULL CHECK (isfinite(withdrawn_at)),
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    operation_id uuid NOT NULL UNIQUE,
    family text NOT NULL DEFAULT 'invitation_withdrawal' CHECK (family = 'invitation_withdrawal'),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    CHECK (expires_at > withdrawn_at),
    FOREIGN KEY (family, operation_id) REFERENCES access.completions (family, operation_id)
        DEFERRABLE INITIALLY DEFERRED
);

CREATE TABLE access.acceptances (
    id uuid PRIMARY KEY REFERENCES access.grants (id),
    invitation_id uuid NOT NULL REFERENCES access.invitations (id),
    sequence bigint NOT NULL CHECK (sequence > 1),
    capacity bigint NOT NULL CHECK (capacity > 0),
    expires_at timestamptz NOT NULL CHECK (isfinite(expires_at)),
    used bigint NOT NULL CHECK (used > 0 AND used <= capacity),
    accepted_at timestamptz NOT NULL CHECK (isfinite(accepted_at)),
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    operation_id uuid NOT NULL UNIQUE,
    family text NOT NULL DEFAULT 'grant' CHECK (family = 'grant'),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    CHECK (expires_at > accepted_at),
    UNIQUE (invitation_id, sequence),
    UNIQUE (id, invitation_id),
    FOREIGN KEY (family, operation_id) REFERENCES access.completions (family, operation_id)
        DEFERRABLE INITIALLY DEFERRED
);
-- Grant and Acceptance refer to each other and are inserted in one transaction.
ALTER TABLE access.grants ADD FOREIGN KEY (id, invitation_id)
    REFERENCES access.acceptances (id, invitation_id) DEFERRABLE INITIALLY DEFERRED;

CREATE VIEW access.invitation_transitions AS
    SELECT id AS invitation_id,sequence,capacity,expires_at,0::bigint AS used,NULL::timestamptz AS withdrawn_at,
        created_at AS happened_at,operator_id,operation_id,transaction_id,xmin AS inserted_in
    FROM access.creations
    UNION ALL
    SELECT invitation_id,sequence,capacity,expires_at,used,NULL::timestamptz,
        adjusted_at,operator_id,operation_id,transaction_id,xmin FROM access.adjustments
    UNION ALL
    SELECT id,sequence,capacity,expires_at,used,withdrawn_at,
        withdrawn_at,operator_id,operation_id,transaction_id,xmin FROM access.withdrawals
    UNION ALL
    SELECT invitation_id,sequence,capacity,expires_at,used,NULL::timestamptz,
        accepted_at,operator_id,operation_id,transaction_id,xmin FROM access.acceptances;

-- Only structural pairing belongs in SQL; current authority and availability belong to Rust.
CREATE FUNCTION access.check_invitation_state() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE previous_sequence bigint; previous_version bigint;
BEGIN
    previous_sequence := CASE WHEN TG_OP = 'INSERT' THEN 0 ELSE OLD.sequence END;
    previous_version := CASE WHEN TG_OP = 'INSERT' THEN 0 ELSE OLD.version END;
    IF NEW.sequence <> previous_sequence + 1 OR NEW.version <> previous_version + 1
        OR (TG_OP = 'UPDATE' AND
            ROW(NEW.id,NEW.domain,NEW.person_id,NEW.grant_id,NEW.epoch,NEW.role_id,NEW.change_id,
                NEW.operator_id,NEW.capacity,NEW.expires_at,NEW.code,NEW.hash,NEW.key_id,
                NEW.created_at,NEW.operation_id,NEW.transaction_id)
            IS DISTINCT FROM
            ROW(OLD.id,OLD.domain,OLD.person_id,OLD.grant_id,OLD.epoch,OLD.role_id,OLD.change_id,
                OLD.operator_id,OLD.capacity,OLD.expires_at,OLD.code,OLD.hash,OLD.key_id,
                OLD.created_at,OLD.operation_id,OLD.transaction_id))
        OR (SELECT count(*) FROM access.invitation_transitions record
            WHERE record.invitation_id=NEW.id AND record.sequence=NEW.sequence
                AND record.inserted_in=pg_catalog.pg_current_xact_id()::text::xid) <> 1
        OR NOT EXISTS (SELECT 1 FROM access.invitation_transitions head
            WHERE head.invitation_id=NEW.id AND head.sequence=NEW.sequence
                AND ROW(head.capacity,head.expires_at,head.used,head.withdrawn_at,head.happened_at)
                    IS NOT DISTINCT FROM
                    ROW(NEW.current_capacity,NEW.current_expiry,NEW.used,NEW.withdrawn_at,NEW.changed_at))
        OR NOT EXISTS (SELECT 1 FROM access.creations first
            WHERE first.id=NEW.id AND
                ROW(first.capacity,first.expires_at,first.created_at,first.operator_id,first.operation_id,first.transaction_id)
                    IS NOT DISTINCT FROM
                    ROW(NEW.capacity,NEW.expires_at,NEW.created_at,NEW.operator_id,NEW.operation_id,NEW.transaction_id)) THEN
        RAISE EXCEPTION 'invitation advancement requires its immutable creation and paired record'
            USING ERRCODE = '23514', CONSTRAINT = 'invitations_state_check';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER invitations_state_check AFTER INSERT OR UPDATE ON access.invitations
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION access.check_invitation_state();
CREATE TRIGGER invitations_isolation_check BEFORE INSERT OR UPDATE ON access.invitations
    FOR EACH STATEMENT EXECUTE FUNCTION identity.check_isolation();
CREATE TRIGGER invitations_deletion_check BEFORE DELETE ON access.invitations
    FOR EACH ROW EXECUTE FUNCTION access.reject_history_mutation();

CREATE FUNCTION access.check_invitation_record() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE target uuid;
BEGIN
    IF TG_TABLE_NAME IN ('creations','withdrawals') THEN
        target := NEW.id;
    ELSE
        target := NEW.invitation_id;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM access.invitations state
        JOIN identity.revisions operator ON operator.id=NEW.operator_id AND operator.person_id=state.person_id
        WHERE state.id=target AND state.sequence>=NEW.sequence
            AND state.xmin=pg_catalog.pg_current_xact_id()::text::xid)
        OR NEW.transaction_id<>pg_catalog.pg_current_xact_id()
        OR (SELECT count(*) FROM access.invitation_transitions record
            WHERE record.invitation_id=target AND record.sequence=NEW.sequence)<>1 THEN
        RAISE EXCEPTION 'invitation record requires its same-transaction state and unique sequence'
            USING ERRCODE = '23514', CONSTRAINT = 'invitation_records_check';
    END IF;
    RETURN NULL;
END
$$;

CREATE FUNCTION access.check_acceptance_pair() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM access.grants admission
        JOIN access.assignments first ON first.grant_id=admission.id AND first.sequence=1
        JOIN access.invitations invitation ON invitation.id=NEW.invitation_id
        WHERE admission.id=NEW.id AND admission.entry='invitation'
            AND admission.invitation_id=NEW.invitation_id AND admission.domain=invitation.domain
            AND ROW(admission.operator_id,admission.granted_at,admission.operation_id,admission.transaction_id)
                IS NOT DISTINCT FROM ROW(NEW.operator_id,NEW.accepted_at,NEW.operation_id,NEW.transaction_id)
            AND first.role_id=invitation.role_id AND first.transaction_id=NEW.transaction_id
            AND admission.xmin=pg_catalog.pg_current_xact_id()::text::xid
            AND first.xmin=pg_catalog.pg_current_xact_id()::text::xid) THEN
        RAISE EXCEPTION 'acceptance requires its new Grant and first assignment'
            USING ERRCODE = '23514', CONSTRAINT = 'acceptances_pair_check';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER acceptances_pair_check AFTER INSERT ON access.acceptances
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION access.check_acceptance_pair();

CREATE CONSTRAINT TRIGGER creations_state_check AFTER INSERT ON access.creations
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION access.check_invitation_record();
CREATE CONSTRAINT TRIGGER adjustments_state_check AFTER INSERT ON access.adjustments
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION access.check_invitation_record();
CREATE CONSTRAINT TRIGGER withdrawals_state_check AFTER INSERT ON access.withdrawals
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION access.check_invitation_record();
CREATE CONSTRAINT TRIGGER acceptances_state_check AFTER INSERT ON access.acceptances
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION access.check_invitation_record();
CREATE TRIGGER creations_isolation_check BEFORE INSERT ON access.creations
    FOR EACH STATEMENT EXECUTE FUNCTION identity.check_isolation();
CREATE TRIGGER adjustments_isolation_check BEFORE INSERT ON access.adjustments
    FOR EACH STATEMENT EXECUTE FUNCTION identity.check_isolation();
CREATE TRIGGER withdrawals_isolation_check BEFORE INSERT ON access.withdrawals
    FOR EACH STATEMENT EXECUTE FUNCTION identity.check_isolation();
CREATE TRIGGER acceptances_isolation_check BEFORE INSERT ON access.acceptances
    FOR EACH STATEMENT EXECUTE FUNCTION identity.check_isolation();
CREATE TRIGGER creations_immutable_check BEFORE UPDATE OR DELETE ON access.creations
    FOR EACH ROW EXECUTE FUNCTION access.reject_history_mutation();
CREATE TRIGGER adjustments_immutable_check BEFORE UPDATE OR DELETE ON access.adjustments
    FOR EACH ROW EXECUTE FUNCTION access.reject_history_mutation();
CREATE TRIGGER withdrawals_immutable_check BEFORE UPDATE OR DELETE ON access.withdrawals
    FOR EACH ROW EXECUTE FUNCTION access.reject_history_mutation();
CREATE TRIGGER acceptances_immutable_check BEFORE UPDATE OR DELETE ON access.acceptances
    FOR EACH ROW EXECUTE FUNCTION access.reject_history_mutation();
