//! Logout advances the account and its fixed session set in the kernel transaction.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio_postgres::Client;

use crate::{
    history::{self, Read, Verified, View},
    identity::{
        account::{self, record::Event},
        current::{self, Actor},
        operation,
        revocation::{Completion, Error, Rejection, Revocation, Scope, Service},
        session,
    },
    kernel::{App, Authorization, Command, Query, completion, operation::Family, tx::Tx},
};

fn accepts(action: &str, target: &str) -> bool {
    action == "logout"
        && target
            .parse::<account::Id>()
            .is_ok_and(|id| id.to_string() == target)
}

pub(crate) const COMPLETIONS: completion::Template =
    completion::Template::personal(Family::Logout, accepts);

fn request(
    account_id: account::Id,
    session_id: session::Id,
    scope: Scope,
    authority: &Authorization,
) -> Result<completion::Request, Error> {
    #[derive(Serialize)]
    struct Input<'a> {
        session_id: session::Id,
        scope: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        target_id: Option<session::Id>,
    }
    completion::Request::new(
        "logout",
        account_id.to_string(),
        &Input {
            session_id,
            scope: scope.as_ref(),
            target_id: match scope {
                Scope::Other(id) => Some(id),
                _ => None,
            },
        },
        authority,
    )
}

pub(super) struct Write {
    pub(super) service: Service,
    pub(super) actor: Actor,
    pub(super) operation_id: operation::Id,
    pub(super) scope: Scope,
}

pub(super) struct Facts {
    account: Verified<account::state::State>,
    sessions: Vec<Verified<session::state::State>>,
}

pub(super) struct Selected {
    change: account::transition::Revocation,
    result: Completion,
}

impl Command for Write {
    const COMPLETIONS: completion::Template = COMPLETIONS;
    type Facts = Facts;
    type Change = Selected;
    type Output = Completion;
    type Rejection = Rejection;

    fn session(&self) -> Option<(&current::Service, &Actor)> {
        Some((&self.service.current, &self.actor))
    }

    fn request(&self, authority: &Authorization) -> Result<completion::Request, Error> {
        request(
            self.actor.account_id(),
            self.actor.session_id(),
            self.scope,
            authority,
        )
    }

    async fn locks(&self, tx: &Tx) -> Result<(), Error> {
        account::state::lock(tx.client(), self.actor.account_id()).await?;
        session::state::lock_account(tx.client(), self.actor.account_id()).await?;
        Ok(())
    }

    async fn authorize(
        &self,
        _: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, Error> {
        let subject = subject.ok_or(Error::corrupt("logout declares its session subject"))?;
        Ok(Authorization {
            person_id: subject.person_id(),
            revision_id: subject.revision_id(),
            now: subject.observed_at(),
        })
    }

    async fn load(&self, tx: &Tx, authority: &Authorization) -> Result<Facts, Error> {
        let app = &self.service.app;
        let account = history::find::<account::state::State>(
            tx.client(),
            app.keys(),
            app.anchors(),
            Read::Locked,
            self.actor.account_id(),
            authority.now,
        )
        .await?
        .ok_or(Error::corrupt("the locked logout account remains present"))?;
        let sessions = session::state::for_account(
            tx.client(),
            app.keys(),
            app.anchors(),
            Read::Locked,
            self.actor.account_id(),
            authority.now,
        )
        .await?;
        Ok(Facts { account, sessions })
    }

    fn decide(&self, facts: Facts, now: DateTime<Utc>) -> Result<Selected, Error> {
        if let Scope::Other(id) = self.scope {
            if id == self.actor.session_id() {
                return Err(Error::Rejected(Rejection::CurrentSession));
            }
            if !facts
                .sessions
                .iter()
                .any(|session| session.id() == id && session.available(now))
            {
                return Err(Error::Rejected(Rejection::NotFound));
            }
        }
        let change = account::transition::logout(
            &facts.account,
            self.actor.session_id(),
            self.scope,
            &facts.sessions,
            self.operation_id,
            now,
        )?;
        let result = Completion::new(change.sessions.len(), now)?;
        Ok(Selected { change, result })
    }

    async fn apply(
        &self,
        tx: &Tx,
        selected: Selected,
        pending: &mut Option<history::Pending>,
    ) -> Result<Completion, Error> {
        let app = &self.service.app;
        let account::transition::Revocation { account, sessions } = selected.change;
        *pending = Some(
            account
                .write(tx.client(), app.keys(), app.anchors())
                .await?,
        );
        let group = pending
            .as_mut()
            .ok_or(Error::corrupt("logout retains its account change"))?;
        for (id, change) in sessions {
            group.extend(change.write(tx.client(), app.keys(), app.anchors()).await?)?;
            session::handoff::remove(tx.client(), id).await?;
        }
        Ok(selected.result)
    }

    async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<Completion, Error> {
        let app = &self.service.app;
        let record = account::record::logout(
            tx.client(),
            app.keys(),
            self.actor.account_id(),
            self.operation_id,
            now,
        )
        .await?
        .ok_or(Error::corrupt(
            "logout completion retains its permanent account record",
        ))?;
        let Event::Logout {
            initiator_id,
            scope,
            sessions,
        } = record.event()
        else {
            return Err(Error::corrupt(
                "logout lookup returns its declared record kind",
            ));
        };
        if *initiator_id != self.actor.session_id()
            || *scope != self.scope
            || record.person_id() != request.person_id()
        {
            return Err(Error::corrupt(
                "logout completion and permanent original set agree",
            ));
        }
        app.anchors().confirm::<account::state::State>(&record)?;
        Completion::new(sessions.len(), record.happened_at())
    }
}

pub(super) struct Original<'a> {
    pub(super) service: &'a Service,
    pub(super) actor: &'a Actor,
    pub(super) operation_id: operation::Id,
}

