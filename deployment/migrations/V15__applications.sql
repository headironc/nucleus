-- Immutable badge resources are prepared before their database references are accepted.
CREATE TABLE workforce.resources (
    id uuid PRIMARY KEY,
    kind text NOT NULL CHECK (kind IN ('photo', 'background', 'font')),
    media_type text NOT NULL,
    size bigint NOT NULL CHECK (size > 0),
    CHECK ((kind IN ('photo', 'background') AND media_type = 'image/webp')
        OR (kind = 'font' AND media_type IN ('font/ttf', 'font/otf'))),
    CHECK (size <= CASE WHEN kind = 'font' THEN 33554432 ELSE 68157440 END),
    UNIQUE (id, kind)
);

CREATE TABLE workforce.photos (
    id uuid PRIMARY KEY,
    person_id uuid NOT NULL REFERENCES identity.persons (id),
    resource_id uuid NOT NULL UNIQUE,
    resource_kind text GENERATED ALWAYS AS ('photo'::text) STORED,
    width integer NOT NULL CHECK (width BETWEEN 1 AND 4096),
    height integer NOT NULL CHECK (height BETWEEN 1 AND 4096),
    CHECK (width::bigint * height <= 16000000),
    FOREIGN KEY (resource_id, resource_kind) REFERENCES workforce.resources (id, kind),
    UNIQUE (id, person_id)
);

