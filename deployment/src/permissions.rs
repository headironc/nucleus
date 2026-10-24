//! Verify deployment ownership and the shared runtime permission boundary.

use domain::database;
use platform::database::{Connections, User};
use thiserror::Error;
use tokio::time::timeout;

/// Check deployment ownership and runtime grants in one bounded session.
pub async fn verify(connections: &Connections, runtime_user: &User) -> Result<(), Error> {
    let session = connections.recover().await?;
    let result = timeout(crate::OPERATION_BUDGET, async {
        let client = session.client();
        let owner: bool = client
            .query_one(
                r#"SELECT count(*) = 4 AND bool_and(relowner = current_user::regrole)
                FROM pg_catalog.pg_class
                WHERE oid IN ('security.challenges'::regclass, 'security.mobiles'::regclass,
                    'security.encryptions'::regclass, 'security.messages'::regclass)"#,
                &[],
            )
            .await?
            .try_get(0)?;
        if !owner {
            return Err(Error::Ownership);
        }
        database::permissions::verify(client, runtime_user).await?;
        Ok(())
    })
    .await;
    let ended = session.terminate().await;
    let error = match result {
        Ok(Ok(())) => {
            ended?;
            return Ok(());
        }
        Ok(Err(error)) => error,
        Err(error) => error.into(),
    };
    match ended {
        Ok(()) => Err(error),
        Err(termination) => Err(Error::Combined {
            cause: Box::new(error),
            termination,
        }),
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("deployment identity does not own all key tables")]
    Ownership,
    #[error("{0}")]
    Database(#[from] database::Error),
    #[error("{0}")]
    Connection(#[from] platform::database::Error),
    #[error("deployment permission verification budget expired")]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error("deployment permission verification and connection termination failed")]
    Combined {
        #[source]
        cause: Box<Error>,
        termination: platform::database::Error,
    },
}

impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Database(error.into())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::time::Duration;

    use domain::database;
    use platform::database::User;
    use tokio::{runtime::Runtime, time::timeout};
    use tokio_postgres::error::SqlState;
    use uuid::Uuid;

    use crate::{
        keys, migration,
        permissions::{Error, verify},
    };

    #[test]
    fn accepts_custom_users_and_rejects_privilege_overlap() {
        let server = crate::tests::server();
        Runtime::new().expect("runtime").block_on(async {
            let suffix = Uuid::new_v4().simple().to_string();
            let deployment_user = format!(r#"owner"{suffix}"#);
            let runtime_user = format!("worker'{suffix}");
            let admin = server
                .connections("postgres", "postgres")
                .recover()
                .await
                .expect("admin");
            for user in [&deployment_user, &runtime_user] {
                let sql: String = admin
                    .client()
                    .query_one(
                        r#"SELECT format('CREATE ROLE %I LOGIN PASSWORD %L', $1::text, $2::text)"#,
                        &[user, &fixture::PASSWORD],
                    )
                    .await
                    .expect("quote role")
                    .get(0);
                admin
                    .client()
                    .batch_execute(&sql)
                    .await
                    .expect("create fictional role");
            }
            admin.terminate().await.expect("end admin");
            let name = crate::tests::database_for(&server, &deployment_user, &runtime_user).await;
            let connections = server.connections(&name, &deployment_user);
            let deployment_user = User::new(deployment_user).expect("deployment user");
            let runtime_user = User::new(runtime_user).expect("runtime user");
            migration::run(&connections, &runtime_user)
                .await
                .expect("custom migration");
            verify(&connections, &runtime_user)
                .await
                .expect("custom permissions");
            let metadata = crate::tests::metadata(0, false);
            keys::initialize(&connections, &metadata)
                .await
                .expect("custom initialization");
            let runtime = server
                .connections(&name, runtime_user.as_ref())
                .recover()
                .await
                .expect("runtime");
            domain::identity::keys::database::verify(&metadata, runtime.client())
                .await
                .expect("read access");
            let error = runtime
                .client()
                .batch_execute("DELETE FROM security.challenges")
                .await
                .expect_err("write denied");
            assert_eq!(error.code(), Some(&SqlState::INSUFFICIENT_PRIVILEGE));
            runtime.terminate().await.expect("end runtime");
            assert!(matches!(
                migration::run(&connections, &deployment_user).await,
                Err(migration::Error::Database(_))
            ));
            assert!(
                migration::run(
                    &connections,
                    &User::new("postgres".into()).expect("admin user")
                )
                .await
                .is_err()
            );
            assert!(matches!(
                verify(
                    &connections,
                    &User::new("clerk".into()).expect("ungranted user")
                )
                .await,
                Err(Error::Database(database::Error::Permissions))
            ));
            let admin = server
                .connections(&name, "postgres")
                .recover()
                .await
                .expect("admin");
            let sql: String = admin
                .client()
                .query_one(
                    r#"SELECT format('GRANT %I TO %I', $1::text, $2::text)"#,
                    &[&deployment_user, &runtime_user],
                )
                .await
                .expect("quote membership")
                .get(0);
            admin
                .client()
                .batch_execute(&sql)
                .await
                .expect("invalid membership fixture");
            admin.terminate().await.expect("end admin");
            assert!(matches!(
                verify(&connections, &runtime_user).await,
                Err(Error::Database(database::Error::Permissions))
            ));
            assert!(migration::run(&connections, &runtime_user).await.is_err());
        });
    }

    #[test]
    fn shares_runtime_key_permissions_and_requires_the_exact_deployment_owner() {
        let server = crate::tests::server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let name = crate::tests::database(&server).await;
                let connections = server.connections(&name, "steward");
                let user = User::new("clerk".into()).expect("runtime identity");
                migration::run(&connections, &user).await.expect("migrations");
                let runtime = server.connections(&name, "clerk").recover().await.expect("runtime");
                let admin = server.connections(&name, "postgres").recover().await.expect("admin");
                let group = format!("group_{}", Uuid::new_v4().simple());
                // Interpolated names and privileges are fixed fixtures or generated identifiers.
                admin.client().batch_execute(&format!("CREATE ROLE {group}"))
                    .await.expect("fictional group");
                for table in ["challenges", "mobiles", "encryptions", "messages"] {
                    verify(&connections, &user).await.expect("ordinary permissions");
                    admin.client().batch_execute(&format!("REVOKE SELECT ON security.{table} FROM clerk"))
                        .await.expect("remove required read access");
                    assert!(matches!(database::permissions::verify(runtime.client(), &user).await, Err(database::Error::Permissions)));
                    assert!(matches!(verify(&connections, &user).await, Err(Error::Database(database::Error::Permissions))));
                    admin.client().batch_execute(&format!("GRANT SELECT ON security.{table} TO clerk"))
                        .await.expect("restore read access");

                    for recipient in ["clerk", "PUBLIC", &group] {
                        for privilege in ["INSERT", "UPDATE", "DELETE", "TRUNCATE", "REFERENCES", "TRIGGER", "MAINTAIN", "INSERT (fingerprint)", "UPDATE (fingerprint)", "REFERENCES (fingerprint)"] {
                            admin.client().batch_execute(&format!("GRANT {privilege} ON security.{table} TO {recipient}"))
                                .await.expect("excess key privilege");
                            // A group exercises both inherited and SET ROLE-only grants.
                            for inherited in [false, true] {
                                if recipient == group {
                                    let inherit = if inherited { "TRUE" } else { "FALSE" };
                                    admin.client().batch_execute(&format!("GRANT {group} TO clerk WITH INHERIT {inherit}, SET TRUE"))
                                        .await.expect("runtime group membership");
                                } else if inherited {
                                    continue;
                                }
                                assert!(matches!(database::permissions::verify(runtime.client(), &user).await, Err(database::Error::Permissions)), "{table}: {privilege}, {recipient}, inherited={inherited}");
                                assert!(matches!(verify(&connections, &user).await, Err(Error::Database(database::Error::Permissions))), "{table}: {privilege}, {recipient}, inherited={inherited}");
                                if recipient == group {
                                    admin.client().batch_execute(&format!("REVOKE {group} FROM clerk"))
                                        .await.expect("remove membership");
                                }
                            }
                            admin.client().batch_execute(&format!("REVOKE {privilege} ON security.{table} FROM {recipient}"))
                                .await.expect("remove excess key privilege");
                            verify(&connections, &user).await.expect("ordinary permissions restored");
                        }
                    }

                    admin.client().batch_execute(&format!("ALTER TABLE security.{table} OWNER TO postgres"))
                        .await.expect("change only the deployment owner");
                    database::permissions::verify(runtime.client(), &user).await.expect("runtime policy remains satisfied");
                    assert!(matches!(verify(&connections, &user).await, Err(Error::Ownership)), "{table}");
                    admin.client().batch_execute(&format!("ALTER TABLE security.{table} OWNER TO steward"))
                        .await.expect("restore the exact owner");
                    verify(&connections, &user).await.expect("deployment owner restored");
                }
                runtime.terminate().await.expect("runtime ended");
                admin.terminate().await.expect("admin ended");
                connections.close().await;
            }).await.expect("permission parity test budget");
        });
    }

    #[test]
    fn rejects_trigger_bypass_privileges_and_effective_replica_sessions() {
        // Parameter grants are cluster-wide; PUBLIC must not affect other tests.
        let server = crate::tests::Server::new(fixture::Server::start());
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let bootstrap = server.connections("postgres", "postgres").recover().await.expect("bootstrap");
                for user in ["steward", "clerk", "delegated", "intermediary"] {
                    let sql: String = bootstrap.client().query_one(
                        "SELECT format('CREATE ROLE %I LOGIN PASSWORD %L', $1::text, $2::text)",
                        &[&user, &fixture::PASSWORD],
                    ).await.expect("quote fictional role").get(0);
                    bootstrap.client().batch_execute(&sql).await.expect("fictional role");
                }
                bootstrap.terminate().await.expect("bootstrap ended");
                let name = crate::tests::database_for(&server, "steward", "clerk").await;
                let connections = server.connections(&name, "steward");
                let user = User::new("clerk".into()).expect("runtime identity");
                migration::run(&connections, &user).await.expect("complete migrations");
                let runtime = server.connections(&name, "clerk").recover().await.expect("runtime");
                let admin = server.connections(&name, "postgres").recover().await.expect("admin");
                for privilege in ["SET", "ALTER SYSTEM"] {
                    for (recipient, membership) in [
                        ("clerk", ""),
                        ("PUBLIC", ""),
                        ("delegated", "GRANT delegated TO clerk WITH INHERIT TRUE, SET TRUE"),
                        ("delegated", "GRANT delegated TO clerk WITH INHERIT FALSE, SET TRUE"),
                        ("delegated", "GRANT delegated TO intermediary WITH INHERIT FALSE, SET TRUE; GRANT intermediary TO clerk WITH INHERIT FALSE, SET TRUE"),
                    ] {
                        admin.client().batch_execute(membership).await.expect("membership fixture");
                        admin.client().batch_execute(&format!("GRANT {privilege} ON PARAMETER session_replication_role TO {recipient}"))
                            .await.expect("isolated parameter grant");
                        assert!(matches!(database::permissions::verify(runtime.client(), &user).await, Err(database::Error::Permissions)));
                        assert!(matches!(verify(&connections, &user).await, Err(Error::Database(database::Error::Permissions))));
                        admin.client().batch_execute(&format!("REVOKE {privilege} ON PARAMETER session_replication_role FROM {recipient}; REVOKE delegated, intermediary FROM clerk; REVOKE delegated FROM intermediary"))
                            .await.expect("remove bypass capability");
                        verify(&connections, &user).await.expect("repaired grants accepted");
                        database::permissions::verify(runtime.client(), &user).await.expect("ordinary runtime accepted");
                    }
                }
                let error = runtime.client().batch_execute("SET session_replication_role=replica").await.expect_err("runtime cannot bypass triggers");
                assert_eq!(error.code(), Some(&SqlState::INSUFFICIENT_PRIVILEGE));
                admin.client().batch_execute("ALTER ROLE clerk SET session_replication_role=replica").await.expect("unsafe login default fixture");
                let replica = server.connections(&name, "clerk").recover().await.expect("fresh runtime session");
                let role: String = replica.client().query_one("SHOW session_replication_role", &[]).await.expect("effective mode").get(0);
                assert_eq!(role, "replica");
                assert!(matches!(database::permissions::verify(replica.client(), &user).await, Err(database::Error::Permissions)));
                replica.terminate().await.expect("replica ended");
                admin.client().batch_execute("ALTER ROLE clerk RESET session_replication_role").await.expect("remove unsafe default");
                runtime.terminate().await.expect("runtime ended");
                admin.terminate().await.expect("admin ended");
                connections.close().await;
            }).await.expect("parameter privilege test budget");
        });
    }

    #[test]
    fn rejects_server_capabilities_through_direct_and_indirect_membership() {
        let server = crate::tests::server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let suffix = Uuid::new_v4().simple().to_string();
                let deployment_user = format!("owner_{suffix}");
                let runtime_user = format!("worker_{suffix}");
                let intermediary = format!("group_{suffix}");
                let bootstrap = server.connections("postgres", "postgres").recover().await.expect("bootstrap");
                for user in [&deployment_user, &runtime_user, &intermediary] {
                    let sql: String = bootstrap.client().query_one(
                        "SELECT format('CREATE ROLE %I LOGIN PASSWORD %L', $1::text, $2::text)",
                        &[user, &fixture::PASSWORD],
                    ).await.expect("quote fictional identity").get(0);
                    bootstrap.client().batch_execute(&sql).await.expect("fictional role");
                }
                bootstrap.terminate().await.expect("bootstrap ended");
                let name = crate::tests::database_for(&server, &deployment_user, &runtime_user).await;
                let connections = server.connections(&name, &deployment_user);
                let user = User::new(runtime_user.clone()).expect("runtime identity");
                migration::run(&connections, &user).await.expect("valid migration");
                verify(&connections, &user).await.expect("valid permissions");
                let runtime = server.connections(&name, &runtime_user).recover().await.expect("runtime");
                let admin = server.connections(&name, "postgres").recover().await.expect("admin");
                admin.client().query_one(
                    "SELECT set_config('nucleus.deployment_user', $1, false), set_config('nucleus.runtime_user', $2, false)",
                    &[&deployment_user, &runtime_user],
                ).await.expect("bind identities for role preparation");

                for role in ["pg_read_server_files", "pg_write_server_files", "pg_execute_server_program"] {
                    for recipient in [&runtime_user, &deployment_user] {
                        for indirect in [false, true] {
                            for inherited in [false, true] {
                                let member = if indirect { &intermediary } else { recipient };
                                // All interpolated names are fixed roles or generated UUID identifiers.
                                let inherit = if inherited { "TRUE" } else { "FALSE" };
                                admin.client().batch_execute(&format!("GRANT {role} TO {member} WITH INHERIT {inherit}, SET TRUE"))
                                    .await.expect("isolated server capability");
                                if indirect {
                                    admin.client().batch_execute(&format!("GRANT {intermediary} TO {recipient} WITH INHERIT {inherit}, SET TRUE"))
                                        .await.expect("indirect membership");
                                }
                                let membership = runtime.client().query_one(
                                    "SELECT pg_has_role($1::text, $2::text, 'MEMBER'), pg_has_role($1::text, $2::text, 'USAGE'), pg_has_role($1::text, $2::text, 'SET')",
                                    &[recipient, &role],
                                ).await.expect("actual role capabilities");
                                assert!(membership.get::<_, bool>(0));
                                assert_eq!(membership.get::<_, bool>(1), inherited);
                                assert!(membership.get::<_, bool>(2));

                                let application = database::permissions::verify(runtime.client(), &user).await;
                                let deployment = verify(&connections, &user).await;
                                if recipient == &runtime_user {
                                    assert!(matches!(application, Err(database::Error::Permissions)), "{role}, indirect={indirect}, inherited={inherited}");
                                    assert!(matches!(deployment, Err(Error::Database(database::Error::Permissions))));
                                } else {
                                    application.expect("runtime identity remains restricted");
                                    deployment.expect("runtime grants remain valid");
                                }
                                let error = admin.client().batch_execute(include_str!("../roles.sql")).await.expect_err("role preparation rejects server capabilities");
                                assert_eq!(error.code(), Some(&SqlState::RAISE_EXCEPTION));
                                assert!(matches!(migration::run(&connections, &user).await, Err(migration::Error::Database(_))));

                                if indirect {
                                    admin.client().batch_execute(&format!("REVOKE {intermediary} FROM {recipient}"))
                                        .await.expect("remove indirect membership");
                                }
                                admin.client().batch_execute(&format!("REVOKE {role} FROM {member}"))
                                    .await.expect("remove isolated server capability");
                                admin.client().batch_execute(include_str!("../roles.sql")).await.expect("ordinary roles accepted");
                                verify(&connections, &user).await.expect("ordinary runtime accepted");
                            }
                        }
                    }
                }
                migration::run(&connections, &user).await.expect("unchanged history remains valid");
                runtime.terminate().await.expect("runtime ended");
                admin.terminate().await.expect("admin ended");
                connections.close().await;
            }).await.expect("permission boundary test budget");
        });
    }

    #[test]
    fn challenge_references_only_its_own_key_table_and_protects_retained_versions() {
        let server = crate::tests::server();
        Runtime::new().expect("runtime").block_on(async {
            let name = crate::tests::database(&server).await;
            let connections = server.connections(&name, "steward");
            let user = User::new("clerk".into()).expect("runtime identity");
            migration::run(&connections, &user).await.expect("migrations");
            keys::initialize(&connections, &crate::tests::metadata(0, false)).await.expect("keys");
            let owner = connections.recover().await.expect("owner");
            owner.client().batch_execute(
                "INSERT INTO security.mobiles VALUES (9, decode(repeat('00',32),'hex'), false); INSERT INTO security.encryptions VALUES (9, decode(repeat('00',32),'hex'), false)"
            ).await.expect("same key ID exists in other purposes");
            let runtime = server.connections(&name, "clerk").recover().await.expect("runtime");
            let sql = "INSERT INTO identity.challenges (id, operation_id, purpose, hash, key_id, mobile, source, calls, created_at, expires_at) VALUES ('550e8400-e29b-41d4-a716-446655440000', '00000000-0000-0000-0000-000000000000', 'authentication', decode(repeat('01',32),'hex'), 9, ROW(decode(repeat('02',37),'hex'), decode(repeat('03',32),'hex'))::identity.mobile, '192.0.2.1', 1, '2026-10-03 00:00:00+00', '2026-10-03 00:00:10+00')";
            let error = runtime.client().batch_execute(sql).await.expect_err("other purposes cannot satisfy the reference");
            assert_eq!(error.code(), Some(&SqlState::FOREIGN_KEY_VIOLATION));
            assert_eq!(error.as_db_error().expect("foreign key error").constraint(), Some("challenges_key_id_fkey"));
            owner.client().batch_execute("INSERT INTO security.challenges VALUES (9, decode(repeat('00',32),'hex'), false)").await.expect("retained challenge key");
            runtime.client().batch_execute(sql).await.expect("correct purpose permits the reference");
            let error = owner.client().batch_execute("DELETE FROM security.challenges WHERE id = 9").await.expect_err("referenced version cannot be removed");
            assert_eq!(error.code(), Some(&SqlState::FOREIGN_KEY_VIOLATION));
            owner.client().batch_execute("DELETE FROM security.mobiles WHERE id = 9; DELETE FROM security.encryptions WHERE id = 9").await.expect("unrelated purposes do not own this reference");
            runtime.terminate().await.expect("end runtime");
            owner.terminate().await.expect("end owner");
        });
    }

    #[test]
    fn migrated_domains_reject_invalid_nested_values_under_the_runtime_identity() {
        let server = crate::tests::server();
        Runtime::new().expect("runtime").block_on(async {
            let name = crate::tests::database(&server).await;
            let owner = server.connections(&name, "steward");
            let user = User::new("clerk".into()).expect("runtime identity");
            migration::run(&owner, &user).await.expect("migrations");
            keys::initialize(&owner, &crate::tests::metadata(0, false)).await.expect("keys");
            verify(&owner, &user).await.expect("delivered privileges");
            let runtime = server.connections(&name, "clerk").recover().await.expect("runtime");
            for (value, hash, state) in [
                ("NULL", "decode(repeat('03', 32), 'hex')", SqlState::NOT_NULL_VIOLATION),
                ("decode(repeat('02', 37), 'hex')", "NULL", SqlState::NOT_NULL_VIOLATION),
                ("decode(repeat('02', 36), 'hex')", "decode(repeat('03', 32), 'hex')", SqlState::CHECK_VIOLATION),
                ("decode(repeat('02', 37), 'hex')", "decode(repeat('03', 31), 'hex')", SqlState::CHECK_VIOLATION),
                ("decode(repeat('02', 37), 'hex')", "decode(repeat('03', 33), 'hex')", SqlState::CHECK_VIOLATION),
            ] {
                // Expressions are fixed fixtures, not external SQL input.
                let sql = format!("SELECT ROW({value}, {hash})::identity.mobile");
                let error = runtime.client().query_one(&sql, &[]).await.expect_err("invalid composite rejected");
                assert_eq!(error.code(), Some(&state));
            }
            let sql = "INSERT INTO identity.challenges (id, operation_id, purpose, hash, key_id, mobile, source, calls, created_at, expires_at) VALUES ('550e8400-e29b-41d4-a716-446655440000', '00000000-0000-0000-0000-000000000000', 'authentication', decode(repeat('01', 32), 'hex'), 0, ROW(decode(repeat('02', 37), 'hex'), decode(repeat('03', 32), 'hex'))::identity.mobile, '192.0.2.1', 1, '2026-10-03 00:00:00+00', '2026-10-03 00:00:10+00')";
            runtime.client().batch_execute(sql).await.expect("valid grouped representation");
            for expression in [
                "ROW((NULL::identity.mobile).value, decode(repeat('03', 32), 'hex'))::identity.mobile",
                "ROW(decode(repeat('02', 37), 'hex'), (NULL::identity.mobile).hash)::identity.mobile",
            ] {
                let invalid = sql.replace("ROW(decode(repeat('02', 37), 'hex'), decode(repeat('03', 32), 'hex'))::identity.mobile", expression)
                    .replace("550e8400-e29b-41d4-a716-446655440000", "550e8400-e29b-41d4-a716-446655440001")
                    .replace("00000000-0000-0000-0000-000000000000", "ffffffff-ffff-ffff-ffff-ffffffffffff")
                    .replace("repeat('01', 32)", "repeat('04', 32)");
                let error = runtime.client().batch_execute(&invalid).await.expect_err("typed null child rejected");
                assert_eq!(error.code(), Some(&SqlState::CHECK_VIOLATION));
            }
            for (original, replacement, state) in [
                ("ROW(decode(repeat('02', 37), 'hex'), decode(repeat('03', 32), 'hex'))::identity.mobile", "NULL", SqlState::NOT_NULL_VIOLATION),
                ("'192.0.2.1'", "'192.0.2.1/24'", SqlState::CHECK_VIOLATION),
                ("'192.0.2.1'", "'::ffff:192.0.2.1'", SqlState::CHECK_VIOLATION),
                ("'192.0.2.1', 1", "'192.0.2.1', 2", SqlState::CHECK_VIOLATION),
                ("'2026-10-03 00:00:10+00'", "'2026-10-03 00:00:11+00'", SqlState::CHECK_VIOLATION),
                ("'authentication'", "'mobile_change'", SqlState::CHECK_VIOLATION),
            ] {
                let error = runtime.client().batch_execute(&sql.replace(original, replacement)).await.expect_err("invalid row rejected");
                assert_eq!(error.code(), Some(&state));
            }
            // Keep identity collision and token occupancy as separate real constraints.
            let error = runtime.client().batch_execute(&sql.replace("550e8400-e29b-41d4-a716-446655440000", "550e8400-e29b-41d4-a716-446655440001")).await.expect_err("operation collision");
            assert_eq!(error.as_db_error().expect("database error").constraint(), Some("challenges_operation_id_key"));
            let error = runtime.client().batch_execute(&sql.replace("550e8400-e29b-41d4-a716-446655440000", "550e8400-e29b-41d4-a716-446655440001")
                .replace("00000000-0000-0000-0000-000000000000", "ffffffff-ffff-ffff-ffff-ffffffffffff")).await.expect_err("token collision");
            assert_eq!(error.as_db_error().expect("database error").constraint(), Some("challenges_key_id_hash_key"));
            runtime.terminate().await.expect("end runtime");
        });
    }
}
