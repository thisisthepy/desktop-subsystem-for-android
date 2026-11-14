// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// How long a guest process keeps its reference to a gralloc buffer's host ColorBuffer
// (dsa_color_buffer_refs.h), run on the build machine.

#include "dsa_color_buffer_refs.h"

#include <fcntl.h>
#include <unistd.h>

#include <cstdint>
#include <cstdio>
#include <map>
#include <utility>
#include <vector>

static int failures = 0;

#define EXPECT(cond)                                                                \
    do {                                                                            \
        if (!(cond)) {                                                              \
            std::fprintf(stderr, "%s:%d: expected %s\n", __FILE__, __LINE__, #cond); \
            ++failures;                                                             \
        }                                                                           \
    } while (0)

// The process's open descriptors, as the fake sees them: fd to (device, inode).
static std::map<int, std::pair<uint64_t, uint64_t>> openFiles;
// Whether the fake's listing of every descriptor can be read, and how often it was.
static bool listingReadable = true;
static int listings = 0;

static bool identifyFake(int fd, uint64_t* device, uint64_t* inode) {
    auto it = openFiles.find(fd);
    if (it == openFiles.end()) return false;
    *device = it->second.first;
    *inode = it->second.second;
    return true;
}

static bool listFake(std::vector<dsa::FileId>* files) {
    files->clear();
    listings++;
    if (!listingReadable) return false;
    for (const auto& [fd, id] : openFiles) {
        dsa::FileId file;
        file.device = id.first;
        file.inode = id.second;
        files->push_back(file);
    }
    return true;
}

static void reset(std::map<int, std::pair<uint64_t, uint64_t>> files) {
    openFiles = std::move(files);
    listingReadable = true;
    listings = 0;
}

constexpr uint64_t kDmaBufDevice = 13;
constexpr uint32_t kBuffer = 72;
constexpr uint32_t kHost = 0x80000048u;

static void pr3_a_buffer_the_gpu_filled_keeps_its_color_buffer_between_two_imports() {
    // hwui's upload: the uploader's EGLImage imports the bitmap's buffer, the GPU copies the
    // pixels in, and the EGLImage goes. The bitmap still has the buffer, so the reference
    // stays and the host keeps the pixels for the render thread's import that comes later.
    reset({{40, {kDmaBufDevice, 9001}}});
    dsa::ColorBufferRefs refs(identifyFake, listFake);
    const int fds[] = {40};
    EXPECT(refs.opened(kBuffer, kHost, fds, 1));
    EXPECT(refs.hold(kBuffer, kHost, fds, 1));
    EXPECT(refs.holders(kBuffer) == 1);
    EXPECT(refs.drop(kBuffer).hostHandle == 0);
    EXPECT(refs.find(kBuffer) == kHost);
    EXPECT(refs.sweep().empty());
    // The render thread's import finds the same reference, so it opens nothing new.
    EXPECT(refs.find(kBuffer) == kHost);
    EXPECT(refs.hold(kBuffer, kHost, fds, 1));
    EXPECT(refs.holders(kBuffer) == 1);
    // A descriptor that still names the buffer answers without listing every descriptor.
    EXPECT(listings == 0);
}

static void pr3_a_buffer_the_process_has_under_other_descriptors_keeps_its_color_buffer() {
    // SurfaceFlinger imported the buffer through one handle; the handle it has now (the buffer
    // received again over binder, a mapper clone, a dup) has other descriptor numbers, and the
    // one seen at the import is closed. The process still has the buffer, so its reference
    // stays: closing it let the host free the ColorBuffer while the buffer was being imported
    // and drawn.
    reset({{40, {kDmaBufDevice, 9001}}});
    dsa::ColorBufferRefs refs(identifyFake, listFake);
    const int fds[] = {40};
    EXPECT(refs.opened(kBuffer, kHost, fds, 1));
    EXPECT(refs.hold(kBuffer, kHost, fds, 1));
    reset({{77, {kDmaBufDevice, 9001}}, {40, {kDmaBufDevice, 9100}}});
    EXPECT(refs.drop(kBuffer).hostHandle == 0);
    EXPECT(refs.sweep().empty());
    EXPECT(refs.find(kBuffer) == kHost);
    EXPECT(listings == 2);
    // Once no descriptor of the process is the buffer's file, it closes.
    reset({{40, {kDmaBufDevice, 9100}}});
    const auto closed = refs.sweep();
    EXPECT(closed.size() == 1 && closed[0].bufferId == kBuffer && closed[0].hostHandle == kHost);
}

static void pr3_a_process_whose_descriptors_cannot_be_listed_keeps_every_buffer() {
    reset({{40, {kDmaBufDevice, 9001}}});
    dsa::ColorBufferRefs refs(identifyFake, listFake);
    const int fds[] = {40};
    EXPECT(refs.opened(kBuffer, kHost, fds, 1));
    EXPECT(refs.hold(kBuffer, kHost, fds, 1));
    openFiles.clear();
    listingReadable = false;
    EXPECT(refs.drop(kBuffer).hostHandle == 0);
    EXPECT(refs.sweep().empty());
    EXPECT(refs.find(kBuffer) == kHost);
    // The next sweep that can read the list closes it.
    listingReadable = true;
    EXPECT(refs.sweep().size() == 1);
}

static void pr3_a_buffer_with_no_known_file_closes_only_with_its_last_holder() {
    // Nothing identified the buffer's file, so nothing says the process let go of it: a
    // sweep leaves it, and the last holder closes it, as before.
    reset({});
    dsa::ColorBufferRefs refs(identifyFake, listFake);
    const int fds[] = {40};
    EXPECT(refs.opened(kBuffer, kHost, fds, 1));
    EXPECT(refs.sweep().empty());
    EXPECT(refs.hold(kBuffer, kHost, fds, 1));
    const dsa::ColorBufferRefs::Closed closed = refs.drop(kBuffer);
    EXPECT(closed.bufferId == kBuffer && closed.hostHandle == kHost);
}

static void pr3_a_hold_without_the_reference_says_nothing_holds_it() {
    // The caller opens and holds in one step under its lock; a hold that finds no reference,
    // or another handle, counts nothing and says so.
    reset({{40, {kDmaBufDevice, 9001}}});
    dsa::ColorBufferRefs refs(identifyFake, listFake);
    const int fds[] = {40};
    EXPECT(!refs.hold(kBuffer, kHost, fds, 1));
    EXPECT(refs.opened(kBuffer, kHost, fds, 1));
    EXPECT(!refs.hold(kBuffer, kHost + 1, fds, 1));
    EXPECT(refs.holders(kBuffer) == 0);
}

static void nfr1_a_buffer_the_process_let_go_of_closes_its_color_buffer() {
    reset({{40, {kDmaBufDevice, 9001}}});
    dsa::ColorBufferRefs refs(identifyFake, listFake);
    const int fds[] = {40};
    EXPECT(refs.opened(kBuffer, kHost, fds, 1));
    EXPECT(refs.hold(kBuffer, kHost, fds, 1));
    // The memory's AHardwareBuffer was the last one: its release closed the descriptor before
    // the holder went, so the reference closes at once.
    openFiles.clear();
    const dsa::ColorBufferRefs::Closed closed = refs.drop(kBuffer);
    EXPECT(closed.bufferId == kBuffer && closed.hostHandle == kHost);
    EXPECT(refs.size() == 0);
}

static void nfr1_a_buffer_freed_after_its_last_import_closes_at_the_next_sweep() {
    // The app frees a bitmap some time after the render thread stopped drawing it.
    reset({{40, {kDmaBufDevice, 9001}}, {41, {kDmaBufDevice, 9002}}});
    dsa::ColorBufferRefs refs(identifyFake, listFake);
    const int first[] = {40};
    const int second[] = {41};
    EXPECT(refs.opened(kBuffer, kHost, first, 1));
    EXPECT(refs.opened(kBuffer + 1, kHost + 1, second, 1));
    EXPECT(refs.hold(kBuffer, kHost, first, 1));
    EXPECT(refs.hold(kBuffer + 1, kHost + 1, second, 1));
    EXPECT(refs.drop(kBuffer).hostHandle == 0);
    EXPECT(refs.drop(kBuffer + 1).hostHandle == 0);
    openFiles.erase(40);
    const auto closed = refs.sweep();
    EXPECT(closed.size() == 1 && closed[0].bufferId == kBuffer && closed[0].hostHandle == kHost);
    EXPECT(refs.find(kBuffer) == 0);
    EXPECT(refs.find(kBuffer + 1) == kHost + 1);
    // One listing answered the whole sweep.
    EXPECT(listings == 1);
}

static void nfr1_a_descriptor_number_reused_for_another_file_does_not_keep_the_buffer() {
    reset({{40, {kDmaBufDevice, 9001}}});
    dsa::ColorBufferRefs refs(identifyFake, listFake);
    const int fds[] = {40};
    EXPECT(refs.opened(kBuffer, kHost, fds, 1));
    // The buffer's descriptor closed and the number now names another buffer, or a socket on
    // another device with the same inode number.
    openFiles = {{40, {kDmaBufDevice, 9100}}};
    EXPECT(refs.sweep().size() == 1);
    openFiles = {{40, {kDmaBufDevice, 9001}}};
    EXPECT(refs.opened(kBuffer, kHost, fds, 1));
    openFiles = {{40, {kDmaBufDevice + 1, 9001}}};
    EXPECT(refs.sweep().size() == 1);
}

static void nfr1_a_buffer_the_process_received_twice_stays_while_either_handle_lives() {
    reset({{40, {kDmaBufDevice, 9001}}, {55, {kDmaBufDevice, 9001}}});
    dsa::ColorBufferRefs refs(identifyFake, listFake);
    const int first[] = {40};
    const int second[] = {55};
    EXPECT(refs.opened(kBuffer, kHost, first, 1));
    EXPECT(refs.hold(kBuffer, kHost, second, 1));
    EXPECT(refs.drop(kBuffer).hostHandle == 0);
    openFiles.erase(40);
    EXPECT(refs.sweep().empty());
    openFiles.erase(55);
    EXPECT(refs.sweep().size() == 1);
}

static void nfr1_many_handles_of_one_buffer_are_one_file() {
    // Each import through a new handle names the same file: none of them crowds the others
    // out, and the buffer stays while the newest is open.
    reset({});
    dsa::ColorBufferRefs refs(identifyFake, listFake);
    for (int fd = 40; fd < 40 + 3 * static_cast<int>(dsa::ColorBufferRefs::kMaxFiles); fd++) {
        openFiles = {{fd, {kDmaBufDevice, 9001}}};
        const int fds[] = {fd};
        if (fd == 40) {
            EXPECT(refs.opened(kBuffer, kHost, fds, 1));
        }
        EXPECT(refs.hold(kBuffer, kHost, fds, 1));
        EXPECT(refs.drop(kBuffer).hostHandle == 0);
    }
    EXPECT(refs.sweep().empty());
    openFiles.clear();
    EXPECT(refs.sweep().size() == 1);
}

static void nfr1_a_held_reference_never_closes() {
    // A window image on a buffer its queue already freed still draws into the ColorBuffer.
    reset({{40, {kDmaBufDevice, 9001}}});
    dsa::ColorBufferRefs refs(identifyFake, listFake);
    const int fds[] = {40};
    EXPECT(refs.opened(kBuffer, kHost, fds, 1));
    EXPECT(refs.hold(kBuffer, kHost, fds, 1));
    EXPECT(refs.hold(kBuffer, kHost, fds, 1));
    openFiles.clear();
    EXPECT(refs.sweep().empty());
    EXPECT(refs.drop(kBuffer).hostHandle == 0);
    EXPECT(refs.drop(kBuffer).hostHandle == kHost);
    // A drop with no reference left, or of an unknown buffer, closes nothing.
    EXPECT(refs.drop(kBuffer).hostHandle == 0);
    EXPECT(refs.drop(kBuffer + 7).hostHandle == 0);
}

static void nfr1_a_second_open_of_one_buffer_keeps_one_reference() {
    reset({{40, {kDmaBufDevice, 9001}}});
    dsa::ColorBufferRefs refs(identifyFake, listFake);
    const int fds[] = {40};
    EXPECT(refs.opened(kBuffer, kHost, fds, 1));
    EXPECT(!refs.opened(kBuffer, kHost, fds, 1));
    EXPECT(refs.size() == 1);
    EXPECT(refs.bufferOf(kHost) == kBuffer);
    EXPECT(refs.bufferOf(kHost + 9) == 0);
    // A hold with another handle than the reference's is not one of its holders.
    EXPECT(!refs.hold(kBuffer, kHost + 1, fds, 1));
    EXPECT(refs.holders(kBuffer) == 0);
}

static void nfr1_a_sweep_is_due_at_most_once_a_second() {
    dsa::ColorBufferRefs refs(identifyFake, listFake);
    const uint64_t second = dsa::ColorBufferRefs::kSweepIntervalNs;
    EXPECT(refs.sweepDue(5 * second));
    EXPECT(!refs.sweepDue(5 * second + 1));
    EXPECT(!refs.sweepDue(6 * second - 1));
    EXPECT(refs.sweepDue(6 * second));
}

static void nfr1_the_real_check_follows_a_file_the_process_opens_dups_and_closes() {
    // A file of its own, so no other descriptor of this process names it.
    std::FILE* file = std::tmpfile();
    EXPECT(file != nullptr);
    if (!file) return;
    const int fd = fileno(file);
    dsa::ColorBufferRefs refs;
    const int fds[] = {fd};
    EXPECT(refs.opened(kBuffer, kHost, fds, 1));
    EXPECT(refs.sweep().empty());
    // The process keeps the file only through a dup: the listing finds it.
    const int dupped = dup(fd);
    EXPECT(dupped >= 0);
    std::fclose(file);
    EXPECT(refs.sweep().empty());
    close(dupped);
    EXPECT(refs.sweep().size() == 1);
    uint64_t device = 0;
    uint64_t inode = 0;
    EXPECT(!dsa::identifyOpenFile(-1, &device, &inode));
    std::vector<dsa::FileId> files;
    EXPECT(dsa::listOpenFiles(&files));
    EXPECT(!files.empty());
}

int main() {
    pr3_a_buffer_the_gpu_filled_keeps_its_color_buffer_between_two_imports();
    pr3_a_buffer_the_process_has_under_other_descriptors_keeps_its_color_buffer();
    pr3_a_process_whose_descriptors_cannot_be_listed_keeps_every_buffer();
    pr3_a_buffer_with_no_known_file_closes_only_with_its_last_holder();
    pr3_a_hold_without_the_reference_says_nothing_holds_it();
    nfr1_a_buffer_the_process_let_go_of_closes_its_color_buffer();
    nfr1_a_buffer_freed_after_its_last_import_closes_at_the_next_sweep();
    nfr1_a_descriptor_number_reused_for_another_file_does_not_keep_the_buffer();
    nfr1_a_buffer_the_process_received_twice_stays_while_either_handle_lives();
    nfr1_many_handles_of_one_buffer_are_one_file();
    nfr1_a_held_reference_never_closes();
    nfr1_a_second_open_of_one_buffer_keeps_one_reference();
    nfr1_a_sweep_is_due_at_most_once_a_second();
    nfr1_the_real_check_follows_a_file_the_process_opens_dups_and_closes();
    if (failures != 0) {
        std::fprintf(stderr, "%d failure(s)\n", failures);
        return 1;
    }
    std::printf("color_buffer_refs_test: all passed\n");
    return 0;
}
