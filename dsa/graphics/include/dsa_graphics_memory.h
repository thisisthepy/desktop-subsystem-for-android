// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The graphics memory the host shares with the guest: Vulkan host-visible memory, and the
// staging memory a gralloc buffer's CPU upload or read back goes through, without a copy
// through the stream.
//
// Without it, a Hyper-V VM has no memory the guest and the host renderer share. The guest
// driver keeps a copy of every host-visible allocation, finds what changed in it before each
// submit, sends that through the stream and reads back what the device wrote, and an app that
// uploads a lot spends a CPU on it.
//
// The host's service makes two sections when it makes the VM and attaches both to it as HCS
// shared memory regions:
//
// - the data region, which the guest may write: what the allocations hold. Its first
//   graphicsAckBytes() bytes are the release slots (below) and are never handed out.
// - the grants table, which the guest may only read: a header, then one entry for each
//   kGraphicsGranule of the data region.
//
// The renderer maps both. When a guest process allocates host-visible memory, the renderer
// takes whole granules of the data region (first fit), imports them into Vulkan
// (VK_EXT_external_memory_host) and writes the grant's entry: a random 64-bit key, the length
// and a sequence number. The key goes only to the process that allocated, on its own stream.
// The guest kernel's driver lets a file map the granules only once the file has shown the
// key, and lets one file at a time hold a grant, so an app maps only its own allocations.
//
// A grant is let go of in two halves. The renderer revokes it (zeroes the key) when the guest
// frees the memory, when the process ends, or when the renderer starts again. The guest kernel
// lets go of it when the file holding it releases it or closes: it takes away every mapping of
// the granules, then writes "released" in the grant's release slot. The renderer hands the
// granules out again only once it has seen that, so no app ever maps another's memory, and
// scrubs them (zeroes them and gives the pages back) first.
//
// The two sides meet without a lock, as two flags do (Dekker's protocol). The guest, to bind,
// reads the entry, writes "bound" in the slot, then reads the key again and backs out if it is
// gone. The renderer, to revoke, zeroes the key, then reads the slot. A full fence sits
// between the write and the read on both sides, so at least one of them sees the other: either
// the renderer sees "bound" and waits for "released", or the guest sees the key gone.
//
// The sections only reserve their size (SEC_RESERVE on Windows): a section made committed
// charges all of it against the system's commit limit at once, 1 GiB for a 1 GiB region,
// whether or not anything is ever granted. The service commits what both sides read from the
// start (the grants table and the release slots) when it makes them. The renderer commits a
// grant's granules in its own view before it writes the grant's key, so neither the guest nor
// a driver ever touches a page that is not committed; a grant whose granules cannot be
// committed (the commit limit is reached) is refused, and the allocation takes the copy path.
// A granule stays committed once it is (Windows gives the commit back only when the section
// closes, with the VM), so the renderer keeps a bitmap of them and commits each one once. Its
// pages still go back to the system when it is scrubbed.
//
// Entry i describes the grant that starts at byte i * kGraphicsGranule of the data region.
// The layout is the guest kernel's too (dsa/kernel/patches/dsa-shared-memory.patch, which
// a test keeps in step with these constants).

#pragma once

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <optional>
#include <random>
#include <vector>

