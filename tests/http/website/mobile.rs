//! Durable observations around a real browser's mobile replacement and result read.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio_postgres::Client;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    account: Uuid,
    person: Uuid,
    transfer: Uuid,
    operation: Uuid,
    message: Uuid,
    session: Uuid,
    originals: Vec<Uuid>,
    completed_at: Option<DateTime<Utc>>,
    later: Option<Uuid>,
}

#[derive(Default)]
pub(super) struct Observer {
    step: usize,
    original: Option<Request>,
    unchanged: Option<String>,
    mobile: Option<Vec<u8>>,
    expiries: Vec<(Uuid, DateTime<Utc>)>,
    read: Option<String>,
}

impl Observer {
    pub(super) fn complete(&self) -> bool {
        self.step == 4
    }

    pub(super) async fn observe(&mut self, client: &Client, material: &Path) {
        let phases = ["before", "after", "query-before", "query-after"];
        let Some(phase) = phases.get(self.step) else {
            return;
        };
        let path = material.join(format!("mobile-{phase}"));
        if !path.with_extension("request").exists() {
            return;
        }
        let request: Request = serde_json::from_slice(
            &std::fs::read(path.with_extension("request")).expect("mobile observation request"),
        )
        .expect("mobile observation binding");
        if let Some(original) = &self.original {
            assert_eq!(request.account, original.account);
            assert_eq!(request.person, original.person);
            assert_eq!(request.transfer, original.transfer);
            assert_eq!(request.operation, original.operation);
            assert_eq!(request.message, original.message);
            assert_eq!(request.session, original.session);
            assert_eq!(request.originals, original.originals);
            if self.step > 1 {
                assert_eq!(request.completed_at, original.completed_at);
            }
        }
        let unchanged: String = client.query_one(
            r#"SELECT jsonb_build_object(
                'person',(SELECT to_jsonb(person) FROM identity.persons person WHERE person.id=$1),
                'profile',(SELECT to_jsonb(account)-ARRAY['mobile','changed_at','sequence','version','seal'] FROM identity.accounts account WHERE account.id=$2 AND account.person_id=$1),
                'other_accounts',(SELECT jsonb_agg(to_jsonb(account) ORDER BY account.id) FROM identity.accounts account WHERE account.id<>$2),
                'other_sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY session.id) FROM identity.sessions session WHERE session.account_id<>$2)
            )::text"#,
            &[&request.person, &request.account],
        ).await.expect("mobile replacement preserves identity and other accounts").get(0);
        if let Some(before) = &self.unchanged {
            assert!(
                before == &unchanged,
                "replacement preserves the person, account profile and other accounts"
            );
        } else {
            self.unchanged = Some(unchanged);
        }
        let phone: Vec<u8> = client
            .query_one(
                "SELECT (mobile).hash FROM identity.accounts WHERE id=$1 AND person_id=$2",
                &[&request.account, &request.person],
            )
            .await
            .expect("observe account mobile ownership")
            .get(0);
        if self.step == 0 {
            assert_eq!(request.originals.len(), 2);
            assert!(request.originals.contains(&request.session));
            assert!(request.completed_at.is_none() && request.later.is_none());
            let sessions = client.query(
                "SELECT id,expires_at FROM identity.sessions WHERE account_id=$1 AND revoked_at IS NULL AND expires_at>clock_timestamp() ORDER BY id",
                &[&request.account],
            ).await.expect("original live session set");
            self.expiries = sessions
                .iter()
                .map(|row| (row.get("id"), row.get("expires_at")))
                .collect();
            assert_eq!(
                self.expiries.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
                request.originals
            );
            let prepared: bool = client.query_one(
                "SELECT (SELECT count(*) FROM identity.handoffs WHERE id=ANY($1))=2 AND NOT EXISTS(SELECT 1 FROM identity.replacements WHERE id=$2)",
                &[&request.originals, &request.transfer],
            ).await.expect("original Handoffs and absent completion").get(0);
            assert!(prepared);
            self.mobile = Some(phone);
        } else {
            assert!(
                self.mobile
                    .as_ref()
                    .is_some_and(|original| original != &phone),
                "the account's mobile ownership changed"
            );
            let time = request
                .completed_at
                .expect("browser observed original completion time");
            let replacement = client.query_one(
                "SELECT replacement.sessions, replacement.account_id=$2 AND replacement.person_id=$3 AND replacement.operation_id=$4 AND replacement.session_id=$5 AND replacement.message_id=$6 AND replacement.completed_at=$7 AND account.sequence=replacement.sequence AND account.changed_at=replacement.completed_at AND completion.transaction_id=replacement.transaction_id AS consistent FROM identity.replacements replacement JOIN identity.accounts account ON account.id=replacement.account_id JOIN identity.completions completion ON completion.family='mobile_completion' AND completion.operation_id=replacement.operation_id WHERE replacement.id=$1",
                &[&request.transfer, &request.account, &request.person, &request.operation, &request.session, &request.message, &time],
            ).await.expect("one permanent original replacement and completion");
            assert!(replacement.get::<_, bool>("consistent"));
            let mut frozen: Vec<Uuid> = replacement.get("sessions");
            frozen.sort();
            assert_eq!(frozen, request.originals);
            for (id, expires_at) in &self.expiries {
                let revoked: bool = client.query_one(
                    "SELECT session.account_id=$2 AND session.expires_at=$3 AND session.revoked_at=$4 AND revocation.mode='transfer' AND revocation.cause_id=$5 AND revocation.operation_id=$6 AND revocation.revoked_at=$4 AND revocation.transaction_id=replacement.transaction_id AND NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=session.id) FROM identity.sessions session JOIN identity.revocations revocation ON revocation.id=session.id JOIN identity.replacements replacement ON replacement.id=$5 WHERE session.id=$1",
                    &[id, &request.account, expires_at, &time, &request.transfer, &request.operation],
                ).await.expect("original session revocation and Handoff removal").get(0);
                assert!(revoked);
            }
        }
        if self.step >= 2 {
            let later = request
                .later
                .expect("new-number login before reading the original result");
            assert!(!request.originals.contains(&later));
            let retained: bool = client.query_one(
                "SELECT EXISTS(SELECT 1 FROM identity.sessions session JOIN identity.handoffs handoff ON handoff.id=session.id WHERE session.id=$1 AND session.account_id=$2 AND session.revoked_at IS NULL AND session.expires_at>clock_timestamp()) AND NOT EXISTS(SELECT 1 FROM identity.revocations WHERE id=$1)",
                &[&later, &request.account],
            ).await.expect("later Session and Handoff remain active").get(0);
            assert!(retained);
            let snapshot: String = client.query_one(
                r#"SELECT jsonb_build_object(
                    'account',(SELECT to_jsonb(account) FROM identity.accounts account WHERE account.id=$1),
                    'sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY session.id) FROM identity.sessions session WHERE session.account_id=$1),
                    'handoffs',(SELECT jsonb_agg(to_jsonb(handoff) ORDER BY handoff.id) FROM identity.handoffs handoff JOIN identity.sessions session ON session.id=handoff.id WHERE session.account_id=$1),
                    'replacement',(SELECT to_jsonb(replacement) FROM identity.replacements replacement WHERE replacement.id=$2),
                    'completion',(SELECT to_jsonb(completion) FROM identity.completions completion WHERE completion.family='mobile_completion' AND completion.operation_id=$3)
                )::text"#,
                &[&request.account, &request.transfer, &request.operation],
            ).await.expect("original-result read snapshot").get(0);
            if let Some(before) = &self.read {
                assert!(
                    before == &snapshot,
                    "reading the original outcome changes no identity, completion, Cookie handoff or session expiry"
                );
            } else {
                self.read = Some(snapshot);
            }
        }
        if self.step <= 1 {
            self.original = Some(request);
        }
        self.step += 1;
        std::fs::write(path.with_extension("ack"), "ok")
            .expect("mobile observation acknowledgement");
    }
}
