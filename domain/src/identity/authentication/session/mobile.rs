//! Phone-code occupancy belongs to this protocol, not to SMS verification records.

use chrono::{DateTime, TimeDelta, Utc};
use platform::cryptography::Encoded;
use tokio::time::{Instant, timeout_at};
use tokio_postgres::{
    Client, Row,
    types::{FromSql, ToSql},
};
use uuid::Uuid;

use super::{Creation, Outcome, Receipt, Session, database, inconsistent};
use crate::{
    database::{
        transaction::{self, Completion, Known},
        verification,
    },
    history::Read,
    identity::{
        account,
        authentication::{
            self, Command, Error, Execute, Inner, TRANSACTION_BUDGET, registry::Family,
        },
        mini::{self, Code, client},
        mobile::Mobile,
        operation, request, session,
    },
};

pub struct Input {
    operation_id: operation::Id,
    secret: request::Secret,
    code: Code,
}

impl Input {
    pub fn new(operation_id: operation::Id, secret: request::Secret, code: Code) -> Self {
        Self {
            operation_id,
            secret,
            code,
        }
    }
    pub(super) fn secret(&self) -> &request::Secret {
        &self.secret
    }
    pub(super) fn operation_id(&self) -> operation::Id {
        self.operation_id
    }
    pub(super) fn encoded(
        &self,
        origin: &session::Origin,
    ) -> Result<Encoded, platform::cryptography::Error> {
        Encoded::fields(&[
            b"nucleus/identity/request/v1",
            b"POST",
            b"/sessions",
            b"create_session",
            origin.app_id().as_bytes(),
            self.operation_id.as_bytes(),
            b"bearer",
            b"mobile",
            self.code.as_bytes(),
        ])
    }
    fn occupancy(&self, app_id: &mini::AppId) -> Result<Encoded, platform::cryptography::Error> {
        Encoded::fields(&[
            b"nucleus/identity/code/v1",
            b"mini_mobile",
            app_id.as_ref().as_bytes(),
            self.code.as_bytes(),
        ])
    }
}

impl Command for Input {
    fn operation_id(&self) -> operation::Id {
        self.operation_id
    }
    fn family(&self) -> Family {
        Family::CreateSession
    }
}

pub(in crate::identity::authentication) struct Action;
impl Execute<Input> for Action {
    type Output = Receipt;
    async fn execute(self, service: &Inner, input: &Input) -> Result<Receipt, Error> {
        let admission = admit(service, input).await?;
        let claim = match admission {
            Admission::Call(claim) => claim,
            Admission::Original(outcome) => return outcome.deliver(),
            Admission::Rejected(reason) => return Err(reason.error()),
        };
        let now = service.context.read_time().await?;
        if now >= claim.deadline {
            exhaust(service, input, Reason::Verification).await?;
            return Err(Error::Verification);
        }
        let remaining = (claim.deadline - now).to_std().map_err(invalid_remaining)?;
        let started = Instant::now();
        let result = supplier(service)?.exchange(&input.code, remaining).await;
        match result {
            Ok(client::Outcome::Mobile(mobile)) => finalize(service, input, &claim, &mobile).await,
            Ok(client::Outcome::Invalid) => {
                exhaust(service, input, Reason::Invalid).await?;
                Err(Error::MobileCodeInvalid)
            }
            Ok(client::Outcome::Unsupported) => {
                exhaust(service, input, Reason::Unsupported).await?;
                Err(Error::MobileUnsupported)
            }
            Err(cause) => {
                let cause = Error::Mini {
                    cause,
                    elapsed: started.elapsed(),
                };
                match exhaust(service, input, Reason::Verification).await {
                    Ok(()) => Err(cause),
                    Err(recovery) => Err(Error::Unknown {
                        cause: Box::new(cause),
                        recovery: Box::new(recovery),
                    }),
                }
            }
        }
    }
}

