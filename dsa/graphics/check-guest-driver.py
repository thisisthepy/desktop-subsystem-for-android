# /// script
# requires-python = ">=3.10"
# dependencies = ["pyelftools"]
# ///
"""Checks that the guest Vulkan driver can load on the image it ships in.

The Android Vulkan loader opens the driver with immediate binding, so every symbol the
driver imports must be defined by a library it loads, or the driver fails to load and
every app falls back to a stub with no Vulkan at all. A link on the build machine cannot
prove this: it resolves against stubs, and a C++ symbol built against the NDK's libc++
is mangled differently from the platform's. This check resolves the driver's imports
against the libraries in the image's own target_files.

    uv run dsa/graphics/check-guest-driver.py vulkan.ranchu.so target_files.zip
"""

import io
import sys
import zipfile
from dataclasses import dataclass, field
from pathlib import Path

# Where a vendor driver's dependencies are found: the vendor's own libraries, the
# platform libraries it may use, and bionic (which lives in an APEX, with a copy here).
LIBRARY_DIRS = ["VENDOR/lib64/", "SYSTEM/lib64/", "SYSTEM/lib64/bootstrap/"]

HAL_MODULE = "HMI"


@dataclass
class Library:
    needed: list = field(default_factory=list)
    defined: set = field(default_factory=set)
    undefined: set = field(default_factory=set)
    weak: set = field(default_factory=set)


def problems(driver, image):
    """What stops the driver loading, given the image's libraries by file name."""
    found = []
    if HAL_MODULE not in driver.defined:
        found.append(f"the driver does not export {HAL_MODULE}, the module the Android Vulkan loader looks up")

    # The dynamic linker searches the driver's dependencies and theirs, breadth first.
    available = set()
    queue = list(driver.needed)
    seen = set()
    while queue:
        name = queue.pop(0)
        if name in seen:
            continue
        seen.add(name)
        lib = image.get(name)
        if lib is None:
            if name in driver.needed:
                found.append(f"the image has no {name}")
            continue
        available |= lib.defined
        queue += lib.needed

    for symbol in sorted(driver.undefined - driver.weak):
        if symbol not in available:
            found.append(f"{symbol} is not defined by any library the driver loads")
    return found


def read_library(data):
    from elftools.elf.elffile import ELFFile

    elf = ELFFile(io.BytesIO(data))
    lib = Library()
    dynamic = elf.get_section_by_name(".dynamic")
    if dynamic is not None:
        lib.needed = [tag.needed for tag in dynamic.iter_tags() if tag.entry.d_tag == "DT_NEEDED"]
    dynsym = elf.get_section_by_name(".dynsym")
    if dynsym is not None:
        for sym in dynsym.iter_symbols():
            if not sym.name:
                continue
            if sym["st_shndx"] == "SHN_UNDEF":
                lib.undefined.add(sym.name)
                if sym["st_info"]["bind"] == "STB_WEAK":
                    lib.weak.add(sym.name)
            elif sym["st_info"]["bind"] in ("STB_GLOBAL", "STB_WEAK"):
                lib.defined.add(sym.name)
    return lib


def find_library(z, name):
    """The contents of the image's copy of a library, or None if it has none.

    target_files stores a symbolic link as the path it points to, so an entry that is not
    an ELF file is a link (SYSTEM/lib64/libc.so points into the runtime APEX); the search
    goes on to the next directory.
    """
    members = set(z.namelist())
    for directory in LIBRARY_DIRS:
        path = directory + name
        if path in members:
            data = z.read(path)
            if data.startswith(b"\x7fELF"):
                return data
    return None


def image_libraries(target_files, names):
    """The image's libraries that the driver may load, read as they are needed."""
    libs = {}
    with zipfile.ZipFile(target_files) as z:
        queue = list(names)
        while queue:
            name = queue.pop(0)
            if name in libs:
                continue
            data = find_library(z, name)
            if data is None:
                continue
            libs[name] = read_library(data)
            queue += libs[name].needed
    return libs


def main(argv):
    if len(argv) != 3:
        print(__doc__.strip().splitlines()[-1].strip(), file=sys.stderr)
        return 2
    driver = read_library(Path(argv[1]).read_bytes())
    found = problems(driver, image_libraries(argv[2], driver.needed))
    for problem in found:
        print(f"problem: {problem}")
    if found:
        print(f"{len(found)} problems: the driver would not load on this image")
        return 1
    print(f"{Path(argv[1]).name} loads on this image: {len(driver.undefined)} imports resolve")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
