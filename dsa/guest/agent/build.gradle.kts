// The guest agent app. Build it from this folder:
//
//   ./gradlew assembleRelease testDebugUnitTest
//
// The release APK is unsigned: the image build signs it with the platform key, which is what
// makes it a platform app.

import org.gradle.api.DefaultTask
import org.gradle.api.file.DirectoryProperty
import org.gradle.api.file.RegularFileProperty
import org.gradle.api.tasks.InputDirectory
import org.gradle.api.tasks.InputFile
import org.gradle.api.tasks.OutputDirectory
import org.gradle.api.tasks.PathSensitive
import org.gradle.api.tasks.PathSensitivity
import org.gradle.api.tasks.TaskAction
import org.gradle.process.ExecOperations
import javax.inject.Inject

plugins {
    id("com.android.application") version "8.10.1"
    id("com.android.library") version "8.10.1" apply false
    id("org.jetbrains.kotlin.android") version "2.2.20"
}

android {
    namespace = "io.github.thisisthepy.dsa.agent"
    compileSdk = 36

    defaultConfig {
        applicationId = "io.github.thisisthepy.dsa.agent"
        // The guest images are Android 16 (API 36) and later.
        minSdk = 36
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // Signed by the image build with the platform key.
            signingConfig = null
        }
    }

    sourceSets["main"].java.srcDir("../../protocol/java/src/main/java")

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    lint {
        // The agent uses permissions only platform apps hold and framework classes the SDK
        // does not publish, on purpose; lint reports both as errors.
        checkReleaseBuilds = false
        abortOnError = false
    }

    testOptions {
        unitTests.isReturnDefaultValues = false
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}

dependencies {
    // The runtime of the protoc release the checked-in Java codec was generated with.
    implementation("com.google.protobuf:protobuf-javalite:3.21.12")
    compileOnly(project(":hidden-api"))
    testImplementation("junit:junit:4.13.2")
}

/**
 * Generates the Java binder classes of the channel interface from the AIDL files the guest's
 * control daemon is built from, as a stable (VINTF) interface.
 *
 * A stable interface's stub marks itself with Binder.markVintfStability(), which the SDK
 * does not publish. The generated call is pointed at VintfStability.mark(), which reaches
 * the same method at run time; the build fails if the generated code no longer has the call.
 */
abstract class ChannelAidl : DefaultTask() {
    @get:InputFile
    @get:PathSensitive(PathSensitivity.NONE)
    abstract val aidl: RegularFileProperty

    @get:InputDirectory
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val sourceDir: DirectoryProperty

    @get:OutputDirectory
    abstract val outputDir: DirectoryProperty

    @get:Inject
    abstract val exec: ExecOperations

    @TaskAction
    fun generate() {
        val out = outputDir.get().asFile
        out.deleteRecursively()
        out.mkdirs()
        val root = sourceDir.get().asFile
        val inputs = root.walkTopDown().filter { it.isFile && it.extension == "aidl" }.sortedBy { it.path }.toList()
        check(inputs.isNotEmpty()) { "no AIDL files under $root" }
        exec.exec {
            commandLine(
                listOf(
                    aidl.get().asFile.absolutePath,
                    "--lang=java",
                    "--structured",
                    "--stability=vintf",
                    "--version=1",
                    "-I", root.absolutePath,
                    "-o", out.absolutePath,
                ) + inputs.map { it.absolutePath },
            )
        }
        val call = "this.markVintfStability();"
        val replacement = "io.github.thisisthepy.dsa.agent.channel.VintfStability.mark(this);"
        out.walkTopDown().filter { it.isFile && it.extension == "java" }.forEach { file ->
            val text = file.readText()
            check(call in text) { "${file.name} no longer marks its stub stable; check the aidl output" }
            file.writeText(text.replace(call, replacement))
        }
    }
}

val channelAidl = tasks.register<ChannelAidl>("channelAidl") {
    aidl.set(androidComponents.sdkComponents.aidl.flatMap { it.executable })
    sourceDir.set(layout.projectDirectory.dir("../aidl"))
    outputDir.set(layout.buildDirectory.dir("generated/source/channelAidl"))
}

androidComponents {
    onVariants { variant ->
        variant.sources.java?.addGeneratedSourceDirectory(channelAidl, ChannelAidl::outputDir)
    }
}