fn supplier(service: &Inner) -> Result<&client::Client, Error> {
    service.mini.as_ref().ok_or(Error::MiniUnavailable)
}
pub(super) fn origin(service: &Inner) -> Result<session::Origin, Error> {
    let origin = session::Origin::MiniMobile(supplier(service)?.app_id().clone());
    if !service.settings.session().issuance().accepts(&origin) {
        return Err(Error::MiniUnavailable);
    }
    Ok(origin)
}

#[derive(Clone, Copy)]
enum Reason {
    Invalid,
    Unsupported,
    Verification,
}
impl Reason {
    fn text(self) -> &'static str {
        match self {
            Self::Invalid => "authentication_invalid",
            Self::Unsupported => "mobile_unsupported",
            Self::Verification => "verification_required",
        }
    }
    fn error(self) -> Error {
        match self {
            Self::Invalid => Error::MobileCodeInvalid,
            Self::Unsupported => Error::MobileUnsupported,
            Self::Verification => Error::Verification,
        }
    }
    fn restore(text: &str) -> Result<Self, Error> {
        match text {
            "authentication_invalid" => Ok(Self::Invalid),
            "mobile_unsupported" => Ok(Self::Unsupported),
            "verification_required" => Ok(Self::Verification),
            _ => Err(inconsistent("exchange has a known terminal reason")),
        }
    }
}

enum State {
    Claimed,
    Resolved,
    Exhausted(Reason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, FromSql, ToSql)]
#[postgres(transparent)]
pub(super) struct Id(Uuid);

struct Exchange {
    id: Id,
    worker_id: Uuid,
    app_id: mini::AppId,
    proof: request::Proof,
    hash: [u8; 32],
    key_id: crate::keys::Id,
    lookup_key_id: crate::keys::Id,
    claimed_at: DateTime<Utc>,
    deadline: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    state: State,
}
impl TryFrom<Row> for Exchange {
    type Error = Error;
    fn try_from(row: Row) -> Result<Self, Error> {
        let state = match row.try_get::<_, &str>("status")? {
            "claimed" => State::Claimed,
            "resolved" => State::Resolved,
            "exhausted" => State::Exhausted(Reason::restore(row.try_get("reason")?)?),
            _ => return Err(inconsistent("exchange has a known state")),
        };
        let hash: Vec<u8> = row.try_get("hash")?;
        Ok(Self {
            id: row.try_get("id")?,
            worker_id: row.try_get("worker_id")?,
            app_id: row
                .try_get::<_, &str>("app_id")?
                .parse()
                .map_err(invalid_app)?,
            proof: row.try_get("proof")?,
            hash: hash.try_into().map_err(invalid_hash)?,
            key_id: row.try_get("key_id")?,
            lookup_key_id: row.try_get("lookup_key_id")?,
            claimed_at: row.try_get("claimed_at")?,
            deadline: row.try_get("deadline")?,
            finished_at: row.try_get("finished_at")?,
            state,
        })
    }
}
fn invalid_app(_: crate::identity::website::Error) -> Error {
    inconsistent("exchange application identity is valid")
}
fn invalid_hash(_: Vec<u8>) -> Error {
    inconsistent("exchange input digest has 32 bytes")
}
fn invalid_remaining(_: chrono::OutOfRangeError) -> Error {
    Error::Verification
}

impl Exchange {
    fn verify(&self, service: &Inner, input: &Input) -> Result<(), Error> {
        if self.proof != input.secret.proof() {
            return Err(Error::Proof);
        }
        let origin = origin(service)?;
        if origin.app_id() != self.app_id.as_ref()
            || !service
                .keys
                .input
                .fetch(self.key_id)?
                .verify(&input.encoded(&origin)?, &self.hash)
        {
            return Err(Error::Mismatch);
        }
        service.keys.challenge.fetch(self.lookup_key_id)?;
        Ok(())
    }
    fn validate(&self, now: DateTime<Utc>) -> Result<(), Error> {
        if self.claimed_at > now
            || self.claimed_at.checked_add_signed(TimeDelta::seconds(30)) != Some(self.deadline)
            || self
                .finished_at
                .is_some_and(|finished| finished < self.claimed_at || finished > now)
        {
            return Err(inconsistent("exchange claim has a valid trusted deadline"));
        }
        Ok(())
    }
}

