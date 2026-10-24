//! A pooled write session is reusable only after an acknowledged transaction end.

use deadpool::managed::Object;
use scopeguard::ScopeGuard;
use tokio_postgres::Client;

use crate::database::{Connections, Error, Manager};

pub struct Lease(ScopeGuard<Object<Manager>, fn(Object<Manager>)>);

impl Lease {
    pub(super) fn new(session: Object<Manager>) -> Self {
        Self(scopeguard::guard(session, |session| {
            drop(Object::take(session));
        }))
    }

    pub fn client(&self) -> &Client {
        self.0.client()
    }

    /// Call only after a definite COMMIT or ROLLBACK response.
    pub fn release(self) {
        drop(ScopeGuard::into_inner(self.0));
    }

    pub async fn discard(self) -> Result<(), Error> {
        Connections::discard(ScopeGuard::into_inner(self.0)).await
    }
}
