-- Directory writes, their name scope and original completions share this transaction lock.
CREATE FUNCTION workforce.lock_directory() RETURNS void
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION 'directory writes require fresh statement snapshots'
            USING ERRCODE = '23514', CONSTRAINT = 'directory_isolation_check';
    END IF;
    PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtextextended('workforce/directory', 0));
END
$$;

CREATE FUNCTION workforce.lock_completion(uuid) RETURNS void
LANGUAGE sql VOLATILE SET search_path = '' AS $$
    SELECT pg_catalog.pg_advisory_xact_lock(('x' || pg_catalog.substr(pg_catalog.encode(pg_catalog.sha256(
        pg_catalog.int4send(30) || pg_catalog.convert_to('workforce/completion/operation', 'UTF8')
        || pg_catalog.int4send(16) || pg_catalog.uuid_send($1)), 'hex'), 1, 16))::bit(64)::bigint)
$$;

CREATE TABLE workforce.groups (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE,
    enabled boolean NOT NULL,
    index integer NOT NULL CHECK (index >= 0),
    operation_id uuid NOT NULL UNIQUE,
    CONSTRAINT groups_name_check CHECK (char_length(name) BETWEEN 1 AND 64 AND name IS NFC NORMALIZED
        AND name COLLATE "C" !~ U&'[\0001-\001F\007F-\009F]'
        AND name = btrim(name, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'))
);

CREATE TABLE workforce.positions (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE,
    enabled boolean NOT NULL,
    index integer NOT NULL CHECK (index >= 0),
    operation_id uuid NOT NULL UNIQUE,
    CONSTRAINT positions_name_check CHECK (char_length(name) BETWEEN 1 AND 64 AND name IS NFC NORMALIZED
        AND name COLLATE "C" !~ U&'[\0001-\001F\007F-\009F]'
        AND name = btrim(name, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'))
);

-- The title tree: categories hold disciplines, and only complete titles are selectable.
CREATE TABLE workforce.categories (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE,
    enabled boolean NOT NULL,
    index integer NOT NULL CHECK (index >= 0),
    operation_id uuid NOT NULL UNIQUE,
    CONSTRAINT categories_name_check CHECK (char_length(name) BETWEEN 1 AND 64 AND name IS NFC NORMALIZED
        AND name COLLATE "C" !~ U&'[\0001-\001F\007F-\009F]'
        AND name = btrim(name, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'))
);

CREATE TABLE workforce.disciplines (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE,
    enabled boolean NOT NULL,
    index integer NOT NULL CHECK (index >= 0),
    operation_id uuid NOT NULL UNIQUE,
    category_id uuid NOT NULL REFERENCES workforce.categories (id),
    CONSTRAINT disciplines_name_check CHECK (char_length(name) BETWEEN 1 AND 64 AND name IS NFC NORMALIZED
        AND name COLLATE "C" !~ U&'[\0001-\001F\007F-\009F]'
        AND name = btrim(name, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'))
);
CREATE INDEX disciplines_category_id_idx ON workforce.disciplines (category_id);

CREATE TABLE workforce.titles (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE,
    enabled boolean NOT NULL,
    index integer NOT NULL CHECK (index >= 0),
    operation_id uuid NOT NULL UNIQUE,
    discipline_id uuid NOT NULL REFERENCES workforce.disciplines (id),
    CONSTRAINT titles_name_check CHECK (char_length(name) BETWEEN 1 AND 64 AND name IS NFC NORMALIZED
        AND name COLLATE "C" !~ U&'[\0001-\001F\007F-\009F]'
        AND name = btrim(name, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'))
);
CREATE INDEX titles_discipline_id_idx ON workforce.titles (discipline_id);

CREATE TABLE workforce.departments (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE,
    enabled boolean NOT NULL,
    index integer NOT NULL CHECK (index >= 0),
    operation_id uuid NOT NULL UNIQUE,
    group_id uuid NOT NULL REFERENCES workforce.groups (id),
    parent_id uuid,
    UNIQUE (id, group_id),
    CONSTRAINT departments_name_check CHECK (char_length(name) BETWEEN 1 AND 64 AND name IS NFC NORMALIZED
        AND name COLLATE "C" !~ U&'[\0001-\001F\007F-\009F]'
        AND name = btrim(name, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000')),
    CONSTRAINT departments_parent_id_group_id_fkey FOREIGN KEY (parent_id, group_id)
        REFERENCES workforce.departments (id, group_id) DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT departments_parent_check CHECK (parent_id IS NULL OR parent_id <> id)
);
CREATE INDEX departments_group_id_idx ON workforce.departments (group_id);
CREATE INDEX departments_parent_id_idx ON workforce.departments (parent_id);

CREATE TABLE workforce.rules (
    id smallint PRIMARY KEY,
    department_id uuid NOT NULL REFERENCES workforce.departments (id),
    position boolean NOT NULL,
    CONSTRAINT rules_id_check CHECK (id IN (100, 200, 300, 400, 500, 600, 700, 801, 802, 803, 804, 901, 902, 903, 904, 1000))
);
CREATE INDEX rules_department_id_idx ON workforce.rules (department_id);

-- A department's selectable positions; the department and position references never change.
CREATE TABLE workforce.rosters (
    id uuid PRIMARY KEY,
    department_id uuid NOT NULL REFERENCES workforce.departments (id),
    position_id uuid NOT NULL REFERENCES workforce.positions (id),
    enabled boolean NOT NULL,
    index integer NOT NULL CHECK (index >= 0),
    UNIQUE (department_id, position_id)
);
CREATE INDEX rosters_position_id_idx ON workforce.rosters (position_id);

CREATE TABLE workforce.completions (
    family text NOT NULL CHECK (family IN ('workforce_configuration', 'number_allocation', 'number_correction', 'number_recovery')),
    operation_id uuid NOT NULL,
    action text NOT NULL,
    target text NOT NULL CHECK (target <> '' AND target IS NFC NORMALIZED
        AND target COLLATE "C" !~ U&'[\0001-\001F\007F-\009F]'
        AND target = btrim(target, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000')),
    input bytea NOT NULL CHECK (octet_length(input) >= 37),
    subject text NOT NULL CHECK (subject = 'session'),
    person_id uuid NOT NULL REFERENCES identity.persons (id),
    revision_id uuid NOT NULL,
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    expires_at timestamptz,
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    PRIMARY KEY (family, operation_id),
    CONSTRAINT completions_action_check CHECK ((family = 'workforce_configuration' AND action IN ('create', 'update', 'delete', 'replace'))
        OR (family = 'number_allocation' AND action IN ('self_service', 'assistance', 'supplement'))
        OR (family = 'number_correction' AND action = 'correct')
        OR (family = 'number_recovery' AND action = 'recover')),
    CONSTRAINT completions_retention_check CHECK ((family = 'workforce_configuration' AND expires_at IS NOT NULL AND isfinite(expires_at)
        AND expires_at = created_at + interval '24 hours')
        OR (family <> 'workforce_configuration' AND expires_at IS NULL)),
    FOREIGN KEY (revision_id, person_id) REFERENCES identity.revisions (id, person_id)
);
CREATE INDEX completions_created_at_operation_id_idx ON workforce.completions (created_at, operation_id);

ALTER TABLE workforce.allocations
    ADD COLUMN family text GENERATED ALWAYS AS ('number_allocation'::text) STORED,
    ADD FOREIGN KEY (family,operation_id) REFERENCES workforce.completions (family,operation_id)
        DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE workforce.corrections
    ADD COLUMN family text GENERATED ALWAYS AS ('number_correction'::text) STORED,
    ADD FOREIGN KEY (family,operation_id) REFERENCES workforce.completions (family,operation_id)
        DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE workforce.recoveries
    ADD COLUMN family text GENERATED ALWAYS AS ('number_recovery'::text) STORED,
    ADD FOREIGN KEY (family,operation_id) REFERENCES workforce.completions (family,operation_id)
        DEFERRABLE INITIALLY DEFERRED;

-- The cross-table name and original-creation checks are fixed invoker constraints. The trigger
-- reacquires the application lock and then reads the other table without filtering disabled rows.
CREATE FUNCTION workforce.check_directory_name() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF TG_TABLE_SCHEMA <> 'workforce' OR TG_TABLE_NAME NOT IN ('groups', 'departments') THEN
        RAISE EXCEPTION 'unsupported directory name scope' USING ERRCODE = '23514';
    END IF;
    PERFORM workforce.lock_directory();
    IF TG_TABLE_NAME = 'groups' THEN
        IF EXISTS (SELECT 1 FROM workforce.departments WHERE name = NEW.name) THEN
            RAISE EXCEPTION 'group and department names share one Workforce scope'
                USING ERRCODE = '23505', SCHEMA = 'workforce', TABLE = 'groups', CONSTRAINT = 'groups_departments_name_key';
        END IF;
    ELSIF TG_TABLE_NAME = 'departments' THEN
        IF EXISTS (SELECT 1 FROM workforce.groups WHERE name = NEW.name) THEN
            RAISE EXCEPTION 'group and department names share one Workforce scope'
                USING ERRCODE = '23505', SCHEMA = 'workforce', TABLE = 'departments', CONSTRAINT = 'departments_groups_name_key';
        END IF;
    ELSE
        RAISE EXCEPTION 'unsupported directory name scope' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER groups_name_scope_check BEFORE INSERT OR UPDATE OF name ON workforce.groups
    FOR EACH ROW EXECUTE FUNCTION workforce.check_directory_name();
CREATE TRIGGER departments_name_scope_check BEFORE INSERT OR UPDATE OF name ON workforce.departments
    FOR EACH ROW EXECUTE FUNCTION workforce.check_directory_name();

-- Categories, disciplines and complete titles share the third Workforce name scope.
CREATE FUNCTION workforce.check_title_name() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF TG_TABLE_SCHEMA <> 'workforce' OR TG_TABLE_NAME NOT IN ('categories', 'disciplines', 'titles') THEN
        RAISE EXCEPTION 'unsupported title name scope' USING ERRCODE = '23514';
    END IF;
    PERFORM workforce.lock_directory();
    IF (TG_TABLE_NAME <> 'categories' AND EXISTS (SELECT 1 FROM workforce.categories WHERE name = NEW.name))
        OR (TG_TABLE_NAME <> 'disciplines' AND EXISTS (SELECT 1 FROM workforce.disciplines WHERE name = NEW.name))
        OR (TG_TABLE_NAME <> 'titles' AND EXISTS (SELECT 1 FROM workforce.titles WHERE name = NEW.name)) THEN
        RAISE EXCEPTION 'title tree names share one Workforce scope'
            USING ERRCODE = '23505', SCHEMA = 'workforce', TABLE = TG_TABLE_NAME, CONSTRAINT = 'title_names_key';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER categories_name_scope_check BEFORE INSERT OR UPDATE OF name ON workforce.categories
    FOR EACH ROW EXECUTE FUNCTION workforce.check_title_name();
CREATE TRIGGER disciplines_name_scope_check BEFORE INSERT OR UPDATE OF name ON workforce.disciplines
    FOR EACH ROW EXECUTE FUNCTION workforce.check_title_name();
CREATE TRIGGER titles_name_scope_check BEFORE INSERT OR UPDATE OF name ON workforce.titles
    FOR EACH ROW EXECUTE FUNCTION workforce.check_title_name();

-- One original creation locates at most one directory item in any collection, even after its
-- completion has been reclaimed; each table's UNIQUE covers only that table.
CREATE FUNCTION workforce.check_directory_operation() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF TG_TABLE_SCHEMA <> 'workforce'
        OR TG_TABLE_NAME NOT IN ('groups', 'departments', 'positions', 'categories', 'disciplines', 'titles') THEN
        RAISE EXCEPTION 'unsupported directory collection' USING ERRCODE = '23514';
    END IF;
    PERFORM workforce.lock_directory();
    IF (TG_TABLE_NAME <> 'groups' AND EXISTS (SELECT 1 FROM workforce.groups WHERE operation_id = NEW.operation_id))
        OR (TG_TABLE_NAME <> 'departments' AND EXISTS (SELECT 1 FROM workforce.departments WHERE operation_id = NEW.operation_id))
        OR (TG_TABLE_NAME <> 'positions' AND EXISTS (SELECT 1 FROM workforce.positions WHERE operation_id = NEW.operation_id))
        OR (TG_TABLE_NAME <> 'categories' AND EXISTS (SELECT 1 FROM workforce.categories WHERE operation_id = NEW.operation_id))
        OR (TG_TABLE_NAME <> 'disciplines' AND EXISTS (SELECT 1 FROM workforce.disciplines WHERE operation_id = NEW.operation_id))
        OR (TG_TABLE_NAME <> 'titles' AND EXISTS (SELECT 1 FROM workforce.titles WHERE operation_id = NEW.operation_id)) THEN
        RAISE EXCEPTION 'one original creation locates at most one directory item'
            USING ERRCODE = '23505', SCHEMA = 'workforce', TABLE = TG_TABLE_NAME, CONSTRAINT = 'directory_operation_id_key';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER groups_operation_check BEFORE INSERT OR UPDATE OF operation_id ON workforce.groups
    FOR EACH ROW EXECUTE FUNCTION workforce.check_directory_operation();
CREATE TRIGGER departments_operation_check BEFORE INSERT OR UPDATE OF operation_id ON workforce.departments
    FOR EACH ROW EXECUTE FUNCTION workforce.check_directory_operation();
CREATE TRIGGER positions_operation_check BEFORE INSERT OR UPDATE OF operation_id ON workforce.positions
    FOR EACH ROW EXECUTE FUNCTION workforce.check_directory_operation();
CREATE TRIGGER categories_operation_check BEFORE INSERT OR UPDATE OF operation_id ON workforce.categories
    FOR EACH ROW EXECUTE FUNCTION workforce.check_directory_operation();
CREATE TRIGGER disciplines_operation_check BEFORE INSERT OR UPDATE OF operation_id ON workforce.disciplines
    FOR EACH ROW EXECUTE FUNCTION workforce.check_directory_operation();
CREATE TRIGGER titles_operation_check BEFORE INSERT OR UPDATE OF operation_id ON workforce.titles
    FOR EACH ROW EXECUTE FUNCTION workforce.check_directory_operation();

-- A parent cannot be the item or one of its descendants. The deferred composite foreign key
-- checks the direct same-group rule after a whole-subtree move has been assembled.
CREATE FUNCTION workforce.check_department_parent() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE ancestors integer; cyclic boolean;
BEGIN
    PERFORM workforce.lock_directory();
    IF NEW.parent_id IS NOT NULL THEN
        WITH RECURSIVE lineage(id, parent_id) AS (
            SELECT department.id, department.parent_id
            FROM workforce.departments department WHERE department.id = NEW.parent_id
            UNION
            SELECT department.id, department.parent_id
            FROM workforce.departments department JOIN lineage parent ON department.id = parent.parent_id
        )
        SELECT count(*) FILTER (WHERE id = NEW.parent_id), bool_or(id = NEW.id)
        INTO ancestors, cyclic FROM lineage;
        IF ancestors <> 1 OR cyclic THEN
            RAISE EXCEPTION 'department parent must stay outside its subtree'
                USING ERRCODE = '23514', CONSTRAINT = 'departments_ancestor_check';
        END IF;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER departments_ancestor_check BEFORE INSERT OR UPDATE OF parent_id, group_id ON workforce.departments
    FOR EACH ROW EXECUTE FUNCTION workforce.check_department_parent();

-- A cross-group move changes the whole subtree atomically; deferred checking admits either update order.
CREATE FUNCTION workforce.check_department_subtree() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    PERFORM workforce.lock_directory();
    IF EXISTS (
        WITH RECURSIVE subtree(id, group_id) AS (
            SELECT NEW.id, NEW.group_id
            UNION ALL
            SELECT department.id, department.group_id
            FROM workforce.departments department JOIN subtree parent ON department.parent_id = parent.id
        ) SELECT 1 FROM subtree WHERE group_id <> NEW.group_id
    ) THEN
        RAISE EXCEPTION 'a department move includes its complete subtree'
            USING ERRCODE = '23514', CONSTRAINT = 'departments_subtree_check';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER departments_subtree_check AFTER UPDATE OF group_id ON workforce.departments
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION workforce.check_department_subtree();

-- Every directory mutation has a matching completion appended in the same transaction.
CREATE FUNCTION workforce.check_directory_write() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE item uuid; collection text; command text;
BEGIN
    PERFORM workforce.lock_directory();
    item := CASE WHEN TG_OP = 'DELETE' THEN OLD.id ELSE NEW.id END;
    collection := TG_TABLE_NAME;
    command := lower(TG_OP);
    IF command = 'insert' THEN command := 'create'; END IF;
    IF command = 'create' THEN
        IF NOT EXISTS (SELECT 1 FROM workforce.completions
            WHERE operation_id = NEW.operation_id AND action = 'create' AND target = collection
                AND xmin = pg_catalog.pg_current_xact_id()::text::xid) THEN
            RAISE EXCEPTION 'directory creation requires its original completion'
                USING ERRCODE = '23514', CONSTRAINT = 'directory_completion_check';
        END IF;
    ELSIF EXISTS (SELECT 1 FROM workforce.completions completion
        WHERE completion.action = command AND completion.target = item::text
            AND completion.xmin = pg_catalog.pg_current_xact_id()::text::xid) THEN
        RETURN NULL;
    ELSIF TG_TABLE_NAME = 'departments' AND TG_OP = 'UPDATE' THEN
        -- A descendant's sole group change belongs to the moved ancestor's completion.
        IF NEW.group_id <> OLD.group_id AND NEW.parent_id IS NOT DISTINCT FROM OLD.parent_id
            AND ROW(NEW.id, NEW.operation_id, NEW.name, NEW.enabled, NEW.index)
                = ROW(OLD.id, OLD.operation_id, OLD.name, OLD.enabled, OLD.index)
            AND EXISTS (
                WITH RECURSIVE ancestors(id, parent_id) AS (
                    SELECT id, parent_id FROM workforce.departments WHERE id = NEW.parent_id
                    UNION
                    SELECT department.id, department.parent_id FROM workforce.departments department
                    JOIN ancestors child ON department.id = child.parent_id
                ) SELECT 1 FROM ancestors JOIN workforce.completions completion ON completion.target = ancestors.id::text
                    WHERE completion.action = 'update' AND completion.xmin = pg_catalog.pg_current_xact_id()::text::xid
            ) THEN
            RETURN NULL;
        END IF;
        RAISE EXCEPTION 'directory mutation requires its original completion'
            USING ERRCODE = '23514', CONSTRAINT = 'directory_completion_check';
    ELSE
        RAISE EXCEPTION 'directory mutation requires its original completion'
            USING ERRCODE = '23514', CONSTRAINT = 'directory_completion_check';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER groups_completion_check AFTER INSERT OR UPDATE OR DELETE ON workforce.groups
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION workforce.check_directory_write();
CREATE CONSTRAINT TRIGGER positions_completion_check AFTER INSERT OR UPDATE OR DELETE ON workforce.positions
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION workforce.check_directory_write();
CREATE CONSTRAINT TRIGGER departments_completion_check AFTER INSERT OR UPDATE OR DELETE ON workforce.departments
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION workforce.check_directory_write();
CREATE CONSTRAINT TRIGGER categories_completion_check AFTER INSERT OR UPDATE OR DELETE ON workforce.categories
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION workforce.check_directory_write();
CREATE CONSTRAINT TRIGGER disciplines_completion_check AFTER INSERT OR UPDATE OR DELETE ON workforce.disciplines
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION workforce.check_directory_write();
CREATE CONSTRAINT TRIGGER titles_completion_check AFTER INSERT OR UPDATE OR DELETE ON workforce.titles
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION workforce.check_directory_write();

-- A rule is configured only against a currently selectable department and its complete lineage.
CREATE FUNCTION workforce.check_rule_department() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE selectable boolean;
BEGIN
    PERFORM workforce.lock_directory();
    WITH RECURSIVE lineage(id, parent_id, group_id, enabled) AS (
        SELECT department.id, department.parent_id, department.group_id, department.enabled
        FROM workforce.departments department WHERE department.id = NEW.department_id
        UNION
        SELECT department.id, department.parent_id, department.group_id, department.enabled
        FROM workforce.departments department JOIN lineage child ON department.id = child.parent_id
    )
    SELECT COALESCE(bool_and(lineage.enabled), false)
        AND COALESCE((SELECT directory_group.enabled FROM workforce.groups directory_group
            WHERE directory_group.id = (SELECT group_id FROM lineage LIMIT 1)), false)
    INTO selectable FROM lineage;
    IF NOT selectable THEN
        RAISE EXCEPTION 'rule department and its ancestors must be selectable'
            USING ERRCODE = '23514', CONSTRAINT = 'rules_department_check';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER rules_department_check BEFORE INSERT OR UPDATE ON workforce.rules
    FOR EACH ROW EXECUTE FUNCTION workforce.check_rule_department();

-- Rule insertion and replacement have their own completion; later directory edits do not rewrite rules.
CREATE FUNCTION workforce.check_rule_write() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE command text;
BEGIN
    PERFORM workforce.lock_directory();
    command := CASE WHEN TG_OP = 'INSERT' THEN 'create' ELSE 'update' END;
    IF NOT EXISTS (SELECT 1 FROM workforce.completions completion
        WHERE completion.action = command AND completion.target = NEW.id::text
            AND completion.xmin = pg_catalog.pg_current_xact_id()::text::xid) THEN
        RAISE EXCEPTION 'rule mutation requires its original completion'
            USING ERRCODE = '23514', CONSTRAINT = 'rules_completion_check';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER rules_completion_check AFTER INSERT OR UPDATE ON workforce.rules
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION workforce.check_rule_write();

-- Every roster row change belongs to its department's complete-list replacement in this transaction.
CREATE FUNCTION workforce.check_roster_write() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE department_id uuid;
BEGIN
    PERFORM workforce.lock_directory();
    department_id := CASE WHEN TG_OP = 'DELETE' THEN OLD.department_id ELSE NEW.department_id END;
    IF NOT EXISTS (SELECT 1 FROM workforce.completions completion
        WHERE completion.action = 'replace' AND completion.target = department_id::text
            AND completion.xmin = pg_catalog.pg_current_xact_id()::text::xid) THEN
        RAISE EXCEPTION 'roster mutation requires its department replacement completion'
            USING ERRCODE = '23514', CONSTRAINT = 'rosters_completion_check';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER rosters_completion_check AFTER INSERT OR UPDATE OR DELETE ON workforce.rosters
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION workforce.check_roster_write();

-- Completion shape agrees with the affected collection and the visible post-write state.
CREATE FUNCTION workforce.check_completion() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE target_id uuid;
BEGIN
    IF NEW.family <> 'workforce_configuration' THEN
        IF NOT EXISTS (SELECT 1 FROM workforce.transitions record
            WHERE record.operation_id=NEW.operation_id AND record.person_id::text=NEW.target
                AND record.operator_id=NEW.revision_id AND record.happened_at=NEW.created_at
                AND record.kind=CASE NEW.family WHEN 'number_allocation' THEN 'allocation'
                    WHEN 'number_correction' THEN 'correction' WHEN 'number_recovery' THEN 'recovery' END) THEN
            RAISE EXCEPTION 'number completion retains its original record'
                USING ERRCODE = '23514', CONSTRAINT = 'completions_pairing_check';
        END IF;
        RETURN NULL;
    END IF;
    PERFORM workforce.lock_directory();
    IF NEW.created_at < transaction_timestamp() OR NEW.created_at > clock_timestamp() THEN
        RAISE EXCEPTION 'completion time belongs to its writing transaction'
            USING ERRCODE = '23514', CONSTRAINT = 'completions_transaction_check';
    END IF;
    IF NEW.target IN ('100', '200', '300', '400', '500', '600', '700', '801', '802', '803', '804', '901', '902', '903', '904', '1000') THEN
        IF NEW.action NOT IN ('create', 'update') THEN
            RAISE EXCEPTION 'rule completions only create or update a fixed subtype'
                USING ERRCODE = '23514', CONSTRAINT = 'completions_target_check';
        ELSIF NOT EXISTS (SELECT 1 FROM workforce.rules WHERE id = NEW.target::smallint) THEN
            RAISE EXCEPTION 'completed rule is missing'
                USING ERRCODE = '23514', CONSTRAINT = 'completions_pairing_check';
        END IF;
        RETURN NULL;
    END IF;
    IF NEW.action = 'create' THEN
        IF NEW.target = 'groups' AND NOT EXISTS (SELECT 1 FROM workforce.groups WHERE operation_id = NEW.operation_id) THEN
            RAISE EXCEPTION 'group creation is missing' USING ERRCODE = '23514', CONSTRAINT = 'completions_pairing_check';
        ELSIF NEW.target = 'departments' AND NOT EXISTS (SELECT 1 FROM workforce.departments WHERE operation_id = NEW.operation_id) THEN
            RAISE EXCEPTION 'department creation is missing' USING ERRCODE = '23514', CONSTRAINT = 'completions_pairing_check';
        ELSIF NEW.target = 'positions' AND NOT EXISTS (SELECT 1 FROM workforce.positions WHERE operation_id = NEW.operation_id) THEN
            RAISE EXCEPTION 'position creation is missing' USING ERRCODE = '23514', CONSTRAINT = 'completions_pairing_check';
        ELSIF NEW.target = 'categories' AND NOT EXISTS (SELECT 1 FROM workforce.categories WHERE operation_id = NEW.operation_id) THEN
            RAISE EXCEPTION 'category creation is missing' USING ERRCODE = '23514', CONSTRAINT = 'completions_pairing_check';
        ELSIF NEW.target = 'disciplines' AND NOT EXISTS (SELECT 1 FROM workforce.disciplines WHERE operation_id = NEW.operation_id) THEN
            RAISE EXCEPTION 'discipline creation is missing' USING ERRCODE = '23514', CONSTRAINT = 'completions_pairing_check';
        ELSIF NEW.target = 'titles' AND NOT EXISTS (SELECT 1 FROM workforce.titles WHERE operation_id = NEW.operation_id) THEN
            RAISE EXCEPTION 'title creation is missing' USING ERRCODE = '23514', CONSTRAINT = 'completions_pairing_check';
        ELSIF NEW.target NOT IN ('groups', 'departments', 'positions', 'categories', 'disciplines', 'titles') THEN
            RAISE EXCEPTION 'unsupported directory creation target' USING ERRCODE = '23514', CONSTRAINT = 'completions_pairing_check';
        END IF;
    ELSE
        BEGIN
            target_id := NEW.target::uuid;
        EXCEPTION WHEN invalid_text_representation THEN
            RAISE EXCEPTION 'directory update, deletion or replacement targets an item UUID'
                USING ERRCODE = '23514', CONSTRAINT = 'completions_target_check';
        END;
        IF NEW.target <> target_id::text THEN
            RAISE EXCEPTION 'directory target UUID must be canonical'
                USING ERRCODE = '23514', CONSTRAINT = 'completions_target_check';
        END IF;
        IF NEW.action = 'replace' AND NOT EXISTS (SELECT 1 FROM workforce.departments WHERE id = target_id) THEN
            RAISE EXCEPTION 'replaced roster department is missing' USING ERRCODE = '23514', CONSTRAINT = 'completions_pairing_check';
        ELSIF NEW.action = 'update' AND NOT EXISTS (
            SELECT 1 FROM workforce.groups WHERE id = target_id
            UNION ALL SELECT 1 FROM workforce.departments WHERE id = target_id
            UNION ALL SELECT 1 FROM workforce.positions WHERE id = target_id
            UNION ALL SELECT 1 FROM workforce.categories WHERE id = target_id
            UNION ALL SELECT 1 FROM workforce.disciplines WHERE id = target_id
            UNION ALL SELECT 1 FROM workforce.titles WHERE id = target_id
        ) THEN
            RAISE EXCEPTION 'updated directory item is missing' USING ERRCODE = '23514', CONSTRAINT = 'completions_pairing_check';
        ELSIF NEW.action = 'delete' AND EXISTS (
            SELECT 1 FROM workforce.groups WHERE id = target_id
            UNION ALL SELECT 1 FROM workforce.departments WHERE id = target_id
            UNION ALL SELECT 1 FROM workforce.positions WHERE id = target_id
            UNION ALL SELECT 1 FROM workforce.categories WHERE id = target_id
            UNION ALL SELECT 1 FROM workforce.disciplines WHERE id = target_id
            UNION ALL SELECT 1 FROM workforce.titles WHERE id = target_id
        ) THEN
            RAISE EXCEPTION 'deleted directory item remains' USING ERRCODE = '23514', CONSTRAINT = 'completions_pairing_check';
        END IF;
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER completions_pairing_check AFTER INSERT ON workforce.completions
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION workforce.check_completion();

-- Cleanup alone may delete completions, and only after their replay responsibility has expired.
CREATE FUNCTION workforce.check_completion_cleanup() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE observed_at timestamptz;
BEGIN
    IF OLD.family <> 'workforce_configuration' THEN
        RAISE EXCEPTION 'number completions are permanent'
            USING ERRCODE = '23514', CONSTRAINT = 'completions_retention_check';
    END IF;
    PERFORM workforce.lock_completion(OLD.operation_id);
    PERFORM workforce.lock_directory();
    observed_at := pg_catalog.clock_timestamp();
    IF OLD.created_at > observed_at - interval '24 hours' THEN
        RAISE EXCEPTION 'completion remains inside its replay retention window'
            USING ERRCODE = '23514', CONSTRAINT = 'completions_retention_check';
    END IF;
    RETURN OLD;
END
$$;
CREATE TRIGGER completions_retention_check BEFORE DELETE ON workforce.completions
    FOR EACH ROW EXECUTE FUNCTION workforce.check_completion_cleanup();

REVOKE ALL ON workforce.groups, workforce.departments, workforce.positions, workforce.categories, workforce.disciplines, workforce.titles, workforce.rules, workforce.rosters, workforce.completions FROM PUBLIC;
REVOKE ALL ON FUNCTION workforce.lock_directory(), workforce.check_directory_name(),
    workforce.check_department_parent(), workforce.check_department_subtree(),
    workforce.check_directory_write(), workforce.check_rule_department(), workforce.check_rule_write(), workforce.check_roster_write(), workforce.check_title_name(),
    workforce.check_completion(), workforce.check_completion_cleanup(), workforce.lock_completion(uuid) FROM PUBLIC;
