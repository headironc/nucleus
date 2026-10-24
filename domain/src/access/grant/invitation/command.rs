//! Invitation creation binds its complete input and advances the sealed object once.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use super::{Code, Id, Input, Invitation, database, record, state, transition};
use crate::{
    access::{
        Entry,
        capability::Scope,
        grant::{self, CommandError, Error, Rejection, Service, member},
        role,
    },
    history::{self, Change},
    identity::{
        current::{self, Actor},
        operation,
    },
    kernel::{Authorization, Command, completion, operation::Family, tx::Tx},
};

pub(super) async fn lock_management(actor: &Actor, tx: &Tx, id: Id) -> Result<(), CommandError> {
    tx.shared(b"access/scope", b"workforce").await?;
    actor.lock_account(tx.client()).await?;
    actor.lock_session(tx.client()).await?;
    grant::state::lock_operator(tx.client(), actor.account_id()).await?;
    crate::kernel::tx::object(tx.client(), b"access/invitation", id.as_bytes()).await?;
    Ok(())
}

pub(super) const COMPLETIONS: completion::Template =
    completion::Template::personal(Family::Invitation, |action, target| {
        action == "create" && target == "invitations"
    });

pub(super) async fn create(
    service: &Service,
    actor: Actor,
    operation_id: operation::Id,
    input: Input,
) -> Result<Invitation, Error> {
    Ok(service
        .app
        .command(
            operation_id,
            Create {
                service: service.clone(),
                actor,
                input,
                id: Id::allocate(),
            },
        )
        .await?)
}

pub(super) async fn authorize(
    service: &Service,
    tx: &Tx,
    subject: Option<current::database::State>,
) -> Result<Authorization, CommandError> {
    let subject = subject.ok_or(CommandError::corrupt(
        "invitation management has a qualified session",
    ))?;
    let (_, person_id, revision_id, now) = member::authorize_subject(
        tx.client(),
        &service.roles,
        subject,
        role::Code::WorkforceAdmission,
    )
    .await?;
    Ok(Authorization {
        person_id,
        revision_id,
        now,
    })
}

struct Create {
    service: Service,
    actor: Actor,
    input: Input,
    id: Id,
}

struct Facts {
    creator: history::Verified<grant::state::State>,
    manager: role::Role,
    selected: Option<role::Role>,
    authority: Authorization,
    operation_id: operation::Id,
}

struct Selected {
    creator: history::Verified<grant::state::State>,
    authority: Authorization,
    operation_id: operation::Id,
}

impl Command for Create {
    const COMPLETIONS: completion::Template = COMPLETIONS;
    type Facts = Facts;
    type Change = Selected;
    type Output = Invitation;
    type Rejection = Rejection;

    fn session(&self) -> Option<(&current::Service, &Actor)> {
        Some((&self.service.current, &self.actor))
    }

    fn request(&self, authority: &Authorization) -> Result<completion::Request, CommandError> {
        completion::Request::new("create", "invitations".to_owned(), &self.input, authority)
    }

    async fn locks(&self, tx: &Tx) -> Result<(), CommandError> {
        tx.shared(b"access/scope", b"workforce").await?;
        self.actor.lock_account(tx.client()).await?;
        self.actor.lock_session(tx.client()).await?;
        grant::state::lock_operator(tx.client(), self.actor.account_id()).await?;
        role::database::lock_shared(tx.client(), self.input.role.id, Scope::Workforce).await?;
        Ok(())
    }

    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, CommandError> {
        authorize(&self.service, tx, subject).await
    }

    async fn load(&self, tx: &Tx, authority: &Authorization) -> Result<Facts, CommandError> {
        let creator = grant::state::current(
            tx.client(),
            &self.service.roles,
            authority.person_id,
            history::Read::Shared,
            authority.now,
        )
        .await?
        .ok_or(CommandError::corrupt("qualified creator retains its Grant"))?;
        let manager = self
            .service
            .roles
            .find(
                tx.client(),
                creator.role_id(),
                Scope::Workforce,
                authority.now,
            )
            .await?
            .ok_or(CommandError::corrupt("qualified creator retains its role"))?;
        let selected = self
            .service
            .roles
            .find(
                tx.client(),
                self.input.role.id,
                Scope::Workforce,
                authority.now,
            )
            .await?;
        Ok(Facts {
            creator,
            manager,
            selected,
            authority: Authorization {
                person_id: authority.person_id,
                revision_id: authority.revision_id,
                now: authority.now,
            },
            operation_id: tx.operation_id(),
        })
    }

