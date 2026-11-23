// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The graphics memory the host shares with the guest (dsa_graphics_memory.h): the renderer's
// allocator and the guest kernel's grants, over a fake region in this process's memory, run
// on the build machine.

#include "dsa_graphics_memory.h"

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <optional>
#include <vector>

static int failures = 0;

#define EXPECT(cond)                                                                \
    do {                                                                            \
        if (!(cond)) {                                                              \
            std::fprintf(stderr, "%s:%d: expected %s\n", __FILE__, __LINE__, #cond); \
            ++failures;                                                             \
        }                                                                           \
    } while (0)

constexpr uint64_t kMiB = 1024 * 1024;
constexpr uint64_t kGranule = dsa::kGraphicsGranule;

// The two sections the service makes, as this process's memory.
struct FakeRegion {
    explicit FakeRegion(uint64_t length, uint64_t id = 7)
        : data(length, 0), grants(2 * kMiB, 0), length(length) {
        dsa::writeGrantsHeader(grants.data(), id, length);
    }
    std::vector<uint8_t> data;
    std::vector<uint8_t> grants;
    uint64_t length;
};

struct ScrubLog {
    std::vector<std::pair<uint64_t, uint64_t>> ranges;  // offset, length
    uint8_t* base = nullptr;
};

static void recordScrub(void* context, uint8_t* at, uint64_t length) {
    auto* log = static_cast<ScrubLog*>(context);
    std::memset(at, 0, length);
    log->ranges.push_back({static_cast<uint64_t>(at - log->base), length});
}

static bool attach(dsa::GraphicsMemory& memory, FakeRegion& region, ScrubLog* log) {
    log->base = region.data.data();
    const bool ok = memory.attach(region.data.data(), region.length, region.grants.data(),
                                  region.grants.size(), recordScrub, log);
    memory.seedKeys(1234);
    log->ranges.clear();
    return ok;
}

// pr7: grants come in whole 64 KiB granules after the release slots, first fit, and say
// their key in the table for the guest.
static void pr7_grants_are_whole_granules_after_the_release_slots() {
    FakeRegion region(16 * kMiB);
    dsa::GraphicsMemory memory;
    ScrubLog log;
    EXPECT(attach(memory, region, &log));
    const uint64_t slots = dsa::graphicsAckBytes(16 * kMiB);
    EXPECT(slots == kGranule);  // 256 granules, 8 bytes each, in one granule
    auto first = memory.allocate(1, 11);
    auto second = memory.allocate(kGranule + 1, 11);
    EXPECT(first && second);
    EXPECT(first->offset == slots);
    EXPECT(first->length == kGranule);
    EXPECT(second->offset == slots + kGranule);
    EXPECT(second->length == 2 * kGranule);
    EXPECT(first->key != 0 && second->key != 0 && first->key != second->key);
    const uint8_t* entry =
        region.grants.data() + dsa::kGrantsHeaderBytes + (second->offset / kGranule) * 32;
    EXPECT(dsa::sharedLoad(entry + dsa::kGrantKeyAt) == second->key);
    EXPECT(dsa::sharedLoad(entry + dsa::kGrantLengthAt) == 2 * kGranule);
    EXPECT(dsa::sharedLoad(entry + dsa::kGrantOwnerAt) == 11);
    EXPECT(memory.pointer(second->offset) == region.data.data() + second->offset);
}

// pr7: an allocation the region cannot hold is refused with its reason, so it takes the copy
// path; no region at all is refused too.
static void pr7_an_allocation_the_region_cannot_hold_takes_the_copy_path() {
    dsa::GraphicsMemory none;
    dsa::GrantRefusal why = dsa::GrantRefusal::NotRefused;
    EXPECT(!none.allocate(kGranule, 1, &why));
    EXPECT(why == dsa::GrantRefusal::NoRegion);

    FakeRegion region(4 * kMiB);
    dsa::GraphicsMemory memory;
    ScrubLog log;
    EXPECT(attach(memory, region, &log));
    EXPECT(!memory.allocate(8 * kMiB, 1, &why));
    EXPECT(why == dsa::GrantRefusal::TooLarge);
    // 64 granules, one of them the release slots.
    EXPECT(memory.allocate(63 * kGranule, 1, &why));
    EXPECT(!memory.allocate(1, 1, &why));
    EXPECT(why == dsa::GrantRefusal::Full);
}

// pr7: a table that is not the service's is not taken over.
static void pr7_a_table_that_is_not_the_services_is_refused() {
    FakeRegion region(4 * kMiB);
    region.grants[0] = 'X';
    dsa::GraphicsMemory memory;
    ScrubLog log;
    EXPECT(!attach(memory, region, &log));
    FakeRegion other(4 * kMiB);
    EXPECT(!memory.attach(other.data.data(), 8 * kMiB, other.grants.data(), other.grants.size(),
                          nullptr, nullptr));
}

// pr7: the guest maps a grant only with its key and length, and only one file holds it.
static void pr7_the_guest_maps_only_a_grant_it_has_the_key_of() {
    FakeRegion region(16 * kMiB);
    dsa::GraphicsMemory memory;
    ScrubLog log;
    EXPECT(attach(memory, region, &log));
    auto grant = memory.allocate(3 * kGranule, 21);
    auto other = memory.allocate(kGranule, 22);
    EXPECT(grant && other);
    dsa::GuestGrantTable guest(region.data.data(), region.grants.data());
    // Another app's key, a made-up key, a wrong length, the release slots: all refused.
    EXPECT(guest.bind(1, grant->offset, grant->length, other->key) != 0);
    EXPECT(guest.bind(1, grant->offset, grant->length, grant->key ^ 1) != 0);
    EXPECT(guest.bind(1, grant->offset, kGranule, grant->key) != 0);
    EXPECT(guest.bind(1, 0, kGranule, grant->key) != 0);
    EXPECT(guest.bind(1, grant->offset, grant->length, grant->key) == 0);
    // A second file with the key does not get it while the first holds it.
    EXPECT(guest.bind(2, grant->offset, grant->length, grant->key) != 0);
    EXPECT(guest.mayMap(1, grant->offset, grant->length));
    EXPECT(guest.mayMap(1, grant->offset + kGranule, kGranule));
    EXPECT(!guest.mayMap(1, grant->offset, grant->length + kGranule));
    EXPECT(!guest.mayMap(2, grant->offset, kGranule));
    EXPECT(!guest.mayMap(1, other->offset, kGranule));
}

// pr7: a grant the guest never bound comes back, scrubbed, at the first reclaim after it is
// revoked; the revoke itself, which runs under the decoder's lock, scrubs nothing.
static void pr7_a_grant_the_guest_never_bound_comes_back_at_the_next_reclaim() {
    FakeRegion region(16 * kMiB);
    dsa::GraphicsMemory memory;
    ScrubLog log;
    EXPECT(attach(memory, region, &log));
    auto grant = memory.allocate(2 * kGranule, 3);
    EXPECT(grant);
    std::memset(region.data.data() + grant->offset, 0xAB, grant->length);
    memory.release(grant->offset);
    EXPECT(log.ranges.empty());
    EXPECT(memory.reclaim() == grant->length);
    EXPECT(log.ranges.size() == 1);
    EXPECT(log.ranges[0].first == grant->offset && log.ranges[0].second == grant->length);
    EXPECT(region.data[grant->offset] == 0);
    EXPECT(memory.stats().grantedBytes == 0 && memory.stats().heldBytes == 0);
    // The key is gone, so a late bind fails.
    dsa::GuestGrantTable guest(region.data.data(), region.grants.data());
    EXPECT(guest.bind(1, grant->offset, grant->length, grant->key) != 0);
}

// pr7: a grant the guest holds is not handed out again until the guest lets it go, and then
// it is scrubbed first, so no app sees another's memory.
static void pr7_a_held_grant_is_not_handed_out_again_until_the_guest_lets_go() {
    FakeRegion region(4 * kMiB);
    dsa::GraphicsMemory memory;
    ScrubLog log;
    EXPECT(attach(memory, region, &log));
    auto grant = memory.allocate(63 * kGranule, 5);  // the whole region
    EXPECT(grant);
    dsa::GuestGrantTable guest(region.data.data(), region.grants.data());
    EXPECT(guest.bind(9, grant->offset, grant->length, grant->key) == 0);
    std::memset(region.data.data() + grant->offset, 0x5A, grant->length);
    memory.release(grant->offset);
    EXPECT(log.ranges.empty());
    EXPECT(memory.stats().heldBytes == grant->length);
    dsa::GrantRefusal why = dsa::GrantRefusal::NotRefused;
    EXPECT(!memory.allocate(kGranule, 6, &why));
    EXPECT(why == dsa::GrantRefusal::Full);
    EXPECT(memory.reclaim() == 0);
    // The app frees its memory: the driver takes its mappings away and says so.
    EXPECT(guest.release(9, grant->offset) == 0);
    EXPECT(!guest.mayMap(9, grant->offset, kGranule));
    auto next = memory.allocate(kGranule, 6);
    EXPECT(next);
    EXPECT(log.ranges.size() == 1 && log.ranges[0].second == grant->length);
    EXPECT(region.data[next->offset] == 0);
}

// pr7: a process that ends lets go of every grant it held: its file closes in the guest and
// the renderer revokes what it owned.
static void pr7_an_ended_process_gives_back_every_grant() {
    FakeRegion region(16 * kMiB);
    dsa::GraphicsMemory memory;
    ScrubLog log;
    EXPECT(attach(memory, region, &log));
    auto a = memory.allocate(kGranule, 40);
    auto b = memory.allocate(4 * kGranule, 40);
    auto c = memory.allocate(kGranule, 41);
    EXPECT(a && b && c);
    dsa::GuestGrantTable guest(region.data.data(), region.grants.data());
    EXPECT(guest.bind(1, a->offset, a->length, a->key) == 0);
    EXPECT(guest.bind(1, b->offset, b->length, b->key) == 0);
    memory.releaseOwner(40);
    EXPECT(memory.stats().heldBytes == 5 * kGranule);
    EXPECT(memory.stats().grantedBytes == kGranule);
    guest.close(1);
    EXPECT(memory.reclaim() == 5 * kGranule);
    EXPECT(memory.stats().heldBytes == 0);
}

// pr7: the guest binding and the renderer revoking at once never both go ahead: once the key
// is gone, a bind that read it backs out and says the grant is let go of.
static void pr7_a_bind_racing_a_revoke_never_leaves_a_mapped_grant_free() {
    FakeRegion region(4 * kMiB);
    dsa::GraphicsMemory memory;
    ScrubLog log;
    EXPECT(attach(memory, region, &log));
    auto grant = memory.allocate(kGranule, 1);
    EXPECT(grant);
    const uint64_t index = grant->offset / kGranule;
    uint8_t* slot = region.data.data() + index * 8;
    const uint8_t* entry = region.grants.data() + dsa::kGrantsHeaderBytes + index * 32;
    const uint64_t seq = dsa::sharedLoad(entry + dsa::kGrantSeqAt);
    // The guest read the key and wrote "bound"; the renderer then revokes and sees it.
    dsa::sharedStore(slot, dsa::slotValue(seq, dsa::kSlotBound));
    memory.release(grant->offset);
    EXPECT(memory.stats().heldBytes == kGranule);
    // The guest reads the key again, finds it gone and backs out.
    EXPECT(dsa::sharedLoad(entry + dsa::kGrantKeyAt) == 0);
    dsa::sharedStore(slot, dsa::slotValue(seq, dsa::kSlotReleased));
    EXPECT(memory.reclaim() == kGranule);
}

// pr7: a renderer that starts again revokes what the one before it granted; what the guest
// held comes back once it lets go, the rest at once, and sequence numbers go on.
static void pr7_a_restarted_renderer_takes_back_the_last_ones_grants() {
    FakeRegion region(16 * kMiB);
    dsa::GuestGrantTable guest(region.data.data(), region.grants.data());
    uint64_t heldOffset = 0;
    uint64_t lastSeq = 0;
    {
        dsa::GraphicsMemory first;
        ScrubLog log;
        EXPECT(attach(first, region, &log));
        auto held = first.allocate(2 * kGranule, 1);
        auto unbound = first.allocate(kGranule, 1);
        EXPECT(held && unbound);
        EXPECT(guest.bind(3, held->offset, held->length, held->key) == 0);
        heldOffset = held->offset;
        lastSeq = dsa::sharedLoad(region.grants.data() + dsa::kGrantsNextSeqAt);
        // The renderer dies here, without revoking anything.
    }
    dsa::GraphicsMemory second;
    ScrubLog log;
    EXPECT(attach(second, region, &log));
    EXPECT(dsa::sharedLoad(region.grants.data() + dsa::kGrantsGenerationAt) == 2);
    const uint8_t* entry =
        region.grants.data() + dsa::kGrantsHeaderBytes + (heldOffset / kGranule) * 32;
    EXPECT(dsa::sharedLoad(entry + dsa::kGrantKeyAt) == 0);
    EXPECT(second.stats().heldBytes == 2 * kGranule);
    auto fresh = second.allocate(kGranule, 2);
    EXPECT(fresh);
    EXPECT(fresh->offset != heldOffset && fresh->offset != heldOffset + kGranule);
    const uint8_t* freshEntry =
        region.grants.data() + dsa::kGrantsHeaderBytes + (fresh->offset / kGranule) * 32;
    EXPECT(dsa::sharedLoad(freshEntry + dsa::kGrantSeqAt) >= lastSeq);
    guest.close(3);
    EXPECT(second.reclaim() == 2 * kGranule);
}

// pr7: a grant of the region before it was taken over again (the last boot's memory, freed
// late) does not revoke the grant that has its offset now.
static void pr7_a_late_free_from_before_a_take_over_leaves_the_new_grant_alone() {
    FakeRegion region(16 * kMiB);
    dsa::GraphicsMemory memory;
    ScrubLog log;
    EXPECT(attach(memory, region, &log));
    auto old = memory.allocate(kGranule, 1);
    EXPECT(old && old->generation != 0);
    EXPECT(attach(memory, region, &log));
    auto fresh = memory.allocate(kGranule, 2);
    EXPECT(fresh && fresh->offset == old->offset);
    memory.release(old->offset, old->generation);
    EXPECT(memory.stats().grantedBytes == kGranule);
    memory.release(fresh->offset, fresh->generation);
    EXPECT(memory.stats().grantedBytes == 0);
}

// pr7: a staging copy may touch only bytes inside one live grant of the process that names it.
static void pr7_a_staging_copy_stays_inside_its_own_grant() {
    FakeRegion region(16 * kMiB);
    dsa::GraphicsMemory memory;
    ScrubLog log;
    EXPECT(attach(memory, region, &log));
    auto mine = memory.allocate(2 * kGranule, 70);
    auto theirs = memory.allocate(kGranule, 71);
    EXPECT(mine && theirs);
    const uint8_t* at = memory.pointer(mine->offset);
    EXPECT(memory.owns(at, mine->length, 70));
    EXPECT(memory.owns(at + 100, mine->length - 100, 70));
    EXPECT(!memory.owns(at, mine->length + 1, 70));
    EXPECT(!memory.owns(at, kGranule, 71));
    EXPECT(!memory.owns(memory.pointer(theirs->offset), kGranule, 70));
    EXPECT(!memory.owns(region.data.data(), 8, 70));  // the release slots
    EXPECT(memory.offsetOf(at) && *memory.offsetOf(at) == mine->offset);
    memory.release(mine->offset);
    EXPECT(!memory.owns(at, kGranule, 70));
}

// The commit layer of a section that only reserves its size, faked: it records what it
// committed, can refuse, and checks that no grant over a range is visible in the table yet when
// the range is committed.
struct FakeCommit {
    FakeRegion* region = nullptr;
    std::vector<bool> committed;  // one per granule of the data region
    std::vector<std::pair<uint64_t, uint64_t>> calls;  // offset in the data region, length
    bool refuse = false;
    bool grantsCommitted = false;
    bool keyVisibleAtCommit = false;
};

static bool fakeCommit(void* context, uint8_t* at, uint64_t length) {
    auto* fake = static_cast<FakeCommit*>(context);
    if (fake->refuse) return false;
    uint8_t* data = fake->region->data.data();
    if (at == fake->region->grants.data()) {
        fake->grantsCommitted = true;
        return true;
    }
    const uint64_t offset = static_cast<uint64_t>(at - data);
    fake->calls.push_back({offset, length});
    const uint8_t* entry = fake->region->grants.data() + dsa::kGrantsHeaderBytes +
                           (offset / kGranule) * dsa::kGrantEntryBytes;
    if (dsa::sharedLoad(entry + dsa::kGrantKeyAt) != 0) fake->keyVisibleAtCommit = true;
    for (uint64_t i = offset / kGranule; i < (offset + length) / kGranule; ++i) {
        fake->committed[i] = true;
    }
    return true;
}

static bool attachCommitted(dsa::GraphicsMemory& memory, FakeRegion& region, ScrubLog* log,
                            FakeCommit* fake, const char** failure = nullptr) {
    log->base = region.data.data();
    fake->region = &region;
    fake->committed.assign(dsa::graphicsGranules(region.length), false);
    const bool ok = memory.attach(region.data.data(), region.length, region.grants.data(),
                                  region.grants.size(), recordScrub, log, fakeCommit, fake,
                                  failure);
    memory.seedKeys(99);
    log->ranges.clear();
    return ok;
}

static bool allCommitted(const FakeCommit& fake, uint64_t offset, uint64_t length) {
    for (uint64_t i = offset / kGranule; i < (offset + length) / kGranule; ++i) {
        if (!fake.committed[i]) return false;
    }
    return true;
}

// pr7: the renderer commits the grants table and the release slots when it takes the region
// over, and a grant's granules before its key is in the table, so the guest can never touch a
// page of the section that is only reserved.
static void pr7_a_grant_is_committed_before_the_guest_can_see_it() {
    FakeRegion region(16 * kMiB);
    dsa::GraphicsMemory memory;
    ScrubLog log;
    FakeCommit fake;
    EXPECT(attachCommitted(memory, region, &log, &fake));
    EXPECT(fake.grantsCommitted);
    EXPECT(allCommitted(fake, 0, dsa::graphicsAckBytes(region.length)));
    EXPECT(!fake.committed[dsa::graphicsAckBytes(region.length) / kGranule]);
    auto grant = memory.allocate(3 * kGranule, 4);
    EXPECT(grant);
    EXPECT(allCommitted(fake, grant->offset, grant->length));
    EXPECT(!fake.keyVisibleAtCommit);
    // Nothing past the grant: the rest of the region stays reserved.
    EXPECT(!fake.committed[(grant->offset + grant->length) / kGranule]);
    EXPECT(memory.stats().committedBytes == dsa::graphicsAckBytes(region.length) + 3 * kGranule);
    // The guest binds it only after that.
    dsa::GuestGrantTable guest(region.data.data(), region.grants.data());
    EXPECT(guest.bind(1, grant->offset, grant->length, grant->key) == 0);
}

// pr7: a grant whose granules the system will not commit is refused with its own reason, so
// the allocation takes the copy path, and the table says nothing of it; granules committed
// already are still handed out.
static void pr7_a_grant_that_cannot_be_committed_takes_the_copy_path() {
    FakeRegion region(16 * kMiB);
    dsa::GraphicsMemory memory;
    ScrubLog log;
    FakeCommit fake;
    EXPECT(attachCommitted(memory, region, &log, &fake));
    auto first = memory.allocate(2 * kGranule, 1);
    EXPECT(first);
    memory.release(first->offset);
    EXPECT(memory.reclaim() == 2 * kGranule);

    fake.refuse = true;
    dsa::GrantRefusal why = dsa::GrantRefusal::NotRefused;
    EXPECT(!memory.allocate(4 * kGranule, 2, &why));
    EXPECT(why == dsa::GrantRefusal::CommitFailed);
    EXPECT(std::strstr(dsa::grantRefusalText(why), "commit") != nullptr);
    for (uint64_t index = 0; index < dsa::graphicsGranules(region.length); ++index) {
        const uint8_t* entry =
            region.grants.data() + dsa::kGrantsHeaderBytes + index * dsa::kGrantEntryBytes;
        EXPECT(dsa::sharedLoad(entry + dsa::kGrantKeyAt) == 0);
    }
    EXPECT(memory.stats().grants == 0);
    // The two granules the first grant committed fit an allocation of that size still.
    auto again = memory.allocate(2 * kGranule, 2, &why);
    EXPECT(again && again->offset == first->offset);

    fake.refuse = false;
    EXPECT(memory.allocate(4 * kGranule, 2, &why));
}

// pr7: a granule is committed once: a grant over granules committed before commits nothing
// more, so the commit is the high-water mark of what grants used.
static void pr7_granules_are_committed_once_up_to_the_high_water_mark() {
    FakeRegion region(16 * kMiB);
    dsa::GraphicsMemory memory;
    ScrubLog log;
    FakeCommit fake;
    EXPECT(attachCommitted(memory, region, &log, &fake));
    fake.calls.clear();
    auto a = memory.allocate(4 * kGranule, 1);
    EXPECT(a);
    EXPECT(fake.calls.size() == 1 && fake.calls[0].first == a->offset &&
           fake.calls[0].second == 4 * kGranule);
    memory.release(a->offset);
    EXPECT(memory.reclaim() == 4 * kGranule);
    // Scrubbed and given back, but still committed: the same granules again commit nothing.
    auto b = memory.allocate(2 * kGranule, 1);
    auto c = memory.allocate(2 * kGranule, 1);
    EXPECT(b && c);
    EXPECT(fake.calls.size() == 1);
    // One more granule than has been committed commits only that one.
    memory.release(b->offset);
    memory.release(c->offset);
    EXPECT(memory.reclaim() == 4 * kGranule);
    auto d = memory.allocate(5 * kGranule, 1);
    EXPECT(d && d->offset == a->offset);
    EXPECT(fake.calls.size() == 2 && fake.calls[1].first == a->offset + 4 * kGranule &&
           fake.calls[1].second == kGranule);
    EXPECT(memory.stats().committedBytes == dsa::graphicsAckBytes(region.length) + 5 * kGranule);
}

// pr7: a region whose grants table or release slots cannot be committed is not taken over,
// and says why.
static void pr7_a_region_that_cannot_be_committed_is_not_taken_over() {
    FakeRegion region(16 * kMiB);
    dsa::GraphicsMemory memory;
    ScrubLog log;
    FakeCommit fake;
    fake.refuse = true;
    const char* failure = nullptr;
    EXPECT(!attachCommitted(memory, region, &log, &fake, &failure));
    EXPECT(failure && std::strstr(failure, "committed") != nullptr);
    EXPECT(!memory.attached());
    dsa::GrantRefusal why = dsa::GrantRefusal::NotRefused;
    EXPECT(!memory.allocate(kGranule, 1, &why));
    EXPECT(why == dsa::GrantRefusal::NoRegion);
}

// A scrub that, the first time it is armed, lets another render thread ask for a grant while
// it runs (the scrub runs with no lock held), and has that grant's owner write it at once, as
// the hwcomposer packs a video picture into its new staging grant.
struct RacingScrub {
    dsa::GraphicsMemory* memory = nullptr;
    uint8_t* base = nullptr;
    bool armed = false;
    uint64_t scrubbedOffset = 0;
    uint64_t scrubbedLength = 0;
    std::optional<dsa::GraphicsGrant> during;
};

static void scrubWhileAnotherThreadAllocates(void* context, uint8_t* at, uint64_t length) {
    auto* race = static_cast<RacingScrub*>(context);
    if (race->armed) {
        race->armed = false;
        race->scrubbedOffset = static_cast<uint64_t>(at - race->base);
        race->scrubbedLength = length;
        race->during = race->memory->allocate(length, 2);
        if (race->during) {
            std::memset(race->base + race->during->offset, 0xCD, race->during->length);
        }
    }
    std::memset(at, 0, length);
}

// pr7 (seen as fr26 green video): a granule the trim takes back is not granted to anyone until
// its scrub has finished. Marked free before the scrub, which runs with no lock held, it went
// to the next allocation on another thread, and the late scrub zeroed what that owner had just
// written: the hwcomposer's packed picture reached the host as zeros, solid green.
static void pr7_a_granule_being_scrubbed_is_not_granted_until_the_scrub_ends() {
    FakeRegion region(16 * kMiB);
    dsa::GraphicsMemory memory;
    RacingScrub race;
    race.memory = &memory;
    race.base = region.data.data();
    EXPECT(memory.attach(region.data.data(), region.length, region.grants.data(),
                         region.grants.size(), scrubWhileAnotherThreadAllocates, &race));
    memory.seedKeys(5);
    auto first = memory.allocate(3 * kGranule, 1);
    EXPECT(first);
    memory.release(first->offset);
    race.armed = true;
    EXPECT(memory.reclaim() == 3 * kGranule);
    EXPECT(race.scrubbedOffset == first->offset && race.scrubbedLength == 3 * kGranule);
    EXPECT(race.during.has_value());
    if (race.during) {
        // Not over the granules still being scrubbed...
        const uint64_t start = race.during->offset;
        const uint64_t end = start + race.during->length;
        EXPECT(end <= first->offset || start >= first->offset + first->length);
        // ...so what its owner wrote is still there after the scrub.
        bool kept = true;
        for (uint64_t i = start; i < end; ++i) {
            kept = kept && region.data[i] == 0xCD;
        }
        EXPECT(kept);
    }
    // Once scrubbed, the granules are granted again, first fit.
    auto again = memory.allocate(3 * kGranule, 3);
    EXPECT(again && again->offset == first->offset);
    EXPECT(region.data[first->offset] == 0);
}

// pr7: the layout constants the guest kernel uses.
static void pr7_the_layout_is_the_kernels() {
    EXPECT(dsa::kGraphicsGranule == 65536);
    EXPECT(std::memcmp(dsa::kGrantsMagic, "DSAGRNT1", 8) == 0);
    EXPECT(dsa::kGrantsHeaderBytes == 64 && dsa::kGrantEntryBytes == 32);
    EXPECT(dsa::slotValue(5, dsa::kSlotBound) == 21 && dsa::slotValue(5, dsa::kSlotReleased) == 22);
    EXPECT(dsa::graphicsGrantsBytes(256 * kMiB) == 64 + 4096 * 32);
    EXPECT(dsa::graphicsAckBytes(256 * kMiB) == kGranule);
    EXPECT(dsa::graphicsAckBytes(1024 * kMiB) == 2 * kGranule);
}

int main() {
    pr7_grants_are_whole_granules_after_the_release_slots();
    pr7_an_allocation_the_region_cannot_hold_takes_the_copy_path();
    pr7_a_table_that_is_not_the_services_is_refused();
    pr7_the_guest_maps_only_a_grant_it_has_the_key_of();
    pr7_a_grant_the_guest_never_bound_comes_back_at_the_next_reclaim();
    pr7_a_held_grant_is_not_handed_out_again_until_the_guest_lets_go();
    pr7_an_ended_process_gives_back_every_grant();
    pr7_a_bind_racing_a_revoke_never_leaves_a_mapped_grant_free();
    pr7_a_restarted_renderer_takes_back_the_last_ones_grants();
    pr7_a_late_free_from_before_a_take_over_leaves_the_new_grant_alone();
    pr7_a_staging_copy_stays_inside_its_own_grant();
    pr7_a_grant_is_committed_before_the_guest_can_see_it();
    pr7_a_grant_that_cannot_be_committed_takes_the_copy_path();
    pr7_granules_are_committed_once_up_to_the_high_water_mark();
    pr7_a_region_that_cannot_be_committed_is_not_taken_over();
    pr7_a_granule_being_scrubbed_is_not_granted_until_the_scrub_ends();
    pr7_the_layout_is_the_kernels();
    if (failures != 0) {
        std::fprintf(stderr, "%d failure(s)\n", failures);
        return 1;
    }
    std::printf("graphics memory: all tests passed\n");
    return 0;
}
