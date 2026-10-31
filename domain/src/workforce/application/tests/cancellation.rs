use super::*;

fn own(expected_status: CancellableStatus) -> CancellationInput {
    CancellationInput::SelfService { expected_status }
}

fn managed(expected_status: CancellableStatus) -> CancellationInput {
    CancellationInput::Manager {
        expected_status,
        reason: "  Duplicate application\n "
            .to_owned()
            .try_into()
            .expect("reason"),
    }
}

async fn full(service: &Service, actor: &Actor, id: Id) -> Application {
    let Detail::Full { application } = Box::pin(service.detail(actor, id))
        .await
        .expect("full application detail")
    else {
        panic!("expected full access");
    };
    *application
}

async fn reassign(services: &Services, institution: &Actor, mobile: &str, kind: RoleKind) {
    let candidate = Box::pin(services.assignment().candidate(
        institution,
        Entry::Institution,
        mobile.parse().expect("mobile"),
        Pagination::new(1, 20).expect("roles page"),
    ))
    .await
    .expect("assignment candidate");
    let selected = candidate
        .roles()
        .iter()
        .find(|role| role.kind() == kind)
        .expect("selected role");
    let subject = candidate.identity();
    let input = serde_json::from_value(json!({"entry":"institution","target":{"accountId":subject.id(),"personId":subject.person_id(),"revisionId":subject.revision_id(),"mobile":mobile,"grantId":candidate.grant_id(),"assignmentId":candidate.assignment_id()},"role":{"id":selected.id(),"changeId":selected.change_id()}})).expect("assignment input");
    Box::pin(
        services
            .assignment()
            .assign(institution.clone(), operation(), input),
    )
    .await
    .expect("replace current role");
}

async fn audit_count(client: &Client, id: Id) -> i64 {
    client.query_one("SELECT count(*) FROM audit.events WHERE target_id=$1 AND action='workforce.application.cancelled'", &[&id])
        .await.expect("cancellation audits").get(0)
}

async fn update_waiter(client: &Client, user: &str, previous: Option<i32>) -> i32 {
    timeout(Duration::from_secs(3), async {
        let mut cadence = tokio::time::interval(Duration::from_millis(10));
        loop {
            cadence.tick().await;
            if let Some(row) = client.query_opt(
                "SELECT pid FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND usename=$1 AND wait_event_type='Lock' AND query LIKE 'UPDATE workforce.applications SET status=%' AND ($2::int IS NULL OR pid<>$2)",
                &[&user, &previous],
            ).await.expect("specific application writer") {
                break row.get(0);
            }
        }
    }).await.expect("writer reached the controlled application row lock")
}

async fn ended_backend(client: &Client, backend: i32) {
    timeout(Duration::from_secs(3), async {
        let mut cadence = tokio::time::interval(Duration::from_millis(10));
        loop {
            cadence.tick().await;
            if client
                .query_one(
                    "SELECT NOT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid=$1)",
                    &[&backend],
                )
                .await
                .expect("original backend state")
                .get::<_, bool>(0)
            {
                break;
            }
        }
    })
    .await
    .expect("unknown write backend ended before fixture cleanup");
}

