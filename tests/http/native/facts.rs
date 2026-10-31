//! Independent observations for the private native harness.
//!
//! Every value in this module comes from this run's own PostgreSQL connection with fixed,
//! parameterized queries. Replies contain IDs, counts, statuses and comparisons only; tokens,
//! hashes, ciphertext and raw rows never leave this module.

use std::{
    ops::AsyncFnOnce,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use platform::database::Session;
use serde_json::{Value, json};
use tokio::{sync::Mutex, time::timeout};
use uuid::Uuid;

use crate::database::connections;

pub(super) struct Facts {
    session: Mutex<Option<Session>>,
    poisoned: AtomicBool,
}

pub(super) struct Snapshot {
    pub(super) state: Value,
    pub(super) accounts: i64,
    pub(super) persons: i64,
    pub(super) sessions: i64,
    pub(super) revocations: i64,
    pub(super) logouts: i64,
    pub(super) handoffs: i64,
    pub(super) attempts: i64,
    pub(super) receipts: i64,
    pub(super) exchanges: i64,
    pub(super) authentications: i64,
    pub(super) registrations: i64,
}

impl Snapshot {
    pub(super) fn evidence(&self) -> Value {
        json!({
            "state": self.state,
            "accounts": self.accounts,
            "persons": self.persons,
            "sessions": self.sessions,
            "revocations": self.revocations,
            "logouts": self.logouts,
            "handoffs": self.handoffs,
            "attempts": self.attempts,
            "receipts": self.receipts,
            "exchanges": self.exchanges,
            "authentications": self.authentications,
            "registrations": self.registrations,
        })
    }
}

impl Facts {
    pub(super) fn unavailable() -> Self {
        Self {
            session: Mutex::new(None),
            poisoned: AtomicBool::new(true),
        }
    }

    pub(super) async fn connect(server: &fixture::Server, name: &str) -> Result<Self, String> {
        let session = connections(server, name, "clerk")
            .recover()
            .await
            .map_err(|error| format!("observer connection failed: {error}"))?;
        Ok(Self {
            session: Mutex::new(Some(session)),
            poisoned: AtomicBool::new(false),
        })
    }

    async fn with_session<T>(
        &self,
        operation: impl AsyncFnOnce(&tokio_postgres::Client) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut guard = self.session.lock().await;
        let session = guard
            .as_ref()
            .ok_or_else(|| "observer connection already stopped".to_owned())?;
        match timeout(Duration::from_secs(5), operation(session.client())).await {
            Ok(result) => result,
            Err(_) => {
                self.poisoned.store(true, Ordering::SeqCst);
                if let Some(session) = guard.take() {
                    let _ = timeout(Duration::from_secs(5), session.terminate()).await;
                }
                Err("observer query timed out; connection cannot be reused".into())
            }
        }
    }

    pub(super) async fn snapshot(&self) -> Result<Snapshot, String> {
        self.with_session(async |client| {
            let row = client
                .query_one(
                    r#"SELECT
                        (SELECT count(*) FROM identity.accounts),
                        (SELECT count(*) FROM identity.persons),
                        (SELECT count(*) FROM identity.sessions),
                        (SELECT count(*) FROM identity.revocations),
                        (SELECT count(*) FROM identity.logouts),
                        (SELECT count(*) FROM identity.handoffs),
                        (SELECT count(*) FROM identity.attempts),
                        (SELECT count(*) FROM identity.receipts),
                        (SELECT count(*) FROM identity.exchanges),
                        (SELECT count(*) FROM identity.authentications),
                        (SELECT count(*) FROM identity.registrations),
                        json_build_object(
                            'accounts', (SELECT coalesce(json_agg(json_build_array(id, person_id, suspension_id IS NOT NULL, cancellation_id IS NOT NULL) ORDER BY id), '[]'::json) FROM identity.accounts),
                            'persons', (SELECT coalesce(json_agg(json_build_array(id, revision_id) ORDER BY id), '[]'::json) FROM identity.persons),
                            'sessions', (SELECT coalesce(json_agg(json_build_array(id, account_id, method, created_at, expires_at, revoked_at) ORDER BY id), '[]'::json) FROM identity.sessions),
                            'revocations', (SELECT coalesce(json_agg(json_build_array(id, operation_id, mode, revoked_at) ORDER BY id), '[]'::json) FROM identity.revocations),
                            'logouts', (SELECT coalesce(json_agg(json_build_array(id, initiator_id, scope, sessions, revoked_at) ORDER BY id), '[]'::json) FROM identity.logouts),
                            'handoffs', (SELECT coalesce(json_agg(json_build_array(id, expires_at) ORDER BY id), '[]'::json) FROM identity.handoffs),
                            'attempts', (SELECT coalesce(json_agg(json_build_array(operation_id, status) ORDER BY operation_id), '[]'::json) FROM identity.attempts),
                            'receipts', (SELECT coalesce(json_agg(json_build_array(id, operation_id) ORDER BY id), '[]'::json) FROM identity.receipts),
                            'exchanges', (SELECT coalesce(json_agg(json_build_array(operation_id, status) ORDER BY operation_id), '[]'::json) FROM identity.exchanges),
                            'authentications', (SELECT coalesce(json_agg(json_build_array(operation_id, status) ORDER BY operation_id), '[]'::json) FROM identity.authentications),
                            'registrations', (SELECT coalesce(json_agg(json_build_array(id, authentication_id, created_at, expires_at) ORDER BY id), '[]'::json) FROM identity.registrations))::text"#,
                    &[],
                )
                .await
                .map_err(|error| format!("snapshot query failed: {error}"))?;
            Ok(Snapshot {
                accounts: row.get(0),
                persons: row.get(1),
                sessions: row.get(2),
                revocations: row.get(3),
                logouts: row.get(4),
                handoffs: row.get(5),
                attempts: row.get(6),
                receipts: row.get(7),
                exchanges: row.get(8),
                authentications: row.get(9),
                registrations: row.get(10),
                state: serde_json::from_str(&row.get::<_, String>(11)).map_err(|_| "observer projection is malformed")?,
            })
        })
        .await
    }

    /// Prove that a successful registration response is durable before the relay drops it.
    pub(super) async fn durable_registration(&self, operation: Uuid) -> Result<bool, String> {
        self.with_session(async |client| {
            let row = client
                .query_one(
                    r#"SELECT EXISTS(
                        SELECT 1 FROM identity.attempts attempt
                        JOIN identity.receipts receipt ON receipt.operation_id = attempt.operation_id
                        JOIN identity.sessions session ON session.receipt_id = receipt.id
                        WHERE attempt.operation_id = $1
                          AND attempt.status = 'registered'
                          AND session.revoked_at IS NULL
                          AND EXISTS (SELECT 1 FROM identity.issuances issuance WHERE issuance.id = session.id)
                    )"#,
                    &[&operation],
                )
                .await
                .map_err(|error| format!("registration durability query failed: {error}"))?;
            Ok(row.get(0))
        })
        .await
    }

    /// Prove that a successful logout response has its permanent scope and revocations.
    pub(super) async fn durable_logout(
        &self,
        operation: Uuid,
        scope: &str,
        initiator: Uuid,
    ) -> Result<bool, String> {
        self.with_session(async |client| {
            let row = client
                .query_one(
                    r#"SELECT EXISTS(
                        SELECT 1 FROM identity.logouts logout
                        WHERE logout.id = $1 AND logout.scope = $2 AND logout.initiator_id = $3
                          AND (SELECT count(*) FROM identity.revocations revocation
                               WHERE revocation.logout_id = logout.id) = cardinality(logout.sessions)
                          AND NOT EXISTS (SELECT 1 FROM identity.handoffs handoff
                               WHERE handoff.id = ANY(logout.sessions))
                          AND EXISTS (SELECT 1 FROM identity.completions completion
                               WHERE completion.family = 'logout' AND completion.operation_id = logout.id)
                    )"#,
                    &[&operation, &scope, &initiator],
                )
                .await
                .map_err(|error| format!("logout durability query failed: {error}"))?;
            Ok(row.get(0))
        })
        .await
    }

    /// Complete registration evidence after the client recovered the original Session.
    pub(super) async fn registration(&self, operation: Uuid) -> Result<Value, String> {
        self.with_session(async |client| {
            let row = client
                .query_one(
                    r#"SELECT
                        attempt.id, attempt.status,
                        authentication.operation_id, authentication.status, authentication.app_id, authentication.transport,
                        exchange.status, exchange.app_id,
                        receipt.id, receipt.account_id, receipt.person_id, receipt.revision_id,
                        creation.source,
                        session.id, session.revoked_at IS NOT NULL, session.app_id, session.method,
                        issuance.id,
                        (SELECT count(*) FROM identity.accounts candidate WHERE candidate.person_id = receipt.person_id),
                        (SELECT count(*) FROM identity.persons candidate WHERE candidate.id = receipt.person_id),
                        (SELECT count(*) FROM identity.receipts candidate WHERE candidate.account_id = receipt.account_id),
                        (SELECT count(*) FROM identity.sessions candidate WHERE candidate.receipt_id = receipt.id),
                        (SELECT count(*) FROM identity.attempts candidate WHERE candidate.authentication_id = attempt.authentication_id),
                        (SELECT count(*) FROM identity.handoffs handoff WHERE handoff.id = session.id),
                        (SELECT count(*) FROM identity.creations candidate WHERE candidate.id = account.id
                            AND candidate.source = 'registration' AND candidate.operation_id = $1)
                    FROM identity.attempts attempt
                    JOIN identity.authentications authentication ON authentication.id = attempt.authentication_id
                    JOIN identity.exchanges exchange ON exchange.id = authentication.exchange_id
                    JOIN identity.receipts receipt ON receipt.operation_id = attempt.operation_id
                    JOIN identity.accounts account ON account.id = receipt.account_id
                    JOIN identity.creations creation ON creation.id = account.id
                    JOIN identity.sessions session ON session.receipt_id = receipt.id
                    JOIN identity.issuances issuance ON issuance.id = session.id
                    WHERE attempt.operation_id = $1"#,
                    &[&operation],
                )
                .await
                .map_err(|error| format!("registration facts are unavailable: {error}"))?;
            let attempt_status: String = row.get(1);
            let authentication_status: String = row.get(3);
            let exchange_status: String = row.get(6);
            let creation_source: String = row.get(12);
            let revoked: bool = row.get(14);
            let app_id: String = row.get(4);
            let exchange_app_id: String = row.get(7);
            let transport: String = row.get(5);
            let method: String = row.get(16);
            let counts = [row.get::<_, i64>(18), row.get(19), row.get(20), row.get(21), row.get(22), row.get(23), row.get(24)];
            if attempt_status != "registered"
                || authentication_status != "registrable"
                || exchange_status != "resolved"
                || creation_source != "registration"
                || revoked
                || app_id != "mini-fixture"
                || exchange_app_id != "mini-fixture"
                || transport != "bearer"
                || method != "mobile"
                || counts.iter().any(|count| *count != 1)
            {
                return Err("registration facts do not match one recovered mini account".into());
            }
            Ok(json!({
                "operation": operation,
                "attemptId": row.get::<_, Uuid>(0),
                "attemptStatus": attempt_status,
                "authenticationOperation": row.get::<_, Uuid>(2),
                "authenticationStatus": authentication_status,
                "exchangeStatus": exchange_status,
                "appId": app_id,
                "receiptId": row.get::<_, Uuid>(8),
                "accountId": row.get::<_, Uuid>(9),
                "personId": row.get::<_, Uuid>(10),
                "revisionId": row.get::<_, Uuid>(11),
                "creationSource": creation_source,
                "sessionId": row.get::<_, Uuid>(13),
                "issuanceId": row.get::<_, Uuid>(17),
                "sessionMethod": method,
                "counts": {"accounts": counts[0], "persons": counts[1], "receipts": counts[2],
                    "sessions": counts[3], "attempts": counts[4], "handoffs": counts[5], "creations": counts[6]},
                "oneOriginalOperation": true,
                "sessionUsable": !revoked,
            }))
        })
        .await
    }

    /// Complete permanent Logout evidence for one lost-success case.
    pub(super) async fn logout(
        &self,
        operation: Uuid,
        actor: Uuid,
        target: Option<Uuid>,
        later: Option<Uuid>,
        foreign_session: Option<Uuid>,
    ) -> Result<Value, String> {
        self.with_session(async |client| {
            let row = client
                .query_one(
                    r#"SELECT logout.account_id, logout.person_id, logout.scope, logout.initiator_id,
                        logout.sessions, logout.revoked_at,
                        (SELECT count(*) FROM identity.revocations revocation WHERE revocation.logout_id = logout.id),
                        (SELECT coalesce(array_agg(revocation.id ORDER BY revocation.id), '{}') FROM identity.revocations revocation WHERE revocation.logout_id = logout.id),
                        (SELECT count(*) FROM identity.handoffs handoff WHERE handoff.id = ANY(logout.sessions)),
                        (SELECT count(*) FROM identity.completions completion WHERE completion.family = 'logout' AND completion.operation_id = logout.id),
                        (SELECT count(*) FROM identity.sessions session WHERE session.id = logout.initiator_id
                            AND session.revoked_at IS NULL AND session.expires_at > clock_timestamp()),
                        (SELECT count(*) FROM identity.accounts account WHERE account.id = logout.account_id
                            AND account.suspension_id IS NULL AND account.cancellation_id IS NULL),
                        (SELECT count(*) FROM identity.revocations revocation WHERE revocation.logout_id=logout.id AND revocation.revoked_at<>logout.revoked_at)
                    FROM identity.logouts logout WHERE logout.id = $1"#,
                    &[&operation],
                )
                .await
                .map_err(|error| format!("logout facts are unavailable: {error}"))?;
            let account: Uuid = row.get(0);
            let person: Uuid = row.get(1);
            let scope: String = row.get(2);
            let initiator: Uuid = row.get(3);
            let sessions: Vec<Uuid> = row.get(4);
            let revoked_ids: Vec<Uuid> = row.get(7);
            let mut expected = sessions.clone();
            expected.sort_unstable();
            if initiator != actor || revoked_ids != expected || row.get::<_, i64>(12) != 0 {
                return Err("permanent Logout belongs to another initiator or collection".into());
            }
            let count = row.get::<_, i64>(6);
            if count != i64::try_from(sessions.len()).map_err(|_| "logout count overflow".to_owned())? {
                return Err("permanent Logout revocations do not match its sealed collection".into());
            }
            if row.get::<_, i64>(8) != 0 || row.get::<_, i64>(9) != 1 {
                return Err("permanent Logout retained a handoff or lost its completion".into());
            }
            let mut checks = serde_json::Map::new();
            let expected_scope = match scope.as_str() {
                "current" => {
                    if sessions.as_slice() != [initiator] || count != 1 || row.get::<_, i64>(10) != 0 {
                        return Err("current Logout did not seal exactly its initiator".into());
                    }
                    "current"
                }
                "all" => {
                    if !sessions.contains(&initiator) || row.get::<_, i64>(10) != 0 {
                        return Err("all Logout omitted its initiator".into());
                    }
                    "all"
                }
                "other" => {
                    let target = target.ok_or("other Logout has no bound target")?;
                    if sessions.as_slice() != [target] || sessions.contains(&initiator) || row.get::<_, i64>(10) != 1 {
                        return Err("other Logout did not seal exactly its other target".into());
                    }
                    "other"
                }
                _ => return Err("permanent Logout has an unknown scope".into()),
            };
            let account_live = row.get::<_, i64>(11) == 1;
            if !account_live {
                return Err("permanent Logout changed its account eligibility".into());
            }
            if let Some(later) = later {
                let row = client
                    .query_one(
                        r#"SELECT (SELECT count(*) FROM identity.sessions session
                                WHERE session.id = $1 AND session.account_id = $2
                                  AND session.revoked_at IS NULL AND session.expires_at > clock_timestamp()),
                            (SELECT count(*) FROM identity.sessions session
                                WHERE session.id = $1 AND session.account_id = $2
                                  AND session.id = ANY($3))"#,
                        &[&later, &account, &sessions],
                    )
                    .await
                    .map_err(|error| format!("later login facts are unavailable: {error}"))?;
                let later_valid = row.get::<_, i64>(0) == 1 && row.get::<_, i64>(1) == 0;
                checks.insert("laterLoginExcluded".into(), json!(row.get::<_, i64>(1) == 0));
                checks.insert("laterLoginUsable".into(), json!(later_valid));
                if !later_valid {
                    return Err("a later login was added to the original Logout or became unusable".into());
                }
            }
            let mut foreign_isolated = None;
            if let Some(foreign) = foreign_session {
                let row = client
                    .query_one(
                        "SELECT account_id FROM identity.sessions WHERE id = $1",
                        &[&foreign],
                    )
                    .await
                    .map_err(|error| format!("foreign account facts are unavailable: {error}"))?;
                let foreign_account: Uuid = row.get(0);
                foreign_isolated = Some(foreign_account != account);
                if foreign_account == account {
                    return Err("cross-account isolation used a session of the same account".into());
                }
            }
            let target_revoked = target.map(|target| {
                sessions.contains(&target) && revoked_ids.contains(&target)
            });
            checks.insert("scope".into(), json!(expected_scope));
            checks.insert("initiatorPreserved".into(), json!(initiator == actor));
            checks.insert("handoffsDeleted".into(), json!(row.get::<_, i64>(8) == 0));
            checks.insert("singleCompletion".into(), json!(row.get::<_, i64>(9) == 1));
            checks.insert("initiatorUsable".into(), json!(row.get::<_, i64>(10) == 1));
            checks.insert("singleSealedCollection".into(), json!(revoked_ids == expected));
            if let Some(value) = target_revoked {
                checks.insert("targetRevoked".into(), json!(value));
            }
            if let Some(value) = foreign_isolated {
                checks.insert("otherAccountIsolated".into(), json!(value));
            }
            Ok(json!({
                "operation": operation,
                "accountId": account,
                "personId": person,
                "scope": scope,
                "initiatorId": initiator,
                "revokedAt": row.get::<_, chrono::DateTime<chrono::Utc>>(5),
                "count": count,
                "revokedIds": revoked_ids,
                "sealedIds": sessions,
                "checks": checks,
            }))
        })
        .await
    }

    /// A bounded, read-only comparison around a dependency failure.
    pub(super) async fn retention(&self, actor: Uuid, armed: &Snapshot) -> Result<Value, String> {
        self.with_session(async |client| {
            let row = client
                .query_one(
                    r#"SELECT count(*), count(*) FILTER (WHERE revoked_at IS NULL AND expires_at > clock_timestamp())
                       FROM identity.sessions WHERE id = $1"#,
                    &[&actor],
                )
                .await
                .map_err(|error| format!("retained session facts are unavailable: {error}"))?;
            if row.get::<_, i64>(0) != 1 || row.get::<_, i64>(1) != 1 {
                return Err("the dependency failure consumed or cleared the selected Session".into());
            }
            Ok(json!({
                "actor": actor,
                "sessionRetained": true,
                "sessionUsable": true,
            }))
        })
        .await
        .map(|mut value| {
            value["armedCounts"] = armed.evidence();
            value
        })
    }

    pub(super) async fn stop(&self) -> Result<(), String> {
        let session = self.session.lock().await.take();
        if self.poisoned.load(Ordering::SeqCst) {
            return Err("observer termination was unconfirmed after a timeout".into());
        }
        match session {
            Some(session) => timeout(Duration::from_secs(5), session.terminate())
                .await
                .map_err(|_| "observer termination timed out".to_owned())?
                .map_err(|_| "observer termination failed".to_owned()),
            None => Ok(()),
        }
    }
}
