//! Independent decision, authority and retained-resource observations for real review pages.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Duration,
};

use chrono::{DateTime, Utc};
use domain::workforce::application::Snapshot;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_postgres::Client;
use uuid::Uuid;

use super::badge;

#[derive(Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Caller {
    account_id: Uuid,
    person_id: Uuid,
    session: Uuid,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnRead {
    caller: Caller,
    response: Value,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    account_id: Uuid,
    person_id: Uuid,
    session: Uuid,
    application_ids: Vec<Uuid>,
    target_id: Uuid,
    resource_id: Uuid,
    role_id: Uuid,
    original_grant_id: Uuid,
    owner_grant_id: Uuid,
    operation: Option<Uuid>,
    input: Option<Value>,
    status: Option<u16>,
    application: Option<Value>,
    own_read: Option<OwnRead>,
}

struct Bindings {
    owner: Caller,
    reviewer: Caller,
    applications: BTreeSet<Uuid>,
    role: Uuid,
    original_grant: Uuid,
}

struct Window {
    request: Request,
    applications: BTreeMap<Uuid, Value>,
    audit: Vec<Value>,
    related: String,
}

#[derive(Default)]
pub(super) struct Observer {
    step: usize,
    bindings: Option<Bindings>,
    before: Option<Window>,
    immutable: Option<String>,
    reason: Option<String>,
    approved: Option<Uuid>,
    rejected: Option<Uuid>,
}

impl Observer {
    pub(super) fn complete(&self) -> bool {
        self.step == 16
    }

    pub(super) fn evidence(&self) -> Value {
        let bindings = self.bindings.as_ref().expect("review caller bindings");
        json!({"phases":self.step,"ownerId":bindings.owner.person_id,
            "reviewerId":bindings.reviewer.person_id,"approved":self.approved,"rejected":self.rejected,
            "decisions":usize::from(self.approved.is_some())+usize::from(self.rejected.is_some()),
            "pairedAuditAndFixedResources":self.complete(),"rejectedWritesAndReadsUnchanged":self.complete(),
            "freshGrantAndCurrentAuthority":self.complete(),"restrictedCount":if self.complete(){Some(3)}else{None}})
    }

