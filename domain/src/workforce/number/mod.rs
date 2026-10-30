//! Permanent number state, immutable values and object-ordered history.

mod command;
mod error;
pub mod history;
mod query;
mod record;
pub mod recovery;
mod revision;
mod state;
mod transition;
mod value;
#[cfg(test)]
mod vectors;
mod verification;

use std::sync::{Arc, atomic::AtomicI32};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_postgres::{
    Client,
    types::{FromSql, ToSql},
};
use utoipa::ToSchema;
use uuid::Uuid;

pub use crate::workforce::number::{
    error::{Error, Rejection},
    value::{Number, Reason, Subtype, ValueError},
};
pub(crate) use crate::workforce::number::{
    record::{ALLOCATIONS, CORRECTIONS, RECOVERIES, REVOCATIONS, TRANSITIONS},
    revision::TABLE as REVISIONS,
    state::TABLE as NUMBERS,
};
use crate::{
    access::{
        grant::{self, member},
        role,
    },
    identity::{
        account,
        current::{self, Actor},
        keys,
        mobile::Mobile,
        operation, person,
    },
    kernel::App,
    pagination::Pagination,
};

/// A fixed number revision; later corrections create new revisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, FromSql, ToSql, ToSchema)]
#[serde(transparent)]
#[postgres(transparent)]
#[schema(as = number::Id, value_type = Uuid)]
pub struct Id(Uuid);

impl Id {
    fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }

    fn allocate() -> Self {
        Self(Uuid::new_v4())
    }
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = number::Input)]
pub struct Input {
    subtype: Subtype,
}

/// Management assistance input: the subtype, the stated actual entry year, a specific reason and
/// the confirmed target, as in Grant management.
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = number::Assistance)]
pub struct Assistance {
    subtype: Subtype,
    #[serde(deserialize_with = "value::year")]
    #[schema(minimum = 1)]
    year: i32,
    reason: Reason,
    target: member::Target,
}

/// Historical number supplement input: the complete original number, a specific reason and the
/// confirmed target, as in management assistance.
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = number::Supplement)]
pub struct Supplement {
    subtype: Subtype,
    #[serde(deserialize_with = "value::year")]
    #[schema(minimum = 1)]
    year: i32,
    #[serde(deserialize_with = "value::serial")]
    #[schema(minimum = 1, maximum = 999)]
    serial: i16,
    reason: Reason,
    target: member::Target,
}

/// Correct an existing number, confirming the person's current real-name and number revisions.
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = number::Correction)]
pub struct Correction {
    revision_id: person::revision::Id,
    expected: Id,
    replacement: Number,
    reason: Reason,
}

/// An eligible member's masked identity and current Grant for allocation confirmation.
pub struct Candidate {
    identity: account::Subject,
    grant_id: grant::Id,
    mobile: Mobile,
}

impl Candidate {
    pub fn identity(&self) -> &account::Subject {
        &self.identity
    }
    pub fn grant_id(&self) -> grant::Id {
        self.grant_id
    }
    pub fn mobile(&self) -> &Mobile {
        &self.mobile
    }
}

/// A manageable person's masked identity and current number for correction confirmation.
pub struct CorrectionCandidate {
    identity: account::Subject,
    mobile: Mobile,
    number: Revision,
}

impl CorrectionCandidate {
    pub fn identity(&self) -> &account::Subject {
        &self.identity
    }
    pub fn mobile(&self) -> &Mobile {
        &self.mobile
    }
    pub fn number(&self) -> &Revision {
        &self.number
    }
}

/// A current number holder's fixed masked identity and number for correction confirmation.
pub struct CorrectionHolder {
    identity: person::revision::Identity,
    number: Revision,
}

impl CorrectionHolder {
    pub fn identity(&self) -> &person::revision::Identity {
        &self.identity
    }
    pub fn number(&self) -> &Revision {
        &self.number
    }
}

/// The fixed revision a person holds, with its complete number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Revision {
    id: Id,
    number: Number,
}

impl Revision {
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn number(&self) -> Number {
        self.number
    }
}

/// The source of a permanent Workforce number allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = number::Source)]
pub enum Source {
    SelfService,
    Assistance,
    Supplement,
}

/// The source and fixed number versions of a history item.
pub enum HistoryKind {
    Allocation {
        source: Source,
        number: Revision,
        reason: Option<String>,
    },
    Correction {
        before: Revision,
        after: Revision,
        reason: String,
    },
    /// The beneficiary's restored revision repeating its original value.
    Recovery {
        number: Revision,
        origin: Id,
        reason: String,
    },
    /// A current number ended by the named recovery.
    Revocation {
        number: Revision,
        recovery: recovery::RecoveryId,
        reason: String,
    },
}

/// A verified permanent Workforce number history fact, exposed only for reading.
pub struct HistoryItem {
    id: history::Id,
    kind: HistoryKind,
    time: DateTime<Utc>,
    sequence: i64,
    operation_id: operation::Id,
    operator: person::revision::Identity,
}

impl HistoryItem {
    pub fn id(&self) -> history::Id {
        self.id
    }

    pub fn kind(&self) -> &HistoryKind {
        &self.kind
    }

    pub fn sequence(&self) -> i64 {
        self.sequence
    }

    pub fn time(&self) -> DateTime<Utc> {
        self.time
    }

    pub fn operation_id(&self) -> operation::Id {
        self.operation_id
    }

    pub fn operator(&self) -> &person::revision::Identity {
        &self.operator
    }
}

/// A page of Workforce number history facts.
pub struct HistoryList {
    count: i64,
    revisions: Vec<HistoryItem>,
}

impl HistoryList {
    pub fn count(&self) -> i64 {
        self.count
    }

    pub fn revisions(&self) -> &[HistoryItem] {
        &self.revisions
    }
}

#[derive(Clone)]
pub struct Service {
    app: App,
    current: current::Service,
    roles: role::Reader,
    keys: Arc<keys::Keys>,
    accounts: account::Reader,
    revisions: person::revision::Reader,
}

pub(crate) async fn verify(
    client: &Client,
    app: &App,
    revisions: &person::revision::Reader,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    crate::history::initialize(
        app.anchors(),
        verification::inspect(client, app.keys(), revisions, now).await?,
    )?;
    Ok(())
}

impl Service {
    pub(crate) fn prepare(app: App, current: current::Service, keys: Arc<keys::Keys>) -> Self {
        Self {
            roles: role::Reader::new(app.clone()),
            accounts: account::Reader::new(app.clone(), Arc::clone(&keys)),
            revisions: person::revision::Reader::new(app.clone()),
            app,
            current,
            keys,
        }
    }

    #[cfg(test)]
    pub(crate) async fn verify(&self, client: &Client, now: DateTime<Utc>) -> Result<(), Error> {
        verification::inspect(client, self.app.keys(), &self.revisions, now).await?;
        Ok(())
    }

    async fn allocate_input(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        input: command::Allocation,
    ) -> Result<Revision, Error> {
        self.app
            .command(
                operation_id,
                command::Allocate {
                    service: self.clone(),
                    actor,
                    input,
                    id: Uuid::new_v4(),
                    number_id: Id::allocate(),
                    locked_year: AtomicI32::new(0),
                },
            )
            .await
    }

    pub async fn allocate(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        input: Input,
    ) -> Result<Revision, Error> {
        self.allocate_input(actor, operation_id, command::Allocation::Current(input))
            .await
    }

    pub async fn allocate_member(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        id: person::Id,
        input: Assistance,
    ) -> Result<Revision, Error> {
        self.allocate_input(
            actor,
            operation_id,
            command::Allocation::Assistance { person: id, input },
        )
        .await
    }

    pub async fn supplement(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        id: person::Id,
        input: Supplement,
    ) -> Result<Revision, Error> {
        self.allocate_input(
            actor,
            operation_id,
            command::Allocation::Supplement { person: id, input },
        )
        .await
    }

    pub async fn correct(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        id: person::Id,
        input: Correction,
    ) -> Result<Revision, Error> {
        self.app
            .command(
                operation_id,
                command::Correct {
                    service: self.clone(),
                    actor,
                    person: id,
                    input,
                    id: Uuid::new_v4(),
                    number_id: Id::allocate(),
                },
            )
            .await
    }

    pub async fn candidate(&self, actor: &Actor, mobile: Mobile) -> Result<Candidate, Error> {
        let (identity, grant, _) = self
            .app
            .query(&query::Candidate {
                service: self,
                actor,
                mobile: &mobile,
                kind: query::CandidateKind::Allocation,
            })
            .await?;
        Ok(Candidate {
            identity,
            grant_id: grant.ok_or_else(|| {
                Error::corrupt("an allocation candidate retains its qualified Grant")
            })?,
            mobile,
        })
    }

    pub async fn correction_candidate(
        &self,
        actor: &Actor,
        mobile: Mobile,
    ) -> Result<CorrectionCandidate, Error> {
        let (identity, _, number) = self
            .app
            .query(&query::Candidate {
                service: self,
                actor,
                mobile: &mobile,
                kind: query::CandidateKind::Correction,
            })
            .await?;
        Ok(CorrectionCandidate {
            identity,
            mobile,
            number: number.ok_or_else(|| {
                Error::corrupt("a correction candidate retains its current number")
            })?,
        })
    }

    pub async fn correction_holder(
        &self,
        actor: &Actor,
        number: Number,
    ) -> Result<CorrectionHolder, Error> {
        self.app
            .query(&query::Holder {
                service: self,
                actor,
                number,
            })
            .await
    }

    pub async fn read_current(&self, actor: &Actor) -> Result<Option<Revision>, Error> {
        self.app
            .query(&query::Current {
                service: self,
                actor,
            })
            .await
    }

