//! Sealed session state and its fixed issuance and revocation history.

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, Row, types::ToSql};
use zeroize::Zeroizing;

use crate::{
    history::{self, Change, Draft, Error, Keys, Kind, Locator, Read, Token, Verified},
    identity::{
        account, authentication, operation, registration::attempt, session::Id, suspension,
        transfer, website::AppId,
    },
    keys::seal::Seal,
};

pub(crate) const TABLE: &str = "identity.sessions";
pub(crate) const ISSUANCES: &str = "identity.issuances";
pub(crate) const REVOCATIONS: &str = "identity.revocations";

pub(crate) async fn lock_shared(client: &Client, id: Id) -> Result<(), Error> {
    client
        .query_opt(
            "SELECT id FROM identity.sessions WHERE id=$1 FOR SHARE",
            &[&id],
        )
        .await?;
    Ok(())
}

/// Discovery is only a lock hint; the caller authenticates the row after both locks.
pub(crate) async fn lock_existing(client: &Client, id: Id) -> Result<(), Error> {
    if let Some(row) = client
        .query_opt(
            "SELECT account_id FROM identity.sessions WHERE id=$1",
            &[&id],
        )
        .await?
    {
        account::state::lock(client, row.try_get("account_id")?).await?;
    }
    client
        .query_opt(
            "SELECT id FROM identity.sessions WHERE id=$1 FOR UPDATE",
            &[&id],
        )
        .await?;
    Ok(())
}

pub(crate) async fn lock_authentication(
    client: &Client,
    id: authentication::Id,
) -> Result<(), Error> {
    if let Some(row) = client
        .query_opt(
            "SELECT account_id FROM identity.sessions WHERE authentication_id=$1",
            &[&id],
        )
        .await?
    {
        account::state::lock(client, row.try_get("account_id")?).await?;
    }
    Ok(())
}

pub(crate) async fn lock_source(client: &Client, id: authentication::Id) -> Result<(), Error> {
    client
        .query_opt(
            "SELECT id FROM identity.sessions WHERE authentication_id=$1 FOR UPDATE",
            &[&id],
        )
        .await?;
    Ok(())
}

pub(crate) async fn lock_receipt(client: &Client, id: attempt::Id) -> Result<(), Error> {
    client
        .query_opt(
            "SELECT id FROM identity.sessions WHERE receipt_id=$1 FOR UPDATE",
            &[&id],
        )
        .await?;
    Ok(())
}

/// These unqualified candidates only schedule a later locked, authenticated selection.
pub(crate) async fn authentication_candidates(
    client: &Client,
    cursor: Option<operation::Id>,
    now: DateTime<Utc>,
) -> Result<Vec<operation::Id>, Error> {
    let rows = client.query(
        "SELECT authentication.operation_id FROM identity.sessions session JOIN identity.authentications authentication ON authentication.id=session.authentication_id LEFT JOIN identity.handoffs handoff ON handoff.id=session.id WHERE session.receipt_id IS NULL AND ($1::uuid IS NULL OR authentication.operation_id>$1) AND (LEAST(session.expires_at,session.revoked_at)+interval '24 hours'<=$2 OR handoff.expires_at<=$2 OR (handoff.id IS NOT NULL AND session.revoked_at IS NOT NULL)) ORDER BY authentication.operation_id LIMIT 128",
        &[&cursor,&now],
    ).await?;
    rows.into_iter()
        .map(|row| row.try_get(0).map_err(Error::decode))
        .collect()
}

pub(crate) async fn registration_candidates(
    client: &Client,
    cursor: Option<operation::Id>,
    now: DateTime<Utc>,
) -> Result<Vec<operation::Id>, Error> {
    let rows = client.query(
        "SELECT receipt.operation_id FROM identity.sessions session JOIN identity.receipts receipt ON receipt.id=session.receipt_id LEFT JOIN identity.handoffs handoff ON handoff.id=session.id WHERE ($1::uuid IS NULL OR receipt.operation_id>$1) AND (LEAST(session.expires_at,session.revoked_at)+interval '24 hours'<=$2 OR handoff.expires_at<=$2 OR (handoff.id IS NOT NULL AND session.revoked_at IS NOT NULL)) ORDER BY receipt.operation_id LIMIT 128",
        &[&cursor,&now],
    ).await?;
    rows.into_iter()
        .map(|row| row.try_get(0).map_err(Error::decode))
        .collect()
}

pub(crate) async fn removed(client: &Client, id: Id) -> Result<(), Error> {
    let retained: bool = client.query_one(
        "SELECT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$1) OR EXISTS(SELECT 1 FROM identity.issuances WHERE id=$1) OR EXISTS(SELECT 1 FROM identity.revocations WHERE id=$1)",
        &[&id],
    ).await?.try_get(0).map_err(Error::decode)?;
    if retained {
        return Err(Error::Integrity(
            "a removed session leaves no credential or history",
        ));
    }
    Ok(())
}

/// The caller's exclusive Account lock fixes this set before decisive time is taken.
pub(crate) async fn lock_account(client: &Client, id: account::Id) -> Result<(), Error> {
    client
        .query(
            "SELECT id FROM identity.sessions WHERE account_id=$1 ORDER BY id FOR UPDATE",
            &[&id],
        )
        .await?;
    Ok(())
}

