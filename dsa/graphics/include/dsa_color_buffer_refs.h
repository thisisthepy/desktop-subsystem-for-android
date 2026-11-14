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
// gralloc buffer while it has one of the buffer's files open: the mapper closes the
// descriptors of a handle when the last AHardwareBuffer on it goes. Each reference remembers
// which files the buffer's descriptors were (device and inode), and once it has no Vulkan
// holder left it is closed only when no descriptor of the process is any of those files.
//
// The question is asked of every descriptor the process has (/proc/self/fd), not of the
// descriptor numbers seen at an import. A process holds one buffer through many handles, and
// each has descriptors of its own: SurfaceFlinger gets a buffer again over binder, a mapper
// import clones the handle, a caller dups the descriptors. Asking only the numbers once seen
// could judge a buffer the process still used as let go and close its reference; when no
// other process had the buffer open, the host freed the ColorBuffer while the process still
// imported and drew the buffer. The numbers seen are kept only as a quick yes: a number that
// still names the buffer's file answers without the listing. When the listing cannot be
// read, every buffer counts as still had.
//
// A reference whose files could not be identified at all closes with its last holder, as
// before, and never at a sweep: nothing says the process let go of it.
//
// Checking costs one fstat for each descriptor of the process, when a reference with no
// holder is not answered by its numbers, at most once a second at a sweep, so what the
// process let go of is closed within about a second of a later check. A check runs at each
// submit and each time one of the process's Vulkan objects on a buffer goes: an app in the
// background submits nothing, but its window images and the memory of its caches go as it is
// hidden and trimmed, and the buffers it let go of before then close with them instead of
// staying on the host until it draws again.
//
// The host keeps the ColorBuffer's memory for as long as the app keeps the buffer, as a
// device's GPU memory does for its gralloc buffers, and frees it when the app frees the
// buffer or ends.
//
// The caller keeps these references behind one lock, held from the decision to open or close
// a reference until the host has been sent the open or the close (MinigbmGralloc).

#pragma once

#include <dirent.h>
#include <stdlib.h>
#include <sys/stat.h>

#include <cstddef>
#include <cstdint>
#include <unordered_map>
#include <utility>
#include <vector>

namespace dsa {

// One open file, as the kernel names it.
struct FileId {
    uint64_t device = 0;
    uint64_t inode = 0;
};

inline bool operator==(const FileId& a, const FileId& b) {
    return a.device == b.device && a.inode == b.inode;
}

// Which file a descriptor refers to now. False when it is not open.
inline bool identifyOpenFile(int fd, uint64_t* device, uint64_t* inode) {
    if (fd < 0) return false;
    struct stat st = {};
    if (fstat(fd, &st) != 0) return false;
    *device = static_cast<uint64_t>(st.st_dev);
    *inode = static_cast<uint64_t>(st.st_ino);
    return true;
}

// Every file the process has open now. False when the list cannot be read.
inline bool listOpenFiles(std::vector<FileId>* files) {
    files->clear();
    DIR* dir = opendir("/proc/self/fd");
    if (!dir) return false;
    const int own = dirfd(dir);
    while (struct dirent* entry = readdir(dir)) {
        char* end = nullptr;
        const long fd = strtol(entry->d_name, &end, 10);
        // "." and "..", and the descriptor the listing itself reads through.
        if (end == entry->d_name || *end != '\0' || fd < 0 || fd == own) continue;
        FileId file;
        if (identifyOpenFile(static_cast<int>(fd), &file.device, &file.inode)) {
            files->push_back(file);
        }
    }
    closedir(dir);
    return true;
}

class ColorBufferRefs {
   public:
    using IdentifyFile = bool (*)(int fd, uint64_t* device, uint64_t* inode);
    using ListFiles = bool (*)(std::vector<FileId>* files);

    // A reference to close with the host, and the buffer it was for.
    struct Closed {
        uint32_t bufferId = 0;
        uint32_t hostHandle = 0;
    };

