// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Link-time stand-in for the libbinder_ndk.so calls a HAL service makes that the NDK's copy
// of the library leaves out. Only the symbols matter: on the device the hwcomposer loads
// the platform's own library. check-guest-driver.py checks each one resolves there.

#include <stdbool.h>
#include <stdint.h>

typedef struct AIBinder AIBinder;
typedef int32_t binder_status_t;
typedef int32_t binder_exception_t;

binder_status_t AServiceManager_addService(AIBinder* binder, const char* instance) { return -1; }
AIBinder* AServiceManager_checkService(const char* instance) { return 0; }
bool ABinderProcess_setThreadPoolMaxThreadCount(uint32_t numThreads) { return false; }
void ABinderProcess_startThreadPool(void) {}
void ABinderProcess_joinThreadPool(void) {}
void AIBinder_markVintfStability(AIBinder* binder) {}
void AIBinder_setMinSchedulerPolicy(AIBinder* binder, int policy, int priority) {}
void AIBinder_setInheritRt(AIBinder* binder, bool inheritRt) {}
void AIBinder_markVendorStability(AIBinder* binder) {}
void AIBinder_forceDowngradeToVendorStability(AIBinder* binder) {}