    pub(super) async fn observe(
        &mut self,
        client: &Client,
        badge: &badge::Observer,
        material: &Path,
    ) -> bool {
        assert!(
            badge.complete(),
            "the original sixteen observations precede reviews"
        );
        let phases = [
            "denied",
            "unavailable",
            "rejected",
            "fixed",
            "approved",
            "refresh",
            "revoked",
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
        let path = material.join(format!("badge-review-{phase}-{position}"));
        if !path.with_extension("request").exists() {
            return false;
        }
        let wire: Value = serde_json::from_slice(
            &std::fs::read(path.with_extension("request")).expect("review request"),
        )
        .expect("review JSON");
        for field in [
            "accountId",
            "personId",
            "session",
            "applicationIds",
            "targetId",
            "resourceId",
            "roleId",
            "originalGrantId",
            "ownerGrantId",
            "operation",
            "input",
            "status",
            "application",
            "ownRead",
        ] {
            assert!(
                wire.get(field).is_some(),
                "explicit review observation fields"
            );
        }
        let request: Request = serde_json::from_value(wire).expect("strict review request");
        if self.bindings.is_none() {
            let prior = |name| {
                serde_json::from_slice::<Value>(
                    &std::fs::read(material.join(name)).expect("completed prior request"),
                )
                .expect("prior request JSON")
            };
            let own = prior("badge-restricted-after.request");
            let foreign = prior("badge-foreign-after.request");
            self.bindings = Some(Bindings {
                owner: prior_caller(&own),
                reviewer: prior_caller(&foreign),
                applications: serde_json::from_value::<Vec<Uuid>>(own["applicationIds"].clone())
                    .expect("retained application IDs")
                    .into_iter()
                    .collect(),
                role: request.role_id,
                original_grant: request.original_grant_id,
            });
        }
        tokio::time::timeout(
            Duration::from_secs(3),
            Box::pin(self.capture(client, badge, request, phase, position)),
        )
        .await
        .expect("bounded review observation");
        std::fs::write(path.with_extension("ack"), "ok").expect("review acknowledgement");
        self.step += 1;
        true
    }

    async fn capture(
        &mut self,
        client: &Client,
        badge: &badge::Observer,
        request: Request,
        phase: &str,
        position: &str,
    ) {
        let bindings = self.bindings.as_ref().expect("review bindings");
        let caller = Caller {
            account_id: request.account_id,
            person_id: request.person_id,
            session: request.session,
        };
        assert!(
            caller
                == if phase == "restricted" {
                    bindings.owner
                } else {
                    bindings.reviewer
                },
            "the phase retains its actual caller"
        );
        assert!(bindings.owner != bindings.reviewer);
        assert_eq!(request.application_ids.len(), 4);
        assert!(
            request
                .application_ids
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                == bindings.applications
        );
        assert!(bindings.applications.contains(&request.target_id));
        assert_eq!(request.role_id, bindings.role);
        assert_eq!(request.original_grant_id, bindings.original_grant);
        let original = badge.original(request.target_id);
        assert_eq!(
            original["photo"]["resourceId"],
            request.resource_id.to_string()
        );
        let expected_kind = match phase {
            "denied" | "unavailable" | "rejected" => "duplication",
            "revoked" => "issuance",
            _ => "replacement",
        };
        assert_eq!(original["kind"], expected_kind);
        client
            .batch_execute(
                "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY; SET LOCAL statement_timeout='3s'",
            )
            .await
            .expect("read-only review window");
        let review_actor = phase != "restricted";
        let permission = !matches!(phase, "denied" | "revoked" | "restricted");
        let active: bool = client.query_one(
            "SELECT EXISTS(SELECT 1 FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id
             JOIN access.grants grant_record ON grant_record.person_id=account.person_id JOIN access.roles role ON role.id=grant_record.role_id
             WHERE session.id=$1 AND account.id=$2 AND account.person_id=$3 AND session.revoked_at IS NULL AND session.expires_at>clock_timestamp()
             AND account.suspension_id IS NULL AND account.cancellation_id IS NULL AND role.scope='workforce' AND grant_record.suspension_id IS NULL
             AND (($4 AND role.id=$5 AND grant_record.termination IS NULL AND role.permissions::text[] @> ARRAY['workforce.directory']
                 AND cardinality(role.permissions)=(CASE WHEN $6 THEN 2 ELSE 1 END)
                 AND (role.permissions::text[] @> ARRAY['workforce.approval'])=$6)
               OR (NOT $4 AND grant_record.id=$7 AND grant_record.termination IS NOT NULL AND role.level=1 AND cardinality(role.permissions)=0)))",
            &[&caller.session,&caller.account_id,&caller.person_id,&review_actor,&bindings.role,&permission,&request.owner_grant_id],
        ).await.expect("current review caller, role and participation").get(0);
        assert!(
            active,
            "current account, session and real role match the phase"
        );
        let original_grant: bool = client.query_one("SELECT EXISTS(SELECT 1 FROM access.grants WHERE id=$1 AND person_id=$2 AND termination IS NOT NULL)",
            &[&bindings.original_grant,&bindings.owner.person_id]).await.expect("original terminated grant").get(0);
        assert!(
            original_grant,
            "rejoining never restores the original grant"
        );
        let rejoined = matches!(
            phase,
            "fixed" | "approved" | "refresh" | "revoked" | "restricted"
        );
        assert_eq!(request.owner_grant_id != bindings.original_grant, rejoined);
        let participation: bool = client.query_one("SELECT EXISTS(SELECT 1 FROM access.grants WHERE id=$1 AND person_id=$2 AND suspension_id IS NULL AND (termination IS NULL)=$3)",
            &[&request.owner_grant_id,&bindings.owner.person_id,&(rejoined && phase != "restricted")]).await.expect("applicant grant facts").get(0);
        assert!(
            participation,
            "the actual new grant determines current eligibility"
        );
        let account: bool = client.query_one("SELECT EXISTS(SELECT 1 FROM identity.accounts account JOIN identity.sessions session ON session.account_id=account.id WHERE account.id=$1 AND account.person_id=$2 AND account.suspension_id IS NULL AND account.cancellation_id IS NULL AND session.id=$3 AND session.revoked_at IS NULL AND session.expires_at>clock_timestamp())",
            &[&bindings.owner.account_id,&bindings.owner.person_id,&bindings.owner.session]).await.expect("original applicant account and session").get(0);
        assert!(account);

        let mut applications = BTreeMap::new();
        let mut immutable = BTreeMap::new();
        for row in client.query("SELECT id,to_jsonb(application)::text AS value FROM workforce.applications application ORDER BY id", &[]).await.expect("independent application rows") {
            let id: Uuid = row.get("id");
            assert!(bindings.applications.contains(&id));
            let value: Value = serde_json::from_str(row.get("value")).expect("application row JSON");
            assert_eq!(value["person_id"], bindings.owner.person_id.to_string());
            let old = badge.original(id);
            let stored: Snapshot = serde_json::from_value(value["snapshot"].clone()).expect("stored snapshot");
            let submitted: Snapshot = serde_json::from_value(old["snapshot"].clone()).expect("original submitted snapshot");
            assert_eq!(stored, submitted);
            assert_eq!(value["photo_id"], old["photo"]["photoId"]);
            let mut fixed = value.clone();
            for field in ["status","reviewer_id","reviewed_at","rejection_reason"] { fixed.as_object_mut().expect("row object").remove(field); }
            immutable.insert(id, fixed);
            applications.insert(id, value);
        }
        assert_eq!(applications.len(), 4);
        let fixed = serde_json::to_string(&immutable).expect("fixed application facts");
        if let Some(original) = &self.immutable {
            assert!(
                original == &fixed,
                "all original application facts remain immutable"
            );
        } else {
            self.immutable = Some(fixed);
        }
        badge.verify_resources();
        let expected_reviews = if self.step < 5 {
            0
        } else if self.step < 9 {
            1
        } else {
            2
        };
        let reason = if phase == "rejected" && position == "after" {
            Some(
                request.input.as_ref().expect("actual rejection input")["reason"]
                    .as_str()
                    .expect("reason")
                    .trim()
                    .to_owned(),
            )
        } else {
            self.reason.clone()
        };
        for (id, value) in &applications {
            let status = if value["kind"] == "duplication" && expected_reviews >= 1 {
                "rejected"
            } else if value["kind"] == "replacement" && expected_reviews >= 2 {
                "approved"
            } else {
                "submitted"
            };
            assert_eq!(value["status"], status);
            if status == "submitted" {
                for field in ["reviewer_id", "reviewed_at", "rejection_reason"] {
                    assert!(value[field].is_null());
                }
            } else {
                assert_eq!(
                    value["reviewer_id"],
                    bindings.reviewer.person_id.to_string()
                );
                assert!(instant(&value["reviewed_at"]) >= instant(&value["submitted_at"]));
                if status == "rejected" {
                    assert_eq!(
                        value["rejection_reason"],
                        reason.as_deref().expect("original rejection reason")
                    );
                    self.rejected = Some(*id);
                } else {
                    assert!(value["rejection_reason"].is_null());
                    self.approved = Some(*id);
                }
            }
        }
        let audit: Vec<Value> = client.query("SELECT to_jsonb(event)::text FROM audit.events event WHERE action IN ('workforce.application.approved','workforce.application.rejected') ORDER BY id",&[])
            .await.expect("independent review audit").into_iter().map(|row|serde_json::from_str(row.get(0)).expect("audit JSON")).collect();
        assert_eq!(audit.len(), expected_reviews);
        let mut targets = BTreeSet::new();
        for event in &audit {
            let id = event["target_id"]
                .as_str()
                .expect("audit target")
                .parse::<Uuid>()
                .expect("target UUID");
            assert!(targets.insert(id), "each decision has one audit event");
            let application = applications
                .get(&id)
                .expect("audit references an original application");
            assert_eq!(event["target_type"], "workforce.application");
            assert_eq!(event["actor_id"], bindings.reviewer.person_id.to_string());
            assert_eq!(event["occurred_at"], application["reviewed_at"]);
            assert_eq!(
                event["action"],
                format!(
                    "workforce.application.{}",
                    application["status"].as_str().expect("status")
                )
            );
            assert_eq!(
                event["details"],
                json!({"previousStatus":"submitted","status":application["status"],"rejectionReason":application["rejection_reason"]})
            );
        }
        let related = badge::related_facts(
            client,
            &[
                "workforce.application.approved",
                "workforce.application.rejected",
            ],
        )
        .await;
        if position == "before" {
            assert!(request.status.is_none() && request.application.is_none());
            assert!(request.own_read.is_none());
            self.before = Some(Window {
                request,
                applications,
                audit,
                related,
            });
        } else {
            let before = self.before.take().expect("matching review before-window");
            assert!(
                before.related == related,
                "review windows preserve identity, grants, directories, numbers, photos and unrelated audit"
            );
            assert_eq!(before.request.target_id, request.target_id);
            assert_eq!(before.request.operation, request.operation);
            assert!(before.request.input == request.input);
            assert_eq!(before.request.owner_grant_id, request.owner_grant_id);
            if matches!(phase, "rejected" | "approved") {
                assert!(request.operation.is_some());
                assert_eq!(request.status, Some(200));
                let input = request.input.as_ref().expect("actual decision input");
                assert_eq!(
                    input["decision"],
                    if phase == "approved" {
                        "approve"
                    } else {
                        "reject"
                    }
                );
                assert_eq!(
                    before.applications[&request.target_id]["status"],
                    "submitted"
                );
                for (id, application) in &applications {
                    if *id != request.target_id {
                        assert!(
                            before.applications.get(id) == Some(application),
                            "only the selected application changes"
                        );
                    }
                }
                assert_eq!(audit.len(), before.audit.len() + 1);
                let application = request
                    .application
                    .as_ref()
                    .expect("actual response before delivery");
                let stored = &applications[&request.target_id];
                verify_application(application, stored, badge.original(request.target_id));
                if phase == "rejected" {
                    assert_eq!(application["rejectionReason"], stored["rejection_reason"]);
                    self.reason = reason;
                } else {
                    assert!(application.get("rejectionReason").is_none());
                }
            } else {
                assert!(
                    before.applications == applications && before.audit == audit,
                    "reads and rejected writes preserve all decisions and audit"
                );
                let expected = match phase {
                    "unavailable" | "refresh" => Some(409),
                    "revoked" => Some(403),
                    _ => None,
                };
                assert_eq!(request.status, expected);
                assert!(request.application.is_none());
                if expected.is_some() {
                    assert!(request.operation.is_some());
                    assert_eq!(request.input, Some(json!({"decision":"approve"})));
                } else {
                    assert!(request.operation.is_none() && request.input.is_none());
                }
            }
            if matches!(phase, "refresh" | "restricted") {
                let own = request
                    .own_read
                    .as_ref()
                    .expect("actual owner read with its own caller");
                assert!(
                    own.caller == bindings.owner,
                    "owner reads never inherit the reviewer's identity"
                );
                assert_eq!(own.response["code"], "ok");
                assert_eq!(
                    own.response["access"],
                    if phase == "restricted" {
                        "restricted"
                    } else {
                        "full"
                    }
                );
                let expected: BTreeSet<_> = applications
                    .iter()
                    .filter(|(_, value)| phase != "restricted" || value["status"] != "rejected")
                    .map(|(id, _)| *id)
                    .collect();
                assert_eq!(own.response["count"].as_u64(), Some(expected.len() as u64));
                let rows = own.response["applications"]
                    .as_array()
                    .expect("actual owner history");
                assert_eq!(rows.len(), expected.len());
                let mut actual = BTreeSet::new();
                for value in rows {
                    let id = value["id"]
                        .as_str()
                        .expect("owner item ID")
                        .parse::<Uuid>()
                        .expect("owner item UUID");
                    assert!(actual.insert(id));
                    let stored = applications.get(&id).expect("an original owner item");
                    if phase == "restricted" {
                        let keys: BTreeSet<_> = value
                            .as_object()
                            .expect("restricted item")
                            .keys()
                            .map(String::as_str)
                            .collect();
                        assert!(keys == BTreeSet::from(["id", "kind", "status", "submittedAt"]));
                        assert_eq!(value["kind"], stored["kind"]);
                        assert_eq!(value["status"], stored["status"]);
                        assert_eq!(
                            instant(&value["submittedAt"]),
                            instant(&stored["submitted_at"])
                        );
                    } else {
                        verify_application(value, stored, badge.original(id));
                    }
                }
                assert!(
                    actual == expected,
                    "owner count, items and restricted eligibility agree"
                );
            } else {
                assert!(request.own_read.is_none());
            }
        }
        client
            .batch_execute("COMMIT")
            .await
            .expect("complete review observation");
    }
}