pub(super) struct Sessions<'a> {
    pub(super) service: &'a Service,
    pub(super) actor: &'a Actor,
}

impl Query for Sessions<'_> {
    type Authorized = current::database::State;
    type Output = Vec<super::Session>;
    type Rejection = Rejection;

    async fn authorize(&self, client: &Client, view: View) -> Result<Self::Authorized, Error> {
        Ok(self
            .service
            .current
            .at(view)
            .fetch_state(client, self.actor, Read::Snapshot(view))
            .await?)
    }

    async fn read(
        &self,
        client: &Client,
        subject: Self::Authorized,
        view: View,
    ) -> Result<Self::Output, Error> {
        let sessions = session::state::for_account(
            client,
            self.service.app.keys(),
            self.service.app.anchors(),
            Read::Snapshot(view),
            subject.account_id(),
            subject.observed_at(),
        )
        .await?;
        Ok(sessions
            .into_iter()
            .filter(|session| session.available(subject.observed_at()))
            .map(|session| super::Session {
                id: session.id(),
                origin: session.origin().clone(),
                created_at: session.created_at(),
                current: session.id() == self.actor.session_id(),
            })
            .collect())
    }
}

impl Query for Original<'_> {
    const RETAIN_ON_END_FAILURE: bool = true;
    type Authorized = current::database::State;
    type Output = Option<super::Original>;
    type Rejection = Rejection;

    async fn authorize(&self, client: &Client, view: View) -> Result<Self::Authorized, Error> {
        Ok(self
            .service
            .current
            .at(view)
            .fetch_state(client, self.actor, Read::Snapshot(view))
            .await?)
    }

    async fn read(
        &self,
        client: &Client,
        subject: Self::Authorized,
        _: View,
    ) -> Result<Option<super::Original>, Error> {
        let Some(record) = account::record::logout(
            client,
            self.service.app.keys(),
            subject.account_id(),
            self.operation_id,
            subject.observed_at(),
        )
        .await?
        else {
            return Ok(None);
        };
        if record.person_id() != subject.person_id() {
            return Err(Error::corrupt("an account's logout retains its person"));
        }
        let Event::Logout {
            sessions,
            scope,
            initiator_id,
        } = record.event()
        else {
            return Err(Error::corrupt(
                "logout lookup returns its declared record kind",
            ));
        };
        Ok(Some(super::Original {
            scope: *scope,
            initiator_id: *initiator_id,
            revocations: sessions
                .iter()
                .map(|id| Revocation {
                    id: *id,
                    revoked_at: record.happened_at(),
                })
                .collect(),
        }))
    }
}

/// The permanent result and kernel completion retain the same original request.
pub(super) async fn verify(client: &Client, app: &App, now: DateTime<Utc>) -> Result<(), Error> {
    use history::Record as _;

    completion::verify_identity_families::<Rejection>(client).await?;
    let mut completions: HashMap<_, _> =
        completion::fetch_all::<Rejection>(client, app.keys(), COMPLETIONS)
            .await?
            .into_iter()
            .map(|record| (record.operation_id(), record))
            .collect();
    for record in account::record::logouts(client, app.keys(), now).await? {
        let completion = completions
            .remove(&record.operation_id())
            .ok_or(Error::corrupt(
                "every logout retains its permanent completion",
            ))?;
        let Event::Logout {
            initiator_id,
            scope,
            ..
        } = record.event()
        else {
            return Err(Error::corrupt(
                "logout scan returns its declared record kind",
            ));
        };
        if completion.created_at() != record.happened_at()
            || completion.person_id() != record.person_id()
        {
            return Err(Error::corrupt(
                "logout completion retains the original attribution and time",
            ));
        }
        let authority = Authorization {
            person_id: completion.person_id(),
            revision_id: completion.revision_id(),
            now,
        };
        let expected = request(record.account_id(), *initiator_id, *scope, &authority)?;
        match completion.matches(COMPLETIONS, app.keys(), &expected) {
            Err(Error::Mismatch) => {
                return Err(Error::corrupt(
                    "logout completion binds its original request",
                ));
            }
            result => result?,
        }
    }
    if !completions.is_empty() {
        return Err(Error::corrupt(
            "every logout completion retains its original account record",
        ));
    }
    Ok(())
}
