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

TEST(fr4_an_app_has_drawn_at_its_size_when_a_drawn_layer_fills_the_task) {
    const Rect bounds{100, 100, 700, 600};
    // The old, smaller surface, and a solid colour that fills the task.
    const std::vector<LayerFill> beforeRedraw{{{100, 100, 500, 400}, true}, {bounds, false}};
    const std::vector<LayerFill> afterRedraw{{{100, 100, 500, 400}, true}, {bounds, true}};
    EXPECT_TRUE(!drawnAtSize(bounds, beforeRedraw));
    EXPECT_TRUE(drawnAtSize(bounds, afterRedraw));
    EXPECT_TRUE(!drawnAtSize(bounds, {}));
}

namespace {

constexpr uint64_t kOldLayout = 7;
constexpr uint64_t kNewLayout = 8;
const Rect kOldSize{0, 0, 400, 300};
const Rect kGrown{0, 0, 600, 500};

// A task whose first frame, at 400x300, filled it.
ResizeGate filledAt400x300() {
    ResizeGate gate;
    gate.decide(1, 0, 0, kOldSize, true, 0, kOldLayout);
    return gate;
}

}  // namespace

TEST(fr4_a_first_frame_goes_as_it_is) {
    ResizeGate gate;
    // Nothing to stretch yet: a dialog's first frame, not filling its task, goes.
    EXPECT_TRUE(gate.decide(1, 0, 0, kGrown, false, 0, kOldLayout) == ResizeGate::Decision::Post);
}

TEST(fr4_a_filling_task_is_held_until_its_app_draws_at_the_new_size) {
    ResizeGate gate = filledAt400x300();
    EXPECT_TRUE(gate.decide(1, 400, 300, kGrown, false, 1000, kOldLayout) ==
                ResizeGate::Decision::Hold);
    EXPECT_TRUE(gate.decide(1, 400, 300, kGrown, false, 1200, kOldLayout) ==
                ResizeGate::Decision::Hold);
    EXPECT_TRUE(gate.decide(1, 400, 300, kGrown, true, 1250, kNewLayout) ==
                ResizeGate::Decision::Post);
    EXPECT_TRUE(!gate.nextDeadline());
}

TEST(fr4_a_filling_app_that_has_not_responded_stays_stretched_past_3_s) {
    ResizeGate gate = filledAt400x300();
    // An hour with its old layers unchanged: its frame, the old picture beside black, never goes.
    for (int64_t now = 0; now < 3600000; now += 500) {
        const ResizeGate::Decision decision =
            gate.decide(1, 400, 300, kGrown, false, now, kOldLayout);
        EXPECT_TRUE(decision == ResizeGate::Decision::Hold ||
                    decision == ResizeGate::Decision::HoldSlow);
    }
    // And the moment it fills the size, it does.
    EXPECT_TRUE(gate.decide(1, 400, 300, kGrown, true, 3600000, kNewLayout) ==
                ResizeGate::Decision::Post);
}

TEST(fr4_a_filling_app_that_lays_out_with_margins_is_shown_after_settling) {
    ResizeGate gate = filledAt400x300();
    EXPECT_TRUE(gate.decide(1, 400, 300, kGrown, false, 1000, kOldLayout) ==
                ResizeGate::Decision::Hold);
    // It responds, without filling the task: not at once, in case more is on its way.
    EXPECT_TRUE(gate.decide(1, 400, 300, kGrown, false, 1100, kNewLayout) ==
                ResizeGate::Decision::Hold);
    EXPECT_TRUE(gate.nextDeadline() == std::optional<int64_t>(1100 + ResizeGate::kSettleMs));
    EXPECT_TRUE(gate.decide(1, 400, 300, kGrown, false, 1100 + ResizeGate::kSettleMs - 1,
                            kNewLayout) == ResizeGate::Decision::Hold);
    EXPECT_TRUE(gate.decide(1, 400, 300, kGrown, false, 1100 + ResizeGate::kSettleMs, kNewLayout) ==
                ResizeGate::Decision::PostSettled);
    EXPECT_TRUE(!gate.nextDeadline());
}

TEST(fr4_a_layout_still_changing_has_not_settled) {
    ResizeGate gate = filledAt400x300();
    EXPECT_TRUE(gate.decide(1, 400, 300, kGrown, false, 0, kNewLayout) ==
                ResizeGate::Decision::Hold);
    EXPECT_TRUE(gate.decide(1, 400, 300, kGrown, false, 400, kNewLayout + 1) ==
                ResizeGate::Decision::Hold);
    EXPECT_TRUE(gate.decide(1, 400, 300, kGrown, false, 800, kNewLayout + 1) ==
                ResizeGate::Decision::Hold);
    EXPECT_TRUE(gate.decide(1, 400, 300, kGrown, false, 900, kNewLayout + 1) ==
                ResizeGate::Decision::PostSettled);
}

