// The guest agent: a platform-signed system app that reports app tasks to the host and
// acts on the host's requests. The root project is the app; hidden-api holds compile-only
// declarations of the framework classes the app links against but the SDK does not publish.

pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "dsa-agent"
include(":hidden-api")
