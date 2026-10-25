CREATE TABLE identity.avatars (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL,
    revision uuid NOT NULL,
    digest bytea NOT NULL CHECK (octet_length(digest) = 32),
    size bigint NOT NULL CHECK (size > 0 AND size <= 8388608),
    width integer NOT NULL CHECK (width BETWEEN 1 AND 1024),
    height integer NOT NULL CHECK (height = width),
    released_at timestamptz CHECK (isfinite(released_at)),
    release text CHECK (release IN ('cancellation', 'replacement')),
    release_operation_id uuid,
    release_transaction_id xid8,
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    UNIQUE (account_id, revision),
    CHECK ((released_at IS NULL AND release IS NULL AND release_operation_id IS NULL
            AND release_transaction_id IS NULL)
        OR (released_at IS NOT NULL AND release IS NOT NULL AND release_operation_id IS NOT NULL
            AND release_transaction_id IS NOT NULL))
);
CREATE UNIQUE INDEX avatars_current_idx ON identity.avatars (account_id) WHERE released_at IS NULL;
CREATE INDEX avatars_retention_idx ON identity.avatars (released_at, id) WHERE released_at IS NOT NULL;

CREATE SCHEMA access;
CREATE TABLE access.capabilities (
    id text PRIMARY KEY,
    scope text NOT NULL CHECK (scope IN ('institution', 'workforce')),
    UNIQUE (id, scope)
);
INSERT INTO access.capabilities (id, scope) VALUES
    ('institution.roles', 'institution'), ('institution.assignments', 'institution'),
    ('institution.workforce', 'institution'), ('institution.accounts', 'institution'),
    ('institution.corrections', 'institution'), ('institution.affiliations.read', 'institution'),
    ('institution.affiliations.correct', 'institution'), ('institution.articles', 'institution'),
    ('workforce.roles', 'workforce'), ('workforce.assignments', 'workforce'),
    ('workforce.admission', 'workforce'), ('workforce.suspension', 'workforce'),
    ('workforce.termination', 'workforce'), ('workforce.numbers', 'workforce'),
    ('workforce.directory', 'workforce'), ('workforce.templates', 'workforce'),
    ('workforce.approval', 'workforce'), ('workforce.production', 'workforce'),
    ('workforce.delivery', 'workforce');