namespace dsa {

// The unit the data region is handed out in. A multiple of every driver's
// minImportedHostPointerAlignment seen (4 KiB and 64 KiB) and of the guest's largest page.
inline constexpr uint64_t kGraphicsGranule = 64 * 1024;

// The grants table's header: the magic, then little-endian u64s at these offsets.
inline constexpr char kGrantsMagic[8] = {'D', 'S', 'A', 'G', 'R', 'N', 'T', '1'};
inline constexpr uint64_t kGrantsHeaderBytes = 64;
inline constexpr uint64_t kGrantsRegionIdAt = 8;
inline constexpr uint64_t kGrantsDataLengthAt = 16;
inline constexpr uint64_t kGrantsGranuleAt = 24;
inline constexpr uint64_t kGrantsAckBytesAt = 32;
// How many times a renderer has taken the table over (it counts up at each start).
inline constexpr uint64_t kGrantsGenerationAt = 40;
// The next sequence number, kept in the table so a renderer that starts again never reuses one.
inline constexpr uint64_t kGrantsNextSeqAt = 48;

// An entry: key, length in bytes, sequence number, owner (the guest process, for logs).
inline constexpr uint64_t kGrantEntryBytes = 32;
inline constexpr uint64_t kGrantKeyAt = 0;
inline constexpr uint64_t kGrantLengthAt = 8;
inline constexpr uint64_t kGrantSeqAt = 16;
inline constexpr uint64_t kGrantOwnerAt = 24;

// A release slot holds (seq << 2) | state, written by the guest kernel.
inline constexpr uint64_t kSlotBound = 1;
inline constexpr uint64_t kSlotReleased = 2;

inline constexpr uint64_t slotValue(uint64_t seq, uint64_t state) { return (seq << 2) | state; }

inline constexpr uint64_t graphicsGranules(uint64_t dataLength) {
    return dataLength / kGraphicsGranule;
}

// The release slots at the start of the data region, in whole granules.
inline constexpr uint64_t graphicsAckBytes(uint64_t dataLength) {
    return (graphicsGranules(dataLength) * 8 + kGraphicsGranule - 1) / kGraphicsGranule *
           kGraphicsGranule;
}

// The bytes the grants table needs for a data region of `dataLength`.
inline constexpr uint64_t graphicsGrantsBytes(uint64_t dataLength) {
    return kGrantsHeaderBytes + graphicsGranules(dataLength) * kGrantEntryBytes;
}

// Plain loads and stores of the shared memory, which the other side changes under us. Aligned
// 8-byte accesses are single accesses on x86-64 and arm64.
inline uint64_t sharedLoad(const uint8_t* at) {
    return *reinterpret_cast<const volatile uint64_t*>(at);
}
inline void sharedStore(uint8_t* at, uint64_t value) {
    *reinterpret_cast<volatile uint64_t*>(at) = value;
}

// Writes the grants table's header for a new region (the service does, when it makes the
// sections). The entries must be zero.
inline void writeGrantsHeader(uint8_t* grants, uint64_t regionId, uint64_t dataLength) {
    std::memcpy(grants, kGrantsMagic, sizeof(kGrantsMagic));
    sharedStore(grants + kGrantsRegionIdAt, regionId);
    sharedStore(grants + kGrantsDataLengthAt, dataLength);
    sharedStore(grants + kGrantsGranuleAt, kGraphicsGranule);
    sharedStore(grants + kGrantsAckBytesAt, graphicsAckBytes(dataLength));
    sharedStore(grants + kGrantsGenerationAt, 0);
    sharedStore(grants + kGrantsNextSeqAt, 1);
}

// A grant: where it starts in the data region, how long it is, and its key.
struct GraphicsGrant {
    uint64_t offset = 0;
    uint64_t length = 0;
    uint64_t key = 0;
    // Which attach of the region it is from: a grant of a region taken over again (the next
    // boot) is no longer the allocator's to revoke.
    uint64_t generation = 0;
};

// Why an allocation did not get a grant, for the renderer's one log line each.
enum class GrantRefusal {
    NotRefused,
    NoRegion,  // the renderer has no region (the service made none, or it is off)
    TooLarge,  // larger than the region
    Full,      // no run of free granules is long enough
    CommitFailed,  // the system would not commit the granules (its commit limit is reached)
};

inline const char* grantRefusalText(GrantRefusal why) {
    switch (why) {
        case GrantRefusal::NotRefused:
            return "none";
        case GrantRefusal::NoRegion:
            return "there is no graphics memory shared with the guest";
        case GrantRefusal::TooLarge:
            return "the allocation is larger than the memory shared with the guest";
        case GrantRefusal::Full:
            return "the memory shared with the guest is full";
        case GrantRefusal::CommitFailed:
            return "the system would not commit more of the memory shared with the guest (its "
                   "commit limit is reached)";
    }
    return "unknown";
}

// The renderer's side: hands out the data region and keeps the grants table.
//
// Thread safe. `scrub` is called with no lock held on a range nothing maps any more, to zero
// it and give its pages back to the system; it must leave the range reading as zeros.
// `commit` is called with the lock held on a range of this process's view of a section, to
// commit its pages (the sections only reserve their size); false when the system refuses. A
// null `commit` takes every page as committed already (memory that is not a reserved section).
class GraphicsMemory {
   public:
    using Scrub = void (*)(void* context, uint8_t* at, uint64_t length);
    using Commit = bool (*)(void* context, uint8_t* at, uint64_t length);

    struct Stats {
        uint64_t regionBytes = 0;
        uint64_t grantedBytes = 0;
        // Revoked by the renderer, still held by the guest.
        uint64_t heldBytes = 0;
        // The granules of the data region committed so far: the high-water mark of the grants.
        uint64_t committedBytes = 0;
        uint32_t grants = 0;
    };

