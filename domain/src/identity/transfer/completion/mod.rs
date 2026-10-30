//! Mobile completion changes the account and its fixed session set in one kernel command.

pub(super) mod database;

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use platform::cryptography;
use serde::{Deserialize, Serialize};
use tokio_postgres::Client;
use utoipa::ToSchema;

use crate::{
    access::{self, appointment, role},
    history::{self, Read, Record as _, Verified},
    identity::{
        account::{self, record::Event},
        current::{self, Actor},
        message,
        mobile::{
            Mobile,
            envelope::{Binding, Envelope},
        },
        operation, person, session,
        transfer::{Id, Service, Target},
    },
    kernel::{
        self, App, Authorization, Command, completion,
        operation::Family,
        tx::{self, Tx},
    },
};

pub type Error = kernel::Error<Rejection>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Absent,
    CodeInvalid,
    Verification,
    Conflict,
    Taken,
}

impl kernel::Rejection for Rejection {
    fn status(&self) -> kernel::Status {
        match self {
            Self::Absent => kernel::Status::NotFound,
            Self::CodeInvalid => kernel::Status::Forbidden,
            Self::Verification => kernel::Status::Gone,
            Self::Conflict | Self::Taken => kernel::Status::Conflict,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Absent => "target_unavailable",
            Self::CodeInvalid => "verification_invalid",
            Self::Verification => "verification_required",
            Self::Conflict => "state_conflict",
            Self::Taken => "mobile_taken",
        }
    }
}

impl From<Rejection> for Error {
    fn from(value: Rejection) -> Self {
        Self::Rejected(value)
    }
}

impl From<super::Error> for Error {
    fn from(error: super::Error) -> Self {
        use crate::runtime::work::Failure as _;
        match error {
            super::Error::Absent => Rejection::Absent.into(),
            super::Error::CodeInvalid => Rejection::CodeInvalid.into(),
            super::Error::Verification => Rejection::Verification.into(),
            super::Error::Conflict => Rejection::Conflict.into(),
            super::Error::Taken => Rejection::Taken.into(),
            super::Error::Mismatch => Self::Mismatch,
            super::Error::Current(error) => error.into(),
            super::Error::Role(error) => error.into(),
            super::Error::Appointment(error) => error.into(),
            super::Error::History(error) => error.into(),
            super::Error::Account(error) => error.into(),
            super::Error::Completion(error) => *error,
            error => {
                let corrupt = error.unreliable();
                let fault =
                    kernel::Fault::new(kernel::Stage::Command, kernel::FaultKind::Transfer(error));
                if corrupt {
                    Self::Corrupt(fault)
                } else {
                    Self::Unavailable(fault)
                }
            }
        }
    }
}

pub(super) const FAMILY: &[u8] = b"transfer/completion";

fn accepts(action: &str, target: &str) -> bool {
    matches!(action, "self" | "assisted")
        && target
            .parse::<account::Id>()
            .is_ok_and(|id| id.to_string() == target)
}

pub(crate) const COMPLETIONS: completion::Template =
    completion::Template::personal(Family::MobileCompletion, accepts);

pub struct Input {
    subject: Subject,
    operation_id: operation::Id,
    mobile: Mobile,
    message_id: message::Id,
    code: message::Code,
}

impl Input {
    pub fn new(
        operation_id: operation::Id,
        mobile: Mobile,
        message_id: message::Id,
        code: message::Code,
    ) -> Self {
        Self {
            subject: Subject::Current,
            operation_id,
            mobile,
            message_id,
            code,
        }
    }
}

enum Subject {
    Current,
    Assisted(account::Id),
}

impl Subject {
    fn account_id(&self, actor: &Actor) -> account::Id {
        match self {
            Self::Current => actor.account_id(),
            Self::Assisted(id) => *id,
        }
    }
    fn action(&self) -> &'static str {
        match self {
            Self::Current => "self",
            Self::Assisted(_) => "assisted",
        }
    }
    fn as_bytes(&self) -> &[u8] {
        self.action().as_bytes()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = transfer::Completion)]
pub struct Completion {
    id: Id,
    #[schema(value_type = String, format = DateTime)]
    completed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
#[schema(as = transfer::State)]
pub enum State {
    #[serde(rename_all = "camelCase")]
    Pending {
        #[schema(value_type = String, format = DateTime)]
        expires_at: DateTime<Utc>,
    },
    #[serde(rename_all = "camelCase")]
    Completed {
        #[schema(value_type = String, format = DateTime)]
        completed_at: DateTime<Utc>,
    },
    Expired,
}

// Rejections remain short-lived verification facts; their proof also binds the code counter.
struct Proof<'a> {
    input: &'a Input,
    transfer_id: Id,
    person_id: person::Id,
    operator_id: person::Id,
    session_id: session::Id,
}
impl cryptography::Input<10> for Proof<'_> {
    fn fields(&self) -> [&[u8]; 10] {
        [
            b"nucleus/identity/mobile-completion/v1",
            self.input.subject.as_bytes(),
            self.input.operation_id.as_bytes(),
            self.transfer_id.as_bytes(),
            self.operator_id.as_bytes(),
            self.person_id.as_bytes(),
            self.session_id.as_bytes(),
            self.input.mobile.as_bytes(),
            self.input.message_id.as_bytes(),
            self.input.code.as_bytes(),
        ]
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Canonical {
    session_id: session::Id,
    mobile: Mobile,
    message_id: message::Id,
    code: message::Code,
}

#[derive(Serialize)]
struct Value<'a> {
    session_id: session::Id,
    mobile: &'a str,
    message_id: message::Id,
    code: &'a str,
}

fn request(
    action: &'static str,
    target: account::Id,
    input: &Canonical,
    authority: &Authorization,
) -> Result<completion::Request, Error> {
    completion::Request::new(
        action,
        target.to_string(),
        &Value {
            session_id: input.session_id,
            mobile: input.mobile.as_ref(),
            message_id: input.message_id,
            code: input.code.as_ref(),
        },
        authority,
    )
}

enum Conclusion {
    Completed(Completion),
    Rejected,
}

impl Conclusion {
    fn deliver(self) -> Result<Completion, Error> {
        match self {
            Self::Completed(value) => Ok(value),
            Self::Rejected => Err(Rejection::CodeInvalid.into()),
        }
    }
}

impl Service {
    pub async fn complete_assisted(
        &self,
        actor: Actor,
        account_id: account::Id,
        mut input: Input,
    ) -> Result<Completion, Error> {
        input.subject = Subject::Assisted(account_id);
        self.complete(actor, input).await
    }

    pub async fn complete(&self, actor: Actor, input: Input) -> Result<Completion, Error> {
        self.0
            .app
            .command(
                input.operation_id,
                Write {
                    service: self.clone(),
                    actor,
                    input,
                },
            )
            .await?
            .deliver()
    }
}

struct Write {
    service: Service,
    actor: Actor,
    input: Input,
}

struct Facts {
    account: Verified<account::state::State>,
    target: Target,
    confirmation: Option<database::Confirmation>,
    valid: bool,
    taken: bool,
    mobile: Envelope,
    sessions: Vec<Verified<session::state::State>>,
}

enum Selected {
    Completed {
        change: Box<account::transition::Revocation>,
        value: Completion,
        deadline: DateTime<Utc>,
    },
    Rejected {
        record: database::Rejected,
        expires_at: DateTime<Utc>,
        deadline: DateTime<Utc>,
    },
}

impl Command for Write {
    const COMPLETIONS: completion::Template = COMPLETIONS;
    type Facts = Facts;
    type Change = Selected;
    type Output = Conclusion;
    type Rejection = Rejection;

    fn session(&self) -> Option<(&current::Service, &Actor)> {
        Some((&self.service.0.current, &self.actor))
    }

    fn request(&self, authority: &Authorization) -> Result<completion::Request, Error> {
        completion::Request::new(
            self.input.subject.action(),
            self.input.subject.account_id(&self.actor).to_string(),
            &Value {
                session_id: self.actor.session_id(),
                mobile: self.input.mobile.as_ref(),
                message_id: self.input.message_id,
                code: self.input.code.as_ref(),
            },
            authority,
        )
    }

