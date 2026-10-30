//! Permanent state history disclosed only after complete fact and identity authentication.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use tokio_postgres::Client;

use crate::{
    history::{self, Record as _},
    identity::{
        account,
        account::record::Event,
        operation,
        person::revision,
        suspension::{Error, Id},
    },
    pagination::Pagination,
};

pub struct List {
    count: i64,
    suspensions: Vec<Suspension>,
}

impl List {
    pub fn count(&self) -> i64 {
        self.count
    }
    pub fn suspensions(&self) -> &[Suspension] {
        &self.suspensions
    }
}

pub struct Suspension {
    id: Id,
    subject: revision::Identity,
    operator: revision::Identity,
    sequence: i64,
    suspended_at: DateTime<Utc>,
    operation_id: operation::Id,
    resumption: Option<Resumption>,
}

impl Suspension {
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn subject(&self) -> &revision::Identity {
        &self.subject
    }
    pub fn operator(&self) -> &revision::Identity {
        &self.operator
    }
    pub fn sequence(&self) -> i64 {
        self.sequence
    }
    pub fn suspended_at(&self) -> DateTime<Utc> {
        self.suspended_at
    }
    pub fn operation_id(&self) -> operation::Id {
        self.operation_id
    }
    pub fn resumption(&self) -> Option<&Resumption> {
        self.resumption.as_ref()
    }
}

pub struct Resumption {
    subject: revision::Identity,
    operator: revision::Identity,
    sequence: i64,
    resumed_at: DateTime<Utc>,
    operation_id: operation::Id,
}

impl Resumption {
    pub fn subject(&self) -> &revision::Identity {
        &self.subject
    }
    pub fn operator(&self) -> &revision::Identity {
        &self.operator
    }
    pub fn sequence(&self) -> i64 {
        self.sequence
    }
    pub fn resumed_at(&self) -> DateTime<Utc> {
        self.resumed_at
    }
    pub fn operation_id(&self) -> operation::Id {
        self.operation_id
    }
}

pub(crate) async fn fetch_list(
    client: &Client,
    keys: history::Keys<'_>,
    revisions: &revision::Reader,
    account: &history::Verified<account::state::State>,
    pagination: Pagination,
    now: DateTime<Utc>,
) -> Result<List, Error> {
    let page = account::record::suspensions(client, keys, account.id(), pagination, now)
        .await
        .map_err(Error::History)?;
    if page.records.is_empty() {
        return Ok(List {
            count: page.count,
            suspensions: Vec::new(),
        });
    }
    let mut revision_ids = HashSet::new();
    for record in &page.records {
        if record.account_id() != account.id() || record.person_id() != account.person_id() {
            return Err(Error::Facts {
                rule: "a suspension page belongs to its selected account and person",
            });
        }
        let Event::Suspension { confirmation, .. } = record.event() else {
            return Err(Error::Facts {
                rule: "a suspension page contains suspension records",
            });
        };
        revision_ids.insert(confirmation.revision_id());
        revision_ids.insert(confirmation.operator_id());
    }
    let mut children = HashMap::with_capacity(page.resumptions.len());
    for record in page.resumptions {
        if record.account_id() != account.id() || record.person_id() != account.person_id() {
            return Err(Error::Facts {
                rule: "a resumption belongs to its suspension account and person",
            });
        }
        let Event::Resumption { id, confirmation } = record.event() else {
            return Err(Error::Facts {
                rule: "a suspension page pairs only resumption records",
            });
        };
        revision_ids.insert(confirmation.revision_id());
        revision_ids.insert(confirmation.operator_id());
        children.insert(*id, record);
    }
    let identities = revisions
        .fetch_identities(client, &revision_ids.into_iter().collect::<Vec<_>>())
        .await
        .map_err(Error::Revision)?;
    let current = account
        .current()
        .and_then(|current| current.suspension_id());
    let mut suspensions = Vec::with_capacity(page.records.len());
    for record in page.records {
        let Event::Suspension {
            id, confirmation, ..
        } = record.event()
        else {
            return Err(Error::Facts {
                rule: "a suspension page contains suspension records",
            });
        };
        let subject = identities
            .get(&confirmation.revision_id())
            .ok_or(Error::Revision(revision::ReadError::Missing))?
            .clone();
        let operator = identities
            .get(&confirmation.operator_id())
            .ok_or(Error::Revision(revision::ReadError::Missing))?
            .clone();
        let resumption = match (current == Some(*id), children.remove(id)) {
            (true, None) => None,
            (false, Some(resumption)) => {
                let Event::Resumption { confirmation, .. } = resumption.event() else {
                    return Err(Error::Facts {
                        rule: "a suspension page pairs only resumption records",
                    });
                };
                Some(Resumption {
                    subject: identities
                        .get(&confirmation.revision_id())
                        .ok_or(Error::Revision(revision::ReadError::Missing))?
                        .clone(),
                    operator: identities
                        .get(&confirmation.operator_id())
                        .ok_or(Error::Revision(revision::ReadError::Missing))?
                        .clone(),
                    sequence: resumption.sequence(),
                    resumed_at: resumption.happened_at(),
                    operation_id: resumption.operation_id(),
                })
            }
            _ => {
                return Err(Error::Facts {
                    rule: "only the account's current suspension has no resumption",
                });
            }
        };
        suspensions.push(Suspension {
            id: *id,
            subject,
            operator,
            sequence: record.sequence(),
            suspended_at: record.happened_at(),
            operation_id: record.operation_id(),
            resumption,
        });
    }
    if !children.is_empty() {
        return Err(Error::Facts {
            rule: "every resumption belongs to a suspension on the selected page",
        });
    }
    Ok(List {
        count: page.count,
        suspensions,
    })
}
