// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

package io.github.thisisthepy.dsa.mediaprobe;

import android.os.Bundle;
import android.system.ErrnoException;
import android.system.Os;
import android.system.OsConstants;
import android.util.Log;

import java.io.BufferedReader;
import java.io.FileReader;
import java.io.IOException;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;

import javax.net.ssl.HttpsURLConnection;
import javax.net.ssl.SSLException;
import javax.net.ssl.SSLSocket;

/**
 * Opens an HTTPS address as an ordinary app does, with the platform's default trust: the
 * system certificates under the app's network security config. It tells apart a connection
 * that worked, one the certificate check refused, and one that never reached a server:
 *
 * <pre>
 *   am instrument -w -r -e check https -e url https://example.test:8443/ \
 *       -e address 127.0.0.1 -e certificate 1a2b3c4d.0 \
 *       io.github.thisisthepy.dsa.mediaprobe/.MediaProbe
 * </pre>
 *
 * With "address", the check connects to that address instead of looking the URL's host up, and
 * still checks the server's certificate against the URL's host, as the app would after a
 * lookup. A test's server on the device's own loopback address is reached that way under a
 * name of its own: Android 17 gives "localhost" and every loopback address a network security
 * config of their own that trusts no certificate at all, so a server reached under such a name
 * is refused whatever Android trusts.
 *
 * The results are "outcome" ("connected", "untrusted", "misnamed" or "unreachable"), the HTTP
 * "status" when connected, and the exception's "detail" otherwise. They also say what the app
 * sees of Android's certificates, so a refusal can be told apart from a certificate that never
 * reached the app: "certs.property" (the VM's system.certs.enabled), "certs.apex" and
 * "certs.system" (how many files each of Android's two folders holds, and whether the file
 * "certificate" names is among them), "certs.mounts" (the lines of the app's mount table that
 * name a certificate folder) and "certs.namespace" (the app's mount namespace).
 */
final class HttpsCheck {
    private static final int TIMEOUT_MS = 15_000;
    private static final String APEX_FOLDER = "/apex/com.android.conscrypt/cacerts";
    private static final String SYSTEM_FOLDER = "/system/etc/security/cacerts";

    private HttpsCheck() {}

    static void run(String url, String address, String certificate, Bundle results) {
        describeCertificates(certificate, results);
        if (address == null || address.isEmpty()) {
            viaUrlConnection(url, results);
        } else {
            viaAddress(url, address, results);
        }
    }

    /** Opens `url` with HttpsURLConnection, looking its host up. */
    private static void viaUrlConnection(String url, Bundle results) {
        HttpsURLConnection connection = null;
        try {
            connection = (HttpsURLConnection) new URL(url).openConnection();
            connection.setConnectTimeout(TIMEOUT_MS);
            connection.setReadTimeout(TIMEOUT_MS);
            connection.setUseCaches(false);
            int status = connection.getResponseCode();
            try (InputStream body = status < 400 ? connection.getInputStream() : connection.getErrorStream()) {
                if (body != null) {
                    byte[] buffer = new byte[4096];
                    while (body.read(buffer) >= 0) {
                        // Read to the end, so the whole exchange happened.
                    }
                }
            }
            results.putString("outcome", "connected");
            results.putString("status", Integer.toString(status));
        } catch (SSLException e) {
            refused(url, e, results);
        } catch (IOException e) {
            unreachable(url, e, results);
        } finally {
            if (connection != null) {
                connection.disconnect();
            }
        }
    }

