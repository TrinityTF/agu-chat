package lol.agu.chat

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive

/**
 * The phone's equivalent of the desktop's taskbar flash: a normal notification for a mention or highlight that
 * arrives while the app is in the background, and the quiet ongoing one of the background service. The sound and the
 * vibration are the alert's own (`Alerts`), so these notifications make none.
 */
object Notifier {
    const val CHANNEL_MENTIONS = "mentions"
    const val CHANNEL_CONNECTION = "connection"
    const val EXTRA_VK = "jumpVk"
    const val EXTRA_SK = "jumpSk"
    const val EXTRA_KEY = "jumpKey"
    private const val MENTION_ID = 2

    fun createChannels(c: Context) {
        val nm = c.getSystemService(NotificationManager::class.java) ?: return
        nm.createNotificationChannel(
            NotificationChannel(CHANNEL_MENTIONS, "Mentions", NotificationManager.IMPORTANCE_DEFAULT).apply {
                description = "Messages that mention you or match a highlight while the app is in the background"
                setSound(null, null)
                enableVibration(false)
            },
        )
        nm.createNotificationChannel(
            NotificationChannel(CHANNEL_CONNECTION, "Connection", NotificationManager.IMPORTANCE_LOW).apply {
                description = "Shown while Agu Chat keeps your chats connected in the background"
                setShowBadge(false)
            },
        )
    }

    /** Whether the phone lets the app post notifications (Android 13 and up asks first). */
    fun allowed(c: Context): Boolean =
        Build.VERSION.SDK_INT < 33 || ContextCompat.checkSelfPermission(c, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED

    private fun open(c: Context, code: Int, vararg extras: Pair<String, String>): PendingIntent {
        val i = Intent(c, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        for ((k, v) in extras) i.putExtra(k, v)
        return PendingIntent.getActivity(c, code, i, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
    }

    /** The ongoing notification of the background service. */
    fun connection(c: Context, chats: Int): Notification =
        NotificationCompat.Builder(c, CHANNEL_CONNECTION)
            .setSmallIcon(R.drawable.ic_stat_chat)
            .setContentTitle("Agu Chat is connected to $chats ${if (chats == 1) "chat" else "chats"}")
            .setContentIntent(open(c, 1))
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setSilent(true)
            .setCategory(NotificationCompat.CATEGORY_SERVICE)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .build()

    /** A `notify` event of the core: `{vk, key, sk, channel, user, text, whisper}`. Tapping it opens the app on that message. */
    fun mention(c: Context, data: JsonElement) {
        val o = data as? JsonObject ?: return
        if (!allowed(c)) return
        fun s(k: String) = o[k]?.jsonPrimitive?.contentOrNull.orEmpty()
        val whisper = s("whisper") == "true"
        val user = s("user").ifEmpty { "Someone" }
        val title = if (whisper) "Whisper from $user" else if (s("channel").isEmpty()) user else "$user in ${s("channel")}"
        val key = s("key")
        val n = NotificationCompat.Builder(c, CHANNEL_MENTIONS)
            .setSmallIcon(R.drawable.ic_stat_chat)
            .setContentTitle(title)
            .setContentText(s("text").take(300))
            .setStyle(NotificationCompat.BigTextStyle().bigText(s("text").take(300)))
            .setContentIntent(open(c, 2 + (key.hashCode() and 0xffff), EXTRA_VK to s("vk"), EXTRA_SK to s("sk"), EXTRA_KEY to key))
            .setAutoCancel(true)
            .setSilent(true)
            .setCategory(NotificationCompat.CATEGORY_MESSAGE)
            .setGroup(CHANNEL_MENTIONS)
            .build()
        runCatching { c.getSystemService(NotificationManager::class.java)?.notify(key, MENTION_ID, n) }
    }

    /** The app is on screen again: what it was telling you is in front of you. */
    fun clearMentions(c: Context) {
        runCatching {
            val nm = c.getSystemService(NotificationManager::class.java) ?: return
            for (a in nm.activeNotifications) if (a.id == MENTION_ID) nm.cancel(a.tag, a.id)
        }
    }
}
