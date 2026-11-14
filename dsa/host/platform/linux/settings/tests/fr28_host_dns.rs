//! The DNS row of the network page in DSA Settings for Linux, without a display: who answers
//! Android's names, as the service reports it.

use dsa_settings_linux::model::{DnsResolver, Settings};
use dsa_settings_linux::view;
use serde_json::json;

fn settings_with(network: serde_json::Value) -> Settings {
    Settings::read(
        &json!({ "render_gpu": "display", "advanced_networking": false, "network": network }),
    )
}

#[test]
fn fr28_the_network_page_says_this_computers_resolver_answers_androids_names() {
    let settings = settings_with(json!({
        "mode": "nat", "addresses": ["10.0.2.15"], "dns_servers": ["10.0.2.3"],
        "gateway": "10.0.2.2", "problem": null, "dns_resolver": "host"
    }));
    let network = settings.network.as_ref().unwrap();
    assert_eq!(network.dns_resolver, DnsResolver::Host);
    assert_eq!(
        view::network_dns_text(Some(network)),
        "Answered by this computer's resolver, so its hosts file, VPN and DNS filters apply to Android apps."
    );
}

#[test]
fn fr28_the_network_page_says_when_android_asks_the_networks_dns_servers_instead() {
    let settings = settings_with(json!({
        "mode": "nat", "addresses": ["10.0.2.15"], "dns_servers": ["10.0.2.3", "fd00::3"],
        "gateway": "10.0.2.2", "problem": null, "dns_resolver": "network"
    }));
    let network = settings.network.as_ref().unwrap();
    assert_eq!(network.dns_resolver, DnsResolver::Network);
    assert_eq!(
        view::network_dns_text(Some(network)),
        "This computer's resolver does not answer, so Android asks the network's DNS servers: \
         10.0.2.3, fd00::3"
    );
}

#[test]
fn fr28_a_service_that_does_not_say_who_answers_names_shows_the_dns_servers() {
    let settings = settings_with(json!({
        "mode": "nat", "addresses": ["10.0.2.15"], "dns_servers": ["10.0.2.3"],
        "gateway": "10.0.2.2", "problem": null
    }));
    let network = settings.network.as_ref().unwrap();
    assert_eq!(network.dns_resolver, DnsResolver::Unknown);
    assert_eq!(view::network_dns_text(Some(network)), "10.0.2.3");
    assert_eq!(view::network_dns_text(None), "None");
}

#[test]
fn fr28_the_dns_texts_are_translated() {
    let po = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/po/ko.po")).unwrap();
    for text in [
        "DNS",
        "Answered by this computer's resolver, so its hosts file, VPN and DNS filters apply to Android apps.",
        "This computer's resolver does not answer, so Android asks the network's DNS servers: {0}",
    ] {
        assert!(po.contains(&format!("msgid \"{text}\"")), "ko.po lacks {text}");
    }
}
