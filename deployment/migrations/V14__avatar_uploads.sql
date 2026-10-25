-- Avatar replacement: permanent account advancement, private publication, original result
-- and private-file deletion intentions. All migrations are still disposable test material.

ALTER TABLE identity.avatars
    ADD COLUMN content_seal bytea CHECK (octet_length(content_seal) = 32),
    ADD COLUMN content_seal_key_id bigint REFERENCES security.keys (id),
    ADD COLUMN content_seal_version smallint CHECK (content_seal_version = 1),
    ADD CONSTRAINT avatars_content_proof_check CHECK (
        (content_seal IS NULL) = (content_seal_key_id IS NULL)
        AND (content_seal IS NULL) = (content_seal_version IS NULL));

CREATE FUNCTION identity.check_avatar_content_proof() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF ROW(NEW.content_seal, NEW.content_seal_key_id, NEW.content_seal_version)
        IS DISTINCT FROM ROW(OLD.content_seal, OLD.content_seal_key_id, OLD.content_seal_version) THEN
        RAISE EXCEPTION 'avatar content proof is immutable' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER avatars_content_proof_check BEFORE UPDATE ON identity.avatars
    FOR EACH ROW EXECUTE FUNCTION identity.check_avatar_content_proof();

CREATE TABLE identity.portraits (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL,
    person_id uuid NOT NULL,
    sequence bigint NOT NULL CHECK (sequence > 1),
    operation_id uuid NOT NULL UNIQUE,
    portrait_at timestamptz NOT NULL CHECK (isfinite(portrait_at)),
    previous_revision uuid NOT NULL,
    revision uuid NOT NULL,
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    CHECK (previous_revision <> revision),
    FOREIGN KEY (account_id, person_id) REFERENCES identity.accounts (id, person_id),
    UNIQUE (account_id, sequence),
    UNIQUE (account_id, revision)
);
CREATE INDEX portraits_account_id_portrait_at_idx ON identity.portraits (account_id, portrait_at);

CREATE TABLE identity.uploads (
    operation_id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES identity.accounts (id),
    changed boolean NOT NULL,
    revision uuid NOT NULL,
    completed_at timestamptz NOT NULL CHECK (isfinite(completed_at)),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id()
);
CREATE INDEX uploads_account_id_completed_at_idx ON identity.uploads (account_id, completed_at);

CREATE TABLE identity.publications (
    operation_id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES identity.accounts (id),
    expected_revision uuid NOT NULL,
    media_type text NOT NULL CHECK (media_type IN ('image/png', 'image/jpeg', 'image/webp')),
    input_digest bytea NOT NULL CHECK (octet_length(input_digest) = 32),
    input_size bigint NOT NULL CHECK (input_size > 0 AND input_size <= 2097152),
    candidate_id uuid NOT NULL UNIQUE,
    work_id uuid NOT NULL UNIQUE,
    output_digest bytea NOT NULL CHECK (octet_length(output_digest) = 32),
    output_size bigint NOT NULL CHECK (output_size > 0 AND output_size <= 8388608),
    width integer NOT NULL CHECK (width BETWEEN 1 AND 1024),
    height integer NOT NULL CHECK (height = width),
    private boolean NOT NULL,
    revision uuid NOT NULL,
    changed boolean,
    completed_at timestamptz CHECK (isfinite(completed_at)),
    result_seal bytea CHECK (octet_length(result_seal) = 32),
    result_seal_key_id bigint REFERENCES security.keys (id),
    result_seal_version smallint CHECK (result_seal_version = 1),
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    CHECK ((changed IS NULL) = (completed_at IS NULL)),
    CHECK ((changed IS NULL) = (result_seal IS NULL)),
    CHECK ((changed IS NULL) = (result_seal_key_id IS NULL)),
    CHECK ((changed IS NULL) = (result_seal_version IS NULL)),
    CHECK (changed IS NULL OR changed = (revision <> expected_revision))
);

CREATE TABLE identity.deletions (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES identity.accounts (id),
    digest bytea NOT NULL CHECK (octet_length(digest) = 32),
    size bigint NOT NULL CHECK (size > 0 AND size <= 8388608),
    released_at timestamptz NOT NULL CHECK (isfinite(released_at)),
    release text NOT NULL CHECK (release IN ('cancellation', 'replacement')),
    release_operation_id uuid NOT NULL,
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id()
);

CREATE TRIGGER portraits_immutable_check BEFORE UPDATE OR DELETE ON identity.portraits
    FOR EACH ROW EXECUTE FUNCTION access.reject_history_mutation();