async fn find(client: &Client, operation: operation::Id) -> Result<Option<Exchange>, Error> {
    client.query_opt("SELECT id, worker_id, app_id, proof, hash, key_id, lookup_key_id, claimed_at, deadline, finished_at, status, reason FROM identity.exchanges WHERE operation_id=$1", &[&operation]).await?.map(Exchange::try_from).transpose()
}

pub(super) async fn check_message_at(client: &Client, input: &super::Input) -> Result<(), Error> {
    if let Some(exchange) = find(client, input.operation_id).await? {
        if exchange.proof != input.secret.proof() {
            return Err(Error::Proof);
        }
        return Err(Error::Mismatch);
    }
    Ok(())
}

struct Claim {
    id: Id,
    worker_id: Uuid,
    deadline: DateTime<Utc>,
}
enum Admission {
    Call(Claim),
    Original(Outcome),
    Rejected(Reason),
}
enum Recovery {
    Original(Admission),
    Refused(Error),
}

async fn original(
    service: &Inner,
    client: &Client,
    input: &Input,
    exchange: &Exchange,
) -> Result<Admission, Error> {
    match original_once(service, client, input, exchange).await {
        Err(Error::History(crate::history::Error::Changed)) => {
            // An account transition can advance the anchor between discovery and its lock.
            client
                .batch_execute("ROLLBACK")
                .await
                .map_err(|cause| crate::history::Error::Retry(cause.into()))?;
            authentication::transaction::begin_for(
                &service.context,
                client,
                b"authentication/operation",
                input.operation_id,
            )
            .await?;
            let exchange = find(client, input.operation_id)
                .await?
                .ok_or(Error::Verification)?;
            original_once(service, client, input, &exchange).await
        }
        result => result,
    }
}

async fn original_once(
    service: &Inner,
    client: &Client,
    input: &Input,
    exchange: &Exchange,
) -> Result<Admission, Error> {
    exchange.verify(service, input)?;
    exchange.validate(service.now(client).await?)?;
    match exchange.state {
        State::Resolved => {
            let now = service.now(client).await?;
            let authentication =
                match database::find_by_operation(client, input.operation_id).await? {
                    Some(authentication) => authentication,
                    None if exchange
                        .finished_at
                        .and_then(|finished| finished.checked_add_signed(TimeDelta::hours(24)))
                        .is_some_and(|until| until <= now) =>
                    {
                        return Err(Error::Verification);
                    }
                    None => {
                        return Err(inconsistent(
                            "resolved exchange retains its authentication during recovery",
                        ));
                    }
                };
            authentication.verify_mobile(service, input, &origin(service)?)?;
            Ok(Admission::Original(
                authentication.deliver(service, client).await?,
            ))
        }
        State::Exhausted(reason) => Ok(Admission::Rejected(reason)),
        State::Claimed => {
            // The shared Work permit has ended; a later process has also completed its startup barrier.
            // Recovery never obtains the original worker's permission to call the supplier.
            conclude(
                client,
                input.operation_id,
                Reason::Verification,
                service.now(client).await?,
            )
            .await?;
            Ok(Admission::Rejected(Reason::Verification))
        }
    }
}

