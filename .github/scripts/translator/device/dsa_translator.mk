# A minimal product for building the ARM64 to x86_64 translator (Digitalis) and its arm64
# guest libraries. It starts from the product AOSP uses to build modules on their own
# (module_common.mk, a Soong-only build) instead of a phone, so product configuration
# reads no device, vendor or app makefiles, none of which the sparse checkout has.
$(call inherit-product, $(SRC_TARGET_DIR)/product/module_common.mk)
$(call inherit-product, $(SRC_TARGET_DIR)/product/core_64_bit_only.mk)

# What frameworks/libs/binary_translation/enable_arm64_to_x86_64.mk sets for the build
# itself. Its PRODUCT_PACKAGES and system properties are left out: the build names its
# modules one by one, and DSA's image sets the properties when the package is used.
PRODUCT_SOONG_NAMESPACES += frameworks/libs/native_bridge_support/android_api/libc
BUILD_BERBERIS := true
BUILD_BERBERIS_ARM64_TO_X86_64 := true
$(call soong_config_set,berberis,translation_arch,arm64_to_x86_64)

PRODUCT_NAME := dsa_translator
PRODUCT_DEVICE := dsa_translator
PRODUCT_BRAND := Android
PRODUCT_MODEL := DSA translator build
