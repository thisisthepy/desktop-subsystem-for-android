//! When the daemon tells the host how much memory the guest has.

use std::time::{Duration, Instant};

use dsa_guest_channeld::pressure::{PressureReporter, Sample, CALM_INTERVAL};
use dsa_protocol::control::envelope::Body;

const MEMINFO: &str = "MemTotal:        2969600 kB\nMemFree:          102400 kB\n\
                       MemAvailable:     409600 kB\nCached:           300000 kB\n";
const PSI: &str = "some avg10=12.50 avg60=3.00 avg300=1.00 total=123\n\
                   full avg10=1.00 avg60=0.00 avg300=0.00 total=12\n";

#[test]
fn nfr2_a_sample_reads_the_total_the_available_memory_and_the_stall() {
    let sample = Sample::parse(MEMINFO, PSI);
    assert_eq!(sample.mem_total_kb, 2_969_600);
    assert_eq!(sample.mem_available_kb, 409_600);
    assert_eq!(sample.stall_percent, 12.5);
    assert!(sample.short());
    let Some(Body::GuestMemoryPressure(report)) =
        dsa_protocol::decode(&sample.frame()).unwrap().body
    else {
        panic!("not a pressure report");
    };
    assert_eq!(report.stall_centipercent, 1250);
    assert_eq!(report.mem_available_kb, 409_600);
}

#[test]
fn nfr2_a_short_guest_reports_at_once_and_a_calm_one_every_30_seconds() {
    let start = Instant::now();
    let mut reporter = PressureReporter::default();
    let calm = Sample {
        mem_total_kb: 2_969_600,
        mem_available_kb: 2_000_000,
        stall_percent: 0.0,
    };
    let short = Sample {
        mem_available_kb: 300_000,
        ..calm
    };
    assert!(reporter.due(&calm, start));
    assert!(!reporter.due(&calm, start + Duration::from_secs(2)));
    assert!(reporter.due(&short, start + Duration::from_secs(4)));
    assert!(reporter.due(&short, start + Duration::from_secs(6)));
    // Calm again: the memory that came back is news once, then the calm pace resumes.
    assert!(reporter.due(&calm, start + Duration::from_secs(8)));
    assert!(!reporter.due(&calm, start + Duration::from_secs(10)));
    assert!(reporter.due(&calm, start + Duration::from_secs(8) + CALM_INTERVAL));
}

#[test]
fn nfr2_memory_the_guest_frees_is_reported_at_once_so_the_host_takes_it_back() {
    let start = Instant::now();
    let mut reporter = PressureReporter::default();
    let busy = Sample {
        mem_total_kb: 4_194_304,
        mem_available_kb: 2_000_000,
        stall_percent: 0.0,
    };
    assert!(reporter.due(&busy, start));
    // A little more free is not news.
    let a_little = Sample {
        mem_available_kb: 2_050_000,
        ..busy
    };
    assert!(!reporter.due(&a_little, start + Duration::from_secs(2)));
    // An app closed and 512 MiB came free: the host hears it now, not in 30 seconds.
    let freed = Sample {
        mem_available_kb: 2_000_000 + 512 * 1024,
        ..busy
    };
    assert!(reporter.due(&freed, start + Duration::from_secs(4)));
    assert!(!reporter.due(&freed, start + Duration::from_secs(6)));
}