    // Takes over the region: `data` (`dataLength` bytes) and the grants table (`grants`,
    // `grantsLength` bytes) the service made. Every grant a renderer before this one wrote is
    // revoked; the granules a guest still holds come back once it lets them go. False, with
    // nothing taken, when the table is not the service's (its magic, length or granule differ),
    // the region is smaller than its release slots, or the table and the release slots cannot
    // be committed; `failure`, when given, then says which.
    bool attach(uint8_t* data, uint64_t dataLength, uint8_t* grants, uint64_t grantsLength,
                Scrub scrub, void* scrubContext, Commit commit = nullptr,
                void* commitContext = nullptr, const char** failure = nullptr) {
        std::vector<Range> scrubs;
        {
            std::lock_guard<std::mutex> lock(mMutex);
            mData = nullptr;
            mGrants = nullptr;
            mCommitted.clear();
            if (failure) *failure = "the memory the service shares is not a grants table this "
                                    "renderer knows";
            if (!data || !grants || grantsLength < kGrantsHeaderBytes) return false;
            // The service committed the table when it made it; committing it again costs
            // nothing and makes sure, before anything here reads it.
            if (commit && !commit(commitContext, grants, grantsLength)) {
                if (failure) *failure = "the grants table of the memory the service shares "
                                        "could not be committed";
                return false;
            }
            if (std::memcmp(grants, kGrantsMagic, sizeof(kGrantsMagic)) != 0 ||
                sharedLoad(grants + kGrantsDataLengthAt) != dataLength ||
                sharedLoad(grants + kGrantsGranuleAt) != kGraphicsGranule ||
                dataLength % kGraphicsGranule != 0 ||
                graphicsGrantsBytes(dataLength) > grantsLength ||
                graphicsAckBytes(dataLength) >= dataLength) {
                return false;
            }
            mData = data;
            mDataLength = dataLength;
            mCommit = commit;
            mCommitContext = commitContext;
            mCommitted.assign(graphicsGranules(dataLength), false);
            // The release slots, which the guest writes and the take over below reads.
            if (!commitLocked(0, graphicsAckBytes(dataLength) / kGraphicsGranule)) {
                mData = nullptr;
                mDataLength = 0;
                mCommitted.clear();
                if (failure) *failure = "the release slots of the memory the service shares "
                                        "could not be committed";
                return false;
            }
            mGrants = grants;
            mScrub = scrub;
            mScrubContext = scrubContext;
            ++mGeneration;
            sharedStore(grants + kGrantsGenerationAt,
                        sharedLoad(grants + kGrantsGenerationAt) + 1);
            takeOverLocked(&scrubs);
        }
        runScrubs(scrubs);
        return true;
    }

    // Lets go of the region: nothing is handed out any more. What the guest maps stays its own
    // until it lets go of it.
    void detach() {
        std::lock_guard<std::mutex> lock(mMutex);
        mData = nullptr;
        mDataLength = 0;
        mGrants = nullptr;
        mState.clear();
        mGrantsByStart.clear();
        mCommitted.clear();
    }

    bool attached() const {
        std::lock_guard<std::mutex> lock(mMutex);
        return mData != nullptr;
    }

    // The region's id, from the table's header, which the guest compares with the one it maps.
    uint64_t regionId() const {
        std::lock_guard<std::mutex> lock(mMutex);
        return mGrants ? sharedLoad(mGrants + kGrantsRegionIdAt) : 0;
    }

    uint64_t length() const {
        std::lock_guard<std::mutex> lock(mMutex);
        return mDataLength;
    }

    // A grant of at least `size` bytes, in whole granules, for the guest process `owner`.
    // Granules the guest has let go of since are taken back first, and scrubbed before the
    // grant is chosen, so this grant may use them.
    std::optional<GraphicsGrant> allocate(uint64_t size, uint64_t owner,
                                          GrantRefusal* why = nullptr) {
        std::vector<Range> scrubs;
        {
            std::lock_guard<std::mutex> lock(mMutex);
            if (mData) reclaimLocked(&scrubs);
        }
        runScrubs(scrubs);
        std::optional<GraphicsGrant> grant;
        GrantRefusal refusal = GrantRefusal::NotRefused;
        {
            std::lock_guard<std::mutex> lock(mMutex);
            grant = allocateLocked(size, owner, &refusal);
        }
        if (why) *why = refusal;
        return grant;
    }

    // Revokes the grant at `offset`. Its granules come back at the next reclaim once the guest
    // does not hold them: at once when it never bound the grant or has let go of it already.
    // A `generation` other than 0 names the attach the grant came from; a grant of an earlier
    // one is left alone, since its offset may be another grant's now.
    void release(uint64_t offset, uint64_t generation = 0) {
        std::vector<Range> scrubs;
        {
            std::lock_guard<std::mutex> lock(mMutex);
            if (generation != 0 && generation != mGeneration) return;
            revokeLocked(offset, &scrubs);
        }
        runScrubs(scrubs);
    }

