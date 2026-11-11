use std::fs;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use crate::auth;
use crate::build_app;
use crate::state::*;
use axum::{body::Body, http::{header, Request, StatusCode}, response::Response, Router};
use serde_json::json;
use tower::ServiceExt;


    
    

    struct Env {
        base: PathBuf,
        profiles: PathBuf,
        data: PathBuf,
        archive: PathBuf,
        backup: PathBuf,
        key: Arc<Vec<u8>>,
    }

    impl Env {
        fn new(tag: &str) -> Env {
            let base = std::env::temp_dir().join(format!(
                "mbox_it_{}_{}_{}",
                tag,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .subsec_nanos()
            ));
            let env = Env {
                profiles: base.join("profiles"),
                data: base.join("data"),
                archive: base.join("archive"),
                backup: base.join("backup"),
                base,
                key: Arc::new((0..32u8).collect()),
            };
            fs::create_dir_all(&env.profiles).unwrap();
            env
        }
        fn add(&self, slug: &str, name: &str, password: &str) {
            fs::create_dir_all(self.profiles.join(slug)).unwrap();
            auth::write_profile(&self.profiles, slug, name, &auth::sha256_hex(password)).unwrap();
        }
        /// Mirrors the boot path exactly: roster off disk, epoch seeded from
        /// the stored secret. Two calls = process restart against the same disk.
        fn root(&self) -> Root {
            let mut tenants = HashMap::new();
            for p in auth::load_profiles(&self.profiles) {
                tenants.insert(
                    p.slug.clone(),
                    seed_tenant(&self.profiles, &p.slug, &p.name, &self.data, &self.archive, &self.backup),
                );
            }
            Root {
                tenants: Arc::new(tenants),
                profiles_dir: self.profiles.clone(),
                key: self.key.clone(),
            }
        }
        fn app(&self) -> Router {
            build_app(self.root())
        }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn req(method: &str, uri: &str, body: Body, cookie: Option<&str>, json: bool) -> Request<Body> {
        let mut rb = Request::builder().method(method).uri(uri);
        if json {
            rb = rb.header(header::CONTENT_TYPE, "application/json");
        }
        if let Some(c) = cookie {
            rb = rb.header(header::COOKIE, c);
        }
        rb.body(body).unwrap()
    }

    async fn send(app: &Router, r: Request<Body>) -> Response {
        app.clone().oneshot(r).await.unwrap()
    }

    async fn login(app: &Router, slug: &str, password: &str) -> Response {
        send(
            app,
            req(
                "POST",
                "/api/login",
                Body::from(json!({ "slug": slug, "password": password }).to_string()),
                None,
                true,
            ),
        )
        .await
    }

    fn cookie_of(r: &Response) -> String {
        r.headers()
            .get(header::SET_COOKIE)
            .expect("response carries no Set-Cookie")
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    }

    async fn whoami_status(app: &Router, cookie: &str) -> StatusCode {
        send(app, req("GET", "/api/whoami", Body::empty(), Some(cookie), false))
            .await
            .status()
    }

    async fn json_of(r: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn body_str(r: Response) -> String {
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 22).await.unwrap();
        String::from_utf8_lossy(&bytes).to_string()
    }

    #[tokio::test]
    async fn anonymous_and_bad_credentials_are_rejected() {
        let env = Env::new("reject");
        env.add("daniel", "Daniel", "daniel");
        let app = env.app();
        assert_eq!(whoami_status(&app, "mbx_s=nope").await, StatusCode::UNAUTHORIZED);
        assert_eq!(login(&app, "daniel", "hunter2").await.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(login(&app, "ghost", "daniel").await.status(), StatusCode::UNAUTHORIZED);
        let body = json_of(login(&app, "daniel", "hunter2").await).await;
        assert_eq!(body["error"], "wrong password");
    }

    #[tokio::test]
    async fn login_upgrades_legacy_secret_and_both_sessions_work() {
        let env = Env::new("upgrade");
        env.add("daniel", "Daniel", "daniel");
        let app = env.app();
        let r = login(&app, "daniel", "daniel").await;
        assert_eq!(r.status(), StatusCode::OK);
        let body = json_of(send(&app, req("GET", "/api/whoami", Body::empty(), Some(&cookie_of(&r)), false)).await).await;
        assert_eq!(body["slug"], "daniel");
        assert!(
            auth::read_profile(&env.profiles, "daniel").unwrap().secret.starts_with("$2"),
            "legacy secret should have been upgraded to bcrypt"
        );
        // and the bcrypt path must issue working sessions too
        let r2 = login(&app, "daniel", "daniel").await;
        assert_eq!(r2.status(), StatusCode::OK);
        assert_eq!(whoami_status(&app, &cookie_of(&r2)).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn sessions_survive_a_restart() {
        // Regression guard for boot-time epoch seeding: a fresh Root built
        // from the same disk must keep accepting cookies signed under the
        // current secret.
        let env = Env::new("restart");
        env.add("daniel", "Daniel", "daniel");
        let cookie = cookie_of(&login(&env.app(), "daniel", "daniel").await);
        let rebooted = env.app();
        assert_eq!(whoami_status(&rebooted, &cookie).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn password_change_revokes_every_older_session() {
        let env = Env::new("revoke");
        env.add("daniel", "Daniel", "daniel");
        let app = env.app();
        let a = cookie_of(&login(&app, "daniel", "daniel").await);
        let b = cookie_of(&login(&app, "daniel", "daniel").await);
        let changed = send(
            &app,
            req(
                "POST",
                "/api/profile/password",
                Body::from(json!({ "current": "daniel", "new": "secretive" }).to_string()),
                Some(&a),
                true,
            ),
        )
        .await;
        assert_eq!(changed.status(), StatusCode::OK);
        let a2 = cookie_of(&changed);
        assert_eq!(whoami_status(&app, &a2).await, StatusCode::OK, "changed device keeps working");
        assert_eq!(whoami_status(&app, &a).await, StatusCode::UNAUTHORIZED, "old cookie revoked");
        assert_eq!(whoami_status(&app, &b).await, StatusCode::UNAUTHORIZED, "other device revoked");
        assert_eq!(login(&app, "daniel", "daniel").await.status(), StatusCode::UNAUTHORIZED, "old password dead");
        let fresh = login(&app, "daniel", "secretive").await;
        assert_eq!(fresh.status(), StatusCode::OK, "new password logs in");
        assert_eq!(whoami_status(&app, &cookie_of(&fresh)).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn too_short_new_password_is_400_and_changes_nothing() {
        let env = Env::new("shortpw");
        env.add("daniel", "Daniel", "daniel");
        let app = env.app();
        let a = cookie_of(&login(&app, "daniel", "daniel").await);
        let before = auth::read_profile(&env.profiles, "daniel").unwrap().secret;
        let r = send(
            &app,
            req(
                "POST",
                "/api/profile/password",
                Body::from(json!({ "current": "daniel", "new": "abc" }).to_string()),
                Some(&a),
                true,
            ),
        )
        .await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        assert_eq!(auth::read_profile(&env.profiles, "daniel").unwrap().secret, before);
        assert_eq!(whoami_status(&app, &a).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn tampered_cookie_is_rejected() {
        let env = Env::new("tamper");
        env.add("daniel", "Daniel", "daniel");
        let app = env.app();
        let cookie = cookie_of(&login(&app, "daniel", "daniel").await);
        let (_, val) = cookie.split_once('=').unwrap();
        let (ph, sig) = val.split_once('.').unwrap();
        let mut chars: Vec<char> = ph.chars().collect();
        chars[0] = if chars[0] == '0' { '1' } else { '0' };
        let bad = format!("mbx_s={}.{}", chars.iter().collect::<String>(), sig);
        assert_eq!(whoami_status(&app, &bad).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn logout_expires_the_session_cookie() {
        let env = Env::new("logout");
        env.add("daniel", "Daniel", "daniel");
        let app = env.app();
        let r = send(&app, req("POST", "/api/logout", Body::empty(), None, false)).await;
        assert_eq!(r.status(), StatusCode::OK);
        let sc = r.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap();
        assert!(sc.contains("Max-Age=0"));
    }

    async fn get_json(app: &Router, uri: &str, cookie: Option<&str>) -> (StatusCode, serde_json::Value) {
        let r = send(app, req("GET", uri, Body::empty(), cookie, false)).await;
        let status = r.status();
        (status, json_of(r).await)
    }

    #[tokio::test]
    async fn rename_updates_all_views_without_touching_sessions() {
        let env = Env::new("rename");
        env.add("daniel", "Daniel", "daniel");
        let app = env.app();
        let r = login(&app, "daniel", "daniel").await;
        let cookie = cookie_of(&r);
        let secret_before = auth::read_profile(&env.profiles, "daniel").unwrap().secret;

        let changed = send(
            &app,
            req(
                "POST",
                "/api/profile/name",
                Body::from(json!({ "name": "  Dan  " }).to_string()),
                Some(&cookie),
                true,
            ),
        )
        .await;
        assert_eq!(changed.status(), StatusCode::OK);

        let (s, body) = get_json(&app, "/api/whoami", Some(&cookie)).await;
        assert_eq!(s, StatusCode::OK, "renamed device keeps its session");
        assert_eq!(body["name"], "Dan", "whoami shows trimmed new name");
        let (_, prof) = get_json(&app, "/api/profile", Some(&cookie)).await;
        assert_eq!(prof["name"], "Dan");
        let (s, roster) = get_json(&app, "/api/profiles", None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(roster[0]["name"], "Dan", "chooser reads new name pre-login");

        assert_eq!(
            auth::read_profile(&env.profiles, "daniel").unwrap().secret,
            secret_before,
            "rename must not alter the secret"
        );
        assert_eq!(login(&app, "daniel", "daniel").await.status(), StatusCode::OK, "old password still works");
    }

    #[tokio::test]
    async fn rename_rejects_blank_and_overlong_names() {
        let env = Env::new("badname");
        env.add("daniel", "Daniel", "daniel");
        let app = env.app();
        let cookie = cookie_of(&login(&app, "daniel", "daniel").await);
        for name in ["", "   ", &"x".repeat(81)] {
            let r = send(
                &app,
                req(
                    "POST",
                    "/api/profile/name",
                    Body::from(json!({ "name": name }).to_string()),
                    Some(&cookie),
                    true,
                ),
            )
            .await;
            assert_eq!(r.status(), StatusCode::BAD_REQUEST, "accepted name {:?}", name);
        }
        assert_eq!(auth::read_profile(&env.profiles, "daniel").unwrap().name, "Daniel");
    }

    // ---- archive API: fixtures + helpers -------------------------------

    impl Env {
        fn mail(&self, slug: &str, file: &str, content: &str) {
            let dir = self.data.join(slug);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(file), content).unwrap();
        }
        fn add_with_addresses(&self, slug: &str, name: &str, password: &str, addresses: &[String]) {
            self.add(slug, name, password);
            let sp = auth::read_profile(&self.profiles, slug).unwrap();
            auth::write_profile_full(&self.profiles, slug, name, &sp.secret, addresses).unwrap();
        }
    }

    /// A two-message thread (reply chains to q1) plus, in a second file, a
    /// newer solo message and an edited copy of q1 for merge dedup tests.
    /// mboxshell threads solely on Gmail's `X-GM-THRID`, which real Takeout
    /// exports carry on every message — so the fixture does too.
    const THREAD_MBOX: &str = r#"From dave@example.com Mon Jan  5 10:00:00 2026
From: Dave Liang <dave@example.com>
To: me@example.com
Subject: Quarterly report
Date: Mon, 5 Jan 2026 10:00:00 +0000
Message-ID: <q1@example.com>
X-GM-THRID: 18ab3c4d5e6f7081

Q1 numbers are in. See the quarterly report summary for details.

From me@example.com Tue Jan  6 09:00:00 2026
From: Me <me@example.com>
To: dave@example.com
Subject: Re: Quarterly report
Date: Tue, 6 Jan 2026 09:00:00 +0000
Message-ID: <q2@example.com>
In-Reply-To: <q1@example.com>
References: <q1@example.com>
X-GM-THRID: 18ab3c4d5e6f7081

Thanks, reviewing it now.
"#;

    const LUNCH_MBOX: &str = r#"From dave@example.com Thu Jan  8 12:00:00 2026
From: Dave Liang <dave@example.com>
To: me+inbox@example.com
Subject: Lunch plans
Date: Thu, 8 Jan 2026 12:00:00 +0000
Message-ID: <l1@example.com>

Pizza or sushi, decide quickly.
"#;

    const UPDATE_MBOX: &str = r#"From dave@example.com Fri Jan  9 12:00:00 2026
From: Dave Liang <dave@example.com>
To: me@example.com
Subject: Quarterly report final
Date: Fri, 9 Jan 2026 12:00:00 +0000
Message-ID: <q1@example.com>
X-GM-THRID: 18ab3c4d5e6f7081

Same message id, corrected body text here.
"#;

    async fn scan(app: &Router, cookie: &str) {
        let r = send(app, req("POST", "/api/scan", Body::empty(), Some(cookie), false)).await;
        assert_eq!(r.status(), StatusCode::OK);
    }

    async fn post(app: &Router, cookie: &str, uri: &str) -> StatusCode {
        send(app, req("POST", uri, Body::empty(), Some(cookie), false)).await.status()
    }

    async fn wait_indexed(app: &Router, cookie: &str, total: u64) -> serde_json::Value {
        for _ in 0..200 {
            let (_, st) = get_json(app, "/api/status", Some(cookie)).await;
            if !st["busy"].as_bool().unwrap() && st["total"].as_u64() == Some(total) {
                return st;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("index never reached total={total}");
    }

    async fn threads(app: &Router, cookie: &str, uri: &str) -> serde_json::Value {
        let (s, v) = get_json(app, uri, Some(cookie)).await;
        assert_eq!(s, StatusCode::OK, "{uri}");
        v
    }

    /// Thread ids are raw Message-IDs (`<a1@x>`), illegal bare in a URI.
    fn enc(s: &str) -> String {
        s.replace('%', "%25").replace('<', "%3C").replace('>', "%3E").replace(' ', "%20")
    }

    #[tokio::test]
    async fn scan_indexes_threads_email_and_facets() {
        let env = Env::new("scanapi");
        env.add("daniel", "Daniel", "daniel");
        // one file: with several raw mboxes and no archive, scan indexes only the newest
        env.mail("daniel", "full.mbox", &format!("{THREAD_MBOX}\n{LUNCH_MBOX}"));
        let app = env.app();
        let cookie = cookie_of(&login(&app, "daniel", "daniel").await);

        scan(&app, &cookie).await;
        let st = wait_indexed(&app, &cookie, 3).await;
        assert_eq!(st["busy"], false);

        let list = threads(&app, &cookie, "/api/messages").await;
        assert_eq!(list.as_array().unwrap().len(), 2, "two threads: reply folded into root");
        assert_eq!(list[0]["subject"], "Lunch plans", "newest thread first");
        let q = &list[1];
        assert_eq!(q["subject"], "Quarterly report");
        assert_eq!(q["count"], 2);
        assert_eq!(q["from"], "Dave Liang <dave@example.com>");

        let (_, fac) = get_json(&app, "/api/filters", Some(&cookie)).await;
        assert_eq!(fac["total"], 3);
        assert!(fac["min_ts"].as_i64().unwrap() < fac["max_ts"].as_i64().unwrap());

        let tid = q["tid"].as_str().unwrap();
        let (_, det) = get_json(&app, &format!("/api/thread?tid={}", enc(tid)), Some(&cookie)).await;
        let msgs = det["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(
            msgs[0]["date"].as_str().unwrap() < msgs[1]["date"].as_str().unwrap(),
            "thread messages ascending"
        );

        let id = msgs[0]["id"].as_str().unwrap();
        let r = send(&app, req("GET", &format!("/api/email/{id}"), Body::empty(), Some(&cookie), false)).await;
        assert_eq!(r.status(), StatusCode::OK);
        assert!(body_str(r).await.contains("quarterly report summary"));
        let (_, meta) = get_json(&app, &format!("/api/email/{id}/meta"), Some(&cookie)).await;
        assert_eq!(meta["subject"], "Quarterly report");
        assert_eq!(meta["attachments"].as_array().unwrap().len(), 0);

        let r = send(&app, req("GET", "/api/thread?tid=nope", Body::empty(), Some(&cookie), false)).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn search_matches_subject_but_bodies_only_when_requested() {
        let env = Env::new("searchapi");
        env.add("daniel", "Daniel", "daniel");
        env.mail("daniel", "full.mbox", &format!("{THREAD_MBOX}\n{LUNCH_MBOX}"));
        let app = env.app();
        let cookie = cookie_of(&login(&app, "daniel", "daniel").await);
        scan(&app, &cookie).await;
        wait_indexed(&app, &cookie, 3).await;

        let r = threads(&app, &cookie, "/api/search?q=quarterly").await;
        assert_eq!(r.as_array().unwrap().len(), 1, "subject hit folds to one thread");
        assert_eq!(r[0]["subject"], "Quarterly report");
        assert!(r[0]["matched"].as_u64().unwrap() >= 1);

        let r = threads(&app, &cookie, "/api/search?q=from:dave").await;
        assert_eq!(r.as_array().unwrap().len(), 2, "both threads are from dave (one message each)");

        // "summary" exists only in the body: absent by default, found with body=1
        let r = threads(&app, &cookie, "/api/search?q=summary").await;
        assert_eq!(r.as_array().unwrap().len(), 0);
        let r = threads(&app, &cookie, "/api/search?q=summary&body=1").await;
        assert_eq!(r.as_array().unwrap().len(), 1);

        let r = threads(&app, &cookie, "/api/search?q=zzq-nonexistent").await;
        assert_eq!(r.as_array().unwrap().len(), 0);
        let r = threads(&app, &cookie, "/api/search?q=").await;
        assert_eq!(r.as_array().unwrap().len(), 0, "empty query never scans");
    }

    #[tokio::test]
    async fn merge_dedups_by_message_id_and_rescan_reads_the_archive() {
        let env = Env::new("mergeapi");
        env.add("daniel", "Daniel", "daniel");
        env.mail("daniel", "01.mbox", THREAD_MBOX);
        env.mail("daniel", "02.mbox", LUNCH_MBOX);
        env.mail("daniel", "03.mbox", UPDATE_MBOX);
        let app = env.app();
        let cookie = cookie_of(&login(&app, "daniel", "daniel").await);

        assert_eq!(post(&app, &cookie, "/api/merge").await, StatusCode::OK);
        for _ in 0..200 {
            if !env.archive.join("daniel").join("merged.mbox").exists() {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            } else {
                break;
            }
        }
        let merged = fs::read_to_string(env.archive.join("daniel").join("merged.mbox")).expect("merged.mbox");
        assert_eq!(merged.matches("Message-ID: <q1@example.com>").count(), 1, "duplicate id collapsed");
        assert!(merged.contains("<l1@example.com>") && merged.contains("<q2@example.com>"));

        scan(&app, &cookie).await;
        let st = wait_indexed(&app, &cookie, 3).await;
        assert_eq!(st["source"], "merged.mbox", "scan prefers the archive over raw exports");
    }

    #[tokio::test]
    async fn tenants_only_ever_see_their_own_archive() {
        let env = Env::new("scopeapi");
        env.add("daniel", "Daniel", "daniel");
        env.add("hajni", "Hajni", "hajni");
        env.mail("daniel", "export.mbox", THREAD_MBOX);
        env.mail("hajni", "export.mbox", LUNCH_MBOX);
        let app = env.app();
        let d = cookie_of(&login(&app, "daniel", "daniel").await);
        let h = cookie_of(&login(&app, "hajni", "hajni").await);
        scan(&app, &d).await;
        scan(&app, &h).await;
        wait_indexed(&app, &d, 2).await;
        wait_indexed(&app, &h, 1).await;

        let list = threads(&app, &h, "/api/messages").await;
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0]["subject"], "Lunch plans");
        let r = threads(&app, &h, "/api/search?q=quarterly").await;
        assert_eq!(r.as_array().unwrap().len(), 0, "another tenant's mail never leaks into search");
    }

    #[tokio::test]
    async fn addresses_tag_filter_and_edit_live() {
        let env = Env::new("addrs");
        env.add_with_addresses("daniel", "Daniel", "daniel", &[
            "me@example.com".into(),
            "dave@example.com".into(),
        ]);
        env.mail("daniel", "full.mbox", &format!("{THREAD_MBOX}\n{LUNCH_MBOX}"));
        let app = env.app();
        let cookie = cookie_of(&login(&app, "daniel", "daniel").await);
        scan(&app, &cookie).await;
        wait_indexed(&app, &cookie, 3).await;

        let list = threads(&app, &cookie, "/api/messages").await;
        let accts = list[0]["accts"].as_array().unwrap();
        assert_eq!(accts.len(), 2, "every fixture thread involves both declared identities");
        assert_eq!(accts[0], "me@example.com", "declaration order");
        let (_, fac) = get_json(&app, "/api/filters", Some(&cookie)).await;
        let counts = fac["addresses"].as_array().unwrap();
        assert_eq!(counts.len(), 2);
        assert_eq!(counts[0][1], 3, "me@example.com in all three messages");

        let both = threads(&app, &cookie, "/api/messages?acct=dave@example.com").await;
        assert_eq!(both.as_array().unwrap().len(), 2);
        let none = threads(&app, &cookie, "/api/messages?acct=zz@zz.zz").await;
        assert_eq!(none.as_array().unwrap().len(), 0);
        let s = threads(&app, &cookie, "/api/search?q=quarterly&acct=me@example.com").await;
        assert_eq!(s.as_array().unwrap().len(), 1);
        let s2 = threads(&app, &cookie, "/api/search?q=quarterly&acct=zz@zz.zz").await;
        assert_eq!(s2.as_array().unwrap().len(), 0);

        // live edit: lowercased, deduped, applied without rescan, epoch intact
        let r = send(
            &app,
            req(
                "POST",
                "/api/profile/addresses",
                Body::from(
                    json!({ "addresses": ["  Hajni@Gmail.com ", "hajni+work@gmail.com", "hajni@gmail.com"] })
                        .to_string(),
                ),
                Some(&cookie),
                true,
            ),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        let body = json_of(r).await;
        assert_eq!(
            body["addresses"],
            json!(["hajni@gmail.com", "hajni+work@gmail.com"])
        );
        let (st, _) = get_json(&app, "/api/whoami", Some(&cookie)).await;
        assert_eq!(st, StatusCode::OK, "address edit must not revoke sessions");
        let (_, prof) = get_json(&app, "/api/profile", Some(&cookie)).await;
        assert_eq!(prof["addresses"].as_array().unwrap().len(), 2);

        let list2 = threads(&app, &cookie, "/api/messages").await;
        assert!(list2[0].get("accts").is_none(), "no involved identity -> tag omitted, no rescan");
        let (_, fac2) = get_json(&app, "/api/filters", Some(&cookie)).await;
        assert_eq!(fac2["addresses"].as_array().unwrap().len(), 0);

        let bad = send(
            &app,
            req(
                "POST",
                "/api/profile/addresses",
                Body::from(json!({ "addresses": ["nope"] }).to_string()),
                Some(&cookie),
                true,
            ),
        )
        .await;
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
        let after = fs::read_to_string(env.profiles.join("daniel").join("profile.json")).unwrap();
        assert!(after.contains("hajni+work@gmail.com"), "rejected edit changed nothing");
    }

    #[tokio::test]
    async fn single_declared_address_stays_invisible() {
        let env = Env::new("oneaddr");
        env.add_with_addresses("daniel", "Daniel", "daniel", &["me@example.com".into()]);
        env.mail("daniel", "export.mbox", THREAD_MBOX);
        let app = env.app();
        let cookie = cookie_of(&login(&app, "daniel", "daniel").await);
        scan(&app, &cookie).await;
        wait_indexed(&app, &cookie, 2).await;
        let list = threads(&app, &cookie, "/api/messages").await;
        assert!(list[0].get("accts").is_none(), "one identity is not a distinction");
        let (_, fac) = get_json(&app, "/api/filters", Some(&cookie)).await;
        assert_eq!(fac["addresses"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn identity_search_folds_aliases_and_status_reports_search() {
        let env = Env::new("idsearch");
        env.add("daniel", "Daniel", "daniel");
        env.mail("daniel", "full.mbox", &format!("{THREAD_MBOX}\n{LUNCH_MBOX}"));
        let app = env.app();
        let cookie = cookie_of(&login(&app, "daniel", "daniel").await);
        scan(&app, &cookie).await;
        let st = wait_indexed(&app, &cookie, 3).await;
        assert_eq!(st["search"]["running"], false, "idle status exposes the search block");
        assert_eq!(st["search"]["processed"], 0);

        // to:me@example.com must find the lunch thread whose To literally says
        // me+inbox@example.com — the whole point of fold-aware identity terms
        let t = threads(&app, &cookie, "/api/search?q=to:me@example.com").await;
        let subjects: Vec<&str> = t.as_array().unwrap().iter().map(|x| x["subject"].as_str().unwrap()).collect();
        assert!(subjects.contains(&"Lunch plans"), "plus-alias recipient found: {subjects:?}");
        assert!(subjects.contains(&"Quarterly report"));

        // from: narrows to the sender pool only
        let t = threads(&app, &cookie, "/api/search?q=to:dave@example.com").await;
        assert_eq!(t.as_array().unwrap().len(), 1, "only the reply went to dave");
        assert_eq!(t[0]["subject"], "Quarterly report");

        // term + residual query: both constraints apply
        let t = threads(&app, &cookie, "/api/search?q=to:me@example.com%20subject:lunch").await;
        assert_eq!(t.as_array().unwrap().len(), 1);
        assert_eq!(t[0]["subject"], "Lunch plans");

        // unknown identity finds nothing
        let t = threads(&app, &cookie, "/api/search?q=to:nobody@nowhere.io").await;
        assert_eq!(t.as_array().unwrap().len(), 0);
    }
