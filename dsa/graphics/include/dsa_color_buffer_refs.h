// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// How long a guest process keeps its reference to a gralloc buffer's host ColorBuffer.
//
// Without virtio-gpu nothing on the host stands for a gralloc buffer: each process that uses
// the buffer opens the buffer's shared ColorBuffer (dsa_shared_color_buffer.h), and the host
// frees the ColorBuffer, and what it holds, as soon as no process has it open. A buffer only
// the GPU writes holds its pixels nowhere else: the guest memory minigbm allocated for it
// stays as it was made.
//
// A process closed its reference when the last Vulkan object on the buffer went (the memory
// that imported it, a window image made on it). That is not when the process lets go of the
// buffer. A hardware bitmap is the common case: hwui's uploader imports the bitmap's buffer
// as an EGLImage, the GPU copies the pixels in, and the uploader destroys the EGLImage. The
// app's render thread imports the buffer again when it first draws the bitmap, which for a
// bitmap decoded ahead (a feed's thumbnails, an ad, avatars) is frames or seconds later. In
// between the process had no Vulkan object on the buffer, so it closed its reference, the
// host freed the ColorBuffer with the pixels, and the render thread's import made a new,
// empty one: every such image showed as an empty box.
//
// So the reference now lasts as long as the process has the buffer itself. A process has a
// gralloc buffer while it has the buffer's file descriptors open: the mapper closes them when
// the last AHardwareBuffer on the handle goes. Each reference remembers which files those
// descriptors were (device and inode), and once it has no Vulkan holder left it is closed
// only when none of them is still open as the same file. A descriptor number the process
// reuses for another file no longer counts. Checking costs one fstat for each descriptor of
// each buffer with no holder, at most once a second, so what the process let go of is closed
// within about a second of a later check. A check runs at each submit and each time one of the
// process's Vulkan objects on a buffer goes: an app in the background submits nothing, but its
// window images and the memory of its caches go as it is hidden and trimmed, and the buffers it
// let go of before then close with them instead of staying on the host until it draws again.
//
// The host keeps the ColorBuffer's memory for as long as the app keeps the buffer, as a
// device's GPU memory does for its gralloc buffers, and frees it when the app frees the
// buffer or ends.

#pragma once

#include <sys/stat.h>

#include <cstddef>
#include <cstdint>
#include <unordered_map>
#include <utility>
#include <vector>

namespace dsa {

// Which file a descriptor refers to now. False when it is not open.
inline bool identifyOpenFile(int fd, uint64_t* device, uint64_t* inode) {
    if (fd < 0) return false;
    struct stat st = {};
    if (fstat(fd, &st) != 0) return false;
    *device = static_cast<uint64_t>(st.st_dev);
    *inode = static_cast<uint64_t>(st.st_ino);
    return true;
}

class ColorBufferRefs {
   public:
    using IdentifyFile = bool (*)(int fd, uint64_t* device, uint64_t* inode);

    // A reference to close with the host, and the buffer it was for.
    struct Closed {
        uint32_t bufferId = 0;
        uint32_t hostHandle = 0;
    };

    // The shortest time between two sweeps.
    static constexpr uint64_t kSweepIntervalNs = 1000000000ULL;
    // The most descriptors remembered for one buffer: one AHardwareBuffer's planes share one
    // file, and a process that receives the buffer more than once has a handle for each.
    static constexpr size_t kMaxFiles = 8;

    explicit ColorBufferRefs(IdentifyFile identify = identifyOpenFile) : identify_(identify) {}

    // The host handle this process has open for the buffer, or 0.
    uint32_t find(uint32_t bufferId) const {
        auto it = refs_.find(bufferId);
        return it == refs_.end() ? 0 : it->second.hostHandle;
    }

    // The buffer whose reference has this host handle, or 0.
    uint32_t bufferOf(uint32_t hostHandle) const {
        for (const auto& [id, ref] : refs_) {
            if (ref.hostHandle == hostHandle) return id;
        }
        return 0;
    }