fn prior_caller(value: &Value) -> Caller {
    serde_json::from_value(json!({"accountId":value["accountId"],"personId":value["personId"],"session":value["session"]}))
        .expect("previously checked caller identity")
}

fn verify_application(actual: &Value, stored: &Value, original: &Value) {
    assert_eq!(actual["id"], stored["id"]);
    assert_eq!(actual["kind"], stored["kind"]);
    assert_eq!(actual["status"], stored["status"]);
    assert_eq!(
        instant(&actual["submittedAt"]),
        instant(&stored["submitted_at"])
    );
    let snapshot: Snapshot =
        serde_json::from_value(actual["snapshot"].clone()).expect("actual public snapshot");
    let fixed: Snapshot =
        serde_json::from_value(stored["snapshot"].clone()).expect("fixed stored snapshot");
    assert_eq!(snapshot, fixed);
    assert_eq!(actual["photo"], original["photo"]);
    if stored["status"] == "submitted" {
        for key in [
            "reviewerId",
            "reviewerName",
            "reviewedAt",
            "rejectionReason",
        ] {
            assert!(actual.get(key).is_none());
        }
    } else {
        assert_eq!(actual["reviewerId"], stored["reviewer_id"]);
        assert_eq!(actual["reviewerName"], "B*");
        assert_eq!(
            instant(&actual["reviewedAt"]),
            instant(&stored["reviewed_at"])
        );
        if stored["status"] == "rejected" {
            assert_eq!(actual["rejectionReason"], stored["rejection_reason"]);
        } else {
            assert!(actual.get("rejectionReason").is_none());
        }
    }
}

fn instant(value: &Value) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value.as_str().expect("timestamp string"))
        .expect("valid time")
        .with_timezone(&Utc)
}
