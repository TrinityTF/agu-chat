package lol.agu.chat.ui

import androidx.compose.runtime.Immutable
import androidx.compose.runtime.mutableStateListOf
import kotlinx.serialization.Serializable

/** One tap-to-insert suggestion of the strip; [replace] takes the place of the text from the strip's start to its end. */
@Serializable
@Immutable
data class Suggestion(val name: String, val img: String = "", val hint: String = "", val dim: Boolean = false, val replace: String)

/** What the strip above the keyboard offers: [kind] is emote | command | mention | word, [start]..[end] the text it replaces (UTF-16). */
@Serializable
@Immutable
data class Suggestions(val kind: String, val start: Int, val end: Int, val items: List<Suggestion>)

@Serializable
@Immutable
data class PickerItem(val key: String, val name: String, val img: String = "", val tip: String = "", val insert: String)

@Serializable
@Immutable
data class PickerSection(val id: String, val label: String, val provider: String = "", val items: List<PickerItem>)

@Serializable
@Immutable
data class PickerTab(val id: String, val label: String)

@Serializable
@Immutable
data class PickerData(val tabs: List<PickerTab> = emptyList(), val sections: List<PickerSection> = emptyList())

/** How a chat's modes apply to your account: [level] blocked (the box locks), restricted (it may refuse), info (a hint). */
@Serializable
@Immutable
data class Gate(val level: String, val text: String) {
    val blocked get() = level == "blocked"
}

/** `/clear` and `/raid` ask first: [kind] clear | raid. */
@Immutable
data class Confirm(val kind: String, val sk: String, val login: String?, val text: String)

/** The answer to Send: [clear] empties the box; [toast] is the message to show (a warning when [warn]); [confirm] asks first. */
@Immutable
data class SendResult(val clear: Boolean, val toast: String? = null, val warn: Boolean = false, val confirm: Confirm? = null)

/** The messages you sent in a tab, newest last; what the desktop's Up key brings back. */
class SentHistory {
    private val items = mutableStateListOf<String>()
    val list: List<String> get() = items.asReversed()

    fun add(text: String) {
        val t = text.trim()
        if (t.isEmpty()) return
        if (items.lastOrNull() != t) items.add(t)
        if (items.size > MAX) items.subList(0, items.size - MAX).clear()
    }

    companion object {
        const val MAX = 100
    }
}

/** The most characters a message has (the box cuts at it, like the desktop's). */
const val MAX_CHARS = 500

/** [s] cut to [MAX_CHARS] characters (code points, so an emoji is not split). */
fun clampMessage(s: String): String =
    if (s.codePointCount(0, s.length) <= MAX_CHARS) s else s.substring(0, s.offsetByCodePoints(0, MAX_CHARS))