    // Revokes every grant of the guest process `owner`, which has ended.
    void releaseOwner(uint64_t owner) {
        std::vector<Range> scrubs;
        {
            std::lock_guard<std::mutex> lock(mMutex);
            std::vector<uint64_t> starts;
            for (const auto& entry : mGrantsByStart) {
                if (entry.owner == owner && entry.live) starts.push_back(entry.start);
            }
            for (uint64_t start : starts) revokeLocked(start * kGraphicsGranule, &scrubs);
        }
        runScrubs(scrubs);
    }

    // Takes back the granules the guest has let go of since they were revoked. Returns the
    // bytes taken back.
    uint64_t reclaim() {
        std::vector<Range> scrubs;
        {
            std::lock_guard<std::mutex> lock(mMutex);
            reclaimLocked(&scrubs);
        }
        uint64_t bytes = 0;
        for (const Range& range : scrubs) bytes += range.length;
        runScrubs(scrubs);
        return bytes;
    }

    // The renderer's pointer to `offset` of the data region, or null outside it.
    uint8_t* pointer(uint64_t offset) const {
        std::lock_guard<std::mutex> lock(mMutex);
        return mData && offset < mDataLength ? mData + offset : nullptr;
    }

    // Where `pointer` is in the data region, if it is in it.
    std::optional<uint64_t> offsetOf(const void* pointer) const {
        std::lock_guard<std::mutex> lock(mMutex);
        const uint8_t* at = static_cast<const uint8_t*>(pointer);
        if (!mData || at < mData || at >= mData + mDataLength) return std::nullopt;
        return static_cast<uint64_t>(at - mData);
    }

    // Whether `size` bytes from `pointer` lie inside one live grant of `owner`: what a staging
    // copy the guest names may touch.
    bool owns(const void* pointer, uint64_t size, uint64_t owner) const {
        std::lock_guard<std::mutex> lock(mMutex);
        const uint8_t* at = static_cast<const uint8_t*>(pointer);
        if (!mData || at < mData || at >= mData + mDataLength) return false;
        const uint64_t offset = static_cast<uint64_t>(at - mData);
        const Grant* grant = grantAtLocked(offset / kGraphicsGranule);
        if (!grant || !grant->live || grant->owner != owner) return false;
        const uint64_t start = grant->start * kGraphicsGranule;
        const uint64_t end = start + grant->granules * kGraphicsGranule;
        return size <= end - offset;
    }

    Stats stats() const {
        std::lock_guard<std::mutex> lock(mMutex);
        Stats stats;
        stats.regionBytes = mDataLength;
        for (bool committed : mCommitted) {
            if (committed) stats.committedBytes += kGraphicsGranule;
        }
        for (const Grant& grant : mGrantsByStart) {
            const uint64_t bytes = grant.granules * kGraphicsGranule;
            if (grant.live) {
                stats.grantedBytes += bytes;
                stats.grants++;
            } else {
                stats.heldBytes += bytes;
            }
        }
        return stats;
    }

    // For tests only: keys from a seeded generator, so a test can repeat them.
    void seedKeys(uint64_t seed) {
        std::lock_guard<std::mutex> lock(mMutex);
        mRandom.seed(seed);
        mSeeded = true;
    }

   private:
    // Unusable: a granule a renderer before this one granted that cannot be committed now, so
    // it is never scrubbed (that would touch it) nor handed out again.
    //
    // Scrubbing: taken back, and waiting for its scrub, which runs with no lock held. It becomes
    // Free only once the scrub has finished. A granule marked Free before its scrub could be
    // granted on another render thread meanwhile (the trim's reclaim runs once a second beside
    // every allocation), and the late scrub then zeroed and discarded what the new owner had
    // just written: a hwcomposer's packed video picture read as zeros (solid green), or an
    // app's fresh vertex data as nothing.
    enum class Granule : uint8_t { Free, Busy, Slots, Unusable, Scrubbing };

    struct Grant {
        uint64_t start = 0;  // in granules
        uint64_t granules = 0;
        uint64_t seq = 0;
        uint64_t owner = 0;
        // False once revoked: the granules wait for the guest to let go of them.
        bool live = true;
    };

    struct Range {
        uint8_t* at;
        uint64_t length;
        // The granules it covers, and the attach they belong to: they become Free once
        // scrubbed, unless the region was taken over meanwhile.
        uint64_t start;
        uint64_t granules;
        uint64_t generation;
    };

