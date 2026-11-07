//! The Google apps' image the host installs in the guest, through the router as the host
//! reaches it, with a scratch folder standing in for the user's disk.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;

use dsa_guest_channeld::gapps::{Gapps, GuestFacts, IMAGE};
use dsa_guest_channeld::router::Router;
use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{
    Envelope, GappsChunk, GappsInstall, GappsQuery, GappsRemove, GappsState, GoogleDeviceId,
};
use sha2::{Digest, Sha256};

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

fn answers(from_guest: &mpsc::Receiver<Vec<u8>>) -> Vec<GappsState> {
    std::iter::from_fn(|| from_guest.try_recv().ok())
        .map(|frame| match dsa_protocol::decode(&frame).unwrap().body {
            Some(Body::GappsState(state)) => state,
            other => panic!("expected GappsState, got {other:?}"),
        })
        .collect()
}

fn install(router: &Router, id: u32, image: &[u8], sha256: Vec<u8>) {
    send(
        router,
        Body::GappsInstall(GappsInstall {
            transfer_id: id,
            size: image.len() as u64,
            sha256,
        }),
    );
    for (index, data) in image.chunks(4096).enumerate() {
        send(
            router,
            Body::GappsChunk(GappsChunk {
                transfer_id: id,
                offset: (index * 4096) as u64,
                data: data.to_vec(),
            }),
        );
    }
}

#[test]
fn fr32_the_image_is_kept_reported_and_removed() {
    let dir = scratch("whole");
    let (router, from_guest) = router(&dir);
    send(&router, Body::GappsQuery(GappsQuery {}));
    let before = answers(&from_guest).remove(0);
    assert!(!before.installed);
    assert_eq!(before.sdk, 36);
    assert_eq!(before.abi, "x86_64");
    assert_eq!(before.abilist, "x86_64,arm64-v8a");

    let image: Vec<u8> = (0..10_000u32).map(|i| i as u8).collect();
    install(&router, 9, &image, Sha256::digest(&image).to_vec());
    let last = answers(&from_guest).pop().unwrap();
    assert!(last.done, "{}", last.error);
    assert!(last.installed);
    assert!(!last.active, "the image is used from the next boot");
    assert_eq!(std::fs::read(dir.join(IMAGE)).unwrap(), image);

    send(&router, Body::GappsRemove(GappsRemove {}));
    let removed = answers(&from_guest).remove(0);
    assert!(removed.done);
    assert!(!removed.installed);
    assert!(!dir.join(IMAGE).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fr32_an_image_that_does_not_arrive_intact_leaves_the_old_one() {
    let dir = scratch("digest");
    let (router, from_guest) = router(&dir);
    let old = b"old image".to_vec();
    install(&router, 1, &old, Sha256::digest(&old).to_vec());
    let _ = answers(&from_guest);
    let new = b"new image, damaged on the way".to_vec();
    install(&router, 2, &new, Sha256::digest(b"something else").to_vec());
    let last = answers(&from_guest).pop().unwrap();
    assert!(!last.done);
    assert!(last.error.contains("SHA-256"), "{}", last.error);
    assert_eq!(std::fs::read(dir.join(IMAGE)).unwrap(), old);
    let _ = std::fs::remove_dir_all(&dir);
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
    let id = Body::GoogleDeviceId(GoogleDeviceId { id: "1".into() });
    assert!(router
        .from_guest(peer, dsa_protocol::encode(&Envelope { body: Some(id) }))
        .is_err());
}
