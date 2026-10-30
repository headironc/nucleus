CREATE SCHEMA workforce;
REVOKE ALL ON SCHEMA workforce FROM PUBLIC;

CREATE TYPE workforce.number AS (subtype smallint, year integer, serial smallint);
CREATE FUNCTION workforce.valid(value workforce.number) RETURNS boolean
LANGUAGE sql IMMUTABLE SET search_path = '' AS $$
    SELECT value IS NOT NULL AND (value).subtype IS NOT NULL AND (value).year IS NOT NULL AND (value).serial IS NOT NULL
        AND (value).subtype IN (100,200,300,400,500,600,700,801,802,803,804,901,902,903,904,1000)
        AND (value).year > 0 AND (value).serial BETWEEN 1 AND 999
$$;

CREATE TABLE workforce.revisions (
    id uuid PRIMARY KEY,
    person_id uuid NOT NULL REFERENCES identity.persons (id),
    number workforce.number NOT NULL CHECK (workforce.valid(number)),
    allocation_id uuid,
    correction_id uuid,
    recovery_id uuid,
    seal bytea NOT NULL CHECK (octet_length(seal)=32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version=1),
    CHECK (num_nonnulls(allocation_id,correction_id,recovery_id)=1),
    UNIQUE (id,person_id),
    UNIQUE (id,person_id,number),
    UNIQUE (id,person_id,allocation_id),
    UNIQUE (id,person_id,correction_id),
    UNIQUE (id,person_id,recovery_id)
);
CREATE INDEX revisions_person_id_idx ON workforce.revisions (person_id);

CREATE TABLE workforce.numbers (
    id uuid PRIMARY KEY REFERENCES identity.persons (id),
    revision_id uuid NOT NULL,
    number workforce.number NOT NULL CHECK (workforce.valid(number)),
    held boolean NOT NULL,
    tenure_id uuid NOT NULL,
    changed_at timestamptz NOT NULL CHECK (isfinite(changed_at)),
    sequence bigint NOT NULL CHECK (sequence>0),
    version bigint NOT NULL CHECK (version>0),
    seal bytea NOT NULL CHECK (octet_length(seal)=32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version=1),
    FOREIGN KEY (revision_id,id,number) REFERENCES workforce.revisions (id,person_id,number)
);
CREATE UNIQUE INDEX numbers_pool_key ON workforce.numbers ((((number).subtype / 100)),((number).year),((number).serial)) WHERE held;

CREATE TABLE workforce.allocations (
    id uuid PRIMARY KEY,
    person_id uuid NOT NULL REFERENCES workforce.numbers (id) DEFERRABLE INITIALLY DEFERRED,
    revision_id uuid NOT NULL,
    number_id uuid NOT NULL UNIQUE,
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    allocated_at timestamptz NOT NULL CHECK (isfinite(allocated_at)),
    sequence bigint NOT NULL CHECK (sequence>0),
    operation_id uuid NOT NULL UNIQUE,
    tenure_id uuid NOT NULL,
    held boolean NOT NULL CHECK (held),
    source text NOT NULL CHECK (source IN ('self_service','assistance','supplement')),
    reason text,
    seal bytea NOT NULL CHECK (octet_length(seal)=32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version=1),
    CHECK ((source='self_service' AND reason IS NULL) OR (source<>'self_service' AND reason IS NOT NULL AND reason<>'')),
    UNIQUE (person_id,sequence),
    UNIQUE (id,number_id),
    FOREIGN KEY (revision_id,person_id) REFERENCES identity.revisions (id,person_id),
    FOREIGN KEY (number_id,person_id,id) REFERENCES workforce.revisions (id,person_id,allocation_id)
);

