//! Authenticated original-operation records shared by configuration and permanent commands.

use std::{convert::Infallible, fmt, sync::Arc};

use chrono::{DateTime, Utc};
use platform::cryptography::{Encoded, envelope};
use serde::Serialize;
use tokio::time::{Duration, Instant, timeout_at};
use tokio_postgres::{Client, Row};
use unicode_normalization::UnicodeNormalization;
use zeroize::Zeroizing;

use crate::{
    database::sealed::Sealed,
    history::{self, Keys},
    identity::{operation, person, person::revision},
    kernel::{
        App, Authorization,
        error::{Error, Fault, FaultKind, Stage},
        operation::Family,
        tx::{Connection, Tx},
    },
    keys::seal::Seal,
    runtime::{registry, work},
};

pub(crate) const CENTER: &str = "center.completions";
pub(crate) const CONTENT: &str = "content.completions";
pub(crate) const ACCESS: &str = "access.completions";
pub(crate) const WORKFORCE: &str = "workforce.completions";
pub(crate) const IDENTITY: &str = "identity.completions";

/// A closed family, action/target vocabulary, and its established replay subject rule.
#[derive(Clone, Copy)]
pub(crate) struct Template {
    family: Family,
    accepts: fn(&str, &str) -> bool,
    personal: bool,
}

impl Template {
    pub(crate) const fn new(family: Family, accepts: fn(&str, &str) -> bool) -> Self {
        Self {
            family,
            accepts,
            personal: false,
        }
    }

    pub(crate) const fn personal(family: Family, accepts: fn(&str, &str) -> bool) -> Self {
        Self {
            family,
            accepts,
            personal: true,
        }
    }

    pub(super) fn family(self) -> Family {
        self.family
    }

    pub(crate) fn expires(self, created_at: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self.family {
            Family::ContentColumn
            | Family::CenterAffiliation
            | Family::InstitutionRole
            | Family::WorkforceRole
            | Family::InstitutionAppointment
            | Family::Grant
            | Family::WorkforceAssignment
            | Family::GrantSuspension
            | Family::GrantResumption
            | Family::GrantTermination
            | Family::Invitation
            | Family::InvitationAdjustment
            | Family::InvitationWithdrawal
            | Family::Initialization
            | Family::Logout
            | Family::Suspension
            | Family::Resumption
            | Family::MobileCompletion
            | Family::Correction
            | Family::Cancellation
            | Family::SystemCorrection
            | Family::NumberAllocation
            | Family::NumberCorrection
            | Family::NumberRecovery => None,
            Family::WorkforceConfiguration => {
                created_at.checked_add_signed(chrono::Duration::hours(24))
            }
            #[cfg(test)]
            Family::Toy => created_at.checked_add_signed(chrono::Duration::hours(24)),
        }
    }

    fn accepts_expiry(self, created_at: DateTime<Utc>, expires_at: Option<DateTime<Utc>>) -> bool {
        if self.family == Family::MobileCompletion {
            // Success is permanent. A retained code rejection covers its confirmation's
            // deadline plus 24 hours, checked against that confirmation by its owner.
            expires_at.is_none_or(|expires| {
                created_at
                    .checked_add_signed(chrono::Duration::hours(24))
                    .is_some_and(|minimum| expires >= minimum)
            })
        } else {
            expires_at == self.expires(created_at)
        }
    }
}

/// Attribution is permanent; replay does not replace it with a later current identity.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Subject {
    Session {
        person_id: person::Id,
        revision_id: revision::Id,
    },
    System {
        person_id: person::Id,
        revision_id: revision::Id,
    },
    Bootstrap {
        person_id: person::Id,
        revision_id: revision::Id,
    },
}

impl Subject {
    fn of(authority: &Authorization) -> Self {
        Self::Session {
            person_id: authority.person_id,
            revision_id: authority.revision_id,
        }
    }

    fn person(self) -> person::Id {
        match self {
            Self::Session { person_id, .. }
            | Self::System { person_id, .. }
            | Self::Bootstrap { person_id, .. } => person_id,
        }
    }

    fn revision(self) -> revision::Id {
        match self {
            Self::Session { revision_id, .. }
            | Self::System { revision_id, .. }
            | Self::Bootstrap { revision_id, .. } => revision_id,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Session { .. } => "session",
            Self::System { .. } => "system",
            Self::Bootstrap { .. } => "bootstrap",
        }
    }

    fn accepts(self, family: Family, action: &str) -> bool {
        match (family, self) {
            (Family::Initialization, Self::Bootstrap { .. }) => action == "initialize",
            (Family::InstitutionAppointment, Self::Session { .. }) => action == "assign",
            (Family::InstitutionAppointment, Self::System { .. }) => {
                matches!(action, "grant" | "revoke")
            }
            (Family::SystemCorrection, Self::System { .. }) => action == "correct",
            (Family::CenterAffiliation, Self::Session { .. }) => {
                matches!(action, "fill" | "correct")
            }
            (Family::CenterAffiliation, Self::System { .. }) => action == "system_correct",
            (
                Family::Initialization | Family::InstitutionAppointment | Family::SystemCorrection,
                _,
            ) => false,
            (Family::CenterAffiliation, _) => false,
            (_, Self::Session { .. }) => true,
            _ => false,
        }
    }
}

/// Canonical input remains zeroized and is never formatted into diagnostics.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Request {
    action: &'static str,
    target: String,
    input: Zeroizing<Vec<u8>>,
    subject: Subject,
}

impl fmt::Debug for Request {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Request(..)")
    }
}

impl Request {
    pub(crate) fn system<R>(
        action: &'static str,
        target: String,
        input: &impl Serialize,
        authority: &Authorization,
    ) -> Result<Self, Error<R>> {
        let mut request = Self::new(action, target, input, authority)?;
        request.subject = Subject::System {
            person_id: authority.person_id,
            revision_id: authority.revision_id,
        };
        Ok(request)
    }

    pub(crate) fn bootstrap<R>(
        target: String,
        input: &impl Serialize,
        authority: &Authorization,
    ) -> Result<Self, Error<R>> {
        let mut request = Self::new("initialize", target, input, authority)?;
        request.subject = Subject::Bootstrap {
            person_id: authority.person_id,
            revision_id: authority.revision_id,
        };
        Ok(request)
    }

