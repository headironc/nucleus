//! Independent facts around the member's first allocation, original replay and current read.

use std::path::Path;

use serde::Deserialize;
use tokio_postgres::Client;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    operation: Uuid,
    input: Input,
    account_id: Uuid,
    person_id: Uuid,
    session: Uuid,
    number: Option<Number>,
}

#[derive(PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    subtype: i16,
}

#[derive(PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Number {
    id: Uuid,
    subtype: i16,
    year: i32,
    serial: i16,
}

#[derive(Default)]
pub(super) struct Observer {
    step: usize,
    original: Option<Request>,
    number: Option<Number>,
    pool: Option<(i32, i16)>,
    unchanged: Option<String>,
    committed: Option<String>,
}

impl Observer {
    pub(super) fn complete(&self) -> bool {
        self.step == 6
    }

    pub(super) fn evidence(&self) -> serde_json::Value {
        assert!(self.complete());
        let original = self.original.as_ref().expect("original allocation");
        let number = self.number.as_ref().expect("committed number");
        serde_json::json!({
            "operation": original.operation,
            "accountId": original.account_id,
            "personId": original.person_id,
            "number": {"id": number.id, "subtype": number.subtype, "year": number.year, "serial": number.serial},
            "phases": self.step,
            "before": {"numbers": 0, "revisions": 0, "allocations": 0, "completions": 0},
            "after": {"numbers": 1, "revisions": 1, "allocations": 1, "completions": 1},
            "minimumFreeSerial": true,
            "replayAndReadUnchanged": true,
            "identityPermissionsAndOtherNumbersUnchanged": true,
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
        let path = material.join(format!("allocation-{phase}"));
        if !path.with_extension("request").exists() {
            return;
        }
        let request: Request = serde_json::from_slice(
            &std::fs::read(path.with_extension("request")).expect("allocation observation request"),
        )
        .expect("allocation observation binding");
        assert_eq!(request.input.subtype, 902);
        let admission: serde_json::Value = serde_json::from_slice(
            &std::fs::read(material.join("admission-current-after.request"))
                .expect("completed admission observation"),
        )
        .expect("admission binding");
        assert_eq!(
            admission["input"]["target"]["accountId"],
            request.account_id.to_string()
        );
        assert_eq!(
            admission["input"]["target"]["personId"],
            request.person_id.to_string()
        );
        assert_eq!(admission["session"], request.session.to_string());
        let grant: Uuid = admission["receipt"]["grantId"]
            .as_str()
            .expect("observed admitted Grant")
            .parse()
            .expect("Grant UUID");
        if let Some(original) = &self.original {
            assert!(
                request.operation == original.operation
                    && request.input == original.input
                    && request.account_id == original.account_id
                    && request.person_id == original.person_id
                    && request.session == original.session,
                "all phases retain the original operation, input and member Session"
            );
        }
        let revision: Uuid = client
            .query_one(
                "SELECT person.revision_id FROM identity.sessions session
             JOIN identity.accounts account ON account.id=session.account_id
             JOIN identity.persons person ON person.id=account.person_id
             JOIN access.grants grant_record ON grant_record.person_id=person.id
             JOIN access.roles role ON role.id=grant_record.role_id
             WHERE session.id=$1 AND account.id=$2 AND person.id=$3 AND grant_record.id=$4
             AND session.revoked_at IS NULL AND session.expires_at>clock_timestamp()
             AND account.suspension_id IS NULL AND account.cancellation_id IS NULL
             AND grant_record.suspension_id IS NULL AND grant_record.termination IS NULL
             AND role.scope='workforce' AND role.level=1 AND cardinality(role.permissions)=0",
                &[
                    &request.session,
                    &request.account_id,
                    &request.person_id,
                    &grant,
                ],
            )
            .await
            .expect("one active ordinary member and own live Session")
            .get(0);
        let counts = client.query_one(
            "SELECT (SELECT count(*) FROM workforce.numbers WHERE id=$1),
             (SELECT count(*) FROM workforce.revisions WHERE person_id=$1),
             (SELECT count(*) FROM workforce.allocations WHERE person_id=$1),
             (SELECT count(*) FROM workforce.completions WHERE family='number_allocation' AND operation_id=$2)",
            &[&request.person_id, &request.operation],
        ).await.expect("independent first-allocation counts");
        let expected: i64 = if self.step == 0 { 0 } else { 1 };
        for index in 0..4 {
            assert_eq!(counts.get::<_, i64>(index), expected);
        }
        if self.step == 0 {
            assert!(request.number.is_none());
            let pool = client.query_one(
                "WITH current_year AS (SELECT extract(year FROM clock_timestamp() AT TIME ZONE 'Asia/Shanghai')::integer AS year)
                 SELECT year, (SELECT min(candidate)::smallint FROM generate_series(1,999) candidate
                    WHERE NOT EXISTS(SELECT 1 FROM workforce.numbers WHERE held
                        AND (number).subtype/100=$1::smallint/100 AND (number).year=year AND (number).serial=candidate)) AS serial
                 FROM current_year",
                &[&request.input.subtype],
            ).await.expect("independent current-year minimum free serial");
            self.pool = Some((pool.get("year"), pool.get("serial")));
        } else {
            let number = request
                .number
                .as_ref()
                .expect("actual successful response number");
            if let Some(original) = &self.number {
                assert!(
                    number == original,
                    "replay and current read retain the complete original number"
                );
            }
            assert_eq!(number.subtype, request.input.subtype);
            assert_eq!(
                (number.year, number.serial),
                self.pool.expect("pool before allocation")
            );
            let paired: bool = client.query_one(
                "SELECT EXISTS(SELECT 1 FROM workforce.numbers current
                 JOIN workforce.revisions revision ON revision.id=current.revision_id AND revision.person_id=current.id AND revision.number=current.number
                 JOIN workforce.allocations allocation ON allocation.id=revision.allocation_id AND allocation.person_id=current.id AND allocation.number_id=revision.id
                 JOIN workforce.completions completion ON completion.family='number_allocation' AND completion.operation_id=allocation.operation_id
                 WHERE current.id=$1 AND revision.id=$2 AND (current.number).subtype=$3 AND (current.number).year=$4 AND (current.number).serial=$5
                 AND current.held AND current.sequence=1 AND current.version=1 AND current.tenure_id=allocation.tenure_id AND current.changed_at=allocation.allocated_at
                 AND allocation.revision_id=$6 AND allocation.operator_id=$6 AND allocation.source='self_service' AND allocation.reason IS NULL
                 AND allocation.sequence=1 AND allocation.held AND allocation.operation_id=$7
                 AND revision.correction_id IS NULL AND revision.recovery_id IS NULL
                 AND extract(year FROM allocation.allocated_at AT TIME ZONE 'Asia/Shanghai')::integer=$4
                 AND completion.action='self_service' AND completion.target=$8 AND completion.subject='session'
                 AND completion.person_id=$1 AND completion.revision_id=$6 AND completion.expires_at IS NULL)",
                &[&request.person_id, &number.id, &number.subtype, &number.year, &number.serial,
                    &revision, &request.operation, &request.person_id.to_string()],
            ).await.expect("paired number, first revision, allocation and permanent completion").get(0);
            assert!(
                paired,
                "the browser receipt matches one held self-service allocation"
            );
            let snapshot: String = client.query_one(
                "SELECT jsonb_build_object(
                    'numbers',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM workforce.numbers record),
                    'revisions',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM workforce.revisions record),
                    'allocations',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM workforce.allocations record),
                    'completions',(SELECT jsonb_agg(to_jsonb(record) ORDER BY family,operation_id) FROM workforce.completions record))::text", &[],
            ).await.expect("committed number snapshot").get(0);
            if let Some(committed) = &self.committed {
                assert!(
                    snapshot == *committed,
                    "replay and current read add or alter no number facts"
                );
            } else {
                self.committed = Some(snapshot);
            }
        }
        let unchanged: String = client.query_one(
            "SELECT jsonb_build_object(
                'accounts',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM identity.accounts record),
                'persons',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM identity.persons record),
                'identities',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM identity.revisions record),
                'sessions',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM identity.sessions record),
                'revocations',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM identity.revocations record),
                'grants',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM access.grants record),
                'assignments',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM access.assignments record),
                'holders',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM access.holders record),
                'roles',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM access.roles record),
                'access_completions',(SELECT jsonb_agg(to_jsonb(record) ORDER BY family,operation_id) FROM access.completions record),
                'other_numbers',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM workforce.numbers record WHERE id<>$1),
                'other_revisions',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM workforce.revisions record WHERE person_id<>$1),
                'other_allocations',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM workforce.allocations record WHERE person_id<>$1),
                'other_completions',(SELECT jsonb_agg(to_jsonb(record) ORDER BY family,operation_id) FROM workforce.completions record WHERE operation_id<>$2),
                'corrections',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM workforce.corrections record),
                'recoveries',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM workforce.recoveries record),
                'number_revocations',(SELECT jsonb_agg(to_jsonb(record) ORDER BY id) FROM workforce.revocations record),
                'other_transitions',(SELECT jsonb_agg(to_jsonb(record) ORDER BY person_id,sequence) FROM workforce.transitions record WHERE person_id<>$1))::text",
            &[&request.person_id, &request.operation],
        ).await.expect("identity, permission and unrelated number snapshot").get(0);
        if let Some(original) = &self.unchanged {
            assert!(
                unchanged == *original,
                "first allocation and recovery preserve identity, permissions and unrelated numbers"
            );
        } else {
            self.unchanged = Some(unchanged);
        }
        if self.step == 0 {
            self.original = Some(request);
        } else if self.step == 1 {
            self.number = request.number;
        }
        std::fs::write(path.with_extension("ack"), "ok")
            .expect("allocation observation acknowledgement");
        self.step += 1;
    }
}
