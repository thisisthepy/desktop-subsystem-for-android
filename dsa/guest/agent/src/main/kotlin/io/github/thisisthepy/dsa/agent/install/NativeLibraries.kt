package io.github.thisisthepy.dsa.agent.install

import io.github.thisisthepy.dsa.control.v1.NativeLibraries as NativeLibrariesMessage

/**
 * The native libraries of an installed app, by ABI: the file names under `lib/<abi>/` of its
 * base APK and its splits together. The host compares them to tell whether the app's x86_64
 * code is whole, and whether an APK on the device holds its ARM code, which the "Run as ARM"
 * switch needs.
 */
object NativeLibraries {
    /** The most ABIs and the most names of one ABI that are reported; real apps have far fewer. */
    const val MAX_ABIS = 8
    const val MAX_NAMES = 2048

    /** The libraries among the entry names of each APK, merged: ABI to library names, both in order. */
    fun of(apks: List<Iterable<String>>): Map<String, List<String>> {
        val byAbi = sortedMapOf<String, java.util.SortedSet<String>>()
        for (entries in apks) {
            for (entry in entries) {
                val (abi, name) = library(entry) ?: continue
                if (abi !in byAbi && byAbi.size >= MAX_ABIS) continue
                val names = byAbi.getOrPut(abi) { sortedSetOf() }
                if (names.size < MAX_NAMES) names += name
            }
        }
        return byAbi.mapValues { it.value.toList() }
    }

    /** The ABI and the file name of [entry] when it is a native library (`lib/<abi>/<name>.so`); else null. */
    fun library(entry: String): Pair<String, String>? {
        if (!entry.startsWith("lib/")) return null
        val rest = entry.removePrefix("lib/")
        val slash = rest.indexOf('/')
        if (slash <= 0) return null
        val abi = rest.substring(0, slash)
        val name = rest.substring(slash + 1)
        val abiOk = abi.length <= 32 && abi.all { it in 'a'..'z' || it in '0'..'9' || it == '_' || it == '-' }
        val nameOk = name.length in 4..255 && name.endsWith(".so") && '/' !in name && '\\' !in name &&
            name.none { it.isISOControl() }
        return if (abiOk && nameOk) abi to name else null
    }

    /** [libraries] as the control channel carries them. */
    fun messages(libraries: Map<String, List<String>>): List<NativeLibrariesMessage> =
        libraries.map { (abi, names) -> NativeLibrariesMessage.newBuilder().setAbi(abi).addAllNames(names).build() }
}
