//! Independent durable observations around the browser's original cancellations.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio_postgres::Client;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    operation: Uuid,
    account: Uuid,
    person: Uuid,
    revision: Uuid,
    session: Uuid,
    grant: Uuid,
    cancellation: Uuid,
    cancelled_at: DateTime<Utc>,
    posts: u64,
    replacement: Option<Replacement>,
    same_mobile: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Replacement {
    account: Uuid,
    person: Uuid,
}

#[derive(Default)]
pub(super) struct Observer {
    step: usize,
    success: Option<Uuid>,
    lost: Option<Uuid>,
}

impl Observer {
    pub(super) fn complete(&self) -> bool {
        self.step == 3
    }

    pub(super) async fn observe(&mut self, client: &Client, material: &Path) {
        let phases = ["success-after", "lost-after", "lost-later"];
        let Some(phase) = phases.get(self.step) else {
            return;
        };
        let path = material.join(format!("cancellation-{phase}"));
        if !path.with_extension("request").exists() {
            return;
        }
        let request: Request = serde_json::from_slice(
            &std::fs::read(path.with_extension("request"))
                .expect("cancellation observation request"),
        )
        .expect("cancellation observation binding");
        assert_eq!(request.posts, 1, "the browser sends exactly one POST");
        match self.step {
            0 => {
                assert!(request.replacement.is_none() && request.same_mobile.is_none());
                self.success = Some(request.account);
            }
            1 => {
                assert!(request.replacement.is_none() && request.same_mobile.is_none());
                assert_ne!(request.account, self.success.expect("normal cancellation"));
                self.lost = Some(request.account);
            }
            2 => {
                assert_eq!(request.same_mobile, Some(true));
                assert_eq!(request.account, self.lost.expect("lost cancellation"));
            }
            _ => unreachable!(),
        }
        observe_fact(client, &request).await;
        if self.step == 2 {
            observe_later(client, &request).await;
        }
        std::fs::write(path.with_extension("ack"), "ok")
            .expect("cancellation observation acknowledgement");
        self.step += 1;
    }
}