    uint8_t* entry(uint64_t index) const {
        return mGrants + kGrantsHeaderBytes + index * kGrantEntryBytes;
    }
    uint8_t* slot(uint64_t index) const { return mData + index * 8; }

    const Grant* grantAtLocked(uint64_t granule) const {
        for (const Grant& grant : mGrantsByStart) {
            if (granule >= grant.start && granule < grant.start + grant.granules) return &grant;
        }
        return nullptr;
    }

    // From the system's random source: an app sees the keys of its own grants, and a
    // generator whose state those reveal would let it work out another app's.
    uint64_t nextKeyLocked() {
        uint64_t key = 0;
        while (key == 0) {
            if (mSeeded) {
                key = mRandom();
            } else {
                std::random_device device;
                key = (static_cast<uint64_t>(device()) << 32) ^ device();
            }
        }
        return key;
    }

    uint64_t nextSeqLocked() {
        uint64_t seq = sharedLoad(mGrants + kGrantsNextSeqAt);
        if (seq == 0) seq = 1;
        sharedStore(mGrants + kGrantsNextSeqAt, seq + 1);
        return seq;
    }

    // Commits the granules [start, start + count) that are not committed yet, in runs, and
    // marks them. False when the system refuses a run; the runs committed before it stay
    // marked, since they are committed.
    bool commitLocked(uint64_t start, uint64_t count) {
        const uint64_t end = start + count;
        if (end > mCommitted.size()) return false;
        uint64_t index = start;
        while (index < end) {
            if (mCommitted[index]) {
                ++index;
                continue;
            }
            uint64_t runEnd = index;
            while (runEnd < end && !mCommitted[runEnd]) ++runEnd;
            if (mCommit && !mCommit(mCommitContext, mData + index * kGraphicsGranule,
                                    (runEnd - index) * kGraphicsGranule)) {
                return false;
            }
            for (; index < runEnd; ++index) mCommitted[index] = true;
        }
        return true;
    }

    // Whether every granule of [start, start + count) is committed already.
    bool committedLocked(uint64_t start, uint64_t count) const {
        if (start + count > mCommitted.size()) return false;
        for (uint64_t i = start; i < start + count; ++i) {
            if (!mCommitted[i]) return false;
        }
        return true;
    }

    // Whether the guest kernel shows grant `seq` at `index` as bound and not yet released.
    bool guestHoldsLocked(uint64_t index, uint64_t seq) const {
        return sharedLoad(slot(index)) == slotValue(seq, kSlotBound);
    }

    // Takes the grant's granules back. They stay out of every allocation until runScrubs has
    // scrubbed them.
    void freeLocked(const Grant& grant, std::vector<Range>* scrubs) {
        for (uint64_t i = 0; i < grant.granules; ++i) {
            mState[grant.start + i] = Granule::Scrubbing;
        }
        scrubs->push_back(Range{mData + grant.start * kGraphicsGranule,
                                grant.granules * kGraphicsGranule, grant.start, grant.granules,
                                mGeneration});
    }

    void takeOverLocked(std::vector<Range>* scrubs) {
        const uint64_t granules = graphicsGranules(mDataLength);
        const uint64_t slotGranules = graphicsAckBytes(mDataLength) / kGraphicsGranule;
        mState.assign(granules, Granule::Free);
        mGrantsByStart.clear();
        for (uint64_t i = 0; i < slotGranules; ++i) mState[i] = Granule::Slots;
        // The grants a renderer before this one made: revoked now; the guest's are waited for.
        for (uint64_t index = slotGranules; index < granules;) {
            uint8_t* at = entry(index);
            const uint64_t key = sharedLoad(at + kGrantKeyAt);
            const uint64_t length = sharedLoad(at + kGrantLengthAt);
            const uint64_t seq = sharedLoad(at + kGrantSeqAt);
            const uint64_t count = (length + kGraphicsGranule - 1) / kGraphicsGranule;
            if (key == 0 || count == 0 || index + count > granules) {
                ++index;
                continue;
            }
            sharedStore(at + kGrantKeyAt, 0);
            std::atomic_thread_fence(std::memory_order_seq_cst);
            // The renderer before committed the grant before it wrote the key, so this commits
            // nothing new; it marks the granules, which the scrub below then touches. Granules
            // it cannot commit were never touched, and are left out for good.
            if (!commitLocked(index, count)) {
                for (uint64_t i = 0; i < count; ++i) mState[index + i] = Granule::Unusable;
                index += count;
                continue;
            }
            Grant grant{index, count, seq, sharedLoad(at + kGrantOwnerAt), false};
            for (uint64_t i = 0; i < count; ++i) mState[index + i] = Granule::Busy;
            if (guestHoldsLocked(index, seq)) {
                mGrantsByStart.push_back(grant);
            } else {
                freeLocked(grant, scrubs);
            }
            index += count;
        }
    }