    async fn locks(&self, tx: &Tx) -> Result<(), Error> {
        let service = &self.service.0;
        if matches!(self.input.subject, Subject::Assisted(_)) {
            tx.shared(b"access/scope", b"institution").await?;
        }
        let target = self.input.subject.account_id(&self.actor);
        let hash = service
            .keys
            .mobile
            .hash(&self.input.mobile)
            .map_err(super::Error::from)?;
        let occupied = account::state::discover_mobile(tx.client(), &hash).await?;
        account::state::lock_replacement(tx.client(), self.actor.account_id(), target, occupied)
            .await?;
        let old = account::state::discover_envelope(tx.client(), target)
            .await?
            .map(|value| {
                value.open(
                    &service.keys.encryption,
                    &service.keys.mobile,
                    Binding::Account(target),
                )
            })
            .transpose()
            .map_err(super::Error::from)?;
        let mut mobiles = vec![&self.input.mobile];
        mobiles.extend(old.as_ref());
        mobiles.sort_by_key(|value| value.as_bytes());
        mobiles.dedup_by_key(|value| value.as_bytes());
        for mobile in mobiles {
            tx::object(tx.client(), b"identity/mobile", mobile.as_bytes()).await?;
        }
        if account::state::discover_mobile(tx.client(), &hash).await? != occupied {
            return Err(Error::lock_set_changed());
        }
        session::state::lock_management(tx.client(), self.actor.session_id(), target).await?;
        database::lock_confirmation(tx.client(), self.input.message_id).await?;
        Ok(())
    }

    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, Error> {
        let subject = subject.ok_or(Error::corrupt("mobile completion declares its session"))?;
        if let Subject::Assisted(account) = self.input.subject {
            let (role, person_id, revision_id, now) = access::authorize_subject(
                tx.client(),
                &self.service.0.roles,
                subject,
                role::Code::InstitutionAccounts,
            )
            .await?;
            let target =
                appointment::find_by_account(tx.client(), &self.service.0.roles, account, now)
                    .await?
                    .ok_or(Rejection::Absent)?;
            if target.person_id() == person_id || target.role().level() >= role.level() {
                return Err(Rejection::Absent.into());
            }
            Ok(Authorization {
                person_id,
                revision_id,
                now,
            })
        } else {
            Ok(Authorization {
                person_id: subject.person_id(),
                revision_id: subject.revision_id(),
                now: subject.observed_at(),
            })
        }
    }

    async fn load(&self, tx: &Tx, authority: &Authorization) -> Result<Facts, Error> {
        let service = &self.service.0;
        let target = self.input.subject.account_id(&self.actor);
        let account = service
            .accounts
            .state(tx.client(), target, authority.now)
            .await?
            .filter(|state| state.current().is_some())
            .ok_or(Rejection::Absent)?;
        let subject = service
            .accounts
            .find_current(tx.client(), target, authority.now)
            .await?
            .ok_or(Error::corrupt(
                "a locked current mobile target has its identity",
            ))?;
        let current = account
            .current()
            .ok_or(Error::corrupt("the mobile target is current"))?;
        let mobile = current
            .mobile()
            .open(
                &service.keys.encryption,
                &service.keys.mobile,
                Binding::Account(target),
            )
            .map_err(super::Error::from)?;
        let target = Target::qualified(
            target,
            subject.person_id(),
            subject.revision_id(),
            authority,
            mobile,
        );
        let confirmation = database::find_confirmation(
            tx.client(),
            service.app.keys(),
            self.input.message_id,
            authority.now,
        )
        .await?;
        let valid = confirmation
            .as_ref()
            .map(|value| value.verify(&service.keys, &self.input.code))
            .transpose()?
            .unwrap_or(false);
        // Incorrect codes never disclose or decide the new phone's occupancy.
        let taken = if valid {
            service
                .accounts
                .with_read(Read::Shared)
                .find_by_mobile(tx.client(), &self.input.mobile, authority.now)
                .await?
                .is_some()
        } else {
            false
        };
        let mobile = Envelope::seal(
            &service.keys.encryption,
            &service.keys.mobile,
            Binding::Account(account.id()),
            &self.input.mobile,
        )
        .map_err(super::Error::from)?;
        let sessions = session::state::for_account(
            tx.client(),
            service.app.keys(),
            service.app.anchors(),
            Read::Locked,
            account.id(),
            authority.now,
        )
        .await?;
        Ok(Facts {
            account,
            target,
            confirmation,
            valid,
            taken,
            mobile,
            sessions,
        })
    }

    fn decide(&self, facts: Facts, now: DateTime<Utc>) -> Result<Selected, Error> {
        if facts.target.mobile().as_bytes() == self.input.mobile.as_bytes() {
            return Err(Rejection::Conflict.into());
        }
        let confirmation = facts.confirmation.ok_or(Rejection::Verification)?;
        confirmation.qualify(
            &self.service.0.keys,
            &self.actor,
            &facts.target,
            &self.input,
            now,
        )?;
        let deadline = confirmation.deadline()?;
        if !facts.valid {
            return Ok(Selected::Rejected {
                record: database::Rejected::new(
                    &self.actor,
                    &self.input,
                    &facts.target,
                    &confirmation,
                    &self.service.0.keys,
                    now,
                )?,
                expires_at: confirmation.retained_until()?,
                deadline,
            });
        }
        if facts.taken {
            return Err(Rejection::Taken.into());
        }
        let event = Event::Replacement {
            id: confirmation.transfer_id(),
            operator_id: confirmation.operator_id(),
            session_id: self.actor.session_id(),
            message_id: self.input.message_id,
            sessions: Vec::new(),
        };
        let change = account::transition::replace(
            &facts.account,
            facts.mobile,
            self.service.0.app.keys().data,
            event,
            &facts.sessions,
            self.input.operation_id,
            now,
        )?;
        Ok(Selected::Completed {
            change: Box::new(change),
            value: Completion {
                id: confirmation.transfer_id(),
                completed_at: now,
            },
            deadline,
        })
    }

    fn expires_at(&self, selected: &Selected, _: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match selected {
            Selected::Completed { .. } => None,
            Selected::Rejected { expires_at, .. } => Some(*expires_at),
        }
    }

    async fn apply(
        &self,
        tx: &Tx,
        selected: Selected,
        pending: &mut Option<history::Pending>,
    ) -> Result<Conclusion, Error> {
        let (deadline, result) = match selected {
            Selected::Rejected {
                record, deadline, ..
            } => {
                record.insert(tx.client()).await?;
                (deadline, Conclusion::Rejected)
            }
            Selected::Completed {
                change,
                value,
                deadline,
            } => {
                let app = &self.service.0.app;
                *pending = Some(
                    change
                        .account
                        .write(tx.client(), app.keys(), app.anchors())
                        .await?,
                );
                let group = pending
                    .as_mut()
                    .ok_or(Error::corrupt("replacement retains its account change"))?;
                for (id, change) in change.sessions {
                    group.extend(change.write(tx.client(), app.keys(), app.anchors()).await?)?;
                    session::handoff::remove(tx.client(), id).await?;
                }
                (deadline, Conclusion::Completed(value))
            }
        };
        // A blocked write must not retain success or a counted guess after either deadline.
        // The executor rolls back these effects and settles their pending anchors together.
        if tx.now(&self.service.0.context).await? >= deadline {
            return Err(Rejection::Verification.into());
        }
        Ok(result)
    }

