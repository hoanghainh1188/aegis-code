//! Personal API tokens (#231): a person's own, named and ending, at most
//! ten live, revoked one by one; a service gets none this way.

use aegis_store::testing::test_db;
use aegis_store::{Actor, MAX_PERSONAL_TOKENS, StoreError, UserKind};

#[tokio::test]
async fn a_person_makes_names_and_revokes_tokens_of_their_own() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let lan = s.sign_in("sub-lan", "lan@example.com", true).await.unwrap();
    let actor = Actor::user(&lan, aegis_store::ActorKind::User);
    let hash = |n: u8| [n; 32];

    let made = s
        .create_personal_token(lan.id, " laptop ", 30, &hash(1), &actor)
        .await
        .unwrap();
    assert_eq!(made.label, "laptop");
    let days = (made.expires_at - made.created_at).num_days();
    assert_eq!(days, 30);
    let user = s.token_user(&hash(1)).await.unwrap().unwrap();
    assert_eq!((user.id, user.kind), (lan.id, UserKind::Person));
    assert_eq!(s.personal_tokens(lan.id).await.unwrap().len(), 1);

    for bad in [("", 30), ("x", 0), ("x", 91)] {
        let err = s
            .create_personal_token(lan.id, bad.0, bad.1, &hash(9), &actor)
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Invalid { .. }), "{bad:?}: {err}");
    }
    for n in 2..=u8::try_from(MAX_PERSONAL_TOKENS).unwrap() {
        s.create_personal_token(lan.id, "more", 1, &hash(n), &actor)
            .await
            .unwrap();
    }
    let err = s
        .create_personal_token(lan.id, "one too many", 1, &hash(99), &actor)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err}");

    // Someone else's id revokes nothing; the owner's does.
    let eve = s.sign_in("sub-eve", "eve@example.com", true).await.unwrap();
    assert!(
        !s.revoke_personal_token(eve.id, made.id, &actor)
            .await
            .unwrap()
    );
    assert!(
        s.revoke_personal_token(lan.id, made.id, &actor)
            .await
            .unwrap()
    );
    assert!(s.token_user(&hash(1)).await.unwrap().is_none());
    assert!(
        !s.revoke_personal_token(lan.id, made.id, &actor)
            .await
            .unwrap()
    );

    let service = s
        .create_token("jira-sync", &hash(100), &Actor::cli())
        .await
        .unwrap();
    let err = s
        .create_personal_token(service.id, "x", 1, &hash(101), &actor)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("for people"), "{err}");
    db.drop().await;
}
