//! Verify Access's retained cross-object references under the startup boundary.

use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio_postgres::Client;

use crate::{
    access::{Error, capability, holder},
    database::verification,
    history::{self, Read},
    identity::account,
    kernel::App,
};

/// Sealed state and completion scanning belongs to each service's startup verification.
pub(crate) async fn verify(client: &Client, app: &App, now: DateTime<Utc>) -> Result<(), Error> {
    verification::Stage::Capabilities
        .batch(Duration::from_secs(10), capability::verify(client))
        .await??;
    let (accounts, holders) = verification::Stage::AccessRelations
        .batch(Duration::from_secs(10), async {
            // Startup holds the exclusive barrier after all three state owners initialized.
            let read = Read::Snapshot(app.anchors().view()?);
            let accounts =
                history::all::<account::state::State>(client, app.keys(), app.anchors(), read, now)
                    .await?;
            let holders =
                history::all::<holder::State>(client, app.keys(), app.anchors(), read, now).await?;
            crate::access::role::database::verify_links(
                client,
                app,
                read,
                accounts.is_empty(),
                now,
            )
            .await?;
            Ok::<_, Error>((accounts, holders))
        })
        .await??;
    holder::verify_links(client, app.keys(), &accounts, &holders).await?;
    crate::access::grant::database::verify(client, &accounts, now).await?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use fixture::database_proxy::{Fault, Proxy};
    use tokio::{runtime::Runtime, time::timeout};

    use crate::{
        access::{Error, verification::verify},
        database::verification::{Stage, Waiting},
        identity::authentication::{self, Database, server},
    };

    #[test]
    fn stalled_capability_verification_reports_its_step() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(25), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let supplier = authentication::Supplier::new().await;
                let (context, services) = crate::access::role::tests::start(
                    database.connections(&database.user),
                    &supplier,
                )
                .await;
                let proxy = Proxy::start(&server).await;
                let connections = authentication::proxied(&database, &proxy);
                let session = connections.recover().await.expect("runtime session");
                let now = session
                    .client()
                    .query_one("SELECT clock_timestamp()", &[])
                    .await
                    .expect("fixture time")
                    .get(0);
                proxy.arm(Fault::StalledQuery);
                let (result, ()) =
                    tokio::join!(verify(session.client(), services.app(), now), async {
                        timeout(Duration::from_secs(5), proxy.intercepted())
                            .await
                            .expect("capability query intercepted");
                    });
                let error = result.expect_err("stalled capability query times out");
                assert!(matches!(&error, Error::Timeout(cause)
                    if cause.stage() == Stage::Capabilities
                        && cause.waiting() == Waiting::QueryAndValidation));
                assert!(format!("{error:?}").contains("Capabilities"));
                assert!(error.to_string().contains("capabilities"));
                session.terminate().await.expect("original driver ended");
                proxy.release();
                connections.close().await;
                services.close();
                services.drain().await.expect("drained");
                context.stop().await.expect("context stopped");
                supplier.stop().await;
                proxy.stop().await;
            })
            .await
            .expect("bounded capability verification");
        });
    }
}
