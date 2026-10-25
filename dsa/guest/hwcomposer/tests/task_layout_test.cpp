// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The rules that split the freeform display into per-task frames.

#include <cstring>

#include "compose_request.h"
#include "task_layout.h"
#include "test_main.h"

using namespace dsa::hwc;

namespace {

const Rect kDisplay{0, 0, 1920, 1080};

std::vector<int64_t> layersOf(const Layout& layout, int32_t task) {
    for (const TaskLayers& t : layout.tasks) {
        if (t.task == task) {
            return t.layers;
        }
    }
    return {};
}

bool hasTask(const Layout& layout, int32_t task) {
    for (const TaskLayers& t : layout.tasks) {
        if (t.task == task) {
            return true;
        }
    }
    return false;
}

}  // namespace

TEST(fr3_a_layer_inside_one_task_goes_to_that_task) {
    const std::vector<Task> tasks{{1, {100, 100, 900, 700}}, {2, {1000, 100, 1800, 700}}};
    const std::vector<LayerPlacement> layers{
        {10, 1, {100, 100, 900, 700}},
        {20, 2, {1000, 100, 1800, 700}},
        {21, 3, {1200, 300, 1400, 400}},
    };
    const Layout layout = assignLayersToTasks(layers, tasks, kDisplay);
    EXPECT_EQ(layersOf(layout, 1), (std::vector<int64_t>{10}));
    EXPECT_EQ(layersOf(layout, 2), (std::vector<int64_t>{20, 21}));
    EXPECT_TRUE(layout.unassigned.empty());
}

TEST(fr3_layers_no_task_contains_are_never_sent) {
    // A wallpaper covering the display, and a layer straddling the window's edge.
    const std::vector<Task> tasks{{1, {100, 100, 900, 700}}};
    const std::vector<LayerPlacement> layers{
        {1, 0, kDisplay},
        {10, 1, {100, 100, 900, 700}},
        {11, 2, {800, 600, 1000, 800}},
    };
    const Layout layout = assignLayersToTasks(layers, tasks, kDisplay);
    EXPECT_EQ(layersOf(layout, 1), (std::vector<int64_t>{10}));
    EXPECT_EQ(layout.unassigned, (std::vector<int64_t>{1, 11}));
}

TEST(fr3_without_tasks_nothing_is_sent) {
    const std::vector<LayerPlacement> layers{{1, 0, kDisplay}, {2, 1, {0, 0, 10, 10}}};
    const Layout layout = assignLayersToTasks(layers, {}, kDisplay);
    EXPECT_TRUE(layout.tasks.empty());
    EXPECT_EQ(layout.unassigned, (std::vector<int64_t>{1, 2}));
}

TEST(fr3_a_window_over_another_keeps_its_own_surface) {
    // Task 2's window lies wholly inside task 1's, above it.
    const std::vector<Task> tasks{{1, {0, 0, 1600, 1000}}, {2, {400, 300, 1000, 800}}};
    const std::vector<LayerPlacement> layers{
        {10, 1, {0, 0, 1600, 1000}},
        {20, 2, {400, 300, 1000, 800}},
    };
    const Layout layout = assignLayersToTasks(layers, tasks, kDisplay);
    EXPECT_EQ(layersOf(layout, 1), (std::vector<int64_t>{10}));
    EXPECT_EQ(layersOf(layout, 2), (std::vector<int64_t>{20}));
}

TEST(fr3_a_popup_goes_to_the_window_just_below_it) {
    // Both popups lie inside both windows; each belongs to the window beneath it in z.
    const std::vector<Task> tasks{{1, {0, 0, 1600, 1000}}, {2, {400, 300, 1000, 800}}};
    const std::vector<LayerPlacement> layers{
        {10, 1, {0, 0, 1600, 1000}},
        {11, 2, {500, 400, 700, 500}},
        {20, 3, {400, 300, 1000, 800}},
        {21, 4, {600, 500, 800, 600}},
    };
    const Layout layout = assignLayersToTasks(layers, tasks, kDisplay);
    EXPECT_EQ(layersOf(layout, 1), (std::vector<int64_t>{10, 11}));
    EXPECT_EQ(layersOf(layout, 2), (std::vector<int64_t>{20, 21}));
}

TEST(fr3_an_otherwise_ambiguous_layer_goes_to_the_smaller_task) {
    const std::vector<Task> tasks{{1, {0, 0, 1600, 1000}}, {2, {400, 300, 1000, 800}}};
    const std::vector<LayerPlacement> layers{{30, 1, {500, 400, 700, 500}}};
    const Layout layout = assignLayersToTasks(layers, tasks, kDisplay);
    EXPECT_EQ(layersOf(layout, 2), (std::vector<int64_t>{30}));
    EXPECT_TRUE(!hasTask(layout, 1));
}

TEST(fr3_layers_keep_their_z_order_within_a_task) {
    const std::vector<Task> tasks{{5, {100, 100, 900, 700}}};
    const std::vector<LayerPlacement> layers{
        {3, 7, {200, 200, 300, 300}},
        {1, 2, {100, 100, 900, 700}},
        {2, 5, {150, 150, 250, 250}},
    };
    const Layout layout = assignLayersToTasks(layers, tasks, kDisplay);
    EXPECT_EQ(layersOf(layout, 5), (std::vector<int64_t>{1, 2, 3}));
}

TEST(fr3_a_window_partly_off_the_display_still_gets_its_layers) {
    // SurfaceFlinger may place the layer beyond the display's edge; only the visible part
    // has to lie inside the task.
    const std::vector<Task> tasks{{7, {-40, -10, 600, 400}}};
    const std::vector<LayerPlacement> layers{{1, 1, {-40, -10, 600, 400}}};
    const Layout layout = assignLayersToTasks(layers, tasks, kDisplay);
    EXPECT_EQ(layersOf(layout, 7), (std::vector<int64_t>{1}));
}

