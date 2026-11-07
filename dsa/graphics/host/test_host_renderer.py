"""Smoke test for gfxstream host renderer initialized on software Vulkan."""

import ctypes
import json
import os
import subprocess
import sys
import unittest
from pathlib import Path

DSA_GFX_GPU_LUID = 1
DSA_GFX_GPU_NAME = 4


class GpuInfo(ctypes.Structure):
    _fields_ = [
        ("name", ctypes.c_char * 256),
        ("device_type", ctypes.c_uint32),
        ("luid", ctypes.c_uint8 * 8),
        ("luid_valid", ctypes.c_int32),
        ("preference_matched", ctypes.c_int32),
    ]


def probe_gpu_selection(dll_path, rule, argument):
    """Asks the backend for a GPU, starts it, and prints what it chose as JSON.

    Runs in a process of its own, because the renderer starts once per process."""
    if hasattr(os, "add_dll_directory"):
        os.add_dll_directory(str(Path(dll_path).parent))
    lib = ctypes.CDLL(dll_path)
    lib.dsa_gfx_select_gpu.argtypes = [ctypes.c_uint32, ctypes.c_void_p, ctypes.c_char_p]
    lib.dsa_gfx_select_gpu.restype = ctypes.c_int
    lib.dsa_gfx_init.argtypes = [ctypes.c_uint32, ctypes.c_uint32]
    lib.dsa_gfx_init.restype = ctypes.c_int
    lib.dsa_gfx_get_gpu_info.argtypes = [ctypes.POINTER(GpuInfo)]
    lib.dsa_gfx_get_gpu_info.restype = ctypes.c_int
    lib.dsa_gfx_teardown.argtypes = []
    lib.dsa_gfx_teardown.restype = None

    luid = (ctypes.c_uint8 * 8)(*bytes.fromhex(argument)) if rule == DSA_GFX_GPU_LUID else None
    name = argument.encode() if rule == DSA_GFX_GPU_NAME else None
    result = {"select": lib.dsa_gfx_select_gpu(rule, luid and ctypes.addressof(luid), name)}
    result["init"] = lib.dsa_gfx_init(256, 256)
    info = GpuInfo()
    result["info"] = lib.dsa_gfx_get_gpu_info(ctypes.byref(info))
    result["name"] = info.name.decode(errors="replace")
    result["matched"] = info.preference_matched
    # Too late: the renderer has already chosen.
    result["select_after_init"] = lib.dsa_gfx_select_gpu(DSA_GFX_GPU_NAME, None, b"x")
    lib.dsa_gfx_teardown()
    print(json.dumps(result))


class StreamRendererParam(ctypes.Structure):
    _fields_ = [
        ("key", ctypes.c_uint64),
        ("value", ctypes.c_uint64),
    ]


class StreamRendererResourceCreateArgs(ctypes.Structure):
    _fields_ = [
        ("handle", ctypes.c_uint32),
        ("target", ctypes.c_uint32),
        ("format", ctypes.c_uint32),
        ("bind", ctypes.c_uint32),
        ("width", ctypes.c_uint32),
        ("height", ctypes.c_uint32),
        ("depth", ctypes.c_uint32),
        ("array_size", ctypes.c_uint32),
        ("last_level", ctypes.c_uint32),
        ("nr_samples", ctypes.c_uint32),
        ("flags", ctypes.c_uint32),
    ]


FENCE_CALLBACK_TYPE = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_void_p)

STREAM_RENDERER_PARAM_USER_DATA = 1
STREAM_RENDERER_PARAM_RENDERER_FLAGS = 2
STREAM_RENDERER_PARAM_FENCE_CALLBACK = 3
STREAM_RENDERER_PARAM_WIN0_WIDTH = 4
STREAM_RENDERER_PARAM_WIN0_HEIGHT = 5

STREAM_RENDERER_FLAGS_USE_SURFACELESS_BIT = 1 << 3
STREAM_RENDERER_FLAGS_USE_VK_BIT = 1 << 5