CREATE TRIGGER uploads_immutable_check BEFORE UPDATE ON identity.uploads
    FOR EACH ROW EXECUTE FUNCTION identity.reject_record_mutation();
CREATE TRIGGER deletions_immutable_check BEFORE UPDATE ON identity.deletions
    FOR EACH ROW EXECUTE FUNCTION identity.reject_record_mutation();

CREATE FUNCTION identity.check_upload_deletion() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF NOT identity.cancelling(OLD.account_id) THEN
        RAISE EXCEPTION 'avatar original results are removed only by their account cancellation'
            USING ERRCODE = '23514';
    END IF;
    RETURN OLD;
END
$$;
CREATE TRIGGER uploads_deletion_check BEFORE DELETE ON identity.uploads
    FOR EACH ROW EXECUTE FUNCTION identity.check_upload_deletion();

CREATE FUNCTION identity.check_avatar_completion_deletion() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF NOT identity.cancelling(OLD.target::uuid) THEN
        RAISE EXCEPTION 'avatar completions are removed only by their account cancellation'
            USING ERRCODE = '23514';
    END IF;
    RETURN OLD;
END
$$;
CREATE TRIGGER avatar_completion_deletion_check BEFORE DELETE ON identity.completions
    FOR EACH ROW WHEN (OLD.family = 'avatar')
    EXECUTE FUNCTION identity.check_avatar_completion_deletion();
CREATE TRIGGER avatar_completion_immutable_check BEFORE UPDATE ON identity.completions
    FOR EACH ROW WHEN (OLD.family = 'avatar' OR NEW.family = 'avatar')
    EXECUTE FUNCTION identity.reject_record_mutation();

CREATE FUNCTION identity.check_cancelled_uploads() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM identity.uploads WHERE account_id = NEW.account_id)
        OR EXISTS (SELECT 1 FROM identity.completions WHERE family = 'avatar'
            AND target = NEW.account_id::text) THEN
        RAISE EXCEPTION 'cancellation removes every public avatar original result'
            USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER cancellations_uploads_check AFTER INSERT ON identity.cancellations
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_cancelled_uploads();
REVOKE ALL ON FUNCTION identity.check_avatar_completion_deletion(), identity.check_cancelled_uploads() FROM PUBLIC;

CREATE FUNCTION identity.check_publication_update() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF ROW(NEW.operation_id, NEW.account_id, NEW.expected_revision, NEW.media_type,
        NEW.input_digest, NEW.input_size, NEW.candidate_id, NEW.work_id, NEW.output_digest,
        NEW.output_size, NEW.width, NEW.height, NEW.private, NEW.revision, NEW.created_at,
        NEW.seal, NEW.seal_key_id, NEW.seal_version, NEW.transaction_id)
        IS DISTINCT FROM ROW(OLD.operation_id, OLD.account_id, OLD.expected_revision, OLD.media_type,
        OLD.input_digest, OLD.input_size, OLD.candidate_id, OLD.work_id, OLD.output_digest,
        OLD.output_size, OLD.width, OLD.height, OLD.private, OLD.revision, OLD.created_at,
        OLD.seal, OLD.seal_key_id, OLD.seal_version, OLD.transaction_id)
        OR (OLD.completed_at IS NOT NULL AND ROW(NEW.changed, NEW.completed_at,
            NEW.result_seal, NEW.result_seal_key_id, NEW.result_seal_version)
            IS DISTINCT FROM ROW(OLD.changed, OLD.completed_at,
                OLD.result_seal, OLD.result_seal_key_id, OLD.result_seal_version)) THEN
        RAISE EXCEPTION 'avatar publication identity is immutable' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER publications_immutable_check BEFORE UPDATE ON identity.publications
    FOR EACH ROW EXECUTE FUNCTION identity.check_publication_update();

CREATE FUNCTION identity.check_upload() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE
    original uuid := NEW.operation_id;
    result identity.uploads%ROWTYPE;
    publication identity.publications%ROWTYPE;