async fn ordered_decisions(
    service: &Service,
    actors: (&Actor, &Actor),
    id: Id,
    review_first: bool,
    clients: (&Client, &Client),
    user: &str,
) {
    let (applicant, reviewer) = actors;
    let (owner, observer) = clients;
    owner
        .batch_execute("BEGIN")
        .await
        .expect("controlled row gate");
    owner
        .query_one(
            "SELECT id FROM workforce.applications WHERE id=$1 FOR UPDATE",
            &[&id],
        )
        .await
        .expect("hold the application row");
    let start_review = || {
        let service = service.clone();
        let reviewer = reviewer.clone();
        tokio::spawn(async move { service.review(&reviewer, id, ReviewInput::Approve {}).await })
    };
    let start_cancel = || {
        let service = service.clone();
        let applicant = applicant.clone();
        tokio::spawn(async move {
            service
                .cancel(&applicant, id, own(CancellableStatus::Submitted))
                .await
        })
    };
    let (review, cancel) = if review_first {
        let review = start_review();
        let first = update_waiter(observer, user, None).await;
        let cancel = start_cancel();
        update_waiter(observer, user, Some(first)).await;
        (review, cancel)
    } else {
        let cancel = start_cancel();
        let first = update_waiter(observer, user, None).await;
        let review = start_review();
        update_waiter(observer, user, Some(first)).await;
        (review, cancel)
    };
    owner
        .batch_execute("COMMIT")
        .await
        .expect("release ordered writers");
    let review = timeout(Duration::from_secs(5), review)
        .await
        .expect("bounded review")
        .expect("review task");
    let cancel = timeout(Duration::from_secs(5), cancel)
        .await
        .expect("bounded cancellation")
        .expect("cancellation task");
    if review_first {
        assert!(review.is_ok(), "first approval: {review:?}");
        assert!(
            matches!(cancel, Err(Error::StateChanged)),
            "stale submitted confirmation: {cancel:?}"
        );
        assert_eq!(audit_count(observer, id).await, 0);
        assert!(matches!(
            Box::pin(full(service, applicant, id)).await.decision,
            Decision::Approved { .. }
        ));
        Box::pin(service.cancel(applicant, id, own(CancellableStatus::Approved)))
            .await
            .expect("fresh explicit confirmation");
    } else {
        assert_eq!(cancel.expect("first cancellation"), id);
        assert!(
            matches!(review, Err(Error::StateChanged)),
            "review cannot revive cancellation: {review:?}"
        );
        assert_eq!(observer.query_one("SELECT count(*) FROM audit.events WHERE target_id=$1 AND action='workforce.application.approved'", &[&id]).await.expect("no approval audit").get::<_,i64>(0), 0);
    }
    assert_eq!(audit_count(observer, id).await, 1);
}

