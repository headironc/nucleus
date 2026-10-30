//! The real identity application behind the built website and an isolated Nginx TLS entry.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::Ordering,
    time::Duration,
};

use nucleus::process::{self, Application};
use tokio::{runtime::Runtime, time::timeout};

use crate::{application::*, database::*, supplier::*};

mod admission;
mod allocation;
mod cancellation;
mod mobile;
mod termination;

/// A browser run's evidence binding. Standalone runs leave it absent; partial
/// configuration and any write failure are errors, but failures are accumulated
/// so the existing child, backend and SMS cleanup still runs to completion.
#[derive(Debug, Default)]
struct BrowserEvidence {
    binding: Option<BrowserEvidenceBinding>,
    failures: Vec<String>,
}

#[derive(Debug)]
struct BrowserEvidenceBinding {
    directory: std::path::PathBuf,
    run: String,
}

impl BrowserEvidence {
    fn from_environment() -> Result<Self, String> {
        Self::new(
            std::env::var_os("HALO_TEST_EVIDENCE").map(std::path::PathBuf::from),
            std::env::var_os("HALO_TEST_RUN_ID"),
            std::env::var_os("HALO_TEST_EVIDENCE_DIR").map(std::path::PathBuf::from),
        )
    }

    fn new(
        root: Option<std::path::PathBuf>,
        run: Option<std::ffi::OsString>,
        directory: Option<std::path::PathBuf>,
    ) -> Result<Self, String> {
        let (root, run, directory) = match (root, run, directory) {
            (None, None, None) => return Ok(Self::default()),
            (Some(root), Some(run), Some(directory)) => (root, run, directory),
            _ => return Err(
                "browser evidence requires HALO_TEST_EVIDENCE, HALO_TEST_RUN_ID and HALO_TEST_EVIDENCE_DIR together"
                    .to_owned(),
            ),
        };
        let run = run
            .into_string()
            .map_err(|_| "HALO_TEST_RUN_ID is not valid Unicode".to_owned())?;
        if !safe_run_id(&run) {
            return Err("HALO_TEST_RUN_ID is not a safe run identifier".to_owned());
        }
        if directory != root.join("runs").join(&run) {
            return Err("browser evidence directory is not the current run directory".to_owned());
        }
        if !directory.is_dir() {
            return Err("browser evidence run directory is missing".to_owned());
        }
        let current = std::fs::read_to_string(root.join("current.json"))
            .map_err(|error| format!("browser evidence current binding is unreadable: {error}"))?;
        let current: serde_json::Value = serde_json::from_str(&current)
            .map_err(|error| format!("browser evidence current binding is malformed: {error}"))?;
        if current.get("runId").and_then(serde_json::Value::as_str) != Some(run.as_str()) {
            return Err("browser evidence is bound to a different current run".to_owned());
        }
        Ok(Self {
            binding: Some(BrowserEvidenceBinding { directory, run }),
            failures: Vec::new(),
        })
    }

    fn record(&mut self, event: &str, data: serde_json::Value) {
        let Some(binding) = &self.binding else {
            return;
        };
        if let Err(error) = append_evidence(binding, event, data)
            && self.failures.len() < 8
        {
            self.failures.push(format!("{event}: {error}"));
        }
    }

    fn finish(&self) -> Result<(), String> {
        if self.failures.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "browser evidence writes failed: {}",
                self.failures.join("; ")
            ))
        }
    }
}

fn safe_run_id(run: &str) -> bool {
    !run.is_empty()
        && run.len() <= 160
        && run != "."
        && run != ".."
        && run
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

impl Drop for BrowserEvidence {
    fn drop(&mut self) {
        // Preserve secondary write failures when the original browser assertion
        // unwinds before finish(), without replacing that assertion or panicking
        // again if the diagnostic consumer has closed stderr.
        if std::thread::panicking()
            && let Err(error) = self.finish()
        {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr().lock(), "{error}");
        }
    }
}

fn append_evidence(
    binding: &BrowserEvidenceBinding,
    event: &str,
    data: serde_json::Value,
) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut record = match data {
        serde_json::Value::Object(record) => record,
        _ => serde_json::Map::new(),
    };
    record.insert("version".into(), 1.into());
    record.insert("runId".into(), binding.run.clone().into());
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| format!("{}.{:03}", elapsed.as_secs(), elapsed.subsec_millis()))
        .unwrap_or_else(|_| "unknown".to_string());
    record.insert("at".into(), at.into());
    record.insert("event".into(), event.into());
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(binding.directory.join("backend.jsonl"))?;
    writeln!(file, "{}", serde_json::Value::Object(record))?;
    file.flush()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn browser_evidence_accumulates_write_failures_and_keeps_trying() {
        let root = tempfile::tempdir().expect("evidence root");
        let directory = root.path().join("run");
        std::fs::create_dir(&directory).expect("run directory");
        let binding = BrowserEvidenceBinding {
            directory: directory.clone(),
            run: "run".to_owned(),
        };
        let mut evidence = BrowserEvidence {
            binding: Some(binding),
            failures: Vec::new(),
        };
        evidence.record("first", json!({ "outcome": "completed" }));
        evidence.finish().expect("writable evidence");

        let log = directory.join("backend.jsonl");
        std::fs::remove_file(&log).expect("remove log");
        std::fs::create_dir(&log).expect("block log");
        evidence.record("second", json!({}));
        evidence.record("third", json!({}));
        let error = evidence.finish().expect_err("write failures are errors");
        assert!(
            error.contains("second"),
            "diagnostic keeps the operation: {error}"
        );
        assert!(
            error.contains("third"),
            "later cleanup writes are still attempted: {error}"
        );
    }

    #[test]
    fn browser_evidence_configuration_is_all_or_nothing() {
        let root = tempfile::tempdir().expect("evidence root");
        assert!(
            BrowserEvidence::new(None, None, None)
                .expect("standalone")
                .binding
                .is_none()
        );
        let run = std::ffi::OsString::from("run");
        assert!(BrowserEvidence::new(Some(root.path().to_path_buf()), None, None).is_err());
        assert!(
            BrowserEvidence::new(Some(root.path().to_path_buf()), Some(run.clone()), None).is_err()
        );
        assert!(
            BrowserEvidence::new(
                Some(root.path().to_path_buf()),
                Some(run),
                Some(root.path().join("run")),
            )
            .is_err()
        );
        assert!(!safe_run_id("."));
        assert!(!safe_run_id(".."));
        assert!(!safe_run_id("a/b"));
        assert!(safe_run_id("run-1"));
    }
}