async fn admit(service: &Inner, input: &Input) -> Result<Admission, Error> {
    let worker_id = Uuid::new_v4();
    let lease = service.context.connections().lease().await?;
    let deadline = Instant::now() + TRANSACTION_BUDGET;
    let result = timeout_at(deadline, async {
        authentication::transaction::begin_for(&service.context, lease.client(), b"authentication/operation", input.operation_id).await?;
        let client = lease.client();
        if let Some(exchange) = find(client, input.operation_id).await? {
            let changed = matches!(exchange.state, State::Claimed);
            let value = original(service, client, input, &exchange).await?;
            return Ok(if changed { transaction::Outcome::Pending(value) } else { transaction::Outcome::Known(value) });
        }
        if let Some(authentication) = database::find_by_operation(client, input.operation_id).await? {
            authentication.verify_mobile(service, input, &origin(service)?)?;
            return Err(inconsistent("mobile authentication retains its exchange"));
        }
        let app_id = supplier(service)?.app_id();
        let occupancy = input.occupancy(app_id)?;
        authentication::transaction::object(client, b"identity/mini-mobile/code", occupancy.as_ref()).await?;
        for key in service.keys.challenge.all() {
            let hash = key.phone_code(&occupancy);
            if client.query_opt("SELECT id FROM identity.exchanges WHERE lookup_key_id=$1 AND code_hash=$2", &[&key.id(), &&hash.as_ref()[..]]).await?.is_some() { return Err(Error::Verification); }
        }
        let now = service.now(client).await?;
        let until = now.checked_add_signed(TimeDelta::seconds(30)).ok_or(inconsistent("exchange deadline fits trusted time"))?;
        let id = Id(Uuid::new_v4());
        let key = service.keys.input.active();
        let hash = key.sign(&input.encoded(&origin(service)?)?);
        let lookup = service.keys.challenge.active();
        let code_hash = lookup.phone_code(&occupancy);
        client.execute("INSERT INTO identity.exchanges (id, operation_id, app_id, proof, hash, key_id, code_hash, lookup_key_id, worker_id, claimed_at, deadline, status) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,'claimed')", &[&id, &input.operation_id, &app_id.as_ref(), &input.secret.proof(), &&hash.as_ref()[..], &key.id(), &&code_hash.as_ref()[..], &lookup.id(), &worker_id, &now, &until]).await?;
        Ok(transaction::Outcome::Pending(Admission::Call(Claim { id, worker_id, deadline: until })))
    }).await.map_err(Error::from).and_then(std::convert::identity);
    let value = match result {
        Ok(value) => value,
        Err(cause) => return Err(authentication::transaction::rollback(lease, cause).await),
    };
    let (completion, cleanup) =
        transaction::commit(value, deadline, lease.client().batch_execute("COMMIT"))
            .await
            .recycle(lease)
            .await;
    match completion {
        Completion::Known(Known { value, end }) => {
            if end.is_err() || cleanup.is_err() {
                tracing::error!(stage="mini_claim_completion", commit=?end, cleanup=?cleanup);
            }
            Ok(value)
        }
        Completion::Pending { value, error } => {
            let cause = Error::from(error).combine(cleanup);
            let Admission::Call(claim) = value else {
                return match readback(service, input, &mut None).await {
                    Ok(Recovery::Original(Admission::Call(_))) => Err(Error::NotCommitted {
                        cause: Box::new(cause),
                    }),
                    Ok(Recovery::Original(value)) => Ok(value),
                    Ok(Recovery::Refused(refusal)) => Err(Error::SessionRefused {
                        cause: Box::new(cause),
                        refusal: Box::new(refusal),
                    }),
                    Err(recovery) => Err(Error::Unknown {
                        cause: Box::new(cause),
                        recovery: Box::new(recovery),
                    }),
                };
            };
            match confirm_claim(service, input, &claim).await {
                Ok(true) => Ok(Admission::Call(claim)),
                Ok(false) => Err(Error::NotCommitted {
                    cause: Box::new(cause),
                }),
                Err(recovery) => Err(Error::Unknown {
                    cause: Box::new(cause),
                    recovery: Box::new(recovery),
                }),
            }
        }
    }
}

