# x86_64 only, as Cuttlefish's vsoc_x86_64_only board sets it, so the code is built for the
# same CPU as the image's own.
TARGET_CPU_ABI := x86_64
TARGET_ARCH := x86_64
TARGET_ARCH_VARIANT := silvermont

TARGET_NO_KERNEL := true
TARGET_NO_BOOTLOADER := true

# Nothing here is Java, and the tree has no ART to compile with: with dexpreopt on, Soong's
# boot image step looks for dex2oat and stops the whole analysis without it.
WITH_DEXPREOPT := false