    pub(crate) fn new<R>(
        action: &'static str,
        target: String,
        input: &impl Serialize,
        authority: &Authorization,
    ) -> Result<Self, Error<R>> {
        let input = match serde_json::to_vec(input) {
            Ok(input) => Zeroizing::new(input),
            Err(error) => {
                return Err(Error::Unavailable(Fault::new(
                    Stage::Encoding,
                    FaultKind::Encoding(error),
                )));
            }
        };
        Ok(Self {
            action,
            target,
            input,
            subject: Subject::of(authority),
        })
    }

    pub(crate) fn person_id(&self) -> person::Id {
        self.subject.person()
    }
    pub(crate) fn revision_id(&self) -> revision::Id {
        self.subject.revision()
    }
    pub(crate) fn target(&self) -> &str {
        &self.target
    }
}

/// The AEAD identity uses length-prefixed fields, including the subject's closed tag.
struct Binding(Encoded);

impl Binding {
    fn new(
        family: Family,
        operation: operation::Id,
        subject: Subject,
    ) -> Result<Self, history::Error> {
        let person = subject.person();
        let revision = subject.revision();
        let encoded = Encoded::fields(&[
            family.label().as_bytes(),
            operation.as_bytes(),
            subject.label().as_bytes(),
            person.as_bytes(),
            revision.as_bytes(),
        ])
        .map_err(crate::keys::seal::Error::from)?;
        Ok(Self(encoded))
    }
}

impl envelope::Binding for Binding {
    fn domain(&self) -> &[u8] {
        b"nucleus/completion/v1"
    }
    fn identity(&self) -> &[u8] {
        self.0.as_ref()
    }
    fn field(&self) -> &[u8] {
        b"completion.input"
    }
}

struct Input<'a> {
    binding: Binding,
    plaintext: &'a [u8],
}

impl envelope::Binding for Input<'_> {
    fn domain(&self) -> &[u8] {
        self.binding.domain()
    }
    fn identity(&self) -> &[u8] {
        self.binding.identity()
    }
    fn field(&self) -> &[u8] {
        self.binding.field()
    }
}
impl envelope::Input for Input<'_> {
    fn plaintext(&self) -> &[u8] {
        self.plaintext
    }
}

/// A retained completion; callers authenticate its canonical plaintext before use.
pub(crate) struct Record {
    family: Family,
    operation_id: operation::Id,
    action: String,
    target: String,
    input: Sealed,
    subject: Subject,
    created_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    seal: Seal,
}

impl fmt::Debug for Record {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Record(..)")
    }
}

impl Record {
    fn fields(&self, input: &[u8]) -> Zeroizing<Vec<Vec<u8>>> {
        Zeroizing::new(vec![
            self.family.label().as_bytes().to_vec(),
            self.operation_id.as_bytes().to_vec(),
            self.action.as_bytes().to_vec(),
            self.target.as_bytes().to_vec(),
            input.to_vec(),
            self.subject.label().as_bytes().to_vec(),
            self.subject.person().as_bytes().to_vec(),
            self.subject.revision().as_bytes().to_vec(),
            self.created_at.timestamp_micros().to_string().into_bytes(),
            self.expires_at
                .map(|value| value.timestamp_micros().to_string().into_bytes())
                .unwrap_or_default(),
        ])
    }

    fn decode<R>(template: Template, row: &Row, keys: Keys<'_>) -> Result<Self, Error<R>> {
        let record = Self::stored(template, row)?;
        let _ = record.verify(keys)?;
        Ok(record)
    }

    fn stored<R>(template: Template, row: &Row) -> Result<Self, Error<R>> {
        let family: String = row.try_get("family").map_err(Error::decode)?;
        let subject: String = row.try_get("subject").map_err(Error::decode)?;
        let action: String = row.try_get("action").map_err(Error::decode)?;
        let target: String = row.try_get("target").map_err(Error::decode)?;
        if family != template.family.label()
            || !canonical(&target)
            || !(template.accepts)(&action, &target)
        {
            return Err(Error::corrupt(
                "completion retains its family, subject and action/target vocabulary",
            ));
        }
        let person_id = row.try_get("person_id").map_err(Error::decode)?;
        let revision_id = row.try_get("revision_id").map_err(Error::decode)?;
        let subject = match subject.as_str() {
            "session" => Subject::Session {
                person_id,
                revision_id,
            },
            "system" => Subject::System {
                person_id,
                revision_id,
            },
            "bootstrap" => Subject::Bootstrap {
                person_id,
                revision_id,
            },
            _ => return Err(Error::corrupt("completion retains its declared subject")),
        };
        if !subject.accepts(template.family, &action) {
            return Err(Error::corrupt(
                "completion subject belongs to its action family",
            ));
        }
        let record = Self {
            family: template.family,
            operation_id: row.try_get("operation_id").map_err(Error::decode)?,
            action,
            target,
            input: row.try_get("input").map_err(Error::decode)?,
            subject,
            created_at: row.try_get("created_at").map_err(Error::decode)?,
            expires_at: row.try_get("expires_at").map_err(Error::decode)?,
            seal: Seal::stored(
                row.try_get("seal").map_err(Error::decode)?,
                row.try_get("seal_key_id").map_err(Error::decode)?,
                row.try_get("seal_version").map_err(Error::decode)?,
            )
            .map_err(history::Error::from)?,
        };
        if !template.accepts_expiry(record.created_at, record.expires_at) {
            return Err(Error::corrupt(
                "completion expiry follows its family's retention",
            ));
        }
        Ok(record)
    }

    fn verify(&self, keys: Keys<'_>) -> Result<Zeroizing<Vec<u8>>, history::Error> {
        let input = self.open(keys)?;
        let fields = self.fields(&input);
        keys.seals.check(
            "kernel.completion",
            1,
            &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
            &self.seal,
        )?;
        Ok(input)
    }

    fn open(&self, keys: Keys<'_>) -> Result<Zeroizing<Vec<u8>>, history::Error> {
        Ok(keys.data.open(
            &self.input.0,
            &Binding::new(self.family, self.operation_id, self.subject)?,
        )?)
    }

    pub(crate) fn operation_id(&self) -> operation::Id {
        self.operation_id
    }
    pub(crate) fn action(&self) -> &str {
        &self.action
    }
    pub(crate) fn target(&self) -> &str {
        &self.target
    }
    pub(crate) fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    pub(crate) fn expires_at(&self) -> Option<DateTime<Utc>> {
        self.expires_at
    }

