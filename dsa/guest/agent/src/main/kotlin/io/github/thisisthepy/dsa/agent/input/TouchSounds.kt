package io.github.thisisthepy.dsa.agent.input

/**
 * Turns off the sound an app's click plays.
 *
 * Nothing carries the guest's sound to the host, so nobody can hear it. It still costs: on a
 * click the framework decodes the sound effects if it has not yet, and mixes one, in the
 * moments the app has to answer that click.
 */
fun interface TouchSounds {
    fun turnOff()

    companion object {
        val NONE = TouchSounds {}
    }
}
