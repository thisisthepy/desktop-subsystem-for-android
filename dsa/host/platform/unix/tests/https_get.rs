//! The HTTPS fetch the passkey check reads Digital Asset Links with: curl's arguments and how
//! its answer is read.

use dsa_host_core::passkeys::asset_links::HttpsGet;
use dsa_host_unix::https_get::{curl_args, read_answer, CurlHttpsGet};

const URL: &str = "https://bank.example/.well-known/assetlinks.json";

#[test]
fn fr36_asset_links_are_fetched_over_https_without_redirects() {
    let args = curl_args(URL, 128 * 1024);
    assert_eq!(
        args[0], "-q",
        "a ~/.curlrc is never read, and -q must come first"
    );
    assert!(args.windows(2).any(|w| w == ["--proto", "=https"]));
    assert!(args.windows(2).any(|w| w == ["--max-redirs", "0"]));
    assert!(args.windows(2).any(|w| w == ["--max-filesize", "131072"]));
    assert!(args.iter().any(|a| a == "--max-time"));
    assert!(args.iter().any(|a| a == "--connect-timeout"));
    for never in ["-L", "--location", "-k", "--insecure", "--proto-redir"] {
        assert!(!args.iter().any(|a| a == never), "{never} is never passed");
    }
    assert_eq!(args.last().unwrap(), URL);
    assert_eq!(
        args[args.len() - 2],
        "--",
        "the address is never read as an option"
    );
}

#[test]
fn fr36_a_redirect_or_an_error_answer_verifies_nothing() {
    assert_eq!(read_answer(URL, b"[]200", 10), Ok(b"[]".to_vec()));
    let redirect = read_answer(URL, b"301", 10).unwrap_err();
    assert!(redirect.contains("redirect"), "{redirect}");
    assert!(read_answer(URL, b"<html>moved</html>302", 100).is_err());
    assert!(read_answer(URL, b"not found404", 100).is_err());
    assert!(read_answer(URL, b"000", 100).is_err());
    assert!(read_answer(URL, b"", 100).is_err());
    assert!(read_answer(URL, b"[1,2,3]200", 3).is_err(), "too large");
}

#[test]
fn fr36_only_https_addresses_are_fetched() {
    let curl = CurlHttpsGet::new("/nonexistent/curl");
    let refused = curl
        .get("http://bank.example/.well-known/assetlinks.json", 10)
        .unwrap_err();
    assert!(refused.contains("not an HTTPS address"), "{refused}");
}
