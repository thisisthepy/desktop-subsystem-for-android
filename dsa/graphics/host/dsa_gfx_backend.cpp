/*
 * Copyright 2026 The DSA Authors
 * SPDX-License-Identifier: Apache-2.0
 */

#include "dsa_gfx_backend.h"
#include "dsa_gfx_display.h"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstring>
#include <memory>
#include <mutex>
#include <thread>
#include <vector>

#include "aemu/base/system/System.h"
#include "gfxstream/virtio-gpu-gfxstream-renderer.h"
#include "gfxstream/virtio-gpu-gfxstream-renderer-unstable.h"
#include "host-common/opengles.h"
#include "render-utils/RenderChannel.h"
#include "render-utils/Renderer.h"
#include "FrameBuffer.h"
#include "vulkan/VkCommonOperations.h"
#include "vulkan/VulkanDispatch.h"

namespace {

static std::atomic<bool> s_initialized{false};
static std::atomic<uint64_t> s_next_puid{1};
static std::mutex s_post_cb_mutex;
static dsa_color_buffer_post_fn s_color_buffer_post_cb = nullptr;
static void* s_color_buffer_post_ctx = nullptr;

static void dummy_fence_cb(void*, struct stream_renderer_fence*) {}

static void internal_on_post(void* context,
                             uint32_t display_id,
                             int width,
                             int height,
                             int ydir,
                             int format,
                             int type,
                             unsigned char* pixels) {
    auto cb = reinterpret_cast<dsa_post_callback_fn>(context);
    if (cb) {
        cb(nullptr, display_id, width, height, ydir, format, type, pixels);
    }
}

struct DsaChannelHolder {
    gfxstream::RenderChannelPtr channel;
};

}  // namespace

extern "C" DSA_GFX_EXPORT int dsa_gfx_init(uint32_t width, uint32_t height) {
    if (s_initialized.load(std::memory_order_acquire)) {
        return 0;
    }

    if (width == 0) {
        width = 1280;
    }
    if (height == 0) {
        height = 720;
    }

    static uint64_t s_cookie = 0x534144; /* "DSA" */
    const uint64_t flags = STREAM_RENDERER_FLAGS_USE_VK_BIT | STREAM_RENDERER_FLAGS_USE_SURFACELESS_BIT;

    // The guest reaches us over a Hyper-V socket, with no memory the two share, so the
    // guest keeps a copy of host-visible memory and the stream carries its contents. The
    // decoder copies them only when neither GlDirectMem nor VirtioGpuNext is on, and the
    // guest decides from the same features.
    static const char kFeatures[] = "VirtioGpuNext:disabled";

    struct stream_renderer_param params[] = {
        {STREAM_RENDERER_PARAM_USER_DATA, reinterpret_cast<uint64_t>(&s_cookie)},
        {STREAM_RENDERER_PARAM_FENCE_CALLBACK, reinterpret_cast<uint64_t>(dummy_fence_cb)},
        {STREAM_RENDERER_PARAM_RENDERER_FLAGS, flags},
        {STREAM_RENDERER_PARAM_RENDERER_FEATURES, reinterpret_cast<uint64_t>(kFeatures)},
        {STREAM_RENDERER_PARAM_WIN0_WIDTH, width},
        {STREAM_RENDERER_PARAM_WIN0_HEIGHT, height},
    };

    int ret = stream_renderer_init(params, sizeof(params) / sizeof(params[0]));
    if (ret == 0) {
        s_initialized.store(true, std::memory_order_release);
    }
    return ret;
}

extern "C" DSA_GFX_EXPORT int dsa_gfx_select_gpu(uint32_t rule, const uint8_t* luid, const char* name) {
    if (s_initialized.load(std::memory_order_acquire)) {
        return -1;
    }
    using Rule = gfxstream::vk::GpuPreference::Rule;
    gfxstream::vk::GpuPreference preference;
    switch (rule) {
        case DSA_GFX_GPU_RANKING:
            preference.rule = Rule::None;
            break;
        case DSA_GFX_GPU_LUID:
            if (!luid) {
                return -1;
            }
            preference.rule = Rule::Luid;
            std::memcpy(preference.luid, luid, sizeof(preference.luid));
            break;
        case DSA_GFX_GPU_INTEGRATED:
            preference.rule = Rule::Integrated;
            break;
        case DSA_GFX_GPU_DISCRETE:
            preference.rule = Rule::Discrete;
            break;
        case DSA_GFX_GPU_NAME:
            if (!name || !*name) {
                return -1;
            }
            preference.rule = Rule::Name;
            preference.name = name;
            break;
        default:
            return -1;
    }
    gfxstream::vk::setGpuPreference(preference);
    return 0;
}

