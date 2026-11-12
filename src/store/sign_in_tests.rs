//! Store tests of user sign-in (JD-37 STEP-07): every account state against
//! every request, with a fixed clock and no real password hashing (the
//! store never hashes; the tests pass the verifier's answer in).

use secrecy::{ExposeSecret as _, SecretString};

use super::super::*;
use super::{Fixture, fixture};
use crate::auth::OsRandom;
use crate::password::Verified;

const OK: Verified = Verified {
    matches: true,
    needs_rehash: false,
};
const WRONG: Verified = Verified {
    matches: false,
    needs_rehash: false,
};

fn web() -> SignInRequest<'static> {
    SignInRequest {
        code: None,
        device: None,
        session_hours: 8,
    }
}

fn with_code<'a>(code: &'a str, device: Option<&'a str>) -> SignInRequest<'a> {
    SignInRequest {
        code: Some(code),
        device,
        session_hours: 8,
    }
}

fn now_code(fx: &Fixture, secret: &[u8]) -> String {
    crate::totp::code(secret, crate::totp::step_at(fx.clock.now().as_second()))
}

/// Signs `name` in with a correct password and `request`.
fn sign_in(fx: &mut Fixture, name: &str, request: &SignInRequest<'_>) -> Result<SignedIn, Error> {
    let begin = fx.store.sign_in_begin(name).unwrap();
    if let Some(locked) = begin.locked() {
        return Err(locked.clone().into());
    }
    fx.store
        .sign_in_finish(&begin, OK, None, request, &OsRandom)
}

/// A user who has set up TOTP and chosen a password; returns the TOTP
/// secret. The clock is moved past the setup code's step.
fn active_user(fx: &mut Fixture, name: &str) -> Vec<u8> {
    fx.store
        .create_user(name, "$argon2id$one-time", &OsRandom)
        .unwrap();
    let SignedIn::Setup { ticket, totp, .. } = sign_in(fx, name, &web()).unwrap() else {
        panic!("setup first");
    };
    let secret = crate::totp::base32_decode(totp.unwrap().secret.expose_secret()).unwrap();
    let code = now_code(fx, &secret);
    let done = fx
        .store
        .setup_finish(
            &ticket,
            "$argon2id$chosen",
            &with_code(&code, None),
            &OsRandom,
        )
        .unwrap();
    assert!(matches!(done, SignedIn::Session { device: None, .. }));
    fx.clock.advance(30);
    secret
}