PIPE_TEXTURE_2D = 2
VIRGL_FORMAT_R8G8B8A8_UNORM = 67
VIRGL_BIND_SAMPLER_VIEW = 1 << 3
VIRGL_BIND_SCANOUT = 1 << 18
VIRGL_BIND_SHARED = 1 << 20


@FENCE_CALLBACK_TYPE
def dummy_fence_callback(user_data, fence_ptr):
    pass


class HostRendererSmokeTests(unittest.TestCase):
    def test_pr3_host_renderer_starts_on_vulkan(self):
        env_path = os.environ.get("GFXSTREAM_BACKEND_PATH")
        if not env_path or not Path(env_path).exists():
            self.skipTest("GFXSTREAM_BACKEND_PATH is not set or library does not exist.")

        dll_path = Path(env_path).resolve()
        if hasattr(os, "add_dll_directory"):
            os.add_dll_directory(str(dll_path.parent))

        lib = ctypes.CDLL(str(dll_path))

        lib.stream_renderer_init.argtypes = [
            ctypes.POINTER(StreamRendererParam),
            ctypes.c_uint64,
        ]
        lib.stream_renderer_init.restype = ctypes.c_int

        lib.stream_renderer_teardown.argtypes = []
        lib.stream_renderer_teardown.restype = None

        lib.stream_renderer_resource_create.argtypes = [
            ctypes.POINTER(StreamRendererResourceCreateArgs),
            ctypes.c_void_p,
            ctypes.c_uint32,
        ]
        lib.stream_renderer_resource_create.restype = ctypes.c_int

        lib.stream_renderer_resource_unref.argtypes = [ctypes.c_uint32]
        lib.stream_renderer_resource_unref.restype = None

        cookie = ctypes.c_uint64(0x1234)
        flags = STREAM_RENDERER_FLAGS_USE_VK_BIT | STREAM_RENDERER_FLAGS_USE_SURFACELESS_BIT

        params_list = [
            StreamRendererParam(STREAM_RENDERER_PARAM_USER_DATA, ctypes.addressof(cookie)),
            StreamRendererParam(
                STREAM_RENDERER_PARAM_FENCE_CALLBACK,
                ctypes.cast(dummy_fence_callback, ctypes.c_void_p).value,
            ),
            StreamRendererParam(STREAM_RENDERER_PARAM_RENDERER_FLAGS, flags),
            StreamRendererParam(STREAM_RENDERER_PARAM_WIN0_WIDTH, 256),
            StreamRendererParam(STREAM_RENDERER_PARAM_WIN0_HEIGHT, 256),
        ]
        params_array = (StreamRendererParam * len(params_list))(*params_list)

        init_ret = lib.stream_renderer_init(params_array, len(params_list))
        self.assertEqual(init_ret, 0, f"stream_renderer_init returned non-zero code {init_ret}")

        try:
            create_args = StreamRendererResourceCreateArgs(
                handle=1,
                target=PIPE_TEXTURE_2D,
                format=VIRGL_FORMAT_R8G8B8A8_UNORM,
                bind=VIRGL_BIND_SAMPLER_VIEW | VIRGL_BIND_SCANOUT | VIRGL_BIND_SHARED,
                width=256,
                height=256,
                depth=1,
                array_size=1,
                last_level=0,
                nr_samples=0,
                flags=0,
            )
            create_ret = lib.stream_renderer_resource_create(
                ctypes.byref(create_args), None, 0
            )
            self.assertEqual(
                create_ret,
                0,
                f"stream_renderer_resource_create failed with code {create_ret}",
            )

            lib.stream_renderer_resource_unref(1)
        finally:
            lib.stream_renderer_teardown()