    pub(crate) fn input<T: serde::de::DeserializeOwned, R>(
        &self,
        keys: Keys<'_>,
    ) -> Result<T, Error<R>> {
        let input = self.verify(keys)?;
        serde_json::from_slice(&input).map_err(|_| {
            Error::corrupt("a completion retains its capability's canonical input shape")
        })
    }
    pub(crate) fn person_id(&self) -> person::Id {
        self.subject.person()
    }
    pub(crate) fn revision_id(&self) -> revision::Id {
        self.subject.revision()
    }
    pub(crate) fn system(&self) -> bool {
        matches!(self.subject, Subject::System { .. })
    }

    pub(super) fn authority(&self, now: DateTime<Utc>) -> Authorization {
        Authorization {
            person_id: self.person_id(),
            revision_id: self.revision_id(),
            now,
        }
    }

    pub(crate) fn matches<R>(
        &self,
        template: Template,
        keys: Keys<'_>,
        request: &Request,
    ) -> Result<(), Error<R>> {
        let input = self.verify(keys)?;
        if self.subject.label() != request.subject.label()
            || (template.personal && self.person_id() != request.person_id())
            || self.action != request.action
            || self.target != request.target
            || *input != *request.input
        {
            return Err(Error::Mismatch);
        }
        Ok(())
    }

    pub(crate) fn check_time<R>(&self, now: DateTime<Utc>) -> Result<(), Error<R>> {
        if self.created_at > now {
            return Err(Error::corrupt(
                "completion time does not exceed the decisive clock",
            ));
        }
        Ok(())
    }
}

fn canonical(value: &str) -> bool {
    !value.is_empty()
        && !value.chars().any(char::is_control)
        && value.trim() == value
        && value.nfc().eq(value.chars())
}

const COLUMNS: &str = "family, operation_id, action, target, input, subject, person_id, revision_id, created_at, expires_at, seal, seal_key_id, seal_version";

/// A shared table cannot hide malformed family labels from the family-specific scans.
pub(crate) async fn verify_role_families<R>(client: &Client) -> Result<(), Error<R>> {
    let row = crate::database::verification::Stage::HistoryRecords
        .batch(Duration::from_secs(10), client.query_one(
            "SELECT NOT EXISTS (SELECT 1 FROM access.completions WHERE family IS NULL OR family NOT IN ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12))",
            &[&Family::InstitutionRole.label(), &Family::WorkforceRole.label(),
                &Family::InstitutionAppointment.label(), &Family::Initialization.label(),
                &Family::Grant.label(), &Family::WorkforceAssignment.label(), &Family::GrantSuspension.label(),
                &Family::GrantResumption.label(), &Family::GrantTermination.label(),
                &Family::Invitation.label(), &Family::InvitationAdjustment.label(), &Family::InvitationWithdrawal.label()],
        )).await.map_err(history::Error::from)?.map_err(Error::query)?;
    if !row.try_get::<_, bool>(0).map_err(Error::decode)? {
        return Err(Error::corrupt(
            "Access completions retain one of their declared families",
        ));
    }
    Ok(())
}

/// All Workforce family scans share this vocabulary check.
pub(crate) async fn verify_workforce_families<R>(client: &Client) -> Result<(), Error<R>> {
    let row = crate::database::verification::Stage::HistoryRecords
        .batch(Duration::from_secs(10), client.query_one(
            "SELECT NOT EXISTS (SELECT 1 FROM workforce.completions WHERE family IS NULL OR family NOT IN ($1,$2,$3,$4))",
            &[&Family::WorkforceConfiguration.label(), &Family::NumberAllocation.label(), &Family::NumberCorrection.label(), &Family::NumberRecovery.label()],
        )).await.map_err(history::Error::from)?.map_err(Error::query)?;
    if !row.try_get::<_, bool>(0).map_err(Error::decode)? {
        return Err(Error::corrupt(
            "Workforce completions retain a known family",
        ));
    }
    Ok(())
}

/// All Identity family scans share this vocabulary check.
pub(crate) async fn verify_identity_families<R>(client: &Client) -> Result<(), Error<R>> {
    let row = crate::database::verification::Stage::HistoryRecords
        .batch(Duration::from_secs(10), client.query_one(
            "SELECT NOT EXISTS (SELECT 1 FROM identity.completions WHERE family IS NULL OR family NOT IN ($1,$2,$3,$4,$5,$6,$7))",
            &[&Family::Logout.label(), &Family::Suspension.label(), &Family::Resumption.label(), &Family::MobileCompletion.label(), &Family::SystemCorrection.label(), &Family::Correction.label(), &Family::Cancellation.label()],
        )).await.map_err(history::Error::from)?.map_err(Error::query)?;
    if !row.try_get::<_, bool>(0).map_err(Error::decode)? {
        return Err(Error::corrupt(
            "Identity completions retain one of their declared families",
        ));
    }
    Ok(())
}

/// System and bootstrap callers identify the retained input before current authorization.
pub(crate) async fn original<R>(
    client: &Client,
    keys: Keys<'_>,
    template: Template,
    operation_id: operation::Id,
) -> Result<Option<Record>, Error<R>> {
    let option = client
        .query_opt(
            &format!(
                "SELECT {COLUMNS} FROM {} WHERE operation_id=$1 AND family=$2",
                template.family.completions()
            ),
            &[&operation_id, &template.family.label()],
        )
        .await
        .map_err(Error::query)?;
    let Some(row) = option else { return Ok(None) };
    let record = Record::decode(template, &row, keys)?;
    Ok(Some(record))
}

/// Authenticates the retained completions of one family for a bounded set of operations.
/// Callers still compare the canonical input and every fact they depend on.
pub(crate) async fn retained<G, R>(
    client: &G,
    keys: Keys<'_>,
    template: Template,
    operations: &[operation::Id],
) -> Result<Vec<Record>, Error<R>>
where
    G: tokio_postgres::GenericClient + Sync,
{
    if operations.is_empty() {
        return Ok(Vec::new());
    }
    let rows = client
        .query(
            &format!(
                "SELECT {COLUMNS} FROM {} WHERE family=$1 AND operation_id=ANY($2)",
                template.family.completions()
            ),
            &[&template.family.label(), &operations],
        )
        .await
        .map_err(Error::query)?;
    rows.iter()
        .map(|row| Record::decode(template, row, keys))
        .collect()
}