TEST(fr4_a_dialog_style_task_keeps_updating_after_a_resize) {
    ResizeGate gate;
    // Its first frame did not fill the task, so there is no full frame to keep.
    gate.decide(1, 0, 0, kOldSize, false, 0, kOldLayout);
    EXPECT_TRUE(gate.decide(1, 400, 300, kGrown, false, 10, kOldLayout) ==
                ResizeGate::Decision::Post);
    EXPECT_TRUE(gate.decide(1, 600, 500, kGrown, false, 20, kNewLayout) ==
                ResizeGate::Decision::Post);
    EXPECT_TRUE(!gate.nextDeadline());
}

TEST(fr4_a_task_that_settled_on_margins_keeps_updating_and_can_fill_again) {
    ResizeGate gate = filledAt400x300();
    gate.decide(1, 400, 300, kGrown, false, 0, kNewLayout);
    EXPECT_TRUE(gate.decide(1, 400, 300, kGrown, false, ResizeGate::kSettleMs, kNewLayout) ==
                ResizeGate::Decision::PostSettled);
    EXPECT_TRUE(gate.decide(1, 600, 500, kGrown, false, 1000, kNewLayout) ==
                ResizeGate::Decision::Post);
    // It fills the task again, so the next incomplete frame is held.
    EXPECT_TRUE(gate.decide(1, 600, 500, kGrown, true, 1100, kNewLayout) ==
                ResizeGate::Decision::Post);
    EXPECT_TRUE(gate.decide(1, 600, 500, kGrown, false, 1200, kNewLayout + 1) ==
                ResizeGate::Decision::Hold);
}

TEST(fr4_an_incomplete_frame_of_the_same_size_does_not_replace_a_full_one_until_it_settles) {
    ResizeGate gate = filledAt400x300();
    // An app recreated at the end of a drag: its surface is gone, a small layer remains.
    EXPECT_TRUE(gate.decide(1, 400, 300, kOldSize, false, 1000, kNewLayout) ==
                ResizeGate::Decision::Hold);
    EXPECT_TRUE(gate.decide(1, 400, 300, kOldSize, false, 1200, kNewLayout) ==
                ResizeGate::Decision::Hold);
    // Its full frame arrives before it settles.
    EXPECT_TRUE(gate.decide(1, 400, 300, kOldSize, true, 1300, kOldLayout) ==
                ResizeGate::Decision::Post);
    // One that stays that way is its layout now.
    gate.decide(1, 400, 300, kOldSize, false, 2000, kNewLayout);
    EXPECT_TRUE(gate.decide(1, 400, 300, kOldSize, false, 2000 + ResizeGate::kSettleMs,
                            kNewLayout) == ResizeGate::Decision::PostSettled);
}

TEST(fr4_a_slow_app_is_logged_once_rather_than_shown) {
    ResizeGate gate = filledAt400x300();
    gate.decide(2, 0, 0, kOldSize, true, 0, kOldLayout);
    EXPECT_TRUE(gate.decide(1, 400, 300, kGrown, false, 0, kOldLayout) ==
                ResizeGate::Decision::Hold);
    EXPECT_TRUE(gate.decide(2, 400, 300, kGrown, false, 100, kOldLayout) ==
                ResizeGate::Decision::Hold);
    EXPECT_TRUE(gate.nextDeadline() == std::optional<int64_t>(ResizeGate::kSlowMs));
    EXPECT_TRUE(gate.decide(1, 400, 300, kGrown, false, ResizeGate::kSlowMs, kOldLayout) ==
                ResizeGate::Decision::HoldSlow);
    EXPECT_TRUE(gate.decide(1, 400, 300, kGrown, false, ResizeGate::kSlowMs + 1, kOldLayout) ==
                ResizeGate::Decision::Hold);
    // The next look is for the task not logged yet; with none left, nothing is asked for.
    EXPECT_TRUE(gate.nextDeadline() == std::optional<int64_t>(100 + ResizeGate::kSlowMs));
    gate.forget(2);
    EXPECT_TRUE(!gate.nextDeadline());
}