async fn confirm_claim(service: &Inner, input: &Input, claim: &Claim) -> Result<bool, Error> {
    let session = service.context.connections().recover().await?;
    let deadline = Instant::now() + TRANSACTION_BUDGET;
    let result = timeout_at(deadline, async {
        authentication::transaction::begin_for(
            &service.context,
            session.client(),
            b"authentication/operation",
            input.operation_id,
        )
        .await?;
        let Some(exchange) = find(session.client(), input.operation_id).await? else {
            return Ok(false);
        };
        exchange.verify(service, input)?;
        let now = service.now(session.client()).await?;
        exchange.validate(now)?;
        if exchange.id != claim.id || exchange.worker_id != claim.worker_id {
            return Err(inconsistent("claim readback preserves its original worker"));
        }
        if !matches!(exchange.state, State::Claimed) || now >= exchange.deadline {
            return Err(Error::Verification);
        }
        Ok(true)
    })
    .await
    .map_err(Error::from)
    .and_then(std::convert::identity);
    let result = match result {
        Ok(value) => {
            Ok(
                transaction::confirm(value, deadline, session.client().batch_execute("COMMIT"))
                    .await,
            )
        }
        Err(cause) => Err(cause),
    };
    match result {
        Ok(value) => {
            let (value, cleanup) = value.terminate(session).await;
            if value.end.is_err() || cleanup.is_err() {
                tracing::error!(stage="mini_claim_readback", commit=?value.end, cleanup=?cleanup);
            }
            Ok(value.value)
        }
        Err(cause) => Err(cause.combine(session.terminate().await)),
    }
}

async fn conclude(
    client: &Client,
    operation: operation::Id,
    reason: Reason,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    if client.execute("UPDATE identity.exchanges SET status='exhausted', reason=$2, finished_at=$3, transaction_id=pg_catalog.pg_current_xact_id() WHERE operation_id=$1 AND status='claimed'", &[&operation, &reason.text(), &now]).await? != 1 {
        return Err(inconsistent("exchange concludes only its original claim"));
    }
    Ok(())
}

async fn exhaust(service: &Inner, input: &Input, reason: Reason) -> Result<(), Error> {
    let lease = service.context.connections().lease().await?;
    let deadline = Instant::now() + TRANSACTION_BUDGET;
    let result = timeout_at(deadline, async {
        authentication::transaction::begin_for(
            &service.context,
            lease.client(),
            b"authentication/operation",
            input.operation_id,
        )
        .await?;
        let exchange = find(lease.client(), input.operation_id)
            .await?
            .ok_or(inconsistent("the admitted exchange remains retained"))?;
        exchange.verify(service, input)?;
        conclude(
            lease.client(),
            input.operation_id,
            reason,
            service.now(lease.client()).await?,
        )
        .await
    })
    .await
    .map_err(Error::from)
    .and_then(std::convert::identity);
    if let Err(cause) = result {
        return Err(authentication::transaction::rollback(lease, cause).await);
    }
    let (completion, cleanup) = transaction::commit(
        transaction::Outcome::Pending(()),
        deadline,
        lease.client().batch_execute("COMMIT"),
    )
    .await
    .recycle(lease)
    .await;
    match completion {
        Completion::Known(_) => Ok(()),
        Completion::Pending { error, .. } => {
            let cause = Error::from(error).combine(cleanup);
            match readback(service, input, &mut None).await {
                Ok(Recovery::Original(Admission::Rejected(actual)))
                    if actual.text() == reason.text() =>
                {
                    Ok(())
                }
                Ok(Recovery::Refused(refusal)) => Err(Error::SessionRefused {
                    cause: Box::new(cause),
                    refusal: Box::new(refusal),
                }),
                Ok(_) => Err(cause),
                Err(recovery) => Err(Error::Unknown {
                    cause: Box::new(cause),
                    recovery: Box::new(recovery),
                }),
            }
        }
    }
}