async fn observe_fact(client: &Client, request: &Request) {
    let fact = client.query_opt(
        r#"SELECT id, cancelled_at,
            account_id=$2 AND person_id=$3 AND revision_id=$4 AND operator_id=$4
                AND sequence=2 AND operation_id=$1 AND mobile IS NOT NULL AS bound,
            EXISTS(SELECT 1 FROM identity.persons WHERE id=$3 AND sequence=2) AS person_advanced,
            EXISTS(SELECT 1 FROM identity.completions completion
                WHERE completion.family='cancellation' AND completion.operation_id=$1
                    AND completion.action='self' AND completion.target=$2::text
                    AND completion.subject='session' AND completion.person_id=$3
                    AND completion.revision_id=$4
                    AND completion.created_at=identity.cancellations.cancelled_at
                    AND completion.expires_at IS NULL) AS completion,
            EXISTS(SELECT 1 FROM identity.accounts account
                JOIN identity.projections projection ON projection.account_id=account.id
                    AND projection.id=account.cancellation_id
                WHERE account.id=$2 AND account.person_id=$3
                    AND account.cancellation_id=identity.cancellations.id
                    AND account.mobile IS NULL AND account.id_number IS NULL
                    AND account.suspension_id IS NULL AND account.sequence=2
                    AND account.version=account.sequence
                    AND account.changed_at=identity.cancellations.cancelled_at
                    AND projection.cause='cancellation' AND projection.person_id=account.person_id
                    AND projection.sequence=account.sequence
                    AND projection.projected_at=account.changed_at
                    AND projection.operation_id=identity.cancellations.operation_id
                    AND projection.transaction_id=identity.cancellations.transaction_id) AS account_tombstone,
            EXISTS(SELECT 1 FROM access.holders holder
                JOIN access.projections projection ON projection.holder_id=holder.id
                    AND projection.id=holder.cancellation_id
                WHERE holder.id=$2 AND projection.id=identity.cancellations.id
                    AND projection.person_id=$3 AND holder.sequence=projection.sequence
                    AND holder.changed_at=projection.projected_at
                    AND projection.operation_id=identity.cancellations.operation_id
                    AND projection.transaction_id=identity.cancellations.transaction_id) AS holder,
            NOT EXISTS(SELECT 1 FROM identity.sessions WHERE account_id=$2) AS sessions,
            NOT EXISTS(SELECT 1 FROM identity.issuances WHERE account_id=$2) AS issuances,
            NOT EXISTS(SELECT 1 FROM identity.revocations WHERE account_id=$2) AS revocations,
            NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$5) AS handoff,
            NOT EXISTS(SELECT 1 FROM identity.receipts WHERE account_id=$2) AS receipts,
            (SELECT count(*)=1 FROM identity.avatars
                WHERE account_id=$2
                    AND released_at=identity.cancellations.cancelled_at) AS avatar,
            NOT EXISTS(SELECT 1 FROM access.grants
                WHERE person_id=$3 AND termination IS NULL) AS grants,
            EXISTS(SELECT 1 FROM access.grants grant_record
                JOIN access.terminations termination ON termination.id=grant_record.id
                WHERE grant_record.id=$6 AND grant_record.person_id=$3
                    AND grant_record.termination='cancellation'
                    AND termination.cancellation_id=identity.cancellations.id
                    AND termination.person_id=$3 AND termination.operation_id=$1
                    AND termination.operator_id=$4 AND termination.revision_id=$4
                    AND termination.mode='cancellation'
                    AND termination.terminated_at=identity.cancellations.cancelled_at) AS granted_service_ended,
            NOT EXISTS(SELECT 1 FROM access.terminations termination
                WHERE termination.cancellation_id=identity.cancellations.id
                    AND (termination.person_id<>$3 OR termination.operation_id<>$1
                        OR termination.mode<>'cancellation'
                        OR termination.terminated_at<>identity.cancellations.cancelled_at)) AS terminations,
            (SELECT count(*) FROM identity.cancellations WHERE operation_id=$1)=1 AS one_cancellation,
            (SELECT count(*) FROM identity.completions
                WHERE family='cancellation' AND operation_id=$1)=1 AS one_completion
        FROM identity.cancellations WHERE operation_id=$1"#,
        &[&request.operation, &request.account, &request.person, &request.revision, &request.session, &request.grant],
    )
    .await
    .expect("observe one durable cancellation")
    .expect("cancellation exists");
    assert_eq!(fact.get::<_, Uuid>("id"), request.cancellation);
    assert_eq!(
        fact.get::<_, DateTime<Utc>>("cancelled_at"),
        request.cancelled_at
    );
    for column in [
        "bound",
        "person_advanced",
        "completion",
        "account_tombstone",
        "holder",
        "sessions",
        "issuances",
        "revocations",
        "handoff",
        "receipts",
        "avatar",
        "grants",
        "granted_service_ended",
        "terminations",
        "one_cancellation",
        "one_completion",
    ] {
        assert!(
            fact.get::<_, bool>(column),
            "cancellation preserves {column}"
        );
    }
}

async fn observe_later(client: &Client, request: &Request) {
    let replacement = request.replacement.as_ref().expect("new registration");
    let later: bool = client
        .query_one(
            r#"SELECT (SELECT count(*) FROM identity.cancellations)=2
                AND (SELECT count(*) FROM identity.cancellations
                    WHERE person_id=$2)=1
                AND NOT EXISTS(SELECT 1 FROM identity.cancellations
                    WHERE account_id=$3 OR person_id=$4)
                AND NOT EXISTS(SELECT 1 FROM identity.completions
                    WHERE family='cancellation' AND target=$3::text)
                AND EXISTS(SELECT 1 FROM identity.accounts
                    WHERE id=$3 AND person_id=$4 AND cancellation_id IS NULL
                        AND mobile IS NOT NULL)
                AND EXISTS(SELECT 1 FROM identity.persons
                    WHERE id=$4 AND sequence=1)
                AND EXISTS(SELECT 1 FROM identity.sessions
                    WHERE account_id=$3 AND revoked_at IS NULL
                        AND expires_at>clock_timestamp())
                AND NOT EXISTS(SELECT 1 FROM identity.sessions WHERE account_id=$1)"#,
            &[
                &request.account,
                &request.person,
                &replacement.account,
                &replacement.person,
            ],
        )
        .await
        .expect("observe the separate replacement registration")
        .get(0);
    assert!(
        later,
        "a later ordinary registration neither inherits nor rewrites the old cancellation"
    );
}