def probe_init_without_vulkan(dll_path):
    """Starts the renderer on Vulkan in a process that has no Vulkan driver, and prints what
    stream_renderer_init returned. Runs in a process of its own, so that a crash shows up as
    the process dying rather than taking the test runner with it."""
    if hasattr(os, "add_dll_directory"):
        os.add_dll_directory(str(Path(dll_path).parent))
    lib = ctypes.CDLL(dll_path)
    lib.stream_renderer_init.argtypes = [ctypes.POINTER(StreamRendererParam), ctypes.c_uint64]
    lib.stream_renderer_init.restype = ctypes.c_int
    lib.stream_renderer_teardown.argtypes = []
    lib.stream_renderer_teardown.restype = None
    cookie = ctypes.c_uint64(0x1234)
    flags = STREAM_RENDERER_FLAGS_USE_VK_BIT | STREAM_RENDERER_FLAGS_USE_SURFACELESS_BIT
    params_list = [
        StreamRendererParam(STREAM_RENDERER_PARAM_USER_DATA, ctypes.addressof(cookie)),
        StreamRendererParam(
            STREAM_RENDERER_PARAM_FENCE_CALLBACK,
            ctypes.cast(dummy_fence_callback, ctypes.c_void_p).value,
        ),
        StreamRendererParam(STREAM_RENDERER_PARAM_RENDERER_FLAGS, flags),
        StreamRendererParam(STREAM_RENDERER_PARAM_WIN0_WIDTH, 256),
        StreamRendererParam(STREAM_RENDERER_PARAM_WIN0_HEIGHT, 256),
    ]
    params_array = (StreamRendererParam * len(params_list))(*params_list)
    result = lib.stream_renderer_init(params_array, len(params_list))
    if result == 0:
        lib.stream_renderer_teardown()
    print(json.dumps({"init": result}))


class MemoryStats(ctypes.Structure):
    _fields_ = [
        ("device_local_bytes", ctypes.c_uint64),
        ("host_visible_bytes", ctypes.c_uint64),
        ("imported_bytes", ctypes.c_uint64),
        ("largest_bytes", ctypes.c_uint64),
        ("device_local_count", ctypes.c_uint32),
        ("host_visible_count", ctypes.c_uint32),
        ("imported_count", ctypes.c_uint32),
        ("guest_processes", ctypes.c_int32),
    ]


def probe_memory_stats(dll_path):
    """Starts the renderer, reads its memory accounting, and prints it as JSON. Runs in a
    process of its own, because the renderer starts once per process."""
    if hasattr(os, "add_dll_directory"):
        os.add_dll_directory(str(Path(dll_path).parent))
    lib = ctypes.CDLL(dll_path)
    lib.dsa_gfx_init.argtypes = [ctypes.c_uint32, ctypes.c_uint32]
    lib.dsa_gfx_init.restype = ctypes.c_int
    lib.dsa_gfx_get_memory_stats.argtypes = [ctypes.POINTER(MemoryStats)]
    lib.dsa_gfx_get_memory_stats.restype = ctypes.c_int
    lib.dsa_gfx_teardown.argtypes = []
    lib.dsa_gfx_teardown.restype = None
    before = lib.dsa_gfx_get_memory_stats(ctypes.byref(MemoryStats()))
    init = lib.dsa_gfx_init(256, 256)
    stats = MemoryStats()
    running = lib.dsa_gfx_get_memory_stats(ctypes.byref(stats))
    lib.dsa_gfx_teardown()
    after = lib.dsa_gfx_get_memory_stats(ctypes.byref(MemoryStats()))
    print(json.dumps({
        "init": init,
        "before": before,
        "running": running,
        "after": after,
        "processes": stats.guest_processes,
    }))