async fn verify(
    services: &Services,
    database: &Database,
    supplier: &mut Supplier,
    institution: &Actor,
    applicant: (&Actor, grant::Id),
    ids: &[Id],
    proxy: &fixture::database_proxy::Proxy,
) {
    let (applicant, grant) = applicant;
    let service = services.application();
    let reviewer = Box::pin(crate::access::appointment::tests::register_numbered(
        database, services, supplier, 21,
    ))
    .await;
    let mobile = crate::access::appointment::tests::mobile(21);
    Box::pin(admit(
        services,
        institution,
        Entry::Institution,
        &mobile,
        RoleKind::Full,
    ))
    .await;
    let observer = database
        .connections(&database.user)
        .recover()
        .await
        .expect("cancellation observer");
    let owner = database
        .connections(&database.owner)
        .recover()
        .await
        .expect("isolated fixture owner");
    let client = observer.client();
    let original = serde_json::to_value(Box::pin(full(service, applicant, ids[0])).await)
        .expect("application JSON");
    assert!(matches!(
        Box::pin(service.cancel(institution, ids[0], managed(CancellableStatus::Submitted))).await,
        Err(Error::Grant(grant::Error::Denied))
    ));
    assert!(matches!(
        Box::pin(service.cancel(&reviewer, ids[0], own(CancellableStatus::Submitted))).await,
        Err(Error::TargetUnavailable)
    ));
    assert_eq!(audit_count(client, ids[0]).await, 0);
    Box::pin(service.cancel(applicant, ids[0], own(CancellableStatus::Submitted)))
        .await
        .expect("self submitted cancellation");
    let mut cancelled = original.clone();
    cancelled["status"] = json!("cancelled");
    assert_eq!(
        serde_json::to_value(Box::pin(full(service, applicant, ids[0])).await).expect("JSON"),
        cancelled
    );
    let approved = Box::pin(service.review(&reviewer, ids[1], ReviewInput::Approve {}))
        .await
        .expect("approved fixture");
    let mut approved = serde_json::to_value(approved).expect("approved JSON");
    assert!(matches!(
        Box::pin(service.cancel(applicant, ids[1], own(CancellableStatus::Submitted))).await,
        Err(Error::StateChanged)
    ));
    assert_eq!(audit_count(client, ids[1]).await, 0);
    Box::pin(service.cancel(applicant, ids[1], managed(CancellableStatus::Approved)))
        .await
        .expect("explicit manager cancellation of own application");
    approved["status"] = json!("cancelled");
    assert_eq!(
        serde_json::to_value(
            Box::pin(service.managed_detail(&reviewer, ids[1]))
                .await
                .expect("manager retained detail")
                .application
        )
        .expect("JSON"),
        approved
    );
    let audit = client.query_one("SELECT actor_id,details::text FROM audit.events WHERE target_id=$1 AND action='workforce.application.cancelled'", &[&ids[1]]).await.expect("manager audit");
    let person: person::Id = client
        .query_one(
            "SELECT person_id FROM identity.accounts WHERE id=$1",
            &[&applicant.account_id()],
        )
        .await
        .expect("operator person")
        .get(0);
    assert_eq!(audit.get::<_, person::Id>(0), person);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&audit.get::<_, String>(1))
            .expect("audit details"),
        json!({"previousStatus":"approved","status":"cancelled","mode":"manager","reason":"Duplicate application"})
    );
    let details: String = client.query_one("SELECT details::text FROM audit.events WHERE target_id=$1 AND action='workforce.application.cancelled'", &[&ids[0]]).await.expect("self audit").get(0);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&details).expect("self details"),
        json!({"previousStatus":"submitted","status":"cancelled","mode":"self"})
    );
    Box::pin(service.cancel(&reviewer, ids[2], managed(CancellableStatus::Submitted)))
        .await
        .expect("management cancellation across persons");
    for id in &ids[..3] {
        assert!(matches!(
            Box::pin(service.cancel(applicant, *id, own(CancellableStatus::Submitted))).await,
            Err(Error::StateChanged)
        ));
        assert_eq!(audit_count(client, *id).await, 1);
    }
    owner.client().batch_execute("CREATE FUNCTION audit.reject_cancellation_fixture() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture cancellation audit rejection'; END $$; CREATE TRIGGER reject_cancellation_fixture BEFORE INSERT ON audit.events FOR EACH ROW EXECUTE FUNCTION audit.reject_cancellation_fixture()").await.expect("inject audit failure");
    assert!(
        Box::pin(service.cancel(applicant, ids[3], own(CancellableStatus::Submitted)))
            .await
            .is_err()
    );
    assert!(matches!(
        Box::pin(full(service, applicant, ids[3])).await.decision,
        Decision::Submitted
    ));
    assert_eq!(audit_count(client, ids[3]).await, 0);
    owner.client().batch_execute("DROP TRIGGER reject_cancellation_fixture ON audit.events; DROP FUNCTION audit.reject_cancellation_fixture()").await.expect("remove audit failure");
    proxy.arm(fixture::database_proxy::Fault::LostCommit);
    let result =
        Box::pin(service.cancel(applicant, ids[3], own(CancellableStatus::Submitted))).await;
    assert!(
        matches!(result, Err(Error::Unknown(_))),
        "lost commit: {result:?}"
    );
    let backend = proxy.intercepted().await;
    assert!(matches!(
        Box::pin(full(service, applicant, ids[3])).await.decision,
        Decision::Cancelled { approval: None }
    ));
    assert_eq!(audit_count(client, ids[3]).await, 1, "no automatic replay");
    proxy.release();
    ended_backend(client, backend).await;
    ordered_decisions(
        service,
        (applicant, &reviewer),
        ids[4],
        true,
        (owner.client(), client),
        &database.user,
    )
    .await;
    ordered_decisions(
        service,
        (applicant, &reviewer),
        ids[5],
        false,
        (owner.client(), client),
        &database.user,
    )
    .await;
    owner
        .client()
        .batch_execute("BEGIN")
        .await
        .expect("duplicate gate");
    owner
        .client()
        .query_one(
            "SELECT id FROM workforce.applications WHERE id=$1 FOR UPDATE",
            &[&ids[6]],
        )
        .await
        .expect("hold duplicate row");
    let mut tasks = Vec::new();
    let mut previous = None;
    for _ in 0..2 {
        let service = service.clone();
        let actor = applicant.clone();
        let id = ids[6];
        tasks.push(tokio::spawn(async move {
            service
                .cancel(&actor, id, own(CancellableStatus::Submitted))
                .await
        }));
        previous = Some(update_waiter(client, &database.user, previous).await);
    }
    owner
        .client()
        .batch_execute("COMMIT")
        .await
        .expect("release duplicate cancellations");
    let mut successes = 0;
    for task in tasks {
        match timeout(Duration::from_secs(5), task)
            .await
            .expect("bounded duplicate")
            .expect("duplicate task")
        {
            Ok(id) => {
                assert_eq!(id, ids[6]);
                successes += 1;
            }
            Err(Error::StateChanged) => {}
            result => panic!("unexpected duplicate result: {result:?}"),
        }
    }
    assert_eq!(successes, 1);
    assert_eq!(audit_count(client, ids[6]).await, 1);
    for sql in [
        "UPDATE workforce.applications SET status='submitted' WHERE id=$1",
        "UPDATE workforce.applications SET status='cancelled' WHERE id=$1",
        "UPDATE workforce.applications SET reviewer_id=person_id,reviewed_at=statement_timestamp() WHERE id=$1",
        r#"UPDATE workforce.applications SET snapshot=jsonb_set(snapshot,'{pickup}','"Changed"') WHERE id=$1"#,
        "UPDATE workforce.applications SET kind='replacement' WHERE id=$1",
        "DELETE FROM workforce.applications WHERE id=$1",
    ] {
        let error = owner
            .client()
            .execute(sql, &[&ids[0]])
            .await
            .expect_err("terminal and fixed facts protected");
        assert_eq!(
            error.code(),
            Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION),
            "{sql}: {error}"
        );
    }
    Box::pin(service.review(&reviewer, ids[12], ReviewInput::Approve {}))
        .await
        .expect("fresh approved database protection fixture");
    for sql in [
        "UPDATE workforce.applications SET status='cancelled',reviewer_id=NULL WHERE id=$1",
        "UPDATE workforce.applications SET status='cancelled',reviewed_at=statement_timestamp() WHERE id=$1",
        "UPDATE workforce.applications SET status='cancelled',rejection_reason='Changed' WHERE id=$1",
    ] {
        owner
            .client()
            .execute(sql, &[&ids[12]])
            .await
            .expect_err("retained approval cannot be rewritten");
    }
    owner.client().execute("INSERT INTO workforce.applications (id,person_id,kind,status,snapshot) SELECT $1,person_id,kind,'cancelled',snapshot FROM workforce.applications WHERE id=$2", &[&Id::allocate(),&ids[0]]).await.expect_err("new applications cannot begin cancelled");
    let rejected = serde_json::from_value(json!({"decision":"reject","reason":"Wrong photo"}))
        .expect("reject input");
    Box::pin(service.review(&reviewer, ids[7], rejected))
        .await
        .expect("rejected terminal");
    assert!(matches!(
        Box::pin(service.cancel(applicant, ids[7], own(CancellableStatus::Submitted))).await,
        Err(Error::StateChanged)
    ));
    assert_eq!(audit_count(client, ids[7]).await, 0);
    let resource = Box::pin(full(service, applicant, ids[0]))
        .await
        .photo
        .resource_id;
    Box::pin(service.resource(applicant, resource, read_capacity(), false))
        .await
        .expect("normal retained photo");
    Box::pin(service.resource(&reviewer, resource, read_capacity(), true))
        .await
        .expect("manager retained photo");

    // Reassignment removes approval authority while retaining a normal active Grant.
    proxy.arm(fixture::database_proxy::Fault::StalledQuery);
    let pending = {
        let service = service.clone();
        let reviewer = reviewer.clone();
        let id = ids[8];
        tokio::spawn(async move {
            service
                .cancel(&reviewer, id, managed(CancellableStatus::Submitted))
                .await
        })
    };
    let waiting = proxy.intercepted().await;
    Box::pin(reassign(services, institution, &mobile, RoleKind::Default)).await;
    proxy.release();
    assert!(matches!(
        timeout(Duration::from_secs(5), pending)
            .await
            .expect("revoked request ended")
            .expect("revoked task"),
        Err(Error::Grant(grant::Error::Denied))
    ));
    assert_eq!(audit_count(client, ids[8]).await, 0);
    assert!(client.query_one("SELECT NOT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND state='active')", &[&waiting]).await.expect("original request ended").get::<_,bool>(0));
    Box::pin(reassign(services, institution, &mobile, RoleKind::Full)).await;

    Box::pin(reassign(
        services,
        institution,
        "13800000000",
        RoleKind::Default,
    ))
    .await;
    let candidate = Box::pin(
        services
            .grant()
            .suspension_candidate(&reviewer, "13800000000".parse().expect("mobile")),
    )
    .await
    .expect("suspension candidate");
    let subject = candidate.identity();
    let target = json!({"target":{"accountId":subject.id(),"personId":subject.person_id(),"revisionId":subject.revision_id(),"mobile":"13800000000"}});
    let suspension = Box::pin(services.grant().suspend(
        reviewer.clone(),
        operation(),
        grant,
        serde_json::from_value(target.clone()).expect("suspension input"),
    ))
    .await
    .expect("suspend participation");
    Box::pin(service.cancel(applicant, ids[8], own(CancellableStatus::Submitted)))
        .await
        .expect("suspended Grant still permits self cancellation");
    assert!(
        matches!(
            Box::pin(service.detail(applicant, ids[8])).await,
            Err(Error::TargetUnavailable)
        ),
        "restricted invisibility is not a success receipt"
    );
    let restricted = serde_json::to_value(
        Box::pin(service.list(applicant, Pagination::new(1, 100).expect("page")))
            .await
            .expect("restricted list"),
    )
    .expect("JSON");
    let count: i64 = client.query_one("SELECT count(*) FROM workforce.applications WHERE person_id=$1 AND status IN ('submitted','approved')", &[&person]).await.expect("independent restricted set").get(0);
    assert_eq!(restricted["access"], "restricted");
    assert_eq!(restricted["count"], count);
    for row in restricted["applications"]
        .as_array()
        .expect("restricted rows")
    {
        assert_eq!(row.as_object().expect("four-field summary").len(), 4);
        assert!(row["status"] == "submitted" || row["status"] == "approved");
    }
    Box::pin(services.grant().resume(
        reviewer.clone(),
        operation(),
        suspension.id(),
        serde_json::from_value(target).expect("resumption input"),
    ))
    .await
    .expect("resume participation");
    Box::pin(reassign(
        services,
        institution,
        "13800000000",
        RoleKind::Full,
    ))
    .await;
    proxy.arm(fixture::database_proxy::Fault::StalledQuery);
    let pending = {
        let service = service.clone();
        let actor = applicant.clone();
        let id = ids[9];
        tokio::spawn(async move {
            service
                .cancel(&actor, id, managed(CancellableStatus::Submitted))
                .await
        })
    };
    proxy.intercepted().await;
    Box::pin(
        services
            .grant()
            .terminate(applicant.clone(), operation(), grant),
    )
    .await
    .expect("end participation before final authorization");
    proxy.release();
    assert!(
        matches!(
            timeout(Duration::from_secs(5), pending)
                .await
                .expect("ended Grant request finished")
                .expect("manager task"),
            Err(Error::Grant(grant::Error::Denied))
        ),
        "manager mode must never fall back to self"
    );
    assert_eq!(audit_count(client, ids[9]).await, 0);
    Box::pin(service.cancel(applicant, ids[9], own(CancellableStatus::Submitted)))
        .await
        .expect("ended Grant permits a newly explicit self request");
    Box::pin(service.cancel(&reviewer, ids[10], managed(CancellableStatus::Submitted)))
        .await
        .expect("manager may cancel an ineligible applicant");
    let candidate = Box::pin(
        services
            .account()
            .candidate(institution, "13800000000".parse().expect("mobile")),
    )
    .await
    .expect("account suspension candidate");
    let subject = candidate.identity();
    Box::pin(services.suspension().suspend(institution.clone(), operation(), applicant.account_id(), serde_json::from_value(json!({"personId":subject.person_id(),"revisionId":subject.revision_id(),"mobile":"13800000000"})).expect("account suspension input"))).await.expect("suspend the account and revoke sessions");
    assert!(matches!(
        Box::pin(service.cancel(applicant, ids[11], own(CancellableStatus::Submitted))).await,
        Err(Error::Identity(current::Error::Invalid))
    ));
    assert_eq!(audit_count(client, ids[11]).await, 0);
    Box::pin(service.cancel(&reviewer, ids[11], managed(CancellableStatus::Submitted)))
        .await
        .expect("current manager cancels a suspended account's application");
    observer.terminate().await.expect("observer ended");
    owner.terminate().await.expect("fixture owner ended");
}