async fn finalize(
    service: &Inner,
    input: &Input,
    claim: &Claim,
    mobile: &Mobile,
) -> Result<Receipt, Error> {
    for _ in 0..3 {
        let lease = service.context.connections().lease().await?;
        let deadline = Instant::now() + TRANSACTION_BUDGET;
        let mut creation = None;
        let result = timeout_at(
            deadline,
            write_result(service, lease.client(), input, claim, mobile, &mut creation),
        )
        .await
        .map_err(Error::from)
        .and_then(std::convert::identity);
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(cause) => {
                let cause = authentication::transaction::rollback(lease, cause).await;
                return match exhaust(service, input, Reason::Verification).await {
                    Ok(()) => Err(cause),
                    Err(recovery) => Err(Error::Unknown {
                        cause: Box::new(cause),
                        recovery: Box::new(recovery),
                    }),
                };
            }
        };
        let (completion, cleanup) = transaction::commit(
            transaction::Outcome::Pending(outcome),
            deadline,
            lease.client().batch_execute("COMMIT"),
        )
        .await
        .recycle(lease)
        .await;
        match completion {
            Completion::Known(Known { value, end }) => {
                if let Some(creation) = creation {
                    creation.pending.committed()?;
                }
                if end.is_err() || cleanup.is_err() {
                    tracing::error!(stage="mini_result_completion", commit=?end, cleanup=?cleanup);
                }
                return value.deliver();
            }
            Completion::Pending { error, .. } => {
                let cause = Error::from(error).combine(cleanup);
                match readback(service, input, &mut creation).await {
                    Ok(Recovery::Original(Admission::Original(value))) => return value.deliver(),
                    Ok(Recovery::Original(Admission::Rejected(reason))) => {
                        return Err(reason.error());
                    }
                    Ok(Recovery::Original(Admission::Call(_))) => continue,
                    Ok(Recovery::Refused(refusal)) => {
                        return Err(Error::SessionRefused {
                            cause: Box::new(cause),
                            refusal: Box::new(refusal),
                        });
                    }
                    Err(recovery) => {
                        if let Some(creation) = creation {
                            creation.pending.uncertain();
                        }
                        return Err(Error::Unknown {
                            cause: Box::new(cause),
                            recovery: Box::new(recovery),
                        });
                    }
                }
            }
        }
    }
    exhaust(service, input, Reason::Verification).await?;
    Err(Error::Verification)
}