/// The caller owns this family's original-operation lock and deletes its dependent
/// short-lived evidence in this same transaction. Permanent originals cannot be removed.
pub(crate) async fn remove_expired<R>(
    client: &Client,
    keys: Keys<'_>,
    template: Template,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<(), Error<R>> {
    let record = original(client, keys, template, operation_id)
        .await?
        .ok_or(Error::corrupt(
            "a retained deletion has its original completion",
        ))?;
    record.check_time(now)?;
    if record.expires_at.is_none_or(|expires| expires > now) {
        return Err(Error::corrupt(
            "only an expired completion can be reclaimed",
        ));
    }
    let changed = client.execute(
        &format!("DELETE FROM {} WHERE family=$1 AND operation_id=$2 AND seal=$3 AND seal_key_id=$4 AND seal_version=$5", template.family.completions()),
        &[&template.family.label(), &operation_id, &record.seal.tag(), &record.seal.key_id(), &record.seal.version()],
    ).await.map_err(Error::query)?;
    if changed != 1 {
        return Err(Error::corrupt(
            "completion cleanup removes its authenticated candidate",
        ));
    }
    Ok(())
}

pub(crate) async fn find<R>(
    client: &Client,
    keys: Keys<'_>,
    template: Template,
    operation_id: operation::Id,
    request: &Request,
) -> Result<Option<Record>, Error<R>> {
    let option = client
        .query_opt(
            &format!(
                "SELECT {COLUMNS} FROM {} WHERE operation_id = $1 AND (family = $2 OR $3)",
                template.family.completions()
            ),
            &[
                &operation_id,
                &template.family.label(),
                &!template.family.shares_completions(),
            ],
        )
        .await
        .map_err(Error::query)?;
    let Some(row) = option else {
        return Ok(None);
    };
    // This comparison only rejects unrelated callers; it never qualifies the stored row.
    // A matching caller must still authenticate the full subject and canonical input.
    if template.personal
        && row
            .try_get::<_, person::Id>("person_id")
            .map_err(Error::decode)?
            != request.subject.person()
    {
        return Err(Error::Mismatch);
    }
    let record = Record::stored(template, &row)?;
    record.matches(template, keys, request)?;
    Ok(Some(record))
}

pub(crate) async fn insert<R>(
    client: &Client,
    keys: Keys<'_>,
    template: Template,
    operation_id: operation::Id,
    request: &Request,
    now: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
) -> Result<(), Error<R>> {
    if !canonical(&request.target)
        || !(template.accepts)(request.action, &request.target)
        || !request.subject.accepts(template.family, request.action)
        || !template.accepts_expiry(now, expires_at)
    {
        return Err(Error::corrupt(
            "a command supplies its declared completion action and target",
        ));
    }
    let input = Sealed(
        keys.data
            .seal(&Input {
                binding: Binding::new(template.family, operation_id, request.subject)?,
                plaintext: &request.input,
            })
            .map_err(history::Error::from)?,
    );
    // Compute the seal before constructing the stored record; no dummy seal is needed.
    let time = now.timestamp_micros().to_string();
    let expiry = expires_at
        .map(|value| value.timestamp_micros().to_string())
        .unwrap_or_default();
    let seal = keys
        .seals
        .sign(
            "kernel.completion",
            1,
            &[
                template.family.label().as_bytes(),
                operation_id.as_bytes(),
                request.action.as_bytes(),
                request.target.as_bytes(),
                &request.input,
                request.subject.label().as_bytes(),
                request.subject.person().as_bytes(),
                request.subject.revision().as_bytes(),
                time.as_bytes(),
                expiry.as_bytes(),
            ],
        )
        .map_err(history::Error::from)?;
    client
        .execute(
            &format!(
                "INSERT INTO {} ({COLUMNS}) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
                template.family.completions()
            ),
            &[
                &template.family.label(),
                &operation_id,
                &request.action,
                &request.target,
                &input,
                &request.subject.label(),
                &request.subject.person(),
                &request.subject.revision(),
                &now,
                &expires_at,
                &seal.tag(),
                &seal.key_id(),
                &seal.version(),
            ],
        )
        .await
        .map_err(Error::query)?;
    Ok(())
}

/// Startup authenticates every retained completion with bounded primary-key batches.
pub(crate) async fn fetch_all<R>(
    client: &Client,
    keys: Keys<'_>,
    template: Template,
) -> Result<Vec<Record>, Error<R>> {
    let mut result = Vec::new();
    let mut cursor: Option<operation::Id> = None;
    loop {
        let sql = format!(
            "SELECT {COLUMNS} FROM {} WHERE ($1::uuid IS NULL OR operation_id > $1) AND (family = $2 OR $3) ORDER BY operation_id LIMIT 128",
            template.family.completions()
        );
        let rows = crate::database::verification::Stage::HistoryRecords
            .batch(
                Duration::from_secs(10),
                client.query(
                    &sql,
                    &[
                        &cursor,
                        &template.family.label(),
                        &!template.family.shares_completions(),
                    ],
                ),
            )
            .await
            .map_err(history::Error::from)?
            .map_err(Error::query)?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            let record = Record::decode(template, &row, keys)?;
            cursor = Some(record.operation_id);
            result.push(record);
        }
    }
    Ok(result)
}

const BATCH: usize = 128;
const BUDGET: Duration = Duration::from_secs(10);

/// Reclaims expired completions of one family while retaining original-operation ownership.
/// Cleanup deletes only the completion, never the configuration it completed.
pub struct Retention {
    app: App,
    template: Template,
    cursor: Option<(DateTime<Utc>, operation::Id)>,
}

impl Retention {
    pub(crate) fn new(app: App, template: Template) -> Self {
        Self {
            app,
            template,
            cursor: None,
        }
    }

    pub async fn run(&mut self) -> Result<(), Error<Infallible>> {
        let registry = self.app.registry();
        registry.ready().map_err(Error::from)?;
        let candidates = match self.fetch_candidates().await {
            Ok(candidates) => candidates,
            Err(error) if error.is_closed() || registry.ready().is_err() => return Err(error),
            Err(error) if error.is_corrupt() => {
                registry.close();
                return Err(error);
            }
            Err(error) => {
                tracing::error!(stage = "completion_retention_scan", family = self.template.family.label(), error = ?error);
                return Ok(());
            }
        };
        let complete = candidates.len() < BATCH;
        for candidate in candidates {
            self.cursor = Some((candidate.created_at, candidate.operation_id));
            let mut permit = match registry
                .try_acquire_for(
                    registry::Family::Kernel(self.template.family),
                    candidate.operation_id,
                )
                .await
            {
                Ok(permit) => permit,
                Err(work::Error::Busy) => continue,
                Err(error) => return Err(error.into()),
            };
            permit.begin();
            let closing = Arc::clone(registry);
            let guard = scopeguard::guard((), |_| {
                closing.close();
                tracing::error!(operation_id = %candidate.operation_id, stage = "completion_retention", error = ?work::Error::Worker);
            });
            let result = self.reclaim(&candidate).await;
            scopeguard::ScopeGuard::into_inner(guard);
            permit.finish();
            if let Err(error) = result {
                tracing::error!(operation_id = %candidate.operation_id, stage = "completion_retention", family = self.template.family.label(), error = ?error);
                if error.is_corrupt() {
                    registry.close();
                    return Err(error);
                }
                if error.is_closed() || registry.ready().is_err() {
                    return Err(error);
                }
            }
        }
        if complete {
            self.cursor = None;
        }
        Ok(())
    }

