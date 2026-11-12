//! The host's root certificates in Android's system certificate folders, with scratch folders
//! standing in for the tmpfs folders init binds over Android's, and the router carrying the
//! host's message to the installer as on the guest.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use dsa_guest_channeld::cacerts::{
    certificate_of, pem, setup, subject_der, subject_hash_old, Installer, MANIFEST,
};
use dsa_guest_channeld::router::{Delivery, Listener, Peer, Router};
use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{Envelope, HostCertificates};

/// A root made with OpenSSL for these tests ("C=US, O=DSA Test, CN=DSA Test Root").
/// `openssl x509 -subject_hash_old` says 7adeea37.
const TEST_ROOT: &str = "308201c53082016ba0030201020214654e01a5e825634d96c53bcc6ae8639711abe960300a06082a8648ce3d0403023038310b30090603550406130255533111300f060355040a0c0844534120546573743116301406035504030c0d445341205465737420526f6f74301e170d3236313131303233313135335a170d3336313130373233313135335a3038310b30090603550406130255533111300f060355040a0c0844534120546573743116301406035504030c0d445341205465737420526f6f743059301306072a8648ce3d020106082a8648ce3d0301070342000476fd783ab17bbb95b7b37bebdd4550a280a599b5bdf50d57de7d77f51bd63caf6b6834db798a7c67738fe1734b65b3f37984a513323713765cd75a6714210f20a3533051301d0603551d0e04160414186a6db6ddc20e83693310fc48373d96ca315683301f0603551d23041830168014186a6db6ddc20e83693310fc48373d96ca315683300f0603551d130101ff040530030101ff300a06082a8648ce3d040302034800304502204b3a928b51675d39146381315d23ab0996e6f07e18e17270c615e07f37273395022100ceb52db6ccef71e54626f6028a414c20977828afd322cef318cedd154de3fc8a";

/// Another root of the same subject, with a key of its own: the same hash, another file.
const SAME_SUBJECT: &str = "308201c53082016ba003020102021459910e2b2348e637d175e405aab3f376bee90a31300a06082a8648ce3d0403023038310b30090603550406130255533111300f060355040a0c0844534120546573743116301406035504030c0d445341205465737420526f6f74301e170d3236313131303233313230315a170d3336313130373233313230315a3038310b30090603550406130255533111300f060355040a0c0844534120546573743116301406035504030c0d445341205465737420526f6f743059301306072a8648ce3d020106082a8648ce3d030107034200049ada36a4c052a3e2565bf1e9974de8f9e0319b0b57699dcc48d3247588df031bf8b686a1550737891e4c83ca47233f9e761f9afd897a4af911f8ea3a98829168a3533051301d0603551d0e04160414ccaeb509d3b183f811b940a4f5ac1c5197e7b810301f0603551d23041830168014ccaeb509d3b183f811b940a4f5ac1c5197e7b810300f0603551d130101ff040530030101ff300a06082a8648ce3d0403020348003045022100bec39865157b9fd30bd802b15ee706875626fc85f6047147635845a9e09745f9022020614084e6a68a949398b30418c185916e7c82c5443767c963583c9051452c9f";

const HASH: &str = "7adeea37";

fn der(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dsa-channeld-cacerts-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    for folder in ["conscrypt", "system"] {
        std::fs::create_dir_all(dir.join(folder)).unwrap();
    }
    dir
}

fn installer(root: &Path) -> Installer {
    Installer::new(root, vec!["conscrypt".into(), "system".into()])
}

fn names(dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect()
}

/// One of Android's own certificate files: PEM followed by OpenSSL's text, as Android's are.
fn android_file(der: &[u8]) -> String {
    format!(
        "{}Certificate:\n    Data:\n        Version: 3 (0x2)\n",
        pem(der)
    )
}

#[test]
fn fr28_host_certificate_files_are_named_by_subject_hash() {
    let root = der(TEST_ROOT);
    assert_eq!(
        subject_der(&root).unwrap(),
        &root[0x89..0xc3],
        "the subject is the name after the validity"
    );
    assert_eq!(subject_hash_old(&root).unwrap(), HASH);
    assert_eq!(subject_hash_old(&der(SAME_SUBJECT)).unwrap(), HASH);

    let dir = scratch("named");
    let report = installer(&dir).install(std::slice::from_ref(&root));
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    for folder in ["conscrypt", "system"] {
        let file = dir.join(folder).join(format!("{HASH}.0"));
        let text = std::fs::read(&file).unwrap();
        assert!(text.starts_with(b"-----BEGIN CERTIFICATE-----\n"));
        assert_eq!(certificate_of(&text), Some(root.clone()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o644, "every app reads it");
        }
    }
    assert_eq!(
        names(&dir.join("conscrypt")),
        BTreeSet::from([format!("{HASH}.0")]),
        "no partial file is left behind"
    );
    let manifest = std::fs::read_to_string(dir.join(MANIFEST)).unwrap();
    assert_eq!(manifest, format!("conscrypt/{HASH}.0\nsystem/{HASH}.0\n"));
}

#[test]
fn fr28_a_host_certificate_takes_the_next_number_after_androids_own() {
    let dir = scratch("collision");
    let android = der(SAME_SUBJECT);
    for folder in ["conscrypt", "system"] {
        std::fs::write(
            dir.join(folder).join(format!("{HASH}.0")),
            android_file(&android),
        )
        .unwrap();
    }
    let report = installer(&dir).install(&[der(TEST_ROOT)]);
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    let file = dir.join("conscrypt").join(format!("{HASH}.1"));
    assert_eq!(
        certificate_of(&std::fs::read(file).unwrap()),
        Some(der(TEST_ROOT))
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("conscrypt").join(format!("{HASH}.0"))).unwrap(),
        android_file(&android),
        "Android's own file is untouched"
    );
}

