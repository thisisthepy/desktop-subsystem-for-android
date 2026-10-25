package io.github.thisisthepy.dsa.channel;
import io.github.thisisthepy.dsa.channel.IChannelListener;
@VintfStability
interface IChannel {
    // Sends one control frame (one encoded dsa.control.v1.Envelope) to the host.
    void send(in byte[] frame);
    // Frames from the host, and the task messages other guest clients send, go to
    // `listener` until its process dies.
    void subscribe(in IChannelListener listener);
}
