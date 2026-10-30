//! Independent durable observations around the browser's original and replayed termination.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio_postgres::Client;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    operation: Uuid,
    grant: Uuid,
    person: Uuid,
    account: Uuid,
    session: Uuid,
    replacement: Option<Uuid>,
    terminated_at: Option<DateTime<Utc>>,
}

#[derive(Default)]
pub(super) struct Observer {
    step: usize,
    original: Option<Request>,
    identity: Option<String>,
    replay: Option<String>,
}

impl Observer {
    pub(super) fn complete(&self) -> bool {
        self.step == 4
    }

    pub(super) async fn observe(&mut self, client: &Client, material: &Path) {
        let phases = [
            "original-before",
            "original-after",
            "replay-before",
            "replay-after",
        ];
        let Some(phase) = phases.get(self.step) else {
            return;
        };
        let path = material.join(format!("termination-{phase}"));
        if !path.with_extension("request").exists() {
            return;
        }
        let request: Request = serde_json::from_slice(
            &std::fs::read(path.with_extension("request"))
                .expect("termination observation request"),
        )
        .expect("termination observation binding");
        if let Some(original) = &self.original {
            assert_eq!(request.operation, original.operation);
            assert_eq!(request.grant, original.grant);
            assert_eq!(request.person, original.person);
            assert_eq!(request.account, original.account);
            if self.step == 1 {
                assert_eq!(request.session, original.session);
            } else {
                assert_ne!(
                    request.session, original.session,
                    "recovery uses a real new Session"
                );
                assert_eq!(request.terminated_at, original.terminated_at);
            }
        }
        let live: bool = client.query_one(
            "SELECT EXISTS(SELECT 1 FROM identity.sessions s JOIN identity.accounts a ON a.id=s.account_id WHERE s.id=$1 AND a.id=$2 AND a.person_id=$3 AND s.revoked_at IS NULL AND s.expires_at>clock_timestamp())",
            &[&request.session, &request.account, &request.person],
        ).await.expect("observe live same-account Session").get(0);
        assert!(
            live,
            "termination and recovery preserve the selected Session"
        );
        if self.step == 0 {
            let untouched: bool = client.query_one(
                "SELECT EXISTS(SELECT 1 FROM access.grants WHERE id=$1 AND person_id=$2 AND termination IS NULL) AND NOT EXISTS(SELECT 1 FROM access.terminations WHERE id=$1) AND NOT EXISTS(SELECT 1 FROM access.completions WHERE family='grant_termination' AND operation_id=$3)",
                &[&request.grant, &request.person, &request.operation],
            ).await.expect("observe eligible original Grant").get(0);
            assert!(untouched);
            assert!(request.terminated_at.is_none() && request.replacement.is_none());
        } else {
            let time = request
                .terminated_at
                .expect("browser's permanent receipt time");
            let ended: bool = client.query_one(
                "SELECT EXISTS(SELECT 1 FROM access.terminations t JOIN access.grants g ON g.id=t.id JOIN access.completions c ON c.family='grant_termination' AND c.operation_id=t.operation_id WHERE t.id=$1 AND t.person_id=$2 AND t.operation_id=$3 AND t.mode='self' AND t.operator_id=t.revision_id AND t.terminated_at=$4 AND g.termination='self' AND c.action='leave' AND c.target=$5 AND c.subject='session' AND c.person_id=$2 AND c.revision_id=t.revision_id AND c.expires_at IS NULL)",
                &[&request.grant, &request.person, &request.operation, &time, &request.grant.to_string()],
            ).await.expect("observe exact permanent self termination and completion").get(0);
            assert!(ended, "the original receipt is backed by permanent facts");
        }
        if self.step < 2 {
            let identity: String = client.query_one(
                "SELECT jsonb_build_object('accounts',(SELECT jsonb_agg(to_jsonb(a) ORDER BY id) FROM identity.accounts a),'sessions',(SELECT jsonb_agg(to_jsonb(s) ORDER BY id) FROM identity.sessions s),'revocations',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM identity.revocations r))::text", &[],
            ).await.expect("identity snapshot around original termination").get(0);
            if let Some(before) = &self.identity {
                assert!(
                    before == &identity,
                    "ending service preserves accounts, Sessions, expiry and revocations"
                );
            } else {
                self.identity = Some(identity);
            }
            self.original = Some(request);
        } else {
            let replacement = request.replacement.expect("new Grant before replay");
            assert_ne!(replacement, request.grant);
            let active: bool = client.query_one(
                "SELECT EXISTS(SELECT 1 FROM access.grants WHERE id=$1 AND person_id=$2 AND termination IS NULL AND suspension_id IS NULL) AND NOT EXISTS(SELECT 1 FROM access.terminations WHERE id=$1)",
                &[&replacement, &request.person],
            ).await.expect("observe the separate active replacement Grant").get(0);
            assert!(active, "replaying the original does not end the new Grant");
            let snapshot: String = client.query_one(
                r#"SELECT jsonb_build_object(
                    'grants',(SELECT jsonb_agg(to_jsonb(g) ORDER BY id) FROM access.grants g),
                    'assignments',(SELECT jsonb_agg(to_jsonb(a) ORDER BY id) FROM access.assignments a),
                    'suspensions',(SELECT jsonb_agg(to_jsonb(s) ORDER BY id) FROM access.suspensions s),
                    'resumptions',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM access.resumptions r),
                    'terminations',(SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM access.terminations t),
                    'completions',(SELECT jsonb_agg(to_jsonb(c) ORDER BY family,operation_id) FROM access.completions c),
                    'accounts',(SELECT jsonb_agg(to_jsonb(a) ORDER BY id) FROM identity.accounts a),
                    'sessions',(SELECT jsonb_agg(to_jsonb(s) ORDER BY id) FROM identity.sessions s),
                    'revocations',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM identity.revocations r),
                    'numbers',(SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM workforce.numbers n)
                )::text"#, &[],
            ).await.expect("business snapshot around original replay").get(0);
            if let Some(before) = &self.replay {
                assert!(
                    before == &snapshot,
                    "original replay changes no business fact or Session expiry"
                );
            } else {
                self.replay = Some(snapshot);
            }
        }
        std::fs::write(path.with_extension("ack"), "ok")
            .expect("termination observation acknowledgement");
        self.step += 1;
    }
}
