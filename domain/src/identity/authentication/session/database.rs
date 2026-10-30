//! Fixed queries restore facts; proof and current-qualification checks precede delivery.

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, Row};

use crate::{
    database::{sealed::Sealed, verification},
    history::Read,
    identity::{
        account,
        authentication::{
            self, Error, Inner, TRANSACTION_BUDGET,
            session::{Input, Receipt, Registration, Session, inconsistent},
            transaction,
        },
        challenge, credential, keys, message,
        mobile::envelope::{Binding, Envelope},
        operation, registration, request, session, website,
    },
};

pub(super) struct Authentication {
    id: authentication::Id,
    proof: request::Proof,
    hash: [u8; 32],
    key_id: crate::keys::Id,
    app_id: website::AppId,
    status: Status,
    finished_at: DateTime<Utc>,
}

pub(super) enum Status {
    Invalid,
    Suspended,
    Authenticated,
    Registrable,
}

impl TryFrom<Row> for Authentication {
    type Error = Error;
    fn try_from(row: Row) -> Result<Self, Error> {
        let hash: Vec<u8> = row.try_get("hash")?;
        if row.try_get::<_, String>("purpose")? != "create_session"
            || row.try_get::<_, String>("transport")? != "cookie"
        {
            return Err(inconsistent(
                "retained authentication must have the website session source",
            ));
        }
        let status = match row.try_get::<_, &str>("status")? {
            "invalid" => Status::Invalid,
            "suspended" => Status::Suspended,
            "authenticated" => Status::Authenticated,
            "registrable" => Status::Registrable,
            _ => {
                return Err(inconsistent(
                    "authentication must have a terminal conclusion",
                ));
            }
        };
        Ok(Self {
            id: row.try_get("id")?,
            proof: row.try_get("proof")?,
            hash: hash.try_into().map_err(invalid_hash)?,
            key_id: row.try_get("key_id")?,
            app_id: row.try_get("app_id")?,
            status,
            finished_at: row.try_get("finished_at")?,
        })
    }
}

fn invalid_hash(_: Vec<u8>) -> Error {
    inconsistent("stored credential and input digests must contain 32 bytes")
}

impl Authentication {
    pub(super) fn verify(&self, service: &Inner, input: &Input) -> Result<(), Error> {
        if self.proof != input.secret.proof() {
            return Err(Error::Proof);
        }
        let key = service.keys.input.fetch(self.key_id)?;
        if self.app_id != *service.settings.session().app_id()
            || !key.verify(
                &input.encoded(service.settings.session().app_id())?,
                &self.hash,
            )
        {
            return Err(Error::Mismatch);
        }
        Ok(())
    }

