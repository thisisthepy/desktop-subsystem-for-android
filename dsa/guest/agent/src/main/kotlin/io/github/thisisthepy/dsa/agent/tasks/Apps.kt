package io.github.thisisthepy.dsa.agent.tasks

/** An app's own name and launcher icon, as the host's window for the app shows them. */
class AppDetails(
    /** The app's name, as a launcher shows it. */
    val label: String,
    /** The launcher icon as a square PNG image; empty when the app has none to draw. */
    val iconPng: ByteArray,
)

/** Looks up the name and icon of an installed app. */
fun interface AppInfoSource {
    /** The details of [packageName], or null when the package is not installed. */
    fun appInfo(packageName: String): AppDetails?

    companion object {
        /** Knows no app, so no app's details are sent. */
        val NONE = AppInfoSource { null }
    }
}
