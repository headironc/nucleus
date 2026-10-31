//! Independent retained facts around real badge submissions and read-only failure windows.

use std::{collections::BTreeMap, io::Cursor, path::Path, time::Duration};

use domain::workforce::application::Snapshot;
use platform::files::Directory;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_postgres::Client;
use uuid::Uuid;

pub(super) fn settings() -> (domain::workforce::application::Settings, Directory) {
    let storage = crate::database::avatar_storage();
    let directory = storage.open().expect("badge observation directory");
    let mut background = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        850,
        540,
        image::Rgb([245, 250, 246]),
    ))
    .write_to(&mut background, image::ImageFormat::Png)
    .expect("fictional light background");
    let mut template = serde_json::to_value(domain::workforce::application::fixture_template())
        .expect("fixture layout");
    template["fontId"] = json!(Uuid::new_v4());
    let settings = domain::workforce::application::Settings::new(
        storage,
        serde_json::from_value(template).expect("legal template"),
        "image/png",
        background.into_inner(),
        include_bytes!("../../../domain/tests/fixtures/fonts/NotoSansSC-Regular.otf").to_vec(),
        vec!["Fixture reception".to_owned()],
        domain::workforce::application::photo::Limits::new(4, 1 << 30, 8, 2 << 30)
            .expect("isolated photo limits"),
        domain::workforce::application::render::Resolution::try_from(300)
            .expect("fixture resolution"),
    )
    .expect("licensed CJK badge fixture");
    (settings, directory)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    account_id: Uuid,
    person_id: Uuid,
    session: Uuid,
    operation: Option<Uuid>,
    input: Option<Value>,
    application: Option<Value>,
    application_ids: Vec<Uuid>,
    resource_id: Option<Uuid>,
}

pub(super) struct Observer {
    directory: Directory,
    step: usize,
    owner: Option<(Uuid, Uuid, Uuid)>,
    original: Option<Request>,
    unchanged: Option<String>,
    applications: BTreeMap<Uuid, Value>,
    photos: BTreeMap<Uuid, Vec<u8>>,
}

impl Observer {
    pub(super) fn new(directory: Directory) -> Self {
        Self {
            directory,
            step: 0,
            owner: None,
            original: None,
            unchanged: None,
            applications: BTreeMap::new(),
            photos: BTreeMap::new(),
        }
    }

    pub(super) fn complete(&self) -> bool {
        self.step == 16
    }

    pub(super) fn verify_resources(&self) {
        assert!(self.complete(), "submission observations precede reviews");
        let (_, person, _) = self.owner.expect("bound photo owner");
        for (id, original) in &self.photos {
            let bytes = self
                .directory
                .open(
                    &["workforce", "photos", &person.to_string()],
                    &id.to_string(),
                )
                .expect("retained private directory")
                .expect("retained photo")
                .read(original.len() as u64)
                .expect("retained photo bytes");
            assert!(
                bytes == *original,
                "reviews retain exact original photo bytes"
            );
        }
        let usage = self
            .directory
            .exclusive(&["workforce", "photos"])
            .expect("photo accounting lock")
            .usage(&[])
            .expect("retained photo accounting");
        assert_eq!(usage.files, 4, "reviews prepare no extra photos");
    }

    pub(super) fn original(&self, id: Uuid) -> &Value {
        assert!(self.complete(), "complete submissions bind review targets");
        self.applications
            .get(&id)
            .expect("an original submitted application")
    }

    pub(super) fn evidence(&self) -> Value {
        let (account, person, _) = self.owner.expect("bound badge owner");
        json!({"accountId":account,"personId":person,"phases":self.step,
            "applications":self.applications.keys().collect::<Vec<_>>(),"photos":self.photos.len(),
            "submittedKinds":self.applications.values().map(|application| application["kind"].clone()).collect::<Vec<_>>(),
            "snapshotsAndAuditPaired":self.complete(),"lostResponseAndReadsUnchanged":self.complete(),
            "foreignCallerBound":self.complete(),"capacityRejectedWithoutWrites":self.complete(),
            "restrictedResourcesRetained":self.complete()})
    }