    pub(super) async fn deliver(
        &self,
        service: &Inner,
        client: &Client,
    ) -> Result<authentication::session::Outcome, Error> {
        match self.status {
            Status::Invalid => Ok(authentication::session::Outcome::Invalid),
            Status::Suspended => Ok(authentication::session::Outcome::Suspended),
            Status::Authenticated => {
                let view = service.app.anchors().view().map_err(session::Error::from)?;
                let before = service.now(client).await?;
                let Some(session) = session::find_by_authentication(
                    client,
                    &service.app,
                    Read::Snapshot(view),
                    self.id,
                    before,
                )
                .await?
                else {
                    return Err(Error::Verification);
                };
                lock_account(client, session.account_id()).await?;
                let now = service.now(client).await?;
                self.validate(
                    &service.keys.input,
                    service.settings.session().app_id(),
                    now,
                )?;
                let Some(session) = session::find_by_authentication(
                    client,
                    &service.app,
                    Read::Shared,
                    self.id,
                    now,
                )
                .await?
                else {
                    return Err(Error::Verification);
                };
                let token = session::handoff::deliver(
                    client,
                    &session,
                    &service.keys.credential,
                    service.settings.session().app_id(),
                    now,
                )
                .await?;
                if !session.available(client, &service.app, now).await? {
                    return Err(Error::Verification);
                }
                Ok(authentication::session::Outcome::Credential(
                    Receipt::Session(Session {
                        id: session.id(),
                        token,
                        expires_at: session.expires_at(),
                    }),
                ))
            }
            Status::Registrable => {
                let Some(registration) =
                    find_registration_by_authentication(client, self.id).await?
                else {
                    return Err(Error::Verification);
                };
                let mobile = registration.mobile.open(
                    &service.keys.encryption,
                    &service.keys.mobile,
                    Binding::Registration(registration.id),
                )?;
                transaction::object(client, b"identity/mobile", mobile.as_bytes()).await?;
                let now = service.now(client).await?;
                self.validate(
                    &service.keys.input,
                    service.settings.session().app_id(),
                    now,
                )?;
                let Some(registration) =
                    find_registration_by_authentication(client, self.id).await?
                else {
                    return Err(Error::Verification);
                };
                let token = registration.open(&service.keys, self)?;
                if registration.expires_at <= now
                    || account::state::located(
                        client,
                        service.app.keys(),
                        service.app.anchors(),
                        Read::Snapshot(service.app.anchors().view()?),
                        account::state::Location::Mobile(&service.keys.mobile.hash(&mobile)?),
                        now,
                    )
                    .await?
                    .is_some()
                {
                    return Err(Error::Verification);
                }
                Ok(authentication::session::Outcome::Credential(
                    Receipt::Registration(Registration {
                        id: registration.id,
                        token: registration::Token(token),
                        expires_at: registration.expires_at,
                    }),
                ))
            }
        }
    }

    pub(super) fn validate(
        &self,
        keys: &keys::request::Keys,
        app_id: &website::AppId,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        keys.fetch(self.key_id)?;
        if self.finished_at > now || self.app_id != *app_id {
            return Err(inconsistent(
                "authentication event and application must match the running website",
            ));
        }
        Ok(())
    }
}

pub(super) async fn find_by_operation(
    client: &Client,
    operation_id: operation::Id,
) -> Result<Option<Authentication>, Error> {
    client.query_opt("SELECT id, proof, hash, key_id, purpose, app_id, transport, status, finished_at FROM identity.authentications WHERE operation_id = $1", &[&operation_id]).await?.map(Authentication::try_from).transpose()
}

pub(super) async fn lock_account(client: &Client, id: account::Id) -> Result<(), Error> {
    client
        .query_one("SELECT identity.lock_account($1)", &[&id])
        .await?;
    Ok(())
}

pub(super) async fn lock_check(client: &Client, id: message::Id) -> Result<(), Error> {
    client
        .query_opt(
            "SELECT id FROM identity.checks WHERE id = $1 FOR UPDATE",
            &[&id],
        )
        .await?;
    Ok(())
}

pub(super) struct Verification {
    hash: message::proof::Proof,
    key_id: crate::keys::Id,
}
impl Verification {
    pub(super) fn matches(&self, keys: &keys::message::Keys, input: &Input) -> Result<bool, Error> {
        Ok(keys
            .fetch(self.key_id)?
            .verify(input.message_id, &input.code, &self.hash)?)
    }
}