CREATE TABLE workforce.corrections (
    id uuid PRIMARY KEY,
    person_id uuid NOT NULL REFERENCES workforce.numbers (id) DEFERRABLE INITIALLY DEFERRED,
    revision_id uuid NOT NULL,
    before_id uuid NOT NULL UNIQUE,
    after_id uuid NOT NULL UNIQUE CHECK (after_id<>before_id),
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    corrected_at timestamptz NOT NULL CHECK (isfinite(corrected_at)),
    sequence bigint NOT NULL CHECK (sequence>0),
    operation_id uuid NOT NULL UNIQUE,
    tenure_id uuid NOT NULL,
    held boolean NOT NULL,
    reason text NOT NULL CHECK (reason<>''),
    seal bytea NOT NULL CHECK (octet_length(seal)=32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version=1),
    UNIQUE (person_id,sequence),
    UNIQUE (id,after_id),
    FOREIGN KEY (revision_id,person_id) REFERENCES identity.revisions (id,person_id),
    FOREIGN KEY (before_id,person_id) REFERENCES workforce.revisions (id,person_id),
    FOREIGN KEY (after_id,person_id,id) REFERENCES workforce.revisions (id,person_id,correction_id)
);

CREATE TABLE workforce.recoveries (
    id uuid PRIMARY KEY,
    person_id uuid NOT NULL REFERENCES workforce.numbers (id) DEFERRABLE INITIALLY DEFERRED,
    revision_id uuid NOT NULL,
    origin_id uuid NOT NULL REFERENCES workforce.revisions (id),
    number_id uuid NOT NULL UNIQUE CHECK (number_id<>origin_id),
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    recovered_at timestamptz NOT NULL CHECK (isfinite(recovered_at)),
    sequence bigint NOT NULL CHECK (sequence>0),
    operation_id uuid NOT NULL UNIQUE,
    tenure_id uuid NOT NULL,
    held boolean NOT NULL CHECK (held),
    occupier_id uuid NOT NULL,
    revocation_id uuid CHECK (revocation_id<>occupier_id),
    reason text NOT NULL CHECK (reason<>''),
    seal bytea NOT NULL CHECK (octet_length(seal)=32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version=1),
    UNIQUE (person_id,sequence),
    UNIQUE (id,number_id),
    FOREIGN KEY (revision_id,person_id) REFERENCES identity.revisions (id,person_id),
    FOREIGN KEY (number_id,person_id,id) REFERENCES workforce.revisions (id,person_id,recovery_id)
);

CREATE TABLE workforce.revocations (
    id uuid PRIMARY KEY,
    recovery_id uuid NOT NULL REFERENCES workforce.recoveries (id) DEFERRABLE INITIALLY DEFERRED,
    person_id uuid NOT NULL REFERENCES workforce.numbers (id) DEFERRABLE INITIALLY DEFERRED,
    revision_id uuid NOT NULL,
    number_id uuid NOT NULL UNIQUE,
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    revoked_at timestamptz NOT NULL CHECK (isfinite(revoked_at)),
    sequence bigint NOT NULL CHECK (sequence>0),
    operation_id uuid NOT NULL,
    tenure_id uuid NOT NULL,
    held boolean NOT NULL CHECK (NOT held),
    reason text NOT NULL CHECK (reason<>''),
    seal bytea NOT NULL CHECK (octet_length(seal)=32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version=1),
    UNIQUE (person_id,sequence),
    UNIQUE (recovery_id,person_id),
    UNIQUE (id,recovery_id),
    FOREIGN KEY (revision_id,person_id) REFERENCES identity.revisions (id,person_id),
    FOREIGN KEY (number_id,person_id) REFERENCES workforce.revisions (id,person_id)
);

-- Values and their creators, and recoveries and their fixed revocations, form circular references.
ALTER TABLE workforce.revisions ADD FOREIGN KEY (allocation_id,id) REFERENCES workforce.allocations (id,number_id) DEFERRABLE INITIALLY DEFERRED,
    ADD FOREIGN KEY (correction_id,id) REFERENCES workforce.corrections (id,after_id) DEFERRABLE INITIALLY DEFERRED,
    ADD FOREIGN KEY (recovery_id,id) REFERENCES workforce.recoveries (id,number_id) DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE workforce.recoveries ADD FOREIGN KEY (occupier_id,id) REFERENCES workforce.revocations (id,recovery_id) DEFERRABLE INITIALLY DEFERRED,
    ADD FOREIGN KEY (revocation_id,id) REFERENCES workforce.revocations (id,recovery_id) DEFERRABLE INITIALLY DEFERRED;