    async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<Conclusion, Error> {
        let service = &self.service.0;
        let original = completion::original(
            tx.client(),
            service.app.keys(),
            COMPLETIONS,
            self.input.operation_id,
        )
        .await?
        .ok_or(Error::corrupt("a mobile replay retains its completion"))?;
        if original.expires_at().is_some() {
            let rejected =
                database::find_rejection(tx.client(), service.app.keys(), self.input.operation_id)
                    .await?
                    .ok_or(Error::corrupt(
                        "a code rejection retains its counter evidence",
                    ))?;
            rejected.validate(&service.keys, now)?;
            rejected.matches(&service.keys, &self.actor, &self.input)?;
            rejected.paired(&original)?;
            return Ok(Conclusion::Rejected);
        }
        let record = account::record::replacement_by_operation(
            tx.client(),
            service.app.keys(),
            self.input.operation_id,
            now,
        )
        .await?
        .ok_or(Error::corrupt(
            "a mobile success retains its permanent replacement",
        ))?;
        let Event::Replacement {
            id,
            session_id,
            message_id,
            ..
        } = record.event()
        else {
            return Err(Error::corrupt(
                "replacement lookup returns its own record kind",
            ));
        };
        if record.account_id() != self.input.subject.account_id(&self.actor)
            || *session_id != self.actor.session_id()
            || *message_id != self.input.message_id
            || original.person_id() != request.person_id()
            || original.created_at() != record.happened_at()
        {
            return Err(Error::corrupt(
                "mobile completion and its permanent result agree",
            ));
        }
        service
            .app
            .anchors()
            .confirm::<account::state::State>(&record)?;
        Ok(Conclusion::Completed(Completion {
            id: *id,
            completed_at: record.happened_at(),
        }))
    }
}

pub(super) async fn verify(
    client: &Client,
    app: &App,
    keys: &crate::identity::keys::Keys,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let mut completions: HashMap<_, _> =
        completion::fetch_all::<Rejection>(client, app.keys(), COMPLETIONS)
            .await?
            .into_iter()
            .map(|record| (record.operation_id(), record))
            .collect();
    for record in account::record::replacements(client, app.keys(), now).await? {
        let completion = completions
            .remove(&record.operation_id())
            .ok_or(Error::corrupt(
                "each permanent replacement retains its completion",
            ))?;
        completion.check_time(now)?;
        let Event::Replacement {
            session_id,
            message_id,
            operator_id,
            ..
        } = record.event()
        else {
            return Err(Error::corrupt("replacement startup reads its own event"));
        };
        let input: Canonical = completion.input(app.keys())?;
        let operator = person::revision::fetch(client, app.keys(), *operator_id)
            .await
            .map_err(person::revision::ReadError::from)?
            .person_id();
        let action = if operator == record.person_id() {
            "self"
        } else {
            "assisted"
        };
        if completion.expires_at().is_some()
            || completion.created_at() != record.happened_at()
            || completion.person_id() != operator
            || input.session_id != *session_id
            || input.message_id != *message_id
        {
            return Err(Error::corrupt(
                "a mobile completion preserves its operator, time and original session/message",
            ));
        }
        let expected = request(
            action,
            record.account_id(),
            &input,
            &Authorization {
                person_id: operator,
                revision_id: completion.revision_id(),
                now,
            },
        )?;
        completion
            .matches(COMPLETIONS, app.keys(), &expected)
            .map_err(|error| match error {
                Error::Mismatch => {
                    Error::corrupt("a replacement retains its canonical completion input")
                }
                error => error,
            })?;
    }
    for rejected in database::rejections(client, app.keys()).await? {
        let completion = completions
            .remove(&rejected.operation_id())
            .ok_or(Error::corrupt(
                "each retained code rejection has its completion",
            ))?;
        completion.check_time(now)?;
        rejected.validate(keys, now)?;
        rejected.paired(&completion)?;
        let input: Canonical = completion.input(app.keys())?;
        rejected.verify_input(keys, &completion, input)?;
    }
    if !completions.is_empty() {
        return Err(Error::corrupt(
            "every mobile completion has exactly one retained result",
        ));
    }
    Ok(())
}

