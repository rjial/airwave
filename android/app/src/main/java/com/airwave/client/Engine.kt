package com.airwave.client

import android.annotation.SuppressLint
import android.content.Context
import android.hardware.usb.UsbAccessory
import android.hardware.usb.UsbManager
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.ParcelFileDescriptor
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import java.io.FileInputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.Inet4Address
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.NetworkInterface
import java.net.ServerSocket
import java.net.Socket
import java.net.SocketTimeoutException
import java.util.concurrent.CopyOnWriteArrayList
import kotlin.concurrent.thread
import kotlin.math.abs
import kotlin.math.max

/**
 * Same wire format as the computer app: a series of packets (see Codec.kt),
 * on Wi-Fi and on the USB cable alike.
 * Discovery = UDP "AIRWAVE?" to port 59201, answer "AIRWAVE!<port>|<name>".
 */
const val STREAM_PORT = 59200
const val DISCOVERY_PORT = 59201
private const val PROBE = "AIRWAVE?"
private const val REPLY_PREFIX = "AIRWAVE!"
const val BUFFER_MIN_MS = 20
const val BUFFER_MAX_MS = 1000

/** Accessory reads must ask for 16 KB, or the rest of a USB transfer is lost. */
private const val READ_SIZE = 16384

sealed class PlayerState {
    data object Idle : PlayerState()
    data class Connecting(val from: String) : PlayerState()
    data class Playing(val from: String, val format: String) : PlayerState()
    data class Failed(val message: String) : PlayerState()
}

sealed class SenderState {
    data object Off : SenderState()
    data object Live : SenderState()
    data class Failed(val message: String) : SenderState()
}

data class Found(val name: String, val host: String, val port: Int, val seenAt: Long) {
    val address get() = "$host:$port"
}

private val main = Handler(Looper.getMainLooper())
private fun ui(block: () -> Unit) = main.post(block)

/** Everything the screens show, plus the actions they can take. */
object Airwave {
    var player by mutableStateOf<PlayerState>(PlayerState.Idle)
    var playerLevel by mutableFloatStateOf(0f)
    var waitingMs by mutableIntStateOf(0)
    var volume by mutableFloatStateOf(0.8f)
    var muted by mutableStateOf(false)
    var bufferMs by mutableIntStateOf(100)
    var outputEngine by mutableStateOf(OutputEngine.AudioTrack)
    val servers = mutableStateListOf<Found>()
    var searching by mutableStateOf(false)

    var sender by mutableStateOf<SenderState>(SenderState.Off)
    var senderLevel by mutableFloatStateOf(0f)
    var listeners by mutableIntStateOf(0)
    var stereo by mutableStateOf(false)
    var sendCodec by mutableStateOf(Codec.Pcm)

    var accessoryAttached by mutableStateOf(false)

    @Volatile private var activePlayer: Player? = null
    @Volatile private var activeSender: Sender? = null
    private var lastSource: Pair<String, () -> Source>? = null
    private var loaded = false

    // ------------------------------------------------------------- settings

    fun load(context: Context) {
        if (loaded) return
        loaded = true
        val p = context.getSharedPreferences("airwave", Context.MODE_PRIVATE)
        volume = p.getFloat("volume", 0.8f)
        bufferMs = p.getInt("buffer_ms", 100).coerceIn(BUFFER_MIN_MS, BUFFER_MAX_MS)
        outputEngine = OutputEngine.entries.firstOrNull { it.name == p.getString("output", null) } ?: OutputEngine.AudioTrack
        sendCodec = Codec.entries.firstOrNull { it.name == p.getString("codec", null) } ?: Codec.Pcm
        stereo = p.getBoolean("stereo", false)
    }

    fun save(context: Context) {
        context.getSharedPreferences("airwave", Context.MODE_PRIVATE).edit()
            .putFloat("volume", volume)
            .putInt("buffer_ms", bufferMs)
            .putString("output", outputEngine.name)
            .putString("codec", sendCodec.name)
            .putBoolean("stereo", stereo)
            .apply()
    }

    // ------------------------------------------------------------ listening

    fun listenTo(context: Context, address: String) {
        val trimmed = address.trim()
        if (trimmed.isEmpty()) return
        val (host, port) = splitAddress(trimmed)
        startPlayer(context, "$host:$port") {
            val s = Socket()
            s.connect(InetSocketAddress(host, port), 3000)
            s.tcpNoDelay = true
            s.soTimeout = 5000
            Source(s.getInputStream()) { s.close() }
        }
    }

