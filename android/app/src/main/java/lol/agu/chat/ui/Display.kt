package lol.agu.chat.ui

import android.text.format.DateFormat
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.staticCompositionLocalOf
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.floatOrNull
import kotlinx.serialization.json.jsonPrimitive
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale
import kotlin.math.roundToInt

/**
 * How chat lines are drawn: the display settings the desktop has, read from the core's settings object. The desktop
 * sizes things in px at a 13 px font; the phone's body text is [bodySp] (13 shows as 15 sp), and emotes follow the
 * desktop's multiples of it.
 */
@Immutable
data class Display(
    val fontSize: Float = 13f,
    val timestamps: Boolean = false,
    val timestampSeconds: Boolean = false,
    val alternateRows: Boolean = true,
    val emoteSize: String = "medium",
    val showBadges: Boolean = true,
    val animatedEmotes: Boolean = true,
    /** strike (greyed, reason after it) | hide | show */
    val deletedMessages: String = "strike",
    val linkPreviews: Boolean = true,
    val showPlatformMarks: Boolean = true,
) {
    val bodySp: Float get() = fontSize * 15f / 13f

    /** The emote box's height: the desktop's 1.45 / 1.9 / 2.5 times the font. */
    val emoteDp: Float get() = (bodySp * when (emoteSize) { "small" -> 1.45f; "large" -> 2.5f; else -> 1.9f }).roundToInt().toFloat()

    val badgeDp: Float get() = (bodySp * 1.1f).roundToInt().toFloat()

    companion object {
        fun parse(data: JsonElement, base: Display = Display()): Display {
            val o = data as? JsonObject ?: return base
            fun bool(k: String, d: Boolean) = o[k]?.jsonPrimitive?.booleanOrNull ?: d
            fun str(k: String, d: String) = o[k]?.jsonPrimitive?.contentOrNull ?: d
            return Display(
                fontSize = (o["fontSize"]?.jsonPrimitive?.floatOrNull ?: base.fontSize).coerceIn(8f, 40f),
                timestamps = bool("timestamps", base.timestamps),
                timestampSeconds = bool("timestampSeconds", base.timestampSeconds),
                alternateRows = bool("alternateRows", base.alternateRows),
                emoteSize = str("emoteSize", base.emoteSize),
                showBadges = bool("showBadges", base.showBadges),
                animatedEmotes = bool("animatedEmotes", base.animatedEmotes),
                deletedMessages = str("deletedMessages", base.deletedMessages),
                linkPreviews = bool("linkPreviews", base.linkPreviews),
                showPlatformMarks = bool("showPlatformMarks", base.showPlatformMarks),
            )
        }
    }
}

val LocalDisplay = staticCompositionLocalOf { Display() }

/** The time of a line, like the desktop's: the phone's own 12 or 24 hour clock, with seconds when asked. */
fun formatTime(ts: Long, seconds: Boolean, is24h: Boolean): String {
    val pattern = when {
        is24h -> if (seconds) "HH:mm:ss" else "HH:mm"
        else -> if (seconds) "hh:mm:ss a" else "hh:mm a"
    }
    return DateTimeFormatter.ofPattern(pattern, Locale.getDefault()).withZone(ZoneId.systemDefault()).format(Instant.ofEpochMilli(ts))
}

fun is24Hour(context: android.content.Context): Boolean = DateFormat.is24HourFormat(context)

/** Link preview cards that have arrived (by link key), shared by every row; a row asks for its link once it shows. */
class PreviewStore(private val ask: (String) -> Unit) {
    val cards = mutableStateMapOf<String, Preview>()
    private val asked = HashSet<String>()

    fun request(link: LinkRef) {
        if (link.key in cards) return
        if (asked.add(link.key)) ask(link.href)
    }

    /** Ask again (a failed request is retried by the core after a while: let a row that scrolls back ask). */
    fun forget(key: String) {
        asked.remove(key)
    }
}

val LocalPreviews = compositionLocalOf<PreviewStore?> { null }
