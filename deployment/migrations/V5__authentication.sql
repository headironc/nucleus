-- R1 reads current account ownership. Account creation and management are not granted.
CREATE TABLE identity.completions (
    family text NOT NULL CHECK (family IN ('logout', 'suspension', 'resumption', 'mobile_completion', 'correction', 'system_correction', 'cancellation')),
    operation_id uuid NOT NULL,
    action text NOT NULL CHECK (
        (family = 'logout' AND action = 'logout')
        OR (family = 'suspension' AND action = 'suspend')
        OR (family = 'resumption' AND action = 'resume')
        OR (family = 'mobile_completion' AND action IN ('self', 'assisted'))
        OR (family = 'cancellation' AND action IN ('self', 'managed'))
        OR (family IN ('correction', 'system_correction') AND action = 'correct')),
    target text NOT NULL CHECK (target <> ''
        AND (family NOT IN ('correction', 'system_correction', 'cancellation')
            OR target ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$')),
    input bytea NOT NULL CHECK (octet_length(input) >= 37),
    subject text NOT NULL CHECK ((family = 'system_correction' AND subject = 'system')
        OR (family <> 'system_correction' AND subject = 'session')),
    person_id uuid NOT NULL REFERENCES identity.persons (id),
    revision_id uuid NOT NULL,
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    expires_at timestamptz CHECK ((family = 'system_correction' AND expires_at IS NULL)
        OR (family = 'mobile_completion' AND (expires_at IS NULL
            OR (isfinite(expires_at) AND expires_at >= created_at + interval '24 hours')))
        OR (family NOT IN ('mobile_completion', 'system_correction') AND expires_at IS NULL)),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    PRIMARY KEY (family, operation_id),
    UNIQUE (family, operation_id, person_id),
    FOREIGN KEY (revision_id, person_id) REFERENCES identity.revisions (id, person_id)
);

CREATE TABLE identity.accounts (
    id uuid PRIMARY KEY,
    person_id uuid NOT NULL UNIQUE REFERENCES identity.persons (id),
    mobile identity.mobile,
    id_number bytea CONSTRAINT accounts_id_number_key UNIQUE CHECK (octet_length(id_number) = 32),
    avatar uuid NOT NULL,
    wechat uuid NOT NULL,
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    changed_at timestamptz NOT NULL CHECK (isfinite(changed_at) AND changed_at >= created_at),
    suspension_id uuid,
    cancellation_id uuid,
    sequence bigint NOT NULL CHECK (sequence > 0),
    version bigint NOT NULL CHECK (version > 0),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    CHECK ((cancellation_id IS NULL AND (mobile).value IS NOT NULL AND (mobile).hash IS NOT NULL AND id_number IS NOT NULL)
        OR (cancellation_id IS NOT NULL AND mobile IS NULL AND id_number IS NULL AND suspension_id IS NULL)),
    CHECK (avatar <> wechat),
    UNIQUE (id, person_id)
);
CREATE UNIQUE INDEX accounts_mobile_hash_key ON identity.accounts (((mobile).hash));
CREATE TABLE identity.creations (
    id uuid PRIMARY KEY,
    person_id uuid NOT NULL,
    revision_id uuid NOT NULL,
    source text NOT NULL CHECK (source IN ('initialization', 'registration')),
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    sequence bigint NOT NULL CHECK (sequence = 1),
    operation_id uuid NOT NULL,
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    FOREIGN KEY (id, person_id) REFERENCES identity.accounts (id, person_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (revision_id, person_id) REFERENCES identity.revisions (id, person_id),
    UNIQUE (id, person_id, revision_id, operation_id, created_at, transaction_id)
);
-- Each account and its first permanent record are created together.
ALTER TABLE identity.accounts ADD FOREIGN KEY (id)
    REFERENCES identity.creations (id) DEFERRABLE INITIALLY DEFERRED;

-- The first Person record fixes the original revision, account, operation and time. It is
-- written before the Person state and binds structurally to the Account creation.
CREATE TABLE identity.origins (
    person_id uuid PRIMARY KEY,
    account_id uuid NOT NULL UNIQUE,
    revision_id uuid NOT NULL,
    operation_id uuid NOT NULL,
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    sequence bigint NOT NULL CHECK (sequence = 1),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    FOREIGN KEY (account_id, person_id, revision_id, operation_id, created_at, transaction_id)
        REFERENCES identity.creations (id, person_id, revision_id, operation_id, created_at, transaction_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (person_id) REFERENCES identity.persons (id) DEFERRABLE INITIALLY DEFERRED
);
ALTER TABLE identity.creations ADD FOREIGN KEY (person_id)
    REFERENCES identity.origins (person_id) DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE identity.receipts (
    id uuid PRIMARY KEY,
    operation_id uuid NOT NULL UNIQUE,
    credential_id uuid NOT NULL UNIQUE,
    app_id text NOT NULL CHECK (app_id <> '' AND app_id !~ '[[:space:][:cntrl:]]'),
    method text NOT NULL CHECK (method = 'message'),
    transport text NOT NULL CHECK (transport = 'cookie'),
    proof bytea NOT NULL CHECK (octet_length(proof) = 32),
    account_id uuid NOT NULL UNIQUE REFERENCES identity.accounts (id),
    person_id uuid NOT NULL REFERENCES identity.persons (id),
    revision_id uuid NOT NULL,
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    FOREIGN KEY (revision_id, person_id) REFERENCES identity.revisions (id, person_id)
);
CREATE TABLE identity.suspensions (
    id uuid PRIMARY KEY,
    family text GENERATED ALWAYS AS ('suspension') STORED,
    account_id uuid NOT NULL,
    person_id uuid NOT NULL REFERENCES identity.persons (id),
    revision_id uuid NOT NULL,
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    mobile bytea NOT NULL CHECK (octet_length(mobile) = 48),
    suspended_at timestamptz NOT NULL CHECK (isfinite(suspended_at)),
    sequence bigint NOT NULL CHECK (sequence > 0),
    operation_id uuid NOT NULL UNIQUE,
    sessions uuid[] NOT NULL CHECK (array_position(sessions, NULL) IS NULL),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    FOREIGN KEY (family, operation_id) REFERENCES identity.completions (family, operation_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (account_id, person_id) REFERENCES identity.accounts (id, person_id),
    FOREIGN KEY (revision_id, person_id) REFERENCES identity.revisions (id, person_id),
    UNIQUE (id, person_id),
    UNIQUE (id, account_id),
    UNIQUE (account_id, sequence)
);
CREATE TABLE identity.resumptions (
    id uuid PRIMARY KEY,
    family text GENERATED ALWAYS AS ('resumption') STORED,
    account_id uuid NOT NULL,
    person_id uuid NOT NULL,
    revision_id uuid NOT NULL,
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    mobile bytea NOT NULL CHECK (octet_length(mobile) = 48),
    resumed_at timestamptz NOT NULL CHECK (isfinite(resumed_at)),
    sequence bigint NOT NULL CHECK (sequence > 1),
    operation_id uuid NOT NULL UNIQUE,
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    FOREIGN KEY (family, operation_id) REFERENCES identity.completions (family, operation_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (id, person_id) REFERENCES identity.suspensions (id, person_id),
    FOREIGN KEY (id, account_id) REFERENCES identity.suspensions (id, account_id),
    FOREIGN KEY (revision_id, person_id) REFERENCES identity.revisions (id, person_id),
    UNIQUE (account_id, sequence)
);

CREATE TABLE identity.cancellations (
    id uuid PRIMARY KEY,
    family text GENERATED ALWAYS AS ('cancellation') STORED,
    account_id uuid NOT NULL UNIQUE,
    person_id uuid NOT NULL UNIQUE REFERENCES identity.persons (id),
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    revision_id uuid NOT NULL,
    mobile bytea NOT NULL CHECK (octet_length(mobile) = 48),
    cancelled_at timestamptz NOT NULL CHECK (isfinite(cancelled_at)),
    sequence bigint NOT NULL CHECK (sequence > 0),
    operation_id uuid NOT NULL UNIQUE,
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    FOREIGN KEY (family, operation_id) REFERENCES identity.completions (family, operation_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (revision_id, person_id) REFERENCES identity.revisions (id, person_id),
    UNIQUE (id, account_id),
    UNIQUE (person_id, sequence)
);
CREATE INDEX cancellations_cancelled_at_idx ON identity.cancellations (cancelled_at DESC);

-- Account state points at its own permanent causes, which in turn refer to Account.
ALTER TABLE identity.accounts ADD FOREIGN KEY (suspension_id, id)
    REFERENCES identity.suspensions (id, account_id) DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE identity.accounts ADD FOREIGN KEY (cancellation_id, id)
    REFERENCES identity.cancellations (id, account_id) DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE identity.replacements (
    id uuid PRIMARY KEY,
    family text GENERATED ALWAYS AS ('mobile_completion') STORED,
    account_id uuid NOT NULL,
    person_id uuid NOT NULL,
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    -- Original inputs survive the legal cleanup of their session, message and transfer.
    session_id uuid NOT NULL,
    message_id uuid NOT NULL,
    completed_at timestamptz NOT NULL CHECK (isfinite(completed_at)),
    sequence bigint NOT NULL CHECK (sequence > 1),
    operation_id uuid NOT NULL UNIQUE,
    sessions uuid[] NOT NULL CHECK (array_position(sessions, NULL) IS NULL),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    FOREIGN KEY (family, operation_id) REFERENCES identity.completions (family, operation_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (account_id, person_id) REFERENCES identity.accounts (id, person_id),
    UNIQUE (id, account_id),
    UNIQUE (account_id, sequence)
);
CREATE INDEX replacements_account_id_idx ON identity.replacements (account_id);

CREATE TABLE identity.logouts (
    id uuid PRIMARY KEY,
    family text GENERATED ALWAYS AS ('logout'::text) STORED,
    account_id uuid NOT NULL,
    person_id uuid NOT NULL,
    sequence bigint NOT NULL CHECK (sequence > 1),
    revoked_at timestamptz NOT NULL CHECK (isfinite(revoked_at)),
    initiator_id uuid NOT NULL,
    scope text NOT NULL CHECK (scope IN ('current', 'all')),
    sessions uuid[] NOT NULL CHECK (array_position(sessions, NULL) IS NULL),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    FOREIGN KEY (account_id, person_id) REFERENCES identity.accounts (id, person_id),
    FOREIGN KEY (family, id, person_id) REFERENCES identity.completions (family, operation_id, person_id)
        DEFERRABLE INITIALLY DEFERRED,
    UNIQUE (account_id, sequence),
    UNIQUE (id, account_id),
    CHECK (cardinality(sessions) > 0 AND initiator_id = ANY(sessions)),
    CHECK (scope = 'all' OR sessions = ARRAY[initiator_id])
);

CREATE TABLE identity.projections (
    id uuid NOT NULL,
    cause text NOT NULL CHECK (cause IN ('correction', 'cancellation')),
    account_id uuid NOT NULL,
    person_id uuid NOT NULL,
    sequence bigint NOT NULL CHECK (sequence > 1),
    operation_id uuid NOT NULL,
    projected_at timestamptz NOT NULL CHECK (isfinite(projected_at)),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    PRIMARY KEY (cause, id),
    FOREIGN KEY (account_id, person_id) REFERENCES identity.accounts (id, person_id),
    UNIQUE (account_id, sequence),
    UNIQUE (cause, operation_id)
);

-- Child deletion proves its original ownership before Account is removed.
CREATE FUNCTION identity.cancelling(uuid) RETURNS boolean
LANGUAGE sql VOLATILE SET search_path = '' AS $$
    SELECT EXISTS (SELECT 1 FROM identity.accounts account JOIN identity.cancellations cancellation
        ON cancellation.account_id = account.id AND cancellation.person_id = account.person_id
        WHERE account.id = $1 AND cancellation.transaction_id = pg_catalog.pg_current_xact_id())
$$;

CREATE FUNCTION identity.lock_account(uuid) RETURNS void
LANGUAGE sql VOLATILE SECURITY DEFINER SET search_path = '' AS $$
    SELECT FROM identity.accounts WHERE id = $1 FOR SHARE
$$;

-- The first account is the actor (SHARE), the second is the target (UPDATE).
CREATE FUNCTION identity.lock_accounts(uuid, uuid) RETURNS void
LANGUAGE plpgsql VOLATILE SECURITY DEFINER SET search_path = '' AS $$
BEGIN
    IF $1 = $2 THEN
        PERFORM FROM identity.accounts WHERE id = $2 FOR UPDATE;
    ELSIF $1 < $2 THEN
        PERFORM FROM identity.accounts WHERE id = $1 FOR SHARE;
        PERFORM FROM identity.accounts WHERE id = $2 FOR UPDATE;
    ELSE
        PERFORM FROM identity.accounts WHERE id = $2 FOR UPDATE;
        PERFORM FROM identity.accounts WHERE id = $1 FOR SHARE;
    END IF;
END
$$;

CREATE TABLE identity.sessions (
    id uuid PRIMARY KEY,
    authentication_id uuid NOT NULL UNIQUE REFERENCES identity.authentications (id),
    account_id uuid NOT NULL REFERENCES identity.accounts (id),
    sequence bigint NOT NULL CHECK (sequence IN (1, 2)),
    version bigint NOT NULL CHECK (version = sequence),
    hash bytea NOT NULL CHECK (octet_length(hash) = 32),
    method text NOT NULL CHECK (method = 'message'),
    app_id text NOT NULL CHECK (app_id <> '' AND app_id !~ '[[:space:][:cntrl:]]'),
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    expires_at timestamptz NOT NULL CHECK (isfinite(expires_at) AND expires_at > created_at),
    revoked_at timestamptz CHECK (isfinite(revoked_at) AND revoked_at >= created_at),
    revocation_id uuid GENERATED ALWAYS AS (CASE WHEN revoked_at IS NOT NULL THEN id END) STORED,
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    receipt_id uuid UNIQUE REFERENCES identity.receipts (id),
    CHECK ((sequence = 1 AND revoked_at IS NULL) OR (sequence = 2 AND revoked_at IS NOT NULL)),
    UNIQUE (id, account_id),
    UNIQUE (id, account_id, revoked_at)
);
CREATE INDEX sessions_expires_at_id_idx ON identity.sessions (expires_at, id);
CREATE INDEX sessions_account_id_id_idx ON identity.sessions (account_id, id);

CREATE TABLE identity.issuances (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL,
    sequence bigint NOT NULL CHECK (sequence = 1),
    operation_id uuid NOT NULL,
    authentication_id uuid NOT NULL REFERENCES identity.authentications (id),
    receipt_id uuid REFERENCES identity.receipts (id),
    hash bytea NOT NULL CHECK (octet_length(hash) = 32),
    method text NOT NULL CHECK (method = 'message'),
    app_id text NOT NULL CHECK (app_id <> '' AND app_id !~ '[[:space:][:cntrl:]]'),
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    expires_at timestamptz NOT NULL CHECK (isfinite(expires_at) AND expires_at > created_at),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    FOREIGN KEY (id, account_id) REFERENCES identity.sessions (id, account_id) DEFERRABLE INITIALLY DEFERRED
);
-- A retained session always retains its issuance; both disappear in one retention transaction.
ALTER TABLE identity.sessions ADD FOREIGN KEY (id)
    REFERENCES identity.issuances (id) DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE identity.revocations (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL,
    sequence bigint NOT NULL CHECK (sequence = 2),
    mode text NOT NULL CHECK (mode IN ('logout', 'suspension', 'transfer')),
    cause_id uuid NOT NULL,
    operation_id uuid NOT NULL,
    revoked_at timestamptz NOT NULL CHECK (isfinite(revoked_at)),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    logout_id uuid GENERATED ALWAYS AS (CASE WHEN mode = 'logout' THEN cause_id END) STORED,
    suspension_id uuid GENERATED ALWAYS AS (CASE WHEN mode = 'suspension' THEN cause_id END) STORED,
    replacement_id uuid GENERATED ALWAYS AS (CASE WHEN mode = 'transfer' THEN cause_id END) STORED,
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    FOREIGN KEY (id, account_id, revoked_at) REFERENCES identity.sessions (id, account_id, revoked_at)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (logout_id, account_id) REFERENCES identity.logouts (id, account_id),
    FOREIGN KEY (suspension_id, account_id) REFERENCES identity.suspensions (id, account_id),
    FOREIGN KEY (replacement_id, account_id) REFERENCES identity.replacements (id, account_id),
    UNIQUE (id, account_id, revoked_at),
    CHECK (mode <> 'logout' OR cause_id = operation_id)
);
-- The optional state pointer and its record are written or removed as one group.
ALTER TABLE identity.sessions ADD FOREIGN KEY (revocation_id, account_id, revoked_at)
    REFERENCES identity.revocations (id, account_id, revoked_at) DEFERRABLE INITIALLY DEFERRED;
CREATE INDEX revocations_account_id_idx ON identity.revocations (account_id);
CREATE INDEX revocations_mode_operation_id_id_idx ON identity.revocations (mode, operation_id, id);
CREATE TABLE identity.handoffs (
    id uuid PRIMARY KEY REFERENCES identity.sessions (id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    token bytea NOT NULL CHECK (octet_length(token) = 69),
    expires_at timestamptz NOT NULL CHECK (isfinite(expires_at))
);
CREATE TABLE identity.registrations (
    id uuid PRIMARY KEY,
    authentication_id uuid NOT NULL UNIQUE REFERENCES identity.authentications (id),
    mobile identity.mobile NOT NULL,
    token bytea NOT NULL CHECK (octet_length(token) = 69),
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    expires_at timestamptz NOT NULL CHECK (isfinite(expires_at) AND expires_at > created_at),
    CHECK ((mobile).value IS NOT NULL AND (mobile).hash IS NOT NULL)
);

CREATE UNIQUE INDEX registrations_mobile_hash_key ON identity.registrations (((mobile).hash));
CREATE INDEX registrations_expires_at_id_idx ON identity.registrations (expires_at, id);

-- R2 arbitrates account uniqueness in a savepoint. Stamp the parent transaction on
-- insertion; immutable records keep that server-owned pairing after subtransaction release.
CREATE FUNCTION identity.stamp_record() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    NEW.transaction_id := pg_catalog.pg_current_xact_id();
    RETURN NEW;
END
$$;
CREATE FUNCTION identity.reject_record_mutation() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    RAISE EXCEPTION 'sealed history is immutable' USING ERRCODE = '23514';
END
$$;
CREATE TRIGGER creations_transaction_check BEFORE INSERT ON identity.creations
    FOR EACH ROW EXECUTE FUNCTION identity.stamp_record();
CREATE TRIGGER creations_immutable_check BEFORE UPDATE OR DELETE ON identity.creations
    FOR EACH ROW EXECUTE FUNCTION identity.reject_record_mutation();
CREATE TRIGGER suspensions_transaction_check BEFORE INSERT ON identity.suspensions
    FOR EACH ROW EXECUTE FUNCTION identity.stamp_record();
CREATE TRIGGER resumptions_transaction_check BEFORE INSERT ON identity.resumptions
    FOR EACH ROW EXECUTE FUNCTION identity.stamp_record();
CREATE TRIGGER replacements_transaction_check BEFORE INSERT ON identity.replacements
    FOR EACH ROW EXECUTE FUNCTION identity.stamp_record();
CREATE TRIGGER logouts_transaction_check BEFORE INSERT ON identity.logouts
    FOR EACH ROW EXECUTE FUNCTION identity.stamp_record();
CREATE TRIGGER logouts_immutable_check BEFORE UPDATE OR DELETE ON identity.logouts
    FOR EACH ROW EXECUTE FUNCTION identity.reject_record_mutation();
CREATE TRIGGER projections_transaction_check BEFORE INSERT ON identity.projections
    FOR EACH ROW EXECUTE FUNCTION identity.stamp_record();
CREATE TRIGGER projections_immutable_check BEFORE UPDATE OR DELETE ON identity.projections
    FOR EACH ROW EXECUTE FUNCTION identity.reject_record_mutation();
CREATE TRIGGER issuances_transaction_check BEFORE INSERT ON identity.issuances
    FOR EACH ROW EXECUTE FUNCTION identity.stamp_record();
CREATE TRIGGER issuances_immutable_check BEFORE UPDATE ON identity.issuances
    FOR EACH ROW EXECUTE FUNCTION identity.reject_record_mutation();
CREATE TRIGGER revocations_transaction_check BEFORE INSERT ON identity.revocations
    FOR EACH ROW EXECUTE FUNCTION identity.stamp_record();

CREATE FUNCTION identity.check_account_records() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE previous_sequence bigint; previous_version bigint;
BEGIN
    previous_sequence := CASE WHEN TG_OP = 'INSERT' THEN 0 ELSE OLD.sequence END;
    previous_version := CASE WHEN TG_OP = 'INSERT' THEN 0 ELSE OLD.version END;
    IF NEW.sequence <= previous_sequence OR NEW.version <> previous_version + 1
        OR (TG_OP = 'UPDATE' AND ROW(NEW.id, NEW.person_id, NEW.created_at, NEW.transaction_id)
            IS DISTINCT FROM ROW(OLD.id, OLD.person_id, OLD.created_at, OLD.transaction_id))
        OR EXISTS (
            SELECT 1 FROM pg_catalog.generate_series(previous_sequence + 1, NEW.sequence) expected(sequence)
            WHERE (SELECT count(*) FROM (
                SELECT sequence FROM identity.creations WHERE id = NEW.id
                    AND sequence = expected.sequence AND transaction_id = pg_catalog.pg_current_xact_id()
                UNION ALL SELECT sequence FROM identity.suspensions WHERE account_id = NEW.id
                    AND sequence = expected.sequence AND transaction_id = pg_catalog.pg_current_xact_id()
                UNION ALL SELECT sequence FROM identity.resumptions WHERE account_id = NEW.id
                    AND sequence = expected.sequence AND transaction_id = pg_catalog.pg_current_xact_id()
                UNION ALL SELECT sequence FROM identity.replacements WHERE account_id = NEW.id
                    AND sequence = expected.sequence AND transaction_id = pg_catalog.pg_current_xact_id()
                UNION ALL SELECT sequence FROM identity.logouts WHERE account_id = NEW.id
                    AND sequence = expected.sequence AND transaction_id = pg_catalog.pg_current_xact_id()
                UNION ALL SELECT sequence FROM identity.projections WHERE account_id = NEW.id
                    AND sequence = expected.sequence AND transaction_id = pg_catalog.pg_current_xact_id()
            ) records) <> 1)
        OR NOT EXISTS (
            SELECT 1 FROM (
                SELECT id AS account_id, person_id, sequence, created_at AS happened_at FROM identity.creations
                UNION ALL SELECT account_id, person_id, sequence, suspended_at FROM identity.suspensions
                UNION ALL SELECT account_id, person_id, sequence, resumed_at FROM identity.resumptions
                UNION ALL SELECT account_id, person_id, sequence, completed_at FROM identity.replacements
                UNION ALL SELECT account_id, person_id, sequence, revoked_at FROM identity.logouts
                UNION ALL SELECT account_id, person_id, sequence, projected_at FROM identity.projections
            ) record WHERE record.account_id = NEW.id AND record.person_id = NEW.person_id
                AND record.sequence = NEW.sequence AND record.happened_at = NEW.changed_at)
    THEN
        RAISE EXCEPTION 'account advancement requires its contiguous records and paired head'
            USING ERRCODE = '23514', CONSTRAINT = 'accounts_records_check';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER accounts_records_check AFTER INSERT OR UPDATE ON identity.accounts
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_account_records();
CREATE TRIGGER accounts_permanent_check BEFORE DELETE ON identity.accounts
    FOR EACH ROW EXECUTE FUNCTION identity.reject_record_mutation();

CREATE FUNCTION identity.check_session_records() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE previous_sequence bigint; previous_version bigint;
BEGIN
    previous_sequence := CASE WHEN TG_OP = 'INSERT' THEN 0 ELSE OLD.sequence END;
    previous_version := CASE WHEN TG_OP = 'INSERT' THEN 0 ELSE OLD.version END;
    IF NEW.sequence <> previous_sequence + 1 OR NEW.version <> previous_version + 1
        OR (TG_OP = 'UPDATE' AND ROW(NEW.id, NEW.account_id, NEW.authentication_id, NEW.receipt_id,
            NEW.hash, NEW.method, NEW.app_id, NEW.created_at, NEW.expires_at, NEW.transaction_id)
            IS DISTINCT FROM ROW(OLD.id, OLD.account_id, OLD.authentication_id, OLD.receipt_id,
            OLD.hash, OLD.method, OLD.app_id, OLD.created_at, OLD.expires_at, OLD.transaction_id))
        OR NOT EXISTS (SELECT 1 FROM identity.issuances record
            WHERE record.id = NEW.id AND ROW(record.account_id, record.authentication_id, record.receipt_id,
                record.hash, record.method, record.app_id, record.created_at, record.expires_at)
                IS NOT DISTINCT FROM ROW(NEW.account_id, NEW.authentication_id, NEW.receipt_id,
                NEW.hash, NEW.method, NEW.app_id, NEW.created_at, NEW.expires_at))
        OR (CASE WHEN NEW.sequence = 1 THEN
            NOT EXISTS (SELECT 1 FROM identity.issuances record WHERE record.id = NEW.id
                AND record.transaction_id = pg_catalog.pg_current_xact_id())
        ELSE NOT EXISTS (SELECT 1 FROM identity.revocations record WHERE record.id = NEW.id
            AND record.account_id = NEW.account_id AND record.revoked_at = NEW.revoked_at
            AND record.transaction_id = pg_catalog.pg_current_xact_id()) END)
    THEN
        RAISE EXCEPTION 'session advancement requires its immutable issuance and paired head'
            USING ERRCODE = '23514', CONSTRAINT = 'sessions_records_check';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER sessions_records_check AFTER INSERT OR UPDATE ON identity.sessions
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_session_records();

CREATE FUNCTION identity.lock_revocation(uuid, uuid, text) RETURNS void
LANGUAGE plpgsql VOLATILE SECURITY DEFINER SET search_path = '' AS $$
BEGIN
    IF $3 NOT IN ('current', 'all') OR $3 IS NULL THEN
        RAISE EXCEPTION 'invalid logout scope' USING ERRCODE = '23514';
    END IF;
    PERFORM FROM identity.accounts WHERE id = $1 FOR UPDATE;
    PERFORM FROM identity.sessions WHERE account_id = $1 AND ($3 = 'all' OR id = $2) ORDER BY id FOR UPDATE;
END
$$;

CREATE FUNCTION identity.check_verification() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.failures <> 0 OR NEW.authentication_id IS NOT NULL THEN
            RAISE EXCEPTION 'new code cannot already be used' USING ERRCODE = '23514';
        END IF;
    ELSIF ROW(NEW.id, NEW.hash, NEW.key_id, NEW.expires_at) IS DISTINCT FROM ROW(OLD.id, OLD.hash, OLD.key_id, OLD.expires_at)
        OR OLD.authentication_id IS NOT NULL OR OLD.failures >= 5
        OR NOT ((NEW.failures = OLD.failures + 1 AND NEW.authentication_id IS NULL)
            OR (NEW.failures = OLD.failures AND NEW.authentication_id IS NOT NULL)) THEN
        RAISE EXCEPTION 'invalid code transition' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER checks_transition_check BEFORE INSERT OR UPDATE ON identity.checks
    FOR EACH ROW EXECUTE FUNCTION identity.check_verification();

CREATE FUNCTION identity.check_authentication() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE
    authentication record;
    verification record;
    original record;
BEGIN
    IF TG_TABLE_NAME = 'checks' THEN
        SELECT * INTO STRICT verification FROM identity.checks WHERE id = NEW.id;
        SELECT * INTO STRICT authentication FROM identity.authentications WHERE message_id = verification.id
            ORDER BY finished_at DESC, id DESC LIMIT 1;
    ELSE
        SELECT * INTO STRICT authentication FROM identity.authentications WHERE id = NEW.id;
        SELECT * INTO STRICT verification FROM identity.checks WHERE id = authentication.message_id;
    END IF;
    IF verification.failures <> (SELECT count(*) FROM identity.authentications WHERE message_id = verification.id AND status = 'invalid') THEN
        RAISE EXCEPTION 'code failures require matching original results' USING ERRCODE = '23514';
    END IF;
    SELECT message.created_at, message.ordinal, message.challenge_id, (challenge.mobile).hash AS mobile_hash
        INTO STRICT original FROM identity.messages message JOIN identity.challenges challenge ON challenge.id = message.challenge_id
        WHERE message.id = verification.id;
    IF authentication.message_id <> verification.id OR authentication.finished_at < original.created_at
        OR authentication.finished_at >= verification.expires_at
        OR EXISTS (SELECT 1 FROM identity.messages later JOIN identity.challenges challenge ON challenge.id = later.challenge_id
            WHERE challenge.purpose = 'authentication' AND (challenge.mobile).hash = original.mobile_hash
                AND later.ordinal > original.ordinal)
        OR (authentication.status = 'invalid' AND verification.failures < 1)
        OR (authentication.status <> 'invalid' AND verification.authentication_id IS DISTINCT FROM authentication.id) THEN
        RAISE EXCEPTION 'authentication and code effects must agree' USING ERRCODE = '23514';
    END IF;
    IF authentication.status = 'authenticated' THEN
        IF NOT EXISTS (SELECT 1 FROM identity.sessions session JOIN identity.handoffs handoff ON handoff.id = session.id
            JOIN identity.accounts account ON account.id = session.account_id
            WHERE session.authentication_id = authentication.id AND session.app_id = authentication.app_id
                AND session.created_at = authentication.finished_at AND (account.mobile).hash = original.mobile_hash
                AND handoff.expires_at > session.created_at AND handoff.expires_at <= session.expires_at
                AND NOT EXISTS (SELECT 1 FROM identity.suspensions suspension WHERE suspension.person_id = account.person_id
                    AND NOT EXISTS (SELECT 1 FROM identity.resumptions WHERE id = suspension.id))) THEN
            RAISE EXCEPTION 'authentication requires its eligible session and handoff' USING ERRCODE = '23514';
        END IF;
    ELSIF authentication.status = 'registrable' THEN
        IF NOT EXISTS (SELECT 1 FROM identity.registrations registration WHERE registration.authentication_id = authentication.id
            AND registration.created_at = authentication.finished_at AND (registration.mobile).hash = original.mobile_hash)
            OR EXISTS (SELECT 1 FROM identity.accounts WHERE (mobile).hash = original.mobile_hash) THEN
            RAISE EXCEPTION 'registration authentication requires its unoccupied credential' USING ERRCODE = '23514';
        END IF;
    ELSIF authentication.status = 'suspended' AND NOT EXISTS (SELECT 1 FROM identity.accounts account
        JOIN identity.suspensions suspension ON suspension.person_id = account.person_id
        WHERE (account.mobile).hash = original.mobile_hash AND NOT EXISTS (SELECT 1 FROM identity.resumptions WHERE id = suspension.id)) THEN
        RAISE EXCEPTION 'suspended refusal requires a current suspended account' USING ERRCODE = '23514';
    END IF;
    IF authentication.status IN ('invalid', 'suspended') AND (EXISTS (SELECT 1 FROM identity.sessions WHERE authentication_id = authentication.id)
        OR EXISTS (SELECT 1 FROM identity.registrations WHERE authentication_id = authentication.id)) THEN
        RAISE EXCEPTION 'rejected authentication cannot create credentials' USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER authentications_effects_check AFTER INSERT ON identity.authentications
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_authentication();
CREATE CONSTRAINT TRIGGER checks_effects_check AFTER UPDATE ON identity.checks
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_authentication();

CREATE TRIGGER authentications_isolation_check BEFORE INSERT OR DELETE ON identity.authentications
    FOR EACH STATEMENT EXECUTE FUNCTION identity.check_isolation();
CREATE TRIGGER registrations_isolation_check BEFORE INSERT OR DELETE ON identity.registrations
    FOR EACH STATEMENT EXECUTE FUNCTION identity.check_isolation();
CREATE TRIGGER handoffs_isolation_check BEFORE INSERT OR DELETE ON identity.handoffs
    FOR EACH STATEMENT EXECUTE FUNCTION identity.check_isolation();

REVOKE ALL ON identity.persons, identity.revisions, identity.accounts, identity.completions,
    identity.creations, identity.logouts, identity.projections, identity.issuances,
    identity.receipts, identity.suspensions, identity.resumptions, identity.cancellations, identity.replacements, identity.sessions,
    identity.handoffs, identity.registrations, identity.revocations FROM PUBLIC;
REVOKE ALL ON FUNCTION identity.lock_account(uuid), identity.lock_accounts(uuid,uuid), identity.check_verification(), identity.check_authentication() FROM PUBLIC;
REVOKE ALL ON FUNCTION identity.lock_revocation(uuid, uuid, text) FROM PUBLIC;
REVOKE ALL ON FUNCTION identity.cancelling(uuid) FROM PUBLIC;