#[test]
#[ignore = "requires the built halo release, Node 26, Playwright Chromium and HALO_NGINX"]
fn browser_c1_preserves_original_send() {
    let mut logs = BrowserEvidence::from_environment().expect("browser evidence configuration");
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        // Keep 120 seconds beyond the browser's budget, including real SMS intervals.
        timeout(Duration::from_secs(1020), async {
            // Hold both reservations together: an ephemeral port must not be
            // chosen twice, and the media origin must not become the site origin.
            let site_reservation = std::net::TcpListener::bind("127.0.0.1:0")
                .expect("reserve fixture port");
            let media_reservation = std::net::TcpListener::bind("127.0.0.1:0")
                .expect("reserve fixture media port");
            let port = site_reservation.local_addr().expect("port").port();
            let media_port = media_reservation.local_addr().expect("media port").port();
            assert_ne!(port, media_port, "site and media fixtures need distinct ports");
            drop((site_reservation, media_reservation));
            let media_origin = format!("https://127.0.0.1:{media_port}");
            let supplier = Supplier::echoing().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let name = database(&server).await;
            let origin = format!("https://127.0.0.1:{port}");
            assert_ne!(media_origin, origin, "the configured media origin differs from the site origin");
            let application = Application::start(
                connections(&server, &name, "clerk"), keys(0),
                services_with_avatar(8, avatar_settings_at(&media_origin, Duration::from_secs(60))).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"),
                supplier.client(), message_at(sms.address(), sms.authority()),
            ).await.expect("isolated application");
            let (base, stop, mut task) = running_at(application, origin).await;
            let material = tempfile::tempdir().expect("private fictional SMS material");
            let history_observer = connections(&server, &name, "clerk").recover().await.expect("history read observer");
            let mut history_tick = tokio::time::interval(Duration::from_millis(100));
            let mut history_before: Option<String> = None;
            let mut history_checked = false;
            let identity_observer = connections(&server, &name, "clerk").recover().await.expect("identity read observer");
            let mut identity_tick = tokio::time::interval(Duration::from_millis(100));
            let mut identity_before = BTreeMap::<&str, (uuid::Uuid, String)>::new();
            let mut identity_checked = BTreeSet::<&str>::new();
            let affiliation_observer = connections(&server, &name, "clerk").recover().await.expect("affiliation read observer");
            let mut affiliation_tick = tokio::time::interval(Duration::from_millis(100));
            let mut affiliation_original_before: Option<(String, String)> = None;
            let mut affiliation_dropped: Option<(String, String)> = None;
            let mut affiliation_read_before: Option<String> = None;
            let mut affiliation_conflict_before: Option<String> = None;
            let mut affiliation_checked = BTreeSet::<&str>::new();
            let workforce_observer = connections(&server, &name, "clerk").recover().await.expect("Workforce read observer");
            let mut workforce_tick = tokio::time::interval(Duration::from_millis(100));
            let mut workforce_before = BTreeMap::<&str, String>::new();
            let mut workforce_checked = BTreeSet::<&str>::new();
            let admission_observer = connections(&server, &name, "clerk").recover().await.expect("admission observer");
            let mut admission_tick = tokio::time::interval(Duration::from_millis(100));
            let mut admission = admission::Observer::default();
            let allocation_observer = connections(&server, &name, "clerk").recover().await.expect("first allocation observer");
            let mut allocation_tick = tokio::time::interval(Duration::from_millis(100));
            let mut allocation = allocation::Observer::default();
            let termination_observer = connections(&server, &name, "clerk").recover().await.expect("termination read observer");
            let mut termination_tick = tokio::time::interval(Duration::from_millis(100));
            let mut termination = termination::Observer::default();
            let mobile_observer = connections(&server, &name, "clerk").recover().await.expect("mobile change observer");
            let mut mobile_tick = tokio::time::interval(Duration::from_millis(100));
            let mut mobile_change = mobile::Observer::default();
            let cancellation_observer = connections(&server, &name, "clerk").recover().await.expect("cancellation read observer");
            let mut cancellation_tick = tokio::time::interval(Duration::from_millis(100));
            let mut cancellation = cancellation::Observer::default();
            let number_observer = connections(&server, &name, "clerk").recover().await.expect("number read observer");
            let mut number_tick = tokio::time::interval(Duration::from_millis(100));
            let mut number_before = BTreeMap::<&str, String>::new();
            let mut number_checked = BTreeSet::<&str>::new();
            let mut child = tokio::process::Command::new("node")
                .arg("tools/browser.ts")
                .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/halo"))
                .env("HALO_TEST_BACKEND", base)
                .env("HALO_TEST_PORT", port.to_string())
                .env("HALO_TEST_MEDIA_PORT", media_port.to_string())
                .env("HALO_AVATAR_ORIGIN", &media_origin)
                .env("HALO_TEST_MATERIAL", material.path())
                .kill_on_drop(true)
                .spawn().expect("browser acceptance runner");
            let mut counts = BTreeMap::<String, usize>::new();
            let mut status = None;
            let mut stop = Some(stop);
            let (application, result) = loop {
                tokio::select! {
                    // Observe queued calls before accepting the backend's completed drain.
                    biased;
                    _ = admission_tick.tick(), if !admission.complete() => {
                        admission.observe(admission_observer.client(), material.path()).await;
                    }
                    _ = allocation_tick.tick(), if admission.complete() && !allocation.complete() => {
                        allocation.observe(allocation_observer.client(), material.path()).await;
                    }
                    _ = mobile_tick.tick(), if !mobile_change.complete() => {
                        mobile_change.observe(mobile_observer.client(), material.path()).await;
                    }
                    _ = cancellation_tick.tick(), if !cancellation.complete() => {
                        cancellation.observe(cancellation_observer.client(), material.path()).await;
                    }
                    _ = termination_tick.tick(), if !termination.complete() => {
                        termination.observe(termination_observer.client(), material.path()).await;
                    }
                    _ = number_tick.tick(), if number_checked.len() < 5 => {
                        for phase in ["empty", "allocated", "rejoined", "corrected", "revoked"] {
                            if number_checked.contains(phase) { continue; }
                            let position = if number_before.contains_key(phase) { "after" } else { "before" };
                            let path = material.path().join(format!("numbers-{phase}-{position}"));
                            if !path.with_extension("request").exists() { continue; }
                            let snapshot: String = number_observer.client().query_one(
                                r#"SELECT jsonb_build_object(
                                    'numbers',(SELECT jsonb_agg(to_jsonb(number) ORDER BY id) FROM workforce.numbers number),
                                    'revisions',(SELECT jsonb_agg(to_jsonb(revision) ORDER BY id) FROM workforce.revisions revision),
                                    'allocations',(SELECT jsonb_agg(to_jsonb(allocation) ORDER BY id) FROM workforce.allocations allocation),
                                    'corrections',(SELECT jsonb_agg(to_jsonb(correction) ORDER BY id) FROM workforce.corrections correction),
                                    'recoveries',(SELECT jsonb_agg(to_jsonb(recovery) ORDER BY id) FROM workforce.recoveries recovery),
                                    'revocations',(SELECT jsonb_agg(to_jsonb(revocation) ORDER BY id) FROM workforce.revocations revocation),
                                    'number_completions',(SELECT jsonb_agg(to_jsonb(completion) ORDER BY family,operation_id) FROM workforce.completions completion),
                                    'grants',(SELECT jsonb_agg(to_jsonb(grant_record) ORDER BY id) FROM access.grants grant_record),
                                    'assignments',(SELECT jsonb_agg(to_jsonb(assignment) ORDER BY id) FROM access.assignments assignment),
                                    'terminations',(SELECT jsonb_agg(to_jsonb(termination) ORDER BY id) FROM access.terminations termination),
                                    'access_completions',(SELECT jsonb_agg(to_jsonb(completion) ORDER BY family,operation_id) FROM access.completions completion),
                                    'accounts',(SELECT jsonb_agg(to_jsonb(account) ORDER BY id) FROM identity.accounts account),
                                    'sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session),
                                    'persons',(SELECT jsonb_agg(to_jsonb(person) ORDER BY id) FROM identity.persons person),
                                    'identities',(SELECT jsonb_agg(to_jsonb(revision) ORDER BY id) FROM identity.revisions revision)
                                )::text"#, &[],
                            ).await.expect("independent number read snapshot").get(0);
                            if let Some(before) = number_before.get(phase) {
                                assert!(before == &snapshot, "number reads preserve number states, revisions, history, grants, accounts, sessions and expiry");
                                number_checked.insert(phase);
                            } else { number_before.insert(phase, snapshot); }
                            std::fs::write(path.with_extension("ack"), "ok").expect("number snapshot acknowledgement");
                        }
                    }
                    _ = workforce_tick.tick(), if workforce_checked.len() < 6 => {
                        for phase in ["absent", "active", "suspended", "resumed", "terminated", "rejoined"] {
                            if workforce_checked.contains(phase) { continue; }
                            let position = if workforce_before.contains_key(phase) { "after" } else { "before" };
                            let path = material.path().join(format!("workforce-{phase}-{position}"));
                            if !path.with_extension("request").exists() { continue; }
                            let snapshot: String = workforce_observer.client().query_one(
                                r#"SELECT jsonb_build_object(
                                    'grants',(SELECT jsonb_agg(to_jsonb(grant_record) ORDER BY id) FROM access.grants grant_record),
                                    'assignments',(SELECT jsonb_agg(to_jsonb(assignment) ORDER BY id) FROM access.assignments assignment),
                                    'suspensions',(SELECT jsonb_agg(to_jsonb(suspension) ORDER BY id) FROM access.suspensions suspension),
                                    'resumptions',(SELECT jsonb_agg(to_jsonb(resumption) ORDER BY id) FROM access.resumptions resumption),
                                    'terminations',(SELECT jsonb_agg(to_jsonb(termination) ORDER BY id) FROM access.terminations termination),
                                    'completions',(SELECT jsonb_agg(to_jsonb(completion) ORDER BY family,operation_id) FROM access.completions completion),
                                    'sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session),
                                    'accounts',(SELECT jsonb_agg(to_jsonb(account) ORDER BY id) FROM identity.accounts account),
                                    'revocations',(SELECT jsonb_agg(to_jsonb(revocation) ORDER BY id) FROM identity.revocations revocation)
                                )::text"#, &[],
                            ).await.expect("independent business snapshot around service reads").get(0);
                            if let Some(before) = workforce_before.get(phase) {
                                assert!(before == &snapshot, "service reads preserve Grant, Session, expiry and business facts");
                                workforce_checked.insert(phase);
                            } else { workforce_before.insert(phase, snapshot); }
                            std::fs::write(path.with_extension("ack"), "ok").expect("service snapshot acknowledgement");
                        }
                    }
                    _ = history_tick.tick(), if !history_checked => {
                        let phase = if history_before.is_some() { "after" } else { "before" };
                        let request = material.path().join(format!("history-{phase}.request"));
                        if request.exists() {
                            let person = std::fs::read_to_string(request).expect("fictional history owner");
                            let person = uuid::Uuid::parse_str(&person).expect("history Person ID");
                            let snapshot: String = history_observer.client().query_one(
                                r#"SELECT jsonb_build_object(
                                    'person',(SELECT to_jsonb(person) FROM identity.persons person WHERE id=$1),
                                    'account',(SELECT to_jsonb(account) FROM identity.accounts account WHERE person_id=$1),
                                    'corrections',(SELECT jsonb_agg(to_jsonb(correction) ORDER BY sequence) FROM identity.corrections correction WHERE person_id=$1),
                                    'revisions',(SELECT jsonb_agg(to_jsonb(revision) ORDER BY id) FROM identity.revisions revision WHERE person_id=$1),
                                    'sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session WHERE account_id=(SELECT id FROM identity.accounts WHERE person_id=$1)),
                                    'grants',(SELECT jsonb_agg(to_jsonb(grant_record) ORDER BY id) FROM access.grants grant_record WHERE person_id=$1),
                                    'completions',(SELECT jsonb_agg(to_jsonb(completion) ORDER BY family,operation_id) FROM identity.completions completion),
                                    'logouts',(SELECT jsonb_agg(to_jsonb(logout) ORDER BY id) FROM identity.logouts logout)
                                )::text"#, &[&person],
                            ).await.expect("one statement observes business facts around browser reads").get(0);
                            if let Some(before) = &history_before {
                                assert!(before == &snapshot, "history page reads must preserve business facts");
                                history_checked = true;
                            } else {
                                history_before = Some(snapshot);
                            }
                            std::fs::write(material.path().join(format!("history-{phase}.ack")), "ok").expect("history snapshot acknowledgement");
                        }
                    }
                    _ = identity_tick.tick(), if identity_checked.len() < 3 => {
                        for phase in ["original", "corrected", "ordinary"] {
                            if identity_checked.contains(phase) { continue; }
                            let position = if identity_before.contains_key(phase) { "after" } else { "before" };
                            let path = material.path().join(format!("identity-{phase}-{position}"));
                            let request = path.with_extension("request");
                            if !request.exists() { continue; }
                            let person = std::fs::read_to_string(request).expect("fictional identity owner");
                            let person = uuid::Uuid::parse_str(&person).expect("identity Person ID");
                            let qualification = identity_observer.client().query_one(
                                r#"SELECT (SELECT count(*) FROM access.grants WHERE person_id=$1) AS grants,
                                    EXISTS(SELECT 1 FROM access.holders holder JOIN access.roles role ON role.id=holder.role_id
                                        WHERE holder.person_id=$1 AND 'institution.corrections'=ANY(role.permissions)) AS manager"#,
                                &[&person],
                            ).await.expect("independently observe ordinary owner eligibility");
                            assert_eq!(qualification.get::<_, i64>("grants"), 0, "current identity needs no Workforce membership");
                            assert!(!qualification.get::<_, bool>("manager"), "current identity needs no correction permission");
                            let snapshot: String = identity_observer.client().query_one(
                                r#"SELECT jsonb_build_object(
                                    'persons',(SELECT jsonb_agg(to_jsonb(person) ORDER BY id) FROM identity.persons person),
                                    'accounts',(SELECT jsonb_agg(to_jsonb(account) ORDER BY id) FROM identity.accounts account),
                                    'revisions',(SELECT jsonb_agg(to_jsonb(revision) ORDER BY id) FROM identity.revisions revision),
                                    'corrections',(SELECT jsonb_agg(to_jsonb(correction) ORDER BY id) FROM identity.corrections correction),
                                    'sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session),
                                    'handoffs',(SELECT jsonb_agg(to_jsonb(handoff) ORDER BY id) FROM identity.handoffs handoff),
                                    'authentications',(SELECT jsonb_agg(to_jsonb(authentication) ORDER BY id) FROM identity.authentications authentication),
                                    'receipts',(SELECT jsonb_agg(to_jsonb(receipt) ORDER BY id) FROM identity.receipts receipt),
                                    'attempts',(SELECT jsonb_agg(to_jsonb(attempt) ORDER BY id) FROM identity.attempts attempt),
                                    'creations',(SELECT jsonb_agg(to_jsonb(creation) ORDER BY id) FROM identity.creations creation),
                                    'revocations',(SELECT jsonb_agg(to_jsonb(revocation) ORDER BY id) FROM identity.revocations revocation),
                                    'logouts',(SELECT jsonb_agg(to_jsonb(logout) ORDER BY id) FROM identity.logouts logout),
                                    'completions',(SELECT jsonb_agg(to_jsonb(completion) ORDER BY family,operation_id) FROM identity.completions completion),
                                    'grants',(SELECT jsonb_agg(to_jsonb(grant_record) ORDER BY id) FROM access.grants grant_record),
                                    'avatars',(SELECT jsonb_agg(to_jsonb(avatar) ORDER BY id) FROM identity.avatars avatar),
                                    'holders',(SELECT jsonb_agg(to_jsonb(holder) ORDER BY id) FROM access.holders holder)
                                )::text"#, &[],
                            ).await.expect("independent statement snapshots all identity facts around current reads").get(0);
                            if let Some((original, before)) = identity_before.get(phase) {
                                assert!(original == &person && before == &snapshot, "current reads preserve the original person's complete business facts");
                                identity_checked.insert(phase);
                            } else { identity_before.insert(phase, (person, snapshot)); }
                            std::fs::write(path.with_extension("ack"), "ok").expect("current identity snapshot acknowledgement");
                        }
                    }
                    _ = affiliation_tick.tick(), if affiliation_checked.len() < 7 => {
                        for phase in ["original-before", "dropped-before", "dropped-after", "read-before", "read-after", "conflict-before", "conflict-after"] {
                            if affiliation_checked.contains(phase) { continue; }
                            let path = material.path().join(format!("affiliation-{phase}"));
                            let request = path.with_extension("request");
                            if !request.exists() { continue; }
                            let marker: serde_json::Value = serde_json::from_str(
                                &std::fs::read_to_string(&request).expect("fictional affiliation owner"),
                            ).expect("affiliation observation JSON");
                            let person = uuid::Uuid::parse_str(
                                marker["personId"].as_str().expect("affiliation Person ID"),
                            ).expect("affiliation Person ID");
                            let center: String = affiliation_observer.client().query_one(
                                r#"SELECT jsonb_build_object(
                                    'completions',(SELECT jsonb_agg(to_jsonb(completion) ORDER BY operation_id) FROM center.completions completion),
                                    'affiliations',(SELECT jsonb_agg(to_jsonb(affiliation) ORDER BY sequence) FROM center.affiliations affiliation),
                                    'profiles',(SELECT jsonb_agg(to_jsonb(profile) ORDER BY id) FROM center.profiles profile)
                                )::text"#, &[],
                            ).await.expect("independent center snapshot").get(0);
                            let non_center: String = affiliation_observer.client().query_one(
                                r#"SELECT jsonb_build_object(
                                    'persons',(SELECT jsonb_agg(to_jsonb(person) ORDER BY id) FROM identity.persons person),
                                    'accounts',(SELECT jsonb_agg(to_jsonb(account) ORDER BY id) FROM identity.accounts account),
                                    'revisions',(SELECT jsonb_agg(to_jsonb(revision) ORDER BY id) FROM identity.revisions revision),
                                    'corrections',(SELECT jsonb_agg(to_jsonb(correction) ORDER BY id) FROM identity.corrections correction),
                                    'sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session),
                                    'handoffs',(SELECT jsonb_agg(to_jsonb(handoff) ORDER BY id) FROM identity.handoffs handoff),
                                    'authentications',(SELECT jsonb_agg(to_jsonb(authentication) ORDER BY id) FROM identity.authentications authentication),
                                    'receipts',(SELECT jsonb_agg(to_jsonb(receipt) ORDER BY id) FROM identity.receipts receipt),
                                    'attempts',(SELECT jsonb_agg(to_jsonb(attempt) ORDER BY id) FROM identity.attempts attempt),
                                    'creations',(SELECT jsonb_agg(to_jsonb(creation) ORDER BY id) FROM identity.creations creation),
                                    'revocations',(SELECT jsonb_agg(to_jsonb(revocation) ORDER BY id) FROM identity.revocations revocation),
                                    'logouts',(SELECT jsonb_agg(to_jsonb(logout) ORDER BY id) FROM identity.logouts logout),
                                    'identity_completions',(SELECT jsonb_agg(to_jsonb(completion) ORDER BY family,operation_id) FROM identity.completions completion),
                                    'grants',(SELECT jsonb_agg(to_jsonb(grant_record) ORDER BY id) FROM access.grants grant_record),
                                    'assignments',(SELECT jsonb_agg(to_jsonb(assignment) ORDER BY id) FROM access.assignments assignment),
                                    'holders',(SELECT jsonb_agg(to_jsonb(holder) ORDER BY id) FROM access.holders holder),
                                    'terminations',(SELECT jsonb_agg(to_jsonb(termination) ORDER BY id) FROM access.terminations termination),
                                    'access_completions',(SELECT jsonb_agg(to_jsonb(completion) ORDER BY family,operation_id) FROM access.completions completion),
                                    'avatars',(SELECT jsonb_agg(to_jsonb(avatar) ORDER BY id) FROM identity.avatars avatar)
                                )::text"#, &[],
                            ).await.expect("independent non-center snapshot").get(0);
                            let center: serde_json::Value = serde_json::from_str(&center).expect("center JSON");
                            match phase {
                                "original-before" => {
                                    for name in ["completions", "affiliations", "profiles"] {
                                        assert!(center[name].is_null(), "first affiliation begins without {name}");
                                    }
                                    affiliation_original_before = Some((center.to_string(), non_center.clone()));
                                }
                                "dropped-before" => {
                                    let before = affiliation_original_before.as_ref().expect("original before-state observed");
                                    assert_eq!(&non_center, &before.1, "first fill changes only center affiliation facts");
                                    let operation = marker["operation"].as_str().expect("dropped original operation");
                                    let operation = uuid::Uuid::parse_str(operation).expect("original operation ID");
                                    let completions = center["completions"].as_array().expect("one center completion");
                                    let affiliations = center["affiliations"].as_array().expect("one affiliation");
                                    let profiles = center["profiles"].as_array().expect("one profile");
                                    assert_eq!(completions.len(), 1, "the dropped success has one original completion");
                                    assert_eq!(affiliations.len(), 1, "the dropped success has one original affiliation");
                                    assert_eq!(profiles.len(), 1, "the dropped success has one profile");
                                    let completion = &completions[0];
                                    let affiliation = &affiliations[0];
                                    let profile = &profiles[0];
                                    assert_eq!(completion["family"], "center_affiliation");
                                    assert_eq!(completion["action"], "fill");
                                    assert_eq!(completion["subject"], "session");
                                    assert_eq!(completion["operation_id"], operation.to_string());
                                    assert_eq!(completion["person_id"], person.to_string());
                                    assert_eq!(affiliation["person_id"], person.to_string());
                                    assert_eq!(affiliation["operation_id"], operation.to_string());
                                    assert_eq!(affiliation["sequence"], 1);
                                    assert_eq!(profile["id"], person.to_string());
                                    assert_eq!(profile["affiliation_id"], affiliation["id"]);
                                    assert_eq!(profile["sequence"], 1);
                                    assert_eq!(profile["created_at"], affiliation["created_at"]);
                                    assert_eq!(completion["created_at"], affiliation["created_at"]);
                                    affiliation_dropped = Some((center.to_string(), non_center.clone()));
                                }
                                "dropped-after" => {
                                    let before = affiliation_dropped.as_ref().expect("dropped state observed");
                                    assert_eq!(&non_center, &before.1, "original replay changes no unrelated fact");
                                    assert_eq!(&center.to_string(), &before.0, "original replay adds no second completion or fact");
                                }
                                "read-before" => {
                                    affiliation_read_before = Some(format!("{center}\n{non_center}"));
                                }
                                "read-after" => {
                                    let before = affiliation_read_before.as_ref().expect("read before-state observed");
                                    assert_eq!(&format!("{center}\n{non_center}"), before, "current affiliation reads change no business fact");
                                }
                                "conflict-before" => {
                                    affiliation_conflict_before = Some(format!("{center}\n{non_center}"));
                                }
                                "conflict-after" => {
                                    let before = affiliation_conflict_before.as_ref().expect("conflict before-state observed");
                                    assert_eq!(&format!("{center}\n{non_center}"), before, "a new operation key cannot create a second affiliation fact");
                                }
                                _ => unreachable!(),
                            }
                            affiliation_checked.insert(phase);
                            std::fs::write(path.with_extension("ack"), "ok").expect("affiliation snapshot acknowledgement");
                        }
                    }
                    request = sms.request() => {
                        let text = std::str::from_utf8(&request.bytes).expect("fictional wire");
                        let target = text.lines().next().expect("request").split_whitespace().nth(1).expect("target");
                        let url = url::Url::parse(&format!("https://dypnsapi.aliyuncs.com{target}")).expect("wire URL");
                        let mobile = url.query_pairs().find(|(name, _)| name == "PhoneNumber").expect("mobile").1.into_owned();
                        let message = url.query_pairs().find(|(name, _)| name == "OutId").expect("message").1.into_owned();
                        let template = url.query_pairs().find(|(name, _)| name == "TemplateParam").expect("template").1.into_owned();
                        let code = serde_json::from_str::<serde_json::Value>(&template).expect("template JSON")["code"].as_str().expect("code").to_owned();
                        let pending = material.path().join(format!("{message}.pending"));
                        std::fs::write(&pending, code).expect("fictional code for browser test process");
                        std::fs::rename(pending, material.path().join(format!("{message}.code"))).expect("complete fictional code");
                        *counts.entry(mobile.clone()).or_default() += 1;
                        let template_code = url.query_pairs().find(|(name, _)| name == "TemplateCode").expect("template code").1.into_owned();
                        let expected_template = match (mobile.as_str(), counts[&mobile]) {
                            ("13900000040", 3) => "change",
                            ("13900000041", 1) => "bind",
                            _ => "auth",
                        };
                        assert_eq!(template_code, expected_template, "each actual PNVS call uses its own purpose");
                        match mobile.as_str() {
                            "13900000003" => {
                                let body = r#"{"Code":"isv.BUSINESS_LIMIT_CONTROL","Success":false}"#;
                                request.reply.send(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).into_bytes()).expect("supplier refusal");
                            }
                            "13900000004" => drop(request.reply),
                            _ => accept_sms(request),
                        }
                    }
                    result = child.wait(), if status.is_none() => {
                        let observed = result.expect("browser runner completion");
                        #[cfg(unix)]
                        let signal = {
                            use std::os::unix::process::ExitStatusExt;
                            observed.signal()
                        };
                        #[cfg(not(unix))]
                        let signal: Option<i32> = None;
                        logs.record("browser_child_observed", serde_json::json!({
                            "success": observed.success(),
                            "code": observed.code(),
                            "signal": signal,
                        }));
                        status = Some(observed);
                        stop.take().expect("one stop").send(()).expect("stop backend");
                    }
                    result = &mut task, if status.is_some() => break result.expect("backend task"),
                }
            };
            match &result {
                Ok(()) => logs.record(
                    "backend_drain",
                    serde_json::json!({ "outcome": "completed" }),
                ),
                Err(_) => logs.record(
                    "backend_drain",
                    serde_json::json!({ "outcome": "failed" }),
                ),
            }
            result.expect("drained");
            drop(application);
            sms.stop().await;
            let browser_status = status.expect("browser completed");
            logs.record("browser_exit", serde_json::json!({
                "success": browser_status.success(),
                "code": browser_status.code(),
            }));
            assert!(browser_status.success(), "browser acceptance failed");
            let media = std::fs::read_to_string(material.path().join("media.jsonl"))
                .expect("real signed-image reads through the fictional media entry");
            let mut images = 0_usize;
            let mut expired = 0_usize;
            for line in media.lines() {
                let entry: serde_json::Value = serde_json::from_str(line).expect("one media observation");
                let path = entry["path"].as_str().expect("media path");
                let Some(digest) = path
                    .strip_prefix("/api/v1/avatars/")
                    .and_then(|path| path.strip_suffix(".webp"))
                else { continue };
                assert_eq!(digest.len(), 64, "the media request names a content digest");
                assert_eq!(entry["method"], "GET");
                assert_eq!(entry["setCookie"], false, "a signed image read never renews a Session");
                assert_eq!(entry["authorization"], false, "the image carries no Session credential");
                assert_eq!(entry["sessionId"], false);
                assert_eq!(entry["operationId"], false, "native media has no JSON operation binding");
                assert_eq!(entry["nosniff"], "nosniff", "the backend supplied nosniff; the fixture only forwards it");
                match entry["status"].as_u64().expect("media status") {
                    200 => {
                        images += 1;
                        assert_eq!(entry["contentType"], "image/webp");
                        assert_eq!(entry["sha256"], digest, "returned bytes match the backend's stored digest");
                        assert!(entry["bytes"].as_u64().expect("media bytes") > 0);
                        let cache = entry["cacheControl"].as_str().expect("private cache policy");
                        let seconds: u64 = cache
                            .strip_prefix("private, max-age=")
                            .and_then(|cache| cache.strip_suffix(", immutable"))
                            .expect("private immutable media cache")
                            .parse()
                            .expect("bounded media cache seconds");
                        // A response near the signed expiry may legitimately have max-age=0.
                        assert!(seconds <= 60, "cache never exceeds the signed lifetime");
                    }
                    403 => {
                        expired += 1;
                        assert_eq!(entry["code"], "avatar_link_invalid", "the expired link is the backend's own refusal");
                        assert_eq!(entry["contentType"], "application/json");
                        assert_eq!(entry["cacheControl"], "no-store", "expired link errors are not cached");
                    }
                    status => panic!("unexpected signed-image status {status}"),
                }
            }
            assert!(images >= 2, "both current-read phases load real image bytes");
            assert_eq!(expired, 1, "the transport-delay fault reaches exactly one real signed-link expiry");
            let avatar_expiry: serde_json::Value = serde_json::from_slice(
                &std::fs::read(material.path().join("avatar-expiry.json")).expect("browser avatar expiry evidence"),
            ).expect("non-secret avatar expiry evidence");
            assert_eq!(avatar_expiry["status"], 403);
            assert_eq!(avatar_expiry["code"], "avatar_link_invalid");
            assert_eq!(avatar_expiry["cacheControl"], "no-store");
            assert_eq!(avatar_expiry["contentType"], "application/json");
            assert_eq!(avatar_expiry["nosniff"], "nosniff");
            assert!(avatar_expiry["currentDelayMs"].as_u64().expect("delayed Current read") < 35_000);
            assert!(avatar_expiry["imageDelayMs"].as_u64().expect("held media read") < 35_000);
            assert_eq!(avatar_expiry["releasedAfterExpiry"], true, "the held image continues only after the backend-issued expiry");
            assert_eq!(avatar_expiry["freshStatus"], 200);
            let fresh_cache = avatar_expiry["freshCacheControl"].as_str().expect("fresh media cache");
            let fresh_seconds: u64 = fresh_cache
                .strip_prefix("private, max-age=")
                .and_then(|cache| cache.strip_suffix(", immutable"))
                .expect("fresh private immutable media cache")
                .parse()
                .expect("fresh bounded media cache seconds");
            assert!(fresh_seconds <= 60, "explicit recovery also respects the signed lifetime");
            assert!(history_checked, "browser history reads have independent before/after evidence");
            let history: serde_json::Value = serde_json::from_slice(
                &std::fs::read(material.path().join("history.json")).expect("browser history evidence"),
            ).expect("fictional history evidence");
            let person = uuid::Uuid::parse_str(history["personId"].as_str().expect("history Person ID")).expect("Person UUID");
            let corrections = history_observer.client().query(
                "SELECT id, corrected_at FROM identity.corrections WHERE person_id=$1 ORDER BY sequence DESC, id DESC",
                &[&person],
            ).await.expect("permanent correction order and time");
            assert_eq!(corrections.len(), 3);
            for (row, record) in corrections.iter().zip(history["records"].as_array().expect("browser records")) {
                assert_eq!(row.get::<_, uuid::Uuid>("id").to_string(), record["id"].as_str().expect("correction ID"));
                assert_eq!(row.get::<_, chrono::DateTime<chrono::Utc>>("corrected_at"), chrono::DateTime::parse_from_rfc3339(record["correctedAt"].as_str().expect("correction time")).expect("wire time"));
            }
            history_observer.terminate().await.expect("end history observer");
            assert_eq!(identity_checked.len(), 3, "original, corrected and ordinary reads have independent no-write evidence");
            identity_observer.terminate().await.expect("end identity observer");
            assert_eq!(workforce_checked.len(), 6, "all service lifecycle read windows have independent no-write evidence");
            workforce_observer.terminate().await.expect("end Workforce observer");
            assert!(admission.complete(), "admission, original replay and member read have independent durable evidence");
            logs.record("admission_acceptance", admission.evidence());
            admission_observer.terminate().await.expect("end admission observer");
            assert!(allocation.complete(), "first allocation, original replay and current number read have independent durable evidence");
            logs.record("allocation_acceptance", allocation.evidence());
            allocation_observer.terminate().await.expect("end allocation observer");
            assert!(termination.complete(), "original termination and new-Session replay have independent durable evidence");
            termination_observer.terminate().await.expect("end termination observer");
            assert!(mobile_change.complete(), "mobile completion and original-result reads have independent durable evidence");
            mobile_observer.terminate().await.expect("end mobile observer");
            assert!(cancellation.complete(), "normal and lost-response cancellations have independent durable evidence before later registration");
            cancellation_observer.terminate().await.expect("end cancellation observer");
            assert_eq!(number_checked.len(), 5, "all number read windows have independent no-write evidence");
            number_observer.terminate().await.expect("end number observer");
            for mobile in ["13900000000", "13900000001", "13900000002", "13900000003", "13900000004",
                "13900000005", "13900000006", "13900000007", "13900000008",
                "13900000009", "13900000010", "13900000011", "13900000012", "13900000013", "13900000014",
                "13900000015", "13900000016", "13900000017"] {
                let expected = if mobile == "13900000000" { 4 }
                    else if matches!(mobile, "13900000013" | "13900000016") { 2 }
                    else { 1 };
                assert_eq!(counts.get(mobile), Some(&expected), "each explicit original send reaches PNVS once");
            }
            assert_eq!(counts.get("13900000040"), Some(&3), "registration, second login and original-mobile verification each send once");
            assert_eq!(counts.get("13900000041"), Some(&2), "new-mobile verification and later login each send once");
            assert_eq!(counts.len(), 20, "only explicit identity flows, re-registration and the separate number and admission beneficiary setups send SMS");
            assert!(supplier.calls.load(Ordering::SeqCst) >= 28);
            let login: serde_json::Value = serde_json::from_slice(
                &std::fs::read(material.path().join("login.json")).expect("browser login evidence"),
            ).expect("non-secret login evidence");
            let operation = login["operation"].as_str().expect("original login operation");
            let observer = connections(&server, &name, "clerk").recover().await.expect("isolated runtime observer");
            let sessions = observer.client().query(
                "SELECT session.id, session.expires_at FROM identity.sessions session JOIN identity.authentications authentication ON authentication.id=session.authentication_id WHERE authentication.operation_id=$1::text::uuid",
                &[&operation],
            ).await.expect("observe original login effects");
            assert_eq!(sessions.len(), 1, "original login recovery creates no second Session");
            assert_eq!(sessions[0].get::<_, uuid::Uuid>(0).to_string(), login["session"]["id"].as_str().expect("session ID"));
            assert_eq!(sessions[0].get::<_, chrono::DateTime<chrono::Utc>>(1), chrono::DateTime::parse_from_rfc3339(login["session"]["expiresAt"].as_str().expect("original expiry")).expect("expiry"));

            let registration: serde_json::Value = serde_json::from_slice(
                &std::fs::read(material.path().join("r2.json")).expect("browser registration evidence"),
            ).expect("non-secret registration evidence");
            let operation = registration["operation"].as_str().expect("original registration operation");
            let authentication = registration["authenticationOperation"].as_str().expect("original authentication operation");
            assert_ne!(operation, authentication, "R2 retains its own operation identity");
            assert_eq!(registration["statuses"], serde_json::json!([201, 200]));
            assert_eq!(registration["requestCount"].as_u64(), Some(2));
            // The application has drained: observe permanent effects without replaying a request.
            let registrations = observer.client().query(
                r#"SELECT receipt.account_id, receipt.person_id, session.id AS session_id,
                    session.expires_at, authentication.operation_id AS authentication_operation,
                    (SELECT count(*) FROM identity.creations WHERE operation_id=receipt.operation_id) AS creations,
                    (SELECT count(*) FROM identity.origins origin
                        JOIN identity.persons created ON created.id=origin.person_id
                        WHERE origin.transaction_id=receipt.transaction_id
                            AND origin.operation_id=receipt.operation_id AND origin.account_id=receipt.account_id
                            AND origin.person_id=receipt.person_id AND origin.revision_id=receipt.revision_id
                            AND origin.sequence=1) AS persons,
                    (SELECT count(*) FROM identity.sessions WHERE authentication_id=authentication.id) AS sessions,
                    (SELECT count(*) FROM access.grants WHERE person_id=receipt.person_id) AS grants
                FROM identity.receipts receipt
                JOIN identity.attempts attempt ON attempt.id=receipt.id
                    AND attempt.operation_id=receipt.operation_id AND attempt.status='registered'
                JOIN identity.accounts account ON account.id=receipt.account_id AND account.person_id=receipt.person_id
                JOIN identity.persons person ON person.id=receipt.person_id
                JOIN identity.sessions session ON session.receipt_id=receipt.id AND session.account_id=receipt.account_id
                JOIN identity.authentications authentication ON authentication.id=attempt.authentication_id
                    AND authentication.id=session.authentication_id
                WHERE receipt.operation_id=$1::text::uuid"#,
                &[&operation],
            ).await.expect("observe original registration effects");
            assert_eq!(registrations.len(), 1, "original registration retains one complete Receipt and identity chain");
            let registered = &registrations[0];
            for column in ["creations", "persons", "sessions"] {
                assert_eq!(registered.get::<_, i64>(column), 1, "registration recovery preserves one {column}");
            }
            assert_eq!(registered.get::<_, i64>("grants"), 2, "the registered person receives a managed Grant and explicitly rejoins for number preservation acceptance");
            assert_eq!(registered.get::<_, uuid::Uuid>("authentication_operation").to_string(), authentication);
            assert_eq!(registered.get::<_, uuid::Uuid>("account_id").to_string(), registration["account"]["id"].as_str().expect("registered Account ID"));
            assert_eq!(registered.get::<_, uuid::Uuid>("person_id").to_string(), registration["account"]["personId"].as_str().expect("registered Person ID"));
            assert_eq!(registered.get::<_, uuid::Uuid>("session_id").to_string(), registration["session"]["id"].as_str().expect("original R2 Session ID"));
            assert_eq!(registered.get::<_, chrono::DateTime<chrono::Utc>>("expires_at"), chrono::DateTime::parse_from_rfc3339(registration["session"]["expiresAt"].as_str().expect("original R2 expiry")).expect("R2 expiry"));
            let logout: serde_json::Value = serde_json::from_slice(
                &std::fs::read(material.path().join("logout.json")).expect("browser logout evidence"),
            ).expect("non-secret logout evidence");
            let operation = logout["operation"].as_str().expect("original logout operation");
            let original = logout["original"].as_str().expect("original logout Session");
            let replacement = logout["replacement"].as_str().expect("new same-account Session");
            let untouched = logout["untouched"].as_str().expect("other account Session");
            assert_eq!(logout["writes"].as_u64(), Some(1));
            assert_eq!(logout["reads"].as_u64(), Some(1));
            assert_ne!(original, replacement);
            let completed = observer.client().query_one(
                r#"SELECT logout.account_id, logout.person_id, logout.revoked_at, logout.sessions,
                    logout.initiator_id=$2::text::uuid AND logout.scope='current' AS bound,
                    revocation.operation_id=logout.id AND revocation.revoked_at=logout.revoked_at AS revoked,
                    NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=logout.initiator_id) AS removed,
                    (SELECT count(*) FROM identity.logouts) AS logouts,
                    (SELECT count(*) FROM identity.revocations WHERE mode='logout') AS revocations
                FROM identity.logouts logout
                JOIN identity.revocations revocation ON revocation.id=logout.initiator_id
                WHERE logout.id=$1::text::uuid"#,
                &[&operation, &original],
            ).await.expect("observe permanent browser logout after application drain");
            assert!(completed.get::<_, bool>("bound"));
            assert!(completed.get::<_, bool>("revoked"));
            assert!(completed.get::<_, bool>("removed"));
            assert_eq!(completed.get::<_, i64>("logouts"), 3, "three explicit confirmations; GET creates no additional logout");
            assert_eq!(completed.get::<_, i64>("revocations"), 4, "current plus the two fixed all-session sets");
            assert_eq!(completed.get::<_, Vec<uuid::Uuid>>("sessions"), vec![uuid::Uuid::parse_str(original).expect("original UUID")]);
            assert_eq!(completed.get::<_, uuid::Uuid>("account_id").to_string(), logout["account"]["id"].as_str().expect("original Account"));
            assert_eq!(completed.get::<_, uuid::Uuid>("person_id").to_string(), logout["account"]["personId"].as_str().expect("original Person"));
            assert_eq!(completed.get::<_, chrono::DateTime<chrono::Utc>>("revoked_at"), chrono::DateTime::parse_from_rfc3339(logout["revokedAt"].as_str().expect("wire logout time")).expect("logout time"));
            let all: serde_json::Value = serde_json::from_slice(
                &std::fs::read(material.path().join("logout-all.json")).expect("browser all logout evidence"),
            ).expect("non-secret all logout evidence");
            let single: serde_json::Value = serde_json::from_slice(
                &std::fs::read(material.path().join("logout-all-single.json")).expect("browser single-session all logout evidence"),
            ).expect("non-secret single-session evidence");
            assert_eq!(all["reads"].as_u64(), Some(1));
            assert_eq!(single["reads"].as_u64(), Some(1));
            assert_eq!(all["interruptedBeforeClassification"], true);
            assert_eq!(single["classificationSaveFailed"], true);
            assert_eq!(all["original"].as_str(), Some(replacement));
            assert_eq!(single["original"].as_str(), Some(untouched));
            for (evidence, count) in [(&all, 2), (&single, 1)] {
                assert_eq!(evidence["writes"].as_u64(), Some(1));
                let operation = evidence["operation"].as_str().expect("all logout operation");
                let original = evidence["original"].as_str().expect("all logout initiator");
                let expected: Vec<uuid::Uuid> = evidence["sessions"].as_array().expect("original fixed set").iter()
                    .map(|id| uuid::Uuid::parse_str(id.as_str().expect("session ID")).expect("session UUID")).collect();
                assert_eq!(expected.len(), count);
                let completed = observer.client().query_one(
                    r#"SELECT logout.account_id, logout.person_id, logout.revoked_at, logout.sessions,
                        logout.initiator_id=$2::text::uuid AND logout.scope='all' AS bound,
                        ARRAY(SELECT id FROM identity.revocations WHERE operation_id=logout.id ORDER BY id) AS revoked,
                        NOT EXISTS(SELECT 1 FROM identity.revocations WHERE operation_id=logout.id
                            AND (account_id<>logout.account_id OR revoked_at<>logout.revoked_at OR mode<>'logout' OR cause_id<>logout.id)) AS consistent,
                        NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=ANY(logout.sessions)) AS removed,
                        EXISTS(SELECT 1 FROM identity.completions WHERE family='logout' AND operation_id=logout.id
                            AND person_id=logout.person_id AND created_at=logout.revoked_at AND action='logout') AS permanent
                    FROM identity.logouts logout WHERE logout.id=$1::text::uuid"#,
                    &[&operation, &original],
                ).await.expect("observe the sealed all-session result after application drain");
                for column in ["bound", "consistent", "removed", "permanent"] {
                    assert!(completed.get::<_, bool>(column), "all logout preserves {column}");
                }
                assert_eq!(completed.get::<_, Vec<uuid::Uuid>>("sessions"), expected);
                assert_eq!(completed.get::<_, Vec<uuid::Uuid>>("revoked"), expected);
                assert_eq!(completed.get::<_, uuid::Uuid>("account_id").to_string(), evidence["account"]["id"].as_str().expect("all logout account"));
                assert_eq!(completed.get::<_, uuid::Uuid>("person_id").to_string(), evidence["account"]["personId"].as_str().expect("all logout person"));
                assert_eq!(completed.get::<_, chrono::DateTime<chrono::Utc>>("revoked_at"), chrono::DateTime::parse_from_rfc3339(evidence["revokedAt"].as_str().expect("all logout time")).expect("timestamp"));
            }
            let retained = observer.client().query(
                r#"SELECT session.id, session.account_id, authentication.operation_id,
                    revocation.operation_id AS logout_operation
                FROM identity.sessions session
                JOIN identity.authentications authentication ON authentication.id=session.authentication_id
                JOIN identity.revocations revocation ON revocation.id=session.id
                WHERE session.id IN ($1::text::uuid, $2::text::uuid)"#,
                &[&replacement, &untouched],
            ).await.expect("observe the later explicit all logouts without another business request");
            assert_eq!(retained.len(), 2);
            for session in retained {
                if session.get::<_, uuid::Uuid>("id").to_string() == replacement {
                    assert_eq!(session.get::<_, uuid::Uuid>("logout_operation").to_string(), all["operation"].as_str().expect("later all logout"));
                    assert_eq!(session.get::<_, uuid::Uuid>("account_id"), completed.get::<_, uuid::Uuid>("account_id"));
                    assert_eq!(session.get::<_, uuid::Uuid>("operation_id").to_string(), logout["authenticationOperation"].as_str().expect("new login operation"));
                } else {
                    assert_eq!(session.get::<_, uuid::Uuid>("logout_operation").to_string(), single["operation"].as_str().expect("later single-session all logout"));
                    assert_eq!(session.get::<_, uuid::Uuid>("account_id"), registered.get::<_, uuid::Uuid>("account_id"));
                }
            }
            for evidence in [&all, &single] {
            let later = evidence["replacement"].as_str().expect("session created after all logout");
            let surviving = observer.client().query_one(
                r#"SELECT session.account_id, authentication.operation_id,
                    session.revoked_at IS NULL AND NOT EXISTS(SELECT 1 FROM identity.revocations WHERE id=session.id) AS active,
                    (SELECT count(*) FROM identity.sessions WHERE account_id=session.account_id AND revoked_at IS NULL) AS remaining
                FROM identity.sessions session JOIN identity.authentications authentication ON authentication.id=session.authentication_id
                WHERE session.id=$1::text::uuid"#,
                &[&later],
            ).await.expect("observe the later login preserved by original-result reads");
            assert!(surviving.get::<_, bool>("active"));
            assert_eq!(surviving.get::<_, i64>("remaining"), 1);
            assert_eq!(surviving.get::<_, uuid::Uuid>("account_id").to_string(), evidence["account"]["id"].as_str().expect("original account"));
            assert_eq!(surviving.get::<_, uuid::Uuid>("operation_id").to_string(), evidence["authenticationOperation"].as_str().expect("later login operation"));
            }
            observer.terminate().await.expect("end runtime observer");
            logs.record(
                "backend_acceptance",
                serde_json::json!({ "outcome": "completed" }),
            );
            logs.finish().expect("browser evidence writes");
            println!("Identity evidence: 28 explicit sends for 20 mobiles, including separate admission, number recovery and mobile-change accounts; original login and registration retain their Sessions and expiry; registration retains one Account and Person with later explicit Workforce admission and rejoining; current logout preserves its original singleton; all logout freezes two sessions and separately accepts one; pre-classification navigation and classification-save failure both recover after login; both later logins survive original-result reads; normal and lost-response cancellations retain independent Account, Person, Holder, Grant, Session, R2 and avatar evidence, while a same-number re-registration is distinct; delayed avatar expiry and five current-number windows preserve independently observed business facts, Cookies and expiry; mobile replacement revokes both originals and removes their Handoffs, while the later new-number login survives reading the original completion");
        }).await.expect("bounded website acceptance");
    });
}