extern "C" DSA_GFX_EXPORT int dsa_gfx_get_gpu_info(dsa_gfx_gpu_info* info) {
    if (!info || !s_initialized.load(std::memory_order_acquire)) {
        return -1;
    }
    gfxstream::FrameBuffer* fb = gfxstream::FrameBuffer::getFB();
    if (!fb || !fb->hasEmulationVk()) {
        return -1;
    }
    gfxstream::vk::VkEmulation& emu = fb->getEmulationVk();
    gfxstream::vk::VulkanDispatch* ivk = emu.getInstanceDispatch();
    VkPhysicalDevice device = emu.getPhysicalDevice();

    std::memset(info, 0, sizeof(*info));
    VkPhysicalDeviceProperties properties = {};
    ivk->vkGetPhysicalDeviceProperties(device, &properties);
    std::strncpy(info->name, properties.deviceName, sizeof(info->name) - 1);
    info->device_type = static_cast<uint32_t>(properties.deviceType);

    auto get_properties2 = ivk->vkGetPhysicalDeviceProperties2
                               ? ivk->vkGetPhysicalDeviceProperties2
                               : ivk->vkGetPhysicalDeviceProperties2KHR;
    if (get_properties2) {
        VkPhysicalDeviceIDProperties id_properties = {};
        id_properties.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ID_PROPERTIES;
        VkPhysicalDeviceProperties2 properties2 = {};
        properties2.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2;
        properties2.pNext = &id_properties;
        get_properties2(device, &properties2);
        if (id_properties.deviceLUIDValid) {
            std::memcpy(info->luid, id_properties.deviceLUID, sizeof(info->luid));
            info->luid_valid = 1;
        }
    }
    info->preference_matched = gfxstream::vk::gpuPreferenceMatched();
    return 0;
}

extern "C" DSA_GFX_EXPORT void dsa_gfx_teardown(void) {
    if (!s_initialized.load(std::memory_order_acquire)) {
        return;
    }
    dsa_gfx_display_shutdown();
    stream_renderer_teardown();
    s_initialized.store(false, std::memory_order_release);
}

extern "C" DSA_GFX_EXPORT uint64_t dsa_gfx_on_process_create(void) {
    uint64_t puid = s_next_puid.fetch_add(1, std::memory_order_relaxed);
    const auto& renderer = android_getOpenglesRenderer();
    if (renderer) {
        renderer->onGuestGraphicsProcessCreate(puid);
    }
    return puid;
}

extern "C" DSA_GFX_EXPORT void dsa_gfx_on_process_destroy(uint64_t puid) {
    const auto& renderer = android_getOpenglesRenderer();
    if (renderer) {
        renderer->cleanupProcGLObjects(puid);
    }
}

extern "C" DSA_GFX_EXPORT int dsa_gfx_process_pipe(dsa_stream_read_fn read_cb,
                                                  dsa_stream_write_fn write_cb,
                                                  void* user_data) {
    if (!read_cb || !write_cb) {
        return -1;
    }

    int32_t confirm = 0;
    size_t received = 0;
    while (received < sizeof(confirm)) {
        int n = read_cb(user_data, reinterpret_cast<char*>(&confirm) + received, sizeof(confirm) - received);
        if (n < 0) {
            return -1;
        }
        if (n == 0) {
            continue;
        }
        received += static_cast<size_t>(n);
    }

    if (confirm != 100) {
        return -2;
    }

    uint64_t puid = dsa_gfx_on_process_create();

    size_t sent = 0;
    while (sent < sizeof(puid)) {
        int n = write_cb(user_data, reinterpret_cast<const char*>(&puid) + sent, sizeof(puid) - sent);
        if (n <= 0) {
            dsa_gfx_on_process_destroy(puid);
            return -3;
        }
        sent += static_cast<size_t>(n);
    }

    /* Keep connection alive until client disconnects. */
    char dummy[256];
    for (;;) {
        int n = read_cb(user_data, dummy, sizeof(dummy));
        if (n < 0) {
            break;
        }
    }

    dsa_gfx_on_process_destroy(puid);
    return 0;
}

extern "C" DSA_GFX_EXPORT void* dsa_gfx_channel_create(void) {
    const auto& renderer = android_getOpenglesRenderer();
    if (!renderer) {
        return nullptr;
    }
    auto channel = renderer->createRenderChannel();
    if (!channel) {
        return nullptr;
    }
    return new DsaChannelHolder{std::move(channel)};
}

extern "C" DSA_GFX_EXPORT void dsa_gfx_channel_destroy(void* handle) {
    auto* holder = static_cast<DsaChannelHolder*>(handle);
    delete holder;
}

extern "C" DSA_GFX_EXPORT void dsa_gfx_channel_stop(void* handle) {
    if (!handle) {
        return;
    }
    auto* holder = static_cast<DsaChannelHolder*>(handle);
    if (holder->channel) {
        holder->channel->stop();
    }
}

extern "C" DSA_GFX_EXPORT int dsa_gfx_channel_write(void* handle, const void* buf, size_t size) {
    if (!handle || (!buf && size != 0)) {
        return -1;
    }
    auto* holder = static_cast<DsaChannelHolder*>(handle);
    auto& channel = holder->channel;
    if (!channel) {
        return -1;
    }

    const auto* src = static_cast<const char*>(buf);
    gfxstream::RenderChannel::Buffer buffer(src, src + size);
    auto res = channel->tryWrite(std::move(buffer));
    while (res == gfxstream::RenderChannel::IoResult::TryAgain) {
        channel->waitUntilWritable();
        if ((channel->state() & gfxstream::RenderChannel::State::Stopped) != gfxstream::RenderChannel::State::Empty) {
            return -1;
        }
        res = channel->tryWrite(std::move(buffer));
    }
    return res == gfxstream::RenderChannel::IoResult::Ok ? 0 : -1;
}

