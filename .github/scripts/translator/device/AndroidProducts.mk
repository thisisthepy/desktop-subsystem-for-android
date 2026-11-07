# The product the translator build lunches: an x86_64, 64-bit only device whose native
# bridge guest is arm64. It builds nothing but the modules the translator package needs.
PRODUCT_MAKEFILES := $(LOCAL_DIR)/dsa_translator.mk
COMMON_LUNCH_CHOICES := dsa_translator-trunk_staging-userdebug