fn refused(result: Result<SignedIn, Error>) -> AdminRefusal {
    match result {
        Err(Error::AdminRefused(refusal)) => refusal,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_new_user_sets_up_totp_then_signs_in_with_a_code_and_gets_a_device() {
    let mut fx = fixture();
    let secret = active_user(&mut fx, "alice");
    let row = fx.store.user("alice").unwrap();
    assert!(!row.must_change_password);
    assert!(row.totp_enrolled_at.is_some());
    assert_eq!(
        fx.store.sign_in_begin("alice").unwrap().phc(),
        Some("$argon2id$chosen")
    );
    let code = now_code(&fx, &secret);
    let SignedIn::Session {
        session, device, ..
    } = sign_in(&mut fx, "alice", &with_code(&code, Some("laptop"))).unwrap()
    else {
        panic!("a session");
    };
    let device = device.unwrap();
    assert_eq!(device.name, "laptop");
    assert!(device.credential.expose_secret().starts_with("jdd_"));
    assert!(session.expose_secret().starts_with("jdu_"));
    let checked = fx
        .store
        .user_api_session(&device.credential, &session, 30)
        .unwrap();
    assert_eq!(checked.user.name, "alice");
    // An API session is not a web session.
    assert!(fx.store.user_web_session(&session).is_err());
}

#[test]
fn a_password_alone_never_opens_a_session_or_registers_a_device() {
    let mut fx = fixture();
    fx.store
        .create_user("alice", "$argon2id$one-time", &OsRandom)
        .unwrap();
    // Waiting for setup: the one-time password only starts setup.
    for request in [web(), with_code("123456", Some("laptop"))] {
        assert!(matches!(
            sign_in(&mut fx, "alice", &request).unwrap(),
            SignedIn::Setup { totp: Some(_), .. }
        ));
    }
    assert_eq!(fx.store.users().unwrap()[0].devices, 0);
    // Set up: a password without a code is refused.
    active_user(&mut fx, "bob");
    for request in [
        web(),
        SignInRequest {
            device: Some("x"),
            ..web()
        },
    ] {
        assert_eq!(
            refused(sign_in(&mut fx, "bob", &request)),
            AdminRefusal::Unauthorized
        );
    }
}

#[test]
fn every_refusal_is_the_same_and_counts_against_the_name() {
    let mut fx = fixture();
    let secret = active_user(&mut fx, "alice");
    let dave = active_user(&mut fx, "dave");
    fx.store.disable_user("dave").unwrap();
    fx.store
        .create_user("erin", "$argon2id$x", &OsRandom)
        .unwrap();
    let code = now_code(&fx, &secret);
    let unknown = {
        let begin = fx.store.sign_in_begin("nobody").unwrap();
        assert_eq!(begin.phc(), None);
        assert!(begin.user().is_none());
        fx.store
            .sign_in_finish(&begin, OK, None, &with_code(&code, None), &OsRandom)
    };
    let wrong_password = {
        let begin = fx.store.sign_in_begin("alice").unwrap();
        fx.store
            .sign_in_finish(&begin, WRONG, None, &with_code(&code, None), &OsRandom)
    };
    let wrong_code = sign_in(&mut fx, "alice", &with_code("000000", None));
    let dave_code = now_code(&fx, &dave);
    let disabled = sign_in(&mut fx, "dave", &with_code(&dave_code, None));
    fx.clock.advance(73 * 3600);
    let lapsed = sign_in(&mut fx, "erin", &web());
    for result in [unknown, wrong_password, wrong_code, disabled, lapsed] {
        assert_eq!(refused(result), AdminRefusal::Unauthorized);
    }
}

#[test]
fn five_failures_lock_a_real_or_unknown_name_for_sign_in_only() {
    let mut fx = fixture();
    let secret = active_user(&mut fx, "alice");
    let code = now_code(&fx, &secret);
    let SignedIn::Session {
        session, device, ..
    } = sign_in(&mut fx, "alice", &with_code(&code, Some("laptop"))).unwrap()
    else {
        panic!("a session");
    };
    let device = device.unwrap();
    let web_code = {
        fx.clock.advance(30);
        now_code(&fx, &secret)
    };
    let SignedIn::Session {
        session: web_session,
        ..
    } = sign_in(&mut fx, "alice", &with_code(&web_code, None)).unwrap()
    else {
        panic!("a web session");
    };
    for name in ["alice", "nobody"] {
        for _ in 0..5 {
            let begin = fx.store.sign_in_begin(name).unwrap();
            let _ = fx
                .store
                .sign_in_finish(&begin, WRONG, None, &web(), &OsRandom);
        }
        let begin = fx.store.sign_in_begin(name).unwrap();
        assert!(
            matches!(begin.locked(), Some(AdminRefusal::Locked { retry_after, .. }) if *retry_after > 0),
            "{name}"
        );
        // Even the right password is refused while locked.
        assert!(matches!(
            refused(fx.store.sign_in_finish(&begin, OK, None, &web(), &OsRandom)),
            AdminRefusal::Locked { .. }
        ));
    }
    // Sessions and device renewals keep working.
    fx.store
        .user_api_session(&device.credential, &session, 30)
        .unwrap();
    fx.store.user_web_session(&web_session).unwrap();
    fx.clock.advance(30);
    let renewal = now_code(&fx, &secret);
    assert!(matches!(
        fx.store
            .device_session(&device.credential, &renewal, &web(), 30, &OsRandom)
            .unwrap(),
        SignedIn::Session { .. }
    ));
    fx.store.unlock_user("alice").unwrap();
    assert!(fx.store.sign_in_begin("alice").unwrap().locked().is_none());
}

#[test]
fn a_code_is_accepted_once_across_sign_in_renewal_and_password_change() {
    let mut fx = fixture();
    let secret = active_user(&mut fx, "alice");
    let code = now_code(&fx, &secret);
    let SignedIn::Session {
        session, device, ..
    } = sign_in(&mut fx, "alice", &with_code(&code, Some("laptop"))).unwrap()
    else {
        panic!("a session");
    };
    let device = device.unwrap();
    assert_eq!(
        refused(sign_in(&mut fx, "alice", &with_code(&code, None))),
        AdminRefusal::Unauthorized
    );
    assert_eq!(
        refused(
            fx.store
                .device_session(&device.credential, &code, &web(), 30, &OsRandom)
        ),
        AdminRefusal::Unauthorized
    );
    let keep = fx
        .store
        .user_api_session(&device.credential, &session, 30)
        .unwrap();
    let user = fx.store.user("alice").unwrap();
    assert!(matches!(
        fx.store
            .change_password(&user, &code, "$argon2id$chosen", "$argon2id$new", &keep),
        Err(Error::AdminRefused(AdminRefusal::Unauthorized))
    ));
}

#[test]
fn devices_lapse_after_unused_days_and_lock_after_five_wrong_codes() {
    let mut fx = fixture();
    let secret = active_user(&mut fx, "alice");
    let code = now_code(&fx, &secret);
    let SignedIn::Session { device, .. } =
        sign_in(&mut fx, "alice", &with_code(&code, Some("laptop"))).unwrap()
    else {
        panic!("a session");
    };
    let device = device.unwrap();
    for _ in 0..5 {
        let _ = fx
            .store
            .device_session(&device.credential, "000000", &web(), 30, &OsRandom);
    }
    fx.clock.advance(30);
    let code = now_code(&fx, &secret);
    assert!(matches!(
        refused(
            fx.store
                .device_session(&device.credential, &code, &web(), 30, &OsRandom)
        ),
        AdminRefusal::Locked { .. }
    ));
    // The name is not locked by the device's failures.
    assert!(fx.store.sign_in_begin("alice").unwrap().locked().is_none());
    fx.clock.advance(16 * 60);
    let code = now_code(&fx, &secret);
    fx.store
        .device_session(&device.credential, &code, &web(), 30, &OsRandom)
        .unwrap();
    fx.clock.advance(31 * 86_400);
    let code = now_code(&fx, &secret);
    assert_eq!(
        refused(
            fx.store
                .device_session(&device.credential, &code, &web(), 30, &OsRandom)
        ),
        AdminRefusal::DeviceInvalid
    );
    let devices = fx
        .store
        .devices_of(&fx.store.user("alice").unwrap())
        .unwrap();
    assert!(
        devices[0].revoked_at.is_some(),
        "a lapsed device is revoked"
    );
}

#[test]
fn resets_and_disabling_end_sessions_and_devices_and_a_disable_between_phases_wins() {
    let mut fx = fixture();
    let secret = active_user(&mut fx, "alice");
    let code = now_code(&fx, &secret);
    let SignedIn::Session {
        session, device, ..
    } = sign_in(&mut fx, "alice", &with_code(&code, Some("laptop"))).unwrap()
    else {
        panic!("a session");
    };
    let device = device.unwrap();
    let old_phc = fx
        .store
        .sign_in_begin("alice")
        .unwrap()
        .phc()
        .map(str::to_owned);
    fx.store.reset_totp("alice", "$argon2id$reset").unwrap();
    assert!(
        fx.store
            .user_api_session(&device.credential, &session, 30)
            .is_err()
    );
    let begin = fx.store.sign_in_begin("alice").unwrap();
    assert_ne!(
        begin.phc().map(str::to_owned),
        old_phc,
        "the old password opens nothing"
    );
    // The next sign-in sets up a new authenticator.
    assert!(matches!(
        fx.store
            .sign_in_finish(&begin, OK, None, &web(), &OsRandom)
            .unwrap(),
        SignedIn::Setup { totp: Some(_), .. }
    ));

    let secret = active_user(&mut fx, "bob");
    let begin = fx.store.sign_in_begin("bob").unwrap();
    fx.store.disable_user("bob").unwrap();
    let code = now_code(&fx, &secret);
    assert_eq!(
        refused(
            fx.store
                .sign_in_finish(&begin, OK, None, &with_code(&code, None), &OsRandom)
        ),
        AdminRefusal::Unauthorized
    );
}

#[test]
fn a_reset_password_keeps_the_authenticator_and_asks_only_for_a_new_password() {
    let mut fx = fixture();
    let secret = active_user(&mut fx, "alice");
    fx.store.reset_password("alice", "$argon2id$temp").unwrap();
    // Without a code, nothing.
    assert_eq!(
        refused(sign_in(&mut fx, "alice", &web())),
        AdminRefusal::Unauthorized
    );
    let code = now_code(&fx, &secret);
    let SignedIn::Setup { ticket, totp, .. } =
        sign_in(&mut fx, "alice", &with_code(&code, None)).unwrap()
    else {
        panic!("a password change is due");
    };
    assert!(totp.is_none());
    assert!(!fx.store.setup_begin(&ticket).unwrap().totp);
    let done = fx
        .store
        .setup_finish(
            &ticket,
            "$argon2id$mine",
            &SignInRequest {
                device: Some("pc"),
                ..web()
            },
            &OsRandom,
        )
        .unwrap();
    assert!(matches!(
        done,
        SignedIn::Session {
            device: Some(_),
            ..
        }
    ));
    // The ticket is used up.
    assert!(fx.store.setup_begin(&ticket).is_err());
}

#[test]
fn setup_tickets_lapse_after_ten_minutes_and_a_wrong_code_confirms_nothing() {
    let mut fx = fixture();
    fx.store
        .create_user("alice", "$argon2id$one-time", &OsRandom)
        .unwrap();
    let SignedIn::Setup { ticket, totp, .. } = sign_in(&mut fx, "alice", &web()).unwrap() else {
        panic!("setup");
    };
    let secret = crate::totp::base32_decode(totp.unwrap().secret.expose_secret()).unwrap();
    assert_eq!(
        refused(fx.store.setup_finish(
            &ticket,
            "$argon2id$x",
            &with_code("000000", None),
            &OsRandom
        )),
        AdminRefusal::Unauthorized
    );
    assert!(fx.store.user("alice").unwrap().totp_enrolled_at.is_none());
    fx.clock.advance(11 * 60);
    let code = now_code(&fx, &secret);
    assert_eq!(
        refused(
            fx.store
                .setup_finish(&ticket, "$argon2id$x", &with_code(&code, None), &OsRandom)
        ),
        AdminRefusal::Unauthorized
    );
    assert!(
        fx.store
            .setup_begin(&SecretString::from("jdt_nonsense".to_owned()))
            .is_err()
    );
}

#[test]
fn a_rehash_replaces_the_stored_hash_on_a_successful_sign_in_only() {
    let mut fx = fixture();
    let secret = active_user(&mut fx, "alice");
    let begin = fx.store.sign_in_begin("alice").unwrap();
    let _ = fx.store.sign_in_finish(
        &begin,
        OK,
        Some("$argon2id$wrongcode"),
        &with_code("000000", None),
        &OsRandom,
    );
    assert_eq!(
        fx.store.sign_in_begin("alice").unwrap().phc(),
        Some("$argon2id$chosen")
    );
    let code = now_code(&fx, &secret);
    let begin = fx.store.sign_in_begin("alice").unwrap();
    fx.store
        .sign_in_finish(
            &begin,
            OK,
            Some("$argon2id$stronger"),
            &with_code(&code, None),
            &OsRandom,
        )
        .unwrap();
    assert_eq!(
        fx.store.sign_in_begin("alice").unwrap().phc(),
        Some("$argon2id$stronger")
    );
}

#[test]
fn a_password_change_keeps_the_session_making_it_and_ends_the_others() {
    let mut fx = fixture();
    let secret = active_user(&mut fx, "alice");
    let mut sessions = Vec::new();
    for device in ["laptop", "desktop"] {
        let code = now_code(&fx, &secret);
        let SignedIn::Session {
            session, device, ..
        } = sign_in(&mut fx, "alice", &with_code(&code, Some(device))).unwrap()
        else {
            panic!("a session");
        };
        sessions.push((device.unwrap(), session));
        fx.clock.advance(30);
    }
    let (laptop, laptop_session) = &sessions[0];
    let (desktop, desktop_session) = &sessions[1];
    let keep = fx
        .store
        .user_api_session(&laptop.credential, laptop_session, 30)
        .unwrap();
    let user = fx.store.user("alice").unwrap();
    let code = now_code(&fx, &secret);
    fx.store
        .change_password(&user, &code, "$argon2id$chosen", "$argon2id$new", &keep)
        .unwrap();
    fx.store
        .user_api_session(&laptop.credential, laptop_session, 30)
        .unwrap();
    assert_eq!(
        fx.store
            .user_api_session(&desktop.credential, desktop_session, 30)
            .unwrap_err()
            .to_string(),
        AdminRefusal::DeviceInvalid.to_string()
    );
    let user = fx.store.user("alice").unwrap();
    let revoked = fx.store.revoke_device_of(&user, "laptop").unwrap();
    assert!(revoked.revoked_at.is_some());
    assert!(fx.store.revoke_device_of(&user, "laptop").is_err());
    let ended = fx.store.end_user_session(laptop_session, false).unwrap();
    assert_eq!(ended.name, "alice");
}

/// Rows in `table`, to show a refused call wrote nothing.
fn rows(fx: &Fixture, table: &str) -> i64 {
    fx.store
        .conn
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[derive(Debug, Clone, Copy)]
enum Meanwhile {
    ResetPassword,
    ResetTotp,
    PasswordChange,
}

#[test]
fn a_sign_in_whose_password_changed_while_it_was_checked_is_refused_and_issues_nothing() {
    for meanwhile in [
        Meanwhile::ResetPassword,
        Meanwhile::ResetTotp,
        Meanwhile::PasswordChange,
    ] {
        let mut fx = fixture();
        let secret = active_user(&mut fx, "alice");
        let code = now_code(&fx, &secret);
        let SignedIn::Session { session, .. } =
            sign_in(&mut fx, "alice", &with_code(&code, None)).unwrap()
        else {
            panic!("a web session");
        };
        fx.clock.advance(30);
        // Phase 1 reads the chosen password; then, before phase 3, it
        // changes.
        let begin = fx.store.sign_in_begin("alice").unwrap();
        match meanwhile {
            Meanwhile::ResetPassword => {
                fx.store.reset_password("alice", "$argon2id$reset").unwrap();
            }
            Meanwhile::ResetTotp => {
                fx.store.reset_totp("alice", "$argon2id$reset").unwrap();
            }
            Meanwhile::PasswordChange => {
                let keep = fx.store.user_web_session(&session).unwrap();
                let user = fx.store.user("alice").unwrap();
                let code = now_code(&fx, &secret);
                fx.store
                    .change_password(&user, &code, "$argon2id$chosen", "$argon2id$new", &keep)
                    .unwrap();
                fx.clock.advance(30);
            }
        }
        let stored = fx
            .store
            .sign_in_begin("alice")
            .unwrap()
            .phc()
            .map(str::to_owned);
        let (sessions, devices) = (rows(&fx, "user_sessions"), rows(&fx, "devices"));
        let code = now_code(&fx, &secret);
        for request in [web(), with_code(&code, Some("laptop"))] {
            let result =
                fx.store
                    .sign_in_finish(&begin, OK, Some("$argon2id$rehash"), &request, &OsRandom);
            assert_eq!(refused(result), AdminRefusal::Unauthorized, "{meanwhile:?}");
        }
        assert_eq!(
            fx.store
                .sign_in_begin("alice")
                .unwrap()
                .phc()
                .map(str::to_owned),
            stored,
            "{meanwhile:?}: no rehash"
        );
        assert_eq!(rows(&fx, "user_sessions"), sessions, "{meanwhile:?}");
        assert_eq!(rows(&fx, "devices"), devices, "{meanwhile:?}");
        let tickets: i64 = fx
            .store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM users WHERE setup_ticket_sha256 IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tickets, 0, "{meanwhile:?}: no setup ticket");
    }
}

#[test]
fn a_password_change_checked_against_an_old_password_is_refused() {
    let mut fx = fixture();
    let secret = active_user(&mut fx, "alice");
    let code = now_code(&fx, &secret);
    let SignedIn::Session { session, .. } =
        sign_in(&mut fx, "alice", &with_code(&code, None)).unwrap()
    else {
        panic!("a web session");
    };
    fx.clock.advance(30);
    let keep = fx.store.user_web_session(&session).unwrap();
    let user = fx.store.user("alice").unwrap();
    let code = now_code(&fx, &secret);
    assert!(matches!(
        fx.store
            .change_password(&user, &code, "$argon2id$older", "$argon2id$new", &keep),
        Err(Error::AdminRefused(AdminRefusal::Unauthorized))
    ));
    assert_eq!(
        fx.store.sign_in_begin("alice").unwrap().phc(),
        Some("$argon2id$chosen")
    );
    fx.store.user_web_session(&session).unwrap();
}