    // The shortest time between two sweeps.
    static constexpr uint64_t kSweepIntervalNs = 1000000000ULL;
    // The most files remembered for one buffer. A buffer's planes share one file, so this is
    // only a bound on what a malformed handle could add.
    static constexpr size_t kMaxFiles = 8;

    explicit ColorBufferRefs(IdentifyFile identify = identifyOpenFile,
                             ListFiles list = listOpenFiles)
        : identify_(identify), list_(list) {}

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
    // already had a reference: the caller closes the one it opened, so the process keeps one.
    bool opened(uint32_t bufferId, uint32_t hostHandle, const int* fds, size_t fdCount) {
        auto [it, added] = refs_.emplace(bufferId, Ref{hostHandle, 0, {}});
        remember(it->second, fds, fdCount);
        return added;
    }

    // One more Vulkan object holds the reference; the buffer's handle has `fds`. False when
    // the process has no reference with that handle, so nothing holds it.
    bool hold(uint32_t bufferId, uint32_t hostHandle, const int* fds, size_t fdCount) {
        auto it = refs_.find(bufferId);
        if (it == refs_.end() || it->second.hostHandle != hostHandle) return false;
        it->second.holders++;
        remember(it->second, fds, fdCount);
        return true;
    }

    // One holder went. The reference to close now, when no holder is left and the process
    // no longer has the buffer; otherwise nothing, and a later sweep closes it.
    Closed drop(uint32_t bufferId) {
        auto it = refs_.find(bufferId);
        if (it == refs_.end()) return {};
        if (it->second.holders > 0) it->second.holders--;
        if (it->second.holders > 0) return {};
        if (!it->second.files.empty()) {
            if (stillOpenByNumber(it->second)) return {};
            std::vector<FileId> open;
            if (!list_(&open) || stillOpen(it->second, open)) return {};
        }
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
        std::vector<uint32_t> candidates;
        for (const auto& [id, ref] : refs_) {
            if (ref.holders == 0 && !ref.files.empty() && !stillOpenByNumber(ref)) {
                candidates.push_back(id);
            }
        }
        std::vector<Closed> closed;
        if (candidates.empty()) return closed;
        std::vector<FileId> open;
        if (!list_(&open)) return closed;
        for (uint32_t id : candidates) {
            auto it = refs_.find(id);
            if (it == refs_.end() || stillOpen(it->second, open)) continue;
            closed.push_back(Closed{id, it->second.hostHandle});
            refs_.erase(it);
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
        // The descriptor last seen naming the file: a quick way to find it still open.
        int fd = -1;
        FileId id;
    };

    struct Ref {
        uint32_t hostHandle = 0;
        // The Vulkan objects on the buffer: the device memory that imported it, each window
        // image made on it.
        uint32_t holders = 0;
        // The buffer's files, one entry each, however many handles named them.
        std::vector<File> files;
    };

    void remember(Ref& ref, const int* fds, size_t fdCount) {
        for (size_t i = 0; fds && i < fdCount; i++) {
            File file;
            file.fd = fds[i];
            if (!identify_(file.fd, &file.id.device, &file.id.inode)) continue;
            bool known = false;
            for (File& old : ref.files) {
                if (old.id == file.id) {
                    // The same file under a newer number.
                    old.fd = file.fd;
                    known = true;
                    break;
                }
            }
            if (!known && ref.files.size() < kMaxFiles) ref.files.push_back(file);
        }
    }

    // True when a number seen at an import still names the buffer's file. False says
    // nothing: the process may have the file under another number.
    bool stillOpenByNumber(const Ref& ref) const {
        for (const File& file : ref.files) {
            FileId now;
            if (identify_(file.fd, &now.device, &now.inode) && now == file.id) return true;
        }
        return false;
    }

    static bool stillOpen(const Ref& ref, const std::vector<FileId>& open) {
        for (const File& file : ref.files) {
            for (const FileId& one : open) {
                if (one == file.id) return true;
            }
        }
        return false;
    }

    IdentifyFile identify_;
    ListFiles list_;
    std::unordered_map<uint32_t, Ref> refs_;
    uint64_t lastSweepNs_ = 0;
};

}  // namespace dsa
