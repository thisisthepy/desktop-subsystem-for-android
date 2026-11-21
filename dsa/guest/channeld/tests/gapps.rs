//! The Google apps' image an older host left on the user's disk, which the host reads and then
//! removes through the router as it reaches it, with a scratch folder standing in for the
//! user's disk.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;

use dsa_guest_channeld::gapps::{Gapps, GuestFacts, IMAGE};
use dsa_guest_channeld::router::Router;
use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{
    AddonImage, AddonImagePart, AddonImageRead, Envelope, GappsQuery, GappsRemove, GappsState,
    GoogleDeviceId,
};

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("dsa-channeld-gapps-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn router(dir: &Path) -> (Router, mpsc::Receiver<Vec<u8>>) {
    let (to_host, from_guest) = mpsc::channel();
    let facts = GuestFacts {
        active: Box::new(|| false),
        sdk: Box::new(|| 36),
        abi: Box::new(|| "x86_64".to_string()),
        abilist: Box::new(|| "x86_64,arm64-v8a".to_string()),
    };
    let router = Router::new(to_host, |_| Err("not the agent".to_string()))
        .on_gapps(Arc::new(Gapps::new(dir.to_path_buf(), facts)));
    (router, from_guest)
}

fn send(router: &Router, body: Body) {
    router.from_host(&dsa_protocol::encode(&Envelope { body: Some(body) }));
}

fn state(from_guest: &mpsc::Receiver<Vec<u8>>) -> GappsState {
    match dsa_protocol::decode(&from_guest.try_recv().unwrap())
        .unwrap()
        .body
    {
        Some(Body::GappsState(state)) => state,
        other => panic!("expected GappsState, got {other:?}"),
    }
}

fn part(from_guest: &mpsc::Receiver<Vec<u8>>) -> AddonImagePart {
    match dsa_protocol::decode(&from_guest.try_recv().unwrap())
        .unwrap()
        .body
    {
        Some(Body::AddonImagePart(part)) => part,
        other => panic!("expected AddonImagePart, got {other:?}"),
    }
}

#[test]
fn fr32_an_image_left_on_data_is_reported_read_and_removed() {
    let dir = scratch("whole");
    let (router, from_guest) = router(&dir);
    send(&router, Body::GappsQuery(GappsQuery {}));
    let before = state(&from_guest);
    assert!(!before.installed);
    assert_eq!(before.sdk, 36);
    assert_eq!(before.abi, "x86_64");
    assert_eq!(before.abilist, "x86_64,arm64-v8a");

    // What an older host sent.
    let image: Vec<u8> = (0..10_000u32).map(|i| i as u8).collect();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(IMAGE), &image).unwrap();
    send(&router, Body::GappsQuery(GappsQuery {}));
    assert!(state(&from_guest).installed);

    let mut read = Vec::new();
    loop {
        send(
            &router,
            Body::AddonImageRead(AddonImageRead {
                image: AddonImage::Gapps as i32,
                offset: read.len() as u64,
                length: 4096,
            }),
        );
        let part = part(&from_guest);
        assert!(part.error.is_empty(), "{}", part.error);
        assert_eq!(part.image(), AddonImage::Gapps);
        assert_eq!(part.size, image.len() as u64);
        if part.data.is_empty() {
            break;
        }
        read.extend(part.data);
    }
    assert_eq!(read, image);

    send(&router, Body::GappsRemove(GappsRemove {}));
    let removed = state(&from_guest);
    assert!(removed.done, "{}", removed.error);
    assert!(!removed.installed);
    assert!(!removed.active, "a mounted image stays until the next boot");
    assert!(!dir.join(IMAGE).exists());

    // A read of an image that is not there says so.
    send(
        &router,
        Body::AddonImageRead(AddonImageRead {
            image: AddonImage::Gapps as i32,
            offset: 0,
            length: 4096,
        }),
    );
    let gone = part(&from_guest);
    assert!(!gone.error.is_empty());
    assert_eq!(gone.size, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fr32_a_guest_without_the_translator_store_says_it_has_no_such_image() {
    let dir = scratch("other");
    let (router, from_guest) = router(&dir);
    send(
        &router,
        Body::AddonImageRead(AddonImageRead {
            image: AddonImage::ArmTranslation as i32,
            offset: 0,
            length: 16,
        }),
    );
    let answer = part(&from_guest);
    assert_eq!(answer.image(), AddonImage::ArmTranslation);
    assert!(
        answer.error.contains("no such add-on image"),
        "{}",
        answer.error
    );
}

#[test]
fn fr32_only_the_agent_says_the_device_id_and_no_client_says_where_the_apps_stand() {
    let dir = scratch("clients");
    let (router, _from_guest) = router(&dir);
    let peer = dsa_guest_channeld::router::Peer(1234);
    let state = Body::GappsState(GappsState::default());
    assert!(router
        .from_guest(peer, dsa_protocol::encode(&Envelope { body: Some(state) }))
        .is_err());
    let part = Body::AddonImagePart(AddonImagePart::default());
    assert!(router
        .from_guest(peer, dsa_protocol::encode(&Envelope { body: Some(part) }))
        .is_err());
    let id = Body::GoogleDeviceId(GoogleDeviceId { id: "1".into() });
    assert!(router
        .from_guest(peer, dsa_protocol::encode(&Envelope { body: Some(id) }))
        .is_err());
}

fn agent_router(
    read: impl Fn() -> Option<String> + Send + Sync + 'static,
) -> (Router, mpsc::Receiver<Vec<u8>>) {
    let (to_host, from_guest) = mpsc::channel();
    let router = Router::new(to_host, |_| Ok(())).on_google_device_id(read);
    (router, from_guest)
}

fn device_id_sent_to_host(from_guest: &mpsc::Receiver<Vec<u8>>) -> String {
    match dsa_protocol::decode(&from_guest.try_recv().unwrap())
        .unwrap()
        .body
    {
        Some(Body::GoogleDeviceId(id)) => id.id,
        other => panic!("expected GoogleDeviceId, got {other:?}"),
    }
}

fn agent_says_device_id(router: &Router, id: &str) {
    let answer = Body::GoogleDeviceId(GoogleDeviceId { id: id.into() });
    router
        .from_guest(
            dsa_guest_channeld::router::Peer(1),
            dsa_protocol::encode(&Envelope { body: Some(answer) }),
        )
        .unwrap();
}

#[test]
fn fr32_an_empty_device_id_from_the_agent_is_filled_in_from_googles_own_files() {
    let (router, from_guest) = agent_router(|| Some("4562097385933913176".to_string()));
    agent_says_device_id(&router, "");
    assert_eq!(device_id_sent_to_host(&from_guest), "4562097385933913176");
}

#[test]
fn fr32_a_device_id_the_agent_read_is_passed_on_as_it_is() {
    let (router, from_guest) = agent_router(|| Some("999".to_string()));
    agent_says_device_id(&router, "123");
    assert_eq!(device_id_sent_to_host(&from_guest), "123");
}

#[test]
fn fr32_with_no_id_anywhere_the_host_still_hears_the_empty_answer() {
    let (router, from_guest) = agent_router(|| None);
    agent_says_device_id(&router, "");
    assert_eq!(device_id_sent_to_host(&from_guest), "");
}