    // The process opened `hostHandle` for the buffer, whose handle has `fds`. False when it
    // already had a reference (another thread opened it meanwhile): the caller closes the one
    // it opened, so the process keeps one.
    bool opened(uint32_t bufferId, uint32_t hostHandle, const int* fds, size_t fdCount) {
        auto [it, added] = refs_.emplace(bufferId, Ref{hostHandle, 0, {}});
        remember(it->second, fds, fdCount);
        return added;
    }

    // One more Vulkan object holds the reference; the buffer's handle has `fds`.
    void hold(uint32_t bufferId, uint32_t hostHandle, const int* fds, size_t fdCount) {
        auto it = refs_.find(bufferId);
        if (it == refs_.end() || it->second.hostHandle != hostHandle) return;
        it->second.holders++;
        remember(it->second, fds, fdCount);
    }

    // One holder went. The reference to close now, when no holder is left and the process
    // no longer has the buffer; otherwise nothing, and a later sweep closes it.
    Closed drop(uint32_t bufferId) {
        auto it = refs_.find(bufferId);
        if (it == refs_.end()) return {};
        if (it->second.holders > 0) it->second.holders--;
        if (it->second.holders > 0 || stillOpen(it->second)) return {};
        const Closed closed{bufferId, it->second.hostHandle};
        refs_.erase(it);
        return closed;
    }

    // Whether a sweep is due at `nowNs` (a monotonic clock). True at most once an interval.
    bool sweepDue(uint64_t nowNs) {
        if (lastSweepNs_ != 0 && nowNs - lastSweepNs_ < kSweepIntervalNs) return false;
        lastSweepNs_ = nowNs == 0 ? 1 : nowNs;
        return true;
    }

    // Takes out every reference with no holder whose buffer the process no longer has, for
    // the caller to close.
    std::vector<Closed> sweep() {
        std::vector<Closed> closed;
        for (auto it = refs_.begin(); it != refs_.end();) {
            if (it->second.holders == 0 && !stillOpen(it->second)) {
                closed.push_back(Closed{it->first, it->second.hostHandle});
                it = refs_.erase(it);
            } else {
                ++it;
            }
        }
        return closed;
    }

    size_t size() const { return refs_.size(); }

    uint32_t holders(uint32_t bufferId) const {
        auto it = refs_.find(bufferId);
        return it == refs_.end() ? 0 : it->second.holders;
    }

   private:
    struct File {
        int fd = -1;
        uint64_t device = 0;
        uint64_t inode = 0;
    };

    struct Ref {
        uint32_t hostHandle = 0;
        // The Vulkan objects on the buffer: the device memory that imported it, each window
        // image made on it.
        uint32_t holders = 0;
        // The buffer's descriptors as they were when the process used them.
        std::vector<File> files;
    };

    void remember(Ref& ref, const int* fds, size_t fdCount) {
        for (size_t i = 0; fds && i < fdCount; i++) {
            File file;
            file.fd = fds[i];
            if (!identify_(file.fd, &file.device, &file.inode)) continue;
            bool known = false;
            for (File& old : ref.files) {
                if (old.fd == file.fd) {
                    // The number may name another file now; the newest is the buffer's.
                    old = file;
                    known = true;
                    break;
                }
            }
            if (!known && ref.files.size() < kMaxFiles) ref.files.push_back(file);
        }
    }

    bool stillOpen(const Ref& ref) const {
        for (const File& file : ref.files) {
            uint64_t device = 0;
            uint64_t inode = 0;
            if (identify_(file.fd, &device, &inode) && device == file.device &&
                inode == file.inode) {
                return true;
            }
        }
        return false;
    }

    IdentifyFile identify_;
    std::unordered_map<uint32_t, Ref> refs_;
    uint64_t lastSweepNs_ = 0;
};

}  // namespace dsa