TEST(fr3_tasks_without_layers_are_left_out_of_the_layout) {
    const std::vector<Task> tasks{{1, {100, 100, 900, 700}}, {2, {1000, 100, 1800, 700}}};
    const std::vector<LayerPlacement> layers{{10, 1, {100, 100, 900, 700}}};
    const Layout layout = assignLayersToTasks(layers, tasks, kDisplay);
    EXPECT_EQ(layout.tasks.size(), 1u);
    EXPECT_TRUE(hasTask(layout, 1));
}

TEST(fr3_the_registry_keeps_the_latest_bounds) {
    TaskRegistry registry;
    EXPECT_TRUE(registry.update(4, {0, 0, 10, 10}));
    EXPECT_TRUE(!registry.update(4, {0, 0, 10, 10}));
    EXPECT_TRUE(registry.update(4, {5, 5, 20, 20}));
    EXPECT_TRUE(registry.update(2, {1, 1, 2, 2}));
    const std::vector<Task> tasks = registry.tasks();
    EXPECT_EQ(tasks.size(), 2u);
    EXPECT_EQ(tasks[0].id, 2);
    EXPECT_TRUE(tasks[1].bounds == (Rect{5, 5, 20, 20}));
    EXPECT_TRUE(registry.remove(4));
    EXPECT_TRUE(!registry.remove(4));
    EXPECT_EQ(registry.tasks().size(), 1u);
}

TEST(fr3_each_task_gets_its_own_display) {
    TaskDisplays displays(1, 10);
    const auto a = displays.acquire(30);
    const auto b = displays.acquire(31);
    EXPECT_TRUE(a && b);
    EXPECT_EQ(a->displayId, 1u);
    EXPECT_EQ(b->displayId, 2u);
    EXPECT_TRUE(a->created && b->created);
}

TEST(fr3_a_task_keeps_its_display_until_it_ends) {
    TaskDisplays displays(1, 10);
    displays.acquire(30);
    const auto again = displays.acquire(30);
    EXPECT_TRUE(again);
    EXPECT_EQ(again->displayId, 1u);
    EXPECT_TRUE(!again->created);
    EXPECT_EQ(displays.release(30), std::optional<uint32_t>(1u));
    EXPECT_TRUE(!displays.find(30));
    EXPECT_TRUE(!displays.release(30));
}

TEST(fr3_a_freed_display_is_paired_again) {
    TaskDisplays displays(1, 10);
    displays.acquire(30);
    displays.acquire(31);
    displays.release(30);
    const auto next = displays.acquire(32);
    EXPECT_EQ(next->displayId, 1u);
    EXPECT_EQ(displays.pairings(),
              (std::vector<std::pair<int32_t, uint32_t>>{{31, 2u}, {32, 1u}}));
}

TEST(fr3_no_display_is_paired_when_all_are_taken) {
    TaskDisplays displays(1, 2);
    displays.acquire(1);
    displays.acquire(2);
    EXPECT_TRUE(!displays.acquire(3));
}

TEST(fr3_task_frames_are_composed_relative_to_the_task_origin) {
    ComposeInput surface;
    surface.colorBuffer = 0x80000007u;
    surface.frame = {100, 80, 1180, 800};
    surface.crop = {0, 0, 1080, 720};
    surface.blendMode = 2;
    ComposeInput popup;
    popup.colorBuffer = 0x80000009u;
    popup.frame = {300, 200, 500, 300};
    popup.crop = {0, 0, 200, 100};
    ComposeInput dim;
    dim.mode = ComposeMode::SolidColor;
    dim.colorBuffer = 1234;
    dim.frame = {100, 80, 1180, 800};
    dim.color[3] = 128;

    const Rect bounds{100, 80, 1180, 800};
    const std::vector<uint8_t> bytes = buildComposeRequest(6, 77, bounds, {surface, popup, dim});
    EXPECT_EQ(bytes.size(), sizeof(WireComposeDeviceV2) + 3 * sizeof(WireComposeLayer));

    WireComposeDeviceV2 header;
    std::memcpy(&header, bytes.data(), sizeof(header));
    EXPECT_EQ(header.version, 2u);
    EXPECT_EQ(header.displayId, 6u);
    EXPECT_EQ(header.targetHandle, 77u);
    EXPECT_EQ(header.numLayers, 3u);

    WireComposeLayer layers[3];
    std::memcpy(layers, bytes.data() + sizeof(header), sizeof(layers));
    EXPECT_EQ(layers[0].cbHandle, 0x80000007u);
    EXPECT_EQ(layers[0].composeMode, 2);
    EXPECT_EQ(layers[0].displayFrame.left, 0);
    EXPECT_EQ(layers[0].displayFrame.top, 0);
    EXPECT_EQ(layers[0].displayFrame.right, 1080);
    EXPECT_EQ(layers[0].displayFrame.bottom, 720);
    EXPECT_EQ(layers[0].crop.right, 1080.0f);
    EXPECT_EQ(layers[0].blendMode, 2);
    EXPECT_EQ(layers[1].displayFrame.left, 200);
    EXPECT_EQ(layers[1].displayFrame.top, 120);
    EXPECT_EQ(layers[1].displayFrame.right, 400);
    EXPECT_EQ(layers[1].displayFrame.bottom, 220);
    // A solid colour has no buffer behind it.
    EXPECT_EQ(layers[2].cbHandle, 0u);
    EXPECT_EQ(layers[2].composeMode, 3);
    EXPECT_EQ(layers[2].color.a, 128);
}

int main() { return ::dsa::test::runAll(); }