    void reclaimLocked(std::vector<Range>* scrubs) {
        for (size_t i = 0; i < mGrantsByStart.size();) {
            const Grant grant = mGrantsByStart[i];
            if (!grant.live && !guestHoldsLocked(grant.start, grant.seq)) {
                freeLocked(grant, scrubs);
                mGrantsByStart.erase(mGrantsByStart.begin() + static_cast<std::ptrdiff_t>(i));
                continue;
            }
            ++i;
        }
    }

    std::optional<GraphicsGrant> allocateLocked(uint64_t size, uint64_t owner,
                                                GrantRefusal* why) {
        if (!mData) {
            *why = GrantRefusal::NoRegion;
            return std::nullopt;
        }
        const uint64_t count = size == 0 ? 1 : (size + kGraphicsGranule - 1) / kGraphicsGranule;
        if (count > mState.size()) {
            *why = GrantRefusal::TooLarge;
            return std::nullopt;
        }
        uint64_t run = 0;
        // Once the system refuses a commit, only granules committed already are taken.
        bool commitRefused = false;
        for (uint64_t index = 0; index < mState.size(); ++index) {
            run = mState[index] == Granule::Free ? run + 1 : 0;
            if (run < count) continue;
            const uint64_t start = index + 1 - count;
            // Committed before the key is written below, which is what lets the guest map the
            // granules, and before the grant goes to the caller: nothing ever touches a page
            // that is only reserved.
            if (commitRefused ? !committedLocked(start, count) : !commitLocked(start, count)) {
                commitRefused = true;
                continue;
            }
            for (uint64_t i = 0; i < count; ++i) mState[start + i] = Granule::Busy;
            Grant grant{start, count, nextSeqLocked(), owner, true};
            mGrantsByStart.push_back(grant);
            const uint64_t key = nextKeyLocked();
            uint8_t* at = entry(start);
            // The key goes last: an entry with a key is whole.
            sharedStore(at + kGrantLengthAt, count * kGraphicsGranule);
            sharedStore(at + kGrantSeqAt, grant.seq);
            sharedStore(at + kGrantOwnerAt, owner);
            std::atomic_thread_fence(std::memory_order_release);
            sharedStore(at + kGrantKeyAt, key);
            std::atomic_thread_fence(std::memory_order_seq_cst);
            return GraphicsGrant{start * kGraphicsGranule, count * kGraphicsGranule, key,
                                 mGeneration};
        }
        *why = commitRefused ? GrantRefusal::CommitFailed : GrantRefusal::Full;
        return std::nullopt;
    }

    void revokeLocked(uint64_t offset, std::vector<Range>* scrubs) {
        if (!mData || offset % kGraphicsGranule != 0) return;
        const uint64_t start = offset / kGraphicsGranule;
        for (size_t i = 0; i < mGrantsByStart.size(); ++i) {
            Grant& grant = mGrantsByStart[i];
            if (grant.start != start || !grant.live) continue;
            sharedStore(entry(start) + kGrantKeyAt, 0);
            // Between zeroing the key and any later read of the slot: the guest's bind writes
            // the slot, then reads the key, so one of the two sees the other.
            std::atomic_thread_fence(std::memory_order_seq_cst);
            // Taken back by the next reclaim (the renderer's trim, once a second, or the next
            // allocation), not here: the caller may hold the decoder's lock, and scrubbing a
            // large grant takes milliseconds.
            grant.live = false;
            (void)scrubs;
            return;
        }
    }

    void runScrubs(const std::vector<Range>& scrubs) {
        Scrub scrub;
        void* context;
        {
            std::lock_guard<std::mutex> lock(mMutex);
            scrub = mScrub;
            context = mScrubContext;
        }
        for (const Range& range : scrubs) {
            if (scrub) {
                scrub(context, range.at, range.length);
            } else {
                std::memset(range.at, 0, range.length);
            }
        }
        if (scrubs.empty()) return;
        // Only now may the granules be granted again: whatever their next owner writes comes
        // after the scrub, never before it.
        std::lock_guard<std::mutex> lock(mMutex);
        for (const Range& range : scrubs) {
            if (range.generation != mGeneration) continue;
            for (uint64_t i = range.start; i < range.start + range.granules && i < mState.size();
                 ++i) {
                if (mState[i] == Granule::Scrubbing) mState[i] = Granule::Free;
            }
        }
    }