TEST(fr4_each_new_size_of_a_drag_starts_its_wait_anew) {
    ResizeGate gate = filledAt400x300();
    // A drag that keeps moving ahead of an app that answers each size late: its answers are
    // never taken for a settled layout, however long the drag takes.
    int64_t now = 0;
    for (int32_t step = 0; step < 100; ++step, now += 300) {
        const Rect size{0, 0, 600 + step, 500};
        EXPECT_TRUE(gate.decide(1, 400, 300, size, false, now, kNewLayout + step) ==
                    ResizeGate::Decision::Hold);
    }
    EXPECT_TRUE(now > ResizeGate::kSlowMs);
}

TEST(fr4_clipping_or_moving_an_old_surface_is_not_the_app_responding) {
    LayerFill surface;
    surface.frame = Rect{100, 100, 900, 700};
    surface.drawn = true;
    surface.bufferWidth = 800;
    surface.bufferHeight = 600;
    LayerFill colour;
    colour.frame = Rect{100, 100, 900, 700};
    const uint64_t before = drawnGeometry(Rect{100, 100, 900, 700}, {surface, colour});

    // The task shrank and SurfaceFlinger clips the surface; a solid colour changes nothing.
    surface.frame = Rect{100, 100, 500, 400};
    colour.frame = Rect{100, 100, 500, 400};
    EXPECT_TRUE(drawnGeometry(Rect{100, 100, 500, 400}, {surface, colour}) == before);

    // The task moved, its surface with it.
    surface.frame = Rect{300, 300, 700, 600};
    EXPECT_TRUE(drawnGeometry(Rect{300, 300, 700, 600}, {surface}) == before);

    // The app drew at another size, or moved its window inside the task.
    surface.bufferWidth = 400;
    surface.bufferHeight = 300;
    EXPECT_TRUE(drawnGeometry(Rect{300, 300, 700, 600}, {surface}) != before);
    LayerFill moved = surface;
    moved.frame = Rect{320, 300, 720, 600};
    EXPECT_TRUE(drawnGeometry(Rect{300, 300, 700, 600}, {moved}) !=
                drawnGeometry(Rect{300, 300, 700, 600}, {surface}));
}

TEST(fr4_an_old_surface_clipped_to_a_shrunken_task_is_not_drawn_at_its_size) {
    const Rect shrunk{100, 100, 500, 400};
    // SurfaceFlinger clips the 800x600 surface to the task: its frame fills the bounds, and it
    // shows the top left 400x300 of the buffer, pixel for pixel.
    LayerFill clipped;
    clipped.frame = shrunk;
    clipped.drawn = true;
    clipped.bufferWidth = 800;
    clipped.bufferHeight = 600;
    clipped.cropWidth = 400;
    clipped.cropHeight = 300;
    EXPECT_TRUE(!drawnAtSize(shrunk, {clipped}));

    // Once the app has drawn at the size, all of a buffer of that size.
    LayerFill redrawn = clipped;
    redrawn.bufferWidth = 400;
    redrawn.bufferHeight = 300;
    EXPECT_TRUE(drawnAtSize(shrunk, {redrawn}));

    // A surface an app scales to its window (a game at a fixed size) has drawn at it.
    LayerFill scaled = clipped;
    scaled.cropWidth = 800;
    scaled.cropHeight = 600;
    EXPECT_TRUE(drawnAtSize(shrunk, {scaled}));

    // A layer whose buffer is not known counts by its frame.
    LayerFill unknown;
    unknown.frame = shrunk;
    unknown.drawn = true;
    EXPECT_TRUE(drawnAtSize(shrunk, {unknown}));
}

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

TEST(fr3_two_tasks_at_one_place_cannot_be_told_apart) {
    // Android's freeform launch gives every new task the same default place. With identical
    // bounds both surfaces match both tasks exactly, so every layer goes to one task and the
    // other gets none: the agent has to move tasks apart.
    const Rect place{686, 53, 1234, 1027};
    const std::vector<Task> tasks{{4, place}, {5, place}};
    const std::vector<LayerPlacement> layers{{40, 1, place}, {50, 2, place}};
    const Layout layout = assignLayersToTasks(layers, tasks, kDisplay);
    EXPECT_EQ(layersOf(layout, 4), (std::vector<int64_t>{40, 50}));
    EXPECT_TRUE(!hasTask(layout, 5));
}