    pub(super) async fn observe(&mut self, client: &Client, material: &Path) -> bool {
        let phases = [
            "issuance",
            "duplication",
            "replacement",
            "lost",
            "refresh",
            "foreign",
            "capacity",
            "restricted",
        ];
        let Some(phase) = phases.get(self.step / 2) else {
            return false;
        };
        let position = if self.step.is_multiple_of(2) {
            "before"
        } else {
            "after"
        };
        let path = material.join(format!("badge-{phase}-{position}"));
        if !path.with_extension("request").exists() {
            return false;
        }
        let wire: Value = serde_json::from_slice(
            &std::fs::read(path.with_extension("request")).expect("badge observation request"),
        )
        .expect("badge observation JSON");
        for field in [
            "accountId",
            "personId",
            "session",
            "operation",
            "input",
            "application",
            "applicationIds",
            "resourceId",
        ] {
            assert!(
                wire.get(field).is_some(),
                "badge observation fields are explicit"
            );
        }
        let request: Request =
            serde_json::from_value(wire).expect("strict badge observation binding");
        if self.owner.is_none() {
            let allocation: Value = serde_json::from_slice(
                &std::fs::read(material.join("allocation-current-after.request"))
                    .expect("completed first-number observation"),
            )
            .expect("first-number binding");
            let parse = |field| {
                allocation[field]
                    .as_str()
                    .expect("first-number identity")
                    .parse::<Uuid>()
                    .expect("identity UUID")
            };
            self.owner = Some((parse("accountId"), parse("personId"), parse("session")));
        }
        tokio::time::timeout(
            Duration::from_secs(3),
            self.capture(client, &request, phase, position),
        )
        .await
        .expect("bounded badge observation");
        if position == "before" {
            self.original = Some(request);
        } else {
            self.original = None;
        }
        std::fs::write(path.with_extension("ack"), "ok")
            .expect("badge observation acknowledgement");
        self.step += 1;
        true
    }