/// Both Account locks precede this one ordered Session set, including the acting session.
pub(crate) async fn lock_management(
    client: &Client,
    actor: Id,
    account: account::Id,
) -> Result<(), Error> {
    client
        .query(
            "SELECT id FROM identity.sessions WHERE id=$1 OR account_id=$2 ORDER BY id FOR UPDATE",
            &[&actor, &account],
        )
        .await?;
    Ok(())
}

/// Unqualified issuance input; only `create` produces a paired state transition.
#[derive(Clone)]
pub(crate) struct Input {
    pub(crate) id: Id,
    pub(crate) account_id: account::Id,
    pub(crate) authentication_id: authentication::Id,
    pub(crate) receipt_id: Option<attempt::Id>,
    pub(crate) hash: [u8; 32],
    pub(crate) app_id: AppId,
    pub(crate) expires_at: DateTime<Utc>,
}

#[derive(Clone)]
pub(crate) struct State {
    id: Id,
    account_id: account::Id,
    authentication_id: authentication::Id,
    receipt_id: Option<attempt::Id>,
    hash: [u8; 32],
    app_id: AppId,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
    sequence: i64,
    version: i64,
}

impl State {
    pub(crate) fn id(&self) -> Id {
        self.id
    }
    pub(crate) fn account_id(&self) -> account::Id {
        self.account_id
    }
    pub(crate) fn authentication_id(&self) -> authentication::Id {
        self.authentication_id
    }
    pub(crate) fn receipt_id(&self) -> Option<attempt::Id> {
        self.receipt_id
    }
    pub(crate) fn hash(&self) -> &[u8; 32] {
        &self.hash
    }
    pub(crate) fn app_id(&self) -> &AppId {
        &self.app_id
    }
    pub(crate) fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    pub(crate) fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
    pub(crate) fn revoked_at(&self) -> Option<DateTime<Utc>> {
        self.revoked_at
    }
    pub(crate) fn available(&self, at: DateTime<Utc>) -> bool {
        self.revoked_at.is_none() && at < self.expires_at
    }

    fn input(&self) -> Input {
        Input {
            id: self.id,
            account_id: self.account_id,
            authentication_id: self.authentication_id,
            receipt_id: self.receipt_id,
            hash: self.hash,
            app_id: self.app_id.clone(),
            expires_at: self.expires_at,
        }
    }

    fn validate(&self, now: DateTime<Utc>) -> Result<(), Error> {
        validate_lifetime(self.created_at, self.expires_at, now)?;
        match self.revoked_at {
            None if self.sequence == 1 && self.version == 1 => Ok(()),
            Some(at)
                if self.sequence == 2
                    && self.version == 2
                    && at >= self.created_at
                    && at <= now =>
            {
                Ok(())
            }
            _ => Err(Error::Integrity(
                "a session has its issuance or a non-future single revocation head",
            )),
        }
    }
}

pub(crate) struct Data {
    input: Input,
    created_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
}

impl Kind for State {
    type Error = Error;
    const NAME: &'static str = "identity.session";
    const SELECT: &'static str = "SELECT state.id,state.account_id,state.authentication_id,state.receipt_id,state.hash,state.method,state.app_id,state.created_at,state.expires_at,state.revoked_at,state.sequence,state.version,state.seal,state.seal_key_id,state.seal_version FROM identity.sessions state";

    fn decode(_: Token, row: &Row, _: Keys<'_>, now: DateTime<Utc>) -> Result<Self, Error> {
        let value = Self {
            id: row.try_get("id").map_err(Error::decode)?,
            account_id: row.try_get("account_id").map_err(Error::decode)?,
            authentication_id: row.try_get("authentication_id").map_err(Error::decode)?,
            receipt_id: row.try_get("receipt_id").map_err(Error::decode)?,
            hash: decode_hash(row)?,
            app_id: row.try_get("app_id").map_err(Error::decode)?,
            created_at: row.try_get("created_at").map_err(Error::decode)?,
            expires_at: row.try_get("expires_at").map_err(Error::decode)?,
            revoked_at: row.try_get("revoked_at").map_err(Error::decode)?,
            sequence: row.try_get("sequence").map_err(Error::decode)?,
            version: row.try_get("version").map_err(Error::decode)?,
        };
        check_method(row)?;
        value.validate(now)?;
        Ok(value)
    }

    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        let mut fields = issuance_fields(&self.input(), self.created_at);
        fields.push(time_field(self.revoked_at));
        fields.push(self.sequence.to_string().into_bytes());
        fields.push(self.version.to_string().into_bytes());
        fields
    }
}

impl history::State for State {
    type Id = Id;
    type Data = Data;
    type Record = Record;

