//! Windows' DNS client answering the guest's DNS queries, through the host's DNS service.
#![cfg(windows)]

use std::sync::Arc;

use dsa_host_core::dns::wire::{self, Name, Rcode, Rdata, TYPE_A};
use dsa_host_core::dns::DnsService;
use dsa_host_windows::dns_resolver::WindowsResolver;

#[test]
fn fr28_windows_answers_a_name_its_own_resolver_knows() {
    // Windows' DNS client answers localhost itself, as it does for a hosts file entry.
    let service = DnsService::new(Arc::new(WindowsResolver));
    let query = wire::encode(&wire::query(
        0x2828,
        &Name::from_dotted("localhost").unwrap(),
        TYPE_A,
    ));
    let answer = wire::parse(&service.answer(&query, false).expect("an answer")).unwrap();
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