CREATE TABLE access.completions (
    family text NOT NULL CHECK (family IN ('institution_role', 'workforce_role', 'institution_appointment', 'initialization', 'grant', 'workforce_assignment', 'grant_suspension', 'grant_resumption', 'grant_termination', 'invitation', 'invitation_adjustment', 'invitation_withdrawal')),
    operation_id uuid NOT NULL,
    action text NOT NULL,
    target text NOT NULL CHECK (target <> ''),
    input bytea NOT NULL CHECK (octet_length(input) >= 37),
    subject text NOT NULL,
    person_id uuid NOT NULL REFERENCES identity.persons (id),
    revision_id uuid NOT NULL,
    created_at timestamptz NOT NULL CHECK (isfinite(created_at)),
    expires_at timestamptz CHECK (expires_at IS NULL),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    PRIMARY KEY (family, operation_id),
    CHECK (CASE family
        WHEN 'initialization' THEN subject = 'bootstrap' AND action = 'initialize'
        WHEN 'institution_appointment' THEN
            (subject = 'session' AND action = 'assign') OR (subject = 'system' AND action IN ('grant', 'revoke'))
        WHEN 'grant' THEN subject = 'session' AND action IN ('create', 'accept')
        WHEN 'workforce_assignment' THEN subject = 'session' AND action = 'assign'
        WHEN 'grant_suspension' THEN subject = 'session' AND action = 'suspend'
        WHEN 'grant_resumption' THEN subject = 'session' AND action = 'resume'
        WHEN 'grant_termination' THEN subject = 'session' AND action IN ('leave', 'terminate')
        WHEN 'invitation' THEN subject = 'session' AND action = 'create'
        WHEN 'invitation_adjustment' THEN subject = 'session' AND action = 'adjust'
        WHEN 'invitation_withdrawal' THEN subject = 'session' AND action = 'withdraw'
        ELSE subject = 'session' AND action IN ('create', 'update', 'delete') END),
    FOREIGN KEY (revision_id, person_id) REFERENCES identity.revisions (id, person_id)
);
CREATE TABLE access.changes (
    id uuid PRIMARY KEY,
    role_id uuid NOT NULL,
    previous_id uuid,
    sequence bigint NOT NULL CHECK (sequence > 0),
    scope text NOT NULL DEFAULT 'institution' CHECK (scope IN ('institution', 'workforce')),
    entry text,
    action text NOT NULL DEFAULT 'create' CHECK (action IN ('create', 'update', 'delete')),
    name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 64
        AND name IS NFC NORMALIZED
        AND name COLLATE "C" !~ U&'[\0001-\001F\007F-\009F]'
        -- Unicode White_Space, independent of the database locale.
        AND name = btrim(name, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000')),
    level smallint NOT NULL CHECK (level BETWEEN 1 AND 1000),
    permissions text[] CHECK (permissions IS NULL OR CASE
        WHEN cardinality(permissions) = 0 THEN true
        WHEN array_ndims(permissions) = 1 AND array_lower(permissions, 1) = 1
            THEN array_position(permissions, NULL) IS NULL
        ELSE false END),
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    changed_at timestamptz NOT NULL CHECK (isfinite(changed_at)),
    operation_id uuid NOT NULL,
    -- Four system changes share the original initialization completion.
    family text GENERATED ALWAYS AS (
        CASE WHEN level BETWEEN 2 AND 999 THEN scope || '_role' ELSE 'initialization' END
    ) STORED,
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    UNIQUE (id, scope),
    UNIQUE (id, role_id, scope),
    UNIQUE (role_id, sequence),
    UNIQUE (id, role_id, scope, sequence, changed_at),
    UNIQUE (operation_id, role_id),
    FOREIGN KEY (family, operation_id) REFERENCES access.completions (family, operation_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (previous_id, role_id, scope) REFERENCES access.changes (id, role_id, scope),
    CHECK ((sequence = 1 AND previous_id IS NULL) OR (sequence > 1 AND previous_id IS NOT NULL)),
    CHECK ((action = 'delete' AND permissions IS NULL) OR (action <> 'delete' AND permissions IS NOT NULL)),
    CONSTRAINT changes_entry_check CHECK (
        CASE WHEN scope = 'workforce' AND level BETWEEN 2 AND 999
            THEN entry IS NOT NULL AND entry IN ('domain', 'institution')
            ELSE entry IS NULL END)
);
CREATE INDEX changes_role_id_changed_at_idx ON access.changes (role_id, changed_at);
-- Initialization owns four system changes; each online scope has its own operation family.
CREATE UNIQUE INDEX changes_institution_operation_id_key ON access.changes (operation_id)
    WHERE scope = 'institution' AND level BETWEEN 2 AND 999;
CREATE UNIQUE INDEX changes_workforce_operation_id_key ON access.changes (operation_id)
    WHERE scope = 'workforce' AND level BETWEEN 2 AND 999;
CREATE VIEW access.entries AS
    SELECT change.id AS change_id, change.scope, permission.capability_id
    FROM access.changes change CROSS JOIN LATERAL unnest(change.permissions) permission(capability_id);
CREATE TABLE access.roles (
    id uuid PRIMARY KEY,
    scope text NOT NULL DEFAULT 'institution' CHECK (scope IN ('institution', 'workforce')),
    name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 64
        AND name IS NFC NORMALIZED
        AND name COLLATE "C" !~ U&'[\0001-\001F\007F-\009F]'
        AND name = btrim(name, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000')),
    level smallint NOT NULL CHECK (level BETWEEN 1 AND 1000),
    change_id uuid NOT NULL,
    permissions text[] NOT NULL CHECK (CASE
        WHEN cardinality(permissions) = 0 THEN true
        WHEN array_ndims(permissions) = 1 AND array_lower(permissions, 1) = 1
            THEN array_position(permissions, NULL) IS NULL
        ELSE false END),
    active boolean NOT NULL,
    changed_at timestamptz NOT NULL CHECK (isfinite(changed_at)),
    sequence bigint NOT NULL CHECK (sequence > 0),
    version bigint NOT NULL CHECK (version > 0),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    UNIQUE (id, scope),
    FOREIGN KEY (change_id, id, scope, sequence, changed_at)
        REFERENCES access.changes (id, role_id, scope, sequence, changed_at) DEFERRABLE INITIALLY DEFERRED
);
-- The state and its records form one object and are written in the same transaction.
ALTER TABLE access.changes ADD FOREIGN KEY (role_id, scope)
    REFERENCES access.roles (id, scope) DEFERRABLE INITIALLY DEFERRED;
CREATE UNIQUE INDEX roles_scope_name_key ON access.roles (scope, name) WHERE active;
CREATE UNIQUE INDEX roles_system_idx ON access.roles (scope, level) WHERE level IN (1, 1000);
CREATE VIEW access.permissions AS
    SELECT role.id AS role_id, role.scope, permission.capability_id
    FROM access.roles role CROSS JOIN LATERAL unnest(role.permissions) permission(capability_id)
    WHERE role.active;
CREATE TABLE access.appointments (
    id uuid PRIMARY KEY,
    holder_id uuid NOT NULL,
    person_id uuid NOT NULL REFERENCES identity.persons (id),
    role_id uuid NOT NULL,
    scope text GENERATED ALWAYS AS ('institution'::text) STORED,
    previous_id uuid,
    change_id uuid NOT NULL,
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    revision_id uuid NOT NULL,
    assigned_at timestamptz NOT NULL CHECK (isfinite(assigned_at)),
    sequence bigint NOT NULL CHECK (sequence > 0),
    operation_id uuid NOT NULL,
    source text NOT NULL CHECK (source IN ('initialization', 'registration', 'session', 'system')),
    family text GENERATED ALWAYS AS (
        CASE source WHEN 'initialization' THEN 'initialization'
            WHEN 'session' THEN 'institution_appointment' WHEN 'system' THEN 'institution_appointment' END
    ) STORED,
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    UNIQUE (id, person_id, role_id),
    UNIQUE (id, person_id),
    UNIQUE (id, holder_id, person_id),
    UNIQUE (holder_id, sequence),
    CHECK (CASE WHEN source IN ('initialization', 'registration')
        THEN sequence = 1 AND previous_id IS NULL AND operator_id = revision_id
        ELSE sequence > 1 AND previous_id IS NOT NULL END),
    FOREIGN KEY (previous_id, holder_id, person_id) REFERENCES access.appointments (id, holder_id, person_id),
    FOREIGN KEY (change_id, role_id, scope) REFERENCES access.changes (id, role_id, scope),
    FOREIGN KEY (family, operation_id) REFERENCES access.completions (family, operation_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (revision_id, person_id) REFERENCES identity.revisions (id, person_id)
);
CREATE INDEX appointments_role_id_assigned_at_idx ON access.appointments (role_id, assigned_at);
CREATE INDEX appointments_operation_id_idx ON access.appointments (operation_id);
CREATE INDEX appointments_assigned_at_id_idx ON access.appointments (assigned_at DESC, id DESC);
CREATE UNIQUE INDEX appointments_original_key ON access.appointments (family, operation_id) WHERE family IS NOT NULL;
CREATE TABLE identity.corrections (
    id uuid PRIMARY KEY,
    person_id uuid NOT NULL REFERENCES identity.persons (id),
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    before_id uuid NOT NULL,
    corrected_at timestamptz NOT NULL CHECK (isfinite(corrected_at)),
    sequence bigint NOT NULL CHECK (sequence > 0),
    appointment_id uuid,
    operation_id uuid NOT NULL UNIQUE,
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    UNIQUE (person_id, sequence),
    FOREIGN KEY (appointment_id, person_id) REFERENCES access.appointments (id, person_id),
    FOREIGN KEY (before_id, person_id) REFERENCES identity.revisions (id, person_id),
    FOREIGN KEY (id, person_id) REFERENCES identity.revisions (id, person_id) DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE access.holders (
    id uuid PRIMARY KEY,
    person_id uuid NOT NULL UNIQUE REFERENCES identity.persons (id),
    role_id uuid NOT NULL,
    scope text GENERATED ALWAYS AS ('institution'::text) STORED,
    assignment_id uuid NOT NULL UNIQUE,
    cancellation_id uuid UNIQUE REFERENCES identity.cancellations (id),
    changed_at timestamptz NOT NULL CHECK (isfinite(changed_at)),
    sequence bigint NOT NULL CHECK (sequence > 0),
    version bigint NOT NULL CHECK (version > 0),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    UNIQUE (id, person_id),
    CONSTRAINT holders_role_id_scope_fkey FOREIGN KEY (role_id, scope) REFERENCES access.roles (id, scope),
    FOREIGN KEY (assignment_id, person_id, role_id) REFERENCES access.appointments (id, person_id, role_id) DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX holders_role_id_idx ON access.holders (role_id);
-- Holder points to its latest appointment; every appointment also retains its Holder.
ALTER TABLE access.appointments ADD FOREIGN KEY (holder_id, person_id)
    REFERENCES access.holders (id, person_id) DEFERRABLE INITIALLY DEFERRED;
CREATE TABLE access.projections (
    id uuid PRIMARY KEY REFERENCES identity.cancellations (id),
    holder_id uuid NOT NULL,
    person_id uuid NOT NULL,
    role_id uuid NOT NULL,
    assignment_id uuid NOT NULL,
    projected_at timestamptz NOT NULL CHECK (isfinite(projected_at)),
    sequence bigint NOT NULL CHECK (sequence > 1),
    operation_id uuid NOT NULL,
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    UNIQUE (holder_id, sequence),
    FOREIGN KEY (holder_id, person_id) REFERENCES access.holders (id, person_id),
    FOREIGN KEY (assignment_id, person_id, role_id) REFERENCES access.appointments (id, person_id, role_id)
);

CREATE TABLE access.grants (
    id uuid PRIMARY KEY,
    person_id uuid NOT NULL REFERENCES identity.persons (id),
    domain text NOT NULL DEFAULT 'workforce' CHECK (domain = 'workforce'),
    entry text NOT NULL CHECK (entry IN ('domain', 'institution', 'invitation')),
    invitation_id uuid,
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    revision_id uuid NOT NULL,
    granted_at timestamptz NOT NULL CHECK (isfinite(granted_at)),
    sequence bigint NOT NULL CHECK (sequence > 0),
    version bigint NOT NULL CHECK (version > 0),
    operation_id uuid NOT NULL UNIQUE,
    family text GENERATED ALWAYS AS ('grant'::text) STORED,
    role_id uuid NOT NULL,
    assignment_id uuid NOT NULL,
    suspension_id uuid,
    termination text CHECK (termination IN ('self', 'manager', 'cancellation')),
    epoch bigint NOT NULL CHECK (epoch >= 0),
    changed_at timestamptz NOT NULL CHECK (isfinite(changed_at) AND changed_at >= granted_at),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    UNIQUE (id, person_id),
    CHECK ((entry = 'invitation') = (invitation_id IS NOT NULL)),
    FOREIGN KEY (family, operation_id) REFERENCES access.completions (family, operation_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (role_id, domain) REFERENCES access.roles (id, scope),
    FOREIGN KEY (revision_id, person_id) REFERENCES identity.revisions (id, person_id)
);
CREATE INDEX grants_granted_at_id_idx ON access.grants (granted_at DESC, id DESC);
CREATE UNIQUE INDEX grants_person_id_domain_unfinished_key ON access.grants (person_id, domain) WHERE termination IS NULL;
CREATE INDEX grants_person_id_granted_at_id_idx ON access.grants (person_id, granted_at DESC, id DESC);
CREATE INDEX grants_role_id_unfinished_idx ON access.grants (role_id, id) WHERE termination IS NULL;
CREATE TABLE access.suspensions (
    id uuid PRIMARY KEY,
    grant_id uuid NOT NULL,
    person_id uuid NOT NULL,
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    revision_id uuid NOT NULL,
    suspended_at timestamptz NOT NULL CHECK (isfinite(suspended_at)),
    sequence bigint NOT NULL CHECK (sequence > 1),
    operation_id uuid NOT NULL UNIQUE,
    family text GENERATED ALWAYS AS ('grant_suspension'::text) STORED,
    role_id uuid NOT NULL,
    assignment_id uuid NOT NULL,
    epoch bigint NOT NULL CHECK (epoch >= 0),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    UNIQUE (id, person_id),
    UNIQUE (id, grant_id, person_id),
    UNIQUE (grant_id, sequence),
    CHECK (operator_id <> revision_id),
    FOREIGN KEY (family, operation_id) REFERENCES access.completions (family, operation_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (grant_id, person_id) REFERENCES access.grants (id, person_id),
    FOREIGN KEY (revision_id, person_id) REFERENCES identity.revisions (id, person_id)
);
CREATE INDEX suspensions_grant_id_idx ON access.suspensions (grant_id);
CREATE INDEX suspensions_suspended_at_id_idx ON access.suspensions (suspended_at DESC, id DESC);
CREATE TABLE access.assignments (
    id uuid PRIMARY KEY,
    grant_id uuid NOT NULL,
    person_id uuid NOT NULL,
    role_id uuid NOT NULL,
    scope text GENERATED ALWAYS AS ('workforce'::text) STORED,
    previous_id uuid,
    change_id uuid NOT NULL,
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    revision_id uuid NOT NULL,
    assigned_at timestamptz NOT NULL CHECK (isfinite(assigned_at)),
    sequence bigint NOT NULL CHECK (sequence > 0),
    operation_id uuid NOT NULL,
    entry text CHECK (entry IN ('domain', 'institution')),
    family text GENERATED ALWAYS AS (CASE WHEN entry IS NULL THEN 'grant' ELSE 'workforce_assignment' END) STORED,
    suspension_id uuid,
    epoch bigint NOT NULL CHECK (epoch >= 0),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    CHECK ((sequence = 1 AND previous_id IS NULL AND entry IS NULL AND suspension_id IS NULL AND epoch = 0)
        OR (sequence > 1 AND previous_id IS NOT NULL AND entry IS NOT NULL)),
    UNIQUE (id, grant_id, person_id, role_id),
    UNIQUE (id, grant_id, person_id),
    UNIQUE (grant_id, sequence),
    FOREIGN KEY (family, operation_id) REFERENCES access.completions (family, operation_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (previous_id, grant_id, person_id) REFERENCES access.assignments (id, grant_id, person_id),
    FOREIGN KEY (change_id, role_id, scope) REFERENCES access.changes (id, role_id, scope),
    FOREIGN KEY (suspension_id, grant_id, person_id) REFERENCES access.suspensions (id, grant_id, person_id),
    FOREIGN KEY (grant_id, person_id) REFERENCES access.grants (id, person_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (revision_id, person_id) REFERENCES identity.revisions (id, person_id)
);
CREATE UNIQUE INDEX assignments_operation_id_key ON access.assignments (operation_id) WHERE entry IS NOT NULL;
CREATE INDEX assignments_operation_id_idx ON access.assignments (operation_id);
CREATE INDEX assignments_assigned_at_id_idx ON access.assignments (assigned_at DESC, id DESC);
CREATE INDEX assignments_person_id_assigned_at_id_idx ON access.assignments (person_id, assigned_at DESC, id DESC);
CREATE TABLE access.resumptions (
    id uuid PRIMARY KEY,
    grant_id uuid NOT NULL,
    person_id uuid NOT NULL,
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    revision_id uuid NOT NULL,
    resumed_at timestamptz NOT NULL CHECK (isfinite(resumed_at)),
    sequence bigint NOT NULL CHECK (sequence > 1),
    operation_id uuid NOT NULL UNIQUE,
    family text GENERATED ALWAYS AS ('grant_resumption'::text) STORED,
    role_id uuid NOT NULL,
    assignment_id uuid NOT NULL,
    epoch bigint NOT NULL CHECK (epoch >= 0),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    UNIQUE (grant_id, sequence),
    CHECK (operator_id <> revision_id),
    FOREIGN KEY (family, operation_id) REFERENCES access.completions (family, operation_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (id, grant_id, person_id) REFERENCES access.suspensions (id, grant_id, person_id),
    FOREIGN KEY (assignment_id, grant_id, person_id, role_id) REFERENCES access.assignments (id, grant_id, person_id, role_id),
    FOREIGN KEY (revision_id, person_id) REFERENCES identity.revisions (id, person_id)
);
CREATE INDEX resumptions_resumed_at_id_idx ON access.resumptions (resumed_at DESC, id DESC);
CREATE TABLE access.terminations (
    id uuid PRIMARY KEY,
    person_id uuid NOT NULL,
    operator_id uuid NOT NULL REFERENCES identity.revisions (id),
    revision_id uuid NOT NULL,
    terminated_at timestamptz NOT NULL CHECK (isfinite(terminated_at)),
    sequence bigint NOT NULL CHECK (sequence > 1),
    operation_id uuid NOT NULL,
    mode text NOT NULL CHECK (mode IN ('self', 'manager', 'cancellation')),
    cancellation_id uuid REFERENCES identity.cancellations (id),
    family text GENERATED ALWAYS AS (CASE WHEN mode <> 'cancellation' THEN 'grant_termination' END) STORED,
    role_id uuid NOT NULL,
    assignment_id uuid NOT NULL,
    suspension_id uuid,
    epoch bigint NOT NULL CHECK (epoch >= 0),
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    CHECK ((mode = 'cancellation') = (cancellation_id IS NOT NULL)),
    CHECK (mode = 'cancellation' OR ((mode = 'self') = (operator_id = revision_id))),
    FOREIGN KEY (family, operation_id) REFERENCES access.completions (family, operation_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (id, person_id) REFERENCES access.grants (id, person_id),
    FOREIGN KEY (assignment_id, id, person_id, role_id) REFERENCES access.assignments (id, grant_id, person_id, role_id),
    FOREIGN KEY (suspension_id, id, person_id) REFERENCES access.suspensions (id, grant_id, person_id),
    FOREIGN KEY (revision_id, person_id) REFERENCES identity.revisions (id, person_id)
);
CREATE INDEX terminations_terminated_at_id_idx ON access.terminations (terminated_at DESC, id DESC);
CREATE UNIQUE INDEX terminations_operation_id_human_key ON access.terminations (operation_id) WHERE mode <> 'cancellation';
CREATE TABLE access.losses (
    id uuid PRIMARY KEY,
    grant_id uuid NOT NULL,
    person_id uuid NOT NULL,
    role_id uuid NOT NULL,
    scope text GENERATED ALWAYS AS ('workforce'::text) STORED,
    assignment_id uuid NOT NULL,
    suspension_id uuid,
    epoch bigint NOT NULL CHECK (epoch > 0),
    change_id uuid NOT NULL,
    projected_at timestamptz NOT NULL CHECK (isfinite(projected_at)),
    sequence bigint NOT NULL CHECK (sequence > 1),
    operation_id uuid NOT NULL,
    seal bytea NOT NULL CHECK (octet_length(seal) = 32),
    seal_key_id bigint NOT NULL REFERENCES security.keys (id),
    seal_version smallint NOT NULL CHECK (seal_version = 1),
    transaction_id xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
    UNIQUE (grant_id, sequence),
    UNIQUE (grant_id, change_id),
    FOREIGN KEY (grant_id, person_id) REFERENCES access.grants (id, person_id),
    FOREIGN KEY (change_id, role_id, scope) REFERENCES access.changes (id, role_id, scope),
    FOREIGN KEY (assignment_id, grant_id, person_id, role_id) REFERENCES access.assignments (id, grant_id, person_id, role_id),
    FOREIGN KEY (suspension_id, grant_id, person_id) REFERENCES access.suspensions (id, grant_id, person_id)
);
-- Current pointers and their records form a circular, same-transaction object.
ALTER TABLE access.grants ADD FOREIGN KEY (assignment_id, id, person_id, role_id)
    REFERENCES access.assignments (id, grant_id, person_id, role_id) DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE access.grants ADD FOREIGN KEY (suspension_id, id, person_id)
    REFERENCES access.suspensions (id, grant_id, person_id) DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE access.suspensions ADD FOREIGN KEY (assignment_id, grant_id, person_id, role_id)
    REFERENCES access.assignments (id, grant_id, person_id, role_id);

CREATE FUNCTION access.loss_valid(target uuid) RETURNS boolean
LANGUAGE sql STABLE SET search_path = '' AS $$
    SELECT EXISTS (SELECT 1 FROM access.losses loss
        JOIN access.changes cause ON cause.id = loss.change_id
        WHERE loss.id = target AND cause.role_id = loss.role_id AND cause.scope = 'workforce'
            AND cause.action = 'update'
            AND cause.changed_at = loss.projected_at AND cause.operation_id = loss.operation_id
            AND cause.transaction_id = loss.transaction_id)
$$;

CREATE FUNCTION access.lock_grant(uuid) RETURNS void
LANGUAGE sql VOLATILE SECURITY DEFINER SET search_path = '' AS $$
    SELECT FROM access.grants WHERE id = $1 FOR SHARE
$$;


-- Corrections retain the assignment observed when they were accepted. Holder and person
-- sequences belong to different objects and cannot be compared.
CREATE FUNCTION identity.administered(appointment uuid) RETURNS boolean
LANGUAGE sql STABLE SET search_path = '' AS $$
    SELECT EXISTS (SELECT 1 FROM access.appointments observed
        JOIN access.changes definition ON definition.id = observed.change_id
            AND definition.role_id = observed.role_id AND definition.scope = observed.scope
        WHERE observed.id = appointment AND definition.level = 1000)
$$;

CREATE FUNCTION identity.check_avatar() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE target uuid;
BEGIN
    IF TG_TABLE_NAME = 'accounts' THEN
        target := CASE WHEN TG_OP = 'DELETE' THEN OLD.id ELSE NEW.id END;
    ELSE
        target := CASE WHEN TG_OP = 'DELETE' THEN OLD.account_id ELSE NEW.account_id END;
    END IF;
    IF EXISTS (SELECT 1 FROM identity.accounts WHERE id = target AND cancellation_id IS NULL) THEN
        IF (SELECT count(*) FROM identity.avatars WHERE account_id = target AND released_at IS NULL) <> 1
            OR NOT EXISTS (SELECT 1 FROM identity.accounts account
                JOIN identity.avatars avatar ON avatar.account_id = account.id AND avatar.released_at IS NULL
                WHERE account.id = target AND account.avatar = avatar.revision) THEN
            RAISE EXCEPTION 'account requires exactly one current avatar' USING ERRCODE = '23514';
        END IF;
    ELSIF EXISTS (SELECT 1 FROM identity.avatars WHERE account_id = target AND released_at IS NULL) THEN
        RAISE EXCEPTION 'current avatar requires its account' USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER accounts_avatar_check AFTER INSERT OR UPDATE OF cancellation_id ON identity.accounts
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_avatar();
CREATE CONSTRAINT TRIGGER avatars_account_check AFTER INSERT OR UPDATE OR DELETE ON identity.avatars
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_avatar();
CREATE FUNCTION identity.check_avatar_transition() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF ROW(NEW.id, NEW.account_id, NEW.digest, NEW.size, NEW.width, NEW.height)
        IS DISTINCT FROM ROW(OLD.id, OLD.account_id, OLD.digest, OLD.size, OLD.width, OLD.height)
        OR NEW.revision IS DISTINCT FROM OLD.revision
        OR OLD.released_at IS NOT NULL OR NEW.released_at IS NULL THEN
        RAISE EXCEPTION 'avatar content is immutable and release is final' USING ERRCODE = '23514';
    END IF;
    IF NEW.release_transaction_id IS DISTINCT FROM pg_catalog.pg_current_xact_id() THEN
        RAISE EXCEPTION 'avatar release is stamped by its replacing transaction' USING ERRCODE = '23514';
    END IF;
    IF NEW.release = 'cancellation' THEN
        IF NOT identity.cancelling(OLD.account_id)
            OR NEW.released_at IS DISTINCT FROM (SELECT cancelled_at FROM identity.cancellations WHERE account_id = OLD.account_id)
            OR NEW.release_operation_id IS DISTINCT FROM (SELECT operation_id FROM identity.cancellations WHERE account_id = OLD.account_id) THEN
            RAISE EXCEPTION 'avatar release requires its current account cancellation' USING ERRCODE = '23514';
        END IF;
    ELSIF NEW.release = 'replacement' THEN
        IF identity.cancelling(OLD.account_id)
            OR NOT EXISTS (SELECT 1 FROM identity.portraits portrait
                JOIN identity.accounts account ON account.id = portrait.account_id
                WHERE portrait.account_id = OLD.account_id AND portrait.operation_id = NEW.release_operation_id
                    AND portrait.previous_revision = OLD.revision
                    AND portrait.portrait_at = NEW.released_at
                    AND portrait.transaction_id = pg_catalog.pg_current_xact_id()
                    AND account.avatar = portrait.revision
                    AND account.person_id = portrait.person_id
                    AND account.sequence = portrait.sequence
                    AND account.changed_at = portrait.portrait_at) THEN
            RAISE EXCEPTION 'avatar replacement release requires its paired advancement' USING ERRCODE = '23514';
        END IF;
    ELSE
        RAISE EXCEPTION 'avatar release declares its source' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER avatars_transition_check BEFORE UPDATE ON identity.avatars
    FOR EACH ROW EXECUTE FUNCTION identity.check_avatar_transition();

CREATE FUNCTION identity.check_avatar_deletion() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF OLD.released_at IS NULL THEN
        RAISE EXCEPTION 'current avatar cannot be deleted' USING ERRCODE = '23514';
    END IF;
    RETURN OLD;
END
$$;
CREATE TRIGGER avatars_deletion_check BEFORE DELETE ON identity.avatars
    FOR EACH ROW EXECUTE FUNCTION identity.check_avatar_deletion();

-- Unmigrated assignment consumers must select a live state.
CREATE FUNCTION access.check_current_role(target uuid) RETURNS void
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM access.roles WHERE id = target AND active) THEN
        RAISE EXCEPTION 'role selection requires a live state' USING ERRCODE = '23514';
    END IF;
END
$$;

-- Call after scope protection and the operator account, before the Workforce role row.
CREATE FUNCTION access.lock_role_holders(target uuid, authority uuid) RETURNS void
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    PERFORM holder.id FROM access.holders holder JOIN access.grants member ON member.person_id = holder.person_id
        WHERE member.role_id = target AND member.termination IS NULL ORDER BY holder.id FOR SHARE OF holder;
    PERFORM role.id FROM access.roles role WHERE role.id = authority OR role.id IN (
        SELECT holder.role_id FROM access.holders holder JOIN access.grants member ON member.person_id = holder.person_id
            WHERE member.role_id = target AND member.termination IS NULL)
        ORDER BY role.id FOR SHARE;
END
$$;

-- Structural pairing only: every newly allocated sequence has a record from this transaction.
CREATE FUNCTION access.check_role() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE
    previous_sequence bigint;
    previous_version bigint;
BEGIN
    previous_sequence := CASE WHEN TG_OP = 'INSERT' THEN 0 ELSE OLD.sequence END;
    previous_version := CASE WHEN TG_OP = 'INSERT' THEN 0 ELSE OLD.version END;
    IF NEW.sequence <= previous_sequence OR NEW.version <> previous_version + 1
        OR (TG_OP = 'UPDATE' AND NEW.id <> OLD.id)
        OR EXISTS (
            SELECT 1 FROM pg_catalog.generate_series(previous_sequence + 1, NEW.sequence) expected(sequence)
            WHERE NOT EXISTS (
                SELECT 1 FROM access.changes record
                WHERE record.role_id = NEW.id AND record.sequence = expected.sequence
                    AND record.xmin = pg_catalog.pg_current_xact_id()::text::xid
            )
        ) THEN
        RAISE EXCEPTION 'state advancement requires its new contiguous records'
            USING ERRCODE = '23514', CONSTRAINT = 'roles_records_check';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER roles_records_check AFTER INSERT OR UPDATE ON access.roles
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION access.check_role();


CREATE FUNCTION access.check_holder() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE target uuid;
BEGIN
    target := CASE WHEN TG_OP = 'DELETE' THEN OLD.id ELSE NEW.id END;
    IF EXISTS (SELECT 1 FROM identity.accounts WHERE id = target)
        AND NOT EXISTS (SELECT 1 FROM access.holders WHERE id = target) THEN
        RAISE EXCEPTION 'account requires its current institutional assignment' USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER accounts_holder_check AFTER INSERT ON identity.accounts
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION access.check_holder();

-- Each holder advancement pairs new object-local records and the exact resulting head.
CREATE FUNCTION access.check_holder_records() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE previous_sequence bigint; previous_version bigint;
BEGIN
    previous_sequence := CASE WHEN TG_OP = 'INSERT' THEN 0 ELSE OLD.sequence END;
    previous_version := CASE WHEN TG_OP = 'INSERT' THEN 0 ELSE OLD.version END;
    IF NEW.sequence <= previous_sequence OR NEW.version <> previous_version + 1
        OR (TG_OP = 'UPDATE' AND ROW(NEW.id, NEW.person_id) IS DISTINCT FROM ROW(OLD.id, OLD.person_id))
        OR EXISTS (
            SELECT 1 FROM pg_catalog.generate_series(previous_sequence + 1, NEW.sequence) expected(sequence)
            WHERE (SELECT count(*) FROM (
                SELECT sequence FROM access.appointments
                    WHERE holder_id = NEW.id AND sequence = expected.sequence
                        AND xmin = pg_catalog.pg_current_xact_id()::text::xid
                UNION ALL SELECT sequence FROM access.projections
                    WHERE holder_id = NEW.id AND sequence = expected.sequence
                        AND xmin = pg_catalog.pg_current_xact_id()::text::xid
            ) records) <> 1)
        OR NOT (CASE WHEN NEW.cancellation_id IS NULL THEN
            EXISTS (SELECT 1 FROM access.appointments record WHERE record.holder_id = NEW.id
                AND record.person_id = NEW.person_id AND record.sequence = NEW.sequence
                AND record.id = NEW.assignment_id AND record.role_id = NEW.role_id
                AND record.assigned_at = NEW.changed_at)
        ELSE EXISTS (SELECT 1 FROM access.projections record
            JOIN identity.cancellations cause ON cause.id = record.id
            WHERE record.holder_id = NEW.id AND record.person_id = NEW.person_id
                AND record.sequence = NEW.sequence AND record.id = NEW.cancellation_id
                AND record.assignment_id = NEW.assignment_id AND record.role_id = NEW.role_id
                AND record.projected_at = NEW.changed_at
                AND cause.account_id = NEW.id AND cause.person_id = NEW.person_id
                AND cause.cancelled_at = record.projected_at AND cause.operation_id = record.operation_id
                AND cause.xmin = pg_catalog.pg_current_xact_id()::text::xid) END) THEN
        RAISE EXCEPTION 'holder advancement requires its contiguous records and paired head'
            USING ERRCODE = '23514', CONSTRAINT = 'holders_records_check';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER holders_records_check AFTER INSERT OR UPDATE ON access.holders
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION access.check_holder_records();

-- Structural pairing of the Grant state and its object-local records.
CREATE VIEW access.transitions AS
    SELECT grant_id,person_id,role_id,id AS assignment_id,suspension_id,NULL::text AS termination,
        epoch,assigned_at AS happened_at,sequence,operation_id,transaction_id,xmin AS inserted_in
    FROM access.assignments
    UNION ALL
    SELECT grant_id,person_id,role_id,assignment_id,id,NULL::text,
        epoch,suspended_at,sequence,operation_id,transaction_id,xmin FROM access.suspensions
    UNION ALL
    SELECT grant_id,person_id,role_id,assignment_id,NULL::uuid,NULL::text,
        epoch,resumed_at,sequence,operation_id,transaction_id,xmin FROM access.resumptions
    UNION ALL
    SELECT id,person_id,role_id,assignment_id,suspension_id,mode,
        epoch,terminated_at,sequence,operation_id,transaction_id,xmin FROM access.terminations
    UNION ALL
    SELECT grant_id,person_id,role_id,assignment_id,suspension_id,NULL::text,
        epoch,projected_at,sequence,operation_id,transaction_id,xmin FROM access.losses;

-- Check only this advancement; business transitions and retained history belong to Rust.
CREATE FUNCTION access.check_grant_state() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE previous_sequence bigint; previous_version bigint;
BEGIN
    previous_sequence := CASE WHEN TG_OP = 'INSERT' THEN 0 ELSE OLD.sequence END;
    previous_version := CASE WHEN TG_OP = 'INSERT' THEN 0 ELSE OLD.version END;
    IF NEW.sequence <= previous_sequence OR NEW.version <> previous_version + 1
        OR (TG_OP = 'UPDATE' AND
            ROW(NEW.id,NEW.person_id,NEW.domain,NEW.entry,NEW.invitation_id,NEW.operator_id,NEW.revision_id,NEW.granted_at,NEW.operation_id,NEW.transaction_id)
            IS DISTINCT FROM ROW(OLD.id,OLD.person_id,OLD.domain,OLD.entry,OLD.invitation_id,OLD.operator_id,OLD.revision_id,OLD.granted_at,OLD.operation_id,OLD.transaction_id))
        OR EXISTS (
            SELECT 1 FROM pg_catalog.generate_series(previous_sequence + 1, NEW.sequence) expected(sequence)
            WHERE (SELECT count(*) FROM access.transitions record
                WHERE record.grant_id=NEW.id AND record.sequence=expected.sequence
                    AND record.inserted_in=pg_catalog.pg_current_xact_id()::text::xid) <> 1)
        OR EXISTS (SELECT 1 FROM access.losses loss
            JOIN access.changes cause ON cause.id=loss.change_id
            WHERE loss.grant_id=NEW.id AND loss.sequence>previous_sequence AND loss.sequence<=NEW.sequence
                AND (NOT access.loss_valid(loss.id)
                    OR cause.xmin<>pg_catalog.pg_current_xact_id()::text::xid))
        OR NOT EXISTS (SELECT 1 FROM access.transitions head
            WHERE head.grant_id=NEW.id AND head.sequence=NEW.sequence
                AND ROW(head.person_id,head.role_id,head.assignment_id,head.suspension_id,head.termination,head.epoch,head.happened_at)
                    IS NOT DISTINCT FROM ROW(NEW.person_id,NEW.role_id,NEW.assignment_id,NEW.suspension_id,NEW.termination,NEW.epoch,NEW.changed_at))
        OR NOT EXISTS (SELECT 1 FROM access.assignments first
            WHERE first.grant_id=NEW.id AND first.sequence=1
                AND ROW(first.person_id,first.operator_id,first.revision_id,first.assigned_at,first.operation_id,first.transaction_id)
                    IS NOT DISTINCT FROM ROW(NEW.person_id,NEW.operator_id,NEW.revision_id,NEW.granted_at,NEW.operation_id,NEW.transaction_id)) THEN
        RAISE EXCEPTION 'grant advancement requires its contiguous records and paired head'
            USING ERRCODE = '23514', CONSTRAINT = 'grants_state_check';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER grants_state_check AFTER INSERT OR UPDATE ON access.grants
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION access.check_grant_state();

CREATE FUNCTION access.check_system_creation() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF NEW.action = 'create' AND NEW.level IN (1, 1000)
        AND NOT EXISTS (SELECT 1 FROM access.appointments appointment
            WHERE appointment.source = 'initialization' AND appointment.operation_id = NEW.operation_id
                AND appointment.transaction_id = NEW.transaction_id
                AND appointment.operator_id = NEW.operator_id AND appointment.assigned_at = NEW.changed_at) THEN
        RAISE EXCEPTION 'system role creation requires complete initialization' USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER changes_initialization_check AFTER INSERT ON access.changes
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION access.check_system_creation();

CREATE FUNCTION identity.check_cancellation_deletion() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE target uuid;
BEGIN
    IF TG_TABLE_NAME IN ('accounts', 'holders') THEN
        target := OLD.id;
    ELSE
        target := OLD.account_id;
    END IF;
    IF NOT identity.cancelling(target) THEN
        RAISE EXCEPTION 'account-owned deletion requires its original account and current cancellation' USING ERRCODE = '23514';
    END IF;
    RETURN OLD;
END
$$;
CREATE TRIGGER receipts_cancellation_check BEFORE DELETE ON identity.receipts
    FOR EACH ROW EXECUTE FUNCTION identity.check_cancellation_deletion();

CREATE FUNCTION identity.check_cancellation_completion() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE target uuid; person uuid;
BEGIN
    target := NEW.account_id;
    person := NEW.person_id;
    IF NOT EXISTS (SELECT 1 FROM identity.cancellations cancellation WHERE cancellation.account_id = target
            AND cancellation.person_id = person AND cancellation.transaction_id = pg_catalog.pg_current_xact_id())
        OR NOT EXISTS (SELECT 1 FROM access.holders holder
            JOIN identity.cancellations cancellation ON cancellation.id = holder.cancellation_id
            WHERE holder.id = target AND holder.person_id = person
                AND cancellation.account_id = target AND cancellation.person_id = person)
        OR NOT EXISTS (SELECT 1 FROM identity.accounts account
            WHERE account.id = target AND account.person_id = person
                AND account.cancellation_id = NEW.id)
        OR EXISTS (SELECT 1 FROM identity.sessions WHERE account_id = target)
        OR EXISTS (SELECT 1 FROM identity.receipts WHERE account_id = target)
        OR EXISTS (SELECT 1 FROM identity.avatars WHERE account_id = target AND released_at IS NULL)
        OR EXISTS (SELECT 1 FROM access.grants WHERE person_id = person AND termination IS NULL) THEN
        RAISE EXCEPTION 'cancellation, account tombstone and current-credential removal must commit together' USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER cancellations_completion_check AFTER INSERT ON identity.cancellations
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_cancellation_completion();

CREATE FUNCTION identity.check_correction_binding() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE original record; paired integer;
BEGIN
    IF TG_TABLE_NAME = 'completions' THEN
        IF NEW.family NOT IN ('system_correction', 'correction') THEN
            RETURN NULL;
        END IF;
        IF NEW.family = 'system_correction' THEN
            SELECT count(*) INTO paired FROM identity.corrections correction
                JOIN identity.revisions revision ON revision.id = correction.operator_id
                WHERE correction.operation_id = NEW.operation_id
                    AND correction.person_id = NEW.target::uuid
                    AND correction.operator_id = NEW.revision_id
                    AND revision.person_id = NEW.person_id
                    AND correction.corrected_at = NEW.created_at
                    AND correction.transaction_id = NEW.transaction_id
                    AND identity.administered(correction.appointment_id);
            IF paired <> 1 OR EXISTS (SELECT 1 FROM identity.corrections correction
                WHERE correction.operation_id = NEW.operation_id
                    AND NOT identity.administered(correction.appointment_id))
                OR EXISTS (SELECT 1 FROM identity.completions completion
                    WHERE completion.operation_id = NEW.operation_id AND completion.family = 'correction') THEN
                RAISE EXCEPTION 'system correction completion requires its same-transaction correction'
                    USING ERRCODE = '23514';
            END IF;
        ELSE
            SELECT count(*) INTO paired FROM identity.corrections correction
                JOIN identity.revisions revision ON revision.id = correction.operator_id
                WHERE correction.operation_id = NEW.operation_id
                    AND correction.person_id = NEW.target::uuid
                    AND correction.operator_id = NEW.revision_id
                    AND revision.person_id = NEW.person_id
                    AND correction.corrected_at = NEW.created_at
                    AND correction.transaction_id = NEW.transaction_id
                    AND NOT identity.administered(correction.appointment_id);
            IF paired <> 1 OR EXISTS (SELECT 1 FROM identity.corrections correction
                WHERE correction.operation_id = NEW.operation_id
                    AND identity.administered(correction.appointment_id))
                OR EXISTS (SELECT 1 FROM identity.completions completion
                    WHERE completion.operation_id = NEW.operation_id AND completion.family = 'system_correction') THEN
                RAISE EXCEPTION 'online correction completion requires exactly its same-transaction correction'
                    USING ERRCODE = '23514';
            END IF;
        END IF;
        RETURN NULL;
    END IF;
    SELECT appointment_id, operator_id, person_id, corrected_at, operation_id, transaction_id, sequence
        INTO STRICT original FROM identity.corrections WHERE id = NEW.id;
    IF NOT EXISTS (SELECT 1 FROM identity.persons person
        WHERE person.id = original.person_id AND person.sequence >= original.sequence)
        OR (SELECT count(*) FROM identity.changes record
            WHERE record.person_id = original.person_id AND record.sequence = original.sequence) <> 1 THEN
        RAISE EXCEPTION 'correction requires its unique position in the advanced person state'
            USING ERRCODE = '23514';
    END IF;
    IF identity.administered(original.appointment_id) THEN
        SELECT count(*) INTO paired FROM identity.completions completion
            JOIN identity.revisions revision ON revision.id = original.operator_id
            WHERE completion.family = 'system_correction'
                AND completion.operation_id = original.operation_id
                AND completion.action = 'correct'
                AND completion.subject = 'system'
                AND completion.target = original.person_id::text
                AND completion.person_id = revision.person_id
                AND completion.revision_id = original.operator_id
                AND completion.created_at = original.corrected_at
                AND completion.transaction_id = original.transaction_id
                AND completion.expires_at IS NULL;
        IF paired <> 1 OR EXISTS (SELECT 1 FROM identity.completions
            WHERE operation_id = original.operation_id AND family = 'correction') THEN
            RAISE EXCEPTION 'system correction requires its same-transaction completion'
                USING ERRCODE = '23514';
        END IF;
    ELSE
        SELECT count(*) INTO paired FROM identity.completions completion
            JOIN identity.revisions revision ON revision.id = original.operator_id
            WHERE completion.family = 'correction'
                AND completion.operation_id = original.operation_id
                AND completion.action = 'correct'
                AND completion.subject = 'session'
                AND completion.target = original.person_id::text
                AND completion.person_id = revision.person_id
                AND completion.revision_id = original.operator_id
                AND completion.created_at = original.corrected_at
                AND completion.transaction_id = original.transaction_id
                AND completion.expires_at IS NULL;
        IF paired <> 1 OR EXISTS (SELECT 1 FROM identity.completions
            WHERE operation_id = original.operation_id AND family = 'system_correction') THEN
            RAISE EXCEPTION 'online correction requires exactly its same-transaction completion'
                USING ERRCODE = '23514';
        END IF;
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER corrections_binding_check AFTER INSERT ON identity.corrections
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_correction_binding();
CREATE CONSTRAINT TRIGGER completions_correction_binding_check AFTER INSERT ON identity.completions
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW
    WHEN (NEW.family IN ('system_correction', 'correction'))
    EXECUTE FUNCTION identity.check_correction_binding();

CREATE FUNCTION access.reject_history_mutation() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    RAISE EXCEPTION 'permanent identity and access facts are immutable' USING ERRCODE = '23514';
END
$$;
CREATE TRIGGER revisions_immutable_check BEFORE UPDATE OR DELETE ON identity.revisions
    FOR EACH ROW EXECUTE FUNCTION access.reject_history_mutation();
CREATE TRIGGER corrections_immutable_check BEFORE UPDATE OR DELETE ON identity.corrections
    FOR EACH ROW EXECUTE FUNCTION access.reject_history_mutation();
CREATE TRIGGER completions_permanent_delete_check BEFORE DELETE ON identity.completions
    FOR EACH ROW WHEN (OLD.family IN ('system_correction', 'correction', 'cancellation'))
    EXECUTE FUNCTION access.reject_history_mutation();
CREATE TRIGGER completions_cancellation_update_check BEFORE UPDATE ON identity.completions
    FOR EACH ROW WHEN (OLD.family = 'cancellation' OR NEW.family = 'cancellation')
    EXECUTE FUNCTION access.reject_history_mutation();
CREATE TRIGGER cancellations_immutable_check BEFORE UPDATE OR DELETE ON identity.cancellations
    FOR EACH ROW EXECUTE FUNCTION access.reject_history_mutation();

CREATE FUNCTION identity.check_cancellation_binding() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE paired integer;
BEGIN
    SELECT count(*) INTO paired FROM identity.cancellations cancellation
        JOIN identity.revisions operator ON operator.id = cancellation.operator_id
        JOIN identity.persons person ON person.id = cancellation.person_id
            AND person.sequence >= cancellation.sequence
        JOIN identity.completions completion
            ON completion.family = 'cancellation'
                AND completion.operation_id = cancellation.operation_id
        WHERE cancellation.operation_id = NEW.operation_id
            AND completion.action = CASE WHEN operator.person_id = cancellation.person_id
                THEN 'self' ELSE 'managed' END
            AND completion.subject = 'session'
            AND completion.target = cancellation.account_id::text
            AND completion.person_id = operator.person_id
            AND completion.revision_id = cancellation.operator_id
            AND completion.created_at = cancellation.cancelled_at
            AND completion.expires_at IS NULL
            AND (SELECT count(*) FROM identity.changes record
                WHERE record.person_id = cancellation.person_id
                    AND record.sequence = cancellation.sequence) = 1
            AND completion.transaction_id = cancellation.transaction_id;
    IF paired <> 1 THEN
        RAISE EXCEPTION 'cancellation and its permanent completion require the same transaction and origin'
            USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER cancellations_binding_check AFTER INSERT ON identity.cancellations
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_cancellation_binding();
CREATE CONSTRAINT TRIGGER completions_cancellation_binding_check AFTER INSERT ON identity.completions
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW WHEN (NEW.family = 'cancellation')
    EXECUTE FUNCTION identity.check_cancellation_binding();

-- The Person record family: the origin, corrections and cancellation records share one
-- object-local sequence. Loaders select one event family before decoding a record.
CREATE VIEW identity.changes AS
    SELECT origin.person_id, origin.sequence, origin.operation_id, origin.created_at AS happened_at,
        'origin'::text AS kind, origin.revision_id AS id, origin.revision_id, NULL::uuid AS before_id,
        origin.revision_id AS operator_id, NULL::uuid AS appointment_id,
        origin.account_id, NULL::bytea AS mobile,
        EXISTS (SELECT 1 FROM identity.creations creation
                JOIN identity.revisions revision ON revision.id = origin.revision_id
                    AND revision.person_id = origin.person_id
                WHERE creation.id = origin.account_id AND creation.person_id = origin.person_id
                    AND creation.revision_id = origin.revision_id AND creation.operation_id = origin.operation_id
                    AND creation.created_at = origin.created_at
                    AND creation.transaction_id = origin.transaction_id
                    AND revision.transaction_id = origin.transaction_id) AS paired,
        origin.seal, origin.seal_key_id, origin.seal_version,
        origin.transaction_id
    FROM identity.origins origin
    UNION ALL
    SELECT correction.person_id, correction.sequence, correction.operation_id, correction.corrected_at,
        'correction', correction.id, correction.id, correction.before_id, correction.operator_id, correction.appointment_id,
        NULL::uuid, NULL::bytea,
        EXISTS (SELECT 1 FROM identity.revisions revision
                WHERE revision.id = correction.id AND revision.person_id = correction.person_id
                    AND revision.transaction_id = correction.transaction_id),
        correction.seal, correction.seal_key_id, correction.seal_version,
        correction.transaction_id
    FROM identity.corrections correction
    UNION ALL
    SELECT cancellation.person_id, cancellation.sequence, cancellation.operation_id, cancellation.cancelled_at,
        'cancellation', cancellation.id, cancellation.revision_id, NULL::uuid, cancellation.operator_id, NULL::uuid,
        cancellation.account_id, cancellation.mobile,
        EXISTS (SELECT 1 FROM identity.revisions revision
                WHERE revision.id = cancellation.revision_id AND revision.person_id = cancellation.person_id),
        cancellation.seal, cancellation.seal_key_id, cancellation.seal_version,
        cancellation.transaction_id
    FROM identity.cancellations cancellation;

-- A new immutable value belongs to exactly one origin or correction in the same transaction.
CREATE FUNCTION identity.check_revision_source() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
BEGIN
    IF (SELECT count(*) FROM identity.changes source
        WHERE source.kind IN ('origin', 'correction')
            AND source.person_id = NEW.person_id AND source.revision_id = NEW.id
            AND source.transaction_id = NEW.transaction_id AND source.paired) <> 1 THEN
        RAISE EXCEPTION 'real-name revision requires its same-transaction source'
            USING ERRCODE = '23514', CONSTRAINT = 'revisions_source_check';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER revisions_source_check AFTER INSERT ON identity.revisions
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_revision_source();

-- Every Person advancement pairs its new contiguous records and exact resulting head.
CREATE FUNCTION identity.check_person() RETURNS trigger
LANGUAGE plpgsql VOLATILE SET search_path = '' AS $$
DECLARE previous_sequence bigint; previous_version bigint;
BEGIN
    previous_sequence := CASE WHEN TG_OP = 'INSERT' THEN 0 ELSE OLD.sequence END;
    previous_version := CASE WHEN TG_OP = 'INSERT' THEN 0 ELSE OLD.version END;
    IF NEW.sequence <= previous_sequence OR NEW.version <> previous_version + 1
        OR (TG_OP = 'UPDATE' AND NEW.id <> OLD.id)
        OR (TG_OP = 'INSERT' AND NOT EXISTS (
            SELECT 1 FROM identity.origins origin
            WHERE origin.person_id = NEW.id AND origin.revision_id = NEW.revision_id
                AND origin.sequence = 1 AND origin.transaction_id = pg_catalog.pg_current_xact_id()))
        OR EXISTS (
            SELECT 1 FROM pg_catalog.generate_series(previous_sequence + 1, NEW.sequence) expected(sequence)
            WHERE (SELECT count(*) FROM identity.changes record
                WHERE record.person_id = NEW.id AND record.sequence = expected.sequence
                    AND record.transaction_id = pg_catalog.pg_current_xact_id()
                    AND record.paired) <> 1)
        OR NOT EXISTS (SELECT 1 FROM identity.changes head
            WHERE head.person_id = NEW.id AND head.sequence = NEW.sequence
                AND head.revision_id = NEW.revision_id AND head.happened_at = NEW.changed_at) THEN
        RAISE EXCEPTION 'person advancement requires its contiguous records and paired head'
            USING ERRCODE = '23514', CONSTRAINT = 'persons_records_check';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER persons_records_check AFTER INSERT OR UPDATE ON identity.persons
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.check_person();

REVOKE ALL ON FUNCTION identity.check_cancellation_binding() FROM PUBLIC;
REVOKE ALL ON SCHEMA access FROM PUBLIC;
REVOKE ALL ON ALL TABLES IN SCHEMA access FROM PUBLIC;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA access FROM PUBLIC;
REVOKE ALL ON identity.avatars FROM PUBLIC;
REVOKE ALL ON FUNCTION identity.check_avatar(), identity.check_avatar_transition(), identity.check_avatar_deletion() FROM PUBLIC;
REVOKE ALL ON FUNCTION identity.check_cancellation_deletion(), identity.check_cancellation_completion() FROM PUBLIC;
REVOKE ALL ON identity.corrections, identity.cancellations, identity.origins, identity.changes FROM PUBLIC;
REVOKE ALL ON FUNCTION identity.administered(uuid) FROM PUBLIC;
REVOKE ALL ON FUNCTION identity.check_correction_binding() FROM PUBLIC;
REVOKE ALL ON FUNCTION identity.check_person() FROM PUBLIC;
REVOKE ALL ON FUNCTION identity.check_revision_source() FROM PUBLIC;
