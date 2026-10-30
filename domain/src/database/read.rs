//! One budget for an authorized read snapshot and its server-side timeouts.

use std::time::Duration;

use tokio::time::Instant;
use tokio_postgres::Client;

use crate::{
    database::Query,
    history::{self, Anchors, View},
};

/// Only an anchor mismatch after the captured view permits a complete snapshot retry.
pub(crate) trait Failure: From<Query> + From<history::Error> {
    fn changed(&self) -> bool;
}

/// Legacy read errors retain the typed history cause through their owner wrappers.
pub(crate) fn changed(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut source = Some(error);
    while let Some(error) = source {
        if matches!(
            error.downcast_ref::<history::Error>(),
            Some(history::Error::Changed)
        ) {
            return true;
        }
        source = error.source();
    }
    false
}

#[derive(Clone, Copy)]
pub(crate) struct Budget {
    duration: Duration,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            duration: Duration::from_secs(3),
        }
    }
}

impl Budget {
    #[cfg(test)]
    pub(crate) fn new(duration: Duration) -> Self {
        assert!(!duration.is_zero());
        assert_eq!(duration.subsec_nanos() % 1_000_000, 0);
        assert!(i32::try_from(duration.as_millis()).is_ok());
        Self { duration }
    }

    pub(crate) fn deadline(self) -> Instant {
        Instant::now() + self.duration
    }

    pub(crate) async fn begin(self, client: &Client) -> Result<(), tokio_postgres::Error> {
        let milliseconds = self.duration.as_millis();
        client
            .batch_execute(&format!(
                "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY; SET LOCAL statement_timeout = '{milliseconds}ms'; SET LOCAL idle_in_transaction_session_timeout = '{milliseconds}ms'"
            ))
            .await
    }

    /// The caller retains its deadline, connection and final COMMIT/cleanup responsibilities.
    pub(crate) async fn run<
        T,
        E: Failure,
        F: Fn(View) -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
    >(
        self,
        client: &Client,
        anchors: &Anchors,
        query: F,
    ) -> Result<T, E> {
        for retry in [false, true] {
            let view = anchors.view()?;
            self.begin(client).await.map_err(Query::from)?;
            let result = query(view).await;
            if result.as_ref().is_err_and(Failure::changed) {
                if retry {
                    return Err(history::Error::Integrity(
                        "a fresh snapshot agrees with its anchor view",
                    )
                    .into());
                }
                if let Err(error) = client.batch_execute("ROLLBACK").await {
                    return Err(history::Error::Retry(error.into()).into());
                }
                continue;
            }
            return result;
        }
        Err(history::Error::Integrity("a snapshot ends within its single retry").into())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{sync::Arc, time::Duration};

    use tokio::{runtime::Runtime, time::Instant};

    use crate::{
        identity::authentication::{Database, server},
        runtime::Context,
    };

    #[test]
    fn context_budget_binds_deadline_and_server_snapshot_timeouts() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(Arc::clone(&server)).await;
            for (duration, expected, setting) in [
                (None, Duration::from_secs(3), "3s"),
                (Some(Duration::from_millis(1250)), Duration::from_millis(1250), "1250ms"),
                (Some(Duration::from_secs(30)), Duration::from_secs(30), "30s"),
            ] {
                let connections = database.connections(&database.user);
                let context = match duration {
                    None => Context::new(connections).await,
                    Some(duration) => Context::with_read_budget(connections, duration).await,
                }.expect("context");
                let shared = context.clone();
                let budget = shared.read_budget();
                let before = Instant::now();
                let deadline = budget.deadline();
                let after = Instant::now();
                assert!(deadline >= before + expected && deadline <= after + expected);
                let lease = shared.connections().lease().await.expect("snapshot connection");
                budget.begin(lease.client()).await.expect("snapshot begins");
                let settings = lease.client().query_one(
                    "SELECT current_setting('statement_timeout'), current_setting('idle_in_transaction_session_timeout'), current_setting('transaction_isolation'), current_setting('transaction_read_only')", &[]
                ).await.expect("actual transaction settings");
                assert_eq!(settings.get::<_, &str>(0), setting);
                assert_eq!(settings.get::<_, &str>(1), setting);
                assert_eq!(settings.get::<_, &str>(2), "repeatable read");
                assert_eq!(settings.get::<_, &str>(3), "on");
                lease.client().batch_execute("ROLLBACK").await.expect("snapshot ended");
                lease.release();
                context.stop().await.expect("context stopped");
            }
        });
    }
}
