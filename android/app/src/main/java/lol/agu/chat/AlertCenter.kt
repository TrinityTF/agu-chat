package lol.agu.chat

import android.app.Application
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import lol.agu.chat.ui.Alerts

/**
 * Rings highlights and posts the background notifications for the whole life of the process, not of one screen: the
 * background service keeps the process (and the chats) alive after the screen is gone, and the alerts must still fire.
 */
object AlertCenter {
    lateinit var alerts: Alerts
        private set

    fun start(app: Application) {
        alerts = Alerts(app)
        Notifier.createChannels(app)
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        scope.launch { alerts.prepare() }
        scope.launch {
            Core.events.collect { e ->
                when (e.kind) {
                    "alert" -> alerts.fire(e.data)
                    "notify" -> Notifier.mention(app, e.data)
                    // the bell's numbers changed: keep the list on disk, also while no screen is open
                    "mentions" -> launch { MentionStore.refresh(app) }
                }
            }
        }
    }
}