    mutable std::mutex mMutex;
    uint8_t* mData = nullptr;
    uint64_t mDataLength = 0;
    uint8_t* mGrants = nullptr;
    Scrub mScrub = nullptr;
    void* mScrubContext = nullptr;
    Commit mCommit = nullptr;
    void* mCommitContext = nullptr;
    // One bit for each granule of the data region: committed in the section. Only ever set
    // while attached, since a committed page stays committed until the section closes.
    std::vector<bool> mCommitted;
    std::vector<Granule> mState;
    std::vector<Grant> mGrantsByStart;
    std::mt19937_64 mRandom;
    bool mSeeded = false;
    uint64_t mGeneration = 0;
};

// The renderer's one region. A function-local static, so every translation unit of the
// renderer's library sees the same one.
inline GraphicsMemory& graphicsMemory() {
    static GraphicsMemory memory;
    return memory;
}

// Which guest process the calling render thread serves. The embedder sets it (the render
// thread knows its process from rcSetPuid); 0 until then.
using GraphicsOwnerLookup = uint64_t (*)();
inline std::atomic<GraphicsOwnerLookup>& graphicsOwnerLookup() {
    static std::atomic<GraphicsOwnerLookup> lookup{nullptr};
    return lookup;
}
inline uint64_t currentGraphicsOwner() {
    GraphicsOwnerLookup lookup = graphicsOwnerLookup().load();
    return lookup ? lookup() : 0;
}

// Says each reason the region was not used once, through `log`.
class GraphicsRefusalLog {
   public:
    template <typename Log>
    void note(GrantRefusal why, Log log) {
        const size_t index = static_cast<size_t>(why);
        if (index >= kReasons || mSaid[index].exchange(true)) return;
        log(grantRefusalText(why));
    }

   private:
    static constexpr size_t kReasons = 5;
    std::atomic<bool> mSaid[kReasons] = {};
};

// The guest kernel's side of the grants, as dsa_graphics_memory.c does it, for tests: binding
// a grant to a file, which may then map it, and letting it go.
class GuestGrantTable {
   public:
    GuestGrantTable(uint8_t* data, const uint8_t* grants) : mData(data), mGrants(grants) {
        const uint64_t length = sharedLoad(grants + kGrantsDataLengthAt);
        mBound.assign(graphicsGranules(length), 0);
        mAckBytes = graphicsAckBytes(length);
        mLength = length;
    }

    // DSA_GRAPHICS_MEMORY_BIND from `file`: 0, or the errno the driver returns.
    int bind(uint64_t file, uint64_t offset, uint64_t length, uint64_t key) {
        if (offset % kGraphicsGranule || offset < mAckBytes || offset >= mLength || key == 0) {
            return 22;  // EINVAL
        }
        const uint64_t index = offset / kGraphicsGranule;
        const uint8_t* at = entry(index);
        if (sharedLoad(at + kGrantKeyAt) != key) return 13;  // EACCES
        std::atomic_thread_fence(std::memory_order_acquire);
        const uint64_t seq = sharedLoad(at + kGrantSeqAt);
        if (sharedLoad(at + kGrantLengthAt) != length || length > mLength - offset) return 13;
        if (mBound[index]) return 16;  // EBUSY
        sharedStore(mData + index * 8, slotValue(seq, kSlotBound));
        std::atomic_thread_fence(std::memory_order_seq_cst);
        if (sharedLoad(at + kGrantKeyAt) != key) {
            sharedStore(mData + index * 8, slotValue(seq, kSlotReleased));
            return 13;
        }
        mBound[index] = file;
        mBindings.push_back(Binding{file, index, length, seq});
        return 0;
    }

    // Whether `file` may map `size` bytes at `offset`: inside one grant it holds.
    bool mayMap(uint64_t file, uint64_t offset, uint64_t size) const {
        for (const Binding& binding : mBindings) {
            const uint64_t start = binding.index * kGraphicsGranule;
            if (binding.file == file && offset >= start && size <= binding.length &&
                offset - start <= binding.length - size) {
                return true;
            }
        }
        return false;
    }

    // DSA_GRAPHICS_MEMORY_RELEASE from `file`.
    int release(uint64_t file, uint64_t offset) {
        for (size_t i = 0; i < mBindings.size(); ++i) {
            const Binding binding = mBindings[i];
            if (binding.file == file && binding.index * kGraphicsGranule == offset) {
                letGo(i);
                return 0;
            }
        }
        return 22;
    }

    // The file closed: every grant it holds is let go of.
    void close(uint64_t file) {
        for (size_t i = mBindings.size(); i-- > 0;) {
            if (mBindings[i].file == file) letGo(i);
        }
    }

