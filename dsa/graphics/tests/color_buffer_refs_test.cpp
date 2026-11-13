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

static bool identifyFake(int fd, uint64_t* device, uint64_t* inode) {
    auto it = openFiles.find(fd);
    if (it == openFiles.end()) return false;
    *device = it->second.first;
    *inode = it->second.second;
    return true;
}

constexpr uint64_t kDmaBufDevice = 13;
constexpr uint32_t kBuffer = 72;
constexpr uint32_t kHost = 0x80000048u;

static void pr3_a_buffer_the_gpu_filled_keeps_its_color_buffer_between_two_imports() {
    // hwui's upload: the uploader's EGLImage imports the bitmap's buffer, the GPU copies the
    // pixels in, and the EGLImage goes. The bitmap still has the buffer, so the reference
    // stays and the host keeps the pixels for the render thread's import that comes later.
    openFiles = {{40, {kDmaBufDevice, 9001}}};
    dsa::ColorBufferRefs refs(identifyFake);
    const int fds[] = {40};
    EXPECT(refs.opened(kBuffer, kHost, fds, 1));
    refs.hold(kBuffer, kHost, fds, 1);
    EXPECT(refs.holders(kBuffer) == 1);
    EXPECT(refs.drop(kBuffer).hostHandle == 0);
    EXPECT(refs.find(kBuffer) == kHost);
    EXPECT(refs.sweep().empty());
    // The render thread's import finds the same reference, so it opens nothing new.
    EXPECT(refs.find(kBuffer) == kHost);
    refs.hold(kBuffer, kHost, fds, 1);
    EXPECT(refs.holders(kBuffer) == 1);
}

static void nfr1_a_buffer_the_process_let_go_of_closes_its_color_buffer() {
    openFiles = {{40, {kDmaBufDevice, 9001}}};
    dsa::ColorBufferRefs refs(identifyFake);
    const int fds[] = {40};
    EXPECT(refs.opened(kBuffer, kHost, fds, 1));
    refs.hold(kBuffer, kHost, fds, 1);
    // The memory's AHardwareBuffer was the last one: its release closed the descriptor before
    // the holder went, so the reference closes at once.
    openFiles.clear();
    const dsa::ColorBufferRefs::Closed closed = refs.drop(kBuffer);
    EXPECT(closed.bufferId == kBuffer && closed.hostHandle == kHost);
    EXPECT(refs.size() == 0);
}

static void nfr1_a_buffer_freed_after_its_last_import_closes_at_the_next_sweep() {
    // The app frees a bitmap some time after the render thread stopped drawing it.
    openFiles = {{40, {kDmaBufDevice, 9001}}, {41, {kDmaBufDevice, 9002}}};
    dsa::ColorBufferRefs refs(identifyFake);
    const int first[] = {40};
    const int second[] = {41};
    EXPECT(refs.opened(kBuffer, kHost, first, 1));
    EXPECT(refs.opened(kBuffer + 1, kHost + 1, second, 1));
    refs.hold(kBuffer, kHost, first, 1);
    refs.hold(kBuffer + 1, kHost + 1, second, 1);
    EXPECT(refs.drop(kBuffer).hostHandle == 0);
    EXPECT(refs.drop(kBuffer + 1).hostHandle == 0);
    openFiles.erase(40);
    const auto closed = refs.sweep();
    EXPECT(closed.size() == 1 && closed[0].bufferId == kBuffer && closed[0].hostHandle == kHost);
    EXPECT(refs.find(kBuffer) == 0);
    EXPECT(refs.find(kBuffer + 1) == kHost + 1);
}

static void nfr1_a_descriptor_number_reused_for_another_file_does_not_keep_the_buffer() {
    openFiles = {{40, {kDmaBufDevice, 9001}}};
    dsa::ColorBufferRefs refs(identifyFake);
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
    openFiles = {{40, {kDmaBufDevice, 9001}}, {55, {kDmaBufDevice, 9001}}};
    dsa::ColorBufferRefs refs(identifyFake);
    const int first[] = {40};
    const int second[] = {55};
    EXPECT(refs.opened(kBuffer, kHost, first, 1));
    refs.hold(kBuffer, kHost, second, 1);
    EXPECT(refs.drop(kBuffer).hostHandle == 0);
    openFiles.erase(40);
    EXPECT(refs.sweep().empty());
    openFiles.erase(55);
    EXPECT(refs.sweep().size() == 1);
}

