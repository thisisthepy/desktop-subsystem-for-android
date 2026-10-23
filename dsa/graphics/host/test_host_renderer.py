"""Smoke test for gfxstream host renderer initialized on software Vulkan."""

import ctypes
import os
import unittest
from pathlib import Path


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


if __name__ == "__main__":
    unittest.main()
