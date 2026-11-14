# The product the framework build lunches: a 64-bit only device of the image's architecture
# (BoardConfig.mk, which build.sh takes from x86_64/ or arm64/). It builds nothing but the
# files the image takes: SurfaceFlinger, libhwui, libEGL and DSA's KeyMint.
PRODUCT_MAKEFILES := $(LOCAL_DIR)/dsa_framework.mk
