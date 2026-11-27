package lol.agu.chat

import android.content.Context

/** The few settings that exist only on the phone (the core's settings are shared with the desktop). */
object Prefs {
    private fun p(c: Context) = c.applicationContext.getSharedPreferences("agu_prefs", Context.MODE_PRIVATE)

    /** Keep the chats connected while the app is in the background (a foreground service with a small notification). */
    fun stayConnected(c: Context): Boolean = runCatching { p(c).getBoolean("stay_connected", true) }.getOrDefault(true)
    fun setStayConnected(c: Context, on: Boolean) { runCatching { p(c).edit().putBoolean("stay_connected", on).apply() } }

    /** The notification permission was asked for once: the phone only asks twice, so the app does not keep trying. */
    fun askedNotifications(c: Context): Boolean = runCatching { p(c).getBoolean("asked_notifications", false) }.getOrDefault(false)
    fun setAskedNotifications(c: Context) { runCatching { p(c).edit().putBoolean("asked_notifications", true).apply() } }
}