    async fn fetch_candidates(&self) -> Result<Vec<Record>, Error<Infallible>> {
        let context = self.app.context();
        let lease = self
            .app
            .registry()
            .lease_until_closed::<Waiting>(context.connections())
            .await
            .map_err(Waiting::into_error)?;
        let (time, operation) = self.cursor.unzip();
        let result = timeout_at(Instant::now() + BUDGET, async {
            let now = context.now(lease.client()).await.map_err(Error::clock)?;
            lease.client().query(&format!("SELECT {COLUMNS} FROM {} WHERE expires_at <= $1 AND ($2::timestamptz IS NULL OR (created_at, operation_id) > ($2, $3::uuid)) AND family=$4 ORDER BY created_at, operation_id LIMIT 128", self.template.family.completions()), &[&now, &time, &operation, &self.template.family.label()]).await.map_err(Error::query)
        })
        .await;
        let cause = match result {
            Ok(Ok(rows)) => {
                lease.release();
                self.app.registry().ready().map_err(Error::from)?;
                return rows
                    .iter()
                    .map(|row| Record::decode(self.template, row, self.app.keys()))
                    .collect();
            }
            Ok(Err(error)) => error.at(Stage::Retention),
            Err(elapsed) => {
                Error::Unavailable(Fault::new(Stage::Retention, FaultKind::Timeout(elapsed)))
            }
        };
        Err(match (cause, lease.discard().await) {
            (cause, Ok(())) => cause,
            (Error::Unavailable(fault), Err(termination)) => Error::Unavailable(fault.then(
                Fault::new(Stage::Termination, FaultKind::Connection(termination)),
            )),
            (cause, Err(_)) => cause,
        })
    }

    async fn reclaim(&self, candidate: &Record) -> Result<(), Error<Infallible>> {
        let context = self.app.context();
        let lease = context
            .connections()
            .lease()
            .await
            .map_err(Error::connection)?;
        let (tx, begun) = Tx::begin(
            context,
            Connection::Online(lease),
            Instant::now() + BUDGET,
            self.template.family,
            candidate.operation_id,
        )
        .await;
        let result = match begun {
            Ok(()) => timeout_at(tx.deadline(), self.delete(&tx, candidate))
                .await
                .unwrap_or_else(Error::elapsed),
            Err(fault) => Err(Error::Unavailable(fault)),
        };
        let cause = match result {
            Ok(()) => match tx.commit().await {
                Ok(()) => return Ok(()),
                Err(fault) => fault,
            },
            Err(Error::Unavailable(fault)) | Err(Error::Unknown(fault)) => {
                fault.then_result(tx.discard().await)
            }
            Err(Error::Corrupt(fault)) => {
                return Err(Error::Corrupt(fault.then_result(tx.discard().await)));
            }
            Err(error) => {
                let _ = tx.discard().await;
                return Err(error);
            }
        };
        match self.deleted(candidate).await {
            Ok((true, end)) => {
                tracing::error!(operation_id = %candidate.operation_id, stage = "completion_retention", outcome = "deleted", error = ?cause.then_result(end));
                Ok(())
            }
            Ok((false, end)) => Err(Error::Unavailable(cause.then_result(end))),
            Err(recovery) => {
                if recovery.is_corrupt() {
                    self.app.registry().close();
                }
                Err(Error::Unknown(cause.then_error(recovery)))
            }
        }
    }

    async fn delete(&self, tx: &Tx, candidate: &Record) -> Result<(), Error<Infallible>> {
        let now = tx.now(self.app.context()).await?;
        let Some(record) = self.current(tx, candidate.operation_id).await? else {
            return Ok(());
        };
        if record.seal != candidate.seal {
            return Ok(());
        }
        tx.client().execute(&format!("DELETE FROM {} WHERE operation_id=$1 AND seal=$2 AND seal_key_id=$3 AND seal_version=$4 AND expires_at <= $5 AND family=$6", self.template.family.completions()), &[&candidate.operation_id, &candidate.seal.tag(), &candidate.seal.key_id(), &candidate.seal.version(), &now, &self.template.family.label()]).await.map_err(Error::query)?;
        Ok(())
    }

    async fn current(
        &self,
        tx: &Tx,
        operation: operation::Id,
    ) -> Result<Option<Record>, Error<Infallible>> {
        let row = tx
            .client()
            .query_opt(
                &format!(
                    "SELECT {COLUMNS} FROM {} WHERE operation_id=$1 AND family=$2",
                    self.template.family.completions()
                ),
                &[&operation, &self.template.family.label()],
            )
            .await
            .map_err(Error::query)?;
        row.as_ref()
            .map(|row| Record::decode(self.template, row, self.app.keys()))
            .transpose()
    }

    /// The original deletion is resolved under the same original-operation lock.
    async fn deleted(
        &self,
        candidate: &Record,
    ) -> Result<(bool, Result<(), Fault>), Error<Infallible>> {
        let context = self.app.context();
        let session = context
            .connections()
            .recover()
            .await
            .map_err(Error::connection)?;
        let (tx, begun) = Tx::begin(
            context,
            Connection::Recovery(session),
            Instant::now() + BUDGET,
            self.template.family,
            candidate.operation_id,
        )
        .await;
        let result = match begun {
            Ok(()) => timeout_at(tx.deadline(), async {
                Ok(self
                    .current(&tx, candidate.operation_id)
                    .await?
                    .is_none_or(|record| record.seal != candidate.seal))
            })
            .await
            .unwrap_or_else(Error::elapsed),
            Err(fault) => Err(Error::Unavailable(fault)),
        };
        match result {
            Ok(deleted) => Ok((deleted, tx.commit().await)),
            Err(error) => {
                let error = error.at(Stage::Recovery);
                let end = tx.discard().await;
                Err(match error {
                    Error::Unavailable(fault) => Error::Unavailable(fault.then_result(end)),
                    Error::Unknown(fault) => Error::Unknown(fault.then_result(end)),
                    Error::Corrupt(fault) => Error::Corrupt(fault.then_result(end)),
                    error => error,
                })
            }
        }
    }
}