pub(super) async fn verification(
    service: &Inner,
    client: &Client,
    input: &Input,
    now: DateTime<Utc>,
) -> Result<Verification, Error> {
    let option = client.query_opt("SELECT verification.hash, verification.key_id, verification.failures, verification.authentication_id IS NOT NULL AS consumed, verification.expires_at, message.created_at, challenge.id AS challenge_id, challenge.purpose, challenge.mobile, EXISTS (SELECT 1 FROM identity.messages later JOIN identity.challenges original ON original.id = later.challenge_id WHERE original.purpose = 'authentication' AND (original.mobile).hash = (challenge.mobile).hash AND later.ordinal > message.ordinal) AS superseded FROM identity.checks verification JOIN identity.messages message ON message.id = verification.id JOIN identity.challenges challenge ON challenge.id = message.challenge_id WHERE verification.id = $1", &[&input.message_id]).await?;
    let Some(row) = option else {
        return Err(Error::Verification);
    };
    let mobile: Envelope = row.try_get("mobile")?;
    let mobile = mobile.open(
        &service.keys.encryption,
        &service.keys.mobile,
        Binding::Challenge(row.try_get("challenge_id")?),
    )?;
    let created_at: DateTime<Utc> = row.try_get("created_at")?;
    let expires_at: DateTime<Utc> = row.try_get("expires_at")?;
    let failures: i16 = row.try_get("failures")?;
    if created_at > now || expires_at <= created_at || !(0..=5).contains(&failures) {
        return Err(inconsistent(
            "code facts must have valid times and failure count",
        ));
    }
    if mobile.as_bytes() != input.mobile.as_bytes()
        || row.try_get::<_, challenge::Purpose>("purpose")? != challenge::Purpose::Authentication
        || row.try_get::<_, bool>("superseded")?
        || now >= expires_at
        || failures == 5
        || row.try_get::<_, bool>("consumed")?
    {
        return Err(Error::Verification);
    }
    Ok(Verification {
        hash: row.try_get("hash")?,
        key_id: row.try_get("key_id")?,
    })
}

pub(super) async fn insert(
    service: &Inner,
    client: &Client,
    input: &Input,
    id: authentication::Id,
    status: Status,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let key = service.keys.input.active();
    let hash = key.sign(&input.encoded(service.settings.session().app_id())?);
    let status = match status {
        Status::Invalid => "invalid",
        Status::Suspended => "suspended",
        Status::Authenticated => "authenticated",
        Status::Registrable => "registrable",
    };
    client.execute("INSERT INTO identity.authentications (id, operation_id, proof, hash, key_id, purpose, app_id, transport, message_id, status, finished_at) VALUES ($1,$2,$3,$4,$5,'create_session',$6,'cookie',$7,$8,$9)", &[&id, &input.operation_id, &input.secret.proof(), &hash.as_ref().as_slice(), &key.id(), service.settings.session().app_id(), &input.message_id, &status, &now]).await?;
    Ok(())
}

pub(super) async fn consume(
    client: &Client,
    message_id: message::Id,
    id: authentication::Id,
) -> Result<(), Error> {
    if client
        .execute(
            "UPDATE identity.checks SET authentication_id = $2 WHERE id = $1",
            &[&message_id, &id],
        )
        .await?
        != 1
    {
        return Err(inconsistent("consumption requires the locked code"));
    }
    Ok(())
}

pub(super) async fn create_registration(
    service: &Inner,
    client: &Client,
    input: &Input,
    authentication_id: authentication::Id,
    now: DateTime<Utc>,
) -> Result<Receipt, Error> {
    let id = registration::Id::new();
    let token = credential::Secret::generate()?;
    let sealed = token.seal(
        &service.keys.credential,
        credential::Binding::Registration(id),
    )?;
    let mobile = Envelope::seal(
        &service.keys.encryption,
        &service.keys.mobile,
        Binding::Registration(id),
        &input.mobile,
    )?;
    let expires_at = now
        .checked_add_signed(service.settings.session().registration())
        .ok_or(inconsistent("registration lifetime must form a timestamp"))?;
    client
        .execute(
            "DELETE FROM identity.registrations WHERE (mobile).hash = $1",
            &[&service.keys.mobile.hash(&input.mobile)?],
        )
        .await?;
    client.execute("INSERT INTO identity.registrations (id, authentication_id, mobile, token, created_at, expires_at) VALUES ($1,$2,$3,$4,$5,$6)", &[&id, &authentication_id, &mobile, &sealed.as_ref(), &now, &expires_at]).await?;
    Ok(Receipt::Registration(Registration {
        id,
        token: registration::Token(token),
        expires_at,
    }))
}