async fn write_result(
    service: &Inner,
    client: &Client,
    input: &Input,
    claim: &Claim,
    mobile: &Mobile,
    creation: &mut Option<Creation>,
) -> Result<Outcome, Error> {
    for _ in 0..3 {
        authentication::transaction::begin_for(
            &service.context,
            client,
            b"authentication/operation",
            input.operation_id,
        )
        .await?;
        let exchange = find(client, input.operation_id)
            .await?
            .ok_or(inconsistent("the admitted exchange is retained"))?;
        exchange.verify(service, input)?;
        if exchange.id != claim.id
            || exchange.worker_id != claim.worker_id
            || !matches!(exchange.state, State::Claimed)
        {
            return Err(Error::Verification);
        }
        let hash = service.keys.mobile.hash(mobile)?;
        let account = account::state::discover_mobile(client, &hash).await?;
        if let Some(account) = account {
            database::lock_account(client, account).await?;
        }
        authentication::transaction::object(client, b"identity/mobile", mobile.as_bytes()).await?;
        if account::state::discover_mobile(client, &hash).await? != account {
            if let Some(id) = account
                && crate::history::find::<account::state::State>(
                    client,
                    service.app.keys(),
                    service.app.anchors(),
                    Read::Locked,
                    id,
                    service.now(client).await?,
                )
                .await?
                .is_some_and(|state| state.current().is_some())
            {
                return Err(Error::Verification);
            }
            client.batch_execute("ROLLBACK").await?;
            continue;
        }
        let now = service.now(client).await?;
        exchange.validate(now)?;
        if now >= claim.deadline {
            conclude(client, input.operation_id, Reason::Verification, now).await?;
            return Ok(Outcome::Verification);
        }
        let current = account::state::located(
            client,
            service.app.keys(),
            service.app.anchors(),
            Read::Locked,
            account::state::Location::Mobile(&hash),
            now,
        )
        .await?;
        let id = authentication::Id::new();
        let status = match current.as_ref() {
            Some(account)
                if account
                    .current()
                    .is_some_and(|current| current.suspension_id().is_some()) =>
            {
                "suspended"
            }
            Some(_) => "authenticated",
            None => "registrable",
        };
        let origin = origin(service)?;
        client.execute("UPDATE identity.exchanges SET status='resolved', mobile_hash=$2, finished_at=$3, transaction_id=pg_catalog.pg_current_xact_id() WHERE id=$1 AND status='claimed'", &[&claim.id, &hash, &now]).await?;
        let key = service.keys.input.active();
        let hash = key.sign(&input.encoded(&origin)?);
        client.execute("INSERT INTO identity.authentications (id, operation_id, proof, hash, key_id, purpose, app_id, transport, exchange_id, status, finished_at) VALUES ($1,$2,$3,$4,$5,'create_session',$6,'bearer',$7,$8,$9)", &[&id, &input.operation_id, &input.secret.proof(), &&hash.as_ref()[..], &key.id(), &origin.app_id(), &claim.id, &status, &now]).await?;
        return match current {
            Some(_) if status == "suspended" => Ok(Outcome::Suspended),
            Some(account) => {
                let (id, token, expires_at, pending) = session::create(
                    client,
                    &service.app,
                    &service.keys.credential,
                    service.settings.session().issuance(),
                    session::Input {
                        source: session::Source::Authentication(id),
                        origin,
                        account_id: account.id(),
                        operation_id: input.operation_id,
                        created_at: now,
                    },
                )
                .await?;
                *creation = Some(Creation {
                    id,
                    created_at: now,
                    pending,
                });
                Ok(Outcome::Credential(Receipt::Session(Session {
                    id,
                    token,
                    expires_at,
                })))
            }
            None => Ok(Outcome::Credential(
                database::create_registration(service, client, mobile, id, now).await?,
            )),
        };
    }
    Err(Error::Busy)
}

async fn readback(
    service: &Inner,
    input: &Input,
    creation: &mut Option<Creation>,
) -> Result<Recovery, Error> {
    let session = service.context.connections().recover().await?;
    let deadline = Instant::now() + TRANSACTION_BUDGET;
    let result = timeout_at(deadline, async {
        authentication::transaction::begin_for(
            &service.context,
            session.client(),
            b"authentication/operation",
            input.operation_id,
        )
        .await?;
        let exchange = find(session.client(), input.operation_id)
            .await?
            .ok_or(inconsistent("exchange survives result recovery"))?;
        exchange.verify(service, input)?;
        if let Some(candidate) = creation.as_ref() {
            let committed = match candidate.pending.resolution()? {
                Some(committed) => committed,
                None => {
                    session::state::issued(
                        session.client(),
                        service.app.keys(),
                        service.app.anchors(),
                        candidate.id,
                        input.operation_id,
                        candidate.created_at,
                    )
                    .await?
                }
            };
            if let Some(candidate) = creation.take() {
                if committed {
                    candidate.pending.committed()?;
                } else {
                    candidate.pending.rolled_back()?;
                }
            }
        }
        if matches!(exchange.state, State::Claimed) {
            return Ok(Admission::Call(Claim {
                id: exchange.id,
                worker_id: exchange.worker_id,
                deadline: exchange.deadline,
            }));
        }
        original(service, session.client(), input, &exchange).await
    })
    .await
    .map_err(Error::from)
    .and_then(std::convert::identity);
    match result {
        Ok(value) => {
            let (value, cleanup) =
                transaction::confirm(value, deadline, session.client().batch_execute("COMMIT"))
                    .await
                    .terminate(session)
                    .await;
            if value.end.is_err() || cleanup.is_err() {
                tracing::error!(stage="mini_result_readback", commit=?value.end, cleanup=?cleanup);
            }
            Ok(Recovery::Original(value.value))
        }
        Err(cause) if cause.rejection() => {
            let (completion, cleanup) = transaction::rollback(
                cause,
                Instant::now() + TRANSACTION_BUDGET,
                session.client().batch_execute("ROLLBACK"),
            )
            .await
            .terminate(session)
            .await;
            Ok(Recovery::Refused(
                authentication::transaction::refusal(completion).combine(cleanup),
            ))
        }
        Err(cause) => Err(cause.combine(session.terminate().await)),
    }
}