/// Discovery's pool wait reports closure or the connection failure through this adapter.
enum Waiting {
    Closed,
    Connection(platform::database::Error),
}

impl From<work::Error> for Waiting {
    fn from(_: work::Error) -> Self {
        Self::Closed
    }
}

impl From<platform::database::Error> for Waiting {
    fn from(error: platform::database::Error) -> Self {
        Self::Connection(error)
    }
}

impl Waiting {
    fn into_error(self) -> Error<Infallible> {
        match self {
            Self::Closed => Error::from(work::Error::Closed),
            Self::Connection(error) => Error::connection(error),
        }
    }
}

#[cfg(test)]
pub(crate) struct Seed;

#[cfg(test)]
impl Seed {
    async fn record(
        client: &Client,
        keys: Keys<'_>,
        template: Template,
        operation: operation::Id,
    ) -> Result<Record, Error<Infallible>> {
        let row = client
            .query_one(
                &format!(
                    "SELECT {COLUMNS} FROM {} WHERE operation_id=$1 AND family=$2",
                    template.family.completions()
                ),
                &[&operation, &template.family.label()],
            )
            .await
            .map_err(Error::query)?;
        Record::decode(template, &row, keys)
    }

    pub(crate) async fn age(
        client: &Client,
        keys: Keys<'_>,
        template: Template,
        operation: operation::Id,
        age: chrono::Duration,
    ) -> Result<(), Error<Infallible>> {
        let mut record = Self::record(client, keys, template, operation).await?;
        record.created_at -= age;
        record.expires_at = record.expires_at.map(|at| at - age);
        let input = record.open(keys)?;
        let fields = record.fields(&input);
        let seal = keys
            .seals
            .sign(
                "kernel.completion",
                1,
                &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
            )
            .map_err(history::Error::from)?;
        client.execute(&format!("UPDATE {} SET created_at=$2, expires_at=$3, seal=$4, seal_key_id=$5, seal_version=$6 WHERE operation_id=$1 AND family=$7", template.family.completions()), &[&operation, &record.created_at, &record.expires_at, &seal.tag(), &seal.key_id(), &seal.version(), &template.family.label()]).await.map_err(Error::query)?;
        Ok(())
    }

    pub(crate) async fn age_family(
        client: &Client,
        keys: Keys<'_>,
        template: Template,
        elapsed: chrono::TimeDelta,
    ) -> Result<(), Error<Infallible>> {
        for record in fetch_all::<Infallible>(client, keys, template).await? {
            Self::age(client, keys, template, record.operation_id(), elapsed).await?;
        }
        Ok(())
    }

    pub(crate) async fn input(
        client: &Client,
        keys: Keys<'_>,
        template: Template,
        operation: operation::Id,
    ) -> Result<Zeroizing<Vec<u8>>, Error<Infallible>> {
        Ok(Self::record(client, keys, template, operation)
            .await?
            .open(keys)?)
    }

