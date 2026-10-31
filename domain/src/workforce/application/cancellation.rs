//! Cancel an expected unfinished state without changing its retained facts.

use super::*;

impl Service {
    pub async fn cancel(
        &self,
        actor: &Actor,
        id: Id,
        input: CancellationInput,
    ) -> Result<Id, Error> {
        self.app.registry().ready()?;
        let lease = self.app.context().connections().lease().await?;
        let client = lease.client();
        let result = async {
            transaction::begin_current(self.app.context(), client).await?;
            let manager = matches!(&input, CancellationInput::Manager { .. });
            if manager {
                transaction::shared(client, b"access/scope", b"workforce").await?;
            }
            actor.lock_account(client).await?;
            let operator = account::state::discover_owner(client, actor.account_id())
                .await?
                .ok_or(current::Error::Invalid)?;
            person::state::lock_shared(client, operator).await?;
            actor.lock_session(client).await?;
            let operator = if manager {
                member::lock_current(client, actor.account_id()).await?;
                Box::pin(self.manager(client, actor, Read::Shared)).await?.0
            } else {
                // Self cancellation needs a normal account, regardless of current Grant state.
                self.current
                    .fetch_state(client, actor, Read::Shared)
                    .await?
                    .person_id()
            };
            database::cancel(client, id, operator, input).await
        }
        .await;
        finish(self, lease, result, true).await
    }
}