    fun listenToAccessory(context: Context, accessory: UsbAccessory) {
        val manager = context.getSystemService(Context.USB_SERVICE) as UsbManager
        startPlayer(context, "USB cable") {
            val pfd: ParcelFileDescriptor = manager.openAccessory(accessory)
                ?: throw IOException("Could not open the USB connection")
            ui { accessoryAttached = true }
            Source(FileInputStream(pfd.fileDescriptor), canSkip = false) {
                pfd.close()
                ui { accessoryAttached = false }
            }
        }
    }

    /** Output or buffer changed while playing: start again so it applies now. */
    fun restartIfPlaying(context: Context) {
        val last = lastSource ?: return
        if (isPlaying) startPlayer(context, last.first, last.second)
    }

    private fun startPlayer(context: Context, label: String, open: () -> Source) {
        activePlayer?.stop()
        lastSource = label to open
        val p = Player(label, open) { StreamService.update(context) }
        activePlayer = p
        player = PlayerState.Connecting(label)
        StreamService.update(context)
        p.start()
    }

    fun stopPlayer(context: Context) {
        activePlayer?.stop()
        activePlayer = null
        player = PlayerState.Idle
        playerLevel = 0f
        StreamService.update(context)
    }

    val isPlaying get() = player is PlayerState.Playing || player is PlayerState.Connecting

    // -------------------------------------------------------------- sending

    fun startSending(context: Context) {
        stopSending(context)
        val s = Sender(context.applicationContext, stereo, sendCodec)
        activeSender = s
        sender = SenderState.Live
        StreamService.update(context)
        s.start()
    }

    fun stopSending(context: Context) {
        activeSender?.stop()
        activeSender = null
        if (sender is SenderState.Live) sender = SenderState.Off
        senderLevel = 0f
        listeners = 0
        StreamService.update(context)
    }

    val isSending get() = sender is SenderState.Live

    // ------------------------------------------------------------ discovery

    fun search(context: Context) {
        if (searching) return
        searching = true
        val wifi = context.applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
        thread(name = "airwave-search") {
            val lock = wifi.createMulticastLock("airwave").apply { setReferenceCounted(false) }
            val found = mutableListOf<Found>()
            try {
                lock.acquire()
                DatagramSocket().use { sock ->
                    sock.broadcast = true
                    sock.soTimeout = 150
                    val probe = PROBE.toByteArray()
                    for (target in listOf("255.255.255.255", "127.0.0.1")) {
                        runCatching {
                            sock.send(DatagramPacket(probe, probe.size, InetAddress.getByName(target), DISCOVERY_PORT))
                        }
                    }
                    val end = System.currentTimeMillis() + 1200
                    val buf = ByteArray(512)
                    while (System.currentTimeMillis() < end) {
                        val packet = DatagramPacket(buf, buf.size)
                        try {
                            sock.receive(packet)
                        } catch (_: SocketTimeoutException) {
                            continue
                        }
                        val text = String(packet.data, 0, packet.length)
                        if (!text.startsWith(REPLY_PREFIX)) continue
                        val rest = text.removePrefix(REPLY_PREFIX)
                        val port = rest.substringBefore('|').toIntOrNull() ?: continue
                        val name = rest.substringAfter('|', "Computer")
                        val host = packet.address.hostAddress ?: continue
                        found += Found(name, host, port, System.currentTimeMillis())
                    }
                }
            } catch (_: Exception) {
            } finally {
                runCatching { lock.release() }
            }
            ui {
                val now = System.currentTimeMillis()
                for (f in found) {
                    val i = servers.indexOfFirst { it.name == f.name && it.port == f.port }
                    if (i >= 0) {
                        val keepOld = !servers[i].host.startsWith("127.") && f.host.startsWith("127.")
                        servers[i] = if (keepOld) servers[i].copy(seenAt = now) else f
                    } else {
                        servers += f
                    }
                }
                servers.removeAll { now - it.seenAt > 15_000 }
                searching = false
            }
        }
    }
}

fun splitAddress(text: String): Pair<String, Int> {
    val i = text.lastIndexOf(':')
    if (i > 0 && text.indexOf(':') == i) {
        val port = text.substring(i + 1).toIntOrNull()
        if (port != null) return text.substring(0, i) to port
    }
    return text to STREAM_PORT
}