    /**
     * Opens `url` at `address`: a plain connection to the address, and TLS over it with the
     * platform's default socket factory under the URL's host, which is the host the network
     * security config and the certificate check see. The host's name is then checked against
     * the server's certificate with the platform's default verifier, and one GET is sent.
     */
    private static void viaAddress(String url, String address, Bundle results) {
        String host;
        int port;
        try {
            URL target = new URL(url);
            host = target.getHost();
            port = target.getPort() < 0 ? target.getDefaultPort() : target.getPort();
        } catch (IOException e) {
            results.putString("outcome", "unreachable");
            results.putString("detail", "the url is not usable: " + e);
            return;
        }
        Socket plain = new Socket();
        try {
            plain.connect(new InetSocketAddress(InetAddress.getByName(address), port), TIMEOUT_MS);
            plain.setSoTimeout(TIMEOUT_MS);
        } catch (IOException e) {
            closeQuietly(plain);
            unreachable(url + " at " + address, e, results);
            return;
        }
        SSLSocket socket = null;
        try {
            socket = (SSLSocket) HttpsURLConnection.getDefaultSSLSocketFactory()
                    .createSocket(plain, host, port, true);
            socket.startHandshake();
            if (!HttpsURLConnection.getDefaultHostnameVerifier().verify(host, socket.getSession())) {
                results.putString("outcome", "misnamed");
                results.putString("detail", "the server's certificate does not name " + host);
                return;
            }
            OutputStream out = socket.getOutputStream();
            out.write(("GET / HTTP/1.0\r\nHost: " + host + "\r\n\r\n").getBytes(StandardCharsets.US_ASCII));
            out.flush();
            BufferedReader in = new BufferedReader(
                    new InputStreamReader(socket.getInputStream(), StandardCharsets.ISO_8859_1));
            String statusLine = in.readLine();
            while (in.readLine() != null) {
                // Read to the end, so the whole exchange happened.
            }
            String[] parts = statusLine == null ? new String[0] : statusLine.split(" ");
            if (parts.length < 2 || !parts[0].startsWith("HTTP/")) {
                results.putString("outcome", "unreachable");
                results.putString("detail", "the server answered no HTTP status: " + statusLine);
                return;
            }
            results.putString("outcome", "connected");
            results.putString("status", parts[1]);
        } catch (SSLException e) {
            refused(url + " at " + address, e, results);
        } catch (IOException e) {
            unreachable(url + " at " + address, e, results);
        } finally {
            closeQuietly(socket != null ? socket : plain);
        }
    }

    private static void refused(String what, SSLException e, Bundle results) {
        Log.i(MediaProbe.TAG, "the certificate check refused " + what, e);
        results.putString("outcome", "untrusted");
        results.putString("detail", String.valueOf(e));
    }

    private static void unreachable(String what, IOException e, Bundle results) {
        Log.i(MediaProbe.TAG, what + " cannot be reached", e);
        results.putString("outcome", "unreachable");
        results.putString("detail", String.valueOf(e));
    }

    private static void closeQuietly(Socket socket) {
        try {
            socket.close();
        } catch (IOException ignored) {
            // Nothing is left to do with it.
        }
    }

    /** What this app's process sees of Android's certificate folders. */
    private static void describeCertificates(String certificate, Bundle results) {
        String property = System.getProperty("system.certs.enabled");
        results.putString("certs.property", property == null ? "unset" : property);
        results.putString("certs.apex", describeFolder(APEX_FOLDER, certificate));
        results.putString("certs.system", describeFolder(SYSTEM_FOLDER, certificate));
        List<String> mounts = new ArrayList<>();
        try (BufferedReader table = new BufferedReader(new FileReader("/proc/self/mountinfo"))) {
            for (String line = table.readLine(); line != null; line = table.readLine()) {
                if (line.contains("cacerts")) {
                    mounts.add(line);
                }
            }
            results.putString("certs.mounts", mounts.isEmpty() ? "none" : String.join(" | ", mounts));
        } catch (IOException e) {
            results.putString("certs.mounts", "unreadable: " + e);
        }
        try {
            results.putString("certs.namespace", Os.readlink("/proc/self/ns/mnt"));
        } catch (ErrnoException e) {
            results.putString("certs.namespace", "unreadable: " + e);
        }
    }

    /** "<n> files", and whether `certificate` is among them as this process sees it. */
    private static String describeFolder(String folder, String certificate) {
        String[] names = new java.io.File(folder).list();
        if (names == null) {
            return "cannot be listed";
        }
        StringBuilder text = new StringBuilder(names.length + " files");
        if (certificate != null && !certificate.isEmpty()) {
            String path = folder + "/" + certificate;
            try {
                Os.stat(path);
                text.append(", ").append(certificate).append(" is there");
                try {
                    Os.close(Os.open(path, OsConstants.O_RDONLY, 0));
                    text.append(" and readable");
                } catch (ErrnoException e) {
                    text.append(" but cannot be opened: ").append(e.getMessage());
                }
            } catch (ErrnoException e) {
                text.append(", ").append(certificate).append(" is not: ").append(e.getMessage());
            }
        }
        return text.toString();
    }
}
