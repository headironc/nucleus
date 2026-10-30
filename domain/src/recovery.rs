//! Controlled changes between an accepted write and its independent recovery.

#![allow(clippy::expect_used)]

use std::{future::Future, time::Duration};

use fixture::database_proxy::{Fault, Proxy};
use tokio::time::timeout;
use uuid::Uuid;

use crate::identity::{authentication::Database, current::Actor, person::revision, revocation};

pub(crate) async fn after_revision_loss<T>(
    database: &Database,
    proxy: &Proxy,
    actor: &Actor,
    request: impl Future<Output = T>,
) -> T {
    let owner = database
        .connections("postgres")
        .recover()
        .await
        .expect("isolated corruption owner");
    let observer = database
        .connections("postgres")
        .recover()
        .await
        .expect("independent lock observer");
    let row = owner.client().query_one("SELECT pg_backend_pid(), person.revision_id FROM identity.persons person JOIN identity.accounts account ON account.person_id=person.id WHERE account.id=$1", &[&actor.account_id()]).await.expect("original operator revision");
    let pid: i32 = row.get(0);
    let revision_id: revision::Id = row.get(1);
    owner
        .client()
        .batch_execute("BEGIN; SET LOCAL session_replication_role=replica")
        .await
        .expect("privileged damage confined to this disposable database");
    proxy.arm(Fault::DelayedCommit);
    let damage = async {
        timeout(Duration::from_secs(3), proxy.intercepted())
            .await
            .expect("original write reached its lost commit response");
        let lock = async {
            owner
                .client()
                .query_one(
                    "SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE",
                    &[&actor.account_id()],
                )
                .await
        };
        tokio::pin!(lock);
        let blocked = async {
            loop {
                if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND datname=current_database() AND wait_event_type='Lock' AND cardinality(pg_blocking_pids(pid))>0)", &[&pid]).await.expect("damage waits behind the accepted account lock").get::<_, bool>(0) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        };
        timeout(Duration::from_secs(3), async {
            tokio::select! {
                result = &mut lock => panic!("damage must wait for the original transaction: {result:?}"),
                () = blocked => (),
            }
        })
        .await
        .expect("damage queued before recovery reaches the account");
        proxy.release();
        lock.await
            .expect("original transaction released the account");
        assert_eq!(
            owner
                .client()
                .execute(
                    "DELETE FROM identity.revisions WHERE id=$1",
                    &[&revision_id]
                )
                .await
                .expect("remove only the referenced operator revision"),
            1
        );
        owner
            .client()
            .batch_execute("COMMIT")
            .await
            .expect("damage precedes recovery");
    };
    let (result, ()) = timeout(Duration::from_secs(15), async {
        tokio::join!(request, damage)
    })
    .await
    .expect("accepted write and damaged recovery finish");
    owner.terminate().await.expect("owner ended");
    observer.terminate().await.expect("observer ended");
    result
}

pub(crate) async fn after_revocation<T>(
    database: &Database,
    proxy: &Proxy,
    service: &revocation::Service,
    actor: &Actor,
    request: impl Future<Output = T>,
) -> T {
    let observer = database
        .connections(&database.user)
        .recover()
        .await
        .expect("independent recovery observer");
    proxy.arm(Fault::DelayedCommit);
    let revoke = async {
        let pid = timeout(Duration::from_secs(3), proxy.intercepted())
            .await
            .expect("original write reached its lost commit response");
        let service = service.clone();
        let actor = actor.clone();
        let revoked = tokio::spawn(async move {
            service
                .revoke(
                    actor,
                    Uuid::new_v4()
                        .to_string()
                        .parse()
                        .expect("revocation operation"),
                    revocation::Scope::Current,
                )
                .await
                .expect("later actual session revocation")
        });
        timeout(Duration::from_secs(3), async {
            loop {
                if observer.client().query_one("WITH RECURSIVE blocked(pid) AS (SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) UNION SELECT backend.pid FROM pg_stat_activity backend JOIN blocked ON blocked.pid=ANY(pg_blocking_pids(backend.pid)) WHERE backend.datname=current_database()) SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid IN (SELECT pid FROM blocked) AND wait_event_type='Lock' AND query = 'SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE')", &[&pid]).await.expect("revocation waits behind the original account lock").get::<_,bool>(0) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("revocation is queued before recovery takes the account lock");
        proxy.release();
        timeout(Duration::from_secs(5), revoked)
            .await
            .expect("revocation deadline")
            .expect("revocation task");
    };
    let (result, ()) = timeout(Duration::from_secs(15), async {
        tokio::join!(request, revoke)
    })
    .await
    .expect("accepted write and later revocation finish");
    assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.revocations WHERE id=$1 AND account_id=$2 AND mode='logout')", &[&actor.session_id(), &actor.account_id()]).await.expect("independent later revocation fact").get::<_,bool>(0));
    observer.terminate().await.expect("observer ended");
    result
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::runtime::Runtime;

    use super::*;
    use crate::{
        access::role::tests::{actor, start},
        identity::authentication::{self, Supplier, server},
    };

    #[test]
    fn original_work_is_identified_among_other_locked_transactions() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                let actor = actor(&database, &services, &mut supplier).await;
                let observer = database.connections(&database.user).recover().await.expect("another runtime connection");
                observer.client().batch_execute("BEGIN; SELECT pg_catalog.pg_advisory_xact_lock(2147483647, 2147483647)").await.expect("unrelated idle transaction owns an exclusive lock");
                let operation_id = Uuid::new_v4().to_string().parse().expect("operation");
                after_revocation(&database, &proxy, services.revocation(), &actor,
                    services.role().create(actor.clone(), operation_id, serde_json::from_value(serde_json::json!({"name":"Recovered Role", "level":20, "permissions":[]})).expect("legacy role input")))
                    .await.expect("accepted work recovers after actual revocation");
                observer.client().batch_execute("ROLLBACK").await.expect("unrelated transaction ends");
                let row = observer.client().query_one("SELECT (SELECT count(*) FROM access.changes WHERE operation_id=$1), EXISTS(SELECT 1 FROM identity.revocations WHERE id=$2 AND account_id=$3)", &[&operation_id, &actor.session_id(), &actor.account_id()]).await.expect("independent legacy facts before replay or drain");
                assert_eq!(row.get::<_, i64>(0), 1);
                assert!(row.get::<_, bool>(1));
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                proxy.stop().await;
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded recovery observation");
        }));
    }
}