/** This phone's address on Wi-Fi, for showing on the Send screen. */
fun localAddress(): String? = runCatching {
    NetworkInterface.getNetworkInterfaces().toList()
        .filter { it.isUp && !it.isLoopback }
        .flatMap { it.inetAddresses.toList() }
        .filterIsInstance<Inet4Address>()
        .firstOrNull { it.isSiteLocalAddress }
        ?.hostAddress
}.getOrNull()

/** Where the stream comes from. [canSkip]: old sound may be skipped to cut delay. */
class Source(val input: InputStream, val canSkip: Boolean = true, val close: () -> Unit)

private fun peakOf(samples: ShortArray, count: Int): Float {
    var peak = 0
    var i = 0
    while (i < count) {
        peak = max(peak, abs(samples[i].toInt()))
        i += 4 // every 4th sample is plenty for a meter
    }
    return peak / 32768f
}

// ------------------------------------------------------------------ player

private class Player(
    val label: String,
    val open: () -> Source,
    val onEnd: () -> Unit,
) {
    @Volatile private var running = true
    @Volatile private var source: Source? = null

    fun start() = thread(name = "airwave-player") { run() }

    fun stop() {
        running = false
        runCatching { source?.close?.invoke() }
    }

    private fun run() {
        var out: AudioOut? = null
        try {
            val src = open()
            source = src
            if (!running) return
            val reader = PacketReader()
            val buf = ByteArray(READ_SIZE)
            var rate = 0
            var channels = 0
            var codec: Codec? = null
            val startedAt = System.currentTimeMillis()
            var lastUi = 0L
            while (running) {
                val n = src.input.read(buf, 0, READ_SIZE)
                if (n < 0) throw IOException(if (src.canSkip) "The sender stopped" else "The cable was disconnected")
                if (n > 0) reader.push(buf, n)

                // Too much sound waiting on the network? Skip ahead so the delay stays small.
                if (src.canSkip && rate > 0) {
                    val waiting = runCatching { src.input.available() }.getOrDefault(0)
                    val bytesPerMs = max(1, rate * channels * 2 / 1000 / compressionFactor(codec))
                    if (waiting > bytesPerMs * Airwave.bufferMs * 3 + READ_SIZE) {
                        runCatching { src.input.skip((waiting - bytesPerMs * Airwave.bufferMs).toLong()) }
                        reader.drop() // packets resync on the next "AWP2"
                    }
                }

                while (true) {
                    val p = reader.next() ?: break
                    if (p.rate != rate || p.channels != channels || out == null) {
                        out?.release()
                        rate = p.rate
                        channels = p.channels
                        out = openOutput(Airwave.outputEngine, rate, channels.coerceAtMost(2), Airwave.bufferMs)
                    }
                    if (p.codec != codec || lastUi == 0L) {
                        codec = p.codec
                        val format = "%.1f kHz · %s · %s · %s".format(
                            rate / 1000f,
                            if (channels == 1) "mono" else "stereo",
                            p.codec.describe,
                            out.name,
                        )
                        ui { if (running) Airwave.player = PlayerState.Playing(label, format) }
                        onEnd()
                    }
                    val gain = if (Airwave.muted) 0f else Airwave.volume * Airwave.volume
                    out.setVolume(gain)
                    val samples = if (channels > 2) downmix(p.samples, channels) else p.samples
                    out.write(samples, samples.size)
                    val now = System.currentTimeMillis()
                    if (now - lastUi > 60) {
                        lastUi = now
                        val level = peakOf(samples, samples.size) * gain
                        ui { Airwave.playerLevel = level }
                    }
                }
                if (rate == 0 && System.currentTimeMillis() - startedAt > 5000) {
                    throw IOException("That is not an Airwave sender (or it is an older version)")
                }
            }
        } catch (e: Exception) {
            if (running) {
                val msg = e.message ?: "The connection was lost"
                ui { Airwave.player = PlayerState.Failed(msg) }
            }
        } finally {
            running = false
            out?.release()
            runCatching { source?.close?.invoke() }
            ui { Airwave.playerLevel = 0f }
            onEnd()
        }
    }

    private fun compressionFactor(codec: Codec?) = when (codec) {
        Codec.MuLaw -> 2
        Codec.Adpcm -> 4
        else -> 1
    }

    /** Keeps the first two channels of a multi-channel stream. */
    private fun downmix(s: ShortArray, ch: Int): ShortArray {
        val frames = s.size / ch
        return ShortArray(frames * 2) { i -> s[(i / 2) * ch + (i % 2)] }
    }
}