pub(super) async fn verify_retained(
    client: &Client,
    app: &App,
    keys: &crate::identity::keys::Keys,
    operation: operation::Id,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let original = completion::original(client, app.keys(), COMPLETIONS, operation).await?;
    let rejected = database::find_rejection(client, app.keys(), operation).await?;
    match (original, rejected) {
        (None, None) => Ok(()),
        (Some(original), Some(rejected)) => {
            original.check_time(now)?;
            rejected.validate(keys, now)?;
            rejected.paired(&original)?;
            rejected.verify_input(keys, &original, original.input(app.keys())?)
        }
        _ => Err(Error::corrupt(
            "a retained rejection has its complete original evidence",
        )),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use chrono::TimeDelta;
    use fixture::database_proxy::{Fault, Proxy};
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;

    use crate::{
        access::role::tests::actor,
        identity::{
            authentication::{
                self, Database, Supplier, server,
                session::{
                    Receipt,
                    tests::{
                        CODE, age, input as authentication_input, message as authentication_message,
                    },
                },
            },
            current::{Actor, Proof},
            message, operation,
            transfer::{
                self, State,
                completion::{Error, Input, Rejection},
                tests::{bind, input as transfer_input, operation, send, start, start_on},
            },
        },
        startup::Services,
    };

    fn input(operation_id: operation::Id, message_id: message::Id, code: &str) -> Input {
        Input::new(
            operation_id,
            "13800000000".parse().expect("new mobile"),
            message_id,
            code.parse().expect("code"),
        )
    }

    #[test]
    fn state_schema_matches_the_public_json_for_every_outcome() {
        use std::collections::BTreeSet;

        use serde_json::json;

        let time = "2026-10-15T00:00:00Z".parse().expect("fixed time");
        let schema =
            serde_json::to_value(<State as utoipa::PartialSchema>::schema()).expect("state schema");
        let variants = schema["oneOf"].as_array().expect("exclusive outcomes");
        assert_eq!(variants.len(), 3);
        for (state, expected) in [
            (
                State::Pending { expires_at: time },
                json!({"status":"pending","expiresAt":"2026-10-15T00:00:00Z"}),
            ),
            (
                State::Completed { completed_at: time },
                json!({"status":"completed","completedAt":"2026-10-15T00:00:00Z"}),
            ),
            (State::Expired, json!({"status":"expired"})),
        ] {
            assert_eq!(serde_json::to_value(state).expect("public JSON"), expected);
            let variant = variants
                .iter()
                .find(|variant| {
                    variant["properties"]["status"]["enum"] == json!([expected["status"]])
                })
                .expect("documented outcome");
            let fields = expected.as_object().expect("public fields");
            let properties = variant["properties"]
                .as_object()
                .expect("documented fields");
            let names = fields.keys().map(String::as_str).collect::<BTreeSet<_>>();
            assert_eq!(
                properties
                    .keys()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>(),
                names
            );
            let required = variant["required"]
                .as_array()
                .expect("required fields")
                .iter()
                .map(|field| field.as_str().expect("field name"))
                .collect::<BTreeSet<_>>();
            assert_eq!(required, names);
            for field in fields.keys().filter(|field| field.as_str() != "status") {
                assert_eq!(properties[field]["type"], "string");
                assert_eq!(properties[field]["format"], "date-time");
            }
        }
    }

    #[test]
    fn assisted_completion_matches_an_independent_input_proof_vector() {
        use crate::identity::transfer::completion::{Proof, Subject};
        let mut input = Input::new(
            Uuid::from_u128(1).to_string().parse().expect("operation"),
            "13600000000".parse().expect("phone"),
            Uuid::from_u128(7).to_string().parse().expect("message"),
            "001234".parse().expect("code"),
        );
        input.subject = Subject::Assisted(
            serde_json::from_value(serde_json::json!(Uuid::from_u128(8))).expect("account"),
        );
        let proof = Proof {
            input: &input,
            transfer_id: serde_json::from_value(serde_json::json!(Uuid::from_u128(6)))
                .expect("transfer"),
            operator_id: serde_json::from_value(serde_json::json!(Uuid::from_u128(2)))
                .expect("operator"),
            person_id: serde_json::from_value(serde_json::json!(Uuid::from_u128(3)))
                .expect("person"),
            session_id: Uuid::from_u128(5).to_string().parse().expect("session"),
        };
        // Independently calculated with Python struct, uuid, hashlib and hmac:
        // Python HKDF-SHA-256 of the fixture root (32 bytes of 1; salt "nucleus/key/v1", info "proof"), then hmac.
        assert_eq!(
            hex::encode(
                authentication::keys()
                    .input
                    .active()
                    .sign(&platform::cryptography::Encoded::new(&proof).expect("encoding"))
                    .as_ref()
            ),
            "a7b7d94e347d12a52bda1c1c0dda3f68e5d91ca1f888370bbcd032f7cc8cb745"
        );
    }

    async fn prepare(
        database: &Database,
        services: &Services,
        actor: &Actor,
        supplier: &mut Supplier,
        sms: &mut fixture::supplier::Supplier,
    ) -> (operation::Id, transfer::Transfer, message::Id, String) {
        age(database, services.authentication(), TimeDelta::seconds(61)).await;
        let (_, sent, code) = send(database, services, actor, supplier, sms).await;
        let operation_id = operation();
        let transfer = services
            .transfer()
            .create(
                actor.clone(),
                transfer_input(operation_id, sent.id(), &code),
            )
            .await
            .expect("established transfer");
        let (_, sent, code) = bind(database, services, actor, transfer.id(), supplier, sms).await;
        (operation_id, transfer, sent.id(), code)
    }

    async fn authenticate(
        database: &Database,
        services: &Services,
        supplier: &mut Supplier,
    ) -> Actor {
        let id =
            authentication_message(database, services.authentication(), supplier, "13800000000")
                .await;
        let Receipt::Session(issued) = services
            .authentication()
            .create_session(authentication_input(
                Uuid::new_v4(),
                id,
                "13800000000",
                CODE,
            ))
            .await
            .expect("session with new phone")
        else {
            panic!("existing account must remain bound");
        };
        services
            .current()
            .authenticate(Proof {
                id: issued.id(),
                token: issued.token().text().parse().expect("token"),
            })
            .await
            .expect("new actor")
    }

    #[test]
    fn completion_changes_only_the_phone_and_revokes_the_original_session_set() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(35), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let (context, services) = start(&database, &supplier, &sms).await;
                let original = actor(&database, &services, &mut supplier).await;
                let other = actor(&database, &services, &mut supplier).await;
                let (establishment, transfer, message_id, code) = prepare(&database, &services, &original, &mut supplier, &mut sms).await;
                let observer = database.connections(&database.user).recover().await.expect("independent facts");
                let before = observer.client().query_one("SELECT jsonb_build_object('id',id,'person_id',person_id,'id_number',id_number,'avatar',avatar,'wechat',wechat,'created_at',created_at,'suspension_id',suspension_id,'cancellation_id',cancellation_id)::text AS business,sequence,version FROM identity.accounts WHERE id=$1", &[&original.account_id()]).await.expect("account identity and state head before replacement");
                let before_sessions: i64 = observer.client().query_one("SELECT count(*) FROM identity.sessions WHERE account_id=$1", &[&original.account_id()]).await.expect("original session count").get(0);
                let expected_hash = authentication::keys().mobile.hash(&"13800000000".parse().expect("new mobile")).expect("new mobile digest");
                let completion = Box::pin(async {
                    assert_eq!(services.account().fetch_transfer(&original, transfer.id()).await.expect("pending"), State::Pending { expires_at: transfer.expires_at() });
                    assert!(matches!(services.account().fetch_transfer(&original, transfer::Id(Uuid::new_v4())).await, Err(crate::account::Error::Absent)));
                    let wrong = if code == "000000" { "000001" } else { "000000" };
                    for _ in 0..2 {
                        let result = services.transfer().complete(original.clone(), input(establishment, message_id, wrong)).await;
                        assert!(matches!(result, Err(Error::Rejected(Rejection::CodeInvalid))), "{result:?}");
                    }
                    assert!(matches!(services.transfer().complete(original.clone(), input(establishment, message_id, &code)).await, Err(Error::Mismatch)));
                    assert_eq!(observer.client().query_one("SELECT failures FROM identity.confirmations WHERE id=$1", &[&message_id]).await.expect("one counted guess").get::<_,i16>(0), 1);
                    let operation_id = operation();
                    let result = services.transfer().complete(original.clone(), input(operation_id, message_id, &code)).await.expect("complete phone replacement");
                    assert_eq!(result.id, transfer.id());
                    let fact = observer.client().query_one("SELECT completion.id=$2 AND completion.session_id=$3 AND completion.message_id=$4 AND completion.account_id=$5 AND completion.person_id=account.person_id AND completion.completed_at=$6, (SELECT count(*) FROM identity.revocations WHERE cause_id=completion.id AND mode='transfer' AND operation_id=completion.operation_id AND revoked_at=completion.completed_at), NOT EXISTS(SELECT 1 FROM identity.handoffs handoff JOIN identity.sessions session ON session.id=handoff.id WHERE session.account_id=account.id), (account.mobile).hash=$7, jsonb_build_object('id',account.id,'person_id',account.person_id,'id_number',account.id_number,'avatar',account.avatar,'wechat',account.wechat,'created_at',account.created_at,'suspension_id',account.suspension_id,'cancellation_id',account.cancellation_id)::text, account.sequence=$8::bigint+1 AND account.version=$9::bigint+1 AND account.changed_at=completion.completed_at FROM identity.replacements completion JOIN identity.accounts account ON account.id=completion.account_id WHERE completion.operation_id=$1", &[&operation_id, &transfer.id(), &original.session_id(), &message_id, &original.account_id(), &result.completed_at, &expected_hash, &before.get::<_,i64>("sequence"), &before.get::<_,i64>("version")]).await.expect("atomic persistent completion before any replay");
                    assert!(fact.get::<_,bool>(0)); assert_eq!(fact.get::<_,i64>(1), before_sessions); assert!(fact.get::<_,bool>(2)); assert!(fact.get::<_,bool>(3)); assert_eq!(fact.get::<_,String>(4), before.get::<_,String>("business")); assert!(fact.get::<_,bool>(5));
                    for old in [&original, &other] {
                        assert!(services.current().read(old).await.is_err());
                        assert_eq!(services.transfer().complete(old.clone(), input(if old.session_id() == original.session_id() { operation_id } else { operation() }, message_id, &code)).await.expect_err("revoked session cannot complete").code(), Some("authentication_invalid"));
                    }
                    result
                });
                let completed = tokio::select! { biased;
                    _ = sms.request() => panic!("completion never sends SMS"),
                    _ = supplier.observe() => panic!("completion never calls verification"),
                    completed = completion => completed,
                };
                age(&database, services.authentication(), TimeDelta::seconds(61)).await;
                let new_actor = authenticate(&database, &services, &mut supplier).await;
                assert_eq!(services.account().fetch_transfer(&new_actor, transfer.id()).await.expect("new session reads completion"), State::Completed { completed_at: completed.completed_at - TimeDelta::seconds(61) });
                services.drain().await.expect("all accepted work ended"); context.stop().await.expect("runtime ended");
                let (context, services) = start(&database, &supplier, &sms).await;
                let work = Box::pin(async {
                    assert!(matches!(services.account().fetch_transfer(&new_actor, transfer.id()).await.expect("retained completion after restart"), State::Completed { .. }));
                    services.drain().await.expect("restarted services drained"); context.stop().await.expect("restarted runtime ended");
                });
                tokio::select! { biased; _ = sms.request() => panic!("read and restart never send SMS"), _ = supplier.observe() => panic!("read never calls verification"), () = work => {} }
                observer.terminate().await.expect("observer ended"); sms.stop().await; supplier.stop().await;
            })).await.expect("bounded complete phone lifecycle");
        }));
    }

    #[test]
    fn completion_checks_original_session_latest_code_and_five_guesses_before_number_occupancy() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(35), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let (context, services) = start(&database, &supplier, &sms).await;
                let original = actor(&database, &services, &mut supplier).await;
                let other = actor(&database, &services, &mut supplier).await;
                let occupied = crate::access::appointment::tests::register(&database, &services, &mut supplier).await;
                let (_, transfer, message_id, code) = prepare(&database, &services, &original, &mut supplier, &mut sms).await;
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let work = Box::pin(async {
                    assert!(matches!(services.account().fetch_transfer(&occupied, transfer.id()).await, Err(crate::account::Error::Absent)));
                    assert!(matches!(services.transfer().complete(other, input(operation(), message_id, &code)).await, Err(Error::Rejected(Rejection::Verification))));
                    assert!(matches!(services.transfer().complete(original.clone(), Input::new(operation(), "13900000000".parse().expect("current mobile"), message_id, code.parse().expect("code"))).await, Err(Error::Rejected(Rejection::Conflict))));
                    assert!(matches!(services.transfer().complete(original.clone(), input(operation(), message_id, &code)).await, Err(Error::Rejected(Rejection::Taken))));
                    assert_eq!(observer.client().query_one("SELECT failures FROM identity.confirmations WHERE id=$1", &[&message_id]).await.expect("occupied number does not count as incorrect code").get::<_,i16>(0), 0);
                    let wrong = if code == "000000" { "000001" } else { "000000" };
                    for _ in 0..5 {
                        let id = operation();
                        for _ in 0..2 {
                            assert!(matches!(services.transfer().complete(original.clone(), input(id, message_id, wrong)).await, Err(Error::Rejected(Rejection::CodeInvalid))));
                        }
                    }
                    for submitted in [&code[..], wrong] {
                        assert!(matches!(services.transfer().complete(original.clone(), input(operation(), message_id, submitted)).await, Err(Error::Rejected(Rejection::Verification))));
                    }
                    assert!(observer.client().query_one("SELECT (SELECT failures=5 FROM identity.confirmations WHERE id=$1) AND (SELECT count(*)=5 FROM identity.rejections WHERE purpose='completion' AND message_id=$1) AND NOT EXISTS(SELECT 1 FROM identity.replacements) AND NOT EXISTS(SELECT 1 FROM identity.revocations)", &[&message_id]).await.expect("five attempts and no phone effects").get::<_,bool>(0));
                });
                tokio::select! { biased; _ = sms.request() => panic!("completion refusals never send SMS"), _ = supplier.observe() => panic!("completion refusals never verify"), () = work => {} }
                age(&database, services.authentication(), TimeDelta::seconds(61)).await;
                let (_, newer, newer_code) = bind(&database, &services, &original, transfer.id(), &mut supplier, &mut sms).await;
                assert!(matches!(services.transfer().complete(original.clone(), input(operation(), message_id, &code)).await, Err(Error::Rejected(Rejection::Verification))));
                assert!(matches!(services.transfer().complete(original.clone(), input(operation(), newer.id(), &newer_code)).await, Err(Error::Rejected(Rejection::Taken))));
                age(&database, services.authentication(), TimeDelta::minutes(11)).await;
                let work = Box::pin(async {
                    assert_eq!(services.account().fetch_transfer(&original, transfer.id()).await.expect("expired status"), State::Expired);
                    assert!(matches!(services.transfer().complete(original, input(operation(), newer.id(), &newer_code)).await, Err(Error::Rejected(Rejection::Verification))));
                    services.drain().await.expect("drained"); context.stop().await.expect("runtime ended");
                });
                tokio::select! { biased; _ = sms.request() => panic!("expired verification never sends SMS"), _ = supplier.observe() => panic!("expired verification never calls verification"), () = work => {} }
                observer.terminate().await.expect("observer ended"); sms.stop().await; supplier.stop().await;
            })).await.expect("bounded refusal and privacy checks");
        }));
    }

    #[test]
    fn completion_survives_short_record_cleanup_and_cancellation_preserves_original_evidence() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for completed in [false, true] {
                timeout(Duration::from_secs(35), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let mut sms = fixture::supplier::Supplier::start().await;
                    let (context, services) = start(&database, &supplier, &sms).await;
                    let original = actor(&database, &services, &mut supplier).await;
                    let (establishment, transfer, message_id, code) = prepare(&database, &services, &original, &mut supplier, &mut sms).await;
                    let operation_id = operation();
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let current = if completed {
                        let wrong = if code == "000000" { "000001" } else { "000000" };
                        assert!(matches!(services.transfer().complete(original.clone(), input(establishment, message_id, wrong)).await, Err(Error::Rejected(Rejection::CodeInvalid))));
                        services.transfer().complete(original.clone(), input(operation_id, message_id, &code)).await.expect("completion before retention");
                        crate::registration::tests::age_facts(&database, &services, TimeDelta::hours(26)).await;
                        for _ in 0..3 {
                            services.transfer().retention().run().await.expect("ended wrong-code family and transfers");
                            services.authentication().retention().run().await.expect("ended dependent messages");
                        }
                        assert!(observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.transfers WHERE id=$1) AND NOT EXISTS(SELECT 1 FROM identity.rejections) AND NOT EXISTS(SELECT 1 FROM identity.confirmations) AND EXISTS(SELECT 1 FROM identity.replacements WHERE id=$1)", &[&transfer.id()]).await.expect("short-term dependencies removed but completion retained").get::<_,bool>(0));
                        let current = authenticate(&database, &services, &mut supplier).await;
                        assert!(matches!(services.account().fetch_transfer(&current, transfer.id()).await.expect("completion without short transfer"), State::Completed { .. }));
                        current
                    } else { original.clone() };
                    let work = Box::pin(async {
                        let account = services.current().read(&current).await.expect("current account before cancellation");
                        let sessions = observer.client().query("SELECT id FROM identity.sessions WHERE account_id=$1 ORDER BY id", &[&original.account_id()]).await.expect("retained sessions before cancellation").into_iter().map(|row| row.get::<_,crate::identity::session::Id>(0)).collect::<Vec<_>>();
                        let confirmation = serde_json::from_value(serde_json::json!({"accountId":account.id(), "personId":account.person_id(), "revisionId":account.revision_id()})).expect("confirmed self identity");
                        services.cancellation().cancel(current, operation(), confirmation).await.expect("actual account cancellation");
                        let facts = observer.client().query_one("SELECT account.cancellation_id IS NOT NULL AND account.mobile IS NULL AND account.id_number IS NULL AND account.suspension_id IS NULL AS tombstone,EXISTS(SELECT 1 FROM identity.replacements WHERE id=$2) AS replacement,EXISTS(SELECT 1 FROM identity.completions WHERE family='mobile_completion' AND operation_id=$3) AS completion,NOT EXISTS(SELECT 1 FROM identity.sessions WHERE account_id=$1) AS sessions_absent,NOT EXISTS(SELECT 1 FROM identity.issuances WHERE account_id=$1) AS issuances_absent,NOT EXISTS(SELECT 1 FROM identity.revocations WHERE account_id=$1) AS revocations_absent,NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=ANY($4::uuid[])) AS handoffs_absent,EXISTS(SELECT 1 FROM identity.cancellations WHERE account_id=$1) AS cancelled FROM identity.accounts account WHERE account.id=$1", &[&original.account_id(), &transfer.id(), &operation_id, &sessions]).await.expect("permanent evidence and complete short-lived cleanup after cancellation");
                        for field in ["tombstone", "sessions_absent", "issuances_absent", "revocations_absent", "handoffs_absent", "cancelled"] {
                            assert!(facts.get::<_,bool>(field), "{field}");
                        }
                        assert_eq!(facts.get::<_,bool>("replacement"), completed);
                        assert_eq!(facts.get::<_,bool>("completion"), completed);
                        let evidence = crate::kernel::completion::original::<Rejection>(observer.client(), services.transfer().0.app.keys(), super::COMPLETIONS, operation_id).await.expect("authenticate the original mobile completion evidence");
                        assert_eq!(evidence.is_some(), completed);
                        if let Some(evidence) = evidence {
                            assert_eq!(evidence.operation_id(), operation_id);
                        }
                        services.drain().await.expect("drained"); context.stop().await.expect("runtime ended");
                    });
                    tokio::select! { biased; _ = sms.request() => panic!("cleanup and recovery never send SMS"), _ = supplier.observe() => panic!("cleanup and recovery never verify"), () = work => {} }
                    observer.terminate().await.expect("observer ended"); sms.stop().await; supplier.stop().await;
                })).await.expect("bounded retained outcome and cancellation");
            }
        }));
    }

    #[test]
    fn completion_and_original_phone_authentication_coordinate_in_both_orders() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for completion_first in [false, true] {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let mut sms = fixture::supplier::Supplier::start().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start_on(authentication::proxied(&database, &proxy), &supplier, &sms).await;
                    let original = actor(&database, &services, &mut supplier).await;
                    let (_, transfer, message_id, code) = prepare(&database, &services, &original, &mut supplier, &mut sms).await;
                    let old_code = authentication_message(&database, services.authentication(), &mut supplier, "13900000000").await;
                    let authentication_id = Uuid::new_v4();
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let authentication = services.authentication().clone();
                    let transfer_service = services.transfer().clone(); let actor = original.clone();
                    let create = Box::pin(async move { authentication.create_session(authentication_input(authentication_id, old_code, "13900000000", CODE)).await });
                    let complete = Box::pin(async move { transfer_service.complete(actor, input(operation(), message_id, &code)).await });
                    let work = Box::pin(async {
                        proxy.arm(Fault::DelayedCommit);
                        let (creation, completion, blocker) = if completion_first {
                            let completion = tokio::spawn(complete);
                            let blocker = timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("completion holds commit");
                            (tokio::spawn(create), completion, blocker)
                        } else {
                            let creation = tokio::spawn(create);
                            let blocker = timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("authentication holds commit");
                            (creation, tokio::spawn(complete), blocker)
                        };
                        let query = if completion_first { "SELECT identity.lock_account($1)" } else { "SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE" };
                        timeout(Duration::from_secs(3), async { loop {
                            if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity waiter JOIN pg_catalog.pg_stat_activity original ON original.pid=ANY(pg_catalog.pg_blocking_pids(waiter.pid)) WHERE waiter.datname=current_database() AND original.datname=current_database() AND original.pid=$1 AND waiter.wait_event_type='Lock' AND waiter.query=$2)", &[&blocker, &query]).await.expect("opposing account lock waiter").get::<_,bool>(0) { break; }
                            tokio::task::yield_now().await;
                        }}).await.expect("actual concurrent account coordination");
                        proxy.release();
                        completion.await.expect("completion worker").expect("phone change commits");
                        let created = creation.await.expect("authentication worker");
                        let facts = observer.client().query_one("SELECT (SELECT count(*) FROM identity.revocations WHERE mode='transfer' AND cause_id=$1), NOT EXISTS(SELECT 1 FROM identity.sessions session WHERE account_id=$2 AND NOT EXISTS(SELECT 1 FROM identity.revocations WHERE id=session.id)), NOT EXISTS(SELECT 1 FROM identity.handoffs handoff JOIN identity.sessions session ON session.id=handoff.id WHERE session.account_id=$2), (SELECT count(*) FROM identity.sessions session JOIN identity.authentications authentication ON authentication.id=session.authentication_id WHERE authentication.operation_id=$3)", &[&transfer.id(), &original.account_id(), &authentication_id]).await.expect("persistent account outcome before replay");
                        assert_eq!(facts.get::<_,i64>(0), if completion_first { 1 } else { 2 }); assert!(facts.get::<_,bool>(1)); assert!(facts.get::<_,bool>(2));
                        assert_eq!(facts.get::<_,i64>(3), i64::from(!completion_first));
                        if completion_first {
                            assert_eq!(created.err().expect("old account no longer owns phone").failure(), authentication::Failure::Verification);
                        } else {
                            match created { Ok(Receipt::Session(_)) => {}, Ok(Receipt::Registration(_)) => panic!("old account was still current at authentication commit"), Err(error) => assert_eq!(error.failure(), authentication::Failure::Verification) }
                        }
                        services.drain().await.expect("both works drained"); context.stop().await.expect("runtime ended");
                    });
                    tokio::select! { biased; _ = sms.request() => panic!("coordination never sends SMS"), _ = supplier.observe() => panic!("coordination never calls verification"), () = work => {} }
                    observer.terminate().await.expect("observer ended"); sms.stop().await; supplier.stop().await; proxy.stop().await;
                })).await.expect("bounded authentication and completion race");
            }
        }));
    }

    #[test]
    fn completion_and_registration_share_the_new_number_occupancy_in_both_orders() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for completion_first in [false, true] {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let mut sms = fixture::supplier::Supplier::start().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start_on(authentication::proxied(&database, &proxy), &supplier, &sms).await;
                    let original = actor(&database, &services, &mut supplier).await;
                    let (_, transfer, message_id, code) = prepare(&database, &services, &original, &mut supplier, &mut sms).await;
                    let id = authentication_message(&database, services.authentication(), &mut supplier, "13800000000").await;
                    let Receipt::Registration(credential) = services.authentication().create_session(authentication_input(Uuid::new_v4(), id, "13800000000", CODE)).await.expect("unoccupied new phone") else { panic!("new phone has no account yet"); };
                    let registration_input = crate::registration::Input::new(operation(), credential.id(), credential.token().text().parse().expect("registration proof"), "Other Person".parse().expect("name"), "00000020000229007X".parse().expect("number"));
                    let registration = services.registration().clone(); let transfer_service = services.transfer().clone(); let actor = original.clone();
                    let register = Box::pin(async move { registration.register(registration_input).await });
                    let complete = Box::pin(async move { transfer_service.complete(actor, input(operation(), message_id, &code)).await });
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let work = Box::pin(async {
                        proxy.arm(Fault::DelayedCommit);
                        let (registration, completion) = if completion_first {
                            let completion = tokio::spawn(complete);
                            timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("completion holds number lock");
                            (tokio::spawn(register), completion)
                        } else {
                            let registration = tokio::spawn(register);
                            timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("registration holds number lock");
                            (registration, tokio::spawn(complete))
                        };
                        timeout(Duration::from_secs(3), async { loop {
                            // Independently computed LP32("identity/mobile", "13800000000") SHA-256 prefix.
                            if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_locks WHERE locktype='advisory' AND NOT granted AND classid::bigint=2777574140 AND objid::bigint=3627076760 AND objsubid=1 AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()))", &[]).await.expect("exact new-number lock waiter").get::<_,bool>(0) { break; }
                            tokio::task::yield_now().await;
                        }}).await.expect("actual competing number lock");
                        proxy.release();
                        let registered = registration.await.expect("registration worker"); let completed = completion.await.expect("completion worker");
                        if completion_first { completed.expect("completion wins"); assert_eq!(registered.err().expect("number already owned").failure(), crate::registration::Failure::MobileTaken); }
                        else { registered.expect("registration wins"); assert_eq!(completed.expect_err("number already owned").code(), Some("mobile_taken")); }
                        let hash = authentication::keys().mobile.hash(&"13800000000".parse().expect("new mobile")).expect("hash");
                        let facts = observer.client().query_one("SELECT (SELECT count(*) FROM identity.accounts WHERE (mobile).hash=$1), EXISTS(SELECT 1 FROM identity.accounts WHERE id=$2 AND (mobile).hash=$1), EXISTS(SELECT 1 FROM identity.replacements WHERE id=$3), EXISTS(SELECT 1 FROM identity.revocations WHERE id=$4), (SELECT failures FROM identity.confirmations WHERE id=$5)", &[&hash, &original.account_id(), &transfer.id(), &original.session_id(), &message_id]).await.expect("exclusive number owner and atomic loser rollback");
                        assert_eq!(facts.get::<_,i64>(0), 1); for index in [1,2,3] { assert_eq!(facts.get::<_,bool>(index), completion_first); } assert_eq!(facts.get::<_,i16>(4), 0);
                        services.drain().await.expect("both works drained"); context.stop().await.expect("runtime ended");
                    });
                    tokio::select! { biased; _ = sms.request() => panic!("number competition never sends SMS"), _ = supplier.observe() => panic!("number competition never calls verification"), () = work => {} }
                    observer.terminate().await.expect("observer ended"); sms.stop().await; supplier.stop().await; proxy.stop().await;
                })).await.expect("bounded registration and completion race");
            }
        }));
    }

    #[test]
    fn completion_rejects_an_unsigned_mobile_change_after_runtime_pairing_rejects_it() {
        use tokio_postgres::error::SqlState;

        use crate::identity::mobile::{
            Mobile,
            envelope::{Binding, Envelope},
        };

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(35), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let (context, services) = start(&database, &supplier, &sms).await;
                let original = actor(&database, &services, &mut supplier).await;
                let (_, _, message_id, code) = prepare(&database, &services, &original, &mut supplier, &mut sms).await;
                let operation_id = operation();
                let command = input(operation_id, message_id, &code);
                let writer = database.connections(&database.user).recover().await.expect("runtime identity");
                let client = writer.client();
                let keys = authentication::keys();
                let tampered: Mobile = "13700000000".parse().expect("different valid mobile");
                assert_ne!(tampered.as_ref(), command.mobile.as_ref());
                let account = client.query_one("SELECT mobile,seal FROM identity.accounts WHERE id=$1", &[&original.account_id()]).await.expect("original sealed account");
                let retained: Envelope = account.get("mobile");
                assert!(!retained.matches(&keys.mobile, &tampered).expect("different current mobile"));
                let original_seal: Vec<u8> = account.get("seal");
                let mobile = Envelope::seal(&keys.encryption, &keys.mobile, Binding::Account(original.account_id()), &tampered).expect("valid account-bound envelope");
                let observation = "SELECT (SELECT count(*) FROM identity.replacements WHERE account_id=$1) AS replacements,(SELECT count(*) FROM identity.rejections WHERE purpose='completion' AND operation_id=$2) AS rejections,(SELECT failures FROM identity.confirmations WHERE id=$3) AS failures,(SELECT count(*) FROM identity.revocations WHERE account_id=$1) AS revocations,(SELECT json_agg(handoff ORDER BY handoff.id)::text FROM identity.handoffs handoff JOIN identity.sessions session ON session.id=handoff.id WHERE session.account_id=$1) AS handoffs";
                let before = client.query_one(observation, &[&original.account_id(), &operation_id, &message_id]).await.expect("confirmation and handoffs before either attempted write");
                assert_eq!(before.get::<_,i64>("replacements"), 0);
                assert_eq!(before.get::<_,i64>("rejections"), 0);
                assert_eq!(before.get::<_,i64>("revocations"), 0);
                let handoffs: String = before.get("handoffs");
                let work = Box::pin(async {
                    let rejected = client.execute("UPDATE identity.accounts SET mobile=$2 WHERE id=$1", &[&original.account_id(), &mobile]).await.expect_err("runtime bare update fails structural pairing");
                    assert_eq!(rejected.code(), Some(&SqlState::CHECK_VIOLATION));
                    assert_eq!(rejected.as_db_error().expect("pairing constraint").constraint(), Some("accounts_records_check"));
                    let owner = database.connections("postgres").recover().await.expect("isolated storage corruption owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("explicit storage corruption fixture");
                    assert_eq!(owner.client().execute("UPDATE identity.accounts SET mobile=$2 WHERE id=$1", &[&original.account_id(), &mobile]).await.expect("owner injects unsigned mobile change"), 1);
                    owner.client().batch_execute("COMMIT").await.expect("storage corruption committed");
                    owner.terminate().await.expect("corruption owner ended");
                    let damaged = client.query_one("SELECT mobile,seal FROM identity.accounts WHERE id=$1", &[&original.account_id()]).await.expect("independent committed corruption observation");
                    let changed: Envelope = damaged.get("mobile");
                    assert!(changed.matches(&keys.mobile, &tampered).expect("the intended mobile was stored"));
                    assert!(damaged.get::<_,Vec<u8>>("seal") == original_seal, "the fixture never re-signs the account");
                    let error = services.transfer().complete(original.clone(), command).await.expect_err("a valid envelope cannot replace the state seal");
                    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
                    assert!(services.transfer().0.app.registry().ready().is_err(), "stored corruption closes common admission");
                    let after = client.query_one(observation, &[&original.account_id(), &operation_id, &message_id]).await.expect("no completion consequences after corruption rejection");
                    for field in ["replacements", "rejections", "revocations"] {
                        assert_eq!(after.get::<_,i64>(field), before.get::<_,i64>(field), "{field}");
                    }
                    assert_eq!(after.get::<_,i16>("failures"), before.get::<_,i16>("failures"));
                    assert!(after.get::<_,String>("handoffs") == handoffs, "all original handoffs remain unchanged");
                    services.drain().await.expect("drained"); context.stop().await.expect("runtime ended");
                });
                tokio::select! { biased; _ = sms.request() => panic!("corruption rejection never sends SMS"), _ = supplier.observe() => panic!("corruption rejection never calls verification"), () = work => {} }
                writer.terminate().await.expect("writer ended"); sms.stop().await; supplier.stop().await;
            })).await.expect("bounded structural and sealed-state rejection");
        }));
    }

    #[test]
    fn completion_expiry_at_insert_rolls_back_success_and_rejection_without_counting() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for (transfer_deadline, valid, failed_rollback) in [
                (false, true, false), (false, false, false),
                (true, true, false), (true, false, false), (false, true, true),
            ] {
                    timeout(Duration::from_secs(25), Box::pin(async {
                        let database = Database::initialized(Arc::clone(&server)).await;
                        let mut supplier = Supplier::new().await;
                        let mut sms = fixture::supplier::Supplier::start().await;
                        let proxy = Proxy::start(&server).await;
                        let (context, services) = start_on(authentication::proxied(&database, &proxy), &supplier, &sms).await;
                        let original = actor(&database, &services, &mut supplier).await;
                        age(&database, services.authentication(), TimeDelta::seconds(61)).await;
                        let (_, sent, code) = send(&database, &services, &original, &mut supplier, &mut sms).await;
                        let transfer = services.transfer().create(original.clone(), transfer_input(operation(), sent.id(), &code)).await.expect("transfer");
                        if transfer_deadline { age(&database, services.authentication(), TimeDelta::seconds(595)).await; }
                        let (_, sent, code) = bind(&database, &services, &original, transfer.id(), &mut supplier, &mut sms).await;
                        let observer = database.connections(&database.user).recover().await.expect("independent reader");
                        if !transfer_deadline {
                            let expires_at: chrono::DateTime<chrono::Utc> = observer.client().query_one("SELECT expires_at FROM identity.confirmations WHERE id=$1", &[&sent.id()]).await.expect("code deadline").get(0);
                            age(&database, services.authentication(), expires_at - database.now().await - TimeDelta::seconds(5)).await;
                        }
                        let table = if valid { "replacements" } else { "rejections" };
                        let blocker = database.connections("postgres").recover().await.expect("isolated insert barrier");
                        blocker.client().batch_execute(&format!("CREATE FUNCTION public.completion_deadline() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path='' AS $$ BEGIN PERFORM pg_catalog.pg_advisory_xact_lock(1074, 1); RETURN NEW; END $$; CREATE TRIGGER completion_deadline BEFORE INSERT ON identity.{table} FOR EACH ROW EXECUTE FUNCTION public.completion_deadline()")).await.expect("insert barrier before the completion guard");
                        blocker.client().batch_execute("BEGIN; SELECT pg_catalog.pg_advisory_xact_lock(1074, 1)").await.expect("hold insert");
                        let blocker_id: i32 = blocker.client().query_one("SELECT pg_catalog.pg_backend_pid()", &[]).await.expect("barrier backend").get(0);
                        let operation_id = operation();
                        let code = if valid { code } else if code == "000000" { "000001".into() } else { "000000".into() };
                        let service = services.transfer().clone();
                        let actor = original.clone();
                        let message_id = sent.id();
                        let attempted_code = code.clone();
                        let task = tokio::spawn(async move { service.complete(actor, input(operation_id, message_id, &attempted_code)).await });
                        let work = Box::pin(async {
                            timeout(Duration::from_secs(3), async {
                                loop {
                                    if observer.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname=pg_catalog.current_database() AND $1=ANY(pg_catalog.pg_blocking_pids(pid)))", &[&blocker_id]).await.expect("actual insert waiter").get::<_,bool>(0) { break; }
                                    tokio::task::yield_now().await;
                                }
                            }).await.expect("completion qualified before the insert barrier");
                            let deadline = observer.client().query_one("SELECT LEAST(transfer.created_at+interval '10 minutes',confirmation.expires_at), transfer.created_at+interval '10 minutes'<confirmation.expires_at, pg_catalog.clock_timestamp()<LEAST(transfer.created_at+interval '10 minutes',confirmation.expires_at) FROM identity.transfers transfer CROSS JOIN identity.confirmations confirmation WHERE transfer.id=$1 AND confirmation.id=$2", &[&transfer.id(), &message_id]).await.expect("independent first deadline");
                            assert_eq!(deadline.get::<_,bool>(1), transfer_deadline);
                            assert!(deadline.get::<_,bool>(2), "qualified insert is still before its deadline");
                            let deadline: chrono::DateTime<chrono::Utc> = deadline.get(0);
                            observer.client().query_one("SELECT pg_catalog.pg_sleep_until($1)", &[&deadline]).await.expect("wait for the actual persisted deadline");
                            assert!(observer.client().query_one("SELECT pg_catalog.clock_timestamp()>=$1", &[&deadline]).await.expect("expired before release").get::<_,bool>(0));
                            if failed_rollback { proxy.arm(Fault::RejectedRollback); }
                            blocker.client().batch_execute("COMMIT").await.expect("release insert after expiry");
                            let error = task.await.expect("completion ended").expect_err("expiry refuses the new fact");
                            assert_eq!(error.code(), Some("verification_required"), "{error:?}");
                            if failed_rollback {
                                timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("actual rollback acknowledgement failure");
                            }
                            let facts = observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.replacements), NOT EXISTS(SELECT 1 FROM identity.rejections WHERE purpose='completion'), (SELECT failures FROM identity.confirmations WHERE id=$1), NOT EXISTS(SELECT 1 FROM identity.revocations), EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$2)", &[&message_id, &original.session_id()]).await.expect("independent rollback facts before replay");
                            assert!(facts.get::<_,bool>(0)); assert!(facts.get::<_,bool>(1)); assert_eq!(facts.get::<_,i16>(2), 0); assert!(facts.get::<_,bool>(3)); assert!(facts.get::<_,bool>(4));
                            services.current().read(&original).await.expect("rolled-back account and session remain usable");
                            let error = services.transfer().complete(original, input(operation_id, message_id, &code)).await.expect_err("replay remains expired");
                            assert_eq!(error.code(), Some("verification_required"), "{error:?}");
                            services.ready().await.expect("settled or subsequently resolved rollback keeps admission open");
                            assert!(!observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.completions WHERE family='mobile_completion' AND operation_id=$1)", &[&operation_id]).await.expect("expiry has no fixed success or counted rejection").get::<_,bool>(0));
                            services.drain().await.expect("drained"); context.stop().await.expect("runtime ended");
                        });
                        tokio::select! { biased; _ = sms.request() => panic!("expiry never repeats SMS"), _ = supplier.observe() => panic!("expiry never repeats verification"), () = work => {} }
                        observer.terminate().await.expect("reader ended"); blocker.terminate().await.expect("barrier ended"); sms.stop().await; supplier.stop().await; proxy.stop().await;
                    })).await.expect("bounded completion insert expiry");
            }
        }));
    }

    #[test]
    fn completion_recovers_commit_loss_delayed_commit_and_rollback_for_both_conclusions() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for valid in [true, false] {
                for (fault, delayed, rolled_back) in [(Fault::LostCommit, false, false), (Fault::DelayedCommit, true, false), (Fault::RolledBack, false, true)] {
                    timeout(Duration::from_secs(30), Box::pin(async {
                        let database = Database::initialized(Arc::clone(&server)).await;
                        let mut supplier = Supplier::new().await;
                        let mut sms = fixture::supplier::Supplier::start().await;
                        let proxy = Proxy::start(&server).await;
                        let (context, services) = start_on(authentication::proxied(&database, &proxy), &supplier, &sms).await;
                        let original = actor(&database, &services, &mut supplier).await;
                        let (_, transfer, message_id, code) = prepare(&database, &services, &original, &mut supplier, &mut sms).await;
                        let code = if valid { code } else if code == "000000" { "000001".into() } else { "000000".into() };
                        let observer = database.connections(&database.user).recover().await.expect("independent facts");
                        let operation_id = operation();
                        let work = Box::pin(async {
                            proxy.arm(fault);
                            let service = services.transfer().clone(); let actor = original.clone(); let command = input(operation_id, message_id, &code);
                            let task = tokio::spawn(async move { service.complete(actor, command).await });
                            timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("actual completion commit intercepted");
                            if delayed {
                                timeout(Duration::from_secs(5), async {
                                    loop {
                                        if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_locks WHERE locktype='advisory' AND NOT granted AND objsubid=1 AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()))", &[]).await.expect("recovery wait").get::<_,bool>(0) { break; }
                                        tokio::task::yield_now().await;
                                    }
                                }).await.expect("recovery blocked behind original operation");
                                task.abort(); assert!(task.await.expect_err("cancelled waiter").is_cancelled());
                                assert!(!observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.replacements WHERE id=$1)", &[&transfer.id()]).await.expect("not yet committed").get::<_,bool>(0));
                                proxy.release();
                            } else {
                                let result = task.await.expect("completion work");
                                if rolled_back { assert!(matches!(result, Err(Error::Unavailable(_))), "{result:?}"); }
                                else if valid { result.expect("lost commit recovered"); }
                                else { assert!(matches!(result, Err(Error::Rejected(Rejection::CodeInvalid))), "{result:?}"); }
                            }
                            if rolled_back {
                                assert!(observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.replacements WHERE operation_id=$1) AND NOT EXISTS(SELECT 1 FROM identity.rejections WHERE purpose='completion' AND operation_id=$1) AND NOT EXISTS(SELECT 1 FROM identity.revocations WHERE id=$2) AND (SELECT failures=0 FROM identity.confirmations WHERE id=$3)", &[&operation_id, &original.session_id(), &message_id]).await.expect("rollback has no partial effects").get::<_,bool>(0));
                                let result = services.transfer().complete(original.clone(), input(operation_id, message_id, &code)).await;
                                if valid { result.expect("explicit retry after rollback"); } else { assert!(matches!(result, Err(Error::Rejected(Rejection::CodeInvalid)))); }
                            }
                            timeout(Duration::from_secs(5), async {
                                loop {
                                    if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.replacements WHERE operation_id=$1) OR EXISTS(SELECT 1 FROM identity.rejections WHERE purpose='completion' AND operation_id=$1)", &[&operation_id]).await.expect("original commit before replay").get::<_,bool>(0) { break; }
                                    tokio::task::yield_now().await;
                                }
                            }).await.expect("committed conclusion");
                            let fact = observer.client().query_one("SELECT (SELECT count(*) FROM identity.replacements WHERE operation_id=$1), (SELECT count(*) FROM identity.rejections WHERE purpose='completion' AND operation_id=$1), (SELECT failures FROM identity.confirmations WHERE id=$2), EXISTS(SELECT 1 FROM identity.revocations WHERE id=$3)", &[&operation_id, &message_id, &original.session_id()]).await.expect("independent outcome");
                            assert_eq!(fact.get::<_,i64>(0), i64::from(valid)); assert_eq!(fact.get::<_,i64>(1), i64::from(!valid)); assert_eq!(fact.get::<_,i16>(2), i16::from(!valid)); assert_eq!(fact.get::<_,bool>(3), valid);
                            assert_eq!(services.transfer().complete(original, input(operation_id, message_id, &code)).await.expect_err("replay").code(), Some(if valid { "authentication_invalid" } else { "verification_invalid" }));
                            services.drain().await.expect("worker retained through recovery and drain"); context.stop().await.expect("runtime ended");
                        });
                        tokio::select! { biased; _ = sms.request() => panic!("recovery never repeats SMS"), _ = supplier.observe() => panic!("recovery never repeats verification"), () = work => {} }
                        observer.terminate().await.expect("observer ended"); sms.stop().await; supplier.stop().await; proxy.stop().await;
                    })).await.expect("bounded completion recovery");
                }
            }
        }));
    }
}