static void nfr1_a_held_reference_never_closes() {
    // A window image on a buffer its queue already freed still draws into the ColorBuffer.
    openFiles = {{40, {kDmaBufDevice, 9001}}};
    dsa::ColorBufferRefs refs(identifyFake);
    const int fds[] = {40};
    EXPECT(refs.opened(kBuffer, kHost, fds, 1));
    refs.hold(kBuffer, kHost, fds, 1);
    refs.hold(kBuffer, kHost, fds, 1);
    openFiles.clear();
    EXPECT(refs.sweep().empty());
    EXPECT(refs.drop(kBuffer).hostHandle == 0);
    EXPECT(refs.drop(kBuffer).hostHandle == kHost);
    // A drop with no reference left, or of an unknown buffer, closes nothing.
    EXPECT(refs.drop(kBuffer).hostHandle == 0);
    EXPECT(refs.drop(kBuffer + 7).hostHandle == 0);
}

static void nfr1_a_second_open_of_one_buffer_keeps_one_reference() {
    openFiles = {{40, {kDmaBufDevice, 9001}}};
    dsa::ColorBufferRefs refs(identifyFake);
    const int fds[] = {40};
    EXPECT(refs.opened(kBuffer, kHost, fds, 1));
    EXPECT(!refs.opened(kBuffer, kHost, fds, 1));
    EXPECT(refs.size() == 1);
    EXPECT(refs.bufferOf(kHost) == kBuffer);
    EXPECT(refs.bufferOf(kHost + 9) == 0);
    // A hold with another handle than the reference's is not one of its holders.
    refs.hold(kBuffer, kHost + 1, fds, 1);
    EXPECT(refs.holders(kBuffer) == 0);
}

static void nfr1_a_sweep_is_due_at_most_once_a_second() {
    dsa::ColorBufferRefs refs(identifyFake);
    const uint64_t second = dsa::ColorBufferRefs::kSweepIntervalNs;
    EXPECT(refs.sweepDue(5 * second));
    EXPECT(!refs.sweepDue(5 * second + 1));
    EXPECT(!refs.sweepDue(6 * second - 1));
    EXPECT(refs.sweepDue(6 * second));
}

static void nfr1_the_real_check_follows_a_descriptor_the_process_opens_and_closes() {
    const int fd = open("/dev/null", O_RDONLY);
    EXPECT(fd >= 0);
    dsa::ColorBufferRefs refs;
    const int fds[] = {fd};
    EXPECT(refs.opened(kBuffer, kHost, fds, 1));
    EXPECT(refs.sweep().empty());
    close(fd);
    EXPECT(refs.sweep().size() == 1);
    uint64_t device = 0;
    uint64_t inode = 0;
    EXPECT(!dsa::identifyOpenFile(-1, &device, &inode));
}

int main() {
    pr3_a_buffer_the_gpu_filled_keeps_its_color_buffer_between_two_imports();
    nfr1_a_buffer_the_process_let_go_of_closes_its_color_buffer();
    nfr1_a_buffer_freed_after_its_last_import_closes_at_the_next_sweep();
    nfr1_a_descriptor_number_reused_for_another_file_does_not_keep_the_buffer();
    nfr1_a_buffer_the_process_received_twice_stays_while_either_handle_lives();
    nfr1_a_held_reference_never_closes();
    nfr1_a_second_open_of_one_buffer_keeps_one_reference();
    nfr1_a_sweep_is_due_at_most_once_a_second();
    nfr1_the_real_check_follows_a_descriptor_the_process_opens_and_closes();
    if (failures != 0) {
        std::fprintf(stderr, "%d failure(s)\n", failures);
        return 1;
    }
    std::printf("color_buffer_refs_test: all passed\n");
    return 0;
}
