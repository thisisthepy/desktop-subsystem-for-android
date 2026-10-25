package io.github.thisisthepy.dsa.channel;
@VintfStability
interface IChannelListener {
    oneway void onFrame(in byte[] frame);
}
