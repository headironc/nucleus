//! Fixed registration queries and reconstruction of their retained facts.

use chrono::{DateTime, NaiveDate, Utc};
use tokio_postgres::{Client, Row};

use crate::{
    access::holder,
    database::{sealed::Sealed, verification},
    history::{self, Record as _},
    identity::{
        account, authentication, credential, keys,
        mobile::{
            Mobile,
            envelope::{Binding, Envelope},
            hash,
        },
        operation,
        person::revision,
        registration::{self, attempt},
        session, website,
    },
    registration::{Admission, BUDGET, Error, Input},
};

fn facts(rule: &'static str) -> Error {
    Error::Facts { rule }
}
fn invalid_digest(_: Vec<u8>) -> Error {
    facts("stored proof and input digests contain exactly 32 bytes")
}

pub(super) struct Source {
    pub(super) authentication_id: authentication::Id,
    pub(super) app_id: website::AppId,
    finished_at: DateTime<Utc>,
}

impl Source {
    fn read(row: &Row) -> Result<Self, Error> {
        if row.try_get::<_, &str>("authentication_status")? != "registrable"
            || row.try_get::<_, &str>("purpose")? != "create_session"
            || row.try_get::<_, &str>("transport")? != "cookie"
        {
            return Err(facts(
                "registration source is a registrable website SMS authentication",
            ));
        }
        Ok(Self {
            authentication_id: row.try_get("authentication_id")?,
            app_id: row.try_get("authentication_app_id")?,
            finished_at: row.try_get("finished_at")?,
        })
    }

    fn verify(&self, app_id: &website::AppId) -> Result<(), Error> {
        if &self.app_id != app_id {
            return Err(Error::Mismatch);
        }
        Ok(())
    }

    fn validate(&self, occurred_at: DateTime<Utc>, now: DateTime<Utc>) -> Result<(), Error> {
        if self.finished_at > occurred_at || occurred_at > now {
            return Err(facts(
                "registration events follow authentication and precede trusted time",
            ));
        }
        Ok(())
    }
}

pub(super) struct Credential {
    id: registration::Id,
    pub(super) source: Source,
    token: Sealed,
    mobile: Envelope,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

impl TryFrom<Row> for Credential {
    type Error = Error;
    fn try_from(row: Row) -> Result<Self, Error> {
        Ok(Self {
            id: row.try_get("id")?,
            source: Source::read(&row)?,
            token: row.try_get("token")?,
            mobile: row.try_get("mobile")?,
            created_at: row.try_get("created_at")?,
            expires_at: row.try_get("expires_at")?,
        })
    }
}

impl Credential {
    pub(super) fn verify(
        &self,
        credentials: &keys::credential::Keys,
        encryption: &keys::encryption::Keys,
        hashes: &keys::mobile::Keys,
        app_id: &website::AppId,
        input: &Input,
    ) -> Result<Mobile, Error> {
        let token = credential::Secret::open(
            credentials,
            &self.token.0,
            credential::Binding::Registration(self.id),
        )?;
        if !input.token.0.matches(&token.hash()) {
            return Err(Error::Proof);
        }
        self.source.verify(app_id)?;
        Ok(self
            .mobile
            .open(encryption, hashes, Binding::Registration(self.id))?)
    }

