#!/usr/bin/env bash
# The gfxstream transport patch is source-controlled but the upstream tree is fetched only
# for a graphics build. Keep its pin and its required Hyper-V socket adapter visible to the
# fast repository gate.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

uv run dsa/graphics/test_fetch.py
uv run dsa/graphics/test_hvsocket_patch.py
uv run dsa/graphics/test_stream_loss_patch.py
uv run dsa/graphics/test_cpu_readback_patch.py
uv run dsa/graphics/test_hardware_buffer_import.py
uv run dsa/graphics/test_coherent_memory_patch.py
uv run dsa/graphics/test_noncoherent_memory_patch.py
uv run dsa/graphics/test_mapped_memory_patch.py
uv run dsa/graphics/test_readback_patch.py
uv run dsa/graphics/test_memory_count_patch.py
uv run dsa/graphics/test_device_garbage_patch.py
uv run dsa/graphics/test_free_retired_patch.py
uv run dsa/graphics/test_lazy_staging_patch.py
uv run dsa/graphics/test_upload_order_patch.py
uv run dsa/graphics/test_guest_release_patch.py
uv run dsa/graphics/test_stream_buffers_patch.py
uv run dsa/graphics/test_spare_pages_patch.py
uv run dsa/graphics/test_color_buffer_refs_patch.py
uv run dsa/graphics/test_build.py
uv run dsa/graphics/test_check_guest_driver.py
uv run dsa/graphics/host/test_host_build.py
uv run dsa/graphics/host/test_host_renderer.py
uv run dsa/graphics/verify/test_verify.py