    pub(crate) async fn insert(
        client: &Client,
        keys: Keys<'_>,
        template: Template,
        operation: operation::Id,
        source: operation::Id,
        (action, target): (&'static str, String),
        input: &impl Serialize,
    ) -> Result<(), Error<Infallible>> {
        let record = Self::record(client, keys, template, source).await?;
        let now = client
            .query_one("SELECT clock_timestamp()", &[])
            .await
            .map_err(Error::query)?
            .try_get(0)
            .map_err(Error::decode)?;
        let authority = Authorization {
            person_id: record.subject.person(),
            revision_id: record.subject.revision(),
            now,
        };
        let request = Request::new(action, target, input, &authority)?;
        insert(
            client,
            keys,
            template,
            operation,
            &request,
            now,
            template.expires(now),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{convert::Infallible, sync::Arc, time::Duration};

    use fixture::database_proxy::{self, Proxy};
    use tokio::{runtime::Runtime, time::timeout};

    use crate::{
        identity::{
            authentication::{self, Database, server},
            operation,
        },
        kernel::{
            App, Error, FaultKind, Stage,
            completion::{Record, Retention, Seed, fetch_all},
            executor::tests::{COMPLETIONS, counts, operation, start, stop, toy},
        },
    };

    #[test]
    fn completion_encoding_and_envelope_match_independent_python_vectors() {
        use chrono::DateTime;
        use platform::cryptography::{
            Encoded,
            envelope::{Binding as _, Envelope},
            sha256,
        };
        use zeroize::Zeroizing;

        use super::{Binding, Input, Subject};
        use crate::{
            database::sealed::Sealed,
            history,
            identity::authentication,
            kernel::operation::Family,
            keys::{self, Derived, Entry, Material, seal},
        };

        let key_id = keys::Id::new(0).expect("key");
        let material = || {
            Derived::fixed(
                key_id,
                vec![Entry::new(
                    key_id,
                    Material(Zeroizing::new(std::array::from_fn(|index| {
                        u8::try_from(index).expect("32 bytes")
                    }))),
                )],
            )
        };
        let seals = seal::Keys::new(material());
        let encryption = crate::kernel::keys::Keys::new(material());
        let keys = history::Keys {
            data: &encryption,
            seals: &seals,
        };
        let operation = "00000000-0000-4000-8000-000000000005"
            .parse()
            .expect("operation");
        let subject = Subject::Session {
            person_id: serde_json::from_str("\"00000000-0000-4000-8000-000000000003\"")
                .expect("operator"),
            revision_id: serde_json::from_str("\"00000000-0000-4000-8000-000000000004\"")
                .expect("revision"),
        };
        let binding = Binding::new(Family::CenterAffiliation, operation, subject).expect("binding");
        let bytes = hex::decode("010000000000000000000102030405060708090a0b702d5ff009f3413efed09f2d30bdbcc6c21569ffb41111d60df521c1fde907c2f89f175f").expect("Python AESGCMSIV vector");
        let aad = Encoded::fields(&[
            binding.domain(),
            &bytes[..21],
            binding.identity(),
            binding.field(),
        ])
        .expect("AAD");
        assert_eq!(aad.as_ref().len(), 167);
        assert_eq!(
            hex::encode(sha256::Digest::new(&aad).as_ref()),
            "ff3ea82997a8dc611e7ddfd287869d77674c64dfde5ab19ec840fd41db5a21ff"
        );
        let mut record = Record {
            family: Family::CenterAffiliation,
            operation_id: operation,
            action: "correct".into(),
            target: "00000000-0000-4000-8000-000000000001".into(),
            input: Sealed(Envelope::new(bytes).expect("envelope")),
            subject,
            created_at: DateTime::from_timestamp_micros(1_700_000_000_123_000).expect("time"),
            expires_at: None,
            seal: seal::Seal::stored(
                hex::decode("b355b7447e73c7015dc231ba722c2e515d7d8d1f41d456af33f686c43ae6558c")
                    .expect("Python HMAC"),
                key_id,
                1,
            )
            .expect("seal"),
        };
        let plaintext = br#"{"affiliation":"01"}"#;
        assert_eq!(
            record
                .verify(keys)
                .expect("independent seal and ciphertext")
                .as_slice(),
            plaintext
        );
        let fields = record.fields(plaintext);
        let encoded = seal::encode_fields(
            "kernel.completion",
            1,
            &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        )
        .expect("seal encoding");
        assert_eq!(encoded.as_ref().len(), 237);
        assert_eq!(
            hex::encode(sha256::Digest::new(&encoded).as_ref()),
            "f66a3717f2311efb988a16f20f5f5e0e307e6d73b8a8a3f2b3d33585794509ef"
        );
        // Re-encryption changes envelope bytes, while the seal still authenticates the same input.
        let inventory = authentication::inventory(authentication::keys());
        record.input = Sealed(
            inventory
                .encryption
                .seal(&Input { binding, plaintext })
                .expect("different key and nonce"),
        );
        record
            .verify(history::Keys {
                data: &inventory.encryption,
                seals: &seals,
            })
            .expect("seal survives re-encryption");
        record.action = "fill".into();
        assert!(
            record
                .verify(history::Keys {
                    data: &inventory.encryption,
                    seals: &seals
                })
                .is_err(),
            "the action remains bound to the original input"
        );
    }

    #[test]
    fn system_correction_completion_seal_matches_independent_python_vector() {
        use chrono::DateTime;
        use platform::cryptography::{envelope::Envelope, sha256};
        use zeroize::Zeroizing;

        use super::Subject;
        use crate::{
            database::sealed::Sealed,
            identity::person,
            kernel::operation::Family,
            keys::{self, Derived, Entry, Material, seal},
        };

        let key_id = keys::Id::new(0).expect("key");
        let seals = seal::Keys::new(Derived::fixed(
            key_id,
            vec![Entry::new(
                key_id,
                Material(Zeroizing::new(std::array::from_fn(|index| {
                    u8::try_from(index).expect("32 bytes")
                }))),
            )],
        ));
        let operation = "00000000-0000-4000-8000-000000000006"
            .parse()
            .expect("operation");
        let subject = Subject::System {
            person_id: "00000000-0000-4000-8000-000000000007"
                .parse::<person::Id>()
                .expect("operator"),
            revision_id: "00000000-0000-4000-8000-000000000008"
                .parse()
                .expect("operator revision"),
        };
        let plaintext = r#"{"operator":{"mobile":"13800000000"},"target":{"personId":"00000000-0000-4000-8000-000000000001","revisionId":"00000000-0000-4000-8000-000000000002"},"name":"张三","idNumber":"11010519491231002X"}"#.as_bytes();
        let mut envelope = vec![0; 37];
        envelope[0] = 1;
        let record = Record {
            family: Family::SystemCorrection,
            operation_id: operation,
            action: "correct".into(),
            target: "00000000-0000-4000-8000-000000000001".into(),
            input: Sealed(Envelope::new(envelope).expect("envelope")),
            subject,
            created_at: DateTime::from_timestamp_micros(1_700_000_000_123_000).expect("time"),
            expires_at: None,
            seal: seal::Seal::stored(vec![0; 32], key_id, 1).expect("placeholder seal"),
        };
        let fields = record.fields(plaintext);
        let encoded = seal::encode_fields(
            "kernel.completion",
            1,
            &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        )
        .expect("seal encoding");
        assert_eq!(encoded.as_ref().len(), 413);
        assert_eq!(
            hex::encode(sha256::Digest::new(&encoded).as_ref()),
            "a328e672c1e7b0e01de8761c7fd4e17f73db9a3ef637e755404cea9ae8c52ed7"
        );
        let seal = seals
            .sign(
                "kernel.completion",
                1,
                &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
            )
            .expect("system correction seal");
        assert_eq!(
            hex::encode(seal.tag()),
            "3fdf2085c61744de0269edf82d40cd40523e20f4c2d228e46e320f431eda418b"
        );
    }

    async fn expired(database: &Database, app: &App, original: operation::Id) -> Record {
        app.command(original, toy(app, "alpha"))
            .await
            .expect("completion to expire");
        let owner = database
            .connections("postgres")
            .recover()
            .await
            .expect("fixture owner");
        Seed::age(
            owner.client(),
            app.keys(),
            COMPLETIONS,
            original,
            chrono::Duration::hours(25),
        )
        .await
        .expect("expired completion fixture");
        owner
            .client()
            .batch_execute(&format!(
                "GRANT DELETE ON testing.completions TO \"{}\"",
                database.user
            ))
            .await
            .expect("completion retention grant");
        let records = fetch_all::<Infallible>(owner.client(), app.keys(), COMPLETIONS)
            .await
            .expect("stored candidate");
        owner.terminate().await.expect("owner ended");
        records.into_iter().next().expect("one expired candidate")
    }

    #[test]
    fn corrupt_recovery_closes_admission_and_preserves_the_completion() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let proxy = Proxy::start(&server).await;
                let app = start(&database, authentication::proxied(&database, &proxy), 4).await;
                let original = operation();
                let _ = expired(&database, &app, original).await;
                proxy.arm(database_proxy::Fault::DelayedRollback);
                let task = tokio::spawn({
                    let mut retention = Retention::new(app.clone(), COMPLETIONS);
                    async move { retention.run().await }
                });
                timeout(Duration::from_secs(5), proxy.intercepted())
                    .await
                    .expect("cleanup will roll back");
                proxy.arm(database_proxy::Fault::NullCompletionRead);
                proxy.release();
                timeout(Duration::from_secs(5), proxy.intercepted())
                    .await
                    .expect("actual malformed recovery result");
                let result = timeout(Duration::from_secs(5), task)
                    .await
                    .expect("damage stops this pass")
                    .expect("retention task");
                let Err(Error::Unknown(fault)) = result else {
                    panic!(
                        "corrupt recovery must propagate the unresolved original outcome: {result:?}"
                    );
                };
                assert!(
                    fault.later().any(|(stage, kind)| matches!(
                        (stage, kind),
                        (Stage::Recovery, FaultKind::Decode(_))
                    )),
                    "the actual recovery decode failure is retained: {fault:?}"
                );
                assert!(
                    app.registry().ready().is_err(),
                    "recovery damage closes shared admission"
                );
                assert_eq!(counts(&database, original).await, (1, 1));
                stop(app).await;
                proxy.stop().await;
            })
            .await
            .expect("corrupt recovery budget");
        });
    }

    #[test]
    fn rewound_cleanup_retains_its_driver_failure() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let proxy = Proxy::start(&server).await;
                let app = start(&database, authentication::proxied(&database, &proxy), 4).await;
                let original = operation();
                let candidate = expired(&database, &app, original).await;
                proxy.arm(database_proxy::Fault::CorruptTimeTermination);
                let result = Retention::new(app.clone(), COMPLETIONS).reclaim(&candidate).await;
                timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("actual damaged time and FATAL driver response");
                let Err(Error::Unavailable(fault)) = result else {
                    panic!("rewound time remains a clock failure: {result:?}");
                };
                assert!(matches!(fault.kind(), FaultKind::Clock(crate::runtime::Error::ClockInvalid)), "{fault:?}");
                assert!(fault.later().any(|(stage, kind)| matches!(
                    (stage, kind),
                    (Stage::Termination, FaultKind::Connection(platform::database::Error::Postgres(source)))
                        if source.code() == Some(&tokio_postgres::error::SqlState::CONNECTION_FAILURE)
                )), "clock failure retains the driver's actual FATAL: {fault:?}");
                assert_eq!(counts(&database, original).await, (1, 1));
                stop(app).await;
                proxy.stop().await;
            }).await.expect("corrupt cleanup budget");
        });
    }

    #[test]
    fn retained_completion_preserves_recovery_finalization_failures() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for lost_commit in [true, false] {
                timeout(Duration::from_secs(30), async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let proxy = Proxy::start(&server).await;
                    let app = start(&database, authentication::proxied(&database, &proxy), 4).await;
                    let original = operation();
                    let candidate = expired(&database, &app, original).await;
                    proxy.arm(database_proxy::Fault::DelayedRollback);
                    let task = tokio::spawn({
                        let app = app.clone();
                        async move { Retention::new(app, COMPLETIONS).reclaim(&candidate).await }
                    });
                    timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("cleanup will roll back");
                    proxy.arm(if lost_commit { database_proxy::Fault::LostReadCommit } else { database_proxy::Fault::ReadCommitTermination });
                    proxy.release();
                    timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("actual recovery finalization fault");
                    let result = timeout(Duration::from_secs(5), task).await.expect("known retention outcome").expect("cleanup task");
                    proxy.release();
                    let Err(Error::Unavailable(fault)) = result else {
                        panic!("completion remains after rollback: {result:?}");
                    };
                    assert_eq!(fault.stage(), Stage::Commit);
                    if lost_commit {
                        assert!(fault.later().any(|(stage, _)| stage == Stage::Commit), "recovery COMMIT is retained: {fault:?}");
                    } else {
                        assert!(fault.later().any(|(stage, kind)| matches!(
                            (stage, kind),
                            (Stage::Termination, FaultKind::Connection(platform::database::Error::Postgres(source)))
                                if source.code() == Some(&tokio_postgres::error::SqlState::CONNECTION_FAILURE)
                        )), "recovery driver failure is retained: {fault:?}");
                    }
                    assert_eq!(counts(&database, original).await, (1, 1));
                    app.registry().ready().expect("a known failed cleanup leaves admission open");
                    Retention::new(app.clone(), COMPLETIONS).run().await.expect("a later pass retries cleanup");
                    assert_eq!(counts(&database, original).await, (1, 0));
                    stop(app).await;
                    proxy.stop().await;
                }).await.expect("recovery finalization budget");
            }
        });
    }

    #[test]
    fn failed_cleanup_recovery_retains_its_driver_failure() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let proxy = Proxy::start(&server).await;
                let app = start(&database, authentication::proxied(&database, &proxy), 4).await;
                let original = operation();
                let candidate = expired(&database, &app, original).await;
                proxy.arm(database_proxy::Fault::DelayedRollback);
                let task = tokio::spawn({
                    let app = app.clone();
                    async move { Retention::new(app, COMPLETIONS).reclaim(&candidate).await }
                });
                timeout(Duration::from_secs(5), proxy.intercepted())
                    .await
                    .expect("cleanup will roll back");
                proxy.arm(database_proxy::Fault::RejectedBegin);
                proxy.release();
                timeout(Duration::from_secs(5), proxy.intercepted())
                    .await
                    .expect("actual recovery BEGIN failure");
                let result = timeout(Duration::from_secs(5), task)
                    .await
                    .expect("unknown cleanup outcome")
                    .expect("cleanup task");
                let Err(Error::Unknown(fault)) = result else {
                    panic!("failed recovery preserves an unknown outcome: {result:?}");
                };
                assert_eq!(fault.stage(), Stage::Commit);
                let mut later = fault.later();
                assert!(
                    later.any(|(stage, _)| stage == Stage::Begin),
                    "recovery begin failure is retained: {fault:?}"
                );
                assert!(
                    later.any(|(stage, kind)| matches!(
                        (stage, kind),
                        (Stage::Termination, FaultKind::Connection(_))
                    )),
                    "the driver failure after recovery BEGIN is retained: {fault:?}"
                );
                let observer = database
                    .connections("postgres")
                    .recover()
                    .await
                    .expect("independent original-operation observer");
                // Failed recovery did not obtain this lock; confirm the original transaction ended.
                crate::kernel::tx::object(
                    observer.client(),
                    COMPLETIONS.family().lock(),
                    original.as_bytes(),
                )
                .await
                .expect("original cleanup ended before observing its durable facts");
                observer.terminate().await.expect("observer ended");
                assert_eq!(counts(&database, original).await, (1, 1));
                Retention::new(app.clone(), COMPLETIONS)
                    .run()
                    .await
                    .expect("a later pass can reclaim the retained completion");
                assert_eq!(counts(&database, original).await, (1, 0));
                stop(app).await;
                proxy.stop().await;
            })
            .await
            .expect("failed recovery discard budget");
        });
    }
}
