package com.airwave.client

import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Same packets as the computer app (see codec.rs there):
 *  "AWP2" | u32 rate | u16 channels | u8 codec | u8 0 | u32 frames | u32 len | payload
 * Little-endian, 20-byte header, whole packet at most 16 KB.
 * Codecs: 0 = 16-bit PCM, 1 = µ-law (half), 2 = IMA ADPCM (a quarter).
 */
enum class Codec(val id: Int, val label: String, val describe: String) {
    Pcm(0, "Off", "no compression"),
    MuLaw(1, "Light", "light compression"),
    Adpcm(2, "Strong", "strong compression");

    fun payloadLen(frames: Int, ch: Int): Int = when (this) {
        Pcm -> frames * ch * 2
        MuLaw -> frames * ch
        Adpcm -> 4 * ch + (frames * ch + 1) / 2
    }

    fun maxFrames(ch: Int): Int {
        val room = MAX_PACKET - HEADER
        return when (this) {
            Pcm -> room / (ch * 2)
            MuLaw -> room / ch
            Adpcm -> (room - 4 * ch) * 2 / ch
        }
    }

    companion object {
        fun fromId(id: Int) = entries.firstOrNull { it.id == id }
    }
}

private val PACKET_MAGIC = byteArrayOf('A'.code.toByte(), 'W'.code.toByte(), 'P'.code.toByte(), '2'.code.toByte())
const val HEADER = 20
const val MAX_PACKET = 16384

// ------------------------------------------------------------------ µ-law

private const val ULAW_BIAS = 0x84
private const val ULAW_CLIP = 32635

fun ulawEncode(sample: Short): Byte {
    var x = sample.toInt()
    val sign = if (x < 0) { x = -x; 0x80 } else 0
    if (x > ULAW_CLIP) x = ULAW_CLIP
    x += ULAW_BIAS
    var exp = 7
    var mask = 0x4000
    while (exp > 0 && (x and mask) == 0) { exp--; mask = mask shr 1 }
    val mant = (x shr (exp + 3)) and 0x0F
    return (sign or (exp shl 4) or mant).inv().toByte()
}

private val ULAW_TABLE = ShortArray(256) { i ->
    val u = i.inv() and 0xFF
    val exp = (u shr 4) and 7
    val mant = u and 0x0F
    val x = (((mant shl 3) + ULAW_BIAS) shl exp) - ULAW_BIAS
    (if (u and 0x80 != 0) -x else x).toShort()
}

// ------------------------------------------------------------------ ADPCM

private val INDEX_TABLE = intArrayOf(-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8)
private val STEP_TABLE = intArrayOf(
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408,
    449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066,
    2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630,
    9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
)

private class AdpcmChannel(var pred: Int = 0, var index: Int = 0) {
    fun step(nib: Int): Short {
        val step = STEP_TABLE[index]
        var delta = step shr 3
        if (nib and 4 != 0) delta += step
        if (nib and 2 != 0) delta += step shr 1
        if (nib and 1 != 0) delta += step shr 2
        pred = (if (nib and 8 != 0) pred - delta else pred + delta).coerceIn(-32768, 32767)
        index = (index + INDEX_TABLE[nib]).coerceIn(0, 88)
        return pred.toShort()
    }

    fun encode(sample: Int): Int {
        val step = STEP_TABLE[index]
        var diff = sample - pred
        var nib = 0
        if (diff < 0) { nib = 8; diff = -diff }
        if (diff >= step) { nib = nib or 4; diff -= step }
        if (diff >= step shr 1) { nib = nib or 2; diff -= step shr 1 }
        if (diff >= step shr 2) nib = nib or 1
        step(nib)
        return nib
    }
}

/** Turns 16-bit samples into packets (for sending the phone's microphone). */
class PacketEncoder {
    private var adpcm = arrayOf<AdpcmChannel>()

