//! When the daemon has init write what stays cold in zram to its backing file, and how much.

use std::time::Duration;

use dsa_guest_channeld::writeback::{
    day_of, has_backing_device, BackingStat, Day, Run, Writeback, WritebackPolicy, ZramStat,
};

const MIB_PAGES: u64 = 256;

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

/// zram keeping `used` MiB in memory.
fn zram(used: u64) -> ZramStat {
    ZramStat {
        orig_kb: used * 3 * 1024,
        mem_used_kb: used * 1024,
        huge_pages: 0,
    }
}

const TODAY: u64 = 20_400;

#[test]
fn nfr1_zram_writes_back_only_after_no_app_has_been_in_use_for_a_while() {
    let policy = WritebackPolicy::default();
    let mut writeback = Writeback::new(policy, TODAY, None);
    // An app in use: never.
    assert_eq!(writeback.decide(secs(0), TODAY, 1, &zram(150)), None);
    // Idle from 100 s on: not before idle_after has passed.
    assert_eq!(writeback.decide(secs(100), TODAY, 0, &zram(150)), None);
    assert_eq!(
        writeback.decide(
            secs(100) + policy.idle_after - secs(1),
            TODAY,
            0,
            &zram(150)
        ),
        None
    );
    assert_eq!(
        writeback.decide(secs(100) + policy.idle_after, TODAY, 0, &zram(150)),
        Some(policy.run_pages)
    );
    // An app opening starts the wait again.
    let later = secs(100) + policy.idle_after + policy.gap;
    assert_eq!(writeback.decide(later, TODAY, 1, &zram(150)), None);
    assert_eq!(
        writeback.decide(later + secs(1), TODAY, 0, &zram(150)),
        None
    );
    assert_eq!(
        writeback.decide(later + secs(1) + policy.idle_after, TODAY, 0, &zram(150)),
        Some(policy.run_pages)
    );
}

#[test]
fn nfr1_zram_writes_back_at_most_once_a_gap_and_a_run_at_a_time() {
    let policy = WritebackPolicy::default();
    // 32 MiB a run, ten minutes apart: init stays busy a fraction of a second. 512 MiB a boot,
    // the size of the boot's backing file, and 2 GiB a day however often Android starts.
    assert_eq!(policy.run_pages, 32 * MIB_PAGES);
    assert_eq!(policy.boot_pages, 512 * MIB_PAGES);
    assert_eq!(policy.daily_pages, 2048 * MIB_PAGES);
    assert_eq!(policy.gap, secs(600));
    assert_eq!(policy.idle_after, secs(600));
    assert_eq!(policy.age, secs(1800));
    let mut writeback = Writeback::new(policy, TODAY, None);
    writeback.decide(secs(0), TODAY, 0, &zram(150));
    let first = policy.idle_after;
    assert!(writeback.decide(first, TODAY, 0, &zram(150)).is_some());
    assert_eq!(
        writeback.decide(first + secs(30), TODAY, 0, &zram(150)),
        None
    );
    assert_eq!(
        writeback.decide(first + policy.gap - secs(1), TODAY, 0, &zram(150)),
        None
    );
    assert!(writeback
        .decide(first + policy.gap, TODAY, 0, &zram(150))
        .is_some());
}

#[test]
fn nfr1_zram_writes_back_no_more_than_the_boots_budget() {
    let policy = WritebackPolicy {
        idle_after: Duration::ZERO,
        gap: Duration::ZERO,
        ..WritebackPolicy::default()
    };
    let mut writeback = Writeback::new(policy, TODAY, None);
    let mut written = 0;
    let mut now = secs(0);
    while let Some(pages) = writeback.decide(now, TODAY, 0, &zram(150)) {
        assert!(pages <= policy.run_pages);
        written += pages;
        writeback.wrote(pages);
        now += secs(1);
        assert!(written <= policy.boot_pages, "{written} pages");
    }
    assert_eq!(written, policy.boot_pages);
    assert_eq!(writeback.left_pages(), 0);
    assert_eq!(writeback.boot_left_pages(), 0);
    assert_eq!(
        writeback.day_left_pages(),
        policy.daily_pages - policy.boot_pages
    );
    // A new day does not give this boot more: its backing file is the one it filled.
    assert_eq!(writeback.decide(now, TODAY + 1, 0, &zram(150)), None);
    // The last run of the boot gets only what is left.
    let mut nearly = Writeback::new(policy, TODAY, None).with_boot_written(policy.boot_pages - 10);
    assert_eq!(nearly.decide(now, TODAY, 0, &zram(150)), Some(10));
}