    async fn capture(&mut self, client: &Client, request: &Request, phase: &str, position: &str) {
        let (account, person, session) = self.owner.expect("original member binding");
        if phase == "foreign" {
            assert!(
                request.account_id != account
                    && request.person_id != person
                    && request.session != session,
                "foreign caller has a distinct complete identity"
            );
        } else {
            assert!(
                (request.account_id, request.person_id, request.session)
                    == (account, person, session),
                "all own phases retain the admitted member"
            );
        }
        let count = if self.step < 8 {
            self.step.div_ceil(2)
        } else {
            4
        };
        let photo_count = if self.step < 8 { self.step / 2 + 1 } else { 4 };
        assert_eq!(request.application_ids.len(), count);
        client
            .batch_execute(
                "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY; SET LOCAL statement_timeout='3s'",
            )
            .await
            .expect("read-only badge window");
        let active: bool = client.query_one(
            "SELECT EXISTS(SELECT 1 FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id
             JOIN access.grants grant_record ON grant_record.person_id=account.person_id JOIN access.roles role ON role.id=grant_record.role_id
             WHERE session.id=$1 AND account.id=$2 AND account.person_id=$3 AND session.revoked_at IS NULL AND session.expires_at>clock_timestamp()
             AND account.suspension_id IS NULL AND account.cancellation_id IS NULL AND role.scope='workforce'
             AND grant_record.suspension_id IS NULL AND ((grant_record.termination IS NULL)=$4)
             AND (($5 AND role.level=100 AND role.permissions::text[]=ARRAY['workforce.directory']) OR (NOT $5 AND role.level=1 AND cardinality(role.permissions)=0)))",
            &[&request.session,&request.account_id,&request.person_id,&(phase != "restricted"),&(phase == "foreign")],
        ).await.expect("independent caller and participation").get(0);
        assert!(
            active,
            "caller, live session and actual role match the phase"
        );
        let number: String = client
            .query_one(
                "SELECT to_jsonb(number)::text FROM workforce.numbers WHERE id=$1",
                &[&person],
            )
            .await
            .expect("independent original member number")
            .get(0);
        let number: Value = serde_json::from_str(&number).expect("complete current number");
        let rows = client.query(
            "SELECT application.id,application.person_id,application.kind,application.status,application.submitted_at,application.snapshot::text,
             photo.id AS photo_id,photo.resource_id,photo.width,photo.height,resource.size,resource.media_type,
             (SELECT count(*) FROM audit.events event WHERE event.action='workforce.application.submitted' AND event.target_type='workforce.application'
              AND event.target_id=application.id AND event.actor_id=application.person_id AND event.occurred_at=application.submitted_at AND event.details='{}'::jsonb) AS audits
             FROM workforce.applications application JOIN workforce.photos photo ON photo.id=application.photo_id AND photo.person_id=application.person_id
             JOIN workforce.resources resource ON resource.id=photo.resource_id AND resource.kind='photo' ORDER BY application.submitted_at,application.id", &[],
        ).await.expect("independent retained applications");
        assert_eq!(rows.len(), count);
        let mut current = BTreeMap::new();
        for row in rows {
            let id: Uuid = row.get("id");
            assert!(request.application_ids.contains(&id));
            assert_eq!(row.get::<_, Uuid>("person_id"), person);
            assert_eq!(row.get::<_, &str>("status"), "submitted");
            assert_eq!(row.get::<_, i64>("audits"), 1);
            let snapshot: Value =
                serde_json::from_str(row.get("snapshot")).expect("stored printable snapshot");
            assert_eq!(snapshot["name"], "Admission Colleague");
            assert_eq!(snapshot["number"]["subtype"], 902);
            assert_eq!(snapshot["number"], number);
            assert!(snapshot["position"].is_null() && snapshot["title"].is_null());
            assert_eq!(snapshot["pickup"], "Fixture reception");
            let resource: Uuid = row.get("resource_id");
            let photo = json!({"photoId":row.get::<_,Uuid>("photo_id"),"resourceId":resource,"width":row.get::<_,i32>("width"),"height":row.get::<_,i32>("height"),"mediaType":row.get::<_,&str>("media_type")});
            let value = json!({"id":id,"kind":row.get::<_,&str>("kind"),"status":"submitted","snapshot":snapshot,"photo":photo});
            if let Some(original) = self.applications.get(&id) {
                assert_eq!(&value, original, "retained applications never change");
            }
            if request
                .application
                .as_ref()
                .is_some_and(|application| application["id"] == id.to_string())
            {
                let application = request
                    .application
                    .as_ref()
                    .expect("actual creation response");
                for field in ["id", "kind", "status", "photo"] {
                    assert_eq!(application[field], value[field]);
                }
                // JSON.stringify may encode an integral layout coordinate without `.0`.
                // Compare the exact protocol values rather than their numeric spelling.
                let actual: Snapshot = serde_json::from_value(application["snapshot"].clone())
                    .expect("response snapshot values");
                let stored: Snapshot = serde_json::from_value(value["snapshot"].clone())
                    .expect("stored snapshot values");
                assert_eq!(actual, stored);
                let submitted = chrono::DateTime::parse_from_rfc3339(
                    application["submittedAt"]
                        .as_str()
                        .expect("response timestamp"),
                )
                .expect("timestamp")
                .with_timezone(&chrono::Utc);
                assert_eq!(
                    submitted,
                    row.get::<_, chrono::DateTime<chrono::Utc>>("submitted_at")
                );
            }
            current.insert(id, value);
        }
        assert!(
            self.applications.keys().all(|id| current.contains_key(id)),
            "no retained application disappears"
        );
        if self.step < 8 {
            let kind = if phase == "lost" { "issuance" } else { phase };
            let input = request.input.as_ref().expect("actual submission input");
            assert_eq!(input["kind"], kind);
            assert!(request.operation.is_some());
            if position == "after" {
                let before = self.original.as_ref().expect("before submission");
                assert!(
                    before.operation == request.operation && before.input == request.input,
                    "actual write retains its operation and confirmation"
                );
                let application = request
                    .application
                    .as_ref()
                    .expect("committed response before delivery");
                let id = application["id"]
                    .as_str()
                    .expect("application ID")
                    .parse::<Uuid>()
                    .expect("application UUID");
                assert!(
                    !self.applications.contains_key(&id) && current.contains_key(&id),
                    "one actual new application"
                );
                assert_eq!(application["snapshot"], input["expected"]);
                assert_eq!(application["kind"], kind);
                assert_eq!(current.len(), self.applications.len() + 1);
            } else {
                assert!(request.application.is_none());
            }
        } else {
            assert!(request.input.is_none() && request.application.is_none());
            assert_eq!(
                current, self.applications,
                "reads and rejected preparation add no applications"
            );
            if phase == "capacity" {
                assert!(request.operation.is_some());
            } else {
                assert!(request.operation.is_none());
            }
        }
        self.applications = current;
        let photos=client.query("SELECT photo.id,photo.resource_id,photo.width,photo.height,resource.size,
            (SELECT count(*) FROM audit.events event WHERE event.action='workforce.photo.prepared' AND event.target_type='workforce.photo' AND event.target_id=photo.id AND event.actor_id=photo.person_id AND event.details='{}'::jsonb) AS audits
            FROM workforce.photos photo JOIN workforce.resources resource ON resource.id=photo.resource_id WHERE photo.person_id=$1 ORDER BY photo.id", &[&person]).await.expect("retained photo references");
        assert_eq!(photos.len(), photo_count);
        for row in photos {
            assert_eq!(row.get::<_, i64>("audits"), 1);
            let id: Uuid = row.get("resource_id");
            let bytes = self
                .directory
                .open(
                    &["workforce", "photos", &person.to_string()],
                    &id.to_string(),
                )
                .expect("safe private photo")
                .expect("retained photo file")
                .read(u64::try_from(row.get::<_, i64>("size")).expect("positive size"))
                .expect("exact retained bytes");
            let image = image::load_from_memory(&bytes).expect("actual photo bytes decode");
            assert_eq!(
                (image.width(), image.height()),
                (
                    u32::try_from(row.get::<_, i32>("width")).expect("width"),
                    u32::try_from(row.get::<_, i32>("height")).expect("height")
                )
            );
            if let Some(original) = self.photos.get(&id) {
                assert!(
                    original == &bytes,
                    "original private photo bytes remain unchanged"
                );
            }
            self.photos.insert(id, bytes);
        }
        assert_eq!(self.photos.len(), photo_count);
        let physical = self
            .directory
            .exclusive(&["workforce", "photos"])
            .expect("physical photo accounting lock")
            .usage(&[])
            .expect("all retained photo files");
        assert_eq!(
            physical.files, photo_count as u64,
            "preparation refusal creates no extra physical file"
        );
        if let Some(id) = request.resource_id {
            assert!(
                self.photos.contains_key(&id),
                "foreign and restricted probes target an original owner photo"
            );
        }
        let counts=client.query_one("SELECT (SELECT count(*) FROM workforce.resources WHERE kind='photo'),(SELECT count(*) FROM audit.events WHERE action='workforce.photo.prepared'),(SELECT count(*) FROM audit.events WHERE action='workforce.application.submitted')",&[]).await.expect("independent global write counts");
        assert_eq!(counts.get::<_, i64>(0), photo_count as i64);
        assert_eq!(counts.get::<_, i64>(1), photo_count as i64);
        assert_eq!(counts.get::<_, i64>(2), count as i64);
        let unchanged = related_facts(client, &["workforce.application.submitted"]).await;
        if position == "before" {
            self.unchanged = Some(unchanged);
        } else {
            assert!(
                self.unchanged.take().as_ref() == Some(&unchanged),
                "badge writes and reads preserve identity, permissions, numbers, directories and photo facts"
            );
        }
        client
            .batch_execute("COMMIT")
            .await
            .expect("complete read-only badge window");
    }
}

