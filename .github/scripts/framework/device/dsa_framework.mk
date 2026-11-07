# A minimal product for building SurfaceFlinger, libhwui and libEGL with DSA's patches. It
# starts from the product AOSP uses to build modules on their own (module_common.mk, a Soong
# only build) instead of Cuttlefish's phone, so product configuration reads no device, vendor
# or app makefiles, none of which the sparse checkout has. None of the Cuttlefish product's
# settings reach these modules: their Android.bp files read no product or Soong config
# variable, and the release configuration (the build's own, from the lock) sets their flags.
$(call inherit-product, $(SRC_TARGET_DIR)/product/module_common.mk)
$(call inherit-product, $(SRC_TARGET_DIR)/product/core_64_bit_only.mk)

PRODUCT_NAME := dsa_framework
PRODUCT_DEVICE := dsa_framework
PRODUCT_BRAND := Android
PRODUCT_MODEL := DSA framework build