    fn build(_: Token, data: Data, sequence: i64, version: i64) -> Self {
        Self {
            id: data.input.id,
            account_id: data.input.account_id,
            authentication_id: data.input.authentication_id,
            receipt_id: data.input.receipt_id,
            hash: data.input.hash,
            app_id: data.input.app_id,
            created_at: data.created_at,
            expires_at: data.input.expires_at,
            revoked_at: data.revoked_at,
            sequence,
            version,
        }
    }
    fn id(&self) -> Id {
        self.id
    }
    fn bytes(id: Id) -> [u8; 16] {
        *id.as_bytes()
    }
    fn sequence(&self) -> i64 {
        self.sequence
    }
    fn version(&self) -> i64 {
        self.version
    }
    fn has_head(&self, record: &Record) -> bool {
        if self.id != record.session_id
            || self.account_id != record.data.account_id
            || self.sequence != record.sequence
        {
            return false;
        }
        match &record.data.payload {
            Payload::Issuance { input, created_at } => {
                self.revoked_at.is_none()
                    && self.id == input.id
                    && self.account_id == input.account_id
                    && self.authentication_id == input.authentication_id
                    && self.receipt_id == input.receipt_id
                    && self.hash == input.hash
                    && self.app_id == input.app_id
                    && self.created_at == *created_at
                    && self.expires_at == input.expires_at
            }
            Payload::Revocation { revoked_at, .. } => self.revoked_at == Some(*revoked_at),
        }
    }

    fn locators(&self) -> Vec<Locator> {
        let mut member = Vec::with_capacity(32);
        member.extend_from_slice(self.account_id.as_bytes());
        member.extend_from_slice(self.id.as_bytes());
        let mut locations = vec![
            Locator::new::<Self>("authentication", self.authentication_id.as_bytes()),
            Locator::new::<Self>("account", &member),
        ];
        if let Some(id) = self.receipt_id {
            locations.push(Locator::new::<Self>("receipt", id.as_bytes()));
        }
        locations
    }

    async fn write(
        &self,
        _: Token,
        client: &Client,
        seal: &Seal,
        original: Option<&Verified<Self>>,
    ) -> Result<u64, Error> {
        Ok(match original {
            None => client.execute("INSERT INTO identity.sessions (id,account_id,authentication_id,receipt_id,hash,method,app_id,created_at,expires_at,revoked_at,sequence,version,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,'message',$6,$7,$8,$9,$10,$11,$12,$13,$14)", &[&self.id,&self.account_id,&self.authentication_id,&self.receipt_id,&&self.hash[..],&self.app_id,&self.created_at,&self.expires_at,&self.revoked_at,&self.sequence,&self.version,&seal.tag(),&seal.key_id(),&seal.version()]).await?,
            Some(original) => client.execute("UPDATE identity.sessions SET revoked_at=$2,sequence=$3,version=$4,seal=$5,seal_key_id=$6,seal_version=$7 WHERE id=$1 AND version=$8 AND seal=$9 AND seal_key_id=$10 AND seal_version=$11", &[&self.id,&self.revoked_at,&self.sequence,&self.version,&seal.tag(),&seal.key_id(),&seal.version(),&original.version(),&original.seal().tag(),&original.seal().key_id(),&original.seal().version()]).await?,
        })
    }
}

pub(crate) enum Location {
    Authentication(authentication::Id),
    Receipt(attempt::Id),
}

pub(crate) async fn located(
    client: &Client,
    keys: Keys<'_>,
    anchors: &history::Anchors,
    read: Read,
    location: Location,
    now: DateTime<Utc>,
) -> Result<Option<Verified<State>>, Error> {
    let (condition, value, locator): (&str, &(dyn ToSql + Sync), _) = match &location {
        Location::Authentication(id) => (
            "state.authentication_id = $1",
            id,
            Locator::new::<State>("authentication", id.as_bytes()),
        ),
        Location::Receipt(id) => (
            "state.receipt_id = $1",
            id,
            Locator::new::<State>("receipt", id.as_bytes()),
        ),
    };
    let lock = match read {
        Read::Locked => " FOR UPDATE OF state",
        Read::Shared => " FOR SHARE OF state",
        Read::Snapshot(_) => "",
    };
    let row = client
        .query_opt(
            &format!("{} WHERE {condition}{lock}", State::SELECT),
            &[value],
        )
        .await?;
    history::located(row.as_ref(), &locator, keys, anchors, read, now)
}

/// The account lock fixes membership; each selected row and any omitted location are checked.
pub(crate) async fn for_account(
    client: &Client,
    keys: Keys<'_>,
    anchors: &history::Anchors,
    read: Read,
    account_id: account::Id,
    now: DateTime<Utc>,
) -> Result<Vec<Verified<State>>, Error> {
    let lock = match read {
        Read::Locked => " FOR UPDATE OF state",
        Read::Shared => " FOR SHARE OF state",
        Read::Snapshot(_) => "",
    };
    let rows = client
        .query(
            &format!(
                "{} WHERE state.account_id=$1 ORDER BY state.id{lock}",
                State::SELECT
            ),
            &[&account_id],
        )
        .await?;
    history::related(
        &rows,
        &Locator::new::<State>("account", account_id.as_bytes()),
        keys,
        anchors,
        read,
        now,
    )
}

