//! Independent facts around the website's original admission, replay and member read.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio_postgres::Client;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    operation: Uuid,
    input: Input,
    operator: Operator,
    session: Uuid,
    receipt: Option<Receipt>,
}

#[derive(PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    entry: String,
    target: Target,
    role: Role,
}

#[derive(PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Target {
    account_id: Uuid,
    person_id: Uuid,
    revision_id: Uuid,
    mobile: String,
    assignment_id: Uuid,
    suspension_id: Option<Uuid>,
    grant: Option<serde_json::Value>,
}

#[derive(PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Role {
    id: Uuid,
    change_id: Uuid,
}

#[derive(PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Operator {
    account_id: Uuid,
    person_id: Uuid,
}

#[derive(PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Receipt {
    grant_id: Uuid,
    assignment_id: Uuid,
    granted_at: DateTime<Utc>,
}

#[derive(Default)]
pub(super) struct Observer {
    step: usize,
    original: Option<Request>,
    receipt: Option<Receipt>,
    identity: Option<String>,
    committed: Option<String>,
}

impl Observer {
    pub(super) fn complete(&self) -> bool {
        self.step == 6
    }

    pub(super) fn evidence(&self) -> serde_json::Value {
        assert!(self.complete());
        let original = self.original.as_ref().expect("original admission");
        let receipt = self.receipt.as_ref().expect("committed admission");
        serde_json::json!({
            "operation": original.operation,
            "accountId": original.input.target.account_id,
            "personId": original.input.target.person_id,
            "grantId": receipt.grant_id,
            "assignmentId": receipt.assignment_id,
            "grantedAt": receipt.granted_at,
            "phases": self.step,
            "before": {"grants": 0, "assignments": 0, "completions": 0},
            "after": {"grants": 1, "assignments": 1, "completions": 1},
            "defaultRole": true,
            "replayAndReadUnchanged": true,
            "identityUnchanged": true,
        })
    }