TEST(fr3_a_task_moved_apart_gets_its_own_layers_and_keeps_its_display) {
    const Rect place{686, 53, 1234, 1027};
    const Rect moved{0, 0, 548, 974};
    TaskRegistry registry;
    TaskDisplays displays(1, 10);
    registry.update(4, place);
    registry.update(5, place);
    const auto before = displays.acquire(5);

    // The agent moves task 5 and reports its new bounds; its surface follows.
    EXPECT_TRUE(registry.update(5, moved));
    const std::vector<LayerPlacement> layers{
        {40, 1, place},
        {50, 2, moved},
        {51, 3, {100, 200, 300, 260}},
    };
    const Layout layout = assignLayersToTasks(layers, registry.tasks(), kDisplay);
    EXPECT_EQ(layersOf(layout, 4), (std::vector<int64_t>{40}));
    EXPECT_EQ(layersOf(layout, 5), (std::vector<int64_t>{50, 51}));
    EXPECT_TRUE(layout.unassigned.empty());

    const auto after = displays.acquire(5);
    EXPECT_TRUE(before && after);
    EXPECT_EQ(after->displayId, before->displayId);
    EXPECT_TRUE(!after->created);
}

TEST(fr3_a_surface_not_yet_at_its_task_new_place_goes_to_no_other_task) {
    // Between the agent's report and the window manager's move, the surface is still at the
    // old place, which no task's bounds hold any more: it is left out, not given to a
    // neighbour.
    const std::vector<Task> tasks{{4, {0, 0, 548, 974}}, {5, {548, 0, 1096, 974}}};
    const std::vector<LayerPlacement> layers{
        {40, 1, {0, 0, 548, 974}},
        {50, 2, {686, 53, 1234, 1027}},
    };
    const Layout layout = assignLayersToTasks(layers, tasks, kDisplay);
    EXPECT_EQ(layersOf(layout, 4), (std::vector<int64_t>{40}));
    EXPECT_TRUE(!hasTask(layout, 5));
    EXPECT_EQ(layout.unassigned, (std::vector<int64_t>{50}));
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

TEST(fr4_a_task_buffers_are_made_at_the_display_size) {
    // The first frame of a 600x400 task on a 1920x1080 display.
    const BufferSize first = taskBufferSize({}, kDisplay, 600, 400);
    EXPECT_EQ(first.width, 1920);
    EXPECT_EQ(first.height, 1080);
}

TEST(fr4_resizing_a_task_within_the_display_keeps_its_buffers) {
    BufferSize buffers = taskBufferSize({}, kDisplay, 600, 400);
    // A drag through many sizes, smaller and larger, up to the whole display.
    const int64_t sizes[][2] = {{610, 405}, {580, 390}, {1200, 800}, {1920, 1080}, {300, 200}};
    for (const auto& size : sizes) {
        const BufferSize next = taskBufferSize(buffers, kDisplay, size[0], size[1]);
        EXPECT_TRUE(next == buffers);
        buffers = next;
    }
}

TEST(fr4_a_task_larger_than_the_display_gets_buffers_that_hold_it) {
    const BufferSize buffers = taskBufferSize({1920, 1080}, kDisplay, 2000, 900);
    EXPECT_EQ(buffers.width, 2000);
    EXPECT_EQ(buffers.height, 1080);
    // Once they hold it, they stay.
    EXPECT_TRUE(taskBufferSize(buffers, kDisplay, 1990, 1000) == buffers);
}

TEST(fr4_a_larger_display_remakes_the_buffers_once) {
    const Rect larger{0, 0, 2560, 1440};
    const BufferSize buffers = taskBufferSize({1920, 1080}, larger, 600, 400);
    EXPECT_EQ(buffers.width, 2560);
    EXPECT_EQ(buffers.height, 1440);
    EXPECT_TRUE(taskBufferSize(buffers, larger, 640, 420) == buffers);
}

TEST(fr4_a_resized_task_is_composed_into_the_top_left_of_its_buffers) {
    // The task moved and grew; its layers land at its own origin in the larger buffer, so
    // the frame fills exactly the task's size from (0, 0).
    const Rect bounds{300, 200, 1000, 700};
    std::vector<ComposeInput> inputs(1);
    inputs[0].mode = ComposeMode::Device;
    inputs[0].colorBuffer = 7;
    inputs[0].frame = bounds;
    inputs[0].crop = FRect{0, 0, 700, 500};
    const std::vector<uint8_t> request = buildComposeRequest(0, 9, bounds, inputs);
    WireComposeDeviceV2 header;
    std::memcpy(&header, request.data(), sizeof(header));
    EXPECT_EQ(header.targetHandle, 9u);
    WireComposeLayer layer;
    std::memcpy(&layer, request.data() + sizeof(header), sizeof(layer));
    EXPECT_EQ(layer.displayFrame.left, 0);
    EXPECT_EQ(layer.displayFrame.top, 0);
    EXPECT_EQ(layer.displayFrame.right, 700);
    EXPECT_EQ(layer.displayFrame.bottom, 500);
}

int main() { return ::dsa::test::runAll(); }
