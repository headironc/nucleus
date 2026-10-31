//! Observe real badge cancellations without sharing the writer's transaction.

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

use super::{badge, reviews};

#[derive(Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Caller {
    account_id: Uuid,
    person_id: Uuid,
    session: Uuid,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Attempt {
    operation: Uuid,
    input: Value,
    response: Option<Value>,
    status: Option<u16>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OwnRead {
    caller: Caller,
    target_id: Uuid,
    resource_id: Uuid,
    response: Value,
    detail_status: u16,
    resource_status: u16,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    caller: Caller,
    target_id: Uuid,
    attempt: Option<Attempt>,
    own_read: Option<OwnRead>,
    managed: Option<Value>,
}

struct Bindings {
    owner: Caller,
    manager: Caller,
    role: Uuid,
    grant: Uuid,
    applications: BTreeSet<Uuid>,
    pending: Uuid,
    approved: Uuid,
    rejected: Uuid,
    approved_response: Value,
}

struct Window {
    request: Request,
    applications: BTreeMap<Uuid, Value>,
    audit: BTreeMap<Uuid, Value>,
    related: String,
    observed_at: DateTime<Utc>,
}

#[derive(Default)]
pub(super) struct Observer {
    step: usize,
    bindings: Option<Bindings>,
    before: Option<Window>,
    fixed: Option<BTreeMap<Uuid, Value>>,
    reason: Option<String>,
}

impl Observer {
    pub(super) fn complete(&self) -> bool {
        self.step == 6
    }

    pub(super) fn evidence(&self) -> Value {
        json!({"phases":self.step,"cancellations":if self.step<2{0}else if self.step<4{1}else{2},
            "pairedAuditAndRetainedFacts":self.complete(),"refreshWithoutWrites":self.complete(),
            "restrictedCount":if self.complete(){Some(1)}else{None}})
    }

    pub(super) async fn observe(
        &mut self,
        client: &Client,
        badge: &badge::Observer,
        reviews: &reviews::Observer,
        material: &Path,
    ) -> bool {
        assert!(
            badge.complete() && reviews.complete(),
            "cancellations follow all original observation windows"
        );
        let Some(phase) = ["self", "manager", "refresh"].get(self.step / 2).copied() else {
            return false;
        };
        let position = if self.step.is_multiple_of(2) {
            "before"
        } else {
            "after"
        };
        let path = material.join(format!("badge-cancellation-{phase}-{position}"));
        if !path.with_extension("request").exists() {
            return false;
        }
        let wire: Value = serde_json::from_slice(
            &std::fs::read(path.with_extension("request")).expect("cancellation request"),
        )
        .expect("cancellation JSON");
        for field in ["caller", "targetId", "attempt", "ownRead", "managed"] {
            assert!(
                wire.get(field).is_some(),
                "explicit cancellation observation fields"
            );
        }
        if !wire["attempt"].is_null() {
            for field in ["operation", "input", "response", "status"] {
                assert!(
                    wire["attempt"].get(field).is_some(),
                    "explicit attempt fields"
                );
            }
        }
        let request: Request = serde_json::from_value(wire).expect("strict cancellation request");
        if self.bindings.is_none() {
            let prior = |name: &str| {
                serde_json::from_slice::<Value>(
                    &std::fs::read(material.join(name)).expect("completed review request"),
                )
                .expect("completed review JSON")
            };
            let restricted = prior("badge-review-restricted-after.request");
            let approved = prior("badge-review-approved-after.request");
            self.bindings = Some(Bindings {
                owner: caller(&restricted),
                manager: caller(&approved),
                role: uuid(&restricted["roleId"]),
                grant: uuid(&restricted["ownerGrantId"]),
                applications: serde_json::from_value::<Vec<Uuid>>(
                    restricted["applicationIds"].clone(),
                )
                .expect("original application IDs")
                .into_iter()
                .collect(),
                pending: uuid(&prior("badge-review-revoked-after.request")["targetId"]),
                approved: uuid(&approved["targetId"]),
                rejected: uuid(&prior("badge-review-rejected-after.request")["targetId"]),
                approved_response: approved["application"].clone(),
            });
        }
        tokio::time::timeout(
            Duration::from_secs(3),
            Box::pin(self.capture(client, badge, request, phase, position)),
        )
        .await
        .expect("bounded read-only cancellation observation");
        std::fs::write(path.with_extension("ack"), "ok").expect("cancellation acknowledgement");
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
        let bindings = self.bindings.as_ref().expect("cancellation bindings");
        assert!(bindings.owner != bindings.manager);
        assert!(
            request.caller
                == if phase == "self" {
                    bindings.owner
                } else {
                    bindings.manager
                },
            "the actual caller matches the explicit mode"
        );
        assert_eq!(
            request.target_id,
            if phase == "self" {
                bindings.pending
            } else {
                bindings.approved
            }
        );
        client
            .batch_execute(
                "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY; SET LOCAL statement_timeout='3s'",
            )
            .await
            .expect("read-only cancellation window");
        let observed_at: DateTime<Utc> = client
            .query_one("SELECT clock_timestamp()", &[])
            .await
            .expect("database observation time")
            .get(0);
        for actor in [bindings.owner, bindings.manager] {
            let active: bool = client.query_one("SELECT EXISTS(SELECT 1 FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id WHERE session.id=$1 AND account.id=$2 AND account.person_id=$3 AND session.revoked_at IS NULL AND session.expires_at>clock_timestamp() AND account.suspension_id IS NULL AND account.cancellation_id IS NULL)",
                &[&actor.session,&actor.account_id,&actor.person_id]).await.expect("original cancellation caller").get(0);
            assert!(active, "both original accounts and sessions remain valid");
        }
        let ended: bool = client.query_one("SELECT EXISTS(SELECT 1 FROM access.grants WHERE id=$1 AND person_id=$2 AND termination IS NOT NULL)", &[&bindings.grant,&bindings.owner.person_id]).await.expect("ended applicant grant").get(0);
        assert!(
            ended,
            "both modes cancel an applicant without current participation"
        );
        let approval = phase != "self";
        let manager: bool = client.query_one("SELECT EXISTS(SELECT 1 FROM access.grants grant_record JOIN access.roles role ON role.id=grant_record.role_id WHERE grant_record.person_id=$1 AND grant_record.suspension_id IS NULL AND grant_record.termination IS NULL AND role.id=$2 AND role.scope='workforce' AND role.permissions::text[] @> ARRAY['workforce.directory'] AND (role.permissions::text[] @> ARRAY['workforce.approval'])=$3 AND cardinality(role.permissions)=(CASE WHEN $3 THEN 2 ELSE 1 END))", &[&bindings.manager.person_id,&bindings.role,&approval]).await.expect("current manager authority").get(0);
        assert!(
            manager,
            "role preparation stays outside observation windows"
        );

        let mut applications = BTreeMap::new();
        let mut fixed = BTreeMap::new();
        for row in client.query("SELECT id,to_jsonb(application)::text AS value FROM workforce.applications application ORDER BY id", &[]).await.expect("retained cancellation rows") {
            let id: Uuid = row.get("id");
            assert!(bindings.applications.contains(&id));
            let value: Value = serde_json::from_str(row.get("value")).expect("application row JSON");
            let original = badge.original(id);
            let stored: Snapshot = serde_json::from_value(value["snapshot"].clone()).expect("stored snapshot");
            let submitted: Snapshot = serde_json::from_value(original["snapshot"].clone()).expect("original snapshot");
            assert_eq!(stored, submitted);
            assert_eq!(value["person_id"], bindings.owner.person_id.to_string());
            assert_eq!(value["photo_id"], original["photo"]["photoId"]);
            let mut retained = value.clone();
            retained.as_object_mut().expect("row object").remove("status");
            fixed.insert(id, retained);
            applications.insert(id, value);
        }
        assert_eq!(applications.len(), 4);
        if let Some(original) = &self.fixed {
            assert!(
                original == &fixed,
                "cancellation changes no snapshot, photo or prior review fact"
            );
        } else {
            self.fixed = Some(fixed);
        }
        badge.verify_resources();
        let expected_count = match self.step {
            0 => 0,
            1 | 2 => 1,
            _ => 2,
        };
        for (id, value) in &applications {
            let expected = if (*id == bindings.pending && expected_count >= 1)
                || (*id == bindings.approved && expected_count >= 2)
            {
                "cancelled"
            } else if *id == bindings.approved {
                "approved"
            } else if *id == bindings.rejected {
                "rejected"
            } else {
                "submitted"
            };
            assert_eq!(value["status"], expected);
        }
        let audit: BTreeMap<Uuid,Value> = client.query("SELECT id,to_jsonb(event)::text AS value FROM audit.events event WHERE action='workforce.application.cancelled' ORDER BY id", &[]).await.expect("independent cancellation audits").into_iter().map(|row| (row.get("id"),serde_json::from_str(row.get("value")).expect("audit JSON"))).collect();
        assert_eq!(audit.len(), expected_count);
        let related = badge::related_facts(client, &["workforce.application.cancelled"]).await;
        if position == "before" {
            assert!(request.own_read.is_none() && request.managed.is_none());
            if phase == "refresh" {
                assert!(request.attempt.is_none());
            } else {
                let attempt = request.attempt.as_ref().expect("actual page cancellation");
                assert!(attempt.status.is_none() && attempt.response.is_none());
            }
            self.before = Some(Window {
                request,
                applications,
                audit,
                related,
                observed_at,
            });
        } else {
            let before = self
                .before
                .take()
                .expect("matching cancellation before-window");
            assert!(
                before.related == related,
                "cancellation preserves identity, roles, grants, numbers, photos and all other audit"
            );
            assert!(before.request.caller == request.caller);
            assert_eq!(before.request.target_id, request.target_id);
            if phase == "refresh" {
                assert!(request.attempt.is_none());
                assert!(
                    before.applications == applications && before.audit == audit,
                    "GET refresh changes no database fact"
                );
            } else {
                let attempt = request
                    .attempt
                    .as_ref()
                    .expect("actual completed cancellation");
                let original = before
                    .request
                    .attempt
                    .as_ref()
                    .expect("original cancellation request");
                assert_eq!(attempt.operation, original.operation);
                assert_eq!(attempt.input, original.input);
                assert_eq!(attempt.status, Some(200));
                assert_eq!(
                    attempt.response,
                    Some(
                        json!({"code":"ok","application":{"id":request.target_id,"status":"cancelled"}})
                    )
                );
                let previous = if phase == "self" {
                    "submitted"
                } else {
                    "approved"
                };
                assert_eq!(before.applications[&request.target_id]["status"], previous);
                let reason = if phase == "manager" {
                    Some(
                        attempt.input["reason"]
                            .as_str()
                            .expect("manager reason")
                            .trim()
                            .to_owned(),
                    )
                } else {
                    None
                };
                let mut input = json!({"mode":phase,"expectedStatus":previous});
                if phase == "manager" {
                    input["reason"] = attempt.input["reason"].clone();
                }
                assert_eq!(attempt.input, input);
                let mut expected = before.applications.clone();
                expected
                    .get_mut(&request.target_id)
                    .expect("original target")["status"] = json!("cancelled");
                assert!(
                    expected == applications,
                    "only the confirmed target status changes"
                );
                for (id, event) in &before.audit {
                    assert_eq!(audit.get(id), Some(event));
                }
                let added: Vec<_> = audit
                    .iter()
                    .filter(|(id, _)| !before.audit.contains_key(id))
                    .collect();
                assert_eq!(
                    added.len(),
                    1,
                    "one successful write has one cancellation audit"
                );
                let event = added[0].1;
                assert_eq!(event["actor_id"], request.caller.person_id.to_string());
                assert_eq!(event["target_id"], request.target_id.to_string());
                assert_eq!(event["target_type"], "workforce.application");
                assert_eq!(event["action"], "workforce.application.cancelled");
                let mut details =
                    json!({"previousStatus":previous,"status":"cancelled","mode":phase});
                if let Some(value) = &reason {
                    assert!(!value.is_empty());
                    details["reason"] = json!(value);
                }
                assert_eq!(event["details"], details);
                let occurred = instant(&event["occurred_at"]);
                assert!(
                    occurred >= before.observed_at && occurred <= observed_at,
                    "database-generated cancellation time lies within the actual window"
                );
                if phase == "manager" {
                    self.reason = reason;
                }
            }
            if phase == "self" || phase == "refresh" {
                let own = request
                    .own_read
                    .as_ref()
                    .expect("owner read with its actual caller");
                assert!(own.caller == bindings.owner);
                assert_eq!(own.target_id, request.target_id);
                assert_eq!(
                    own.resource_id,
                    uuid(&badge.original(request.target_id)["photo"]["resourceId"])
                );
                assert_eq!((own.detail_status, own.resource_status), (404, 404));
                assert_eq!(own.response["code"], "ok");
                assert_eq!(own.response["access"], "restricted");
                let expected: BTreeSet<_> = applications
                    .iter()
                    .filter(|(_, value)| {
                        matches!(value["status"].as_str(), Some("submitted" | "approved"))
                    })
                    .map(|(id, _)| *id)
                    .collect();
                assert_eq!(own.response["count"].as_u64(), Some(expected.len() as u64));
                let rows = own.response["applications"]
                    .as_array()
                    .expect("restricted owner records");
                assert_eq!(rows.len(), expected.len());
                let mut actual = BTreeSet::new();
                for value in rows {
                    let id = uuid(&value["id"]);
                    assert!(actual.insert(id));
                    let stored = applications
                        .get(&id)
                        .expect("an original owner application");
                    assert_eq!(
                        value
                            .as_object()
                            .expect("summary object")
                            .keys()
                            .map(String::as_str)
                            .collect::<BTreeSet<_>>(),
                        BTreeSet::from(["id", "kind", "status", "submittedAt"])
                    );
                    assert_eq!(value["kind"], stored["kind"]);
                    assert_eq!(value["status"], stored["status"]);
                    assert_eq!(
                        instant(&value["submittedAt"]),
                        instant(&stored["submitted_at"])
                    );
                }
                assert!(actual == expected);
            } else {
                assert!(request.own_read.is_none());
            }
            if phase == "refresh" {
                let managed = request.managed.as_ref().expect("actual management GET");
                let mut expected = bindings.approved_response.clone();
                expected["status"] = json!("cancelled");
                assert_eq!(managed["code"], "ok");
                assert_eq!(managed["application"], expected);
                assert_eq!(managed["current"]["participationActive"], false);
                assert_eq!(managed["current"]["accountActive"], true);
                assert!(
                    !managed
                        .to_string()
                        .contains(self.reason.as_deref().expect("manager audit-only reason"))
                );
            } else {
                assert!(request.managed.is_none());
            }
        }
        client
            .batch_execute("COMMIT")
            .await
            .expect("complete cancellation observation");
    }
}

fn caller(value: &Value) -> Caller {
    serde_json::from_value(json!({"accountId":value["accountId"],"personId":value["personId"],"session":value["session"]})).expect("previously verified review caller")
}

fn uuid(value: &Value) -> Uuid {
    value.as_str().expect("UUID string").parse().expect("UUID")
}

fn instant(value: &Value) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value.as_str().expect("timestamp string"))
        .expect("valid time")
        .with_timezone(&Utc)
}
