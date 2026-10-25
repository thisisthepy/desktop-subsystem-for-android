package io.github.thisisthepy.dsa.agent.platform

import android.os.IBinder
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.channel.ChannelEndpoint
import io.github.thisisthepy.dsa.agent.channel.ChannelLocator
import io.github.thisisthepy.dsa.channel.IChannel
import io.github.thisisthepy.dsa.channel.IChannelListener

/**
 * Finds the control daemon's binder service in the service manager. ServiceManager is not in
 * the SDK and is called by reflection; checkService returns at once, null while the daemon
 * has not registered.
 */
class BinderChannelLocator(private val log: Log) : ChannelLocator {
    private val checkService = Class.forName("android.os.ServiceManager").getMethod("checkService", String::class.java)

    override fun find(): ChannelEndpoint? {
        val binder = checkService.invoke(null, SERVICE) as IBinder? ?: return null
        return BinderChannel(binder, IChannel.Stub.asInterface(binder), log)
    }

    private companion object {
        const val SERVICE = "io.github.thisisthepy.dsa.channel.IChannel/default"
    }
}

private class BinderChannel(
    private val binder: IBinder,
    private val channel: IChannel,
    private val log: Log,
) : ChannelEndpoint {
    // Held so the daemon's reference keeps working for as long as this connection lasts.
    private var listener: IChannelListener.Stub? = null
    private var death: IBinder.DeathRecipient? = null

    override fun send(frame: ByteArray) {
        channel.send(frame)
    }

    override fun subscribe(onFrame: (ByteArray) -> Unit) {
        val stub = object : IChannelListener.Stub() {
            override fun onFrame(frame: ByteArray?) {
                if (frame == null) {
                    log.warn("the control daemon delivered an empty frame")
                    return
                }
                onFrame(frame)
            }

            override fun getInterfaceVersion(): Int = IChannelListener.VERSION
        }
        listener = stub
        channel.subscribe(stub)
    }

    override fun onGone(onGone: () -> Unit) {
        val recipient = IBinder.DeathRecipient { onGone() }
        binder.linkToDeath(recipient, 0)
        death = recipient
    }
}
