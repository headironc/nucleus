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
    status text NOT NULL DEFAULT 'submitted' CHECK (status = 'submitted'),
    submitted_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
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

-- Runtime grants are append-only; these triggers also protect retained facts from accidental
-- writes through a maintenance identity. They do not claim to detect database tampering.
CREATE FUNCTION workforce.reject_application_mutation() RETURNS trigger
LANGUAGE plpgsql SET search_path = '' AS $$
BEGIN
    RAISE EXCEPTION 'retained badge facts cannot be changed'
        USING ERRCODE = '23514', CONSTRAINT = 'applications_immutable_check';
END
$$;
CREATE TRIGGER applications_immutable BEFORE UPDATE OR DELETE ON workforce.applications
    FOR EACH ROW EXECUTE FUNCTION workforce.reject_application_mutation();
CREATE TRIGGER photos_immutable BEFORE UPDATE OR DELETE ON workforce.photos
    FOR EACH ROW EXECUTE FUNCTION workforce.reject_application_mutation();
CREATE TRIGGER resources_immutable BEFORE UPDATE OR DELETE ON workforce.resources
    FOR EACH ROW EXECUTE FUNCTION workforce.reject_application_mutation();
CREATE TRIGGER audit_immutable BEFORE UPDATE OR DELETE ON audit.events
    FOR EACH ROW EXECUTE FUNCTION workforce.reject_application_mutation();