BEGIN
    IF TG_TABLE_NAME = 'publications' THEN
        IF NEW.completed_at IS NULL THEN RETURN NULL; END IF;
    END IF;
    SELECT * INTO result FROM identity.uploads WHERE operation_id = original;
    SELECT * INTO publication FROM identity.publications WHERE operation_id = original;
    IF result.operation_id IS NULL OR publication.operation_id IS NULL
        OR result.transaction_id <> pg_catalog.pg_current_xact_id()
        OR publication.transaction_id = pg_catalog.pg_current_xact_id()
        OR ROW(publication.account_id, publication.changed, publication.revision, publication.completed_at,
            publication.result_seal, publication.result_seal_key_id, publication.result_seal_version)
            IS DISTINCT FROM ROW(result.account_id, result.changed, result.revision, result.completed_at,
                result.seal, result.seal_key_id, result.seal_version)
        OR NOT EXISTS (SELECT 1 FROM identity.completions completion
            WHERE completion.family = 'avatar' AND completion.operation_id = original
                AND completion.target = result.account_id::text
                AND completion.created_at = result.completed_at
                AND completion.transaction_id = result.transaction_id)
        OR NOT EXISTS (SELECT 1 FROM identity.accounts account
            JOIN identity.avatars avatar ON avatar.account_id = account.id AND avatar.released_at IS NULL
            WHERE account.id = result.account_id AND account.cancellation_id IS NULL
                AND account.avatar = result.revision AND avatar.revision = result.revision) THEN
        RAISE EXCEPTION 'avatar completion requires its confirmed intention and paired current facts'
            USING ERRCODE = '23514';
    END IF;
    IF result.changed THEN
        IF NOT EXISTS (SELECT 1 FROM identity.portraits portrait
            JOIN identity.accounts account ON account.id = portrait.account_id
            JOIN identity.avatars avatar ON avatar.id = publication.candidate_id
            WHERE portrait.operation_id = original AND portrait.account_id = result.account_id
                AND portrait.person_id = account.person_id AND portrait.sequence = account.sequence
                AND portrait.previous_revision = publication.expected_revision
                AND portrait.revision = result.revision AND portrait.portrait_at = result.completed_at
                AND portrait.transaction_id = result.transaction_id
                AND account.changed_at = result.completed_at
                AND avatar.transaction_id = result.transaction_id
                AND avatar.account_id = result.account_id AND avatar.revision = result.revision
                AND avatar.digest = publication.output_digest AND avatar.size = publication.output_size
                AND avatar.width = publication.width AND avatar.height = publication.height
                AND (avatar.content_seal IS NOT NULL) = publication.private) THEN
            RAISE EXCEPTION 'a changed avatar has its exact permanent account advancement'
                USING ERRCODE = '23514';
        END IF;
    ELSIF EXISTS (SELECT 1 FROM identity.portraits WHERE operation_id = original)
        OR publication.revision <> publication.expected_revision THEN
        RAISE EXCEPTION 'an unchanged avatar does not advance its account' USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END
$$;

CREATE CONSTRAINT TRIGGER portraits_completion_check AFTER INSERT ON identity.portraits
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_upload();
CREATE CONSTRAINT TRIGGER uploads_completion_check AFTER INSERT ON identity.uploads
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_upload();
CREATE CONSTRAINT TRIGGER publications_completion_check AFTER INSERT OR UPDATE ON identity.publications
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_upload();
CREATE CONSTRAINT TRIGGER avatar_completions_check AFTER INSERT ON identity.completions
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW WHEN (NEW.family = 'avatar')
    EXECUTE FUNCTION identity.check_upload();

CREATE FUNCTION identity.check_avatar_removal() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF OLD.released_at IS NULL OR NOT EXISTS (SELECT 1 FROM identity.deletions deletion
        WHERE deletion.id = OLD.id AND deletion.account_id = OLD.account_id
            AND deletion.digest = OLD.digest AND deletion.size = OLD.size
            AND deletion.released_at = OLD.released_at AND deletion.release = OLD.release
            AND deletion.release_operation_id = OLD.release_operation_id
            AND deletion.transaction_id = pg_catalog.pg_current_xact_id()) THEN
        RAISE EXCEPTION 'avatar removal retains its exact private deletion intention'
            USING ERRCODE = '23514';
    END IF;
    RETURN OLD;
END
$$;
CREATE TRIGGER avatars_removal_intention_check BEFORE DELETE ON identity.avatars
    FOR EACH ROW EXECUTE FUNCTION identity.check_avatar_removal();

CREATE FUNCTION identity.check_deletion() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM identity.avatars WHERE id = NEW.id)
        OR EXISTS (SELECT 1 FROM identity.publications WHERE candidate_id = NEW.id AND changed IS NULL) THEN
        RAISE EXCEPTION 'a deletion intention cannot remove a retained avatar or pending candidate'
            USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER deletions_resource_check AFTER INSERT ON identity.deletions
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_deletion();

REVOKE ALL ON identity.portraits, identity.uploads, identity.publications, identity.deletions FROM PUBLIC;
REVOKE ALL ON FUNCTION identity.check_upload_deletion(), identity.check_publication_update() FROM PUBLIC;
REVOKE ALL ON FUNCTION identity.check_upload(), identity.check_avatar_removal(), identity.check_deletion() FROM PUBLIC;