#[test]
fn nfr1_each_boot_gets_its_own_zram_writeback_budget() {
    // The owner's PC, 1.0.1-dev.37: earlier boots that day had spent the day's 512 MiB, so the
    // boot measured wrote nothing ("0 MiB left today"), though their backing files, and all they
    // wrote, were deleted with them.
    let policy = WritebackPolicy {
        idle_after: Duration::ZERO,
        ..WritebackPolicy::default()
    };
    let earlier = Day {
        day: TODAY,
        pages: 512 * MIB_PAGES,
    };
    let mut booted = Writeback::new(policy, TODAY, Day::parse(&earlier.to_property()));
    assert_eq!(booted.boot_left_pages(), policy.boot_pages);
    assert_eq!(booted.left_pages(), policy.boot_pages);
    assert_eq!(
        booted.decide(secs(0), TODAY, 0, &zram(150)),
        Some(policy.run_pages)
    );
    // What it writes counts against both.
    assert_eq!(
        booted.wrote(policy.run_pages),
        Day {
            day: TODAY,
            pages: 512 * MIB_PAGES + policy.run_pages
        }
    );
    assert_eq!(
        booted.boot_left_pages(),
        policy.boot_pages - policy.run_pages
    );
    // A daemon started again within the boot counts what zram's backing device already took
    // (bd_stat), so the boot's budget is not given twice.
    let restarted =
        Writeback::new(policy, TODAY, Some(earlier)).with_boot_written(policy.boot_pages);
    assert_eq!(restarted.left_pages(), 0);
    // The day's cap still bounds what many boots write in one day.
    let mut many = Writeback::new(
        policy,
        TODAY,
        Some(Day {
            day: TODAY,
            pages: policy.daily_pages - 10,
        }),
    );
    assert_eq!(many.decide(secs(0), TODAY, 0, &zram(150)), Some(10));
    let spent = Day {
        day: TODAY,
        pages: policy.daily_pages,
    };
    let mut spent = Writeback::new(policy, TODAY, Some(spent));
    assert_eq!(spent.decide(secs(0), TODAY, 0, &zram(150)), None);
    // The next day has its budget again, up to what is left of the boot's.
    assert_eq!(
        spent.decide(secs(1), TODAY + 1, 0, &zram(150)),
        Some(policy.run_pages)
    );
}

#[test]
fn nfr1_a_restart_of_android_keeps_the_days_budget_spent() {
    let policy = WritebackPolicy {
        idle_after: Duration::ZERO,
        ..WritebackPolicy::default()
    };
    // What was written today counts after a restart; another day's does not.
    let spent = Day {
        day: TODAY,
        pages: policy.daily_pages,
    };
    let mut restarted = Writeback::new(policy, TODAY, Day::parse(&spent.to_property()));
    assert_eq!(restarted.decide(secs(0), TODAY, 0, &zram(150)), None);
    let yesterday = Day {
        day: TODAY - 1,
        pages: policy.daily_pages,
    };
    let mut fresh = Writeback::new(policy, TODAY, Some(yesterday));
    assert_eq!(fresh.day_left_pages(), policy.daily_pages);
    assert_eq!(
        fresh.decide(secs(0), TODAY, 0, &zram(150)),
        Some(policy.run_pages)
    );
    assert_eq!(
        fresh.wrote(100),
        Day {
            day: TODAY,
            pages: 100
        }
    );
    assert_eq!(
        Day::parse("20400 100"),
        Some(Day {
            day: 20_400,
            pages: 100
        })
    );
    for bad in ["", "20400", "x 1", "1 2 3"] {
        assert_eq!(Day::parse(bad), None, "{bad:?}");
    }
    assert_eq!(day_of(86_400 * 3 + 5), 3);
}

#[test]
fn nfr1_a_nearly_empty_zram_is_not_written_back() {
    let policy = WritebackPolicy {
        idle_after: Duration::ZERO,
        ..WritebackPolicy::default()
    };
    let mut writeback = Writeback::new(policy, TODAY, None);
    let small = ZramStat {
        mem_used_kb: policy.min_store_kb - 1,
        ..zram(0)
    };
    assert_eq!(writeback.decide(secs(0), TODAY, 0, &small), None);
    assert!(writeback.decide(secs(1), TODAY, 0, &zram(150)).is_some());
}

#[test]
fn nfr1_the_boot_can_shorten_the_writeback_times() {
    let policy = WritebackPolicy::with_times("30", "60");
    assert_eq!(policy.idle_after, secs(30));
    assert_eq!(policy.age, secs(60));
    // Unset or malformed keeps the defaults.
    assert_eq!(
        WritebackPolicy::with_times("", "soon"),
        WritebackPolicy::default()
    );
}

#[test]
fn nfr1_zrams_statistics_are_read_for_what_a_run_wrote() {
    // mm_stat in bytes: orig compr used limit used_max same compacted huge huge_since.
    let mm =
        ZramStat::parse("503316480 160432128 167772160 0 180355072 1200 300 512 900\n").unwrap();
    assert_eq!(
        mm,
        ZramStat {
            orig_kb: 480 * 1024,
            mem_used_kb: 160 * 1024,
            huge_pages: 512
        }
    );
    assert_eq!(ZramStat::parse("1 2"), None);
    // bd_stat in 4 KiB units.
    let before = BackingStat::parse("0 0 0\n").unwrap();
    let after = BackingStat::parse("8000 10 8192\n").unwrap();
    assert_eq!(after.stored_kb, 32_000);
    assert_eq!(after.written_kb, 32 * 1024);
    assert_eq!(BackingStat::parse("1 2"), None);
    assert_eq!(BackingStat::parse("a b c"), None);
    assert!(has_backing_device("/dev/block/loop7\n"));
    assert!(!has_backing_device("none\n"));
    assert!(!has_backing_device(""));

    let run = Run {
        before: mm,
        after: ZramStat {
            mem_used_kb: 150 * 1024,
            orig_kb: 448 * 1024,
            huge_pages: 0,
        },
        backing_before: before,
        backing_after: after,
        took: Duration::from_millis(640),
        boot_left_pages: 480 * MIB_PAGES,
        boot_pages: 512 * MIB_PAGES,
        left_pages: 1504 * MIB_PAGES,
        daily_pages: 2048 * MIB_PAGES,
    };
    assert_eq!(run.written_pages(), 8192);
    // The boot check reads these numbers back from the guest's log.
    assert_eq!(
        run.line(),
        "zram writeback: wrote 32768 KiB of cold pages to the backing file in 640 ms; zram store \
         163840 -> 153600 KiB (holding 491520 -> 458752 KiB, 512 -> 0 incompressible pages), \
         32000 KiB on the disk; 480 of this boot's 512 MiB and 1504 of today's 2048 MiB left"
    );
}
