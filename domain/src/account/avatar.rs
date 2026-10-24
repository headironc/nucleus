//! A qualified current actor may read a known account's current avatar.

use tokio::time::timeout_at;

use crate::{
    account::{Error, Service},
    identity::{account, avatar, current::Actor},
};

impl Service {
    pub async fn fetch_avatar(
        &self,
        actor: &Actor,
        id: account::Id,
    ) -> Result<avatar::Current, Error> {
        self.registry.ready()?;
        let lease = self.context.connections().lease().await?;
        let budget = self.context.read_budget();
        let deadline = budget.deadline();
        let result = timeout_at(deadline, async {
            let client = lease.client();
            budget
                .run(client, self.roles.anchors(), |view| async move {
                    let (_, _, now) = self.current.at(view).fetch_person(client, actor).await?;
                    let account = self
                        .accounts
                        .at(view)
                        .state(client, id, now)
                        .await?
                        .filter(|account| account.current().is_some())
                        .ok_or(Error::Absent)?;
                    let snapshot = avatar::find_by_account(client, &account).await?.ok_or(
                        avatar::Error::Facts {
                            rule: "a current account retains its current avatar",
                        },
                    )?;
                    Ok((snapshot, now))
                })
                .await
        })
        .await
        .map_err(Error::from)
        .and_then(std::convert::identity);
        let (snapshot, now) = self.finish(lease, deadline, result).await?;
        Ok(self.avatar.sign(snapshot, now)?)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc, time::Duration};

    use serde_json::json;
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;

    use crate::{
        access::{
            appointment,
            role::tests::{actor, start},
        },
        account::{Failure, tests::register_manager},
        identity::{
            authentication::{Database, Supplier, server},
            avatar, operation, revocation,
        },
    };

    fn operation() -> operation::Id {
        Uuid::new_v4().to_string().parse().expect("operation ID")
    }

    fn proof(avatar: &avatar::Current) -> avatar::link::Proof {
        let url = url::Url::parse(avatar.link().href()).expect("signed avatar URL");
        let digest = url
            .path()
            .strip_prefix("/api/v1/avatars/")
            .expect("image route")
            .strip_suffix(".webp")
            .expect("WebP suffix")
            .parse()
            .expect("digest");
        let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        avatar::link::Proof::new(
            digest,
            query["key_id"].parse().expect("key ID"),
            query["expires_at"].parse().expect("expiry"),
            query["signature"].parse().expect("signature"),
        )
    }