#[test]
fn fr28_a_certificate_android_already_has_is_not_added_again() {
    let dir = scratch("identical");
    std::fs::write(
        dir.join("conscrypt").join(format!("{HASH}.0")),
        android_file(&der(TEST_ROOT)),
    )
    .unwrap();
    let report = installer(&dir).install(&[der(TEST_ROOT), der(TEST_ROOT)]);
    assert_eq!(report.already_trusted, 1);
    assert_eq!(
        names(&dir.join("conscrypt")),
        BTreeSet::from([format!("{HASH}.0")])
    );
    assert_eq!(
        names(&dir.join("system")),
        BTreeSet::from([format!("{HASH}.0")]),
        "a folder without it gets one copy"
    );
}

#[test]
fn fr28_certificates_the_host_no_longer_names_are_taken_away_and_androids_stay() {
    let dir = scratch("removal");
    let android_cert = "5ad8a5d6.0";
    for folder in ["conscrypt", "system"] {
        std::fs::write(dir.join(folder).join(android_cert), "Android's own").unwrap();
    }
    let installer = installer(&dir);
    installer.install(&[der(TEST_ROOT), der(SAME_SUBJECT)]);
    assert_eq!(
        names(&dir.join("conscrypt")),
        BTreeSet::from([
            android_cert.into(),
            format!("{HASH}.0"),
            format!("{HASH}.1")
        ])
    );

    // The second is taken away; the first keeps its file.
    let report = installer.install(&[der(TEST_ROOT)]);
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert_eq!(report.kept.len(), 2);
    assert_eq!(
        names(&dir.join("conscrypt")),
        BTreeSet::from([android_cert.into(), format!("{HASH}.0")])
    );

    // Off: every one of the host's goes, and Android's stay.
    let report = installer.install(&[]);
    assert_eq!(report.removed.len(), 2);
    for folder in ["conscrypt", "system"] {
        assert_eq!(
            names(&dir.join(folder)),
            BTreeSet::from([android_cert.to_string()])
        );
        assert_eq!(
            std::fs::read_to_string(dir.join(folder).join(android_cert)).unwrap(),
            "Android's own"
        );
    }
    assert_eq!(std::fs::read_to_string(dir.join(MANIFEST)).unwrap(), "");
}

#[test]
fn fr28_a_certificate_that_is_not_x509_is_left_out() {
    let dir = scratch("malformed");
    let report = installer(&dir).install(&[vec![0x30, 0x03, 0x02, 0x01, 0x00], der(TEST_ROOT)]);
    assert_eq!(report.problems.len(), 1, "{:?}", report.problems);
    assert_eq!(
        names(&dir.join("conscrypt")),
        BTreeSet::from([format!("{HASH}.0")])
    );
}

#[test]
fn fr28_setup_copies_androids_certificates_into_the_folders_init_binds() {
    let dir = scratch("setup");
    let root = dir.join("dsa_cacerts");
    std::fs::create_dir_all(&root).unwrap();
    let android = dir.join("apex-cacerts");
    std::fs::create_dir_all(&android).unwrap();
    std::fs::write(android.join("5ad8a5d6.0"), "Android's own").unwrap();
    let android = android.to_string_lossy().into_owned();
    let missing = dir.join("no-such-folder").to_string_lossy().into_owned();
    let problems = setup(
        &root,
        &[
            ("conscrypt", android.as_str()),
            ("system", missing.as_str()),
        ],
    );
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(
        std::fs::read_to_string(root.join("conscrypt").join("5ad8a5d6.0")).unwrap(),
        "Android's own"
    );
    assert_eq!(
        names(&root),
        BTreeSet::from(["conscrypt".to_string()]),
        "a folder that could not be copied is not there, so init does not bind it over Android's"
    );
}

/// The agent, which looks at the folders each time it hears of the host's certificates.
struct Agent {
    folder: PathBuf,
    saw: Mutex<Vec<BTreeSet<String>>>,
}

impl Listener for Agent {
    fn deliver(&self, frame: &[u8]) -> Delivery {
        if let Ok(Envelope {
            body: Some(Body::HostCertificates(_)),
        }) = dsa_protocol::decode(frame)
        {
            self.saw.lock().unwrap().push(names(&self.folder));
        }
        Delivery::Delivered
    }
}

#[test]
fn fr28_android_trusts_the_root_certificates_the_host_added() {
    let dir = scratch("router");
    std::fs::write(dir.join("conscrypt").join("5ad8a5d6.0"), "Android's own").unwrap();
    let (to_host, _host) = mpsc::channel();
    let installer = installer(&dir);
    let router = Router::new(to_host, |_| Ok(())).on_host_certificates(move |certificates| {
        let wanted = if certificates.enabled {
            certificates.certificates.clone()
        } else {
            Vec::new()
        };
        installer.install(&wanted);
    });
    let agent = Arc::new(Agent {
        folder: dir.join("conscrypt"),
        saw: Mutex::new(Vec::new()),
    });
    router.subscribe(Peer(1200), agent.clone());
    let send = |enabled: bool, certificates: Vec<Vec<u8>>| {
        router.from_host(&dsa_protocol::encode(&Envelope {
            body: Some(Body::HostCertificates(HostCertificates {
                enabled,
                certificates,
            })),
        }))
    };

    send(true, vec![der(TEST_ROOT)]);
    send(false, Vec::new());

    let android = "5ad8a5d6.0".to_string();
    assert_eq!(
        *agent.saw.lock().unwrap(),
        vec![
            BTreeSet::from([android.clone(), format!("{HASH}.0")]),
            BTreeSet::from([android]),
        ],
        "the agent hears once Android's folders hold what the host said"
    );
}
