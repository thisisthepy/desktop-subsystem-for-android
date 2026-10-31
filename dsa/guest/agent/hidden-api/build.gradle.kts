// Compile-only declarations of framework classes the SDK does not publish. Nothing here is
// packaged: the app depends on this project with compileOnly, and at run time the classes
// come from the guest's framework. Each declaration mirrors the framework's own signature,
// so a call compiled against it links to the real method.

plugins {
    id("com.android.library")
}

android {
    namespace = "io.github.thisisthepy.dsa.agent.hiddenapi"
    compileSdk = 36

    defaultConfig {
        minSdk = 36
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}
