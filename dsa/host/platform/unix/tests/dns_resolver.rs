//! The system's resolver answering the guest's DNS queries, through the host's DNS service.
#![cfg(unix)]

use std::sync::Arc;

use dsa_host_core::dns::wire::{self, Name, Rcode, Rdata, TYPE_A, TYPE_AAAA};
use dsa_host_core::dns::DnsService;
use dsa_host_unix::dns_resolver::SystemResolver;

fn ask(service: &DnsService, name: &str, qtype: u16) -> wire::Message {
    let query = wire::encode(&wire::query(
        0x2828,
        &Name::from_dotted(name).unwrap(),
        qtype,
    ));
    wire::parse(&service.answer(&query, false).expect("an answer")).unwrap()
}

#[test]
fn fr28_the_system_resolver_answers_a_name_from_the_hosts_file() {
    // localhost is in every hosts file, which DNS servers never answer from.
    let service = DnsService::new(Arc::new(SystemResolver));
    let answer = ask(&service, "localhost", TYPE_A);
    assert_eq!(answer.id, 0x2828);
    assert_eq!(answer.rcode, Rcode::NoError, "{answer:?}");
    assert!(
        answer
            .answers
            .iter()
            .any(|r| r.rtype == TYPE_A && r.data == Rdata::Raw(vec![127, 0, 0, 1])),
        "{answer:?}"
    );
}

#[test]
fn fr28_a_name_that_exists_without_the_family_asked_is_not_nxdomain() {
    let service = DnsService::new(Arc::new(SystemResolver));
    let v4 = ask(&service, "localhost", TYPE_A);
    let v6 = ask(&service, "localhost", TYPE_AAAA);
    assert_eq!(v4.rcode, Rcode::NoError);
    // ::1 may or may not be in the hosts file; either way the name exists.
    assert_ne!(v6.rcode, Rcode::NxDomain, "{v6:?}");
}