// ------------------------------------------------------------------ sender

/** Sends this phone's microphone to computers (over Wi-Fi or `adb forward`). */
private class Sender(val context: Context, val stereo: Boolean, val codec: Codec) {
    @Volatile private var running = true
    private val clients = CopyOnWriteArrayList<Pair<Socket, OutputStream>>()
    private var server: ServerSocket? = null
    private var responder: DatagramSocket? = null

    fun start() {
        thread(name = "airwave-send") { capture() }
        thread(name = "airwave-accept") { accept() }
        thread(name = "airwave-answer") { answer() }
    }

    fun stop() {
        running = false
        runCatching { server?.close() }
        runCatching { responder?.close() }
        clients.forEach { runCatching { it.first.close() } }
        clients.clear()
    }

    private fun fail(msg: String) {
        if (!running) return
        ui { Airwave.sender = SenderState.Failed(msg) }
        stop()
    }

    private val rate = 48000
    private val channels get() = if (stereo) 2 else 1

    private fun accept() {
        try {
            val s = ServerSocket()
            s.reuseAddress = true
            s.bind(InetSocketAddress(STREAM_PORT))
            server = s
            while (running) {
                val c = s.accept()
                c.tcpNoDelay = true
                c.soTimeout = 1000
                clients += c to c.getOutputStream()
                ui { Airwave.listeners = clients.size }
            }
        } catch (e: Exception) {
            fail("Could not open port $STREAM_PORT: ${e.message}")
        }
    }

    private fun answer() {
        val wifi = context.getSystemService(Context.WIFI_SERVICE) as WifiManager
        val lock = wifi.createMulticastLock("airwave-answer").apply { setReferenceCounted(false) }
        try {
            lock.acquire()
            val sock = DatagramSocket(null)
            sock.reuseAddress = true
            sock.bind(InetSocketAddress(DISCOVERY_PORT))
            responder = sock
            val name = "${Build.MANUFACTURER} ${Build.MODEL}".trim()
            val reply = "$REPLY_PREFIX$STREAM_PORT|$name".toByteArray()
            val buf = ByteArray(64)
            while (running) {
                val p = DatagramPacket(buf, buf.size)
                sock.receive(p)
                if (String(p.data, 0, p.length) == PROBE) {
                    sock.send(DatagramPacket(reply, reply.size, p.address, p.port))
                }
            }
        } catch (_: Exception) {
            // Discovery is optional; typing the address still works.
        } finally {
            runCatching { lock.release() }
        }
    }

    @SuppressLint("MissingPermission")
    private fun capture() {
        val mask = if (stereo) AudioFormat.CHANNEL_IN_STEREO else AudioFormat.CHANNEL_IN_MONO
        val min = AudioRecord.getMinBufferSize(rate, mask, AudioFormat.ENCODING_PCM_16BIT)
        if (min <= 0) return fail("This phone can't record at $rate Hz")
        val rec = try {
            AudioRecord(MediaRecorder.AudioSource.MIC, rate, mask, AudioFormat.ENCODING_PCM_16BIT, max(min, rate * channels * 2 / 10))
        } catch (e: Exception) {
            return fail("Microphone not available: ${e.message}")
        }
        if (rec.state != AudioRecord.STATE_INITIALIZED) {
            rec.release()
            return fail("Microphone not available (permission?)")
        }
        val chunk = ShortArray(rate * channels / 100) // 10 ms
        val encoder = PacketEncoder()
        var lastUi = 0L
        try {
            rec.startRecording()
            while (running) {
                val n = rec.read(chunk, 0, chunk.size)
                if (n <= 0) continue
                val packets = encoder.packets(codec, rate, channels, chunk, n)
                for (c in clients) {
                    try {
                        for (p in packets) c.second.write(p)
                    } catch (_: IOException) {
                        runCatching { c.first.close() }
                        clients.remove(c)
                    }
                }
                val now = System.currentTimeMillis()
                if (now - lastUi > 60) {
                    lastUi = now
                    val level = peakOf(chunk, n)
                    val count = clients.size
                    ui {
                        Airwave.senderLevel = level
                        Airwave.listeners = count
                    }
                }
            }
        } catch (e: Exception) {
            fail("Recording stopped: ${e.message}")
        } finally {
            runCatching { rec.stop() }
            rec.release()
        }
    }
}