impl history::Removable for State {
    async fn remove(_: Token, client: &Client, original: &Verified<Self>) -> Result<u64, Error> {
        client
            .execute("DELETE FROM identity.handoffs WHERE id=$1", &[&original.id])
            .await?;
        client
            .execute(
                "DELETE FROM identity.revocations WHERE id=$1",
                &[&original.id],
            )
            .await?;
        client
            .execute(
                "DELETE FROM identity.issuances WHERE id=$1",
                &[&original.id],
            )
            .await?;
        Ok(client.execute(
            "DELETE FROM identity.sessions WHERE id=$1 AND version=$2 AND seal=$3 AND seal_key_id=$4 AND seal_version=$5",
            &[&original.id, &original.version, &original.seal().tag(),
              &original.seal().key_id(), &original.seal().version()],
        ).await?)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cause {
    Logout(operation::Id),
    Suspension(suspension::Id),
    Transfer(transfer::Id),
}

impl Cause {
    fn mode(self) -> &'static str {
        match self {
            Self::Logout(_) => "logout",
            Self::Suspension(_) => "suspension",
            Self::Transfer(_) => "transfer",
        }
    }
    fn as_bytes(&self) -> &[u8; 16] {
        match self {
            Self::Logout(id) => id.as_bytes(),
            Self::Suspension(id) => id.as_bytes(),
            Self::Transfer(id) => id.as_bytes(),
        }
    }
    fn id(&self) -> &(dyn ToSql + Sync) {
        match self {
            Self::Logout(id) => id,
            Self::Suspension(id) => id,
            Self::Transfer(id) => id,
        }
    }
    fn validate(self, operation_id: operation::Id) -> Result<(), Error> {
        if let Self::Logout(id) = self
            && id != operation_id
        {
            return Err(Error::Integrity(
                "a logout revocation names its permanent operation as the cause",
            ));
        }
        Ok(())
    }
}

pub(crate) struct Record {
    session_id: Id,
    sequence: i64,
    data: RecordData,
}

#[cfg_attr(any(test, feature = "test-support"), derive(Clone))]
pub(crate) struct RecordData {
    account_id: account::Id,
    operation_id: operation::Id,
    payload: Payload,
}

#[cfg_attr(any(test, feature = "test-support"), derive(Clone))]
enum Payload {
    Issuance {
        input: Input,
        created_at: DateTime<Utc>,
    },
    Revocation {
        cause: Cause,
        revoked_at: DateTime<Utc>,
    },
}

#[cfg(test)]
impl Record {
    pub(crate) fn cause(&self) -> Option<Cause> {
        match self.data.payload {
            Payload::Issuance { .. } => None,
            Payload::Revocation { cause, .. } => Some(cause),
        }
    }
}

impl Kind for Record {
    type Error = Error;
    const NAME: &'static str = "identity.session.record";
    const SELECT: &'static str = "SELECT record.session_id,record.account_id,record.sequence,record.kind,record.authentication_id,record.receipt_id,record.hash,record.method,record.app_id,record.created_at,record.expires_at,record.mode,record.operation_id,record.cause_id,record.revoked_at,record.seal,record.seal_key_id,record.seal_version,record.transaction_id FROM (SELECT issuance.id AS session_id,issuance.account_id,issuance.sequence,'issuance'::text AS kind,issuance.authentication_id,issuance.receipt_id,issuance.hash,issuance.method,issuance.app_id,issuance.created_at,issuance.expires_at,NULL::text AS mode,issuance.operation_id,NULL::uuid AS cause_id,NULL::timestamptz AS revoked_at,issuance.seal,issuance.seal_key_id,issuance.seal_version,issuance.transaction_id FROM identity.issuances issuance UNION ALL SELECT revocation.id,revocation.account_id,revocation.sequence,'revocation'::text,NULL::uuid,NULL::uuid,NULL::bytea,NULL::text,NULL::text,NULL::timestamptz,NULL::timestamptz,revocation.mode,revocation.operation_id,revocation.cause_id,revocation.revoked_at,revocation.seal,revocation.seal_key_id,revocation.seal_version,revocation.transaction_id FROM identity.revocations revocation) record";