   private:
    struct Binding {
        uint64_t file;
        uint64_t index;
        uint64_t length;
        uint64_t seq;
    };

    const uint8_t* entry(uint64_t index) const {
        return mGrants + kGrantsHeaderBytes + index * kGrantEntryBytes;
    }

    void letGo(size_t i) {
        const Binding binding = mBindings[i];
        // The driver takes the mappings away first (unmap_mapping_range), then says so.
        mBound[binding.index] = 0;
        std::atomic_thread_fence(std::memory_order_seq_cst);
        sharedStore(mData + binding.index * 8, slotValue(binding.seq, kSlotReleased));
        mBindings.erase(mBindings.begin() + static_cast<std::ptrdiff_t>(i));
    }

    uint8_t* mData;
    const uint8_t* mGrants;
    uint64_t mLength = 0;
    uint64_t mAckBytes = 0;
    std::vector<uint64_t> mBound;
    std::vector<Binding> mBindings;
};

// What the renderer answers a guest that asks, with vkGetMemoryHostAddressInfoGOOGLE, about
// its graphics memory. A guest device asks once with no memory: the answer's address is
// kGraphicsMemoryHello, its size the region's length and its id the region's id, and from then
// on the renderer puts that device's host-visible memory in the region where it can. For a
// memory, the answer is the grant (offset, length, key) or VK_ERROR_FEATURE_NOT_PRESENT for one
// that is not in the region.
inline constexpr uint64_t kGraphicsMemoryHello = 0x4453414752414648ULL;  // "DSAGRAFH"

// A staging grant over a gralloc buffer's CPU upload or read back goes to the renderer as the
// DMA address rcUpdateColorBufferDMA and rcReadColorBufferDMA carry: the grant's offset in
// the data region with this tag in its top bits, which no guest physical address the goldfish
// DMA device gives has.
inline constexpr uint64_t kGraphicsDmaTag = 0xD5A0000000000000ULL;
inline constexpr uint64_t kGraphicsDmaTagMask = 0xFFF0000000000000ULL;

// A guest process's staging grant, mapped: where a gralloc buffer's planes are packed for a
// CPU upload, or where a read back lands, instead of in the stream.
struct SharedStaging {
    uint8_t* ptr = nullptr;
    uint64_t offset = 0;
    uint64_t length = 0;

    bool holds(uint64_t bytes) const { return ptr && bytes <= length; }
    // The address rcUpdateColorBufferDMA and rcReadColorBufferDMA carry for it.
    uint64_t dmaAddress() const { return kGraphicsDmaTag | offset; }
};

// The process pipe's request for a staging grant (hwcomposer, which has no Vulkan device):
// after the handshake the guest writes {kStagingRequest, 0, size} and the renderer answers
// {0 or -errno, 0, offset, length, key}. A size of 0 lets the process's staging grant go.
inline constexpr uint32_t kStagingRequest = 0x31475344;  // "DSG1"
struct StagingRequest {
    uint32_t op;
    uint32_t reserved;
    uint64_t size;
};
struct StagingReply {
    int32_t result;
    uint32_t reserved;
    uint64_t offset;
    uint64_t length;
    uint64_t key;
};

// The process pipe's word that says who the guest process is, so the renderer's memory report
// names its device and its ColorBuffers: after the handshake the guest writes
// {kIdentifyRequest, pid, name} once, and the renderer answers nothing. The name is the
// process's, as /proc/self/cmdline starts (an app's package name), cut to fit and ended with
// a 0. A renderer that does not know the word reads it as words it drops.
inline constexpr uint32_t kIdentifyRequest = 0x31495344;  // "DSI1"
struct ProcessIdentity {
    uint32_t op;
    uint32_t pid;
    char name[56];
};
static_assert(sizeof(ProcessIdentity) == 64, "the process pipe's identity is 64 bytes");

// Writes this process's name into `name` (`size` bytes, ended with a 0): the first word of
// /proc/self/cmdline, or "" where there is none.
inline void readProcessName(char* name, size_t size) {
    if (!name || size == 0) {
        return;
    }
    name[0] = '\0';
    std::FILE* file = std::fopen("/proc/self/cmdline", "rb");
    if (!file) {
        return;
    }
    const size_t read = std::fread(name, 1, size - 1, file);
    std::fclose(file);
    name[read] = '\0';
}

// The renderer's GL extension that says it takes all of the above.
inline constexpr const char kGraphicsMemoryExtension[] = "ANDROID_EMU_dsa_shared_graphics_memory";

}  // namespace dsa