    #[test]
    fn known_account_avatars_need_no_management_right_and_keep_paused_targets_visible() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(40), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let manager = Box::pin(actor(&database, &services, &mut supplier)).await;
                let target = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                let viewer = Box::pin(register_manager(&database, &services, &mut supplier)).await;
                let candidate = services.account().candidate(&manager, "13800000000".parse().expect("phone")).await.expect("target");
                let id = candidate.identity().id();
                let observer = database.connections(&database.user).recover().await.expect("independent SQL reader");
                let work = Box::pin(async {
                    assert_eq!(services.account().candidate(&viewer, "13800000000".parse().expect("phone")).await.err().expect("viewer has no management right").failure(), Failure::Denied);
                    let before: String = observer.client().query_one("SELECT jsonb_build_array((SELECT jsonb_agg(to_jsonb(avatar) ORDER BY id) FROM identity.avatars avatar), (SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session))::text", &[]).await.expect("independent original avatars and sessions").get(0);
                    let expected: avatar::Revision = observer.client().query_one("SELECT avatar FROM identity.accounts WHERE id=$1", &[&id]).await.expect("target revision").get(0);
                    for reader in [&viewer, &target] {
                        let value = services.account().fetch_avatar(reader, id).await.expect("known target readable by any current actor");
                        assert_eq!(value.revision(), expected);
                        assert_eq!(services.avatar().read(proof(&value)).await.expect("signed anonymous image").bytes(), include_bytes!("../../../assets/avatars/0-cat.webp"));
                    }
                    assert_ne!(services.account().fetch_avatar(&viewer, viewer.account_id()).await.expect("self through known-account entry").revision(), expected);
                    let after: String = observer.client().query_one("SELECT jsonb_build_array((SELECT jsonb_agg(to_jsonb(avatar) ORDER BY id) FROM identity.avatars avatar), (SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session))::text", &[]).await.expect("reads preserve avatar and session facts").get(0);
                    assert_eq!(before, after);
                    let confirmation = json!({"personId":candidate.identity().person_id(), "revisionId":candidate.identity().revision_id(), "mobile":"13800000000"});
                    Box::pin(services.suspension().suspend(manager.clone(), operation(), id, serde_json::from_value(confirmation.clone()).expect("confirmed target"))).await.expect("real suspension");
                    assert_eq!(services.account().fetch_avatar(&viewer, id).await.expect("paused target remains visible").revision(), expected);
                    assert_eq!(services.account().fetch_avatar(&target, viewer.account_id()).await.err().expect("previously authenticated paused caller is invalid").failure(), Failure::Invalid);
                    let cancelled=Box::pin(services.cancellation().cancel_account(manager, operation(), id, serde_json::from_value(confirmation).expect("confirmed cancellation"))).await.expect("real target cancellation");
                    assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.accounts WHERE id=$1 AND person_id=$2 AND cancellation_id=$3 AND mobile IS NULL AND id_number IS NULL AND suspension_id IS NULL AND avatar=$4) AND EXISTS(SELECT 1 FROM identity.avatars WHERE account_id=$1 AND released_at=$5) AND NOT EXISTS(SELECT 1 FROM identity.avatars WHERE account_id=$1 AND released_at IS NULL)", &[&id,&candidate.identity().person_id(),&cancelled.id(),&expected,&cancelled.cancelled_at()]).await.expect("cancelled target tombstone with retained released content").get::<_,bool>(0));
                    for hidden in [id, Uuid::new_v4().to_string().parse().expect("unknown account")] {
                        assert_eq!(services.account().fetch_avatar(&viewer, hidden).await.err().expect("unknown and cancelled targets hidden").failure(), Failure::Absent);
                    }
                    services.revocation().revoke(viewer.clone(), operation(), revocation::Scope::Current).await.expect("actual caller logout");
                    assert_eq!(services.account().fetch_avatar(&viewer, id).await.err().expect("decisive identity precedes target absence").failure(), Failure::Invalid);
                    services.drain().await.expect("drained"); context.stop().await.expect("runtime ended");
                });
                tokio::select! { biased; _=supplier.observe()=>panic!("avatar reads and management do not call verification"), ()=work=>{} }
                observer.terminate().await.expect("observer ended"); supplier.stop().await;
            })).await.expect("bounded known-account avatar lifecycle");
        }));
    }

    #[test]
    fn avatar_reads_distinguish_missing_required_metadata_from_a_query_failure() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for damage in [None, Some("DELETE FROM identity.avatars"), Some("UPDATE identity.avatars SET width=256, height=256"), Some("UPDATE identity.avatars SET digest=decode(repeat('00',32),'hex')"), Some("UPDATE identity.avatars SET size=size+1")] {
                for current in [false, true] {
                    timeout(Duration::from_secs(25), Box::pin(async {
                        let database = Database::initialized(Arc::clone(&server)).await;
                        let mut supplier = Supplier::new().await;
                        let (context, services) = start(database.connections(&database.user), &supplier).await;
                        let actor = Box::pin(actor(&database, &services, &mut supplier)).await;
                        let owner = database.connections("postgres").recover().await.expect("isolated database owner");
                        let work = Box::pin(async {
                            if let Some(sql) = damage {
                                owner.client().batch_execute(&format!("BEGIN; SET LOCAL session_replication_role=replica; {sql}; COMMIT")).await.expect("isolated metadata damage");
                            } else {
                                owner.client().batch_execute(&format!("REVOKE SELECT ON identity.avatars FROM {}", database.user)).await.expect("ordinary read failure");
                            }
                            if current {
                                let error = services.current().read(&actor).await.err().expect("self profile withholds bad avatar");
                                assert_eq!(error.unreliable(), damage.is_some(), "{error:?}");
                            } else {
                                let error = services.account().fetch_avatar(&actor, actor.account_id()).await.err().expect("withhold bad avatar");
                                assert_eq!(error.failure(), Failure::Unavailable);
                                assert_eq!(error.unreliable(), damage.is_some(), "{error:?}");
                            }
                            assert_eq!(services.account().registry.ready().is_err(), damage.is_some());
                            if damage.is_none() {
                                owner.client().batch_execute(&format!("GRANT SELECT ON identity.avatars TO {}", database.user)).await.expect("restore disposable permission");
                                services.account().fetch_avatar(&actor, actor.account_id()).await.expect("query failure did not close admission");
                                services.current().read(&actor).await.expect("shared projection remains usable");
                            }
                            services.close(); services.drain().await.expect("drained"); context.stop().await.expect("runtime ended");
                        });
                        tokio::select! { biased; _=supplier.observe()=>panic!("damaged reads never call verification"), ()=work=>{} }
                        owner.terminate().await.expect("owner ended"); supplier.stop().await;
                    })).await.expect("bounded avatar damage and ordinary failure");
                }
            }
        }));
    }

    #[test]
    fn avatar_read_snapshot_retains_its_qualified_actor_until_the_next_request() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(25), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy=Proxy::start(&server).await;
                let (context, services) = start(authentication::proxied(&database,&proxy), &supplier).await;
                let actor = Box::pin(actor(&database, &services, &mut supplier)).await;
                let observer = database.connections(&database.user).recover().await.expect("independent SQL observer");
                let work = Box::pin(async {
                    let expected: avatar::Revision = observer.client().query_one("SELECT avatar FROM identity.accounts WHERE id=$1", &[&actor.account_id()]).await.expect("original revision").get(0);
                    proxy.arm(Fault::PausedReadEnd);
                    let service = services.account().clone(); let reader = actor.clone();
                    let pending = tokio::spawn(async move { service.fetch_avatar(&reader, reader.account_id()).await });
                    let backend=timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("the qualified avatar snapshot committed before its receipt is released");
                    assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid=$1 AND state='idle' AND xact_start IS NULL AND query='COMMIT')",&[&backend]).await.expect("the exact read backend has ended its qualified snapshot").get::<_,bool>(0));
                    services.revocation().revoke(actor.clone(), operation(), revocation::Scope::Current).await.expect("real logout after the qualified snapshot committed");
                    assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.revocations WHERE id=$1)", &[&actor.session_id()]).await.expect("logout committed before the snapshot receipt was delivered").get::<_,bool>(0));
                    proxy.release();
                    assert_eq!(pending.await.expect("read task").expect("original qualified snapshot").revision(), expected);
                    assert_eq!(services.account().fetch_avatar(&actor, actor.account_id()).await.err().expect("next snapshot refuses the revoked actor").failure(), Failure::Invalid);
                    services.drain().await.expect("drained"); context.stop().await.expect("runtime ended");
                });
                tokio::select! { biased; _=supplier.observe()=>panic!("snapshot reads never call verification"), ()=work=>{} }
                observer.terminate().await.expect("observer ended"); proxy.stop().await; supplier.stop().await;
            })).await.expect("bounded avatar snapshot and logout");
        }));
    }

    #[test]
    fn avatar_read_deadline_and_cancellation_end_the_actual_query_connection() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(20), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let actor = Box::pin(actor(&database, &services, &mut supplier)).await;
                let owner = database.connections("postgres").recover().await.expect("isolated lock owner");
                let observer = database.connections(&database.user).recover().await.expect("independent SQL observer");
                let blocker: i32 = owner.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("lock owner PID").get(0);
                let query="SELECT $2::uuid AS avatar, avatar.digest, avatar.size, avatar.width, avatar.height
                FROM identity.avatars avatar WHERE avatar.account_id=$1 AND avatar.released_at IS NULL";
                let work = Box::pin(async {
                    for cancelled in [false, true] {
                        owner.client().batch_execute("BEGIN; LOCK TABLE identity.avatars IN ACCESS EXCLUSIVE MODE").await.expect("block avatar query");
                        let service = services.account().clone(); let reader = actor.clone();
                        let started = std::time::Instant::now();
                        let pending = tokio::spawn(async move { service.fetch_avatar(&reader, reader.account_id()).await });
                        let backend: i32 = timeout(Duration::from_secs(2), async { loop {
                            if let Some(row) = observer.client().query_opt("SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) AND wait_event_type='Lock' AND query=$2", &[&blocker,&query]).await.expect("actual target read waits") { break row.get(0); }
                            tokio::task::yield_now().await;
                        }}).await.expect("bounded query wait");
                        if cancelled {
                            pending.abort(); assert!(pending.await.err().expect("cancelled reader").is_cancelled());
                        } else {
                            let error = timeout(Duration::from_secs(4), pending).await.expect("three-second read deadline").expect("reader task").err().expect("unfinished snapshot withheld");
                            assert_eq!(error.failure(), Failure::Unavailable); assert!(!error.unreliable(), "{error:?}");
                            assert!(started.elapsed() >= Duration::from_millis(2500) && started.elapsed() < Duration::from_millis(4500));
                        }
                        owner.client().batch_execute("ROLLBACK").await.expect("release query gate");
                        timeout(Duration::from_secs(3), async { loop {
                            if !observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid=$1)", &[&backend]).await.expect("actual read backend ended").get::<_,bool>(0) { break; }
                            tokio::task::yield_now().await;
                        }}).await.expect("discarded reader connection");
                        services.account().fetch_avatar(&actor, actor.account_id()).await.expect("new connection remains usable");
                    }
                    services.drain().await.expect("drained"); context.stop().await.expect("runtime ended");
                });
                tokio::select! { biased; _=supplier.observe()=>panic!("interrupted reads never verify"), ()=work=>{} }
                observer.terminate().await.expect("observer ended"); owner.terminate().await.expect("owner ended"); supplier.stop().await;
            })).await.expect("bounded avatar reader cleanup");
        }));
    }
}
