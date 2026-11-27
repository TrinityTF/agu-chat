package lol.agu.chat.ui

import androidx.compose.runtime.Immutable

/** The send target meaning "every chat of the tab I can write in". */
const val ALL = "*"

/** One chat of a tab as the send picker lists it. [reason] says why it can't be written in. */
@Immutable
data class SendOption(val sk: String, val platform: String, val channel: String, val canSend: Boolean, val reason: String?)

/**
 * The chats of a tab and whether you can write in each: the connection's own word (a chat that is not heard from yet
 * counts as writable), and when not, why, worded like the desktop: "not logged in", "offline" (X, YouTube and Rumble
 * only take messages while the stream is live) or "read-only".
 */
fun sendOptions(tab: Tab, conns: Map<String, Conn>): List<SendOption> = tab.channels.map { ch ->
    val sk = ch.sk()
    val conn = conns[sk]
    val can = conn?.canSend != false
    val reason = when {
        can -> null
        conn?.loggedOut == true -> "not logged in"
        conn?.live == false && ch.platform != "twitch" && ch.platform != "kick" -> "offline"
        else -> "read-only"
    }
    SendOption(sk, ch.platform, ch.channel, can, reason)
}

/**
 * Where the message goes: a reply goes to the chat it answers (its id only means something there), else the chosen
 * chat or all of them while more than one can be written in, else the first chat that can. "" when none can.
 */
fun chosenTarget(wanted: String?, sendable: List<String>, replySk: String?): String {
    if (replySk != null && replySk in sendable) return replySk
    if (wanted == ALL && sendable.size > 1) return ALL
    return sendable.firstOrNull { it == wanted } ?: sendable.firstOrNull().orEmpty()
}