    pub(super) fn validate(&self, now: DateTime<Utc>) -> Result<(), Error> {
        self.source.validate(self.created_at, now)?;
        if self.created_at != self.source.finished_at || self.expires_at <= self.created_at {
            return Err(facts(
                "registration lifetime belongs to its original authentication",
            ));
        }
        if self.expires_at <= now {
            return Err(Error::Verification);
        }
        Ok(())
    }
}

pub(super) async fn find_credential(
    client: &Client,
    id: registration::Id,
) -> Result<Option<Credential>, Error> {
    client.query_opt("SELECT registration.id, registration.authentication_id, registration.token, registration.mobile, registration.created_at, registration.expires_at, authentication.app_id AS authentication_app_id, authentication.status AS authentication_status, authentication.purpose, authentication.transport, authentication.finished_at FROM identity.registrations registration JOIN identity.authentications authentication ON authentication.id = registration.authentication_id WHERE registration.id = $1", &[&id]).await?.map(Credential::try_from).transpose()
}

pub(super) async fn lock_credential(client: &Client, id: registration::Id) -> Result<(), Error> {
    client
        .query_one("SELECT identity.lock_registration($1)", &[&id])
        .await?;
    Ok(())
}

pub(super) async fn lock_account(client: &Client, id: account::Id) -> Result<(), Error> {
    client
        .query_one("SELECT identity.lock_account($1)", &[&id])
        .await?;
    Ok(())
}

pub(super) async fn day(client: &Client, accepted_at: DateTime<Utc>) -> Result<NaiveDate, Error> {
    Ok(client
        .query_one(
            "SELECT ($1::timestamptz AT TIME ZONE 'Asia/Shanghai')::date",
            &[&accepted_at],
        )
        .await?
        .try_get(0)?)
}

pub(super) async fn quota(
    client: &Client,
    mobile: &hash::Hash,
    accepted_at: DateTime<Utc>,
    hourly: usize,
) -> Result<(), Error> {
    let row = client.query_one("SELECT count(*) AS count, min(accepted_at) + interval '1 hour' AS retry_at FROM identity.attempts WHERE (mobile).hash = $1 AND accepted_at > $2::timestamptz - interval '1 hour'", &[mobile, &accepted_at]).await?;
    let hourly = i64::try_from(hourly).map_err(invalid_limit)?;
    if row.try_get::<_, i64>("count")? >= hourly {
        return Err(Error::Quota {
            retry_at: row.try_get("retry_at")?,
        });
    }
    Ok(())
}

fn invalid_limit(_: std::num::TryFromIntError) -> Error {
    facts("validated registration limit fits the SQL count")
}

pub(super) enum Original {
    Receipt(Completion),
    Attempt(Attempt),
}

pub(super) async fn find_by_operation(
    client: &Client,
    operation_id: operation::Id,
) -> Result<Option<Original>, Error> {
    if let Some(receipt) = find_receipt_by_operation(client, operation_id).await? {
        return Ok(Some(Original::Receipt(receipt)));
    }
    Ok(find_attempt_by_operation(client, operation_id)
        .await?
        .map(Original::Attempt))
}

pub(super) struct Attempt {
    id: attempt::Id,
    credential_id: registration::Id,
    source: Source,
    proof: [u8; 32],
    hash: [u8; 32],
    key_id: crate::keys::Id,
    mobile: Envelope,
    accepted_at: DateTime<Utc>,
    pub(super) status: attempt::Status,
}

impl TryFrom<Row> for Attempt {
    type Error = Error;
    fn try_from(row: Row) -> Result<Self, Error> {
        Ok(Self {
            id: row.try_get("id")?,
            credential_id: row.try_get("credential_id")?,
            source: Source::read(&row)?,
            proof: row
                .try_get::<_, Vec<u8>>("proof")?
                .try_into()
                .map_err(invalid_digest)?,
            hash: row
                .try_get::<_, Vec<u8>>("hash")?
                .try_into()
                .map_err(invalid_digest)?,
            key_id: row.try_get("key_id")?,
            mobile: row.try_get("mobile")?,
            accepted_at: row.try_get("accepted_at")?,
            status: row.try_get("status")?,
        })
    }
}

pub(super) async fn find_attempt_by_operation(
    client: &Client,
    operation_id: operation::Id,
) -> Result<Option<Attempt>, Error> {
    client.query_opt("SELECT attempt.id, attempt.credential_id, attempt.authentication_id, attempt.proof, attempt.hash, attempt.key_id, attempt.mobile, attempt.accepted_at, attempt.status, authentication.app_id AS authentication_app_id, authentication.status AS authentication_status, authentication.purpose, authentication.transport, authentication.finished_at FROM identity.attempts attempt JOIN identity.authentications authentication ON authentication.id = attempt.authentication_id WHERE attempt.operation_id = $1", &[&operation_id]).await?.map(Attempt::try_from).transpose()
}

impl Attempt {
    pub(super) fn validate_receipt(
        &self,
        receipt: &Completion,
        account: &history::Verified<account::state::State>,
    ) -> Result<(), Error> {
        if self.status != attempt::Status::Registered
            || self.id != receipt.id
            || self.credential_id != receipt.credential_id
            || self.source.app_id != receipt.app_id
            || self.proof != receipt.proof
            || self.accepted_at != account.created_at()
        {
            return Err(facts(
                "retained attempt and receipt preserve the same completion",
            ));
        }
        Ok(())
    }
    pub(super) fn verify(
        &self,
        keys: &keys::request::Keys,
        app_id: &website::AppId,
        input: &Input,
    ) -> Result<(), Error> {
        if !input.token.0.matches(&self.proof) {
            return Err(Error::Proof);
        }
        self.source.verify(app_id)?;
        if self.credential_id != input.credential_id
            || !keys
                .fetch(self.key_id)?
                .verify(&input.encoded()?, &self.hash)
        {
            return Err(Error::Mismatch);
        }
        Ok(())
    }

