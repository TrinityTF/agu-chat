package lol.agu.chat.ui

import androidx.compose.runtime.Immutable
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonPrimitive

@Serializable
@Immutable
data class RowUser(val login: String? = null, val name: String? = null, val color: String? = null)

@Serializable
@Immutable
data class Badge(val url: String, val title: String? = null)

@Serializable
@Immutable
data class Overlay(val name: String? = null, val url: String)

/** One piece of a message; [t] is text / emote / emoji / mention / link / gif / cheer, the rest depends on it. */
@Serializable
@Immutable
data class Part(
    val t: String,
    val s: String? = null,
    val name: String? = null,
    val url: String? = null,
    val alt: String? = null,
    val login: String? = null,
    val color: String? = null,
    val amount: Double? = null,
    val overlays: List<Overlay> = emptyList(),
)

@Serializable
@Immutable
data class ReplyRef(val user: String? = null, val text: String? = null, val parentId: JsonPrimitive? = null)

/** A matched highlight rule: the row's colour, and how strong its tint is. */
@Serializable
@Immutable
data class Highlight(val color: String, val alpha: Float = 0.14f)

@Serializable
@Immutable
data class Row(
    val key: String,
    val sk: String = "",
    val platform: String = "",
    val ts: Long = 0,
    val backlog: Boolean = false,
    val kind: String = "msg",
    val id: JsonPrimitive? = null,
    val user: RowUser? = null,
    val badges: List<Badge> = emptyList(),
    val parts: List<Part> = emptyList(),
    val action: Boolean = false,
    val firstMsg: Boolean = false,
    val deleted: Boolean = false,
    val deletedNote: String? = null,
    val reply: ReplyRef? = null,
    val system: String? = null,
    val bits: Double? = null,
    val status: String? = null,
    /** a highlight rule matched this line */
    val highlight: Highlight? = null,
    /** the line names you (drawn red) */
    val mention: Boolean = false,
) {
    /** The text of the message as plain text, for copying. */
    fun plainText(): String = parts.joinToString("") {
        when (it.t) {
            "emote" -> it.name.orEmpty()
            "gif" -> it.alt ?: it.url.orEmpty()
            "cheer" -> it.name.orEmpty() + (it.amount?.toLong()?.toString() ?: "")
            else -> it.s.orEmpty()
        }
    }
}

@Serializable
@Immutable
data class Conn(
    val state: String = "connecting",
    val detail: String? = null,
    val live: Boolean? = null,
    val title: String? = null,
    val canSend: Boolean? = null,
    val readOnlyReason: String? = null,
    val loggedOut: Boolean? = null,
)

@Serializable
@Immutable
data class AccountUser(val id: JsonPrimitive? = null, val login: String? = null, val name: String? = null)

@Serializable
@Immutable
data class Pending(val userCode: String = "", val verificationUri: String = "")

@Serializable
@Immutable
data class AccountState(
    val connected: Boolean = false,
    val user: AccountUser? = null,
    val pending: Pending? = null,
    val error: String? = null,
    val unsupported: Boolean = false,
)

/** One channel of a tab. */
@Serializable
@Immutable
data class ChannelRef(val platform: String, val channel: String)

@Serializable
@Immutable
data class SavedTab(val name: String = "", val channels: List<ChannelRef> = emptyList())

/** The core's source key of a channel (Twitch and Kick names are kept lowercase). */
fun ChannelRef.sk(): String = "$platform:${if (platform == "twitch" || platform == "kick") channel.lowercase() else channel}"

/**
 * An open tab: one or more channels from any platforms merged into one list. [id] is the core's view key; [name] is
 * the user's own title ("" shows the channels' names).
 */
@Immutable
data class Tab(val id: String, val name: String, val channels: List<ChannelRef>) {
    val sks get() = channels.map { it.sk() }

    /** The title in the tab strip: the name, else the first channel (and "…" when there are more). */
    fun title(full: Boolean = false): String = when {
        name.isNotBlank() -> name
        full || channels.size < 2 -> channels.joinToString(", ") { it.channel }
        else -> channels.first().channel + ", …"
    }
}

/** What a tab's small circle says; a mention beats live, live beats new messages. */
enum class Dot { Message, Live, Highlight }

/** One line of the Mentions screen. */
@Immutable
data class MentionEntry(val id: String, val vk: String, val sk: String, val channel: String, val platform: String, val ts: Long, val read: Boolean, val whisper: Boolean, val row: Row)

/** Show this row of this tab (a tapped mention), once. */
@Immutable
data class Jump(val vk: String, val key: String, val nonce: Long)

/** The core wants a sign-in screen: a WebView on [url]. [show] means "point the open window here" (Twitch's authorise page). */
@Immutable
data class LoginRequest(val id: Long, val platform: String, val url: String, val show: Boolean = false)