CREATE VIEW workforce.transitions AS
    SELECT 'allocation'::text AS kind,id,person_id,revision_id,number_id,operator_id,allocated_at AS happened_at,sequence,operation_id,tenure_id,held,source,reason,
        NULL::uuid AS before_id,NULL::uuid AS origin_id,NULL::uuid AS occupier_id,NULL::uuid AS revocation_id,NULL::uuid AS recovery_id,seal,seal_key_id,seal_version,xmin AS inserted_in FROM workforce.allocations
    UNION ALL SELECT 'correction',id,person_id,revision_id,after_id,operator_id,corrected_at,sequence,operation_id,tenure_id,held,NULL,reason,before_id,NULL,NULL,NULL,NULL,seal,seal_key_id,seal_version,xmin FROM workforce.corrections
    UNION ALL SELECT 'recovery',id,person_id,revision_id,number_id,operator_id,recovered_at,sequence,operation_id,tenure_id,held,NULL,reason,NULL,origin_id,occupier_id,revocation_id,NULL,seal,seal_key_id,seal_version,xmin FROM workforce.recoveries
    UNION ALL SELECT 'revocation',id,person_id,revision_id,number_id,operator_id,revoked_at,sequence,operation_id,tenure_id,held,NULL,reason,NULL,NULL,NULL,NULL,recovery_id,seal,seal_key_id,seal_version,xmin FROM workforce.revocations;

CREATE FUNCTION workforce.check_number_state() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE previous_sequence bigint; previous_version bigint;
BEGIN
    previous_sequence := CASE WHEN TG_OP='INSERT' THEN 0 ELSE OLD.sequence END;
    previous_version := CASE WHEN TG_OP='INSERT' THEN 0 ELSE OLD.version END;
    IF NEW.sequence<=previous_sequence OR NEW.version<>previous_version+1
        OR (TG_OP='UPDATE' AND NEW.id<>OLD.id)
        OR EXISTS (SELECT 1 FROM pg_catalog.generate_series(previous_sequence+1,NEW.sequence) expected(sequence)
            WHERE (SELECT count(*) FROM workforce.transitions record WHERE record.person_id=NEW.id AND record.sequence=expected.sequence
                AND record.inserted_in=pg_catalog.pg_current_xact_id()::text::xid)<>1)
        OR NOT EXISTS (SELECT 1 FROM workforce.transitions head WHERE head.person_id=NEW.id AND head.sequence=NEW.sequence
            AND ROW(head.number_id,head.held,head.tenure_id,head.happened_at) IS NOT DISTINCT FROM ROW(NEW.revision_id,NEW.held,NEW.tenure_id,NEW.changed_at))
        OR EXISTS (SELECT 1 FROM workforce.revocations revocation JOIN workforce.recoveries recovery ON recovery.id=revocation.recovery_id
            WHERE revocation.person_id=NEW.id AND revocation.sequence>previous_sequence AND revocation.sequence<=NEW.sequence
                AND revocation.id<>recovery.occupier_id AND revocation.id IS DISTINCT FROM recovery.revocation_id) THEN
        RAISE EXCEPTION 'number advancement requires its contiguous records and paired head' USING ERRCODE='23514',CONSTRAINT='numbers_state_check';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER numbers_state_check AFTER INSERT OR UPDATE ON workforce.numbers
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION workforce.check_number_state();

REVOKE ALL ON ALL TABLES IN SCHEMA workforce FROM PUBLIC;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA workforce FROM PUBLIC;
REVOKE ALL ON TYPE workforce.number FROM PUBLIC;