#[test]
fn cancellations_preserve_authority_expected_states_audit_and_retained_facts() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(90), Box::pin(async {
            let database=Database::initialized(Arc::clone(&server)).await;
            let mut supplier=Supplier::new().await;
            let (root,mount,device)=server.files();
            let storage=platform::files::Settings::new(root,mount,device).expect("storage");
            let mut background=Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(85,54).write_to(&mut background,image::ImageFormat::Png).expect("background");
            let settings=Settings::new(storage,fixture_template(),"image/png",background.into_inner(),include_bytes!("../../../../tests/fixtures/fonts/NotoSansSC-Regular.otf").to_vec(),vec!["Fixture reception".to_owned()],photo::Limits::new(1,1 << 30,2,2 << 30).expect("limits")).expect("settings");
            let proxy=fixture::database_proxy::Proxy::start(&server).await;
            let context=Context::new(authentication::proxied(&database,&proxy)).await.expect("context");
            let services=application_services(&context,ServicesSettings::new(4,crate::identity::message::Limits::default(),authentication::session_settings(),crate::registration::Limits::default(),authentication::avatar_settings()).expect("services settings").with_application(settings),&supplier).await;
            let institution=Box::pin(actor(&database,&services,&mut supplier)).await;
            let applicant=Box::pin(register(&database,&services,&mut supplier)).await;
            let grant=Box::pin(admit(&services,&institution,Entry::Institution,"13800000000",RoleKind::Full)).await;
            Box::pin(services.number().allocate(applicant.clone(),operation(),serde_json::from_value(json!({"subtype":100})).expect("number"))).await.expect("allocate number");
            let Item::Group(group)=Box::pin(create(&services,&applicant,Creation::Group(serde_json::from_value(json!({"name":"Group","enabled":true,"index":0})).expect("group")))).await else {panic!("group")};
            let Item::Department(department)=Box::pin(create(&services,&applicant,Creation::Department(serde_json::from_value(json!({"name":"Department","enabled":true,"index":0,"groupId":group.id(),"parentId":null})).expect("department")))).await else {panic!("department")};
            for subtype in number::Subtype::ALL {Box::pin(services.directory().configure_rule(applicant.clone(),operation(),subtype,serde_json::from_value(json!({"departmentId":department.id(),"position":false})).expect("rule"))).await.expect("complete rules");}
            let mut image=Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(120,160).write_to(&mut image,image::ImageFormat::Png).expect("photo");
            let photo=Box::pin(services.application().prepare_photo(&applicant,"image/png".to_owned(),image.into_inner())).await.expect("prepare photo");
            let selections=Selections {department_id:department.id(),roster_id:None,title_id:None,photo_id:photo.photo_id,pickup:"Fixture reception".to_owned()};
            let preview=Box::pin(services.application().preview(&applicant,PreviewInput {kind:Kind::Issuance,selections:selections.clone()})).await.expect("preview");
            let mut ids=Vec::new();
            for _ in 0..13 {ids.push(Box::pin(services.application().submit(&applicant,Input {kind:Kind::Issuance,selections:selections.clone(),expected:preview.snapshot.clone()})).await.expect("submitted cancellation fixture").id);}
            Box::pin(verify(&services,&database,&mut supplier,&institution,(&applicant,grant.id()),&ids,&proxy)).await;
            services.close(); services.drain().await.expect("services drained"); context.stop().await.expect("context stopped"); proxy.stop().await; supplier.stop().await;
        })).await.expect("bounded cancellation acceptance");
    }));
}