    pub(super) fn validate(
        &self,
        input: &keys::request::Keys,
        encryption: &keys::encryption::Keys,
        mobile: &keys::mobile::Keys,
        app_id: &website::AppId,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        if &self.source.app_id != app_id {
            return Err(facts("attempt retains the running website source"));
        }
        self.source.validate(self.accepted_at, now)?;
        input.fetch(self.key_id)?;
        self.mobile
            .open(encryption, mobile, Binding::Attempt(self.id))?;
        Ok(())
    }
}

pub(super) struct Completion {
    pub(super) id: attempt::Id,
    operation_id: operation::Id,
    person_id: crate::identity::person::Id,
    credential_id: registration::Id,
    app_id: website::AppId,
    proof: [u8; 32],
    pub(super) account_id: account::Id,
    revision_id: revision::Id,
}

impl TryFrom<Row> for Completion {
    type Error = Error;

    fn try_from(row: Row) -> Result<Self, Error> {
        if row.try_get::<_, &str>("method")? != "message"
            || row.try_get::<_, &str>("transport")? != "cookie"
        {
            return Err(facts("registration receipt uses website SMS credentials"));
        }
        Ok(Self {
            id: row.try_get("id")?,
            operation_id: row.try_get("operation_id")?,
            person_id: row.try_get("person_id")?,
            credential_id: row.try_get("credential_id")?,
            app_id: row.try_get("app_id")?,
            proof: row
                .try_get::<_, Vec<u8>>("proof")?
                .try_into()
                .map_err(invalid_digest)?,
            account_id: row.try_get("account_id")?,
            revision_id: row.try_get("revision_id")?,
        })
    }
}

pub(super) async fn find_receipt_by_operation(
    client: &Client,
    operation_id: operation::Id,
) -> Result<Option<Completion>, Error> {
    client.query_opt("SELECT id, operation_id, person_id, credential_id, app_id, method, transport, proof, account_id, revision_id FROM identity.receipts WHERE operation_id = $1", &[&operation_id]).await?.map(Completion::try_from).transpose()
}

impl Completion {
    pub(super) async fn verify(
        &self,
        client: &Client,
        keys: history::Keys<'_>,
        app_id: &website::AppId,
        input: &Input,
    ) -> Result<(), Error> {
        if !input.token.0.matches(&self.proof) {
            return Err(Error::Proof);
        }
        if &self.app_id != app_id {
            return Err(Error::Mismatch);
        }
        let profile = revision::fetch(client, keys, self.revision_id).await?;
        if profile.person_id() != self.person_id {
            return Err(revision::Error::Person.into());
        }
        if self.credential_id != input.credential_id
            || profile.name().as_ref() != input.name.as_ref()
            || profile.id_number().as_ref() != input.id_number.as_ref()
        {
            return Err(Error::Mismatch);
        }
        Ok(())
    }

    pub(super) async fn validate(
        &self,
        client: &Client,
        app: &crate::kernel::App,
        read: history::Read,
        app_id: &website::AppId,
        now: DateTime<Utc>,
    ) -> Result<history::Verified<account::state::State>, Error> {
        let account = history::find::<account::state::State>(
            client,
            app.keys(),
            app.anchors(),
            read,
            self.account_id,
            now,
        )
        .await?
        .ok_or_else(|| facts("a retained receipt has its account"))?;
        if account.person_id() != self.person_id
            || account.current().is_none()
            || &self.app_id != app_id
        {
            return Err(facts(
                "receipt retains its current account, person and running website",
            ));
        }
        let creation = holder::creation(client, app.keys(), self.account_id, now)
            .await?
            .ok_or_else(|| facts("a retained registration receipt has its first appointment"))?;
        let assignment = creation
            .assignment()
            .ok_or_else(|| facts("registration creates an appointment"))?;
        if creation.operation_id() != self.operation_id
            || creation.person_id() != self.person_id
            || assignment.source != holder::Source::Registration
            || assignment.operator_id != self.revision_id
            || assignment.revision_id != self.revision_id
            || creation.assigned_at() != account.created_at()
        {
            return Err(facts(
                "registration receipt belongs to its first self assignment",
            ));
        }
        let profile = revision::fetch(client, app.keys(), self.revision_id).await?;
        if profile.person_id() != self.person_id {
            return Err(revision::Error::Person.into());
        }
        Ok(account)
    }