pub(super) async fn verify_all(
    client: &Client,
    settings: &session::settings::Settings,
    keys: &crate::identity::keys::Keys,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let mut cursor: Option<operation::Id> = None;
    loop {
        let complete = verification::Stage::ExchangesBatch
            .batch(TRANSACTION_BUDGET, async {
                let rows = client.query("SELECT operation_id FROM identity.exchanges WHERE ($1::uuid IS NULL OR operation_id>$1) ORDER BY operation_id LIMIT 128", &[&cursor]).await?;
                let complete = rows.len() < 128;
                for row in rows {
                    let operation: operation::Id = row.try_get(0)?;
                    let exchange = find(client, operation)
                        .await?
                        .ok_or(inconsistent("startup exchange remains in its snapshot"))?;
                    exchange.validate(now)?;
                    if !settings.accepts(&session::Origin::MiniMobile(exchange.app_id)) {
                        return Err(inconsistent(
                            "retained mini-program exchange requires its configured application identity",
                        ));
                    }
                    keys.input.fetch(exchange.key_id)?;
                    keys.challenge.fetch(exchange.lookup_key_id)?;
                    cursor = Some(operation);
                }
                Ok::<_, Error>(complete)
            })
            .await??;
        if complete {
            break;
        }
    }
    let valid: bool = verification::Stage::ExchangeRelations.query(TRANSACTION_BUDGET, client.query_one("SELECT NOT EXISTS (SELECT 1 FROM identity.exchanges exchange LEFT JOIN identity.authentications authentication ON authentication.exchange_id=exchange.id WHERE (exchange.status='resolved' AND authentication.id IS NULL AND exchange.finished_at + interval '24 hours' > $1) OR (authentication.id IS NOT NULL AND (exchange.status<>'resolved' OR ROW(exchange.operation_id,exchange.app_id,exchange.proof,exchange.hash,exchange.key_id,exchange.finished_at) IS DISTINCT FROM ROW(authentication.operation_id,authentication.app_id,authentication.proof,authentication.hash,authentication.key_id,authentication.finished_at))))", &[&now])).await??.try_get(0)?;
    if !valid {
        return Err(inconsistent(
            "exchange and retained authentication preserve their original conclusion",
        ));
    }
    Ok(())
}

pub(super) async fn retention(
    client: &Client,
    operation: operation::Id,
    now: DateTime<Utc>,
) -> Result<Option<Id>, Error> {
    let Some(exchange) = find(client, operation).await? else {
        return Ok(None);
    };
    exchange.validate(now)?;
    if matches!(exchange.state, State::Claimed) {
        if now >= exchange.deadline {
            conclude(client, operation, Reason::Verification, now).await?;
        }
        return Ok(None);
    }
    Ok(exchange
        .finished_at
        .and_then(|finished| finished.checked_add_signed(TimeDelta::hours(24)))
        .filter(|until| *until <= now)
        .map(|_| exchange.id))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests;
