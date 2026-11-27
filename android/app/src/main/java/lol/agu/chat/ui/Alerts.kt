package lol.agu.chat.ui

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.os.VibrationEffect
import android.os.Vibrator
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import lol.agu.chat.Core

/**
 * What a highlight rings with: the built-in ping (two sine blips, synthesized by the Rust side like the desktop's)
 * and, in place of the taskbar flash, a short vibration. The Rust side decides when (one alert per message however
 * many tabs show it, one sound per burst); this only plays.
 */
class Alerts(context: Context) {
    private val app = context.applicationContext
    private var rate = 44100
    private var ping: ShortArray? = null

    /** Fetch the ping's samples once. */
    suspend fun prepare() {
        val o = runCatching { Core.call("alerts.ping") as JsonObject }.getOrNull() ?: return
        rate = o["rate"]?.jsonPrimitive?.contentOrNull?.toIntOrNull() ?: 44100
        val pcm = o["pcm"] as? JsonArray ?: return
        ping = ShortArray(pcm.size) { pcm[it].jsonPrimitive.contentOrNull?.toIntOrNull()?.toShort() ?: 0 }
    }

    /** An `alert` event: `{sound: {file, volume} | null, vibrate}`. A custom sound file plays as the ping for now. */
    fun fire(data: JsonElement) {
        val o = data as? JsonObject ?: return
        val sound = o["sound"] as? JsonObject
        if (sound != null) playPing((sound["volume"]?.jsonPrimitive?.contentOrNull?.toIntOrNull() ?: 70).coerceIn(0, 100))
        if (o["vibrate"]?.jsonPrimitive?.contentOrNull == "true") vibrate()
    }

    private fun playPing(volume: Int) {
        val samples = ping ?: return
        runCatching {
            val track = AudioTrack.Builder()
                .setAudioAttributes(AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_NOTIFICATION).setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION).build())
                .setAudioFormat(AudioFormat.Builder().setEncoding(AudioFormat.ENCODING_PCM_16BIT).setSampleRate(rate).setChannelMask(AudioFormat.CHANNEL_OUT_MONO).build())
                .setBufferSizeInBytes(samples.size * 2)
                .setTransferMode(AudioTrack.MODE_STATIC)
                .build()
            track.write(samples, 0, samples.size)
            track.setVolume(volume / 100f)
            track.setNotificationMarkerPosition(samples.size)
            track.setPlaybackPositionUpdateListener(object : AudioTrack.OnPlaybackPositionUpdateListener {
                override fun onMarkerReached(t: AudioTrack) { t.release() }
                override fun onPeriodicNotification(t: AudioTrack) {}
            })
            track.play()
        }
    }

    @Suppress("DEPRECATION")
    private fun vibrate() {
        runCatching {
            val v = app.getSystemService(Context.VIBRATOR_SERVICE) as? Vibrator ?: return
            v.vibrate(VibrationEffect.createOneShot(250, VibrationEffect.DEFAULT_AMPLITUDE))
        }
    }
}