    fun packets(codec: Codec, rate: Int, ch: Int, samples: ShortArray, count: Int): List<ByteArray> {
        if (adpcm.size != ch) adpcm = Array(ch) { AdpcmChannel() }
        val framesTotal = count / ch
        val maxFrames = codec.maxFrames(ch)
        val out = ArrayList<ByteArray>()
        var f0 = 0
        while (f0 < framesTotal) {
            val frames = minOf(framesTotal - f0, maxFrames)
            val len = codec.payloadLen(frames, ch)
            val bb = ByteBuffer.allocate(HEADER + len).order(ByteOrder.LITTLE_ENDIAN)
            bb.put(PACKET_MAGIC).putInt(rate).putShort(ch.toShort())
                .put(codec.id.toByte()).put(0).putInt(frames).putInt(len)
            val from = f0 * ch
            val to = (f0 + frames) * ch
            when (codec) {
                Codec.Pcm -> for (i in from until to) bb.putShort(samples[i])
                Codec.MuLaw -> for (i in from until to) bb.put(ulawEncode(samples[i]))
                Codec.Adpcm -> {
                    for (c in adpcm) {
                        bb.putShort(c.pred.toShort()).put(c.index.toByte()).put(0)
                    }
                    var byte = 0
                    for (i in from until to) {
                        val k = i - from
                        val nib = adpcm[k % ch].encode(samples[i].toInt())
                        if (k % 2 == 0) byte = nib else bb.put((byte or (nib shl 4)).toByte())
                    }
                    if ((to - from) % 2 == 1) bb.put(byte.toByte())
                }
            }
            out += bb.array()
            f0 += frames
        }
        return out
    }
}

class Packet(val rate: Int, val channels: Int, val codec: Codec, val samples: ShortArray)

/** Pulls packets out of a byte stream, searching again after damage. */
class PacketReader {
    private var buf = ByteArray(MAX_PACKET * 4)
    private var start = 0
    private var end = 0

    fun push(src: ByteArray, n: Int) {
        if (end + n > buf.size) {
            System.arraycopy(buf, start, buf, 0, end - start)
            end -= start
            start = 0
            if (end + n > buf.size) buf = buf.copyOf(maxOf(buf.size * 2, end + n))
        }
        System.arraycopy(src, 0, buf, end, n)
        end += n
    }

    /** Bytes waiting that are not yet played. */
    val waiting get() = end - start

    fun drop() {
        start = 0
        end = 0
    }

    private fun magicAt(i: Int) = buf[i] == PACKET_MAGIC[0] && buf[i + 1] == PACKET_MAGIC[1] &&
        buf[i + 2] == PACKET_MAGIC[2] && buf[i + 3] == PACKET_MAGIC[3]

    fun next(): Packet? {
        while (end - start >= HEADER) {
            if (!magicAt(start)) {
                start++
                continue
            }
            val bb = ByteBuffer.wrap(buf).order(ByteOrder.LITTLE_ENDIAN)
            val rate = bb.getInt(start + 4)
            val ch = bb.getShort(start + 8).toInt()
            val codec = Codec.fromId(buf[start + 10].toInt() and 0xFF)
            val frames = bb.getInt(start + 12)
            val len = bb.getInt(start + 16)
            if (rate !in 8000..192000 || ch !in 1..8 || codec == null || len < 0 || len > MAX_PACKET * 4 ||
                frames < 0 || len != codec.payloadLen(frames, ch)
            ) {
                start++
                continue
            }
            if (end - start < HEADER + len) return null
            val p = start + HEADER
            start = p + len
            val n = frames * ch
            val samples = ShortArray(n)
            when (codec) {
                Codec.Pcm -> for (i in 0 until n) samples[i] = bb.getShort(p + i * 2)
                Codec.MuLaw -> for (i in 0 until n) samples[i] = ULAW_TABLE[buf[p + i].toInt() and 0xFF]
                Codec.Adpcm -> {
                    val st = Array(ch) { c ->
                        AdpcmChannel(bb.getShort(p + c * 4).toInt(), (buf[p + c * 4 + 2].toInt() and 0xFF).coerceAtMost(88))
                    }
                    val body = p + 4 * ch
                    for (i in 0 until n) {
                        val b = buf[body + i / 2].toInt() and 0xFF
                        val nib = if (i % 2 == 0) b and 0x0F else b shr 4
                        samples[i] = st[i % ch].step(nib)
                    }
                }
            }
            if (start == end) { start = 0; end = 0 }
            return Packet(rate, ch, codec, samples)
        }
        return null
    }
}
