CREATE SCHEMA content;
REVOKE ALL ON SCHEMA content FROM PUBLIC;

CREATE TABLE content.completions (
    family text NOT NULL CHECK (family = 'content_column'),
    operation_id uuid PRIMARY KEY,
    action text NOT NULL CHECK (action = 'create'),
    target text NOT NULL CHECK (target COLLATE "C" ~ '^[a-z][a-z0-9-]{1,31}$'),
    input bytea NOT NULL CHECK (octet_length(input) >= 37),
    subject text NOT NULL CHECK (subject = 'session'),
    person_id uuid NOT NULL REFERENCES identity.persons (id),
    revision_id uuid NOT NULL,
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    expires_at timestamptz CHECK (expires_at IS NULL),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    FOREIGN KEY (revision_id, person_id) REFERENCES identity.revisions (id, person_id)
);

CREATE TABLE content.changes (
    id uuid PRIMARY KEY,
    column_id uuid NOT NULL UNIQUE,
    sequence bigint NOT NULL CHECK (sequence = 1),
    key text NOT NULL CHECK (key COLLATE "C" ~ '^[a-z][a-z0-9-]{1,31}$'),
    name text NOT NULL,
    description text NOT NULL,
    enabled boolean NOT NULL,
    index integer NOT NULL UNIQUE CHECK (index BETWEEN 0 AND 99),
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    operation_id uuid NOT NULL UNIQUE REFERENCES content.completions (operation_id) DEFERRABLE INITIALLY DEFERRED,
    digest bytea NOT NULL CHECK (octet_length(digest) = 32),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    UNIQUE (column_id, sequence),
    UNIQUE (id, column_id, key, name, description, enabled, created_at),
    UNIQUE (id, column_id, operation_id, created_at, transaction_id),
    CONSTRAINT changes_name_check CHECK (char_length(name) BETWEEN 1 AND 20 AND name IS NFC NORMALIZED
        AND name COLLATE "C" !~ U&'[\0001-\001F\007F-\009F]'
        AND name = btrim(name, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000')),
    CONSTRAINT changes_description_check CHECK (char_length(description) <= 100 AND description IS NFC NORMALIZED
        AND description COLLATE "C" !~ U&'[\0001-\001F\007F-\009F]'
        AND description = btrim(description, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'))
);

CREATE TABLE content.columns (
    id uuid PRIMARY KEY,
    key text NOT NULL UNIQUE,
    name text NOT NULL UNIQUE,
    description text NOT NULL,
    enabled boolean NOT NULL,
    change_id uuid NOT NULL UNIQUE,
    created_at timestamptz NOT NULL,
    sequence bigint NOT NULL CHECK (sequence = 1),
    version bigint NOT NULL CHECK (version = 1),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    FOREIGN KEY (change_id, id, key, name, description, enabled, created_at)
        REFERENCES content.changes (id, column_id, key, name, description, enabled, created_at)
        DEFERRABLE INITIALLY DEFERRED
);
ALTER TABLE content.changes ADD FOREIGN KEY (column_id) REFERENCES content.columns (id) DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE content.appends (
    id uuid PRIMARY KEY,
    navigation_id uuid NOT NULL,
    sequence bigint NOT NULL CHECK (sequence BETWEEN 1 AND 100),
    column_id uuid NOT NULL UNIQUE REFERENCES content.columns (id) DEFERRABLE INITIALLY DEFERRED,
    change_id uuid NOT NULL UNIQUE,
    "order" uuid[] NOT NULL CHECK (array_ndims("order") = 1 AND array_lower("order", 1) = 1
        AND cardinality("order") = sequence AND "order"[sequence] = column_id
        AND array_position("order", NULL) IS NULL),
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    operation_id uuid NOT NULL UNIQUE,
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    UNIQUE (navigation_id, sequence),
    UNIQUE (id, navigation_id, sequence, "order", created_at),
    FOREIGN KEY (change_id, column_id, operation_id, created_at, transaction_id)
        REFERENCES content.changes (id, column_id, operation_id, created_at, transaction_id)
        DEFERRABLE INITIALLY DEFERRED
);

CREATE TABLE content.navigations (
    id uuid PRIMARY KEY,
    singleton boolean NOT NULL UNIQUE CHECK (singleton),
    "order" uuid[] NOT NULL,
    append_id uuid NOT NULL,
    changed_at timestamptz NOT NULL,
    sequence bigint NOT NULL CHECK (sequence BETWEEN 1 AND 100),
    version bigint NOT NULL CHECK (version = sequence),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    FOREIGN KEY (append_id, id, sequence, "order", changed_at)
        REFERENCES content.appends (id, navigation_id, sequence, "order", created_at)
        DEFERRABLE INITIALLY DEFERRED
);
ALTER TABLE content.appends ADD FOREIGN KEY (navigation_id) REFERENCES content.navigations (id) DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE content.changes ADD FOREIGN KEY (id) REFERENCES content.appends (change_id) DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE content.completions ADD FOREIGN KEY (operation_id) REFERENCES content.changes (operation_id) DEFERRABLE INITIALLY DEFERRED;

-- Pair each append with a creation from this transaction and its navigation state.
CREATE FUNCTION content.check_append() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF NEW.transaction_id <> pg_catalog.pg_current_xact_id()
        OR (SELECT count(DISTINCT item) FROM pg_catalog.unnest(NEW."order") item) <> NEW.sequence
        OR NOT EXISTS (SELECT 1 FROM content.changes change
            WHERE change.id = NEW.change_id AND change.index = NEW.sequence - 1
                AND change.xmin = pg_catalog.pg_current_xact_id()::text::xid)
        OR NOT EXISTS (SELECT 1 FROM content.navigations navigation
            WHERE navigation.id = NEW.navigation_id AND navigation.sequence >= NEW.sequence
                AND navigation."order"[NEW.sequence] = NEW.column_id) THEN
        RAISE EXCEPTION 'navigation append requires its creation and complete predecessor'
            USING ERRCODE = '23514', CONSTRAINT = 'appends_creation_check';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER appends_creation_check AFTER INSERT ON content.appends
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION content.check_append();

CREATE FUNCTION content.check_navigation() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE
    previous_sequence bigint;
BEGIN
    previous_sequence := CASE WHEN TG_OP = 'INSERT' THEN 0 ELSE OLD.sequence END;
    IF NEW.sequence <> previous_sequence + 1
        OR (TG_OP = 'UPDATE' AND NEW.id <> OLD.id)
        OR NOT EXISTS (SELECT 1 FROM content.appends append
            WHERE append.id = NEW.append_id AND append.sequence = NEW.sequence
                AND append.xmin = pg_catalog.pg_current_xact_id()::text::xid)
        OR EXISTS (SELECT 1 FROM content.columns column_state
            WHERE NOT column_state.id = ANY(NEW."order")) THEN
        RAISE EXCEPTION 'navigation advancement requires its new contiguous record'
            USING ERRCODE = '23514', CONSTRAINT = 'navigations_records_check';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER navigations_records_check AFTER INSERT OR UPDATE ON content.navigations
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION content.check_navigation();

REVOKE ALL ON ALL TABLES IN SCHEMA content FROM PUBLIC;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA content FROM PUBLIC;