    pub(super) fn validate_session(
        &self,
        session: &session::Session,
        account: &history::Verified<account::state::State>,
    ) -> Result<(), Error> {
        if session.account_id() != self.account_id
            || session.app_id() != &self.app_id
            || session.created_at() != account.created_at()
        {
            return Err(facts(
                "session belongs to its original registration completion",
            ));
        }
        Ok(())
    }
}

pub(super) async fn insert_attempt(
    client: &Client,
    input: &Input,
    admission: &Admission<'_>,
    encryption: &keys::encryption::Keys,
    mobile: &keys::mobile::Keys,
    keys: &keys::request::Keys,
    status: attempt::Status,
) -> Result<(), Error> {
    let envelope = Envelope::seal(
        encryption,
        mobile,
        Binding::Attempt(admission.id),
        admission.mobile,
    )?;
    let key = keys.active();
    let hash = key.sign(&input.encoded()?);
    client.execute("INSERT INTO identity.attempts (id, operation_id, credential_id, authentication_id, proof, hash, key_id, mobile, accepted_at, status) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)", &[&admission.id, &input.operation_id, &input.credential_id, &admission.credential.source.authentication_id, &&input.token.0.hash()[..], &&hash.as_ref()[..], &key.id(), &envelope, &admission.accepted_at, &status.as_ref()]).await?;
    Ok(())
}

pub(super) async fn insert_receipt(
    client: &Client,
    input: &Input,
    admission: &Admission<'_>,
    identity: &account::Creation,
) -> Result<(), Error> {
    client.execute("INSERT INTO identity.receipts (id, operation_id, credential_id, app_id, method, transport, proof, account_id, person_id, revision_id) VALUES ($1, $2, $3, $4, 'message', 'cookie', $5, $6, $7, $8)", &[&admission.id, &input.operation_id, &input.credential_id, &admission.credential.source.app_id, &&input.token.0.hash()[..], &identity.account_id, &identity.person_id, &identity.revision_id]).await?;
    Ok(())
}

pub(super) async fn consume(client: &Client, credential_id: registration::Id) -> Result<(), Error> {
    if client
        .execute(
            "DELETE FROM identity.registrations WHERE id = $1",
            &[&credential_id],
        )
        .await?
        != 1
    {
        return Err(facts(
            "accepted registration consumes exactly its original credential",
        ));
    }
    Ok(())
}

pub(super) async fn verify_all(
    client: &Client,
    app: &crate::kernel::App,
    keys: &keys::Keys,
    app_id: &website::AppId,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let read = crate::history::Read::Snapshot(app.anchors().view()?);
    let keys::Keys {
        input,
        encryption,
        mobile,
        credential,
        ..
    } = keys;
    let mut option: Option<attempt::Id> = None;
    loop {
        let rows = verification::Stage::RegistrationAttempts.query(BUDGET, client.query("SELECT id, operation_id FROM identity.attempts WHERE ($1::uuid IS NULL OR id > $1) ORDER BY id LIMIT 128", &[&option])).await??;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            let id: attempt::Id = row.try_get("id")?;
            let operation_id = row.try_get("operation_id")?;
            let attempt = verification::Stage::RegistrationAttempt
                .batch(BUDGET, find_attempt_by_operation(client, operation_id))
                .await??
                .ok_or_else(|| facts("retained attempt has its source"))?;
            attempt.source.verify(app_id)?;
            attempt.validate(input, encryption, mobile, app_id, now)?;
            if let Some(receipt) = verification::Stage::RegistrationAttemptReceipt
                .batch(BUDGET, find_receipt_by_operation(client, operation_id))
                .await??
            {
                let account = verification::Stage::RegistrationAttemptReceipt
                    .batch(BUDGET, receipt.validate(client, app, read, app_id, now))
                    .await??;
                attempt.validate_receipt(&receipt, &account)?;
            }
            option = Some(id);
        }
    }
    option = None;
    loop {
        let rows = verification::Stage::RegistrationReceipts.query(BUDGET, client.query("SELECT id, operation_id FROM identity.receipts WHERE ($1::uuid IS NULL OR id > $1) ORDER BY id LIMIT 128", &[&option])).await??;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            let id = row.try_get("id")?;
            let operation_id = row.try_get("operation_id")?;
            let receipt = verification::Stage::RegistrationReceipt
                .batch(BUDGET, find_receipt_by_operation(client, operation_id))
                .await??
                .ok_or_else(|| {
                    facts("retained receipt has its immutable identity and first assignment")
                })?;
            let account = verification::Stage::RegistrationReceipt
                .batch(BUDGET, receipt.validate(client, app, read, app_id, now))
                .await??;
            if let Some(session) = verification::Stage::RegistrationSession
                .batch(
                    BUDGET,
                    session::find_by_receipt(client, app, read, receipt.id, now),
                )
                .await??
            {
                receipt.validate_session(&session, &account)?;
                verification::Stage::RegistrationSessionSource
                    .batch(BUDGET, session::verify(client, &session, credential, now))
                    .await??;
            }
            option = Some(id);
        }
    }
    Ok(())
}