CREATE TABLE workforce.applications (
    id uuid PRIMARY KEY,
    person_id uuid NOT NULL REFERENCES identity.persons (id),
    kind text NOT NULL CHECK (kind IN ('issuance', 'duplication', 'replacement')),
    status text NOT NULL DEFAULT 'submitted' CHECK (status IN ('submitted', 'approved', 'rejected', 'ready', 'cancelled')),
    submitted_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    reviewer_id uuid REFERENCES identity.persons (id),
    reviewed_at timestamptz,
    rejection_reason text,
    render_error text CHECK (render_error IN ('missing_glyph', 'text_overflow', 'dependency_unavailable')),
    CHECK (status = 'approved' OR render_error IS NULL),
    CONSTRAINT applications_review_check CHECK (
        (status = 'submitted' AND reviewer_id IS NULL AND reviewed_at IS NULL AND rejection_reason IS NULL)
        OR (status IN ('approved', 'ready', 'rejected') AND reviewer_id IS NOT NULL
            AND reviewed_at IS NOT NULL AND reviewed_at >= submitted_at
            AND ((status IN ('approved', 'ready') AND rejection_reason IS NULL)
                OR (status = 'rejected' AND rejection_reason IS NOT NULL
                    AND length(btrim(rejection_reason, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000')) > 0)))
        OR (status = 'cancelled' AND rejection_reason IS NULL
            AND ((reviewer_id IS NULL AND reviewed_at IS NULL)
                OR (reviewer_id IS NOT NULL AND reviewed_at IS NOT NULL AND reviewed_at >= submitted_at)))),
    snapshot jsonb NOT NULL,
    -- Projection columns give the JSON's one owned reference real database protection.
    department_id uuid GENERATED ALWAYS AS ((snapshot->'department'->>'id')::uuid) STORED NOT NULL
        REFERENCES workforce.departments (id),
    roster_id uuid GENERATED ALWAYS AS ((snapshot->'position'->>'rosterId')::uuid) STORED
        REFERENCES workforce.rosters (id),
    title_id uuid GENERATED ALWAYS AS ((snapshot->'title'->>'id')::uuid) STORED
        REFERENCES workforce.titles (id),
    photo_id uuid GENERATED ALWAYS AS ((snapshot->>'photoId')::uuid) STORED NOT NULL,
    background_id uuid GENERATED ALWAYS AS ((snapshot->'template'->>'backgroundId')::uuid) STORED NOT NULL,
    background_kind text GENERATED ALWAYS AS ('background'::text) STORED,
    font_id uuid GENERATED ALWAYS AS ((snapshot->'template'->>'fontId')::uuid) STORED NOT NULL,
    font_kind text GENERATED ALWAYS AS ('font'::text) STORED,
    CONSTRAINT applications_roster_department_fkey FOREIGN KEY (roster_id, department_id)
        REFERENCES workforce.rosters (id, department_id),
    FOREIGN KEY (photo_id, person_id) REFERENCES workforce.photos (id, person_id),
    FOREIGN KEY (background_id, background_kind) REFERENCES workforce.resources (id, kind),
    FOREIGN KEY (font_id, font_kind) REFERENCES workforce.resources (id, kind),
    CONSTRAINT applications_snapshot_check CHECK (COALESCE(
        jsonb_typeof(snapshot) = 'object'
        AND snapshot ?& ARRAY['name','number','printedNumber','department','position','title','printedTitle','photoId','template','pickup','text']
        AND jsonb_typeof(snapshot->'name') = 'string' AND length(snapshot->>'name') > 0
        AND jsonb_typeof(snapshot->'number') = 'object'
        AND jsonb_typeof(snapshot->'number'->'subtype') = 'number'
        AND (snapshot->'number'->>'subtype')::smallint IN (100,200,300,400,500,600,700,801,802,803,804,901,902,903,904,1000)
        AND jsonb_typeof(snapshot->'number'->'year') = 'number'
        AND (snapshot->'number'->>'year')::integer >= 1
        AND jsonb_typeof(snapshot->'number'->'serial') = 'number'
        AND (snapshot->'number'->>'serial')::smallint BETWEEN 1 AND 999
        AND jsonb_typeof(snapshot->'printedNumber') = 'string' AND length(snapshot->>'printedNumber') > 0
        AND jsonb_typeof(snapshot->'department') = 'object'
        AND jsonb_typeof(snapshot->'department'->'name') = 'string' AND length(snapshot->'department'->>'name') > 0
        AND jsonb_typeof(snapshot->'department'->'printed') = 'string' AND length(snapshot->'department'->>'printed') > 0
        AND (snapshot->'position' = 'null'::jsonb OR (jsonb_typeof(snapshot->'position') = 'object'
            AND roster_id IS NOT NULL AND jsonb_typeof(snapshot->'position'->'name') = 'string'))
        AND (snapshot->'title' = 'null'::jsonb OR (jsonb_typeof(snapshot->'title') = 'object'
            AND title_id IS NOT NULL AND jsonb_typeof(snapshot->'title'->'name') = 'string'))
        AND jsonb_typeof(snapshot->'printedTitle') = 'string' AND length(snapshot->>'printedTitle') > 0
        AND jsonb_typeof(snapshot->'template') = 'object'
        AND jsonb_typeof(snapshot->'template'->'layout') = 'object'
        AND snapshot->'template'->'layout' ?& ARRAY['widthUm','heightUm','photo','name','number','department','title']
        AND jsonb_typeof(snapshot->'text') = 'object'
        AND jsonb_typeof(snapshot->'text'->'name') = 'array'
        AND jsonb_typeof(snapshot->'text'->'number') = 'array'
        AND jsonb_typeof(snapshot->'text'->'department') = 'array'
        AND jsonb_typeof(snapshot->'text'->'title') = 'array'
        AND jsonb_typeof(snapshot->'pickup') = 'string' AND length(btrim(snapshot->>'pickup')) > 0,
        false))
);
CREATE INDEX applications_person_id_submitted_at_id_idx
    ON workforce.applications (person_id, submitted_at DESC, id DESC);
CREATE INDEX applications_status_submitted_at_id_idx
    ON workforce.applications (status, submitted_at DESC, id DESC);

CREATE TABLE workforce.badges (
    id uuid PRIMARY KEY,
    application_id uuid NOT NULL UNIQUE REFERENCES workforce.applications (id),
    generated_by uuid NOT NULL REFERENCES identity.persons (id),
    generated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    size bigint NOT NULL CHECK (size BETWEEN 1 AND 16777216)
);

CREATE FUNCTION workforce.admit_badge() RETURNS trigger
LANGUAGE plpgsql SET search_path = '' AS $$
DECLARE stage text;
BEGIN
    SELECT status INTO stage FROM workforce.applications WHERE id = NEW.application_id FOR SHARE;
    IF stage IS DISTINCT FROM 'ready' THEN
        RAISE EXCEPTION 'a new badge requires the ready transition'
            USING ERRCODE = '23514', CONSTRAINT = 'badges_admission_check';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER badges_admission BEFORE INSERT ON workforce.badges
    FOR EACH ROW EXECUTE FUNCTION workforce.admit_badge();

-- Adoption spans two tables; validate their final state at the transaction boundary.
CREATE FUNCTION workforce.check_badge_pair() RETURNS trigger
LANGUAGE plpgsql SET search_path = '' AS $$
DECLARE
    application uuid;
    stage text;
    present boolean;
BEGIN
    IF TG_TABLE_NAME = 'badges' THEN application := NEW.application_id;
    ELSE application := NEW.id;
    END IF;
    SELECT status INTO stage FROM workforce.applications WHERE id = application;
    SELECT EXISTS (SELECT 1 FROM workforce.badges WHERE application_id = application) INTO present;
    IF (stage = 'ready' AND NOT present) OR (present AND stage NOT IN ('ready', 'cancelled')) THEN
        RAISE EXCEPTION 'application and adopted badge must agree'
            USING ERRCODE = '23514', CONSTRAINT = 'applications_badge_check';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER applications_badge_pair AFTER INSERT OR UPDATE ON workforce.applications
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION workforce.check_badge_pair();
CREATE CONSTRAINT TRIGGER badges_application_pair AFTER INSERT ON workforce.badges
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION workforce.check_badge_pair();

CREATE SCHEMA audit;
CREATE TABLE audit.events (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    actor_id uuid NOT NULL REFERENCES identity.persons (id),
    occurred_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    action text NOT NULL CHECK (length(action) > 0),
    target_type text NOT NULL CHECK (length(target_type) > 0),
    target_id uuid NOT NULL,
    details jsonb NOT NULL CHECK (jsonb_typeof(details) = 'object')
);

-- Retained facts also remain protected against accidental maintenance writes.
CREATE FUNCTION workforce.protect_application() RETURNS trigger
LANGUAGE plpgsql SET search_path = '' AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.status <> 'submitted' THEN
            RAISE EXCEPTION 'applications start awaiting review'
                USING ERRCODE = '23514', CONSTRAINT = 'applications_transition_check';
        END IF;
        RETURN NEW;
    END IF;
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'applications must be retained'
            USING ERRCODE = '23514', CONSTRAINT = 'applications_immutable_check';
    END IF;
    IF ROW(NEW.id, NEW.person_id, NEW.kind, NEW.submitted_at, NEW.snapshot)
            IS DISTINCT FROM ROW(OLD.id, OLD.person_id, OLD.kind, OLD.submitted_at, OLD.snapshot) THEN
        RAISE EXCEPTION 'fixed application facts cannot be changed'
            USING ERRCODE = '23514', CONSTRAINT = 'applications_transition_check';
    END IF;
    IF OLD.status = 'submitted' AND NEW.status IN ('approved', 'rejected') THEN
        RETURN NEW;
    END IF;
    IF OLD.status = 'approved' AND NEW.status IN ('approved', 'ready')
        AND ROW(NEW.reviewer_id, NEW.reviewed_at, NEW.rejection_reason)
            IS NOT DISTINCT FROM ROW(OLD.reviewer_id, OLD.reviewed_at, OLD.rejection_reason) THEN
        RETURN NEW;
    END IF;
    IF OLD.status IN ('submitted', 'approved', 'ready') AND NEW.status = 'cancelled'
        AND ROW(NEW.reviewer_id, NEW.reviewed_at, NEW.rejection_reason)
            IS NOT DISTINCT FROM ROW(OLD.reviewer_id, OLD.reviewed_at, OLD.rejection_reason) THEN
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'only one review or cancellation may change an application'
        USING ERRCODE = '23514', CONSTRAINT = 'applications_transition_check';
END
$$;
CREATE TRIGGER applications_immutable BEFORE INSERT OR UPDATE OR DELETE ON workforce.applications
    FOR EACH ROW EXECUTE FUNCTION workforce.protect_application();

-- These triggers also protect retained facts from accidental
-- writes through a maintenance identity. They do not claim to detect database tampering.
CREATE FUNCTION workforce.reject_application_mutation() RETURNS trigger
LANGUAGE plpgsql SET search_path = '' AS $$
BEGIN
    RAISE EXCEPTION 'retained badge facts cannot be changed'
        USING ERRCODE = '23514', CONSTRAINT = 'applications_immutable_check';
END
$$;
CREATE TRIGGER photos_immutable BEFORE UPDATE OR DELETE ON workforce.photos
    FOR EACH ROW EXECUTE FUNCTION workforce.reject_application_mutation();
CREATE TRIGGER resources_immutable BEFORE UPDATE OR DELETE ON workforce.resources
    FOR EACH ROW EXECUTE FUNCTION workforce.reject_application_mutation();
CREATE TRIGGER audit_immutable BEFORE UPDATE OR DELETE ON audit.events
    FOR EACH ROW EXECUTE FUNCTION workforce.reject_application_mutation();
CREATE TRIGGER badges_immutable BEFORE UPDATE OR DELETE ON workforce.badges
    FOR EACH ROW EXECUTE FUNCTION workforce.reject_application_mutation();