struct RegistrationFact {
    id: registration::Id,
    mobile: Envelope,
    token: Sealed,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}
impl TryFrom<Row> for RegistrationFact {
    type Error = Error;
    fn try_from(row: Row) -> Result<Self, Error> {
        let fact = Self {
            id: row.try_get("id")?,
            mobile: row.try_get("mobile")?,
            token: row.try_get("token")?,
            created_at: row.try_get("created_at")?,
            expires_at: row.try_get("expires_at")?,
        };
        if fact.expires_at <= fact.created_at {
            return Err(inconsistent("registration must have a positive lifetime"));
        }
        Ok(fact)
    }
}
impl RegistrationFact {
    fn open(
        &self,
        keys: &keys::Keys,
        authentication: &Authentication,
    ) -> Result<credential::Secret, Error> {
        if !matches!(authentication.status, Status::Registrable)
            || self.created_at != authentication.finished_at
        {
            return Err(inconsistent(
                "registration must retain its authentication time",
            ));
        }
        self.mobile.open(
            &keys.encryption,
            &keys.mobile,
            Binding::Registration(self.id),
        )?;
        Ok(credential::Secret::open(
            &keys.credential,
            &self.token.0,
            credential::Binding::Registration(self.id),
        )?)
    }
}
async fn find_registration_by_authentication(
    client: &Client,
    authentication_id: authentication::Id,
) -> Result<Option<RegistrationFact>, Error> {
    client.query_opt("SELECT id, mobile, token, created_at, expires_at FROM identity.registrations WHERE authentication_id = $1", &[&authentication_id]).await?.map(RegistrationFact::try_from).transpose()
}

pub(super) async fn verify_all(
    client: &Client,
    app: &crate::kernel::App,
    keys: &keys::Keys,
    app_id: &website::AppId,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let read = Read::Snapshot(app.anchors().view()?);
    verification::Stage::AuthenticationsOpen.query(TRANSACTION_BUDGET, client.batch_execute("DECLARE authentication_history NO SCROLL CURSOR FOR SELECT id, proof, hash, key_id, purpose, app_id, transport, status, finished_at FROM identity.authentications ORDER BY id")).await??;
    loop {
        let complete = verification::Stage::AuthenticationsBatch
            .batch(TRANSACTION_BUDGET, async {
                let rows = client
                    .query("FETCH FORWARD 128 FROM authentication_history", &[])
                    .await?;
                let complete = rows.len() < 128;
                for row in rows {
                    let authentication = Authentication::try_from(row)?;
                    authentication.validate(&keys.input, app_id, now)?;
                    if let Some(session) =
                        session::find_by_authentication(client, app, read, authentication.id, now)
                            .await?
                    {
                        session::verify(client, &session, &keys.credential, now).await?;
                    }
                    if let Some(registration) =
                        find_registration_by_authentication(client, authentication.id).await?
                    {
                        registration.open(keys, &authentication)?;
                    }
                }
                Ok::<_, Error>(complete)
            })
            .await??;
        if complete {
            break;
        }
    }
    verification::Stage::AuthenticationAccountsClose
        .query(
            TRANSACTION_BUDGET,
            client.batch_execute("CLOSE authentication_history"),
        )
        .await??;
    let valid: bool = verification::Stage::AuthenticationRelations.query(TRANSACTION_BUDGET, client.query_one(
        "SELECT NOT EXISTS (SELECT 1 FROM identity.checks verification WHERE verification.failures <> (SELECT count(*) FROM identity.authentications WHERE message_id = verification.id AND status = 'invalid') OR (verification.authentication_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM identity.authentications WHERE id = verification.authentication_id AND message_id = verification.id AND status <> 'invalid')))", &[])).await??.try_get(0)?;
    if !valid {
        return Err(inconsistent(
            "retained verification facts must preserve their original pairing",
        ));
    }
    Ok(())
}