class RendererMemoryTests(unittest.TestCase):
    def test_nfr2_the_renderer_reports_its_memory_only_while_it_runs(self):
        env_path = os.environ.get("GFXSTREAM_BACKEND_PATH")
        if not env_path or not Path(env_path).exists():
            self.skipTest("GFXSTREAM_BACKEND_PATH is not set or library does not exist.")
        completed = subprocess.run(
            [sys.executable, __file__, "--probe-memory", str(Path(env_path).resolve())],
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        result = json.loads(completed.stdout.strip().splitlines()[-1])
        self.assertEqual(result["init"], 0, result)
        self.assertEqual(result["before"], -1, result)
        self.assertEqual(result["running"], 0, result)
        self.assertEqual(result["after"], -1, result)
        self.assertEqual(result["processes"], 0, result)


class RendererWithoutVulkanTests(unittest.TestCase):
    def test_pr3_renderer_survives_a_host_without_a_vulkan_driver(self):
        env_path = os.environ.get("GFXSTREAM_BACKEND_PATH")
        if not env_path or not Path(env_path).exists():
            self.skipTest("GFXSTREAM_BACKEND_PATH is not set or library does not exist.")
        env = dict(os.environ)
        # The loader finds no driver: the one manifest it is pointed at does not exist.
        missing = str(Path(env_path).resolve().parent / "no-such-vulkan-driver.json")
        env["VK_ICD_FILENAMES"] = missing
        env["VK_DRIVER_FILES"] = missing
        env.pop("ANDROID_EMU_VK_ICD", None)
        done = subprocess.run(
            [sys.executable, __file__, "--probe-init-without-vulkan",
             str(Path(env_path).resolve())],
            capture_output=True,
            text=True,
            timeout=300,
            env=env,
        )
        # Starting may fail, but with an answer, not a crash.
        self.assertGreaterEqual(
            done.returncode, 0, f"the renderer died with signal {-done.returncode}:\n{done.stderr}"
        )
        self.assertEqual(done.returncode, 0, done.stderr)
        result = json.loads(done.stdout.strip().splitlines()[-1])
        self.assertIsInstance(result["init"], int)


class RenderGpuSelectionTests(unittest.TestCase):
    def run_probe(self, rule, argument):
        env_path = os.environ.get("GFXSTREAM_BACKEND_PATH")
        if not env_path or not Path(env_path).exists():
            self.skipTest("GFXSTREAM_BACKEND_PATH is not set or library does not exist.")
        done = subprocess.run(
            [sys.executable, __file__, "--probe-gpu", str(Path(env_path).resolve()), str(rule), argument],
            capture_output=True,
            text=True,
            timeout=300,
        )
        self.assertEqual(done.returncode, 0, done.stderr)
        result = json.loads(done.stdout.strip().splitlines()[-1])
        self.assertEqual(result["select"], 0)
        self.assertEqual(result["init"], 0)
        self.assertEqual(result["info"], 0)
        self.assertEqual(result["select_after_init"], -1)
        return result

    # The one GPU the runner has: Lavapipe's Vulkan device is called "llvmpipe (LLVM ...)",
    # and on a Mac runner MoltenVK shows "Apple Paravirtual device".
    gpu_name = os.environ.get("DSA_TEST_GPU_NAME", "llvmpipe").lower()

    def test_pr3_renderer_runs_on_the_gpu_asked_for(self):
        # Asked in another case, so the match is not case sensitive.
        result = self.run_probe(DSA_GFX_GPU_NAME, self.gpu_name.upper())
        self.assertEqual(result["matched"], 1)
        self.assertIn(self.gpu_name, result["name"].lower())

    def test_pr3_renderer_ranks_the_gpus_when_none_matches(self):
        # No GPU has this LUID, so the renderer ranks them and says it did.
        result = self.run_probe(DSA_GFX_GPU_LUID, "ffffffffffffff7f")
        self.assertEqual(result["matched"], 0)
        self.assertIn(self.gpu_name, result["name"].lower())


if __name__ == "__main__":
    if len(sys.argv) == 5 and sys.argv[1] == "--probe-gpu":
        probe_gpu_selection(sys.argv[2], int(sys.argv[3]), sys.argv[4])
    elif len(sys.argv) == 3 and sys.argv[1] == "--probe-memory":
        probe_memory_stats(sys.argv[2])
    elif len(sys.argv) == 3 and sys.argv[1] == "--probe-init-without-vulkan":
        probe_init_without_vulkan(sys.argv[2])
    else:
        unittest.main()