    pub(super) async fn observe(&mut self, client: &Client, material: &Path) {
        let phases = [
            "original-before",
            "original-after",
            "replay-before",
            "replay-after",
            "current-before",
            "current-after",
        ];
        let Some(phase) = phases.get(self.step) else {
            return;
        };
        let path = material.join(format!("admission-{phase}"));
        if !path.with_extension("request").exists() {
            return;
        }
        let request: Request = serde_json::from_slice(
            &std::fs::read(path.with_extension("request")).expect("admission observation request"),
        )
        .expect("admission observation binding");
        if let Some(original) = &self.original {
            assert!(
                request.operation == original.operation
                    && request.input == original.input
                    && request.operator == original.operator,
                "every phase retains the complete original admission"
            );
            if self.step < 4 {
                assert!(
                    request.session == original.session,
                    "original and replay use the same manager Session"
                );
            }
        }
        let target = &request.input.target;
        assert_eq!(request.input.entry, "institution");
        assert_eq!(target.mobile, "13900000017");
        assert!(target.suspension_id.is_none() && target.grant.is_none());
        assert_ne!(target.person_id, request.operator.person_id);
        let account = if self.step < 4 {
            request.operator.account_id
        } else {
            target.account_id
        };
        let person = if self.step < 4 {
            request.operator.person_id
        } else {
            target.person_id
        };
        let qualified: bool = client.query_one(
            "SELECT EXISTS(SELECT 1 FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id WHERE session.id=$1 AND account.id=$2 AND account.person_id=$3 AND session.revoked_at IS NULL AND session.expires_at>clock_timestamp() AND account.suspension_id IS NULL AND account.cancellation_id IS NULL)
             AND EXISTS(SELECT 1 FROM identity.accounts account JOIN access.holders holder ON holder.person_id=account.person_id JOIN access.roles role ON role.id=holder.role_id WHERE account.id=$4 AND account.person_id=$5 AND holder.cancellation_id IS NULL AND role.scope='institution' AND role.level=1000 AND 'institution.workforce'=ANY(role.permissions))
             AND NOT EXISTS(SELECT 1 FROM access.grants WHERE person_id=$5)
             AND EXISTS(SELECT 1 FROM identity.accounts account JOIN identity.persons person ON person.id=account.person_id JOIN access.holders holder ON holder.person_id=person.id WHERE account.id=$6 AND person.id=$7 AND person.revision_id=$8 AND holder.assignment_id=$9 AND account.suspension_id IS NULL AND account.cancellation_id IS NULL)",
            &[&request.session, &account, &person, &request.operator.account_id, &request.operator.person_id,
                &target.account_id, &target.person_id, &target.revision_id, &target.assignment_id],
        ).await.expect("independent manager and registered colleague facts").get(0);
        assert!(
            qualified,
            "institutional authority needs no manager Workforce Grant; reads use the colleague's own Session"
        );
        let counts = client.query_one(
            "SELECT (SELECT count(*) FROM access.grants WHERE person_id=$1), (SELECT count(*) FROM access.assignments WHERE person_id=$1), (SELECT count(*) FROM access.completions WHERE family='grant' AND operation_id=$2)",
            &[&target.person_id, &request.operation],
        ).await.expect("count independent admission facts");
        let expected: i64 = if self.step == 0 { 0 } else { 1 };
        for index in 0..3 {
            assert_eq!(counts.get::<_, i64>(index), expected);
        }
        if self.step == 0 {
            assert!(request.receipt.is_none());
        } else {
            let receipt = request
                .receipt
                .as_ref()
                .expect("committed admission receipt");
            if let Some(original) = &self.receipt {
                assert!(
                    receipt == original,
                    "replay and current read retain the original receipt"
                );
            }
            let paired: bool = client.query_one(
                "SELECT EXISTS(SELECT 1 FROM access.grants grant_record
                 JOIN access.assignments assignment ON assignment.id=grant_record.assignment_id AND assignment.grant_id=grant_record.id AND assignment.person_id=grant_record.person_id AND assignment.role_id=grant_record.role_id
                 JOIN access.roles role ON role.id=assignment.role_id
                 JOIN identity.revisions operator ON operator.id=grant_record.operator_id
                 JOIN access.completions completion ON completion.family='grant' AND completion.operation_id=grant_record.operation_id
                 WHERE grant_record.id=$1 AND assignment.id=$2 AND grant_record.person_id=$3 AND grant_record.revision_id=$4
                 AND grant_record.operation_id=$5 AND assignment.operation_id=$5 AND grant_record.entry='institution' AND grant_record.invitation_id IS NULL
                 AND grant_record.granted_at=$6 AND assignment.assigned_at=$6 AND grant_record.sequence=1 AND assignment.sequence=1
                 AND grant_record.suspension_id IS NULL AND grant_record.termination IS NULL AND assignment.previous_id IS NULL AND assignment.entry IS NULL
                 AND assignment.revision_id=$4 AND assignment.operator_id=operator.id AND operator.person_id=$7
                 AND role.scope='workforce' AND role.level=1 AND cardinality(role.permissions)=0 AND role.id=$8 AND assignment.change_id=$9
                 AND completion.subject='session' AND completion.action='create' AND completion.person_id=$7 AND completion.revision_id=operator.id AND completion.target=$10 AND completion.expires_at IS NULL)",
                &[&receipt.grant_id, &receipt.assignment_id, &target.person_id, &target.revision_id,
                    &request.operation, &receipt.granted_at, &request.operator.person_id,
                    &request.input.role.id, &request.input.role.change_id, &target.account_id.to_string()],
            ).await.expect("observe paired Grant, first Assignment and permanent completion").get(0);
            assert!(
                paired,
                "the website receipt matches one active ordinary-member admission"
            );
            let snapshot: String = client.query_one(
                "SELECT jsonb_build_object(
                    'grants',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM access.grants record),
                    'assignments',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM access.assignments record),
                    'completions',(SELECT jsonb_agg(to_jsonb(record) ORDER BY family,operation_id) FROM access.completions record),
                    'holders',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM access.holders record),
                    'numbers',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM workforce.numbers record))::text", &[],
            ).await.expect("committed admission snapshot").get(0);
            if let Some(committed) = &self.committed {
                assert!(
                    snapshot == *committed,
                    "original replay and member refresh change no admission or number facts"
                );
            } else {
                self.committed = Some(snapshot);
            }
        }
        let identity: String = client.query_one(
            "SELECT jsonb_build_object(
                'accounts',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM identity.accounts record),
                'persons',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM identity.persons record),
                'revisions',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM identity.revisions record),
                'sessions',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM identity.sessions record),
                'revocations',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM identity.revocations record),
                'holders',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM access.holders record),
                'roles',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM access.roles record),
                'other_grants',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM access.grants record WHERE person_id<>$1),
                'other_assignments',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM access.assignments record WHERE person_id<>$1),
                'other_completions',(SELECT jsonb_agg(to_jsonb(record) ORDER BY family,operation_id) FROM access.completions record WHERE operation_id<>$2),
                'numbers',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM workforce.numbers record))::text",
            &[&target.person_id, &request.operation],
        ).await.expect("identity snapshot around admission and member reads").get(0);
        if let Some(original) = &self.identity {
            assert!(
                identity == *original,
                "admission, replay and member reads preserve accounts and Sessions"
            );
        } else {
            self.identity = Some(identity);
        }
        if self.step == 0 {
            self.original = Some(request);
        } else if self.step == 1 {
            self.receipt = request.receipt;
        }
        std::fs::write(path.with_extension("ack"), "ok")
            .expect("admission observation acknowledgement");
        self.step += 1;
    }
}