pub(super) async fn related_facts(client: &Client, actions: &[&str]) -> String {
    client.query_one(
        "SELECT jsonb_build_object(
         'accounts',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM identity.accounts record),
         'persons',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM identity.persons record),
         'identities',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM identity.revisions record),
         'sessions',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM identity.sessions record),
         'revocations',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM identity.revocations record),
         'grants',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM access.grants record),
         'assignments',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM access.assignments record),
         'roles',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM access.roles record),
         'holders',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM access.holders record),
         'access_completions',(SELECT jsonb_agg(to_jsonb(record) ORDER BY family,operation_id) FROM access.completions record),
         'terminations',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM access.terminations record),
         'numbers',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM workforce.numbers record),
         'number_revisions',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM workforce.revisions record),
         'allocations',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM workforce.allocations record),
         'completions',(SELECT jsonb_agg(to_jsonb(record) ORDER BY family,operation_id) FROM workforce.completions record),
         'groups',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM workforce.groups record),
         'departments',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM workforce.departments record),
         'rules',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM workforce.rules record),
         'positions',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM workforce.positions record),
         'rosters',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM workforce.rosters record),
         'titles',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM workforce.titles record),
         'resources',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM workforce.resources record),
         'photos',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM workforce.photos record),
         'other_audit',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM audit.events record WHERE NOT(action=ANY($1::text[]))))::text", &[&actions],
    ).await.expect("identity, permissions, number, directory and resource window").get(0)
}