    fn decode(_: Token, row: &Row, _: Keys<'_>, now: DateTime<Utc>) -> Result<Self, Error> {
        let session_id = row.try_get("session_id").map_err(Error::decode)?;
        let account_id = row.try_get("account_id").map_err(Error::decode)?;
        let sequence = row.try_get("sequence").map_err(Error::decode)?;
        let operation_id = row.try_get("operation_id").map_err(Error::decode)?;
        let kind = row.try_get::<_, &str>("kind").map_err(Error::decode)?;
        let payload = match (kind, sequence) {
            ("issuance", 1) => {
                check_method(row)?;
                let created_at = row.try_get("created_at").map_err(Error::decode)?;
                let expires_at = row.try_get("expires_at").map_err(Error::decode)?;
                validate_lifetime(created_at, expires_at, now)?;
                Payload::Issuance {
                    input: Input {
                        id: session_id,
                        account_id,
                        authentication_id: row
                            .try_get("authentication_id")
                            .map_err(Error::decode)?,
                        receipt_id: row.try_get("receipt_id").map_err(Error::decode)?,
                        hash: decode_hash(row)?,
                        app_id: row.try_get("app_id").map_err(Error::decode)?,
                        expires_at,
                    },
                    created_at,
                }
            }
            ("revocation", 2) => {
                let cause = match row.try_get::<_, &str>("mode").map_err(Error::decode)? {
                    "logout" => Cause::Logout(row.try_get("cause_id").map_err(Error::decode)?),
                    "suspension" => {
                        Cause::Suspension(row.try_get("cause_id").map_err(Error::decode)?)
                    }
                    "transfer" => Cause::Transfer(row.try_get("cause_id").map_err(Error::decode)?),
                    _ => {
                        return Err(Error::Integrity(
                            "a session revocation has a fixed cause kind",
                        ));
                    }
                };
                cause.validate(operation_id)?;
                let revoked_at = row.try_get("revoked_at").map_err(Error::decode)?;
                if revoked_at > now {
                    return Err(Error::Integrity(
                        "a session revocation is not in the future",
                    ));
                }
                Payload::Revocation { cause, revoked_at }
            }
            _ => {
                return Err(Error::Integrity(
                    "a session record has its fixed kind and sequence",
                ));
            }
        };
        Ok(Self {
            session_id,
            sequence,
            data: RecordData {
                account_id,
                operation_id,
                payload,
            },
        })
    }

    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        let mut fields = match &self.data.payload {
            Payload::Issuance { input, created_at } => {
                let mut fields = issuance_fields(input, *created_at);
                fields.insert(0, b"issuance".to_vec());
                fields
            }
            Payload::Revocation { cause, revoked_at } => Zeroizing::new(vec![
                b"revocation".to_vec(),
                self.session_id.as_bytes().to_vec(),
                self.data.account_id.as_bytes().to_vec(),
                cause.mode().as_bytes().to_vec(),
                cause.as_bytes().to_vec(),
                revoked_at.timestamp_micros().to_string().into_bytes(),
            ]),
        };
        fields.push(self.data.operation_id.as_bytes().to_vec());
        fields.push(self.sequence.to_string().into_bytes());
        fields
    }
}

impl history::Record for Record {
    const OBJECT: &'static str = "session_id";
    type Object = Id;
    type Data = RecordData;

    fn build(_: Token, session_id: Id, sequence: i64, data: RecordData) -> Self {
        Self {
            session_id,
            sequence,
            data,
        }
    }
    fn object(&self) -> Id {
        self.session_id
    }
    fn sequence(&self) -> i64 {
        self.sequence
    }
    fn operation_id(&self) -> operation::Id {
        self.data.operation_id
    }
    fn follows(&self, previous: &Self) -> bool {
        self.session_id == previous.session_id
            && self.data.account_id == previous.data.account_id
            && self.sequence == 2
            && previous.sequence == 1
            && matches!(
                (&self.data.payload, &previous.data.payload),
                (Payload::Revocation { revoked_at, .. }, Payload::Issuance { created_at, .. })
                    if revoked_at >= created_at
            )
    }

    async fn insert(
        &self,
        _: Token,
        client: &Client,
        _: Keys<'_>,
        seal: &Seal,
    ) -> Result<(), Error> {
        match &self.data.payload {
            Payload::Issuance { input, created_at } => {
                client.execute("INSERT INTO identity.issuances (id,account_id,authentication_id,receipt_id,hash,method,app_id,created_at,expires_at,operation_id,sequence,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,'message',$6,$7,$8,$9,$10,$11,$12,$13)", &[&self.session_id,&self.data.account_id,&input.authentication_id,&input.receipt_id,&&input.hash[..],&input.app_id,created_at,&input.expires_at,&self.data.operation_id,&self.sequence,&seal.tag(),&seal.key_id(),&seal.version()]).await?;
            }
            Payload::Revocation { cause, revoked_at } => {
                client.execute("INSERT INTO identity.revocations (id,account_id,sequence,mode,operation_id,cause_id,revoked_at,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)", &[&self.session_id,&self.data.account_id,&self.sequence,&cause.mode(),&self.data.operation_id,cause.id(),revoked_at,&seal.tag(),&seal.key_id(),&seal.version()]).await?;
            }
        }
        Ok(())
    }
}