    fn decide(&self, facts: Facts, now: DateTime<Utc>) -> Result<Selected, CommandError> {
        let selected = facts.selected.ok_or(Rejection::RoleChanged)?;
        if selected.change_id() != self.input.role.change_id {
            return Err(Rejection::RoleChanged.into());
        }
        if !Entry::Domain.can_assign(&facts.manager, &selected) {
            return Err(CommandError::Denied);
        }
        if DateTime::<Utc>::from(self.input.expires_at) <= now {
            return Err(Rejection::Expiry.into());
        }
        Ok(Selected {
            creator: facts.creator,
            authority: facts.authority,
            operation_id: facts.operation_id,
        })
    }

    async fn apply(
        &self,
        tx: &Tx,
        selected: Selected,
        pending: &mut Option<history::Pending>,
    ) -> Result<Invitation, CommandError> {
        let now = selected.authority.now;
        let mut unique = None;
        for _ in 0..super::ATTEMPTS {
            let code = Code::generate().map_err(Error::from)?;
            if state::locate_code(
                tx.client(),
                &self.service.invitations,
                &code,
                self.service.roles.read()?,
                &self.service.roles,
                now,
            )
            .await?
            .is_none()
            {
                unique = Some(code);
                break;
            }
        }
        let code = unique.ok_or(CommandError::corrupt(
            "new invitation codes do not repeat retained lookup digests",
        ))?;
        let hash = code
            .lookup(self.service.invitations.lookup.active())
            .map_err(Error::from)?;
        let sealed = code
            .seal(
                &self.service.invitations,
                self.id,
                selected.creator.id(),
                selected.creator.epoch(),
            )
            .map_err(Error::from)?;
        let change: Change<state::State> = transition::create(state::Creation {
            id: self.id,
            person_id: selected.authority.person_id,
            grant_id: selected.creator.id(),
            epoch: selected.creator.epoch(),
            role_id: self.input.role.id,
            change_id: self.input.role.change_id,
            operator_id: selected.authority.revision_id,
            capacity: self.input.capacity,
            expires_at: self.input.expires_at,
            code: Arc::new(sealed),
            plaintext: Arc::new(code),
            hash,
            key_id: self.service.invitations.lookup.active().id(),
            created_at: now,
            operation_id: selected.operation_id,
        });
        *pending = Some(
            change
                .write(
                    tx.client(),
                    self.service.app.keys(),
                    self.service.app.anchors(),
                )
                .await?,
        );
        database::find_by_id(tx.client(), &self.service.roles, self.id, now)
            .await?
            .ok_or(CommandError::corrupt(
                "created invitation retains its state",
            ))?
            .deliver(&self.service.invitations)
            .map_err(CommandError::from)
    }

    async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<Invitation, CommandError> {
        let original = record::original(
            tx.client(),
            &self.service.roles,
            Family::Invitation,
            tx.operation_id(),
            now,
        )
        .await?
        .ok_or(CommandError::corrupt(
            "invitation completion retains its creation record",
        ))?;
        if original.person_id() != request.person_id() {
            return Err(CommandError::corrupt(
                "invitation completion retains its creator",
            ));
        }
        let fact =
            database::find_by_operation(tx.client(), &self.service.roles, tx.operation_id(), now)
                .await?
                .ok_or(CommandError::corrupt(
                    "original invitation retains its state",
                ))?;
        if fact.state.id() != original.invitation_id()
            || fact.state.role_id() != self.input.role.id
            || fact.state.change_id() != self.input.role.change_id
            || fact.state.original_capacity() != self.input.capacity
            || fact.state.original_expires_at() != self.input.expires_at
        {
            return Err(CommandError::corrupt(
                "invitation creation agrees with its completed input",
            ));
        }
        fact.deliver(&self.service.invitations)
            .map_err(CommandError::from)
    }
}
