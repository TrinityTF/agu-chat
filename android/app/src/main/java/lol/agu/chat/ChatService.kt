package lol.agu.chat

import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat

/**
 * A foreground service that exists while at least one chat is open: the phone does not pause the app in the
 * background, so the chats stay connected, highlight alerts still fire and a browser login (Kick) can come back. All
 * the work is in the Rust core; this only keeps the process alive and says so in one low-key notification.
 */
class ChatService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val chats = intent?.getIntExtra(EXTRA_CHATS, 0) ?: 0
        if (chats <= 0) {
            // restarted by the phone after the process died: there are no chats open any more, nothing to keep
            stopSelf()
            return START_NOT_STICKY
        }
        Notifier.createChannels(this)
        val type = if (Build.VERSION.SDK_INT >= 34) ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE else 0
        runCatching { ServiceCompat.startForeground(this, NOTIFICATION_ID, Notifier.connection(this, chats), type) }
            .onFailure { stopSelf() }
        return START_NOT_STICKY
    }

    companion object {
        private const val EXTRA_CHATS = "chats"
        private const val NOTIFICATION_ID = 1

        /** Runs the service for [chats] open chats, or stops it when there are none or [enabled] is off. */
        fun sync(context: Context, chats: Int, enabled: Boolean) {
            val app = context.applicationContext
            runCatching {
                if (enabled && chats > 0) ContextCompat.startForegroundService(app, Intent(app, ChatService::class.java).putExtra(EXTRA_CHATS, chats))
                else app.stopService(Intent(app, ChatService::class.java))
            }
        }
    }
}