/// Verify the original issuance after crossing its R1 operation lock.
pub(crate) async fn issued(
    client: &Client,
    keys: Keys<'_>,
    anchors: &history::Anchors,
    id: Id,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<bool, Error> {
    let row = client
        .query_opt(
            &format!(
                "{} WHERE record.session_id=$1 AND record.sequence=1",
                Record::SELECT
            ),
            &[&id],
        )
        .await?;
    let Some(row) = row else {
        if client
            .query_opt("SELECT id FROM identity.sessions WHERE id=$1", &[&id])
            .await?
            .is_some()
        {
            return Err(Error::Integrity(
                "a retained session has its issuance record",
            ));
        }
        return Ok(false);
    };
    let record = history::record::<Record>(&row, keys, now)?;
    if record.session_id != id || record.data.operation_id != operation_id {
        return Err(Error::Integrity(
            "issuance retains its original session and operation",
        ));
    }
    anchors.confirm::<State>(&record)?;
    Ok(true)
}

pub(crate) fn create(
    input: Input,
    operation_id: operation::Id,
    at: DateTime<Utc>,
) -> Result<Change<State>, Error> {
    validate_lifetime(at, input.expires_at, at)?;
    let account_id = input.account_id;
    Ok(Change::new(
        Draft::create(Data {
            input: input.clone(),
            created_at: at,
            revoked_at: None,
        }),
        RecordData {
            account_id,
            operation_id,
            payload: Payload::Issuance {
                input,
                created_at: at,
            },
        },
    ))
}

/// Rebuild the retained session group before a disposable fixture installs new anchors.
#[cfg(any(test, feature = "test-support"))]
pub(crate) async fn age(
    client: &Client,
    keys: Keys<'_>,
    now: DateTime<Utc>,
    elapsed: chrono::TimeDelta,
) -> Result<Vec<Verified<State>>, Error> {
    use history::Seed;

    let states = history::inspect::<State>(client, keys, now).await?;
    let rows = client.query(Record::SELECT, &[]).await?;
    let records = rows
        .iter()
        .map(|row| history::record::<Record>(row, keys, now))
        .collect::<Result<Vec<_>, _>>()?;
    for record in records {
        let mut data = record.data.clone();
        match &mut data.payload {
            Payload::Issuance { input, created_at } => {
                input.expires_at -= elapsed;
                *created_at -= elapsed;
            }
            Payload::Revocation { revoked_at, .. } => *revoked_at -= elapsed,
        }
        let dated = Seed::record::<Record>(record.session_id, record.sequence, data, keys)?;
        match &dated.data.payload {
            Payload::Issuance { input, created_at } => {
                client.execute(
                    "UPDATE identity.issuances SET created_at=$2,expires_at=$3,seal=$4,seal_key_id=$5,seal_version=$6 WHERE id=$1",
                    &[&dated.session_id, created_at, &input.expires_at, &dated.seal().tag(),
                      &dated.seal().key_id(), &dated.seal().version()],
                ).await?;
            }
            Payload::Revocation { revoked_at, .. } => {
                client.execute(
                    "UPDATE identity.revocations SET revoked_at=$2,seal=$3,seal_key_id=$4,seal_version=$5 WHERE id=$1",
                    &[&dated.session_id, revoked_at, &dated.seal().tag(),
                      &dated.seal().key_id(), &dated.seal().version()],
                ).await?;
            }
        }
    }
    let mut dated_states = Vec::new();
    for state in states {
        let mut input = state.input();
        input.expires_at -= elapsed;
        let dated = Seed::state::<State>(
            Data {
                input,
                created_at: state.created_at - elapsed,
                revoked_at: state.revoked_at.map(|at| at - elapsed),
            },
            state.sequence,
            state.version,
            keys,
        )?;
        client.execute(
            "UPDATE identity.sessions SET created_at=$2,expires_at=$3,revoked_at=$4,seal=$5,seal_key_id=$6,seal_version=$7 WHERE id=$1",
            &[&dated.id, &dated.created_at, &dated.expires_at, &dated.revoked_at, &dated.seal().tag(),
              &dated.seal().key_id(), &dated.seal().version()],
        ).await?;
        dated_states.push(dated);
    }
    Ok(dated_states)
}

pub(crate) fn revoke(
    original: &Verified<State>,
    operation_id: operation::Id,
    cause: Cause,
    at: DateTime<Utc>,
) -> Result<Change<State>, Error> {
    original.validate(at)?;
    if original.revoked_at.is_some() {
        return Err(Error::Integrity("a session is revoked at most once"));
    }
    cause.validate(operation_id)?;
    Ok(Change::new(
        Draft::advance(
            original,
            Data {
                input: original.input(),
                created_at: original.created_at,
                revoked_at: Some(at),
            },
        )?,
        RecordData {
            account_id: original.account_id,
            operation_id,
            payload: Payload::Revocation {
                cause,
                revoked_at: at,
            },
        },
    ))
}

fn validate_lifetime(
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    if created_at > now || expires_at <= created_at {
        return Err(Error::Integrity(
            "a session retains a non-future issuance and a strictly later expiry",
        ));
    }
    Ok(())
}

fn check_method(row: &Row) -> Result<(), Error> {
    if row.try_get::<_, &str>("method").map_err(Error::decode)? != "message" {
        return Err(Error::Integrity(
            "a session retains its message authentication method",
        ));
    }
    Ok(())
}

fn decode_hash(row: &Row) -> Result<[u8; 32], Error> {
    let bytes = Zeroizing::new(row.try_get::<_, Vec<u8>>("hash").map_err(Error::decode)?);
    match bytes.as_slice().try_into() {
        Ok(hash) => Ok(hash),
        Err(_) => Err(Error::Integrity("a session token digest is 32 bytes")),
    }
}

fn issuance_fields(input: &Input, created_at: DateTime<Utc>) -> Zeroizing<Vec<Vec<u8>>> {
    Zeroizing::new(vec![
        input.id.as_bytes().to_vec(),
        input.account_id.as_bytes().to_vec(),
        input.authentication_id.as_bytes().to_vec(),
        input
            .receipt_id
            .map(|id| id.as_bytes().to_vec())
            .unwrap_or_default(),
        input.hash.to_vec(),
        b"message".to_vec(),
        input.app_id.as_ref().as_bytes().to_vec(),
        created_at.timestamp_micros().to_string().into_bytes(),
        input.expires_at.timestamp_micros().to_string().into_bytes(),
    ])
}

fn time_field(option: Option<DateTime<Utc>>) -> Vec<u8> {
    option
        .map(|at| at.timestamp_micros().to_string().into_bytes())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use chrono::{DateTime, TimeDelta, Utc};
    use platform::cryptography::sha256;
    use tokio_postgres::types::{FromSql, Type};

    use crate::{
        history::{self, Kind, Record as _, Seed, State as _},
        identity::{
            authentication,
            session::state::{
                Cause, Data, Input, Payload, Record, RecordData, State, create, revoke,
            },
        },
        keys::seal,
    };

    fn id<T: for<'a> FromSql<'a>>(number: u8) -> T {
        let mut bytes = [0; 16];
        bytes[6] = 0x40;
        bytes[8] = 0x80;
        bytes[15] = number;
        T::from_sql(&Type::UUID, &bytes).expect("fixed UUID fixture")
    }

    fn time() -> DateTime<Utc> {
        DateTime::from_timestamp_micros(1_700_000_000_123_000).expect("fixture time")
    }

    fn input(receipt: bool) -> Input {
        Input {
            id: id(1),
            account_id: id(2),
            authentication_id: id(3),
            receipt_id: receipt.then(|| id(4)),
            hash: std::array::from_fn(|index| u8::try_from(index).expect("32 bytes")),
            app_id: "website-fixture".parse().expect("app identity"),
            expires_at: time() + TimeDelta::hours(1),
        }
    }

    fn data(receipt: bool, revoked_at: Option<DateTime<Utc>>) -> Data {
        Data {
            input: input(receipt),
            created_at: time(),
            revoked_at,
        }
    }

    fn issuance(receipt: bool) -> RecordData {
        RecordData {
            account_id: id(2),
            operation_id: id(5),
            payload: Payload::Issuance {
                input: input(receipt),
                created_at: time(),
            },
        }
    }

    fn revocation(cause: Cause, at: DateTime<Utc>) -> RecordData {
        RecordData {
            account_id: id(2),
            operation_id: id(5),
            payload: Payload::Revocation {
                cause,
                revoked_at: at,
            },
        }
    }

    fn digest<K: Kind>(value: &K) -> String {
        let fields = value.fields();
        let bytes = seal::encode_fields(
            K::NAME,
            K::VERSION,
            &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        )
        .expect("fixed encoding");
        hex::encode(sha256::Digest::new(&bytes).as_ref())
    }

    #[test]
    fn encodings_match_independent_python_vectors() {
        let inventory = authentication::inventory(authentication::keys());
        let keys = history::Keys {
            data: &inventory.encryption,
            seals: &inventory.seal,
        };
        let revoked_at = time() + TimeDelta::hours(2);
        // Python uuid.UUID.bytes, struct.pack(">I", len(field)) and hashlib.sha256.
        for (data, sequence, expected) in [
            (
                data(false, None),
                1,
                "cf2f96c846d472c960e50a37767d17870aca8b7102055d48017d9bd03ba71c09",
            ),
            (
                data(true, None),
                1,
                "ff133ca31d3243273c61d860d051264ccba22ad2de37d0762c55db679fff98fa",
            ),
            (
                data(true, Some(revoked_at)),
                2,
                "f8bc6717d47a0c9e3bb310b7e6d0bd019ab5b450ebf57e2516634f2c423ef870",
            ),
        ] {
            let state = Seed::state::<State>(data, sequence, sequence, keys).expect("state");
            assert_eq!(digest(&*state), expected);
        }
        for (data, sequence, expected) in [
            (
                issuance(false),
                1,
                "fe8b1d86bf6b4a5d6fa0e85556ea131fc16b524a017f8e156afc8212411d581a",
            ),
            (
                issuance(true),
                1,
                "a06fbf60ac66fd79b47a65d91ec3975c415ddfbe2520b86a22cb8b76d328f232",
            ),
            (
                revocation(Cause::Logout(id(5)), revoked_at),
                2,
                "158115c8e9439c0277a0776e0cdf3a5a5c78ae25e3ff6a22c1afc3710ca1dc1b",
            ),
            (
                revocation(Cause::Suspension(id(6)), revoked_at),
                2,
                "ada0e48c966ccf85134dd7dc10d4ff3f6f36e3ca57b586da91325d8f97b3c15d",
            ),
            (
                revocation(Cause::Transfer(id(7)), revoked_at),
                2,
                "060d610bb6f38531b67f3cc319a0312da7aa3b5777dbd0fe744e63255dbe7e4a",
            ),
        ] {
            let record = Seed::record::<Record>(id(1), sequence, data, keys).expect("record");
            assert_eq!(digest(&*record), expected);
        }
    }

    #[test]
    fn transitions_reject_invalid_lifetimes_and_repeated_revocation() {
        let inventory = authentication::inventory(authentication::keys());
        let keys = history::Keys {
            data: &inventory.encryption,
            seals: &inventory.seal,
        };
        for receipt in [false, true] {
            assert!(create(input(receipt), id(5), time()).is_ok());
        }
        for expires_at in [time(), time() - TimeDelta::microseconds(1)] {
            let mut input = input(false);
            input.expires_at = expires_at;
            assert!(create(input, id(5), time()).is_err());
        }
        let original = Seed::state::<State>(data(true, None), 1, 1, keys).expect("issued");
        assert!(
            revoke(
                &original,
                id(5),
                Cause::Logout(id(5)),
                time() - TimeDelta::microseconds(1)
            )
            .is_err()
        );
        let at = time() + TimeDelta::hours(2);
        for cause in [
            Cause::Logout(id(5)),
            Cause::Suspension(id(6)),
            Cause::Transfer(id(7)),
        ] {
            assert!(revoke(&original, id(5), cause, at).is_ok());
        }
        assert!(revoke(&original, id(5), Cause::Logout(id(8)), at).is_err());
        let revoked = Seed::state::<State>(data(true, Some(at)), 2, 2, keys).expect("revoked");
        assert!(revoke(&revoked, id(5), Cause::Logout(id(5)), at).is_err());
    }

    #[test]
    fn state_eligibility_and_time_checks_preserve_retained_terminal_facts() {
        let inventory = authentication::inventory(authentication::keys());
        let keys = history::Keys {
            data: &inventory.encryption,
            seals: &inventory.seal,
        };
        let state = Seed::state::<State>(data(false, None), 1, 1, keys).expect("issued");
        assert!(state.available(time()));
        assert!(!state.available(state.expires_at()));
        assert!(
            state
                .validate(state.expires_at() + TimeDelta::hours(1))
                .is_ok()
        );
        assert!(state.validate(time() - TimeDelta::microseconds(1)).is_err());
        let at = time() + TimeDelta::hours(2);
        let state = Seed::state::<State>(data(false, Some(at)), 2, 2, keys).expect("revoked");
        assert!(!state.available(at));
        assert!(state.validate(at).is_ok());
        assert!(state.validate(at - TimeDelta::microseconds(1)).is_err());
        let state = Seed::state::<State>(
            data(false, Some(time() - TimeDelta::microseconds(1))),
            2,
            2,
            keys,
        )
        .expect("damaged fixture");
        assert!(state.validate(at).is_err());
        for (sequence, version) in [(0, 1), (1, 2), (2, 1), (3, 3)] {
            let state = Seed::state::<State>(data(false, None), sequence, version, keys)
                .expect("damaged counters");
            assert!(state.validate(at).is_err());
        }
    }

    #[test]
    fn heads_and_predecessors_bind_the_original_session_and_issuance() {
        let inventory = authentication::inventory(authentication::keys());
        let keys = history::Keys {
            data: &inventory.encryption,
            seals: &inventory.seal,
        };
        let issued = Seed::state::<State>(data(true, None), 1, 1, keys).expect("issued");
        let first = Seed::record::<Record>(id(1), 1, issuance(true), keys).expect("issuance");
        assert!(issued.has_head(&first));
        assert!(!issued.has_head(
            &Seed::record::<Record>(id(1), 1, issuance(false), keys).expect("wrong source")
        ));
        let mut altered = issuance(true);
        if let Payload::Issuance { input, .. } = &mut altered.payload {
            input.hash[0] ^= 1;
        }
        assert!(
            !issued.has_head(
                &Seed::record::<Record>(id(1), 1, altered, keys).expect("changed digest")
            )
        );
        let at = time() + TimeDelta::hours(2);
        let revoked = Seed::state::<State>(data(true, Some(at)), 2, 2, keys).expect("revoked");
        let second = Seed::record::<Record>(id(1), 2, revocation(Cause::Logout(id(5)), at), keys)
            .expect("revocation");
        assert!(revoked.has_head(&second));
        assert!(second.follows(&first));
        assert!(!second.follows(&second));
        assert!(!issued.has_head(&second));
        let wrong_object =
            Seed::record::<Record>(id(8), 2, revocation(Cause::Logout(id(5)), at), keys)
                .expect("wrong object");
        assert!(!revoked.has_head(&wrong_object));
        assert!(!wrong_object.follows(&first));
        let mut wrong_account = revocation(Cause::Logout(id(5)), at);
        wrong_account.account_id = id(8);
        let wrong_account =
            Seed::record::<Record>(id(1), 2, wrong_account, keys).expect("wrong account");
        assert!(!revoked.has_head(&wrong_account));
        assert!(!wrong_account.follows(&first));
        let earlier = Seed::record::<Record>(
            id(1),
            2,
            revocation(Cause::Logout(id(5)), time() - TimeDelta::microseconds(1)),
            keys,
        )
        .expect("time damage");
        assert!(!earlier.follows(&first));
        assert_eq!(second.operation_id(), id(5));
    }
}
