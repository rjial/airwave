package com.airwave.client

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import kotlin.math.max

/** Ways the phone can play sound, chosen on the Listen screen. */
enum class OutputEngine(val label: String, val hint: String) {
    AudioTrack("AudioTrack", "Android's standard output; works everywhere"),
    LowLatency("AudioTrack · low latency", "Asks Android for its fastest path"),
    PowerSaving("AudioTrack · power saving", "Bigger chunks, easier on the battery"),
    OpenSl("OpenSL ES", "Native output, often the lowest delay"),
    Oboe("Oboe", "Native; uses AAudio for the lowest delay, falls back below API 27"),
}

interface AudioOut {
    val name: String
    fun write(samples: ShortArray, count: Int)
    fun setVolume(gain: Float)
    fun release()
}

/** OpenSL ES is native code; it is only in the app when built with the Android NDK. */
object NativeAudio {
    val available: Boolean = runCatching { System.loadLibrary("airwave_opensl"); true }.getOrDefault(false)

    external fun create(rate: Int, channels: Int, bufferFrames: Int): Long
    external fun write(handle: Long, data: ShortArray, count: Int): Int
    external fun setGain(handle: Long, gain: Float)
    external fun destroy(handle: Long)
}

/** Oboe is native code; it is only in the app when built with the Android NDK. */
object NativeOboe {
    val available: Boolean = runCatching { System.loadLibrary("airwave_oboe"); true }.getOrDefault(false)

    external fun create(rate: Int, channels: Int, bufferFrames: Int): Long
    external fun write(handle: Long, data: ShortArray, count: Int): Int
    external fun setGain(handle: Long, gain: Float)
    external fun destroy(handle: Long)
}

private class OpenSlOut(rate: Int, channels: Int, bufferMs: Int) : AudioOut {
    override val name = "OpenSL ES"
    private var handle = NativeAudio.create(rate, channels, max(rate / 50, rate * bufferMs / 1000))

    init {
        if (handle == 0L) throw IllegalStateException("OpenSL ES could not start")
    }

    override fun write(samples: ShortArray, count: Int) {
        if (handle != 0L) NativeAudio.write(handle, samples, count)
    }

    override fun setVolume(gain: Float) {
        if (handle != 0L) NativeAudio.setGain(handle, gain)
    }

    override fun release() {
        val h = handle
        handle = 0L
        if (h != 0L) NativeAudio.destroy(h)
    }
}

private class OboeOut(rate: Int, channels: Int, bufferMs: Int) : AudioOut {
    override val name = "Oboe"
    private var handle = NativeOboe.create(rate, channels, max(rate / 50, rate * bufferMs / 1000))

    init {
        if (handle == 0L) throw IllegalStateException("Oboe could not start")
    }

    override fun write(samples: ShortArray, count: Int) {
        if (handle != 0L) NativeOboe.write(handle, samples, count)
    }

    override fun setVolume(gain: Float) {
        if (handle != 0L) NativeOboe.setGain(handle, gain)
    }

    override fun release() {
        val h = handle
        handle = 0L
        if (h != 0L) NativeOboe.destroy(h)
    }
}

private class TrackOut(rate: Int, channels: Int, bufferMs: Int, engine: OutputEngine) : AudioOut {
    override val name = engine.label
    private val track: AudioTrack

    init {
        val mask = if (channels == 1) AudioFormat.CHANNEL_OUT_MONO else AudioFormat.CHANNEL_OUT_STEREO
        val bytesPerMs = rate * channels * 2 / 1000
        val min = AudioTrack.getMinBufferSize(rate, mask, AudioFormat.ENCODING_PCM_16BIT)
        val wanted = when (engine) {
            OutputEngine.PowerSaving -> max(min * 4, bytesPerMs * bufferMs * 2)
            OutputEngine.LowLatency -> max(min, bytesPerMs * bufferMs / 2)
            else -> max(min, bytesPerMs * bufferMs)
        }
        val mode = when (engine) {
            OutputEngine.LowLatency -> AudioTrack.PERFORMANCE_MODE_LOW_LATENCY
            OutputEngine.PowerSaving -> AudioTrack.PERFORMANCE_MODE_POWER_SAVING
            else -> AudioTrack.PERFORMANCE_MODE_NONE
        }
        track = AudioTrack.Builder()
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_MEDIA)
                    .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                    .build()
            )
            .setAudioFormat(
                AudioFormat.Builder()
                    .setSampleRate(rate)
                    .setChannelMask(mask)
                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                    .build()
            )
            .setBufferSizeInBytes(wanted)
            .setTransferMode(AudioTrack.MODE_STREAM)
            .setPerformanceMode(mode)
            .build()
        track.play()
    }

    override fun write(samples: ShortArray, count: Int) {
        track.write(samples, 0, count)
    }

    override fun setVolume(gain: Float) {
        track.setVolume(gain)
    }

    override fun release() {
        runCatching { track.stop() }
        runCatching { track.release() }
    }
}

/** Opens the chosen output; falls back to AudioTrack if OpenSL ES isn't there. */
fun openOutput(engine: OutputEngine, rate: Int, channels: Int, bufferMs: Int): AudioOut {
    if (engine == OutputEngine.OpenSl) {
        if (NativeAudio.available) {
            runCatching { return OpenSlOut(rate, channels, bufferMs) }
        }
        return TrackOut(rate, channels, bufferMs, OutputEngine.LowLatency)
    }
    if (engine == OutputEngine.Oboe) {
        if (NativeOboe.available) {
            runCatching { return OboeOut(rate, channels, bufferMs) }
        }
        return TrackOut(rate, channels, bufferMs, OutputEngine.LowLatency)
    }
    return TrackOut(rate, channels, bufferMs, engine)
}
