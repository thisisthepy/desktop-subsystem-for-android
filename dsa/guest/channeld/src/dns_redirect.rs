//! The packet filter rules that send the guest's DNS queries to the DNS stub
//! ([`crate::dns_stub`]).
//!
//! `dsa-channeld --dns-redirect` runs as root, which changing the packet filter needs, in a
//! domain of its own (`channeld.cil`). init starts it whenever the daemon sets
//! [`crate::dns_stub::PROPERTY`] (`dsa-channeld.rc`), whether netd runs yet or not: netd never
//! flushes `nat`'s `OUTPUT`, where the jump goes, and `-w` waits while netd holds the packet
//! filter's lock. It reads the property and makes the rules match it:
//!
//! - `on`: a chain of its own in the `nat` table, [`CHAIN`], which `OUTPUT` jumps to, sends
//!   every UDP and TCP packet a program in the guest sends to port 53 to the stub's port on the
//!   loopback address (`REDIRECT`), for IPv4 and IPv6. A query to the loopback address itself
//!   is left alone, so nothing loops.
//! - `off` (or anything else): the chain is emptied, and queries go to their servers again.
//!
//! The chain is the helper's alone, so making it again is the same whether it was there or
//! not, and netd's own chains are never touched. A family whose `nat` table the kernel lacks is
//! reported and skipped; the other still works.

use crate::dns_stub::{PROPERTY, STUB_PORT};

/// The `nat` chain the rules live in.
pub const CHAIN: &str = "dsa_dns";

/// The packet filter programs, for IPv4 and IPv6.
pub const IPTABLES: &str = "/system/bin/iptables";
pub const IP6TABLES: &str = "/system/bin/ip6tables";

/// Whether the property says the redirect is on.
pub fn wanted(value: &str) -> bool {
    value.trim() == "on"
}

/// One run of a packet filter program: its arguments, and whether a failure may be ignored
/// (making a chain that is there already, removing a jump that is not).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub args: Vec<String>,
    pub may_fail: bool,
}

fn step(args: &[&str], may_fail: bool) -> Step {
    Step {
        args: args.iter().map(|a| a.to_string()).collect(),
        may_fail,
    }
}

/// What makes the rules of one family match `on`. `loopback` is the family's loopback
/// network (`127.0.0.0/8`, `::1/128`).
pub fn steps(on: bool, loopback: &str) -> Vec<Step> {
    let port = STUB_PORT.to_string();
    let mut steps = vec![
        // -w waits for netd's lock rather than failing while netd changes its rules.
        step(&["-w", "-t", "nat", "-N", CHAIN], true),
        step(&["-w", "-t", "nat", "-F", CHAIN], false),
    ];
    if on {
        for protocol in ["udp", "tcp"] {
            steps.push(step(
                &[
                    "-w",
                    "-t",
                    "nat",
                    "-A",
                    CHAIN,
                    "!",
                    "-d",
                    loopback,
                    "-p",
                    protocol,
                    "--dport",
                    "53",
                    "-j",
                    "REDIRECT",
                    "--to-ports",
                    &port,
                ],
                false,
            ));
        }
        // The jump goes in once: removed first, so a second run does not add a second.
        steps.push(step(
            &["-w", "-t", "nat", "-D", "OUTPUT", "-j", CHAIN],
            true,
        ));
        steps.push(step(
            &["-w", "-t", "nat", "-I", "OUTPUT", "-j", CHAIN],
            false,
        ));
    }
    steps
}

/// Runs a packet filter program; `Err` with what it said when it failed.
pub type Runner<'a> = dyn Fn(&str, &[String]) -> Result<(), String> + 'a;

/// Makes both families' rules match `on`. Returns the problems, empty when all went well.
pub fn apply(on: bool, run: &Runner) -> Vec<String> {
    let mut problems = Vec::new();
    for (program, loopback) in [(IPTABLES, "127.0.0.0/8"), (IP6TABLES, "::1/128")] {
        for step in steps(on, loopback) {
            if let Err(why) = run(program, &step.args) {
                if !step.may_fail {
                    problems.push(format!("{program} {}: {why}", step.args.join(" ")));
                    // The rest of this family's steps depend on this one.
                    break;
                }
            }
        }
    }
    problems
}

/// `dsa-channeld --dns-redirect`: makes the rules match the property, and again while the
/// property changed meanwhile. Returns the exit code.
pub fn run(read_property: &dyn Fn(&str) -> String, run: &Runner, say: &dyn Fn(&str)) -> i32 {
    let mut applied: Option<bool> = None;
    loop {
        let on = wanted(&read_property(PROPERTY));
        if applied == Some(on) {
            return 0;
        }
        let problems = apply(on, run);
        for problem in &problems {
            say(&format!("the DNS redirect: {problem}"));
        }
        let ipv4_failed = problems
            .iter()
            .any(|p| p.starts_with(&format!("{IPTABLES} ")));
        if on && ipv4_failed {
            say("the DNS redirect could not be switched on; the guest keeps asking its network's DNS servers");
        } else if on {
            say("the guest's DNS queries go to the DNS stub, which asks the host");
        } else {
            say("the guest's DNS queries go to its network's DNS servers");
        }
        applied = Some(on);
    }
}