    pub async fn history(
        &self,
        actor: &Actor,
        target: person::Id,
        pagination: Pagination,
    ) -> Result<HistoryList, Error> {
        self.app
            .query(&query::History {
                service: self,
                actor,
                person: target,
                pagination,
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{sync::Arc, time::Duration};

    use serde_json::json;
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;

    use super::{Error, Input, Rejection, Subtype};
    use crate::{
        access::{
            Entry,
            appointment::tests::{mobile, register, register_numbered},
            grant::tests::admit,
            role::{
                Kind,
                tests::{actor, start},
            },
        },
        identity::{
            authentication::{Database, Supplier, server},
            current::Actor,
            operation,
        },
        startup::Services,
    };

    fn operation() -> operation::Id {
        Uuid::new_v4().to_string().parse().expect("operation")
    }

    fn previous_year() -> i32 {
        use chrono::Datelike;
        (chrono::Utc::now() + chrono::Duration::hours(8)).year() - 1
    }

    async fn promote(services: &Services, institution: &Actor) {
        let role = services
            .role()
            .create(
                institution.clone(),
                operation(),
                serde_json::from_value(
                    json!({"name":"Number office","level":500,"permissions":[]}),
                )
                .expect("office"),
            )
            .await
            .expect("institutional level only");
        let candidate = services
            .appointment()
            .candidate(institution, "13800000000".parse().expect("mobile"))
            .await
            .expect("candidate");
        Box::pin(services.appointment().assign(
            institution.clone(),
            operation(),
            crate::access::appointment::tests::command(&candidate, &role),
        ))
        .await
        .expect("higher institution level");
    }

    async fn confirmation(services: &Services, target: &Actor, mobile: &str) -> serde_json::Value {
        let identity = services
            .current()
            .read(target)
            .await
            .expect("target identity");
        json!({"subtype":801,"year":previous_year(),"reason":"Verified original entry record","target":{"accountId":identity.id(),"personId":identity.person_id(),"revisionId":identity.revision_id(),"mobile":mobile}})
    }

    async fn supplement_confirmation(
        services: &Services,
        target: &Actor,
        mobile: &str,
        serial: i16,
    ) -> serde_json::Value {
        let mut value = confirmation(services, target, mobile).await;
        value["serial"] = json!(serial);
        value
    }

    fn assistance(value: &serde_json::Value) -> super::Assistance {
        serde_json::from_value(value.clone()).expect("typed assistance")
    }

    fn supplement(value: &serde_json::Value) -> super::Supplement {
        serde_json::from_value(value.clone()).expect("typed supplement")
    }

    fn target(value: &serde_json::Value) -> crate::identity::person::Id {
        serde_json::from_value(value["target"]["personId"].clone()).expect("person")
    }

    #[test]
    fn assistance_requires_complete_typed_confirmation_and_positive_year() {
        let value = json!({"subtype":801,"year":2025,"reason":"Verified record","target":{"accountId":Uuid::new_v4(),"personId":Uuid::new_v4(),"revisionId":Uuid::new_v4(),"mobile":"13800000000"}});
        for (field, invalid) in [
            ("year", json!(0)),
            ("year", json!(-1)),
            ("year", json!("2025")),
            ("subtype", json!(800)),
            ("reason", json!(" \t\n")),
            ("reason", json!(null)),
            ("unexpected", json!(true)),
        ] {
            let mut changed = value.clone();
            changed[field] = invalid;
            assert!(
                serde_json::from_value::<super::Assistance>(changed).is_err(),
                "{field}"
            );
        }
        for field in ["accountId", "personId", "revisionId", "mobile"] {
            let mut changed = value.clone();
            changed["target"]
                .as_object_mut()
                .expect("target object")
                .remove(field);
            assert!(
                serde_json::from_value::<super::Assistance>(changed).is_err(),
                "{field}"
            );
        }
        assert!(serde_json::from_value::<super::Assistance>(value.clone()).is_ok());
        assert!(
            serde_json::from_value::<super::Supplement>(value.clone()).is_err(),
            "supplement requires serial"
        );
        let mut supplemented = value;
        supplemented["serial"] = json!(42);
        for (field, invalid) in [
            ("serial", json!(0)),
            ("serial", json!(1000)),
            ("serial", json!(-1)),
            ("serial", json!("42")),
            ("year", json!(0)),
            ("subtype", json!(800)),
            ("reason", json!("   ")),
            ("unexpected", json!(true)),
        ] {
            let mut changed = supplemented.clone();
            changed[field] = invalid;
            assert!(
                serde_json::from_value::<super::Supplement>(changed).is_err(),
                "{field}"
            );
        }
        for field in ["accountId", "personId", "revisionId", "mobile"] {
            let mut changed = supplemented.clone();
            changed["target"]
                .as_object_mut()
                .expect("target object")
                .remove(field);
            assert!(
                serde_json::from_value::<super::Supplement>(changed).is_err(),
                "{field}"
            );
        }
        for serial in [1, 999] {
            let mut valid = supplemented.clone();
            valid["serial"] = json!(serial);
            assert!(serde_json::from_value::<super::Supplement>(valid).is_ok());
        }
    }

    #[test]
    fn assistance_retains_target_sequence_reason_and_original_input_across_lifecycle_changes() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(70), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (institution, manager, peer) = Box::pin(members(&database, &services, &mut supplier)).await;
                promote(&services, &institution).await;
                let body = confirmation(&services, &peer, &mobile(10)).await;
                let id = target(&body);
                let operator = services.current().read(&manager).await.expect("operator identity");
                let observer = database.connections(&database.user).recover().await.expect("observer");
                let before = observer.client().query_one("SELECT (SELECT sequence AS person_sequence FROM identity.persons WHERE id=$1) AS target, (SELECT sequence AS person_sequence FROM identity.persons WHERE id=$2) AS operator", &[&id, &operator.person_id()]).await.expect("sequences");
                let original = operation();
                let first = Box::pin(services.number().allocate_member(manager.clone(), original, id, assistance(&body))).await.expect("assisted allocation");
                assert_eq!((first.number().year(), first.number().serial()), (previous_year(), 1));
                let row = observer.client().query_one("SELECT allocation.source, allocation.reason, allocation.operator_id, allocation.sequence, allocation.operation_id, octet_length(allocation.seal) AS seal_size, allocation.revision_id AS confirmed_revision, (number.number).year, (number.number).serial, number.revision_id, person.sequence AS person_sequence FROM workforce.allocations allocation JOIN workforce.numbers number ON number.id=allocation.person_id AND number.revision_id=allocation.number_id JOIN identity.persons person ON person.id=number.id WHERE allocation.operation_id=$1", &[&original]).await.expect("independent permanent result before replay");
                assert_eq!(row.get::<_, &str>("source"), "assistance");
                assert_eq!(row.get::<_, &str>("reason"), "Verified original entry record");
                assert_eq!(row.get::<_, crate::identity::person::revision::Id>("operator_id"), operator.revision_id());
                assert_eq!(row.get::<_, i64>("sequence"), 1);
                assert_eq!(row.get::<_, i64>("person_sequence"), before.get::<_, i64>("target"));
                assert_eq!(row.get::<_, i32>("seal_size"), 32);
                assert_eq!(row.get::<_, crate::identity::person::revision::Id>("confirmed_revision"), serde_json::from_value(body["target"]["revisionId"].clone()).expect("confirmed revision"));
                assert_eq!(row.get::<_, super::Id>("revision_id"), first.id());
                assert_eq!(observer.client().query_one("SELECT sequence AS person_sequence FROM identity.persons WHERE id=$1", &[&operator.person_id()]).await.expect("operator sequence unchanged").get::<_, i64>(0), before.get::<_, i64>("operator"));
                assert_eq!(Box::pin(services.number().allocate_member(manager.clone(), original, id, assistance(&body))).await.expect("replay").id(), first.id());
                assert!(matches!(Box::pin(services.number().allocate_member(manager.clone(), original, operator.person_id(), assistance(&body))).await, Err(Error::Mismatch)), "path is part of the original input");
                for (field, value) in [("subtype", json!(803)), ("year", json!(previous_year()-1)), ("reason", json!("Another record"))] {
                    let mut changed = body.clone(); changed[field] = value;
                    assert!(matches!(Box::pin(services.number().allocate_member(manager.clone(), original, id, assistance(&changed))).await, Err(Error::Mismatch)), "{field}");
                }
                for (field, value) in [("accountId", json!(Uuid::new_v4())), ("personId", json!(Uuid::new_v4())), ("revisionId", json!(Uuid::new_v4())), ("mobile", json!("13700000000"))] {
                    let mut changed = body.clone(); changed["target"][field] = value;
                    assert!(matches!(Box::pin(services.number().allocate_member(manager.clone(), original, id, assistance(&changed))).await, Err(Error::Mismatch)), "{field}");
                }
                assert!(matches!(Box::pin(services.number().allocate_member(manager.clone(), operation(), id, assistance(&body))).await, Err(Error::Rejected(Rejection::Conflict))));
                assert!(matches!(Box::pin(services.number().allocate(peer.clone(), operation(), input(100))).await, Err(Error::Rejected(Rejection::Conflict))), "assistance also excludes later self-service");
                let second = Box::pin(register_numbered(&database, &services, &mut supplier, 11)).await;
                Box::pin(admit(&services, &institution, Entry::Institution, &mobile(11), Kind::Default)).await;
                let mut next = confirmation(&services, &second, &mobile(11)).await; next["subtype"] = json!(803);
                assert_eq!(Box::pin(services.number().allocate_member(manager.clone(), operation(), target(&next), assistance(&next))).await.expect("same category pool").number().serial(), 2);
                let cancellation = serde_json::from_value(json!({"personId":id,"revisionId":body["target"]["revisionId"],"mobile":mobile(10)})).expect("cancellation input");
                Box::pin(services.cancellation().cancel_account(institution, operation(), serde_json::from_value(body["target"]["accountId"].clone()).expect("account"), cancellation)).await.expect("real target cancellation");
                assert_eq!(Box::pin(services.number().allocate_member(manager.clone(), original, id, assistance(&body))).await.expect("original precedes current target eligibility").id(), first.id());
                let grant = services.grant().read_latest(&manager).await.expect("operator grant");
                Box::pin(services.grant().terminate(manager.clone(), operation(), grant.id())).await.expect("operator loses permission");
                assert!(matches!(Box::pin(services.number().allocate_member(manager.clone(), original, id, assistance(&body))).await, Err(Error::Denied)), "new request requires current basic permission");
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                assert_eq!(services.number().read_current(&second).await.expect("historical authority is not rechecked").expect("held").number().serial(), 2);
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded assistance lifecycle");
        }));
    }

    #[test]
    fn candidate_masks_identity_and_rechecks_all_allocation_eligibility() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(70), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (institution, manager, peer) = Box::pin(members(&database, &services, &mut supplier)).await;
                let service = services.number();
                let peer_mobile = mobile(10);
                let body = confirmation(&services, &peer, &peer_mobile).await;
                for operator in [&institution, &peer] {
                    assert!(matches!(Box::pin(service.candidate(operator, "13600000000".parse().expect("mobile"))).await, Err(Error::Denied)), "basic permission precedes target lookup");
                }
                assert!(matches!(Box::pin(service.candidate(&manager, peer_mobile.parse().expect("mobile"))).await, Err(Error::Rejected(Rejection::Absent))), "equal institutional levels are hidden");
                promote(&services, &institution).await;
                Box::pin(admit(&services, &manager, Entry::Domain, "13900000000", Kind::Default)).await;
                for mobile in ["13600000000", "13800000000", "13900000000"] {
                    assert!(matches!(Box::pin(service.candidate(&manager, mobile.parse().expect("mobile"))).await, Err(Error::Rejected(Rejection::Absent))), "unknown, self and higher institutional levels are hidden");
                }
                let identity = services.current().read(&peer).await.expect("independent target identity");
                let grant = services.grant().read_latest(&peer).await.expect("target Grant");
                let observer = database.connections(&database.user).recover().await.expect("observer");
                let snapshot = "SELECT jsonb_build_object('persons',(SELECT jsonb_agg(to_jsonb(person) ORDER BY id) FROM identity.persons person),'grants',(SELECT jsonb_agg(to_jsonb(grant_state) ORDER BY id) FROM access.grants grant_state),'allocations',(SELECT jsonb_agg(to_jsonb(allocation) ORDER BY id) FROM workforce.allocations allocation))::text";
                let before: String = observer.client().query_one(snapshot, &[]).await.expect("before candidate").get(0);
                let candidate = Box::pin(service.candidate(&manager, peer_mobile.parse().expect("mobile"))).await.expect("eligible candidate");
                assert_eq!(candidate.identity().id(), identity.id());
                assert_eq!(candidate.identity().person_id(), identity.person_id());
                assert_eq!(candidate.identity().revision_id(), identity.revision_id());
                assert_eq!(candidate.identity().name(), "H*");
                assert_eq!(candidate.identity().suffix(), identity.suffix());
                assert_eq!(candidate.identity().suffix().len(), 4);
                assert_eq!(candidate.mobile().as_ref(), peer_mobile);
                assert_eq!(candidate.grant_id(), grant.id());
                assert_eq!(observer.client().query_one(snapshot, &[]).await.expect("after candidate").get::<_, String>(0), before, "confirmation changes no business facts");
                let pause = Box::pin(services.grant().suspend(manager.clone(), operation(), grant.id(), serde_json::from_value(json!({"target":body["target"]})).expect("confirmation"))).await.expect("suspend target Grant");
                assert!(matches!(Box::pin(service.candidate(&manager, peer_mobile.parse().expect("mobile"))).await, Err(Error::Rejected(Rejection::StateConflict))));
                Box::pin(services.grant().resume(manager.clone(), operation(), pause.id(), serde_json::from_value(json!({"target":body["target"]})).expect("confirmation"))).await.expect("resume target Grant");
                let other = Box::pin(register_numbered(&database, &services, &mut supplier, 11)).await;
                assert!(matches!(Box::pin(service.candidate(&manager, mobile(11).parse().expect("mobile"))).await, Err(Error::Rejected(Rejection::Absent))), "no current Grant");
                Box::pin(admit(&services, &institution, Entry::Institution, &mobile(11), Kind::Default)).await;
                Box::pin(service.allocate(other.clone(), operation(), input(100))).await.expect("self-service allocation");
                assert!(matches!(Box::pin(service.candidate(&manager, mobile(11).parse().expect("mobile"))).await, Err(Error::Rejected(Rejection::Conflict))), "any allocation source excludes candidacy");
                let other_grant = services.grant().read_latest(&other).await.expect("other Grant");
                Box::pin(services.grant().terminate(other.clone(), operation(), other_grant.id())).await.expect("end target membership");
                assert!(matches!(Box::pin(service.candidate(&manager, mobile(11).parse().expect("mobile"))).await, Err(Error::Rejected(Rejection::Absent))), "absence precedes number history");
                let account_confirmation = json!({"personId":identity.person_id(),"revisionId":identity.revision_id(),"mobile":peer_mobile});
                Box::pin(services.suspension().suspend(institution.clone(), operation(), identity.id(), serde_json::from_value(account_confirmation.clone()).expect("confirmation"))).await.expect("suspend target account");
                Box::pin(service.candidate(&manager, peer_mobile.parse().expect("mobile"))).await.expect("account suspension does not suspend Grant");
                Box::pin(service.allocate_member(manager.clone(), operation(), identity.person_id(), assistance(&body))).await.expect("assistance for paused account");
                assert!(matches!(Box::pin(service.candidate(&manager, peer_mobile.parse().expect("mobile"))).await, Err(Error::Rejected(Rejection::Conflict))));
                Box::pin(services.grant().suspend(manager.clone(), operation(), grant.id(), serde_json::from_value(json!({"target":body["target"]})).expect("confirmation"))).await.expect("suspend already numbered target");
                assert!(matches!(Box::pin(service.candidate(&manager, peer_mobile.parse().expect("mobile"))).await, Err(Error::Rejected(Rejection::StateConflict))), "Grant state precedes number history");
                Box::pin(services.cancellation().cancel_account(institution.clone(), operation(), identity.id(), serde_json::from_value(account_confirmation).expect("cancellation"))).await.expect("cancel target");
                assert!(matches!(Box::pin(service.candidate(&manager, peer_mobile.parse().expect("mobile"))).await, Err(Error::Rejected(Rejection::Absent))), "cancelled identity remains hidden");

                services.revocation().revoke(manager.clone(), operation(), crate::identity::revocation::Scope::Current).await.expect("revoke manager session");
                assert!(matches!(Box::pin(service.candidate(&manager, "13600000000".parse().expect("mobile"))).await, Err(Error::Unauthenticated)), "session rechecked before target disclosure");
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded candidate eligibility");
        }));
    }

    #[test]
    fn correction_candidate_masks_targets_and_reuses_correction_eligibility() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(70), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (institution, manager, peer) = Box::pin(members(&database, &services, &mut supplier)).await;
                let service = services.number();
                let peer_mobile = mobile(10);
                let body = confirmation(&services, &peer, &peer_mobile).await;
                for operator in [&institution, &peer] {
                    assert!(matches!(Box::pin(service.correction_candidate(operator, "13600000000".parse().expect("mobile"))).await, Err(Error::Denied)), "basic permission precedes target lookup");
                }
                assert!(matches!(Box::pin(service.correction_candidate(&manager, peer_mobile.parse().expect("mobile"))).await, Err(Error::Rejected(Rejection::Absent))), "equal level hides even an unnumbered target");
                promote(&services, &institution).await;
                Box::pin(admit(&services, &manager, Entry::Domain, "13900000000", Kind::Default)).await;
                for mobile in ["13600000000", "13800000000", "13900000000"] {
                    assert!(matches!(Box::pin(service.correction_candidate(&manager, mobile.parse().expect("mobile"))).await, Err(Error::Rejected(Rejection::Absent))), "unknown, self and higher current members are hidden");
                }
                assert!(matches!(Box::pin(service.correction_candidate(&manager, peer_mobile.parse().expect("mobile"))).await, Err(Error::Rejected(Rejection::NumberAbsent))), "manageable target needs a current number");
                let other = Box::pin(register_numbered(&database, &services, &mut supplier, 11)).await;
                assert!(matches!(Box::pin(service.correction_candidate(&manager, mobile(11).parse().expect("mobile"))).await, Err(Error::Rejected(Rejection::NumberAbsent))), "absence of a Grant does not issue a number");
                let first = Box::pin(service.allocate_member(manager.clone(), operation(), target(&body), assistance(&body))).await.expect("numbered target");
                let identity = services.current().read(&peer).await.expect("independent identity");
                let observer = database.connections(&database.user).recover().await.expect("observer");
                let snapshot = "SELECT jsonb_build_object('persons',(SELECT jsonb_agg(to_jsonb(person) ORDER BY id) FROM identity.persons person),'grants',(SELECT jsonb_agg(to_jsonb(grant_state) ORDER BY id) FROM access.grants grant_state),'numbers',(SELECT jsonb_agg(to_jsonb(number) ORDER BY id) FROM workforce.numbers number),'revisions',(SELECT jsonb_agg(to_jsonb(revision) ORDER BY id) FROM workforce.revisions revision),'allocations',(SELECT jsonb_agg(to_jsonb(allocation) ORDER BY id) FROM workforce.allocations allocation),'corrections',(SELECT jsonb_agg(to_jsonb(correction) ORDER BY id) FROM workforce.corrections correction))::text";
                let before: String = observer.client().query_one(snapshot, &[]).await.expect("before candidate").get(0);
                let candidate = Box::pin(service.correction_candidate(&manager, peer_mobile.parse().expect("mobile"))).await.expect("correction candidate");
                assert_eq!(candidate.identity().id(), identity.id());
                assert_eq!(candidate.identity().person_id(), identity.person_id());
                assert_eq!(candidate.identity().revision_id(), identity.revision_id());
                assert_eq!(candidate.identity().name(), "H*");
                assert_eq!(candidate.identity().suffix(), identity.suffix());
                assert_eq!(candidate.identity().suffix().len(), 4);
                assert_eq!(candidate.mobile().as_ref(), peer_mobile);
                assert_eq!(candidate.number().id(), first.id());
                assert_eq!(candidate.number().number(), first.number());
                assert_eq!(observer.client().query_one(snapshot, &[]).await.expect("after candidate").get::<_, String>(0), before, "candidate changes no business facts");
                let grant = services.grant().read_latest(&peer).await.expect("target Grant");
                let pause = Box::pin(services.grant().suspend(manager.clone(), operation(), grant.id(), serde_json::from_value(json!({"target":body["target"]})).expect("confirmation"))).await.expect("suspend target Grant");
                Box::pin(service.correction_candidate(&manager, peer_mobile.parse().expect("mobile"))).await.expect("suspended target Grant remains correctable");
                Box::pin(services.grant().resume(manager.clone(), operation(), pause.id(), serde_json::from_value(json!({"target":body["target"]})).expect("confirmation"))).await.expect("resume target Grant");
                let office = Box::pin(services.role().create(institution.clone(), operation(), serde_json::from_value(json!({"name":"Senior target","level":600,"permissions":[]})).expect("office"))).await.expect("higher target role");
                let appointment = Box::pin(services.appointment().candidate(&institution, peer_mobile.parse().expect("mobile"))).await.expect("appointment confirmation");
                let assignment = serde_json::from_value(json!({"target":{"accountId":appointment.identity().id(),"personId":appointment.identity().person_id(),"revisionId":appointment.identity().revision_id(),"mobile":peer_mobile,"assignmentId":appointment.assignment_id()},"role":{"id":office.id(),"changeId":office.change_id()}})).expect("target appointment confirmation");
                Box::pin(services.appointment().assign(institution.clone(), operation(), assignment)).await.expect("promote target");
                assert!(matches!(Box::pin(service.correction_candidate(&manager, peer_mobile.parse().expect("mobile"))).await, Err(Error::Rejected(Rejection::Absent))), "higher numbered current member remains hidden");
                Box::pin(services.grant().terminate(peer.clone(), operation(), grant.id())).await.expect("end target Grant");
                let candidate = Box::pin(service.correction_candidate(&manager, peer_mobile.parse().expect("mobile"))).await.expect("ended Grant skips the higher institutional level");
                let correction = serde_json::from_value(json!({"revisionId":candidate.identity().revision_id(),"expected":candidate.number().id(),"replacement":{"subtype":902,"year":previous_year(),"serial":42},"reason":"Verified original record"})).expect("candidate confirmation");
                let corrected = Box::pin(service.correct(manager.clone(), operation(), candidate.identity().person_id(), correction)).await.expect("same eligibility for correction");
                let candidate = Box::pin(service.correction_candidate(&manager, peer_mobile.parse().expect("mobile"))).await.expect("current version after correction");
                assert_eq!(candidate.number().id(), corrected.id());
                assert_eq!(candidate.number().number(), corrected.number());
                let account_confirmation = json!({"personId":identity.person_id(),"revisionId":identity.revision_id(),"mobile":peer_mobile});
                Box::pin(services.suspension().suspend(institution.clone(), operation(), identity.id(), serde_json::from_value(account_confirmation.clone()).expect("confirmation"))).await.expect("suspend target account");
                Box::pin(service.correction_candidate(&manager, peer_mobile.parse().expect("mobile"))).await.expect("suspended target account remains correctable");
                Box::pin(services.cancellation().cancel_account(institution.clone(), operation(), identity.id(), serde_json::from_value(account_confirmation).expect("cancellation"))).await.expect("cancel target");
                assert!(matches!(Box::pin(service.correction_candidate(&manager, peer_mobile.parse().expect("mobile"))).await, Err(Error::Rejected(Rejection::Absent))), "cancelled person has no mobile lookup");
                services.revocation().revoke(manager.clone(), operation(), crate::identity::revocation::Scope::Current).await.expect("revoke manager session");
                assert!(matches!(Box::pin(service.correction_candidate(&manager, mobile(11).parse().expect("mobile"))).await, Err(Error::Unauthenticated)), "session rechecked before disclosure");
                assert!(services.current().read(&other).await.is_ok(), "unrelated account remains available");
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded correction candidate eligibility");
        }));
    }

    #[test]
    fn correction_holder_uses_current_occupancy_and_correction_eligibility() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(90), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (institution, manager, peer) = Box::pin(members(&database, &services, &mut supplier)).await;
                let service = services.number();
                let missing = super::Number::new(Subtype::Clinical, 1, 999).expect("valid unknown number");
                for operator in [&institution, &peer] {
                    assert!(matches!(Box::pin(service.correction_holder(operator, missing)).await, Err(Error::Denied)), "basic permission precedes holder lookup");
                }
                Box::pin(promote(&services, &institution)).await;
                assert!(matches!(Box::pin(service.correction_holder(&manager, missing)).await, Err(Error::Rejected(Rejection::Absent))), "unknown complete number is absent");
                let own = Box::pin(service.allocate(manager.clone(), operation(), input(904))).await.expect("manager number");
                assert!(matches!(Box::pin(service.correction_holder(&manager, own.number())).await, Err(Error::Rejected(Rejection::Absent))), "self remains hidden by correction eligibility");
                let body = confirmation(&services, &peer, &mobile(10)).await;
                let id = target(&body);
                let first = Box::pin(service.allocate_member(manager.clone(), operation(), id, assistance(&body))).await.expect("lower current member number");
                let identity = services.current().read(&peer).await.expect("target identity");
                let observer = database.connections(&database.user).recover().await.expect("observer");
                let snapshot = "SELECT jsonb_build_object('persons',(SELECT jsonb_agg(to_jsonb(person) ORDER BY id) FROM identity.persons person),'grants',(SELECT jsonb_agg(to_jsonb(grant_state) ORDER BY id) FROM access.grants grant_state),'numbers',(SELECT jsonb_agg(to_jsonb(number) ORDER BY id) FROM workforce.numbers number),'revisions',(SELECT jsonb_agg(to_jsonb(revision) ORDER BY id) FROM workforce.revisions revision),'allocations',(SELECT jsonb_agg(to_jsonb(allocation) ORDER BY id) FROM workforce.allocations allocation),'corrections',(SELECT jsonb_agg(to_jsonb(correction) ORDER BY id) FROM workforce.corrections correction))::text";
                let before: String = observer.client().query_one(snapshot, &[]).await.expect("before holder lookup").get(0);
                let holder = Box::pin(service.correction_holder(&manager, first.number())).await.expect("current lower-level holder");
                assert_eq!(holder.identity().person_id(), identity.person_id());
                assert_eq!(holder.identity().revision_id(), identity.revision_id());
                assert_eq!(holder.identity().name(), "H*");
                assert_eq!(holder.number().id(), first.id());
                assert_eq!(holder.number().number(), first.number());
                assert_eq!(observer.client().query_one(snapshot, &[]).await.expect("after holder lookup").get::<_, String>(0), before, "holder lookup changes no business facts");
                let correction_input = correction_body(&body, &first, 902, previous_year(), 42);
                let corrected = Box::pin(service.correct(manager.clone(), operation(), id, correction(&correction_input))).await.expect("move current occupancy");
                assert!(matches!(Box::pin(service.correction_holder(&manager, first.number())).await, Err(Error::Rejected(Rejection::Absent))), "a corrected-away historical number is absent");
                let holder = Box::pin(service.correction_holder(&manager, corrected.number())).await.expect("new current version");
                assert_eq!(holder.number().id(), corrected.id());
                for (level, name) in [(500, "Equal target"), (600, "Higher target")] {
                    let office = Box::pin(services.role().create(institution.clone(), operation(), serde_json::from_value(json!({"name":name,"level":level,"permissions":[]})).expect("office"))).await.expect("target office");
                    let appointment = Box::pin(services.appointment().candidate(&institution, mobile(10).parse().expect("mobile"))).await.expect("appointment confirmation");
                    let assignment = serde_json::from_value(json!({"target":{"accountId":appointment.identity().id(),"personId":appointment.identity().person_id(),"revisionId":appointment.identity().revision_id(),"mobile":mobile(10),"assignmentId":appointment.assignment_id()},"role":{"id":office.id(),"changeId":office.change_id()}})).expect("target appointment");
                    Box::pin(services.appointment().assign(institution.clone(), operation(), assignment)).await.expect("change target level");
                    assert!(matches!(Box::pin(service.correction_holder(&manager, corrected.number())).await, Err(Error::Rejected(Rejection::Absent))), "{name} current member is masked");
                }
                let account_confirmation = json!({"personId":identity.person_id(),"revisionId":identity.revision_id(),"mobile":mobile(10)});
                Box::pin(services.cancellation().cancel_account(institution, operation(), identity.id(), serde_json::from_value(account_confirmation).expect("cancellation"))).await.expect("cancel current holder");
                let holder = Box::pin(service.correction_holder(&manager, corrected.number())).await.expect("cancelled holder remains correctable");
                assert_eq!(holder.identity().person_id(), identity.person_id());
                assert_eq!(holder.identity().revision_id(), identity.revision_id());
                assert_eq!(holder.identity().name(), "H*");
                assert_eq!(holder.number().id(), corrected.id());
                let correction = serde_json::from_value(json!({"revisionId":holder.identity().revision_id(),"expected":holder.number().id(),"replacement":{"subtype":903,"year":previous_year(),"serial":43},"reason":"Verified original record"})).expect("holder correction confirmation");
                let final_revision = Box::pin(service.correct(manager.clone(), operation(), holder.identity().person_id(), correction)).await.expect("real correction from cancelled holder confirmation");
                assert_ne!(final_revision.id(), corrected.id());
                services.revocation().revoke(manager.clone(), operation(), crate::identity::revocation::Scope::Current).await.expect("revoke manager session");
                assert!(matches!(Box::pin(service.correction_holder(&manager, final_revision.number())).await, Err(Error::Unauthenticated)), "session is rechecked before holder disclosure");
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded correction holder eligibility");
        }));
    }

    #[test]
    fn management_rejects_suspended_or_ended_number_managers() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(45), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (institution, manager, peer) = Box::pin(members(&database, &services, &mut supplier)).await;
                let role = Box::pin(services.role().create_workforce(institution.clone(), operation(), serde_json::from_value(json!({"entry":"institution","name":"Numbers only","level":200,"permissions":["workforce.numbers"]})).expect("role input"))).await.expect("custom manager role");
                let candidate = Box::pin(services.assignment().candidate(&institution, Entry::Institution, mobile(10).parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("page"))).await.expect("role confirmation");
                let identity = candidate.identity();
                let target = json!({"accountId":identity.id(),"personId":identity.person_id(),"revisionId":identity.revision_id(),"mobile":mobile(10)});
                let mut assignment_target = target.clone();
                assignment_target["grantId"] = json!(candidate.grant_id());
                assignment_target["assignmentId"] = json!(candidate.assignment_id());
                Box::pin(services.assignment().assign(institution, operation(), serde_json::from_value(json!({"entry":"institution","target":assignment_target,"role":{"id":role.id(),"changeId":role.change_id()}})).expect("assignment input"))).await.expect("numbers-only operator");
                let missing=serde_json::from_value(json!(Uuid::new_v4())).expect("person");
                let correction_input=json!({"revisionId":Uuid::new_v4(),"expected":Uuid::new_v4(),"replacement":{"subtype":100,"year":2025,"serial":9},"reason":"Verified original record"});
                assert!(matches!(Box::pin(services.number().correct(peer.clone(),operation(),missing,correction(&correction_input))).await,Err(Error::Rejected(Rejection::Absent))),"active number manager reaches target validation");
                assert!(matches!(Box::pin(services.number().candidate(&peer, "13600000000".parse().expect("mobile"))).await, Err(Error::Rejected(Rejection::Absent))), "active custom role can look up targets");
                assert!(matches!(Box::pin(services.number().correction_candidate(&peer, "13600000000".parse().expect("mobile"))).await, Err(Error::Rejected(Rejection::Absent))), "active custom role can look up correction targets");
                Box::pin(services.grant().suspend(manager, operation(), candidate.grant_id(), serde_json::from_value(json!({"target":target})).expect("confirmation"))).await.expect("suspend manager Grant");
                assert!(matches!(Box::pin(services.number().candidate(&peer, "13600000000".parse().expect("mobile"))).await, Err(Error::Denied)), "suspended manager cannot look up targets");
                assert!(matches!(Box::pin(services.number().correction_candidate(&peer, "13600000000".parse().expect("mobile"))).await, Err(Error::Denied)), "suspended manager cannot look up correction targets");
                assert!(matches!(Box::pin(services.number().correct(peer.clone(),operation(),missing,correction(&correction_input))).await,Err(Error::Denied)),"suspended manager cannot correct");
                Box::pin(services.grant().terminate(peer.clone(), operation(), candidate.grant_id())).await.expect("manager exits");
                assert!(matches!(Box::pin(services.number().candidate(&peer, "13600000000".parse().expect("mobile"))).await, Err(Error::Denied)), "ended manager cannot look up targets");
                assert!(matches!(Box::pin(services.number().correction_candidate(&peer, "13600000000".parse().expect("mobile"))).await, Err(Error::Denied)), "ended manager cannot look up correction targets");
                assert!(matches!(Box::pin(services.number().correct(peer.clone(),operation(),missing,correction(&correction_input))).await,Err(Error::Denied)),"ended manager cannot correct");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded candidate authority checks");
        }));
    }

    #[test]
    fn assistance_rejects_ineligible_targets_stale_confirmation_and_years() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(70), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (institution, manager, peer) = Box::pin(members(&database, &services, &mut supplier)).await;
                let body = confirmation(&services, &peer, &mobile(10)).await;
                let id = target(&body);
                assert!(matches!(Box::pin(services.number().allocate_member(manager.clone(), operation(), id, assistance(&body))).await, Err(Error::Denied)), "higher Workforce level does not replace strictly higher institution level");
                promote(&services, &institution).await;
                for operator in [&institution, &peer] {
                    assert!(matches!(Box::pin(services.number().allocate_member(operator.clone(), operation(), id, assistance(&body))).await, Err(Error::Denied)), "institution privilege or ordinary membership alone is insufficient");
                }
                for year in [previous_year()+1, previous_year()+2] {
                    let mut changed = body.clone(); changed["year"] = json!(year);
                    assert!(matches!(Box::pin(services.number().allocate_member(manager.clone(), operation(), id, assistance(&changed))).await, Err(Error::Rejected(Rejection::Year))));
                }
                for (field, value) in [("personId", json!(Uuid::new_v4())), ("revisionId", json!(Uuid::new_v4())), ("mobile", json!("13700000000")), ("accountId", json!(Uuid::new_v4()))] {
                    let mut changed = body.clone(); changed["target"][field] = value;
                    assert!(matches!(Box::pin(services.number().allocate_member(manager.clone(), operation(), id, assistance(&changed))).await, Err(Error::Rejected(Rejection::TargetChanged))), "{field}");
                }
                let unadmitted = Box::pin(register_numbered(&database, &services, &mut supplier, 11)).await;
                let absent = confirmation(&services, &unadmitted, &mobile(11)).await;
                assert!(matches!(Box::pin(services.number().allocate_member(manager.clone(), operation(), target(&absent), assistance(&absent))).await, Err(Error::Rejected(Rejection::Absent))));
                let grant = services.grant().read_latest(&peer).await.expect("target grant");
                let pause = Box::pin(services.grant().suspend(manager.clone(), operation(), grant.id(), serde_json::from_value(json!({"target":body["target"]})).expect("pause"))).await.expect("suspended grant");
                assert!(matches!(Box::pin(services.number().allocate_member(manager.clone(), operation(), id, assistance(&body))).await, Err(Error::Rejected(Rejection::StateConflict))));
                Box::pin(services.grant().resume(manager.clone(), operation(), pause.id(), serde_json::from_value(json!({"target":body["target"]})).expect("resume"))).await.expect("active again");
                let owner = database.connections("postgres").recover().await.expect("isolated owner");
                Box::pin(services.suspension().suspend(institution.clone(), operation(), serde_json::from_value(body["target"]["accountId"].clone()).expect("account"), serde_json::from_value(json!({"personId":id,"revisionId":body["target"]["revisionId"],"mobile":mobile(10)})).expect("account suspension"))).await.expect("target account suspended while Grant stays active");
                let allocated = Box::pin(services.number().allocate_member(manager.clone(), operation(), id, assistance(&body))).await.expect("suspended account can receive management allocation");
                assert_eq!(allocated.number().serial(), 1);
                Box::pin(admit(&services, &institution, Entry::Institution, &mobile(11), Kind::Default)).await;
                Box::pin(services.number().allocate(unadmitted.clone(), operation(), input(100))).await.expect("self-service first");
                assert!(matches!(Box::pin(services.number().allocate_member(manager.clone(), operation(), target(&absent), assistance(&absent))).await, Err(Error::Rejected(Rejection::Conflict))));
                let cancelled = Box::pin(register_numbered(&database, &services, &mut supplier, 12)).await;
                Box::pin(admit(&services, &institution, Entry::Institution, &mobile(12), Kind::Default)).await;
                let cancelled = confirmation(&services, &cancelled, &mobile(12)).await;
                Box::pin(services.cancellation().cancel_account(institution, operation(), serde_json::from_value(cancelled["target"]["accountId"].clone()).expect("account"), serde_json::from_value(json!({"personId":cancelled["target"]["personId"],"revisionId":cancelled["target"]["revisionId"],"mobile":mobile(12)})).expect("cancellation"))).await.expect("cancel target");
                assert!(matches!(Box::pin(services.number().allocate_member(manager, operation(), target(&cancelled), assistance(&cancelled))).await, Err(Error::Rejected(Rejection::Absent))));
                owner.terminate().await.expect("owner ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded assistance rejections");
        }));
    }

    #[test]
    fn assistance_recovers_commit_faults_without_reallocating() {
        use fixture::database_proxy::{Fault, Proxy};
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["lost", "rollback", "unknown", "cancelled"] {
                timeout(Duration::from_secs(60), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(crate::identity::authentication::proxied(&database, &proxy), &supplier).await;
                    let (institution, manager, peer) = Box::pin(members(&database, &services, &mut supplier)).await;
                    promote(&services, &institution).await;
                    let body = confirmation(&services, &peer, &mobile(10)).await;
                    let id = target(&body);
                    let original = operation();
                    proxy.arm(match case { "lost" => Fault::LostCommit, "rollback" => Fault::RolledBack, "unknown" => Fault::UnknownCommit, "cancelled" => Fault::DelayedCommit, _ => unreachable!() });
                    let waiter = tokio::spawn({ let service = services.number().clone(); let actor = manager.clone(); let input = assistance(&body); async move { service.allocate_member(actor, original, id, input).await } });
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("actual commit intercepted");
                    if case == "cancelled" {
                        waiter.abort(); assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
                        services.close();
                        assert!(timeout(Duration::from_millis(50), services.drain()).await.is_err(), "accepted work retains its lifetime");
                        proxy.release(); services.drain().await.expect("work completed");
                    } else {
                        proxy.release();
                        let result = timeout(Duration::from_secs(15), waiter).await.expect("bounded completion").expect("waiter");
                        match case {
                            "rollback" => assert!(matches!(result, Err(Error::Unavailable(_)))),
                            "unknown" => assert!(matches!(result, Err(Error::Unknown(_)))),
                            _ => assert_eq!(result.expect("recovered original").number().serial(), 1),
                        }
                        services.close(); services.drain().await.expect("drained");
                    }
                    context.stop().await.expect("stopped");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    // An unknown outcome concludes once the recovery BEGIN fails; release only lets the
                    // proxy forward the original COMMIT. Cross the target's person lock before reading.
                    observer.client().batch_execute("BEGIN").await.expect("observation transaction");
                    observer.client().query_one("SELECT id FROM identity.persons WHERE id = $1 FOR UPDATE", &[&id]).await.expect("original backend released the target's person lock");
                    let row = observer.client().query_one("SELECT (SELECT count(*) FROM workforce.allocations WHERE operation_id=$1) AS allocations, (SELECT count(*) FROM workforce.revisions WHERE person_id=$2) AS revisions, (SELECT count(*) FROM workforce.numbers WHERE id=$2) AS numbers, (SELECT coalesce(max(sequence),0) FROM workforce.numbers WHERE id=$2) AS sequence", &[&original, &id]).await.expect("durable facts before replay");
                    for field in ["allocations", "revisions", "numbers"] { assert_eq!(row.get::<_, i64>(field), i64::from(case != "rollback"), "{case}: {field}"); }
                    assert_eq!(row.get::<_, i64>("sequence"), i64::from(case != "rollback"));
                    observer.client().batch_execute("COMMIT").await.expect("observation complete");
                    proxy.stop().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    Box::pin(services.number().allocate_member(manager, original, id, assistance(&body))).await.expect("original or explicit retry after proven rollback");
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM workforce.allocations", &[]).await.expect("one allocation").get::<_, i64>(0), 1);
                    observer.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded assistance commit fault");
            }
        }));
    }

    #[test]
    fn assistance_samples_after_pool_wait_and_rejects_damaged_retained_facts() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (institution, manager, peer) = Box::pin(members(&database, &services, &mut supplier)).await;
                promote(&services, &institution).await;
                let body = confirmation(&services, &peer, &mobile(10)).await;
                let id = target(&body);
                let original = operation();
                let owner = database.connections("postgres").recover().await.expect("independent observer");
                let blocker = database.connections("postgres").recover().await.expect("pool holder");
                blocker.client().batch_execute("BEGIN").await.expect("begin");
                super::command::pools(blocker.client(), vec![(8, previous_year())]).await.expect("hold pool");
                let pid:i32 = blocker.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("holder pid").get(0);
                let waiting = tokio::spawn({ let service=services.number().clone(); let actor=manager.clone(); let input=assistance(&body); async move { service.allocate_member(actor, original, id, input).await } });
                timeout(Duration::from_secs(3), async { while !owner.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)) AND query LIKE 'SELECT pg_catalog.pg_advisory_xact_lock(%')", &[&pid]).await.expect("actual wait graph").get::<_,bool>(0) { tokio::task::yield_now().await; } }).await.expect("allocation waits for pool");
                let released:chrono::DateTime<chrono::Utc> = blocker.client().query_one("SELECT clock_timestamp()", &[]).await.expect("release time").get(0);
                blocker.client().batch_execute("COMMIT").await.expect("release");
                waiting.await.expect("waiter").expect("allocation");
                let allocated:chrono::DateTime<chrono::Utc> = owner.client().query_one("SELECT allocated_at FROM workforce.allocations WHERE operation_id=$1", &[&original]).await.expect("actual allocation time").get(0);
                assert!(allocated>=released,"time is sampled after the pool lock");
                blocker.terminate().await.expect("holder ended");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica; UPDATE workforce.revisions SET number.year=EXTRACT(YEAR FROM now() AT TIME ZONE 'Asia/Shanghai')::integer; UPDATE workforce.numbers SET number.year=EXTRACT(YEAR FROM now() AT TIME ZONE 'Asia/Shanghai')::integer; COMMIT").await.expect("corrupt assistance year");
                assert!(matches!(services.number().verify(owner.client(), chrono::Utc::now()).await, Err(Error::Corrupt(_))), "startup rule checks retained earlier year");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica; UPDATE workforce.revisions SET number.year=EXTRACT(YEAR FROM now() AT TIME ZONE 'Asia/Shanghai')::integer-1; UPDATE workforce.numbers SET number.year=EXTRACT(YEAR FROM now() AT TIME ZONE 'Asia/Shanghai')::integer-1; DELETE FROM workforce.revisions; COMMIT").await.expect("remove referenced number version");
                assert!(matches!(Box::pin(services.number().allocate_member(manager, original, id, assistance(&body))).await, Err(Error::Corrupt(_))), "missing revision is damage, never an absent original");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let context = crate::runtime::Context::new(database.connections(&database.user)).await.expect("restart context");
                let result = Box::pin(Services::start(crate::database::Barrier::begin(context.clone()).await.expect("startup barrier"), crate::identity::authentication::inventory(crate::identity::authentication::keys()), crate::startup::Settings::new(4,crate::identity::message::Limits::default(),crate::identity::authentication::session_settings(),crate::registration::Limits::default(),crate::identity::authentication::avatar_settings()).expect("settings"), supplier.client(), crate::identity::authentication::unused_client().await)).await;
                assert!(matches!(result,Err(crate::startup::Error::Number(Error::Corrupt(_)))),"startup rejects missing allocation revision");
                context.stop().await.expect("failed startup stopped");
                owner.terminate().await.expect("owner ended");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded assistance lock and integrity checks");
        }));
    }

    #[test]
    fn supplement_retains_complete_number_operator_time_and_replay() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                let (institution, manager, peer) = Box::pin(members(&database, &services, &mut supplier)).await;
                Box::pin(promote(&services, &institution)).await;
                let candidate = Box::pin(services.number().candidate(&manager, mobile(10).parse().expect("mobile"))).await.expect("candidate");
                let body = json!({"subtype":801,"year":previous_year(),"serial":42,"reason":"  Verified historical badge 801  ","target":{"accountId":candidate.identity().id(),"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":candidate.mobile().as_ref()}});
                let id = target(&body);
                let operator = services.current().read(&manager).await.expect("operator identity");
                let observer = database.connections(&database.user).recover().await.expect("observer");
                let before = observer.client().query_one("SELECT (SELECT sequence AS person_sequence FROM identity.persons WHERE id=$1) AS target, (SELECT sequence AS person_sequence FROM identity.persons WHERE id=$2) AS operator, clock_timestamp() AS started", &[&id, &operator.person_id()]).await.expect("sequences");
                let started: chrono::DateTime<chrono::Utc> = before.get("started");
                let original = operation();
                let first = Box::pin(services.number().supplement(manager.clone(), original, id, supplement(&body))).await.expect("historical supplement");
                let finished: chrono::DateTime<chrono::Utc> = observer.client().query_one("SELECT clock_timestamp()", &[]).await.expect("finished").get(0);
                assert_eq!((first.number().subtype(), first.number().year(), first.number().serial()), (Subtype::Company, previous_year(), 42));
                let row = observer.client().query_one("SELECT allocation.source, allocation.reason, allocation.operator_id, allocation.allocated_at, allocation.sequence, allocation.operation_id, octet_length(allocation.seal) AS seal_size, allocation.revision_id AS confirmed_revision, (number.number).subtype, (number.number).year, (number.number).serial, number.revision_id, person.sequence AS person_sequence FROM workforce.allocations allocation JOIN workforce.numbers number ON number.id=allocation.person_id AND number.revision_id=allocation.number_id JOIN identity.persons person ON person.id=number.id WHERE allocation.operation_id=$1", &[&original]).await.expect("permanent supplement row");
                assert_eq!(row.get::<_, &str>("source"), "supplement");
                assert_eq!(row.get::<_, &str>("reason"), "Verified historical badge 801");
                assert_eq!(row.get::<_, crate::identity::person::revision::Id>("operator_id"), operator.revision_id());
                assert_eq!(row.get::<_, crate::identity::person::revision::Id>("confirmed_revision"), candidate.identity().revision_id());
                let allocated_at: chrono::DateTime<chrono::Utc> = row.get("allocated_at");
                assert!(allocated_at >= started && allocated_at <= finished, "stores current transaction time, never entry year");
                assert_eq!(row.get::<_, i64>("sequence"), 1);
                assert_eq!(row.get::<_, i64>("person_sequence"), before.get::<_, i64>("target"));
                assert_eq!(row.get::<_, i32>("seal_size"), 32);
                assert_eq!((row.get::<_, i16>("subtype"), row.get::<_, i32>("year"), row.get::<_, i16>("serial")), (801, previous_year(), 42));
                assert_eq!(row.get::<_, super::Id>("revision_id"), first.id());
                assert_eq!(observer.client().query_one("SELECT sequence AS person_sequence FROM identity.persons WHERE id=$1", &[&operator.person_id()]).await.expect("operator sequence unchanged").get::<_, i64>(0), before.get::<_, i64>("operator"));
                assert_eq!(Box::pin(services.number().supplement(manager.clone(), original, id, supplement(&body))).await.expect("replay").id(), first.id());
                assert!(matches!(Box::pin(services.number().supplement(manager.clone(), original, operator.person_id(), supplement(&body))).await, Err(Error::Mismatch)), "path bound");
                for (field, value) in [("subtype", json!(803)), ("year", json!(previous_year()-1)), ("serial", json!(43)), ("reason", json!("Other reason"))] {
                    let mut changed = body.clone(); changed[field] = value;
                    assert!(matches!(Box::pin(services.number().supplement(manager.clone(), original, id, supplement(&changed))).await, Err(Error::Mismatch)), "{field}");
                }
                for (field, value) in [("accountId", json!(Uuid::new_v4())), ("personId", json!(Uuid::new_v4())), ("revisionId", json!(Uuid::new_v4())), ("mobile", json!("13700000000"))] {
                    let mut changed = body.clone(); changed["target"][field] = value;
                    assert!(matches!(Box::pin(services.number().supplement(manager.clone(), original, id, supplement(&changed))).await, Err(Error::Mismatch)), "{field}");
                }
                let assist_body = Box::pin(confirmation(&services, &peer, &mobile(10))).await;
                assert!(matches!(Box::pin(services.number().allocate_member(manager.clone(), original, id, assistance(&assist_body))).await, Err(Error::Mismatch)), "cross-route replay rejected");
                assert!(matches!(Box::pin(services.number().allocate(manager.clone(), original, input(801))).await, Err(Error::Mismatch)), "self-service replay rejected");
                assert!(matches!(Box::pin(services.number().supplement(manager.clone(), operation(), id, supplement(&body))).await, Err(Error::Rejected(Rejection::Conflict))));
                assert!(matches!(Box::pin(services.number().allocate_member(manager.clone(), operation(), id, assistance(&assist_body))).await, Err(Error::Rejected(Rejection::Conflict))));
                assert!(matches!(Box::pin(services.number().allocate(peer.clone(), operation(), input(100))).await, Err(Error::Rejected(Rejection::Conflict))));
                assert!(matches!(Box::pin(services.number().candidate(&manager, mobile(10).parse().expect("mobile"))).await, Err(Error::Rejected(Rejection::Conflict))));
                let cancellation = serde_json::from_value(json!({"personId":id,"revisionId":body["target"]["revisionId"],"mobile":mobile(10)})).expect("cancellation input");
                Box::pin(services.cancellation().cancel_account(institution, operation(), serde_json::from_value(body["target"]["accountId"].clone()).expect("account"), cancellation)).await.expect("cancel target");
                assert_eq!(Box::pin(services.number().supplement(manager, original, id, supplement(&body))).await.expect("replay after target cancellation").id(), first.id());
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded supplement lifecycle");
        }));
    }

    #[test]
    fn supplement_serializes_pool_occupancy_with_assistance_and_self_service() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(
                Duration::from_secs(75),
                Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) =
                        Box::pin(start(database.connections(&database.user), &supplier)).await;
                    let (institution, manager, peer) =
                        Box::pin(members(&database, &services, &mut supplier)).await;
                    Box::pin(promote(&services, &institution)).await;
                    let body =
                        Box::pin(supplement_confirmation(&services, &peer, &mobile(10), 42)).await;
                    Box::pin(services.number().supplement(
                        manager.clone(),
                        operation(),
                        target(&body),
                        supplement(&body),
                    ))
                    .await
                    .expect("initial supplement");
                    let second =
                        Box::pin(register_numbered(&database, &services, &mut supplier, 11)).await;
                    let third =
                        Box::pin(register_numbered(&database, &services, &mut supplier, 12)).await;
                    let fourth =
                        Box::pin(register_numbered(&database, &services, &mut supplier, 13)).await;
                    let fifth =
                        Box::pin(register_numbered(&database, &services, &mut supplier, 14)).await;
                    for index in 11..=14 {
                        Box::pin(admit(
                            &services,
                            &institution,
                            Entry::Institution,
                            &mobile(index),
                            Kind::Default,
                        ))
                        .await;
                    }
                    let mut occupied =
                        Box::pin(supplement_confirmation(&services, &second, &mobile(11), 42))
                            .await;
                    occupied["subtype"] = json!(803);
                    assert!(
                        matches!(
                            Box::pin(services.number().supplement(
                                manager.clone(),
                                operation(),
                                target(&occupied),
                                supplement(&occupied)
                            ))
                            .await,
                            Err(Error::Rejected(Rejection::Occupied))
                        ),
                        "category pool shares occupancy across subtypes"
                    );

                    // Concurrent supplements for the same (category, year, serial) allow exactly one winner.
                    let body_second =
                        Box::pin(supplement_confirmation(&services, &second, &mobile(11), 1)).await;
                    let body_third =
                        Box::pin(supplement_confirmation(&services, &third, &mobile(12), 1)).await;
                    let (res_second, res_third) = tokio::join!(
                        Box::pin(services.number().supplement(
                            manager.clone(),
                            operation(),
                            target(&body_second),
                            supplement(&body_second)
                        )),
                        Box::pin(services.number().supplement(
                            manager.clone(),
                            operation(),
                            target(&body_third),
                            supplement(&body_third)
                        ))
                    );
                    let (remaining_actor, remaining_mobile) = match (res_second, res_third) {
                        (Ok(rev), Err(Error::Rejected(Rejection::Occupied))) => {
                            assert_eq!(rev.number().serial(), 1);
                            (&third, mobile(12))
                        }
                        (Err(Error::Rejected(Rejection::Occupied)), Ok(rev)) => {
                            assert_eq!(rev.number().serial(), 1);
                            (&second, mobile(11))
                        }
                        _ => panic!("expected one winner and one Occupied conflict"),
                    };

                    // Concurrent assisted allocation and historical supplement on the same earlier-year pool.
                    let assist_remaining =
                        Box::pin(confirmation(&services, remaining_actor, &remaining_mobile)).await;
                    let record_fourth =
                        Box::pin(supplement_confirmation(&services, &fourth, &mobile(13), 3)).await;
                    let (assisted, supplemented) = tokio::join!(
                        Box::pin(services.number().allocate_member(
                            manager.clone(),
                            operation(),
                            target(&assist_remaining),
                            assistance(&assist_remaining)
                        )),
                        Box::pin(services.number().supplement(
                            manager.clone(),
                            operation(),
                            target(&record_fourth),
                            supplement(&record_fourth)
                        ))
                    );
                    assert_eq!(
                        assisted
                            .expect("assisted takes smallest free serial")
                            .number()
                            .serial(),
                        2
                    );
                    assert_eq!(
                        supplemented
                            .expect("supplemented takes requested serial")
                            .number()
                            .serial(),
                        3
                    );

                    // Concurrent current-year supplement and self-service allocation on the same current-year pool.
                    let mut current_record =
                        Box::pin(supplement_confirmation(&services, &fifth, &mobile(14), 2)).await;
                    current_record["year"] = json!(previous_year() + 1);
                    let (recorded_current, self_allocated) = tokio::join!(
                        Box::pin(services.number().supplement(
                            manager.clone(),
                            operation(),
                            target(&current_record),
                            supplement(&current_record)
                        )),
                        Box::pin(services.number().allocate(manager, operation(), input(802)))
                    );
                    let recorded_current = recorded_current.expect("current-year supplement");
                    let self_allocated =
                        self_allocated.expect("self-service on same current-year pool");
                    assert_eq!(
                        (
                            recorded_current.number().year(),
                            recorded_current.number().serial()
                        ),
                        (previous_year() + 1, 2)
                    );
                    assert_eq!(
                        (
                            self_allocated.number().year(),
                            self_allocated.number().serial()
                        ),
                        (previous_year() + 1, 1)
                    );

                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    let (context, services) =
                        Box::pin(start(database.connections(&database.user), &supplier)).await;
                    assert_eq!(
                        services
                            .number()
                            .read_current(&fifth)
                            .await
                            .expect("verified on restart")
                            .expect("held")
                            .id(),
                        recorded_current.id()
                    );
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    assert!(
                        timeout(Duration::from_millis(30), supplier.request())
                            .await
                            .is_err()
                    );
                    supplier.stop().await;
                }),
            )
            .await
            .expect("bounded supplement pool concurrency");
        }));
    }

    #[test]
    fn supplement_rejects_ineligible_targets_stale_confirmation_and_future_years() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                let (institution, manager, peer) = Box::pin(members(&database, &services, &mut supplier)).await;
                let body = Box::pin(supplement_confirmation(&services, &peer, &mobile(10), 7)).await;
                let id = target(&body);
                assert!(matches!(Box::pin(services.number().supplement(manager.clone(), operation(), id, supplement(&body))).await, Err(Error::Denied)), "strictly lower institution level required");
                Box::pin(promote(&services, &institution)).await;
                for operator in [&institution, &peer] {
                    assert!(matches!(Box::pin(services.number().supplement(operator.clone(), operation(), id, supplement(&body))).await, Err(Error::Denied)));
                }
                let mut future = body.clone();
                future["year"] = json!(previous_year() + 2);
                assert!(matches!(Box::pin(services.number().supplement(manager.clone(), operation(), id, supplement(&future))).await, Err(Error::Rejected(Rejection::Year))));
                for (field, value) in [("personId", json!(Uuid::new_v4())), ("revisionId", json!(Uuid::new_v4())), ("mobile", json!("13700000000")), ("accountId", json!(Uuid::new_v4()))] {
                    let mut changed = body.clone(); changed["target"][field] = value;
                    assert!(matches!(Box::pin(services.number().supplement(manager.clone(), operation(), id, supplement(&changed))).await, Err(Error::Rejected(Rejection::TargetChanged))), "{field}");
                }
                let unadmitted = Box::pin(register_numbered(&database, &services, &mut supplier, 11)).await;
                let absent = Box::pin(supplement_confirmation(&services, &unadmitted, &mobile(11), 8)).await;
                assert!(matches!(Box::pin(services.number().supplement(manager.clone(), operation(), target(&absent), supplement(&absent))).await, Err(Error::Rejected(Rejection::Absent))));
                let grant = services.grant().read_latest(&peer).await.expect("target grant");
                let pause = Box::pin(services.grant().suspend(manager.clone(), operation(), grant.id(), serde_json::from_value(json!({"target":body["target"]})).expect("pause"))).await.expect("suspended grant");
                assert!(matches!(Box::pin(services.number().supplement(manager.clone(), operation(), id, supplement(&body))).await, Err(Error::Rejected(Rejection::StateConflict))));
                Box::pin(services.grant().resume(manager.clone(), operation(), pause.id(), serde_json::from_value(json!({"target":body["target"]})).expect("resume"))).await.expect("active again");
                Box::pin(services.suspension().suspend(institution.clone(), operation(), serde_json::from_value(body["target"]["accountId"].clone()).expect("account"), serde_json::from_value(json!({"personId":id,"revisionId":body["target"]["revisionId"],"mobile":mobile(10)})).expect("account suspension"))).await.expect("target account suspended while Grant stays active");
                let supplemented = Box::pin(services.number().supplement(manager.clone(), operation(), id, supplement(&body))).await.expect("supplement succeeds for suspended account with active Grant");
                assert_eq!(supplemented.number().serial(), 7);
                let cancelled = Box::pin(register_numbered(&database, &services, &mut supplier, 12)).await;
                Box::pin(admit(&services, &institution, Entry::Institution, &mobile(12), Kind::Default)).await;
                let cancelled = Box::pin(supplement_confirmation(&services, &cancelled, &mobile(12), 9)).await;
                Box::pin(services.cancellation().cancel_account(institution, operation(), serde_json::from_value(cancelled["target"]["accountId"].clone()).expect("account"), serde_json::from_value(json!({"personId":cancelled["target"]["personId"],"revisionId":cancelled["target"]["revisionId"],"mobile":mobile(12)})).expect("cancellation"))).await.expect("cancel target");
                assert!(matches!(Box::pin(services.number().supplement(manager, operation(), target(&cancelled), supplement(&cancelled))).await, Err(Error::Rejected(Rejection::Absent))));
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded supplement rejections");
        }));
    }

    #[test]
    fn supplement_recovers_commit_faults_and_verifies_retained_facts() {
        use fixture::database_proxy::{Fault, Proxy};
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["lost", "rollback", "unknown", "cancelled"] {
                timeout(Duration::from_secs(60), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = Box::pin(start(crate::identity::authentication::proxied(&database, &proxy), &supplier)).await;
                    let (institution, manager, peer) = Box::pin(members(&database, &services, &mut supplier)).await;
                    Box::pin(promote(&services, &institution)).await;
                    let body = Box::pin(supplement_confirmation(&services, &peer, &mobile(10), 19)).await;
                    let id = target(&body);
                    let original = operation();
                    proxy.arm(match case { "lost" => Fault::LostCommit, "rollback" => Fault::RolledBack, "unknown" => Fault::UnknownCommit, "cancelled" => Fault::DelayedCommit, _ => unreachable!() });
                    let waiter = tokio::spawn({ let service = services.number().clone(); let actor = manager.clone(); let input = supplement(&body); async move { service.supplement(actor, original, id, input).await } });
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("actual commit intercepted");
                    if case == "cancelled" {
                        waiter.abort(); assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
                        services.close();
                        assert!(timeout(Duration::from_millis(50), services.drain()).await.is_err(), "accepted work retains its lifetime");
                        proxy.release(); services.drain().await.expect("work completed");
                    } else {
                        proxy.release();
                        let result = timeout(Duration::from_secs(15), waiter).await.expect("bounded completion").expect("waiter");
                        match case {
                            "rollback" => assert!(matches!(result, Err(Error::Unavailable(_)))),
                            "unknown" => assert!(matches!(result, Err(Error::Unknown(_)))),
                            _ => assert_eq!(result.expect("recovered original").number().serial(), 19),
                        }
                        services.close(); services.drain().await.expect("drained");
                    }
                    context.stop().await.expect("stopped");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    // An unknown outcome concludes once the recovery BEGIN fails; release only lets the
                    // proxy forward the original COMMIT. Cross the target's person lock before reading.
                    observer.client().batch_execute("BEGIN").await.expect("observation transaction");
                    observer.client().query_one("SELECT id FROM identity.persons WHERE id = $1 FOR UPDATE", &[&id]).await.expect("original backend released the target's person lock");
                    let row = observer.client().query_one("SELECT (SELECT count(*) FROM workforce.allocations WHERE operation_id=$1) AS allocations, (SELECT count(*) FROM workforce.revisions WHERE person_id=$2) AS revisions, (SELECT count(*) FROM workforce.numbers WHERE id=$2) AS numbers, (SELECT coalesce(max(sequence),0) FROM workforce.numbers WHERE id=$2) AS sequence", &[&original, &id]).await.expect("durable facts before replay");
                    for field in ["allocations", "revisions", "numbers"] { assert_eq!(row.get::<_, i64>(field), i64::from(case != "rollback"), "{case}: {field}"); }
                    assert_eq!(row.get::<_, i64>("sequence"), i64::from(case != "rollback"));
                    observer.client().batch_execute("COMMIT").await.expect("observation complete");
                    proxy.stop().await;
                    let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                    Box::pin(services.number().supplement(manager.clone(), original, id, supplement(&body))).await.expect("original or explicit retry after proven rollback");
                    if case == "cancelled" {
                        let owner = database.connections("postgres").recover().await.expect("owner");
                        owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica; UPDATE workforce.revisions SET number.year=EXTRACT(YEAR FROM now() AT TIME ZONE 'Asia/Shanghai')::integer+1; UPDATE workforce.numbers SET number.year=EXTRACT(YEAR FROM now() AT TIME ZONE 'Asia/Shanghai')::integer+1; COMMIT").await.expect("corrupt supplement year to future");
                        assert!(matches!(services.number().verify(owner.client(), chrono::Utc::now()).await, Err(Error::Corrupt(_))), "startup rule rejects future supplement year");
                        owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica; UPDATE workforce.revisions SET number.year=EXTRACT(YEAR FROM now() AT TIME ZONE 'Asia/Shanghai')::integer-1; UPDATE workforce.numbers SET number.year=EXTRACT(YEAR FROM now() AT TIME ZONE 'Asia/Shanghai')::integer-1; COMMIT").await.expect("restoring the original year");
                        services.number().verify(owner.client(), chrono::Utc::now()).await.expect("restored supplement passes startup verification");
                        owner.terminate().await.expect("owner ended");
                    }
                    observer.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded supplement commit fault");
            }
        }));
    }

    fn input(subtype: i16) -> Input {
        serde_json::from_value(json!({"subtype":subtype})).expect("typed allocation")
    }

    fn correction(value: &serde_json::Value) -> super::Correction {
        serde_json::from_value(value.clone()).expect("typed correction")
    }

    fn correction_body(
        confirmation: &serde_json::Value,
        before: &super::Revision,
        subtype: i16,
        year: i32,
        serial: i16,
    ) -> serde_json::Value {
        json!({"revisionId":confirmation["target"]["revisionId"],"expected":before.id(),
            "replacement":{"subtype":subtype,"year":year,"serial":serial},"reason":"Verified original personnel record"})
    }

    #[test]
    fn correction_requires_complete_valid_confirmation_and_reason() {
        let body = json!({"revisionId":Uuid::new_v4(),"expected":Uuid::new_v4(),
            "replacement":{"subtype":801,"year":2025,"serial":42},"reason":"Verified record"});
        for field in ["revisionId", "expected", "replacement", "reason"] {
            let mut changed = body.clone();
            changed.as_object_mut().expect("object").remove(field);
            assert!(
                serde_json::from_value::<super::Correction>(changed).is_err(),
                "{field}"
            );
        }
        for (field, value) in [
            ("revisionId", json!("bad")),
            ("expected", json!(null)),
            ("reason", json!(" \n")),
            ("reason", json!("a\u{0000}b")),
            ("extra", json!(true)),
        ] {
            let mut changed = body.clone();
            changed[field] = value;
            assert!(
                serde_json::from_value::<super::Correction>(changed).is_err(),
                "{field}"
            );
        }
        for (field, value) in [
            ("subtype", json!(800)),
            ("year", json!(0)),
            ("year", json!(-1)),
            ("serial", json!(0)),
            ("serial", json!(1000)),
            ("extra", json!(true)),
        ] {
            let mut changed = body.clone();
            changed["replacement"][field] = value;
            assert!(
                serde_json::from_value::<super::Correction>(changed).is_err(),
                "{field}"
            );
        }
        assert!(serde_json::from_value::<super::Correction>(body).is_ok());
    }

    #[test]
    fn correction_changes_pools_preserves_history_and_rejects_old_confirmations() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(90), Box::pin(async {
                let database=Box::pin(Database::initialized(Arc::clone(&server))).await;
                let mut supplier=Supplier::new().await;
                let (context,services)=Box::pin(start(database.connections(&database.user),&supplier)).await;
                let (institution,manager,peer)=Box::pin(members(&database,&services,&mut supplier)).await;
                Box::pin(promote(&services,&institution)).await;
                let confirmed=confirmation(&services,&peer,&mobile(10)).await;
                let id=target(&confirmed);
                let first=Box::pin(services.number().allocate_member(manager.clone(),operation(),id,assistance(&confirmed))).await.expect("original number");
                let body=correction_body(&confirmed,&first,802,previous_year(),1);
                let original=operation();
                let second=Box::pin(services.number().correct(manager.clone(),original,id,correction(&body))).await.expect("same-category subtype correction");
                assert_ne!(first.id(),second.id());
                assert_eq!(second.number(),super::Number::new(Subtype::Security,previous_year(),1).expect("expected number"));
                let observer=database.connections(&database.user).recover().await.expect("observer");
                let operator=services.current().read(&manager).await.expect("operator");
                assert!(observer.client().query_one("SELECT state.tenure_id=allocation.id AND state.version=2 FROM workforce.numbers state JOIN workforce.allocations allocation ON allocation.person_id=state.id WHERE state.id=$1", &[&id]).await.expect("same slot tenure").get::<_,bool>(0));
                let row=observer.client().query_one("SELECT correction.before_id, correction.after_id, correction.revision_id, correction.operator_id, correction.reason, correction.sequence, state.sequence AS state_sequence, correction.corrected_at >= allocation.allocated_at AS ordered, correction.xmin = allocation.xmin AS same_transaction FROM workforce.corrections correction JOIN workforce.numbers state ON state.id=correction.person_id JOIN workforce.allocations allocation ON allocation.person_id=state.id WHERE correction.operation_id=$1", &[&original]).await.expect("permanent facts before replay");
                assert_eq!(row.get::<_,super::Id>("before_id"),first.id());
                assert_eq!(row.get::<_,super::Id>("after_id"),second.id());
                assert_eq!(row.get::<_,crate::identity::person::revision::Id>("operator_id"),operator.revision_id());
                assert_eq!(row.get::<_,crate::identity::person::revision::Id>("revision_id"),correction(&body).revision_id);
                assert_eq!(row.get::<_,&str>("reason"),"Verified original personnel record");
                assert_eq!(row.get::<_,i64>("sequence"),2);
                assert_eq!(row.get::<_,i64>("state_sequence"),2);
                assert!(row.get::<_,bool>("ordered")); assert!(!row.get::<_,bool>("same_transaction"));
                let third_person=Box::pin(register_numbered(&database,&services,&mut supplier,11)).await;
                Box::pin(admit(&services,&institution,Entry::Institution,&mobile(11),Kind::Default)).await;
                let other=confirmation(&services,&third_person,&mobile(11)).await;
                let allocated=Box::pin(services.number().allocate_member(manager.clone(),operation(),target(&other),assistance(&other))).await.expect("same pool remains occupied");
                assert_eq!(allocated.number().serial(),2);
                let mut occupied=correction_body(&confirmed,&second,804,previous_year(),2);
                assert!(matches!(Box::pin(services.number().correct(manager.clone(),operation(),id,correction(&occupied))).await,Err(Error::Rejected(Rejection::Occupied))));
                occupied["replacement"]["serial"]=json!(1); occupied["replacement"]["subtype"]=json!(802);
                assert!(matches!(Box::pin(services.number().correct(manager.clone(),operation(),id,correction(&occupied))).await,Err(Error::Rejected(Rejection::Unchanged))));
                let cross=correction_body(&confirmed,&second,902,previous_year()-1,42);
                let third=Box::pin(services.number().correct(manager.clone(),operation(),id,correction(&cross))).await.expect("cross-pool correction need not take smallest serial");
                assert!(observer.client().query_one("SELECT state.tenure_id=correction.id AND state.version=3 FROM workforce.numbers state JOIN workforce.corrections correction ON correction.after_id=state.revision_id WHERE state.id=$1", &[&id]).await.expect("new slot tenure").get::<_,bool>(0));
                let fourth_person=Box::pin(register_numbered(&database,&services,&mut supplier,12)).await;
                Box::pin(admit(&services,&institution,Entry::Institution,&mobile(12),Kind::Default)).await;
                let fourth=confirmation(&services,&fourth_person,&mobile(12)).await;
                assert_eq!(Box::pin(services.number().allocate_member(manager.clone(),operation(),target(&fourth),assistance(&fourth))).await.expect("old pool released only after correction").number().serial(),1);
                // A -> B -> A has the same value but a new revision, so the old confirmation fails.
                let back=correction_body(&confirmed,&third,802,previous_year(),3);
                let fourth=Box::pin(services.number().correct(manager.clone(),operation(),id,correction(&back))).await.expect("another pool value");
                let again=correction_body(&confirmed,&fourth,902,previous_year()-1,42);
                let fifth=Box::pin(services.number().correct(manager.clone(),operation(),id,correction(&again))).await.expect("value restored with fresh version");
                assert_eq!(fifth.number(),third.number()); assert_ne!(fifth.id(),third.id());
                assert!(matches!(Box::pin(services.number().correct(manager.clone(),operation(),id,correction(&back))).await,Err(Error::Rejected(Rejection::TargetChanged))));
                assert_eq!(Box::pin(services.number().correct(manager.clone(),original,id,correction(&body))).await.expect("original precedes current version").id(),second.id());
                for (field,value) in [("revisionId",json!(Uuid::new_v4())),("expected",json!(first.id())),("reason",json!("Different verified reason")),("replacement",json!({"subtype":803,"year":previous_year(),"serial":1}))] {
                    let mut changed=body.clone(); changed[field]=value;
                    if changed==body { changed["expected"]=json!(Uuid::new_v4()); }
                    assert!(matches!(Box::pin(services.number().correct(manager.clone(),original,id,correction(&changed))).await,Err(Error::Mismatch)),"{field}");
                }
                assert!(matches!(Box::pin(services.number().correct(manager.clone(),original,target(&other),correction(&body))).await,Err(Error::Mismatch)));
                assert_eq!(Box::pin(services.number().read_current(&peer)).await.expect("current").expect("held").id(),fifth.id());
                assert_eq!(observer.client().query_one("SELECT count(*) FROM workforce.revisions WHERE person_id=$1",&[&id]).await.expect("immutable revisions").get::<_,i64>(0),5);
                Box::pin(services.number().verify(observer.client(),chrono::Utc::now())).await.expect("whole chain verifies");
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context,services)=Box::pin(start(database.connections(&database.user),&supplier)).await;
                assert_eq!(Box::pin(services.number().read_current(&peer)).await.expect("restart current").expect("held").id(),fifth.id());
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
            })).await.expect("bounded correction lifecycle");
        }));
    }

    async fn correct_departed_target(
        services: &Services,
        institution: &Actor,
        manager: &Actor,
        peer: &Actor,
        confirmed: &serde_json::Value,
        first: &super::Revision,
    ) -> (operation::Id, serde_json::Value) {
        let id = target(confirmed);
        let body = correction_body(confirmed, first, 802, previous_year(), 42);
        let grant = Box::pin(services.grant().read_latest(peer))
            .await
            .expect("grant");
        let paused = Box::pin(services.grant().suspend(
            manager.clone(),
            operation(),
            grant.id(),
            serde_json::from_value(json!({"target":confirmed["target"]})).expect("pause"),
        ))
        .await
        .expect("pause target grant");
        let second = Box::pin(services.number().correct(
            manager.clone(),
            operation(),
            id,
            correction(&body),
        ))
        .await
        .expect("suspended target grant does not prohibit correction");
        Box::pin(services.grant().resume(
            manager.clone(),
            operation(),
            paused.id(),
            serde_json::from_value(json!({"target":confirmed["target"]})).expect("resume"),
        ))
        .await
        .expect("resume target");
        Box::pin(
            services
                .grant()
                .terminate(peer.clone(), operation(), grant.id()),
        )
        .await
        .expect("target leaves");
        Box::pin(async {
                    let role=Box::pin(services.role().create(institution.clone(),operation(),serde_json::from_value(json!({"name":"Target senior office","level":600,"permissions":[]})).expect("role"))).await.expect("role");
                    let candidate=services.appointment().candidate(institution,mobile(10).parse().expect("mobile")).await.expect("candidate");
                    let assignment=serde_json::from_value(json!({"target":{"accountId":candidate.identity().id(),"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":mobile(10),"assignmentId":candidate.assignment_id()},"role":{"id":role.id(),"changeId":role.change_id()}})).expect("target office confirmation");
                    Box::pin(services.appointment().assign(institution.clone(),operation(),assignment)).await.expect("target higher office");
                }).await;
        let ended = correction_body(confirmed, &second, 803, previous_year(), 42);
        let third = Box::pin(services.number().correct(
            manager.clone(),
            operation(),
            id,
            correction(&ended),
        ))
        .await
        .expect("ended Grant has no current correction level");
        let cancellation=serde_json::from_value(json!({"personId":id,"revisionId":confirmed["target"]["revisionId"],"mobile":mobile(10)})).expect("cancellation");
        Box::pin(services.cancellation().cancel_account(
            institution.clone(),
            operation(),
            serde_json::from_value(confirmed["target"]["accountId"].clone()).expect("account"),
            cancellation,
        ))
        .await
        .expect("cancel target");
        let cancelled = correction_body(confirmed, &third, 904, previous_year() - 1, 88);
        let original = operation();
        let fourth = Box::pin(services.number().correct(
            manager.clone(),
            original,
            id,
            correction(&cancelled),
        ))
        .await
        .expect("cancelled target correction");
        assert_eq!(
            Box::pin(services.number().correct(
                manager.clone(),
                original,
                id,
                correction(&cancelled)
            ))
            .await
            .expect("cancelled target replay")
            .id(),
            fourth.id()
        );

        (original, cancelled)
    }

    #[test]
    fn correction_authority_allows_ended_and_cancelled_targets_without_new_rights() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(90), Box::pin(async {
                let database=Box::pin(Database::initialized(Arc::clone(&server))).await;
                let mut supplier=Supplier::new().await;
                let (context,services)=Box::pin(start(database.connections(&database.user),&supplier)).await;
                let (institution,manager,peer)=Box::pin(members(&database,&services,&mut supplier)).await;
                let confirmed=confirmation(&services,&peer,&mobile(10)).await;
                let id=target(&confirmed);
                let first=Box::pin(services.number().allocate(peer.clone(),operation(),input(801))).await.expect("first number");
                let body=correction_body(&confirmed,&first,802,previous_year(),42);
                assert!(matches!(Box::pin(services.number().correct(manager.clone(),operation(),id,correction(&body))).await,Err(Error::Denied)),"equal institution level rejected");
                Box::pin(promote(&services,&institution)).await;
                for actor in [&institution,&peer] {
                    assert!(matches!(Box::pin(services.number().correct(actor.clone(),operation(),id,correction(&body))).await,Err(Error::Denied)),"exact workforce permission required");
                }
                let missing=serde_json::from_value(json!(Uuid::new_v4())).expect("person id");
                assert!(matches!(Box::pin(services.number().correct(manager.clone(),operation(),missing,correction(&body))).await,Err(Error::Rejected(Rejection::Absent))));
                let manager_confirmation=confirmation(&services,&manager,"13800000000").await;
                // A lower target with no number is never issued one by correction.
                let new_person=Box::pin(register_numbered(&database,&services,&mut supplier,11)).await;
                let unnumbered=confirmation(&services,&new_person,&mobile(11)).await;
                let mut absent=body.clone(); absent["revisionId"]=unnumbered["target"]["revisionId"].clone();
                assert!(matches!(Box::pin(services.number().correct(manager.clone(),operation(),target(&unnumbered),correction(&absent))).await,Err(Error::Rejected(Rejection::Absent))));
                let mut stale=body.clone(); stale["revisionId"]=json!(Uuid::new_v4());
                assert!(matches!(Box::pin(services.number().correct(manager.clone(),operation(),id,correction(&stale))).await,Err(Error::Rejected(Rejection::TargetChanged))));
                stale=body.clone(); stale["expected"]=json!(Uuid::new_v4());
                assert!(matches!(Box::pin(services.number().correct(manager.clone(),operation(),id,correction(&stale))).await,Err(Error::Rejected(Rejection::TargetChanged))));
                let (original,cancelled)=Box::pin(correct_departed_target(&services,&institution,&manager,&peer,&confirmed,&first)).await;
                let observer=database.connections(&database.user).recover().await.expect("observer");
                let row=observer.client().query_one("SELECT (SELECT count(*) FROM identity.accounts WHERE person_id=$1 AND cancellation_id IS NULL) AS current_accounts,(SELECT count(*) FROM identity.accounts account JOIN identity.cancellations cancellation ON cancellation.id=account.cancellation_id AND cancellation.account_id=account.id AND cancellation.person_id=account.person_id WHERE account.person_id=$1 AND account.mobile IS NULL AND account.id_number IS NULL AND account.suspension_id IS NULL) AS tombstones,(SELECT count(*) FROM access.grants WHERE termination IS NULL AND person_id=$1) AS members,(SELECT count(*) FROM access.grants WHERE person_id=$1) AS grants,(SELECT count(*) FROM workforce.allocations WHERE person_id=$1) AS allocations,(SELECT count(*) FROM workforce.numbers WHERE id=$1) AS numbers",&[&id]).await.expect("no new rights or allocations and one permanent Account tombstone");
                for (column,count) in [("current_accounts",0_i64),("tombstones",1),("members",0),("grants",1),("allocations",1),("numbers",1)] { assert_eq!(row.get::<_,i64>(column),count,"{column}"); }
                let manager_grant=Box::pin(services.grant().read_latest(&manager)).await.expect("manager grant");
                // Losing current authority denies new requests even for a known original result.
                Box::pin(services.grant().terminate(manager.clone(),operation(),manager_grant.id())).await.expect("manager leaves");
                assert!(matches!(Box::pin(services.number().correct(manager.clone(),original,id,correction(&cancelled))).await,Err(Error::Denied)));
                Box::pin(services.suspension().suspend(institution.clone(),operation(),manager.account_id(),serde_json::from_value(json!({"personId":target(&manager_confirmation),"revisionId":manager_confirmation["target"]["revisionId"],"mobile":"13800000000"})).expect("operator suspension"))).await.expect("pause operator account");
                assert!(matches!(Box::pin(services.number().correct(manager.clone(),original,id,correction(&cancelled))).await,Err(Error::Unauthenticated)));
                Box::pin(async { services.cancellation().cancel_account(institution,operation(),manager.account_id(),serde_json::from_value(json!({"personId":target(&manager_confirmation),"revisionId":manager_confirmation["target"]["revisionId"],"mobile":"13800000000"})).expect("manager cancellation")).await }).await.expect("cancel operator");
                assert!(matches!(Box::pin(services.number().correct(manager,original,id,correction(&cancelled))).await,Err(Error::Unauthenticated)));
                Box::pin(services.number().verify(observer.client(),chrono::Utc::now())).await.expect("later lost authority does not invalidate historical corrections");
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context,services)=Box::pin(start(database.connections(&database.user),&supplier)).await;
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
            })).await.expect("bounded correction eligibility");
        }));
    }

    /// The beneficiary's original assisted number is corrected away and then supplemented to another
    /// member; returns (institution, manager, beneficiary, occupier, body, original, temporary, occupied).
    async fn misoccupation(
        database: &Database,
        services: &Services,
        supplier: &mut Supplier,
    ) -> (
        Actor,
        Actor,
        Actor,
        Actor,
        serde_json::Value,
        super::Revision,
        super::Revision,
        super::Revision,
    ) {
        let (institution, manager, beneficiary) =
            Box::pin(members(database, services, supplier)).await;
        Box::pin(promote(services, &institution)).await;
        let body = confirmation(services, &beneficiary, &mobile(10)).await;
        let id = target(&body);
        let original = Box::pin(services.number().allocate_member(
            manager.clone(),
            operation(),
            id,
            assistance(&body),
        ))
        .await
        .expect("original number");
        let moved = correction_body(&body, &original, 902, previous_year(), 42);
        let temporary = Box::pin(services.number().correct(
            manager.clone(),
            operation(),
            id,
            correction(&moved),
        ))
        .await
        .expect("temporary number releases the original slot");
        let occupier = Box::pin(register_numbered(database, services, supplier, 11)).await;
        Box::pin(admit(
            services,
            &institution,
            Entry::Institution,
            &mobile(11),
            Kind::Default,
        ))
        .await;
        let slot =
            supplement_confirmation(services, &occupier, &mobile(11), original.number().serial())
                .await;
        let occupied = Box::pin(services.number().supplement(
            manager.clone(),
            operation(),
            target(&slot),
            supplement(&slot),
        ))
        .await
        .expect("another member takes the original slot");
        assert!(occupied.number().shares_pool(original.number()));
        (
            institution,
            manager,
            beneficiary,
            occupier,
            body,
            original,
            temporary,
            occupied,
        )
    }

    fn recovery(value: serde_json::Value) -> super::recovery::Recovery {
        serde_json::from_value(value).expect("typed recovery")
    }

    #[test]
    fn recovery_restores_an_occupied_original_number_and_revokes_atomically() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(120), Box::pin(async {
                let database = Box::pin(Database::initialized(Arc::clone(&server))).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                let (_, manager, beneficiary, occupier, body, original, temporary, occupied) =
                    Box::pin(misoccupation(&database, &services, &mut supplier)).await;
                let service = services.number();
                let beneficiary_id = target(&body);
                let candidate = Box::pin(service.recovery_candidate(&manager, mobile(10).parse().expect("mobile"))).await.expect("beneficiary candidate");
                assert_eq!(candidate.identity().person_id(), beneficiary_id);
                assert_eq!(candidate.number().map(|number| number.id()), Some(temporary.id()));
                let holder = Box::pin(service.correction_holder(&manager, original.number())).await.expect("occupier by complete number");
                let occupier_identity = services.current().read(&occupier).await.expect("occupier identity");
                assert_eq!(holder.identity().person_id(), occupier_identity.person_id());
                let input = json!({
                    "beneficiary": {"personId": beneficiary_id, "revisionId": body["target"]["revisionId"], "current": temporary.id(), "origin": original.id()},
                    "occupier": {"personId": holder.identity().person_id(), "revisionId": holder.identity().revision_id(), "current": holder.number().id()},
                    "reason": "Verified original holder record",
                });
                let observer = database.connections(&database.user).recover().await.expect("observer");
                let sequences = "SELECT (SELECT sequence FROM workforce.numbers WHERE id = $1), (SELECT sequence FROM workforce.numbers WHERE id = $2), (SELECT version FROM workforce.numbers WHERE id = $1), (SELECT version FROM workforce.numbers WHERE id = $2)";
                let before = observer.client().query_one(sequences, &[&beneficiary_id, &occupier_identity.person_id()]).await.expect("sequences before");

                // Rejections leave no facts.
                let mut same = input.clone(); same["occupier"]["personId"] = input["beneficiary"]["personId"].clone();
                let mut foreign_origin = input.clone(); foreign_origin["beneficiary"]["origin"] = json!(occupied.id());
                let mut other_pool = input.clone(); other_pool["beneficiary"]["origin"] = json!(temporary.id());
                let mut stale_current = input.clone(); stale_current["beneficiary"]["current"] = json!(null);
                let mut stale_occupier = input.clone(); stale_occupier["occupier"]["current"] = json!(Uuid::new_v4());
                let mut stale_revision = input.clone(); stale_revision["occupier"]["revisionId"] = json!(Uuid::new_v4());
                let mut unknown = input.clone(); unknown["occupier"]["personId"] = json!(Uuid::new_v4());
                for (case, value, expected) in [
                    ("same person", same, "recovery"), ("foreign origin", foreign_origin, "recovery"), ("other pool", other_pool, "recovery"),
                    ("stale beneficiary number", stale_current, "changed"), ("stale occupier number", stale_occupier, "changed"),
                    ("stale occupier revision", stale_revision, "changed"), ("unknown occupier", unknown, "absent"),
                ] {
                    let result = Box::pin(service.restore(manager.clone(), operation(), recovery(value))).await;
                    assert!(match expected { "recovery" => matches!(result, Err(Error::Rejected(Rejection::Recovery))), "changed" => matches!(result, Err(Error::Rejected(Rejection::TargetChanged))), _ => matches!(result, Err(Error::Rejected(Rejection::Absent))) }, "{case}");
                }
                assert_eq!(observer.client().query_one("SELECT (SELECT count(*) FROM workforce.recoveries) + (SELECT count(*) FROM workforce.revocations)", &[]).await.expect("no facts").get::<_, i64>(0), 0);
                assert!(matches!(Box::pin(service.restore(occupier.clone(), operation(), recovery(input.clone()))).await, Err(Error::Denied)), "plain members cannot recover");

                let original_operation = operation();
                let restored = Box::pin(service.restore(manager.clone(), original_operation, recovery(input.clone()))).await.expect("recovery");
                assert_eq!(restored.person_id(), beneficiary_id);
                assert_eq!(restored.origin(), original.id());
                assert_eq!(restored.number().number(), original.number());
                assert_ne!(restored.number().id(), original.id());
                let revoked: Vec<_> = restored.revocations().iter().map(|revoked| (revoked.person_id(), revoked.number().id())).collect();
                assert_eq!(revoked, vec![(occupier_identity.person_id(), occupied.id()), (beneficiary_id, temporary.id())]);
                assert_eq!(Box::pin(service.restore(manager.clone(), original_operation, recovery(input.clone()))).await.expect("replay"), restored);
                let mut changed = input.clone(); changed["reason"] = json!("Another verified record");
                assert!(matches!(Box::pin(service.restore(manager.clone(), original_operation, recovery(changed))).await, Err(Error::Mismatch)));

                // The beneficiary appends two contiguous records in one state write.
                let after = observer.client().query_one(sequences, &[&beneficiary_id, &occupier_identity.person_id()]).await.expect("sequences after");
                assert_eq!(after.get::<_, i64>(0), before.get::<_, i64>(0) + 2);
                assert_eq!(after.get::<_, i64>(1), before.get::<_, i64>(1) + 1);
                assert_eq!(after.get::<_, i64>(2), before.get::<_, i64>(2) + 1);
                assert_eq!(after.get::<_, i64>(3), before.get::<_, i64>(3) + 1);
                assert!(observer.client().query_one("SELECT bool_and(state.tenure_id=revocation.id AND NOT state.held) FROM workforce.numbers state JOIN workforce.revocations revocation ON revocation.person_id=state.id WHERE state.id=$1", &[&occupier_identity.person_id()]).await.expect("revocation creates a new tombstone tenure").get::<_,bool>(0));
                let row = observer.client().query_one("SELECT recovery.sequence, (SELECT array_agg(revocation.sequence ORDER BY revocation.person_id = recovery.person_id) FROM workforce.revocations revocation WHERE revocation.recovery_id = recovery.id) AS revoked, (SELECT revision_id FROM workforce.numbers WHERE id = recovery.person_id) AS held, (SELECT count(*) FROM workforce.numbers WHERE id = $2 AND held) AS occupier FROM workforce.recoveries recovery WHERE recovery.operation_id = $1", &[&original_operation, &occupier_identity.person_id()]).await.expect("durable recovery");
                assert_eq!(row.get::<_, i64>("sequence"), after.get::<_, i64>(0));
                assert_eq!(row.get::<_, Vec<i64>>("revoked"), vec![after.get::<_, i64>(1), after.get::<_, i64>(0) - 1]);
                assert_eq!(serde_json::to_value(row.get::<_, Uuid>("held")).expect("held"), serde_json::to_value(restored.number().id()).expect("restored"));
                assert_eq!(row.get::<_, i64>("occupier"), 0);

                // Reads, history, a later correction and the released temporary slot.
                assert_eq!(service.read_current(&beneficiary).await.expect("current").expect("restored").id(), restored.number().id());
                assert!(service.read_current(&occupier).await.expect("current").is_none());
                assert!(matches!(Box::pin(service.correction_holder(&manager, temporary.number())).await, Err(Error::Rejected(Rejection::Absent))));
                let page = crate::pagination::Pagination::new(1, 20).expect("page");
                let history = Box::pin(service.history(&manager, beneficiary_id, page)).await.expect("beneficiary history");
                assert_eq!(history.count(), 4);
                assert!(matches!(history.revisions()[0].kind(), super::HistoryKind::Recovery { .. } | super::HistoryKind::Revocation { .. }));
                let history = Box::pin(service.history(&manager, occupier_identity.person_id(), crate::pagination::Pagination::new(1, 20).expect("page"))).await.expect("occupier history");
                assert!(matches!(history.revisions()[0].kind(), super::HistoryKind::Revocation { recovery, .. } if *recovery == restored.id()));
                let later = correction_body(&body, restored.number(), 803, previous_year(), 77);
                Box::pin(service.correct(manager.clone(), operation(), beneficiary_id, correction(&later))).await.expect("a recovered number is correctable");

                // The occupier without a current number can be the beneficiary of a reverse recovery.
                let current = service.read_current(&beneficiary).await.expect("current").expect("held");
                let reverse = json!({
                    "beneficiary": {"personId": occupier_identity.person_id(), "revisionId": occupier_identity.revision_id(), "current": null, "origin": occupied.id()},
                    "occupier": {"personId": beneficiary_id, "revisionId": body["target"]["revisionId"], "current": current.id()},
                    "reason": "Reversed after review",
                });
                let back = Box::pin(service.restore(manager.clone(), operation(), recovery(reverse))).await;
                assert!(matches!(back, Err(Error::Rejected(Rejection::Recovery))), "the current number left the original pool slot after the later correction");
                observer.terminate().await.expect("observer ended");

                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                services.ready().await.expect("recovery facts verify at startup");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                supplier.stop().await;
            })).await.expect("bounded recovery");
        }));
    }

    #[test]
    fn recovery_without_a_beneficiary_number_revokes_only_the_occupier() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(120), Box::pin(async {
                let database = Box::pin(Database::initialized(Arc::clone(&server))).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                let (_, manager, beneficiary, occupier, body, original, temporary, occupied) =
                    Box::pin(misoccupation(&database, &services, &mut supplier)).await;
                let service = services.number();
                let beneficiary_id = target(&body);
                let occupier_identity = services.current().read(&occupier).await.expect("occupier identity");
                let first = json!({
                    "beneficiary": {"personId": beneficiary_id, "revisionId": body["target"]["revisionId"], "current": temporary.id(), "origin": original.id()},
                    "occupier": {"personId": occupier_identity.person_id(), "revisionId": occupier_identity.revision_id(), "current": occupied.id()},
                    "reason": "Verified original holder record",
                });
                let restored = Box::pin(service.restore(manager.clone(), operation(), recovery(first))).await.expect("first recovery");
                // The former occupier now has no current number; a reverse recovery revokes one number only.
                let reverse = json!({
                    "beneficiary": {"personId": occupier_identity.person_id(), "revisionId": occupier_identity.revision_id(), "current": null, "origin": occupied.id()},
                    "occupier": {"personId": beneficiary_id, "revisionId": body["target"]["revisionId"], "current": restored.number().id()},
                    "reason": "Reversed after review",
                });
                let back = Box::pin(service.restore(manager.clone(), operation(), recovery(reverse))).await.expect("reverse recovery");
                assert_eq!(back.revocations().len(), 1);
                assert_eq!(back.number().number(), occupied.number());
                assert_eq!(service.read_current(&occupier).await.expect("current").expect("restored").id(), back.number().id());
                assert!(service.read_current(&beneficiary).await.expect("current").is_none());

                // Startup authenticates every retained recovery proof.
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let owner = database.connections("postgres").recover().await.expect("owner");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica; UPDATE workforce.recoveries SET reason = 'Altered record' WHERE reason = 'Reversed after review'; UPDATE workforce.revocations SET reason = 'Altered record' WHERE reason = 'Reversed after review'; COMMIT").await.expect("alter retained reason");
                owner.terminate().await.expect("owner ended");
                let context = crate::runtime::Context::new(database.connections(&database.user)).await.expect("context");
                let barrier = crate::database::Barrier::begin(context.clone()).await.expect("barrier");
                let result = crate::startup::Services::start(
                    barrier,
                    crate::identity::authentication::inventory(crate::identity::authentication::keys()),
                    crate::startup::Settings::new(4, crate::identity::message::Limits::default(), crate::identity::authentication::session_settings(), crate::registration::Limits::default(), crate::identity::authentication::avatar_settings()).expect("settings"),
                    supplier.client(),
                    crate::identity::authentication::unused_client().await,
                ).await;
                assert!(result.is_err(), "startup rejects a recovery whose reason no longer matches its proof");
                context.stop().await.expect("context stopped");
                supplier.stop().await;
            })).await.expect("bounded single revocation recovery");
        }));
    }

    #[test]
    fn recovery_moves_a_cancelled_holders_number_to_a_new_identity() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(120), Box::pin(async {
                let database = Box::pin(Database::initialized(Arc::clone(&server))).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                let (institution, manager, old) = Box::pin(members(&database, &services, &mut supplier)).await;
                Box::pin(promote(&services, &institution)).await;
                let service = services.number();
                let held = Box::pin(service.allocate(old.clone(), operation(), input(801))).await.expect("old identity number");
                let identity = services.current().read(&old).await.expect("old identity");
                let cancellation = serde_json::from_value(json!({"personId": identity.person_id(), "revisionId": identity.revision_id(), "mobile": mobile(10)})).expect("cancellation");
                Box::pin(services.cancellation().cancel_account(institution.clone(), operation(), identity.id(), cancellation)).await.expect("old identity cancelled");
                let new = Box::pin(register_numbered(&database, &services, &mut supplier, 11)).await;
                Box::pin(admit(&services, &institution, Entry::Institution, &mobile(11), Kind::Default)).await;
                let fresh = services.current().read(&new).await.expect("new identity");
                // The cancelled holder is found by complete number; the new identity has no number.
                let holder = Box::pin(service.correction_holder(&manager, held.number())).await.expect("cancelled holder");
                assert_eq!(holder.identity().person_id(), identity.person_id());
                let candidate = Box::pin(service.recovery_candidate(&manager, mobile(11).parse().expect("mobile"))).await.expect("new identity candidate");
                assert!(candidate.number().is_none());
                let body = json!({
                    "beneficiary": {"personId": fresh.person_id(), "revisionId": fresh.revision_id(), "current": null, "origin": held.id()},
                    "occupier": {"personId": identity.person_id(), "revisionId": holder.identity().revision_id(), "current": held.id()},
                    "reason": "Verified the same natural person after re-registration",
                });
                let mut other_origin = body.clone(); other_origin["beneficiary"]["origin"] = json!(Uuid::new_v4());
                let mut claimed = body.clone(); claimed["beneficiary"]["current"] = json!(held.id());
                for (case, value, expected) in [("unrelated origin", other_origin, "recovery"), ("stale beneficiary number", claimed, "changed")] {
                    let result = Box::pin(service.restore(manager.clone(), operation(), recovery(value))).await;
                    assert!(match expected { "recovery" => matches!(result, Err(Error::Rejected(Rejection::Recovery))), _ => matches!(result, Err(Error::Rejected(Rejection::TargetChanged))) }, "{case}");
                }
                let grant = services.grant().read_latest(&new).await.expect("new Grant");
                let target = json!({"target": {"accountId": fresh.id(), "personId": fresh.person_id(), "revisionId": fresh.revision_id(), "mobile": mobile(11)}});
                let pause = Box::pin(services.grant().suspend(manager.clone(), operation(), grant.id(), serde_json::from_value(target.clone()).expect("confirmation"))).await.expect("suspend new identity");
                assert!(matches!(Box::pin(service.restore(manager.clone(), operation(), recovery(body.clone()))).await, Err(Error::Rejected(Rejection::StateConflict))), "the new identity meets normal Grant conditions");
                Box::pin(services.grant().resume(manager.clone(), operation(), pause.id(), serde_json::from_value(target).expect("confirmation"))).await.expect("resume new identity");

                let restored = Box::pin(service.restore(manager.clone(), operation(), recovery(body))).await.expect("re-registration recovery");
                assert_eq!(restored.number().number(), held.number());
                assert_eq!(restored.origin(), held.id());
                assert_eq!(restored.revocations().len(), 1);
                assert_eq!(restored.revocations()[0].person_id(), identity.person_id());
                assert_eq!(service.read_current(&new).await.expect("current").expect("restored").id(), restored.number().id());
                let history = Box::pin(service.history(&manager, identity.person_id(), crate::pagination::Pagination::new(1, 20).expect("page"))).await.expect("old identity history");
                assert!(matches!(history.revisions()[0].kind(), super::HistoryKind::Revocation { .. }));

                // A person who already held a number cannot receive another person's number this way.
                let third = Box::pin(register_numbered(&database, &services, &mut supplier, 12)).await;
                Box::pin(admit(&services, &institution, Entry::Institution, &mobile(12), Kind::Default)).await;
                Box::pin(service.allocate(third.clone(), operation(), input(100))).await.expect("own number");
                let other = services.current().read(&third).await.expect("third identity");
                let own = service.read_current(&third).await.expect("current").expect("held");
                let reverse = json!({
                    "beneficiary": {"personId": other.person_id(), "revisionId": other.revision_id(), "current": own.id(), "origin": restored.number().id()},
                    "occupier": {"personId": fresh.person_id(), "revisionId": fresh.revision_id(), "current": restored.number().id()},
                    "reason": "Not the same person",
                });
                assert!(matches!(Box::pin(service.restore(manager.clone(), operation(), recovery(reverse))).await, Err(Error::Rejected(Rejection::Recovery))));

                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                services.ready().await.expect("re-registration recovery verifies at startup");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                supplier.stop().await;
            })).await.expect("bounded re-registration recovery");
        }));
    }

    #[test]
    fn revoked_person_corrects_only_its_latest_revision_without_occupancy() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(120), Box::pin(async {
                let database = Box::pin(Database::initialized(Arc::clone(&server))).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                let (_, manager, beneficiary, occupier, body, original, temporary, occupied) =
                    Box::pin(misoccupation(&database, &services, &mut supplier)).await;
                let service = services.number();
                let occupier_identity = services.current().read(&occupier).await.expect("occupier identity");
                let input = json!({
                    "beneficiary": {"personId": target(&body), "revisionId": body["target"]["revisionId"], "current": temporary.id(), "origin": original.id()},
                    "occupier": {"personId": occupier_identity.person_id(), "revisionId": occupier_identity.revision_id(), "current": occupied.id()},
                    "reason": "Verified original holder record",
                });
                let restored = Box::pin(service.restore(manager.clone(), operation(), recovery(input))).await.expect("recovery");
                let id = occupier_identity.person_id();
                let correction_of = |expected: super::Id, subtype: i16, serial: i16| correction(&json!({
                    "revisionId": occupier_identity.revision_id(), "expected": expected,
                    "replacement": {"subtype": subtype, "year": previous_year(), "serial": serial}, "reason": "Corrected historical record",
                }));
                // The replacement may equal a number someone else now holds; no occupancy is taken.
                let restored_value = restored.number().number();
                assert!(matches!(Box::pin(service.correct(manager.clone(), operation(), id, correction_of(occupied.id(), i16::from(occupied.number().subtype()), occupied.number().serial()))).await, Err(Error::Rejected(Rejection::Unchanged))));
                let first = Box::pin(service.correct(manager.clone(), operation(), id, correction_of(occupied.id(), i16::from(restored_value.subtype()) + 1, restored_value.serial()))).await.expect("detached correction");
                assert!(service.read_current(&occupier).await.expect("current").is_none());
                assert_eq!(service.read_current(&beneficiary).await.expect("current").expect("held").id(), restored.number().id());
                assert!(matches!(Box::pin(service.correct(manager.clone(), operation(), id, correction_of(occupied.id(), 803, 5))).await, Err(Error::Rejected(Rejection::TargetChanged))), "only the latest revision continues the chain");
                let second = Box::pin(service.correct(manager.clone(), operation(), id, correction_of(first.id(), 803, 5))).await.expect("next detached correction");
                assert_ne!(second.id(), first.id());
                assert!(service.read_current(&occupier).await.expect("current").is_none());
                let observer = database.connections(&database.user).recover().await.expect("observer");
                assert!(observer.client().query_one("SELECT state.tenure_id=revocation.id AND NOT state.held FROM workforce.numbers state JOIN workforce.revocations revocation ON revocation.person_id=state.id WHERE state.id=$1", &[&id]).await.expect("detached corrections preserve revoked tenure").get::<_,bool>(0));
                observer.terminate().await.expect("observer ended");
                let history = Box::pin(service.history(&manager, id, crate::pagination::Pagination::new(1, 20).expect("page"))).await.expect("occupier history");
                assert!(matches!(history.revisions()[0].kind(), super::HistoryKind::Correction { .. }));
                assert_eq!(history.count(), 4);
                // A later recovery gives the corrected person a number again; the corrected chain stays valid history.
                let back = json!({
                    "beneficiary": {"personId": id, "revisionId": occupier_identity.revision_id(), "current": null, "origin": occupied.id()},
                    "occupier": {"personId": target(&body), "revisionId": body["target"]["revisionId"], "current": restored.number().id()},
                    "reason": "Reversed after review",
                });
                let returned = Box::pin(service.restore(manager.clone(), operation(), recovery(back))).await.expect("later recovery of the corrected person");
                assert_eq!(service.read_current(&occupier).await.expect("current").expect("held").id(), returned.number().id());
                let history = Box::pin(service.history(&manager, id, crate::pagination::Pagination::new(1, 20).expect("page"))).await.expect("history after a later recovery");
                assert_eq!(history.count(), 5);
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                services.ready().await.expect("detached corrections verify at startup");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                supplier.stop().await;
            })).await.expect("bounded detached correction");
        }));
    }

    #[test]
    fn recovery_commit_faults_keep_the_whole_group_together() {
        use fixture::database_proxy::{Fault, Proxy};
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["lost", "rollback", "unknown"] {
                timeout(Duration::from_secs(120), Box::pin(async {
                    let database = Box::pin(Database::initialized(Arc::clone(&server))).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = Box::pin(start(crate::identity::authentication::proxied(&database, &proxy), &supplier)).await;
                    let (_, manager, _, occupier, body, original, temporary, occupied) =
                        Box::pin(misoccupation(&database, &services, &mut supplier)).await;
                    let beneficiary_id = target(&body);
                    let occupier_identity = services.current().read(&occupier).await.expect("occupier identity");
                    let input = json!({
                        "beneficiary": {"personId": beneficiary_id, "revisionId": body["target"]["revisionId"], "current": temporary.id(), "origin": original.id()},
                        "occupier": {"personId": occupier_identity.person_id(), "revisionId": occupier_identity.revision_id(), "current": occupied.id()},
                        "reason": "Verified original holder record",
                    });
                    let original_operation = operation();
                    proxy.arm(match case { "lost" => Fault::LostCommit, "rollback" => Fault::RolledBack, _ => Fault::UnknownCommit });
                    let result = Box::pin(services.number().restore(manager.clone(), original_operation, recovery(input.clone()))).await;
                    proxy.release();
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("actual recovery commit intercepted");
                    match case {
                        "rollback" => assert!(matches!(result, Err(Error::Unavailable(_)))),
                        "unknown" => assert!(matches!(result, Err(Error::Unknown(_)))),
                        _ => assert_eq!(result.expect("recovered").revocations().len(), 2),
                    }
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    let observer = database.connections(&database.user).recover().await.expect("observer");
                    observer.client().batch_execute("BEGIN").await.expect("observation");
                    // The original transaction holds both person rows until it ends.
                    observer.client().query("SELECT id FROM identity.persons WHERE id = ANY($1) ORDER BY id FOR UPDATE", &[&vec![beneficiary_id, occupier_identity.person_id()]]).await.expect("original transaction released both persons");
                    let row = observer.client().query_one("SELECT (SELECT count(*) FROM workforce.recoveries WHERE operation_id = $1) AS recoveries, (SELECT count(*) FROM workforce.revocations) AS revocations, (SELECT count(*) FROM workforce.numbers WHERE id = $2 AND held) AS occupier", &[&original_operation, &occupier_identity.person_id()]).await.expect("durable group");
                    let committed = i64::from(case != "rollback");
                    assert_eq!(row.get::<_, i64>("recoveries"), committed);
                    assert_eq!(row.get::<_, i64>("revocations"), 2 * committed);
                    assert_eq!(row.get::<_, i64>("occupier"), 1 - committed);
                    observer.client().batch_execute("COMMIT").await.expect("observation ended");
                    observer.terminate().await.expect("observer ended");
                    proxy.stop().await;
                    let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                    let replayed = Box::pin(services.number().restore(manager, original_operation, recovery(input))).await.expect("replay or retry after rollback");
                    assert_eq!(replayed.revocations().len(), 2);
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
                })).await.expect("bounded recovery fault");
            }
        }));
    }

    #[test]
    fn recovery_partial_write_rolls_back_every_state_and_record() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(120), Box::pin(async {
                let database = Box::pin(Database::initialized(Arc::clone(&server))).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                let (_, manager, beneficiary, occupier, body, original, temporary, occupied) =
                    Box::pin(misoccupation(&database, &services, &mut supplier)).await;
                let beneficiary_id = target(&body);
                let occupier_identity = services.current().read(&occupier).await.expect("occupier identity");
                let input = json!({
                    "beneficiary": {"personId": beneficiary_id, "revisionId": body["target"]["revisionId"], "current": temporary.id(), "origin": original.id()},
                    "occupier": {"personId": occupier_identity.person_id(), "revisionId": occupier_identity.revision_id(), "current": occupied.id()},
                    "reason": "Verified original holder record",
                });
                let owner = database.connections("postgres").recover().await.expect("isolated fault owner");
                let snapshot = "SELECT jsonb_build_array((SELECT jsonb_agg(row ORDER BY id) FROM workforce.numbers row),(SELECT jsonb_agg(row ORDER BY id) FROM workforce.transitions row),(SELECT jsonb_agg(row ORDER BY id) FROM workforce.revisions row),(SELECT jsonb_agg(row ORDER BY family,operation_id) FROM workforce.completions row))::text";
                let before: String = owner.client().query_one(snapshot, &[]).await.expect("before").get(0);
                owner.client().batch_execute(&format!("ALTER TABLE workforce.numbers ADD CONSTRAINT test_second_number_write CHECK (id<>'{beneficiary_id}'::uuid OR sequence<4)")).await.expect("reject beneficiary after occupier write");
                let original_operation = operation();
                assert!(Box::pin(services.number().restore(manager.clone(), original_operation, recovery(input.clone()))).await.is_err());
                assert_eq!(owner.client().query_one(snapshot, &[]).await.expect("no partial state, record, value or completion").get::<_, String>(0), before);
                assert_eq!(services.number().read_current(&beneficiary).await.expect("beneficiary anchor rolled back").expect("held").id(), temporary.id());
                assert_eq!(services.number().read_current(&occupier).await.expect("occupier anchor rolled back").expect("held").id(), occupied.id());
                owner.client().batch_execute("ALTER TABLE workforce.numbers DROP CONSTRAINT test_second_number_write").await.expect("remove only test fault");
                let restored = Box::pin(services.number().restore(manager, original_operation, recovery(input))).await.expect("retry after confirmed rollback");
                assert_eq!(restored.revocations().len(), 2);
                for sql in [
                    "DELETE FROM workforce.recoveries",
                    "DELETE FROM workforce.revocations",
                    "DELETE FROM workforce.revisions WHERE recovery_id IS NOT NULL",
                ] {
                    owner.client().batch_execute("BEGIN").await.expect("structural trial");
                    let outcome = async {
                        owner.client().batch_execute(sql).await?;
                        owner.client().batch_execute("SET CONSTRAINTS ALL IMMEDIATE").await
                    }.await;
                    assert_eq!(outcome.expect_err("a recovery retains its complete fixed group").code().expect("foreign key").code(), "23503", "{sql}");
                    owner.client().batch_execute("ROLLBACK").await.expect("restore complete group");
                }
                let runtime = database.connections(&database.user).recover().await.expect("runtime");
                for sql in ["UPDATE workforce.recoveries SET reason=reason", "DELETE FROM workforce.revocations", "UPDATE workforce.revocations SET reason=reason"] {
                    assert_eq!(runtime.client().batch_execute(sql).await.expect_err("permanent record").code().expect("permission").code(), "42501", "{sql}");
                }
                runtime.terminate().await.expect("runtime ended");
                owner.terminate().await.expect("owner ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
            })).await.expect("bounded recovery rollback and structural protection");
        }));
    }

    #[test]
    fn correction_recovers_commit_faults_after_the_original_transaction_ends() {
        use fixture::database_proxy::{Fault, Proxy};
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["lost","rollback","unknown","cancelled"] {
                timeout(Duration::from_secs(70),Box::pin(async {
                    let database=Box::pin(Database::initialized(Arc::clone(&server))).await;
                    let mut supplier=Supplier::new().await;
                    let proxy=Proxy::start(&server).await;
                    let (context,services)=Box::pin(start(crate::identity::authentication::proxied(&database,&proxy),&supplier)).await;
                    let (institution,manager,peer)=Box::pin(members(&database,&services,&mut supplier)).await;
                    Box::pin(promote(&services,&institution)).await;
                    let confirmed=confirmation(&services,&peer,&mobile(10)).await;
                    let id=target(&confirmed);
                    let first=Box::pin(services.number().allocate(peer.clone(),operation(),input(801))).await.expect("original");
                    let body=correction_body(&confirmed,&first,902,previous_year(),42);
                    let original=operation();
                    proxy.arm(match case {"lost"=>Fault::LostCommit,"rollback"=>Fault::RolledBack,"unknown"=>Fault::UnknownCommit,"cancelled"=>Fault::DelayedCommit,_=>unreachable!()});
                    let waiter=tokio::spawn({let service=services.number().clone();let actor=manager.clone();let input=correction(&body);async move {Box::pin(service.correct(actor,original,id,input)).await}});
                    timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("actual correction commit intercepted");
                    if case=="cancelled" {
                        waiter.abort();assert!(matches!(waiter.await,Err(error) if error.is_cancelled()));services.close();
                        assert!(timeout(Duration::from_millis(50),services.drain()).await.is_err());
                        proxy.release();services.drain().await.expect("work ended");
                    } else {
                        proxy.release();
                        let result=timeout(Duration::from_secs(15),waiter).await.expect("bounded outcome").expect("waiter");
                        match case {"rollback"=>assert!(matches!(result,Err(Error::Unavailable(_)))),"unknown"=>assert!(matches!(result,Err(Error::Unknown(_)))),_=>assert_eq!(result.expect("recovered").number().serial(),42)}
                        services.close();services.drain().await.expect("drained");
                    }
                    context.stop().await.expect("stopped");
                    let observer=database.connections(&database.user).recover().await.expect("observer");
                    observer.client().batch_execute("BEGIN").await.expect("observation");
                    observer.client().query_one("SELECT id FROM identity.persons WHERE id=$1 FOR UPDATE",&[&id]).await.expect("original transaction has released its person lock");
                    let row=observer.client().query_one("SELECT (SELECT count(*) FROM workforce.corrections WHERE operation_id=$1) AS corrections,(SELECT count(*) FROM workforce.revisions WHERE person_id=$2) AS revisions,sequence AS state_sequence FROM workforce.numbers WHERE id=$2",&[&original,&id]).await.expect("durable state before replay");
                    let committed=i64::from(case!="rollback");
                    assert_eq!(row.get::<_,i64>("corrections"),committed);assert_eq!(row.get::<_,i64>("revisions"),1+committed);assert_eq!(row.get::<_,i64>("state_sequence"),1+committed);
                    observer.client().batch_execute("COMMIT").await.expect("observation ended");
                    observer.terminate().await.expect("observer ended");proxy.stop().await;
                    let (context,services)=Box::pin(start(database.connections(&database.user),&supplier)).await;
                    let result=Box::pin(services.number().correct(manager,original,id,correction(&body))).await.expect("replay or explicit retry after rollback");
                    assert_eq!(result.number().serial(),42);
                    services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");supplier.stop().await;
                })).await.expect("bounded correction fault");
            }
        }));
    }

    #[test]
    fn correction_startup_rejects_changed_operator_after_first_batch() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(90), Box::pin(async {
                let database = Box::pin(Database::initialized(Arc::clone(&server))).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                let (institution, manager, peer) = Box::pin(members(&database, &services, &mut supplier)).await;
                Box::pin(promote(&services, &institution)).await;
                let confirmed = confirmation(&services, &peer, &mobile(10)).await;
                let id = target(&confirmed);
                let other = services.current().read(&institution).await.expect("other real operator").revision_id();
                let mut current = Box::pin(services.number().allocate(peer, operation(), input(801))).await.expect("first number");
                for serial in 1..=130 {
                    let body = correction_body(&confirmed, &current, 902, previous_year(), serial);
                    current = Box::pin(services.number().correct(manager.clone(), operation(), id, correction(&body))).await.expect("real correction");
                }
                services.close();
                services.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                services.close();
                services.drain().await.expect("restart drained");
                context.stop().await.expect("all valid batches pass startup");

                let owner = database.connections("postgres").recover().await.expect("isolated corruption owner");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated damage");
                // The greatest operation ID is read only after the first verification batch.
                assert_eq!(owner.client().execute(
                    "UPDATE workforce.corrections SET operator_id = $1 WHERE operation_id = (SELECT operation_id FROM workforce.corrections ORDER BY operation_id DESC LIMIT 1)",
                    &[&other],
                ).await.expect("replace operator with another real revision"), 1);
                owner.client().batch_execute("COMMIT").await.expect("damage committed");
                let context = crate::runtime::Context::new(database.connections(&database.user)).await.expect("restart context");
                let result = Box::pin(Services::start(
                    crate::database::Barrier::begin(context.clone()).await.expect("startup barrier"),
                    crate::identity::authentication::inventory(crate::identity::authentication::keys()),
                    crate::startup::Settings::new(4, crate::identity::message::Limits::default(), crate::identity::authentication::session_settings(), crate::registration::Limits::default(), crate::identity::authentication::avatar_settings()).expect("settings"),
                    supplier.client(), crate::identity::authentication::unused_client().await,
                )).await;
                let error = result.err().expect("startup must reject a changed retained operator");
                assert!(matches!(error, crate::startup::Error::Number(Error::Corrupt(_))), "changed operator is fact damage: {error:?}");
                context.stop().await.expect("failed startup context stopped");
                owner.terminate().await.expect("owner ended");
                supplier.stop().await;
            })).await.expect("bounded correction proof verification");
        }));
    }

    #[test]
    fn correction_runtime_sql_and_startup_enforce_permanent_chain() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(90),Box::pin(async {
                let database=Box::pin(Database::initialized(Arc::clone(&server))).await;
                let mut supplier=Supplier::new().await;
                let (context,services)=Box::pin(start(database.connections(&database.user),&supplier)).await;
                let (institution,manager,peer)=Box::pin(members(&database,&services,&mut supplier)).await;
                let confirmed=confirmation(&services,&peer,&mobile(10)).await;
                let id=target(&confirmed);
                let first=Box::pin(services.number().allocate(peer.clone(),operation(),input(801))).await.expect("first");
                let body=correction_body(&confirmed,&first,902,previous_year(),42);
                let runtime=database.connections(&database.user).recover().await.expect("runtime writer");
                Box::pin(promote(&services,&institution)).await;
                let _second=Box::pin(services.number().correct(manager.clone(),operation(),id,correction(&body))).await.expect("real correction");
                for sql in [
                    "UPDATE workforce.corrections SET reason='rewritten'",
                    "DELETE FROM workforce.corrections",
                    "UPDATE workforce.revisions SET number.serial=55",
                    "DELETE FROM workforce.revisions",
                    "DELETE FROM workforce.allocations",
                    "DELETE FROM workforce.numbers",
                    "UPDATE workforce.numbers SET revision_id=(SELECT before_id FROM workforce.corrections LIMIT 1)",
                    "INSERT INTO workforce.corrections (transaction_id) VALUES (pg_current_xact_id())",
                ] { assert!(runtime.client().batch_execute(sql).await.is_err(),"{sql}"); }
                let owner=database.connections("postgres").recover().await.expect("isolated corruption owner");
                for sql in [
                    "UPDATE workforce.corrections SET reason=''",
                    "UPDATE workforce.corrections SET before_id=after_id",
                    "UPDATE workforce.corrections SET corrected_at=now()+interval '1 day'",
                    "UPDATE workforce.numbers SET number.serial=999",
                    "DELETE FROM workforce.corrections",
                    "DELETE FROM workforce.revisions WHERE id=(SELECT before_id FROM workforce.corrections LIMIT 1)",
                ] {
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("corruption transaction");
                    // CHECK constraints remain active in replica mode; the corresponding malformed value is already rejected there.
                    match owner.client().batch_execute(sql).await {
                        Ok(())=>assert!(matches!(Box::pin(services.number().verify(owner.client(),chrono::Utc::now())).await,Err(Error::Corrupt(_))),"{sql}"),
                        Err(error)=>assert_eq!(error.code().expect("check SQLSTATE").code(),"23514","{sql}"),
                    }
                    owner.client().batch_execute("ROLLBACK").await.expect("restore original facts");
                }
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica; DELETE FROM workforce.revisions WHERE id=(SELECT after_id FROM workforce.corrections LIMIT 1); COMMIT").await.expect("damage corrected revision");
                services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");
                let context=crate::runtime::Context::new(database.connections(&database.user)).await.expect("restart context");
                let result=Box::pin(Services::start(crate::database::Barrier::begin(context.clone()).await.expect("barrier"),crate::identity::authentication::inventory(crate::identity::authentication::keys()),crate::startup::Settings::new(4,crate::identity::message::Limits::default(),crate::identity::authentication::session_settings(),crate::registration::Limits::default(),crate::identity::authentication::avatar_settings()).expect("settings"),supplier.client(),crate::identity::authentication::unused_client().await)).await;
                assert!(matches!(result,Err(crate::startup::Error::Number(Error::Corrupt(_)))),"startup rejects missing correction version");
                context.stop().await.expect("failed start stopped");
                runtime.terminate().await.expect("runtime ended");owner.terminate().await.expect("owner ended");supplier.stop().await;
            })).await.expect("bounded runtime SQL and startup verification");
        }));
    }

    #[test]
    fn correction_serializes_with_allocation_and_supplement_in_the_same_pool() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for mode in ["assistance","supplement","self_service"] {
                timeout(Duration::from_secs(80),Box::pin(async {
                    let database=Box::pin(Database::initialized(Arc::clone(&server))).await;
                    let mut supplier=Supplier::new().await;
                    let (context,services)=Box::pin(start(database.connections(&database.user),&supplier)).await;
                    let (institution,manager,peer)=Box::pin(members(&database,&services,&mut supplier)).await;
                    Box::pin(promote(&services,&institution)).await;
                    let confirmed=confirmation(&services,&peer,&mobile(10)).await;let id=target(&confirmed);
                    let first=Box::pin(services.number().allocate(peer,operation(),input(801))).await.expect("initial number");
                    let other=Box::pin(register_numbered(&database,&services,&mut supplier,11)).await;
                    Box::pin(admit(&services,&institution,Entry::Institution,&mobile(11),Kind::Default)).await;
                    let year=previous_year()+i32::from(mode=="self_service");
                    let body=correction_body(&confirmed,&first,902,year,1);
                    let mut allocation=supplement_confirmation(&services,&other,&mobile(11),1).await;
                    allocation["subtype"]=json!(904);allocation["year"]=json!(year);
                    let blocker=database.connections(&database.user).recover().await.expect("pool gate");
                    blocker.client().batch_execute("BEGIN").await.expect("gate transaction");
                    super::command::pools(blocker.client(),vec![(Subtype::Internship.category(),year)]).await.expect("hold shared pool");
                    let pid:i32=blocker.client().query_one("SELECT pg_backend_pid()",&[]).await.expect("gate pid").get(0);
                    let first_task=tokio::spawn({let service=services.number().clone();let actor=manager.clone();async move {Box::pin(service.correct(actor,operation(),id,correction(&body))).await}});
                    let second_task=tokio::spawn({let service=services.number().clone();let actor=manager;async move {
                        match mode {
                            "supplement"=>service.supplement(actor,operation(),target(&allocation),supplement(&allocation)).await,
                            "assistance"=>{allocation.as_object_mut().expect("body").remove("serial");service.allocate_member(actor,operation(),target(&allocation),assistance(&allocation)).await},
                            _=>service.allocate(other,operation(),input(904)).await,
                        }
                    }});
                    let observer=database.connections(&database.user).recover().await.expect("observer");
                    timeout(Duration::from_secs(5),async {
                        loop {
                            let count:i64=observer.client().query_one("WITH RECURSIVE blocked(pid) AS (SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) UNION SELECT activity.pid FROM pg_stat_activity activity JOIN blocked ON blocked.pid=ANY(pg_blocking_pids(activity.pid))) SELECT count(*) FROM blocked",&[&pid]).await.expect("actual pool waiters").get(0);
                            if count==2 {break;} tokio::task::yield_now().await;
                        }
                    }).await.expect("both real service operations wait on the same pool");
                    blocker.client().batch_execute("COMMIT").await.expect("release gate");
                    let corrected=first_task.await.expect("correction task");let allocated=second_task.await.expect("allocation task");
                    match (corrected,allocated) {
                        (Ok(corrected),Ok(allocated))=>{assert_ne!(mode,"supplement");assert_eq!(corrected.number().serial(),1);assert_eq!(allocated.number().serial(),2);},
                        (Err(Error::Rejected(Rejection::Occupied)),Ok(allocated))=>assert_eq!(allocated.number().serial(),1),
                        (Ok(corrected),Err(Error::Rejected(Rejection::Occupied)))=>{assert_eq!(mode,"supplement");assert_eq!(corrected.number().serial(),1);},
                        _=>panic!("unexpected concurrent outcome for {mode}"),
                    }
                    Box::pin(services.number().verify(observer.client(),chrono::Utc::now())).await.expect("complete consistent graph after race");
                    blocker.terminate().await.expect("blocker ended");observer.terminate().await.expect("observer ended");
                    services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");supplier.stop().await;
                })).await.expect("bounded correction pool race");
            }
        }));
    }

    /// An institution administrator, a Workforce administrator and a second ordinary member.
    async fn members(
        database: &Database,
        services: &Services,
        supplier: &mut Supplier,
    ) -> (Actor, Actor, Actor) {
        let institution = actor(database, services, supplier).await;
        let member = Box::pin(register(database, services, supplier)).await;
        Box::pin(admit(
            services,
            &institution,
            Entry::Institution,
            "13800000000",
            Kind::Full,
        ))
        .await;
        let peer = Box::pin(register_numbered(database, services, supplier, 10)).await;
        Box::pin(admit(
            services,
            &institution,
            Entry::Institution,
            &mobile(10),
            Kind::Default,
        ))
        .await;
        (institution, member, peer)
    }

    #[test]
    fn concurrent_same_person_commands_do_not_upgrade_shared_accounts() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for correcting in [false, true] {
                timeout(Duration::from_secs(60), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let (institution, manager, peer) = Box::pin(members(&database, &services, &mut supplier)).await;
                    promote(&services, &institution).await;
                    let confirmed = confirmation(&services, &peer, &mobile(10)).await;
                    let id = target(&confirmed);
                    let body = if correcting {
                        let first = Box::pin(services.number().allocate(peer, operation(), input(801))).await.expect("initial");
                        correction_body(&confirmed, &first, 902, previous_year(), 42)
                    } else { confirmed };
                    let blocker = database.connections("postgres").recover().await.expect("person gate");
                    let observer = database.connections("postgres").recover().await.expect("wait observer");
                    blocker.client().batch_execute("BEGIN").await.expect("gate transaction");
                    blocker.client().query_one("SELECT id FROM identity.persons WHERE id=$1 FOR UPDATE", &[&id]).await.expect("hold target");
                    let pid: i32 = blocker.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("gate pid").get(0);
                    let mut tasks = Vec::new();
                    for _ in 0..2 {
                        let service = services.number().clone();
                        let actor = manager.clone();
                        let input = body.clone();
                        tasks.push(tokio::spawn(async move {
                            if correcting { service.correct(actor, operation(), id, correction(&input)).await }
                            else { service.allocate_member(actor, operation(), id, assistance(&input)).await }
                        }));
                    }
                    timeout(Duration::from_secs(3), async {
                        loop {
                            let count: i64 = observer.client().query_one("WITH RECURSIVE blocked(pid) AS (SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) UNION SELECT activity.pid FROM pg_stat_activity activity JOIN blocked ON blocked.pid=ANY(pg_blocking_pids(activity.pid))) SELECT count(*) FROM blocked", &[&pid]).await.expect("both commands hold shared accounts").get(0);
                            if count == 2 { break; }
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("both commands reached the held person");
                    blocker.client().batch_execute("COMMIT").await.expect("release person");
                    let mut accepted = 0;
                    for task in tasks {
                        match timeout(Duration::from_secs(5), task).await.expect("no account lock deadlock").expect("task") {
                            Ok(_) => accepted += 1,
                            Err(Error::Rejected(reason)) => assert_eq!(reason, if correcting { Rejection::TargetChanged } else { Rejection::Conflict }),
                            Err(error) => panic!("unexpected concurrent failure: {error:?}"),
                        }
                    }
                    assert_eq!(accepted, 1);
                    let row = observer.client().query_one("SELECT sequence,version FROM workforce.numbers WHERE id=$1", &[&id]).await.expect("one accepted transition");
                    assert_eq!((row.get::<_,i64>(0),row.get::<_,i64>(1)), if correcting {(2,2)} else {(1,1)});
                    blocker.terminate().await.expect("gate ended"); observer.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
                })).await.expect("bounded same-person race");
            }
        }));
    }

    #[test]
    fn damaged_or_missing_state_never_frees_a_pool_slot() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["deleted", "moved pool", "pointer", "seal", "key", "rollback"] {
                timeout(Duration::from_secs(60), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let (institution, manager, peer) = Box::pin(members(&database, &services, &mut supplier)).await;
                    promote(&services, &institution).await;
                    let confirmed = confirmation(&services, &peer, &mobile(10)).await;
                    let id = target(&confirmed);
                    let first = Box::pin(services.number().allocate(peer, operation(), input(902))).await.expect("occupied pool");
                    let owner = database.connections("postgres").recover().await.expect("isolated owner");
                    let saved: String = owner.client().query_one("SELECT to_jsonb(state)::text FROM workforce.numbers state WHERE id=$1", &[&id]).await.expect("original sealed state").get(0);
                    if case == "rollback" {
                        let body = correction_body(&confirmed, &first, 903, first.number().year(), 2);
                        Box::pin(services.number().correct(manager.clone(), operation(), id, correction(&body))).await.expect("real later state");
                    }
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated damage");
                    if case == "rollback" {
                        owner.client().execute("UPDATE workforce.numbers SET (revision_id,number,held,tenure_id,changed_at,sequence,version,seal,seal_key_id,seal_version)=(SELECT revision_id,number,held,tenure_id,changed_at,sequence,version,seal,seal_key_id,seal_version FROM json_populate_record(NULL::workforce.numbers,$2::text::json)) WHERE id=$1", &[&id,&saved]).await.expect("restore exact earlier seal");
                    } else {
                        owner.client().batch_execute(match case {
                            "deleted" => "DELETE FROM workforce.numbers",
                            "moved pool" => "UPDATE workforce.numbers SET number.year=(number).year-1",
                            "pointer" => "UPDATE workforce.numbers SET revision_id=gen_random_uuid()",
                            "seal" => "UPDATE workforce.numbers SET seal=set_byte(seal,0,get_byte(seal,0)#1)",
                            "key" => "UPDATE workforce.numbers SET seal_key_id=9223372036854775807",
                            _ => unreachable!(),
                        }).await.expect("damage state");
                    }
                    owner.client().batch_execute("COMMIT").await.expect("damage visible");
                    let service = services.number();
                    let now = chrono::Utc::now();
                    assert!(crate::history::find::<super::state::State>(owner.client(),service.app.keys(),service.app.anchors(),crate::history::Read::Locked,id,now).await.is_err(), "{case}: primary key");
                    assert!(super::state::holder(owner.client(),service.app.keys(),service.app.anchors(),crate::history::Read::Locked,first.number(),now).await.is_err(), "{case}: unique locator");
                    assert!(matches!(Box::pin(service.allocate(manager, operation(), input(902))).await, Err(Error::Corrupt(_))), "{case}: deleted or moved rows never free a serial");
                    assert!(service.app.registry().ready().is_err());
                    assert!(matches!(service.verify(owner.client(),now).await, Err(Error::Corrupt(_))), "{case}: startup also rejects");
                    owner.terminate().await.expect("owner ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
                })).await.expect("bounded state damage matrix");
            }
        }));
    }

    #[test]
    fn number_changes_cannot_commit_without_their_original_completion() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(120), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (_, manager, beneficiary, occupier, body, original, temporary, occupied) =
                    Box::pin(misoccupation(&database, &services, &mut supplier)).await;
                let occupier_identity = services.current().read(&occupier).await.expect("occupier");
                let restore = json!({
                    "beneficiary": {"personId": target(&body), "revisionId": body["target"]["revisionId"], "current": temporary.id(), "origin": original.id()},
                    "occupier": {"personId": occupier_identity.person_id(), "revisionId": occupier_identity.revision_id(), "current": occupied.id()},
                    "reason": "Verified original holder record"
                });
                let owner = database.connections("postgres").recover().await.expect("isolated fault owner");
                let snapshot = "SELECT jsonb_build_array((SELECT jsonb_agg(row ORDER BY id) FROM workforce.numbers row),(SELECT jsonb_agg(row ORDER BY id) FROM workforce.transitions row),(SELECT jsonb_agg(row ORDER BY id) FROM workforce.revisions row),(SELECT jsonb_agg(row ORDER BY family,operation_id) FROM workforce.completions row))::text";
                let before: String = owner.client().query_one(snapshot, &[]).await.expect("before").get(0);
                owner.client().batch_execute("CREATE FUNCTION workforce.test_skip_completion() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$; CREATE TRIGGER test_skip_completion BEFORE INSERT ON workforce.completions FOR EACH ROW EXECUTE FUNCTION workforce.test_skip_completion()").await.expect("omit only the completion insert");
                let service = services.number();
                assert!(matches!(Box::pin(service.allocate(manager.clone(), operation(), input(100))).await, Err(Error::Unavailable(_))));
                assert_eq!(owner.client().query_one(snapshot, &[]).await.expect("allocation rolled back").get::<_,String>(0), before);
                let change = correction_body(&body, &temporary, 903, previous_year(), 43);
                assert!(matches!(Box::pin(service.correct(manager.clone(), operation(), target(&body), correction(&change))).await, Err(Error::Unavailable(_))));
                assert_eq!(owner.client().query_one(snapshot, &[]).await.expect("correction rolled back").get::<_,String>(0), before);
                let recovery_operation = operation();
                assert!(matches!(Box::pin(service.restore(manager.clone(), recovery_operation, recovery(restore.clone()))).await, Err(Error::Unavailable(_))));
                assert_eq!(owner.client().query_one(snapshot, &[]).await.expect("whole recovery group rolled back").get::<_,String>(0), before);
                assert_eq!(service.read_current(&beneficiary).await.expect("beneficiary anchor").expect("held").id(), temporary.id());
                assert_eq!(service.read_current(&occupier).await.expect("occupier anchor").expect("held").id(), occupied.id());
                owner.client().batch_execute("DROP TRIGGER test_skip_completion ON workforce.completions; DROP FUNCTION workforce.test_skip_completion()").await.expect("remove only injected fault");
                Box::pin(service.restore(manager, recovery_operation, recovery(restore))).await.expect("original operation remains usable after proven rollback");
                owner.terminate().await.expect("owner ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
            })).await.expect("bounded completion pairing");
        }));
    }

    #[test]
    fn permanent_completions_authenticate_input_actor_and_original_result() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["input", "action", "target", "subject", "time", "seal", "missing"] {
                timeout(Duration::from_secs(60), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let (_, member, _) = Box::pin(members(&database, &services, &mut supplier)).await;
                    let original = operation();
                    Box::pin(services.number().allocate(member.clone(), original, input(801))).await.expect("real permanent result");
                    let owner = database.connections("postgres").recover().await.expect("isolated owner");
                    let row = owner.client().query_one("SELECT expires_at IS NULL, octet_length(input)>37 FROM workforce.completions WHERE family='number_allocation' AND operation_id=$1", &[&original]).await.expect("encrypted permanent completion");
                    assert!(row.get::<_,bool>(0) && row.get::<_,bool>(1));
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated damage");
                    owner.client().batch_execute(match case {
                        "input" => "UPDATE workforce.completions SET input=set_byte(input,36,get_byte(input,36)#1) WHERE family='number_allocation'",
                        "action" => "UPDATE workforce.completions SET action='assistance' WHERE family='number_allocation'",
                        "target" => "UPDATE workforce.completions SET target=gen_random_uuid()::text WHERE family='number_allocation'",
                        "subject" => "ALTER TABLE workforce.completions DROP CONSTRAINT completions_subject_check; UPDATE workforce.completions SET subject='system' WHERE family='number_allocation'",
                        "time" => "UPDATE workforce.completions SET created_at=created_at-interval '1 second' WHERE family='number_allocation'",
                        "seal" => "UPDATE workforce.completions SET seal=set_byte(seal,0,get_byte(seal,0)#1) WHERE family='number_allocation'",
                        "missing" => "DELETE FROM workforce.completions WHERE family='number_allocation'",
                        _ => unreachable!(),
                    }).await.expect("corrupt original");
                    owner.client().batch_execute("COMMIT").await.expect("damage visible");
                    assert!(matches!(services.number().verify(owner.client(), chrono::Utc::now()).await, Err(Error::Corrupt(_))), "{case}: startup binds every record to its completion");
                    if case != "missing" {
                        assert!(matches!(Box::pin(services.number().allocate(member, original, input(801))).await, Err(Error::Corrupt(_))), "{case}: replay authenticates its original");
                        assert!(services.number().app.registry().ready().is_err());
                    }
                    owner.terminate().await.expect("owner ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
                })).await.expect("bounded permanent completion damage");
            }
        }));
    }

    #[test]
    fn input_accepts_only_a_fixed_subtype() {
        for value in [
            json!({}),
            json!({"subtype":101}),
            json!({"subtype":"100"}),
            json!({"subtype":100,"year":2020}),
        ] {
            assert!(serde_json::from_value::<Input>(value).is_err());
        }
        assert_eq!(
            serde_json::from_value::<Input>(json!({"subtype":904}))
                .expect("student")
                .subtype,
            Subtype::Student
        );
    }

    #[test]
    fn self_service_takes_the_smallest_free_serial_of_the_current_year() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (institution, member, peer) = Box::pin(members(&database, &services, &mut supplier)).await;
                assert!(matches!(Box::pin(services.number().allocate(institution.clone(), operation(), input(100))).await, Err(Error::Denied)), "institutional authority without a grant does not allocate");
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let current = services.current().read(&member).await.expect("member identity");
                let before: i64 = observer.client().query_one("SELECT sequence AS person_sequence FROM identity.persons WHERE id = $1", &[&current.person_id()]).await.expect("sequence before").get(0);
                let year: i32 = observer.client().query_one("SELECT date_part('year', now() AT TIME ZONE 'Asia/Shanghai')::integer", &[]).await.expect("independent China Standard Time year").get(0);
                let original = operation();
                let first = Box::pin(services.number().allocate(member.clone(), original, input(801))).await.expect("first self-service number");
                assert_eq!((first.number().subtype(), first.number().year(), first.number().serial()), (Subtype::Company, year, 1));
                let second = Box::pin(services.number().allocate(peer.clone(), operation(), input(802))).await.expect("same category shares the pool");
                assert_eq!((second.number().subtype(), second.number().year(), second.number().serial()), (Subtype::Security, year, 2));
                let other = Box::pin(services.number().allocate(institution.clone(), operation(), input(904))).await;
                assert!(matches!(other, Err(Error::Denied)));
                let row = observer.client().query_one("SELECT number.revision_id, (number.number).serial, allocation.source, allocation.operator_id, allocation.operation_id, allocation.sequence, person.sequence AS person_sequence FROM workforce.numbers number JOIN workforce.allocations allocation ON allocation.number_id = number.revision_id JOIN identity.persons person ON person.id = number.id WHERE number.id = $1", &[&current.person_id()]).await.expect("one permanent allocation before replay");
                assert_eq!(row.get::<_, super::Id>("revision_id"), first.id());
                assert_eq!(row.get::<_, i16>("serial"), 1);
                assert_eq!(row.get::<_, &str>("source"), "self_service");
                assert_eq!(row.get::<_, crate::identity::person::revision::Id>("operator_id"), current.revision_id());
                assert_eq!(row.get::<_, operation::Id>("operation_id"), original);
                assert_eq!((row.get::<_, i64>("sequence"), row.get::<_, i64>("person_sequence")), (1, before));
                let replay = Box::pin(services.number().allocate(member.clone(), original, input(801))).await.expect("original replays");
                assert_eq!(replay.id(), first.id());
                assert!(matches!(Box::pin(services.number().allocate(member.clone(), original, input(803))).await, Err(Error::Mismatch)));
                assert!(matches!(Box::pin(services.number().allocate(member.clone(), operation(), input(100))).await, Err(Error::Rejected(Rejection::Conflict))), "self-service is a first allocation");
                let third = Box::pin(services.number().allocate(Box::pin(register_numbered(&database, &services, &mut supplier, 11)).await, operation(), input(100))).await;
                assert!(matches!(third, Err(Error::Denied)), "a registered account without a grant cannot allocate");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM workforce.allocations", &[]).await.expect("allocations").get::<_, i64>(0), 2);
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                assert_eq!(Box::pin(services.number().allocate(member, original, input(801))).await.expect("restart verifies and replays").id(), first.id());
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded allocation");
        }));
    }

    #[test]
    fn seals_reject_rewrites_that_every_other_number_rule_accepts() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (_, member, _) = Box::pin(members(&database, &services, &mut supplier)).await;
                Box::pin(services.number().allocate(member.clone(), operation(), input(902))).await.expect("real allocation");
                let owner = database.connections("postgres").recover().await.expect("isolated owner");
                let number = services.number();
                let verify = || number.verify(owner.client(), chrono::Utc::now());
                verify().await.expect("real facts authenticate");
                for (case, damage) in [
                    ("allocation time", "UPDATE workforce.allocations SET allocated_at = allocated_at - interval '1 microsecond'"),
                    ("revision value", "UPDATE workforce.revisions SET number.serial = 903; UPDATE workforce.numbers SET number.serial = 903"),
                    ("seal bytes", "UPDATE workforce.allocations SET seal = set_byte(seal, 0, get_byte(seal, 0) # 1)"),
                ] {
                    owner.client().batch_execute(&format!("BEGIN; SET LOCAL session_replication_role=replica; {damage}")).await.expect("isolated damage");
                    let error = verify().await.expect_err(case);
                    assert!(matches!(error, Error::Corrupt(_)), "{case}: {error:?}");
                    assert!(crate::runtime::work::Failure::unreliable(&error), "{case}");
                    owner.client().batch_execute("ROLLBACK").await.expect("restore the original facts");
                }
                let active = crate::keys::Id::new(1).expect("rotated key");
                let rotated = crate::keys::seal::Keys::new(crate::keys::Derived::fixed(active, vec![crate::keys::Entry::new(active, crate::keys::Material(zeroize::Zeroizing::new([13; 32])))]));
                assert!(matches!(super::verification::inspect(owner.client(), crate::history::Keys { data: number.app.keys().data, seals: &rotated }, &number.revisions, chrono::Utc::now()).await, Err(Error::Corrupt(_))), "verification requires every retained seal key");
                // The CHECK keeps the version even in replica mode; drop it in this disposable database.
                owner.client().batch_execute("DO $$ DECLARE name text; BEGIN FOR name IN SELECT conname FROM pg_constraint WHERE conrelid = 'workforce.allocations'::regclass AND contype = 'c' AND pg_get_constraintdef(oid) LIKE '%seal_version%' LOOP EXECUTE format('ALTER TABLE workforce.allocations DROP CONSTRAINT %I', name); END LOOP; END $$; BEGIN; SET LOCAL session_replication_role=replica; UPDATE workforce.allocations SET seal_version = 2").await.expect("version damage");
                assert!(matches!(verify().await, Err(Error::Corrupt(_))), "an unknown seal version is damage");
                owner.client().batch_execute("ROLLBACK").await.expect("version restored");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica; UPDATE workforce.allocations SET allocated_at = allocated_at - interval '1 microsecond'; COMMIT").await.expect("committed damage");
                assert!(services.number().read_current(&member).await.is_ok(), "the current read consumes its intact state seal");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let context = crate::runtime::Context::new(database.connections(&database.user)).await.expect("restart context");
                let result = Box::pin(Services::start(crate::database::Barrier::begin(context.clone()).await.expect("startup barrier"), crate::identity::authentication::inventory(crate::identity::authentication::keys()), crate::startup::Settings::new(4,crate::identity::message::Limits::default(),crate::identity::authentication::session_settings(),crate::registration::Limits::default(),crate::identity::authentication::avatar_settings()).expect("settings"),supplier.client(),crate::identity::authentication::unused_client().await)).await;
                assert!(matches!(result, Err(crate::startup::Error::Number(Error::Corrupt(_)))), "startup rejects a rewritten allocation time");
                context.stop().await.expect("failed startup stopped");
                owner.terminate().await.expect("owner ended"); supplier.stop().await;
            })).await.expect("bounded seal damage check");
        }));
    }

    #[test]
    fn current_read_returns_the_held_revision_after_exit_and_rejects_damage() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (institution, member, _) = Box::pin(members(&database, &services, &mut supplier)).await;
                assert!(services.number().read_current(&member).await.expect("member before allocation").is_none(), "no number reads as an empty value");
                assert!(services.number().read_current(&institution).await.expect("no Grant is needed to read").is_none());
                let allocated = Box::pin(services.number().allocate(member.clone(), operation(), input(902))).await.expect("real allocation");
                let read = services.number().read_current(&member).await.expect("current number").expect("held number");
                assert_eq!((read.id(), read.number()), (allocated.id(), allocated.number()));
                let grant = services.grant().read_latest(&member).await.expect("member grant");
                Box::pin(services.grant().terminate(member.clone(), operation(), grant.id())).await.expect("real exit");
                let kept = services.number().read_current(&member).await.expect("after exit").expect("leaving Workforce keeps the number");
                assert_eq!(kept.id(), allocated.id());
                let owner = database.connections("postgres").recover().await.expect("isolated owner");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica; DELETE FROM workforce.allocations; COMMIT").await.expect("creating fact removed");
                assert_eq!(services.number().read_current(&member).await.expect("current reads its sealed state").expect("held").id(), allocated.id());
                assert!(matches!(services.number().verify(owner.client(), chrono::Utc::now()).await, Err(Error::Corrupt(_))), "startup checks the complete history");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica; DELETE FROM workforce.numbers; COMMIT").await.expect("state removed");
                assert!(matches!(services.number().read_current(&member).await, Err(Error::Corrupt(_))), "missing anchored state is damage");
                assert!(matches!(services.number().read_current(&institution).await, Err(Error::Unavailable(_))), "damage closes admission");
                owner.terminate().await.expect("owner ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded current read");
        }));
    }

    #[test]
    fn allocation_requires_a_current_unsuspended_grant_and_a_free_serial() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (_, member, peer) = Box::pin(members(&database, &services, &mut supplier)).await;
                let grant = services.grant().read_latest(&peer).await.expect("peer grant");
                let current = services.current().read(&peer).await.expect("peer identity");
                let target = json!({"target":{"accountId":current.id(),"personId":current.person_id(),"revisionId":current.revision_id(),"mobile":mobile(10)}});
                let pause = Box::pin(services.grant().suspend(member.clone(), operation(), grant.id(), serde_json::from_value(target.clone()).expect("pause"))).await.expect("real Grant suspension");
                assert!(matches!(Box::pin(services.number().allocate(peer.clone(), operation(), input(100))).await, Err(Error::Denied)), "a suspended grant cannot allocate");
                Box::pin(services.grant().resume(member.clone(), operation(), pause.id(), serde_json::from_value(target).expect("resume"))).await.expect("real resumption");
                Box::pin(services.grant().terminate(peer.clone(), operation(), grant.id())).await.expect("real exit");
                assert!(matches!(Box::pin(services.number().allocate(peer.clone(), operation(), input(100))).await, Err(Error::Denied)), "an ended grant cannot allocate");
                let owner = database.connections("postgres").recover().await.expect("isolated owner");
                // Session and time are decided after the pool lock, not before waiting for it.
                let blocker = database.connections("postgres").recover().await.expect("pool holder");
                blocker.client().batch_execute("BEGIN").await.expect("pool transaction");
                super::command::pools(blocker.client(), vec![(2, previous_year() + 1)]).await.expect("hold the nursing pool");
                let pid: i32 = blocker.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("holder").get(0);
                let waiting = tokio::spawn({ let services = services.clone(); async move { Box::pin(services.number().allocate(member, operation(), input(200))).await } });
                timeout(Duration::from_secs(3), async { while !owner.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)) AND query LIKE 'SELECT pg_catalog.pg_advisory_xact_lock(%')", &[&pid]).await.expect("wait graph").get::<_, bool>(0) { tokio::task::yield_now().await; } }).await.expect("allocation waits for the pool");
                tokio::time::sleep(Duration::from_millis(200)).await;
                let released: chrono::DateTime<chrono::Utc> = blocker.client().query_one("SELECT clock_timestamp()", &[]).await.expect("release time").get(0);
                blocker.client().batch_execute("COMMIT").await.expect("pool released");
                let nursing = waiting.await.expect("allocation task").expect("another category still has serials");
                assert_eq!(nursing.number().serial(), 1);
                let allocated_at: chrono::DateTime<chrono::Utc> = owner.client().query_one("SELECT allocated_at FROM workforce.allocations", &[]).await.expect("one allocation").get(0);
                assert!(allocated_at >= released, "allocation time {allocated_at} is decided after the pool wait ended at {released}");
                blocker.terminate().await.expect("holder ended");
                owner.terminate().await.expect("owner ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded eligibility");
        }));
    }

    #[test]
    fn allocation_recovers_its_original_result_across_commit_faults() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["lost", "rollback", "unknown", "cancelled"] {
                timeout(Duration::from_secs(45), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let (_, member, _) = Box::pin(members(&database, &services, &mut supplier)).await;
                    let operation_id = operation();
                    proxy.arm(match case { "lost" => Fault::LostCommit, "rollback" => Fault::RolledBack, "unknown" => Fault::UnknownCommit, "cancelled" => Fault::DelayedCommit, _ => unreachable!() });
                    let service = services.number().clone(); let actor = member.clone();
                    let waiter = tokio::spawn(async move { service.allocate(actor, operation_id, input(100)).await });
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("original commit intercepted");
                    if case == "cancelled" {
                        waiter.abort(); assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
                        services.close();
                        assert!(timeout(Duration::from_millis(50), services.drain()).await.is_err(), "a cancelled wait keeps accepted work until completion");
                        proxy.release(); services.drain().await.expect("accepted work drained");
                    } else {
                        proxy.release();
                        let result = timeout(Duration::from_secs(15), waiter).await.expect("bounded outcome").expect("waiter");
                        match case {
                            "rollback" => assert!(matches!(result, Err(Error::Unavailable(_)))),
                            "unknown" => assert!(matches!(result, Err(Error::Unknown(_)))),
                            _ => assert_eq!(result.expect("recovered original").number().serial(), 1),
                        }
                        services.close(); services.drain().await.expect("drained");
                    }
                    context.stop().await.expect("stopped");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    // An unknown outcome concludes once the recovery BEGIN fails; release only lets the
                    // proxy forward the original COMMIT. Cross the actor's person lock before reading.
                    observer.client().batch_execute("BEGIN").await.expect("observation transaction");
                    observer.client().query_one("SELECT person.id FROM identity.persons person JOIN identity.accounts account ON account.person_id = person.id WHERE account.id = $1 FOR UPDATE OF person", &[&member.account_id()]).await.expect("original backend released the actor's person lock");
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM workforce.allocations WHERE operation_id = $1", &[&operation_id]).await.expect("durable state before replay").get::<_, i64>(0), i64::from(case != "rollback"));
                    observer.client().batch_execute("COMMIT").await.expect("observation complete");
                    proxy.stop().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    Box::pin(services.number().allocate(member, operation_id, input(100))).await.expect("original result or explicit retry after proven rollback");
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM workforce.numbers", &[]).await.expect("one number after replay").get::<_, i64>(0), 1);
                    observer.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded recovery case");
            }
        }));
    }

    #[test]
    fn runtime_sql_protects_permanent_values_records_and_state_pairing() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(
                Duration::from_secs(60),
                Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) =
                        start(database.connections(&database.user), &supplier).await;
                    let (_, member, _) =
                        Box::pin(members(&database, &services, &mut supplier)).await;
                    Box::pin(services.number().allocate(member, operation(), input(100)))
                        .await
                        .expect("real allocation");
                    let runtime = database
                        .connections(&database.user)
                        .recover()
                        .await
                        .expect("runtime identity");
                    for (statement, code) in [
                        ("UPDATE workforce.numbers SET version=version+1", "23514"),
                        (
                            "UPDATE workforce.numbers SET sequence=sequence+1,version=version+1",
                            "23514",
                        ),
                        (
                            "UPDATE workforce.numbers SET revision_id=gen_random_uuid()",
                            "23503",
                        ),
                        ("DELETE FROM workforce.numbers", "42501"),
                        (
                            "UPDATE workforce.allocations SET sequence=sequence",
                            "42501",
                        ),
                        ("DELETE FROM workforce.allocations", "42501"),
                        (
                            "UPDATE workforce.revisions SET person_id=person_id",
                            "42501",
                        ),
                        ("DELETE FROM workforce.revisions", "42501"),
                        (
                            "DELETE FROM workforce.completions WHERE family='number_allocation'",
                            "23514",
                        ),
                    ] {
                        assert_eq!(
                            runtime
                                .client()
                                .batch_execute(statement)
                                .await
                                .expect_err(statement)
                                .code()
                                .expect("protection")
                                .code(),
                            code,
                            "{statement}"
                        );
                    }
                    runtime.terminate().await.expect("runtime ended");
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    supplier.stop().await;
                }),
            )
            .await
            .expect("bounded structural protection");
        }));
    }

    #[test]
    fn history_pages_all_fact_kinds_with_mixed_ordering_and_masked_fixed_identities() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(
                Duration::from_secs(90),
                Box::pin(async {
                    let database = Box::pin(Database::initialized(Arc::clone(&server))).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) =
                        Box::pin(start(database.connections(&database.user), &supplier)).await;
                    let (institution, manager, peer) =
                        Box::pin(members(&database, &services, &mut supplier)).await;
                    Box::pin(promote(&services, &institution)).await;

                    // 1. Peer does self-service allocation
                    let self_alloc = Box::pin(services.number().allocate(
                        peer.clone(),
                        operation(),
                        input(801),
                    ))
                    .await
                    .expect("self service");
                    let peer_current = services.current().read(&peer).await.expect("peer identity");

                    // Read peer history (self-service fact)
                    let peer_history = services
                        .number()
                        .history(
                            &manager,
                            peer_current.person_id(),
                            crate::pagination::Pagination::new(1, 20).expect("page"),
                        )
                        .await
                        .expect("peer history");
                    assert_eq!(peer_history.count(), 1);
                    assert_eq!(peer_history.revisions().len(), 1);
                    let item = &peer_history.revisions()[0];
                    match item.kind() {
                        super::HistoryKind::Allocation {
                            source,
                            number,
                            reason,
                            ..
                        } => {
                            assert_eq!(*source, super::Source::SelfService);
                            assert_eq!(number.id(), self_alloc.id());
                            assert_eq!(number.number(), self_alloc.number());
                            assert!(reason.is_none());
                            assert_eq!(item.operator().person_id(), peer_current.person_id());
                            assert_eq!(item.operator().name(), "H*");
                            assert_eq!(item.sequence(), 1);
                        }
                        _ => panic!("expected allocation"),
                    }

                    // 2. Member 11: assistance allocation + 2 corrections
                    let other =
                        Box::pin(register_numbered(&database, &services, &mut supplier, 11)).await;
                    Box::pin(admit(
                        &services,
                        &institution,
                        Entry::Institution,
                        &mobile(11),
                        Kind::Default,
                    ))
                    .await;
                    let other_confirm = confirmation(&services, &other, &mobile(11)).await;
                    let other_id = target(&other_confirm);
                    let assist_alloc = Box::pin(services.number().allocate_member(
                        manager.clone(),
                        operation(),
                        other_id,
                        assistance(&other_confirm),
                    ))
                    .await
                    .expect("assistance");

                    let body1 =
                        correction_body(&other_confirm, &assist_alloc, 802, previous_year(), 42);
                    let corr1 = Box::pin(services.number().correct(
                        manager.clone(),
                        operation(),
                        other_id,
                        correction(&body1),
                    ))
                    .await
                    .expect("correction 1");

                    let body2 =
                        correction_body(&other_confirm, &corr1, 902, previous_year() - 1, 10);
                    let corr2 = Box::pin(services.number().correct(
                        manager.clone(),
                        operation(),
                        other_id,
                        correction(&body2),
                    ))
                    .await
                    .expect("correction 2");

                    let other_history = services
                        .number()
                        .history(
                            &manager,
                            other_id,
                            crate::pagination::Pagination::new(1, 20).expect("page"),
                        )
                        .await
                        .expect("other history");
                    assert_eq!(other_history.count(), 3);
                    assert_eq!(other_history.revisions().len(), 3);

                    // Sequence descending ordering
                    let seq0 = other_history.revisions()[0].sequence();
                    let seq1 = other_history.revisions()[1].sequence();
                    let seq2 = other_history.revisions()[2].sequence();
                    assert!(seq0 > seq1 && seq1 > seq2);

                    // Revision 0 is correction 2
                    let mgr_current = services.current().read(&manager).await.expect("manager");
                    let item = &other_history.revisions()[0];
                    match item.kind() {
                        super::HistoryKind::Correction {
                            before,
                            after,
                            reason,
                            ..
                        } => {
                            assert_eq!(before.id(), corr1.id());
                            assert_eq!(after.id(), corr2.id());
                            assert_eq!(reason.as_str(), "Verified original personnel record");
                            assert_eq!(item.operator().person_id(), mgr_current.person_id());
                            assert_eq!(item.operator().name(), "T*");
                        }
                        _ => panic!("expected correction"),
                    }

                    // Revision 1 is correction 1
                    match other_history.revisions()[1].kind() {
                        super::HistoryKind::Correction { before, after, .. } => {
                            assert_eq!(before.id(), assist_alloc.id());
                            assert_eq!(after.id(), corr1.id());
                        }
                        _ => panic!("expected correction"),
                    }

                    // Revision 2 is assistance allocation
                    let item = &other_history.revisions()[2];
                    match item.kind() {
                        super::HistoryKind::Allocation {
                            source,
                            number,
                            reason,
                            ..
                        } => {
                            assert_eq!(*source, super::Source::Assistance);
                            assert_eq!(number.id(), assist_alloc.id());
                            assert_eq!(reason.as_deref(), Some("Verified original entry record"));
                            assert_eq!(item.operator().name(), "T*");
                        }
                        _ => panic!("expected allocation"),
                    }

                    // Pagination: page 1 (size 2), page 2 (size 2), out-of-range page 3 (size 2)
                    let page1 = services
                        .number()
                        .history(
                            &manager,
                            other_id,
                            crate::pagination::Pagination::new(1, 2).expect("page 1"),
                        )
                        .await
                        .expect("page 1");
                    assert_eq!(page1.count(), 3);
                    assert_eq!(page1.revisions().len(), 2);
                    assert_eq!(page1.revisions()[0].id(), other_history.revisions()[0].id());
                    assert_eq!(page1.revisions()[1].id(), other_history.revisions()[1].id());

                    let page2 = services
                        .number()
                        .history(
                            &manager,
                            other_id,
                            crate::pagination::Pagination::new(2, 2).expect("page 2"),
                        )
                        .await
                        .expect("page 2");
                    assert_eq!(page2.count(), 3);
                    assert_eq!(page2.revisions().len(), 1);
                    assert_eq!(page2.revisions()[0].id(), other_history.revisions()[2].id());

                    let page3 = services
                        .number()
                        .history(
                            &manager,
                            other_id,
                            crate::pagination::Pagination::new(3, 2).expect("page 3"),
                        )
                        .await
                        .expect("page 3");
                    assert_eq!(page3.count(), 3);
                    assert!(page3.revisions().is_empty());

                    // 3. Member 12: supplement allocation
                    let supp_person =
                        Box::pin(register_numbered(&database, &services, &mut supplier, 12)).await;
                    Box::pin(admit(
                        &services,
                        &institution,
                        Entry::Institution,
                        &mobile(12),
                        Kind::Default,
                    ))
                    .await;
                    let supp_confirm =
                        supplement_confirmation(&services, &supp_person, &mobile(12), 1).await;
                    let supp_id = target(&supp_confirm);
                    let supp_alloc = Box::pin(services.number().supplement(
                        manager.clone(),
                        operation(),
                        supp_id,
                        supplement(&supp_confirm),
                    ))
                    .await
                    .expect("supplement");

                    let supp_history = services
                        .number()
                        .history(
                            &manager,
                            supp_id,
                            crate::pagination::Pagination::new(1, 20).expect("page"),
                        )
                        .await
                        .expect("supp history");
                    assert_eq!(supp_history.count(), 1);
                    let item = &supp_history.revisions()[0];
                    match item.kind() {
                        super::HistoryKind::Allocation {
                            source,
                            number,
                            reason,
                            ..
                        } => {
                            assert_eq!(*source, super::Source::Supplement);
                            assert_eq!(number.id(), supp_alloc.id());
                            assert_eq!(reason.as_deref(), Some("Verified original entry record"));
                            assert_eq!(item.operator().name(), "T*");
                        }
                        _ => panic!("expected allocation"),
                    }

                    // 4. Masked fixed identities preserved after later real-name correction
                    Box::pin(
                        services.correction().correct(
                            institution.clone(),
                            operation(),
                            serde_json::from_value(json!({
                                "target": {
                                    "personId": mgr_current.person_id(),
                                    "revisionId": mgr_current.revision_id(),
                                },
                                "name": "Renamed Manager",
                                "idNumber": "00000020000229007X",
                            }))
                            .expect("correction"),
                        ),
                    )
                    .await
                    .expect("rename manager");

                    let hist_after_rename = services
                        .number()
                        .history(
                            &manager,
                            other_id,
                            crate::pagination::Pagination::new(1, 20).expect("page"),
                        )
                        .await
                        .expect("history after rename");
                    assert_eq!(hist_after_rename.revisions()[0].operator().name(), "T*");
                    assert_eq!(
                        hist_after_rename.revisions()[0].operator().revision_id(),
                        mgr_current.revision_id()
                    );

                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    supplier.stop().await;
                }),
            )
            .await
            .expect("bounded history disclosure");
        }));
    }

    #[test]
    fn history_allows_ended_grant_and_cancelled_targets_and_enforces_reader_authority() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(
                Duration::from_secs(90),
                Box::pin(async {
                    let database = Box::pin(Database::initialized(Arc::clone(&server))).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) =
                        Box::pin(start(database.connections(&database.user), &supplier)).await;
                    let (institution, manager, peer) =
                        Box::pin(members(&database, &services, &mut supplier)).await;
                    Box::pin(promote(&services, &institution)).await;

                    let peer_alloc = Box::pin(services.number().allocate(
                        peer.clone(),
                        operation(),
                        input(801),
                    ))
                    .await
                    .expect("peer allocation");
                    let peer_current = services.current().read(&peer).await.expect("peer current");

                    // Authority: peer without workforce.numbers cannot read history (even their own!)
                    assert!(matches!(
                        services
                            .number()
                            .history(
                                &peer,
                                peer_current.person_id(),
                                crate::pagination::Pagination::new(1, 20).expect("page")
                            )
                            .await,
                        Err(Error::Denied)
                    ));

                    // Unknown target returns Absent
                    let missing_id =
                        serde_json::from_value(json!(Uuid::new_v4())).expect("person id");
                    assert!(matches!(
                        services
                            .number()
                            .history(
                                &manager,
                                missing_id,
                                crate::pagination::Pagination::new(1, 20).expect("page")
                            )
                            .await,
                        Err(Error::Rejected(Rejection::Absent))
                    ));

                    // Target with 0 facts returns count 0, revisions empty
                    let unnumbered =
                        Box::pin(register_numbered(&database, &services, &mut supplier, 11)).await;
                    let unnumbered_current = services
                        .current()
                        .read(&unnumbered)
                        .await
                        .expect("unnumbered current");
                    let empty_hist = services
                        .number()
                        .history(
                            &manager,
                            unnumbered_current.person_id(),
                            crate::pagination::Pagination::new(1, 20).expect("page"),
                        )
                        .await
                        .expect("empty history");
                    assert_eq!(empty_hist.count(), 0);
                    assert!(empty_hist.revisions().is_empty());

                    // Target whose Grant ended remains readable
                    let peer_grant = services
                        .grant()
                        .read_latest(&peer)
                        .await
                        .expect("peer grant");
                    Box::pin(services.grant().terminate(
                        peer.clone(),
                        operation(),
                        peer_grant.id(),
                    ))
                    .await
                    .expect("terminate peer grant");
                    let ended_hist = services
                        .number()
                        .history(
                            &manager,
                            peer_current.person_id(),
                            crate::pagination::Pagination::new(1, 20).expect("page"),
                        )
                        .await
                        .expect("ended target history");
                    assert_eq!(ended_hist.count(), 1);
                    match ended_hist.revisions()[0].kind() {
                        super::HistoryKind::Allocation { number, .. } => {
                            assert_eq!(number.id(), peer_alloc.id())
                        }
                        _ => panic!("expected allocation"),
                    }

                    // Target whose account was cancelled remains readable
                    let target_val = json!({
                        "personId": peer_current.person_id(),
                        "revisionId": peer_current.revision_id(),
                        "mobile": mobile(10),
                    });
                    Box::pin(services.cancellation().cancel_account(
                        institution.clone(),
                        operation(),
                        peer.account_id(),
                        serde_json::from_value(target_val).expect("cancel"),
                    ))
                    .await
                    .expect("cancel peer account");
                    let cancelled_hist = services
                        .number()
                        .history(
                            &manager,
                            peer_current.person_id(),
                            crate::pagination::Pagination::new(1, 20).expect("page"),
                        )
                        .await
                        .expect("cancelled target history");
                    assert_eq!(cancelled_hist.count(), 1);

                    // Reader suspended cannot read
                    let mgr_current = services
                        .current()
                        .read(&manager)
                        .await
                        .expect("mgr current");
                    let mgr_target = json!({
                        "personId": mgr_current.person_id(),
                        "revisionId": mgr_current.revision_id(),
                        "mobile": "13800000000",
                    });
                    Box::pin(services.suspension().suspend(
                        institution.clone(),
                        operation(),
                        manager.account_id(),
                        serde_json::from_value(mgr_target).expect("pause"),
                    ))
                    .await
                    .expect("suspend manager");
                    assert!(matches!(
                        services
                            .number()
                            .history(
                                &manager,
                                peer_current.person_id(),
                                crate::pagination::Pagination::new(1, 20).expect("page")
                            )
                            .await,
                        Err(Error::Unauthenticated)
                    ));

                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    supplier.stop().await;
                }),
            )
            .await
            .expect("bounded authority and ended targets");
        }));
    }

    #[test]
    fn history_rejects_damaged_facts_and_closes_admission() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["missing_person", "missing_version", "broken_creation", "foreign_operator", "future_time", "changed_sequence"] {
                timeout(Duration::from_secs(45), Box::pin(async {
                    let database = Box::pin(Database::initialized(Arc::clone(&server))).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                    let (institution, manager, peer) = Box::pin(members(&database, &services, &mut supplier)).await;
                    Box::pin(promote(&services, &institution)).await;

                    Box::pin(services.number().allocate(peer.clone(), operation(), input(801))).await.expect("peer allocation");
                    let peer_current = services.current().read(&peer).await.expect("peer current");
                    let owner = database.connections("postgres").recover().await.expect("owner");

                    let corruption = match case {
                        "missing_person" => "DELETE FROM identity.persons WHERE id IN (SELECT person_id FROM workforce.allocations)",
                        "missing_version" => "DELETE FROM workforce.revisions",
                        "broken_creation" => "UPDATE workforce.revisions SET number.year = 1999",
                        "foreign_operator" => "UPDATE workforce.allocations SET operator_id = (SELECT id FROM identity.revisions WHERE person_id <> workforce.allocations.person_id ORDER BY id LIMIT 1)",
                        "future_time" => "UPDATE workforce.allocations SET allocated_at = now() + interval '1 day'",
                        "changed_sequence" => "UPDATE workforce.allocations SET sequence = sequence + 1",
                        _ => unreachable!(),
                    };
                    owner.client().batch_execute(&format!("BEGIN; SET LOCAL session_replication_role=replica; {corruption}; COMMIT")).await.expect("corrupt");

                    assert!(services.number().app.registry().ready().is_ok());
                    let result = services.number().history(&manager, peer_current.person_id(), crate::pagination::Pagination::new(1, 20).expect("page")).await;
                    assert!(result.is_err(), "{case} should error");
                    assert!(services.number().app.registry().ready().is_err(), "{case} must close admission");

                    owner.terminate().await.expect("owner ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
                })).await.expect("bounded damage rejection");
            }
        }));
    }

    #[test]
    fn history_rejects_lost_person_with_retained_facts() {
        assert_history_damage(
            "DELETE FROM identity.persons WHERE id IN (SELECT person_id FROM workforce.allocations)",
        );
    }

    #[test]
    fn history_rejects_deleted_newest_correction_with_retained_revision_and_occupancy() {
        assert_history_damage(
            "DELETE FROM workforce.corrections WHERE sequence = (SELECT max(sequence) FROM workforce.corrections)",
        );
    }

    #[test]
    fn history_does_not_reinterpret_the_targets_past_grant() {
        for corruption in [
            "UPDATE access.grants SET granted_at=granted_at-interval '1 day' WHERE person_id IN (SELECT person_id FROM workforce.corrections)",
            "UPDATE access.grants SET person_id=gen_random_uuid() WHERE person_id IN (SELECT person_id FROM workforce.corrections)",
            "UPDATE access.grants SET seal=set_byte(seal,0,get_byte(seal,0)#1) WHERE person_id IN (SELECT person_id FROM workforce.corrections)",
            "DELETE FROM access.grants WHERE person_id IN (SELECT person_id FROM workforce.corrections)",
        ] {
            history_damage(corruption, false, true);
        }
    }

    #[test]
    fn historical_numbers_do_not_consume_the_targets_current_grant() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for page_number in [1, 4] {
                timeout(Duration::from_secs(45), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let (_, manager, peer) = Box::pin(members(&database, &services, &mut supplier)).await;
                    Box::pin(services.number().allocate(peer.clone(), operation(), input(801))).await.expect("original allocation");
                    let candidate = services.grant().suspension_candidate(&manager, mobile(10).parse().expect("mobile")).await.expect("managed Grant");
                    let identity = candidate.identity();
                    let body = json!({"target":{"accountId":identity.id(),"personId":identity.person_id(),"revisionId":identity.revision_id(),"mobile":mobile(10)}});
                    let owner = database.connections("postgres").recover().await.expect("isolated owner");
                    let original = owner.client().query_one("SELECT sequence,version,changed_at,seal FROM access.grants WHERE id=$1", &[&candidate.grant_id()]).await.expect("original sealed state");
                    let pause = Box::pin(services.grant().suspend(manager.clone(), operation(), candidate.grant_id(), serde_json::from_value(body.clone()).expect("suspension confirmation"))).await.expect("actual Grant suspension");
                    Box::pin(services.grant().resume(manager.clone(), operation(), pause.id(), serde_json::from_value(body).expect("resumption confirmation"))).await.expect("actual Grant resumption");
                    let page = crate::pagination::Pagination::new(page_number, 1).expect("page");
                    assert_eq!(services.number().history(&manager, identity.person_id(), page).await.expect("latest Grant state supports retained allocation").count(), 1);
                    let sequence: i64 = original.get("sequence");
                    let version: i64 = original.get("version");
                    let changed_at: chrono::DateTime<chrono::Utc> = original.get("changed_at");
                    let seal: Vec<u8> = original.get("seal");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated state rollback");
                    assert_eq!(owner.client().execute("UPDATE access.grants SET sequence=$2,version=$3,changed_at=$4,seal=$5,suspension_id=NULL WHERE id=$1", &[&candidate.grant_id(), &sequence, &version, &changed_at, &seal]).await.expect("restore the exact prior sealed state"), 1);
                    owner.client().batch_execute("COMMIT").await.expect("rollback damage committed");
                    assert_eq!(services.number().history(&manager, identity.person_id(), page).await.expect("history does not consume target Grant").count(), 1);
                    assert!(services.ready().await.is_ok());
                    assert!(services.grant().verify(owner.client()).await.is_err(), "startup rejects a state behind its retained records");
                    owner.terminate().await.expect("owner ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded Grant rollback scenario");
            }
        }));
    }

    #[test]
    fn history_rejects_damaged_operator_identity_in_the_requested_page() {
        // The requested complete window consumes the original allocation identity.
        assert_history_damage(
            "UPDATE identity.revisions SET name = set_byte(name, 20, get_byte(name, 20) # 1) WHERE id = (SELECT operator_id FROM workforce.allocations WHERE person_id IN (SELECT person_id FROM workforce.corrections))",
        );
    }

    #[test]
    fn history_rejects_self_service_operator_moved_to_a_later_own_revision() {
        // The later real-name revision authenticates, but was not current when the person allocated.
        history_damage(
            "UPDATE workforce.allocations SET operator_id = person.revision_id FROM identity.persons person WHERE person.id = workforce.allocations.person_id AND workforce.allocations.person_id IN (SELECT person_id FROM workforce.corrections)",
            true,
            false,
        );
    }

    #[test]
    fn history_rejects_correction_target_moved_to_a_later_own_revision() {
        history_damage(
            "UPDATE workforce.corrections SET revision_id = person.revision_id FROM identity.persons person WHERE person.id = workforce.corrections.person_id AND workforce.corrections.sequence = (SELECT min(sequence) FROM workforce.corrections)",
            true,
            false,
        );
    }

    #[test]
    fn history_rejects_changed_correction_sequence() {
        assert_history_damage(
            "UPDATE workforce.corrections SET sequence = sequence + 1 WHERE sequence = (SELECT max(sequence) FROM workforce.corrections)",
        );
    }

    #[test]
    fn history_rejects_revision_and_state_damage() {
        for corruption in [
            "DELETE FROM workforce.corrections WHERE sequence = (SELECT max(sequence) FROM workforce.corrections)",
            "DELETE FROM workforce.numbers",
            "UPDATE workforce.numbers SET revision_id = revision.id, number = revision.number FROM workforce.allocations allocation JOIN workforce.revisions revision ON revision.id = allocation.number_id WHERE workforce.numbers.id = allocation.person_id",
            "UPDATE workforce.numbers SET number.serial = 44",
        ] {
            assert_history_damage(corruption);
        }
    }

    #[test]
    fn history_rejects_allocation_with_missing_seal_key() {
        assert_history_damage("UPDATE workforce.allocations SET seal_key_id=9223372036854775807");
    }

    #[test]
    fn history_rejects_changed_assistance_reason_with_original_seal() {
        assistance_history_damage(
            "UPDATE workforce.allocations SET reason = 'Altered personnel record' WHERE source = 'assistance'",
        );
    }

    #[test]
    fn history_rejects_changed_assistance_operator_with_original_seal() {
        // Another real revision of a non-target person stays structurally valid; only the
        // record seal detects the change.
        assistance_history_damage(
            "UPDATE workforce.allocations SET operator_id = (SELECT revision.id FROM identity.revisions revision WHERE revision.person_id <> workforce.allocations.person_id AND revision.id <> workforce.allocations.operator_id ORDER BY revision.id LIMIT 1) WHERE source = 'assistance'",
        );
    }

    #[test]
    fn history_rejects_changed_assistance_personal_revision() {
        assistance_history_damage(
            "UPDATE workforce.allocations SET revision_id=gen_random_uuid() WHERE source='assistance'",
        );
    }

    /// Damages a retained assisted allocation after startup; the person-scoped history read must
    /// reject it and close admission in the requested window.
    fn assistance_history_damage(corruption: &str) {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for page_number in [1] {
                timeout(Duration::from_secs(60), Box::pin(async {
                    let database = Box::pin(Database::initialized(Arc::clone(&server))).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                    let (institution, manager, peer) = Box::pin(members(&database, &services, &mut supplier)).await;
                    Box::pin(promote(&services, &institution)).await;
                    let body = confirmation(&services, &peer, &mobile(10)).await;
                    let id = target(&body);
                    Box::pin(services.number().allocate_member(manager.clone(), operation(), id, assistance(&body))).await.expect("assisted allocation");
                    let page = crate::pagination::Pagination::new(page_number, 1).expect("page");
                    let history = services.number().history(&manager, id, page).await.expect("intact history");
                    assert_eq!(history.count(), 1);
                    assert_eq!(history.revisions().len(), usize::from(page_number == 1));

                    let owner = database.connections("postgres").recover().await.expect("isolated owner");
                    owner.client().batch_execute(&format!("BEGIN; SET LOCAL session_replication_role=replica; {corruption}; COMMIT")).await.expect("damage retained facts after startup");
                    assert!(services.number().app.registry().ready().is_ok());
                    let result = services.number().history(&manager, id, page).await;
                    let damaged = matches!(result, Err(Error::Corrupt(_)));
                    let admitted = services.number().app.registry().ready().is_ok();
                    let observed = match result {
                        Ok(history) => format!("accepted {} facts", history.count()),
                        Err(error) => format!("{error:?}"),
                    };

                    owner.terminate().await.expect("owner ended");
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    supplier.stop().await;
                    assert!(damaged && !admitted, "page {page_number}: expected fact damage and closed admission; got {observed}, admission open: {admitted}");
                })).await.expect("bounded assisted allocation damage check");
            }
        }));
    }

    #[test]
    fn history_rejects_changed_correction_reason_with_original_seal() {
        assert_history_damage(
            "UPDATE workforce.corrections SET reason = 'Altered personnel record' WHERE sequence = (SELECT min(sequence) FROM workforce.corrections)",
        );
    }

    #[test]
    fn history_rejects_changed_correction_operation_with_original_seal() {
        assert_history_damage(
            "UPDATE workforce.corrections SET operation_id = gen_random_uuid() WHERE sequence = (SELECT min(sequence) FROM workforce.corrections)",
        );
    }

    #[test]
    fn history_rejects_correction_with_missing_seal_key() {
        assert_history_damage(
            "UPDATE workforce.corrections SET seal_key_id=9223372036854775807 WHERE sequence=(SELECT min(sequence) FROM workforce.corrections)",
        );
    }

    fn assert_history_damage(corruption: &str) {
        history_damage(corruption, false, false);
    }

    /// With `renamed`, the target receives a later real-name revision before the damage.
    fn history_damage(corruption: &str, renamed: bool, check_grants: bool) {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for page_number in [1] {
                timeout(Duration::from_secs(60), Box::pin(async {
                    let database = Box::pin(Database::initialized(Arc::clone(&server))).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                    let (institution, manager, peer) = Box::pin(members(&database, &services, &mut supplier)).await;
                    Box::pin(promote(&services, &institution)).await;
                    let confirmed = confirmation(&services, &peer, &mobile(10)).await;
                    let id = target(&confirmed);
                    let peer_revision = services.current().read(&peer).await.expect("peer current").revision_id();
                    let allocation = Box::pin(services.number().allocate(peer, operation(), input(801))).await.expect("allocation");
                    let body = correction_body(&confirmed, &allocation, 902, previous_year(), 42);
                    let corrected = Box::pin(services.number().correct(manager.clone(), operation(), id, correction(&body))).await.expect("correction");
                    let body = correction_body(&confirmed, &corrected, 903, previous_year(), 43);
                    Box::pin(services.number().correct(manager.clone(), operation(), id, correction(&body))).await.expect("newest correction");
                    if renamed {
                        Box::pin(services.correction().correct(institution.clone(), operation(), serde_json::from_value(serde_json::json!({"target": {"personId": id, "revisionId": peer_revision}, "name": "Revised Peer", "idNumber": "000000200002290029"})).expect("real-name correction"))).await.expect("later real-name revision");
                    }
                    let page = crate::pagination::Pagination::new(page_number, 20).expect("complete requested window");
                    let history = services.number().history(&manager, id, page).await.expect("intact history");
                    assert_eq!(history.count(), 3);
                    assert_eq!(history.revisions().len(), 3);

                    let owner = database.connections("postgres").recover().await.expect("isolated owner");
                    owner.client().batch_execute(&format!("BEGIN; SET LOCAL session_replication_role=replica; {corruption}; COMMIT")).await.expect("damage retained facts after startup");
                    assert!(services.number().app.registry().ready().is_ok());
                    let result = services.number().history(&manager, id, page).await;
                    let damaged = result.as_ref().err().is_some_and(crate::runtime::work::Failure::unreliable);
                    let admitted = services.number().app.registry().ready().is_ok();
                    let observed = match result {
                        Ok(history) => format!("accepted {} facts", history.count()),
                        Err(error) => format!("{error:?}"),
                    };

                    if check_grants {
                        assert!(services.grant().verify(owner.client()).await.is_err(), "startup rejects the same consumed Grant damage");
                    }
                    owner.terminate().await.expect("owner ended");
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    supplier.stop().await;
                    assert_eq!((damaged, admitted), (!check_grants, check_grants), "page {page_number}: {observed}, admission open: {admitted}");
                })).await.expect("bounded retained history damage check");
            }
        }));
    }

    #[test]
    fn history_authenticates_only_its_consumed_window() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for (case, corruption) in [
                ("allocation operator", "UPDATE workforce.allocations SET operator_id = (SELECT id FROM identity.revisions WHERE person_id <> workforce.allocations.person_id ORDER BY id LIMIT 1)"),
                ("allocation version", "DELETE FROM workforce.revisions WHERE id IN (SELECT number_id FROM workforce.allocations)"),
                ("allocation source year", "UPDATE workforce.revisions SET number.year = 1999 WHERE id IN (SELECT number_id FROM workforce.allocations)"),
                ("allocation future time", "UPDATE workforce.allocations SET allocated_at = now() + interval '1 day'"),
                ("correction operator", "UPDATE workforce.corrections SET operator_id = gen_random_uuid() WHERE sequence = (SELECT min(sequence) FROM workforce.corrections)"),
                ("correction target version", "UPDATE workforce.corrections SET revision_id = (SELECT id FROM identity.revisions WHERE person_id <> workforce.corrections.person_id ORDER BY id LIMIT 1) WHERE sequence = (SELECT min(sequence) FROM workforce.corrections)"),
            ] {
            for page_number in [1, 4] {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Box::pin(Database::initialized(Arc::clone(&server))).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                let (institution, manager, peer) = Box::pin(members(&database, &services, &mut supplier)).await;
                Box::pin(promote(&services, &institution)).await;
                let confirmed = confirmation(&services, &peer, &mobile(10)).await;
                let id = target(&confirmed);
                let allocation = Box::pin(services.number().allocate(peer, operation(), input(801))).await.expect("allocation");
                let body = correction_body(&confirmed, &allocation, 902, previous_year(), 42);
                let corrected = Box::pin(services.number().correct(manager.clone(), operation(), id, correction(&body))).await.expect("correction");
                let body = correction_body(&confirmed, &corrected, 903, previous_year(), 43);
                let latest = Box::pin(services.number().correct(manager.clone(), operation(), id, correction(&body))).await.expect("newest correction");
                let page = crate::pagination::Pagination::new(page_number, 1).expect("newest fact or empty page");
                let history = services.number().history(&manager, id, page).await.expect("intact history");
                assert_eq!(history.count(), 3);
                if page_number == 1 {
                    assert_eq!(history.revisions().len(), 1);
                    let item = &history.revisions()[0];
                    match item.kind() {
                        super::HistoryKind::Correction { after, .. } => assert_eq!(after.id(), latest.id()),
                        _ => panic!("only the newest correction is on the requested page"),
                    }
                    let observer = database.connections(&database.user).recover().await.expect("fact observer");
                    let after: super::Id = observer.client().query_one("SELECT after_id FROM workforce.corrections WHERE id = $1", &[&item.id()]).await.expect("history fact id binds the retained correction").get(0);
                    assert_eq!(after, latest.id());
                    observer.terminate().await.expect("observer ended");
                } else {
                    assert!(history.revisions().is_empty());
                }

                let owner = database.connections("postgres").recover().await.expect("isolated owner");
                owner.client().batch_execute(&format!("BEGIN; SET LOCAL session_replication_role=replica; {corruption}; COMMIT")).await.expect("damage only older facts");
                assert!(services.number().app.registry().ready().is_ok());
                let result = services.number().history(&manager, id, page).await;
                assert!(result.is_ok(), "{case}, page {page_number}: an unrelated history window stays readable");
                assert!(services.number().app.registry().ready().is_ok());
                let result = services.number().history(&manager, id, crate::pagination::Pagination::new(1, 20).expect("complete window")).await;
                assert!(matches!(result, Err(Error::Corrupt(_))), "{case}: consuming the damaged record fails");
                assert!(services.number().app.registry().ready().is_err(), "{case}, page {page_number}: off-page damage must close admission");

                owner.terminate().await.expect("owner ended");
                services.close();
                services.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                supplier.stop().await;
            })).await.expect("bounded off-page damage check");
            }
            }
        }));
    }

    #[test]
    fn history_keeps_authority_count_and_page_in_one_snapshot() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(
                Duration::from_secs(60),
                Box::pin(async {
                    let database = Box::pin(Database::initialized(Arc::clone(&server))).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = crate::access::role::tests::start_snapshot(
                        crate::identity::authentication::proxied(&database, &proxy),
                        &supplier,
                    )
                    .await;
                    let (institution, manager, peer) =
                        Box::pin(members(&database, &services, &mut supplier)).await;
                    Box::pin(promote(&services, &institution)).await;

                    let confirmed = confirmation(&services, &peer, &mobile(10)).await;
                    let id = target(&confirmed);
                    let first = Box::pin(services.number().allocate(
                        peer.clone(),
                        operation(),
                        input(801),
                    ))
                    .await
                    .expect("initial");
                    let body = correction_body(&confirmed, &first, 902, previous_year(), 42);

                    proxy.arm(Fault::StalledNumberHistoryCount);
                    let service = services.number().clone();
                    let reader_actor = manager.clone();
                    let mut reader = tokio::spawn(async move {
                        service
                            .history(
                                &reader_actor,
                                id,
                                crate::pagination::Pagination::new(1, 20).expect("page"),
                            )
                            .await
                    });
                    timeout(Duration::from_secs(3), proxy.intercepted())
                        .await
                        .expect("count query intercepted");

                    // Concurrent correction committed during stalled read
                    let corr = Box::pin(services.number().correct(
                        manager.clone(),
                        operation(),
                        id,
                        correction(&body),
                    ))
                    .await
                    .expect("concurrent correction");

                    proxy.release();
                    let page = timeout(Duration::from_secs(5), &mut reader)
                        .await
                        .expect("reader completes")
                        .expect("task ok")
                        .expect("snapshot read ok");

                    // Anchor advancement retries the entire snapshot, including count and window.
                    assert_eq!(page.count(), 2);
                    assert_eq!(page.revisions().len(), 2);
                    assert!(matches!(page.revisions()[0].kind(), super::HistoryKind::Correction { after, .. } if after.id() == corr.id()));
                    assert!(matches!(page.revisions()[1].kind(), super::HistoryKind::Allocation { number, .. } if number.id() == first.id()));

                    // Fresh read sees the new count and facts
                    let fresh = services
                        .number()
                        .history(
                            &manager,
                            id,
                            crate::pagination::Pagination::new(1, 20).expect("page"),
                        )
                        .await
                        .expect("fresh read");
                    assert_eq!(fresh.count(), 2);
                    assert_eq!(fresh.revisions().len(), 2);
                    match fresh.revisions()[0].kind() {
                        super::HistoryKind::Correction { after, before, .. } => {
                            assert_eq!(after.id(), corr.id());
                            assert_eq!(before.id(), first.id());
                        }
                        _ => panic!("expected correction"),
                    }
                    match fresh.revisions()[1].kind() {
                        super::HistoryKind::Allocation { number, .. } => {
                            assert_eq!(number.id(), first.id())
                        }
                        _ => panic!("expected allocation"),
                    }

                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    supplier.stop().await;
                    proxy.stop().await;
                }),
            )
            .await
            .expect("bounded snapshot history test");
        }));
    }
}