extern "C" DSA_GFX_EXPORT int dsa_gfx_channel_read(void* handle, void* buf, size_t size, size_t* out_read, uint32_t timeout_ms) {
    if (!handle || !buf || !out_read) {
        return -1;
    }
    auto* holder = static_cast<DsaChannelHolder*>(handle);
    auto& channel = holder->channel;
    if (!channel) {
        return -1;
    }

    gfxstream::RenderChannel::Buffer in_buffer;
    gfxstream::RenderChannel::IoResult res;
    if (timeout_ms == 0) {
        res = channel->tryRead(&in_buffer);
    } else {
        auto wait_us = static_cast<gfxstream::RenderChannel::Duration>(timeout_ms) * 1000;
        auto deadline = android::base::getUnixTimeUs() + wait_us;
        res = channel->readBefore(&in_buffer, deadline);
    }

    if (res == gfxstream::RenderChannel::IoResult::Ok) {
        size_t count = std::min(size, in_buffer.size());
        std::memcpy(buf, in_buffer.data(), count);
        *out_read = count;
        return 0;
    } else if (res == gfxstream::RenderChannel::IoResult::TryAgain) {
        *out_read = 0;
        return 0;
    } else {
        *out_read = 0;
        return -1;
    }
}

extern "C" DSA_GFX_EXPORT int dsa_gfx_serve_stream(dsa_stream_read_fn read_cb,
                                                  dsa_stream_write_fn write_cb,
                                                  void* user_data) {
    if (!read_cb || !write_cb) {
        return -1;
    }
    const auto& renderer = android_getOpenglesRenderer();
    if (!renderer) {
        return -1;
    }
    auto channel = renderer->createRenderChannel();
    if (!channel) {
        return -2;
    }

    std::atomic<bool> stopped{false};

    /* Thread 1: pump Host -> Guest */
    std::thread host_to_guest([&]() {
        while (!stopped.load(std::memory_order_relaxed)) {
            gfxstream::RenderChannel::Buffer out_buf;
            const gfxstream::RenderChannel::Duration kWaitUs = 10000; /* 10ms */
            auto deadline = android::base::getUnixTimeUs() + kWaitUs;
            auto res = channel->readBefore(&out_buf, deadline);
            if (res == gfxstream::RenderChannel::IoResult::Ok) {
                if (out_buf.size() > 0) {
                    int written = write_cb(user_data, out_buf.data(), out_buf.size());
                    if (written < 0) {
                        stopped.store(true, std::memory_order_relaxed);
                        channel->stop();
                        break;
                    }
                }
            } else if (res == gfxstream::RenderChannel::IoResult::Error) {
                stopped.store(true, std::memory_order_relaxed);
                break;
            }
        }
    });

    /* Calling thread: pump Guest -> Host */
    std::vector<char> in_buf(64 * 1024);
    while (!stopped.load(std::memory_order_relaxed)) {
        int n = read_cb(user_data, in_buf.data(), in_buf.size());
        if (n < 0) {
            stopped.store(true, std::memory_order_relaxed);
            channel->stop();
            break;
        }
        if (n == 0) {
            continue;
        }
        gfxstream::RenderChannel::Buffer buf(in_buf.data(), in_buf.data() + n);
        auto res = channel->tryWrite(std::move(buf));
        while (res == gfxstream::RenderChannel::IoResult::TryAgain && !stopped.load(std::memory_order_relaxed)) {
            channel->waitUntilWritable();
            if ((channel->state() & gfxstream::RenderChannel::State::Stopped) != gfxstream::RenderChannel::State::Empty) {
                stopped.store(true, std::memory_order_relaxed);
                break;
            }
            res = channel->tryWrite(std::move(buf));
        }
        if (res == gfxstream::RenderChannel::IoResult::Error) {
            stopped.store(true, std::memory_order_relaxed);
            break;
        }
    }

    stopped.store(true, std::memory_order_relaxed);
    channel->stop();
    if (host_to_guest.joinable()) {
        host_to_guest.join();
    }
    return 0;
}

extern "C" DSA_GFX_EXPORT int dsa_gfx_set_post_callback(dsa_post_callback_fn cb, void* user_data) {
    const auto& renderer = android_getOpenglesRenderer();
    if (!renderer) {
        return -1;
    }
    if (cb) {
        renderer->setPostCallback(internal_on_post, reinterpret_cast<void*>(cb), false, 0);
    } else {
        renderer->setPostCallback(nullptr, nullptr, false, 0);
    }
    return 0;
}

extern "C" DSA_GFX_EXPORT int dsa_gfx_set_color_buffer_post_callback(dsa_color_buffer_post_fn cb, void* user_data) {
    std::lock_guard<std::mutex> lock(s_post_cb_mutex);
    s_color_buffer_post_cb = cb;
    s_color_buffer_post_ctx = user_data;
    return 0;
}
